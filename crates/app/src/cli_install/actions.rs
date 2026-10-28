//! 安全安装动作(设计 §5.2):安装/修复/卸载全部走「rename 隔离 → 复核 →
//! 落定」的原子序列,绝不覆盖或删除非本 App 拥有的条目。
//!
//! 动作入参是**点击时重新判定**得到的 EntryState;复核不过一律还原现场。

use crate::cli_install::classify::{classify_entry, EntryState};
use crate::cli_install::InstallerFs;
use std::io;
use std::path::{Path, PathBuf};

/// 动作失败分类:UI 据此决定文案(§5.3)。
#[derive(Debug)]
pub enum ActionError {
    /// 条目不属于动作允许的类型(Ours 之外的卸载、BrokenApp 之外的修复、
    /// EEXIST 后复核非 Ours 等)。
    NotAllowed,
    /// 文件系统错误。
    Io(io::Error),
    /// 父目录不可写:保留现场,给手动命令。
    NonWritable(PathBuf),
}

/// 动作临时名:同目录下的隐藏文件 `.{原名}.{tag}.{pid}`。
/// 同目录保证 rename 原子;带 pid 避免与其他动作/残留撞名。
pub(crate) fn tmp(path: &Path, tag: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{tag}.{}", std::process::id()))
}

/// 安装:先 symlink 创建;撞 EEXIST 立即重分类——Ours → 幂等成功,
/// 其他 → 「已被占用,不覆盖」;创建成功后复核 Ours 才算成功(设计 §5.2)。
pub fn install(
    fs: &dyn InstallerFs,
    dst: &Path,
    target: &Path,
    bundle_cli: &Path,
) -> Result<(), ActionError> {
    match fs.symlink(target, dst) {
        Ok(()) => match classify_entry(fs, dst, bundle_cli) {
            // 创建后复核:确认真的是我们刚放的链接才算成功。
            Ok(Some(EntryState::Ours { .. })) => Ok(()),
            Ok(_) => Err(ActionError::NotAllowed),
            Err(e) => Err(ActionError::Io(e)),
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            match classify_entry(fs, dst, bundle_cli) {
                Ok(Some(EntryState::Ours { .. })) => Ok(()),
                _ => Err(ActionError::NotAllowed),
            }
        }
        Err(e) => Err(ActionError::Io(e)),
    }
}

/// 修复:只对卷已挂载的 BrokenApp 生效;rename 旧链接到临时名 → 复核仍为
/// BrokenApp → 创建新链接;任何一步失败都把旧链接移回原位,绝不留半成品。
pub fn repair(
    fs: &dyn InstallerFs,
    entry: &EntryState,
    new_target: &Path,
    bundle_cli: &Path,
) -> Result<(), ActionError> {
    let EntryState::BrokenApp {
        path,
        volume_mounted,
        ..
    } = entry
    else {
        return Err(ActionError::NotAllowed);
    };
    // 卷未挂载时禁用:目标是旧卷上的 App,重装指向当前 App 属于误操作场景。
    if !*volume_mounted {
        return Err(ActionError::NotAllowed);
    }
    let parent = path.parent().ok_or(ActionError::NotAllowed)?;
    // 不可写预判:保留旧链接,交给手动命令(设计 §5.2)。
    if !fs.access_w(parent) {
        return Err(ActionError::NonWritable(parent.into()));
    }
    let old = tmp(path, "old");
    fs.rename(path, &old).map_err(ActionError::Io)?;
    // 复核移开的旧链接仍是 BrokenApp 形态(防并发替换)。
    let ok = matches!(
        classify_entry(fs, &old, bundle_cli),
        Ok(Some(EntryState::BrokenApp { .. }))
    );
    if !ok {
        let _ = fs.rename(&old, path);
        return Err(ActionError::NotAllowed);
    }
    match fs.symlink(new_target, path) {
        Ok(()) => {
            let _ = fs.remove_file(&old);
            Ok(())
        }
        Err(e) => {
            // 创建失败:旧链接移回,现场还原。
            let _ = fs.rename(&old, path);
            Err(ActionError::Io(e))
        }
    }
}

/// 卸载:只删第一个 Ours;rename 到临时名复核 dev+ino 仍是 Ours 后再删,
/// 失败还原;动手前条目已消失(ENOENT)按成功处理(设计 §5.2)。
pub fn uninstall(
    fs: &dyn InstallerFs,
    entry: &EntryState,
    bundle_cli: &Path,
) -> Result<(), ActionError> {
    let EntryState::Ours { path, .. } = entry else {
        return Err(ActionError::NotAllowed);
    };
    let parent = path.parent().ok_or(ActionError::NotAllowed)?;
    if !fs.access_w(parent) {
        return Err(ActionError::NonWritable(parent.into()));
    }
    let moved = tmp(path, "rm");
    match fs.rename(path, &moved) {
        // ENOENT:条目已经不在了,卸载目的已达成。
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(ActionError::Io(e)),
        Ok(()) => {}
    }
    if matches!(
        classify_entry(fs, &moved, bundle_cli),
        Ok(Some(EntryState::Ours { .. }))
    ) {
        fs.remove_file(&moved).map_err(ActionError::Io)
    } else {
        let _ = fs.rename(&moved, path);
        Err(ActionError::NotAllowed)
    }
}

/// 生成带守卫的可复制手动命令:目标已存在(含链接)即拒绝打印提示,
/// 否则 `ln -s`(必要时加 sudo)。绝不用无条件的 `-f`/`-sf`。
/// 路径一律单引号转义(`'\''`)。
pub fn manual_install_command(dir: &Path, cli: &Path, sudo: bool) -> String {
    fn q(p: &Path) -> String {
        format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"))
    }
    let dst = q(&dir.join("macEverything"));
    let cli = q(cli);
    let prefix = if sudo { "sudo " } else { "" };
    format!(
        "if [ -e {dst} ] || [ -L {dst} ]; then echo 'refusing: {dst} exists'; \
         else {prefix}ln -s {cli} {dst}; fi"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_install::classify::classify_entry;
    use crate::cli_install::test_fs::FakeFs;

    const BUNDLE: &str = "/A/app/Contents/MacOS/macEverything";
    const DST: &str = "/bin/macEverything";
    const OLD_TARGET: &str = "/Volumes/USB/Old.app/Contents/MacOS/macEverything";

    /// 预置卷已挂载 + 断链 BrokenApp 场景,返回重判定的条目。
    fn broken_app_entry() -> (FakeFs, EntryState) {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.dir("/Volumes/USB");
        fs.link(DST, OLD_TARGET);
        let entry = classify_entry(&fs, Path::new(DST), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        assert!(matches!(
            &entry,
            EntryState::BrokenApp {
                volume_mounted: true,
                ..
            }
        ));
        (fs, entry)
    }

    /// 安装幂等:第一次真实创建;第二次撞 EEXIST 后重分类为 Ours → 成功,
    /// 且不产生第二个链接(总调用数只多这一次尝试)。
    #[test]
    fn install_is_idempotent() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        install(&fs, Path::new(DST), Path::new(BUNDLE), Path::new(BUNDLE)).unwrap();
        assert_eq!(fs.symlink_calls(), 1);
        install(&fs, Path::new(DST), Path::new(BUNDLE), Path::new(BUNDLE)).unwrap();
        assert_eq!(fs.symlink_calls(), 2, "第二次安装只应尝试一次并命中 EEXIST");
        // 链接目标没被改写。
        assert_eq!(
            fs.read_link_of(Path::new(DST)).as_deref(),
            Some(Path::new(BUNDLE))
        );
    }

    /// 已存在的 Ours 链接:EEXIST 重分类 → 成功(不覆盖)。
    #[test]
    fn eexist_reclassifies_to_ours() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.link(DST, BUNDLE);
        assert!(install(&fs, Path::new(DST), Path::new(BUNDLE), Path::new(BUNDLE)).is_ok());
    }

    /// 被占用的目标(别人的普通文件)一律拒绝,且原文件原封不动。
    #[test]
    fn install_does_not_overwrite_foreign() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.file(DST, 7, 7);
        let r = install(&fs, Path::new(DST), Path::new(BUNDLE), Path::new(BUNDLE));
        assert!(matches!(r, Err(ActionError::NotAllowed)));
        let m = fs.meta_of(Path::new(DST)).unwrap();
        assert!(m.is_file && !m.is_symlink, "原文件不得被改动");
        assert!(fs.read_link_of(Path::new(DST)).is_none());
    }

    /// 原子修复:创建新链接失败时,旧链接必须原样回到原位,临时名清理干净。
    #[test]
    fn repair_restores_old_link_on_create_failure() {
        let (mut fs, entry) = broken_app_entry();
        fs.set_fail_symlink(true);
        let r = repair(&fs, &entry, Path::new(BUNDLE), Path::new(BUNDLE));
        assert!(matches!(r, Err(ActionError::Io(_))));
        assert_eq!(
            fs.read_link_of(Path::new(DST)).as_deref(),
            Some(Path::new(OLD_TARGET)),
            "旧链接应回到原位"
        );
        assert!(!fs.exists(&tmp(Path::new(DST), "old")), "临时名应已清理");
    }

    /// rename 后复核:旧链接若已变成非 BrokenApp 形态(并发替换),必须
    /// 中止并把旧链接移回原位。
    #[test]
    fn repair_verifies_moved_link() {
        let (mut fs, entry) = broken_app_entry();
        // 竞态桩:old_tmp 上的 read_link 固定返回非 app 目标。
        let old = tmp(Path::new(DST), "old");
        fs.link_override(&old, "/elsewhere/tool");
        let r = repair(&fs, &entry, Path::new(BUNDLE), Path::new(BUNDLE));
        assert!(matches!(r, Err(ActionError::NotAllowed)));
        assert_eq!(
            fs.read_link_of(Path::new(DST)).as_deref(),
            Some(Path::new(OLD_TARGET)),
            "复核失败必须还原"
        );
        assert!(!fs.exists(&old));
    }

    /// 卷未挂载的 BrokenApp 禁用修复,现场不动。
    #[test]
    fn repair_unmounted_volume_denied() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.set_no_volumes(true);
        fs.link(DST, OLD_TARGET);
        let entry = classify_entry(&fs, Path::new(DST), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        let r = repair(&fs, &entry, Path::new(BUNDLE), Path::new(BUNDLE));
        assert!(matches!(r, Err(ActionError::NotAllowed)));
        assert_eq!(
            fs.read_link_of(Path::new(DST)).as_deref(),
            Some(Path::new(OLD_TARGET))
        );
    }

    /// 父目录不可写:保留旧链接并报 NonWritable(留给手动命令)。
    #[test]
    fn repair_nonwritable_keeps_old_link() {
        let (mut fs, entry) = broken_app_entry();
        fs.set_access_w(false);
        let r = repair(&fs, &entry, Path::new(BUNDLE), Path::new(BUNDLE));
        assert!(matches!(r, Err(ActionError::NonWritable(_))));
        assert_eq!(
            fs.read_link_of(Path::new(DST)).as_deref(),
            Some(Path::new(OLD_TARGET))
        );
    }

    /// 原子修复成功路径:新链接就位、旧临时名清理。
    #[test]
    fn repair_replaces_and_cleans_up() {
        let (fs, entry) = broken_app_entry();
        repair(&fs, &entry, Path::new(BUNDLE), Path::new(BUNDLE)).unwrap();
        assert_eq!(
            fs.read_link_of(Path::new(DST)).as_deref(),
            Some(Path::new(BUNDLE))
        );
        assert!(!fs.exists(&tmp(Path::new(DST), "old")));
    }

    /// 拒绝删除非本 App 的条目。
    #[test]
    fn uninstall_does_not_delete_foreign() {
        let entry = EntryState::Foreign {
            path: DST.into(),
            executable: true,
        };
        assert!(matches!(
            uninstall(&FakeFs::new(), &entry, Path::new(BUNDLE)),
            Err(ActionError::NotAllowed)
        ));
    }

    /// 卸载 Ours:原子移除成功。
    #[test]
    fn uninstall_removes_ours() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.link(DST, BUNDLE);
        let entry = classify_entry(&fs, Path::new(DST), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        assert!(matches!(entry, EntryState::Ours { .. }));
        uninstall(&fs, &entry, Path::new(BUNDLE)).unwrap();
        assert!(!fs.exists(Path::new(DST)), "链接应已移除");
        assert!(!fs.exists(&tmp(Path::new(DST), "rm")));
    }

    /// 条目已在动手前消失(ENOENT)按成功处理(设计 §5.2)。
    #[test]
    fn uninstall_missing_entry_is_success() {
        let fs = FakeFs::new();
        let entry = EntryState::Ours {
            path: DST.into(),
            target: BUNDLE.into(),
        };
        uninstall(&fs, &entry, Path::new(BUNDLE)).unwrap();
    }

    /// 父目录不可写:不动手,报 NonWritable。
    #[test]
    fn uninstall_nonwritable_denied() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.link(DST, BUNDLE);
        let entry = classify_entry(&fs, Path::new(DST), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        fs.set_access_w(false);
        let r = uninstall(&fs, &entry, Path::new(BUNDLE));
        assert!(matches!(r, Err(ActionError::NonWritable(_))));
        assert!(fs.exists(Path::new(DST)), "不可写时不得触碰链接");
    }

    /// 手动命令:带 `[ -e ] || [ -L ]` 守卫,绝无 `-sf`。
    #[test]
    fn manual_command_refuses_existing_entry() {
        let cmd = manual_install_command(Path::new("/opt/homebrew/bin"), Path::new(BUNDLE), false);
        assert!(cmd.contains("[ -e "));
        assert!(cmd.contains("[ -L "));
        assert!(!cmd.contains("-sf"));
        assert!(cmd.contains("ln -s"));
        assert!(cmd.contains("refusing: "));
    }

    /// 引号与 sudo:路径里的空格/单引号必须正确转义。
    #[test]
    fn manual_command_quotes_and_sudo() {
        let cmd = manual_install_command(
            Path::new("/usr/local/bin"),
            Path::new("/A/My App's.app/Contents/MacOS/macEverything"),
            true,
        );
        assert!(cmd.starts_with("if [ -e "), "守卫在最前: {cmd}");
        assert!(cmd.contains("sudo ln -s"));
        assert!(
            cmd.contains("/A/My App'\\''s.app/Contents/MacOS/macEverything"),
            "单引号应转义为 '\\''': {cmd}"
        );
        assert!(!cmd.contains("-sf"));
    }
}

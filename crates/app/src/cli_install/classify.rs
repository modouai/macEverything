//! PATH 条目分类(设计 §5.0-B/C):对 PATH 段下的 `macEverything` 条目判定
//! 归属与状态,并从当前 App 位置推导随包 CLI 与运行环境可用性。
//!
//! 语义锚点(与设计逐条对应):
//! - absent 只认 ENOENT/ENOTDIR,其余一律按错误处理(→ `Unreadable`);
//! - 符号链接跟随 `stat` 后按 `dev`+`ino` 判定 `Ours`;**硬链接**(普通文件
//!   同 inode)刻意不算 `Ours`——不是我们装的(§5.0-B.4,测试钉死);
//! - 断链目标文本匹配 `*.app/Contents/MacOS/macEverything` → `BrokenApp`,
//!   卷根确实不存在时禁用修复;
//! - 每次判定重新 `stat` 随包 CLI,不缓存 inode(App 运行中可能被替换)。

use crate::cli_install::path_env::PathSource;
use crate::cli_install::InstallerFs;
use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

/// PATH 中单个 `macEverything` 条目的分类结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    /// 指向随包 CLI 的符号链接(dev+ino 一致)。
    Ours { path: PathBuf, target: PathBuf },
    /// 指向另一个 macEverything App 的符号链接(目标存在,非同一文件)。
    OtherApp {
        path: PathBuf,
        target: PathBuf,
        executable: bool,
    },
    /// 其他来源:普通文件/目录/FIFO/socket,或链接到非 app 目标。
    Foreign { path: PathBuf, executable: bool },
    /// 断链且目标文本匹配 app 形态(旧安装);卷未挂载时禁用修复。
    BrokenApp {
        path: PathBuf,
        target: PathBuf,
        volume_mounted: bool,
    },
    /// 断链且目标不匹配 app 形态。
    BrokenForeign { path: PathBuf, target: PathBuf },
    /// 非 ENOENT 的 lstat/read_link/stat 错误(含 EACCES/ELOOP):禁用一切动作。
    Unreadable { path: PathBuf, error: String },
}

/// 运行环境可用性(设计 §5.0-C)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Ready,
    /// exe 不在 `<...>.app/Contents/MacOS` 下(cargo 裸跑)。
    DevBuild,
    /// 路径组件含 AppTranslocation。
    Translocated,
    /// 所在卷只读(statfs MNT_RDONLY)。
    ReadOnlyVolume,
    /// `<bundle>/Contents/MacOS/macEverything` 不存在。
    BundledCliMissing,
    /// 随包 CLI 不是普通文件,或没有执行位。
    BundledCliNotExecutable,
}

/// 一次完整观测:PATH 来源 + 全部条目命中 + 生效条目 + 运行环境。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub path_source: PathSource,
    /// PATH 顺序中的全部命中(含被遮蔽项)。
    pub hits: Vec<EntryState>,
    /// 第一个「跟随目标是普通文件且 access(X_OK) 通过」的下标,即终端实际
    /// 会执行的那个;None 表示无有效命中。
    pub effective: Option<usize>,
    pub availability: Availability,
    /// 随包 CLI 路径(仅 Ready 时为 Some)。
    pub bundled_cli: Option<PathBuf>,
    /// PATH 扫描出现过非 ENOENT 错误(UI 视为 Unknown,禁用动作)。
    pub path_unknown: bool,
    /// 探测到的 PATH 原值(`Failed` 时为 None);安装目标选择在 UI 侧
    /// 复用本值(与本次分类同一份 PATH,避免重复跑登录 shell)。
    pub path_var: Option<OsString>,
}

/// 「该段无条目」只认 ENOENT/ENOTDIR;其余错误(权限/循环/IO)一律按
/// 不可读处理(设计 §5.0-B:记 Unreadable,禁用动作)。
fn absent(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// 词法归一:去掉 CurDir、按 ParentDir 弹层。不触碰文件系统,
/// 断链目标文本的 `..` 解析靠它(test_fs 跟随相对链接也复用)。
pub(crate) fn lexical(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 目标文本是否呈现 `<...>.app/Contents/MacOS/macEverything` 形态:
/// 绝对路径、至少四段、最后四段依次匹配;固定组件与 `.app` 后缀均
/// 按 ASCII 大小写不敏感比较(APFS 默认大小写不敏感)。
pub fn is_app_target(p: &Path) -> bool {
    if !p.is_absolute() {
        return false;
    }
    let c: Vec<_> = p.components().collect();
    if c.len() < 4 {
        return false;
    }
    let n = c.len();
    let app = c[n - 4].as_os_str().to_string_lossy();
    app.to_ascii_lowercase().ends_with(".app")
        && c[n - 3]
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("Contents")
        && c[n - 2]
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("MacOS")
        && c[n - 1]
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("macEverything")
}

/// 断链目标所在卷是否还挂着:目标落在 `/Volumes/<name>/…` 下时 stat 卷根,
/// ENOENT → 未挂载(禁用修复);其他错误无法判断(可能是瞬时 EIO),宁可
/// 不禁用修复,让后续动作自己暴露错误,避免误伤可修复状态。
/// 非 /Volumes 目标一律视为可达。
fn volume_mounted(fs: &dyn InstallerFs, target: &Path) -> bool {
    let c: Vec<_> = target.components().collect();
    if c.len() >= 3 && c[0] == Component::RootDir && c[1].as_os_str() == "Volumes" {
        let root = Path::new("/Volumes").join(c[2].as_os_str());
        return match fs.stat(&root) {
            Ok(_) => true,
            Err(e) if absent(&e) => false,
            Err(_) => true,
        };
    }
    true
}

/// 分类单个 `PATH` 段下的 `macEverything` 条目。
///
/// 返回 `Ok(None)` 表示该段无条目(absent);`Err` 是非 ENOENT 错误,
/// 由调用方转为 Unreadable。要求 `bundle_cli` 存在(调用方先过
/// availability 的 Ready 判定,此处 stat 失败直接上抛)。
pub fn classify_entry(
    fs: &dyn InstallerFs,
    path: &Path,
    bundle_cli: &Path,
) -> io::Result<Option<EntryState>> {
    // 每次判定重新 stat 随包 CLI,不缓存 inode(App 运行中可能被替换)。
    let bundle = fs.stat(bundle_cli)?;
    let lm = match fs.lstat(path) {
        Ok(m) => m,
        Err(e) if absent(&e) => return Ok(None),
        Err(e) => return Err(e),
    };
    if lm.is_symlink {
        let text = fs.read_link(path)?;
        // 相对目标按链接父目录解析,再做词法 `..` 归一。
        let target = lexical(&if text.is_absolute() {
            text
        } else {
            path.parent().unwrap_or(Path::new("/")).join(text)
        });
        return match fs.stat(path) {
            Ok(m) => {
                // 跟随后与随包 CLI 同 dev+ino 才算 Ours;is_file 守卫是
                // 防御性的(随包 CLI 必为普通文件)。硬链接走下面的非符号
                // 链接分支,刻意不算 Ours。
                if m.is_file && (m.dev, m.ino) == (bundle.dev, bundle.ino) {
                    Ok(Some(EntryState::Ours {
                        path: path.into(),
                        target,
                    }))
                } else if is_app_target(&target) {
                    Ok(Some(EntryState::OtherApp {
                        path: path.into(),
                        target,
                        executable: fs.access_x(path),
                    }))
                } else {
                    Ok(Some(EntryState::Foreign {
                        path: path.into(),
                        executable: m.is_file && fs.access_x(path),
                    }))
                }
            }
            Err(e) if absent(&e) => {
                if is_app_target(&target) {
                    Ok(Some(EntryState::BrokenApp {
                        path: path.into(),
                        target: target.clone(),
                        volume_mounted: volume_mounted(fs, &target),
                    }))
                } else {
                    Ok(Some(EntryState::BrokenForeign {
                        path: path.into(),
                        target,
                    }))
                }
            }
            Err(e) => Err(e),
        };
    }
    // 非符号链接(普通文件/目录/FIFO/socket)一律 Foreign,永不触碰。
    Ok(Some(EntryState::Foreign {
        path: path.into(),
        executable: lm.is_file && fs.access_x(path),
    }))
}

/// 去尾斜杠(设计 §5.0-B 预处理):`/bin/` 与 `/bin` 是同一段,归一后再做
/// 绝对路径判定与去重;只剥分隔符,其余保持原样(根 `/` 不动)。
/// 安装目标选择(actions)复用同一预处理,保证两条路径的段语义一致。
pub(crate) fn strip_trailing_seps(p: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let mut b = p.as_os_str().as_bytes();
    while b.len() > 1 && b[b.len() - 1] == b'/' {
        b = &b[..b.len() - 1];
    }
    PathBuf::from(std::ffi::OsStr::from_bytes(b))
}

/// 顺序扫描 PATH,返回 (全部命中, effective 下标, 是否出现不可读段)。
///
/// 相对段跳过;每段先去尾斜杠,重复段(按归一后的字面路径)只扫一次;
/// 分类出错(EACCES/ELOOP 等)记 Unreadable 并置 unknown。effective 取
/// 第一个「stat 为普通文件且 access(X_OK) 通过」的条目——终端实际会执行
/// 的那个。
pub fn scan_path(
    fs: &dyn InstallerFs,
    path_var: &std::ffi::OsStr,
    bundle_cli: &Path,
) -> (Vec<EntryState>, Option<usize>, bool) {
    let mut hits = Vec::new();
    let mut effective = None;
    let mut unknown = false;
    let mut seen = std::collections::HashSet::new();
    for seg in std::env::split_paths(path_var) {
        let seg = strip_trailing_seps(&seg);
        if !seg.is_absolute() {
            continue;
        }
        if !seen.insert(seg.clone()) {
            continue;
        }
        let entry = seg.join("macEverything");
        match classify_entry(fs, &entry, bundle_cli) {
            Ok(None) => {}
            Ok(Some(state)) => {
                if effective.is_none()
                    && fs
                        .stat(&entry)
                        .map(|m| m.is_file && fs.access_x(&entry))
                        .unwrap_or(false)
                {
                    effective = Some(hits.len());
                }
                hits.push(state);
            }
            Err(e) => {
                unknown = true;
                hits.push(EntryState::Unreadable {
                    path: entry,
                    error: e.to_string(),
                });
            }
        }
    }
    (hits, effective, unknown)
}

/// 从当前 App 可执行文件推导随包 CLI 与位置状态(设计 §5.0-C)。
///
/// exe 先 canonicalize(设计:「并 canonicalize」):current_exe 可能带着
/// `/var`→`/private/var` 之类的瞬时/别名前缀或符号链接化的 bundle 目录,
/// 这些形态不解析成真实位置,就会泄漏进链接文本与 statfs 目标(例如把
/// 随包 CLI 装成指向别名路径的链接,别名失效即断链)。解析失败视为无法
/// 证明打包形态,按 DevBuild 处理(不给任何动作)。
pub fn availability(fs: &dyn InstallerFs, exe: &Path) -> (Availability, Option<PathBuf>) {
    let exe = match fs.canonicalize(exe) {
        Ok(p) => p,
        Err(_) => return (Availability::DevBuild, None),
    };
    let Some(dir) = exe.parent() else {
        return (Availability::DevBuild, None);
    };
    let c: Vec<_> = dir.components().collect();
    if c.len() < 3 {
        return (Availability::DevBuild, None);
    }
    let n = c.len();
    let app = c[n - 3].as_os_str().to_string_lossy();
    let shape = app.to_ascii_lowercase().ends_with(".app")
        && c[n - 2]
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("Contents")
        && c[n - 1]
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("MacOS");
    if !shape {
        return (Availability::DevBuild, None);
    }
    let cli = dir.join("macEverything");
    let m = match fs.lstat(&cli) {
        Ok(m) => m,
        Err(_) => return (Availability::BundledCliMissing, None),
    };
    if !m.is_file || !fs.access_x(&cli) {
        return (Availability::BundledCliNotExecutable, None);
    }
    // 非空校验(设计 §5.0-C):空文件按内容缺失处理(半拷贝状态)。
    if m.len == 0 {
        return (Availability::BundledCliMissing, None);
    }
    if exe
        .components()
        .any(|c| c.as_os_str() == "AppTranslocation")
    {
        return (Availability::Translocated, None);
    }
    // statfs 失败按非只读处理:修复/安装动作随后自会暴露真实错误。
    if fs.statfs(dir).map(|s| s.read_only).unwrap_or(false) {
        return (Availability::ReadOnlyVolume, None);
    }
    (Availability::Ready, Some(cli))
}

/// 一次完整观测:真实 fs + 登录 shell PATH。
///
/// 会 spawn 登录 shell(可能耗时至 3s 超时),只能在后台线程调用;
/// 单飞与 generation 由 UI 层(Task 8)负责。
pub fn observe() -> Observation {
    let fs = crate::cli_install::RealFs::new();
    let exe = std::env::current_exe().unwrap_or_default();
    let (availability, bundled_cli) = availability(&fs, &exe);
    let (path_source, path_var) = crate::cli_install::path_env::probe_path_env(
        &crate::cli_install::path_env::RealRunner,
        std::env::var("PATH").ok().as_deref(),
    );
    // 随包 CLI 或 PATH 任一不可用即视为 Unknown(禁用动作,只读展示)。
    let (hits, effective, path_unknown) = match (&bundled_cli, &path_var) {
        (Some(cli), Some(p)) => scan_path(&fs, p, cli),
        _ => (Vec::new(), None, true),
    };
    Observation {
        path_source,
        hits,
        effective,
        availability,
        bundled_cli,
        path_unknown,
        path_var,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_install::test_fs::FakeFs;
    use crate::cli_install::{Meta, StatFs};
    use std::ffi::OsStr;

    const BUNDLE: &str = "/A/app/Contents/MacOS/macEverything";

    #[test]
    fn app_pattern_requires_components() {
        assert!(is_app_target(Path::new(
            "/A/Foo.app/Contents/MacOS/macEverything"
        )));
        // 中间不是 .app
        assert!(!is_app_target(Path::new(
            "/tmp/notapp/Contents/MacOS/macEverything"
        )));
        // 末段多出后缀不算
        assert!(!is_app_target(Path::new(
            "/A/Foo.app/Contents/MacOS/macEverything.bak"
        )));
        // 段数不足 / 相对路径都不算
        assert!(!is_app_target(Path::new("/Contents/MacOS/macEverything")));
        assert!(!is_app_target(Path::new(
            "Foo.app/Contents/MacOS/macEverything"
        )));
    }

    /// 大小写不敏感(APFS 默认大小写不敏感),但形态必须精确到四段。
    #[test]
    fn app_pattern_is_case_insensitive_and_exact() {
        assert!(is_app_target(Path::new(
            "/A/foo.APP/contents/macos/maceverything"
        )));
        assert!(is_app_target(Path::new(
            "/A/Foo.App/Contents/MACOS/MacEverything"
        )));
        // 尾部多一段打破形态
        assert!(!is_app_target(Path::new(
            "/A/Foo.app/Contents/MacOS/macEverything/extra"
        )));
    }

    #[test]
    fn relative_link_resolves_against_parent() {
        let mut fs = FakeFs::new();
        fs.file("/A/app/Contents/MacOS/macEverything", 9, 9);
        fs.link(
            "/bin/macEverything",
            "../A/app/Contents/MacOS/macEverything",
        );
        let s = classify_entry(
            &fs,
            Path::new("/bin/macEverything"),
            Path::new("/A/app/Contents/MacOS/macEverything"),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(s, EntryState::Ours { .. }));
    }

    #[test]
    fn eacces_is_unreadable_not_broken() {
        // 条目本身拒绝访问(如父目录无执行位),随包 CLI 可读;scan_path 必须
        // 产出 Unreadable,且不得误判成断链(absent 只认 ENOENT/ENOTDIR)。
        struct Deny;
        impl InstallerFs for Deny {
            fn lstat(&self, _: &Path) -> io::Result<Meta> {
                Err(io::ErrorKind::PermissionDenied.into())
            }
            fn stat(&self, p: &Path) -> io::Result<Meta> {
                if p == Path::new(BUNDLE) {
                    // 随包 CLI 正常存在(否则会在分类入口先短路)。
                    Ok(Meta {
                        dev: 9,
                        ino: 9,
                        len: 4096,
                        is_file: true,
                        is_symlink: false,
                        mode: 0o755,
                    })
                } else {
                    Err(io::ErrorKind::PermissionDenied.into())
                }
            }
            fn read_link(&self, _: &Path) -> io::Result<PathBuf> {
                Err(io::ErrorKind::PermissionDenied.into())
            }
            // 本测试只走 scan_path/classify_entry,不会触达 canonicalize。
            fn canonicalize(&self, _: &Path) -> io::Result<PathBuf> {
                Err(io::ErrorKind::PermissionDenied.into())
            }
            fn access_x(&self, _: &Path) -> bool {
                false
            }
            fn access_w(&self, _: &Path) -> bool {
                false
            }
            fn symlink(&self, _: &Path, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn remove_file(&self, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn statfs(&self, _: &Path) -> io::Result<StatFs> {
                Ok(StatFs { read_only: false })
            }
        }
        let (hits, _, unknown) = scan_path(
            &Deny,
            OsStr::new("/denied"),
            Path::new("/A/app/Contents/MacOS/macEverything"),
        );
        assert!(unknown);
        assert!(matches!(hits[0], EntryState::Unreadable { .. }));
    }

    #[test]
    fn broken_app_volume_unmounted_disables_repair() {
        let mut fs = FakeFs::new();
        fs.set_no_volumes(true);
        fs.file("/A/app/Contents/MacOS/macEverything", 9, 9);
        fs.link(
            "/bin/macEverything",
            "/Volumes/Backup/Old.app/Contents/MacOS/macEverything",
        );
        let s = classify_entry(
            &fs,
            Path::new("/bin/macEverything"),
            Path::new("/A/app/Contents/MacOS/macEverything"),
        )
        .unwrap()
        .unwrap();
        match s {
            EntryState::BrokenApp { volume_mounted, .. } => assert!(!volume_mounted),
            other => panic!("意外分类: {other:?}"),
        }
    }

    /// 硬链接钉死:普通文件即使与随包 CLI 同 dev+ino 也不是 `Ours`
    /// (不是我们装的符号链接,归 Foreign)。
    #[test]
    fn hardlink_is_foreign_not_ours() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.file_mode("/bin/macEverything", 9, 9, 0o755);
        let s = classify_entry(&fs, Path::new("/bin/macEverything"), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        match s {
            EntryState::Foreign { executable, .. } => assert!(executable),
            other => panic!("意外分类: {other:?}"),
        }
    }

    /// OtherApp 钉死:链接指向**另一个** macEverything App(目标存在、
    /// app 形态、dev+ino 不同)→ OtherApp 而非 Ours/Foreign。只有 dev+ino
    /// 一致才算 Ours——丢掉该比较会把别人的 App 误认成自己。
    #[test]
    fn symlink_to_other_app_is_otherapp() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.file_mode("/B/Real.app/Contents/MacOS/macEverything", 8, 8, 0o755);
        fs.link(
            "/bin/macEverything",
            "/B/Real.app/Contents/MacOS/macEverything",
        );
        let s = classify_entry(&fs, Path::new("/bin/macEverything"), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        match s {
            EntryState::OtherApp {
                executable, target, ..
            } => {
                assert!(executable);
                assert_eq!(
                    target,
                    Path::new("/B/Real.app/Contents/MacOS/macEverything")
                );
            }
            other => panic!("意外分类: {other:?}"),
        }
    }

    /// 链接到已存在的非 app 目标 → Foreign(记录执行位)。
    #[test]
    fn symlink_to_existing_nonapp_target_is_foreign() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.file_mode("/usr/bin/tool", 8, 8, 0o755);
        fs.link("/bin/macEverything", "/usr/bin/tool");
        let s = classify_entry(&fs, Path::new("/bin/macEverything"), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        match s {
            EntryState::Foreign { executable, .. } => assert!(executable),
            other => panic!("意外分类: {other:?}"),
        }
    }
    /// effective = 第一个「stat 为普通文件且可执行」的条目:
    /// 断链(不可执行)与指向无执行位目标的 Ours 都被跳过。
    #[test]
    fn effective_skips_dangling_and_non_executable() {
        let mut fs = FakeFs::new();
        fs.file_mode(BUNDLE, 9, 9, 0o644); // 目标存在但无执行位
        fs.link("/s1/macEverything", "/nowhere/tool"); // BrokenForeign,不可执行
        fs.link("/s2/macEverything", BUNDLE); // Ours 但不可执行
        fs.file_mode("/s3/macEverything", 8, 8, 0o755); // 普通 Foreign 文件,可执行
        let (hits, effective, unknown) =
            scan_path(&fs, OsStr::new("/s1:/s2:/s3"), Path::new(BUNDLE));
        assert!(!unknown);
        assert_eq!(effective, Some(2), "第一个可执行命中在第三段");
        assert!(matches!(hits[0], EntryState::BrokenForeign { .. }));
        assert!(matches!(hits[1], EntryState::Ours { .. }));
        assert!(matches!(
            hits[2],
            EntryState::Foreign {
                executable: true,
                ..
            }
        ));
    }

    #[test]
    fn directory_named_macheverything_is_foreign() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.dir("/bin/macEverything");
        let s = classify_entry(&fs, Path::new("/bin/macEverything"), Path::new(BUNDLE))
            .unwrap()
            .unwrap();
        match s {
            EntryState::Foreign { executable, .. } => assert!(!executable),
            other => panic!("意外分类: {other:?}"),
        }
    }

    /// ELOOP 不是「断链」:条目级 stat 的非 ENOENT 错误必须归 Unreadable
    /// 并置 unknown。errno 注入按条目路径限定(随包 CLI 照常可读),否则
    /// 分类会在入口的 bundle stat 处短路,测不到符号链接分支的错误臂。
    #[test]
    fn eloop_is_unreadable() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.link("/bin/macEverything", BUNDLE);
        fs.set_stat_errno_for(Path::new("/bin/macEverything"), libc::ELOOP);
        let (hits, _, unknown) = scan_path(&fs, OsStr::new("/bin"), Path::new(BUNDLE));
        assert!(unknown);
        assert!(matches!(hits[0], EntryState::Unreadable { .. }));
    }

    /// 同一段在 PATH 里出现两次只扫一次(设计 §5.0-B 去重)。
    #[test]
    fn duplicate_segments_scanned_once() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.link("/bin/macEverything", BUNDLE);
        let (hits, _, unknown) = scan_path(&fs, OsStr::new("/bin:/bin"), Path::new(BUNDLE));
        assert!(!unknown);
        assert_eq!(hits.len(), 1);
    }

    /// 去尾斜杠后去重:`/bin/` 与 `/bin` 是同一段,命中路径不带尾斜杠。
    #[test]
    fn trailing_slash_segment_dedups() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.link("/bin/macEverything", BUNDLE);
        let (hits, _, unknown) = scan_path(&fs, OsStr::new("/bin:/bin/"), Path::new(BUNDLE));
        assert!(!unknown);
        assert_eq!(hits.len(), 1);
        match &hits[0] {
            EntryState::Ours { path, .. } => {
                assert_eq!(path, Path::new("/bin/macEverything"))
            }
            other => panic!("意外分类: {other:?}"),
        }
    }

    /// 相对段(如 `.` / `bin`)一律跳过。
    #[test]
    fn relative_segments_skipped() {
        let mut fs = FakeFs::new();
        fs.file(BUNDLE, 9, 9);
        fs.file("/bin/macEverything", 8, 8);
        let (hits, _, _) = scan_path(&fs, OsStr::new("bin:/bin"), Path::new(BUNDLE));
        assert_eq!(hits.len(), 1);
    }

    fn avail_fs(exe: &str) -> FakeFs {
        let mut fs = FakeFs::new();
        fs.file(exe, 9, 9);
        fs
    }

    /// canonicalize 失败(exe 被并发替换/路径不可达)→ DevBuild,不给动作。
    #[test]
    fn availability_canonicalize_failure_is_devbuild() {
        struct CanonErr;
        impl InstallerFs for CanonErr {
            fn lstat(&self, _: &Path) -> io::Result<Meta> {
                Ok(Meta {
                    dev: 9,
                    ino: 9,
                    len: 4096,
                    is_file: true,
                    is_symlink: false,
                    mode: 0o755,
                })
            }
            fn stat(&self, _: &Path) -> io::Result<Meta> {
                Ok(Meta {
                    dev: 9,
                    ino: 9,
                    len: 4096,
                    is_file: true,
                    is_symlink: false,
                    mode: 0o755,
                })
            }
            fn read_link(&self, _: &Path) -> io::Result<PathBuf> {
                Err(io::ErrorKind::InvalidInput.into())
            }
            fn canonicalize(&self, _: &Path) -> io::Result<PathBuf> {
                Err(io::ErrorKind::NotFound.into())
            }
            fn access_x(&self, _: &Path) -> bool {
                true
            }
            fn access_w(&self, _: &Path) -> bool {
                true
            }
            fn symlink(&self, _: &Path, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn remove_file(&self, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn statfs(&self, _: &Path) -> io::Result<StatFs> {
                Ok(StatFs { read_only: false })
            }
        }
        let (a, cli) = availability(
            &CanonErr,
            Path::new("/A/Foo.app/Contents/MacOS/macEverything"),
        );
        assert_eq!(a, Availability::DevBuild);
        assert!(cli.is_none());
    }

    #[test]
    fn availability_devbuild_when_not_bundled() {
        let fs = FakeFs::new();
        let (a, cli) = availability(&fs, Path::new("/tmp/target/debug/macEverything"));
        assert_eq!(a, Availability::DevBuild);
        assert!(cli.is_none());
    }

    #[test]
    fn availability_missing_cli() {
        let fs = FakeFs::new();
        let (a, cli) = availability(&fs, Path::new("/A/Foo.app/Contents/MacOS/macEverything"));
        assert_eq!(a, Availability::BundledCliMissing);
        assert!(cli.is_none());
    }

    /// 随包 CLI 必须是普通文件:符号链接(lstat 非 stat)不算;
    /// 空文件按内容缺失处理(设计 §5.0-C「非空」)。
    #[test]
    fn availability_rejects_symlink_and_empty_cli() {
        let mut fs = FakeFs::new();
        fs.file("/elsewhere/real-cli", 9, 9);
        fs.link(
            "/A/Foo.app/Contents/MacOS/macEverything",
            "/elsewhere/real-cli",
        );
        let (a, cli) = availability(&fs, Path::new("/A/Foo.app/Contents/MacOS/macEverything"));
        assert_eq!(a, Availability::BundledCliNotExecutable);
        assert!(cli.is_none());

        let mut fs = FakeFs::new();
        fs.empty_file("/A/Foo.app/Contents/MacOS/macEverything");
        let (a, cli) = availability(&fs, Path::new("/A/Foo.app/Contents/MacOS/macEverything"));
        assert_eq!(a, Availability::BundledCliMissing);
        assert!(cli.is_none());
    }

    /// 无执行位的随包 CLI。
    #[test]
    fn availability_rejects_non_executable_cli() {
        let mut fs = FakeFs::new();
        fs.file_mode("/A/Foo.app/Contents/MacOS/macEverything", 9, 9, 0o644);
        let (a, cli) = availability(&fs, Path::new("/A/Foo.app/Contents/MacOS/macEverything"));
        assert_eq!(a, Availability::BundledCliNotExecutable);
        assert!(cli.is_none());
    }

    #[test]
    fn availability_translocated() {
        let exe = "/V/AppTranslocation/Foo.app/Contents/MacOS/macEverything";
        let fs = avail_fs(exe);
        let (a, cli) = availability(&fs, Path::new(exe));
        assert_eq!(a, Availability::Translocated);
        assert!(cli.is_none());
    }

    #[test]
    fn availability_read_only_volume() {
        let exe = "/V/Foo.app/Contents/MacOS/macEverything";
        let mut fs = avail_fs(exe);
        fs.set_read_only(true);
        let (a, cli) = availability(&fs, Path::new(exe));
        assert_eq!(a, Availability::ReadOnlyVolume);
        assert!(cli.is_none());
    }

    #[test]
    fn availability_ready_bundled() {
        let exe = "/Applications/Foo.app/Contents/MacOS/macEverything";
        let fs = avail_fs(exe);
        let (a, cli) = availability(&fs, Path::new(exe));
        assert_eq!(a, Availability::Ready);
        assert_eq!(cli.as_deref(), Some(Path::new(exe)));
        // bundle 形态大小写不敏感同样 Ready。
        let exe = "/Applications/foo.APP/Contents/MacOS/macEverything";
        let fs = avail_fs(exe);
        let (a, cli) = availability(&fs, Path::new(exe));
        assert_eq!(a, Availability::Ready);
        assert_eq!(cli.as_deref(), Some(Path::new(exe)));
    }
}

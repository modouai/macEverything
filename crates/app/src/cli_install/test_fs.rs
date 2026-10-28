//! 测试专用内存文件系统:classify / actions 单测共用的 [`InstallerFs`]
//! 实现,带故障注入(fail_symlink / stat 错误 / 只读卷 / 卷未挂载 /
//! 父目录不可写 / read_link 竞态桩)。
//!
//! 只在 `#[cfg(test)]` 下编译(见 mod.rs 的模块声明)。

use super::{InstallerFs, Meta, StatFs};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// 注入的 `stat` 错误码。用原始 errno 而非 `ErrorKind` 构造,保证 `kind()`
/// 走系统错误映射(如 ELOOP → FilesystemLoop)。
pub(crate) type StatErrno = libc::c_int;

#[derive(Default)]
struct State {
    /// 目录项:`lstat`/`stat` 的元数据来源。
    stat: HashMap<PathBuf, Meta>,
    /// 符号链接:路径 → 链接目标文本。
    link: HashMap<PathBuf, PathBuf>,
    /// read_link 竞态桩:固定路径返回伪造目标(rename 不随之移动),
    /// 模拟「动作进行中链接被并发替换」。
    link_override: HashMap<PathBuf, PathBuf>,
    /// 卷未挂载模拟:/Volumes 下的 stat/statfs 一律 NotFound。
    no_volumes: bool,
    /// statfs 只读位。
    read_only: bool,
    symlink_calls: usize,
}

pub(crate) struct FakeFs {
    state: RefCell<State>,
    fail_symlink: Cell<bool>,
    access_w: Cell<bool>,
    /// 按路径限定的 stat 错误注入(只打条目路径,不动随包 CLI——
    /// 否则分类在入口的 bundle stat 处短路,测不到目标分支)。
    stat_errno: RefCell<HashMap<PathBuf, StatErrno>>,
}

impl FakeFs {
    pub(crate) fn new() -> Self {
        Self {
            state: RefCell::new(State::default()),
            fail_symlink: Cell::new(false),
            access_w: Cell::new(true),
            stat_errno: RefCell::new(HashMap::new()),
        }
    }

    /// 放一个可执行普通文件(0o755,非空)。
    pub(crate) fn file(&mut self, p: &str, dev: u64, ino: u64) {
        self.file_mode(p, dev, ino, 0o755);
    }

    /// 放一个指定权限位的普通文件(exec 位决定 access_x)。
    pub(crate) fn file_mode(&mut self, p: &str, dev: u64, ino: u64, mode: u32) {
        self.state.borrow_mut().stat.insert(
            PathBuf::from(p),
            Meta {
                dev,
                ino,
                len: 4096,
                is_file: true,
                is_symlink: false,
                mode,
            },
        );
    }

    /// 放一个空文件(设计 §5.0-C:随包 CLI 非空校验)。
    pub(crate) fn empty_file(&mut self, p: &str) {
        self.state.borrow_mut().stat.insert(
            PathBuf::from(p),
            Meta {
                dev: 9,
                ino: 9,
                len: 0,
                is_file: true,
                is_symlink: false,
                mode: 0o755,
            },
        );
    }

    /// 放一个目录。
    pub(crate) fn dir(&mut self, p: &str) {
        self.dir_at(p, 5, 5);
    }

    /// 放一个指定 dev/ino 的目录(目录身份判定用例需要互不相同的身份;
    /// 默认 `dir()` 共用 (5,5))。
    pub(crate) fn dir_at(&mut self, p: &str, dev: u64, ino: u64) {
        self.state.borrow_mut().stat.insert(
            PathBuf::from(p),
            Meta {
                dev,
                ino,
                len: 0,
                is_file: false,
                is_symlink: false,
                mode: 0o755,
            },
        );
    }

    /// 放一个符号链接(目录项 + 目标文本)。
    pub(crate) fn link(&mut self, p: &str, target: &str) {
        let mut s = self.state.borrow_mut();
        s.link.insert(PathBuf::from(p), PathBuf::from(target));
        s.stat.insert(
            PathBuf::from(p),
            Meta {
                dev: 1,
                ino: 1,
                len: 0,
                is_file: false,
                is_symlink: true,
                mode: 0o777,
            },
        );
    }

    /// read_link 竞态桩:对指定路径固定返回伪造目标(rename 不随之移动)。
    pub(crate) fn link_override(&mut self, p: &Path, target: &str) {
        self.state
            .borrow_mut()
            .link_override
            .insert(p.to_path_buf(), PathBuf::from(target));
    }

    pub(crate) fn set_fail_symlink(&mut self, v: bool) {
        self.fail_symlink.set(v);
    }

    pub(crate) fn set_access_w(&mut self, v: bool) {
        self.access_w.set(v);
    }

    /// 按路径注入 stat 错误(如 `libc::ELOOP`);未注入的路径照常工作。
    pub(crate) fn set_stat_errno_for(&mut self, p: &Path, e: StatErrno) {
        self.stat_errno.borrow_mut().insert(p.to_path_buf(), e);
    }

    pub(crate) fn set_no_volumes(&mut self, v: bool) {
        self.state.borrow_mut().no_volumes = v;
    }

    pub(crate) fn set_read_only(&mut self, v: bool) {
        self.state.borrow_mut().read_only = v;
    }

    /// symlink 累计调用次数(断言「没有多余创建」用)。
    pub(crate) fn symlink_calls(&self) -> usize {
        self.state.borrow().symlink_calls
    }

    /// 测试断言用:读某路径的链接目标(不经过 trait)。
    pub(crate) fn read_link_of(&self, p: &Path) -> Option<PathBuf> {
        self.state.borrow().link.get(p).cloned()
    }

    /// 测试断言用:路径是否还是目录项。
    pub(crate) fn exists(&self, p: &Path) -> bool {
        self.state.borrow().stat.contains_key(p)
    }

    /// 测试断言用:读某路径的元数据(不经过 trait)。
    pub(crate) fn meta_of(&self, p: &Path) -> Option<Meta> {
        self.state.borrow().stat.get(p).copied()
    }
}

impl InstallerFs for FakeFs {
    fn lstat(&self, p: &Path) -> io::Result<Meta> {
        self.state
            .borrow()
            .stat
            .get(p)
            .copied()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn stat(&self, p: &Path) -> io::Result<Meta> {
        if let Some(&e) = self.stat_errno.borrow().get(p) {
            return Err(io::Error::from_raw_os_error(e));
        }
        let s = self.state.borrow();
        if s.no_volumes && p.starts_with("/Volumes") {
            return Err(io::ErrorKind::NotFound.into());
        }
        if let Some(t) = s.link.get(p) {
            // 与真实 fs 一致:相对目标按链接父目录解析后再查。
            let resolved = if t.is_absolute() {
                t.clone()
            } else {
                super::classify::lexical(&p.parent().unwrap_or_else(|| Path::new("/")).join(t))
            };
            return s
                .stat
                .get(&resolved)
                .copied()
                .ok_or_else(|| io::ErrorKind::NotFound.into());
        }
        s.stat
            .get(p)
            .copied()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        let s = self.state.borrow();
        if let Some(t) = s.link_override.get(p) {
            return Ok(t.clone());
        }
        s.link
            .get(p)
            .cloned()
            .ok_or_else(|| io::ErrorKind::InvalidInput.into())
    }

    // 测试里传入的都是合成路径,原样放回,使现有用例的路径断言不受影响。
    fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
        Ok(p.to_path_buf())
    }

    fn access_x(&self, p: &Path) -> bool {
        self.stat(p)
            .map(|m| m.is_file && m.mode & 0o111 != 0)
            .unwrap_or(false)
    }

    fn access_w(&self, _p: &Path) -> bool {
        self.access_w.get()
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let mut s = self.state.borrow_mut();
        s.symlink_calls += 1;
        if self.fail_symlink.get() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // 任何已存在的目录项(文件/目录/链接)都让 symlink 撞 EEXIST,与真实 fs 一致。
        if s.stat.contains_key(link) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        s.link.insert(link.to_path_buf(), target.to_path_buf());
        s.stat.insert(
            link.to_path_buf(),
            Meta {
                dev: 1,
                ino: 1,
                len: 0,
                is_file: false,
                is_symlink: true,
                mode: 0o777,
            },
        );
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut s = self.state.borrow_mut();
        let m = s
            .stat
            .remove(from)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        // 与 POSIX rename 一致:目标已存在则覆盖。
        s.stat.insert(to.to_path_buf(), m);
        if let Some(t) = s.link.remove(from) {
            s.link.insert(to.to_path_buf(), t);
        } else {
            // 非链接覆盖到旧链接上:必须先清掉 to 的旧目标,
            // 否则 stat(to) 会继续跟随幽灵链接。
            s.link.remove(to);
        }
        // link_override 按路径固定,不随 rename 移动(竞态桩语义)。
        Ok(())
    }

    fn remove_file(&self, p: &Path) -> io::Result<()> {
        let mut s = self.state.borrow_mut();
        let m = s
            .stat
            .remove(p)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if m.is_symlink {
            s.link.remove(p);
        }
        Ok(())
    }

    fn statfs(&self, p: &Path) -> io::Result<StatFs> {
        let s = self.state.borrow();
        if s.no_volumes && p.starts_with("/Volumes") {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(StatFs {
            read_only: s.read_only,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// POSIX 覆盖语义:普通文件 rename 盖到旧符号链接上后,stat 必须看到
    /// 普通文件本体——旧的链接目标条目必须随之清除,不得被幽灵跟随。
    #[test]
    fn rename_plain_file_over_link_clears_ghost_target() {
        let mut fs = FakeFs::new();
        fs.file("/real/other-tool", 8, 8);
        fs.link("/bin/macEverything", "/nowhere/ghost");
        fs.rename(
            Path::new("/real/other-tool"),
            Path::new("/bin/macEverything"),
        )
        .unwrap();
        let m = fs.stat(Path::new("/bin/macEverything")).unwrap();
        assert!(
            m.is_file && !m.is_symlink,
            "普通文件覆盖后 stat 不得再跟随旧链接目标"
        );
        assert_eq!((m.dev, m.ino), (8, 8));
    }
}

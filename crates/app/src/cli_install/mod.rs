//! CLI 安装器(v2.5 Phase B:把 `macEverything` 命令行工具装进用户 PATH)。
//!
//! 只承载纯逻辑与进程封装(可单测):PATH 探测([`path_env`])、PATH 条目
//! 分类([`classify`])、安全安装动作([`actions`]);设置页 UI 在 Task 8 接线。
//!
//! 文件系统访问统一收敛到 [`InstallerFs`]:生产用 [`RealFs`](std + libc),
//! 测试用 [`test_fs::FakeFs`](内存模型 + 故障注入)。

pub mod actions;
pub mod classify;
pub mod path_env;

#[cfg(test)]
pub(crate) mod test_fs;

#[allow(unused_imports)]
// Task 8 接线后移除(重导出供设置页取分类类型与 PathSource,当前无消费点)
pub use classify::{Availability, EntryState, Observation};
#[allow(unused_imports)]
pub use path_env::PathSource;

use std::io;
use std::path::{Path, PathBuf};

/// `lstat`/`stat` 归一后的条目元数据(只留分类所需字段)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    pub dev: u64,
    pub ino: u64,
    /// 文件字节数(设计 §5.0-C:随包 CLI 必须非空)。
    pub len: u64,
    pub is_file: bool,
    pub is_symlink: bool,
    pub mode: u32,
}

/// `statvfs` 归一后的观测(目前只关心只读位)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatFs {
    pub read_only: bool,
}

/// 文件系统抽象:分类与动作只依赖本特征,单测注入内存实现。
pub trait InstallerFs {
    /// 不跟随符号链接。
    fn lstat(&self, p: &Path) -> io::Result<Meta>;
    /// 跟随符号链接。
    fn stat(&self, p: &Path) -> io::Result<Meta>;
    fn read_link(&self, p: &Path) -> io::Result<PathBuf>;
    /// `access(2)` `X_OK`。
    fn access_x(&self, p: &Path) -> bool;
    /// `access(2)` `W_OK`(用于动作前的父目录可写预判)。
    fn access_w(&self, p: &Path) -> bool;
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    fn remove_file(&self, p: &Path) -> io::Result<()>;
    fn statfs(&self, p: &Path) -> io::Result<StatFs>;
}

/// 真实文件系统实现(std + libc)。
pub struct RealFs;

impl RealFs {
    pub fn new() -> Self {
        Self
    }
}

impl Default for RealFs {
    fn default() -> Self {
        Self::new()
    }
}

impl InstallerFs for RealFs {
    fn lstat(&self, p: &Path) -> io::Result<Meta> {
        let m = std::fs::symlink_metadata(p)?;
        Ok(meta(&m))
    }
    fn stat(&self, p: &Path) -> io::Result<Meta> {
        let m = std::fs::metadata(p)?;
        Ok(meta(&m))
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        std::fs::read_link(p)
    }
    fn access_x(&self, p: &Path) -> bool {
        access(p, libc::X_OK)
    }
    fn access_w(&self, p: &Path) -> bool {
        access(p, libc::W_OK)
    }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }
    fn remove_file(&self, p: &Path) -> io::Result<()> {
        std::fs::remove_file(p)
    }
    fn statfs(&self, p: &Path) -> io::Result<StatFs> {
        let c = cstr(p);
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: statvfs(3) 只读传入的 NUL 结尾路径,并把结果写入我们提供的结构体。
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // macOS 的 f_flag 是 u_long;MNT_RDONLY 在低位,直接按 u_long 位与,
        // 不经 i32 转换(高位置位的 flag 值转 i32 会变负)。
        Ok(StatFs {
            read_only: s.f_flag & libc::MNT_RDONLY as libc::c_ulong != 0,
        })
    }
}

fn meta(m: &std::fs::Metadata) -> Meta {
    use std::os::unix::fs::MetadataExt;
    Meta {
        dev: m.dev(),
        ino: m.ino(),
        len: m.len(),
        is_file: m.is_file(),
        is_symlink: m.file_type().is_symlink(),
        mode: m.mode(),
    }
}

/// 路径转 NUL 结尾字节串。文件系统路径与环境变量值都不可能含 NUL
/// (两者都以 NUL 结尾传递),故 expect 是安全的。
fn cstr(p: &Path) -> std::ffi::CString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(p.as_os_str().as_bytes()).expect("路径不含 NUL")
}

fn access(p: &Path, mode: libc::c_int) -> bool {
    // SAFETY: access(2) 只读路径权限位,不涉及内存安全。
    unsafe { libc::access(cstr(p).as_ptr(), mode) == 0 }
}

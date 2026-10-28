//! PATH 探测(GUI 安装「命令行工具」时确定该写入哪个 PATH):按回落顺序
//! 经登录交互 shell(`$SHELL -lic 'command env'`)→ 登录 shell(`-lc`)→
//! 当前进程环境取 PATH。只做字节级解析,不做 shell 语义解释。
//!
//! shell 执行抽象成 [`CommandRunner`] 以便单测注入;真实执行 [`RealRunner`]
//! 以独立进程组跑(超时可整组杀掉),不写单测(会真的 spawn shell),
//! 正确性由 Task 9 实机验收覆盖。

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

/// PATH 的来源(按探测回落顺序;Failed 表示三路全空)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSource {
    /// 登录 + 交互 shell(`-lic`)。
    LoginInteractive,
    /// 仅登录 shell(`-lc`)。
    LoginOnly,
    /// 当前进程环境变量。
    ProcessEnv,
    /// 全部回落失败。
    Failed,
}

/// shell 执行抽象(测试注入输出序列的 Fake;生产用 [`RealRunner`])。
pub trait CommandRunner {
    /// 运行 `$SHELL <flag> 'command env'`,返回 stdout;超时/失败返回 None。
    fn run(&self, shell: &Path, flag: &str, timeout: Duration) -> Option<Vec<u8>>;
}

/// 只认最后一个完整 `PATH=` 行;空值不算。
///
/// 输出是原始字节(PATH 值允许非 UTF-8),按字节切行、按字节保留。
pub fn parse_path_env(bytes: &[u8]) -> Option<OsString> {
    let mut found = None;
    for line in bytes.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if let Some(v) = line.strip_prefix(b"PATH=") {
            if !v.is_empty() {
                found = Some(std::ffi::OsStr::from_bytes(v).to_os_string());
            }
        }
    }
    found
}

/// 按回落顺序探测 PATH:`-lic` → `-lc` → 进程环境。
///
/// 每路 shell 至多调一次:某一路给出非空 PATH 即刻返回,不再往下走。
pub fn probe_path_env(
    runner: &dyn CommandRunner,
    process_path: Option<&str>,
) -> (PathSource, Option<OsString>) {
    let shell = std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/bin/zsh".into());
    let timeout = Duration::from_secs(3);
    if let Some(out) = runner.run(&shell, "-lic", timeout) {
        if let Some(p) = parse_path_env(&out) {
            return (PathSource::LoginInteractive, Some(p));
        }
    }
    if let Some(out) = runner.run(&shell, "-lc", timeout) {
        if let Some(p) = parse_path_env(&out) {
            return (PathSource::LoginOnly, Some(p));
        }
    }
    match process_path {
        Some(p) if !p.trim().is_empty() => (PathSource::ProcessEnv, Some(OsString::from(p))),
        _ => (PathSource::Failed, None),
    }
}

/// 真实 shell 执行:独立进程组 spawn(超时可整组杀掉,防 dotfile 里挂起
/// 的交互命令拖死安装流程)。stdout 由独立线程读取,上限 64 KiB。
/// 不写单测(会真的 spawn shell);正确性由 Task 9 实机验收覆盖。
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn run(&self, shell: &Path, flag: &str, timeout: Duration) -> Option<Vec<u8>> {
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new(shell)
            .arg(flag)
            .arg("command env")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .process_group(0)
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            while buf.len() < 64 * 1024 {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            buf
        });
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() < timeout => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                _ => {
                    // 超时/等待失败:SIGTERM 宽限 200ms 仍不退则 SIGKILL 整组。
                    crate::platform::kill_process_group(child.id(), libc::SIGTERM);
                    let grace = std::time::Instant::now();
                    while child.try_wait().ok().flatten().is_none()
                        && grace.elapsed() < Duration::from_millis(200)
                    {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    crate::platform::kill_process_group(child.id(), libc::SIGKILL);
                    let _ = child.wait();
                    break;
                }
            }
        }
        reader.join().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    use std::time::Duration;

    struct FakeRunner {
        outputs: Vec<Option<&'static [u8]>>,
        cursor: Cell<usize>,
    }

    impl FakeRunner {
        fn new(outputs: &[Option<&'static [u8]>]) -> Self {
            Self {
                outputs: outputs.to_vec(),
                cursor: Cell::new(0),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, _shell: &Path, _flag: &str, _timeout: Duration) -> Option<Vec<u8>> {
            let i = self.cursor.get();
            self.cursor.set(i + 1);
            self.outputs.get(i).copied().flatten().map(|b| b.to_vec())
        }
    }

    #[test]
    fn last_complete_path_line_wins_and_noise_is_ignored() {
        let out = b"hello\nPATH=/a:/b\nMANPATH=/x\nPATH=/c:/d\r\n";
        assert_eq!(parse_path_env(out), Some(std::ffi::OsString::from("/c:/d")));
    }

    #[test]
    fn empty_or_truncated_path_is_ignored() {
        assert_eq!(parse_path_env(b"PATH=\n"), None);
        assert_eq!(
            parse_path_env(b"PATH=/a:/b"),
            Some(std::ffi::OsString::from("/a:/b"))
        );
        assert_eq!(
            parse_path_env(b"PATH=/a:/b\nPATH="),
            Some(std::ffi::OsString::from("/a:/b"))
        );
    }

    #[test]
    fn provenance_falls_back_in_order() {
        let r = FakeRunner::new(&[Some(b"PATH=/a\n".as_slice()), Some(b"PATH=/b\n".as_slice())]);
        assert_eq!(
            probe_path_env(&r, Some("/proc:/bin")),
            (PathSource::LoginInteractive, Some("/a".into()))
        );
        let r = FakeRunner::new(&[None, Some(b"PATH=/b\n".as_slice())]);
        assert_eq!(
            probe_path_env(&r, Some("/proc:/bin")),
            (PathSource::LoginOnly, Some("/b".into()))
        );
        let r = FakeRunner::new(&[None, None]);
        assert_eq!(
            probe_path_env(&r, Some("/proc:/bin")),
            (PathSource::ProcessEnv, Some("/proc:/bin".into()))
        );
    }

    /// 突变敏感:`-lic` 已给出结果时不得再回落 `-lc`(调用次数钉死为 1;
    /// 只断言结果值拦不住「多跑一次 -lc 覆盖前值」这类实现走样)。
    #[test]
    fn lic_success_does_not_fall_back_to_lc() {
        let r = FakeRunner::new(&[Some(b"PATH=/a\n".as_slice())]);
        let (source, path) = probe_path_env(&r, Some("/proc:/bin"));
        assert_eq!(source, PathSource::LoginInteractive);
        assert_eq!(path, Some("/a".into()));
        assert_eq!(r.cursor.get(), 1, "-lic 成功后不应再调用 -lc");
    }

    /// 非 UTF-8 字节:不 panic 且按原始字节保留(PATH 值允许任意字节)。
    #[test]
    fn non_utf8_path_bytes_preserved() {
        let parsed = parse_path_env(b"PATH=/a\xff:/b\n").expect("非 UTF-8 也应解析出值");
        assert_eq!(parsed.as_bytes(), b"/a\xff:/b");
    }
}

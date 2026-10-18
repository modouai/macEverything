//! 引擎定位与拉起(启动编排第二步)。
//!
//! 定位三档:① App 可执行文件同目录(随包 `Contents/MacOS/macEverything`)
//! ② PATH 各段 ③ 均失败 → 错误消息逐行列全部搜索过的路径(product §5 / tech
//! 「进程编排」)。拉起 = spawn 引擎 CLI 的 `start`(CLI 自带后台化,再 spawn
//! 自身驻留;App 只需轮询 daemon.sock 连通)。
//!
//! 纯逻辑(候选生成 / 错误文案)在本文件测;spawn 与轮询由实机冒烟验证。

use anyhow::Result;
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::i18n;

/// 拉起后连通轮询:总超时与间隔(tech「进程编排」:≤3s,250ms 间隔)。
const POLL_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// 引擎可执行文件名(与 CLI bin 名一致)。
const ENGINE_BIN: &str = "macEverything";

/// 拉起失败(用户可见)。原始细节(OS 错误、socket/日志路径)只进日志，
/// 变体只保留本地化展示所需的信息。
#[derive(Debug)]
pub enum LaunchError {
    NotFound { tried: Vec<PathBuf> },
    LocateApp,
    Start,
    Timeout,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { tried } => {
                let paths = tried
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                write!(f, "{}\n{paths}", i18n::s().engine_not_found)?;
                write!(f, "\n{}", i18n::s().engine_not_found_hint)
            }
            Self::LocateApp => f.write_str(i18n::s().engine_locate_app_failed),
            Self::Start => f.write_str(i18n::s().engine_start_failed),
            Self::Timeout => f.write_str(i18n::s().engine_start_timeout),
        }
    }
}

impl std::error::Error for LaunchError {}

/// 定位候选(顺序):① exe_dir/macEverything ② PATH 各段/macEverything。
/// 去重按出现顺序保首;空 PATH 段(POSIX 语义 = 当前目录)跳过——进程 cwd
/// 不固定,相对路径候选无意义。
pub fn engine_candidates(exe_dir: &Path, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let mut v = vec![exe_dir.join(ENGINE_BIN)];
    if let Some(p) = path_var {
        for seg in std::env::split_paths(p) {
            if seg.as_os_str().is_empty() {
                continue;
            }
            let c = seg.join(ENGINE_BIN);
            if !v.contains(&c) {
                v.push(c);
            }
        }
    }
    v
}

/// 全失败错误消息:每条候选路径单独一行(顺序与搜索顺序一致)。
/// 仅测试用；生产路径直接构造 [`LaunchError::NotFound`]。
#[cfg(test)]
pub fn no_engine_found_msg(candidates: &[PathBuf]) -> String {
    LaunchError::NotFound {
        tried: candidates.to_vec(),
    }
    .to_string()
}

/// 「存在且可执行」:普通文件(符号链接解析后)+ 任一执行位。
fn is_executable(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// 拉起引擎,直到 daemon.sock 可连:
/// ① 先 connect 探测(已在跑直接 Ok);
/// ② 候选逐个「存在且可执行」检查,取首个;
/// ③ spawn `<候选> start`(继承环境:HOME/MCE_ROOTS 自然传递;stdio 接 null);
/// ④ 轮询连通 ≤3s。
/// 全失败 → Err(消息含全部候选路径逐行)。
pub fn ensure_engine(paths: &client::DaemonPaths) -> Result<()> {
    if client::connect(paths).is_some() {
        log::info!("引擎已在运行,跳过拉起");
        return Ok(());
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            log::error!("定位 App 可执行文件失败: {err:#}");
            return Err(LaunchError::LocateApp.into());
        }
    };
    let exe_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let path_var = std::env::var_os("PATH");
    let candidates = engine_candidates(&exe_dir, path_var.as_deref());

    let Some(cand) = candidates.iter().find(|c| is_executable(c)) else {
        log::error!(
            "未找到引擎可执行文件(macEverything),已搜索 {} 个路径",
            candidates.len()
        );
        return Err(LaunchError::NotFound { tried: candidates }.into());
    };
    log::info!("拉起引擎: {}", cand.display());

    let mut child = match std::process::Command::new(cand)
        .arg("start")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            log::error!("启动引擎失败({}): {err:#}", cand.display());
            return Err(LaunchError::Start.into());
        }
    };
    // CLI `start` 后台化(再 spawn 自身驻留)后很快退出;收尸线程避免留僵尸,
    // 不参与就绪判定(就绪以 socket 轮询为准)。
    std::thread::spawn(move || match child.wait() {
        Ok(st) if !st.success() => log::warn!("引擎 CLI 退出异常({st}),见数据目录 daemon.log"),
        Ok(_) => {}
        Err(e) => log::warn!("等待引擎 CLI 退出失败: {e}"),
    });

    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        if client::connect(paths).is_some() {
            log::info!("引擎已就绪");
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    log::error!(
        "引擎进程已拉起({}),但 {} 秒内未能连接引擎 socket({});请查看 {} 了解启动失败原因",
        cand.display(),
        POLL_TIMEOUT.as_secs(),
        paths.socket().display(),
        paths.log_file().display()
    );
    Err(LaunchError::Timeout.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands(exe_dir: &str, path: Option<&str>) -> Vec<PathBuf> {
        engine_candidates(Path::new(exe_dir), path.map(OsStr::new))
    }

    #[test]
    fn candidates_bundled_dir_first_then_path_segments_in_order() {
        let got = cands("/opt/mce", Some("/usr/bin:/bin"));
        assert_eq!(
            got,
            vec![
                PathBuf::from("/opt/mce/macEverything"),
                PathBuf::from("/usr/bin/macEverything"),
                PathBuf::from("/bin/macEverything"),
            ]
        );
    }

    #[test]
    fn candidates_dedup_keep_first_position() {
        // exe_dir 与 PATH 段重合、PATH 段自身重复:各只保留首次出现
        let got = cands("/usr/bin", Some("/usr/bin:/usr/bin:/opt/bin"));
        assert_eq!(
            got,
            vec![
                PathBuf::from("/usr/bin/macEverything"),
                PathBuf::from("/opt/bin/macEverything"),
            ]
        );
    }

    #[test]
    fn candidates_empty_or_absent_path_var() {
        let only_bundled = vec![PathBuf::from("/opt/mce/macEverything")];
        assert_eq!(cands("/opt/mce", None), only_bundled);
        // 空 PATH(POSIX 空段 = 当前目录):跳过空段,不产出相对路径候选
        assert_eq!(cands("/opt/mce", Some("")), only_bundled);
        assert_eq!(cands("/opt/mce", Some(":")), only_bundled);
    }

    #[test]
    fn no_engine_msg_lists_every_candidate_on_own_line_in_order() {
        let cs = vec![
            PathBuf::from("/a/bin/macEverything"),
            PathBuf::from("/b/bin/macEverything"),
            PathBuf::from("/c/bin/macEverything"),
        ];
        let msg = no_engine_found_msg(&cs);
        let lines: Vec<&str> = msg.lines().collect();
        let mut prev: Option<usize> = None;
        for c in &cs {
            let s = c.display().to_string();
            let idx = lines
                .iter()
                .position(|l| l.trim_start() == s)
                .unwrap_or_else(|| panic!("候选 {s} 未单独成行:\n{msg}"));
            if let Some(p) = prev {
                assert!(idx > p, "候选行顺序与搜索顺序不一致:\n{msg}");
            }
            prev = Some(idx);
        }
        // 可操作提示(文案契约:告诉你怎么办)
        assert!(msg.contains("PATH"), "缺可操作提示:\n{msg}");
    }

    /// LaunchError 的 Display 随语言切换；NotFound 保留全部候选路径与可操作提示。
    #[test]
    fn launch_error_display_is_localized() {
        let tried = vec![
            PathBuf::from("/a/bin/macEverything"),
            PathBuf::from("/b/bin/macEverything"),
        ];
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let msg = LaunchError::NotFound {
                tried: tried.clone(),
            }
            .to_string();
            assert!(msg.contains("未找到引擎可执行文件"), "{msg}");
            assert!(msg.contains("/a/bin/macEverything"), "{msg}");
            assert!(msg.contains("/b/bin/macEverything"), "{msg}");
            assert!(msg.contains("或加入 PATH 后重试"), "{msg}");
            assert_eq!(
                LaunchError::LocateApp.to_string(),
                "定位 App 可执行文件失败"
            );
            assert_eq!(LaunchError::Start.to_string(), "启动引擎失败");
            assert_eq!(
                LaunchError::Timeout.to_string(),
                "引擎进程已拉起,但未能连接引擎 socket"
            );
        });
        i18n::with_lang(i18n::Lang::En, || {
            let msg = LaunchError::NotFound {
                tried: tried.clone(),
            }
            .to_string();
            assert!(
                msg.contains("Could not find the macEverything engine. Searched:"),
                "{msg}"
            );
            assert!(msg.contains("/a/bin/macEverything"), "{msg}");
            assert!(msg.contains("/b/bin/macEverything"), "{msg}");
            assert!(
                msg.contains("Place macEverything next to the app or add it to PATH"),
                "{msg}"
            );
            assert_eq!(
                LaunchError::LocateApp.to_string(),
                "Could not locate the app executable"
            );
            assert_eq!(LaunchError::Start.to_string(), "Failed to start the engine");
            assert_eq!(
                LaunchError::Timeout.to_string(),
                "The engine process started but did not accept connections"
            );
        });
    }
}

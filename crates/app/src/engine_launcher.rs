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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::i18n;

/// 拉起后连通轮询:总超时与间隔(tech「进程编排」:≤3s,250ms 间隔)。
const POLL_TIMEOUT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// daemon 存活监控:可见期探测间隔。连续 2 次不可连才判定退出(单次抖动不拉起)。
const MONITOR_INTERVAL: Duration = Duration::from_secs(5);
/// 四窗全隐期探测间隔:界面不活跃,崩溃晚 30s 发现无感;减少唤醒与 daemon 连接。
const HIDDEN_MONITOR_INTERVAL: Duration = Duration::from_secs(30);
const MONITOR_FAIL_THRESHOLD: u32 = 2;

/// GUI 当前是否「四窗全隐」。平台层在显隐边沿更新(mac.rs):
/// 缩窗定时器确认四窗全隐 → true;任何显示路径(`cancel_surface_shrink`)→ false。
/// 启动即四窗全隐(产品形态),故默认 true。
static UI_HIDDEN: AtomicBool = AtomicBool::new(true);

/// 平台层写入全隐状态(主线程;原子写,无锁)。
pub(crate) fn set_ui_hidden(hidden: bool) {
    UI_HIDDEN.store(hidden, Ordering::Relaxed);
}

fn ui_hidden() -> bool {
    UI_HIDDEN.load(Ordering::Relaxed)
}

/// 当前探测间隔(纯函数,单测钉住):全隐 30s,可见 5s。
fn monitor_interval(hidden: bool) -> Duration {
    if hidden {
        HIDDEN_MONITOR_INTERVAL
    } else {
        MONITOR_INTERVAL
    }
}

/// GUI 是否「期望 daemon 运行」。启动拉起成功 / 托盘手动拉起成功后置 true;
/// 用户主动停止(手动重启的 stop 段)或启动时就是离线态则保持 false —— 离线
/// 是用户可见的合法状态,监控不得擅自把 daemon 拉回来。
static EXPECTED_RUNNING: AtomicBool = AtomicBool::new(false);
/// 用户主动 stop/重启期间的暂停闸:期间探测失败不计入、不触发拉起。
static MONITOR_SUSPENDED: AtomicBool = AtomicBool::new(false);

pub fn set_expected_running(expected: bool) {
    EXPECTED_RUNNING.store(expected, Ordering::SeqCst);
}

pub fn expected_running() -> bool {
    EXPECTED_RUNNING.load(Ordering::SeqCst)
}

/// 暂停存活监控(RAII):手动「重启引擎」的 stop 窗口不得被监控当成崩溃。
/// Drop 自动恢复;直接用 `suspend_monitor()/resume_monitor()` 易漏配平。
pub struct HealthMonitorPause;

impl Drop for HealthMonitorPause {
    fn drop(&mut self) {
        MONITOR_SUSPENDED.store(false, Ordering::SeqCst);
    }
}

pub fn pause_health_monitor() -> HealthMonitorPause {
    MONITOR_SUSPENDED.store(true, Ordering::SeqCst);
    HealthMonitorPause
}

/// 连续失败计数与重启判定(纯逻辑,单测钉住不依赖线程/时钟)。
#[derive(Default)]
struct LivenessTracker {
    failures: u32,
}

impl LivenessTracker {
    /// 返回本轮是否应拉起。`expected=false`(离线/用户停止)、`suspended`
    /// (手动重启窗口)或 `stop_intent`(用户显式 CLI stop 的标记)一律清零
    /// 且不触发。
    fn observe(
        &mut self,
        connectable: bool,
        expected: bool,
        suspended: bool,
        stop_intent: bool,
    ) -> bool {
        if connectable || !expected || suspended || stop_intent {
            self.failures = 0;
            return false;
        }
        self.failures = self.failures.saturating_add(1);
        self.failures >= MONITOR_FAIL_THRESHOLD
    }

    fn reset(&mut self) {
        self.failures = 0;
    }
}

/// 启动 daemon 存活监控线程(可见期每 5s、全隐期每 30s 发一次 Ping 探测)。
/// 判定退出 → `ensure_engine` 拉起;成功后调用 `on_restarted`(主线程外的 UI
/// 清离线提示/重查入口,由调用方自行切回 UI 锁)。
///
/// 探测必须用 [`client::ping`](真实 Ping/Pong 请求)而不是 connect 后立即断开:
/// 后者在 daemon 侧表现为「读了 0 字节就 EOF」,曾以 WARN 刷屏(5s × 全天 ≈
/// 1.7 万行),且探测连接与 Stop 并发时有截断停机落盘的风险。
pub fn spawn_health_monitor<F>(paths: client::DaemonPaths, on_restarted: F)
where
    F: Fn() + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name("daemon-health".to_owned())
        .spawn(move || {
            let mut tracker = LivenessTracker::default();
            let mut last_stop_intent = false;
            loop {
                // 每轮重新取间隔:全隐/可见切换在下一轮生效(最迟一个周期)。
                let interval = monitor_interval(ui_hidden());
                std::thread::sleep(interval);
                let connectable = client::ping(&paths);
                let expected = expected_running();
                let suspended = MONITOR_SUSPENDED.load(Ordering::SeqCst);
                // CLI stop 成功后的意图标记:用户主动停止不是崩溃,不拉起。
                // 标记在 CLI start 时清除,届时监控自动恢复。
                let stop_intent = paths.stop_intent_file().exists();
                if stop_intent != last_stop_intent {
                    if stop_intent {
                        log::info!("检测到用户主动 stop 标记,存活监控暂停自动拉起");
                    } else {
                        log::info!("stop 标记已清除,存活监控恢复");
                    }
                    last_stop_intent = stop_intent;
                }
                if !tracker.observe(connectable, expected, suspended, stop_intent) {
                    if !connectable && expected && !suspended && !stop_intent {
                        log::warn!(
                            "daemon 存活探测失败({}/{MONITOR_FAIL_THRESHOLD}),继续观察",
                            tracker.failures
                        );
                    }
                    continue;
                }
                // 进入拉起前再复核暂停闸:用户可能恰在探测后发起手动重启。
                if MONITOR_SUSPENDED.load(Ordering::SeqCst) {
                    tracker.reset();
                    continue;
                }
                log::error!(
                    "daemon 连续 {MONITOR_FAIL_THRESHOLD} 次不可连,判定已退出,自动拉起引擎"
                );
                match ensure_engine(&paths) {
                    Ok(()) => {
                        log::info!("daemon 存活监控:自动重启完成,清 UI 离线提示并重查");
                        tracker.reset();
                        on_restarted();
                    }
                    Err(err) => {
                        log::error!("daemon 存活监控:自动重启失败: {err:#}");
                        tracker.reset();
                    }
                }
            }
        });
    if let Err(err) = spawned {
        log::error!("daemon 存活监控线程启动失败: {err}");
    }
}

/// 引擎可执行文件名(与 CLI bin 名一致)。
const ENGINE_BIN: &str = "macEverything";

/// 拉起失败(用户可见)。原始细节(OS 错误、socket/日志路径)只进日志，
/// 变体只保留本地化展示所需的信息。
#[derive(Debug)]
pub enum LaunchError {
    NotFound { tried: Vec<PathBuf> },
    LocateApp,
    Start,
    Timeout { log: PathBuf },
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
            Self::Timeout { log } => {
                write!(f, "{}: ", i18n::s().engine_start_timeout)?;
                let log = log.display().to_string();
                f.write_str(&i18n::fill(
                    i18n::s().engine_start_timeout_hint,
                    &[("log", log.as_str())],
                ))
            }
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

/// 组装引擎 spawn 命令:显式固定 MCE_DATA_DIR——App 固定默认目录,
/// 外部环境(终端启动/launchctl setenv)若带同名变量,会让子 daemon 绑到
/// 自定义目录而 App 仍连默认 socket,启动必超时(设计 §4.5)。
fn engine_command(cand: &Path, paths: &client::DaemonPaths) -> std::process::Command {
    let mut c = std::process::Command::new(cand);
    c.arg("start")
        .env("MCE_DATA_DIR", &paths.data_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    c
}

/// 拉起引擎,直到 daemon.sock 可连:
/// ① 先 connect 探测(已在跑直接 Ok);
/// ② 候选逐个「存在且可执行」检查,取首个;
/// ③ spawn `<候选> start`(继承环境:HOME/MCE_ROOTS 自然传递;stdio 接 null);
/// ④ 轮询连通 ≤3s。
/// 全失败 → Err(消息含全部候选路径逐行)。
pub fn ensure_engine(paths: &client::DaemonPaths) -> Result<()> {
    if client::connect(paths).is_some() {
        // 版本偏差自动重启(升级路径):引擎常驻不随 App 退出,App 升级后老引擎
        // 仍在——新协议能力(如卷偏好)会整体不可用。daemon_version 由新引擎
        // 上报;空(旧引擎无此字段)或不等(App 与引擎须同源同版)即重启。
        // 重启先落盘再退出(stop 语义),随后走下方常规拉起。
        let skew = match client::daemon_version(paths) {
            Some(v) if v == env!("CARGO_PKG_VERSION") => {
                log::info!("引擎已在运行,版本匹配({v}),跳过拉起");
                return Ok(());
            }
            other => other,
        };
        log::warn!(
            "引擎版本偏差(运行 {skew:?},App {}),自动重启引擎",
            env!("CARGO_PKG_VERSION")
        );
        if let Err(err) = client::stop(paths) {
            log::warn!("重启引擎:停止旧引擎失败({err:#}),仍尝试拉起");
        }
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

    let mut child = match engine_command(cand, paths).spawn() {
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
    log::warn!(
        "引擎进程已拉起({}),但 {} 秒内未能连接引擎 socket({});请查看 {} 了解启动失败原因",
        cand.display(),
        POLL_TIMEOUT.as_secs(),
        paths.socket().display(),
        paths.log_file().display()
    );
    Err(LaunchError::Timeout {
        log: paths.log_file(),
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cands(exe_dir: &str, path: Option<&str>) -> Vec<PathBuf> {
        engine_candidates(Path::new(exe_dir), path.map(OsStr::new))
    }

    /// 存活判定钉:单次不可连不拉起(抖动容忍),连续 2 次才触发;任何一次
    /// 可连即清零。突变「阈值改 1」→ 首次断言红。
    #[test]
    fn liveness_tracker_restarts_only_after_consecutive_failures() {
        let mut t = LivenessTracker::default();
        assert!(
            !t.observe(false, true, false, false),
            "第 1 次不可连:仅观察"
        );
        assert!(
            t.observe(false, true, false, false),
            "第 2 次不可连:触发拉起"
        );
        t.reset();
        assert!(!t.observe(false, true, false, false));
        assert!(!t.observe(true, true, false, false), "可连即清零");
        assert!(!t.observe(false, true, false, false), "清零后重新从 1 计数");
        assert!(t.observe(false, true, false, false));
    }

    /// 离线态 / 用户主动 stop / 手动重启窗口:探测失败也不得拉起,且计数清零
    /// (否则用户停止后监控会擅自把 daemon 拉回来)。突变「忽略 expected」→ 红。
    #[test]
    fn liveness_tracker_respects_expected_and_suspend() {
        let mut t = LivenessTracker::default();
        assert!(
            !t.observe(false, false, false, false),
            "离线态(期望不运行):不拉起"
        );
        assert!(!t.observe(false, false, false, false));
        assert!(!t.observe(false, true, true, false), "手动重启窗口:不拉起");
        assert!(!t.observe(false, true, false, false), "恢复后重新从 1 计数");
        assert!(
            t.observe(false, true, false, false),
            "期望运行 + 连续 2 次才触发"
        );
    }

    /// CLI stop 意图标记:用户主动停止后监控不得拉起(即使期望位仍为真)。
    /// 突变「忽略 stop_intent」→ 第二次断言红。
    #[test]
    fn liveness_tracker_respects_user_stop_intent() {
        let mut t = LivenessTracker::default();
        assert!(!t.observe(false, true, false, true), "stop 意图:不拉起");
        assert!(
            !t.observe(false, true, false, true),
            "stop 意图期间计数清零"
        );
        assert!(!t.observe(false, true, false, false), "标记清除后从 1 计数");
        assert!(t.observe(false, true, false, false), "恢复后连续 2 次触发");
    }

    /// 探测间隔:可见期 5s、全隐期退避到 30s。突变「恒 5s / 恒 30s」即红。
    #[test]
    fn monitor_interval_backs_off_when_all_hidden() {
        assert_eq!(monitor_interval(false), Duration::from_secs(5));
        assert_eq!(monitor_interval(true), Duration::from_secs(30));
    }

    #[test]
    fn expected_running_is_process_wide_flag() {
        // 仅验证 setter/getter 语义;不并行断言时序(其他测试可能读写同一全局)。
        set_expected_running(true);
        assert!(expected_running());
        set_expected_running(false);
        assert!(!expected_running());
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

    /// spawn 命令钉:显式固定 MCE_DATA_DIR——外部环境(终端启动/launchctl
    /// setenv)若带同名变量,会让子 daemon 绑到自定义目录而 App 仍连默认
    /// socket,启动必超时(设计 §4.5)。突变「漏设 env」此测必红。
    #[test]
    fn engine_command_pins_data_dir() {
        let paths = client::DaemonPaths {
            data_dir: PathBuf::from("/tmp/app-data"),
        };
        let cmd = engine_command(Path::new("/tmp/macEverything"), &paths);
        let env: Vec<_> = cmd.get_envs().collect();
        assert!(env.iter().any(|(k, v)| {
            *k == OsStr::new("MCE_DATA_DIR") && *v == Some(OsStr::new("/tmp/app-data"))
        }));
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
        let log = PathBuf::from("/tmp/mce/daemon.log");
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
                LaunchError::Timeout { log: log.clone() }.to_string(),
                "引擎进程已拉起,但未能连接引擎 socket: 请查看 /tmp/mce/daemon.log 了解启动失败原因"
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
                LaunchError::Timeout { log: log.clone() }.to_string(),
                "The engine process started but did not accept connections: See /tmp/mce/daemon.log for details"
            );
        });
    }
}

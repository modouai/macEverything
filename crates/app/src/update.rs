//! 发现新版本：版本解析/比较、清单判定与检查状态机。
//!
//! 纯逻辑都在本文件；网络与 UI 由上层注入（`platform::fetch_url_bytes` /
//! `ui::spawn_update_check`），因此比较与状态迁移可直接单测。

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

/// 官网版本清单（单一固定地址；开发可用环境变量覆盖）。
pub const MANIFEST_URL: &str = "https://maceverything.modouai.dev/version.json";
/// 当前编译版本（与 Cargo.toml / CFBundleShortVersionString 同一真源）。
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 请求超时。
///
/// 不变量：平台侧抓取的总等待上限为 `REQUEST_TIMEOUT + 2s`（12 秒），必须小于
/// [`WATCHDOG`]；否则合法的慢请求会在结果返回前被看门狗判超时并丢弃。
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 无回调看门狗（UI 侧兜底，避免状态卡在 Checking）。
///
/// 必须大于平台抓取的 `REQUEST_TIMEOUT + 2s`（见 [`REQUEST_TIMEOUT`]）。
pub const WATCHDOG: Duration = Duration::from_secs(15);
/// 响应体上限。
pub const MAX_BODY: usize = 64 * 1024;

/// 归一化版本号：`major.minor.patch`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.strip_prefix('v').unwrap_or(raw);
        let mut parts = raw.split('.');
        let major = parse_version_part(parts.next(), "major")?;
        let minor = parse_version_part(parts.next(), "minor")?;
        let patch = parse_version_part(parts.next(), "patch")?;
        if parts.next().is_some() {
            return Err(format!("版本号段数过多: {raw:?}"));
        }
        Ok(Self {
            major,
            minor,
            patch,
        })
    }

    pub fn display(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

fn parse_version_part(part: Option<&str>, name: &str) -> Result<u64, String> {
    let part = part.ok_or_else(|| format!("版本号缺少 {name}"))?;
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("版本号 {name} 非法: {part:?}"));
    }
    part.parse::<u64>()
        .map_err(|_| format!("版本号 {name} 溢出: {part:?}"))
}

/// 更新检查阶段（UI 唯一状态源）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Available { version: String },
    Failed,
}

/// 一次检查的结果（工作线程 → 状态机）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate,
    Available(String),
    Failed(String),
}

/// 检查状态机：同一时刻至多一个在途请求，15 秒无结果自动失败。
#[derive(Debug)]
pub struct UpdateState {
    phase: UpdatePhase,
    rx: Option<Receiver<CheckOutcome>>,
    started_at: Option<Instant>,
}

impl UpdateState {
    pub fn new() -> Self {
        Self {
            phase: UpdatePhase::Idle,
            rx: None,
            started_at: None,
        }
    }

    pub fn phase(&self) -> &UpdatePhase {
        &self.phase
    }

    pub fn begin(&mut self) -> Option<Sender<CheckOutcome>> {
        self.begin_at(Instant::now())
    }

    pub fn begin_at(&mut self, now: Instant) -> Option<Sender<CheckOutcome>> {
        if self.rx.is_some() {
            return None;
        }
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        self.started_at = Some(now);
        self.phase = UpdatePhase::Checking;
        Some(tx)
    }

    pub fn tick(&mut self) -> bool {
        self.tick_at(Instant::now())
    }

    pub fn tick_at(&mut self, now: Instant) -> bool {
        if self.rx.is_none() {
            return false;
        }
        // 看门狗先于收结果：超过 15 秒的迟到结果一律丢弃。
        if self
            .started_at
            .is_some_and(|started| now.saturating_duration_since(started) >= WATCHDOG)
        {
            self.rx = None;
            self.started_at = None;
            self.phase = UpdatePhase::Failed;
            log::warn!("检查更新超时({} 秒无结果)", WATCHDOG.as_secs());
            return true;
        }
        let Some(rx) = self.rx.take() else {
            return false;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.started_at = None;
                self.phase = match outcome {
                    CheckOutcome::UpToDate => UpdatePhase::UpToDate,
                    CheckOutcome::Available(version) => UpdatePhase::Available { version },
                    CheckOutcome::Failed(detail) => {
                        log::warn!("检查更新失败: {detail}");
                        UpdatePhase::Failed
                    }
                };
                true
            }
            Err(TryRecvError::Empty) => {
                self.rx = Some(rx);
                false
            }
            Err(TryRecvError::Disconnected) => {
                self.started_at = None;
                self.phase = UpdatePhase::Failed;
                log::warn!("检查更新工作线程异常退出(无结果)");
                true
            }
        }
    }

    pub fn spawn_failed(&mut self, detail: String) {
        self.rx = None;
        self.started_at = None;
        self.phase = UpdatePhase::Failed;
        log::warn!("检查更新工作线程启动失败: {detail}");
    }
}

impl Default for UpdateState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    version: String,
}

/// 判定清单与当前版本：只有清单严格更新才算 Available。
pub fn evaluate(bytes: &[u8], current: &str) -> CheckOutcome {
    let current = match Version::parse(current) {
        Ok(version) => version,
        Err(err) => return CheckOutcome::Failed(format!("当前版本非法: {err}")),
    };
    let manifest: Manifest = match serde_json::from_slice(bytes) {
        Ok(manifest) => manifest,
        Err(err) => return CheckOutcome::Failed(format!("版本清单解析失败: {err}")),
    };
    let latest = match Version::parse(&manifest.version) {
        Ok(version) => version,
        Err(err) => return CheckOutcome::Failed(format!("清单版本非法: {err}")),
    };
    if latest > current {
        CheckOutcome::Available(latest.display())
    } else {
        CheckOutcome::UpToDate
    }
}

/// 挂机期定时自动检查间隔。
///
/// 启动检查只覆盖一次；常驻形态此后靠本间隔重查。到点时重读
/// `settings.toml` 的 `check_updates`（运行中关闭即时生效），
/// Checking 期到点由 [`UpdateState::begin`] 的防重入闸吞掉。
pub const AUTO_CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// 执行一次真实检查（工作线程调用）：拉取清单并判定。
pub fn fetch_and_evaluate() -> CheckOutcome {
    let url = manifest_url();
    log::info!("检查更新: {url}");
    match crate::platform::fetch_url_bytes(&url, REQUEST_TIMEOUT, MAX_BODY) {
        Ok(bytes) => evaluate(&bytes, CURRENT_VERSION),
        Err(err) => CheckOutcome::Failed(err),
    }
}

/// 清单地址（开发覆盖：`MACEEVERYTHING_UPDATE_URL`）。
pub fn manifest_url() -> String {
    std::env::var("MACEEVERYTHING_UPDATE_URL").unwrap_or_else(|_| MANIFEST_URL.to_owned())
}

/// 官网下载页（按指定界面语言）。
pub fn download_page(lang: crate::i18n::Lang) -> &'static str {
    match lang {
        crate::i18n::Lang::ZhHans => "https://maceverything.modouai.dev/#download",
        crate::i18n::Lang::En => "https://maceverything.modouai.dev/en/#download",
    }
}

/// 用默认浏览器打开官网下载页；失败返回可读错误（调用方决定是否显示）。
pub fn open_download_page() -> Result<(), String> {
    let page = download_page(crate::i18n::lang());
    log::info!("打开下载页: {page}");
    match std::process::Command::new("open").arg(page).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || match child.wait() {
                Ok(st) if !st.success() => log::warn!("open 退出异常({st}):{page}"),
                Ok(_) => {}
                Err(err) => log::warn!("等待 open 退出失败: {err}"),
            });
            Ok(())
        }
        Err(err) => Err(format!("启动 open 失败: {err}")),
    }
}

/// 设置页版本行状态说明。
pub fn status_text(phase: &UpdatePhase) -> String {
    let s = crate::i18n::s();
    match phase {
        UpdatePhase::Idle => s.update_status_idle.to_owned(),
        UpdatePhase::Checking => s.update_status_checking.to_owned(),
        UpdatePhase::UpToDate => s.update_status_up_to_date.to_owned(),
        UpdatePhase::Available { version } => crate::i18n::format::update_available(version),
        UpdatePhase::Failed => s.update_status_failed.to_owned(),
    }
}

/// 设置页版本行动作文案。
pub fn action_text(phase: &UpdatePhase) -> &'static str {
    match phase {
        UpdatePhase::Available { .. } => crate::i18n::s().update_action_download,
        _ => crate::i18n::s().update_action_check,
    }
}

/// 设置页版本行动作是否可点。
pub fn action_enabled(phase: &UpdatePhase) -> bool {
    !matches!(phase, UpdatePhase::Checking)
}

/// 托盘更新项文案。
pub fn tray_text(phase: &UpdatePhase) -> String {
    let s = crate::i18n::s();
    match phase {
        UpdatePhase::Checking => s.tray_checking.to_owned(),
        UpdatePhase::Available { version } => crate::i18n::format::tray_update_available(version),
        _ => s.tray_check_update.to_owned(),
    }
}

/// 托盘更新项是否可点。
pub fn tray_enabled(phase: &UpdatePhase) -> bool {
    !matches!(phase, UpdatePhase::Checking)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 钉住：定时自动检查间隔 = 24h（防手滑改成秒/分级对官网疯狂请求）。
    #[test]
    fn auto_check_interval_is_daily() {
        assert_eq!(AUTO_CHECK_INTERVAL, std::time::Duration::from_secs(86_400));
    }

    #[test]
    fn parses_and_orders_versions() {
        assert_eq!(Version::parse("2.4.0").unwrap().display(), "2.4.0");
        assert_eq!(Version::parse("v2.4.0").unwrap().display(), "2.4.0");
        assert_eq!(Version::parse("02.004.0").unwrap().display(), "2.4.0");
        assert!(Version::parse("2.3.0").unwrap() < Version::parse("2.4.0").unwrap());
        assert!(Version::parse("9.9.9").unwrap() < Version::parse("10.0.0").unwrap());
        assert!(Version::parse("18446744073709551615.0.0").is_ok());
        for bad in [
            "",
            "2.4",
            "2.4.0.1",
            "2.4.x",
            "2.4.0-beta.1",
            "v",
            "1..2",
            "18446744073709551616.0.0",
        ] {
            assert!(Version::parse(bad).is_err(), "{bad:?} 应被拒绝");
        }
    }

    #[test]
    fn current_version_is_parseable() {
        assert!(Version::parse(CURRENT_VERSION).is_ok());
    }

    #[test]
    fn evaluate_compares_manifest_with_current() {
        let current = "2.3.0";
        assert_eq!(
            evaluate(br#"{"version":"2.4.0"}"#, current),
            CheckOutcome::Available("2.4.0".into())
        );
        // 未知字段忽略；v 前缀与前导零归一化后再比较。
        assert_eq!(
            evaluate(br#"{"version":"v02.004.0","extra":1}"#, current),
            CheckOutcome::Available("2.4.0".into())
        );
        assert_eq!(
            evaluate(br#"{"version":"2.3.0"}"#, current),
            CheckOutcome::UpToDate
        );
        assert_eq!(
            evaluate(br#"{"version":"2.2.9"}"#, current),
            CheckOutcome::UpToDate
        );
        assert!(matches!(
            evaluate(br#"{"version":"3"}"#, current),
            CheckOutcome::Failed(_)
        ));
        assert!(matches!(
            evaluate(br#"{"version":2.4}"#, current),
            CheckOutcome::Failed(_)
        ));
        assert!(matches!(
            evaluate(b"not json", current),
            CheckOutcome::Failed(_)
        ));
        assert!(matches!(
            evaluate(br#"{"version":"2.4.0-beta.1"}"#, current),
            CheckOutcome::Failed(_)
        ));
        assert!(matches!(
            evaluate(br#"{"version":"2.4.0"}"#, "bad-current"),
            CheckOutcome::Failed(_)
        ));
    }

    #[test]
    fn state_machine_accepts_one_result_and_rejects_concurrent_checks() {
        let t0 = Instant::now();
        let mut state = UpdateState::new();
        assert_eq!(state.phase(), &UpdatePhase::Idle);

        let tx = state.begin_at(t0).expect("首次检查应开始");
        assert!(state.begin_at(t0).is_none(), "检查中不得并发");
        assert_eq!(state.phase(), &UpdatePhase::Checking);

        tx.send(CheckOutcome::Available("2.4.0".into())).unwrap();
        assert!(state.tick_at(t0 + Duration::from_millis(1)));
        assert_eq!(
            state.phase(),
            &UpdatePhase::Available {
                version: "2.4.0".into()
            }
        );
        assert!(
            !state.tick_at(t0 + Duration::from_secs(1)),
            "无在途请求不变化"
        );
    }

    #[test]
    fn watchdog_fails_after_timeout_and_discards_queued_result() {
        let t0 = Instant::now();
        let mut state = UpdateState::new();
        let tx = state.begin_at(t0).unwrap();

        assert!(!state.tick_at(t0 + Duration::from_secs(14)));
        // 结果在看门狗到点前已入队：tick 必须优先判超时并丢弃它。
        tx.send(CheckOutcome::Available("9.9.9".into())).unwrap();
        assert!(state.tick_at(t0 + WATCHDOG));
        assert_eq!(state.phase(), &UpdatePhase::Failed);

        let _ = tx.send(CheckOutcome::Available("9.9.9".into()));
        assert!(
            !state.tick_at(t0 + Duration::from_secs(16)),
            "超时后忽略迟到结果"
        );
        assert_eq!(state.phase(), &UpdatePhase::Failed);
    }

    #[test]
    fn spawn_failure_and_timeout_allow_retry() {
        let t0 = Instant::now();
        let mut state = UpdateState::new();

        let _tx = state.begin_at(t0).unwrap();
        state.spawn_failed("boom".into());
        assert_eq!(state.phase(), &UpdatePhase::Failed);

        let tx = state
            .begin_at(t0 + Duration::from_secs(1))
            .expect("启动失败后应可重试");
        tx.send(CheckOutcome::UpToDate).unwrap();
        assert!(state.tick_at(t0 + Duration::from_secs(2)));
        assert_eq!(state.phase(), &UpdatePhase::UpToDate);

        let _tx = state.begin_at(t0 + Duration::from_secs(3)).unwrap();
        assert!(state.tick_at(t0 + Duration::from_secs(3) + WATCHDOG));
        assert_eq!(state.phase(), &UpdatePhase::Failed);

        assert!(
            state.begin_at(t0 + Duration::from_secs(20)).is_some(),
            "看门狗超时后应可重试"
        );
    }

    #[test]
    fn disconnected_worker_fails_without_hanging() {
        let mut state = UpdateState::new();
        let tx = state.begin_at(Instant::now()).unwrap();
        drop(tx);
        assert!(state.tick_at(Instant::now()));
        assert_eq!(state.phase(), &UpdatePhase::Failed);
    }

    #[test]
    fn labels_cover_all_phases_in_both_languages() {
        use crate::i18n::{with_lang, Lang};

        fn available() -> UpdatePhase {
            UpdatePhase::Available {
                version: "2.4.0".into(),
            }
        }

        with_lang(Lang::ZhHans, || {
            // 状态说明：五个阶段全覆盖。
            assert_eq!(status_text(&UpdatePhase::Idle), "尚未检查");
            assert_eq!(status_text(&UpdatePhase::Checking), "正在检查…");
            assert_eq!(status_text(&UpdatePhase::UpToDate), "已是最新版本");
            assert_eq!(status_text(&available()), "发现新版本 v2.4.0");
            assert_eq!(status_text(&UpdatePhase::Failed), "检查失败，请稍后重试");

            // 行内动作：五个阶段全覆盖。
            assert_eq!(action_text(&UpdatePhase::Idle), "检查更新");
            assert_eq!(action_text(&UpdatePhase::Checking), "检查更新");
            assert_eq!(action_text(&UpdatePhase::UpToDate), "检查更新");
            assert_eq!(action_text(&available()), "前往下载");
            assert_eq!(action_text(&UpdatePhase::Failed), "检查更新");

            // 可点性：仅检查中禁用。
            assert!(action_enabled(&UpdatePhase::Idle));
            assert!(!action_enabled(&UpdatePhase::Checking));
            assert!(action_enabled(&UpdatePhase::UpToDate));
            assert!(action_enabled(&available()));
            assert!(action_enabled(&UpdatePhase::Failed));

            // 托盘文案：五个阶段全覆盖。
            assert_eq!(tray_text(&UpdatePhase::Idle), "检查更新…");
            assert_eq!(tray_text(&UpdatePhase::Checking), "检查中…");
            assert_eq!(tray_text(&UpdatePhase::UpToDate), "检查更新…");
            assert_eq!(tray_text(&available()), "有新版本 v2.4.0 — 前往下载");
            assert_eq!(tray_text(&UpdatePhase::Failed), "检查更新…");

            assert!(tray_enabled(&UpdatePhase::Idle));
            assert!(!tray_enabled(&UpdatePhase::Checking));
            assert!(tray_enabled(&UpdatePhase::UpToDate));
            assert!(tray_enabled(&available()));
            assert!(tray_enabled(&UpdatePhase::Failed));
        });

        with_lang(Lang::En, || {
            assert_eq!(status_text(&UpdatePhase::Idle), "Not checked yet");
            assert_eq!(status_text(&UpdatePhase::Checking), "Checking…");
            assert_eq!(status_text(&UpdatePhase::UpToDate), "You're up to date");
            assert_eq!(status_text(&available()), "Version 2.4.0 is available");
            assert_eq!(
                status_text(&UpdatePhase::Failed),
                "Couldn't check for updates. Try again later."
            );

            assert_eq!(action_text(&UpdatePhase::Idle), "Check for Updates");
            assert_eq!(action_text(&UpdatePhase::Checking), "Check for Updates");
            assert_eq!(action_text(&UpdatePhase::UpToDate), "Check for Updates");
            assert_eq!(action_text(&available()), "Download");
            assert_eq!(action_text(&UpdatePhase::Failed), "Check for Updates");

            assert!(action_enabled(&UpdatePhase::Idle));
            assert!(!action_enabled(&UpdatePhase::Checking));
            assert!(action_enabled(&UpdatePhase::UpToDate));
            assert!(action_enabled(&available()));
            assert!(action_enabled(&UpdatePhase::Failed));

            assert_eq!(tray_text(&UpdatePhase::Idle), "Check for Updates…");
            assert_eq!(tray_text(&UpdatePhase::Checking), "Checking…");
            assert_eq!(tray_text(&UpdatePhase::UpToDate), "Check for Updates…");
            assert_eq!(
                tray_text(&available()),
                "Version 2.4.0 Available — Download"
            );
            assert_eq!(tray_text(&UpdatePhase::Failed), "Check for Updates…");

            assert!(tray_enabled(&UpdatePhase::Idle));
            assert!(!tray_enabled(&UpdatePhase::Checking));
            assert!(tray_enabled(&UpdatePhase::UpToDate));
            assert!(tray_enabled(&available()));
            assert!(tray_enabled(&UpdatePhase::Failed));
        });

        // 下载页随显式语言参数切换，与当前界面语言无关。
        assert_eq!(
            download_page(crate::i18n::Lang::ZhHans),
            "https://maceverything.modouai.dev/#download"
        );
        assert_eq!(
            download_page(crate::i18n::Lang::En),
            "https://maceverything.modouai.dev/en/#download"
        );
    }
}

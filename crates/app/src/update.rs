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
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// 无回调看门狗（UI 侧兜底，避免状态卡在 Checking）。
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

/// 清单地址（开发覆盖：`MACEEVERYTHING_UPDATE_URL`）。
pub fn manifest_url() -> String {
    std::env::var("MACEEVERYTHING_UPDATE_URL").unwrap_or_else(|_| MANIFEST_URL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders_versions() {
        assert_eq!(Version::parse("2.4.0").unwrap().display(), "2.4.0");
        assert_eq!(Version::parse("v2.4.0").unwrap().display(), "2.4.0");
        assert_eq!(Version::parse("02.004.0").unwrap().display(), "2.4.0");
        assert!(Version::parse("2.3.0") < Version::parse("2.4.0"));
        assert!(Version::parse("9.9.9") < Version::parse("10.0.0"));
        for bad in ["", "2.4", "2.4.0.1", "2.4.x", "2.4.0-beta.1", "v", "1..2"] {
            assert!(Version::parse(bad).is_err(), "{bad:?} 应被拒绝");
        }
    }

    #[test]
    fn evaluate_compares_manifest_with_current() {
        let current = "2.3.0";
        assert_eq!(
            evaluate(br#"{"version":"2.4.0"}"#, current),
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
    fn watchdog_fails_after_timeout_and_ignores_late_result() {
        let t0 = Instant::now();
        let mut state = UpdateState::new();
        let tx = state.begin_at(t0).unwrap();

        assert!(!state.tick_at(t0 + Duration::from_secs(14)));
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
    fn disconnected_worker_fails_without_hanging() {
        let mut state = UpdateState::new();
        let tx = state.begin_at(Instant::now()).unwrap();
        drop(tx);
        assert!(state.tick_at(Instant::now()));
        assert_eq!(state.phase(), &UpdatePhase::Failed);
    }
}

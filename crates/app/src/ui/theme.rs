//! 外观(深浅色)探测与主题应用。
//!
//! 定案(tech.md「变更方案」):不使用 winit 的 KVO 主题监听——PoC-1 实证 NSPanel
//! 换类后监听随 `NSKVONotifying_WinitWindow` 类失效;自建探测:同进程 objc2 读
//! `NSApplication.effectiveAppearance`(不经子进程),渲染帧驱动的 ~2s 低频节流。

use eframe::egui;
use std::time::{Duration, Instant};

/// 探测节流窗(低频:别每帧查系统)。
pub const CHECK_INTERVAL: Duration = Duration::from_secs(2);

/// 应用主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    Light,
    Dark,
}

/// 外观名 → 主题(纯逻辑,可测)。
///
/// macOS 名形如 "NSAppearanceNameDarkAqua"(深色变体还有
/// "NSAppearanceNameAccessibilityHighContrastDarkAqua" 与 "NSAppearanceNameVibrantDark"):
/// 含 "Dark" 即深色;其余("NSAppearanceNameAqua"、空、未知名)→ 浅色。
pub fn normalize_appearance(name: &str) -> Theme {
    if name.contains("Dark") {
        Theme::Dark
    } else {
        Theme::Light
    }
}

/// 节流器:首次即查,之后距上次 ≥ `interval` 才再查(命中才刷新计时)。
pub struct ThemeThrottle {
    last: Option<Instant>,
    interval: Duration,
}

impl ThemeThrottle {
    pub fn new(interval: Duration) -> Self {
        Self {
            last: None,
            interval,
        }
    }

    pub fn should_check(&mut self, now: Instant) -> bool {
        match self.last {
            Some(last) if now.duration_since(last) < self.interval => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}

/// 当前系统外观(同进程 objc2 查询;非 macOS → Light)。
///
/// `MainThreadMarker` 是 objc2 的凭证式安全前提(非主线程返回 None),
/// 无 unsafe 块;`logic()` 由 eframe 主线程调用,正常必有凭证。
#[cfg(target_os = "macos")]
pub fn current_theme() -> Theme {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(mtm) = MainThreadMarker::new() else {
        // 非主线程(理论不可达)兜底:浅色,勿猜深色
        return Theme::Light;
    };
    let app = NSApplication::sharedApplication(mtm);
    let name = app.effectiveAppearance().name();
    normalize_appearance(&name.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn current_theme() -> Theme {
    Theme::Light
}

/// 应用主题到 egui。
///
/// 用 `set_theme` 显式化偏好而非 `set_visuals`:egui 的 System 偏好依赖
/// winit 注入的 system_theme(换类后不可信),显式偏好下 `ctx.theme()`
/// 与视觉一律取自自建探测。全局 Options 生效,两视口(根窗口 + 悬浮条)
/// 共享同一 ctx,主题自动一致。
pub fn apply(ctx: &egui::Context, theme: Theme) {
    ctx.set_theme(match theme {
        Theme::Dark => egui::Theme::Dark,
        Theme::Light => egui::Theme::Light,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探测分数四态 + 变体(钉:含 "Dark" → Dark;Aqua/空/未知 → Light)。
    #[test]
    fn normalize_appearance_four_states() {
        assert_eq!(
            normalize_appearance("NSAppearanceNameDarkAqua"),
            Theme::Dark,
            "全名深色"
        );
        assert_eq!(normalize_appearance("DarkAqua"), Theme::Dark, "短名深色");
        assert_eq!(
            normalize_appearance("NSAppearanceNameAqua"),
            Theme::Light,
            "标准浅色"
        );
        assert_eq!(normalize_appearance(""), Theme::Light, "空串 → 浅色");
        // 深色其余变体(无障碍高对比 / 活力深色)
        assert_eq!(
            normalize_appearance("NSAppearanceNameAccessibilityHighContrastDarkAqua"),
            Theme::Dark
        );
        assert_eq!(
            normalize_appearance("NSAppearanceNameVibrantDark"),
            Theme::Dark
        );
        // 浅色变体与未知名不误判深色
        assert_eq!(
            normalize_appearance("NSAppearanceNameVibrantLight"),
            Theme::Light
        );
        assert_eq!(
            normalize_appearance("SomeFutureAppearance"),
            Theme::Light,
            "未知名 → 浅色"
        );
    }

    /// 节流三态:首查 true;interval 内 false;到期 true;命中后重新计时。
    #[test]
    fn throttle_first_then_interval() {
        let mut t = ThemeThrottle::new(Duration::from_secs(2));
        let t0 = Instant::now();
        assert!(t.should_check(t0), "首次即查");
        assert!(
            !t.should_check(t0 + Duration::from_millis(1999)),
            "interval 内不查"
        );
        assert!(t.should_check(t0 + Duration::from_secs(2)), "到期即查");
        assert!(
            !t.should_check(t0 + Duration::from_secs(3)),
            "命中后从此刻重新计时"
        );
        assert!(t.should_check(t0 + Duration::from_secs(5)), "再过一窗再查");
    }

    /// 主题应用接线(钉:Theme → egui 主题偏好;ctx 全局可见)。
    #[test]
    fn apply_sets_egui_theme() {
        let ctx = egui::Context::default();
        apply(&ctx, Theme::Dark);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        apply(&ctx, Theme::Light);
        assert_eq!(ctx.theme(), egui::Theme::Light);
    }
}

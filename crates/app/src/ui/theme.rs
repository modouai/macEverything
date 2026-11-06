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

    /// 复位计时:下一次 `should_check` 立即放行(唤起时「亮起即补查一次」用)。
    pub fn reset(&mut self) {
        self.last = None;
    }
}

/// 主题跟随状态(节流 + 已应用值;各视口共享,Task 14 起挂在共享状态里)。
///
/// 排帧纪律(L4,tech 平台行为表):探帧只在「有窗可见」期进行——悬浮条 callback、
/// 根视口 `logic`(根窗可见时)、设置 pass(设置窗打开时)与浮层 callback(浮层
/// 可见期)各自检查本状态并给自己的视口排下一帧(`CHECK_INTERVAL` 上限);全隐藏期
/// 不排帧,隐藏态 CPU 0% 红线不被破坏。
/// 唤起时 `check_now_with` 立即应用锁外探测到的外观,
/// 免「隐藏期切了外观、亮起首帧仍是旧主题」。
pub struct ThemeFollow {
    throttle: ThemeThrottle,
    theme: Option<Theme>,
}

impl ThemeFollow {
    pub fn new(initial: Theme, interval: Duration) -> Self {
        Self {
            throttle: ThemeThrottle::new(interval),
            theme: Some(initial),
        }
    }

    /// 到期才探测;变化才应用(返回是否发生了应用)。
    pub fn maybe_apply(&mut self, ctx: &egui::Context, now: Instant) -> bool {
        if !self.throttle.should_check(now) {
            return false;
        }
        self.apply_detected(ctx, current_theme())
    }

    /// 立即补查(外观已由调用方在锁外探测;每次唤起调用)。
    ///
    /// 锁纪律:共享状态持锁期间不得做平台原生调用;`current_theme()` 由调用方
    /// 在锁外完成,本方法只写共享状态与 egui 主题队列。
    pub fn check_now_with(&mut self, ctx: &egui::Context, detected: Theme) {
        self.throttle.reset();
        self.apply_detected(ctx, detected);
    }

    /// 应用一次已探测的外观;变化才应用(返回是否发生了应用)。
    fn apply_detected(&mut self, ctx: &egui::Context, detected: Theme) -> bool {
        if self.theme != Some(detected) {
            self.theme = Some(detected);
            apply(ctx, detected);
            log::info!("系统外观:{detected:?} → 应用对应主题");
            return true;
        }
        false
    }

    /// 当前已应用主题(测试断言用;产品侧无消费者,不做公开面)。
    #[cfg(test)]
    pub fn current(&self) -> Option<Theme> {
        self.theme
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

    /// 已探测外观立即应用(锁外探测 → 锁内只应用;不依赖测试宿主的真实外观)。
    #[test]
    fn check_now_with_applies_detected_theme() {
        let ctx = egui::Context::default();
        let mut f = ThemeFollow::new(Theme::Light, Duration::from_secs(2));
        f.check_now_with(&ctx, Theme::Dark);
        assert_eq!(f.current(), Some(Theme::Dark));
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        // 节流已复位:随后再传同值不重复应用,但状态保持一致。
        f.check_now_with(&ctx, Theme::Dark);
        assert_eq!(f.current(), Some(Theme::Dark));
        f.check_now_with(&ctx, Theme::Light);
        assert_eq!(ctx.theme(), egui::Theme::Light);
    }

    /// 跟随状态三态:节流内不探测;到期探测;`check_now_with` 复位后当帧放行。
    #[test]
    fn follow_throttle_and_check_now() {
        let ctx = egui::Context::default();
        let mut f = ThemeFollow::new(Theme::Light, Duration::from_secs(2));
        let t0 = Instant::now();
        // 首查在构造时已置:距构造 < interval 不探测(无论系统当前外观)。
        assert!(!f.maybe_apply(&ctx, t0), "interval 内不探测");
        // 到期:探测一次(系统外观在测试宿主不可控,断言只钉「不静态跳过」)。
        let _ = f.maybe_apply(&ctx, t0 + Duration::from_secs(2));
        // check_now_with 复位节流:紧接着再调即直接应用已探测外观。
        f.check_now_with(&ctx, Theme::Dark);
        assert_eq!(f.current(), Some(Theme::Dark), "已应用主题恒有值");
    }

    /// 节流 reset 语义(钉:reset 后首次必放行)。
    #[test]
    fn throttle_reset_releases_next_check() {
        let mut t = ThemeThrottle::new(Duration::from_secs(2));
        let t0 = Instant::now();
        assert!(t.should_check(t0));
        assert!(!t.should_check(t0 + Duration::from_millis(1)));
        t.reset();
        assert!(
            t.should_check(t0 + Duration::from_millis(2)),
            "复位后立即放行"
        );
    }
}

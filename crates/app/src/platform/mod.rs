//! 平台能力 facade:悬浮条窗口的 macOS 补丁走这里。
//!
//! 真实实现集中在 `platform/mac.rs`(objc2;unsafe 逐处 SAFETY);
//! 生效配方与差分结论见 docs/specs/v2-ui/poc-results.md「必须的属性组合」,
//! 活样例见 scripts/poc/poc1b_nspanel/src/main.rs。
//!
//! 非 macOS:同名空实现(本仓当前交付面仅 macOS;stub 只为保持 facade 可编译)。
//! 调用点:根视口 `logic`(每帧核对/唤起)、悬浮条视口回调(重锚)、`App::on_exit`(还原类)。
//!
//! 「切 Space 失焦豁免」(2026-10-16 定,product §1 键位表):mac.rs 注册
//! `NSWorkspaceActiveSpaceDidChangeNotification` 观察者,记录最近一次变更;
//! `SpaceExemptDecider`(纯逻辑,本文件)按「同一次过渡 + 豁免一次」判定失焦是否豁免。
//! 实机实测通知晚于失焦约 +270~380ms(全屏 Space 进出两例),故判据以失焦时刻为锚。

use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
pub mod mac;

/// 悬浮条窗口标题(ui 的 ViewportBuilder 与平台层按标题定位原生窗,两处必须一致)。
pub const PALETTE_TITLE: &str = "palette";

/// 设置窗可接受的两种原生标题(查找时同时匹配,避免切换瞬间竞态)。
///
/// 标题随语言切换(视口标题取 `i18n::s().settings_title`);语言切换后旧窗口
/// 标题可能尚未更新,故平台层按两种标题同时定位。
pub fn settings_window_titles() -> [&'static str; 2] {
    [
        crate::i18n::strings(crate::i18n::Lang::ZhHans).settings_title,
        crate::i18n::strings(crate::i18n::Lang::En).settings_title,
    ]
}

/// 悬浮条最大宽(视觉稿 §1.1/§2.2 = 700;ui 侧 `tokens::PANEL_W` 编译期互钉)。
pub const PALETTE_MAX_W: f64 = 700.0;

/// 悬浮条宽度下限(窄屏收窄后的最小可用宽;visual.md §1.1 = 480)。
pub const PALETTE_MIN_W: f64 = 480.0;

/// 窄屏宽度公式的两侧总留白(每侧 24;visual.md §1.1「可见宽 − 48」)。
pub const SCREEN_SIDE_MARGIN: f64 = 48.0;

/// 由可见区宽计算面板宽(纯函数;Task 11 定案):`min(700, 可见宽 − 48)`,下限 480。
pub fn palette_width(visible_w: f64) -> f64 {
    (visible_w - SCREEN_SIDE_MARGIN).clamp(PALETTE_MIN_W, PALETTE_MAX_W)
}

/// 进程内单调时钟基准(懒初始化;日志打点与 Space 变更判定共用,跨进程不可比)。
static MONO_EPOCH: std::sync::LazyLock<Instant> = std::sync::LazyLock::new(Instant::now);

/// 进程内单调毫秒(实机取证打点:通知/失焦日志按此对齐先后)。
pub fn monotonic_ms() -> u64 {
    MONO_EPOCH.elapsed().as_millis() as u64
}

/// 最近一次「活动 Space 变更」事件(mac.rs 通知回调写入)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpaceChange {
    /// 事件序号(进程内单调递增;「已消费」比对用)。
    pub seq: u64,
    /// 事件本地时刻(单调钟)。
    pub at: Instant,
}

/// 失焦豁免判定器(纯逻辑,可测):与 Space 变更同一次过渡的失焦豁免一次。
///
/// 语义(product §1「(失焦)」行,2026-10-16 定):
/// - 切桌面 Space 导致的活动应用变更不隐藏(心流不打断);
/// - 「同一次过渡」= 变更早于失焦不超过 `window`(通知也可能晚于失焦到达,见 `decide`);
/// - 消费语义:一次 Space 变更至多豁免一次失焦——豁免后再次失焦(如点击其他应用)
///   不复用同一变更。
#[derive(Debug)]
pub struct SpaceExemptDecider {
    window: Duration,
    consumed_seq: Option<u64>,
}

impl SpaceExemptDecider {
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            consumed_seq: None,
        }
    }

    /// 判定:change = 最近一次 Space 变更(None = 从未收到);focus_loss_at = 本次失焦时刻。
    /// true = 豁免(并消费该变更)。
    ///
    /// 判据以**失焦时刻**为锚:变更早于失焦不超过 `window` 即算同一次过渡。
    /// 不与「判定时刻」比较——实机实测(2026-10-16)通知晚于失焦到达(约 +270~380ms),
    /// 判定又在通知之后,锚判定时刻会把应豁免的晚到通知误算成过期。
    pub fn decide(&mut self, change: Option<SpaceChange>, focus_loss_at: Instant) -> bool {
        let Some(c) = change else {
            return false; // 从未收到通知(含非 macOS 恒 None)
        };
        if self.consumed_seq == Some(c.seq) {
            return false; // 该次变更已豁免过一次
        }
        // duration_since 对「变更晚于失焦」(同一次过渡的晚到通知)饱和为 0:不判过期。
        if focus_loss_at.duration_since(c.at) >= self.window {
            return false; // 失焦前太久:非同一次过渡
        }
        self.consumed_seq = Some(c.seq);
        true
    }
}

/// 系统首选语言列表；非 macOS 返回错误（调用方回落到简体中文）。
pub fn preferred_languages() -> anyhow::Result<Vec<String>> {
    #[cfg(target_os = "macos")]
    {
        mac::preferred_languages()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow::anyhow!("非 macOS:无法读取系统语言"))
    }
}

/// 系统目录选择器（主线程；取消/失败 = None）。非 macOS = None。
///
/// 原生 `NSOpenPanel` 模态面板：仅目录、单选。阻塞返回——调用方须在主线程
/// （egui 绘制/事件处理中）调用，且调用期间不得持有共享锁。
///
/// 已知可用性偏差：实现前置的 `NSApplication.activate()`（无参）是 macOS 14+
/// API，本仓最低支持 macOS 13（README）；既有 `show_root_window` /
/// `show_settings_window` 早已在 macOS 13 目标上调用同一方法。本任务不改变
/// 最低系统版本，仅记录该既有偏差（E4 实机在 macOS 26 验证）。
pub fn pick_directory() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        mac::pick_directory()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 破坏性操作原生确认弹窗（主线程；取消/失败 = false）。非 macOS = false。
///
/// macOS 用 `NSAlert`：第一个按钮固定「取消」并持有默认 Return，破坏性
/// `action_label` 为第二个按钮（显式清空 Return 键等价）；仅点击第二个按钮
/// 返回 true。调用方须在主线程且不得持共享锁（模态期间 egui 冻结）。
pub fn confirm_action(title: &str, message: &str, action_label: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::confirm_action(title, message, action_label)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (title, message, action_label);
        false
    }
}

/// 鼠标任意键当前是否按下(「点击导致失焦」的快判信号;非 macOS 恒 false)。
pub fn mouse_button_down() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::mouse_button_down()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 指针是否在悬浮条窗内(ghost 态「点面板外才隐藏」判定;非 macOS/未接窗恒 false)。
pub fn palette_contains_mouse() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::palette_contains_mouse()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 最近一次 Space 变更(非 macOS 恒 None)。
pub fn last_space_change() -> Option<SpaceChange> {
    #[cfg(target_os = "macos")]
    {
        mac::last_space_change()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 结果行图标纹理边长(px):32pt × 2x(Retina 位图)。
///
/// 平台层取图与 ui 层建纹理共用此值(单源,防两侧漂移)。
pub const ICON_PX: u32 = 64;

/// 解析文件的 UTI(目录走 `UTTypeFolder`;无扩展名/解析失败回落 `UTTypeData`)。
///
/// 只是廉价的 UTType 查询,UI 线程直接调用;取图/解码在图标工作线程。
/// `extension` 由调用方从文件名取出(与 `ui::icons::type_key` 同一口径)。
pub fn uti_for(extension: Option<&str>, is_dir: bool) -> String {
    #[cfg(target_os = "macos")]
    {
        mac::uti_for(extension, is_dir)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = extension;
        if is_dir {
            "public.folder"
        } else {
            "public.data"
        }
        .to_owned()
    }
}

/// 按 UTI 取系统图标并转成 [`ICON_PX`] 见方 **预乘** RGBA(取图/解码/缩放在工作线程)。
pub fn type_icon_rgba(uti: &str) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::type_icon_rgba(uti)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = uti;
        None
    }
}

/// 按路径读用户自定义图标(64×64 **预乘** RGBA);无自定义图标/失败 = `None`。
pub fn custom_icon_rgba(path: &str) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::custom_icon_rgba(path)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        None
    }
}

/// 记录根窗句柄(创建期一次)。
/// 安装 Space 变更后的悬浮条重绘钩子(`App::new` 调用一次)。
///
/// 切到他人全屏 Space 时 eframe 可能不主动跑悬浮条视口;通知回调经此唤醒重绘,
/// 并在主队列恢复面板/垫窗层级(见 mac.rs `refresh_palette_after_space_change`)。
pub fn install_space_repaint(ctx: eframe::egui::Context) {
    #[cfg(target_os = "macos")]
    {
        mac::install_space_repaint(ctx);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ctx;
    }
}

pub fn attach_root(cc: &eframe::CreationContext<'_>) {
    #[cfg(target_os = "macos")]
    mac::attach_root(cc);
    #[cfg(not(target_os = "macos"))]
    let _ = cc;
}

/// 每帧核对:悬浮条窗首见即施补丁(五要素+两补丁);已接则核对漂移。
pub fn ensure_patched() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::ensure_patched()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// 唤起悬浮条:原生定位 + `orderFrontRegardless` + `makeKeyWindow`(不激活 app)。
/// 返回实际面板宽(按鼠标所在屏回填;调用方据此同步 UI 布局宽)。
///
/// `top_inset` = 窗口顶预留的形象条高(Task 9):窗口顶 = 面板顶 − top_inset,
/// 面板顶边仍距可见区顶 110。
pub fn show_palette(top_inset: f64) -> anyhow::Result<f64> {
    #[cfg(target_os = "macos")]
    {
        mac::show_palette(top_inset)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = top_inset;
        Err(anyhow::anyhow!("非 macOS:悬浮条原生唤起不可用"))
    }
}

/// 显示完整窗口(根窗):激活 app + 置前 + 成 key(常规 App 窗口语义)。
pub fn show_root_window() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::show_root_window()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow::anyhow!("非 macOS:根窗原生唤起不可用"))
    }
}

/// 设置窗口首次打开居中 + 置前（原生；仅 macOS）。
///
/// `center = true` 只在首次打开传入：按光标所在屏 `visibleFrame` 居中 620×600；
/// 之后传 false 仅激活 app + `orderFrontRegardless` + `makeKeyWindow`，保留用户
/// 拖动位置。窗口未就绪（视口尚未创建）/ 非 macOS → Err，调用方退回 egui
/// `Visible(true)` + `Focus`。
pub fn show_settings_window(center: bool) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::show_settings_window(center)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = center;
        Err(anyhow::anyhow!("非 macOS:设置窗口原生定位/置前不可用"))
    }
}

/// 高度变化时重锚(顶边钉住;悬浮条每次 pass 调用)。
pub fn reanchor() {
    #[cfg(target_os = "macos")]
    mac::reanchor();
}

/// 毛玻璃垫窗每帧同步(显隐随面板;悬浮条每次 pass 调用)。
/// `corner_radius` 由 ui 层传入(垫窗圆角须与面板一致;平台层不反向引 ui 常量)。
/// `top_inset` = 窗口顶部的形象条高;垫窗只覆盖面板,不含形象条(Task 9)。
/// `visible_panel_h` = 本帧可见面板高(动画期为 `clip_h`):垫窗顶边钉面板顶、
/// 高度随可见面板走,避免过渡期磨砂带伸出可见面板底。
pub fn sync_blur(visible: bool, corner_radius: f64, top_inset: f64, visible_panel_h: f64) {
    #[cfg(target_os = "macos")]
    mac::sync_blur(visible, corner_radius, top_inset, visible_panel_h);
    #[cfg(not(target_os = "macos"))]
    let _ = (visible, corner_radius, top_inset, visible_panel_h);
}

/// 退出/销毁前还原窗口类(否则 winit drop → abort)。
pub fn revert_class() {
    #[cfg(target_os = "macos")]
    mac::revert_class();
}

/// 原生菜单项(支持分隔线与子菜单;`tag` 只用于叶子项)。
///
/// 本任务菜单内容仍为旧四项平铺,`Separator`/`Submenu` 由原生桥接消费但尚无
/// 构造点(后续菜单任务会构造),故局部 allow 防 dead_code 警告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        title: &'static str,
        tag: i64,
        enabled: bool,
    },
    #[allow(dead_code)]
    Separator,
    #[allow(dead_code)]
    Submenu {
        title: &'static str,
        items: Vec<MenuEntry>,
    },
}

/// 弹出原生右键菜单(阻塞;主线程)。`items` = 菜单项树;叶子项经 target/action
/// 回传 tag,子菜单父项无 action。返回所选项 tag(None = 取消)。
/// 位置为视图坐标(与 egui 坐标同值,PoC-3 实测;见 mac.rs)。
/// 非 macOS / 非主线程 / 取窗失败返回 None(留痕,调用方按取消处理)。
pub fn popup_menu(
    items: &[MenuEntry],
    at_view_pos: (f64, f64),
    frame: &eframe::Frame,
) -> Option<i64> {
    #[cfg(target_os = "macos")]
    {
        mac::popup_menu(items, at_view_pos, frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (items, at_view_pos, frame);
        None
    }
}

/// 注册托盘菜单(install 期一次;`Menu::ns_menu()` 的裸指针,按屏自弹用)。
/// 非 macOS:no-op(本仓交付面仅 macOS,stub 只为保持 facade 可编译)。
pub fn register_tray_menu(ns_menu: *mut std::ffi::c_void) {
    #[cfg(target_os = "macos")]
    mac::register_tray_menu(ns_menu);
    #[cfg(not(target_os = "macos"))]
    let _ = ns_menu;
}

/// 在光标所在屏弹出托盘菜单(阻塞;主线程)。自弹,绕过 tray-icon 内建的
/// StatusItem 锚定(副屏点击会错屏到主屏;根因见 mac.rs)。非 macOS:no-op。
pub fn popup_tray_menu() {
    #[cfg(target_os = "macos")]
    mac::popup_tray_menu();
}

/// 是否已接上悬浮条窗(诊断;未接上时调用方排下一帧完成接线)。
pub fn attached() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::attached()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 根窗当前原生可见性(首帧纠偏核对;非 macOS/未接句柄恒 None)。
pub fn root_visible() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::root_visible()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 悬浮条窗当前原生可见性(诊断;非 macOS/未接窗恒 None)。
pub fn palette_visible() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::palette_visible()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 设置窗当前原生可见性(诊断;非 macOS/未创建恒 None)。
pub fn settings_visible() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::settings_visible()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(seq: u64, at: Instant) -> Option<SpaceChange> {
        Some(SpaceChange { seq, at })
    }

    /// 宽度策略边界(Task 11;visual.md §1.1):`min(700, 可见宽 − 48)`,下限 480。
    #[test]
    fn palette_width_boundaries() {
        assert_eq!(palette_width(1920.0), 700.0, "常规屏取设计宽");
        assert_eq!(palette_width(748.0), 700.0, "恰 700 的临界可见宽");
        assert_eq!(palette_width(747.0), 699.0, "临界下 1 点即收窄");
        assert_eq!(palette_width(529.0), 481.0);
        assert_eq!(palette_width(528.0), 480.0, "恰到底限");
        assert_eq!(palette_width(500.0), 480.0, "低于底限取 480");
    }

    /// 无变更通知 → 永不豁免(含非 macOS 恒 None 路径)。钉:None 短路。
    #[test]
    fn without_change_never_exempts() {
        let mut d = SpaceExemptDecider::new(Duration::from_millis(500));
        assert!(!d.decide(None, Instant::now()));
    }

    /// 变更先于失焦(窗口期内)豁免一次;第二次同一变更不再豁免(消费语义)。
    #[test]
    fn change_before_loss_exempts_once() {
        let t0 = Instant::now();
        let mut d = SpaceExemptDecider::new(Duration::from_millis(500));
        assert!(d.decide(change(1, t0), t0 + Duration::from_millis(100)));
        assert!(
            !d.decide(change(1, t0), t0 + Duration::from_millis(200)),
            "同一变更只豁免一次"
        );
    }

    /// 变更晚于失焦(通知晚到,实机实测约 +270~380ms)同样豁免——失焦锚定语义。
    #[test]
    fn late_notification_exempts() {
        let loss = Instant::now();
        let mut d = SpaceExemptDecider::new(Duration::from_millis(500));
        assert!(d.decide(change(1, loss + Duration::from_millis(380)), loss));
    }

    /// 失焦前太久(≥ window)的变更不豁免(边界:恰好等于 window 即不豁免)。
    #[test]
    fn stale_change_not_exempt() {
        let loss = Instant::now();
        let mut d = SpaceExemptDecider::new(Duration::from_millis(500));
        assert!(!d.decide(change(1, loss - Duration::from_millis(500)), loss));
        assert!(!d.decide(change(2, loss - Duration::from_millis(900)), loss));
    }

    /// 新一次 Space 变更重新获得豁免资格(消费只作废同一变更)。
    #[test]
    fn new_change_re_exempts() {
        let loss1 = Instant::now();
        let mut d = SpaceExemptDecider::new(Duration::from_millis(500));
        assert!(d.decide(change(1, loss1), loss1));
        let loss2 = loss1 + Duration::from_secs(2);
        assert!(
            d.decide(change(2, loss2 + Duration::from_millis(300)), loss2),
            "第 2 次变更(晚到通知)豁免第 2 次失焦"
        );
    }
}

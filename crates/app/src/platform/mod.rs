//! 平台能力 facade:悬浮条窗口的 macOS 补丁走这里。
//!
//! 真实实现集中在 `platform/mac.rs` 与 `platform/mac_quicklook.rs`(objc2;unsafe 逐处 SAFETY);
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
pub mod ax_ffi;
#[cfg(target_os = "macos")]
pub mod cg_events;
#[cfg(target_os = "macos")]
pub mod dialog_navigator;
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "macos")]
pub mod mac_quicklook;
/// 保存面板助手的跨平台事件/屏蔽状态与决策纯逻辑(UI 接线层与单测使用)。
pub mod panel_assist;
// 保存面板助手(M0 探针 + 检测先行版):与 ax_ffi 同款 cfg,保持模块一致性。
#[cfg(target_os = "macos")]
pub mod panel_detector;
#[cfg(target_os = "macos")]
pub mod panel_probe;

pub use panel_assist::{Decision, Dismissals, HandbackAction, PanelEvent};

/// 跳转结果(执行层 [`dialog_navigator::NavigateOutcome`] 的唯一真相;
/// 非 macOS 的同名 stub 只为保持 facade 可编译)。
#[cfg(target_os = "macos")]
pub use dialog_navigator::NavigateOutcome;
#[cfg(not(target_os = "macos"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigateOutcome {
    Done,
    FellBackToClipboard,
    Aborted,
}

/// 悬浮条窗口标题(ui 的 ViewportBuilder 与平台层按标题定位原生窗,两处必须一致)。
pub const PALETTE_TITLE: &str = "palette";

/// 保存面板目录助手浮层窗口标题(同上:builder 与平台层定位两处必须一致)。
pub const OVERLAY_TITLE: &str = "overlay";

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

/// 按进程组发信号(`kill(-pgid, sig)`;安装器探测 shell 超时整组终止用)。
/// 非 macOS:no-op(本仓交付面仅 macOS,stub 只为保持 facade 可编译)。
///
/// `pid` 由 `Child::id()` 取得:子进程以 `process_group(0)` 自立新进程组,
/// 组 id 即其 pid。
pub fn kill_process_group(pid: u32, signal: i32) {
    #[cfg(target_os = "macos")]
    {
        mac::kill_process_group(pid, signal)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, signal);
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

/// 取路径处 APP bundle 自身图标(`NSWorkspace iconForFile:`)并转成 [`ICON_PX`]
/// 见方 **预乘** RGBA(取图/解码/缩放在工作线程)。调用方负责只传 APP bundle。
pub fn app_icon_rgba(path: &str) -> Option<Vec<u8>> {
    #[cfg(target_os = "macos")]
    {
        mac::app_icon_rgba(path)
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

/// 每帧核对浮层窗补丁(与悬浮条同法;视口窗未创建时 no-op,下帧再试)。
pub fn ensure_overlay_patched() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::ensure_overlay_patched()
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

/// 唤起保存面板目录助手浮层(原生定位 + 置前 + 成 key;不激活 app)。
///
/// `ax_frame` = 检测器上报的保存面板 frame,AX 屏幕坐标(左上原点、y 向下)。
/// 平台层负责换算 AppKit 坐标、按面板中心所在屏定位。
pub fn overlay_show(ax_frame: [f64; 4]) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::overlay_show(ax_frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ax_frame;
        Err(anyhow::anyhow!("非 macOS:浮层原生唤起不可用"))
    }
}

/// 跳转完成后的被动复现:定位 + 置前但不 makeKey(键盘焦点留在宿主面板)。
pub fn overlay_show_passive(ax_frame: [f64; 4]) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::overlay_show_passive(ax_frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ax_frame;
        Err(anyhow::anyhow!("非 macOS:浮层原生唤起不可用"))
    }
}

/// 浮层窗当前原生可见性(诊断用;未创建/非主线程 → None)。
pub fn overlay_visible() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::overlay_visible()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 隐藏浮层(`orderOut`,不销毁窗口)。
pub fn overlay_hide() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::overlay_hide()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// 注册「浮层持 key 交还」回调(design §10;主线程;重复调用覆盖前值)。
///
/// 回调参数为 AppKit 全局坐标点击点(`NSEvent::mouseLocation`);监听只收
/// **其他 app** 窗口的 mouse-down,自家窗口不触发。决策(是否交还/是否恢复
/// 面板焦点)由接线层持 Shared 状态判定(纯逻辑 `panel_assist::handback_action`)。
/// 非 macOS:no-op。
pub fn install_overlay_focus_handback(handler: Box<dyn Fn(f64, f64) + Send + Sync>) {
    #[cfg(target_os = "macos")]
    {
        mac::install_overlay_focus_handback(handler);
    }
    #[cfg(not(target_os = "macos"))]
    {
        drop(handler);
    }
}

/// 浮层窗当前是否为 key window(交还判定;非主线程/未创建 → None)。
pub fn overlay_is_key() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::overlay_is_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 主屏 frame 高(AX ↔ AppKit 坐标换算参照;非主线程/无屏 → None)。
pub fn primary_screen_height() -> Option<f64> {
    #[cfg(target_os = "macos")]
    {
        mac::primary_screen_height()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 交还键盘(design §10):浮层持 key 时 resign(必要时 orderOut 回退)。
/// 浮层可见性与 UI 状态不变(转被动形态)。`Ok(true)` = 已不持 key。
pub fn overlay_yield_key() -> anyhow::Result<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::overlay_yield_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(true)
    }
}

/// 恢复宿主保存面板键盘焦点(阻塞;调用方在后台线程执行)。
///
/// 与跳转兜底共用 navigator 的 `focus_save_panel`(AXRaise/AXFocused +
/// 读回确认);失败 = false 仅留痕,调用方不得据此重试或发合成键(设计 §10.2)。
/// 非 macOS 恒 false。
pub fn panel_focus_restore(pid: i32) -> bool {
    #[cfg(target_os = "macos")]
    {
        dialog_navigator::restore_panel_focus(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        false
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
/// Dock 图标显隐(完整窗口可见期显示;非 macOS 空实现)。
pub fn set_dock_icon(show: bool) {
    #[cfg(target_os = "macos")]
    {
        mac::set_dock_icon(show);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = show;
    }
}

/// 当前挂载的本地外接卷名(设置页「外接磁盘」;非 macOS 恒空)。
pub fn local_volume_names() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        mac::local_volume_names()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

pub fn sync_blur(visible: bool, corner_radius: f64, top_inset: f64, visible_panel_h: f64) {
    #[cfg(target_os = "macos")]
    mac::sync_blur(visible, corner_radius, top_inset, visible_panel_h);
    #[cfg(not(target_os = "macos"))]
    let _ = (visible, corner_radius, top_inset, visible_panel_h);
}

/// I3 回归钉的测试计数(生产不启用):`send_palette_hide` 必须调用本函数。
#[cfg(test)]
pub(crate) static HIDE_BLUR_CALLS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// 立即隐藏毛玻璃垫窗(不依赖绘制帧)。隐藏态 `draw_palette` 在可见性守卫处
/// 直接 return,末尾的 `sync_blur(false)` 不会执行;只发 `Visible(false)` 命令
/// 会让垫窗的磨砂矩形残留在屏上。这里只对本 app 自建的垫窗 `orderOut`
/// (不是 winit 管理的窗口),不会触发同步重入帧,可从帧内(持 Shared 锁)调用。
pub fn hide_blur() {
    #[cfg(test)]
    HIDE_BLUR_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    mac::sync_blur(false, 0.0, 0.0, 0.0);
}

/// 安装字形图集回收用的 Context(主线程;App::new 调用一次)。非 macOS:no-op。
pub fn install_surface_reclaim(ctx: eframe::egui::Context) {
    #[cfg(target_os = "macos")]
    {
        mac::install_surface_reclaim(ctx);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = ctx;
    }
}

/// 安排「四窗全隐 N 秒后缩窗释放 surface」一次性定时器(主线程;重复调用覆盖)。
/// 非 macOS:no-op。
pub fn schedule_surface_shrink() {
    #[cfg(target_os = "macos")]
    {
        mac::schedule_surface_shrink();
    }
}

/// 退出/销毁前还原窗口类(否则 winit drop → abort)。
pub fn revert_class() {
    #[cfg(target_os = "macos")]
    mac::revert_class();
}

/// 菜单项快捷键(仅展示/菜单跟踪期触发;窗口级按键由 results.rs 处理)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuShortcut {
    /// NSMenuItem keyEquivalent(`"c"` 等)。
    pub key: &'static str,
    /// Command(⌘)。
    pub command: bool,
    /// Shift(⇧)。
    pub shift: bool,
}

/// 原生菜单项(支持分隔线;`tag` 只用于菜单项)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        title: &'static str,
        /// 菜单项选择回传标识。
        ///
        /// -1 保留为未选择哨兵,构造方不得使用。
        tag: i64,
        enabled: bool,
        /// 菜单右侧展示的快捷键(菜单打开时也可触发)。
        shortcut: Option<MenuShortcut>,
    },
    Separator,
}

/// 弹出原生右键菜单(阻塞;主线程)。`items` = 菜单项列表;每项经 target/action
/// 回传 tag。返回所选项 tag(None = 取消)。
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

/// 打开/更新 Quick Look 预览面板(主线程;始终置前,不承担关闭语义)。
///
/// `paths` 为空 = no-op。非 macOS / 非主线程 / 面板不可用 → Err。
pub fn quick_look_show(paths: &[String], frame: &eframe::Frame) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac_quicklook::show(paths, frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, frame);
        Err(anyhow::anyhow!("非 macOS:快速查看不可用"))
    }
}

/// 空格语义:面板未显示 → 打开;已显示 → 关闭并归还焦点。
pub fn quick_look_toggle(paths: &[String], frame: &eframe::Frame) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac_quicklook::toggle(paths, frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, frame);
        Err(anyhow::anyhow!("非 macOS:快速查看不可用"))
    }
}

/// Quick Look 面板当前是否可见(非 macOS 恒 false)。
pub fn quick_look_visible() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac_quicklook::visible()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 每帧轮询:系统侧关闭面板后归还结果窗口焦点(非 macOS:no-op)。
pub fn quick_look_poll(frame: &eframe::Frame) {
    #[cfg(target_os = "macos")]
    mac_quicklook::poll(frame);
    #[cfg(not(target_os = "macos"))]
    let _ = frame;
}

/// 静默关闭 Quick Look（完整窗口隐藏/退出时用；不激活 App）。
pub fn quick_look_dismiss() {
    #[cfg(target_os = "macos")]
    mac_quicklook::dismiss();
}

/// 把选中文件写入通用剪贴板(语义 = Finder「拷贝」;主线程)。
/// 空列表 = no-op;非 macOS / 非主线程 / 写入失败 → Err。
pub fn copy_files(paths: &[String]) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::copy_files(paths)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Err(anyhow::anyhow!("非 macOS:文件剪贴板不可用"))
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

/// 辅助功能授权状态(保存面板助手;macOS)。
pub fn panel_assist_trusted() -> bool {
    // 诊断开关:强制按未授权处理。TCC 撤销对运行中进程不生效,且列表里可能
    // 存在多份同名副本(按路径/签名区分);实机验证授权提示与降级路径时用
    // `MCE_FORCE_UNTRUSTED=1` 启动。仅影响保存面板助手,不改其它权限行为。
    if std::env::var_os("MCE_FORCE_UNTRUSTED").is_some() {
        return false;
    }
    #[cfg(target_os = "macos")]
    {
        mac::ax_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 启动保存面板检测器(产品路径;主线程调用)。
///
/// 先注册 NSWorkspace 前台应用切换观察者(检测器的换绑来源),再启动 detector。
/// detector 常驻不可停止;返回 None = 权限缺失/初始化失败(调用方按开关重试或忽略)。
pub fn panel_detector_start() -> Option<std::sync::mpsc::Receiver<PanelEvent>> {
    #[cfg(target_os = "macos")]
    {
        if !mac::install_app_activate_observer() {
            log::warn!("保存面板检测未启动:前台应用切换观察者注册失败");
            return None;
        }
        panel_detector::start_detector()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 打开系统设置 → 隐私与安全性 → 辅助功能(保存面板助手授权入口)。
pub fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    {
        mac::open_accessibility_settings()
    }
    #[cfg(not(target_os = "macos"))]
    {}
}

/// 把指定 pid 的 app 激活为前台(跳转前恢复宿主键盘焦点)。
/// 非 macOS 恒 false。
pub fn activate_app(pid: i32) -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::activate_app(pid)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        false
    }
}

/// 读剪贴板纯文本(None = 无文本/非文本)。navigator 策略 B 的 save。
/// 非 macOS 恒 None。
pub fn clipboard_text() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        mac::clipboard_text()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// 写剪贴板纯文本(先 clearContents 保证只留纯文本一项)。返回写入成败。
/// 非 macOS 恒 false。
pub fn clipboard_set_text(s: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::clipboard_set_text(s)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = s;
        false
    }
}

/// 恢复(写回 save 的文本;None = 原本无文本 → 清空文本项)。非 macOS 恒 true。
pub fn clipboard_restore(saved: Option<&str>) -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::clipboard_restore(saved)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = saved;
        true
    }
}

/// 跳转基线:宿主 app 的窗口快照 + 可写输入框快照。
///
/// 作用:区分「Go-to 路径框」与「保存面板文件名框」——Go-to 候选必须落在
/// **基线之后新出现的窗口**里(AX sheet 是独立窗口),旧窗口(保存面板)内的
/// 输入框一律排除。只比输入框引用不够:用户点击面板/切换目录后 AX 子树可能
/// 重建,旧字段引用全部失效,会把新的文件名框误当候选。
#[cfg(target_os = "macos")]
pub struct PanelBaseline {
    /// 宿主 app 当前全部窗口引用(快照时存在)。
    pub windows: Vec<ax_ffi::AXRef>,
    /// 宿主 app 当前全部可写输入框引用(文件名框等排除集)。
    pub fields: Vec<ax_ffi::AXRef>,
}
/// 非 macOS stub(交付面仅 macOS;空 Vec 可构造且 Send,保持接线层可编译)。
#[cfg(not(target_os = "macos"))]
pub struct PanelBaseline {
    pub windows: Vec<std::convert::Infallible>,
    pub fields: Vec<std::convert::Infallible>,
}

/// 采集跳转基线(阻塞;调用方在后台线程执行)。
#[cfg(target_os = "macos")]
pub fn panel_baseline_collect(pid: i32) -> PanelBaseline {
    dialog_navigator::baseline_for(pid)
}
#[cfg(not(target_os = "macos"))]
pub fn panel_baseline_collect(pid: i32) -> PanelBaseline {
    let _ = pid;
    PanelBaseline {
        windows: Vec::new(),
        fields: Vec::new(),
    }
}

/// 把保存面板跳转到指定目录(阻塞;调用方在后台线程执行)。
///
/// `baseline` 为浮层唤起期预采集的基线;None/空时跳转线程同步采集(兜底)。
/// 状态机驱动策略 A(AX 直设)/策略 B(剪贴板 + ⌘V)降级,收尾恢复剪贴板;
/// 返回终态供浮层接线层决定后续提示(design §5)。
/// 非 macOS 恒 `Aborted`(交付面仅 macOS,stub 只为保持 facade 可编译)。
pub fn navigate_panel(pid: i32, path: &str, baseline: Option<PanelBaseline>) -> NavigateOutcome {
    #[cfg(target_os = "macos")]
    {
        dialog_navigator::run_navigate(pid, path, baseline)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, path, baseline);
        NavigateOutcome::Aborted
    }
}

/// M0 探针启动(环境变量 MCE_PANEL_PROBE=1;开发取证工具,不进产品路径)。
/// 非 macOS:no-op。
pub fn probe_start() {
    #[cfg(target_os = "macos")]
    mac::probe_start();
}

/// AX 兼容性诊断探针(MCE_PANEL_AX_PROBE=<pid>;开发取证工具,不进产品路径)。
/// 非 macOS:no-op。
pub fn ax_diag_probe(pid: i32) {
    #[cfg(target_os = "macos")]
    mac::ax_diag_probe(pid);
}

/// 根窗当前是否为 key window(egui viewport focused 状态补正用;
/// 非 macOS/未接句柄恒 None)。
pub fn root_is_key() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        mac::root_is_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
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

/// 拉取 HTTPS 资源（发现新版本）。
///
/// 同步返回响应体；超时、非 HTTPS、非 200、最终主机不符、超过 `max_bytes`
/// 或网络错误均返回可读错误。非 macOS 直接失败。
///
/// 同步阻塞至多 `timeout + 2s`：调用方须在非 UI 线程调用（不得阻塞 egui 主线程）。
pub fn fetch_url_bytes(
    url: &str,
    timeout: std::time::Duration,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    #[cfg(target_os = "macos")]
    {
        mac::fetch_url_bytes(url, timeout, max_bytes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (url, timeout, max_bytes);
        Err("非 macOS:网络不可用".to_owned())
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

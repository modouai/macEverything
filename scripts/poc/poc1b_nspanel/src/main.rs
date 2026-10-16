//! PoC-1 续（poc1b）：运行期把 eframe/winit 的 NSWindow 改类为 NSPanel，
//! 验证悬浮条能否进入「他人全屏应用新建的 Space」。
//!
//! 背景：PoC-1（`poc1_window`）已实机证明普通 NSWindow 即使设 `setLevel(Floating)` +
//! `setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)`，四组合对照全数进不了
//! 他人全屏 Space（外证：tauri-apps/tauri#11488 同结论，社区解法 = 改类 NSPanel）。
//!
//! 路线来源（外证，不凭记忆）：tauri-nspanel（ahkohd/tauri-nspanel，同为 winit 底座）。
//! 其 `examples/fullscreen` 的生效组合，逐项对照 README 原文：
//!   1. 自定义 NSPanel 子类（覆写 `canBecomeKeyWindow` → true）→ `object_setClass` 换类
//!   2. `setLevel(Floating)`
//!   3. `setStyleMask(NonactivatingPanel)` —— README 原文「The panel must be
//!      non-activating to display over fullscreen windows」
//!   4. `setCollectionBehavior(FullScreenAuxiliary | CanJoinAllSpaces)`
//!   5. 显示走 `orderFrontRegardless`（不激活 app、不切 Space）
//! 本 spike 复刻该组合，并给每项加差分开关（env），实测每项是否必要。
//!
//! 实锁版本（API 名逐一对照实锁 crate 源码确认）：eframe / egui 0.37.0、objc2 0.6.5、
//! objc2-app-kit 0.3.2、objc2-foundation 0.3.2、raw-window-handle 0.6.2。
//! objc2 0.6.5 的换类 API：`objc2::ffi::object_setClass(*mut AnyObject, *const AnyClass)`
//! 与 `AnyObject::set_class(&AnyObject, &AnyClass)`（后者自带实例大小 debug 断言）；
//! `define_class!` 语法为 `#[unsafe(super(NSPanel))]` + `#[thread_kind = MainThreadOnly]`
//! （0.6 起 `declare_class!` 已改名且语法变化）。
//!
//! 键位（窗口可见时）：ESC 退出 ｜ F1 / H 隐藏（3 秒后自动再显示）｜ F2 / L 切换级别。
//!
//! 环境变量（差分开关与自动化触发，详见 docs/specs/v2-ui/poc-results.md 复现步骤）：
//! - `POC1B_MODE=subclass|plain|none` 换类目标：自定义 NSPanel 子类（默认）/ 裸 NSPanel / 不换类（对照组）
//! - `POC1B_NONACTIVATING=0`          不追加 `NSWindowStyleMaskNonactivatingPanel`（默认追加）
//! - `POC1B_LEVEL=status`             起始用 Status(25) 档（默认 Floating(3)）
//! - `POC1B_SHOW=orderfront|winit`    显示路径：`orderFrontRegardless`（默认）/ winit `Visible(true)`（makeKeyAndOrderFront）
//! - `POC1B_KEY=0`                    显示后不 `makeKeyWindow`（默认为 1）
//! - `POC1B_ACCESSORY=1`              切 Accessory 激活策略（无 Dock 图标）
//! - `POC1B_START_HIDDEN=0`           启动即可见（默认隐藏，等触发器）
//! - `POC1B_CMD=<路径>`               触发器文件路径（默认 `/tmp/poc1b.cmd`）
//! - `POC1B_POS=x,y`                  窗口全局坐标（逻辑点）
//!
//! 触发器文件命令字（写入后被读走并删除）：`show` / `hide` / `key` / `type` / `quit`。

use std::path::PathBuf;
use std::time::Duration;

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{define_class, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSEvent, NSEventModifierFlags, NSEventType,
    NSFloatingWindowLevel, NSPanel, NSScreen, NSStatusWindowLevel, NSView, NSWindow,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSProcessInfo, NSPoint, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 隐藏后自动再显示的等待时长（与 poc1 一致，验证 `ViewportCommand::Visible` 双向）。
const REVEAL_AFTER: Duration = Duration::from_secs(3);

/// 自驱间隔：保证隐藏期与空闲期也持续核对补丁漂移、轮询触发器文件。
const CHECK_INTERVAL: Duration = Duration::from_millis(100);

// 自定义 NSPanel 子类：tauri-nspanel 同法（其 `panel!` 宏生成 NSPanel 子类并覆写这两个方法）。
//
// 换类会丢掉 winit `WinitWindow` 的两项覆写（winit 0.30.13 `platform_impl/macos/window.rs`
// 中两者恒为 true），故此处复刻同语义，避免行为回退到 NSWindow 默认实现。
define_class!(
    // SAFETY（define_class 的 unsafe 契约）：
    // - 超类 NSPanel 无子类化约束；
    // - 本类不实现 Drop、无新增实例变量（ivars 缺省为 `()`），实例大小与超类一致——这正是
    //   `object_setClass` 的硬要求（运行时实测并打印三者大小作证据）；
    // - 覆写方法签名与 AppKit 一致（`canBecomeKeyWindow` / `canBecomeMainWindow` 均无参返回 BOOL）。
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "Poc1bPanel"]
    struct Poc1bPanel;

    impl Poc1bPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            true
        }
    }

    unsafe impl NSObjectProtocol for Poc1bPanel {}
);

/// 换类目标。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// 自定义 NSPanel 子类（tauri-nspanel 路线，默认）。
    Subclass,
    /// 裸 NSPanel（任务原文写法，验证子类是否必要）。
    Plain,
    /// 对照组：不换类（复现 PoC-1 的失败形态）。
    None,
}

/// 窗口级别档位。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    /// `NSFloatingWindowLevel` = 3。
    Floating,
    /// `NSStatusWindowLevel` = 25。
    Status,
}

impl Level {
    fn ns_level(self) -> isize {
        match self {
            Self::Floating => NSFloatingWindowLevel,
            Self::Status => NSStatusWindowLevel,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Floating => "Floating(3)",
            Self::Status => "Status(25)",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Floating => Self::Status,
            Self::Status => Self::Floating,
        }
    }
}

/// 显示路径。tauri-nspanel 用 `orderFrontRegardless`（不激活 app）；
/// winit 的 `Visible(true)` 走 `makeKeyAndOrderFront`（会尝试激活 app），作为差分对照。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ShowMode {
    OrderFront,
    Winit,
}

impl ShowMode {
    fn label(self) -> &'static str {
        match self {
            Self::OrderFront => "orderFrontRegardless",
            Self::Winit => "winit Visible(true)（makeKeyAndOrderFront）",
        }
    }
}

/// 运行参数（全走 env，供自动化差分）。
#[derive(Debug)]
struct Spec {
    mode: Mode,
    nonactivating: bool,
    level: Level,
    show: ShowMode,
    take_key: bool,
    accessory: bool,
    start_hidden: bool,
    /// quit 前是否先把窗口类还原（默认 true；=0 用于复现「不还原即崩」）。
    revert_on_quit: bool,
    cmd_path: PathBuf,
    pos: Option<[f32; 2]>,
}

impl Spec {
    fn from_env() -> Self {
        let mode = match std::env::var("POC1B_MODE").as_deref() {
            Ok("plain") => Mode::Plain,
            Ok("none") => Mode::None,
            _ => Mode::Subclass,
        };
        Self {
            mode,
            nonactivating: std::env::var("POC1B_NONACTIVATING").as_deref() != Ok("0"),
            level: match std::env::var("POC1B_LEVEL").as_deref() {
                Ok("status") => Level::Status,
                _ => Level::Floating,
            },
            show: match std::env::var("POC1B_SHOW").as_deref() {
                Ok("winit") => ShowMode::Winit,
                _ => ShowMode::OrderFront,
            },
            take_key: std::env::var("POC1B_KEY").as_deref() != Ok("0"),
            accessory: std::env::var_os("POC1B_ACCESSORY").is_some(),
            start_hidden: std::env::var("POC1B_START_HIDDEN").as_deref() != Ok("0"),
            revert_on_quit: std::env::var("POC1B_REVERT_ON_QUIT").as_deref() != Ok("0"),
            cmd_path: std::env::var("POC1B_CMD")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp/poc1b.cmd")),
            pos: std::env::var("POC1B_POS").ok().and_then(|raw| {
                let (x, y) = raw.split_once(',')?;
                Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
            }),
        }
    }
}

/// 跨 Space 需要的 collection behavior 组合（PoC-1 沿用值，tauri-nspanel 示例同值）。
fn target_behavior() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary
}

/// 打印各屏幕全局坐标（macOS 全局坐标系原点在主屏左上角；供 POC1B_POS 选屏）。
fn dump_screens() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    for (i, screen) in NSScreen::screens(mtm).iter().enumerate() {
        let f = screen.frame();
        eprintln!(
            "[poc1b] screen[{i}] {}：frame=({}, {}) {}x{}",
            screen.localizedName(),
            f.origin.x,
            f.origin.y,
            f.size.width,
            f.size.height
        );
    }
}

/// 读类名（证据用）。
fn class_name(cls: &AnyClass) -> String {
    cls.name().to_string_lossy().into_owned()
}

/// 取对象当前类（`object_getClass`）。
fn obj_class(obj: *const AnyObject) -> &'static AnyClass {
    // SAFETY: obj 指向存活对象；类对象由运行时持有，永续。
    unsafe { &*objc2::ffi::object_getClass(obj) }
}

/// 运行期改类：NSWindow → NSPanel（或其子类）。返回旧类供取证。
///
/// 时序依 tauri-nspanel `FromWindow::from_window`：窗口已创建（本 spike 在 eframe 首个
/// Creator 闭包内——窗口已由 winit 建好、尚未显示、未进主循环；与 tauri 的 setup 时序同构）
/// → `object_setClass` → 立即施加 NSPanel 专属属性。
fn reclass(ns_window: &NSWindow, new_cls: &'static AnyClass) -> &'static AnyClass {
    let obj: *mut AnyObject = (ns_window as *const NSWindow).cast_mut().cast::<AnyObject>();
    // SAFETY: ns_window 指向本进程存活的 NSWindow；新类为 NSPanel 或其子类，
    // 实例大小与旧类一致（调用方已断言并打印），满足 object_setClass 的前提。
    unsafe { &*objc2::ffi::object_setClass(obj, new_cls as *const AnyClass) }
}

/// 换类后的属性施加与回读（取证实证）。
struct ConvReport {
    class_before: String,
    class_after: String,
    /// 原类指针（还原用；tauri-nspanel `to_window` 同法）。
    orig_class: *const AnyClass,
    size_window: usize,
    size_target: usize,
    style_mask_before: NSWindowStyleMask,
    style_mask_after: NSWindowStyleMask,
    level_after: isize,
    behavior_after: NSWindowCollectionBehavior,
    floating_panel: bool,
    becomes_key_only_if_needed: bool,
    can_become_key: bool,
    can_become_main: bool,
}

fn convert_and_configure(ns_window: &NSWindow, spec: &Spec) -> ConvReport {
    let obj: *const AnyObject = (ns_window as *const NSWindow).cast::<AnyObject>();
    let class_before_cls = obj_class(obj);
    let class_before = class_name(class_before_cls);
    let size_window = class_before_cls.instance_size();

    let (class_after, size_target) = match spec.mode {
        Mode::None => (class_before.to_string(), size_window),
        Mode::Plain | Mode::Subclass => {
            let target: &'static AnyClass = match spec.mode {
                Mode::Plain => NSPanel::class(),
                _ => Poc1bPanel::class(),
            };
            let size_target = target.instance_size();
            assert_eq!(
                size_window, size_target,
                "实例大小不一致（NSWindow {size_window} vs 目标类 {size_target}）——object_setClass 会 UB"
            );
            let old = reclass(ns_window, target);
            eprintln!(
                "[poc1b] object_setClass：{} → {}（旧类回读 {}，实例大小 {}=={}）",
                class_before,
                class_name(target),
                class_name(old),
                size_window,
                size_target
            );
            (class_name(target), size_target)
        }
    };

    let style_mask_before = ns_window.styleMask();

    // 1) 浮动级别（沿用 PoC-1）。
    ns_window.setLevel(spec.level.ns_level());
    // 2) 跨 Space collection behavior（沿用 PoC-1）。
    ns_window.setCollectionBehavior(target_behavior());
    // 3) 非激活面板样式位（tauri-nspanel README：跨全屏必须）。
    if spec.nonactivating {
        ns_window.setStyleMask(style_mask_before | NSWindowStyleMask::NonactivatingPanel);
        // 改 styleMask 会打断键盘投递，须按 winit 自带修法补 first responder
        // （winit `window_delegate.rs::set_style_mask` 源码注释：「If we don't do this,
        // key handling will break (at least until the window is clicked again/etc.)」；
        // tauri-nspanel 的 `show_and_make_key` 亦补 `makeFirstResponder(contentView)`）。
        restore_first_responder(ns_window);
    }
    // 4) 失焦不隐藏（悬浮条要常驻）。
    ns_window.setHidesOnDeactivate(false);

    // NSPanel 专属三属性（换类后按 NSPanel 调用）。
    if spec.mode != Mode::None {
        // SAFETY: 已换类为 NSPanel（或其子类），按 NSPanel 解释该对象成立。
        let panel: &NSPanel = unsafe { &*(ns_window as *const NSWindow).cast::<NSPanel>() };
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
    }

    ConvReport {
        class_before,
        class_after,
        orig_class: class_before_cls as *const AnyClass,
        size_window,
        size_target,
        style_mask_before,
        style_mask_after: ns_window.styleMask(),
        level_after: ns_window.level(),
        behavior_after: ns_window.collectionBehavior(),
        floating_panel: spec.mode != Mode::None && as_panel(ns_window).isFloatingPanel(),
        becomes_key_only_if_needed: spec.mode != Mode::None
            && as_panel(ns_window).becomesKeyOnlyIfNeeded(),
        can_become_key: ns_window.canBecomeKeyWindow(),
        can_become_main: ns_window.canBecomeMainWindow(),
    }
}

/// 把内容视图（winit 的 `WinitView`）重新设为 first responder。
///
/// 该调用是 winit `set_style_mask` 自带修法的复刻：改 styleMask 会打断键盘投递链路
/// （实测：不补则合成按键/真实按键都进不了窗口）。
fn restore_first_responder(ns_window: &NSWindow) {
    let Some(view) = ns_window.contentView() else {
        eprintln!("[poc1b] contentView() 为空，无法补 first responder");
        return;
    };
    let responder: &objc2_app_kit::NSResponder = &view;
    let ok = ns_window.makeFirstResponder(Some(responder));
    eprintln!(
        "[poc1b] makeFirstResponder(contentView = {}) -> {ok}",
        class_name(obj_class((&*view as *const NSView).cast::<AnyObject>()))
    );
}

/// 按 NSPanel 解释窗口对象（仅换类后成立）。
fn as_panel(ns_window: &NSWindow) -> &NSPanel {
    // SAFETY: 调用方已确保类换为 NSPanel 或其子类。
    unsafe { &*(ns_window as *const NSWindow).cast::<NSPanel>() }
}

fn main() -> eframe::Result<()> {
    dump_screens();
    let spec = Spec::from_env();
    eprintln!(
        "[poc1b] 配置：mode={:?} nonactivating={} level={} show={:?} take_key={} accessory={} start_hidden={} cmd={}",
        spec.mode,
        spec.nonactivating,
        spec.level.label(),
        spec.show,
        spec.take_key,
        spec.accessory,
        spec.start_hidden,
        spec.cmd_path.display()
    );

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([600.0, 400.0])
        .with_decorations(false)
        .with_always_on_top()
        .with_transparent(true)
        .with_has_shadow(false);
    if let Some(pos) = spec.pos {
        eprintln!("[poc1b] POC1B_POS 指定窗口位置 = {pos:?}");
        viewport = viewport.with_position(pos);
    }

    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "poc1b-npanel",
        opts,
        Box::new(move |cc| {
            if spec.accessory {
                if let Some(mtm) = MainThreadMarker::new() {
                    let app = NSApplication::sharedApplication(mtm);
                    let ok = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
                    eprintln!("[poc1b] POC1B_ACCESSORY：setActivationPolicy(Accessory) -> {ok}");
                }
            }

            let mut app = PocApp {
                spec,
                ns_window: None,
                conv: None,
                want_hidden: false,
                level_drift: 0,
                behavior_drift: 0,
                style_drift: 0,
                text: String::new(),
                typed_seen: 0,
                focus_text: false,
            };

            match ns_window_from_cc(cc) {
                Ok(ns_window) => {
                    let conv = convert_and_configure(&ns_window, &app.spec);
                    eprintln!(
                        "[poc1b] 换类回读：class {} → {}；styleMask {:#x} → {:#x}；level={}；CanJoinAllSpaces={} FullScreenAuxiliary={}；isFloatingPanel={} becomesKeyOnlyIfNeeded={} canBecomeKeyWindow={} canBecomeMainWindow={}",
                        conv.class_before,
                        conv.class_after,
                        conv.style_mask_before.0,
                        conv.style_mask_after.0,
                        conv.level_after,
                        conv.behavior_after
                            .contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
                        conv.behavior_after
                            .contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
                        conv.floating_panel,
                        conv.becomes_key_only_if_needed,
                        conv.can_become_key,
                        conv.can_become_main,
                    );
                    app.want_hidden = app.spec.start_hidden;
                    app.ns_window = Some(ns_window);
                    app.conv = Some(conv);
                }
                Err(err) => eprintln!("[poc1b] 取窗失败：{err}"),
            }
            Ok(Box::new(app))
        }),
    )
}

/// 主路径（同 PoC-1）：`cc.window_handle()` → NSView → `NSView::window()`。
fn ns_window_from_cc(cc: &eframe::CreationContext<'_>) -> Result<Retained<NSWindow>, String> {
    let handle = cc
        .window_handle()
        .map_err(|e| format!("window_handle(): {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => {
            // SAFETY: ns_view 指向 winit 为本 app 创建、当前存活的 NSView（主线程持有），
            // 此处只借读出其所属 NSWindow，不改变所有权。
            let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
            view.window()
                .ok_or_else(|| "NSView::window() 返回 None".to_string())
        }
        other => Err(format!("非 AppKit 窗口句柄：{other:?}")),
    }
}

struct PocApp {
    spec: Spec,
    ns_window: Option<Retained<NSWindow>>,
    conv: Option<ConvReport>,
    /// 期望处于隐藏态（启动隐藏 + `hide` 命令置位；`logic()` 每帧纠偏）。
    want_hidden: bool,
    level_drift: u32,
    behavior_drift: u32,
    style_drift: u32,
    text: String,
    /// 收到过的文本输入字符数（合成按键自测的判据）。
    typed_seen: usize,
    /// `focus` 命令置位：下一帧把键盘焦点交给 TextEdit（让合成文本有落点）。
    focus_text: bool,
}

impl PocApp {
    /// 显示窗口：按 `POC1B_SHOW` 走两条路径。
    fn show(&mut self, ctx: &egui::Context) {
        self.want_hidden = false;
        match self.spec.show {
            ShowMode::OrderFront => {
                if let Some(ns) = &self.ns_window {
                    // tauri-nspanel `show_and_make_key` 的顺序：先补 first responder，
                    // 再 orderFrontRegardless（不激活 app、不切 Space）。
                    restore_first_responder(ns);
                    ns.orderFrontRegardless();
                    // 直接经 AppKit 显现时 egui 侧可见状态未变（info.visible 仍为 None，
                    // eframe 默认按可见处理），这里补一次重绘请求，保证内容立即绘出。
                    ctx.request_repaint();
                    eprintln!("[poc1b] show：orderFrontRegardless");
                }
            }
            ShowMode::Winit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                eprintln!("[poc1b] show：ViewportCommand::Visible(true)");
            }
        }
        if self.spec.take_key {
            self.take_key();
        }
    }

    /// 隐藏窗口：与显示路径对称（orderfront 模式走 AppKit `orderOut`，不动 egui 的 Visible 状态机）。
    fn hide(&mut self, ctx: &egui::Context) {
        self.want_hidden = true;
        match self.spec.show {
            ShowMode::OrderFront => {
                if let Some(ns) = &self.ns_window {
                    ns.orderOut(None);
                    eprintln!("[poc1b] hide：orderOut");
                }
            }
            ShowMode::Winit => {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                eprintln!("[poc1b] hide：ViewportCommand::Visible(false)");
            }
        }
    }

    /// 抢键盘焦点（非激活面板可成为 key window 而不激活 app）。
    /// 把窗口类还原为改类前的类（tauri-nspanel `to_window` 同法：关闭前先还原）。
    ///
    /// 依据：实测「改类后直接 Close」退出即崩（`WindowDelegate` 析构路径触发
    /// non-unwinding panic，见 poc-results.md），tauri-nspanel 的 `close_panel` 在关窗前
    /// 先 `object_setClass` 还原，故此处复刻。
    fn revert_to_original(&self) {
        let (Some(ns), Some(conv)) = (&self.ns_window, &self.conv) else {
            return;
        };
        if self.spec.mode == Mode::None {
            return;
        }
        let obj: *mut AnyObject = (ns.as_ref() as *const NSWindow).cast_mut().cast::<AnyObject>();
        let cur = obj_class(obj);
        if class_name(cur) == conv.class_before {
            return;
        }
        // SAFETY: conv.orig_class 是改类前实测读到的类指针（运行时持有，存活）；
        // 还原回去是改类的逆操作。
        let old = unsafe { &*objc2::ffi::object_setClass(obj, conv.orig_class) };
        eprintln!(
            "[poc1b] 类还原：{} → {}（原类回读 {}）",
            class_name(cur),
            conv.class_before,
            class_name(old)
        );
    }

    fn take_key(&mut self) {
        if let Some(ns) = &self.ns_window {
            ns.makeKeyWindow();
            eprintln!(
                "[poc1b] makeKeyWindow：isKeyWindow={} isMainWindow={}",
                ns.isKeyWindow(),
                ns.isMainWindow()
            );
        }
    }

    /// 合成按键（同步 `NSApp.sendEvent` 路径；与 `self_type` 的队列路径对照）。
    fn self_type_send(&mut self) {
        let Some(ns) = self.ns_window.as_ref() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let ts = NSProcessInfo::processInfo().systemUptime();
        let window_number = ns.windowNumber();
        for (code, ch) in [(35u16, "p"), (31u16, "o"), (8u16, "c")] {
            let chars = NSString::from_str(ch);
            if let Some(event) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                NSEventType::KeyDown,
                NSPoint { x: 0.0, y: 0.0 },
                NSEventModifierFlags::empty(),
                ts,
                window_number,
                None,
                &chars,
                &chars,
                false,
                code,
            ) {
                app.sendEvent(&event);
            }
        }
        eprintln!(
            "[poc1b] 已同步 sendEvent 合成按键 p/o/c（isActive={}，isKeyWindow={}）",
            app.isActive(),
            ns.isKeyWindow()
        );
    }

    /// 进程内合成按键自测：向本 app 事件队列投递 `p` `o` `c` 三键。
    ///
    /// 无授权依赖（不走 CGEventPost/System Events，仅 `NSApp.postEvent:atStart:` 投递到自身
    /// 事件队列），可验证「换类后 AppKit → winit → egui」输入链路是否仍通。
    fn self_type(&mut self) {
        let Some(ns) = self.ns_window.as_ref() else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let ts = NSProcessInfo::processInfo().systemUptime();
        let window_number = ns.windowNumber();
        // 美式键盘键码（winit macos 键盘表同源）：p=35、o=31、c=8。
        for (code, ch) in [(35u16, "p"), (31u16, "o"), (8u16, "c")] {
            let chars = NSString::from_str(ch);
            let empty = NSString::from_str("");
            for (ty, keys, flags) in [
                (
                    NSEventType::KeyDown,
                    &chars,
                    NSEventModifierFlags::empty(),
                ),
                (NSEventType::KeyUp, &empty, NSEventModifierFlags::empty()),
            ] {
                let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                    ty,
                    NSPoint { x: 0.0, y: 0.0 },
                    flags,
                    ts,
                    window_number,
                    None,
                    keys,
                    keys,
                    false,
                    code,
                );
                match event {
                    // at_start=false：按键入顺序入队（at_start=true 会反序）。
                    Some(event) => app.postEvent_atStart(&event, false),
                    None => eprintln!("[poc1b] 合成按键构造失败：{ch}/{ty:?}"),
                }
            }
        }
        eprintln!(
            "[poc1b] 已投递合成按键 p/o/c（进程内 postEvent，windowNumber={window_number}，app isActive={}）；累计收到文本 {} 字",
            app.isActive(),
            self.text.chars().count()
        );
    }

    /// 每帧核对属性是否被 winit/系统覆盖；漂移即重施并计数（证据留痕）。
    fn check_and_reassert(&mut self) {
        let Some(ns) = &self.ns_window else {
            return;
        };
        let want_level = self.spec.level.ns_level();
        if ns.level() != want_level {
            self.level_drift += 1;
            if self.level_drift <= 3 {
                eprintln!(
                    "[poc1b] level 漂移（第 {} 次）：实测 {}，重施 {}",
                    self.level_drift,
                    ns.level(),
                    want_level
                );
            }
            ns.setLevel(want_level);
        }
        let want_behavior = target_behavior();
        if !ns.collectionBehavior().contains(want_behavior) {
            self.behavior_drift += 1;
            if self.behavior_drift <= 3 {
                eprintln!(
                    "[poc1b] collectionBehavior 漂移（第 {} 次），重施",
                    self.behavior_drift
                );
            }
            ns.setCollectionBehavior(want_behavior);
        }
        if self.spec.nonactivating
            && !ns
                .styleMask()
                .contains(NSWindowStyleMask::NonactivatingPanel)
        {
            self.style_drift += 1;
            if self.style_drift <= 3 {
                eprintln!(
                    "[poc1b] styleMask 漂移（第 {} 次），重施 NonactivatingPanel",
                    self.style_drift
                );
            }
            ns.setStyleMask(ns.styleMask() | NSWindowStyleMask::NonactivatingPanel);
            // 与初始施加一致：改 styleMask 会打断键盘投递，补 first responder。
            restore_first_responder(ns);
        }
    }

    /// 轮询触发器文件（隐藏期 `logic()` 仍在跑，见 PoC-1 结论）。
    fn poll_cmd(&mut self, ctx: &egui::Context) {
        let path = &self.spec.cmd_path;
        let Ok(cmd) = std::fs::read_to_string(path) else {
            return;
        };
        let _ = std::fs::remove_file(path);
        let cmd = cmd.trim();
        eprintln!("[poc1b] 收到命令：{cmd}");
        match cmd {
            "show" => self.show(ctx),
            "hide" => self.hide(ctx),
            "key" => self.take_key(),
            // 激活 app（注意：会切到本 app 所在 Space，仅用于「输入链路自测」，
            // 不能在「跨全屏 Space 可见」测试期间使用）。
            "activate" => {
                if let Some(mtm) = MainThreadMarker::new() {
                    let app = NSApplication::sharedApplication(mtm);
                    app.activate();
                    eprintln!("[poc1b] activate：isActive={}", app.isActive());
                }
            }
            "type" => self.self_type(),
            "typesend" => self.self_type_send(),
            "focus" => self.focus_text = true,
            "quit" => {
                if self.spec.revert_on_quit {
                    self.revert_to_original();
                }
                eprintln!("[poc1b] quit：走 ViewportCommand::Close");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            "revert" => self.revert_to_original(),
            other => eprintln!("[poc1b] 未知命令：{other}"),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let (esc, f1, h, f2, l) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::F1),
                i.key_pressed(egui::Key::H),
                i.key_pressed(egui::Key::F2),
                i.key_pressed(egui::Key::L),
            )
        });

        if esc {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if f1 || (h && !typing) {
            self.hide(ctx);
            eprintln!("[poc1b] F1/H 隐藏，{REVEAL_AFTER:?} 后自动再显示");
            // 隐藏后无键盘焦点，用后台线程定时触发（与产品里全局热键线程唤起同构）。
            let ctx = ctx.clone();
            let show = self.spec.show;
            std::thread::spawn(move || {
                std::thread::sleep(REVEAL_AFTER);
                match show {
                    ShowMode::Winit => ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true)),
                    // orderfront 模式不能跨线程调 AppKit；改为写触发器文件，
                    // 由主线程 `logic()` 轮询后执行（同 hotkey 线程 → 主线程派发结构）。
                    ShowMode::OrderFront => {
                        let _ = std::fs::write("/tmp/poc1b.cmd", "show");
                    }
                }
                eprintln!("[poc1b] 隐藏后自动再显示（{show:?}）");
            });
        }

        if f2 || (l && !typing) {
            self.spec.level = self.spec.level.toggle();
            if let Some(ns) = &self.ns_window {
                ns.setLevel(self.spec.level.ns_level());
            }
            eprintln!("[poc1b] 窗口级别切换为 {}", self.spec.level.label());
        }
    }
}

impl eframe::App for PocApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    /// 非渲染逻辑：跑在每帧渲染前，窗口隐藏时也会被补跑。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_cmd(ctx);
        // 诊断：任何进到 egui 的输入事件都留痕（合成按键自测用）。
        let (ev_count, ev_kinds) = ctx.input(|i| {
            let kinds: Vec<String> = i
                .events
                .iter()
                .map(|e| match e {
                    egui::Event::Text(t) => format!("Text({t})"),
                    egui::Event::Key { key, .. } => format!("Key({key:?})"),
                    other => format!("{other:?}").chars().take(24).collect(),
                })
                .collect();
            (i.events.len(), kinds)
        });
        if ev_count > 0 {
            eprintln!("[poc1b] egui 收到 {ev_count} 个输入事件：{ev_kinds:?}");
        }
        self.check_and_reassert();
        // 启动隐藏：eframe 首帧后会 `set_visible(true)`，这里每帧纠偏压回隐藏态。
        if self.want_hidden {
            if let Some(ns) = &self.ns_window {
                if ns.isVisible() {
                    ns.orderOut(None);
                    eprintln!("[poc1b] 启动隐藏：orderOut");
                }
            }
        }
        ctx.request_repaint_after(CHECK_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_keys(ui.ctx());

        if let Some(ns) = &self.ns_window {
            if ns.isKeyWindow() {
                self.typed_seen = self.text.chars().count();
            }
        }

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_black_alpha(220))
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                ui.heading("PoC-1 续：NSPanel 改类 → 跨全屏 Space");
                ui.add_space(4.0);
                ui.label(format!(
                    "模式={:?} ｜ 级别={} ｜ 非激活面板={} ｜ 显示={}",
                    self.spec.mode,
                    self.spec.level.label(),
                    self.spec.nonactivating,
                    self.spec.show.label()
                ));
                match &self.conv {
                    Some(conv) => {
                        ui.label(format!(
                            "换类：{} → {}（实例大小 {} / 目标类 {}；styleMask {:#x} → {:#x}）",
                            conv.class_before,
                            conv.class_after,
                            conv.size_window,
                            conv.size_target,
                            conv.style_mask_before.0,
                            conv.style_mask_after.0
                        ));
                        ui.label(format!(
                            "回读：level={}；CanJoinAllSpaces={}；FullScreenAuxiliary={}；isFloatingPanel={}；becomesKeyOnlyIfNeeded={}；canBecomeKeyWindow={}",
                            conv.level_after,
                            conv
                                .behavior_after
                                .contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
                            conv
                                .behavior_after
                                .contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
                            conv.floating_panel,
                            conv.becomes_key_only_if_needed,
                            conv.can_become_key
                        ));
                        ui.label(format!(
                            "漂移：level {}；behavior {}；styleMask {}（每帧核对，漂移即重施）",
                            self.level_drift, self.behavior_drift, self.style_drift
                        ));
                    }
                    None => {
                        ui.colored_label(egui::Color32::RED, "取窗失败，未施补丁（见 stderr）");
                    }
                }

                ui.separator();
                ui.label("焦点测试（点这里打字）：");
                let resp = ui.text_edit_singleline(&mut self.text);
                if self.focus_text {
                    resp.request_focus();
                    self.focus_text = false;
                    eprintln!("[poc1b] 已把键盘焦点交给 TextEdit");
                }
                if resp.changed() {
                    eprintln!(
                        "[poc1b] TextEdit 收到输入：{:?}（累计 {} 字）",
                        self.text,
                        self.text.chars().count()
                    );
                }
                ui.label(format!(
                    "已收到文本 {} 字｜key window={}",
                    self.typed_text_len(),
                    self.ns_window.as_ref().is_some_and(|w| w.isKeyWindow())
                ));

                ui.separator();
                ui.label("ESC 退出 ｜ F1 / H 隐藏（3 秒自动再显示）｜ F2 / L 切换级别");
                ui.label("自动化：向 POC1B_CMD 文件写 show / hide / key / type / quit。");
            });
    }
}

impl PocApp {
    fn typed_text_len(&self) -> usize {
        self.text.chars().count()
    }
}

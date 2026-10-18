//! macOS 平台补丁实现(objc2;unsafe 集中于此,SAFETY 逐处注释)。
//!
//! Task 14 产品化 PoC-1 续 spike(NSPanel 改类五要素+两补丁;活样例
//! `scripts/poc/poc1b_nspanel/src/main.rs`,结论见 docs/specs/v2-ui/poc-results.md):
//!
//! 1. 自定义 NSPanel 子类(`MacePalettePanel`,覆写 `canBecomeKeyWindow`/
//!    `canBecomeMainWindow` → true)→ `object_setClass` 运行期换类;
//! 2. `setLevel(Floating)`;
//! 3. `setStyleMask(原 mask | NonactivatingPanel)` —— 跨他人全屏 Space 的硬条件;
//! 4. `setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)`;
//! 5. 显示走 `orderFrontRegardless`(不激活 app、不切 Space;
//!    winit `Visible(true)` 走 `makeKeyAndOrderFront` 会激活 app,弃用);
//! 6. NSPanel 专属:`setFloatingPanel(true)`/`setBecomesKeyOnlyIfNeeded(false)`/
//!    `setHidesOnDeactivate(false)`。
//!
//! 两必带补丁:
//! - a) 改 styleMask 后必须补 `makeFirstResponder(contentView)`(winit
//!   `WindowDelegate::set_style_mask` 的同款修法;不做则键盘投递链断);
//! - b) 窗口销毁/退出前必须还原窗口类(改类后 winit drop 路径
//!   `panic in a function that cannot unwind` → abort;spike 三方对照实证)。
//!   Task 14 视口常驻(隐藏走 orderOut 不销毁),还原点收敛到进程退出(`revert_class`,
//!   由 `ui::App::on_exit` 调用)。
//!
//! 切 Space 失焦豁免(Task 14b):`SpaceObserver` 注册
//! `NSWorkspaceActiveSpaceDidChangeNotification`(经 `NSWorkspace.notificationCenter()`,
//! 非默认通知中心),回调记录变更时刻入全局 `SPACE_CHANGE`;ui 侧经 facade
//! `last_space_change` 读,按窗口期判定失焦是否豁免(判定逻辑纯函数在 facade)。
//!
//! 状态是**主线程单例**(thread_local):悬浮条窗口进程内只有一个,视图层(根视口
//! logic / 悬浮条回调)经 facade 自由函数访问。非主线程调用由 `MainThreadMarker`
//! 凭证拦下(返回 Err/直接 no-op),不静默越线。Space 变更记录例外:走全局
//! `Mutex`(通知投递线程文档未承诺为主线程,不预设前提)。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use eframe::egui;
use objc2::ffi::NSInteger;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, AnyThread, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication, NSBackingStoreType,
    NSBezierPath, NSColor, NSEvent, NSFloatingWindowLevel, NSImage, NSMenu, NSMenuItem,
    NSModalResponseAbort, NSModalResponseCancel, NSModalResponseOK, NSOpenPanel, NSPanel,
    NSResponder, NSScreen, NSView, NSViewLayerContentsPlacement, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowCollectionBehavior, NSWindowOrderingMode, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification,
};
use objc2_foundation::{
    NSLocale, NSNotification, NSObject, NSPoint, NSRect, NSSize, NSString, NSURLCustomIconKey,
    NSURL,
};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 悬浮条窗口标题(定义在 facade,ui 与平台层共用;`NSApp.windows()` 中按此定位)。
use super::{
    monotonic_ms, palette_width, settings_window_titles, SpaceChange, ICON_PX, PALETTE_TITLE,
};

/// 面板顶边位置:面板顶边距**可见区顶**(`visibleFrame`,已扣菜单栏)固定 110。
/// 视觉稿 y=110 是含 28px 菜单栏的画布坐标,故物理位置比画布低 28;
/// 2026-10-17 定案,替代旧「可见区顶部向下 1/4」。
///
/// Task 9 起窗口高 = 面板高 + 形象条:屏幕坐标(y 向下)窗口顶 = 面板顶 − 形象条,
/// AppKit 坐标(y 向上)窗口顶 y = 面板顶 y + 形象条(`show_palette` 的 `top_inset`
/// 参数)。本常量仍只约束**面板**口径。
const TOP_INSET: f64 = 110.0;
/// 与屏/可见区边缘的最小留白(定位 clamp 用)。
const EDGE_MARGIN: f64 = 8.0;

// 自定义 NSPanel 子类:复刻 winit `WinitWindow` 的两项覆写(恒 true;
// winit 0.30.13 `platform_impl/macos/window.rs`),保证换类后键盘可达
// (裸 NSPanel 的 `canBecomeKeyWindow` 为 false → 收不到键盘,spike 实测)。
define_class!(
    // SAFETY(define_class 的 unsafe 契约):
    // - 超类 NSPanel 无子类化约束(tauri-nspanel 同法);
    // - 本类不实现 Drop、无新增实例变量(ivars 缺省 `()`),实例大小与超类一致——
    //   这正是 `object_setClass` 的硬要求(运行期断言,见 `convert`);
    // - 覆写方法签名与 AppKit 一致(两个均无参返回 BOOL)。
    #[unsafe(super(NSPanel))]
    #[thread_kind = MainThreadOnly]
    #[name = "MacePalettePanel"]
    struct MacePalettePanel;

    impl MacePalettePanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            true
        }
    }

    unsafe impl NSObjectProtocol for MacePalettePanel {}
);

// Space 变更通知观察者(注册于 NSWorkspace 的通知中心;回调仅记录时刻,不碰窗口)。
define_class!(
    // SAFETY(define_class 的 unsafe 契约):超类 NSObject 无子类化约束;
    // 无新增实例变量(ivars 缺省 `()`);方法签名与 AppKit 一致(单参 NSNotification);无 Drop。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MaceSpaceObserver"]
    struct SpaceObserver;

    impl SpaceObserver {
        #[unsafe(method(activeSpaceDidChange:))]
        fn active_space_did_change(&self, _notification: &NSNotification) {
            note_space_change();
        }
    }

    unsafe impl NSObjectProtocol for SpaceObserver {}
);

impl SpaceObserver {
    /// 创建观察者实例(主线程;实例由调用方持有——通知中心持弱引用,不保活)。
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY:this 为主线程刚分配、ivars 已置的实例;本类未覆写 init,走 NSObject。
        unsafe { msg_send![super(this), init] }
    }
}

/// 最近一次 Space 变更(全局:通知投递线程不预设为主线程,故不走 thread_local)。
static SPACE_CHANGE: Mutex<Option<SpaceChange>> = Mutex::new(None);

/// Space 变更后的重绘钩子(egui Context 线程安全;通知回调线程直接请求重绘)。
/// 切到他人全屏 Space 时 eframe 可能不主动跑悬浮条视口,窗口会停留在一帧空白内容;
/// 通知到达后主动唤醒视口(并延迟再补两帧,覆盖窗口服务器完成 Space 切换的时序)。
static SPACE_REPAINT: Mutex<Option<egui::Context>> = Mutex::new(None);

/// 安装 Space 变更重绘钩子(App::new 调用一次;后装覆盖前装)。
pub fn install_space_repaint(ctx: egui::Context) {
    let mut slot = match SPACE_REPAINT.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Some(ctx);
}

/// 记录一次 Space 变更(通知回调):序号递增 + 记时刻 + 留痕(实机调参取证)。
fn note_space_change() {
    let mut guard = match SPACE_CHANGE.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let seq = guard.map(|c| c.seq).unwrap_or(0) + 1;
    *guard = Some(SpaceChange {
        seq,
        at: std::time::Instant::now(),
    });
    drop(guard);
    log::info!("Space 变更通知(第 {seq} 次;t+{}ms)", monotonic_ms());
    // 全屏 Space 切换后 eframe 可能不跑悬浮条视口:窗口内容会停留在切 Space 前的
    // 空帧(用户实测「全屏里悬浮条只有形象/空白,点一下才出内容」)。通知线程直接
    // 请求重绘(Context 线程安全)+ 主队列重排垫窗/面板层级;延迟两帧覆盖窗口服务器
    // 完成 Space 动画的时序。
    let ctx = SPACE_REPAINT.lock().ok().and_then(|guard| guard.clone());
    if let Some(ctx) = ctx {
        ctx.request_repaint();
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
        ctx.request_repaint_after(std::time::Duration::from_millis(360));
    }
    dispatch2::DispatchQueue::main().exec_async(refresh_palette_after_space_change);
}

/// Space 切换后恢复悬浮条窗口层级并重绘(主线程)。
///
/// 垫窗(NSVisualEffectView 的独立窗口)与面板同 level;窗口服务器在跨 Space 时
/// 可能打乱同级窗顺序,垫窗会盖在面板上——表现为「悬浮条只剩顶部形象、面板内容
/// 全白/空白,点一下才恢复」。这里把面板置前、垫窗压回面板之下,并标记重绘。
fn refresh_palette_after_space_change() {
    let Some(_mtm) = MainThreadMarker::new() else {
        return; // 主队列回调必在主线程;理论不可达
    };
    let (panel, blur, blur_visible) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.palette.clone(), st.blur.clone(), st.blur_visible)
    });
    let Some(panel) = panel else {
        return; // 视口尚未创建
    };
    if !panel.isVisible() {
        return; // 悬浮条隐藏时不抢层级
    }
    panel.orderFrontRegardless();
    if blur_visible {
        if let Some(blur) = blur {
            // 面板先置前、垫窗再压到面板之下:跨 Space 后同级窗顺序可能已翻转。
            blur.orderWindow_relativeTo(NSWindowOrderingMode::Below, panel.windowNumber());
            blur.displayIfNeeded();
        }
    }
    if let Some(view) = panel.contentView() {
        view.setNeedsDisplay(true);
    }
    panel.displayIfNeeded();
    log::info!(
        "Space 变更后悬浮条重排层级/重绘:isVisible={} isKey={} blur_visible={blur_visible}",
        panel.isVisible(),
        panel.isKeyWindow()
    );
}

/// 最近一次 Space 变更(facade 转发;None = 进程内从未收到)。
pub fn last_space_change() -> Option<SpaceChange> {
    match SPACE_CHANGE.lock() {
        Ok(g) => *g,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// 系统首选语言（按偏好顺序；macOS `NSLocale.preferredLanguages`）。
pub fn preferred_languages() -> anyhow::Result<Vec<String>> {
    let langs = NSLocale::preferredLanguages();
    Ok(langs.iter().map(|s| s.to_string()).collect())
}

/// 创建并注册 Space 变更观察者(进程内一次;主线程)。
///
/// 重入安全:创建 + 注册全程**不持 PALETTE 借用**(AppKit 调用可能被 winit 同步派发
/// → 重入一帧 → 帧内 `reanchor`/`ensure_patched` 会 borrow_mut;持借用即 panic)。
/// 返回值由调用方经短借用存入状态保活。
fn make_space_observer(mtm: MainThreadMarker) -> Retained<SpaceObserver> {
    let center = NSWorkspace::sharedWorkspace().notificationCenter();
    let observer = SpaceObserver::new(mtm);
    // SAFETY:observer 为本进程存活对象(Retained 存于 PaletteState 保活,通知中心持弱引用);
    // selector 为本类实装方法,签名匹配;通知名为 AppKit 常量。
    unsafe {
        center.addObserver_selector_name_object(
            &observer,
            objc2::sel!(activeSpaceDidChange:),
            Some(NSWorkspaceActiveSpaceDidChangeNotification),
            None,
        );
    }
    log::info!("Space 变更通知观察者已注册(NSWorkspaceActiveSpaceDidChangeNotification)");
    observer
}

/// 漂移计数(与 PoC-1 续同口径:每帧核对,漂移即重施并留痕)。
///
/// `Copy`:漂移核对在借用外做(AppKit 调用可能重入一帧),计数取出→核对→写回。
#[derive(Default, Clone, Copy)]
struct Drift {
    level: u32,
    behavior: u32,
    style: u32,
}

/// 定位锚:显示时按下述值摆位;内容增高/收缩时「顶边钉住」重锚。
#[derive(Clone, Copy)]
struct Anchor {
    x: f64,
    /// 顶边 y(Cocoa 全局坐标,原点主屏左下、y 向上)。
    top_y: f64,
    /// 上次锚定时窗口高度。
    h: f64,
}

/// 悬浮条窗口平台状态(主线程单例)。
#[derive(Default)]
struct PaletteState {
    /// 根窗(启动时记录):从悬浮条查找中排除。
    root: Option<Retained<NSWindow>>,
    /// 悬浮条窗(视口窗创建后首见即缓存;持引用保活)。
    palette: Option<Retained<NSWindow>>,
    /// 换类前的原类(还原用;tauri-nspanel `to_window` 同法)。
    orig_class: Option<*const AnyClass>,
    /// 换类前类名(诊断 + 还原判据)。
    class_before: String,
    /// 定位锚(显示时置;高度变化时重锚)。
    anchor: Option<Anchor>,
    /// Space 变更观察者(持引用保活;通知中心持弱引用,不保活)。
    space_observer: Option<Retained<SpaceObserver>>,
    drift: Drift,
    /// 毛玻璃垫窗(PoC-7 定案:面板窗下挂同级窗,承载 `NSVisualEffectView`)。
    blur: Option<Retained<NSWindow>>,
    /// 垫窗内容视图(持引用保活)。
    vib: Option<Retained<NSVisualEffectView>>,
    /// 上帧垫窗是否可见(显隐边沿检测:只在翻转时调 orderFront/orderOut)。
    blur_visible: bool,
}

thread_local! {
    static PALETTE: RefCell<PaletteState> = RefCell::new(PaletteState::default());
}

/// 鼠标任意键当前是否按下(全局状态快照;NSEvent.pressedMouseButtons)。
///
/// 免权限:与全局事件监听(addGlobalMonitorForEvents)不同,这是状态查询,
/// 实机探针实证(2026-10-16)注入点击亦更新该状态。用途:失焦/待决期的
/// 「点击导致」快判(窗口期豁免之外的那半——product §1「点击其他应用照常隐藏」)。
pub fn mouse_button_down() -> bool {
    NSEvent::pressedMouseButtons() != 0
}

/// 指针是否落在悬浮条窗 frame 内(ghost 态点击豁免判定用;未接上窗 → false)。
pub fn palette_contains_mouse() -> bool {
    let Some(_mtm) = MainThreadMarker::new() else {
        return false; // 非主线程:调用方在主线程
    };
    PALETTE.with(|cell| {
        let st = cell.borrow();
        let Some(win) = st.palette.as_ref() else {
            return false; // 视口窗未创建
        };
        let f = win.frame();
        let p = NSEvent::mouseLocation();
        p.x >= f.origin.x
            && p.x < f.origin.x + f.size.width
            && p.y >= f.origin.y
            && p.y < f.origin.y + f.size.height
    })
}

/// 记录根窗句柄(创建期一次;仅用于排除同名窗口)。
pub fn attach_root(cc: &eframe::CreationContext<'_>) {
    let Ok(handle) = cc.window_handle() else {
        log::warn!("根窗句柄取不到(attach_root):平台层仍可工作");
        return;
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        log::warn!("根窗非 AppKit 句柄:平台层跳过");
        return;
    };
    // SAFETY:h.ns_view 指向 winit 为本 app 创建、当前存活的 NSView(主线程持有);
    // 只借读出其所属 NSWindow,不改变所有权。
    let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
    let Some(window) = view.window() else {
        log::warn!("根窗 NSView::window() 为空:平台层跳过");
        return;
    };
    PALETTE.with(|cell| cell.borrow_mut().root = Some(window));
}

/// 每帧核对:找悬浮条窗(首见即施加五要素+两补丁);已接则核对漂移。
///
/// 视口窗由 eframe 在「注册该视口的 pass」结束时创建,故首帧找不到属正常,
/// 下帧再试(调用方在未接上时排下一帧)。
///
/// 重入安全(硬要求):本函数也从**主队列直调块**调用(hotkey.rs 的直显路径),
/// 而 AppKit 调用可能被 winit 同步派发(`maybe_queue_event`:不在事件处理中即
/// 立即 `handle_event`)→ 重入一帧 → 帧内 `draw_palette` 会 `borrow_mut` PALETTE。
/// 故状态访问一律收窄成「短借用」:原生调用(改类/属性/first responder/漂移重施)
/// 全在借用外。
pub fn ensure_patched() -> Result<()> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("ensure_patched 非主线程调用"));
    };
    // ① 短借用:状态快照(是否需要注册观察者 / 是否已接窗 / 根窗句柄)
    let (need_observer, attached, root) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (
            st.space_observer.is_none(),
            st.palette.is_some(),
            st.root.clone(),
        )
    });
    if need_observer {
        let observer = make_space_observer(mtm); // 借用外
        PALETTE.with(|cell| cell.borrow_mut().space_observer = Some(observer));
    }
    if !attached {
        let Some(win) = find_palette_window(mtm, root.as_deref()) else {
            return Ok(()); // 视口窗未创建;下帧再试
        };
        // ② 借用外:换类 + 五要素 + 补丁 a(可重入,不持状态借用)
        let (orig_class, class_before) = convert(&win)?;
        PALETTE.with(|cell| {
            let mut st = cell.borrow_mut();
            st.orig_class = Some(orig_class);
            st.class_before = class_before;
            st.palette = Some(win);
        });
    }
    // ③ 漂移核对(借用外;计数取出→核对→写回)
    let (win, drift) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.palette.clone(), st.drift)
    });
    let Some(win) = win else {
        return Ok(()); // 上面刚置位,理论不可达
    };
    let mut drift = drift;
    reassert(&mut drift, &win);
    PALETTE.with(|cell| cell.borrow_mut().drift = drift);
    Ok(())
}

/// 唤起悬浮条:补丁 → 多屏定位(鼠标所在屏,面板顶边固定 110、宽度按屏回填)→
/// 补 first responder → `orderFrontRegardless`(不激活 app、不切 Space)→
/// `makeKeyWindow`(成 key 不夺前台,PoC-1b 结论 #6)。
///
/// `top_inset` = 窗口顶部形象条高(Task 9):窗口顶在面板顶之上。屏幕坐标(y 向下)
/// 口径为「窗口顶 = 面板顶 − top_inset」;AppKit 坐标(y 向上)口径为
/// 「窗口顶 y = 面板顶 y + top_inset」。面板顶边仍距可见区顶 110。
/// 返回实际面板宽(ui 层据此同步布局宽;Task 11)。
///
/// 重入安全(硬要求):本函数会从**主队列直调块**(hotkey.rs 的直显路径)调用。
/// `setFrameOrigin`/`setContentSize` 会触发 `windowDidMove`/`windowDidResize` 通知,
/// 而 winit 的 `maybe_queue_event` 在「不在事件处理中」时**同步派发** → 重入一帧
/// (实测栈:setFrameOrigin → window_did_move → eframe run_ui_and_paint →
/// draw_palette)。故:
/// ① 全程不持 PALETTE 借用(帧内 `reanchor`/`ensure_patched` 会 borrow_mut);
/// ② 窗口句柄经短借用取出(Retained 保活),后续原生调用全在借用外。
pub fn show_palette(top_inset: f64) -> Result<f64> {
    ensure_patched()?;
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("show_palette 非主线程调用"));
    };
    let win = PALETTE
        .with(|cell| cell.borrow().palette.clone())
        .ok_or_else(|| anyhow!("悬浮条窗口未就绪(视口尚未创建)"))?;
    let (anchor, panel_w) = position_palette(&win, mtm, top_inset);
    // 顺序照 PoC-1b `show_and_make_key`:先补 first responder,再 orderFrontRegardless,
    // 最后 makeKeyWindow(定位先于显示,避免可见后跳位)。
    let responder_ok = restore_first_responder(&win);
    win.orderFrontRegardless();
    win.makeKeyWindow();
    PALETTE.with(|cell| cell.borrow_mut().anchor = Some(anchor)); // 短借用回写(重锚用)
    log::info!(
        "悬浮条唤起:origin=({:.0}, {:.0}) 窗口顶 y={:.0} 面板顶 y={:.0} 窗高 {:.0}(形象条 {top_inset:.0}) 宽 {panel_w:.0};first_responder={responder_ok} isKeyWindow={} isVisible={}",
        anchor.x,
        anchor.top_y - anchor.h,
        anchor.top_y,
        anchor.top_y - top_inset,
        anchor.h,
        win.isKeyWindow(),
        win.isVisible()
    );
    Ok(panel_w)
}

/// 显示完整窗口(根窗):激活 app + 置前 + 成 key/main。
///
/// 常规 App 窗口语义(product §2),区别于悬浮条的「不夺前台」;实测:非激活 app 下
/// 单发 `ViewportCommand::Visible(true)`(winit `makeKeyAndOrderFront`)不切 Space——
/// 根窗在别的 Space 时「命令生效但看不到窗口」,故此处先 `NSApp.activate`
/// (激活即带窗口切回其所在 Space)再置前。
pub fn show_root_window() -> Result<()> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("show_root_window 非主线程调用"));
    };
    // 重入安全(同 show_palette):句柄短借用取出,AppKit 调用全在借用外
    // (activate/orderFront 可被 winit 同步派发 → 重入一帧)。
    let root = PALETTE
        .with(|cell| cell.borrow().root.clone())
        .ok_or_else(|| anyhow!("根窗句柄未记录(attach_root 未执行)"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    // 最小化态下单发 orderFront/makeKey 不会从 Dock 恢复:先 deminiaturize
    //（与设置窗同法;2026-10-18 review 补）。
    if root.isMiniaturized() {
        root.deminiaturize(None);
        log::info!("完整窗口(根窗)最小化中:deminiaturize 恢复");
    }
    root.orderFrontRegardless();
    root.makeKeyWindow();
    root.makeMainWindow();
    let f = root.frame();
    log::info!(
        "完整窗口(根窗)唤起:isVisible={} isKey={} frame={:.0}x{:.0}@({:.0},{:.0})",
        root.isVisible(),
        root.isKeyWindow(),
        f.size.width,
        f.size.height,
        f.origin.x,
        f.origin.y
    );
    Ok(())
}

/// 设置窗口首次打开居中 + 置前(原生)。
///
/// - `center = true`(仅首次打开):按**鼠标所在屏**的 `visibleFrame`(已扣菜单栏/
///   Dock)居中,四周 clamp 到 `EDGE_MARGIN`;窗口大于可见区时保持左上可见
///   (顶部/左缘贴留白,底部/右侧允许溢出;标题栏/交通灯不得移出屏);
/// - 一律 `NSApp.activate()` + `orderFrontRegardless()` + `makeKeyWindow()`:
///   ROOT 隐藏(同 Space)时也能带到前台(与 `show_root_window` 同法,设置窗不需要
///   main 语义);最小化态先 `deminiaturize` 恢复。跨 Space 未做额外
///   collectionBehavior,待实机验证(不擅自新增 Space 行为)。
///
/// 窗口按标题定位(eframe 不暴露子视口原生窗回取 API);未创建/非主线程 → Err,
/// 调用方退回 egui `Visible(true)` + `Focus`。原生调用可能被 winit 同步派发
/// (重入一帧),故本函数不持任何共享状态借用——调用方须在 `Shared` 锁外调用。
pub fn show_settings_window(center: bool) -> Result<()> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("show_settings_window 非主线程调用"));
    };
    // 重入安全(同 show_root_window):状态短借用取出,AppKit 调用全在借用外。
    let root = PALETTE.with(|cell| cell.borrow().root.clone());
    let win = find_window_by_title(mtm, root.as_deref(), &settings_window_titles())
        .ok_or_else(|| anyhow!("设置窗口未就绪(视口尚未创建)"))?;
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    // 最小化态不会因 orderFrontRegardless/makeKeyWindow 自动从 Dock 恢复。
    if win.isMiniaturized() {
        win.deminiaturize(None);
        log::info!("设置窗口最小化中:deminiaturize 恢复");
    }
    if center {
        let visible = screen_with_point(mtm, NSEvent::mouseLocation())
            .map(|s| s.visibleFrame())
            .unwrap_or_else(|| {
                // 理论不可达(至少一屏):退化主屏可见区,仍给确定位置。
                log::warn!("设置窗口居中:鼠标所在屏查不到,退化主屏可见区");
                NSScreen::mainScreen(mtm)
                    .map(|s| s.visibleFrame())
                    .unwrap_or_else(|| {
                        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0))
                    })
            });
        let frame = win.frame();
        let origin = settings_origin(visible, frame.size.width, frame.size.height);
        win.setFrameOrigin(origin);
        log::info!(
            "设置窗口首次居中:visible=({:.0},{:.0} {:.0}x{:.0}) 窗口={:.0}x{:.0} origin=({:.0},{:.0})",
            visible.origin.x,
            visible.origin.y,
            visible.size.width,
            visible.size.height,
            frame.size.width,
            frame.size.height,
            origin.x,
            origin.y
        );
    }
    win.orderFrontRegardless();
    win.makeKeyWindow();
    let f = win.frame();
    log::info!(
        "设置窗口唤起:center={center} isVisible={} isKey={} frame={:.0}x{:.0}@({:.0},{:.0})",
        win.isVisible(),
        win.isKeyWindow(),
        f.size.width,
        f.size.height,
        f.origin.x,
        f.origin.y
    );
    Ok(())
}

/// 设置窗口原点(AppKit 坐标,左下角;纯函数,单测钉住):可见区内居中并 clamp
/// (四周留 `EDGE_MARGIN`)。
///
/// 窗口大于可见区时保持**左上**可见:水平贴左留白、垂直贴**顶边**留白
/// (AppKit y 向上,故原点 y 取可见区顶 − 窗高 − 留白;底部允许溢出)——
/// 标题栏/交通灯不得被挤出屏外。
fn settings_origin(visible: NSRect, w: f64, h: f64) -> NSPoint {
    let min_x = visible.origin.x + EDGE_MARGIN;
    let max_x = (visible.origin.x + visible.size.width - w - EDGE_MARGIN).max(min_x);
    let x = (visible.origin.x + (visible.size.width - w) / 2.0).clamp(min_x, max_x);

    let min_y = visible.origin.y + EDGE_MARGIN;
    let top_anchored_y = visible.origin.y + visible.size.height - h - EDGE_MARGIN;
    let centered_y = visible.origin.y + (visible.size.height - h) / 2.0;
    let y = if top_anchored_y >= min_y {
        centered_y.clamp(min_y, top_anchored_y)
    } else {
        // 窗高超过可见区:顶边优先(标题栏可见),底部溢出是既定的无法两全语义。
        top_anchored_y
    };
    NSPoint::new(x, y)
}

/// 系统目录选择器(阻塞;仅主线程调用——调用点须在 egui 主线程)。
///
/// 取消/失败 = None。实现:
/// ① 先激活 app:面板是独立窗口,app 不在前台时可能落在其他窗口之后;
/// ② `NSOpenPanel::openPanel` **新建**一个目录面板(每次调用独立实例,
///    不复用上次选择)→ 仅目录、单选、不选文件;
/// ③ `runModal()` 阻塞至用户确认/取消;`NSModalResponseOK` 才取首个 URL 的
///    本地路径(`NSURL.path`,已解码百分号转义)。取消(`Cancel`)/中止
///    (`Abort`,如模态会话被打断)分别留痕并返回 None。
///
/// 面板期间 egui 冻结(嵌套 run loop),与右键菜单/托盘菜单同款模态语义
/// (关闭后当帧恢复);产品接受。
pub fn pick_directory() -> Option<PathBuf> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("目录选择器:非主线程调用,忽略");
        return None;
    };
    // 先激活:不激活时面板可能被其他 app 的窗口挡住(用户误以为没弹出)。
    // 无参 activate 为 macOS 14+ API(既有 show_root_window/show_settings_window
    // 同款);最低版本仍按 README 的 macOS 13,不在本任务改动。
    NSApplication::sharedApplication(mtm).activate();
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseFiles(false);
    panel.setCanChooseDirectories(true);
    panel.setAllowsMultipleSelection(false);
    let response = panel.runModal(); // 阻塞:嵌套 run loop,返回即面板关闭
    if response == NSModalResponseAbort {
        log::warn!("目录选择器:模态会话中止(response={response})");
        return None;
    }
    if response == NSModalResponseCancel {
        log::info!("目录选择器:用户取消(response={response})");
        return None;
    }
    if response != NSModalResponseOK {
        log::info!("目录选择器:未确认(response={response})");
        return None;
    }
    let Some(url) = panel.URLs().firstObject() else {
        log::warn!("目录选择器:已确认但未返回 URL");
        return None;
    };
    let Some(path) = url.path() else {
        log::warn!("目录选择器:所选 URL 无本地路径");
        return None;
    };
    let path = PathBuf::from(path.to_string());
    if path.as_os_str().is_empty() {
        log::warn!("目录选择器:所选目录路径为空");
        return None;
    }
    log::info!("目录选择器:选中 {path:?}");
    Some(path)
}

/// 破坏性操作原生确认弹窗(阻塞;仅主线程调用——调用点在设置页动作落地,
/// 即 egui 主线程,且不得持共享锁)。
///
/// ① 先激活 app(设置窗可能在其他 app 之后);
/// ② `NSAlert` 第一个按钮「取消」:AppKit 给首按钮默认 Return 键等价,故按回车
///    只会取消;第二个破坏性按钮显式清空 Return 键等价,必须鼠标点选；
/// ③ `runModal()` 阻塞到用户选择；仅响应码为第二按钮时返回 true。
/// 取消/关闭/中止/非主线程均返回 false（宁可不执行破坏性操作）。
/// 注：AppKit 只自动给标题恰为 "Cancel" 的按钮绑定 Esc；中文「取消」不触发 Esc。
/// 两种语言下 Esc 都不会误确认(英文 Esc = 取消,中文 Esc 无显式动作)。
pub fn confirm_action(title: &str, message: &str, action_label: &str) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("确认弹窗:非主线程调用,按取消处理");
        return false;
    };
    NSApplication::sharedApplication(mtm).activate();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(message));
    alert.setAlertStyle(NSAlertStyle::Warning);
    // 按钮添加顺序 = 响应码顺序:取消在前(默认 Return),文案随当前语言。
    let cancel = alert.addButtonWithTitle(&NSString::from_str(crate::i18n::s().cancel));
    let action = alert.addButtonWithTitle(&NSString::from_str(action_label));
    cancel.setKeyEquivalent(&NSString::from_str("\r"));
    action.setKeyEquivalent(&NSString::from_str(""));
    let response = alert.runModal(); // 阻塞:嵌套 run loop,返回即弹窗关闭
    let confirmed = response == NSAlertSecondButtonReturn;
    log::info!(
        "确认弹窗:{title:?}({message:?}) action={action_label:?} response={response} confirmed={confirmed}"
    );
    confirmed
}

/// 显示原生右键菜单(阻塞;仅主线程调用——调用点在结果窗口绘制内,即 egui 主线程)。
///
/// `items` = (标题, tag);返回所选项的 tag(None = 取消/Esc/未选中)。
/// 位置为视图坐标(左上原点、y 向下);PoC-3 实测视图坐标与 egui 坐标同值可直接
/// 互用,此处保留换算核对(弹出前把鼠标窗口坐标经 `convertPoint_fromView` 换算
/// 一份并入日志),以实机观测验证该结论。
///
/// 三个 PoC-3 必知坑在此落地:
/// ① 无 action 的项默认被自动验证判禁用 → `setAutoenablesItems(false)` +
///    逐项 `setEnabled(true)`;
/// ② `NSMenu`/`NSMenuItem` 均 MainThreadOnly(alloc 需 mtm 凭证);
/// ③ `popUpMenuPositioningItem` 返回值 bool 语义不稳定(24 次成功选择里 1 次
///    false)→ **判定一律用 `highlightedItem()`**。
///
/// 弹出期间 egui 冻结(嵌套 run loop 走菜单跟踪模式;winit 对 modal 期事件排队
/// 至默认模式、关闭后投递,PoC-3 实测「关闭当帧恢复」)——已判定可接受。
/// 桥接不接 target/action:模态语义天然同步,阻塞返回 + 读 highlightedItem 即够
/// (PoC-3 定稿,见 plan Task 18)。
///
/// 弹出位置 = 点击点(不偏移):菜单左上角落在点击点,指针停在首项上会被预高亮
/// (PoC-3 观测)——与系统菜单同观感,产品接受(偏移放右下会让菜单脱离光标,
/// 反倒不像系统菜单;实机手感终验随 Task 18 人工清单)。
pub fn popup_menu(
    items: &[(&str, i64)],
    at_view_pos: (f64, f64),
    frame: &eframe::Frame,
) -> Option<i64> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("右键菜单:非主线程调用,忽略");
        return None;
    };
    let view = frame_view(frame)?;
    let menu = build_context_menu(mtm, items);
    let location = NSPoint::new(at_view_pos.0, at_view_pos.1);
    let mouse_view = view
        .window()
        .map(|w| view.convertPoint_fromView(w.mouseLocationOutsideOfEventStream(), None));
    let t0 = std::time::Instant::now();
    // 阻塞调用:内部为嵌套 run loop(菜单跟踪模式),返回即菜单关闭。
    let popped = menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(&view));
    // NSInteger = isize(64 位 mac 上无损;tag 恒为小整数,边界处转换一次)
    let choice = menu.highlightedItem().map(|item| item.tag() as i64);
    log::info!(
        "右键菜单:{} 项,位置=({:.1},{:.1}) 鼠标换算={:?},阻塞 {}ms,返回值={popped}(仅留痕),选择 tag={choice:?}",
        items.len(),
        location.x,
        location.y,
        mouse_view.map(|p| (p.x, p.y)),
        t0.elapsed().as_millis()
    );
    choice
}

// ---------------------------------------------------------------------------
// 托盘菜单自弹(2026-10-16 实机修复:副屏点击 → 菜单落在主屏)

/// 托盘菜单的 NSMenu 句柄(主队列块跨线程只携带「调用本模块函数」的意图,
/// 指针经本静态取回——裸指针非 Send,故以 usize 存)。
static TRAY_MENU: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

/// 注册托盘菜单(install 期主线程一次;`Menu::ns_menu()` 的裸指针)。
/// 重复注册保留首个并留痕(进程内只有一份托盘菜单)。
pub fn register_tray_menu(ns_menu: *mut std::ffi::c_void) {
    if TRAY_MENU.set(ns_menu as usize).is_err() {
        log::warn!("托盘菜单重复注册:保留首个,忽略本次");
    }
}

/// 在光标所在屏弹出托盘菜单(仅主线程调用;自弹,不经 tray-icon 内建路径)。
///
/// ## 为什么自弹(实机根因,2026-10-16)
///
/// tray-icon 的 macOS 实现是 `status_item.setMenu(menu)` + `button.performClick(None)`
/// ——菜单由 AppKit 按**状态项所在的菜单栏窗**锚定;多屏时该窗固定在主屏菜单栏
/// (实机取证:副屏点右键,tray-icon 事件里的窗 rect = 主屏状态项窗 (823,0,38,33)@2x,
/// 内建弹出的菜单窗随之落在主屏 x=818)。鼠标位置是对的(事件 position 与
/// `NSEvent::mouseLocation` 一致),错的是锚定窗。故此处按光标自定位弹出:
/// 菜单必然出现在用户点击的那块屏上。
///
/// 位置 = 光标所在屏、菜单栏底边(`visibleFrame` 顶)、左缘对齐光标 x;
/// 右缘越界时左移(状态项在菜单栏最右,近右缘必越界;同 muda `fit_anchor` 手法)。
/// 弹出阻塞(嵌套 run loop 走菜单跟踪模式),返回即菜单关闭——与 `popup_menu` 同款;
/// 菜单项选择仍经 muda 的 MenuEvent 通道回投(tray.rs 的菜单泵),此处不读返回值。
pub fn popup_tray_menu() {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("托盘菜单:非主线程调用,忽略");
        return;
    };
    let Some(&addr) = TRAY_MENU.get() else {
        log::warn!("托盘菜单:未注册(register_tray_menu 未执行),忽略");
        return;
    };
    // SAFETY:addr 是 register_tray_menu(主线程、install 期)写入的 NSMenu 裸指针,
    // muda 的 Menu 句柄持有该 NSMenu 至进程退出;本函数仅主线程访问,满足 NSMenu
    // 的主线程约束(nonnull 由注册路径保证:muda 的 ns_menu 恒指向存活对象)。
    let menu: &NSMenu = unsafe { &*(addr as *const NSMenu) };
    let mouse = NSEvent::mouseLocation();
    let screen = screen_with_point(mtm, mouse);
    let visible = match &screen {
        Some(screen) => screen.visibleFrame(),
        None => {
            // 理论不可达(至少一屏):退化主屏可见区,仍给确定位置。
            log::warn!("托盘菜单:光标所在屏查不到,退化主屏可见区");
            match NSScreen::mainScreen(mtm) {
                Some(main) => main.visibleFrame(),
                None => NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0)),
            }
        }
    };
    // 可见区已扣菜单栏时栏底即顶边(gap = 0);未扣(理论不可达)时下移兜底栏高,
    // 菜单不压菜单栏(见 screen_menu_bar_height 一族;实机取证:副屏在栏上时
    // visibleFrame 顶 = 984,恰为栏底;鼠标不在该屏时该值回到屏顶)。
    let own_bar = screen.as_deref().map_or(0.0, screen_menu_bar_height);
    let gap = if own_bar > 0.5 {
        0.0
    } else {
        fallback_menu_bar_height(mtm)
    };
    let size = menu.size();
    let at = tray_menu_anchor(mouse.x, visible, size.width, gap);
    let (x, y) = (at.x, at.y);
    let t0 = std::time::Instant::now();
    // 阻塞调用(同 popup_menu):嵌套 run loop,返回即菜单关闭。
    let popped = menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(x, y), None);
    log::info!(
        "托盘菜单弹出:位置=({x:.0},{y:.0}) 光标=({:.0},{:.0}) 屏栏高={own_bar:.0} 菜单={:.0}x{:.0} 阻塞 {}ms 返回={popped}(仅留痕)",
        mouse.x,
        mouse.y,
        size.width,
        size.height,
        t0.elapsed().as_millis()
    );
}

/// 托盘菜单锚点(纯函数,单测钉住):菜单左上角落点(AppKit 屏坐标,点)。
///
/// - x = 光标 x;右缘越界左移让出(muda `fit_anchor` 同法——状态项在菜单栏最右,
///   近右缘必越界),再 clamp 回左缘内(两侧各留 `EDGE_MARGIN`);
/// - y = 可见区顶边 - `unaccounted_bar`:可见区已扣菜单栏时栏底即顶边(传 0),
///   未扣时下移该值(菜单不压菜单栏)。
fn tray_menu_anchor(mouse_x: f64, visible: NSRect, menu_w: f64, unaccounted_bar: f64) -> NSPoint {
    let right = visible.origin.x + visible.size.width;
    let mut x = mouse_x;
    if x + menu_w > right {
        x = right - menu_w - EDGE_MARGIN;
    }
    let x = x.max(visible.origin.x + EDGE_MARGIN);
    let y = visible.origin.y + visible.size.height - unaccounted_bar;
    NSPoint::new(x, y)
}

/// 屏菜单栏高度(点)= frame 顶 - visibleFrame 顶;该屏当前无菜单栏(未扣)时为 0。
fn screen_menu_bar_height(s: &NSScreen) -> f64 {
    let f = s.frame();
    let v = s.visibleFrame();
    (f.origin.y + f.size.height) - (v.origin.y + v.size.height)
}

/// 菜单栏高度兜底(点):主屏栏高;主屏也未扣(异常态)退 24(常见菜单栏高,
/// 只用于不压菜单栏,不阻塞弹出)。
fn fallback_menu_bar_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::mainScreen(mtm)
        .map(|main| screen_menu_bar_height(&main))
        .filter(|h| *h > 0.5)
        .unwrap_or(24.0)
}

/// 取 Frame 的 NSView(结果窗口即根视口窗;取窗路径与 PoC-3 同法)。
fn frame_view(frame: &eframe::Frame) -> Option<Retained<NSView>> {
    let handle = match frame.window_handle() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("右键菜单:窗口句柄取不到({e})");
            return None;
        }
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        log::warn!("右键菜单:非 AppKit 窗口句柄");
        return None;
    };
    // SAFETY:h.ns_view 指向 winit 为本 app 创建、当前存活的 NSView(主线程持有),
    // 生命周期覆盖 eframe 主循环;Retained::retain 增引用计数延长借用
    // (同 attach_root 的取窗路径)。
    unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }
}

/// 构建右键菜单(标题 + tag;无 action——见 `popup_menu` 桥接方式)。
fn build_context_menu(mtm: MainThreadMarker, items: &[(&str, i64)]) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    // 无 action 的项会被自动验证判为禁用;关掉自动验证并显式启用
    // (项可高亮、可选择,highlightedItem 才有值——PoC-3 实锤)。
    menu.setAutoenablesItems(false);
    for (title, tag) in items {
        // SAFETY:selector 传 None(无 action,非「无效 selector」);标题与键等价串
        // 为本次调用内构造的 NSString 临时值,AppKit 拷贝入项;tag/setEnabled 为
        // 普通属性赋值。
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                None,
                &NSString::from_str(""),
            )
        };
        item.setTag(*tag as NSInteger); // i64 → isize(边界转换,见 popup_menu)
        item.setEnabled(true);
        menu.addItem(&item);
    }
    menu
}

/// 重锚:窗口高度变化(结果列表出现/收起)时保持顶边不动。
///
/// 依据:宽度/位置由平台层摆了脚(原生 `setFrameOrigin`),而 egui 的
/// `InnerSize` 走 winit `setContentSize`,其保持的角在 AppKit 侧不作保证——
/// 每次悬浮条 pass 在高度变化时重摆一次,顶边恒定(实测定:内容向下扩)。
///
/// 底部 clamp 回退:show 时若可见高不足,`Anchor.top_y` 记为实际窗口顶(低于 110
/// 线);重锚保持该顶边直到下次唤起。正常屏不触发。
pub fn reanchor() {
    let Some(_mtm) = MainThreadMarker::new() else {
        return; // 非主线程:no-op(不该发生)
    };
    // 重入安全:句柄与锚点短借用取出;setFrameOrigin 在借用外(会触发
    // windowDidMove → 可能同步重入一帧,帧内会 borrow_mut)。
    let (win, anchor) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.palette.clone(), st.anchor)
    });
    let (Some(win), Some(a)) = (win, anchor) else {
        return;
    };
    let h = win.frame().size.height;
    if (h - a.h).abs() < 0.5 {
        return; // 高度未变:不动
    }
    win.setFrameOrigin(NSPoint::new(a.x, a.top_y - h));
    PALETTE.with(|cell| cell.borrow_mut().anchor = Some(Anchor { h, ..a }));
}

/// 退出/销毁前还原窗口类(否则 winit drop → abort;spike 三方对照实证)。
pub fn revert_class() {
    // 重入安全(同 show_palette):状态短借用取出,object_setClass 在借用外。
    let (win, orig, class_before) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.palette.clone(), st.orig_class, st.class_before.clone())
    });
    let (Some(win), Some(orig)) = (win, orig) else {
        return;
    };
    let obj: *mut AnyObject = (Retained::as_ptr(&win) as *const NSWindow)
        .cast_mut()
        .cast();
    let cur = obj_class(obj);
    if class_name(cur) == class_before {
        return; // 已是原类(重复调用/未换类)
    }
    // SAFETY:orig 是换类前 runtime 实测读到的类指针(类对象由运行时持有,存活);
    // 换回是 `object_setClass` 的逆操作,实例大小不变的前提同换类时。
    let old = unsafe { &*objc2::ffi::object_setClass(obj, orig) };
    log::info!(
        "悬浮条窗口类还原:{} → {}(原类回读 {})",
        class_name(cur),
        class_before,
        class_name(old)
    );
    PALETTE.with(|cell| cell.borrow_mut().orig_class = None);
    // 垫窗一并收掉(orderOut + 释放;进程退出前保持桌面干净)
    let blur = PALETTE.with(|cell| cell.borrow_mut().blur.take());
    if let Some(blur) = blur {
        blur.orderOut(None);
    }
    PALETTE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.vib = None;
        st.blur_visible = false;
    });
}

/// 垫窗 frame:面板窗 frame 去掉**顶部形象条**,并跟随本帧**可见面板高**。
///
/// AppKit origin 在左下:面板顶 = 窗口顶 − `top_inset` = `origin.y + height − top_inset`。
/// 垫窗顶边钉在面板顶,向下延伸 `visible_h`:
/// `origin.y = 面板顶 − visible_h`、`height = visible_h`。
/// - 静止(`visible_h` = 面板区高):底边与窗口底一致;
/// - 收缩/增长动画期(`visible_h` < 面板区高):顶边不动、底边随可见面板移动,
///   不会在可见面板下方留磨砂带,也不会先铺出整块目标高磨砂。
///
/// `visible_h` clamp 到 [0, 窗口高 − top_inset];为 0 时调用方 orderOut。
/// 垫窗若盖住形象条,那块会变成磨砂矩形(§2.9 形象须透出桌面背景)。
fn blur_frame(panel: NSRect, top_inset: f64, visible_h: f64) -> NSRect {
    let panel_h = (panel.size.height - top_inset).max(0.0);
    let h = visible_h.clamp(0.0, panel_h);
    let panel_top = panel.origin.y + panel.size.height - top_inset;
    NSRect::new(
        NSPoint::new(panel.origin.x, panel_top - h),
        NSSize::new(panel.size.width, h),
    )
}

/// 毛玻璃垫窗每帧同步(悬浮条 pass 调用,`platform::reanchor` 同址)。
///
/// PoC-7 定案:面板窗下挂同级窗承载 `NSVisualEffectView`(winit 视图即 contentView,
/// 不可换容器/加兄弟层)。只在「frame 变化」与「显隐翻转」时调 AppKit——
/// 静止期零原生调用;高度动画期每帧 resize + mask 重建(≤120ms,可接受)。
/// 重入安全:AppKit 调用全程不持 PALETTE 借用。
///
/// `top_inset` = 窗口顶形象条高;`visible_panel_h` = 本帧可见面板高(Task 9)。
pub fn sync_blur(visible: bool, corner_radius: f64, top_inset: f64, visible_panel_h: f64) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let (panel, has_blur) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.palette.clone(), st.blur.is_some())
    });
    let Some(panel) = panel else {
        return; // 垫窗随面板;面板窗还没接上就不建
    };
    // 可见面板高为 0(隐藏/空裁剪)时不显示垫窗:零面积磨砂无意义,
    // 且避免动画起点先铺一块目标高磨砂。
    let show = visible && visible_panel_h > 0.5;
    if show && !has_blur {
        create_blur(mtm, &panel, corner_radius, top_inset, visible_panel_h);
    }
    let (blur, was_visible) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.blur.clone(), st.blur_visible)
    });
    let Some(blur) = blur else {
        return;
    };
    if show {
        let f = blur_frame(panel.frame(), top_inset, visible_panel_h);
        if blur.frame() != f {
            let was = blur.frame();
            blur.setFrame_display(f, true);
            if let Some(vib) = PALETTE.with(|cell| cell.borrow().vib.clone()) {
                vib.setMaskImage(Some(&rounded_mask(f.size, corner_radius)));
            }
            log::info!("垫窗跟随:blur {was:?} → {f:?}");
        }
        if !was_visible {
            // 先于面板置前、再压到面板之下:顺序即层级
            blur.orderFrontRegardless();
            blur.orderWindow_relativeTo(NSWindowOrderingMode::Below, panel.windowNumber());
            log::info!(
                "毛玻璃垫窗显示:frame={:?} panel_window_number={} blur_window_number={}",
                f,
                panel.windowNumber(),
                blur.windowNumber()
            );
        }
    } else if was_visible {
        blur.orderOut(None);
    }
    PALETTE.with(|cell| cell.borrow_mut().blur_visible = show);
}

/// 建毛玻璃垫窗(borderless/无阴影/不接鼠标/级别与 collectionBehavior 同面板)。
/// 初始 frame 只覆盖本帧可见面板(去掉顶部形象条,Task 9)。
fn create_blur(
    mtm: MainThreadMarker,
    panel: &NSWindow,
    corner_radius: f64,
    top_inset: f64,
    visible_panel_h: f64,
) {
    // **普通 NSWindow（PoC-7 实证可见磨砂的形态）**。
    // 2026-10-17 实机对照：垫窗改「NSPanel 构造」后，浅深两色下磨砂都不再出现
    // （辉哥判读：「PoC 时有半透明，现在没了」）；换 maskImage 也不恢复。故回退到
    // PoC 形态先保住磨砂。代价：**垫窗不进他人全屏 Space**（PoC-1：普通 NSWindow
    // 的硬限制）——全屏下无磨砂，另行处置（见 plan 遗留）。
    // SAFETY: 主线程持有 mtm；标准构造，返回对象由 Retained 保活。
    let frame = blur_frame(panel.frame(), top_inset, visible_panel_h);
    let win = unsafe {
        let alloc = NSWindow::alloc(mtm);
        NSWindow::initWithContentRect_styleMask_backing_defer(
            alloc,
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    win.setOpaque(false);
    win.setBackgroundColor(Some(&NSColor::clearColor()));
    win.setHasShadow(false);
    win.setIgnoresMouseEvents(true);
    win.setLevel(panel.level());
    win.setCollectionBehavior(panel.collectionBehavior());
    let vib = NSVisualEffectView::new(mtm);
    // 材质选型（2026-10-17 实机迭代）：面板底 alpha 到 40% 仍「看不出变化」——
    // 该观感由 material 自身的半透明层主导（Popover 偏亮偏实）。改窗口底色级材质
    // `UnderWindowBackground`（最通透的一档），把「透明感」的杠杆交还材质。
    vib.setMaterial(NSVisualEffectMaterial::UnderWindowBackground);
    vib.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    vib.setState(NSVisualEffectState::Active);
    let size = win.frame().size;
    vib.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), size));
    vib.setMaskImage(Some(&rounded_mask(size, corner_radius)));
    let vib_view: &NSView = &vib;
    win.setContentView(Some(vib_view));
    // 置前后必须立即压到面板之下（否则成"纱"——实机 2026-10-17）。
    win.orderFrontRegardless();
    win.orderWindow_relativeTo(NSWindowOrderingMode::Below, panel.windowNumber());
    // **换类时机实验（2026-10-17）**：vibrancy 装好、排层完成**之后**再换类为 NSPanel
    // 并补 NonactivatingPanel（跨他人全屏 Space 的硬条件）。此前两轮失败均为
    // 「换类/构造在前、vibrancy 在后」，本组合是唯一未试的序；若成功则全屏也有磨砂。
    {
        let obj: *const AnyObject = (win.as_ref() as *const NSWindow).cast::<AnyObject>();
        let before = obj_class(obj);
        let target = NSPanel::class();
        if before.instance_size() == target.instance_size() {
            // SAFETY: 实例大小已断言一致；目标类为系统 NSPanel（无新增 ivars）。
            unsafe {
                objc2::ffi::object_setClass(obj.cast_mut(), target as *const AnyClass);
            }
            // SAFETY: 该对象已换为 NSPanel 类，按 NSPanel 解释成立。
            let panel_ref: &NSPanel =
                unsafe { &*(win.as_ref() as *const NSWindow).cast::<NSPanel>() };
            panel_ref.setStyleMask(panel_ref.styleMask() | NSWindowStyleMask::NonactivatingPanel);
            panel_ref.setFloatingPanel(true);
            panel_ref.setBecomesKeyOnlyIfNeeded(false);
            panel_ref.setHidesOnDeactivate(false);
            log::info!("垫窗换类（vibrancy 之后）→ NSPanel + NonactivatingPanel");
        } else {
            log::warn!("垫窗换类被拒：实例大小不一致（全屏下将无磨砂）");
        }
    }
    PALETTE.with(|cell| {
        let mut st = cell.borrow_mut();
        st.blur = Some(win);
        st.vib = Some(vib);
        st.blur_visible = true;
    });
    log::info!(
        "毛玻璃垫窗已建（普通 NSWindow/PoC-7 形态；material=UnderWindowBackground，面板之下；panel#{}）",
        panel.windowNumber()
    );
}

/// 生成圆角矩形遮罩图（黑填充 + 全透明底；`maskImage` 的形制）。
///
/// `lockFocus`/`unlockFocus` 为弃用 API 但仍受支持，是构造此图最短路径
/// （替代：CG 位图上下文手绘，代码量约为其三倍）。
#[allow(deprecated)]
fn rounded_mask(size: NSSize, radius: f64) -> Retained<NSImage> {
    // NSImage 为 AnyThread 类（objc2-app-kit 生成未标 MainThreadOnly），alloc 不带 mtm；
    // 绘制调用本身仍在主线程（调用点均在 sync_blur/create_blur 内）。
    let img = NSImage::initWithSize(NSImage::alloc(), size);
    img.lockFocus();
    NSColor::blackColor().set();
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), size);
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, radius, radius).fill();
    img.unlockFocus();
    img
}

/// 是否已接上悬浮条窗(诊断/首帧接线轮询用)。
pub fn attached() -> bool {
    PALETTE.with(|cell| cell.borrow().palette.is_some())
}

/// 根窗当前原生可见性(`NSWindow.isVisible()` 回读;首帧纠偏核对用)。
/// 未记录根窗句柄(attach_root 未执行/非主线程)→ None。
pub fn root_visible() -> Option<bool> {
    let _mtm = MainThreadMarker::new()?;
    PALETTE.with(|cell| cell.borrow().root.as_ref().map(|w| w.isVisible()))
}

/// 悬浮条窗当前原生可见性(`NSWindow.isVisible()` 回读;诊断用)。
/// 未接上窗/非主线程 → None。
pub fn palette_visible() -> Option<bool> {
    let _mtm = MainThreadMarker::new()?;
    PALETTE.with(|cell| cell.borrow().palette.as_ref().map(|w| w.isVisible()))
}

/// 设置窗当前原生可见性(按标题回读;诊断用)。
/// 未创建/非主线程 → None。
pub fn settings_visible() -> Option<bool> {
    let mtm = MainThreadMarker::new()?;
    let root = PALETTE.with(|cell| cell.borrow().root.clone());
    find_window_by_title(mtm, root.as_deref(), &settings_window_titles()).map(|w| w.isVisible())
}

// ---------------------------------------------------------------------------
// 内部实现

/// 取对象当前类(`object_getClass`)。
fn obj_class(obj: *const AnyObject) -> &'static AnyClass {
    // SAFETY:obj 指向存活对象;类对象由运行时持有,永续。
    unsafe { &*objc2::ffi::object_getClass(obj) }
}

fn class_name(cls: &AnyClass) -> String {
    cls.name().to_string_lossy().into_owned()
}

/// 在 `NSApp.windows()` 里按标题找悬浮条窗(排除启动时记录的根窗)。
///
/// eframe/winit 不暴露「视口 → 原生窗」的回取 API(视口窗由框架内部创建),
/// 故用标题定位(spike 回退路径同法:`NSApplication.windows()`)。
fn find_palette_window(
    mtm: MainThreadMarker,
    root: Option<&NSWindow>,
) -> Option<Retained<NSWindow>> {
    find_window_by_title(mtm, root, &[PALETTE_TITLE])
}

/// 按标题找视口窗(排除根窗;设置窗诊断走同一路径)。
///
/// `titles` = 可接受标题集:设置窗传 [`settings_window_titles`](切换瞬间两种语言
/// 标题都可能出现),悬浮条恒传 `PALETTE_TITLE`。
fn find_window_by_title(
    mtm: MainThreadMarker,
    root: Option<&NSWindow>,
    titles: &[&str],
) -> Option<Retained<NSWindow>> {
    let app = NSApplication::sharedApplication(mtm);
    for win in app.windows().iter() {
        if root.is_some_and(|r| std::ptr::eq(r, &*win)) {
            continue;
        }
        let actual = win.title().to_string();
        if titles.iter().any(|t| window_title_matches(&actual, t)) {
            return Some(win);
        }
    }
    None
}

/// 窗口标题精确匹配(纯函数,单测):空目标不匹配任何窗口
/// (防止把无标题窗误当成设置/悬浮条窗)。
fn window_title_matches(actual: &str, expected: &str) -> bool {
    !expected.is_empty() && actual == expected
}

/// 换类:NSWindow → `MacePalettePanel`(实例大小须一致,否则拒绝)。
///
/// 重入安全:不碰 PALETTE 状态——原类指针与类名**返回给调用方**存(调用方短借用写回)。
/// 期间 `configure` 的 AppKit 调用可重入一帧,帧内状态访问须无借用冲突。
fn convert(win: &NSWindow) -> Result<(*const AnyClass, String)> {
    let obj: *const AnyObject = (win as *const NSWindow).cast::<AnyObject>();
    let before = obj_class(obj);
    let size_before = before.instance_size();
    let target = MacePalettePanel::class();
    let size_target = target.instance_size();
    if size_before != size_target {
        // 实例大小不一致时换类即 UB:拒绝并留痕(产品不 panic,退化 = 无跨 Space 补丁)。
        return Err(anyhow!(
            "悬浮条窗换类被拒:实例大小不一致(当前类 {} {size_before} vs 目标类 {size_target})",
            class_name(before)
        ));
    }
    // SAFETY:win 指向本进程存活的 NSWindow;目标类为 NSPanel 子类且实例大小
    // 已实测一致(上一步),满足 object_setClass 的前提;返回旧类指针。
    let old = unsafe { &*objc2::ffi::object_setClass(obj.cast_mut(), target as *const AnyClass) };
    let class_before = class_name(before);
    log::info!(
        "悬浮条窗换类:{} → {}(旧类回读 {};实例大小 {size_before}=={size_target})",
        class_before,
        class_name(target),
        class_name(old)
    );
    configure(win);
    Ok((before as *const AnyClass, class_before))
}

/// 换类后施加属性(五要素的 2~4 + NSPanel 三属性 + 补丁 a)。
fn configure(ns: &NSWindow) {
    ns.setLevel(NSFloatingWindowLevel);
    ns.setCollectionBehavior(target_behavior());
    // 跨全屏硬条件(spike 差分实证:缺 NonactivatingPanel 即不在全部 Space 全屏窗上方)。
    ns.setStyleMask(ns.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    // 失焦隐藏由本 app 自管(视口 focused);系统 deactivate 隐藏会误伤常驻形态。
    ns.setHidesOnDeactivate(false);
    // SAFETY:调用方刚把该对象换成 NSPanel 子类,按 NSPanel 解释成立。
    let panel: &NSPanel = unsafe { &*(ns as *const NSWindow).cast::<NSPanel>() };
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(false);
    // 补丁 a:改 styleMask 会打断键盘投递链,必须补 first responder。
    let ok = restore_first_responder(ns);
    // 高度变化呈现(PoC-6 定案):resize 时旧帧钉左上、裁切而非缩放拉伸。
    let placement = set_top_left_placement(ns);
    log::info!(
        "悬浮条窗属性:level={} CanJoinAllSpaces={} FullScreenAuxiliary={} styleMask={:#x} first_responder={ok} layer_placement={placement:?}",
        ns.level(),
        ns.collectionBehavior()
            .contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
        ns.collectionBehavior()
            .contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
        ns.styleMask().0
    );
}

/// resize 时旧帧的放置策略:`layerContentsPlacement = TopLeft`(PoC-6 定案——
/// 窗口先撑/后收期间旧帧钉左上、裁切而非拉伸;默认值 0 = 轴向缩放拉伸)。
/// 返回读回值(11 = TopLeft)供留痕;无 contentView 时返回 None。
fn set_top_left_placement(ns: &NSWindow) -> Option<NSInteger> {
    let view = ns.contentView()?;
    view.setLayerContentsPlacement(NSViewLayerContentsPlacement::TopLeft);
    Some(view.layerContentsPlacement().0)
}

/// 跨 Space 需要的 collection behavior 组合(PoC-1/1b 沿用值)。
fn target_behavior() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary
}

/// 把内容视图(winit 的 `WinitView`)重新设为 first responder(补丁 a)。
fn restore_first_responder(ns: &NSWindow) -> bool {
    let Some(view) = ns.contentView() else {
        log::warn!("contentView() 为空,补 first responder 失败");
        return false;
    };
    // SAFETY:NSView 是 NSResponder 子类(objc2 不建模继承关系,故显式 cast);
    // view 由窗口持有、存活,borrow 期即本调用。
    let responder: &NSResponder = unsafe { &*(Retained::as_ptr(&view) as *const NSResponder) };
    ns.makeFirstResponder(Some(responder))
}

/// 漂移核对(仅可见期做,隐藏期零开销):level/behavior/styleMask 被外力改动即重施。
fn reassert(drift: &mut Drift, win: &NSWindow) {
    if !win.isVisible() {
        return;
    }
    if win.level() != NSFloatingWindowLevel {
        drift.level += 1;
        if drift.level <= 3 {
            log::warn!(
                "level 漂移(第 {} 次):实测 {},重施",
                drift.level,
                win.level()
            );
        }
        win.setLevel(NSFloatingWindowLevel);
    }
    let want = target_behavior();
    if !win.collectionBehavior().contains(want) {
        drift.behavior += 1;
        if drift.behavior <= 3 {
            log::warn!("collectionBehavior 漂移(第 {} 次),重施", drift.behavior);
        }
        win.setCollectionBehavior(want);
    }
    if !win
        .styleMask()
        .contains(NSWindowStyleMask::NonactivatingPanel)
    {
        drift.style += 1;
        if drift.style <= 3 {
            log::warn!(
                "styleMask 漂移(第 {} 次),重施 NonactivatingPanel",
                drift.style
            );
        }
        win.setStyleMask(win.styleMask() | NSWindowStyleMask::NonactivatingPanel);
        restore_first_responder(win); // 与初次施加一致:改 styleMask 必补
    }
}

/// 鼠标所在屏(全局坐标含点判定;查不到退化主屏)。
fn screen_with_point(mtm: MainThreadMarker, p: NSPoint) -> Option<Retained<NSScreen>> {
    let screens = NSScreen::screens(mtm);
    for s in screens.iter() {
        let f = s.frame();
        if p.x >= f.origin.x
            && p.x < f.origin.x + f.size.width
            && p.y >= f.origin.y
            && p.y < f.origin.y + f.size.height
        {
            return Some(s);
        }
    }
    NSScreen::mainScreen(mtm).or_else(|| screens.iter().next())
}

/// 定位(Task 11 定案):鼠标所在屏可见区内水平居中、面板顶边固定 `TOP_INSET`;
/// 宽度按可见区回填(`palette_width`);越界 clamp 回可见区。返回 (锚点, 实际面板宽)。
///
/// Task 9 起窗口高 = 面板高 + `top_inset`(形象条)。口径：
/// - 屏幕坐标(y 向下):窗口顶 = 面板顶 − top_inset；
/// - AppKit 坐标(y 向上,本函数):窗口顶 y = 面板顶 y + top_inset，再减窗高得 origin。
///
/// `Anchor.top_y` 取**实际窗口顶**(origin.y + h)：底边 clamp 时窗口顶不再是
/// 110+strip 线，该位置保持到下次 show（正常屏不触发）。
fn position_palette(win: &NSWindow, mtm: MainThreadMarker, top_inset: f64) -> (Anchor, f64) {
    let mouse = NSEvent::mouseLocation();
    let visible = screen_with_point(mtm, mouse)
        .map(|s| s.visibleFrame())
        .unwrap_or_else(|| {
            // 理论不可达(至少一屏);退化为主屏整屏,仍给确定位置。
            log::warn!("鼠标所在屏查不到:退化使用零原点可见区");
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0))
        });
    let target_w = palette_width(visible.size.width);
    let mut frame = win.frame();
    let old_w = frame.size.width;
    if (old_w - target_w).abs() > 0.5 {
        win.setContentSize(NSSize::new(target_w, frame.size.height));
        frame = win.frame(); // 重读:以实际落地尺寸为准
        log::info!(
            "悬浮条宽度回填:{old_w:.0} → {:.0}(可见区宽 {:.0})",
            frame.size.width,
            visible.size.width
        );
    }
    let (w, h) = (frame.size.width, frame.size.height);
    let origin = palette_origin(visible, w, h, top_inset);
    win.setFrameOrigin(origin);
    (
        Anchor {
            x: origin.x,
            top_y: origin.y + h,
            h,
        },
        w,
    )
}

/// 悬浮条左上角(纯函数,单测钉住):可见区水平居中并 clamp(两侧留 `EDGE_MARGIN`);
/// **窗口顶** = 可见区顶 − `TOP_INSET` + `top_inset`(AppKit y 向上;`top_inset` 为
/// 窗口顶部形象条,Task 9)。换算到屏幕坐标(y 向下)即窗口顶 = 面板顶 − `top_inset`；
/// 面板顶 = 可见区顶 − `TOP_INSET`(两种坐标口径一致)。
/// 底边越界时上移 clamp(Cocoa 原点在左下),窗口顶随之低于目标线并保持到下次 show
/// (正常屏不触发)。
///
/// 水平 clamp 语义:**宽度下限 480 优先**——可见宽 < 496(480 + 2×`EDGE_MARGIN`)
/// 时无法同时保留两侧留白,允许横向溢出(典型 700 宽窗放进 700 可见区,右缘越出 8)。
fn palette_origin(visible: NSRect, w: f64, h: f64, top_inset: f64) -> NSPoint {
    let min_x = visible.origin.x + EDGE_MARGIN;
    let max_x = (visible.origin.x + visible.size.width - w - EDGE_MARGIN).max(min_x);
    let x = (visible.origin.x + (visible.size.width - w) / 2.0).clamp(min_x, max_x);
    let window_top = visible.origin.y + visible.size.height - TOP_INSET + top_inset;
    let y = (window_top - h).max(visible.origin.y + EDGE_MARGIN);
    NSPoint::new(x, y)
}

// ---------------------------------------------------------------------------
// 系统图标(Task 5;visual.md §2.4「文件图标走系统真实图标」)
//
// 类型图标走 `NSWorkspace.iconForContentType`(不按路径);仅当用户给单个文件设过
// 自定义图标时才按路径读 `NSURLCustomIconKey`。`uti_for` 是廉价的 UTType 查询
// (UI 线程);取图/数据化/缩放全部在 `mce-icon-loader` 工作线程串行执行,线程安全
// 依据见 `ui/icons.rs` 模块注释。

/// 解析文件的 UTI(目录 `UTTypeFolder`;无扩展名/解析失败回落 `UTTypeData`)。
pub fn uti_for(extension: Option<&str>, is_dir: bool) -> String {
    let resolved = if is_dir {
        None
    } else {
        extension.and_then(|ext| UTType::typeWithFilenameExtension(&NSString::from_str(ext)))
    };
    // SAFETY:UTCoreTypes 静态量由 UniformTypeIdentifiers 框架在进程生命周期内提供;
    // 此处只读借用,不写入、不跨线程移动,无别名/数据竞争。
    let (folder, data) = unsafe { (UTTypeFolder, UTTypeData) };
    let uti: &UTType = if is_dir {
        folder
    } else {
        resolved.as_deref().unwrap_or(data)
    };
    uti.identifier().to_string()
}

/// 按 UTI 取系统图标并转 64×64 **预乘** RGBA。
///
/// 由图标工作线程调用:只做 `NSWorkspace` 查询与返回图像的 TIFF 数据化,不创建/
/// 操作视图或窗口(跨线程依据见 `ui/icons.rs` 模块注释)。
pub fn type_icon_rgba(uti: &str) -> Option<Vec<u8>> {
    let uti = UTType::typeWithIdentifier(&NSString::from_str(uti))?;
    let icon = NSWorkspace::sharedWorkspace().iconForContentType(&uti);
    image_rgba(&icon)
}

/// 按路径读用户自定义图标(`NSURLCustomIconKey`);无自定义图标/读取失败 = `None`。
///
/// 返回 64×64 **预乘** RGBA;由图标工作线程调用,只读取资源属性与图像数据,
/// 不触碰视图层。
pub fn custom_icon_rgba(path: &str) -> Option<Vec<u8>> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    let mut value: Option<Retained<AnyObject>> = None;
    // SAFETY:`getResourceValue:forKey:error:` 要求 `value` 出参由方法写入「正确
    // 类型」的对象;先置 None,调用后 downcast 校验为 NSImage,类型不符即放弃。
    let result = unsafe { url.getResourceValue_forKey_error(&mut value, NSURLCustomIconKey) };
    if result.is_err() {
        return None;
    }
    let icon = value?.downcast::<NSImage>().ok()?;
    image_rgba(&icon)
}

/// `NSImage` → 64×64 **预乘** RGBA:TIFF 表示 → image 解码 → Lanczos 缩放。
///
/// 系统图标源通常 ≥512px,缩到 64px 即满足 Retina 2x 的 32pt 行图标。
/// 已知代价:`TIFFRepresentation` 本机为 32 页/约 36.9MB,`image` 只解第一页
/// (32pt 图标取首个表示已够用);这里直接借用 NSData 字节,不再整份拷进 Vec。
fn image_rgba(icon: &NSImage) -> Option<Vec<u8>> {
    let tiff = icon.TIFFRepresentation()?;
    let len = tiff.length();
    if len == 0 {
        return None;
    }
    // SAFETY:`tiff` 在同步解码期间保持存活;`bytes` 返回其内部只读缓冲区指针,
    // 长度恰为 `length()`。切片只传给 `image::load_from_memory` 同步解码,期间
    // NSData 不被修改/释放,不越界。
    // 返回类型须声明为 `*const c_void`(编码 `^v`):`NSData.bytes` 的 ABI 是
    // `const void *`;写成 `*const u8`(objc2 编码 `*`,C 字符串)会在 debug 的
    // 方法签名校验里 panic(2026-10-17 实机:图标 worker 整线程退出)。
    let bytes = unsafe {
        let ptr: *const std::ffi::c_void = msg_send![&*tiff, bytes];
        std::slice::from_raw_parts(ptr.cast::<u8>(), len)
    };
    let decoded = image::load_from_memory(bytes).ok()?;
    let resized = decoded.resize_exact(ICON_PX, ICON_PX, image::imageops::FilterType::Lanczos3);
    Some(resized.to_rgba8().into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 副屏可见区(与本机实机同形:Redmi 1920x1080 @ (1470,-65);AppKit 坐标)。
    /// **可见区顶 = origin.y + height = 1015**,即顶边 110 的基准(`visibleFrame`
    /// 已扣菜单栏,不是含菜单栏的屏幕顶)。
    fn vis() -> NSRect {
        NSRect::new(NSPoint::new(1470.0, -65.0), NSSize::new(1920.0, 1080.0))
    }

    /// 顶边口径(Task 11):面板顶边恰为 fixture 的可见区顶(1015)− 110;水平居中。
    /// 注意基准是 `visibleFrame` 顶,不是含 28px 菜单栏的屏幕顶。
    /// Task 9:带形象条时窗口顶 = 面板顶 + strip,面板顶口径不变。
    #[test]
    fn palette_origin_pins_top_110_and_centers() {
        let visible = vis();
        let (w, h) = (700.0, 176.0);
        let visible_top = visible.origin.y + visible.size.height;
        assert_eq!(TOP_INSET, 110.0, "顶边口径固定 110");
        // 无形象条(top_inset = 0):窗口顶 = 可见区顶 − 110。
        let p = palette_origin(visible, w, h, 0.0);
        assert_eq!(
            p.y + h,
            visible_top - TOP_INSET,
            "窗口顶边须为可见区顶 − 110"
        );
        assert_eq!(
            p.x,
            visible.origin.x + (visible.size.width - w) / 2.0,
            "常规宽度水平居中"
        );
        assert!(
            p.x + w <= visible.origin.x + visible.size.width - EDGE_MARGIN,
            "右缘保留 EDGE_MARGIN"
        );
        // 带形象条(Task 9,strip = 70):AppKit 窗口顶 = 可见区顶 − 110 + 70 = −40;
        // 屏幕坐标(面板顶 = 窗口顶 − 70)仍 = 可见区顶 − 110。
        let strip = 70.0;
        let p = palette_origin(visible, w, h + strip, strip);
        let window_top = p.y + h + strip;
        assert_eq!(window_top, visible_top - TOP_INSET + strip);
        assert_eq!(window_top, visible_top - 40.0);
        assert_eq!(
            window_top - strip,
            visible_top - TOP_INSET,
            "面板顶边仍为可见区顶 − 110"
        );
    }

    /// 水平 clamp 语义:宽度下限 480 优先——可见宽 < 496(480+2×EDGE_MARGIN)时
    /// 无法同时保留两侧留白,**允许横向溢出**(右缘越出 EDGE_MARGIN);
    /// 正常窄宽(≤ 可见宽−48)恒在两侧留白内。
    #[test]
    fn palette_origin_clamps_horizontally() {
        // 整宽/近整宽:居中位越过左留白 → 贴左内边距,右缘按既定语义越出。
        let narrow = NSRect::new(vis().origin, NSSize::new(700.0, vis().size.height));
        let p = palette_origin(narrow, 700.0, 176.0, 0.0);
        assert_eq!(p.x, narrow.origin.x + EDGE_MARGIN, "整宽时贴左留白");
        assert_eq!(
            p.x + 700.0,
            narrow.origin.x + 700.0 + EDGE_MARGIN,
            "整宽时右缘越出 EDGE_MARGIN(下限/整宽优先)"
        );
        // 可见宽 < 496 的下限场景:480 窗放进 480 可见区,同样允许右缘越出。
        let tiny = NSRect::new(vis().origin, NSSize::new(480.0, vis().size.height));
        let p = palette_origin(tiny, 480.0, 176.0, 0.0);
        assert_eq!(p.x, tiny.origin.x + EDGE_MARGIN);
        assert_eq!(p.x + 480.0, tiny.origin.x + 480.0 + EDGE_MARGIN);
        // 正常窄宽(可见宽 − 48)仍居中且左右留白。
        let fit = palette_origin(narrow, 652.0, 176.0, 0.0);
        assert_eq!(fit.x, narrow.origin.x + (700.0 - 652.0) / 2.0);
        assert!(fit.x >= narrow.origin.x + EDGE_MARGIN);
        assert!(fit.x + 652.0 <= narrow.origin.x + 700.0 - EDGE_MARGIN);
    }

    /// 底边 clamp:窗口高超过可用高度时,底边不低于可见区底 + EDGE_MARGIN;
    /// 此时实际顶边随之下移(Anchor 取实际窗口顶,不再等于 110 线)。
    #[test]
    fn palette_origin_clamps_bottom() {
        let visible = vis();
        let h = 1080.0; // 恰为可见区高:按 110 顶边摆放会露出底边
        let p = palette_origin(visible, 700.0, h, 0.0);
        assert_eq!(p.y, visible.origin.y + EDGE_MARGIN, "底边贴可见区底留白");
        assert!(
            p.y + h > visible.origin.y + visible.size.height - TOP_INSET,
            "顶边随底边 clamp 下移"
        );
    }

    /// 垫窗 frame 跟随**可见面板高**(Task 9 集成审查修复):
    /// - 静态(visible = 面板区高):底边与窗口底一致;
    /// - 收缩/增长中(visible < 面板区高):顶边恒 = 面板顶,底边随 visible 移动;
    /// - 顶部形象条永远排除;visible clamp 到 [0, 窗口高 − strip];宽度不变。
    #[test]
    fn blur_frame_follows_visible_panel_height() {
        // 空态窗口 = 面板区 176 + 形象条 70 = 246。
        let win = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(700.0, 246.0));
        let panel_top = win.origin.y + win.size.height - 70.0; // 196

        // 静态:可见高 = 面板区高 → 底边与窗口底一致、顶边 = 面板顶。
        let still = blur_frame(win, 70.0, 176.0);
        assert_eq!(still.origin.x, win.origin.x);
        assert_eq!(still.size.width, win.size.width, "宽度不变");
        assert_eq!(still.origin.y, win.origin.y, "静态底边 = 窗口底");
        assert_eq!(still.size.height, 176.0);
        assert_eq!(still.origin.y + still.size.height, panel_top);

        // 收缩中:可见 100 → 顶边仍 = 面板顶,底边上移 76(不在下方留磨砂带)。
        let shrink = blur_frame(win, 70.0, 100.0);
        assert_eq!(shrink.size.height, 100.0);
        assert_eq!(
            shrink.origin.y + shrink.size.height,
            panel_top,
            "顶边钉面板顶"
        );
        assert_eq!(shrink.origin.y, win.origin.y + 76.0, "底边随可见面板上移");

        // 增长中:窗口已撑到新高(面板区 266 + strip 70 = 336),可见仅 150 →
        // 顶边钉面板顶,底边高于窗口底(不先铺整块目标高磨砂)。
        let tall = NSRect::new(NSPoint::new(10.0, 20.0), NSSize::new(700.0, 336.0));
        let grow = blur_frame(tall, 70.0, 150.0);
        let tall_panel_top = tall.origin.y + tall.size.height - 70.0;
        assert_eq!(grow.size.height, 150.0);
        assert_eq!(grow.origin.y + grow.size.height, tall_panel_top);
        assert!(grow.origin.y > tall.origin.y, "增长中底边高于窗口底");

        // 顶部 strip 永远排除:任何可见高下 frame 顶 ≤ 面板顶。
        for h in [0.0, 1.0, 100.0, 176.0, 500.0] {
            let f = blur_frame(win, 70.0, h);
            assert!(f.origin.y + f.size.height <= panel_top + 1e-9);
        }
        // clamp:超面板区高按面板区高;负值按 0;条高超过窗高也不得为负。
        assert_eq!(blur_frame(win, 70.0, 500.0).size.height, 176.0);
        assert_eq!(blur_frame(win, 70.0, -5.0).size.height, 0.0);
        assert_eq!(blur_frame(win, 500.0, 100.0).size.height, 0.0);
    }

    /// 常规:锚点 = 光标 x、可见区顶边(可见区已扣菜单栏 → 栏底,传 0)。
    #[test]
    fn tray_anchor_aligns_cursor_x_with_visible_top() {
        let p = tray_menu_anchor(2662.0, vis(), 110.0, 0.0);
        assert_eq!(p.x, 2662.0);
        assert_eq!(p.y, -65.0 + 1080.0);
    }

    /// 右缘越界左移让出(状态项在菜单栏最右,近右缘必越界):菜单右缘不越屏。
    #[test]
    fn tray_anchor_shifts_left_at_right_edge() {
        let p = tray_menu_anchor(3320.0, vis(), 110.0, 0.0);
        assert_eq!(p.x, 3390.0 - 110.0 - EDGE_MARGIN);
        assert!(p.x + 110.0 <= 3390.0, "菜单右缘不得越屏");
        assert!(p.x < 3320.0, "越界时须相对光标左移");
    }

    /// 左缘 clamp:光标贴左缘时锚点仍在可见区内(留 EDGE_MARGIN)。
    #[test]
    fn tray_anchor_clamps_to_left_edge() {
        let p = tray_menu_anchor(1472.0, vis(), 110.0, 0.0);
        assert_eq!(p.x, 1470.0 + EDGE_MARGIN);
    }

    /// 可见区未扣菜单栏(理论不可达)时按兜底栏高下移:菜单不压菜单栏。
    #[test]
    fn tray_anchor_drops_below_unaccounted_bar() {
        let p = tray_menu_anchor(2662.0, vis(), 110.0, 31.0);
        assert_eq!(p.x, 2662.0, "x 不受栏高影响");
        assert_eq!(p.y, -65.0 + 1080.0 - 31.0);
    }

    /// 设置窗首次居中:可见区(已扣菜单栏/Dock)中心 = 620×600 窗口原点;
    /// 副屏负坐标同样以该屏可见区为基准。
    #[test]
    fn settings_origin_centers_in_visible_frame() {
        let main = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1055.0));
        let p = settings_origin(main, 620.0, 600.0);
        assert_eq!(p.x, (1920.0 - 620.0) / 2.0);
        assert_eq!(p.y, (1055.0 - 600.0) / 2.0);
        assert!(p.x >= main.origin.x + EDGE_MARGIN);
        assert!(p.x + 620.0 <= main.origin.x + main.size.width - EDGE_MARGIN);
        assert!(p.y >= main.origin.y + EDGE_MARGIN);
        assert!(p.y + 600.0 <= main.origin.y + main.size.height - EDGE_MARGIN);

        // 副屏(AppKit 坐标可为负):先按该屏可见区中心落点。
        let secondary = NSRect::new(NSPoint::new(-1280.0, -200.0), NSSize::new(1280.0, 777.0));
        let p = settings_origin(secondary, 620.0, 600.0);
        assert_eq!(p.x, -1280.0 + (1280.0 - 620.0) / 2.0);
        assert_eq!(p.y, -200.0 + (777.0 - 600.0) / 2.0);
    }

    /// 可见区放不下 620×600:保持**顶边与左边**可见（AppKit y 向上，原点 y =
    /// 可见区顶 − 窗高 − 留白），底部允许溢出；不产生反向/NaN 坐标。
    #[test]
    fn settings_origin_keeps_top_left_when_window_larger_than_visible_frame() {
        let tiny = NSRect::new(NSPoint::new(100.0, 60.0), NSSize::new(500.0, 400.0));
        let p = settings_origin(tiny, 620.0, 600.0);
        assert_eq!(p.x, tiny.origin.x + EDGE_MARGIN, "左缘贴可见区左留白");
        assert_eq!(
            p.y,
            tiny.origin.y + tiny.size.height - 600.0 - EDGE_MARGIN,
            "原点 = 可见区顶 − 窗高 − 留白"
        );
        assert_eq!(
            p.y + 600.0,
            tiny.origin.y + tiny.size.height - EDGE_MARGIN,
            "顶边贴可见区顶留白（标题栏/交通灯可见）"
        );
        assert!(
            p.y < tiny.origin.y + EDGE_MARGIN,
            "窗高超过可见区时底部溢出，而非把顶边压出屏"
        );
        assert!(p.x.is_finite() && p.y.is_finite());
    }

    /// 标题查找安全性:精确匹配且目标非空(空标题/不同窗口/仅前缀均不得命中)。
    #[test]
    fn window_title_lookup_requires_exact_nonempty_match() {
        let titles = settings_window_titles();
        assert!(window_title_matches(titles[0], titles[0]));
        assert!(window_title_matches(titles[1], titles[1]));
        assert!(!window_title_matches(PALETTE_TITLE, titles[0]));
        assert!(!window_title_matches("", titles[0]));
        assert!(!window_title_matches("设置窗口", titles[0]));
        assert!(!window_title_matches("", ""), "空目标不得匹配无标题窗");
    }

    /// 设置窗查找接受中英两种原生标题(修复英文设置窗定位回归)。
    #[test]
    fn window_title_lookup_accepts_both_settings_titles() {
        let titles = settings_window_titles();
        assert!(window_title_matches("设置", titles[0]));
        assert!(window_title_matches("Settings", titles[1]));
        assert!(!window_title_matches("设置窗口", titles[0]));
        assert!(!window_title_matches("", titles[0]));
    }
}

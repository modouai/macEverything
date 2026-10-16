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
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{define_class, msg_send, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSEvent, NSFloatingWindowLevel, NSPanel, NSResponder, NSScreen, NSView,
    NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification,
};
use objc2_foundation::{NSNotification, NSObject, NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 悬浮条窗口标题(定义在 facade,ui 与平台层共用;`NSApp.windows()` 中按此定位)。
use super::{monotonic_ms, SpaceChange, PALETTE_TITLE};

/// 顶边位置:可见区顶部向下 1/4(Spotlight 式「居中偏上」,product §1)。
const TOP_INSET_FRACTION: f64 = 0.25;
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
}

/// 最近一次 Space 变更(facade 转发;None = 进程内从未收到)。
pub fn last_space_change() -> Option<SpaceChange> {
    match SPACE_CHANGE.lock() {
        Ok(g) => *g,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// 注册 Space 变更观察者(进程内一次;主线程)。失败仅留痕:豁免退化为不生效。
fn ensure_space_observer(mtm: MainThreadMarker, st: &mut PaletteState) {
    if st.space_observer.is_some() {
        return;
    }
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
    st.space_observer = Some(observer);
}

/// 漂移计数(与 PoC-1 续同口径:每帧核对,漂移即重施并留痕)。
#[derive(Default)]
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
pub fn ensure_patched() -> Result<()> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("ensure_patched 非主线程调用"));
    };
    PALETTE.with(|cell| {
        let mut st = cell.borrow_mut();
        ensure_space_observer(mtm, &mut st); // 进程内一次;与窗口接线解耦
        if st.palette.is_none() {
            let Some(win) = find_palette_window(mtm, st.root.as_deref()) else {
                return Ok(()); // 视口窗未创建;下帧再试
            };
            convert(&mut st, &win)?;
            st.palette = Some(win);
        }
        // 结构体解构拿不相交可变借用:palette 只读、drift 可写。
        let PaletteState { palette, drift, .. } = &mut *st;
        let Some(win) = palette.as_ref() else {
            return Ok(()); // 上面刚置位,理论不可达
        };
        reassert(drift, win);
        Ok(())
    })
}

/// 唤起悬浮条:补丁 → 多屏定位(鼠标所在屏,居中偏上)→ 补 first responder →
/// `orderFrontRegardless`(不激活 app、不切 Space)→ `makeKeyWindow`(成 key 不夺前台,
/// PoC-1b 结论 #6)。
pub fn show_palette() -> Result<()> {
    ensure_patched()?;
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("show_palette 非主线程调用"));
    };
    PALETTE.with(|cell| {
        let mut st = cell.borrow_mut();
        let Some(win) = st.palette.clone() else {
            return Err(anyhow!("悬浮条窗口未就绪(视口尚未创建)"));
        };
        let anchor = position_palette(&win, mtm);
        st.anchor = Some(anchor);
        // 顺序照 PoC-1b `show_and_make_key`:先补 first responder,再 orderFrontRegardless,
        // 最后 makeKeyWindow(定位先于显示,避免可见后跳位)。
        let responder_ok = restore_first_responder(&win);
        win.orderFrontRegardless();
        win.makeKeyWindow();
        log::info!(
            "悬浮条唤起:origin=({:.0}, {:.0}) 顶边 y={:.0} 高 {:.0};first_responder={responder_ok} isKeyWindow={} isVisible={}",
            anchor.x,
            anchor.top_y - anchor.h,
            anchor.top_y,
            anchor.h,
            win.isKeyWindow(),
            win.isVisible()
        );
        Ok(())
    })
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
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    PALETTE.with(|cell| {
        let st = cell.borrow();
        let Some(root) = st.root.clone() else {
            return Err(anyhow!("根窗句柄未记录(attach_root 未执行)"));
        };
        root.orderFrontRegardless();
        root.makeKeyWindow();
        root.makeMainWindow();
        log::info!(
            "完整窗口(根窗)唤起:isVisible={} isKey={}",
            root.isVisible(),
            root.isKeyWindow()
        );
        Ok(())
    })
}

/// 重锚:窗口高度变化(结果列表出现/收起)时保持顶边不动。
///
/// 依据:宽度/位置由平台层摆了脚(原生 `setFrameOrigin`),而 egui 的
/// `InnerSize` 走 winit `setContentSize`,其保持的角在 AppKit 侧不作保证——
/// 每次悬浮条 pass 在高度变化时重摆一次,顶边恒定(实测定:内容向下扩)。
pub fn reanchor() {
    let Some(_mtm) = MainThreadMarker::new() else {
        return; // 非主线程:no-op(不该发生)
    };
    PALETTE.with(|cell| {
        let mut st = cell.borrow_mut();
        let (Some(win), Some(a)) = (st.palette.clone(), st.anchor) else {
            return;
        };
        let h = win.frame().size.height;
        if (h - a.h).abs() < 0.5 {
            return; // 高度未变:不动
        }
        win.setFrameOrigin(NSPoint::new(a.x, a.top_y - h));
        st.anchor = Some(Anchor { h, ..a });
    });
}

/// 退出/销毁前还原窗口类(否则 winit drop → abort;spike 三方对照实证)。
pub fn revert_class() {
    PALETTE.with(|cell| {
        let mut st = cell.borrow_mut();
        let (Some(win), Some(orig)) = (st.palette.clone(), st.orig_class) else {
            return;
        };
        let obj: *mut AnyObject = (Retained::as_ptr(&win) as *const NSWindow)
            .cast_mut()
            .cast();
        let cur = obj_class(obj);
        if class_name(cur) == st.class_before {
            return; // 已是原类(重复调用/未换类)
        }
        // SAFETY:orig 是换类前 runtime 实测读到的类指针(类对象由运行时持有,存活);
        // 换回是 `object_setClass` 的逆操作,实例大小不变的前提同换类时。
        let old = unsafe { &*objc2::ffi::object_setClass(obj, orig) };
        log::info!(
            "悬浮条窗口类还原:{} → {}(原类回读 {})",
            class_name(cur),
            st.class_before,
            class_name(old)
        );
        st.orig_class = None;
    });
}

/// 是否已接上悬浮条窗(诊断/首帧接线轮询用)。
pub fn attached() -> bool {
    PALETTE.with(|cell| cell.borrow().palette.is_some())
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
    let app = NSApplication::sharedApplication(mtm);
    for win in app.windows().iter() {
        if root.is_some_and(|r| std::ptr::eq(r, &*win)) {
            continue;
        }
        if win.title().to_string() == PALETTE_TITLE {
            return Some(win);
        }
    }
    None
}

/// 换类:NSWindow → `MacePalettePanel`(实例大小须一致,否则拒绝)。
fn convert(st: &mut PaletteState, win: &NSWindow) -> Result<()> {
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
    st.class_before = class_name(before);
    st.orig_class = Some(before as *const AnyClass);
    log::info!(
        "悬浮条窗换类:{} → {}(旧类回读 {};实例大小 {size_before}=={size_target})",
        st.class_before,
        class_name(target),
        class_name(old)
    );
    configure(win);
    Ok(())
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
    log::info!(
        "悬浮条窗属性:level={} CanJoinAllSpaces={} FullScreenAuxiliary={} styleMask={:#x} first_responder={ok}",
        ns.level(),
        ns.collectionBehavior()
            .contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
        ns.collectionBehavior()
            .contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
        ns.styleMask().0
    );
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

/// 定位:鼠标所在屏可见区内水平居中、顶边在上 1/4 处;越界 clamp 回可见区。
fn position_palette(win: &NSWindow, mtm: MainThreadMarker) -> Anchor {
    let mouse = NSEvent::mouseLocation();
    let visible = screen_with_point(mtm, mouse)
        .map(|s| s.visibleFrame())
        .unwrap_or_else(|| {
            // 理论不可达(至少一屏);退化为主屏整屏,仍给确定位置。
            log::warn!("鼠标所在屏查不到:退化使用零原点可见区");
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1920.0, 1080.0))
        });
    let frame = win.frame();
    let (w, h) = (frame.size.width, frame.size.height);
    let min_x = visible.origin.x + EDGE_MARGIN;
    let max_x = (visible.origin.x + visible.size.width - w - EDGE_MARGIN).max(min_x);
    let x = (visible.origin.x + (visible.size.width - w) / 2.0).clamp(min_x, max_x);
    let top_y = visible.origin.y + visible.size.height * (1.0 - TOP_INSET_FRACTION);
    let y = (top_y - h).max(visible.origin.y + EDGE_MARGIN);
    win.setFrameOrigin(NSPoint::new(x, y));
    Anchor { x, top_y, h }
}

//! macOS 平台补丁实现(objc2;unsafe 集中在 `platform` 模块的 mac.rs / mac_quicklook.rs,
//! SAFETY 逐处注释)。
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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use block2::RcBlock;
use eframe::egui;
use objc2::ffi::NSInteger;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AnyThread, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertSecondButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSBackingStoreType, NSBezierPath, NSBitmapImageRep, NSColor, NSCompositingOperation,
    NSDeviceRGBColorSpace, NSEvent, NSEventModifierFlags, NSFloatingWindowLevel, NSGraphicsContext,
    NSImage, NSMenu, NSMenuItem, NSModalResponseAbort, NSModalResponseCancel, NSModalResponseOK,
    NSOpenPanel, NSPanel, NSPasteboard, NSPasteboardWriting, NSResponder, NSScreen, NSView,
    NSViewLayerContentsPlacement, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior,
    NSWindowOrderingMode, NSWindowStyleMask, NSWorkspace,
    NSWorkspaceActiveSpaceDidChangeNotification,
};
use objc2_foundation::{
    NSArray, NSData, NSError, NSHTTPURLResponse, NSLocale, NSMutableURLRequest, NSNotification,
    NSObject, NSPoint, NSRect, NSSize, NSString, NSURLCustomIconKey, NSURLRequestCachePolicy,
    NSURLResponse, NSURLSession, NSURL,
};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 枚举当前挂载的本地外接卷名(设置页「外接磁盘」分组;与 daemon 同判定,
/// 副本实现——app 不依赖 daemon 内部模块):非 dot 目录 + MNT_LOCAL +
/// 非 MNT_DONTBROWSE + 自挂载校验(挂载点须为该 /Volumes 路径本身,剔除
/// Firmlink 数据卷入口与系统卷)+ 非网络文件系统。
pub fn local_volume_names() -> Vec<String> {
    const NETWORK_FSTYPES: [&[u8]; 6] = [b"smbfs", b"nfs", b"afp", b"cifs", b"webdav", b"ftp"];
    let Ok(rd) = std::fs::read_dir("/Volumes") else {
        return vec![];
    };
    let mut names: Vec<String> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| match p.file_name().and_then(|n| n.to_str()) {
            Some(n) if n.starts_with('.') => false,
            Some(_) => true,
            None => false,
        })
        .filter_map(|p| {
            let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
            // SAFETY:statfs 只读路径字符串;输出结构栈上全量初始化。
            let mut st: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
                return None;
            }
            let (flags, mnt_len) = (
                st.f_flags as i32,
                st.f_mntonname.iter().position(|&x| x == 0)?,
            );
            if flags & libc::MNT_LOCAL == 0
                || flags & libc::MNT_DONTBROWSE != 0
                || st.f_mntonname[..mnt_len]
                    .iter()
                    .map(|&x| x as u8)
                    .collect::<Vec<u8>>()
                    != p.as_os_str().as_encoded_bytes()
            {
                return None;
            }
            let ty_len = st.f_fstypename.iter().position(|&x| x == 0)?;
            let fstype: Vec<u8> = st.f_fstypename[..ty_len].iter().map(|&x| x as u8).collect();
            if NETWORK_FSTYPES.contains(&fstype.as_slice()) {
                return None;
            }
            p.file_name()?.to_str().map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

/// 按进程组发信号:`kill(-pid, sig)`(子进程以 `process_group(0)` 自立新组,
/// 组 id 即其 pid)。安装器探测 shell 超时时整组终止,不留孤儿子进程。
pub fn kill_process_group(pid: u32, signal: i32) {
    // SAFETY: kill(2) 只作用于传入的进程组;pid 由 Child::id 取得,
    // 调用方在超时路径上使用,不存在内存安全问题。
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

/// 悬浮条窗口标题(定义在 facade,ui 与平台层共用;`NSApp.windows()` 中按此定位)。
use super::{
    monotonic_ms, palette_width, settings_window_titles, MenuEntry, SpaceChange, ICON_PX,
    PALETTE_TITLE,
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

// ---------------------------------------------------------------------------
// 原生菜单 target/action(菜单项选择回传;不依赖 highlightedItem)

/// 菜单选择 tag 的进程内落点(target/action 回传;-1 = 未选择)。
static MENU_CHOICE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
const NO_MENU_CHOICE: i64 = -1;

// 菜单 target:叶子项 action 的接收者(仅记录选择落点,不碰窗口状态)。
define_class!(
    // SAFETY(define_class 的 unsafe 契约):超类 NSObject 无子类化约束;
    // 无新增实例变量(ivars 缺省 `()`);方法签名与 AppKit 一致(单参 NSMenuItem,
    // 可为 nil);无 Drop。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MaceMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuItemChosen:))]
        fn menu_item_chosen(&self, sender: Option<&NSMenuItem>) {
            let tag = sender.map_or(NO_MENU_CHOICE, |item| item.tag() as i64);
            MENU_CHOICE.store(tag, std::sync::atomic::Ordering::SeqCst);
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    /// 创建 target 实例(主线程;实例由调用方持有——NSMenuItem.target 为弱引用,
    /// 须活到 `popUpMenuPositioningItem` 返回)。
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY:this 为主线程刚分配、ivars 已置的实例;本类未覆写 init,走 NSObject。
        unsafe { msg_send![super(this), init] }
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
    /// 全隐缩窗时保存的三窗 contentSize(Some = 当前处于 1×1 缩窗态)。
    ///
    /// 背景:隐藏走 `orderOut` 不销毁,三视口的 GL drawable 池(每窗 1 块
    /// `IOGPUSurfaceMTL` + 3 块 AppKit 共享 WindowServer)常驻约 78MB。
    /// `clearDrawable` 实测只能释放部分且数秒后被 AppKit 重建;把**隐藏窗口
    /// 的 contentSize 缩到 1×1** 可让 WindowServer/GL 真正回收 surface
    /// (缩窗 + 清 drawable 实测 86.4MB → ≈150KB)。显示前按此处保存的尺寸恢复。
    shrunk: ShrunkSizes,
}

/// 缩窗前的 contentSize 快照(主线程;三窗各一份)。
///
/// 不变式:`Some(size)` = 该窗口当前处于 1×1 缩窗态,显示前须恢复为 `size`。
/// 因此**已缩过的窗口不得重新捕获**——重新捕获时读到的是 1×1,会覆盖原始尺寸
/// (C1:用户报告的「完整窗口打不开」根因)。
#[derive(Default, Clone, Copy, PartialEq, Debug)]
struct ShrunkSizes {
    root: Option<NSSize>,
    palette: Option<NSSize>,
    settings: Option<NSSize>,
}

impl ShrunkSizes {
    fn slot(&self, kind: MainWindow) -> Option<NSSize> {
        match kind {
            MainWindow::Root => self.root,
            MainWindow::Palette => self.palette,
            MainWindow::Settings => self.settings,
        }
    }

    fn set(&mut self, kind: MainWindow, size: Option<NSSize>) {
        match kind {
            MainWindow::Root => self.root = size,
            MainWindow::Palette => self.palette = size,
            MainWindow::Settings => self.settings = size,
        }
    }

    fn take(&mut self, kind: MainWindow) -> Option<NSSize> {
        let saved = self.slot(kind);
        self.set(kind, None);
        saved
    }
}

/// 为窗口登记缩前尺寸(纯函数,单测钉住 C1)。已有快照 = 该窗口仍处于 1×1,
/// 保留首次值并返回 `false`(调用方不得再缩/重新捕获);返回 `true` 才表示
/// 本次应执行缩窗。
///
/// 刻意逐项合并而非整体赋值:第二次全隐缩窗时 root/settings 仍在 1×1,
/// 整体覆盖会把它们的「原始尺寸」写成 1×1。
fn record_shrink_slot(saved: &mut ShrunkSizes, kind: MainWindow, observed: NSSize) -> bool {
    if saved.slot(kind).is_some() {
        return false;
    }
    saved.set(kind, Some(observed));
    true
}

/// 三个主视口窗口(缩窗/恢复的定向对象)。
#[derive(Clone, Copy, Debug)]
enum MainWindow {
    Root,
    Palette,
    Settings,
}

/// 全隐缩窗定时器代号:显示动作递增即可作废尚未触发的旧定时器。
static SHRINK_GEN: AtomicU64 = AtomicU64::new(0);

/// 缩窗后清字形图集要用到的 egui Context(App::new 安装一次)。
static RECLAIM_CTX: Mutex<Option<egui::Context>> = Mutex::new(None);

/// 安装字形图集回收用的 Context(主线程,App::new 调用一次)。
pub fn install_surface_reclaim(ctx: egui::Context) {
    let mut slot = match RECLAIM_CTX.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Some(ctx);
}

/// 清空 egui 字形图集并请求一帧,让渲染端把 32MB 的 GPU 纹理重建成最小尺寸。
///
/// 必须在主线程且不在 egui pass 内(本函数由缩窗定时器的主队列块调用)。
/// 隐藏视口在 `App::ui`/视口回调里会被跳过绘制,因此这一帧不会产生新字形,
/// `take_delta` 只会带上缩小后的整图,`glTexImage2D` 随之释放旧纹理。
fn release_glyph_atlas() -> bool {
    let ctx = match RECLAIM_CTX.lock() {
        Ok(g) => g.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };
    let Some(ctx) = ctx else {
        log::warn!("全隐缩窗:回收 Context 未安装,跳过字形图集释放");
        return false;
    };
    let [before_w, before_h] = ctx.fonts(|f| f.font_image_size());
    // fonts_mut 会清空图集、缩回最小尺寸,并让下一次 begin_pass 清空所有 galley 缓存。
    ctx.fonts_mut(|f| f.clear_glyph_atlas());
    let [after_w, after_h] = ctx.fonts(|f| f.font_image_size());
    ctx.request_repaint();
    log::info!(
        "全隐缩窗:字形图集 {before_w}×{before_h} → {after_w}×{after_h},已请求重绘帧(释放 GPU 纹理)"
    );
    true
}

/// 全隐后等待多久缩窗(秒;`MCE_SURFACE_SHRINK_SECS` 仅供实机验证调参)。
const SHRINK_AFTER_SECS: u64 = 30;

/// 缩窗延迟(纯函数,单测钉住):合法环境值优先,否则默认 30s。
fn shrink_delay_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(SHRINK_AFTER_SECS)
}

/// 定时器代号是否仍然有效(纯函数,单测钉住):显示动作会递增代号作废旧定时器。
fn shrink_timer_is_current(scheduled: u64, current: u64) -> bool {
    scheduled == current
}

/// 安排一次「全隐 N 秒后缩窗」一次性定时器(主队列;重复调用覆盖旧代)。
///
/// 必须在主线程调用;全隐期没有帧,定时器是唯一能跑到缩窗逻辑的路径。
/// 触发时再核对三窗原生 `isVisible()`——期间若有窗口显示,直接跳过
/// (显示路径已递增 `SHRINK_GEN`,正常情况连这里都到不了)。
pub fn schedule_surface_shrink() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let gen = SHRINK_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let secs = shrink_delay_secs(std::env::var("MCE_SURFACE_SHRINK_SECS").ok().as_deref());
    let Ok(when) = dispatch2::DispatchTime::try_from(Duration::from_secs(secs)) else {
        return;
    };
    let _ = dispatch2::DispatchQueue::main().after(when, move || {
        let current = SHRINK_GEN.load(Ordering::SeqCst);
        if !shrink_timer_is_current(gen, current) {
            log::info!("全隐缩窗:定时器已作废(gen {gen} != {current},期间有显示动作)");
            return;
        }
        if shrink_hidden_windows() {
            log::info!("全隐 {secs}s:三视口 contentSize 缩到 1×1(surface 释放)");
        }
    });
}

/// 作废待触发的缩窗定时器(任何显示动作入口调用;不释放已缩窗口的尺寸,
/// 尺寸恢复由 [`restore_window_size`] 在具体显示路径完成)。
pub fn cancel_surface_shrink() {
    SHRINK_GEN.fetch_add(1, Ordering::SeqCst);
    // 任何显示路径都会走到这里:退出「三窗全隐」,健康探测恢复 5s 间隔。
    crate::engine_launcher::set_ui_hidden(false);
}

/// 三窗均不可见时把 contentSize 缩到 1×1(返回本次是否有窗口被新缩)。
///
/// 只在主线程;任一主窗可见则整体跳过。尺寸快照存 [`PaletteState::shrunk`],
/// 显示前由 [`restore_window_size`] 恢复。AppKit 调用期间**不持** PALETTE 借用
/// (setContentSize 可触发 winit 同步重入一帧)。
///
/// **逐窗口合并**(C1):已保存过快照的窗口仍是 1×1,直接跳过——绝不能重新
/// 捕获后整体覆盖(第二次缩窗会把 root/settings 的原始尺寸写成 1×1,完整
/// 窗口/设置从此恢复成 1×1 不可用)。
///
/// **重入安全**(I2):每个窗口「先登记尺寸 → 缩 → 立刻回读三窗可见性」。
/// `setContentSize` 可能被 winit 同步派发成一帧,重入帧若执行显示路径并
/// orderFront,就回滚本窗口并放弃本轮,避免把刚显示的窗口继续缩成 1×1。
#[allow(deprecated)] // NSOpenGLContext 自 macOS 10.14 起标记废弃,但 glow/CGL 后端仍用它。
pub(crate) fn shrink_hidden_windows() -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let (root, palette) = PALETTE.with(|cell| {
        let st = cell.borrow();
        (st.root.clone(), st.palette.clone())
    });
    let (Some(root), Some(palette)) = (root, palette) else {
        return false; // 视口窗尚未创建:下次隐藏再排定时器
    };
    let Some(settings) = find_window_by_title(mtm, Some(&root), &settings_window_titles()) else {
        return false;
    };
    let windows = [
        (&root, MainWindow::Root, "root"),
        (&palette, MainWindow::Palette, "palette"),
        (&settings, MainWindow::Settings, "settings"),
    ];
    // 最小化窗口的恢复可能绕过本 app 的显示路径(点 Dock 图标),1×1 尺寸会
    // 直接呈现在用户面前;全屏窗口的尺寸由系统 Space 管理。两类一律不缩。
    if windows.iter().any(|(win, _, _)| {
        win.isVisible()
            || win.isMiniaturized()
            || win.styleMask().contains(NSWindowStyleMask::FullScreen)
    }) {
        log::info!("全隐缩窗:仍有可见/最小化/全屏主窗口,跳过");
        crate::engine_launcher::set_ui_hidden(false);
        return false;
    }
    // 三窗全隐已确认:健康探测退避到 30s(见 engine_launcher::monitor_interval)。
    crate::engine_launcher::set_ui_hidden(true);

    let mut kept = false;
    for (win, kind, name) in windows {
        let before = win
            .contentView()
            .map_or_else(|| win.frame().size, |view| view.frame().size);
        // 先登记再缩:重入帧里的显示路径必须能读到原始尺寸。
        let should_shrink =
            PALETTE.with(|cell| record_shrink_slot(&mut cell.borrow_mut().shrunk, kind, before));
        if !should_shrink {
            log::debug!(
                "全隐缩窗:{name} 已处于缩窗态(保留首次快照 {:.0}×{:.0}),跳过",
                before.width,
                before.height
            );
            continue;
        }
        win.setContentSize(NSSize::new(1.0, 1.0));
        log::info!(
            "全隐缩窗:{name} {:.0}×{:.0} → 1×1",
            before.width,
            before.height
        );
        // AppKit 调用后复核:期间有窗口被显示(重入帧)→ 回滚本窗口并放弃。
        if let Some((_, _, visible_name)) = windows.iter().find(|(w, _, _)| w.isVisible()) {
            log::warn!("全隐缩窗:{visible_name} 在缩窗期间变为可见,回滚 {name} 并放弃本轮");
            win.setContentSize(before);
            PALETTE.with(|cell| cell.borrow_mut().shrunk.set(kind, None));
            crate::engine_launcher::set_ui_hidden(false);
            return kept;
        }
        kept = true;
    }
    if !kept {
        // 三窗早已全部缩过:无需重复清 drawable/图集。
        return false;
    }

    // 仅缩窗有时仍会留下上一尺寸的 AppKit 后备用 surface(实测 9–19MB 波动);
    // 再对三窗 drawable 做一次 clearDrawable,并把上下文停到已缩为 1×1 的
    // root 视图上(ctx.view 保持非 nil,glutin 的 is_view_current 不会 panic;
    // 当前 drawable 只剩 2×2 像素)。两者合起来实测稳定到 ≈150KB。
    //
    // 不做这一步也不会崩,只是释放不彻底;currentContext 取不到(理论不可达:
    // glutin 让上下文常驻 current)时如实留痕并只保留缩窗效果。
    match objc2_app_kit::NSOpenGLContext::currentContext() {
        Some(ctx) => {
            // 只清「本 app 三个视口」的上下文:currentContext 正常即 eframe 的
            // glow 上下文;若 view 关联到别的视图(未来接入第二个 GL 组件),跳过清除。
            let current_view = ctx.view(mtm);
            let owns = current_view.as_deref().is_none_or(|v| {
                let vp = v as *const NSView;
                windows
                    .iter()
                    .filter_map(|(win, _, _)| win.contentView())
                    .any(|own| Retained::as_ptr(&own) == vp)
            });
            if !owns {
                log::warn!("全隐缩窗:当前 GL 上下文不属于三视口,仅缩窗(未清 drawable)");
                return true;
            }
            for (win, _, name) in windows {
                let Some(view) = win.contentView() else {
                    continue;
                };
                ctx.setView(Some(&view), mtm);
                ctx.clearDrawable();
                log::info!("全隐缩窗:{name} drawable cleared");
            }
            // 停到 root 的 1×1 视图(show 时 eframe make_current 会重新绑定)。
            if let Some(view) = root.contentView() {
                ctx.setView(Some(&view), mtm);
                log::info!("全隐缩窗:GL 上下文停在 root 1×1 视图");
            }
        }
        None => log::warn!("全隐缩窗:取不到当前 GL 上下文,仅缩窗(未清 drawable)"),
    }
    // 字形图集(4096×2048×4 ≈ 32MB)占 IOAccelerator 大头:缩窗/清 drawable 后
    // 再清图集并请求一帧,让 GPU 纹理重建成最小尺寸(4096×32)。
    let _ = release_glyph_atlas();
    true
}

/// 显示前恢复某一主窗的 contentSize(未缩过 = no-op;返回是否恢复)。
fn restore_window_size(win: &NSWindow, kind: MainWindow) -> bool {
    let saved = PALETTE.with(|cell| cell.borrow_mut().shrunk.take(kind));
    match saved {
        Some(size) => {
            win.setContentSize(size);
            log::info!(
                "缩窗恢复:{kind:?} 1×1 → {:.0}×{:.0}",
                size.width,
                size.height
            );
            true
        }
        None => false,
    }
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
    // 全隐缩窗态:先作废定时器、恢复 contentSize,再定位( position 读取窗高)。
    cancel_surface_shrink();
    restore_window_size(&win, MainWindow::Palette);
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
/// Dock 图标显隐(external 需求:完整窗口期给用户「永远找得回」的入口)。
///
/// App 是 LSUIElement(Accessory,菜单栏应用形态,无常驻 Dock 图标);完整窗口
/// 可见期切 Regular 让 Dock 图标出现——点击即置前、⌘Tab 可见、可最小化到
/// 程序坞;窗口隐藏后切回 Accessory 收回图标。运行时切换 Accessory↔Regular
/// 是 AppKit 明确允许的转换(仅 Prohibited 受限)。
pub fn set_dock_icon(show: bool) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    let target = if show {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };
    if app.activationPolicy() != target {
        let ok = app.setActivationPolicy(target);
        log::info!(
            "Dock 图标{}:{}",
            if show { "显示" } else { "收回" },
            if ok { "ok" } else { "失败" }
        );
    }
}

pub fn show_root_window() -> Result<()> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("show_root_window 非主线程调用"));
    };
    // 重入安全(同 show_palette):句柄短借用取出,AppKit 调用全在借用外
    // (activate/orderFront 可被 winit 同步派发 → 重入一帧)。
    let root = PALETTE
        .with(|cell| cell.borrow().root.clone())
        .ok_or_else(|| anyhow!("根窗句柄未记录(attach_root 未执行)"))?;
    // 全隐缩窗态:先作废定时器并恢复尺寸,再激活/置前(恢复会触发 resize 通知,
    // 后续 winit/egui 首帧即按正确尺寸布局)。
    cancel_surface_shrink();
    restore_window_size(&root, MainWindow::Root);
    let app = NSApplication::sharedApplication(mtm);
    // Dock 图标(Accessory→Regular policy 切换)先于激活/置前:policy 切换会让系统
    // 重算本 app 的窗口归属,放在置前之后实测概率把窗口压回普通层(2026-10-29
    // 用户报「完整窗口概率性被其他应用遮挡」)。
    set_dock_icon(true);
    // 硬激活:macOS 14+ 起无参 `activate()` 只在系统判定「用户交互允许」时生效,
    // 悬浮条是 non-activating panel(点击不授予激活许可),非激活 app 下调用概率性
    // 静默失败——窗口 orderFront 后拿不到 key,随后被前台应用压下(同上用户报告)。
    // `activateIgnoringOtherApps` 虽标弃用(官方建议 NSApp.activate),但它正是
    // 「无视前台归属抢激活」的语义,启动器类应用唯一可靠路径(实机 2026-10-29)。
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    // 最小化态下单发 orderFront/makeKey 不会从 Dock 恢复:先 deminiaturize
    //（与设置窗同法;2026-10-18 review 补）。
    if root.isMiniaturized() {
        root.deminiaturize(None);
        log::info!("完整窗口(根窗)最小化中:deminiaturize 恢复");
    }
    // 从悬浮条(可能是他人全屏 Space 上的 non-activating panel)进入完整窗口时,
    // 普通窗口默认留在 App 原 Space,会出现「窗口显示在别处且不是 key」。显式允许
    // 跟随当前 Space;保留历史定案的 orderFrontRegardless(激活未完成也置顶),
    // 再用 makeKeyAndOrderFront 取得 key(实机 2026-10-19)。
    root.setCollectionBehavior(
        root.collectionBehavior() | NSWindowCollectionBehavior::MoveToActiveSpace,
    );
    root.orderFrontRegardless();
    root.makeKeyAndOrderFront(None);
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
    // 全隐缩窗态:先恢复尺寸再居中/置前(居中按恢复后的 frame 计算)。
    cancel_surface_shrink();
    restore_window_size(&win, MainWindow::Settings);
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
/// `items` = 平铺菜单项列表([`MenuEntry`]):每项经 target/action(`menuItemChosen:`)
/// 回传 tag;返回所选项的 tag(None = 取消/Esc/未选中)。
/// 位置为视图坐标(左上原点、y 向下);PoC-3 实测视图坐标与 egui 坐标同值可直接
/// 互用,此处保留换算核对(弹出前把鼠标窗口坐标经 `convertPoint_fromView` 换算
/// 一份并入日志),以实机观测验证该结论。
///
/// 三个 PoC-3 必知坑在此落地:
/// ① 无 action 的项默认被自动验证判禁用 → `setAutoenablesItems(false)`,
///    每项按 `enabled` 显式启用;
/// ② `NSMenu`/`NSMenuItem` 均 MainThreadOnly(alloc 需 mtm 凭证);
/// ③ `popUpMenuPositioningItem` 返回值 bool 语义不稳定(24 次成功选择里 1 次
///    false)→ **判定一律用 target/action 回传的 tag**(`MENU_CHOICE`),不读返回值。
///    旧路径 `highlightedItem()` 在子菜单下不可靠,已弃用。
///
/// target/action:`NSMenuItem.target` 为**弱引用**,故 `MenuTarget` 必须是本函数
/// 局部变量并活到 `popUpMenuPositioningItem` 返回(声明在 menu 之前,作用域覆盖)。
///
/// 弹出期间 egui 冻结(嵌套 run loop 走菜单跟踪模式;winit 对 modal 期事件排队
/// 至默认模式、关闭后投递,PoC-3 实测「关闭当帧恢复」)——已判定可接受。
///
/// 弹出位置 = 点击点(不偏移):菜单左上角落在点击点,指针停在首项上会被预高亮
/// (PoC-3 观测)——与系统菜单同观感,产品接受(偏移放右下会让菜单脱离光标,
/// 反倒不像系统菜单;实机手感终验随 Task 18 人工清单)。
pub fn popup_menu(
    items: &[MenuEntry],
    at_view_pos: (f64, f64),
    frame: &eframe::Frame,
) -> Option<i64> {
    let Some(mtm) = MainThreadMarker::new() else {
        log::warn!("右键菜单:非主线程调用,忽略");
        return None;
    };
    let view = frame_view(frame)?;
    // 选择落点先复位:本次未选择(取消/Esc)必须与「选了 tag = -1」区分开。
    MENU_CHOICE.store(NO_MENU_CHOICE, std::sync::atomic::Ordering::SeqCst);
    // target 为弱引用目标:须活到阻塞弹出返回(本函数作用域自然覆盖)。
    let target = MenuTarget::new(mtm);
    let menu = build_context_menu(mtm, items, &target);
    let location = NSPoint::new(at_view_pos.0, at_view_pos.1);
    let mouse_view = view
        .window()
        .map(|w| view.convertPoint_fromView(w.mouseLocationOutsideOfEventStream(), None));
    let t0 = std::time::Instant::now();
    // 阻塞调用:内部为嵌套 run loop(菜单跟踪模式),返回即菜单关闭。
    let popped = menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(&view));
    let raw = MENU_CHOICE.swap(NO_MENU_CHOICE, std::sync::atomic::Ordering::SeqCst);
    let choice = (raw != NO_MENU_CHOICE).then_some(raw);
    log::info!(
        "右键菜单:顶层 {} 项,位置=({:.1},{:.1}) 鼠标换算={:?},阻塞 {}ms,返回值={popped}(仅留痕),选择 tag={choice:?}",
        items.len(),
        location.x,
        location.y,
        mouse_view.map(|p| (p.x, p.y)),
        t0.elapsed().as_millis()
    );
    choice
}

/// 把选中文件 URL 写入通用剪贴板(v2.3 §5.1;语义 = Finder「拷贝」)。
///
/// 写入 NSPasteboard 的 `NSURL` 数组即可被 Finder 等接收方当作文件拷贝
/// (NSURL 遵循 `NSPasteboardWriting`);须主线程；绘制期调用安全（winit 事件处理中
/// AppKit 通知排队），主队列直调路径不得持共享锁。
pub fn copy_files(paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if MainThreadMarker::new().is_none() {
        return Err(anyhow!("{}", crate::i18n::s().copy_file_failed));
    }
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(p)))
        .collect();
    let objects: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = urls
        .iter()
        .map(|url| ProtocolObject::from_ref(&**url))
        .collect();
    let array = NSArray::from_slice(&objects);
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if pasteboard.writeObjects(&array) {
        log::info!("拷贝文件:{} 项已写入剪贴板", paths.len());
        Ok(())
    } else {
        Err(anyhow!("{}", crate::i18n::s().copy_file_failed))
    }
}

// ---------------------------------------------------------------------------
// 托盘菜单自弹(2026-10-16 实机修复:副屏点击 → 菜单落在主屏)

/// 根窗是否 key(egui viewport focused 状态补正用;未接句柄 = None)。
pub(super) fn root_is_key() -> Option<bool> {
    PALETTE.with(|cell| cell.borrow().root.as_ref().map(|root| root.isKeyWindow()))
}

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
/// `pub(super)`:同层 `mac_quicklook` 关闭面板后取结果窗口归还焦点。
pub(super) fn frame_view(frame: &eframe::Frame) -> Option<Retained<NSView>> {
    let handle = match frame.window_handle() {
        Ok(h) => h,
        Err(e) => {
            log::warn!("原生取窗:窗口句柄取不到({e})");
            return None;
        }
    };
    let RawWindowHandle::AppKit(h) = handle.as_raw() else {
        log::warn!("原生取窗:非 AppKit 窗口句柄");
        return None;
    };
    // SAFETY:h.ns_view 指向 winit 为本 app 创建、当前存活的 NSView(主线程持有),
    // 生命周期覆盖 eframe 主循环;Retained::retain 增引用计数延长借用
    // (同 attach_root 的取窗路径)。
    unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }
}

/// 构建右键菜单(菜单项 target/action、可选快捷键、分隔线)。
///
/// `NSMenuItem.target` 为弱引用,由调用方(`popup_menu`)保活 `target` 到弹出返回。
fn build_context_menu(
    mtm: MainThreadMarker,
    items: &[MenuEntry],
    target: &MenuTarget,
) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    // 无 action 的项会被自动验证判为禁用;关掉自动验证,逐项显式启用——
    // 项可高亮、可选择(PoC-3 实锤)。
    menu.setAutoenablesItems(false);
    add_menu_entries(mtm, &menu, items, target);
    menu
}

/// 填充菜单项:标题 + tag + enabled + 可选快捷键 + target/action;分隔线。
fn add_menu_entries(
    mtm: MainThreadMarker,
    menu: &NSMenu,
    items: &[MenuEntry],
    target: &MenuTarget,
) {
    for entry in items {
        match entry {
            MenuEntry::Item {
                title,
                tag,
                enabled,
                shortcut,
            } => {
                // SAFETY:selector 为本类实装的 menuItemChosen:(签名匹配,AppKit 在
                // 菜单跟踪期于主线程投递);标题与键等价串为本次调用内构造的
                // NSString 临时值,AppKit 拷贝入项;target 为弱引用(调用方保活)。
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(title),
                        Some(objc2::sel!(menuItemChosen:)),
                        &NSString::from_str(""),
                    )
                };
                debug_assert!(*tag != NO_MENU_CHOICE, "菜单 tag -1 是未选择哨兵");
                item.setTag(*tag as NSInteger); // i64 → isize(边界转换,见 popup_menu)
                item.setEnabled(*enabled);
                if let Some(shortcut) = shortcut {
                    item.setKeyEquivalent(&NSString::from_str(shortcut.key));
                    let mut mask = NSEventModifierFlags::empty();
                    if shortcut.command {
                        mask |= NSEventModifierFlags::Command;
                    }
                    if shortcut.shift {
                        mask |= NSEventModifierFlags::Shift;
                    }
                    item.setKeyEquivalentModifierMask(mask);
                }
                let target_obj: &AnyObject = target.as_super().as_ref();
                // SAFETY:target_obj 指向调用方保活的 MenuTarget;setTarget 为
                // 弱引用赋值,selector 与目标类方法签名匹配。
                unsafe { item.setTarget(Some(target_obj)) };
                menu.addItem(&item);
            }
            MenuEntry::Separator => {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
        }
    }
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
    // 退出时作废尚未触发的缩窗定时器(避免退出流程里再动窗口尺寸)。
    cancel_surface_shrink();
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
    // 垫窗一并收掉(orderOut + 还原类 + 释放;进程退出前保持桌面干净)。
    // 类必须先于释放还原:垫窗在 `create_blur` 里换成了 NSPanel(换类时机实验),
    // 带 NSPanel 类释放会走 NSPanel 的 teardown 路径,AppKit KVO 记账对不上——
    // removeObserver `_windowLayerContext` 抛 `NSRangeException`(未注册即移除,
    // 2026-10-21 实机退出必现)。与面板窗同因(PoC-1b:「改类窗口被 drop → abort,
    // 还原即干净退出」),同药方。
    let blur = PALETTE.with(|cell| cell.borrow_mut().blur.take());
    if let Some(blur) = blur {
        blur.orderOut(None);
        let obj: *mut AnyObject = (Retained::as_ptr(&blur) as *const NSWindow)
            .cast_mut()
            .cast();
        let window_cls = NSWindow::class();
        let before = obj_class(obj);
        if !std::ptr::eq(before, window_cls) {
            // SAFETY:垫窗由普通 NSWindow 构造(`create_blur`),换回即逆操作;
            // NSWindow/NSPanel 实例大小已在换类时断言一致,类对象由运行时持有。
            unsafe {
                objc2::ffi::object_setClass(obj, window_cls as *const AnyClass);
            }
            log::info!("垫窗窗口类还原:{} → NSWindow(退出前)", class_name(before));
        }
        drop(blur); // 类已还原,释放走 NSWindow teardown,不再触发异常
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
    // 材质 = Popover(PoC-7 实机验收材质;2026-10-21 定版——UnderWindowBackground 奶白层
    // 近乎不透明,叠面板底后「发实无玻璃感」,弃用)。
    vib.setMaterial(NSVisualEffectMaterial::Popover);
    // win.setAlphaValue 不用:低于1会锐利漏背景(看到后面的字)
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
        "毛玻璃垫窗已建（普通 NSWindow/PoC-7 形态；material=Popover，面板之下；panel#{}）",
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

/// 取 APP bundle 自身图标(`NSWorkspace iconForFile:` 返回该 APP 的图标,自然含
/// 用户给 APP 设过的自定义图标)。调用方负责只传 APP bundle 路径。
///
/// 返回 64×64 **预乘** RGBA;由图标工作线程调用,只做 `NSWorkspace` 查询与返回
/// 图像的 TIFF 数据化,不创建/操作视图或窗口(跨线程依据见 `ui/icons.rs` 模块注释)。
pub fn app_icon_rgba(path: &str) -> Option<Vec<u8>> {
    let icon = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
    image_rgba(&icon)
}

/// `NSImage` → 64×64 **预乘** RGBA:直接把图像画进 64×64 位图上下文。
///
/// 旧实现走 `TIFFRepresentation` → `image` 解码 → Lanczos:本机系统图标是
/// 多页 TIFF(类型图标 32 页/≈37MB;APP 图标 16bit float 1024×1024/≈74MB),
/// 每个图标一次数十 MB 的临时分配,macOS 将其滞留为 `Malloc Large (empty)`。
/// 现在只让 AppKit 把目标尺寸(64pt)画进位图 represent — 由 AppKit 自己挑
/// 合适的表示并缩放,单次分配 ~16KB(加源表示解码,量级 MB 以内)。
///
/// 位图格式:8bit/通道、4 采样、alpha 在后、非 planar、deviceRGB、默认
/// bitmapFormat(预乘)——正是图标图集要求的预乘 RGBA 内存序。
///
/// 尺寸取 64×64:行内最大绘制矩形是悬浮条 `ROW_ICON = 32pt`,2x 下 = 64px
/// (1:1);完整窗口 `ROW_ICON = 16pt` → 32px(2x 过采样)。128px 对当前 UI
/// 属过度采样,且会让图集单元/内存翻四倍,故不采用。
fn image_rgba(icon: &NSImage) -> Option<Vec<u8>> {
    let px = ICON_PX as NSInteger;
    let bytes_per_row = px * 4;
    // SAFETY:planes = null 让 NSBitmapImageRep 自行分配像素缓冲;其余参数为
    // 常规 8bit RGBA 配置。返回对象在本函数内独占。
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            px,
            px,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            bytes_per_row,
            32,
        )
    }?;
    let len = (bytes_per_row * px) as usize;
    // SAFETY:非 planar + planes=null 时 AppKit 分配连续缓冲,长度 ≥
    // bytes_per_row × height;先清零,未绘制区域保持透明。
    unsafe {
        let data = rep.bitmapData();
        if data.is_null() {
            return None;
        }
        std::slice::from_raw_parts_mut(data, len).fill(0);
    }

    let gc = NSGraphicsContext::graphicsContextWithBitmapImageRep(&rep)?;
    let previous = NSGraphicsContext::currentContext();
    NSGraphicsContext::setCurrentContext(Some(&gc));
    // 源矩形必须是图像的**完整逻辑边界**(点),目标矩形才是 64×64 像素位图;
    // 两者都传目标矩形会从源图坐标里裁一块 64×64(图像往往只有 32pt),
    // AppKit 按实际内容绘制 → 图标只占单元格约一半,视觉变小。
    let size = icon.size();
    let source = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let dest = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
    icon.drawInRect_fromRect_operation_fraction(
        dest,
        source,
        NSCompositingOperation::SourceOver,
        1.0,
    );
    NSGraphicsContext::setCurrentContext(previous.as_deref());

    // SAFETY:同上的连续缓冲;同步拷贝后 rep 即释放。
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(data, len) }.to_vec())
}

/// 拉取 HTTPS 资源：同步返回响应体；非 200/非 HTTPS/超大/超时 = Err。
///
/// 会话为系统共享 `NSURLSession`（走系统 TLS 与代理）。completion 回调可能在
/// 其他线程执行，因此回调只捕获 `Sender`/`String`/`usize`（均为 Send），不触碰
/// UI。本函数同步阻塞至多 `timeout + 2s`，调用方须在非 UI 线程调用。
pub fn fetch_url_bytes(url: &str, timeout: Duration, max_bytes: usize) -> Result<Vec<u8>, String> {
    let ns_url =
        NSURL::URLWithString(&NSString::from_str(url)).ok_or_else(|| format!("URL 非法: {url}"))?;
    let scheme = ns_url.scheme().map(|s| s.to_string()).unwrap_or_default();
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(format!("仅允许 HTTPS: {scheme}"));
    }
    // 主机必须在请求前确定且非空，否则回调侧「最终主机」比较可能在两侧都缺失时
    // 通过（失效开放）。此后 expected_host 恒为非空字符串。
    let Some(expected_host) = ns_url
        .host()
        .map(|h| h.to_string().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
    else {
        return Err("URL 缺少主机".to_owned());
    };
    // 可变请求:附加 UA(macEverything/<版本>)——版本清单端按其聚合计数
    // 活跃安装分布(仅版本号,无设备标识;见 usage.md 隐私承诺)。
    let request = NSMutableURLRequest::requestWithURL_cachePolicy_timeoutInterval(
        &ns_url,
        NSURLRequestCachePolicy::ReloadIgnoringLocalCacheData,
        timeout.as_secs_f64(),
    );
    let ua = NSString::from_str(concat!("macEverything/", env!("CARGO_PKG_VERSION")));
    request.setValue_forHTTPHeaderField(Some(&ua), &NSString::from_str("User-Agent"));
    let session = NSURLSession::sharedSession();
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    let block: RcBlock<dyn Fn(*mut NSData, *mut NSURLResponse, *mut NSError)> = RcBlock::new(
        move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            // SAFETY: Foundation 只会传入有效对象或 null；handle_fetch_result 内部
            // 逐个判空，并且只在回调期间读取，不把借用带出。
            let result =
                unsafe { handle_fetch_result(data, response, error, &expected_host, max_bytes) };
            let _ = tx.send(result);
        },
    );
    // SAFETY: completionHandler 由 NSURLSession 复制并在其回调队列执行；捕获的
    // Sender/String/usize 均为 Send，符合该 API 的 sendable 要求。
    let task = unsafe { session.dataTaskWithRequest_completionHandler(&request, &block) };
    task.resume();
    match rx.recv_timeout(timeout.saturating_add(Duration::from_secs(2))) {
        Ok(result) => result,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // 请求超时按「空闲」计时：持续滴流的服务器可让任务长期存活。超时返回前
            // 显式取消，避免任务在后台继续占用连接/内存。
            task.cancel();
            Err("请求超时(未收到回调)".to_owned())
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            // 发送端已释放且无待收消息（回调不会再来）：任务已不可等待，同样取消。
            task.cancel();
            Err("检查线程通道已关闭".to_owned())
        }
    }
}

/// 校验并读取 NSURLSession 回调结果。
///
/// # Safety
/// 三个指针来自 Foundation 回调：要么为 null，要么指向在回调期间有效的对象。
unsafe fn handle_fetch_result(
    data: *mut NSData,
    response: *mut NSURLResponse,
    error: *mut NSError,
    expected_host: &str,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if let Some(error) = error.as_ref() {
        return Err(format!("网络错误: {}", error.localizedDescription()));
    }
    let Some(response) = response.as_ref() else {
        return Err("响应为空".to_owned());
    };
    let final_url = response.URL().ok_or_else(|| "响应缺少 URL".to_owned())?;
    let scheme = final_url
        .scheme()
        .map(|s| s.to_string())
        .unwrap_or_default();
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(format!("最终地址非 HTTPS: {scheme}"));
    }
    // 最终主机缺失按空串参与比较：expected_host 非空，故缺失恒失败（失效关闭）。
    let host = final_url
        .host()
        .map(|h| h.to_string().to_ascii_lowercase())
        .unwrap_or_default();
    if host != expected_host {
        return Err(format!("最终主机不符: {host:?}"));
    }
    // objc2 0.6 的 `downcast_ref` 只定义在 `AnyObject` 上；NSURLResponse 与
    // NSHTTPURLResponse 都是 Objective-C 对象（repr(C)、超类为零大小标记），
    // 故先按 AnyObject 解释，再由 `isKindOfClass` 判定。
    // SAFETY: response 在回调期间有效；AnyObject 可与任意 Objective-C 对象互解释。
    let response_obj: &AnyObject = &*(response as *const NSURLResponse).cast::<AnyObject>();
    let Some(http) = response_obj.downcast_ref::<NSHTTPURLResponse>() else {
        return Err("非 HTTP 响应".to_owned());
    };
    let status = http.statusCode();
    if status != 200 {
        return Err(format!("HTTP 状态 {status}"));
    }
    let Some(data) = data.as_ref() else {
        return Err("响应体为空".to_owned());
    };
    let len = data.length();
    if len > max_bytes {
        return Err(format!("响应体过大: {len} 字节"));
    }
    Ok(data.to_vec())
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

    /// 构造离线 `NSHTTPURLResponse`（不触网；URL/状态码可控）。
    fn http_response(url: &str, status: NSInteger) -> Retained<NSHTTPURLResponse> {
        let ns_url = NSURL::URLWithString(&NSString::from_str(url)).expect("test URL");
        // HTTPVersion/headerFields 传 nil 是初始化器允许的用法。
        NSHTTPURLResponse::initWithURL_statusCode_HTTPVersion_headerFields(
            NSHTTPURLResponse::alloc(),
            &ns_url,
            status,
            None,
            None,
        )
        .expect("NSHTTPURLResponse")
    }

    /// 直接驱动回调侧校验（离线；不创建网络任务）。
    fn call_handler(
        url: &str,
        status: NSInteger,
        expected_host: &str,
        body: &[u8],
        max_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        let response = http_response(url, status);
        let data = NSData::with_bytes(body);
        let response_ptr: *mut NSURLResponse = Retained::as_ptr(&response)
            .cast_mut()
            .cast::<NSURLResponse>();
        // SAFETY: 指针指向本函数内保活的对象，仅在校验期间使用；error 传 null。
        unsafe {
            handle_fetch_result(
                Retained::as_ptr(&data).cast_mut(),
                response_ptr,
                std::ptr::null_mut(),
                expected_host,
                max_bytes,
            )
        }
    }

    /// 回调侧拒绝：最终地址非 HTTPS（含请求前 HTTPS、响应被降级的情况）。
    #[test]
    fn fetch_rejects_non_https_final_url() {
        let err = call_handler("http://example.com/x", 200, "example.com", b"{}", 64)
            .expect_err("最终地址非 HTTPS 必须拒绝");
        assert!(err.contains("最终地址非 HTTPS"), "{err}");
    }

    /// 回调侧拒绝：最终主机不符或缺失（失效关闭）。
    #[test]
    fn fetch_rejects_host_mismatch_or_missing() {
        let err = call_handler("https://evil.example/x", 200, "example.com", b"{}", 64)
            .expect_err("跨主机必须拒绝");
        assert!(err.contains("最终主机不符"), "{err}");
        let err = call_handler("https:///x", 200, "example.com", b"{}", 64)
            .expect_err("最终主机缺失必须拒绝");
        assert!(err.contains("最终主机不符"), "{err}");
    }

    /// 回调侧拒绝：非 200 状态。
    #[test]
    fn fetch_rejects_non_200_status() {
        let err = call_handler("https://example.com/x", 404, "example.com", b"{}", 64)
            .expect_err("非 200 必须拒绝");
        assert!(err.contains("HTTP 状态 404"), "{err}");
    }

    /// 回调侧拒绝：响应体超过 `max_bytes`；恰等于上限仍接受。
    #[test]
    fn fetch_enforces_max_bytes() {
        let err = call_handler("https://example.com/x", 200, "example.com", b"12345", 4)
            .expect_err("超过上限必须拒绝");
        assert!(err.contains("响应体过大"), "{err}");
        let ok = call_handler("https://example.com/x", 200, "example.com", b"1234", 4)
            .expect("恰等于上限应接受");
        assert_eq!(ok, b"1234");
    }

    /// 回调侧成功路径：HTTPS + 主机相符 + 200 + 限内 → 原样返回字节。
    #[test]
    fn fetch_accepts_valid_response() {
        let ok = call_handler("https://example.com/x", 200, "example.com", b"[]", 64)
            .expect("合法响应应通过");
        assert_eq!(ok, b"[]");
    }

    /// 请求前校验：非 HTTPS 在触网前即拒绝。
    #[test]
    fn fetch_rejects_non_https_before_request() {
        let err = fetch_url_bytes("http://example.com/", Duration::from_millis(50), 64)
            .expect_err("非 HTTPS 必须拒绝");
        assert!(err.contains("仅允许 HTTPS"), "{err}");
    }

    /// 请求前校验：缺失/空主机在触网前即拒绝（失效关闭）。
    #[test]
    fn fetch_rejects_missing_or_empty_host_before_request() {
        for url in ["https:///path", "https:", "https://@/path"] {
            let err = fetch_url_bytes(url, Duration::from_millis(50), 64)
                .expect_err("缺失/空主机必须拒绝");
            assert!(err.contains("URL 缺少主机"), "{url}: {err}");
        }
    }

    /// 不透明像素的包围盒占比(宽, 高):图标内容必须基本填满 64×64 单元格。
    /// 旧实现把源矩形也传成目标矩形,AppKit 按 32pt 原尺寸绘制 → 占比约 0.47,
    /// 行内图标视觉减半;本钉捕获这类回归。
    fn opaque_bbox(rgba: &[u8], px: usize) -> (f32, f32) {
        let (mut minx, mut miny, mut maxx, mut maxy) = (px, px, 0usize, 0usize);
        for y in 0..px {
            for x in 0..px {
                if rgba[(y * px + x) * 4 + 3] > 8 {
                    minx = minx.min(x);
                    miny = miny.min(y);
                    maxx = maxx.max(x + 1);
                    maxy = maxy.max(y + 1);
                }
            }
        }
        (
            (maxx - minx) as f32 / px as f32,
            (maxy - miny) as f32 / px as f32,
        )
    }

    /// 图标解码钉:直接画进 64×64 位图(不走 TIFFRepresentation)。
    /// 旧路径在本机每个图标产生 37–74MB 的临时分配,实测 1000 条真实结果、
    /// 不滚动、仅加载可见行图标时峰值 845MB;直接光栅化后同场景 216MB。
    /// 本测钉住输出形态:64×64、有可见像素、预乘 RGBA(各通道 ≤ alpha)。
    #[test]
    fn type_icon_rasterizes_64x64_premultiplied() {
        let rgba = super::type_icon_rgba("public.folder").expect("系统文件夹图标");
        assert_eq!(rgba.len(), (super::ICON_PX * super::ICON_PX * 4) as usize);
        let mut visible = 0;
        for p in rgba.as_chunks::<4>().0 {
            assert!(
                p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3],
                "应为预乘 RGBA(通道不得大于 alpha):{p:?}"
            );
            if p[3] > 0 {
                visible += 1;
            }
        }
        assert!(visible > 64 * 64 / 10, "应有可见像素,实际 {visible}");
        let (w, h) = opaque_bbox(&rgba, 64);
        assert!(
            w >= 0.75 && h >= 0.6,
            "图标内容应基本填满单元格(回归:源/目标矩形混用导致缩到一半),实际 {w:.2}x{h:.2}"
        );
    }

    /// APP 图标(16bit float 多页表示)也必须能直接画出可见像素;
    /// 旧路径正是卡在这种表示上走 TIFF/PNG 中转、产生数十 MB 分配。
    #[test]
    fn app_icon_rasterizes_visible_pixels() {
        let path = "/System/Applications/Utilities/Terminal.app";
        let rgba = super::app_icon_rgba(path).expect("系统终端图标");
        assert_eq!(rgba.len(), (super::ICON_PX * super::ICON_PX * 4) as usize);
        let visible = rgba.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count();
        assert!(
            visible > 64 * 64 / 10,
            "APP 图标应有可见像素,实际 {visible}"
        );
        let (w, h) = opaque_bbox(&rgba, 64);
        assert!(
            w >= 0.65 && h >= 0.65,
            "APP 图标内容应基本填满单元格,实际 {w:.2}x{h:.2}"
        );
    }

    /// 进程生命周期峰值 footprint(MB;与 `scripts/mem-watch.sh` 同口径)。
    fn lifetime_peak_mb() -> u64 {
        let out = std::process::Command::new("/usr/bin/footprint")
            .args(["-f", "bytes", &std::process::id().to_string()])
            .output()
            .expect("运行 /usr/bin/footprint 失败");
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if let Some(rest) = line.trim().strip_prefix("phys_footprint_peak:") {
                return rest
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
                    / (1024 * 1024);
            }
        }
        0
    }

    /// footprint 采样(手动):图标解码不得再走 TIFFRepresentation。
    /// 旧路径每个图标 37–74MB 临时分配;真实结果窗口 1000 条、仅可见行图标
    /// 时峰值 845MB,直接光栅化后 216MB。20 个图标实测:新路径 +124MB,
    /// 旧路径 +1557MB;阈值取 256MB。须单线程跑,避免并行测试抬高峰值:
    /// `cargo test -p macEverything-app -- --ignored --nocapture --test-threads=1 icon_decode_footprint`
    #[test]
    #[ignore = "footprint 采样:手动单跑(--test-threads=1),旧 TIFF 路径会显著超标"]
    #[expect(clippy::print_stderr)]
    fn ignored_icon_decode_footprint() {
        let before = lifetime_peak_mb();
        for path in [
            "/System/Applications/Utilities/Terminal.app",
            "/System/Applications/Calculator.app",
            "/System/Applications/Safari.app",
        ] {
            if std::path::Path::new(path).exists() {
                let _ = super::app_icon_rgba(path);
            }
        }
        for uti in [
            "public.folder",
            "public.plain-text",
            "public.png",
            "public.pdf",
            "public.mpeg-4",
            "public.audio",
            "public.image",
            "public.archive",
            "public.executable",
            "public.directory",
            "com.apple.application-bundle",
            "public.html",
            "public.xml",
            "public.rtf",
            "public.spreadsheet",
            "public.presentation",
            "public.video",
            "public.font",
            "public.source-code",
            "public.data",
        ] {
            let _ = super::type_icon_rgba(uti);
        }
        let after = lifetime_peak_mb();
        eprintln!("HARNESS icon_decode footprint {before} -> {after} MB");
        assert!(
            after.saturating_sub(before) <= 256,
            "图标解码峰值增量过大({before} -> {after} MB):疑似回到 TIFFRepresentation 路径"
        );
    }

    /// 实机网络校验：拉取官网首页（不依赖 version.json 是否已部署）。
    #[test]
    #[ignore = "requires network"]
    fn live_fetch_returns_bytes() {
        let bytes = fetch_url_bytes(
            "https://maceverything.modouai.dev/",
            Duration::from_secs(10),
            1024 * 1024,
        )
        .expect("live fetch");
        assert!(!bytes.is_empty());
    }

    /// 缩窗延迟:默认 30s;合法覆盖生效;非法值回落默认(不 panic、不误缩)。
    /// 突变钉:把 `unwrap_or(SHRINK_AFTER_SECS)` 改成 `unwrap_or(0)` 首条即红。
    #[test]
    fn shrink_delay_defaults_and_overrides() {
        assert_eq!(super::shrink_delay_secs(None), 30, "默认 30s");
        assert_eq!(super::shrink_delay_secs(Some("2")), 2, "合法覆盖");
        assert_eq!(super::shrink_delay_secs(Some("abc")), 30, "非法值回落默认");
        assert_eq!(super::shrink_delay_secs(Some("")), 30, "空值回落默认");
    }

    /// 旧代定时器必须被拒绝:显示动作递增代号后,未触发的缩窗不得执行。
    /// 突变钉:把 `scheduled == current` 改成 `true` 本测试即红。
    #[test]
    fn stale_shrink_timer_is_rejected() {
        assert!(super::shrink_timer_is_current(7, 7), "同代有效");
        assert!(!super::shrink_timer_is_current(7, 8), "显示后旧代作废");
        assert!(!super::shrink_timer_is_current(8, 7), "不可能:反序也拒绝");
    }

    /// 模拟一次缩窗轮次(只做纯状态部分):返回每个窗口本轮是否应缩。
    fn shrink_round(saved: &mut ShrunkSizes, live: [NSSize; 3]) -> [bool; 3] {
        let kinds = [MainWindow::Root, MainWindow::Palette, MainWindow::Settings];
        let mut out = [false; 3];
        for (i, kind) in kinds.into_iter().enumerate() {
            out[i] = record_shrink_slot(saved, kind, live[i]);
        }
        out
    }

    /// C1 回归钉:三窗缩窗 → 只打开悬浮条(其快照被 take)→ 再次全隐缩窗。
    /// 第二次观测到的 root/settings 是 1×1,但**不得覆盖**首次快照;之后打开
    /// 完整窗口必须恢复原始尺寸(而不是 1×1)。
    ///
    /// 突变:把 [`record_shrink_slot`] 改回「无条件记录/整体覆盖」→ 第二次
    /// 轮次返回全 true 且 `saved.root` 变成 1×1,本测红。
    #[test]
    fn second_shrink_keeps_original_root_size_after_partial_restore() {
        let root = NSSize::new(1200.0, 800.0);
        let palette = NSSize::new(700.0, 500.0);
        let settings = NSSize::new(620.0, 600.0);
        let one = NSSize::new(1.0, 1.0);

        let mut saved = ShrunkSizes::default();
        assert_eq!(
            shrink_round(&mut saved, [root, palette, settings]),
            [true, true, true],
            "首次全隐:三窗都应缩并记录原始尺寸"
        );

        // 用户只打开悬浮条:显示路径 take 掉 palette 快照并恢复原尺寸;
        // root/settings 保持 1×1,快照原样保留。
        assert_eq!(saved.take(MainWindow::Palette), Some(palette));
        assert_eq!(saved.root, Some(root));
        assert_eq!(saved.settings, Some(settings));

        // 悬浮条再隐藏,30s 后第二次缩窗:root/settings 实际读到 1×1。
        let shrunk = shrink_round(&mut saved, [one, palette, one]);
        assert_eq!(
            shrunk,
            [false, true, false],
            "已缩窗口不得重复缩窗/重新捕获"
        );
        assert_eq!(saved.root, Some(root), "root 原始尺寸不得被 1×1 覆盖");
        assert_eq!(saved.settings, Some(settings), "settings 同上");
        assert_eq!(
            saved.palette,
            Some(palette),
            "palette 应重新捕获恢复后的尺寸"
        );

        // 打开完整窗口:必须恢复原始尺寸(C1 用户可见症状的最终断言)。
        assert_eq!(saved.take(MainWindow::Root), Some(root));
        assert_eq!(saved.root, None, "take 后快照清空(当前不再处于缩窗态)");
    }

    /// 连续两次全隐(期间无任何窗口打开):第二次观测到 1×1,但快照一条都
    /// 不得改写。突变「整体覆盖」→ 第二次任一 true 或快照变 1×1,本测红。
    #[test]
    fn repeated_full_shrink_never_overwrites_saved_entries() {
        let first = [
            NSSize::new(1200.0, 800.0),
            NSSize::new(700.0, 500.0),
            NSSize::new(620.0, 600.0),
        ];
        let one = NSSize::new(1.0, 1.0);
        let mut saved = ShrunkSizes::default();
        assert!(shrink_round(&mut saved, first).iter().all(|v| *v));
        assert!(
            !shrink_round(&mut saved, [one; 3]).iter().any(|v| *v),
            "已缩窗口不应再次缩"
        );
        assert_eq!(
            [saved.root, saved.palette, saved.settings],
            [Some(first[0]), Some(first[1]), Some(first[2])]
        );
    }
}

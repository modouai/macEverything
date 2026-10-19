//! PoC-8：Quick Look（QLPreviewPanel）在 LSUIElement + eframe 窗口下的可行性。
//!
//! 验证目标（docs/specs/context-menu-v2.3/design.md §4.3）：
//! 1. LSUIElement（Accessory）+ eframe 窗口下能正常显示 `QLPreviewPanel`；
//! 2. 空格打开；面板为 key 后空格/Esc 能关闭；关闭后主窗口恢复键盘焦点；
//! 3. 多选画廊显示全部项，左右方向键切换（日志记录 currentPreviewItemIndex）；
//! 4. 搜索框聚焦时空格只输入空格，不触发预览；
//! 5. 文件缺失/无预览类型不崩溃。
//!
//! 运行：
//!   cd scripts/poc/poc8_quicklook
//!   cargo run --release --config 'source.crates-io.replace-with="rsproxy"' \
//!     --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'
//!
//! 自动模式（需终端有辅助功能权限；合成点击/按键投递到前台）：
//!   POC8_AUTO=1 cargo run --release ...
//!
//! 日志：stdout + `/tmp/poc8-quicklook.log`；截图：`/tmp/poc8-*.png`。
//! 环境变量：`POC8_REGULAR=1` 用 Regular（有 Dock 图标）对照，默认 Accessory（产品形态）。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use eframe::egui;
use objc2::ffi::NSInteger;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSRunningApplication, NSScreen, NSView, NSWindow,
};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGKeyCode,
    CGMouseButton,
};
use objc2_foundation::{NSObject, NSString, NSURL};
use objc2_quick_look_ui::{QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 自动注入：app 已激活（逐帧重试 activate 的停用标志）。
static APP_ACTIVE: AtomicBool = AtomicBool::new(false);
/// 自动注入：结束测试后请求 app 关闭。
static EXIT_REQUESTED: AtomicBool = AtomicBool::new(false);
/// 自动注入阶段（0 = 未开始；10 = 请求搜索框聚焦；11 = 已请求）。
static AUTO_PHASE: AtomicU8 = AtomicU8::new(0);

// 预览项 URL（主线程；QL data source 回调在主线程读取）。
thread_local! {
    static QL_URLS: RefCell<Vec<Retained<NSURL>>> = const { RefCell::new(Vec::new()) };
}

// QLPreviewPanelDataSource 实现（无实例变量；数据走 `QL_URLS`）。
define_class!(
    // SAFETY: 超类 NSObject 无子类化约束；无 Drop；无新增 ivars。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "Poc8QLDataSource"]
    struct QlDataSource;

    // SAFETY: NSObjectProtocol 无安全要求。
    unsafe impl NSObjectProtocol for QlDataSource {}

    // SAFETY: 方法签名与 QLPreviewPanelDataSource 协议一致；数据源在面板存续期内
    // 由 PocApp 持有（面板对 dataSource 为弱引用）。
    unsafe impl QLPreviewPanelDataSource for QlDataSource {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        unsafe fn number_of_preview_items(
            &self,
            _panel: Option<&QLPreviewPanel>,
        ) -> NSInteger {
            QL_URLS.with(|u| u.borrow().len() as NSInteger)
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        unsafe fn preview_item_at_index(
            &self,
            _panel: Option<&QLPreviewPanel>,
            index: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            QL_URLS.with(|u| {
                u.borrow()
                    .get(index as usize)
                    .cloned()
                    .map(ProtocolObject::from_retained)
            })
        }
    }
);

impl QlDataSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: 主线程刚分配、ivars 已置；本类未覆写 init，走 NSObject。
        unsafe { msg_send![super(this), init] }
    }
}

/// 一条可预览项。
struct PreviewItem {
    name: String,
    path: PathBuf,
    selected: bool,
}

/// 一次 UI 动作（在 CentralPanel 闭包外执行，避免借用冲突）。
#[derive(Clone, Copy, Debug)]
enum Action {
    OpenPreview,
    ClosePreview,
    TogglePreview,
    PreviewMissing,
}

struct PocApp {
    ns_view: Option<Retained<NSView>>,
    ns_window: Option<Retained<NSWindow>>,
    /// 面板对 dataSource 弱引用：本字段必须活到面板关闭（仅保活，不读取）。
    #[allow(dead_code)]
    ql_ds: Option<Retained<QlDataSource>>,
    items: Vec<PreviewItem>,
    search: String,
    last_search: String,
    policy_set: bool,
    auto: bool,
    auto_started: bool,
    activate_retries: u32,
    panel_was_visible: bool,
    last_panel_index: NSInteger,
    focus_search_requested: bool,
}

/// QLPreviewPanel 单例（主线程；无则 None）。
fn shared_panel(mtm: MainThreadMarker) -> Option<Retained<QLPreviewPanel>> {
    unsafe { QLPreviewPanel::sharedPreviewPanel(mtm) }
}

impl PocApp {
    fn log(&self, msg: impl AsRef<str>) {
        log_line(msg.as_ref());
    }

    fn selected_paths(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter(|i| i.selected)
            .map(|i| i.path.clone())
            .collect()
    }

    /// 打开/更新 Quick Look 面板（不承担关闭语义）。
    fn open_quicklook(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            self.log("[poc8] open_quicklook: 无选中项");
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            self.log("[poc8] open_quicklook: 非主线程");
            return;
        };
        let urls: Vec<Retained<NSURL>> = paths
            .iter()
            .map(|p| NSURL::fileURLWithPath(&NSString::from_str(&p.to_string_lossy())))
            .collect();
        QL_URLS.with(|u| *u.borrow_mut() = urls);
        let Some(panel) = shared_panel(mtm) else {
            self.log("[poc8] open_quicklook: QLPreviewPanel 不可用");
            return;
        };
        NSApplication::sharedApplication(mtm).activate();
        unsafe {
            panel.reloadData();
            panel.setCurrentPreviewItemIndex(0);
        }
        panel.makeKeyAndOrderFront(None);
        self.log(format!(
            "[poc8] open_quicklook: {} 项，panelVisible={} panelKey={}",
            paths.len(),
            panel.isVisible(),
            panel.isKeyWindow()
        ));
    }

    fn close_quicklook(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(panel) = shared_panel(mtm) else {
            return;
        };
        panel.orderOut(None);
        self.log(format!("[poc8] close_quicklook: panelVisible={}", panel.isVisible()));
        self.restore_focus("close_quicklook");
    }

    fn restore_focus(&self, why: &str) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        app.activate();
        if let Some(w) = &self.ns_window {
            w.makeKeyAndOrderFront(None);
        }
        self.log(format!(
            "[poc8] restore_focus({why}): appActive={} mainKey={}",
            app.isActive(),
            self.ns_window
                .as_ref()
                .map(|w| w.isKeyWindow())
                .unwrap_or(false)
        ));
    }

    /// 面板是否由 Quick Look 自己关闭（空格/Esc/关闭按钮）→ 恢复主窗口焦点。
    fn poll_panel(&mut self) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(panel) = shared_panel(mtm) else {
            return;
        };
        let visible = panel.isVisible();
        if visible != self.panel_was_visible {
            let index = unsafe { panel.currentPreviewItemIndex() };
            self.log(format!(
                "[poc8] panel visible {} -> {} (index={index}, panelKey={}, appActive={}, mainKey={})",
                self.panel_was_visible,
                visible,
                panel.isKeyWindow(),
                NSApplication::sharedApplication(mtm).isActive(),
                self.ns_window
                    .as_ref()
                    .map(|w| w.isKeyWindow())
                    .unwrap_or(false)
            ));
            if self.panel_was_visible && !visible {
                self.restore_focus("panel closed by QL");
            }
            self.panel_was_visible = visible;
            self.last_panel_index = index;
        } else if visible {
            let index = unsafe { panel.currentPreviewItemIndex() };
            if index != self.last_panel_index {
                self.log(format!("[poc8] panel index {index}"));
                self.last_panel_index = index;
            }
        }
    }
}

impl eframe::App for PocApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);

        // 产品形态是 LSUIElement：建窗后设 Accessory（POC8_REGULAR=1 做对照）。
        if !self.policy_set {
            let regular = std::env::var_os("POC8_REGULAR").is_some();
            let (policy, name) = if regular {
                (NSApplicationActivationPolicy::Regular, "Regular")
            } else {
                (NSApplicationActivationPolicy::Accessory, "Accessory")
            };
            let ok = app.setActivationPolicy(policy);
            self.log(format!(
                "[poc8] setActivationPolicy({name}) -> {ok} (regular={regular})"
            ));
            self.policy_set = true;
        }

        // 激活重试（首帧 activate 不可靠；自动模式最终由合成点击激活）。
        if !APP_ACTIVE.load(Ordering::Relaxed) {
            if app.isActive() {
                APP_ACTIVE.store(true, Ordering::Relaxed);
                self.log(format!(
                    "[poc8] app 已激活（activate 重试 {} 次）",
                    self.activate_retries
                ));
            } else {
                if let Some(w) = &self.ns_window {
                    w.orderFrontRegardless();
                }
                app.activate();
                self.activate_retries += 1;
            }
        }

        if self.auto && !self.auto_started {
            self.auto_started = true;
            match &self.ns_window {
                Some(w) => spawn_auto_injector(w),
                None => self.log("[poc8] 自动模式：未取到 NSWindow，放弃注入"),
            }
        }

        self.poll_panel();

        if EXIT_REQUESTED.load(Ordering::Relaxed) {
            if let Some(panel) = shared_panel(mtm) {
                panel.orderOut(None);
            }
            self.log("[poc8] EXIT_REQUESTED：关闭 app");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mut action: Option<Action> = None;
        let mut search_focused = false;

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("PoC-8 · Quick Look in LSUIElement + eframe");
            ui.label(
                "勾选条目 → 结果区（非搜索框）按空格打开/关闭预览；\
                 面板内空格/Esc 关闭、←/→ 切换画廊。搜索框聚焦时空格应只输入空格。",
            );
            ui.add_space(6.0);

            ui.label("搜索框（聚焦后空格只输入，不触发预览）：");
            let resp = ui.text_edit_singleline(&mut self.search);
            search_focused = resp.has_focus();
            if self.focus_search_requested {
                self.focus_search_requested = false;
                resp.request_focus();
            }
            if AUTO_PHASE.load(Ordering::Relaxed) == 10 {
                resp.request_focus();
                AUTO_PHASE.store(11, Ordering::Relaxed);
                self.log("[poc8] auto：已请求搜索框聚焦");
            }
            if self.search != self.last_search {
                self.log(format!("[poc8] search text = {:?}", self.search));
                self.last_search = self.search.clone();
            }

            ui.separator();
            ui.label("预览项（默认全选；多选进 Quick Look 画廊）：");
            for item in &mut self.items {
                ui.checkbox(
                    &mut item.selected,
                    format!("{}  [{}]", item.name, item.path.display()),
                );
            }

            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("打开预览").clicked() {
                    action = Some(Action::OpenPreview);
                }
                if ui.button("关闭预览").clicked() {
                    action = Some(Action::ClosePreview);
                }
                if ui.button("预览不存在的文件").clicked() {
                    action = Some(Action::PreviewMissing);
                }
                if ui.button("聚焦搜索框").clicked() {
                    self.focus_search_requested = true;
                }
            });

            ui.add_space(8.0);
            if let Some(mtm) = MainThreadMarker::new() {
                let app = NSApplication::sharedApplication(mtm);
                let (panel_visible, panel_key) = match shared_panel(mtm) {
                    Some(panel) => (panel.isVisible(), panel.isKeyWindow()),
                    None => (false, false),
                };
                ui.label(format!(
                    "panel visible={panel_visible} key={panel_key} | app active={} | main key={} | retries={}",
                    app.isActive(),
                    self.ns_window
                        .as_ref()
                        .map(|w| w.isKeyWindow())
                        .unwrap_or(false),
                    self.activate_retries
                ));
                if let Some(view) = &self.ns_view {
                    ui.label(format!("ns_view={:p}", Retained::as_ptr(view)));
                }
            }
            ui.label("退出：关闭窗口（自动模式会自动退出）。");
        });

        // 空格：仅表格区（非搜索框）且选中非空时 toggle；忽略自动重复。
        let space_pressed = ui.ctx().input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Space,
                        pressed: true,
                        repeat: false,
                        ..
                    }
                )
            })
        });
        if space_pressed {
            if search_focused {
                self.log("[poc8] Space while search focused -> text input（不触发预览）");
            } else if self.selected_paths().is_empty() {
                self.log("[poc8] Space 但无选中项");
            } else {
                self.log("[poc8] Space -> toggle Quick Look");
                action = Some(Action::TogglePreview);
            }
        }

        match action {
            Some(Action::OpenPreview) => {
                let paths = self.selected_paths();
                self.open_quicklook(&paths);
            }
            Some(Action::ClosePreview) => self.close_quicklook(),
            Some(Action::TogglePreview) => {
                let visible = MainThreadMarker::new()
                    .and_then(shared_panel)
                    .map(|p| p.isVisible())
                    .unwrap_or(false);
                if visible {
                    self.close_quicklook();
                } else {
                    let paths = self.selected_paths();
                    self.open_quicklook(&paths);
                }
            }
            Some(Action::PreviewMissing) => {
                let missing = PathBuf::from("/tmp/poc8-missing-file-does-not-exist.txt");
                self.log("[poc8] preview missing file（验证不崩溃）");
                self.open_quicklook(&[missing]);
            }
            None => {}
        }
    }
}

fn log_line(msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/poc8-quicklook.log")
    {
        let _ = writeln!(f, "{msg}");
        let _ = f.flush();
    }
}

/// 从 eframe 的 CreationContext 取 NSView / NSWindow（同 PoC-3）。
fn ns_view_and_window(
    cc: &eframe::CreationContext<'_>,
) -> Result<(Retained<NSView>, Retained<NSWindow>), String> {
    let handle = cc.window_handle().map_err(|e| format!("window_handle(): {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => {
            // SAFETY: ns_view 指向 winit 为本 app 创建、当前存活的 NSView（主线程持有），
            // 生命周期覆盖 eframe 主循环；Retained::retain 增引用计数。
            let view = unsafe { Retained::retain(h.ns_view.as_ptr().cast::<NSView>()) }
                .ok_or_else(|| "Retained::retain(ns_view) 返回 None".to_string())?;
            let window = view
                .window()
                .ok_or_else(|| "NSView::window() 返回 None".to_string())?;
            Ok((view, window))
        }
        other => Err(format!("非 AppKit 窗口句柄：{other:?}")),
    }
}

/// 自动注入：合成点击激活 → 空格/Esc/方向键脚本 → 搜索框空格 → 退出。
fn spawn_auto_injector(window: &Retained<NSWindow>) {
    let Some(mtm) = MainThreadMarker::new() else {
        log_line("[poc8] 自动模式：不在主线程，放弃注入");
        return;
    };
    // 激活点：窗口标题栏中点（AppKit 坐标原点左下 → CGEvent 原点左上）。
    let frame = window.frame();
    let main_h = NSScreen::mainScreen(mtm)
        .map(|s| s.frame().size.height)
        .unwrap_or(1080.0);
    let ns_x = frame.origin.x + frame.size.width / 2.0;
    let ns_y = frame.origin.y + frame.size.height - 12.0;
    let click = CGPoint {
        x: ns_x,
        y: main_h - ns_y,
    };
    log_line(&format!(
        "[poc8] 自动模式注入点=({:.0},{:.0}) 窗口 frame=({:.0},{:.0},{:.0}x{:.0}) 主屏高={:.0}",
        click.x, click.y, frame.origin.x, frame.origin.y, frame.size.width, frame.size.height, main_h
    ));

    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1600));

        // 合成点击标题栏激活（后台启动进程拿前台最可靠的方式）。
        let mut activated = false;
        for attempt in 1..=10 {
            post_left_click(click);
            std::thread::sleep(Duration::from_millis(400));
            if NSRunningApplication::currentApplication().isActive() {
                log_line(&format!(
                    "[poc8] 自动模式：合成点击激活成功（第 {attempt} 次）"
                ));
                activated = true;
                break;
            }
        }
        if !activated {
            log_line("[poc8] 自动模式：10 次点击仍未激活，放弃");
            EXIT_REQUESTED.store(true, Ordering::Relaxed);
            return;
        }
        std::thread::sleep(Duration::from_millis(500));

        let shot = |name: &str| {
            let path = format!("/tmp/{name}");
            let _ = std::process::Command::new("/usr/sbin/screencapture")
                .args(["-x", &path])
                .status();
            log_line(&format!("[poc8] 截图 → {path}"));
        };

        // 1. 空格打开
        log_line("[poc8] 注入 Space（打开）");
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(1500));
        shot("poc8-ql-open.png");

        // 2. 空格关闭
        log_line("[poc8] 注入 Space（关闭）");
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(1200));

        // 3. 空格打开，Esc 关闭
        log_line("[poc8] 注入 Space（打开，测 Esc）");
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(1200));
        log_line("[poc8] 注入 Esc（关闭）");
        post_key(KEY_ESCAPE);
        std::thread::sleep(Duration::from_millis(1200));

        // 4. 多选画廊：打开 → →×2 → 截图 → ← → 空格关闭
        log_line("[poc8] 注入 Space（打开，测画廊）");
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(1200));
        log_line("[poc8] 注入 → ×2");
        post_key(KEY_RIGHT);
        std::thread::sleep(Duration::from_millis(600));
        post_key(KEY_RIGHT);
        std::thread::sleep(Duration::from_millis(600));
        shot("poc8-ql-gallery.png");
        log_line("[poc8] 注入 ←");
        post_key(KEY_LEFT);
        std::thread::sleep(Duration::from_millis(500));
        log_line("[poc8] 注入 Space（关闭）");
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(1200));

        // 5. 搜索框聚焦 + 空格（应只输入空格）
        log_line("[poc8] 请求搜索框聚焦（AUTO_PHASE=10）");
        AUTO_PHASE.store(10, Ordering::Relaxed);
        for _ in 0..50 {
            if AUTO_PHASE.load(Ordering::Relaxed) == 11 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        std::thread::sleep(Duration::from_millis(300));
        log_line("[poc8] 注入 Space ×2（搜索框内）");
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(300));
        post_key(KEY_SPACE);
        std::thread::sleep(Duration::from_millis(800));
        shot("poc8-search-space.png");

        std::thread::sleep(Duration::from_millis(500));
        log_line("[poc8] 自动注入结束，请求退出");
        EXIT_REQUESTED.store(true, Ordering::Relaxed);
    });
}

// 虚拟键码（HIToolbox Events.h ANSI 标准位）。
const KEY_SPACE: CGKeyCode = 49;
const KEY_ESCAPE: CGKeyCode = 53;
const KEY_LEFT: CGKeyCode = 123;
const KEY_RIGHT: CGKeyCode = 124;

fn event_source() -> Option<objc2_core_foundation::CFRetained<CGEventSource>> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
}

fn post_mouse(point: CGPoint, kind: CGEventType, button: CGMouseButton) {
    let src = event_source();
    if let Some(ev) = CGEvent::new_mouse_event(src.as_deref(), kind, point, button) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
    }
}

fn post_left_click(point: CGPoint) {
    post_mouse(point, CGEventType::LeftMouseDown, CGMouseButton::Left);
    std::thread::sleep(Duration::from_millis(60));
    post_mouse(point, CGEventType::LeftMouseUp, CGMouseButton::Left);
}

fn post_key(code: CGKeyCode) {
    let src = event_source();
    if let Some(down) = CGEvent::new_keyboard_event(src.as_deref(), code, true) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&down));
    }
    std::thread::sleep(Duration::from_millis(40));
    if let Some(up) = CGEvent::new_keyboard_event(src.as_deref(), code, false) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&up));
    }
}

fn main() -> eframe::Result<()> {
    let _ = std::fs::write(
        "/tmp/poc8-quicklook.log",
        "# poc8 quicklook log\n",
    );
    let auto = std::env::var_os("POC8_AUTO").is_some();
    log_line(&format!("[poc8] start auto={auto}"));

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("repo root")
        .to_path_buf();
    let candidates = [
        ("README.md", root.join("README.md")),
        ("Cargo.toml", root.join("Cargo.toml")),
        (
            "mascot.png",
            root.join("docs/specs/floatbar-visual/蓝色文件夹与放大镜吉祥物.png"),
        ),
    ];
    let items: Vec<PreviewItem> = candidates
        .iter()
        .map(|(name, path)| PreviewItem {
            name: (*name).to_string(),
            path: path.clone(),
            selected: true,
        })
        .collect();
    for i in &items {
        log_line(&format!(
            "[poc8] item {} exists={} path={}",
            i.name,
            i.path.exists(),
            i.path.display()
        ));
    }

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 520.0])
            .with_title("PoC-8 Quick Look in LSUIElement + eframe"),
        ..Default::default()
    };

    eframe::run_native(
        "poc8-quicklook",
        opts,
        Box::new(move |cc| {
            let (ns_view, ns_window) = match ns_view_and_window(cc) {
                Ok((v, w)) => {
                    log_line(&format!(
                        "[poc8] 取窗成功：view={:p} window=#{}",
                        Retained::as_ptr(&v),
                        w.windowNumber()
                    ));
                    (Some(v), Some(w))
                }
                Err(e) => {
                    log_line(&format!("[poc8] 取窗失败：{e}"));
                    (None, None)
                }
            };

            let mtm = MainThreadMarker::new().expect("run_native creator 在主线程");
            let ds = QlDataSource::new(mtm);
            match shared_panel(mtm) {
                Some(panel) => {
                    // SAFETY: dataSource 为弱引用；ds 由 PocApp 持有，直到进程退出。
                    unsafe {
                        panel.setDataSource(Some(ProtocolObject::from_ref(&*ds)));
                    }
                    log_line("[poc8] QL dataSource 已安装");
                }
                None => log_line("[poc8] QLPreviewPanel 不可用，dataSource 未安装"),
            }

            Ok(Box::new(PocApp {
                ns_view,
                ns_window,
                ql_ds: Some(ds),
                items,
                search: String::new(),
                last_search: String::new(),
                policy_set: false,
                auto,
                auto_started: false,
                activate_retries: 0,
                panel_was_visible: false,
                last_panel_index: -1,
                focus_search_requested: false,
            }))
        }),
    )
}

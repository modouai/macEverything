//! PoC-7：系统毛玻璃（NSVisualEffectView）与 eframe/GL 面板的叠加可行性验证。
//!
//! 背景：`floatbar-visual` §6.1 —— 面板底走 ② 系统毛玻璃（2026-10-17 拍板），但 spec
//! 明确标注「GL 图层与 vibrancy 的叠加未验证，别假设可行」；本探针做这一验证。
//!
//! 层级事实（winit 0.30.13 源码，`platform_impl/macos/window_delegate.rs`）：
//! winit 把 `WinitView` **直接设为窗口 contentView**（`setContentView(Some(&view))`），
//! 且 `WindowDelegate::view()` 恒按 `WinitView` 解释 contentView（含实例变量读取）——
//! 换容器/换父子层级会把 winit 的 ivars 读成垃圾（UB），故 vibrancy **不能**作为
//! contentView 的兄弟或父级。本探针走可行路线：**面板窗下方挂一个同级毛玻璃窗**
//! （`addChildWindow:ordered:Below`，随机每帧同步 frame）。
//!
//! 键位：`V` 开关毛玻璃窗 · `M` 循环 material · `1/2/3` 面板底 alpha（0 / 60% / 80%）·
//!       `S` 打印层级与 frame · `Q` 退出
//! 看什么：面板下是否透出桌面**模糊**底（而不是原样桌面）；12pt 灰色「所在目录」小字
//! 在模糊底上是否可读；开关两态对比（V）。

use eframe::egui;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObjectProtocol};
use objc2::{define_class, ClassType, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

// 无边框窗口默认 `canBecomeKeyWindow = false`——点走一次焦点就永远收不到键盘
// （app 侧靠 NSPanel 改类覆写这两项，spike 实测；本探针同法覆写）。
define_class!(
    // SAFETY: 超类 NSWindow 无子类化约束；无新增实例变量（ivars 缺省 `()`），
    // 实例大小与超类一致——object_setClass 的硬要求（运行期断言见 reclass_for_keys）。
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "Poc7KeyWindow"]
    struct Poc7KeyWindow;

    impl Poc7KeyWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool {
            true
        }
    }

    unsafe impl NSObjectProtocol for Poc7KeyWindow {}
);

fn obj_class(obj: *const AnyObject) -> &'static AnyClass {
    // SAFETY: obj 指向存活对象；类对象由运行时持有，永续。
    unsafe { &*objc2::ffi::object_getClass(obj) }
}

/// 运行期换类 → 键盘可达；并把窗口置为 key。
fn reclass_for_keys(win: &NSWindow) {
    let obj: *const AnyObject = (win as *const NSWindow).cast::<AnyObject>();
    let before = obj_class(obj);
    let target = Poc7KeyWindow::class();
    if before.instance_size() != target.instance_size() {
        eprintln!("[poc7] 换类被拒：实例大小不一致（{before:?} vs {target:?}）");
        return;
    }
    // SAFETY: 实例大小已断言一致；目标类为本进程注册的 NSWindow 子类（无新增 ivars）。
    unsafe {
        objc2::ffi::object_setClass(obj.cast_mut(), target as *const AnyClass);
    }
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).activate();
    }
    win.makeKeyWindow();
    win.orderFrontRegardless();
    eprintln!("[poc7] 键盘可达补丁：class → Poc7KeyWindow；activate + makeKeyWindow 完成");
}

const W: f64 = 700.0;
const H: f64 = 176.0;

/// 面板底 alpha 三档（0% = 纯毛玻璃；80% = 设计稿值 `#191C21CC`）。
const PANEL_ALPHAS: [(u8, &str); 3] = [
    (0x00, "0%（纯毛玻璃）"),
    (0x99, "60%"),
    (0xCC, "80%（设计稿）"),
];

/// material 循环候选（深浅主题各挑代表）。
const MATERIALS: [(NSVisualEffectMaterial, &str, u8); 4] = [
    (NSVisualEffectMaterial::HUDWindow, "HUDWindow(深)", 0xCC),
    (NSVisualEffectMaterial::Popover, "Popover", 0xCC),
    (NSVisualEffectMaterial::Sidebar, "Sidebar", 0xCC),
    (
        NSVisualEffectMaterial::UnderWindowBackground,
        "UnderWindowBackground",
        0xCC,
    ),
];

struct PocApp {
    panel: Option<Retained<NSWindow>>,
    blur: Option<Retained<NSWindow>>,
    vib: Option<Retained<NSVisualEffectView>>,
    vib_on: bool,
    mat_idx: usize,
    alpha_idx: usize,
    logged: bool,
}

impl PocApp {
    fn new() -> Self {
        Self {
            panel: None,
            blur: None,
            vib: None,
            vib_on: false,
            mat_idx: 0,
            alpha_idx: 2,
            logged: false,
        }
    }

    /// 建立毛玻璃窗（面板窗的子窗，ordered: Below）并挂 NSVisualEffectView。
    fn ensure_blur(&mut self) {
        if self.blur.is_some() {
            return;
        }
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let (mat, mname, _) = MATERIALS[self.mat_idx];
        // SAFETY: 主线程持有 mtm；initWithContentRect 为标准构造，返回的窗由 Retained 持有。
        let win = unsafe {
            let alloc = NSWindow::alloc(mtm);
            NSWindow::initWithContentRect_styleMask_backing_defer(
                alloc,
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)),
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        win.setOpaque(false);
        win.setBackgroundColor(Some(&NSColor::clearColor()));
        win.setHasShadow(false);
        win.setIgnoresMouseEvents(true);
        let vib = NSVisualEffectView::new(mtm);
        vib.setMaterial(mat);
        vib.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        vib.setState(NSVisualEffectState::Active);
        vib.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)));
        let vib_view: &NSView = &vib;
        win.setContentView(Some(vib_view));
        if let Some(panel) = &self.panel {
            win.setLevel(panel.level());
            win.setCollectionBehavior(panel.collectionBehavior());
            // SAFETY: 两窗均为本进程存活对象；主线程调用。
            unsafe {
                panel.addChildWindow_ordered(&win, NSWindowOrderingMode::Below);
            }
        }
        win.orderFrontRegardless();
        eprintln!(
            "[poc7] 毛玻璃窗已建：frame={:?} material={mname}（子窗 ordered:Below）",
            win.frame()
        );
        self.blur = Some(win);
        self.vib = Some(vib);
    }

    /// 每帧把毛玻璃窗 frame 同步到面板（子窗随动只覆盖「移动」，resize 要自己跟）。
    fn sync_blur_frame(&self) {
        let (Some(blur), Some(panel)) = (&self.blur, &self.panel) else {
            return;
        };
        let f = panel.frame();
        if blur.frame() != f {
            blur.setFrame_display(f, true);
        }
    }

    fn set_material(&mut self, idx: usize) {
        self.mat_idx = idx % MATERIALS.len();
        let (mat, mname, _) = MATERIALS[self.mat_idx];
        if let Some(vib) = &self.vib {
            vib.setMaterial(mat);
            eprintln!("[poc7] material → {mname}（读回 {}）", vib.material().0);
        }
    }

    fn dump_layers(&mut self) {
        let Some(panel) = &self.panel else { return };
        let cv = panel.contentView();
        eprintln!(
            "[poc7] 层级：panel.contentView={:?}；blur.frame={:?} panel.frame={:?}",
            cv.as_ref().map(|v| v.class().name().to_string_lossy().to_string()),
            self.blur.as_ref().map(|b| b.frame()),
            panel.frame()
        );
        if let Some(v) = &self.vib {
            eprintln!(
                "[poc7] vibrancy：material={} blending={} state={}",
                v.material().0,
                v.blendingMode().0,
                v.state().0
            );
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        let o = ui.max_rect().min;
        let p = ui.painter();
        let (alpha, alabel) = PANEL_ALPHAS[self.alpha_idx];
        let (_, mname, _) = MATERIALS[self.mat_idx];
        // —— 诊断布局：面板底只画左半（右半完全不画）——
        // 右半是「未遮挡区」：毛玻璃若生效，那里应能看到桌面**模糊**；若与窗外桌面一样
        // 清晰，则 vibrancy 没透出来（与 alpha/材质无关的根因）。
        let rect = egui::Rect::from_min_size(o, egui::vec2(W as f32 / 2.0, H as f32));
        p.rect_filled(
            rect,
            egui::CornerRadius::same(12),
            egui::Color32::from_rgba_unmultiplied(0x19, 0x1C, 0x21, alpha),
        );
        p.rect_stroke(
            rect,
            egui::CornerRadius::same(12),
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 0x29)),
            egui::StrokeKind::Inside,
        );
        let txt = |y: f32, s: &str, size: f32, color: egui::Color32| {
            p.text(
                egui::pos2(o.x + 20.0, o.y + y),
                egui::Align2::LEFT_CENTER,
                s,
                egui::FontId::proportional(size),
                color,
            );
        };
        let fg = egui::Color32::from_rgb(0xF2, 0xF4, 0xF7);
        let name = egui::Color32::from_rgb(0xE4, 0xE7, 0xEB);
        let dir = egui::Color32::from_rgb(0xA2, 0xAA, 0xB4);
        let meta = egui::Color32::from_rgb(0x8B, 0x92, 0x9A);
        txt(28.0, "毛玻璃探针", 21.0, fg);
        txt(62.0, "年度工作报告.docx", 15.0, name);
        txt(84.0, "~/Documents/工作/2026/汇报材料", 12.0, dir);
        txt(
            112.0,
            &format!(
                "{} · {mname} · alpha {alabel}",
                if self.vib_on { "毛玻璃开" } else { "毛玻璃关" }
            ),
            12.0,
            meta,
        );
        txt(136.0, "V 开关 · M 材质 · 1/2/3 透明度", 12.0, meta);
        // 右半（未遮挡区）提示 + 直接可读性样本：这里的字叠在毛玻璃上（若生效）
        p.text(
            egui::pos2(o.x + W as f32 * 0.55, o.y + 28.0),
            egui::Align2::LEFT_CENTER,
            "右半未遮挡 → 桌面模糊了吗？",
            egui::FontId::proportional(12.0),
            fg,
        );
        p.text(
            egui::pos2(o.x + W as f32 * 0.55, o.y + 62.0),
            egui::Align2::LEFT_CENTER,
            "年度工作报告.docx",
            egui::FontId::proportional(15.0),
            name,
        );
        p.text(
            egui::pos2(o.x + W as f32 * 0.55, o.y + 84.0),
            egui::Align2::LEFT_CENTER,
            "~/Documents/工作/2026",
            egui::FontId::proportional(12.0),
            dir,
        );
    }
}

impl eframe::App for PocApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 键位派发在 ui()(需 egui input) —— 这里只做 frame 同步。
        self.sync_blur_frame();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (v, m, one, two, three, s, q) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::V),
                i.key_pressed(egui::Key::M),
                i.key_pressed(egui::Key::Num1),
                i.key_pressed(egui::Key::Num2),
                i.key_pressed(egui::Key::Num3),
                i.key_pressed(egui::Key::S),
                i.key_pressed(egui::Key::Q),
            )
        });
        if q {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if v {
            self.vib_on = !self.vib_on;
            if self.vib_on {
                self.ensure_blur();
            } else if let Some(b) = &self.blur {
                b.orderOut(None);
            }
            eprintln!("[poc7] 毛玻璃 {}", if self.vib_on { "开" } else { "关" });
        }
        if m {
            let next = self.mat_idx + 1;
            self.set_material(next);
        }
        if one {
            self.alpha_idx = 0;
        }
        if two {
            self.alpha_idx = 1;
        }
        if three {
            self.alpha_idx = 2;
        }
        if s {
            self.dump_layers();
        }
        if !self.logged {
            self.logged = true;
            self.dump_layers();
        }
        self.draw(ui);
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// 主路径（同 poc1b/poc6）：`cc.window_handle()` → NSView → `NSView::window()`。
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

fn main() -> eframe::Result<()> {
    eprintln!("[poc7] 键位：V 开关毛玻璃 · M 材质 · 1/2/3 面板 alpha(0/60%/80%) · S 层级 · Q 退出");
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([W as f32, H as f32])
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_has_shadow(false)
        .with_position([300.0, 140.0]);
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "poc7-vibrancy",
        opts,
        Box::new(|cc| {
            let mut app = PocApp::new();
            match ns_window_from_cc(cc) {
                Ok(w) => {
                    reclass_for_keys(&w);
                    app.panel = Some(w);
                }
                Err(e) => eprintln!("[poc7] 取窗失败：{e}"),
            }
            Ok(Box::new(app))
        }),
    )
}

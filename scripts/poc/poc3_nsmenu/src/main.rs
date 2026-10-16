//! PoC-3：原生 NSMenu 在 egui 主线程弹出（右键菜单系统观感 + 嵌套 run loop 阻塞观测）。
//!
//! 判断依据（Evidence）：见 docs/specs/v2-ui/poc-results.md「Task 3」节。
//!
//! 验证目标（plan Task 3）：
//! 1. 右键 → 构造原生 NSMenu（四项：打开 / 在访达中打开 / 在访达中显示 / 拷贝路径，
//!    tag 1..4）并在鼠标位置弹出，菜单为系统观感；
//! 2. `popUpMenuPositioningItem:atLocation:inView:` 是嵌套 run loop 的**阻塞调用**——
//!    验证它阻塞 egui 主循环期间与关闭后的行为（冻结可接受，关闭后须恢复）；
//! 3. 关闭后读 `menu.highlightedItem()` 取回选项 tag；连续弹 5 次无崩溃/泄漏。
//!
//! 实锁版本（API 名逐一对照实锁 crate 源码确认，非凭记忆）：
//! eframe / egui 0.37.0、objc2 0.6.5、objc2-app-kit 0.3.2、objc2-foundation 0.3.2、
//! objc2-core-foundation 0.3.2、objc2-core-graphics 0.3.2、raw-window-handle 0.6.2。
//! - `NSMenu::popUpMenuPositioningItem_atLocation_inView(&self, item: Option<&NSMenuItem>,
//!   location: NSPoint, view: Option<&NSView>) -> bool`
//!   （objc2-app-kit-0.3.2/src/generated/NSMenu.rs:166-175）；变体
//!   `popUpContextMenu_withEvent_forView(menu: &NSMenu, event: &NSEvent, view: &NSView)`
//!   （同文件 :146-148）需要真实 NSEvent，本 PoC 不用。
//! - `NSMenu` / `NSMenuItem` 均 `#[thread_kind = MainThreadOnly]`：`alloc(mtm)` 需 MainThreadMarker
//!   （NSMenu.rs:77-82、NSMenuItem.rs:15-20）。
//! - 无 action 的 NSMenuItem 会被自动验证（autoenablesItems=true）判为禁用，故
//!   `menu.setAutoenablesItems(false)` + `item.setEnabled(true)`（NSMenu.rs:329-331、
//!   NSMenuItem.rs:320-325）。
//! - 位置计算：`NSWindow::mouseLocationOutsideOfEventStream()`（窗口底坐标系，原点左下）
//!   经 `NSView::convertPoint_fromView(point, None)`（view=None 即从窗口底坐标转换，
//!   NSView.rs:495）得到视图坐标；winit 的 view 是 flipped（winit-0.30.13
//!   view.rs:159-163 `isFlipped -> true`，原点左上、y 向下），AppKit 的转换自动处理翻转。
//!
//! 环境变量：
//! - `POC3_POS=x,y`   指定窗口全局坐标（逻辑点；自动化注入需固定位置）
//! - `POC3_AUTO=1`    自动模式：后台线程用 CGEvent 注入 5 轮「右键 → ↓×N → Return/Esc」，
//!                    每轮菜单期抓屏到 /tmp/poc3-r<N>-menu.png；收尾注入 a/b 两键后退出
//! - `POC3_DARK=1`    把 app 有效外观置为 darkAqua（对照验证菜单观感由有效外观驱动）
//!
//! 运行（拉依赖走 rsproxy 镜像，见报告复现步骤）：
//! `cargo run --release --config 'source.crates-io.replace-with="rsproxy"' \
//!   --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'`

use std::time::{Duration, Instant};

use eframe::egui;
use objc2::ffi::NSInteger;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameDarkAqua, NSApplication, NSMenu,
    NSMenuItem, NSRunningApplication, NSScreen, NSView, NSWindow,
};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGKeyCode,
    CGMouseButton,
};
use objc2_foundation::NSString;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 自动模式：已确认前台后置位，`logic()` 据此停掉重复的「举窗 + activate」动作
/// （置位由 `logic()` 完成；注入线程的起跑条件另用 `NSRunningApplication::isActive()` 自判）。
static APP_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 菜单四项（plan Step 1 指定）与 tag 1..4。
const MENU_ITEMS: [(&str, NSInteger); 4] = [
    ("打开", 1),
    ("在访达中打开", 2),
    ("在访达中显示", 3),
    ("拷贝路径", 4),
];

// macOS 虚拟键码（CGEvent 注入用；Carbon/HIToolbox Events.h 的 ANSI 标准位）。
const KEY_RETURN: CGKeyCode = 36;
const KEY_ESCAPE: CGKeyCode = 53;
const KEY_DOWN: CGKeyCode = 125;
/// 字母 a / b（验证菜单关闭后键盘输入到达 egui）。
const KEY_A: CGKeyCode = 0;
const KEY_B: CGKeyCode = 11;

/// 一次弹出的观测结果。
struct PopOutcome {
    /// `popUpMenuPositioningItem` 的 bool 返回值。实测语义不稳定（成功选择的轮次里
    /// true/false 都出现过），只作日志留痕；判定选择一律用 `highlightedItem()`。
    popped: bool,
    /// 阻塞时长（= egui 主线程被嵌套 run loop 占用、窗口冻结的时长）。
    blocked: Duration,
    /// 关闭后 `highlightedItem()` 读回的项（None = 取消/Esc/未选中）。
    choice: Option<(NSInteger, String)>,
    /// 视图坐标（弹出位置，供日志核对）。
    view_point: (f64, f64),
    /// 窗口底坐标系的鼠标点。
    window_point: (f64, f64),
}

struct PocApp {
    /// `CreationContext` 取到的 NSView / NSWindow（供 NSMenu 弹出定位）。
    ns_view: Option<Retained<NSView>>,
    ns_window: Option<Retained<NSWindow>>,
    init_log: String,

    // ---- 观测计数 ----
    pops: u32,
    last_choice: Option<String>,
    last_block_ms: f64,
    blocked_total_ms: f64,

    // ---- 自动注入 ----
    auto: bool,
    auto_started: bool,
    /// 前台激活的重试次数（首跑实测：创建期激活不可靠，窗口上屏后需再拉起）。
    activate_retries: u32,

    /// 输入测试（验证菜单关闭后键盘输入正常）。
    text: String,
    /// 菜单关闭后 egui 实际收到的按键（证明输入管线恢复）。
    received_keys: Vec<String>,
}

fn log_line(msg: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// 从 eframe 的 `CreationContext` 取 NSView 与 NSWindow（同 PoC-1 的取窗路径）。
fn ns_view_and_window(
    cc: &eframe::CreationContext<'_>,
) -> Result<(Retained<NSView>, Retained<NSWindow>), String> {
    let handle = cc
        .window_handle()
        .map_err(|e| format!("window_handle(): {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => {
            // SAFETY: ns_view 指向 winit 为本 app 创建、当前存活的 NSView（主线程持有），
            // 生命周期覆盖 eframe 主循环；此处取一份 Retained（+1 引用计数）延长借用。
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

/// 构造四项 NSMenu（tag 1..4；PoC 阶段不带 action，回调桥接见 plan Task 18）。
fn build_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    // 无 action 的项会被自动验证判为禁用；关掉自动验证并显式启用，
    // 保证项可高亮、可选择（highlightedItem 才有值）。
    menu.setAutoenablesItems(false);
    for (title, tag) in MENU_ITEMS {
        // SAFETY: selector 传 None（无 action，非「无效 selector」）；其余参数为
        // 本次调用内构造的 NSString 临时值，AppKit 会拷贝标题与键等价串。
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str(title),
                None,
                &NSString::from_str(""),
            )
        };
        item.setTag(tag);
        item.setEnabled(true);
        menu.addItem(&item);
    }
    menu
}

impl PocApp {
    /// 在主线程弹出 NSMenu（阻塞式）并读回结果。
    fn pop_menu(&self) -> Result<PopOutcome, String> {
        let mtm = MainThreadMarker::new().ok_or("不在主线程")?;
        let view = self.ns_view.as_ref().ok_or("无 NSView")?;
        let window = self.ns_window.as_ref().ok_or("无 NSWindow")?;

        let menu = build_menu(mtm);

        // 弹出位置：鼠标在窗口底坐标系的位置 → 视图坐标（AppKit 转换自动处理 flipped）。
        let window_point = window.mouseLocationOutsideOfEventStream();
        let location = view.convertPoint_fromView(window_point, None);

        let t0 = Instant::now();
        // 阻塞调用：内部是嵌套 run loop（菜单跟踪模式），返回即菜单关闭。
        let popped = menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(view));
        let blocked = t0.elapsed();

        // 关闭后读高亮项（无选择 = None）。
        let choice = menu
            .highlightedItem()
            .map(|item| (item.tag(), item.title().to_string()));

        Ok(PopOutcome {
            popped,
            blocked,
            choice,
            view_point: (location.x, location.y),
            window_point: (window_point.x, window_point.y),
        })
    }

    /// 弹出一次并记录观测。
    fn pop_and_log(&mut self) {
        match self.pop_menu() {
            Ok(outcome) => {
                self.pops += 1;
                let blocked_ms = outcome.blocked.as_secs_f64() * 1000.0;
                self.last_block_ms = blocked_ms;
                self.blocked_total_ms += blocked_ms;
                let desc = match &outcome.choice {
                    Some((tag, title)) => {
                        let text = format!("{title}（tag={tag}）");
                        self.last_choice = Some(text.clone());
                        text
                    }
                    None => {
                        self.last_choice = None;
                        "取消（未选择）".to_string()
                    }
                };
                log_line(&format!(
                    "[poc3] 第 {} 次弹出：返回值 popped={}；阻塞 {:.0}ms；位置 窗口坐标=({:.1},{:.1}) 视图坐标=({:.1},{:.1})；选择={}",
                    self.pops,
                    outcome.popped,
                    blocked_ms,
                    outcome.window_point.0,
                    outcome.window_point.1,
                    outcome.view_point.0,
                    outcome.view_point.1,
                    desc
                ));
            }
            Err(err) => {
                log_line(&format!("[poc3] 弹出失败：{err}"));
                self.init_log = format!("弹出失败：{err}");
            }
        }
    }
}

impl eframe::App for PocApp {
    /// 非渲染逻辑：自动注入模式的启动判定。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.auto {
            // 注入的鼠标/键盘事件投给前台窗口：直到确认前台为止，每帧把本窗口
            // `orderFrontRegardless`（非激活也能压到最前，保证注入点击落在本窗口上）
            // 并调用 activate()；最终激活由注入线程的合成点击完成（logic 只在辅助位）。
            if !APP_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(mtm) = MainThreadMarker::new() {
                    let app = NSApplication::sharedApplication(mtm);
                    if app.isActive() {
                        APP_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
                        log_line(&format!(
                            "[poc3] 自动模式：app 已激活（activate() 重试 {} 次）",
                            self.activate_retries
                        ));
                    } else {
                        if let Some(window) = &self.ns_window {
                            window.orderFrontRegardless();
                        }
                        app.activate();
                        self.activate_retries += 1;
                    }
                }
            }
            if !self.auto_started {
                if let Some(window) = &self.ns_window {
                    self.auto_started = true;
                    spawn_auto_injector(window);
                } else {
                    self.auto_started = true;
                    log_line("[poc3] 自动模式：未取到 NSView/NSWindow，无法定位注入点");
                }
            }
            // 驱动重绘：注入期 250ms 一帧刷状态（否则 egui 空闲不跑 logic）。
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 右键检测：egui 的 secondary_clicked（鼠标右键「点击」= 按下+抬起）。
        let secondary = ui.ctx().input(|i| i.pointer.secondary_clicked());
        if secondary {
            self.pop_and_log();
            // 菜单关闭 → 本帧继续执行（阻塞期间本函数不返回，帧冻结）。
        }

        // 记录 egui 收到的按键（菜单关闭后注入的按键应到达这里，证明输入管线恢复）。
        // 日志带「已弹出次数」，便于把杂散键（共享会话上的真实/其它会话按键）与注入键区分。
        let keys: Vec<String> = ui.ctx().input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        repeat: false,
                        ..
                    } => Some(format!("{key:?}")),
                    egui::Event::Text(t) => Some(format!("Text({t})")),
                    _ => None,
                })
                .collect()
        });
        if !keys.is_empty() {
            log_line(&format!(
                "[poc3] egui 收到按键（此前已弹出 {} 次）：{}",
                self.pops,
                keys.join(", ")
            ));
            self.received_keys.extend(keys);
        }

        // 手动模式的退出键：ESC（菜单开启时被菜单消费，不会走到这里）。
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("PoC-3：原生 NSMenu 与 egui 主循环");
            ui.add_space(4.0);
            ui.label("在窗口内任意位置右键：应弹出系统观感菜单（深/浅色跟随系统、支持 ↑↓ 键盘导航、Esc 取消）。");
            ui.add_space(6.0);

            // 可右键区域（视觉提示；右键检测走全窗口 input）。
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), 90.0),
                egui::Sense::click(),
            );
            ui.painter().rect_filled(
                rect,
                6.0,
                if resp.hovered() {
                    egui::Color32::from_rgb(50, 70, 110)
                } else {
                    egui::Color32::from_rgb(40, 44, 52)
                },
            );
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "在这里右键（本区域仅作提示，整个窗口均可右键）",
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(220),
            );

            ui.add_space(8.0);
            ui.separator();
            ui.label(format!("弹出次数：{}", self.pops));
            ui.label(format!(
                "上次选择：{}",
                self.last_choice
                    .clone()
                    .unwrap_or_else(|| "（尚无）".to_string())
            ));
            ui.label(format!(
                "上次阻塞：{:.0} ms（累计 {:.0} ms）——阻塞期间窗口冻结，关闭后恢复",
                self.last_block_ms, self.blocked_total_ms
            ));
            if !self.init_log.is_empty() {
                ui.colored_label(egui::Color32::RED, &self.init_log);
            }

            ui.label(format!(
                "egui 已收到按键：{} 个 {}",
                self.received_keys.len(),
                if self.received_keys.is_empty() {
                    String::new()
                } else {
                    format!("（最近：{}）", self.received_keys.join(", "))
                }
            ));

            ui.add_space(8.0);
            ui.label("关闭菜单后输入测试（键盘输入应立即恢复）：");
            ui.text_edit_singleline(&mut self.text);

            ui.add_space(6.0);
            ui.label("ESC 退出窗口。");
        });
    }
}

/// 自动模式：后台线程按脚本注入「右键 → 按键」共 5 轮（验证连续弹出/恢复/无崩溃），
/// 并在每轮菜单弹出期间抓屏留证。
fn spawn_auto_injector(window: &Retained<NSWindow>) {
    // 计算注入点：窗口 frame（全局显示坐标，原点主屏左下）→ CGEvent 坐标（原点主屏左上）。
    // 说明：frame 是整个窗口（含标题栏）；x 取水平中点、y 取竖直中点，落在内容区内。
    // 注：窗口以 `orderFrontRegardless` 被举到最前（logic() 内），故点击必落本窗口。
    let mtm = match MainThreadMarker::new() {
        Some(mtm) => mtm,
        None => {
            log_line("[poc3] 自动模式：不在主线程，放弃注入");
            return;
        }
    };
    let frame = window.frame();
    let main_h = NSScreen::mainScreen(mtm).map(|s| s.frame().size.height).unwrap_or(1080.0);
    // 目标是窗口内容区中心（避开标题栏）：frame 中心即安全。
    let ns_x = frame.origin.x + frame.size.width / 2.0;
    let ns_y = frame.origin.y + frame.size.height / 2.0;
    let cg_point = CGPoint {
        x: ns_x,
        y: main_h - ns_y,
    };
    log_line(&format!(
        "[poc3] 自动模式：注入点 CGEvent 坐标=({:.0},{:.0})（窗口 frame 全局坐标原点 ({:.0},{:.0}) 尺寸 {:.0}x{:.0}，主屏高 {:.0}）",
        cg_point.x, cg_point.y, frame.origin.x, frame.origin.y, frame.size.width, frame.size.height, main_h
    ));

    std::thread::spawn(move || {
        // 等窗口上屏、winit 事件循环就绪。
        std::thread::sleep(Duration::from_millis(1500));

        // 激活前置：合成左键点击本窗口（窗口已被 orderFrontRegardless 举到最前）。
        // 合成点击视为用户输入，是后台启动的进程拿到前台最可靠的方式
        // （首跑实测：仅靠主线程 activate() 有时要等约 60 秒）。
        let mut activated = false;
        for attempt in 1..=10 {
            post_left_click(cg_point);
            std::thread::sleep(Duration::from_millis(500));
            if NSRunningApplication::currentApplication().isActive() {
                log_line(&format!("[poc3] 自动模式：合成点击激活成功（第 {attempt} 次尝试）"));
                activated = true;
                break;
            }
            log_line(&format!("[poc3] 自动模式：第 {attempt} 次合成点击后仍未激活，重试"));
        }
        if !activated {
            log_line("[poc3] 自动模式：10 次合成点击仍无法激活前台，放弃注入（注入事件的投递目标不可靠）");
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(400));

        // 每轮：右键注入 → ↓×N 移动高亮 → 抓屏（留「键盘导航高亮」证据）→ 提交键 → 等关闭。
        // 轮 1/2/3/5 用 ↓×N + Return 选择第 N 项；轮 4 用 Esc 取消。
        let rounds: [(&str, usize, CGKeyCode); 5] = [
            ("↓×1 + Return（选「打开」tag=1）", 1, KEY_RETURN),
            (
                "↓×2 + Return（选「在访达中打开」tag=2）",
                2,
                KEY_RETURN,
            ),
            ("↓×4 + Return（选「拷贝路径」tag=4）", 4, KEY_RETURN),
            ("Esc（取消）", 0, KEY_ESCAPE),
            (
                "↓×3 + Return（选「在访达中显示」tag=3）",
                3,
                KEY_RETURN,
            ),
        ];

        for (i, (desc, downs, commit)) in rounds.iter().enumerate() {
            let n = i + 1;
            log_line(&format!("[poc3] 自动注入 轮 {n}：右键 → {desc}"));
            post_right_click(cg_point);
            std::thread::sleep(Duration::from_millis(700)); // 菜单应已弹出
            // 鼠标移出菜单：菜单左上角正好落在点击点上，指针停在首项上时会把它预高亮，
            // ↓ 便从第 2 项开始（实测踩到过，轮 1 的 ↓×1 选中 tag=2）。
            // 先移开指针、让菜单无高亮项，↓×N 才稳定命中第 N 项。
            post_mouse_move(CGPoint {
                x: cg_point.x - 160.0,
                y: cg_point.y + 160.0,
            });
            std::thread::sleep(Duration::from_millis(200));
            for _ in 0..*downs {
                post_key(KEY_DOWN);
                std::thread::sleep(Duration::from_millis(150));
            }
            let shot = format!("/tmp/poc3-r{n}-menu.png");
            let _ = std::process::Command::new("screencapture")
                .args(["-x", &shot])
                .status();
            log_line(&format!("[poc3] 自动注入 轮 {n}：菜单期抓屏 → {shot}"));
            post_key(*commit);
            std::thread::sleep(Duration::from_millis(900)); // 菜单关闭、egui 恢复
        }

        std::thread::sleep(Duration::from_millis(500));
        log_line("[poc3] 自动注入：5 轮完成（无崩溃则文本编辑器仍可输入）");

        // 收尾：连弹 5 次后再注入 a/b 两键并抓屏——证明菜单关闭后键盘输入到达 egui。
        for key in [KEY_A, KEY_B] {
            post_key(key);
            std::thread::sleep(Duration::from_millis(150));
        }
        std::thread::sleep(Duration::from_millis(500));
        let _ = std::process::Command::new("screencapture")
            .args(["-x", "/tmp/poc3-after-keys.png"])
            .status();
        log_line("[poc3] 收尾抓屏 → /tmp/poc3-after-keys.png");

        // 结束进程：退出码 0 作为「全 5 轮无崩溃」证据（抓屏与日志已落盘）。
        std::thread::sleep(Duration::from_millis(300));
        std::process::exit(0);
    });
}

/// 注入事件源（HID 系统态；与 PoC-2 合成按键同源）。
fn event_source() -> Option<objc2_core_foundation::CFRetained<CGEventSource>> {
    CGEventSource::new(CGEventSourceStateID::HIDSystemState)
}

fn post_mouse(point: CGPoint, kind: CGEventType, button: CGMouseButton) {
    let src = event_source();
    if let Some(ev) = CGEvent::new_mouse_event(src.as_deref(), kind, point, button) {
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&ev));
    }
}

/// 移动鼠标（菜单开启期间用于把指针移出菜单，避免预高亮）。
fn post_mouse_move(point: CGPoint) {
    post_mouse(point, CGEventType::MouseMoved, CGMouseButton::Left);
}

fn post_left_click(point: CGPoint) {
    post_mouse(point, CGEventType::LeftMouseDown, CGMouseButton::Left);
    std::thread::sleep(Duration::from_millis(60));
    post_mouse(point, CGEventType::LeftMouseUp, CGMouseButton::Left);
}

fn post_right_click(point: CGPoint) {
    post_mouse(point, CGEventType::RightMouseDown, CGMouseButton::Right);
    std::thread::sleep(Duration::from_millis(60));
    post_mouse(point, CGEventType::RightMouseUp, CGMouseButton::Right);
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

/// 解析 `POC3_POS=x,y`。
fn env_position() -> Option<[f32; 2]> {
    let raw = std::env::var("POC3_POS").ok()?;
    let (x, y) = raw.split_once(',')?;
    Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
}

fn main() -> eframe::Result<()> {
    let auto = std::env::var_os("POC3_AUTO").is_some();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([640.0, 460.0])
        .with_title("PoC-3 原生 NSMenu 与 egui");
    if let Some(pos) = env_position() {
        log_line(&format!("[poc3] POC3_POS 指定窗口位置 = {pos:?}"));
        viewport = viewport.with_position(pos);
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "poc3-nsmenu",
        opts,
        Box::new(move |cc| {
            let (ns_view, ns_window, init_log) = match ns_view_and_window(cc) {
                Ok((view, window)) => {
                    let log = format!(
                        "取窗成功：view={:p}，window=#{:?}",
                        Retained::as_ptr(&view),
                        window.windowNumber()
                    );
                    log_line(&format!("[poc3] {log}"));
                    (Some(view), Some(window), log)
                }
                Err(err) => {
                    log_line(&format!("[poc3] 取窗失败：{err}"));
                    (None, None, format!("取窗失败：{err}"))
                }
            };

            // 注：不在此处激活——首跑实测创建期（窗口未上屏）调用 activate 不可靠，
            // 自动模式改在 `logic()` 每帧核对 isActive 并重试（见 logic 实现）。

            // POC3_DARK=1：把本 app 外观强制为 darkAqua（系统当前为浅色）。
            // 用途：验证菜单观感由「app 有效外观（effectiveAppearance）」驱动——
            // 产品中 app 有效外观即系统外观，故菜单深/浅色跟随系统。
            if std::env::var_os("POC3_DARK").is_some() {
                if let Some(mtm) = MainThreadMarker::new() {
                    let app = NSApplication::sharedApplication(mtm);
                    match NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
                        Some(dark) => {
                            NSAppearanceCustomization::setAppearance(&*app, Some(&dark));
                            log_line("[poc3] POC3_DARK：app 外观已设为 darkAqua");
                        }
                        None => log_line("[poc3] POC3_DARK：拿不到 darkAqua 外观对象"),
                    }
                }
            }

            Ok(Box::new(PocApp {
                ns_view,
                ns_window,
                init_log,
                pops: 0,
                last_choice: None,
                last_block_ms: 0.0,
                blocked_total_ms: 0.0,
                auto,
                auto_started: false,
                activate_retries: 0,
                text: String::new(),
                received_keys: Vec::new(),
            }))
        }),
    )
}

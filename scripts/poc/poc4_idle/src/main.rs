//! PoC-4：eframe 两红线实测 ——「隐藏时 App CPU 0%」×「唤起响应 <100ms」。
//!
//! 判断依据（Evidence：全部读自实锁 crate 源码，非凭记忆；版本 = eframe/egui 0.37.0、
//! winit 0.30.13、objc2 0.6.5、objc2-app-kit 0.3.2、raw-window-handle 0.6.2）：
//!
//! 1. 隐藏态为何能零 CPU：事件循环空闲即 `ControlFlow::Wait`，只要没有任何 repaint 请求，
//!    隐藏视口不唤醒（eframe `src/native/run.rs:288-300` 显式 set_control_flow）。
//! 2. 隐藏视口的跨线程唤起被节流 100ms：`UserEvent::RequestRepaint` 处理时若窗口不可见，
//!    唤醒时刻被推迟到 `max(when, now + INVISIBLE_WINDOW_REPAINT_INTERVAL)`，
//!    常量 = 100ms（eframe `src/native/run.rs:27`、`run.rs:377-383`）。
//! 3. 「不可见」判定 = winit `Window::is_visible() == Some(false)`；macOS 上
//!    `set_visible(false)` 即 `orderOut`、`is_visible()` 即 `NSWindow.isVisible()`
//!    （winit `platform_impl/macos/window_delegate.rs:904-914`）——
//!    即 `ViewportCommand::Visible(false)` 后窗口对 eframe 就是「不可见」，节流生效。
//! 4. 隐藏窗口被唤醒时跑的是整帧（logic + ui + 绘制），不是 logic-only：
//!    eframe 用 `viewport.info.visible().unwrap_or(true)`（`native/glow_integration.rs:680`）判断，
//!    而 egui 的 `ViewportInfo::visible()` 只由 minimized/occluded 推导
//!    （`egui/src/data/input/viewport_info.rs:98`）；orderOut 后二者为 (Some(false), None)
//!    ⇒ `visible()` 返回 None ⇒ 按 true 处理。显示命令在帧末 `handle_viewport_output`
//!    才被应用（`window.set_visible(true)` → `makeKeyAndOrderFront`）。
//! 5. 跨线程唤醒通道存在：eframe 用 `set_request_repaint_callback` 把任意线程的
//!    `request_repaint` 经 `EventLoopProxy` 投递给 Wait 中的事件循环
//!    （`native/glow_integration.rs:364-378`）。
//!
//! 由此，本 PoC 的预期形态（待实测确认）：
//! - `cycle`（egui 路径）唤起延迟 ≈ 100ms（节流）+ 帧处理开销 —— 触红线的嫌疑点；
//! - `native`（主线程 `orderFrontRegardless` + `request_repaint`）绕开节流：
//!   窗口先在屏（内容为上一帧，视觉零延迟），再正常重绘首帧。
//!
//! 模式（`POC4_MODE`）：
//! - `hide`   常隐 SECS 秒（默认 60）：量隐藏态 CPU，并逐帧打点证明无唤醒源
//! - `show`   常显 SECS 秒（默认 15）：常显态对照
//! - `cycle`（默认）30 轮 [隐藏 2.5s → 显示 0.5s]；显示走 `ViewportCommand::Visible(true)`
//! - `native` 同 cycle，但显示走主队列 `orderFrontRegardless`（dispatch_async_f）
//! - `warm`   隐藏 + keep-warm（每 WARM_MS 申请一次重绘）：量 keep-warm 的 CPU 代价
//!
//! 环境变量：`POC4_MODE`（上）｜`POC4_SECS`（hide/show/warm 运行秒数，默认 60/15/60）｜
//! `POC4_WARM_MS`（warm 申请间隔，默认 100）｜`POC4_POS=x,y`｜
//! `POC4_T0_FILE`（每轮 t0 落盘 `纪元纳秒 轮次`，供外部轮询对齐「窗口在屏」时刻）。
//!
//! 打点（stderr，供脚本统计）：
//! - `SHOW_LATENCY_MS=<ms>`                 每轮一行：t0（发出显示请求）→ 首帧 `ui()` 进入
//! - `SHOW_LOGIC_MS=<ms>`                   每轮一行：t0 → 首个 `logic()`（拆通道成本与帧成本）
//! - `SHOW_DETAIL round=<n> path=<..> logic_ms=<..> win_visible_at_first_ui=<0|1>`
//! - `PASS seq=<n> kind=<logic|ui> win_visible=<0|1> t=<s>`   每帧一行（warm 模式每 50 帧一行）
//! - `SUMMARY ...`                          退出前汇总（帧数 / 隐藏期 ui 次数）
//!
//! 口径：延迟为「进程内 t0 → 首帧 `ui()` 进入」，不含热键系统投递，也不含帧内绘制与
//! 上屏（swap_buffers）耗时。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui;
use objc2::rc::Retained;
use objc2_app_kit::NSWindow;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 启动稳定期：等 eframe 首帧上屏（首帧后 eframe 才把窗口展出）。
const START_SETTLE: Duration = Duration::from_millis(1500);
/// 每轮显示后的停留时长。
const SHOW_STAY: Duration = Duration::from_millis(500);
/// 每轮隐藏的停留时长（+ 显示 500ms ⇒ 每轮 3 秒）。
const HIDDEN_STAY: Duration = Duration::from_millis(2500);
/// 循环轮数。
const ROUNDS: u32 = 30;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Hide,
    Show,
    Cycle,
    Native,
    Warm,
}

impl Mode {
    fn from_env() -> Self {
        match std::env::var("POC4_MODE").as_deref() {
            Ok("hide") => Self::Hide,
            Ok("show") => Self::Show,
            Ok("native") => Self::Native,
            Ok("warm") => Self::Warm,
            _ => Self::Cycle,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Hide => "hide",
            Self::Show => "show",
            Self::Cycle => "cycle",
            Self::Native => "native",
            Self::Warm => "warm",
        }
    }

    /// 显示路径标签（写进日志，便于统计脚本按路径分组）。
    fn path(self) -> &'static str {
        if self == Self::Native {
            "native"
        } else {
            "egui"
        }
    }
}

/// 一次「发出显示请求」：驱动线程写、主线程取（含 t0 与轮次）。
#[derive(Clone, Copy)]
struct ShowReq {
    round: u32,
    t0: Instant,
}

/// 驱动线程与主线程的共享状态。
struct Shared {
    show_req: Mutex<Option<ShowReq>>,
    /// 主线程侧 NSWindow 裸指针（`usize` 穿线程；0 = 未就绪）。
    ns_window: AtomicUsize,
    passes_logic: AtomicU64,
    passes_ui: AtomicU64,
    /// 窗口「不可见」（winit 口径）时跑到的 ui 帧数。
    ui_while_hidden: AtomicU64,
}

impl Shared {
    fn new() -> Self {
        Self {
            show_req: Mutex::new(None),
            ns_window: AtomicUsize::new(0),
            passes_logic: AtomicU64::new(0),
            passes_ui: AtomicU64::new(0),
            ui_while_hidden: AtomicU64::new(0),
        }
    }
}

fn vis_str(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "1",
        Some(false) => "0",
        None => "?",
    }
}

fn ms_since(t0: Instant) -> f64 {
    t0.elapsed().as_secs_f64() * 1000.0
}

/// 取窗口真实可见性（winit 口径 = `NSWindow.isVisible()`；隐藏命令生效后为 Some(false)）。
fn win_visible(frame: &eframe::Frame) -> Option<bool> {
    frame.winit_window().and_then(|w| w.is_visible())
}

/// native 显示路径：把 `orderFrontRegardless` 异步投递到主队列（主线程执行）。
///
/// 说明：macOS SDK 里 `dispatch_get_main_queue()` 已是头文件内联（改用全局量
/// `_dispatch_main_q`），直接声明该函数符号会链接失败——故走 dispatch2 crate 的
/// `DispatchQueue::main()`（其实现即 `&_dispatch_main_q`）。
fn native_show_async(ns_ptr: usize, ctx: egui::Context, round: u32, t0: Instant) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        // SAFETY: ns_ptr 是 Creator 闭包内取到的本进程 NSWindow 裸指针；app 运行期由
        // Poc4App 持有的 `Retained<NSWindow>` 保活，窗口不会先于本闭包释放。
        // 本闭包在主队列（主线程）上执行，AppKit 调用合法。
        let ns: &NSWindow = unsafe { &*(ns_ptr as *const NSWindow) };
        ns.orderFrontRegardless();
        // orderFrontRegardless 立即使窗口在屏（内容 = 上一帧，视觉零延迟）；
        // 此处窗口已可见，随后的 request_repaint 不再落进 eframe 的隐藏节流（+100ms）。
        let to_wake_ms = ms_since(t0);
        ctx.request_repaint();
        eprintln!("NATIVE_MAIN_MS={to_wake_ms:.3} round={round}（主线程 orderFrontRegardless 完成）");
    });
}

/// 把本轮 t0 落盘（`POC4_T0_FILE`，内容 = `纪元纳秒 轮次`）。
///
/// 用途：外部轮询脚本（CGWindowList）据此把「窗口真正在屏」的时刻与 t0 对齐，
/// 得到「对外可见延迟」——补上进程内打点测不到的上屏环节。
fn write_t0_marker(path: Option<&std::path::PathBuf>, round: u32) {
    let Some(path) = path else { return };
    let Ok(d) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    let _ = std::fs::write(path, format!("{} {round}", d.as_nanos()));
}

/// 驱动线程：按模式发显示/隐藏命令，并负责收尾退出（隐藏态无人唤醒，定时退出只能由它做）。
fn spawn_driver(
    ctx: egui::Context,
    shared: Arc<Shared>,
    mode: Mode,
    secs: u64,
    t0_file: Option<std::path::PathBuf>,
) {
    std::thread::spawn(move || {
        match mode {
            Mode::Hide | Mode::Show | Mode::Warm => {
                std::thread::sleep(Duration::from_secs(secs));
                eprintln!("[poc4] {} 模式到时（{secs}s），发 Close", mode.label());
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                std::thread::sleep(Duration::from_secs(2));
                eprintln!(
                    "SUMMARY logic_passes={} ui_passes={} ui_while_hidden={} (Close 兜底 process::exit)",
                    shared.passes_logic.load(Ordering::Relaxed),
                    shared.passes_ui.load(Ordering::Relaxed),
                    shared.ui_while_hidden.load(Ordering::Relaxed),
                );
                std::process::exit(0);
            }
            Mode::Cycle | Mode::Native => {
                std::thread::sleep(START_SETTLE);
                for round in 1..=ROUNDS {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    std::thread::sleep(HIDDEN_STAY);

                    write_t0_marker(t0_file.as_ref(), round);
                    let t0 = Instant::now();
                    *shared.show_req.lock().expect("show_req mutex") = Some(ShowReq { round, t0 });
                    match mode {
                        Mode::Cycle => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        }
                        _ => {
                            let ns = shared.ns_window.load(Ordering::SeqCst);
                            if ns == 0 {
                                eprintln!("[poc4] native 模式无 NSWindow 指针，退化为 egui 路径");
                                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                            } else {
                                native_show_async(ns, ctx.clone(), round, t0);
                            }
                        }
                    }
                    std::thread::sleep(SHOW_STAY);
                }
                eprintln!("[poc4] {ROUNDS} 轮结束，发 Close");
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                std::thread::sleep(Duration::from_secs(2));
                eprintln!(
                    "SUMMARY logic_passes={} ui_passes={} ui_while_hidden={} (Close 兜底 process::exit)",
                    shared.passes_logic.load(Ordering::Relaxed),
                    shared.passes_ui.load(Ordering::Relaxed),
                    shared.ui_while_hidden.load(Ordering::Relaxed),
                );
                std::process::exit(0);
            }
        }
    });
}

struct Poc4App {
    mode: Mode,
    shared: Arc<Shared>,
    started: Instant,
    /// 已取走、等待首帧的显示请求。
    pending: Option<ShowReq>,
    /// 隐藏命令是否已发（hide/warm 模式）。
    hide_sent: bool,
    warm_ms: u64,
    last_latency_ms: f64,
    last_logic_ms: f64,
    last_round: u32,
    last_ui_hidden: Option<bool>,
    pass_seq: u64,
    /// 保活：native 路径的裸指针指向它。
    _ns_window: Option<Retained<NSWindow>>,
}

impl eframe::App for Poc4App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.pass_seq += 1;
        self.shared.passes_logic.fetch_add(1, Ordering::Relaxed);
        let win_visible = win_visible(frame);

        // 取显示请求（每轮只取一次；被遮挡导致上一轮首帧没跑到时，明确记为 MISS 而不是算进延迟）。
        let req = self.shared.show_req.lock().expect("show_req mutex").take();
        if let Some(req) = req {
            self.last_logic_ms = ms_since(req.t0);
            eprintln!("SHOW_LOGIC_MS={:.3}", self.last_logic_ms);
            if let Some(old) = self.pending.replace(req) {
                eprintln!(
                    "SHOW_MISSED round={}（该轮首帧 ui 未跑到即被下一轮取代，多为窗口被他人窗口遮挡）",
                    old.round
                );
            }
        }

        // 模式行为：隐藏命令与 keep-warm 申请（隐藏期唯一的 repaint 申请点）。
        match self.mode {
            Mode::Hide => {
                if !self.hide_sent {
                    self.hide_sent = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    eprintln!("[poc4] hide：已发 Visible(false)；此后不再申请任何重绘");
                }
            }
            Mode::Warm => {
                if !self.hide_sent {
                    self.hide_sent = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    eprintln!(
                        "[poc4] warm：隐藏 + keep-warm（每 {}ms 申请一次重绘）",
                        self.warm_ms
                    );
                }
                ctx.request_repaint_after(Duration::from_millis(self.warm_ms));
            }
            _ => {}
        }

        let occluded = ctx.input(|i| i.viewport().occluded);
        if self.mode == Mode::Warm {
            let n = self.shared.passes_logic.load(Ordering::Relaxed);
            if n % 50 == 0 {
                eprintln!(
                    "WARM_TICK logic_passes={n} t={:.3}s",
                    self.started.elapsed().as_secs_f64()
                );
            }
        } else {
            eprintln!(
                "PASS seq={} kind=logic win_visible={} occluded={} t={:.3}",
                self.pass_seq,
                vis_str(win_visible),
                vis_str(occluded),
                self.started.elapsed().as_secs_f64()
            );
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.shared.passes_ui.fetch_add(1, Ordering::Relaxed);
        let win_visible = win_visible(frame);
        if win_visible == Some(false) {
            self.shared.ui_while_hidden.fetch_add(1, Ordering::Relaxed);
        }

        if let Some(p) = self.pending.take() {
            let ms = ms_since(p.t0);
            self.last_latency_ms = ms;
            self.last_round = p.round;
            self.last_ui_hidden = win_visible;
            eprintln!("SHOW_LATENCY_MS={ms:.3}");
            eprintln!(
                "SHOW_DETAIL round={} path={} logic_ms={:.3} win_visible_at_first_ui={} occluded_at_first_ui={}",
                p.round,
                self.mode.path(),
                self.last_logic_ms,
                vis_str(win_visible),
                vis_str(ui.ctx().input(|i| i.viewport().occluded))
            );
        }

        if self.mode != Mode::Warm {
            let occluded = ui.ctx().input(|i| i.viewport().occluded);
            eprintln!(
                "PASS seq={} kind=ui win_visible={} occluded={} t={:.3}",
                self.pass_seq,
                vis_str(win_visible),
                vis_str(occluded),
                self.started.elapsed().as_secs_f64()
            );
        }

        // UI 全静态（无动画、无自动刷新、无 tooltip 悬停态），避免自造 repaint 源。
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("PoC-4：隐藏零 CPU × 唤起延迟");
            ui.add_space(4.0);
            ui.label(format!("模式 = {}｜轮次 = {}/30", self.mode.label(), self.last_round));
            ui.label(format!(
                "上轮唤起延迟（t0 → 首帧 ui）= {:.3} ms",
                self.last_latency_ms
            ));
            ui.label(format!(
                "本轮首帧 ui 时窗口可见 = {}｜帧计数 logic/ui = {}/{}",
                vis_str(self.last_ui_hidden),
                self.shared.passes_logic.load(Ordering::Relaxed),
                self.shared.passes_ui.load(Ordering::Relaxed),
            ));
        });
    }
}

/// 从 `CreationContext` 取 NSWindow（同 PoC-1 主路径：`cc.window_handle() → NSView::window()`）。
fn ns_window_from_cc(cc: &eframe::CreationContext<'_>) -> Result<Retained<NSWindow>, String> {
    let handle = cc
        .window_handle()
        .map_err(|e| format!("window_handle(): {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => {
            // SAFETY: ns_view 指向 winit 为本 app 创建、当前存活的 NSView（主线程持有）。
            let view: &objc2_app_kit::NSView =
                unsafe { h.ns_view.cast::<objc2_app_kit::NSView>().as_ref() };
            view.window()
                .ok_or_else(|| "NSView::window() 返回 None".to_string())
        }
        other => Err(format!("非 AppKit 窗口句柄：{other:?}")),
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn main() -> eframe::Result<()> {
    let mode = Mode::from_env();
    let warm_ms = env_u64("POC4_WARM_MS", 100);
    let secs = env_u64(
        "POC4_SECS",
        if mode == Mode::Show { 15 } else { 60 },
    );
    let t0_file = std::env::var("POC4_T0_FILE").ok().map(std::path::PathBuf::from);
    let pos: Option<[f32; 2]> = std::env::var("POC4_POS").ok().and_then(|raw| {
        let (x, y) = raw.split_once(',')?;
        Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
    });
    eprintln!(
        "[poc4] 配置：mode={} path={} secs={} warm_ms={} pos={pos:?} t0_file={t0_file:?}",
        mode.label(),
        mode.path(),
        secs,
        warm_ms
    );

    // 悬浮条形态（与 PoC-1b 同）：置顶。默认开——产品里的悬浮条恒在普通窗口之上，
    // 置顶可避免测量期间被他人窗口遮挡（macOS 遮挡态会触发 eframe 的 logic-only 分支，
    // 使「首帧 ui」口径失真）。
    let ontop = std::env::var("POC4_ONTOP").as_deref() != Ok("0");
    let mut viewport = egui::ViewportBuilder::default().with_inner_size([600.0, 400.0]);
    if ontop {
        viewport = viewport.with_always_on_top();
    }
    if let Some(pos) = pos {
        viewport = viewport.with_position(pos);
    }
    eprintln!("[poc4] 窗口置顶（always_on_top）= {ontop}");
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let shared = Arc::new(Shared::new());
    eframe::run_native(
        "poc4-idle",
        opts,
        Box::new(move |cc| {
            let ns_window = match ns_window_from_cc(cc) {
                Ok(ns) => {
                    shared.ns_window.store(Retained::as_ptr(&ns) as usize, Ordering::SeqCst);
                    eprintln!("[poc4] NSWindow 已取到（native 路径可用）");
                    Some(ns)
                }
                Err(err) => {
                    eprintln!("[poc4] 取窗失败（native 路径回退 egui）：{err}");
                    None
                }
            };
            spawn_driver(cc.egui_ctx.clone(), Arc::clone(&shared), mode, secs, t0_file);
            Ok(Box::new(Poc4App {
                mode,
                shared,
                started: Instant::now(),
                pending: None,
                hide_sent: false,
                warm_ms,
                last_latency_ms: 0.0,
                last_logic_ms: 0.0,
                last_round: 0,
                last_ui_hidden: None,
                pass_seq: 0,
                _ns_window: ns_window,
            }))
        }),
    )
}

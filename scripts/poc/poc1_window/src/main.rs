//! PoC-1：eframe 无边框透明窗口 + objc2 补丁（浮动级别 / 跨 Space）。
//!
//! 验证 ADR-8 核心赌注：eframe/winit 公开 API 不提供窗口级别与跨 Space
//! （tech.md 已查证），从 eframe 窗口句柄取 NSWindow 后，用 objc2-app-kit 补丁：
//! `setLevel`（浮动级别档位）与
//! `setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)`。
//! 若 eframe 句柄路径不通，回退 `NSApplication::sharedApplication().windows()` 取窗口。
//!
//! 键位：
//! - ESC      退出
//! - F1 / H   隐藏窗口（3 秒后自动再显示，验证 ViewportCommand::Visible 双向）
//! - F2 / L   切换窗口级别：Floating(3) ↔ Status(25)（两档都试）
//!
//! 环境变量（自动化差分测试用，详见 docs/specs/v2-ui/poc-results.md 复现步骤）：
//! - `POC1_POS=x,y`      指定窗口全局坐标（逻辑点，可跨屏）
//! - `POC1_LEVEL=status` 起始用 Status(25) 档，默认 Floating(3)
//! - `POC1_NO_PATCH=1`   对照组：跳过 objc2 补丁
//! - `POC1_ACCESSORY=1`  附加 Accessory 激活策略（无 Dock 图标）
//! - `POC1_FS_AUTO=1`    自动跨 Space 差分测试：3s 开第二窗进系统全屏、15s 退、18s 关
//!
//! 实锁版本（API 名逐一对照实锁 crate 源码确认，非凭记忆）：
//! eframe / egui 0.37.0、objc2 0.6.5、objc2-app-kit 0.3.2、raw-window-handle 0.6.2。
//! eframe 0.37 的 App trait 为 `logic` + `ui` 两个方法（无 `update`），与 0.29 时代不同。

use std::time::Duration;
use std::time::Instant;

use eframe::egui;
use objc2::ffi::NSInteger;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSFloatingWindowLevel, NSScreen,
    NSStatusWindowLevel, NSView, NSWindow, NSWindowCollectionBehavior,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

/// 隐藏后自动再显示的等待时长（验证 ViewportCommand::Visible 双向切换）。
const REVEAL_AFTER: Duration = Duration::from_secs(3);

/// 漂移核对的自驱间隔（保证隐藏期与空闲期也持续核对补丁）。
const CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// 窗口级别档位（plan Step 3 要求两档都试）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    /// `NSFloatingWindowLevel` = 3。
    Floating,
    /// `NSStatusWindowLevel` = 25。
    Status,
}

impl Level {
    fn ns_level(self) -> NSInteger {
        match self {
            Self::Floating => NSFloatingWindowLevel,
            Self::Status => NSStatusWindowLevel,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Floating => "Floating (NSFloatingWindowLevel = 3)",
            Self::Status => "Status (NSStatusWindowLevel = 25)",
        }
    }

    fn toggle(self) -> Self {
        match self {
            Self::Floating => Self::Status,
            Self::Status => Self::Floating,
        }
    }
}

/// 跨 Space 需要的 collection behavior 组合（ADR-8 指定值）。
fn target_behavior() -> NSWindowCollectionBehavior {
    NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary
}

/// 打印各屏幕的全局坐标（macOS 全局坐标系原点在主屏左上角；供多屏定位参考）。
fn dump_screens() {
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return;
    };
    for (i, screen) in NSScreen::screens(mtm).iter().enumerate() {
        let f = screen.frame();
        eprintln!(
            "[poc1] screen[{i}] {}：frame=({}, {}) {}x{}",
            screen.localizedName(),
            f.origin.x,
            f.origin.y,
            f.size.width,
            f.size.height
        );
    }
}

/// 解析 `POC1_POS=x,y`（逻辑点，全局坐标），用于把窗口放到指定屏。
fn env_position() -> Option<[f32; 2]> {
    let raw = std::env::var("POC1_POS").ok()?;
    let (x, y) = raw.split_once(',')?;
    Some([x.trim().parse().ok()?, y.trim().parse().ok()?])
}

fn main() -> eframe::Result<()> {
    // 多屏坐标诊断（供 POC1_POS 选屏用；也验证 NSScreen 枚举可用性）。
    dump_screens();

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([600.0, 400.0])
        .with_decorations(false)
        .with_always_on_top()
        .with_transparent(true)
        // egui 文档建议：macOS 透明窗口关阴影，避免 ghosting 残影。
        .with_has_shadow(false);
    if let Some(pos) = env_position() {
        eprintln!("[poc1] POC1_POS 指定窗口位置 = {pos:?}");
        viewport = viewport.with_position(pos);
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    // POC1_NO_PATCH=1：对照组——跳过 objc2 补丁，验证「无补丁则无法跨 Space」。
    let no_patch = std::env::var_os("POC1_NO_PATCH").is_some();

    // POC1_FS_AUTO=1：自动做「跨 Space」差分测试（第二窗口进系统全屏）。
    let fs_auto = std::env::var_os("POC1_FS_AUTO").is_some();

    // POC1_LEVEL=status 时起始用 Status 档（25），否则 Floating 档（3）；
    // 供「窗口级别是否影响跨 Space」实测（plan Step 3 要求两档都试）。
    let start_level = match std::env::var("POC1_LEVEL").as_deref() {
        Ok("status") => Level::Status,
        _ => Level::Floating,
    };

    eframe::run_native(
        "poc1-window",
        opts,
        Box::new(move |cc| {
            let level = start_level;
            let mut app = PocApp {
                level,
                no_patch,
                ns_window: None,
                patch_log: String::new(),
                initial_level: None,
                initial_behavior: None,
                level_drift: 0,
                behavior_drift: 0,
                hidden_cycle: 0,
                text: String::new(),
                fs_auto,
                fs_started: fs_auto.then(Instant::now),
                fs_state: FsState::Wait,
            };
            if fs_auto {
                eprintln!("[poc1] POC1_FS_AUTO：将在 3 秒后打开全屏测试窗（15 秒退出、18 秒关窗）");
            }
            // POC1_ACCESSORY=1：切到 Accessory 激活策略（无 Dock 图标）。
            // 外部证据（tauri#11488）：普通 NSWindow 仅设 CanJoinAllSpaces|FullScreenAuxiliary
            // 不足以盖住他人全屏 Space，需 Accessory（LSUIElement）策略或 NSPanel 化。
            if std::env::var_os("POC1_ACCESSORY").is_some() {
                if let Some(mtm) = objc2::MainThreadMarker::new() {
                    let app = NSApplication::sharedApplication(mtm);
                    let ok = app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
                    eprintln!("[poc1] POC1_ACCESSORY：setActivationPolicy(Accessory) -> {ok}");
                }
            }
            if no_patch {
                eprintln!("[poc1] POC1_NO_PATCH：对照组模式，跳过 objc2 补丁");
                app.patch_log = "对照组模式（POC1_NO_PATCH），未施 objc2 补丁".to_string();
                return Ok(Box::new(app));
            }
            match patch_window(cc, level) {
                Ok(outcome) => {
                    eprintln!(
                        "[poc1] 补丁已施：取窗路径 = {}；回读 level={}，behavior=CanJoinAllSpaces:{} FullScreenAuxiliary:{}",
                        outcome.via,
                        outcome.initial_level,
                        outcome.initial_behavior.contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
                        outcome.initial_behavior.contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
                    );
                    app.patch_log = format!("已施补丁，取窗路径 = {}", outcome.via);
                    app.initial_level = Some(outcome.initial_level);
                    app.initial_behavior = Some(outcome.initial_behavior);
                    app.ns_window = Some(outcome.ns_window);
                }
                Err(err) => {
                    eprintln!("[poc1] 补丁失败：{err}");
                    app.patch_log = format!("补丁失败：{err}");
                }
            }
            Ok(Box::new(app))
        }),
    )
}

/// 取 NSWindow 并施加浮动级别 + 跨 Space 补丁。
fn patch_window(cc: &eframe::CreationContext<'_>, level: Level) -> Result<PatchOutcome, String> {
    let (ns_window, via) = match ns_window_from_cc(cc) {
        Ok(win) => (win, "cc.window_handle() → NSView::window()"),
        Err(primary_err) => {
            let win = ns_window_via_app().map_err(|fb_err| {
                format!("句柄路径失败（{primary_err}）；回退路径也失败（{fb_err}）")
            })?;
            (win, "回退：NSApplication::sharedApplication().windows()")
        }
    };

    // objc2-app-kit 的 setLevel: / setCollectionBehavior: 是安全方法，仅写入对象属性；
    // NSWindow 由 winit 创建，生命周期覆盖整个 app。
    ns_window.setLevel(level.ns_level());
    ns_window.setCollectionBehavior(target_behavior());

    Ok(PatchOutcome {
        initial_level: ns_window.level(),
        initial_behavior: ns_window.collectionBehavior(),
        ns_window,
        via,
    })
}

struct PatchOutcome {
    ns_window: Retained<NSWindow>,
    via: &'static str,
    initial_level: NSInteger,
    initial_behavior: NSWindowCollectionBehavior,
}

/// 主路径：eframe `CreationContext` 实现 `HasWindowHandle`，取 AppKit 句柄里的 NSView，
/// 再经 `NSView::window()` 拿所属 NSWindow。
fn ns_window_from_cc(cc: &eframe::CreationContext<'_>) -> Result<Retained<NSWindow>, String> {
    let handle = cc
        .window_handle()
        .map_err(|e| format!("window_handle(): {e}"))?;
    match handle.as_raw() {
        RawWindowHandle::AppKit(h) => {
            // SAFETY: ns_view 指向 winit 为本 app 创建、当前存活的 NSView（主线程持有），
            // 生命周期覆盖 eframe 主循环；此处只借读出其所属 NSWindow，不改变所有权。
            let view: &NSView = unsafe { h.ns_view.cast::<NSView>().as_ref() };
            view.window()
                .ok_or_else(|| "NSView::window() 返回 None".to_string())
        }
        other => Err(format!("非 AppKit 窗口句柄：{other:?}")),
    }
}

/// 回退路径（plan Step 2 备选）：从 sharedApplication 的窗口列表里挑首个可见窗口。
fn ns_window_via_app() -> Result<Retained<NSWindow>, String> {
    let mtm = objc2::MainThreadMarker::new().ok_or("不在主线程，拿不到 MainThreadMarker")?;
    let app = NSApplication::sharedApplication(mtm);
    let windows = app.windows();
    windows
        .iter()
        .find(|w| w.isVisible())
        .or_else(|| windows.iter().next())
        .ok_or_else(|| "sharedApplication().windows() 为空".to_string())
}

struct PocApp {
    level: Level,
    no_patch: bool,
    ns_window: Option<Retained<NSWindow>>,
    patch_log: String,
    initial_level: Option<NSInteger>,
    initial_behavior: Option<NSWindowCollectionBehavior>,
    level_drift: u32,
    behavior_drift: u32,
    hidden_cycle: u32,
    text: String,
    /// `POC1_FS_AUTO=1` 时启用：自动开第二窗口→进系统全屏（新 Space）→退出→关窗。
    fs_auto: bool,
    fs_started: Option<Instant>,
    fs_state: FsState,
}

/// 自动全屏差分测试的状态机。
#[derive(Clone, Copy, PartialEq, Eq)]
enum FsState {
    /// 等待启动延迟。
    Wait,
    /// 已进入系统全屏，等待退出。
    InFullscreen,
    /// 已退出全屏，等待关窗。
    Exited,
    /// 已关窗，测试结束。
    Done,
}

/// 全屏测试窗的 viewport id。
fn fs_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("poc1-fs-test")
}

impl PocApp {
    /// `POC1_FS_AUTO`：自动做「跨 Space」差分测试——第二窗口进系统全屏（macOS 新建
    /// Space），观察悬浮主窗是否随之出现在该 Space；到点自动退出并关窗。
    fn drive_fullscreen_test(&mut self, ctx: &egui::Context) {
        let Some(start) = self.fs_started else {
            return;
        };
        if self.fs_state == FsState::Done {
            return;
        }
        // 每帧调用以维持 viewport（id 幂等，仅更新 builder）。
        ctx.show_viewport_deferred(
            fs_viewport_id(),
            egui::ViewportBuilder::default()
                .with_inner_size([520.0, 320.0])
                // 固定落在主屏（全局坐标 100,100），保证全屏 Space 建在主屏。
                .with_position([100.0, 100.0])
                .with_title("PoC-1 全屏测试窗（自动退出）"),
            |ui, _class| {
                ui.heading("PoC-1 全屏测试窗");
                ui.label("本窗已自动进入系统全屏：macOS 会为其新建一个 Space。");
                ui.label("观察悬浮主窗（Floating 级）是否也出现在这个全屏 Space 里。");
                ui.label("约 12 秒后自动退出全屏并关闭本窗。");
            },
        );
        let el = start.elapsed();
        match self.fs_state {
            FsState::Wait if el >= Duration::from_secs(3) => {
                eprintln!("[poc1] fs-test: 第二窗口进入系统全屏（新建 Space）");
                ctx.send_viewport_cmd_to(fs_viewport_id(), egui::ViewportCommand::Fullscreen(true));
                self.fs_state = FsState::InFullscreen;
            }
            FsState::InFullscreen if el >= Duration::from_secs(15) => {
                eprintln!("[poc1] fs-test: 退出系统全屏");
                ctx.send_viewport_cmd_to(
                    fs_viewport_id(),
                    egui::ViewportCommand::Fullscreen(false),
                );
                self.fs_state = FsState::Exited;
            }
            FsState::Exited if el >= Duration::from_secs(18) => {
                eprintln!("[poc1] fs-test: 关闭测试窗，测试结束");
                ctx.send_viewport_cmd_to(fs_viewport_id(), egui::ViewportCommand::Close);
                self.fs_state = FsState::Done;
            }
            _ => {}
        }
    }

    /// 每帧（含隐藏期的补跑）核对补丁是否被 winit/系统覆盖；漂移即重施并计数（证据留痕）。
    fn check_and_reassert(&mut self) {
        if self.no_patch {
            return;
        }
        let Some(ns) = &self.ns_window else {
            return;
        };
        let want_level = self.level.ns_level();
        if ns.level() != want_level {
            self.level_drift += 1;
            if self.level_drift <= 3 {
                eprintln!(
                    "[poc1] level 漂移（第 {} 次）：实测 {}，重施 {}",
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
                    "[poc1] collectionBehavior 漂移（第 {} 次），重施",
                    self.behavior_drift
                );
            }
            ns.setCollectionBehavior(want_behavior);
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
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.hidden_cycle += 1;
            eprintln!(
                "[poc1] 隐藏（Visible(false)），{REVEAL_AFTER:?} 后自动再显示（隐藏后无键盘焦点，用后台线程定时再显示；与产品里全局热键线程唤起同构）"
            );
            // egui::Context 是 Send+Sync，可跨线程发送窗口命令（隐藏期主循环不跑 ui）。
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(REVEAL_AFTER);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                eprintln!("[poc1] 隐藏后自动再显示（Visible(true)）");
            });
        }

        if f2 || (l && !typing) {
            self.level = self.level.toggle();
            if let Some(ns) = &self.ns_window {
                ns.setLevel(self.level.ns_level());
            }
            eprintln!("[poc1] 窗口级别切换为 {}", self.level.label());
        }
    }
}

impl eframe::App for PocApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // 全透明底色；半透明黑由 CentralPanel 的 frame 填充。
        [0.0, 0.0, 0.0, 0.0]
    }

    /// 非渲染逻辑：跑在每帧渲染前，窗口隐藏时也会被补跑。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.check_and_reassert();
        if self.fs_auto {
            self.drive_fullscreen_test(ctx);
        }
        // 低频自驱，保证漂移核对在空闲期与隐藏期也持续运行。
        ctx.request_repaint_after(CHECK_INTERVAL);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.handle_keys(ui.ctx());

        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(egui::Color32::from_black_alpha(220))
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| {
                ui.heading("PoC-1：切 Space / 全屏 App 上方观察本窗");
                ui.add_space(4.0);

                ui.label(format!("窗口级别：{}", self.level.label()));
                if self.no_patch {
                    ui.colored_label(
                        egui::Color32::YELLOW,
                        "对照组模式（POC1_NO_PATCH）：未施 objc2 补丁，预期无法跨 Space",
                    );
                }
                match (self.initial_level, self.initial_behavior) {
                    (Some(lv), Some(bh)) => {
                        ui.label(format!(
                            "启动回读：level={lv}；CanJoinAllSpaces={}；FullScreenAuxiliary={}",
                            bh.contains(NSWindowCollectionBehavior::CanJoinAllSpaces),
                            bh.contains(NSWindowCollectionBehavior::FullScreenAuxiliary),
                        ));
                        ui.label(format!(
                            "补丁漂移：level {} 次；behavior {} 次（每帧核对，漂移即重施）",
                            self.level_drift, self.behavior_drift
                        ));
                    }
                    _ => {
                        if !self.no_patch {
                            ui.colored_label(
                                egui::Color32::RED,
                                format!("补丁未施：{}", self.patch_log),
                            );
                        }
                    }
                }
                ui.label(format!("取窗路径：{}", self.patch_log));
                ui.label(format!("隐藏/再显示循环已执行 {} 次", self.hidden_cycle));

                ui.separator();
                ui.label("焦点测试（点这里打字）：");
                ui.text_edit_singleline(&mut self.text);

                ui.separator();
                ui.label("ESC 退出 ｜ F1 / H 隐藏（3 秒后自动再显示）｜ F2 / L 切换窗口级别");
                ui.label(
                    "验证步骤：Control+←/→ 切 Space 看本窗是否跟随；再开一个全屏 App 看本窗是否浮在其上。",
                );
            });
    }
}

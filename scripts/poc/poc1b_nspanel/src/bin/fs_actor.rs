//! PoC-1 续配套：独立的「他人全屏应用」进程。
//!
//! 开一个普通窗口，3 秒后进入系统全屏（`toggleFullScreen:`，macOS 会为其新建一个 Space），
//! 直到 `/tmp/poc1b-actor-quit` 出现才退出。与 `poc1b-npanel` 是两个独立进程 / 独立
//! NSApplication，充当被测悬浮条的「他人全屏 Space」。

use std::time::{Duration, Instant};

use eframe::egui;

/// 触发退出的信号文件。
const QUIT_FILE: &str = "/tmp/poc1b-actor-quit";

struct Actor {
    started: Instant,
    fs_requested: bool,
    closing: bool,
}

impl eframe::App for Actor {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let elapsed = self.started.elapsed();
        if !self.fs_requested && elapsed >= Duration::from_secs(3) {
            eprintln!("[actor] 进入系统全屏（新建 Space）");
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            self.fs_requested = true;
        }
        if !self.closing && std::path::Path::new(QUIT_FILE).exists() {
            eprintln!("[actor] 收到退出信号，关窗退出");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            self.closing = true;
        }
        ctx.request_repaint_after(Duration::from_millis(200));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("PoC-1b 全屏角色（他人应用）");
            ui.add_space(6.0);
            ui.label("本窗 3 秒后进入系统全屏：macOS 新建一个 Space，并切到该 Space。");
            ui.label(format!(
                "已运行 {:.0} 秒；测试收尾时创建 {} 即退出。",
                self.started.elapsed().as_secs_f32(),
                QUIT_FILE
            ));
        });
    }
}

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("PoC-1b 全屏角色")
            .with_inner_size([520.0, 320.0])
            .with_position([100.0, 100.0]),
        ..Default::default()
    };
    eprintln!("[actor] 启动；3 秒后进全屏；退出信号={QUIT_FILE}");
    eframe::run_native(
        "poc1b-fs-actor",
        opts,
        Box::new(|_cc| {
            Ok(Box::new(Actor {
                started: Instant::now(),
                fs_requested: false,
                closing: false,
            }))
        }),
    )
}

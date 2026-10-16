//! macEverything v2 GUI 入口。
//!
//! 本任务(Task 9)只搭 eframe 主循环与双视口壳:主视口 = 完整结果窗口占位,
//! 第二视口 = 悬浮条占位。启动编排(单实例 / 引擎拉起)与平台行为分别在
//! Task 10 / Task 14 接线。
mod platform;
mod ui;

use eframe::egui;

fn main() -> anyhow::Result<()> {
    init_logger();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("macEverything")
            .with_inner_size([900.0, 600.0])
            .with_visible(true), // 开发期可见;Task 14 接平台行为后收
        ..Default::default()
    };
    eframe::run_native(
        "macEverything",
        native_options,
        Box::new(|cc| Ok(Box::new(ui::App::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe 启动失败: {e}"))
}

/// 极简日志:stderr(与 cli 的 init_logger 同形态;文件日志待 Task 10 接数据目录)。
fn init_logger() {
    struct StderrLogger;

    impl log::Log for StderrLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::LevelFilter::Info
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                eprintln!("[{}] {}", record.level(), record.args());
            }
        }

        fn flush(&self) {}
    }

    if log::set_boxed_logger(Box::new(StderrLogger)).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

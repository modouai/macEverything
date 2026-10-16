//! macEverything v2 GUI 入口。
//!
//! 启动序列(Task 10):app.sock 单实例(二次启动在 acquire 内唤起已有实例
//! 并退出)→ 引擎探测/拉起(失败不阻塞 UI;悬浮条内展示错误 Task 12 接,
//! 此处 stderr 留痕)→ eframe 主循环(双视口壳,平台行为 Task 14 接)。

mod engine_launcher;
mod platform;
mod single_instance;
mod ui;

use eframe::egui;
use std::path::PathBuf;

/// 运行时数据目录(与 CLI 同规:`$HOME/Library/Application Support/macEverything`)。
fn data_dir() -> anyhow::Result<PathBuf> {
    let home =
        std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME 未设置,无法确定数据目录"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/macEverything"))
}

fn main() -> anyhow::Result<()> {
    init_logger();

    let paths = client::DaemonPaths {
        data_dir: data_dir()?,
    };

    // 0) 单实例(eframe 前):二次启动在 acquire 内「唤起已有实例 + exit(0)」
    let (instance, incoming) = single_instance::acquire(&paths)?;

    // 1) 引擎探测/拉起:失败不阻塞 UI(错误展示 Task 12 接,此处 stderr 留痕)
    if let Err(err) = engine_launcher::ensure_engine(&paths) {
        eprintln!("引擎未就绪: {err:#}");
    }

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
        Box::new(move |cc| {
            instance.attach_ui(cc.egui_ctx.clone());
            Ok(Box::new(ui::App::new(cc, incoming)))
        }),
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

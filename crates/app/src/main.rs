//! macEverything v2 GUI 入口。
//!
//! 启动序列(Task 10):app.sock 单实例(二次启动在 acquire 内唤起已有实例
//! 并退出)→ 引擎探测/拉起(失败不阻塞 UI;悬浮条内展示错误 Task 12 接,
//! 此处 stderr 留痕)→ 开机自启默认注册(Task 20,product §4;打包形态才生效)
//! → eframe 主循环(双视口壳,平台行为 Task 14 接)。

mod autostart;
mod engine_launcher;
mod hotkey;
mod platform;
mod session;
mod single_instance;
mod tray;
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

    // 2) 开机自启(Task 20,product §4):首次启动默认注册;用户曾主动关闭
    // (数据目录标记)则不默认注册,开发形态(未打包)跳过。失败不阻塞启动。
    if let Err(err) = autostart::register_at_startup(&paths.data_dir) {
        log::warn!("开机自启默认注册未完成: {err:#}");
    }

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("macEverything")
            .with_inner_size([900.0, 600.0])
            .with_visible(true),
        // ↑ 完整窗口启动即显示:开发期入口(窗口内 dev 按钮唤起悬浮条/退出)。
        // 产品形态(两窗皆隐、LSUIElement、托盘/热键入口)待 Task 15/16/17/20;
        // 悬浮条视口自身启动隐藏(ui::palette_builder 的 visible(false))。
        ..Default::default()
    };
    eframe::run_native(
        "macEverything",
        native_options,
        Box::new(move |cc| {
            instance.attach_ui(cc.egui_ctx.clone());
            // 全局热键(Task 15):主线程 + NSApplication 就绪后注册(macOS 要求),
            // 起事件泵线程;失败不中断启动(降级为悬浮条提示 + 日志)。
            let hotkey = hotkey::install(&cc.egui_ctx);
            // 菜单栏托盘(Task 16):本闭包在 winit 首个 resumed(StartCause::Init)
            // 之下运行 —— tray-icon 文档要求的「事件循环已在跑、不是刚创建」最早时机;
            // 菜单/图标构建须主线程(mac 实现取 MainThreadMarker)。失败不中断启动:
            // 悬浮条/热键/完整窗口仍在,仅托盘入口缺失(日志留痕,tray.rs 模块注释)。
            let tray = match tray::install(&cc.egui_ctx, paths.clone()) {
                Ok(runtime) => Some(runtime),
                Err(err) => {
                    log::error!("菜单栏托盘不可用:{err:#}");
                    None
                }
            };
            Ok(Box::new(ui::App::new(cc, incoming, paths, hotkey, tray)))
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

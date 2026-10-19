//! macEverything v2 GUI 入口。
//!
//! 启动序列(Task 10):app.sock 单实例(二次启动在 acquire 内唤起已有实例
//! 并退出)→ 引擎探测/拉起(失败不阻塞 UI;悬浮条内展示错误 Task 12 接,
//! 此处 stderr 留痕)→ 开机自启默认注册(Task 20,product §4;打包形态才生效)
//! → eframe 主循环(双视口壳,平台行为 Task 14 接)。

mod autostart;
mod engine_launcher;
mod hotkey;
mod i18n;
mod platform;
mod session;
mod settings;
mod single_instance;
mod tray;
mod ui;
// 剩余未接线的公开 API 是托盘/设置页文案与下载页地址，待 Task 6-7 接入；
// 全部消费后必须移除本模块级 allow。
#[allow(dead_code)]
mod update;
mod window_state;

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

    // 完整窗口尺寸(visual.md §1.2):默认 1080×627,记住用户调整后的尺寸。
    // 读取失败/无文件回落默认;越界由 window_state::load 内的 clamp 收口。
    let window_size = window_state::load(&paths.data_dir).unwrap_or_default();
    log::info!(
        "完整窗口尺寸:{:.0}×{:.0}(最小 {:.0}×{:.0})",
        window_size.w,
        window_size.h,
        window_state::MIN_W,
        window_state::MIN_H
    );

    // 设置(Task 3 起:字号倍率):读取失败/无文件回落默认;损坏文件不改写。
    let settings = settings::load(&paths.data_dir);
    log::info!(
        "设置字号:{:?}(scale {:.2})",
        settings.font_size,
        settings.font_size.scale()
    );

    // i18n：必须在任何视口/托盘创建之前设置语言（首帧与首次安装的菜单都取当前值）。
    let preferred = platform::preferred_languages().unwrap_or_else(|err| {
        log::warn!("读取系统语言失败,按简体中文: {err}");
        Vec::new()
    });
    let lang = i18n::resolve_language(settings.language, &preferred);
    i18n::set_lang(lang);
    log::info!("启动语言:{lang:?}(设置 {:?})", settings.language);

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("macEverything")
            .with_inner_size([window_size.w, window_size.h])
            .with_min_inner_size([window_state::MIN_W, window_state::MIN_H])
            .with_resizable(true)
            // 记忆尺寸可能来自更大/更小的屏幕:交给 winit 按当前显示器收口,
            // 防 3840×2160 的历史值在小屏上开出屏幕外/超出工作区。
            .with_clamp_size_to_monitor_size(true)
            // 标题栏透明化(完整窗口视觉重构;visual.md §3.1/§3.3):保留原生交通灯,
            // 内容绘制到标题栏下方 —— 标题不显示、标题栏透明、内容全尺寸。
            // 窗口本身仍是不透明观感:结果窗口每帧先铺整窗底色(ui/results.rs)。
            .with_title_shown(false)
            .with_titlebar_shown(false)
            .with_fullsize_content_view(true)
            .with_has_shadow(true)
            // 透明根窗(硬要求,勿删):eframe 0.37 的 glow 后端只有一个全局 GL config
            // (glow_integration.rs:76「one painter shared by all viewports」),
            // 其透明位取自**根视口** builder(`with_transparency(native_options.viewport
            // .transparent.unwrap_or(false))`,glow_integration.rs:1212)。子视口(悬浮条)
            // 自己的 with_transparent 只影响窗口属性,改不了该 config;config 报
            // supports_transparency()==Some(false) 时,glutin_winit::finalize_window
            // (glutin-winit-0.5.0/src/lib.rs:199-202)会把窗口属性**强制**改回不透明。
            // 实测后果:悬浮条圆角外区域渲染为近黑(像素取证 (8,8,8),底层为 (41,44,53)),
            // 且 eframe 留痕「Cannot create transparent window: the GL config does not support it」。
            // macOS CGL 的 config.transparency 只是回显模板位(cgl/config.rs:123),
            // 像素格式本身恒带 8 位 alpha——故此位对根窗只是「允许透明」,根窗自绘不透明
            // 底,观感不变;PoC-1b 单视口实测即此路径(透明像素取证 alpha 220)。
            // 完整窗口的「不透明」由内容层每帧铺整窗底色保证(ui/results.rs);
            // 此处**不可**改 false——会连带打死悬浮条透明(GL config 全局单份)。
            .with_transparent(true)
            // 产品形态(Task 21):三窗皆隐、仅菜单栏图标 —— 启动不弹窗,登录自启静默
            // 待命。eframe 0.37 在首帧 post_rendering 里**无条件** set_visible(true)
            // (native/epi_integration.rs:443-450,白闪修复),故 with_visible(false)
            // 本身不足以保持隐藏:首帧纠偏在 `ui::App::logic`(发 Visible(false),
            // 命令在同帧 post_rendering 之后应用,窗口不经 WindowServer 合成,不闪烁)。
            .with_visible(false),
        ..Default::default()
    };
    eframe::run_native(
        "macEverything",
        native_options,
        Box::new(move |cc| {
            instance.attach_ui(cc.egui_ctx.clone());
            // 全局热键(Task 15):主线程 + NSApplication 就绪后注册(macOS 要求),
            // 起事件泵线程;失败不中断启动(降级为悬浮条提示 + 日志)。
            // settings.toml 中的自定义键优先,注册失败回落默认并留提示(Task H2)。
            let hotkey = hotkey::install(&cc.egui_ctx, &settings);
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
            Ok(Box::new(ui::App::new(
                cc, incoming, paths, settings, hotkey, tray,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe 启动失败: {e}"))
}

/// 诊断开关缓存(进程内只读一次:本函数在每条日志的 `enabled` 里调用)。
fn trace_eframe() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var_os("MCE_TRACE_EFRAME").is_some())
}

/// 极简日志:stderr(与 cli 的 init_logger 同形态;文件日志待 Task 10 接数据目录)。
fn init_logger() {
    struct StderrLogger;

    impl log::Log for StderrLogger {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            // 诊断开关:设 MCE_TRACE_EFRAME=1 时放行 eframe/egui 的 Trace 日志(排帧定位)。
            if trace_eframe()
                && (metadata.target().starts_with("eframe")
                    || metadata.target().starts_with("egui"))
            {
                return metadata.level() <= log::LevelFilter::Trace;
            }
            metadata.level() <= log::LevelFilter::Info
        }

        fn log(&self, record: &log::Record<'_>) {
            if self.enabled(record.metadata()) {
                eprintln!("[{}] {}", record.level(), record.args());
            }
        }

        fn flush(&self) {}
    }

    let max = if trace_eframe() {
        log::LevelFilter::Trace
    } else {
        log::LevelFilter::Info
    };
    if log::set_boxed_logger(Box::new(StderrLogger)).is_ok() {
        log::set_max_level(max);
    }
}

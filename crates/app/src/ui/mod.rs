//! UI 层:双视口壳。
//!
//! 主视口(ROOT)= 完整结果窗口占位(Task 17 填结果表格);
//! 第二视口 = 悬浮条(Task 12:输入即搜/结果列表/键位;平台行为 Task 14 套)。
//!
//! 中文字体(2026-10-16 调查结论,不显式加载字体文件):
//! ① 苹方已无稳定路径——macOS 26 起移入 AssetsV2 哈希目录,本机实测
//!    `/System/Library/Fonts/PingFang.ttc` 不存在;
//! ② 稳定路径备选均为 .ttc 集合且 face 0 不是简体面(PingFang.ttc face0=HK、
//!    STHeiti Light.ttc face0=TC,fontTools 实查)——静默 pin index 0 会把简体
//!    用户引向错字形面;按名取面须自解析 name 表,成本/风险不划算;
//! ③ 系统回退已给出期望字体:CoreText(zh_CN 语境)对汉字回退即
//!    PingFangSC-Regular(`CTFontCreateForString` 探针实测);egui `system_fonts`
//!    的 fontique 走同一系统回退源。
//! 回退链(现状,实测生效):egui 默认字体(拉丁/图标)→ FontProvider 按缺失字形
//! 逐簇查系统字体(zh 语境 → 苹方;韩文 → AppleSDGothicNeo;emoji → Apple Color Emoji)。
//! `probe_fonts()` 启动留痕候选存在性;运行期实际命中字体在 `ui()` 留痕一次。

pub mod palette;
pub mod theme;

use eframe::egui;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use crate::platform;
use crate::single_instance::Incoming;

/// 系统 CJK 字体候选路径(探测用;结论见模块注释「中文字体」)。
const CJK_FONT_CANDIDATES: [&str; 2] = [
    "/System/Library/Fonts/PingFang.ttc", // 苹方(macOS ≤ 15 路径)
    "/System/Library/Fonts/STHeiti Light.ttc", // 黑体(旧备选)
];

/// 启动探测候选字体存在性并留痕(不加载——理由见模块注释「中文字体」)。
fn probe_fonts() {
    for path in CJK_FONT_CANDIDATES {
        log::info!(
            "CJK 字体候选 {path}:{}",
            if std::path::Path::new(path).exists() {
                "存在"
            } else {
                "不存在"
            }
        );
    }
}

pub struct App {
    /// 悬浮条是否显示(隐藏/唤起的原生路径 Task 14 接;此处先视口级开关)。
    palette_visible: bool,
    /// 二次启动唤起通道(app.sock 监听线程 → 主线程;main 接线)。
    incoming: Receiver<Incoming>,
    /// 悬浮条状态与查询会话(Task 12)。
    palette: palette::PaletteState,
    /// 外观探测节流(首次即查,之后每 ~2s;见 `logic`)。
    theme_throttle: theme::ThemeThrottle,
    /// 已应用的主题(启动前探测已置;`logic` 里变化才重应用)。
    theme: Option<theme::Theme>,
    /// 运行期系统字体回退留痕一次(`ui()`;诊断用)。
    fonts_logged: bool,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        incoming: Receiver<Incoming>,
        paths: client::DaemonPaths,
    ) -> Self {
        probe_fonts();
        // 首个 pass 之前先应用一次外观:egui 首帧按 System 偏好的 fallback(Dark)
        // 出图,logic() 里首查已晚于该帧 → 浅色系统启动会闪一帧深色。
        let theme = theme::current_theme();
        theme::apply(&cc.egui_ctx, theme);
        log::info!("启动外观:{theme:?}");
        Self {
            palette_visible: true,
            incoming,
            palette: palette::PaletteState::new(cc, paths),
            theme_throttle: theme::ThemeThrottle::new(theme::CHECK_INTERVAL),
            theme: Some(theme),
            fonts_logged: false,
        }
    }

    /// 二次启动唤起:置显示开关 + 请求重绘(Task 14 换 native
    /// orderFrontRegardless 显示路径)。
    fn drain_incoming(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.incoming.try_recv() {
            match ev {
                Incoming::ShowPalette => {
                    log::info!("二次启动唤起:显示悬浮条");
                    self.palette.reset(); // 每次唤起都是全新查询(product §1)
                    self.palette_visible = true;
                    ctx.request_repaint();
                }
            }
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let now = Instant::now();

        // 二次启动唤起:每帧收取 app.sock 事件(投递方已 request_repaint 唤醒)
        self.drain_incoming(ctx);

        // 平台补丁每次 logic 核对(与 PoC-1 续「每帧核对、漂移即重施」同形态;
        // 本任务为空壳,Task 14 填实现)。
        if let Err(err) = platform::patch_palette_window(frame) {
            log::warn!("悬浮条平台补丁失败: {err}");
        }

        // 外观跟随(自建探测,tech 定案):低频节流查系统外观,变化即应用主题。
        // 两视口共享同一 ctx 的全局 Options(Ui 每 pass 按当前主题重建),主题自动一致。
        if self.theme_throttle.should_check(now) {
            let detected = theme::current_theme();
            if self.theme != Some(detected) {
                self.theme = Some(detected);
                theme::apply(ctx, detected);
                log::info!("系统外观:{detected:?} → 应用对应主题");
            }
        }
        // 空闲(无输入/无动画)不排帧 → 系统外观变化会被拖到下一次输入才可见;
        // 显式排 ~2s 后的帧,跟随延迟上限即节流窗(空闲 CPU 代价:约每 2 秒一帧)。
        ctx.request_repaint_after(theme::CHECK_INTERVAL);

        // 查询会话驱动:防抖到期发查询线程 + 收结果(UI 线程不做阻塞查询)
        self.palette.tick(now);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 运行期字体回退留痕一次(诊断:实际命中的系统 CJK 字体;
        // 延迟到 ui() 因 ctx.fonts() 在首个 pass 前不可用)。
        if !self.fonts_logged {
            let names: Vec<String> = ui.ctx().fonts(|f| {
                f.discovered_fonts()
                    .iter()
                    .map(|i| i.name.clone())
                    .collect()
            });
            if !names.is_empty() {
                log::info!("系统字体回退命中:{names:?}");
                self.fonts_logged = true;
            }
        }

        // 主视口:完整窗口占位(Task 17 填表格)。
        ui.heading("macEverything");
        ui.label("完整结果窗口(占位——Task 17)");
        ui.separator();
        if ui.button("显示/隐藏悬浮条").clicked() {
            self.palette_visible = !self.palette_visible;
            if self.palette_visible {
                self.palette.reset();
            }
        }

        // 悬浮条第二视口:输入即搜/结果列表/键位(Task 12)。
        // 只在真实 pass 里显示(视口渲染须由父 pass 驱动);隐藏路径
        // (ViewportCommand / 原生 orderOut)Task 14 接。
        if self.palette_visible {
            let ctx = ui.ctx().clone();
            let mut action = None;
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("palette"),
                egui::ViewportBuilder::default()
                    .with_title("palette")
                    .with_inner_size([palette::PANEL_W, palette::INITIAL_H])
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_always_on_top(),
                |ui, _class| {
                    action = self.palette.draw(ui);
                },
            );
            match action {
                Some(palette::Action::Hide) => {
                    self.palette_visible = false;
                    self.palette.reset(); // 打开/隐藏后清空输入(product §1)
                }
                Some(palette::Action::FullWindow) => {
                    // Task 17 消费:打开完整窗口并带词;此处留痕
                    if let Some(q) = self.palette.full_window_request.as_ref() {
                        log::info!("⌥↵:打开完整窗口并带查询词 {q:?}(Task 17 消费)");
                    }
                    self.palette_visible = false;
                    self.palette.reset();
                }
                None => {}
            }
        }
    }
}

//! UI 层:双视口壳。
//!
//! 主视口(ROOT)= 完整结果窗口占位(Task 17 填结果表格);
//! 第二视口 = 悬浮条(Task 12:输入即搜/结果列表/键位;平台行为 Task 14 套)。

pub mod palette;

use eframe::egui;
use std::sync::mpsc::Receiver;

use crate::platform;
use crate::single_instance::Incoming;

pub struct App {
    /// 悬浮条是否显示(隐藏/唤起的原生路径 Task 14 接;此处先视口级开关)。
    palette_visible: bool,
    /// 二次启动唤起通道(app.sock 监听线程 → 主线程;main 接线)。
    incoming: Receiver<Incoming>,
    /// 悬浮条状态与查询会话(Task 12)。
    palette: palette::PaletteState,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        incoming: Receiver<Incoming>,
        paths: client::DaemonPaths,
    ) -> Self {
        Self {
            palette_visible: true,
            incoming,
            palette: palette::PaletteState::new(cc, paths),
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
        // 二次启动唤起:每帧收取 app.sock 事件(投递方已 request_repaint 唤醒)
        self.drain_incoming(ctx);

        // 平台补丁每次 logic 核对(与 PoC-1 续「每帧核对、漂移即重施」同形态;
        // 本任务为空壳,Task 14 填实现)。
        if let Err(err) = platform::patch_palette_window(frame) {
            log::warn!("悬浮条平台补丁失败: {err}");
        }

        // 查询会话驱动:防抖到期发查询线程 + 收结果(UI 线程不做阻塞查询)
        self.palette.tick(std::time::Instant::now());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
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

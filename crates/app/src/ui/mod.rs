//! UI 层:双视口壳。
//!
//! 主视口(ROOT)= 完整结果窗口占位(Task 17 填结果表格);
//! 第二视口 = 悬浮条占位(Task 12 填 session 搜索,Task 14 套平台行为)。

use eframe::egui;

use crate::platform;

pub struct App {
    /// 悬浮条是否显示(Task 12 接 session 后接管)。
    palette_visible: bool,
    /// 悬浮条输入框内容(Task 12 接管)。
    palette_input: String,
}

impl App {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            palette_visible: true,
            palette_input: String::new(),
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // 平台补丁每次 logic 核对(与 PoC-1 续「每帧核对、漂移即重施」同形态;
        // 本任务为空壳,Task 14 填实现)。
        if let Err(err) = platform::patch_palette_window(frame) {
            log::warn!("悬浮条平台补丁失败: {err}");
        }

        // 悬浮条第二视口:本任务先常规显示(无边框/透明/置顶已设),
        // 平台行为(改类 NSPanel 等)Task 14 接。
        if self.palette_visible {
            ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("palette"),
                egui::ViewportBuilder::default()
                    .with_title("palette")
                    .with_inner_size([600.0, 56.0])
                    .with_decorations(false)
                    .with_transparent(true)
                    .with_always_on_top(),
                |ui, _class| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.text_edit_singleline(&mut self.palette_input);
                    });
                    if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
                        self.palette_visible = false;
                    }
                },
            );
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 主视口:完整窗口占位(Task 17 填表格)。
        ui.heading("macEverything");
        ui.label("完整结果窗口(占位——Task 17)");
        ui.separator();
        if ui.button("显示/隐藏悬浮条占位").clicked() {
            self.palette_visible = !self.palette_visible;
        }
    }
}

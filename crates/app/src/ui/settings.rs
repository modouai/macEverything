//! 设置窗口（v2.1）：620×600 固定、原生交通灯 + 自绘 44 标题栏。
//!
//! 第三个 deferred 视口（与悬浮条同模式：隐藏走 `Visible(false)`，窗口保留不销毁）。
//! Task 1 只交付骨架（底色 + 标题 + 内描边）；控件由后续任务接入。

use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::ui::{lock_shared, Shared};

/// 设置窗口宽（settings-visual.md §2；固定不可缩放）。
pub const SETTINGS_W: f32 = 620.0;
/// 设置窗口高。
pub const SETTINGS_H: f32 = 600.0;
/// 自绘标题栏高（原生交通灯浮在内容之上，标题居中）。
pub const TITLEBAR_H: f32 = 44.0;

/// 设置视口 id（注册与命令定向同一处，勿散落）。
pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("settings")
}

/// 设置视口 builder（常量；每 pass 传同一份——`patch` 无差异则无命令/无重建）。
///
/// min/max 同时锁死尺寸（`resizable(false)` 之外的双保险：builder 尺寸即规格）。
/// `visible(false)`：启动隐藏；打开走 `open_settings` 的 `Visible(true)` + `Focus`。
/// 标题与标题栏关闭 + 全尺寸内容视图 = 原生交通灯浮在自绘 44px 标题栏上。
pub fn builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("设置")
        .with_inner_size([SETTINGS_W, SETTINGS_H])
        .with_min_inner_size([SETTINGS_W, SETTINGS_H])
        .with_max_inner_size([SETTINGS_W, SETTINGS_H])
        .with_resizable(false)
        .with_title_shown(false)
        .with_titlebar_shown(false)
        .with_fullsize_content_view(true)
        .with_has_shadow(true)
        .with_visible(false)
}

/// 设置窗口状态（Task 1 只登记开合；Task 2+ 接入持久化与控件）。
#[derive(Default)]
pub struct SettingsState {
    pub open: bool,
}

/// 设置视口 pass（deferred 回调）。
pub fn draw(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();
    let mut s = lock_shared(shared);

    // 关闭=隐藏（子视口须自行消费 close_requested；红点不退出 App）。
    if ctx.input(|i| i.viewport().close_requested()) {
        s.settings.open = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        log::info!("设置窗口关闭请求:隐藏窗口(open=false)");
    }

    // 整窗底色（标题栏与内容同底；原生交通灯浮于其上）。
    let palette = crate::ui::window_tokens::palette(ui.visuals().dark_mode);
    ui.painter()
        .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, palette.window_fill);

    // 44px 标题栏：原生交通灯在左，标题居中（13pt；Task 3 接全局字号后改 font::id）。
    let bar = egui::Rect::from_min_size(
        ui.max_rect().min,
        egui::vec2(ui.max_rect().width(), TITLEBAR_H),
    );
    ui.painter().text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        "设置",
        egui::FontId::proportional(13.0),
        palette.file_name,
    );

    // 窗口内 1px 描边（原生窗口负责圆角裁剪与投影；内容层按同半径内侧补线）。
    ui.painter().rect_stroke(
        ui.max_rect(),
        egui::CornerRadius::same(crate::ui::window_tokens::WINDOW_RADIUS),
        egui::Stroke::new(
            crate::ui::window_tokens::WINDOW_STROKE,
            palette.window_stroke,
        ),
        egui::StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_size_matches_spec() {
        assert_eq!((SETTINGS_W, SETTINGS_H), (620.0, 600.0));
        assert_eq!(TITLEBAR_H, 44.0);
    }
}

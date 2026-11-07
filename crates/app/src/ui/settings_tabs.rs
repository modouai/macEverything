//! 设置页顶部标签栏：枚举、几何、命中与自绘图标。
//!
//! 只负责标签栏本身；页面绘制在 `settings_view`，状态编排在 `settings`。
//!
//! Task 4 完成页面分发接线前，本模块的绘制/几何自由函数尚无生产消费方：
//! 它们身上的 `#[allow(dead_code)]` 是临时接线标记，必须在接线后删除；后续
//! 新增 allow 也须窄作用域，不得恢复整模块 allow（`settings_tokens` 已按此收敛）。

use eframe::egui;

use crate::i18n;
use crate::ui::font as ui_font;
use crate::ui::settings_tokens::{
    SettingsPalette, TABBAR_H, TAB_FONT, TAB_GAP, TAB_ICON, TAB_ITEM_H, TAB_ITEM_W, TAB_RADIUS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Index,
    Advanced,
    Experimental,
    About,
}

impl SettingsTab {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Index,
        Self::Advanced,
        Self::Experimental,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => i18n::s().settings_tab_general,
            Self::Index => i18n::s().settings_tab_index,
            Self::Advanced => i18n::s().settings_tab_advanced,
            Self::Experimental => i18n::s().settings_tab_experimental,
            Self::About => i18n::s().settings_tab_about,
        }
    }

    #[allow(dead_code)] // Task 4 接线后删除。
    pub fn scroll_id(self) -> &'static str {
        match self {
            Self::General => "settings-page-general",
            Self::Index => "settings-page-index",
            Self::Advanced => "settings-page-advanced",
            Self::Experimental => "settings-page-experimental",
            Self::About => "settings-page-about",
        }
    }
}

/// 5 个标签项的矩形（相对标签栏居中）。
#[allow(dead_code)] // Task 4 接线后删除。
pub fn tab_item_rects(bar: egui::Rect) -> [egui::Rect; 5] {
    let total_w = TAB_ITEM_W * 5.0 + TAB_GAP * 4.0;
    let start_x = bar.center().x - total_w / 2.0;
    let top = bar.center().y - TAB_ITEM_H / 2.0;
    std::array::from_fn(|i| {
        egui::Rect::from_min_size(
            egui::pos2(start_x + i as f32 * (TAB_ITEM_W + TAB_GAP), top),
            egui::vec2(TAB_ITEM_W, TAB_ITEM_H),
        )
    })
}

/// 命中测试：坐标落在哪个标签项内（标签栏外返回 `None`）。
#[allow(dead_code)] // Task 4 只接线 `draw_tab_bar`；本函数收口时仍无生产消费方，
                    // 必须改为消费或删除并移除此 allow。
pub fn hit_test(bar: egui::Rect, pos: egui::Pos2) -> Option<SettingsTab> {
    tab_item_rects(bar)
        .iter()
        .position(|rect| rect.contains(pos))
        .map(|i| SettingsTab::ALL[i])
}

/// 绘制顶部标签栏；返回本帧被点击的标签（无点击为 `None`）。
#[allow(dead_code)] // Task 4 接线后删除。
pub fn draw_tab_bar(
    ui: &mut egui::Ui,
    active: SettingsTab,
    palette: &SettingsPalette,
) -> Option<SettingsTab> {
    let width = ui.available_width();
    let (bar, _) = ui.allocate_exact_size(egui::vec2(width, TABBAR_H), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(bar, egui::CornerRadius::ZERO, palette.window_fill);
    painter.hline(
        bar.x_range(),
        bar.bottom() - 0.5,
        egui::Stroke::new(1.0, palette.divider),
    );

    let mut clicked = None;
    for (i, tab) in SettingsTab::ALL.iter().enumerate() {
        let rect = tab_item_rects(bar)[i];
        let response = ui.interact(
            rect,
            ui.scope_id().with(("settings-tab", i)),
            egui::Sense::click(),
        );
        let selected = *tab == active;
        let hovered = response.hovered();
        if selected {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(TAB_RADIUS),
                palette.tab_selected_fill,
            );
        } else if hovered {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(TAB_RADIUS),
                palette.control_fill,
            );
        }
        let color = if selected {
            palette.switch_on
        } else {
            palette.text_secondary
        };
        let icon_center = egui::pos2(rect.center().x, rect.top() + 8.0 + TAB_ICON / 2.0);
        draw_tab_icon(&painter, *tab, icon_center, TAB_ICON, color);
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 12.0),
            egui::Align2::CENTER_CENTER,
            tab.label(),
            ui_font::id(TAB_FONT),
            color,
        );
        if response.clicked() {
            clicked = Some(*tab);
        }
        let _ = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    clicked
}

/// 自绘标签图标：24×24 设计栅格，按 `size` 等比缩放并以 `center` 居中。
fn draw_tab_icon(
    painter: &egui::Painter,
    tab: SettingsTab,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.7, color);
    let scale = size / 24.0;
    let p =
        |x: f32, y: f32| egui::pos2(center.x + (x - 12.0) * scale, center.y + (y - 12.0) * scale);
    match tab {
        SettingsTab::General => {
            for (y, knob_x) in [(7.0, 8.0), (12.0, 15.0), (17.0, 10.0)] {
                painter.line_segment([p(4.0, y), p(20.0, y)], stroke);
                painter.circle_filled(p(knob_x, y), 1.6 * scale, color);
            }
        }
        SettingsTab::Index => {
            painter.add(egui::Shape::line(
                vec![
                    p(3.0, 6.5),
                    p(9.0, 6.5),
                    p(11.0, 8.5),
                    p(20.0, 8.5),
                    p(20.0, 18.0),
                    p(3.0, 18.0),
                    p(3.0, 6.5),
                ],
                stroke,
            ));
            painter.circle_stroke(p(15.3, 13.3), 2.8 * scale, stroke);
            painter.line_segment([p(17.3, 15.3), p(19.5, 17.5)], stroke);
        }
        SettingsTab::Advanced => {
            let rect =
                egui::Rect::from_min_size(p(3.0, 4.5), egui::vec2(18.0 * scale, 15.0 * scale));
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same((2.5 * scale) as u8),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.add(egui::Shape::line(
                vec![p(7.0, 9.0), p(10.0, 12.0), p(7.0, 15.0)],
                stroke,
            ));
            painter.line_segment([p(12.5, 15.0), p(17.0, 15.0)], stroke);
        }
        SettingsTab::Experimental => {
            painter.line_segment([p(9.5, 3.0), p(14.5, 3.0)], stroke);
            painter.add(egui::Shape::line(
                vec![
                    p(10.5, 3.0),
                    p(10.5, 9.2),
                    p(5.4, 18.0),
                    p(5.8, 20.0),
                    p(7.5, 21.0),
                    p(16.5, 21.0),
                    p(18.2, 20.0),
                    p(18.6, 18.0),
                    p(13.5, 9.2),
                    p(13.5, 3.0),
                ],
                stroke,
            ));
            painter.line_segment([p(7.8, 15.0), p(16.2, 15.0)], stroke);
        }
        SettingsTab::About => {
            painter.circle_stroke(center, 9.0 * scale, stroke);
            painter.line_segment([p(12.0, 10.6), p(12.0, 17.0)], stroke);
            painter.circle_filled(p(12.0, 7.5), 0.9 * scale, color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;
    use crate::ui::settings_tokens::{TABBAR_H, TAB_GAP, TAB_ITEM_H, TAB_ITEM_W};

    #[test]
    fn item_rects_are_centered_and_evenly_spaced() {
        let bar = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(620.0, TABBAR_H));
        let rects = tab_item_rects(bar);
        assert_eq!(rects.len(), 5);
        let total = TAB_ITEM_W * 5.0 + TAB_GAP * 4.0;
        let expected_left = bar.center().x - total / 2.0;
        assert!((rects[0].left() - expected_left).abs() < 0.01);
        let expected_top = bar.center().y - TAB_ITEM_H / 2.0;
        for rect in &rects {
            assert!((rect.top() - expected_top).abs() < 0.01);
            assert!((rect.width() - TAB_ITEM_W).abs() < 0.01);
            assert!((rect.height() - TAB_ITEM_H).abs() < 0.01);
        }
        for pair in rects.windows(2) {
            assert!((pair[1].left() - pair[0].right() - TAB_GAP).abs() < 0.01);
        }
    }

    #[test]
    fn hit_test_maps_points_to_tabs() {
        let bar = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(620.0, TABBAR_H));
        let rects = tab_item_rects(bar);
        for (i, rect) in rects.iter().enumerate() {
            assert_eq!(hit_test(bar, rect.center()), Some(SettingsTab::ALL[i]));
        }
        assert_eq!(hit_test(bar, egui::pos2(2.0, bar.center().y)), None);
    }

    #[test]
    fn tab_bar_renders_five_labels_and_selected_background() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let mut clicked = None;
        let palette = crate::ui::settings_tokens::light();
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(620.0, TABBAR_H),
                )),
                ..Default::default()
            },
            |ui| {
                clicked = draw_tab_bar(ui, SettingsTab::Index, &palette);
            },
        );
        // headless：不做纹理上传，先清增量免得断言失败时 Drop debug_assert 二次 panic。
        out.textures_delta.clear();
        let labels: Vec<String> = out
            .shapes
            .iter()
            .flat_map(|clipped| text_shapes(&clipped.shape))
            .map(|text| text.galley.text().to_owned())
            .collect();
        for tab in SettingsTab::ALL {
            assert!(
                labels.iter().any(|label| label == tab.label()),
                "缺少标签 {}",
                tab.label()
            );
        }
        // 选中项底：Index 的浅蓝底必须精确落在 Index 的标签项矩形上
        // （防选中态漏画或画错项）。
        let bar = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(620.0, TABBAR_H));
        let active_rect = tab_item_rects(bar)[SettingsTab::Index as usize];
        let selected = out
            .shapes
            .iter()
            .flat_map(|clipped| rect_shapes(&clipped.shape))
            .find(|rect| rect.fill == palette.tab_selected_fill);
        let selected = selected.expect("缺少选中标签背景");
        assert_eq!(selected.rect, active_rect);
        assert_eq!(
            (selected.rect.width(), selected.rect.height()),
            (TAB_ITEM_W, TAB_ITEM_H)
        );
        assert!(clicked.is_none(), "无点击不应返回标签");
    }

    fn text_shapes(shape: &egui::Shape) -> Vec<egui::epaint::TextShape> {
        match shape {
            egui::Shape::Text(text) => vec![text.clone()],
            egui::Shape::Vec(shapes) => shapes.iter().flat_map(text_shapes).collect(),
            _ => Vec::new(),
        }
    }

    fn rect_shapes(shape: &egui::Shape) -> Vec<egui::epaint::RectShape> {
        match shape {
            egui::Shape::Rect(rect) => vec![rect.clone()],
            egui::Shape::Vec(shapes) => shapes.iter().flat_map(rect_shapes).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn order_is_general_index_advanced_experimental_about() {
        assert_eq!(
            SettingsTab::ALL,
            [
                SettingsTab::General,
                SettingsTab::Index,
                SettingsTab::Advanced,
                SettingsTab::Experimental,
                SettingsTab::About,
            ]
        );
    }

    #[test]
    fn default_is_general() {
        assert_eq!(SettingsTab::default(), SettingsTab::General);
    }

    #[test]
    fn labels_cover_both_languages() {
        for (lang, expected) in [
            (Lang::ZhHans, ["通用", "索引", "高级", "实验", "关于"]),
            (
                Lang::En,
                ["General", "Index", "Advanced", "Experimental", "About"],
            ),
        ] {
            crate::i18n::with_lang(lang, || {
                let labels: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.label()).collect();
                assert_eq!(labels, expected);
            });
        }
    }

    #[test]
    fn scroll_ids_are_unique() {
        let ids: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.scroll_id()).collect();
        assert!(ids.iter().all(|id| id.starts_with("settings-page-")));
        let mut dedup = ids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len());
    }
}

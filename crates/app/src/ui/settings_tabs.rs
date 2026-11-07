//! 设置页顶部标签栏：枚举、几何、命中与自绘图标。
//!
//! 只负责标签栏本身；页面绘制在 `settings_view`，状态编排在 `settings`。
//! 绘制/几何函数由 `settings_view::draw_body` 与命中测试消费。

use eframe::egui;

use crate::i18n;
use crate::ui::font as ui_font;
use crate::ui::settings_tokens::{
    SettingsPalette, TABBAR_H, TAB_FONT, TAB_GAP, TAB_ICON, TAB_ICON_TOP_GAP, TAB_ITEM_H,
    TAB_ITEM_W, TAB_LABEL_BOTTOM_GAP, TAB_RADIUS,
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
pub fn hit_test(bar: egui::Rect, pos: egui::Pos2) -> Option<SettingsTab> {
    tab_item_rects(bar)
        .iter()
        .position(|rect| rect.contains(pos))
        .map(|i| SettingsTab::ALL[i])
}

/// 绘制顶部标签栏；返回本帧被点击的标签（无点击为 `None`）。
pub fn draw_tab_bar(
    ui: &mut egui::Ui,
    active: SettingsTab,
    palette: &SettingsPalette,
) -> Option<SettingsTab> {
    let width = ui.available_width();
    let (bar, response) = ui.allocate_exact_size(egui::vec2(width, TABBAR_H), egui::Sense::click());
    let painter = ui.painter().clone();
    painter.rect_filled(bar, egui::CornerRadius::ZERO, palette.window_fill);
    painter.hline(
        bar.x_range(),
        bar.bottom() - 0.5,
        egui::Stroke::new(1.0, palette.divider),
    );

    // 命中只有一条路径：整条栏承载交互，悬停/点击再映射到标签项。
    let hovered_tab = response.hover_pos().and_then(|pos| hit_test(bar, pos));
    let clicked = response
        .clicked()
        .then(|| {
            response
                .interact_pointer_pos()
                .and_then(|pos| hit_test(bar, pos))
        })
        .flatten();
    if hovered_tab.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }

    let rects = tab_item_rects(bar);
    for (i, tab) in SettingsTab::ALL.iter().enumerate() {
        let rect = rects[i];
        let selected = *tab == active;
        let hovered = hovered_tab == Some(*tab);
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
        let icon_center = egui::pos2(
            rect.center().x,
            rect.top() + TAB_ICON_TOP_GAP + TAB_ICON / 2.0,
        );
        draw_tab_icon(&painter, *tab, icon_center, TAB_ICON, color);
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - TAB_LABEL_BOTTOM_GAP),
            egui::Align2::CENTER_CENTER,
            tab.label(),
            ui_font::id(TAB_FONT),
            color,
        );
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

    /// 标签栏画布（620×TABBAR_H，左上角为原点，与 run_ui 的 screen_rect 一致）。
    fn bar_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(620.0, TABBAR_H))
    }

    /// 标签项在画布上的矩形（按 `SettingsTab::ALL` 的顺序定位）。
    fn tab_rect(tab: SettingsTab) -> egui::Rect {
        let index = SettingsTab::ALL
            .iter()
            .position(|candidate| *candidate == tab)
            .expect("标签必须在 SettingsTab::ALL 中");
        tab_item_rects(bar_rect())[index]
    }

    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// 在既有 Context 上跑一帧标签栏（跨帧保留交互记忆）。
    fn run_bar(
        ctx: &egui::Context,
        active: SettingsTab,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<SettingsTab>) {
        let palette = crate::ui::settings_tokens::light();
        let mut clicked = None;
        let mut out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(bar_rect()),
                events,
                ..Default::default()
            },
            |ui| {
                clicked = draw_tab_bar(ui, active, &palette);
            },
        );
        // headless：不做纹理上传，先清增量免得断言失败时 Drop debug_assert 二次 panic。
        out.textures_delta.clear();
        (out, clicked)
    }

    #[test]
    fn item_rects_are_centered_and_evenly_spaced() {
        let bar = bar_rect();
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
        let bar = bar_rect();
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
        let (out, clicked) = run_bar(&ctx, SettingsTab::Index, Vec::new());
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
        let palette = crate::ui::settings_tokens::light();
        let active_rect = tab_rect(SettingsTab::Index);
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

    /// 点击映射到被点中的标签（整条栏单一命中路径）。
    #[test]
    fn click_returns_hit_tab() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let pos = tab_rect(SettingsTab::Index).center();
        // 首帧注册 widget 交互记忆；按下、释放分帧完成一次点击。
        let _ = run_bar(&ctx, SettingsTab::General, Vec::new());
        let _ = run_bar(
            &ctx,
            SettingsTab::General,
            vec![egui::Event::PointerMoved(pos), pointer_button(pos, true)],
        );
        let (_, clicked) = run_bar(
            &ctx,
            SettingsTab::General,
            vec![egui::Event::PointerMoved(pos), pointer_button(pos, false)],
        );
        assert_eq!(clicked, Some(SettingsTab::Index));
    }

    /// 悬停未选中项画控件底；选中项优先于悬停。
    #[test]
    fn hovered_tab_gets_control_fill() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let target = SettingsTab::Index;
        let pos = tab_rect(target).center();
        let _ = run_bar(&ctx, SettingsTab::General, Vec::new());
        let (out, clicked) = run_bar(
            &ctx,
            SettingsTab::General,
            vec![egui::Event::PointerMoved(pos)],
        );
        assert!(clicked.is_none(), "仅悬停不应返回标签");
        let palette = crate::ui::settings_tokens::light();
        let hovered = out
            .shapes
            .iter()
            .flat_map(|clipped| rect_shapes(&clipped.shape))
            .any(|rect| rect.fill == palette.control_fill && rect.rect == tab_rect(target));
        assert!(hovered, "悬停标签必须画 control_fill 底");

        // 选中项优先于悬停：悬停当前选中的标签时不得再画 control_fill。
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let _ = run_bar(&ctx, target, Vec::new());
        let (out, _) = run_bar(&ctx, target, vec![egui::Event::PointerMoved(pos)]);
        let selected_hovered = out
            .shapes
            .iter()
            .flat_map(|clipped| rect_shapes(&clipped.shape))
            .any(|rect| rect.fill == palette.control_fill && rect.rect == tab_rect(target));
        assert!(!selected_hovered, "选中项不得被悬停底覆盖");
    }

    /// 每个选中标签都要经 `draw_tab_bar` 画出选中色图标（忽略文字形状）。
    #[test]
    fn each_selected_tab_draws_icon_in_selected_color() {
        let palette = crate::ui::settings_tokens::light();
        for active in SettingsTab::ALL {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::light());
            let (out, _) = run_bar(&ctx, active, Vec::new());
            let has_icon = out
                .shapes
                .iter()
                .any(|clipped| uses_color(&clipped.shape, palette.switch_on));
            assert!(has_icon, "标签 {} 缺少选中色图标", active.label());
        }
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

    /// 该形状（递归，排除文字）是否用 `color` 绘制。
    fn uses_color(shape: &egui::Shape, color: egui::Color32) -> bool {
        match shape {
            egui::Shape::LineSegment { stroke, .. } => stroke.color == color,
            egui::Shape::Circle(circle) => circle.fill == color || circle.stroke.color == color,
            egui::Shape::Path(path) => {
                path.fill == color
                    || matches!(
                        path.stroke.color,
                        egui::epaint::ColorMode::Solid(stroke_color) if stroke_color == color
                    )
            }
            egui::Shape::Rect(rect) => rect.fill == color || rect.stroke.color == color,
            egui::Shape::Vec(shapes) => shapes.iter().any(|shape| uses_color(shape, color)),
            _ => false,
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

    /// 标签文案在中英双语下都不得超出标签项可用宽度（扣左右内边距）。
    /// 默认测试字体可能缺 CJK 字形，中文宽度是 tofu 近似；真实设备由 SF Pro 保证。
    #[test]
    fn labels_fit_item_width_in_both_languages() {
        let ctx = egui::Context::default();
        for lang in [Lang::ZhHans, Lang::En] {
            crate::i18n::with_lang(lang, || {
                let mut out = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(620.0, TABBAR_H),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        for tab in SettingsTab::ALL {
                            let width = ui
                                .painter()
                                .layout_no_wrap(
                                    tab.label().to_owned(),
                                    crate::ui::font::id(TAB_FONT),
                                    egui::Color32::PLACEHOLDER,
                                )
                                .size()
                                .x;
                            assert!(
                                width <= TAB_ITEM_W - 2.0 * crate::ui::settings_tokens::TAB_PAD_X,
                                "{:?} 标签 {:?} 超宽: {width}",
                                lang,
                                tab.label()
                            );
                        }
                    },
                );
                // headless：不做纹理上传，先清增量免得 Drop debug_assert 二次 panic。
                out.textures_delta.clear();
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

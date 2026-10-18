//! 设置窗口（v2.1）：620×600 固定、原生交通灯 + 自绘 44 标题栏。
//!
//! 第三个 deferred 视口（与悬浮条同模式：隐藏走 `Visible(false)`，窗口保留不销毁）。
//! Task 4 交付静态完整布局：三组（外观 / 通用 / 索引）与五项控件，标题栏固定、
//! 内容区可纵向滚动。控件只画 hover/pressed 视觉，不执行动作（Task 5/6/8 接入）。
//! 窗口交互：标题栏可拖拽（左侧预留原生交通灯）、红点/⌘W = 隐藏（不退出 App）。

use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::platform;
use crate::settings::FontSize;
use crate::ui::{
    font as ui_font, lock_shared,
    settings_tokens::{
        self, SettingsPalette, ADD_BUTTON_FONT, ADD_BUTTON_H, ADD_BUTTON_PAD_X, ADD_BUTTON_RADIUS,
        CARD_RADIUS, CONTENT_PAD, DIVIDER_H, EXCLUDE_HEADER_H, EXCLUDE_ROW_H, FIELD, FIELD_CHEVRON,
        FIELD_FONT, FIELD_PAD_X, FIELD_RADIUS, FOLDER_ICON, FOLDER_ICON_GAP, GROUP_GAP,
        GROUP_HEADER_FONT, GROUP_HEADER_GAP, ICON_STROKE, KEYCAP_FONT, KEYCAP_GAP, KEYCAP_H,
        KEYCAP_PAD_X, KEYCAP_RADIUS, LABEL_CONTROL_GAP, LABEL_FONT, REMOVE_ICON,
        REMOVE_ICON_STROKE, ROW_H, ROW_PAD_X, SEG_FONT, SEG_GAP, SEG_H, SEG_ITEM_RADIUS, SEG_PAD,
        SEG_RADIUS, SEG_W, SETTINGS_H, SETTINGS_W, SWITCH_KNOB, SWITCH_KNOB_SHADOW_OFFSET,
        SWITCH_KNOB_SHADOW_SIZE, SWITCH_RADIUS, SWITCH_SIZE, TITLEBAR_H, TRAFFIC_LIGHTS_RESERVE,
        WINDOW_RADIUS, WINDOW_STROKE,
    },
    Shared,
};

/// 添加目录按钮文案（宽度测量与绘制共用）。
const ADD_DIR_LABEL: &str = "＋ 添加目录…";
/// 语言禁用态提示（Task 5 前仅简中）。
const LANGUAGE_TOOLTIP: &str = "暂仅支持简体中文，语言切换将在后续版本提供";

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
        .with_title(platform::SETTINGS_TITLE)
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

/// 设置窗口状态：开合 + 启动读取的设置（Task 3）。
///
/// `file` 为 `settings.toml` 的已加载快照；Task 4 读取字号绘制分段选中态，
/// 写入与落盘由 Task 5 接入。
#[derive(Default)]
pub struct SettingsState {
    pub open: bool,
    /// 已加载的设置快照（Task 4 读取字号；Task 5 起写入）。
    pub file: crate::settings::SettingsFile,
}

/// 设置页静态视图数据（与 `Shared` 解耦；headless 测试可独立构造）。
struct SettingsData {
    /// 字号档（分段控件选中项）。
    font_size: FontSize,
    /// 开机自启开关态（Task 5 前占位 false）。
    autostart: bool,
    /// 语言（Task 5 前仅简中，禁用态）。
    language: &'static str,
    /// 唤起快捷键键帽（Task 6 前为内置默认）。
    palette_keys: Vec<String>,
    /// 完整窗口快捷键键帽（Task 6 前为内置默认）。
    window_keys: Vec<String>,
    /// 排除目录（真实引擎默认；Task 8 换 daemon 列表）。
    excluded: Vec<String>,
}

impl SettingsData {
    /// 本任务静态数据：字号来自设置快照；其余为后续任务接入前的占位真源
    /// （自启 false、语言简中、快捷键 = 当前内置默认；排除目录 = 引擎默认）。
    fn static_from(font_size: FontSize) -> Self {
        Self {
            font_size,
            autostart: false,
            language: "简体中文",
            palette_keys: vec!["⌥".to_owned(), "Space".to_owned()],
            window_keys: vec!["⌥".to_owned(), "⇧".to_owned(), "F".to_owned()],
            excluded: engine::config::default_config().excluded,
        }
    }
}

/// 设置视口 pass（deferred 回调）。
pub fn draw(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();

    // 关闭=隐藏：红点（close_requested）与 ⌘W 同一路径。子视口须自行消费关闭请求
    //（根视口的 ⌘W 处理器不会为它运行）；隐藏不销毁视口，也不退出 App。
    let close_requested = ctx.input(|i| i.viewport().close_requested());
    let cmd_w = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W));
    if close_requested || cmd_w {
        hide(shared, &ctx);
    }

    // 整窗底色（标题栏与内容同底；原生交通灯浮于其上）。
    let palette = settings_tokens::palette(ui.visuals().dark_mode);
    ui.painter()
        .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, palette.window_fill);

    // 44px 标题栏：原生交通灯在左，标题居中（13pt 基准 × 全局字号倍率）。
    let bar = egui::Rect::from_min_size(
        ui.max_rect().min,
        egui::vec2(ui.max_rect().width(), TITLEBAR_H),
    );

    // 拖拽区 = 交通灯预留区之后到标题栏右缘（与结果窗 draw_titlebar 同法；
    // 左侧原生交通灯自带命中，不接管其附近）。
    let drag_rect = egui::Rect::from_min_max(
        egui::pos2(bar.left() + TRAFFIC_LIGHTS_RESERVE, bar.top()),
        bar.max,
    );
    let drag = ui.interact(
        drag_rect,
        ui.scope_id().with("settings-titlebar-drag"),
        egui::Sense::click_and_drag(),
    );
    if drag.drag_started() {
        log::info!("设置窗口标题栏拖拽:StartDrag");
        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    ui.painter().text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        "设置",
        ui_font::id(settings_tokens::TITLE_FONT),
        palette.text_primary,
    );

    // 内容在共享锁外绘制：只短暂读取字号快照，其余静态数据就地构造。
    let font_size = lock_shared(shared).settings.file.font_size;
    let data = SettingsData::static_from(font_size);
    draw_body(ui, &data);

    // 窗口内 1px 描边（原生窗口负责圆角裁剪与投影；内容层按同半径内侧补线）。
    ui.painter().rect_stroke(
        ui.max_rect(),
        egui::CornerRadius::same(WINDOW_RADIUS),
        egui::Stroke::new(WINDOW_STROKE, palette.window_stroke),
        egui::StrokeKind::Inside,
    );
}

/// 绘制标题栏以下的内容（纯绘制：数据来自 [`SettingsData`]，不读 `Shared`）。
///
/// 标题栏固定；内容区自带 (20,24) 内边距，超出可视高时整页纵向滚动。
fn draw_body(ui: &mut egui::Ui, data: &SettingsData) {
    let window = ui.max_rect();
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(window.left(), window.top() + TITLEBAR_H),
        window.max,
    );
    let palette = settings_tokens::palette(ui.visuals().dark_mode);
    let mut body = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    body.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
    // 浮动滚动条不占内容宽（内容宽恒为窗宽，卡片对齐不随滚动条出现而跳动）。
    body.spacing_mut().scroll.floating = true;

    egui::ScrollArea::vertical()
        .id_salt("settings-body")
        .auto_shrink([false, false])
        .show(&mut body, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let full_w = ui.available_width();
            ui.add_space(CONTENT_PAD.0);
            draw_appearance_group(ui, full_w, data, &palette);
            ui.add_space(GROUP_GAP);
            draw_general_group(ui, full_w, data, &palette);
            ui.add_space(GROUP_GAP);
            draw_index_group(ui, full_w, data, &palette);
            ui.add_space(CONTENT_PAD.0);
        });
}

/// 外观组：分组标题 + 单行卡片（字号分段）。
fn draw_appearance_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
) {
    let card = draw_group_frame(ui, full_w, "外观", &[ROW_H], palette);
    let mut y = card.top();
    let row = take_row(card, &mut y, ROW_H);
    let control = draw_row_label(ui, row, "字号", egui::vec2(SEG_W, SEG_H), palette);
    draw_segmented(ui, control, data.font_size, palette);
}

/// 通用组：四行卡片（自启 / 语言 / 两个快捷键）。
fn draw_general_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
) {
    let card = draw_group_frame(ui, full_w, "通用", &[ROW_H; 4], palette);
    let controls = [
        ("开机自启", RowControl::Switch(data.autostart)),
        ("语言", RowControl::Language(data.language)),
        (
            "唤起快捷键",
            RowControl::Keycaps(data.palette_keys.as_slice()),
        ),
        (
            "完整窗口快捷键",
            RowControl::Keycaps(data.window_keys.as_slice()),
        ),
    ];
    let count = controls.len();
    let mut y = card.top();
    for (index, (label, control)) in controls.into_iter().enumerate() {
        let row = take_row(card, &mut y, ROW_H);
        let rect = draw_row_label(ui, row, label, control.size(), palette);
        match control {
            RowControl::Switch(on) => draw_switch(ui, rect, on, palette),
            RowControl::Language(language) => draw_language_field(ui, rect, language, palette),
            RowControl::Keycaps(keys) => draw_keycap_field(ui, rect, keys, index, palette),
        }
        if index + 1 < count {
            draw_row_divider(ui.painter(), card, &mut y, palette);
        }
    }
}

/// 索引组：Header 52（排除目录 + 添加按钮）+ 目录行 40。
fn draw_index_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
) {
    let mut heights = vec![EXCLUDE_HEADER_H];
    heights.resize(1 + data.excluded.len(), EXCLUDE_ROW_H);
    let card = draw_group_frame(ui, full_w, "索引", &heights, palette);
    let mut y = card.top();

    let header = take_row(card, &mut y, EXCLUDE_HEADER_H);
    let button_w = text_width(ui, ADD_DIR_LABEL, ADD_BUTTON_FONT) + 2.0 * ADD_BUTTON_PAD_X;
    let button = draw_row_label(
        ui,
        header,
        "排除目录",
        egui::vec2(button_w, ADD_BUTTON_H),
        palette,
    );
    draw_add_button(ui, button, palette);

    for (index, path) in data.excluded.iter().enumerate() {
        draw_row_divider(ui.painter(), card, &mut y, palette);
        let row = take_row(card, &mut y, EXCLUDE_ROW_H);
        draw_dir_row(ui, row, index, path, palette);
    }
}

/// 分组标题（12pt 次要色）+ 8 + 卡片（圆角 10、卡片底、行间 1px 分隔线）。
/// 返回卡片矩形（行内容由调用方按 `row_heights` 继续排布）。
fn draw_group_frame(
    ui: &mut egui::Ui,
    full_w: f32,
    title: &str,
    row_heights: &[f32],
    palette: &SettingsPalette,
) -> egui::Rect {
    let header_h = ui.fonts_mut(|fonts| fonts.row_height(&ui_font::id(GROUP_HEADER_FONT)));
    let (header_slot, _) =
        ui.allocate_exact_size(egui::vec2(full_w, header_h), egui::Sense::hover());
    let inner_w = (full_w - 2.0 * CONTENT_PAD.1).max(0.0);
    let header = egui::Rect::from_min_size(
        egui::pos2(header_slot.left() + CONTENT_PAD.1, header_slot.top()),
        egui::vec2(inner_w, header_h),
    );
    draw_text(
        ui,
        header,
        title,
        GROUP_HEADER_FONT,
        palette.text_secondary,
        egui::Align2::LEFT_CENTER,
    );

    ui.add_space(GROUP_HEADER_GAP);
    let card_h = card_height(row_heights);
    let (card_slot, _) = ui.allocate_exact_size(egui::vec2(full_w, card_h), egui::Sense::hover());
    let card = egui::Rect::from_min_size(
        egui::pos2(card_slot.left() + CONTENT_PAD.1, card_slot.top()),
        egui::vec2(inner_w, card_h),
    );
    ui.painter().rect_filled(
        card,
        egui::CornerRadius::same(CARD_RADIUS),
        palette.card_fill,
    );
    card
}

/// 卡片总高 = 各行高之和 + 行间 1px 分隔线。
fn card_height(row_heights: &[f32]) -> f32 {
    let rows: f32 = row_heights.iter().sum();
    rows + DIVIDER_H * row_heights.len().saturating_sub(1) as f32
}

/// 从卡片游标取一行并推进行高。
fn take_row(card: egui::Rect, y: &mut f32, height: f32) -> egui::Rect {
    let row = egui::Rect::from_min_size(
        egui::pos2(card.left(), *y),
        egui::vec2(card.width(), height),
    );
    *y += height;
    row
}

/// 行间 1px 分隔线（整卡片宽；§2.3）。
fn draw_row_divider(
    painter: &egui::Painter,
    card: egui::Rect,
    y: &mut f32,
    palette: &SettingsPalette,
) {
    painter.rect_filled(
        egui::Rect::from_min_size(
            egui::pos2(card.left(), *y),
            egui::vec2(card.width(), DIVIDER_H),
        ),
        egui::CornerRadius::ZERO,
        palette.divider,
    );
    *y += DIVIDER_H;
}

/// 行标签（13pt 主要色，左内边距 16）+ 右对齐控件几何。
/// 标签宽止于控件左缘 − 16，超出截断（不与控件重叠）。
fn draw_row_label(
    ui: &egui::Ui,
    row: egui::Rect,
    label: &str,
    control_size: egui::Vec2,
    palette: &SettingsPalette,
) -> egui::Rect {
    let control = egui::Rect::from_min_size(
        egui::pos2(
            row.right() - ROW_PAD_X - control_size.x,
            row.center().y - control_size.y / 2.0,
        ),
        control_size,
    );
    let label_rect = egui::Rect::from_min_max(
        egui::pos2(row.left() + ROW_PAD_X, row.top()),
        egui::pos2(control.left() - LABEL_CONTROL_GAP, row.bottom()),
    );
    draw_text(
        ui,
        label_rect,
        label,
        LABEL_FONT,
        palette.text_primary,
        egui::Align2::LEFT_CENTER,
    );
    control
}

/// 通用行控件（Task 4 只画视觉，不回写状态）。
enum RowControl<'a> {
    Switch(bool),
    Language(&'a str),
    Keycaps(&'a [String]),
}

impl RowControl<'_> {
    fn size(&self) -> egui::Vec2 {
        match self {
            Self::Switch(_) => egui::vec2(SWITCH_SIZE.0, SWITCH_SIZE.1),
            Self::Language(_) | Self::Keycaps(_) => egui::vec2(FIELD.0, FIELD.1),
        }
    }
}

/// 字号三段控件（180×28、圆角 7、pad 2、gap 2；段圆角 6、12pt）。
/// 选中段取设置快照字号；hover/pressed 只改视觉，不执行切换（Task 5）。
fn draw_segmented(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: FontSize,
    palette: &SettingsPalette,
) {
    let painter = ui.painter().clone();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(SEG_RADIUS),
        palette.control_fill,
    );
    let inner = rect.shrink(SEG_PAD);
    let options = [FontSize::Small, FontSize::Standard, FontSize::Large];
    let count = options.len() as f32;
    let item_w = ((inner.width() - SEG_GAP * (count - 1.0)) / count).max(0.0);
    for (index, option) in options.into_iter().enumerate() {
        let item = egui::Rect::from_min_size(
            egui::pos2(
                inner.left() + index as f32 * (item_w + SEG_GAP),
                inner.top(),
            ),
            egui::vec2(item_w, inner.height()),
        );
        let response = ui.interact(
            item,
            ui.scope_id().with(("settings-segment", index)),
            egui::Sense::click(),
        );
        let is_selected = option == selected;
        let fill = if is_selected {
            if response.is_pointer_button_down_on() {
                palette.segment_selected_fill.gamma_multiply(0.8)
            } else {
                palette.segment_selected_fill
            }
        } else if response.is_pointer_button_down_on() {
            palette.control_fill.gamma_multiply(2.0)
        } else if response.hovered() {
            palette.control_fill.gamma_multiply(1.6)
        } else {
            egui::Color32::TRANSPARENT
        };
        if fill != egui::Color32::TRANSPARENT {
            painter.rect_filled(item, egui::CornerRadius::same(SEG_ITEM_RADIUS), fill);
        }
        let text_color = if is_selected {
            palette.segment_selected_text
        } else {
            palette.segment_unselected_text
        };
        painter.text(
            item.center(),
            egui::Align2::CENTER_CENTER,
            option.label(),
            ui_font::id(SEG_FONT),
            text_color,
        );
    }
}

/// 开机自启开关（38×22、圆角 11；钮 18 白色 + 1px 柔和投影）。
/// Task 4 数据占位 false（关态），开态绘制路径已备好供 Task 5 使用。
fn draw_switch(ui: &mut egui::Ui, rect: egui::Rect, on: bool, palette: &SettingsPalette) {
    let response = ui.interact(
        rect,
        ui.scope_id().with("settings-autostart"),
        egui::Sense::click(),
    );
    let base = if on {
        palette.switch_on
    } else {
        palette.control_fill
    };
    let track = if response.is_pointer_button_down_on() {
        base.gamma_multiply(0.85)
    } else if response.hovered() {
        base.gamma_multiply(1.15)
    } else {
        base
    };
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(SWITCH_RADIUS), track);

    let inset = (SWITCH_SIZE.1 - SWITCH_KNOB) / 2.0;
    let knob_x = if on {
        rect.right() - inset - SWITCH_KNOB
    } else {
        rect.left() + inset
    };
    let knob_center = egui::pos2(knob_x + SWITCH_KNOB / 2.0, rect.center().y);
    let knob_radius = SWITCH_KNOB / 2.0;
    painter.circle_filled(
        knob_center + egui::vec2(0.0, SWITCH_KNOB_SHADOW_OFFSET),
        knob_radius + SWITCH_KNOB_SHADOW_SIZE,
        palette.switch_knob_shadow,
    );
    painter.circle_filled(knob_center, knob_radius, palette.switch_knob);
}

/// 语言下拉（152×28、圆角 7；禁用态「简体中文」+ chevron + tooltip）。
/// 点击不执行动作（Task 5 接语言切换）。
fn draw_language_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    language: &str,
    palette: &SettingsPalette,
) {
    let response = ui.interact(
        rect,
        ui.scope_id().with("settings-language"),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(FIELD_RADIUS),
        palette.control_fill,
    );
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + FIELD_PAD_X, rect.top()),
        egui::pos2(rect.right() - FIELD_PAD_X - FIELD_CHEVRON, rect.bottom()),
    );
    draw_text(
        ui,
        text_rect,
        language,
        FIELD_FONT,
        palette.control_text,
        egui::Align2::LEFT_CENTER,
    );
    let chevron = egui::Rect::from_min_size(
        egui::pos2(
            rect.right() - FIELD_PAD_X - FIELD_CHEVRON,
            rect.center().y - FIELD_CHEVRON / 2.0,
        ),
        egui::Vec2::splat(FIELD_CHEVRON),
    );
    draw_chevron_down(painter, chevron, palette.segment_unselected_text);
    let _ = response.on_hover_text(LANGUAGE_TOOLTIP);
}

/// 快捷键键帽字段（152×28、圆角 7；键帽高 20、圆角 4、11pt）。
/// 点击不进入录制（Task 6）。
fn draw_keycap_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    keys: &[String],
    row: usize,
    palette: &SettingsPalette,
) {
    // 两行快捷键字段同帧并存:交互 id 必须按行区分(否则 hover 命中互相覆盖)。
    let response = ui.interact(
        rect,
        ui.scope_id().with(("settings-keycaps", row)),
        egui::Sense::click(),
    );
    let fill = if response.is_pointer_button_down_on() {
        palette.control_fill.gamma_multiply(2.0)
    } else if response.hovered() {
        palette.control_fill.gamma_multiply(1.5)
    } else {
        palette.control_fill
    };
    let painter = ui.painter().clone();
    painter.rect_filled(rect, egui::CornerRadius::same(FIELD_RADIUS), fill);
    let mut x = rect.left() + FIELD_PAD_X;
    let top = rect.center().y - KEYCAP_H / 2.0;
    for key in keys {
        let width = text_width(ui, key, KEYCAP_FONT) + 2.0 * KEYCAP_PAD_X;
        let cap = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(width, KEYCAP_H));
        if cap.right() > rect.right() - FIELD_PAD_X {
            break; // 字段过窄（理论不可达：152 容得下默认键帽）
        }
        painter.rect_filled(
            cap,
            egui::CornerRadius::same(KEYCAP_RADIUS),
            palette.keycap_fill,
        );
        painter.text(
            cap.center(),
            egui::Align2::CENTER_CENTER,
            key,
            ui_font::id(KEYCAP_FONT),
            palette.keycap_text,
        );
        x += width + KEYCAP_GAP;
    }
}

/// 「＋ 添加目录…」按钮（高 28、圆角 7、12pt；宽随文案）。
/// 点击不打开选择器（Task 8）。
fn draw_add_button(ui: &mut egui::Ui, rect: egui::Rect, palette: &SettingsPalette) {
    let response = ui.interact(
        rect,
        ui.scope_id().with("settings-add-dir"),
        egui::Sense::click(),
    );
    let fill = if response.is_pointer_button_down_on() {
        palette.add_button_fill.gamma_multiply(0.8)
    } else if response.hovered() {
        palette.add_button_fill.gamma_multiply(1.4)
    } else {
        palette.add_button_fill
    };
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(ADD_BUTTON_RADIUS), fill);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        ADD_DIR_LABEL,
        ui_font::id(ADD_BUTTON_FONT),
        palette.add_button_text,
    );
}

/// 目录行（40）：文件夹图标 16 + 路径 13pt + 右侧移除 ×14。
/// 移除只画 hover 视觉，不删除条目（Task 8）。
fn draw_dir_row(
    ui: &mut egui::Ui,
    row: egui::Rect,
    index: usize,
    path: &str,
    palette: &SettingsPalette,
) {
    let painter = ui.painter().clone();
    let icon = egui::Rect::from_min_size(
        egui::pos2(row.left() + ROW_PAD_X, row.center().y - FOLDER_ICON / 2.0),
        egui::Vec2::splat(FOLDER_ICON),
    );
    draw_folder_icon(&painter, icon, palette.text_secondary);

    let remove = egui::Rect::from_min_size(
        egui::pos2(
            row.right() - ROW_PAD_X - REMOVE_ICON,
            row.center().y - REMOVE_ICON / 2.0,
        ),
        egui::Vec2::splat(REMOVE_ICON),
    );
    let path_rect = egui::Rect::from_min_max(
        egui::pos2(icon.right() + FOLDER_ICON_GAP, row.top()),
        egui::pos2(remove.left() - LABEL_CONTROL_GAP, row.bottom()),
    );
    draw_text(
        ui,
        path_rect,
        path,
        LABEL_FONT,
        palette.dir_path,
        egui::Align2::LEFT_CENTER,
    );

    let response = ui.interact(
        remove,
        ui.scope_id().with(("settings-remove-dir", index)),
        egui::Sense::click(),
    );
    let color = if response.hovered() || response.is_pointer_button_down_on() {
        palette.text_primary
    } else {
        palette.remove_icon
    };
    draw_remove_icon(&painter, remove, color);
}

/// 单行文本（按 rect 宽截断；垂直居中，水平按 `align`）。
/// `base` 为 token 基准 pt，这里经 `font::size` 应用全局倍率。
fn draw_text(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    base: f32,
    color: egui::Color32,
    align: egui::Align2,
) {
    if rect.width() <= 0.0 || text.is_empty() {
        return;
    }
    let galley = egui::WidgetText::from(
        egui::RichText::new(text)
            .size(ui_font::size(base))
            .color(color),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    let pos = align.align_size_within_rect(galley.size(), rect).min;
    ui.painter().galley(pos, galley, color);
}

/// 单行文本自然宽（键帽/按钮按内容定宽；字号经全局倍率）。
fn text_width(ui: &egui::Ui, text: &str, base: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            ui_font::id(base),
            egui::Color32::PLACEHOLDER,
        )
        .size()
        .x
}

/// Lucide `chevron-down`（14×14，两段线条）。
fn draw_chevron_down(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(ICON_STROKE, color);
    let center = rect.center();
    let wing_x = rect.width() / 4.0;
    let wing_y = rect.height() / 7.0;
    painter.line_segment(
        [
            egui::pos2(center.x - wing_x, center.y - wing_y),
            egui::pos2(center.x, center.y + wing_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(center.x + wing_x, center.y - wing_y),
            egui::pos2(center.x, center.y + wing_y),
        ],
        stroke,
    );
}

/// 文件夹轮廓（16×16，Lucide `folder` 简化网格：带折角的矩形）。
fn draw_folder_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let scale = rect.width() / 24.0;
    let point = |x: f32, y: f32| rect.min + egui::vec2(x, y) * scale;
    painter.add(egui::Shape::closed_line(
        vec![
            point(3.0, 19.0),
            point(3.0, 5.0),
            point(10.0, 5.0),
            point(12.0, 7.0),
            point(21.0, 7.0),
            point(21.0, 19.0),
        ],
        egui::Stroke::new(ICON_STROKE, color),
    ));
}

/// 移除 ×（14×14，两条 1px 斜线）。
fn draw_remove_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(REMOVE_ICON_STROKE, color);
    let inset = rect.width() / 4.0;
    painter.line_segment(
        [
            rect.min + egui::vec2(inset, inset),
            rect.max - egui::vec2(inset, inset),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(rect.right() - inset, rect.top() + inset),
            egui::pos2(rect.left() + inset, rect.bottom() - inset),
        ],
        stroke,
    );
}

/// 隐藏设置窗口（红点关闭 / ⌘W 共用）：开合态置 false + 下发隐藏命令。
/// 命令只入 egui 队列，不销毁视口——再次打开仍是同一实例。
fn hide(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    lock_shared(shared).settings.open = false;
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    log::info!("设置窗口隐藏(open=false)");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::FontSize;

    /// headless 布局数据:真实引擎默认排除项 + 默认键帽(不触 Shared)。
    fn layout_test_data() -> SettingsData {
        SettingsData {
            font_size: FontSize::Standard,
            autostart: false,
            language: "简体中文",
            palette_keys: vec!["⌥".into(), "Space".into()],
            window_keys: vec!["⌥".into(), "⇧".into(), "F".into()],
            excluded: vec!["~/Library".into(), "node_modules".into(), ".git".into()],
        }
    }

    /// 递归收集一帧里的全部文本形状(含 Shape::Vec 内的嵌套形状)。
    fn text_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::TextShape>) {
        match shape {
            egui::Shape::Text(text) => out.push(text.clone()),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    text_shapes(shape, out);
                }
            }
            _ => {}
        }
    }

    /// 跑一帧纯绘制函数(620×600;不构建视口/Shared)。
    fn run_body(data: &SettingsData) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(SETTINGS_W, SETTINGS_H),
            )),
            ..Default::default()
        };
        ctx.run_ui(input, |ui| draw_body(ui, data))
    }

    /// Task 4 布局:三组标题、六项标签、字号三段、语言、添加按钮与目录行齐全;
    /// 内容自标题栏下起排且标准字号下不超过 620×600。
    #[test]
    fn settings_layout_has_all_rows() {
        let data = layout_test_data();
        let mut out = run_body(&data);
        let mut texts = Vec::new();
        for clipped in &out.shapes {
            text_shapes(&clipped.shape, &mut texts);
        }
        let labels: Vec<&str> = texts.iter().map(|t| t.galley.text()).collect();

        for expected in [
            "外观",
            "通用",
            "索引",
            "字号",
            "开机自启",
            "语言",
            "唤起快捷键",
            "完整窗口快捷键",
            "排除目录",
            "小",
            "标准",
            "大",
            "简体中文",
            "＋ 添加目录…",
        ] {
            assert!(
                labels.contains(&expected),
                "缺少文本 {expected:?};实际 {labels:?}"
            );
        }
        for path in &data.excluded {
            assert!(
                labels.contains(&path.as_str()),
                "缺少目录行 {path:?};实际 {labels:?}"
            );
        }

        let min_top = texts.iter().map(|t| t.pos.y).fold(f32::INFINITY, f32::min);
        assert!(
            min_top >= TITLEBAR_H,
            "内容不得侵入固定标题栏(最早文本 y={min_top})"
        );
        let max_bottom = texts
            .iter()
            .map(|t| t.pos.y + t.galley.size().y)
            .fold(0.0_f32, f32::max);
        assert!(
            max_bottom <= SETTINGS_H - CONTENT_PAD.0,
            "标准字号下内容须在 620×600 内(最低文本底 {max_bottom} > {})",
            SETTINGS_H - CONTENT_PAD.0
        );
        out.textures_delta.clear();
    }

    /// 静态数据真源:字号来自设置快照;排除目录 = 引擎默认(非原型示意值);
    /// 其余占位与 Task 5/6 前的内置默认一致。
    #[test]
    fn static_data_uses_file_font_size_and_engine_defaults() {
        let data = SettingsData::static_from(FontSize::Large);
        assert_eq!(data.font_size, FontSize::Large);
        assert!(!data.autostart);
        assert_eq!(data.language, "简体中文");
        assert_eq!(data.palette_keys, vec!["⌥".to_owned(), "Space".to_owned()]);
        assert_eq!(
            data.window_keys,
            vec!["⌥".to_owned(), "⇧".to_owned(), "F".to_owned()]
        );
        assert_eq!(data.excluded, engine::config::default_config().excluded);
    }

    #[test]
    fn window_size_matches_spec() {
        assert_eq!((SETTINGS_W, SETTINGS_H), (620.0, 600.0));
        assert_eq!(TITLEBAR_H, 44.0);
    }

    /// 视口 builder：固定尺寸、启动隐藏、无标题/标题栏 + 全尺寸内容视图
    ///（原生交通灯浮在自绘标题栏上）。
    #[test]
    fn builder_matches_spec() {
        let b = builder();
        assert_eq!(b.title.as_deref(), Some(platform::SETTINGS_TITLE));
        assert_eq!(b.inner_size, Some(egui::vec2(SETTINGS_W, SETTINGS_H)));
        assert_eq!(b.min_inner_size, Some(egui::vec2(SETTINGS_W, SETTINGS_H)));
        assert_eq!(b.max_inner_size, Some(egui::vec2(SETTINGS_W, SETTINGS_H)));
        assert_eq!(b.resizable, Some(false));
        assert_eq!(b.visible, Some(false), "启动隐藏");
        assert_eq!(b.titlebar_shown, Some(false));
        assert_eq!(b.title_shown, Some(false));
        assert_eq!(b.fullsize_content_view, Some(true));
        assert_eq!(b.has_shadow, Some(true));
    }
}

//! 设置页纯视图：快照数据 → 分组/卡片/行控制绘制（不读 `Shared`、不执行动作）。
//!
//! 行为编排在 `ui/settings.rs`：它短暂持锁构造 [`SettingsData`]，出锁绘制，最后
//! 消费 [`SettingsAction`]（字号/自启）。本文件只负责视觉与点击收集。
//!
//! 行模型：[`RowSpec`] 同时决定卡片高度与绘制游标；行内错误行（字号保存失败 /
//! 自启未打包）通过 `with_extra` 增高，卡片底与分隔线随之走同一游标。

use eframe::egui;

use crate::settings::FontSize;
use crate::ui::{
    font as ui_font,
    settings::SettingsState,
    settings_layout::{card_height, take_row, RowSpec},
    settings_tokens::{
        self, SettingsPalette, ADD_BUTTON_FONT, ADD_BUTTON_H, ADD_BUTTON_PAD_X, ADD_BUTTON_RADIUS,
        CARD_RADIUS, CONTENT_PAD, EXCLUDE_HEADER_H, EXCLUDE_ROW_H, FIELD, FIELD_CHEVRON,
        FIELD_FONT, FIELD_PAD_X, FIELD_RADIUS, FOLDER_ICON, FOLDER_ICON_GAP, GROUP_GAP,
        GROUP_HEADER_FONT, GROUP_HEADER_GAP, ICON_STROKE, KEYCAP_FONT, KEYCAP_GAP, KEYCAP_H,
        KEYCAP_PAD_X, KEYCAP_RADIUS, LABEL_CONTROL_GAP, LABEL_FONT, LABEL_SUPPORT_GAP, REMOVE_ICON,
        REMOVE_ICON_STROKE, ROW_H, ROW_PAD_X, SEG_FONT, SEG_GAP, SEG_H, SEG_ITEM_RADIUS, SEG_PAD,
        SEG_RADIUS, SEG_W, SUPPORT_FONT, SWITCH_KNOB, SWITCH_KNOB_SHADOW_OFFSET,
        SWITCH_KNOB_SHADOW_SIZE, SWITCH_RADIUS, SWITCH_SIZE, TITLEBAR_H,
    },
};

/// 添加目录按钮文案（宽度测量与绘制共用）。
const ADD_DIR_LABEL: &str = "＋ 添加目录…";
/// 语言禁用值（本期仅简中）。
const LANGUAGE_LABEL: &str = "简体中文";
/// 语言禁用态提示。
const LANGUAGE_TOOLTIP: &str = "暂仅支持简体中文，语言切换将在后续版本提供";
/// 排除目录空态文案（Task 8 换真实 daemon 列表）。
const EXCLUDED_EMPTY: &str = "未排除任何目录";

/// 开关钮柔和投影：由外到内 3 层（扩边, 相对 token 20% 底色的倍率）。
const KNOB_SHADOW_LAYERS: [(f32, f32); 3] = [
    (SWITCH_KNOB_SHADOW_SIZE + 1.5, 0.30),
    (SWITCH_KNOB_SHADOW_SIZE + 0.75, 0.55),
    (SWITCH_KNOB_SHADOW_SIZE, 1.0),
];

/// 设置页渲染快照（出锁后使用；`SettingsState` 是运行期真源）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SettingsData {
    pub(crate) font_size: FontSize,
    /// 实际自启态；未知（读取失败）按关态显示。
    pub(crate) autostart: bool,
    pub(crate) autostart_error: Option<String>,
    pub(crate) font_error: Option<String>,
    pub(crate) language: &'static str,
    pub(crate) palette_keys: Vec<String>,
    pub(crate) window_keys: Vec<String>,
    pub(crate) excluded: Vec<String>,
}

impl SettingsData {
    /// 从运行期状态复制一帧快照（锁内调用，绘制在锁外）。
    pub(crate) fn from_state(state: &SettingsState) -> Self {
        Self {
            font_size: state.file.font_size,
            autostart: state.autostart.unwrap_or(false),
            autostart_error: state.autostart_error.clone(),
            font_error: state.font_error.clone(),
            language: LANGUAGE_LABEL,
            palette_keys: state.palette_keys.clone(),
            window_keys: state.window_keys.clone(),
            excluded: state.excluded.clone(),
        }
    }
}

/// 设置页产生的用户动作（绘制后在锁外执行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    /// 字号分段点击（立即应用 + 落盘）。
    SetFontSize(FontSize),
    /// 开机自启开关点击（目标态）。
    SetAutostart(bool),
}

/// 一帧绘制结果：动作队列（通常为空或单条）。
pub(crate) struct BodyOutput {
    pub(crate) actions: Vec<SettingsAction>,
}

/// 绘制标题栏以下的内容（纯绘制：数据来自 [`SettingsData`]，动作回传调用方）。
///
/// 标题栏固定；内容区自带 (20,24) 内边距，超出可视高时整页纵向滚动
/// （不做卡片内独立滚动，排除目录列表 Task 8 沿用整页）。
pub(crate) fn draw_body(ui: &mut egui::Ui, data: &SettingsData) -> BodyOutput {
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

    let mut actions = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("settings-body")
        .auto_shrink([false, false])
        .show(&mut body, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let full_w = ui.available_width();
            let extra_h = support_extra_height(ui);
            ui.add_space(CONTENT_PAD.0);
            draw_appearance_group(ui, full_w, data, &palette, extra_h, &mut actions);
            ui.add_space(GROUP_GAP);
            draw_general_group(ui, full_w, data, &palette, extra_h, &mut actions);
            ui.add_space(GROUP_GAP);
            draw_index_group(ui, full_w, data, &palette, &mut actions);
            ui.add_space(CONTENT_PAD.0);
        });
    BodyOutput { actions }
}

/// 内联错误行占用的额外高 = 辅助字号行高 + 标签间距。
fn support_extra_height(ui: &egui::Ui) -> f32 {
    LABEL_SUPPORT_GAP + ui.fonts_mut(|fonts| fonts.row_height(&ui_font::id(SUPPORT_FONT)))
}

fn extra_for(error: Option<&str>, extra_h: f32) -> f32 {
    if error.is_some() {
        extra_h
    } else {
        0.0
    }
}

/// 分组行模型：`kind` 决定控件，`spec` 同时驱动高度与游标，`error` 为行内错误。
struct GroupRow<'a> {
    kind: RowKind<'a>,
    spec: RowSpec,
    error: Option<&'a str>,
}

enum RowKind<'a> {
    FontSize,
    Autostart,
    Language,
    PaletteKeys,
    WindowKeys,
    ExcludeHeader,
    Exclusion { index: usize, path: &'a str },
    ExcludedEmpty,
}

/// 外观组：分组标题 + 单行卡片（字号分段 + 可选的保存错误）。
fn draw_appearance_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let error = data.font_error.as_deref();
    let rows = [GroupRow {
        kind: RowKind::FontSize,
        spec: RowSpec::new(ROW_H).with_extra(extra_for(error, extra_h)),
        error,
    }];
    draw_card(ui, full_w, "外观", &rows, data, palette, actions);
}

/// 通用组：四行卡片（自启 / 语言 / 两个快捷键）。
fn draw_general_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let autostart_error = data.autostart_error.as_deref();
    let rows = [
        GroupRow {
            kind: RowKind::Autostart,
            spec: RowSpec::new(ROW_H).with_extra(extra_for(autostart_error, extra_h)),
            error: autostart_error,
        },
        GroupRow {
            kind: RowKind::Language,
            spec: RowSpec::new(ROW_H),
            error: None,
        },
        GroupRow {
            kind: RowKind::PaletteKeys,
            spec: RowSpec::new(ROW_H),
            error: None,
        },
        GroupRow {
            kind: RowKind::WindowKeys,
            spec: RowSpec::new(ROW_H),
            error: None,
        },
    ];
    draw_card(ui, full_w, "通用", &rows, data, palette, actions);
}

/// 索引组：Header 52（排除目录 + 添加按钮）+ 目录行 40；列表空时一行空态。
fn draw_index_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    let mut rows = vec![GroupRow {
        kind: RowKind::ExcludeHeader,
        spec: RowSpec::new(EXCLUDE_HEADER_H),
        error: None,
    }];
    if data.excluded.is_empty() {
        rows.push(GroupRow {
            kind: RowKind::ExcludedEmpty,
            spec: RowSpec::new(EXCLUDE_ROW_H),
            error: None,
        });
    } else {
        rows.extend(
            data.excluded
                .iter()
                .enumerate()
                .map(|(index, path)| GroupRow {
                    kind: RowKind::Exclusion { index, path },
                    spec: RowSpec::new(EXCLUDE_ROW_H),
                    error: None,
                }),
        );
    }
    draw_card(ui, full_w, "索引", &rows, data, palette, actions);
}

/// 卡片：分组标题 + 底色；行高度与绘制共用同一行模型，末尾校对游标。
fn draw_card(
    ui: &mut egui::Ui,
    full_w: f32,
    title: &str,
    rows: &[GroupRow<'_>],
    data: &SettingsData,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    let specs: Vec<RowSpec> = rows.iter().map(|row| row.spec).collect();
    let card = draw_group_frame(ui, full_w, title, &specs, palette);
    let mut y = card.top();
    for (index, row) in rows.iter().enumerate() {
        let rects = take_row(card, &mut y, row.spec, index > 0);
        if let Some(divider) = rects.divider {
            ui.painter()
                .rect_filled(divider, egui::CornerRadius::ZERO, palette.divider);
        }
        draw_row_body(ui, rects.body, &row.kind, data, palette, actions);
        if let Some(error) = row.error {
            draw_support_line(ui, rects.extra, error, palette);
        }
    }
    debug_assert!(
        (y - card.bottom()).abs() < 0.01,
        "设置卡片高度与行游标不一致:标题={title:?} 卡片高={} 游标高={}",
        card.height(),
        y - card.top()
    );
}

/// 分组标题（12pt 次要色）+ 8 + 卡片（圆角 10、卡片底、行间 1px 分隔线）。
fn draw_group_frame(
    ui: &mut egui::Ui,
    full_w: f32,
    title: &str,
    rows: &[RowSpec],
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
    let card_h = card_height(rows);
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

/// 行主体控件（左标签 + 右侧控件；控件点击写入 `actions`）。
fn draw_row_body(
    ui: &mut egui::Ui,
    body: egui::Rect,
    kind: &RowKind<'_>,
    data: &SettingsData,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    match *kind {
        RowKind::FontSize => {
            let control = draw_row_label(ui, body, "字号", egui::vec2(SEG_W, SEG_H), palette);
            if let Some(size) = draw_segmented(ui, control, data.font_size, palette) {
                actions.push(SettingsAction::SetFontSize(size));
            }
        }
        RowKind::Autostart => {
            let control = draw_row_label(
                ui,
                body,
                "开机自启",
                egui::vec2(SWITCH_SIZE.0, SWITCH_SIZE.1),
                palette,
            );
            if draw_switch(ui, control, data.autostart, palette) {
                actions.push(SettingsAction::SetAutostart(!data.autostart));
            }
        }
        RowKind::Language => {
            let control = draw_row_label(ui, body, "语言", egui::vec2(FIELD.0, FIELD.1), palette);
            draw_language_field(ui, control, data.language, palette);
        }
        RowKind::PaletteKeys => {
            let control = draw_row_label(
                ui,
                body,
                "唤起快捷键",
                egui::vec2(FIELD.0, FIELD.1),
                palette,
            );
            draw_keycap_field(ui, control, &data.palette_keys, "palette", palette);
        }
        RowKind::WindowKeys => {
            let control = draw_row_label(
                ui,
                body,
                "完整窗口快捷键",
                egui::vec2(FIELD.0, FIELD.1),
                palette,
            );
            draw_keycap_field(ui, control, &data.window_keys, "window", palette);
        }
        RowKind::ExcludeHeader => {
            let button_w = text_width(ui, ADD_DIR_LABEL, ADD_BUTTON_FONT) + 2.0 * ADD_BUTTON_PAD_X;
            let control = draw_row_label(
                ui,
                body,
                "排除目录",
                egui::vec2(button_w, ADD_BUTTON_H),
                palette,
            );
            draw_add_button(ui, control, palette);
        }
        RowKind::Exclusion { index, path } => draw_dir_row(ui, body, index, path, palette),
        RowKind::ExcludedEmpty => draw_excluded_empty(ui, body, palette),
    }
}

/// 行内错误（辅助字号、次要红、左对齐；高度已计入行模型）。
fn draw_support_line(ui: &egui::Ui, extra: egui::Rect, text: &str, palette: &SettingsPalette) {
    let rect = egui::Rect::from_min_max(
        egui::pos2(extra.left() + ROW_PAD_X, extra.top() + LABEL_SUPPORT_GAP),
        egui::pos2(extra.right() - ROW_PAD_X, extra.bottom()),
    );
    draw_text(
        ui,
        rect,
        text,
        SUPPORT_FONT,
        palette.error_text,
        egui::Align2::LEFT_CENTER,
    );
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

/// 字号三段控件（180×28、圆角 7、pad 2、gap 2；段圆角 6、12pt）。
/// 返回被点击的档位（选中段仍可点击——可重试上次失败的保存）。
fn draw_segmented(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    selected: FontSize,
    palette: &SettingsPalette,
) -> Option<FontSize> {
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
    let mut clicked = None;
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
        if response.clicked() {
            clicked = Some(option);
        }
    }
    clicked
}

/// 开机自启开关（38×22、圆角 11；钮 18 白色 + 三层柔和投影）。
/// 返回是否点击（目标态由调用方取反）。
fn draw_switch(ui: &mut egui::Ui, rect: egui::Rect, on: bool, palette: &SettingsPalette) -> bool {
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
    // 柔和投影:外圈大而淡、内圈小而实(替代单层硬边 20% 环)。
    for (grow, alpha) in KNOB_SHADOW_LAYERS {
        painter.circle_filled(
            knob_center + egui::vec2(0.0, SWITCH_KNOB_SHADOW_OFFSET),
            knob_radius + grow,
            palette.switch_knob_shadow.gamma_multiply(alpha),
        );
    }
    painter.circle_filled(knob_center, knob_radius, palette.switch_knob);
    response.clicked()
}

/// 语言下拉（152×28、圆角 7；禁用态「简体中文」+ chevron + tooltip）。
/// 文字/chevron 用次要色区分禁用态；点击不改变值（本期仅简中）。
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
        palette.segment_unselected_text,
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
    let _ = response
        .on_hover_cursor(egui::CursorIcon::NotAllowed)
        .on_hover_text(LANGUAGE_TOOLTIP);
}

/// 快捷键键帽字段（152×28、圆角 7；键帽高 20、圆角 4、11pt）。
/// 键帽组在字段内**水平居中**（原型 §2.4）。点击不进入录制（后续 plan）。
fn draw_keycap_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    keys: &[String],
    id_salt: &'static str,
    palette: &SettingsPalette,
) {
    // 两行快捷键字段同帧并存:交互 id 必须按行区分(否则 hover 命中互相覆盖)。
    let response = ui.interact(
        rect,
        ui.scope_id().with(("settings-keycaps", id_salt)),
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

    let widths: Vec<f32> = keys
        .iter()
        .map(|key| text_width(ui, key, KEYCAP_FONT) + 2.0 * KEYCAP_PAD_X)
        .collect();
    let total = widths.iter().sum::<f32>() + KEYCAP_GAP * keys.len().saturating_sub(1) as f32;
    let available = rect.width() - 2.0 * FIELD_PAD_X;
    let mut x = if total <= available {
        rect.center().x - total / 2.0
    } else {
        rect.left() + FIELD_PAD_X
    };
    let top = rect.center().y - KEYCAP_H / 2.0;
    for (key, width) in keys.iter().zip(widths) {
        if x + width > rect.right() - FIELD_PAD_X + 0.01 {
            break; // 字段过窄（理论不可达：152 容得下默认键帽）
        }
        let cap = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(width, KEYCAP_H));
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

/// 排除目录空态（Task 8 接真实列表前的可见空行）。
fn draw_excluded_empty(ui: &egui::Ui, row: egui::Rect, palette: &SettingsPalette) {
    draw_text(
        ui,
        row.shrink2(egui::vec2(ROW_PAD_X, 0.0)),
        EXCLUDED_EMPTY,
        LABEL_FONT,
        palette.text_secondary,
        egui::Align2::CENTER_CENTER,
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::font as ui_font_module;
    use crate::ui::settings_tokens::{SETTINGS_H, SETTINGS_W};

    /// headless 布局数据:默认键帽 + 三项目录（不触 Shared）。
    fn layout_test_data() -> SettingsData {
        SettingsData {
            font_size: FontSize::Standard,
            autostart: false,
            autostart_error: None,
            font_error: None,
            language: LANGUAGE_LABEL,
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

    /// 递归收集矩形填充（选中段/卡片底等几何断言）。
    fn rect_shapes(shape: &egui::Shape, out: &mut Vec<egui::epaint::RectShape>) {
        match shape {
            egui::Shape::Rect(rect) => out.push(rect.clone()),
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    rect_shapes(shape, out);
                }
            }
            _ => {}
        }
    }

    /// 在既有 Context 上跑一帧（交互测试需跨帧保留 widget 记忆）。
    fn run_frame(
        ctx: &egui::Context,
        data: &SettingsData,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, BodyOutput) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(SETTINGS_W, SETTINGS_H),
            )),
            events,
            ..Default::default()
        };
        let mut body = None;
        let mut out = ctx.run_ui(input, |ui| {
            body = Some(draw_body(ui, data));
        });
        out.textures_delta.clear();
        (out, body.expect("draw_body 未运行"))
    }

    /// 跑一帧纯绘制函数（620×600 深色；不构建视口/Shared）。
    fn run_body(data: &SettingsData) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        run_frame(&ctx, data, Vec::new()).0
    }

    fn pointer_button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    /// 点击 = 前帧按下 + 后帧释放（同一 widget 命中）。
    fn click_at(ctx: &egui::Context, data: &SettingsData, pos: egui::Pos2) -> BodyOutput {
        run_frame(
            ctx,
            data,
            vec![egui::Event::PointerMoved(pos), pointer_button(pos, true)],
        );
        run_frame(
            ctx,
            data,
            vec![egui::Event::PointerMoved(pos), pointer_button(pos, false)],
        )
        .1
    }

    fn all_texts(out: &egui::FullOutput) -> Vec<egui::epaint::TextShape> {
        let mut texts = Vec::new();
        for clipped in &out.shapes {
            text_shapes(&clipped.shape, &mut texts);
        }
        texts
    }

    fn all_rects(out: &egui::FullOutput) -> Vec<egui::epaint::RectShape> {
        let mut rects = Vec::new();
        for clipped in &out.shapes {
            rect_shapes(&clipped.shape, &mut rects);
        }
        rects
    }

    fn text_center(text: &egui::epaint::TextShape) -> egui::Pos2 {
        text.pos + text.galley.size() * 0.5
    }

    fn find_text<'a>(
        texts: &'a [egui::epaint::TextShape],
        needle: &str,
    ) -> &'a egui::epaint::TextShape {
        texts
            .iter()
            .find(|text| text.galley.text() == needle)
            .unwrap_or_else(|| panic!("缺少文本 {needle:?}"))
    }

    /// Task 4 布局:三组标题、六项标签、字号三段、语言、添加按钮与目录行齐全;
    /// 内容自标题栏下起排且标准字号下不超过 620×600。
    #[test]
    fn settings_layout_has_all_rows() {
        let data = layout_test_data();
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();

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

        let min_top = texts
            .iter()
            .map(|text| text.pos.y)
            .fold(f32::INFINITY, f32::min);
        assert!(
            min_top >= TITLEBAR_H,
            "内容不得侵入固定标题栏(最早文本 y={min_top})"
        );
        let max_bottom = texts
            .iter()
            .map(|text| text.pos.y + text.galley.size().y)
            .fold(0.0_f32, f32::max);
        assert!(
            max_bottom <= SETTINGS_H - CONTENT_PAD.0,
            "标准字号下内容须在 620×600 内(最低文本底 {max_bottom} > {})",
            SETTINGS_H - CONTENT_PAD.0
        );
    }

    /// 空排除列表:仍显示目录头，并给出「未排除任何目录」空态行。
    #[test]
    fn empty_excluded_list_shows_placeholder_row() {
        let mut data = layout_test_data();
        data.excluded.clear();
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();
        assert!(labels.contains(&"排除目录"), "空态仍须显示目录头");
        assert!(
            labels.contains(&EXCLUDED_EMPTY),
            "空列表须显示 {EXCLUDED_EMPTY:?};实际 {labels:?}"
        );
    }

    /// 行内错误行参与卡片高度:字号保存失败会下移后续分组，错误文案可见。
    #[test]
    fn font_error_row_adds_card_height() {
        let baseline = run_body(&layout_test_data());
        let baseline_general = text_center(find_text(&all_texts(&baseline), "通用")).y;

        let mut data = layout_test_data();
        data.font_error = Some("设置保存失败:磁盘只读".into());
        let with_error = run_body(&data);
        let texts = all_texts(&with_error);
        let error_general = text_center(find_text(&texts, "通用")).y;
        assert!(
            error_general > baseline_general + 10.0,
            "字号错误行必须增高外观卡片:基线 {baseline_general},错误 {error_general}"
        );
        let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();
        assert!(
            labels.iter().any(|text| text.contains("磁盘只读")),
            "错误文案须绘制:实际 {labels:?}"
        );
    }

    /// 自启错误行同理:增高通用卡片（「语言」行随之下移）。
    #[test]
    fn autostart_error_row_adds_card_height() {
        let baseline = run_body(&layout_test_data());
        let baseline_language = text_center(find_text(&all_texts(&baseline), "语言")).y;

        let mut data = layout_test_data();
        data.autostart_error = Some("未打包:开机自启仅在 .app 内可用".into());
        let with_error = run_body(&data);
        let texts = all_texts(&with_error);
        let error_language = text_center(find_text(&texts, "语言")).y;
        assert!(
            error_language > baseline_language + 10.0,
            "自启错误行必须增高通用卡片:基线 {baseline_language},错误 {error_language}"
        );
    }

    /// 键帽组在字段内水平居中（原型 §2.4），文字与同排标签垂直居中对齐。
    #[test]
    fn keycaps_are_centered_in_fields() {
        let out = run_body(&layout_test_data());
        let texts = all_texts(&out);
        let field_center_x = SETTINGS_W - CONTENT_PAD.1 - ROW_PAD_X - FIELD.0 / 2.0;

        let space = find_text(&texts, "Space");
        let opt = texts
            .iter()
            .filter(|text| text.galley.text() == "⌥")
            .min_by(|a, b| {
                let da = (text_center(a).y - text_center(space).y).abs();
                let db = (text_center(b).y - text_center(space).y).abs();
                da.total_cmp(&db)
            })
            .unwrap();
        let group_center =
            (opt.pos.x - KEYCAP_PAD_X + space.pos.x + space.galley.size().x + KEYCAP_PAD_X) / 2.0;
        assert!(
            (group_center - field_center_x).abs() < 0.5,
            "唤起快捷键键帽组须在字段内居中:组中心 {group_center},字段中心 {field_center_x}"
        );
        let label = find_text(&texts, "唤起快捷键");
        assert!(
            (text_center(opt).y - text_center(label).y).abs() < 0.5,
            "键帽须与行标签垂直居中"
        );

        let f_key = find_text(&texts, "F");
        let shift = find_text(&texts, "⇧");
        let opt_window = texts
            .iter()
            .filter(|text| text.galley.text() == "⌥")
            .find(|text| (text_center(text).y - text_center(shift).y).abs() < 0.5)
            .unwrap();
        let group_center =
            (opt_window.pos.x - KEYCAP_PAD_X + f_key.pos.x + f_key.galley.size().x + KEYCAP_PAD_X)
                / 2.0;
        assert!(
            (group_center - field_center_x).abs() < 0.5,
            "完整窗口键帽组须在字段内居中:组中心 {group_center},字段中心 {field_center_x}"
        );
        let label = find_text(&texts, "完整窗口快捷键");
        assert!(
            (text_center(f_key).y - text_center(label).y).abs() < 0.5,
            "键帽须与行标签垂直居中"
        );
    }

    /// 字号选中段有独立填充（标准档 = 中间段）。
    #[test]
    fn selected_segment_has_fill() {
        let out = run_body(&layout_test_data());
        let rects = all_rects(&out);
        let expected = settings_tokens::dark().segment_selected_fill;
        let inner_w = SEG_W - 2.0 * SEG_PAD;
        let item_w = (inner_w - SEG_GAP * 2.0) / 3.0;
        let selected = rects
            .iter()
            .find(|rect| {
                rect.fill == expected
                    && (rect.rect.width() - item_w).abs() < 0.5
                    && (rect.rect.height() - (SEG_H - 2.0 * SEG_PAD)).abs() < 0.5
            })
            .expect("缺少字号选中段填充");
        // 标准档是第二段:段底中心应位于容器中线。
        let container_center_x = rects
            .iter()
            .find(|rect| {
                rect.fill == settings_tokens::dark().control_fill && rect.rect.width() > 170.0
            })
            .map(|rect| rect.rect.center().x)
            .expect("缺少分段容器底");
        assert!(
            (selected.rect.center().x - container_center_x).abs() < 0.5,
            "选中段不是中间段:段中心 {} 容器中心 {container_center_x}",
            selected.rect.center().x
        );
    }

    /// 点击「小」段 → 回传 SetFontSize(Small)（行为接线，不执行保存）。
    #[test]
    fn segment_click_emits_font_size_action() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = layout_test_data();
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let small = text_center(find_text(&all_texts(&out), "小"));
        let output = click_at(&ctx, &data, small);
        assert_eq!(
            output.actions,
            vec![SettingsAction::SetFontSize(FontSize::Small)]
        );
    }

    /// 点击自启开关 → 回传目标态（当前关 → 请求开启）。
    #[test]
    fn autostart_click_emits_toggle_action() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = layout_test_data();
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let label = text_center(find_text(&all_texts(&out), "开机自启"));
        let switch = egui::pos2(
            SETTINGS_W - CONTENT_PAD.1 - ROW_PAD_X - SWITCH_SIZE.0 / 2.0,
            label.y,
        );
        let output = click_at(&ctx, &data, switch);
        assert_eq!(output.actions, vec![SettingsAction::SetAutostart(true)]);
    }

    /// 大字号冒烟:文本仍齐全、布局不 panic（可能整页滚动）。
    #[test]
    fn large_scale_smoke() {
        ui_font_module::with_test_scale(FontSize::Large.scale(), || {
            let out = run_body(&layout_test_data());
            let texts = all_texts(&out);
            let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();
            for expected in ["外观", "通用", "索引", "字号", "简体中文"] {
                assert!(labels.contains(&expected), "大字号缺少 {expected:?}");
            }
        });
    }
}

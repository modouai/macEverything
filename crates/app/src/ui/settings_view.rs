//! 设置页纯视图：快照数据 → 分组/卡片/行控制绘制（不读 `Shared`、不执行动作）。
//!
//! 行为编排在 `ui/settings.rs`：它短暂持锁构造 [`SettingsData`]，出锁绘制，最后
//! 消费 [`SettingsAction`]（字号/自启/快捷键命令/排除目录异步操作；「恢复默认」
//! 只回传动作，原生确认弹窗由设置 pass 在锁外弹出）。本文件只负责视觉与点击/
//! 按键收集；录制期的 suspend/commit 一律由命令队列交给 App（主线程 manager），
//! 排除目录 add/remove/reset 的 daemon 调用一律交给工作线程。
//!
//! 行模型：[`RowSpec`] 同时决定卡片高度与绘制游标；行内错误/提示行（字号保存失败 /
//! 自启未打包 / 快捷键回落或失败）通过 `with_extra` 增高，卡片底与分隔线随之走同一
//! 游标。快捷键行可同时显示一条错误 + 一条回落提示（各占一行高度）。

use eframe::egui;
use global_hotkey::hotkey::HotKey;

use crate::hotkey::{self, HotkeySlot, HotkeySnapshot};
use crate::i18n::{self, LanguageSetting};
use crate::settings::FontSize;
use crate::ui::{
    font as ui_font, hotkey_keys,
    settings::{AutostartError, ExcludeError, LanguageError, SettingsError},
    settings_layout::{card_height, take_row, RowSpec},
    settings_tokens::{
        self, SettingsPalette, ADD_BUTTON_FONT, ADD_BUTTON_H, ADD_BUTTON_PAD_X, ADD_BUTTON_RADIUS,
        CARD_RADIUS, CONTENT_PAD, EXCLUDE_HEADER_H, EXCLUDE_ROW_H, FIELD, FIELD_CHEVRON,
        FIELD_FONT, FIELD_PAD_X, FIELD_RADIUS, FOLDER_ICON, FOLDER_ICON_GAP, GROUP_GAP,
        GROUP_HEADER_FONT, GROUP_HEADER_GAP, ICON_STROKE, KEYCAP_FONT, KEYCAP_GAP, KEYCAP_H,
        KEYCAP_PAD_X, KEYCAP_RADIUS, LABEL_CONTROL_GAP, LABEL_FONT, LABEL_SUPPORT_GAP, REMOVE_ICON,
        REMOVE_ICON_STROKE, RESET_BUTTON_GAP, RESET_BUTTON_PAD_X, RESET_BUTTON_RADIUS, ROW_H,
        ROW_PAD_X, SEG_FONT, SEG_GAP, SEG_H, SEG_ITEM_RADIUS, SEG_PAD, SEG_RADIUS, SEG_W,
        SUPPORT_FONT, SWITCH_KNOB, SWITCH_KNOB_SHADOW_OFFSET, SWITCH_KNOB_SHADOW_SIZE,
        SWITCH_RADIUS, SWITCH_SIZE, TITLEBAR_H,
    },
};

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
    /// 实际自启态（仅 `Status::Enabled`）；未知/待审批按关态显示。
    pub(crate) autostart: bool,
    /// 已注册但等待系统「登录项」允许（显示关态 + 行内提示）。
    pub(crate) autostart_approval: bool,
    pub(crate) autostart_error: Option<AutostartError>,
    pub(crate) font_error: Option<SettingsError>,
    pub(crate) language: LanguageSetting,
    pub(crate) language_error: Option<LanguageError>,
    /// 热键快照（键帽展示/查重的唯一真源；两键帽字段直接读它）。
    pub(crate) hotkeys: HotkeySnapshot,
    /// 当前录制槽；`None` = 空闲（App 挂起同步完成，出现即就绪）。
    pub(crate) recording: Option<HotkeySlot>,
    /// 按槽定位的快捷键内联错误。
    pub(crate) hotkey_error: Option<(HotkeySlot, String)>,
    pub(crate) excluded: Vec<String>,
    /// 排除目录后台操作进行中（禁用添加/移除）。
    pub(crate) exclude_busy: bool,
    /// 在途操作为列表加载（「加载中…」；否则「重扫中…」）。
    pub(crate) exclude_busy_list: bool,
    /// 排除目录加载/变更失败原因（daemon 原始文案原样显示，其余本地化）。
    pub(crate) exclude_error: Option<ExcludeError>,
}

/// 设置页产生的用户动作（绘制后在锁外执行）。
///
/// 快捷键动作只描述意图：设置 pass 把它们翻译成共享态命令队列，
/// 真正的 `suspend`/`resume`/`commit` 由主线程 `App::logic` 执行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    /// 字号分段点击（立即应用 + 落盘）。
    SetFontSize(FontSize),
    /// 语言下拉选择（立即应用 + 落盘）。
    SetLanguage(LanguageSetting),
    /// 开机自启开关点击（目标态）。
    SetAutostart(bool),
    /// 点击键帽字段：开始录制该槽（正在录制另一槽时先取消）。
    BeginHotkeyRecord(HotkeySlot),
    /// 点击录制中的字段 / Esc / 点击他处：取消录制并恢复旧键。
    CancelHotkeyRecord,
    /// 录制到合法组合：提交给 App 校验/注册/落盘。
    CommitHotkey { slot: HotkeySlot, hotkey: HotKey },
    /// 录制中按 Backspace（兼容 Forward Delete）：恢复该槽内置默认
    ///（App 注册默认并清空设置字段）。
    ResetHotkey(HotkeySlot),
    /// 录制组合非法或与另一槽重复：仅显示内联错误（录制继续）。
    HotkeyError { slot: HotkeySlot, message: String },
    /// 点击「＋ 添加目录…」：设置 pass 弹原生目录选择器并启动 add 工作线程。
    AddExclude,
    /// 点击目录行移除 ×：设置 pass 启动 remove 工作线程（原文路径）。
    RemoveExclude(String),
    /// 点击「恢复默认」：设置 pass 先弹原生确认弹窗；确认后启动 reset 工作线程。
    ResetExcludes,
}

/// 一帧绘制结果：动作队列（通常为空或单条）。
pub(crate) struct BodyOutput {
    pub(crate) actions: Vec<SettingsAction>,
}

/// 绘制标题栏以下的内容（纯绘制：数据来自 [`SettingsData`]，动作回传调用方）。
///
/// 标题栏固定；内容区自带 (20,24) 内边距，超出可视高时整页纵向滚动
/// （不做卡片内独立滚动，排除目录列表同样随整页滚动）。
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
    // 点击别处取消录制：按下帧先于所有控件动作入列，保证「取消旧录制 → 开始新槽」
    // 的顺序；点击录制字段本身由字段在抬起时取消（避免按下/抬起跨帧重复开始）。
    if click_away_cancels_recording(ui, data) {
        actions.push(SettingsAction::CancelHotkeyRecord);
    }
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
            draw_index_group(ui, full_w, data, &palette, extra_h, &mut actions);
            ui.add_space(CONTENT_PAD.0);
        });
    BodyOutput { actions }
}

/// 内联错误行占用的额外高 = 辅助字号行高 + 标签间距。
fn support_extra_height(ui: &egui::Ui) -> f32 {
    LABEL_SUPPORT_GAP + ui.fonts_mut(|fonts| fonts.row_height(&ui_font::id(SUPPORT_FONT)))
}

fn extra_for(has_message: bool, extra_h: f32) -> f32 {
    if has_message {
        extra_h
    } else {
        0.0
    }
}

/// 快捷键行的按槽错误（错误优先于回落提示，但两者可同时绘制）。
fn hotkey_error_for(data: &SettingsData, slot: HotkeySlot) -> Option<&str> {
    data.hotkey_error
        .as_ref()
        .and_then(|(error_slot, message)| (*error_slot == slot).then_some(message.as_str()))
}

/// 快捷键行额外高：错误、回落提示、录制提示各占一行（可同时存在）。
fn hotkey_messages_extra(
    error: Option<&str>,
    note: Option<&str>,
    hint: Option<&str>,
    extra_h: f32,
) -> f32 {
    extra_for(error.is_some(), extra_h)
        + extra_for(note.is_some(), extra_h)
        + extra_for(hint.is_some(), extra_h)
}

/// 分组行模型：`kind` 决定控件，`spec` 同时驱动高度与游标；
/// `error`（红色）、`note`（回落提示）、`hint`（录制提示）自上而下可同时显示。
struct GroupRow<'a> {
    kind: RowKind<'a>,
    spec: RowSpec,
    error: Option<&'a str>,
    note: Option<&'a str>,
    hint: Option<&'a str>,
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
    let font_error = data.font_error.as_ref().map(SettingsError::text);
    let error = font_error.as_deref();
    let rows = [GroupRow {
        kind: RowKind::FontSize,
        spec: RowSpec::new(ROW_H).with_extra(extra_for(error.is_some(), extra_h)),
        error,
        note: None,
        hint: None,
    }];
    draw_card(
        ui,
        full_w,
        i18n::s().group_appearance,
        &rows,
        data,
        palette,
        actions,
    );
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
    let autostart_error = data.autostart_error.as_ref().map(AutostartError::text);
    let language_error = data.language_error.as_ref().map(LanguageError::text);
    let autostart_note = if autostart_error.is_none() && data.autostart_approval {
        Some(i18n::s().autostart_approval_note)
    } else {
        None
    };
    let has_message = autostart_error.is_some() || autostart_note.is_some();

    // 快捷键行：内联错误（校验/重复/注册/保存）+ 运行时回落提示（按槽拆分）
    // + 录制中的可见 Esc/点击别处提示（plan §H3 Step 2）。
    let palette_error = hotkey_error_for(data, HotkeySlot::Palette);
    let palette_note = data.hotkeys.note.as_deref();
    let palette_hint =
        (data.recording == Some(HotkeySlot::Palette)).then_some(i18n::s().recording_hint);
    let window_error = hotkey_error_for(data, HotkeySlot::Window);
    let window_note = data.hotkeys.window_note.as_deref();
    let window_hint =
        (data.recording == Some(HotkeySlot::Window)).then_some(i18n::s().recording_hint);

    let rows = [
        GroupRow {
            kind: RowKind::Autostart,
            spec: RowSpec::new(ROW_H).with_extra(extra_for(has_message, extra_h)),
            error: autostart_error.as_deref(),
            note: autostart_note,
            hint: None,
        },
        GroupRow {
            kind: RowKind::Language,
            spec: RowSpec::new(ROW_H).with_extra(extra_for(language_error.is_some(), extra_h)),
            error: language_error.as_deref(),
            note: None,
            hint: None,
        },
        GroupRow {
            kind: RowKind::PaletteKeys,
            spec: RowSpec::new(ROW_H).with_extra(hotkey_messages_extra(
                palette_error,
                palette_note,
                palette_hint,
                extra_h,
            )),
            error: palette_error,
            note: palette_note,
            hint: palette_hint,
        },
        GroupRow {
            kind: RowKind::WindowKeys,
            spec: RowSpec::new(ROW_H).with_extra(hotkey_messages_extra(
                window_error,
                window_note,
                window_hint,
                extra_h,
            )),
            error: window_error,
            note: window_note,
            hint: window_hint,
        },
    ];
    draw_card(
        ui,
        full_w,
        i18n::s().group_general,
        &rows,
        data,
        palette,
        actions,
    );
}

/// 索引组：Header 52（排除目录 + 添加按钮）+ 目录行 40；列表空时一行空态。
/// 加载/重扫与错误提示挂在 Header 下，均由 `RowSpec.extra` 计入卡片高度。
fn draw_index_group(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let busy_label = data.exclude_busy.then_some(if data.exclude_busy_list {
        i18n::s().exclude_loading
    } else {
        i18n::s().exclude_rescanning
    });
    let exclude_error = data.exclude_error.as_ref().map(ExcludeError::text);
    let error = exclude_error.as_deref();
    let extra = extra_for(error.is_some(), extra_h) + extra_for(busy_label.is_some(), extra_h);
    let mut rows = vec![GroupRow {
        kind: RowKind::ExcludeHeader,
        spec: RowSpec::new(EXCLUDE_HEADER_H).with_extra(extra),
        error,
        note: busy_label,
        hint: None,
    }];
    if data.excluded.is_empty() {
        rows.push(GroupRow {
            kind: RowKind::ExcludedEmpty,
            spec: RowSpec::new(EXCLUDE_ROW_H),
            error: None,
            note: None,
            hint: None,
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
                    note: None,
                    hint: None,
                }),
        );
    }
    draw_card(
        ui,
        full_w,
        i18n::s().group_index,
        &rows,
        data,
        palette,
        actions,
    );
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
        draw_support_lines(ui, rects.extra, row.error, row.note, row.hint, palette);
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
            let control = draw_row_label(
                ui,
                body,
                i18n::s().row_font_size,
                egui::vec2(SEG_W, SEG_H),
                palette,
            );
            if let Some(size) = draw_segmented(ui, control, data.font_size, palette) {
                actions.push(SettingsAction::SetFontSize(size));
            }
        }
        RowKind::Autostart => {
            let control = draw_row_label(
                ui,
                body,
                i18n::s().row_autostart,
                egui::vec2(SWITCH_SIZE.0, SWITCH_SIZE.1),
                palette,
            );
            if draw_switch(ui, control, data.autostart, palette) {
                actions.push(SettingsAction::SetAutostart(!data.autostart));
            }
        }
        RowKind::Language => {
            let control = draw_row_label(
                ui,
                body,
                i18n::s().row_language,
                egui::vec2(FIELD.0, FIELD.1),
                palette,
            );
            draw_language_field(ui, control, data.language, palette, actions);
        }
        RowKind::PaletteKeys => {
            let control = draw_row_label(
                ui,
                body,
                i18n::s().row_palette_hotkey,
                egui::vec2(FIELD.0, FIELD.1),
                palette,
            );
            draw_keycap_field(
                ui,
                control,
                HotkeySlot::Palette,
                &data.hotkeys.palette_keys,
                data,
                palette,
                actions,
            );
        }
        RowKind::WindowKeys => {
            let control = draw_row_label(
                ui,
                body,
                i18n::s().row_window_hotkey,
                egui::vec2(FIELD.0, FIELD.1),
                palette,
            );
            draw_keycap_field(
                ui,
                control,
                HotkeySlot::Window,
                &data.hotkeys.window_keys,
                data,
                palette,
                actions,
            );
        }
        RowKind::ExcludeHeader => {
            draw_exclude_header_controls(ui, body, data.exclude_busy, palette, actions);
        }
        RowKind::Exclusion { index, path } => {
            if let Some(action) = draw_dir_row(ui, body, index, path, data.exclude_busy, palette) {
                actions.push(action);
            }
        }
        RowKind::ExcludedEmpty => draw_excluded_empty(ui, body, palette),
    }
}

/// 行内提示（辅助字号、左对齐；错误红 / 提示次要色）。
/// 错误在上、提示在下，各占 `extra` 的等分之一（高度已计入行模型）。
fn draw_support_lines(
    ui: &egui::Ui,
    extra: egui::Rect,
    error: Option<&str>,
    note: Option<&str>,
    hint: Option<&str>,
    palette: &SettingsPalette,
) {
    let messages = [
        error.map(|text| (text, palette.error_text)),
        note.map(|text| (text, palette.text_secondary)),
        hint.map(|text| (text, palette.text_secondary)),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if messages.is_empty() || extra.height() <= 0.0 {
        return;
    }
    let line_h = extra.height() / messages.len() as f32;
    for (index, (text, color)) in messages.into_iter().enumerate() {
        let line = egui::Rect::from_min_size(
            egui::pos2(extra.left(), extra.top() + index as f32 * line_h),
            egui::vec2(extra.width(), line_h),
        );
        let rect = egui::Rect::from_min_max(
            egui::pos2(line.left() + ROW_PAD_X, line.top() + LABEL_SUPPORT_GAP),
            egui::pos2(line.right() - ROW_PAD_X, line.bottom()),
        );
        draw_text(
            ui,
            rect,
            text,
            SUPPORT_FONT,
            color,
            egui::Align2::LEFT_CENTER,
        );
    }
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

/// 字号档位文案（目录随语言切换）。
fn font_label(size: FontSize) -> &'static str {
    match size {
        FontSize::Small => i18n::s().font_small,
        FontSize::Standard => i18n::s().font_standard,
        FontSize::Large => i18n::s().font_large,
    }
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
            font_label(option),
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

/// 语言设置项文案（当前值显示与下拉选项共用）。
fn language_label(setting: LanguageSetting) -> &'static str {
    match setting {
        LanguageSetting::System => i18n::s().language_system,
        LanguageSetting::ZhHans => i18n::s().language_zh_hans,
        LanguageSetting::En => i18n::s().language_en,
    }
}

/// 语言下拉（152×28、圆角 7；当前值 + chevron；点击展开三项菜单）。
fn draw_language_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    current: LanguageSetting,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    let response = ui.interact(
        rect,
        ui.scope_id().with("settings-language"),
        egui::Sense::click(),
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
        language_label(current),
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
    draw_chevron_down(painter, chevron, palette.control_text);
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    egui::Popup::menu(&response).show(|ui| {
        ui.set_min_width(rect.width());
        for setting in [
            LanguageSetting::System,
            LanguageSetting::ZhHans,
            LanguageSetting::En,
        ] {
            if ui
                .selectable_label(current == setting, language_label(setting))
                .clicked()
            {
                actions.push(SettingsAction::SetLanguage(setting));
                ui.close();
            }
        }
    });
}

/// 键帽字段在 egui 临时数据里的 id（跨帧保存几何，供点击他处判定）。
fn keycap_field_id(slot: HotkeySlot) -> egui::Id {
    egui::Id::unique(("settings-keycap-field", slot))
}

/// 点击录制字段之外的左键 → 取消录制（纯输入判定）。
///
/// 录制字段的按下不算「点外」——同字段的取消在抬起时由字段自身处理，
/// 避免按下/抬起跨帧时先取消、抬起又被当成新一次录制。无上帧几何时保守取消。
fn click_away_cancels_recording(ui: &egui::Ui, data: &SettingsData) -> bool {
    let Some(slot) = data.recording else {
        return false;
    };
    if !ui.input(|i| i.pointer.primary_pressed()) {
        return false;
    }
    let origin = ui.input(|i| i.pointer.press_origin());
    let field = ui
        .ctx()
        .data(|d| d.get_temp::<egui::Rect>(keycap_field_id(slot)));
    match (origin, field) {
        (Some(origin), Some(field)) => !field.contains(origin),
        (None, _) => false,
        (Some(_), None) => true,
    }
}

/// 快捷键键帽字段（152×28、圆角 7；键帽高 20、圆角 4、11pt）。
///
/// 键帽组在字段内**水平居中**（原型 §2.4）。空闲点击 → 开始录制；
/// 录制本槽显示「按下快捷键…」，再次点击取消；录制另一槽时点击切换。
/// 录制期本字段消费按键/剪贴板事件（Esc 取消 / 合法组合提交 / 非法或重复显示内联错误）。
fn draw_keycap_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    slot: HotkeySlot,
    keys: &[String],
    data: &SettingsData,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    // 两行字段同帧并存:交互 id 按槽区分(否则 hover 命中互相覆盖);几何留到下帧。
    ui.ctx()
        .data_mut(|d| d.insert_temp(keycap_field_id(slot), rect));
    let recording = data.recording == Some(slot);
    let response = ui.interact(
        rect,
        ui.scope_id().with(("settings-keycaps", slot)),
        egui::Sense::click(),
    );
    let clicked = response.clicked();
    let fill = if recording {
        if response.is_pointer_button_down_on() {
            palette.segment_selected_fill.gamma_multiply(0.8)
        } else if response.hovered() {
            palette.segment_selected_fill.gamma_multiply(0.9)
        } else {
            palette.segment_selected_fill
        }
    } else if response.is_pointer_button_down_on() {
        palette.control_fill.gamma_multiply(2.0)
    } else if response.hovered() {
        palette.control_fill.gamma_multiply(1.5)
    } else {
        palette.control_fill
    };
    let painter = ui.painter().clone();
    painter.rect_filled(rect, egui::CornerRadius::same(FIELD_RADIUS), fill);

    if recording {
        draw_text(
            ui,
            rect.shrink2(egui::vec2(FIELD_PAD_X, 0.0)),
            i18n::s().recording_prompt,
            FIELD_FONT,
            palette.segment_selected_text,
            egui::Align2::CENTER_CENTER,
        );
        capture_recorded_key(ui, slot, data, actions);
        let _ = response.on_hover_text(i18n::s().recording_hint_hover);
    } else {
        draw_keycaps(ui, rect, keys, &painter, palette);
        let _ = response.on_hover_text(if data.recording.is_some() {
            i18n::s().keycap_hover_rerecord
        } else {
            i18n::s().keycap_hover_record
        });
    }

    if clicked {
        actions.push(if recording {
            SettingsAction::CancelHotkeyRecord
        } else {
            SettingsAction::BeginHotkeyRecord(slot)
        });
    }
}

/// 键帽组绘制（字段内水平居中；过宽时从左截断）。
fn draw_keycaps(
    ui: &egui::Ui,
    rect: egui::Rect,
    keys: &[String],
    painter: &egui::Painter,
    palette: &SettingsPalette,
) {
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

/// 录制时取本帧第一个非修饰按键；所有按键/剪贴板/文本事件一律消费。
///
/// - 纯修饰键（⇧⌃⌥⌘）只更新修饰态，不当作一次录制结果；重复事件忽略。
/// - eframe/egui-winit 默认把「仅 Command」的 ⌘C/⌘X/⌘V 转成 Copy/Cut/Paste
///   事件而非 `Event::Key`；这里翻译回 C/X/V，并**用 `input.modifiers`**（保留
///   Ctrl+Cmd+C 之类的额外修饰；`from_egui` 再映射 command→SUPER）。
/// - ⌥Space 在 macOS 上可能不产生可用的 `Event::Key`（释放版实测录制不到）：
///   兜底识别带 Alt 的文本事件 `" "` / `"\u{00A0}"`（选项键产生不断行空格），
///   翻译为 `(Space, modifiers)`。文本事件在录制期一律消费，不进入其它控件。
/// - 已知残留：⌘V 在剪贴板为空时 egui-winit 不产生任何事件（无法录制这一次按键）；
///   不全局关闭剪贴板快捷键，否则搜索框的复制/粘贴会失效。若 ⌥Space 连文本事件
///   都不可见，录制视图的 Backspace「恢复默认」仍是可靠路径。
/// - Esc 由调用方解释为取消，Backspace（兼容 Forward Delete）解释为恢复默认；其余键经
///   `hotkey_keys::from_egui` 翻译。
fn take_recorded_key(ui: &egui::Ui) -> Option<(egui::Key, egui::Modifiers)> {
    ui.input_mut(|input| {
        // 剪贴板/文本事件本身不携带修饰态；本帧 InputState 的最终修饰态即按键时修饰键。
        let event_modifiers = input.modifiers;
        let mut captured = None;
        let mut retained = Vec::with_capacity(input.events.len());
        for event in input.events.drain(..) {
            let clipboard_key = match &event {
                egui::Event::Copy => Some(egui::Key::C),
                egui::Event::Cut => Some(egui::Key::X),
                egui::Event::Paste(_) | egui::Event::PasteImage(_) => Some(egui::Key::V),
                _ => None,
            };
            if let Some(key) = clipboard_key {
                if captured.is_none() {
                    captured = Some((key, event_modifiers));
                }
                continue; // 消费：录制期不得触发剪贴板/其它 UI
            }
            match event {
                egui::Event::Key {
                    key,
                    pressed: true,
                    repeat,
                    modifiers,
                    ..
                } => {
                    if !repeat && captured.is_none() && !is_modifier_key(key) {
                        captured = Some((key, modifiers));
                    }
                }
                egui::Event::Text(text) => {
                    if captured.is_none() && event_modifiers.alt && is_space_text(&text) {
                        captured = Some((egui::Key::Space, event_modifiers));
                    }
                    // 消费：⌥ 组合的文本产出（⌥D→∂ / ⌥Space→NBSP）不得落入其它控件。
                }
                other => retained.push(other),
            }
        }
        input.events = retained;
        captured
    })
}

/// ⌥Space 的文本形态：ASCII 空格或选项键产生的不断行空格。
fn is_space_text(text: &str) -> bool {
    text == " " || text == "\u{00A0}"
}

/// 纯修饰键：录制期只改修饰态，不产生「不支持该键」。
fn is_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

/// 另一槽当前注册键（重复校验；录制挂起期快照仍是旧键）。
fn other_slot_hotkey(data: &SettingsData, slot: HotkeySlot) -> Option<HotKey> {
    match slot {
        HotkeySlot::Palette => data.hotkeys.window,
        HotkeySlot::Window => data.hotkeys.palette,
    }
}

/// 一次按键 → 动作（Esc 取消 / Backspace 恢复默认 / 非法与重复错误 / 合法提交）。
fn capture_recorded_key(
    ui: &egui::Ui,
    slot: HotkeySlot,
    data: &SettingsData,
    actions: &mut Vec<SettingsAction>,
) {
    let Some((key, modifiers)) = take_recorded_key(ui) else {
        return;
    };
    if key == egui::Key::Escape {
        actions.push(SettingsAction::CancelHotkeyRecord);
        return;
    }
    // Backspace（兼容 Forward Delete）是「恢复内置默认」手势，不作为可录制按键
    //（不落白名单）。
    if matches!(key, egui::Key::Delete | egui::Key::Backspace) {
        actions.push(SettingsAction::ResetHotkey(slot));
        return;
    }
    match hotkey_keys::from_egui(key, modifiers) {
        Ok(hotkey) if other_slot_hotkey(data, slot) == Some(hotkey) => {
            actions.push(SettingsAction::HotkeyError {
                slot,
                message: hotkey::DUPLICATE_HOTKEY_MESSAGE.to_owned(),
            });
        }
        Ok(hotkey) => actions.push(SettingsAction::CommitHotkey { slot, hotkey }),
        Err(err) => actions.push(SettingsAction::HotkeyError {
            slot,
            message: err.to_string(),
        }),
    }
}

/// 文字按钮宽度 = 文案自然宽 + 左右内边距（点击区高与添加按钮对齐）。
fn text_button_width(ui: &egui::Ui, label: &str) -> f32 {
    text_width(ui, label, ADD_BUTTON_FONT) + 2.0 * RESET_BUTTON_PAD_X
}

/// 排除目录 Header：右侧「恢复默认」+ 间距 +「＋ 添加目录…」。
///
/// 两个控件同排右对齐；文字按钮静止无底，悬停仅铺一层浅色，视觉上仍是
/// 次要文字而非新的大按钮。点击恢复默认只回传动作，原生确认弹窗由设置 pass
/// 在锁外弹出（本视图不执行模态）。
fn draw_exclude_header_controls(
    ui: &mut egui::Ui,
    body: egui::Rect,
    busy: bool,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    let add_w = text_width(ui, i18n::s().add_folder, ADD_BUTTON_FONT) + 2.0 * ADD_BUTTON_PAD_X;
    let reset_w = text_button_width(ui, i18n::s().restore_defaults);
    let group_w = reset_w + RESET_BUTTON_GAP + add_w;
    let control = draw_row_label(
        ui,
        body,
        i18n::s().row_excluded,
        egui::vec2(group_w, ADD_BUTTON_H),
        palette,
    );
    let reset = egui::Rect::from_min_size(control.min, egui::vec2(reset_w, control.height()));
    let add = egui::Rect::from_min_size(
        egui::pos2(control.right() - add_w, control.top()),
        egui::vec2(add_w, control.height()),
    );
    if draw_reset_text_button(
        ui,
        reset,
        "settings-reset-excludes",
        i18n::s().restore_defaults,
        palette.text_secondary,
        palette.text_primary,
        !busy,
        palette,
    ) {
        actions.push(SettingsAction::ResetExcludes);
    }
    if let Some(action) = draw_add_button(ui, add, busy, palette) {
        actions.push(action);
    }
}

/// 小号文字按钮（12pt；静止透明、悬停浅底；busy 时置灰并禁点）。
/// 返回 true = 本帧点击且启用。
#[allow(clippy::too_many_arguments)]
fn draw_reset_text_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: &'static str,
    label: &str,
    color: egui::Color32,
    hover_color: egui::Color32,
    enabled: bool,
    palette: &SettingsPalette,
) -> bool {
    let response = ui.interact(
        rect,
        ui.scope_id().with(id),
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let hovered = enabled && response.hovered();
    let down = enabled && response.is_pointer_button_down_on();
    if hovered || down {
        let fill = palette
            .control_fill
            .gamma_multiply(if down { 1.6 } else { 1.0 });
        ui.painter()
            .rect_filled(rect, egui::CornerRadius::same(RESET_BUTTON_RADIUS), fill);
    }
    let text_color = if !enabled {
        color.gamma_multiply(0.5)
    } else if hovered || down {
        hover_color
    } else {
        color
    };
    draw_text(
        ui,
        rect,
        label,
        ADD_BUTTON_FONT,
        text_color,
        egui::Align2::CENTER_CENTER,
    );
    if enabled {
        response.clicked()
    } else {
        let _ = response.on_hover_cursor(egui::CursorIcon::NotAllowed);
        false
    }
}

/// 「＋ 添加目录…」按钮（高 28、圆角 7、12pt；宽随文案）。
/// not busy 时点击 → [`SettingsAction::AddExclude`]；busy 时置灰/禁用。
fn draw_add_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    busy: bool,
    palette: &SettingsPalette,
) -> Option<SettingsAction> {
    let response = ui.interact(
        rect,
        ui.scope_id().with("settings-add-dir"),
        if busy {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        },
    );
    let (fill, text_color) = if busy {
        (
            palette.add_button_fill.gamma_multiply(0.5),
            palette.add_button_text.gamma_multiply(0.5),
        )
    } else if response.is_pointer_button_down_on() {
        (
            palette.add_button_fill.gamma_multiply(0.8),
            palette.add_button_text,
        )
    } else if response.hovered() {
        (
            palette.add_button_fill.gamma_multiply(1.4),
            palette.add_button_text,
        )
    } else {
        (palette.add_button_fill, palette.add_button_text)
    };
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(ADD_BUTTON_RADIUS), fill);
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        i18n::s().add_folder,
        ui_font::id(ADD_BUTTON_FONT),
        text_color,
    );
    if busy {
        let _ = response.on_hover_cursor(egui::CursorIcon::NotAllowed);
        None
    } else {
        response.clicked().then_some(SettingsAction::AddExclude)
    }
}

/// 目录行（40）：文件夹图标 16 + 路径 13pt + 右侧移除 ×14。
/// 移除 × 点击 → [`SettingsAction::RemoveExclude`]（原样路径）；busy 时禁用。
fn draw_dir_row(
    ui: &mut egui::Ui,
    row: egui::Rect,
    index: usize,
    path: &str,
    busy: bool,
    palette: &SettingsPalette,
) -> Option<SettingsAction> {
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
        if busy {
            egui::Sense::hover()
        } else {
            egui::Sense::click()
        },
    );
    let response = if busy {
        response.on_hover_cursor(egui::CursorIcon::NotAllowed)
    } else {
        response
    };
    let color = if busy {
        palette.remove_icon.gamma_multiply(0.5)
    } else if response.hovered() || response.is_pointer_button_down_on() {
        palette.text_primary
    } else {
        palette.remove_icon
    };
    draw_remove_icon(&painter, remove, color);
    (!busy && response.clicked()).then(|| SettingsAction::RemoveExclude(path.to_owned()))
}

/// 排除目录空态（真实列表为空时显示；加载/错误提示可与之并存）。
fn draw_excluded_empty(ui: &egui::Ui, row: egui::Rect, palette: &SettingsPalette) {
    draw_text(
        ui,
        row.shrink2(egui::vec2(ROW_PAD_X, 0.0)),
        i18n::s().excluded_empty,
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
    use crate::hotkey::{HotkeySlot, HotkeySnapshot};
    use crate::ui::font as ui_font_module;
    use crate::ui::settings_tokens::{SETTINGS_H, SETTINGS_W};
    use global_hotkey::hotkey::{Code, Modifiers};

    /// headless 布局数据:默认键帽（经快照填充） + 三项目录（不触 Shared）。
    fn layout_test_data() -> SettingsData {
        let hotkeys = HotkeySnapshot {
            palette: Some(HotKey::new(Some(Modifiers::ALT), Code::Space)),
            window: Some(HotKey::new(
                Some(Modifiers::ALT | Modifiers::SHIFT),
                Code::KeyF,
            )),
            palette_keys: vec!["⌥".into(), "Space".into()],
            window_keys: vec!["⌥".into(), "⇧".into(), "F".into()],
            note: None,
            window_note: None,
        };
        SettingsData {
            font_size: FontSize::Standard,
            autostart: false,
            autostart_approval: false,
            autostart_error: None,
            font_error: None,
            language: LanguageSetting::System,
            language_error: None,
            hotkeys,
            recording: None,
            hotkey_error: None,
            excluded: vec!["~/Library".into(), "node_modules".into(), ".git".into()],
            exclude_busy: false,
            exclude_busy_list: false,
            exclude_error: None,
        }
    }

    use crate::i18n;

    fn click_language(ctx: &egui::Context, data: &SettingsData, label: &str) -> BodyOutput {
        let (out, _) = run_frame(ctx, data, Vec::new());
        let field = keycap_field_center(&out, i18n::s().row_language);
        let _ = click_at(ctx, data, field);
        let (out, _) = run_frame(ctx, data, Vec::new());
        let pos = text_center(find_text(&all_texts(&out), label));
        click_at(ctx, data, pos)
    }

    #[test]
    fn language_row_exposes_three_options_and_action() {
        i18n::with_lang(i18n::Lang::En, || {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let data = layout_test_data();
            let out = click_language(&ctx, &data, "English");
            assert!(
                out.actions
                    .contains(&SettingsAction::SetLanguage(i18n::LanguageSetting::En)),
                "点击 English 应产生 SetLanguage(En): {:?}",
                out.actions
            );
        });
    }

    #[test]
    fn language_save_error_renders_one_extra_line() {
        let data = SettingsData {
            language: i18n::LanguageSetting::En,
            language_error: Some(LanguageError::SaveFailed),
            ..layout_test_data()
        };
        let out = run_body(&data);
        let texts = all_texts(&out);
        find_text(&texts, "语言已切换,但设置保存失败");
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

    fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    /// 跑一帧按键事件；返回动作 + 帧末是否仍有按键事件（消费断言）。
    fn run_key(ctx: &egui::Context, data: &SettingsData, event: egui::Event) -> (BodyOutput, bool) {
        run_events(ctx, data, vec![event])
    }

    /// 跑一帧任意事件；`leftover` = 帧末仍残留录制输入(按键/剪贴板/文本)事件。
    fn run_events(
        ctx: &egui::Context,
        data: &SettingsData,
        events: Vec<egui::Event>,
    ) -> (BodyOutput, bool) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(SETTINGS_W, SETTINGS_H),
            )),
            events,
            ..Default::default()
        };
        let mut body = None;
        let mut leftover = false;
        let mut out = ctx.run_ui(input, |ui| {
            body = Some(draw_body(ui, data));
            leftover = ui.ctx().input(|i| {
                i.events.iter().any(|e| {
                    matches!(
                        e,
                        egui::Event::Key { .. }
                            | egui::Event::Copy
                            | egui::Event::Cut
                            | egui::Event::Paste(_)
                            | egui::Event::PasteImage(_)
                            | egui::Event::Text(_)
                    )
                })
            });
        });
        out.textures_delta.clear();
        (body.expect("draw_body 未运行"), leftover)
    }

    /// 键帽字段中心（字段固定宽 152，右对齐；y = 同行标签中心）。
    fn keycap_field_center(out: &egui::FullOutput, label: &str) -> egui::Pos2 {
        let texts = all_texts(out);
        let label = find_text(&texts, label);
        egui::pos2(
            SETTINGS_W - CONTENT_PAD.1 - ROW_PAD_X - FIELD.0 / 2.0,
            text_center(label).y,
        )
    }

    fn click_keycap(ctx: &egui::Context, data: &SettingsData, label: &str) -> BodyOutput {
        let (out, _) = run_frame(ctx, data, Vec::new());
        click_at(ctx, data, keycap_field_center(&out, label))
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
            "恢复默认",
            "小",
            "标准",
            "大",
            "跟随系统",
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
            labels.contains(&i18n::s().excluded_empty),
            "空列表须显示 {:?};实际 {labels:?}",
            i18n::s().excluded_empty
        );
    }

    /// 添加按钮中心（按钮宽随文案，文字中心即按钮中心）。
    fn add_button_center(out: &egui::FullOutput) -> egui::Pos2 {
        text_center(find_text(&all_texts(out), i18n::s().add_folder))
    }

    /// 目录行右侧移除 × 中心（行几何与路径文本同排）。
    fn remove_button_center(out: &egui::FullOutput, path: &str) -> egui::Pos2 {
        let row = text_center(find_text(&all_texts(out), path));
        egui::pos2(
            SETTINGS_W - CONTENT_PAD.1 - ROW_PAD_X - REMOVE_ICON / 2.0,
            row.y,
        )
    }

    /// 文字按钮中心（文字在按钮矩形内居中，故文本中心即点击点）。
    fn text_button_center(out: &egui::FullOutput, label: &str) -> egui::Pos2 {
        text_center(find_text(&all_texts(out), label))
    }

    /// 点击「＋ 添加目录…」→ AddExclude（选择器/worker 由设置 pass 落地）。
    #[test]
    fn add_button_click_emits_add_exclude() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = layout_test_data();
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let pos = add_button_center(&out);
        let output = click_at(&ctx, &data, pos);
        assert_eq!(output.actions, vec![SettingsAction::AddExclude]);
    }

    /// 点击「恢复默认」→ ResetExcludes（原生确认弹窗由设置 pass 在锁外弹出）；
    /// 按钮位于添加按钮左侧，点击 Header 标签等非控件区域不产生动作。
    #[test]
    fn reset_button_click_emits_reset_action() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = layout_test_data();
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let reset = text_button_center(&out, i18n::s().restore_defaults);
        let add = add_button_center(&out);
        assert!(
            reset.x < add.x,
            "恢复默认须在添加按钮左侧:reset {reset:?}, add {add:?}"
        );

        let output = click_at(&ctx, &data, reset);
        assert_eq!(output.actions, vec![SettingsAction::ResetExcludes]);

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let label = text_center(find_text(&all_texts(&out), "排除目录"));
        let output = click_at(&ctx, &data, label);
        assert!(
            output.actions.is_empty(),
            "Header 标签不得产生动作:{:?}",
            output.actions
        );
    }

    /// busy 时恢复默认禁点（添加/移除已由 `busy_disables_add_and_remove_actions`
    /// 覆盖）；确认弹窗只在动作落地时出现，不是视图状态。
    #[test]
    fn busy_disables_reset_action() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut data = layout_test_data();
        data.exclude_busy = true;
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let reset = text_button_center(&out, i18n::s().restore_defaults);
        let output = click_at(&ctx, &data, reset);
        assert!(
            output.actions.is_empty(),
            "busy 时恢复默认不得产生动作:{:?}",
            output.actions
        );
    }

    /// 点击目录行移除 × → RemoveExclude(原文路径)。
    #[test]
    fn remove_click_emits_exact_path() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = layout_test_data();
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let pos = remove_button_center(&out, "node_modules");
        let output = click_at(&ctx, &data, pos);
        assert_eq!(
            output.actions,
            vec![SettingsAction::RemoveExclude("node_modules".to_owned())]
        );
    }

    /// busy 时添加/移除均禁点（无动作；视觉置灰/NotAllowed 由绘制分支保证）。
    #[test]
    fn busy_disables_add_and_remove_actions() {
        let mut data = layout_test_data();
        data.exclude_busy = true;

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let add = add_button_center(&out);
        let remove = remove_button_center(&out, "~/Library");

        let output = click_at(&ctx, &data, add);
        assert!(
            output.actions.is_empty(),
            "busy 时添加不得产生动作:{:?}",
            output.actions
        );
        let output = click_at(&ctx, &data, remove);
        assert!(
            output.actions.is_empty(),
            "busy 时移除不得产生动作:{:?}",
            output.actions
        );
    }

    /// 加载/重扫提示：list →「加载中…」，add/remove →「重扫中…」；均增高 Header。
    #[test]
    fn busy_support_line_distinguishes_list_and_rescan() {
        let baseline = run_body(&layout_test_data());
        let baseline_dir = text_center(find_text(&all_texts(&baseline), "~/Library")).y;

        let mut data = layout_test_data();
        data.exclude_busy = true;
        data.exclude_busy_list = true;
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(
            labels.iter().any(|text| text == i18n::s().exclude_loading),
            "{labels:?}"
        );
        assert!(!labels
            .iter()
            .any(|text| text == i18n::s().exclude_rescanning));
        let list_dir = text_center(find_text(&texts, "~/Library")).y;
        assert!(
            list_dir > baseline_dir + 10.0,
            "加载提示须计入 Header 行高:基线 {baseline_dir},加载 {list_dir}"
        );

        data.exclude_busy_list = false;
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(
            labels
                .iter()
                .any(|text| text == i18n::s().exclude_rescanning),
            "{labels:?}"
        );
        assert!(!labels.iter().any(|text| text == i18n::s().exclude_loading));
        let rescan_dir = text_center(find_text(&texts, "~/Library")).y;
        assert!(rescan_dir > baseline_dir + 10.0);
    }

    /// 错误提示与 busy/空态可并存：错误文案、重扫提示、「未排除任何目录」同帧可见，
    /// 两行提示均计入 Header 高度（空态行下移）。
    #[test]
    fn exclude_error_and_busy_draw_with_empty_state() {
        let mut baseline = layout_test_data();
        baseline.excluded.clear();
        let baseline_out = run_body(&baseline);
        let baseline_empty = text_center(find_text(
            &all_texts(&baseline_out),
            i18n::s().excluded_empty,
        ))
        .y;

        let mut data = layout_test_data();
        data.excluded.clear();
        data.exclude_busy = true;
        data.exclude_busy_list = false;
        data.exclude_error = Some(ExcludeError::Daemon(
            "daemon 未在运行;排除变更需引擎在线".into(),
        ));
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(
            labels.iter().any(|text| text.contains("daemon 未在运行")),
            "daemon 原始错误须原样绘制:{labels:?}"
        );
        assert!(labels
            .iter()
            .any(|text| text == i18n::s().exclude_rescanning));
        assert!(
            labels.iter().any(|text| text == i18n::s().excluded_empty),
            "空列表 + busy/error 仍须显示空态:{labels:?}"
        );
        let empty_y = text_center(find_text(&texts, i18n::s().excluded_empty)).y;
        assert!(
            empty_y > baseline_empty + 20.0,
            "错误 + 重扫两行提示须增高 Header:基线 {baseline_empty},当前 {empty_y}"
        );
    }

    /// 行内错误行参与卡片高度:字号保存失败会下移后续分组，错误文案可见。
    #[test]
    fn font_error_row_adds_card_height() {
        let baseline = run_body(&layout_test_data());
        let baseline_general = text_center(find_text(&all_texts(&baseline), "通用")).y;

        let mut data = layout_test_data();
        data.font_error = Some(SettingsError::SaveFailed);
        let with_error = run_body(&data);
        let texts = all_texts(&with_error);
        let error_general = text_center(find_text(&texts, "通用")).y;
        assert!(
            error_general > baseline_general + 10.0,
            "字号错误行必须增高外观卡片:基线 {baseline_general},错误 {error_general}"
        );
        find_text(&texts, i18n::s().settings_save_failed);
    }

    /// 自启错误行同理:增高通用卡片（「语言」行随之下移），错误文案须实际绘制。
    #[test]
    fn autostart_error_row_adds_card_height_and_draws_text() {
        let baseline = run_body(&layout_test_data());
        let baseline_language = text_center(find_text(&all_texts(&baseline), "语言")).y;

        let mut data = layout_test_data();
        data.autostart_error = Some(AutostartError::SetFailed);
        let with_error = run_body(&data);
        let texts = all_texts(&with_error);
        let error_language = text_center(find_text(&texts, "语言")).y;
        assert!(
            error_language > baseline_language + 10.0,
            "自启错误行必须增高通用卡片:基线 {baseline_language},错误 {error_language}"
        );
        find_text(&texts, i18n::s().autostart_set_failed);
    }

    /// 待审批：开关仍为关态，行内显示「已注册,待系统设置「登录项」允许」。
    #[test]
    fn autostart_approval_note_is_drawn() {
        let mut data = layout_test_data();
        data.autostart_approval = true;
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();
        assert!(
            labels.contains(&i18n::s().autostart_approval_note),
            "审批提示须绘制:实际 {labels:?}"
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

    fn recording_data(slot: HotkeySlot) -> SettingsData {
        let mut data = layout_test_data();
        data.recording = Some(slot);
        data
    }

    /// 空闲点击两个键帽字段 → 各槽 Begin（字段 id 按槽区分，不互相抢占）。
    #[test]
    fn keycap_click_emits_begin_for_each_slot() {
        let data = layout_test_data();

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        assert_eq!(
            click_keycap(&ctx, &data, "唤起快捷键").actions,
            vec![SettingsAction::BeginHotkeyRecord(HotkeySlot::Palette)]
        );

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        assert_eq!(
            click_keycap(&ctx, &data, "完整窗口快捷键").actions,
            vec![SettingsAction::BeginHotkeyRecord(HotkeySlot::Window)]
        );
    }

    /// 录制本槽：字段显示「按下快捷键…」、旧键帽隐藏，行下显示可见的 Esc 提示。
    #[test]
    fn recording_field_shows_prompt_and_visible_hint() {
        let baseline = run_body(&layout_test_data());
        let baseline_window = text_center(find_text(&all_texts(&baseline), "完整窗口快捷键")).y;

        let data = recording_data(HotkeySlot::Palette);
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(
            labels.iter().any(|text| text == i18n::s().recording_prompt),
            "录制中须显示 {:?}:实际 {labels:?}",
            i18n::s().recording_prompt
        );
        assert!(
            labels.iter().any(|text| text == i18n::s().recording_hint),
            "录制提示（Esc/点击别处）须可见:实际 {labels:?}"
        );
        assert!(
            i18n::s().recording_hint.contains("Backspace"),
            "录制提示须告知 Backspace 恢复默认:{}",
            i18n::s().recording_hint
        );
        assert!(
            !labels.iter().any(|text| text == "Space"),
            "录制中不得再画本槽旧键帽（另一槽仍显示）"
        );
        let window_y = text_center(find_text(&texts, "完整窗口快捷键")).y;
        assert!(
            window_y > baseline_window + 10.0,
            "可见提示须计入行高:基线 {baseline_window},录制 {window_y}"
        );
    }

    /// 点击录制中的字段 = 取消；按下帧不提前取消（同一操作只产生一条取消）。
    #[test]
    fn recording_click_cancels_only_on_release() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (out, _) = run_frame(&ctx, &data, Vec::new());
        let field = keycap_field_center(&out, "唤起快捷键");

        let pressed = run_frame(
            &ctx,
            &data,
            vec![
                egui::Event::PointerMoved(field),
                pointer_button(field, true),
            ],
        )
        .1;
        assert!(
            pressed.actions.is_empty(),
            "录制字段按下不得提前取消（抬起时统一取消）:{:?}",
            pressed.actions
        );

        let released = run_frame(
            &ctx,
            &data,
            vec![
                egui::Event::PointerMoved(field),
                pointer_button(field, false),
            ],
        )
        .1;
        assert_eq!(released.actions, vec![SettingsAction::CancelHotkeyRecord]);
    }

    /// 点击录制槽之外 → 取消；点击另一键帽字段 → 开始新槽（设置 pass 先取消旧槽）。
    #[test]
    fn click_away_cancels_and_other_field_starts_new_slot() {
        let data = recording_data(HotkeySlot::Palette);

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        run_frame(&ctx, &data, Vec::new());
        let away = egui::pos2(120.0, 300.0);
        let pressed = run_frame(
            &ctx,
            &data,
            vec![egui::Event::PointerMoved(away), pointer_button(away, true)],
        )
        .1;
        assert_eq!(
            pressed.actions,
            vec![SettingsAction::CancelHotkeyRecord],
            "点击录制字段之外须取消"
        );

        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        assert_eq!(
            click_keycap(&ctx, &data, "完整窗口快捷键").actions,
            vec![SettingsAction::BeginHotkeyRecord(HotkeySlot::Window)],
            "另一字段点击应开始该槽录制（取消由设置 pass 补入队）"
        );
    }

    /// 就绪录制：⌥K → 提交动作；按键事件被消费。
    #[test]
    fn recorded_key_commits_and_is_consumed() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (output, leftover) =
            run_key(&ctx, &data, key_event(egui::Key::K, egui::Modifiers::ALT));
        let expected = hotkey_keys::from_egui(egui::Key::K, egui::Modifiers::ALT).unwrap();
        assert_eq!(
            output.actions,
            vec![SettingsAction::CommitHotkey {
                slot: HotkeySlot::Palette,
                hotkey: expected,
            }]
        );
        assert!(!leftover, "录制按键必须被消费，不得触发其它 UI");
    }

    /// Esc → 取消；按键事件被消费。
    #[test]
    fn escape_cancels_recording_and_is_consumed() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Window);
        let (output, leftover) = run_key(
            &ctx,
            &data,
            key_event(egui::Key::Escape, egui::Modifiers::NONE),
        );
        assert_eq!(output.actions, vec![SettingsAction::CancelHotkeyRecord]);
        assert!(!leftover, "Esc 须被录制消费");
    }

    /// Backspace（兼容 Forward Delete）在录制中 = 恢复内置默认；不得当成
    /// 可录制键报错。
    #[test]
    fn delete_and_backspace_reset_default_and_are_consumed() {
        for key in [egui::Key::Delete, egui::Key::Backspace] {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let data = recording_data(HotkeySlot::Window);
            let (output, leftover) = run_key(&ctx, &data, key_event(key, egui::Modifiers::NONE));
            assert_eq!(
                output.actions,
                vec![SettingsAction::ResetHotkey(HotkeySlot::Window)],
                "{key:?} 应恢复默认"
            );
            assert!(!leftover, "{key:?} 事件必须被录制消费");
        }
    }

    /// ⌥Space 的文本事件兜底：ASCII 空格 / 选项键 NBSP 均提交 ⌥Space。
    #[test]
    fn alt_space_text_events_commit_space() {
        let expected = hotkey_keys::from_egui(egui::Key::Space, egui::Modifiers::ALT).unwrap();
        for text in [" ", "\u{00A0}"] {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let data = recording_data(HotkeySlot::Palette);
            let (output, leftover) = run_events(
                &ctx,
                &data,
                vec![
                    egui::Event::ModifiersChanged(egui::Modifiers::ALT),
                    egui::Event::Text(text.to_owned()),
                ],
            );
            assert_eq!(
                output.actions,
                vec![SettingsAction::CommitHotkey {
                    slot: HotkeySlot::Palette,
                    hotkey: expected,
                }],
                "{text:?} 应兜底录制 ⌥Space"
            );
            assert!(!leftover, "文本事件必须被录制消费");
        }
    }

    /// 若 Key 与 Text 同帧到达，只提交一次（按键优先，文本兜底不重复）。
    #[test]
    fn alt_space_key_and_text_capture_once() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (output, leftover) = run_events(
            &ctx,
            &data,
            vec![
                egui::Event::ModifiersChanged(egui::Modifiers::ALT),
                key_event(egui::Key::Space, egui::Modifiers::ALT),
                egui::Event::Text("\u{00A0}".to_owned()),
            ],
        );
        let expected = hotkey_keys::from_egui(egui::Key::Space, egui::Modifiers::ALT).unwrap();
        assert_eq!(
            output.actions,
            vec![SettingsAction::CommitHotkey {
                slot: HotkeySlot::Palette,
                hotkey: expected,
            }],
            "同帧 Key + Text 只能提交一次"
        );
        assert!(!leftover);
    }

    /// 无 Alt 的普通空格文本不提交/不报错（录制仍等待），且被消费。
    #[test]
    fn plain_space_text_does_not_commit() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (output, leftover) = run_events(&ctx, &data, vec![egui::Event::Text(" ".to_owned())]);
        assert!(output.actions.is_empty(), "{:?}", output.actions);
        assert!(!leftover, "录制期文本事件须被消费");
    }

    /// 无修饰键 → 内联错误（录制继续，不提交）；事件同样被消费。
    #[test]
    fn invalid_recorded_key_reports_inline_error() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (output, leftover) =
            run_key(&ctx, &data, key_event(egui::Key::K, egui::Modifiers::NONE));
        assert_eq!(
            output.actions,
            vec![SettingsAction::HotkeyError {
                slot: HotkeySlot::Palette,
                message: "需包含修饰键".to_owned(),
            }]
        );
        assert!(!leftover);
    }

    /// 纯修饰键按下只更新修饰态：不报错、不提交，被消费。
    #[test]
    fn modifier_press_is_ignored_during_recording() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (output, leftover) = run_key(
            &ctx,
            &data,
            key_event(egui::Key::ShiftLeft, egui::Modifiers::SHIFT),
        );
        assert!(output.actions.is_empty(), "{:?}", output.actions);
        assert!(!leftover, "修饰键事件也应被录制消费");
    }

    /// 剪贴板事件（⌘C/⌘X/⌘V 被 egui-winit 转成 Copy/Cut/Paste/PasteImage）
    /// 翻译回主键并提交；事件必须被消费。
    #[test]
    fn clipboard_events_record_cmd_c_x_v() {
        let image = std::sync::Arc::new(egui::ColorImage::filled([1, 1], egui::Color32::WHITE));
        let cases = [
            (egui::Event::Copy, egui::Key::C),
            (egui::Event::Cut, egui::Key::X),
            (egui::Event::Paste("剪贴板".into()), egui::Key::V),
            (egui::Event::PasteImage(image), egui::Key::V),
        ];
        for (event, key) in cases {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let data = recording_data(HotkeySlot::Palette);
            let (output, leftover) = run_events(
                &ctx,
                &data,
                vec![
                    egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                    event,
                ],
            );
            let expected = hotkey_keys::from_egui(key, egui::Modifiers::COMMAND).unwrap();
            assert_eq!(
                output.actions,
                vec![SettingsAction::CommitHotkey {
                    slot: HotkeySlot::Palette,
                    hotkey: expected,
                }],
                "{key:?} 剪贴板事件应提交"
            );
            assert!(!leftover, "{key:?} 剪贴板事件必须被消费");
        }
    }

    /// 剪贴板事件不带修饰态：读取 `InputState.modifiers`，Ctrl+Cmd+C 保留 Ctrl。
    #[test]
    fn clipboard_event_uses_current_modifiers() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let modifiers = egui::Modifiers::COMMAND | egui::Modifiers::CTRL;
        let (output, leftover) = run_events(
            &ctx,
            &data,
            vec![egui::Event::ModifiersChanged(modifiers), egui::Event::Copy],
        );
        let expected = hotkey_keys::from_egui(egui::Key::C, modifiers).unwrap();
        assert_eq!(
            output.actions,
            vec![SettingsAction::CommitHotkey {
                slot: HotkeySlot::Palette,
                hotkey: expected,
            }]
        );
        assert!(!leftover);
    }

    /// 剪贴板事件同样走 `from_egui` 校验（无修饰 → 内联错误，不提交）。
    #[test]
    fn clipboard_event_without_modifier_reports_validation_error() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = recording_data(HotkeySlot::Palette);
        let (output, _) = run_events(&ctx, &data, vec![egui::Event::Copy]);
        assert_eq!(
            output.actions,
            vec![SettingsAction::HotkeyError {
                slot: HotkeySlot::Palette,
                message: "需包含修饰键".to_owned(),
            }]
        );
    }

    /// 剪贴板事件命中的组合与另一槽重复 → 本地拒绝。
    #[test]
    fn clipboard_duplicate_is_rejected_inline() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut data = recording_data(HotkeySlot::Palette);
        data.hotkeys.window = Some(HotKey::new(Some(Modifiers::SUPER), Code::KeyC));
        let (output, _) = run_events(
            &ctx,
            &data,
            vec![
                egui::Event::ModifiersChanged(egui::Modifiers::COMMAND),
                egui::Event::Copy,
            ],
        );
        assert_eq!(
            output.actions,
            vec![SettingsAction::HotkeyError {
                slot: HotkeySlot::Palette,
                message: hotkey::DUPLICATE_HOTKEY_MESSAGE.to_owned(),
            }]
        );
    }

    /// 与另一槽相同 → 本地拒绝（内联错误），不产生提交。
    #[test]
    fn duplicate_with_other_slot_is_rejected_inline() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut data = recording_data(HotkeySlot::Palette);
        data.hotkeys.window = Some(HotKey::new(Some(Modifiers::ALT), Code::KeyK));
        let (output, _) = run_key(&ctx, &data, key_event(egui::Key::K, egui::Modifiers::ALT));
        assert_eq!(
            output.actions,
            vec![SettingsAction::HotkeyError {
                slot: HotkeySlot::Palette,
                message: hotkey::DUPLICATE_HOTKEY_MESSAGE.to_owned(),
            }]
        );
    }

    /// 行内错误：增高快捷键行、绘制错误文案，并位于该行与下一行之间。
    #[test]
    fn hotkey_error_row_adds_height_and_draws_text() {
        let baseline = run_body(&layout_test_data());
        let baseline_window = text_center(find_text(&all_texts(&baseline), "完整窗口快捷键")).y;

        let mut data = layout_test_data();
        data.hotkey_error = Some((HotkeySlot::Palette, "需包含修饰键".into()));
        let out = run_body(&data);
        let texts = all_texts(&out);
        let window_y = text_center(find_text(&texts, "完整窗口快捷键")).y;
        assert!(
            window_y > baseline_window + 10.0,
            "快捷键错误必须增高行:基线 {baseline_window},错误 {window_y}"
        );
        let palette_y = text_center(find_text(&texts, "唤起快捷键")).y;
        let error_y = text_center(find_text(&texts, "需包含修饰键")).y;
        assert!(
            error_y > palette_y && error_y < window_y,
            "错误须显示在唤起快捷键行下方(palette {palette_y}, error {error_y}, window {window_y})"
        );
    }

    /// 回落提示按槽绘制：`note` 增高唤起行，`window_note` 增高完整窗口行；
    /// 错误与提示同时存在时各占一行。
    #[test]
    fn hotkey_notes_add_height_and_are_drawn_per_slot() {
        let baseline = run_body(&layout_test_data());
        let baseline_index = text_center(find_text(&all_texts(&baseline), "索引")).y;

        let mut data = layout_test_data();
        data.hotkeys.note = Some("唤起槽回落提示".into());
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(labels.iter().any(|text| text == "唤起槽回落提示"));
        let with_note_index = text_center(find_text(&texts, "索引")).y;
        assert!(
            with_note_index > baseline_index + 10.0,
            "唤起槽提示必须增高行:基线 {baseline_index},提示 {with_note_index}"
        );

        data.hotkeys.window_note = Some("窗口槽回落提示".into());
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(labels.iter().any(|text| text == "窗口槽回落提示"));
        let both_index = text_center(find_text(&texts, "索引")).y;
        assert!(
            both_index > with_note_index + 10.0,
            "窗口槽提示必须再增高一行:单提示 {with_note_index},双提示 {both_index}"
        );

        let mut data = layout_test_data();
        data.hotkey_error = Some((HotkeySlot::Palette, "错误行".into()));
        data.hotkeys.note = Some("提示行".into());
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<String> = texts.iter().map(|t| t.galley.text().to_owned()).collect();
        assert!(labels.iter().any(|text| text == "错误行"));
        assert!(labels.iter().any(|text| text == "提示行"));
        let error_note_index = text_center(find_text(&texts, "索引")).y;
        let mut error_only = layout_test_data();
        error_only.hotkey_error = Some((HotkeySlot::Palette, "错误行".into()));
        let out = run_body(&error_only);
        let error_only_index = text_center(find_text(&all_texts(&out), "索引")).y;
        assert!(
            error_note_index > error_only_index + 10.0,
            "错误 + 提示须各占一行:仅错误 {error_only_index},两者 {error_note_index}"
        );
    }

    /// 键帽展示以快照为准（不再有硬编码默认键）。
    #[test]
    fn keycap_display_comes_from_snapshot() {
        let mut data = layout_test_data();
        data.hotkeys.palette = Some(HotKey::new(
            Some(Modifiers::CONTROL | Modifiers::ALT),
            Code::KeyK,
        ));
        data.hotkeys.palette_keys = vec!["⌃".into(), "⌥".into(), "K".into()];

        let out = run_body(&data);
        let labels: Vec<String> = all_texts(&out)
            .iter()
            .map(|text| text.galley.text().to_owned())
            .collect();
        assert!(labels.iter().any(|text| text == "K"), "{labels:?}");
        assert!(
            !labels.iter().any(|text| text == "Space"),
            "展示须来自快照而非硬编码默认:{labels:?}"
        );
    }

    /// 大字号冒烟:文本仍齐全、布局不 panic（可能整页滚动）。
    #[test]
    fn large_scale_smoke() {
        ui_font_module::with_test_scale(FontSize::Large.scale(), || {
            let out = run_body(&layout_test_data());
            let texts = all_texts(&out);
            let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();
            for expected in ["外观", "通用", "索引", "字号", "跟随系统"] {
                assert!(labels.contains(&expected), "大字号缺少 {expected:?}");
            }
        });
    }
}

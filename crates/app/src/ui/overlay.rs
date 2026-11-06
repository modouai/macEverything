//! 保存面板目录助手浮层视口:输入框 + 目录列表;↑↓/Enter/双击/Esc。
//! 查询复用 `palette::spawn_entry_query_worker`(与悬浮条同一执行模型:防抖会话 +
//! `client::query(dir_only=true)` + mpsc 回投 + 定向唤醒),本模块不再另起工作线程。
//! daemon 离线不弹浮层由接线层守门,本模块只管已弹出后的体验。

use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui;
use protocol::HitEntry;

use client::DaemonPaths;

use crate::i18n;
use crate::session::Session;
use crate::ui::palette;
use crate::ui::tokens;

/// 防抖窗口(与悬浮条同值 60ms)。
const DEBOUNCE: Duration = Duration::from_millis(60);
/// 单次查询上限:浮层列表短,无需滚动加载。
const QUERY_LIMIT: u32 = 200;
/// 浮层列表显示上限(查询侧多取:排除 .app 包后仍有足够普通目录)。
const DISPLAY_LIMIT: usize = 30;

pub(crate) fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("overlay")
}

/// 浮层尺寸(逻辑像素;窗口大小。高度自适应时由平台层按
/// `overlay_layout::overlay_frame` 返回值 setContentSize)。
pub const OVERLAY_W: f32 = 600.0;
pub const OVERLAY_H: f32 = 300.0;
/// 结果行高(两行文本)。
const ROW_H: f32 = 44.0;
/// 搜索行高。
const SEARCH_H: f32 = 50.0;
/// 页脚高。
const FOOTER_H: f32 = 30.0;
/// 面板圆角(视觉原型 v1)。
const PANEL_RADIUS: u8 = 12;
/// 搜索行内边距/图标/间距(视觉原型 v1)。
const SEARCH_PAD_X: f32 = 16.0;
const SEARCH_ICON: f32 = 15.0;
const SEARCH_GAP: f32 = 10.0;
/// 结果行文件夹 glyph 尺寸。
const FOLDER_W: f32 = 24.0;
const FOLDER_H: f32 = 19.0;
/// 页脚左右内边距。
const FOOTER_PAD_X: f32 = 14.0;
/// 字号(视觉原型 v1):查询 15 / 目录名 13 / 次文本与键帽标签 11。
const FONT_QUERY: f32 = 15.0;
const FONT_NAME: f32 = 13.0;
const FONT_META: f32 = 11.0;
const FONT_KEYCAP: f32 = 10.0;

/// 按键决策涉及的键(纯逻辑用;绘制层从 `egui::Key` 映射)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKey {
    Enter,
    Escape,
    ArrowDown,
    ArrowUp,
}

/// 按键决策结果(纯逻辑;IME 组合态下一律 None)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayKeyAction {
    /// 不处理(含 IME 组合/忙、空结果下的回车与方向键)。
    None,
    /// 提交当前选中项。
    Commit,
    /// 取消/关闭浮层。
    Dismiss,
    /// 移动到新选中下标(函数内已 clamp 到 `[0, result_count-1]`)。
    Select(usize),
}

/// 浮层按键决策(纯函数;IME 组合态下一律忽略)。
///
/// 语义:IME 组合中或本帧有 IME 事件 → None;Enter 且 `result_count > 0` → Commit
/// (空结果不提交);Escape → Dismiss;↑↓ 在结果非空时移动到 clamp 后的下标。
pub fn key_action(
    key: OverlayKey,
    selected: usize,
    result_count: usize,
    ime_composing: bool,
    ime_busy: bool,
) -> OverlayKeyAction {
    // 组合中的预编辑/选字键归输入法;选字回车与 ImeEvent::Commit 同帧到达,
    // 该帧整帧让位(与悬浮条同款防护)。
    if ime_composing || ime_busy {
        return OverlayKeyAction::None;
    }
    match key {
        OverlayKey::Enter if result_count > 0 => OverlayKeyAction::Commit,
        OverlayKey::Enter => OverlayKeyAction::None,
        OverlayKey::Escape => OverlayKeyAction::Dismiss,
        OverlayKey::ArrowDown if result_count > 0 => {
            OverlayKeyAction::Select(selected.saturating_add(1).min(result_count - 1))
        }
        OverlayKey::ArrowUp if result_count > 0 => OverlayKeyAction::Select(
            selected
                .saturating_sub(1)
                .min(result_count.saturating_sub(1)),
        ),
        OverlayKey::ArrowDown | OverlayKey::ArrowUp => OverlayKeyAction::None,
    }
}

/// 浮层状态(Shared 持有)。
pub struct OverlayState {
    pub ctx: egui::Context,
    pub paths: DaemonPaths,
    pub visible: bool,
    pub input: String,
    pub results: Vec<HitEntry>,
    pub selected: usize,
    /// 最近一次 Shown 的面板 host pid(接线层 commit 时给 navigator 用)。
    pub panel_pid: Option<i32>,
    /// navigator 回退出口:路径已复制提示(design §5)。
    pub clipboard_hint: bool,
    /// 接线层请求标志(每帧消费后清零)。
    pub commit_requested: bool,
    pub dismiss_requested: bool,
    /// 输入法组合中(候选窗开着):不变更查询,Enter/Esc 归输入法。
    ime_composing: bool,
    /// 本帧有 IME 事件(预编辑/提交):选字回车与提交同帧到达,该帧键位归输入法。
    ime_busy: bool,
    /// 选中项变更后把该行滚入可见区(本帧消费后清零)。
    scroll_to_selected: bool,
    /// 查询会话(防抖 + 代际号唯一真相;在途响应以 `Session::accepts` 判定)。
    session: Session,
    query_tx: Sender<(u64, Vec<HitEntry>)>,
    query_rx: Receiver<(u64, Vec<HitEntry>)>,
}

impl OverlayState {
    pub fn new(ctx: egui::Context, paths: DaemonPaths) -> Self {
        // 通道一次创建、跨查询复用(与悬浮条同型:不在每次查询时新建 channel)。
        let (query_tx, query_rx) = std::sync::mpsc::channel();
        Self {
            ctx,
            paths,
            visible: false,
            input: String::new(),
            results: Vec::new(),
            selected: 0,
            panel_pid: None,
            clipboard_hint: false,
            commit_requested: false,
            dismiss_requested: false,
            ime_composing: false,
            ime_busy: false,
            scroll_to_selected: false,
            session: Session::new(DEBOUNCE),
            query_tx,
            query_rx,
        }
    }

    /// 打开浮层(清空上一次输入/结果/提示,复位全部瞬态标志)。
    pub fn open(&mut self) {
        self.visible = true;
        self.session.reset(); // 推进代际号,作废上一轮在途响应
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.clipboard_hint = false;
        self.commit_requested = false;
        self.dismiss_requested = false;
        self.panel_pid = None;
        self.ime_composing = false;
        self.ime_busy = false;
        self.scroll_to_selected = false;
        // 首帧内容:平台层 orderFront 后须有一次本视口的 pass,否则窗内是旧帧/空白。
        self.ctx.request_repaint_of(overlay_viewport_id());
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.session.reset(); // 关闭后在途响应不得回填
        self.input.clear();
        self.results.clear();
        self.selected = 0;
        self.clipboard_hint = false;
        self.commit_requested = false;
        self.dismiss_requested = false;
        self.panel_pid = None;
        self.ime_composing = false;
        self.ime_busy = false;
        self.scroll_to_selected = false;
    }

    /// 输入变更 → 防抖会话;清空输入立即清结果并作废在途响应。
    pub fn on_input(&mut self, now: Instant) {
        self.session.on_input(&self.input, now);
        if self.input.is_empty() {
            // 空词:Session::on_input 已推进代际号(在途旧代不被接受),这里只清展示。
            self.results.clear();
            self.selected = 0;
        } else {
            // 防抖到期帧(与悬浮条同因:输入帧的立即重绘会吞掉防抖排帧,
            // 此处显式补排,免查询拖到光标闪烁帧才发出)。
            self.ctx
                .request_repaint_after_for(DEBOUNCE, overlay_viewport_id());
        }
    }

    /// 每帧:发起到期查询 + 收回投结果(代际校验)。工作线程复用 palette 共享实现。
    pub fn poll(&mut self, now: Instant) {
        // 在途防抖逐帧补排(根因见 `Session::pending_delay`:同帧 0ms 立即重绘
        // 请求会吞掉防抖请求;不补排则要等下一次偶然重绘才发查询)。
        if let Some(delay) = self.session.pending_delay(now) {
            self.ctx
                .request_repaint_after_for(delay, overlay_viewport_id());
        }
        if let Some(fire) = self.session.pending_at(now) {
            log::info!(
                "[overlay] 发起目录查询 gen={} q={:?}",
                fire.generation,
                fire.query
            );
            // 代际号由 Session 唯一持有;线程启动/错误处理/回投/唤醒统一走 palette
            // 共享 worker(勿在本模块复制)。
            palette::spawn_entry_query_worker(
                self.paths.clone(),
                self.ctx.clone(),
                overlay_viewport_id(),
                self.query_tx.clone(),
                fire,
                QUERY_LIMIT,
                true,
            );
        }
        while let Ok((generation, entries)) = self.query_rx.try_recv() {
            // 以 Session 为唯一真相:清空输入/关闭重开/新查询都会推进其代际号。
            if !self.session.accepts(generation) {
                log::warn!("[overlay] 丢弃旧代结果 gen={generation}");
                continue; // 旧代响应丢弃(慢响应后到不得覆盖新结果)
            }
            log::info!("[overlay] 回填 {} 项 gen={generation}", entries.len());
            // 排除 app 包(bundle 是目录且引擎置顶排序,但不是保存目标);
            // 查询多取、显示截断,避免 app 包挤掉普通目录。
            self.results = entries
                .into_iter()
                .filter(|e| !is_app_bundle(&e.name, &e.path))
                .take(DISPLAY_LIMIT)
                .collect();
            self.selected = 0;
            // 新结果回填 = 新列表:滚动复位到首行(否则清空重搜后视口停在
            // 上次滚到的位置;实机 2026-11-05)。
            self.scroll_to_selected = true;
        }
    }

    /// 当前选中目录完整路径。
    pub fn selected_path(&self) -> Option<String> {
        self.results.get(self.selected).map(|e| e.path.clone())
    }

    /// 输入法组合态跟踪(与悬浮条同款):IME 组合中(候选窗开着)Enter/Esc 属输入法。
    /// 依据 `Event::Ime`:Preedit 非空 = 组合中;Preedit 空 / Commit = 组合结束。
    fn track_ime(&mut self, ui: &egui::Ui) {
        let (composing, busy) = ui.input(|i| {
            let mut composing = self.ime_composing;
            let mut busy = false; // 本帧有 IME 事件(预编辑或提交)
            for ev in &i.events {
                match ev {
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        composing = !text.is_empty();
                        busy = true;
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(_)) => {
                        composing = false;
                        busy = true;
                    }
                    _ => {}
                }
            }
            (composing, busy)
        });
        self.ime_composing = composing;
        self.ime_busy = busy;
    }
}

/// 绘制(ui/mod.rs 的 show_viewport_deferred 回调调用;短锁取 state 后绘制)。
/// 视觉:`docs/specs/overlay-visual/`(原型 v1/v2 定版;不透明材质,色板
/// tokens::overlay = 完整结果窗口色板)。结构:面板 chrome → 搜索行 → 分隔线 →
/// 结果区(两行文本 + 文件夹 glyph + 选中行「↵ 跳转」提示)→ 页脚(结果数 +
/// 键帽组)。键盘:↑↓ 改选中、Enter 置 commit_requested、Esc 置 dismiss_requested;
/// 行点击选中、双击 = 选中 + commit_requested。
pub fn show_overlay(ui: &mut egui::Ui, st: &mut OverlayState) {
    let strings = i18n::s();
    let dark = ui.ctx().theme() == egui::Theme::Dark;
    let t = tokens::overlay(dark);
    st.track_ime(ui);

    // —— 面板 chrome:不透明底 + 描边(窗口 clear_color 全透明,圆角外露桌面)——
    let panel_rect = ui.max_rect();
    ui.painter()
        .rect_filled(panel_rect, egui::CornerRadius::same(PANEL_RADIUS), t.bg);
    ui.painter().rect_stroke(
        panel_rect,
        egui::CornerRadius::same(PANEL_RADIUS),
        egui::Stroke::new(1.0, t.border),
        egui::StrokeKind::Inside,
    );

    // —— 搜索行(高 50;左 16 + 图标 15 + 间距 10;右 16)——
    let (search_rect, _) = ui.allocate_exact_size(
        egui::vec2(panel_rect.width(), SEARCH_H),
        egui::Sense::hover(),
    );
    draw_search_icon(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(
                search_rect.left() + SEARCH_PAD_X + SEARCH_ICON / 2.0,
                search_rect.center().y,
            ),
            egui::vec2(SEARCH_ICON, SEARCH_ICON),
        ),
        t.search_icon,
    );

    // 固定 id + **仅在未聚焦时**请求焦点:egui 的 `request_focus` 文档明确
    // 「Calling this will interrupt IME composition」——每帧调用会每帧打断
    // 中文输入法组合(marked text 被清、选字 Commit 丢失,实机 2026-11-05)。
    // 悬浮条同样只在唤起首帧聚焦(ui/palette.rs),此处对齐。
    let input_id = egui::Id::unique("save-panel-assist-overlay-input");
    if !ui.memory(|m| m.has_focus(input_id)) {
        ui.memory_mut(|m| m.request_focus(input_id));
    }
    let input_rect = egui::Rect::from_min_max(
        egui::pos2(
            search_rect.left() + SEARCH_PAD_X + SEARCH_ICON + SEARCH_GAP,
            search_rect.top(),
        ),
        egui::pos2(search_rect.right() - SEARCH_PAD_X, search_rect.bottom()),
    );
    let input = ui.put(
        input_rect,
        egui::TextEdit::singleline(&mut st.input)
            .id(input_id)
            .font(crate::ui::font::id(FONT_QUERY))
            .text_color(t.query)
            .frame(egui::Frame::NONE)
            .align(egui::Align2::LEFT_CENTER)
            .hint_text(strings.overlay_input_hint),
    );
    // 组合中的预编辑文本不触发查询(IME 未上屏;提交帧 composing 已复位,照常查询)。
    // 实机(2026-11-05):中文输入法输入 ASCII 时首帧有 Preedit 事件把 composing
    // 置 true,后续字符作为普通文本上屏且没有 Commit 事件——沿用式 composing 会
    // 卡死,查询永不触发。此处「无 IME 事件的输入变更」视为上屏完成,复位后查询。
    if input.changed() {
        let preedit = st.ime_composing && st.ime_busy;
        if !preedit {
            if st.ime_composing && !st.ime_busy {
                st.ime_composing = false;
            }
            st.on_input(Instant::now());
        }
    }
    // 已聚焦时不得重复 request_focus(会 interrupt_ime,见上)。
    if !input.has_focus() {
        input.request_focus();
    }

    // 键盘动作:IME 组合/忙时整帧让位输入法;其余交给纯函数决策。
    let (up, down, enter, escape) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::ArrowUp),
            i.key_pressed(egui::Key::ArrowDown),
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    let result_count = st.results.len();
    for (pressed, key) in [
        (up, OverlayKey::ArrowUp),
        (down, OverlayKey::ArrowDown),
        (enter, OverlayKey::Enter),
        (escape, OverlayKey::Escape),
    ] {
        if !pressed {
            continue;
        }
        match key_action(
            key,
            st.selected,
            result_count,
            st.ime_composing,
            st.ime_busy,
        ) {
            OverlayKeyAction::None => {}
            OverlayKeyAction::Commit => {
                st.commit_requested = true;
                // 请求由 ROOT 视口的 App::logic 消费;ROOT 全隐时没有帧,必须主动
                // 请求一次(可能被节流 ≥100ms,但会执行),否则请求永久挂起。
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
            OverlayKeyAction::Dismiss => {
                st.dismiss_requested = true;
                ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
            }
            OverlayKeyAction::Select(index) => {
                if index != st.selected {
                    st.selected = index;
                    st.scroll_to_selected = true; // 本帧把新选中行滚入可见区
                }
            }
        }
    }

    // —— 分隔线 ——
    draw_divider(ui.painter(), panel_rect, search_rect.bottom(), t.divider);

    // —— 页脚(高 30;底缘圆角与面板同半径-1;结果数 + 键帽组)——
    egui::containers::panel::Panel::bottom("ov-footer")
        .frame(
            egui::Frame::new()
                .fill(t.footer_bg)
                .inner_margin(egui::Margin::symmetric(
                    FOOTER_PAD_X as i8,
                    ((FOOTER_H - 18.0) / 2.0) as i8,
                ))
                .corner_radius(egui::CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: PANEL_RADIUS - 1,
                    se: PANEL_RADIUS - 1,
                }),
        )
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                ui.add_space(FOOTER_PAD_X);
                ui.add(egui::Label::new(
                    egui::RichText::new(i18n::format::total_items(st.results.len() as u32))
                        .font(crate::ui::font::id(FONT_META))
                        .color(t.footer_text),
                ));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(FOOTER_PAD_X);
                    key_group(ui, &t, strings.overlay_key_close, "esc");
                    key_group(ui, &t, strings.overlay_key_jump, "⏎");
                    key_group(ui, &t, strings.overlay_key_select, "↑↓");
                });
            });
        });

    // —— 结果区(剩余高度;两行文本行 + 空态)——
    let mut clicked_row = None;
    let mut commit = false;
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            if st.clipboard_hint {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(strings.overlay_clipboard_hint)
                            .font(crate::ui::font::id(FONT_META))
                            .color(t.path),
                    )
                    .truncate(),
                );
                ui.add_space(2.0);
            }
            if st.results.is_empty() {
                // 空输入 = 引导态;有输入无匹配 = 无匹配态。居中 ghost + 文案。
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(14.0);
                        let (ghost, _) =
                            ui.allocate_exact_size(egui::vec2(40.0, 26.0), egui::Sense::hover());
                        ui.painter().rect_stroke(
                            ghost,
                            6.0,
                            egui::Stroke::new(1.5, t.empty_text),
                            egui::StrokeKind::Middle,
                        );
                        ui.add_space(8.0);
                        let text = if st.input.is_empty() {
                            strings.overlay_hint
                        } else {
                            strings.empty_no_match
                        };
                        ui.add(egui::Label::new(
                            egui::RichText::new(text)
                                .font(crate::ui::font::id(FONT_META))
                                .color(t.empty_text),
                        ));
                    });
                });
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, entry) in st.results.iter().enumerate() {
                        let response = draw_row(ui, entry, i == st.selected, &t);
                        // 键盘移动后跟随滚动(与悬浮条同款):选中行必须留在可视区内。
                        if i == st.selected && st.scroll_to_selected {
                            ui.scroll_to_rect(response.rect, None);
                        }
                        if response.clicked() {
                            clicked_row = Some(i);
                        }
                        if response.double_clicked() {
                            clicked_row = Some(i);
                            commit = true;
                        }
                    }
                });
        });
    st.scroll_to_selected = false; // 本帧已消费(点击/查询回填同样不重复滚动)
    if let Some(i) = clicked_row {
        st.selected = i;
    }
    if commit {
        st.commit_requested = true;
        // 双击提交与键盘提交同因:唤醒 ROOT 的 logic 消费请求(见上)。
        ui.ctx().request_repaint_of(egui::ViewportId::ROOT);
    }
}

/// 单行(两行文本;原型 v1 定版):文件夹 glyph + 目录名(主)/完整路径(次);
/// 选中 = 选中底 + 目录名加粗 + 右端「↵ 跳转」提示。整行可点击。
/// 动作提示宽度恒预留:未选行不显示,但文本不因选中切换而回流。
fn draw_row(
    ui: &mut egui::Ui,
    entry: &HitEntry,
    selected: bool,
    t: &tokens::Overlay,
) -> egui::Response {
    // 占位分配(拿 y 与点击响应)。行高**精确锁定** ROW_H:内容(字体缩放/加粗/
    // 选中 chip)再高也只在该矩形内绘制——Frame 自动尺寸会被两行文本撑高,
    // 造成「首行比其他行高」(实机 2026-11-05;悬浮条行同款固定高度模式)。
    let (slot, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click(),
    );
    let rect = egui::Rect::from_min_size(slot.min, egui::vec2(slot.width(), ROW_H));
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    row_ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(8),
        if selected {
            t.selected_bg
        } else {
            egui::Color32::TRANSPARENT
        },
    );
    // 文件夹 glyph(矢量两段;内存优先,不落 NSWorkspace 图标纹理)。
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + 10.0 + FOLDER_W / 2.0, rect.center().y),
        egui::vec2(FOLDER_W, FOLDER_H),
    );
    draw_folder_glyph(row_ui.painter(), icon_rect, t);
    // 文本列(图标右 + 10;选中行右缘预留 chip 宽;上下方向居中由字号自然撑出)。
    let tx = icon_rect.max.x + 10.0;
    let chip_w = text_w(
        &row_ui,
        &format!("⏎ {}", crate::i18n::s().overlay_key_jump),
        FONT_META,
    ) + 12.0;
    let text_right = if selected {
        rect.right() - 10.0 - chip_w
    } else {
        rect.right() - 10.0
    };
    let mut text_ui = row_ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_max(
                egui::pos2(tx, rect.top()),
                egui::pos2(text_right, rect.bottom()),
            ))
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    text_ui.add(
        egui::Label::new(
            egui::RichText::new(&entry.name)
                .font(crate::ui::font::id(FONT_NAME))
                .color(if selected { t.name_selected } else { t.name }),
        )
        .truncate(),
    );
    text_ui.add(
        egui::Label::new(
            egui::RichText::new(&entry.path)
                .font(crate::ui::font::id(FONT_META))
                .color(t.path),
        )
        .truncate(),
    );
    // 选中行的动作提示 chip(右端;画在行矩形内右侧,垂直居中)。
    if selected {
        let chip_rect = egui::Rect::from_center_size(
            egui::pos2(rect.right() - 10.0 - chip_w / 2.0, rect.center().y),
            egui::vec2(chip_w, 20.0),
        );
        row_ui.painter().rect_filled(chip_rect, 5.0, t.action_bg);
        row_ui.painter().text(
            chip_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("⏎ {}", crate::i18n::s().overlay_key_jump),
            crate::ui::font::id(FONT_META),
            t.action_text,
        );
    }
    resp
}

/// 行内文本宽测量(键帽/动作提示排版用;悬浮条 text_w 同款)。
fn text_w(ui: &egui::Ui, text: &str, base: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            crate::ui::font::id(base),
            egui::Color32::WHITE,
        )
        .size()
        .x
}

/// 分隔线(面板全宽 1px)。
fn draw_divider(p: &egui::Painter, panel_rect: egui::Rect, y: f32, color: egui::Color32) {
    p.line_segment(
        [
            egui::pos2(panel_rect.left(), y),
            egui::pos2(panel_rect.right(), y),
        ],
        egui::Stroke::new(1.0, color),
    );
}

/// 文件夹 glyph(矢量两段:后标签 + 前身;macOS 蓝色文件夹示意;零纹理内存)。
fn draw_folder_glyph(p: &egui::Painter, rect: egui::Rect, t: &tokens::Overlay) {
    p.rect_filled(
        egui::Rect::from_min_size(
            rect.left_top() + egui::vec2(1.0, 0.0),
            egui::vec2(11.0, 4.5),
        ),
        egui::CornerRadius::same(2),
        t.folder_back,
    );
    p.rect_filled(
        egui::Rect::from_min_size(
            rect.left_bottom() + egui::vec2(0.0, -15.0),
            egui::vec2(rect.width(), 15.0),
        ),
        egui::CornerRadius::same(3),
        t.folder_front,
    );
}

/// 搜索图标(⌕;悬浮条 §5.2 同款形态,5 行复制以维持模块自洽)。
fn draw_search_icon(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    let c = rect.center() - egui::vec2(1.5, 1.5);
    p.circle_stroke(c, 5.5, stroke);
    p.line_segment([c + egui::vec2(4.0, 4.0), c + egui::vec2(8.0, 8.0)], stroke);
}

/// 键帽组:标签 + 键帽(底:圆角 4;悬浮条页脚同族)。
fn key_group(ui: &mut egui::Ui, t: &tokens::Overlay, label: &str, key: &str) {
    ui.add(egui::Label::new(
        egui::RichText::new(label)
            .font(crate::ui::font::id(FONT_META))
            .color(t.keycap_text),
    ));
    let key_w = text_w(ui, key, FONT_KEYCAP) + 12.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(key_w, 18.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 4.0, t.keycap_bg);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        key,
        crate::ui::font::id(FONT_KEYCAP),
        t.keycap_text,
    );
}

/// app 包判定:目录名以 `.app` 结尾,或路径位于某个 `.app` 包内部(Contents 等)。
/// `.app` 是目录(dir_only 不过滤)且被引擎按「APP 置顶」排序,但保存目标不会是
/// 应用包——浮层结果一律排除(实机:搜索结果被 .app 占满)。
fn is_app_bundle(name: &str, path: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".app") || path.to_ascii_lowercase().contains(".app/")
}

#[cfg(test)]
mod app_bundle_tests {
    use super::is_app_bundle;

    #[test]
    fn filters_app_bundles_by_name_or_ancestor() {
        assert!(is_app_bundle("Safari.app", "/Applications/Safari.app"));
        assert!(is_app_bundle("Xcode.APP", "/Applications/Xcode.APP"));
        assert!(
            is_app_bundle("Contents", "/Applications/Safari.app/Contents"),
            "包内目录也应排除"
        );
        assert!(!is_app_bundle("tmp", "/Users/me/tmp"));
        assert!(!is_app_bundle("appbackup", "/Users/me/appbackup"));
        assert!(
            !is_app_bundle("应用.app备份", "/Users/me/应用.app备份"),
            "非 .app 后缀"
        );
        assert!(!is_app_bundle("文档", "/Users/me/文档"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> OverlayState {
        // 测试不发起真实查询:`poll` 仅在输入非空时才会 spawn;这里只测状态机。
        OverlayState::new(
            egui::Context::default(),
            DaemonPaths {
                data_dir: std::path::PathBuf::new(),
            },
        )
    }

    fn entry(name: &str, path: &str, is_dir: bool) -> HitEntry {
        HitEntry {
            name: name.into(),
            path: path.into(),
            size: None,
            mtime: None,
            is_dir,
        }
    }

    /// 按键决策表:IME 组合/忙整帧让位;空结果不提交/不选中;方向键在函数内 clamp。
    #[test]
    fn key_action_table() {
        const KEYS: [OverlayKey; 4] = [
            OverlayKey::Enter,
            OverlayKey::Escape,
            OverlayKey::ArrowDown,
            OverlayKey::ArrowUp,
        ];
        for key in KEYS {
            assert_eq!(
                key_action(key, 0, 3, true, false),
                OverlayKeyAction::None,
                "{key:?} 组合中一律忽略"
            );
            assert_eq!(
                key_action(key, 0, 3, false, true),
                OverlayKeyAction::None,
                "{key:?} IME 事件帧一律忽略"
            );
        }
        assert_eq!(
            key_action(OverlayKey::Enter, 0, 0, false, false),
            OverlayKeyAction::None,
            "空结果不提交"
        );
        assert_eq!(
            key_action(OverlayKey::Enter, 0, 1, false, false),
            OverlayKeyAction::Commit
        );
        assert_eq!(
            key_action(OverlayKey::Escape, 0, 0, false, false),
            OverlayKeyAction::Dismiss
        );
        assert_eq!(
            key_action(OverlayKey::ArrowDown, 0, 3, false, false),
            OverlayKeyAction::Select(1)
        );
        assert_eq!(
            key_action(OverlayKey::ArrowDown, 2, 3, false, false),
            OverlayKeyAction::Select(2),
            "↓到底 clamp 到 count-1"
        );
        assert_eq!(
            key_action(OverlayKey::ArrowUp, 2, 3, false, false),
            OverlayKeyAction::Select(1)
        );
        assert_eq!(
            key_action(OverlayKey::ArrowUp, 0, 3, false, false),
            OverlayKeyAction::Select(0),
            "↑到顶 clamp 0"
        );
        assert_eq!(
            key_action(OverlayKey::ArrowUp, 5, 3, false, false),
            OverlayKeyAction::Select(2),
            "↑ 时 selected 越界也须 clamp 到 count-1"
        );
        assert_eq!(
            key_action(OverlayKey::ArrowDown, 0, 0, false, false),
            OverlayKeyAction::None,
            "空结果不选中"
        );
    }

    /// 关闭必须作废在途响应,并清空输入、结果、提示与全部瞬态标志。
    #[test]
    fn reopen_resets_and_invalidates_inflight() {
        let mut st = state();
        st.open();
        st.input = "报告".to_owned();
        // 产生一个在途代际(验证 close 后作废)。
        let base = Instant::now();
        st.session.on_input("报告", base);
        let inflight = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        assert!(st.session.accepts(inflight));
        st.results = vec![entry("a", "/a", true)];
        st.selected = 1;
        st.clipboard_hint = true;
        st.commit_requested = true;
        st.dismiss_requested = true;
        st.panel_pid = Some(123);
        st.close();
        assert!(!st.visible);
        assert!(st.input.is_empty());
        assert!(st.results.is_empty());
        assert_eq!(st.selected, 0);
        assert!(!st.clipboard_hint);
        assert!(!st.commit_requested, "关闭须清提交请求");
        assert!(!st.dismiss_requested, "关闭须清取消请求");
        assert_eq!(st.panel_pid, None, "关闭须清面板 pid");
        assert!(!st.session.accepts(inflight), "关闭后旧代响应应作废");
    }

    /// 清空输入立即清结果并推进 Session 代际(旧响应不得回填)。
    #[test]
    fn empty_input_clears_results_and_invalidates() {
        let mut st = state();
        st.open();
        let base = Instant::now();
        st.input = "报".to_owned();
        st.on_input(base);
        // 发起一次查询取得基线代际(直接驱动会话,不 spawn 线程)。
        let baseline = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        assert!(st.session.accepts(baseline), "非空输入不推进代际号");
        // 先预置非空结果/选中:否则「清空」断言在恒空状态下恒真(假钉)。
        st.results = vec![entry("a", "/a", true), entry("b", "/b", true)];
        st.selected = 1;
        st.input.clear();
        st.on_input(base);
        assert!(st.results.is_empty());
        assert_eq!(st.selected, 0);
        assert!(!st.session.accepts(baseline), "清空输入须作废在途响应");
    }

    /// poll 只回填当前代际的响应:过期代际即使后到也不得覆盖当前结果。
    #[test]
    fn poll_discards_stale_generation() {
        let mut st = state();
        st.open();
        // 连发两代查询:第一代在第二代发出后即为过期代。
        let base = Instant::now();
        st.session.on_input("a", base);
        let stale = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        st.session.on_input("b", base);
        let current = st
            .session
            .pending_at(base + DEBOUNCE)
            .expect("窗口到应发起")
            .generation;
        assert_ne!(stale, current);
        // 过期代先到:不得回填。
        st.query_tx
            .send((stale, vec![entry("old", "/old", false)]))
            .unwrap();
        st.poll(base);
        assert!(st.results.is_empty(), "过期代结果不得回填");
        // 当前代随后到:回填并复位选中。
        st.selected = 2;
        st.query_tx
            .send((current, vec![entry("dir", "/dir", true)]))
            .unwrap();
        st.poll(base);
        assert_eq!(st.results.len(), 1);
        assert_eq!(st.results[0].path, "/dir");
        assert_eq!(st.selected, 0, "新结果到达复位选中");
    }

    /// 选中路径取当前行;越界/空结果返回 None。
    #[test]
    fn selected_path_tracks_selection() {
        let mut st = state();
        assert_eq!(st.selected_path(), None);
        st.results = vec![
            HitEntry {
                name: "a".into(),
                path: "/a".into(),
                size: None,
                mtime: None,
                is_dir: true,
            },
            HitEntry {
                name: "b".into(),
                path: "/b".into(),
                size: None,
                mtime: None,
                is_dir: true,
            },
        ];
        assert_eq!(st.selected_path().as_deref(), Some("/a"));
        st.selected = 1;
        assert_eq!(st.selected_path().as_deref(), Some("/b"));
        st.selected = 9;
        assert_eq!(st.selected_path(), None);
    }
}

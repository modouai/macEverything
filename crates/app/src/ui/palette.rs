//! 悬浮条视图:输入即搜(防抖 60ms)+ 结果列表 + 状态角标 + 键位(product §1)。
//!
//! 查询执行模型(定案):UI 线程不做阻塞查询(socket 往返 + 按需 stat 会掉帧)。
//! 会话防抖到期 → 工作线程执行 `client::query` → mpsc 回投 `QueryResult` + 唤醒
//! UI;UI 每帧 try_recv,按代际号 `accept_response` 丢弃旧代(Task 11 语义)。
//! 角标状态取自工作线程顺带执行的 `client::status`(product §7 映射)。
//!
//! 本文件只管「可见可搜可开」;平台行为(跨 Space/浮动级别/失焦隐藏/多屏定位、
//! 唤起隐藏路径)在 `ui/mod.rs` 接线 + `platform/`(Task 14,见模块注释「视口形态」)。
//! 失焦隐藏的判定面(含切 Space 豁免,Task 14b)在本文件 `draw` 内
//! (「失焦隐藏 + 切 Space 豁免」节):失焦边沿 → 宽限期待决 → Space 变更窗口期内豁免。

use eframe::egui;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::platform;
use crate::session::{Fire, Session, SortKey, State};
use client::{DaemonPaths, StatusOutcome};
use protocol::HitEntry;

/// 防抖窗口(tech「查询会话」:60ms)。
pub const DEBOUNCE: Duration = Duration::from_millis(60);
/// 「切 Space 失焦豁免」窗口期(product §1 键位表 2026-10-16 定;实机调参值):
/// Space 变更落在**失焦时刻**之前不超过此值 → 本次失焦视为切 Space 所致。
pub const SPACE_EXEMPT_WINDOW: Duration = Duration::from_millis(500);
/// 失焦待决宽限(实机调参值):失焦首帧不立即定夺,等 Space 变更通知到达后复判。
/// 依据:实机实测(2026-10-16)通知晚于失焦 -76ms ~ +550ms(12 轮样本,6 次带时戳),
/// 按最坏 +550ms 留余量。点击导致的失焦走鼠标键快判、不等满宽限(见 draw)。
pub const FOCUS_HIDE_GRACE: Duration = Duration::from_millis(800);
/// ghost 态(可见但未聚焦)探帧间隔:该态下点击面板外不产生聚焦事件,
/// 靠全局鼠标键状态识别,须低延迟轮询;该态通常只在豁免后的常驻期短暂存在。
pub const GHOST_POLL: Duration = Duration::from_millis(150);
/// 悬浮条窗口宽度(与中央视口创建处的 ViewportBuilder 一致)。
pub const PANEL_W: f32 = 600.0;
/// 初始窗口高度(仅输入行;结果出现后自适应增高)。
pub const INITIAL_H: f32 = 44.0;

/// 首查按协议上限请求一次(product §1 分页语义:不重查、不翻页请求)。
const QUERY_LIMIT: u32 = client::RESULT_CAP;
/// 列表初始渲染行数与滚到底增量(product §1:初始 20 行,滚到底 +20)。
const ROWS_INITIAL: usize = 20;
const ROWS_STEP: usize = 20;
/// 列表区最大高度(行数少时收缩到内容高;超出滚动)。
const LIST_MAX_H: f32 = 322.0;
/// 输入行高度。
const INPUT_H: f32 = 26.0;

/// 工作线程回投的查询结果。
pub struct QueryResult {
    /// 发起时的代际号(`Fire::generation`);UI 侧 `accept_response` 校验。
    pub generation: u64,
    pub payload: Payload,
    /// 引擎状态(daemon 四态 + 客户端侧 offline);None = 取不到。
    pub engine: Option<EngineStatus>,
}

/// 查询产出:命中集或失败(失败分流「引擎不可用」与「查询级错误」)。
pub enum Payload {
    Hits {
        entries: Vec<HitEntry>,
        total: u32,
        elapsed_ms: f64,
        stat_ms: f64,
        /// true = 结果来自落盘索引(daemon 不在),角标标注「离线索引」。
        offline: bool,
    },
    /// 查询失败;engine_down = daemon 不可连(拉起中/未运行)。
    Failed { engine_down: bool },
}

/// 引擎状态(status 子命令数据面;悬浮条角标与完整窗口状态栏共用)。
pub struct EngineStatus {
    pub state: String,
    pub entry_count: u32,
    /// 权限缺口目录数(status.gaps;完整窗口缺口徽标用,悬浮条不显示)。
    pub gaps: u32,
}

/// 绘制产出的动作,交上层(`ui::App`)执行。
pub enum Action {
    /// 隐藏悬浮条(打开成功 / Esc / 失焦)。
    Hide,
    /// ⌥↵:打开完整窗口并带当前查询词(上层消费 `full_window_request` → `ResultsState::set_query`)。
    FullWindow,
}

/// 悬浮条状态与查询会话。
pub struct PaletteState {
    pub session: Session,
    pub input: String,
    pub results: Vec<HitEntry>,
    pub selected: usize,
    pub total: u32,
    pub elapsed_ms: f64,
    pub stat_ms: f64,
    /// 角标文案(§7 六态映射:建索引中/补齐中/重扫中/离线索引/引擎启动中/索引构建中)。
    pub status_note: Option<String>,
    /// 热键提示(降级/不可用;Task 15)。常驻:不随唤起复位、不被查询结果覆盖。
    pub hotkey_note: Option<String>,
    /// 列表渲染行数(初始 20,滚到底 +20,至 `results.len()`)。
    pub visible_rows: usize,
    /// ⌥↵ 置位:完整窗口待带入的查询词(`ui::draw_palette` 转交 `ResultsState::set_query`)。
    pub full_window_request: Option<String>,

    paths: DaemonPaths,
    ctx: egui::Context,
    /// 本面板所属视口(查询线程回投时定向唤醒该视口——隐藏期不误唤醒根视口)。
    viewport: egui::ViewportId,
    tx: mpsc::Sender<QueryResult>,
    rx: mpsc::Receiver<QueryResult>,
    /// 输入法组合中(候选窗开着):Enter/Esc 属输入法,应用不处理。
    ime_composing: bool,
    /// 本帧有 IME 事件(预编辑/提交):该帧键位归输入法(选字回车与提交
    /// 事件同帧到达,须整帧让位)。
    ime_busy: bool,
    /// 唤起后请求输入框焦点(首帧 `request_focus`)。
    needs_focus: bool,
    /// 曾观测到聚焦(之后失焦才隐藏,免启动瞬态误隐藏)。
    seen_focus: bool,
    /// 失焦豁免判定器(Space 变更窗口期 + 消费语义;纯逻辑见 platform::SpaceExemptDecider)。
    space_exempt: platform::SpaceExemptDecider,
    /// 失焦待决起点(Some = 已失焦、等宽限期到期复判;宽限期内的失焦不立即隐藏)。
    focus_pending_since: Option<Instant>,
    /// 上帧聚焦观测(失焦边沿检测:`true→false` 跃迁才进入待决;
    /// 豁免后的常驻未聚焦不再触发,免「切 Space 后数百 ms 又被误隐藏」)。
    last_focused: Option<bool>,
    /// selected 变更后把该行滚入可见区。
    scroll_to_selected: bool,
    /// 已下发的窗口高度(变化才发 InnerSize,防每帧抖动)。
    last_h: Option<f32>,
}

impl PaletteState {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        paths: DaemonPaths,
        viewport: egui::ViewportId,
        hotkey_note: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            session: Session::new(DEBOUNCE),
            input: String::new(),
            results: Vec::new(),
            selected: 0,
            total: 0,
            elapsed_ms: 0.0,
            stat_ms: 0.0,
            status_note: None,
            hotkey_note,
            visible_rows: ROWS_INITIAL,
            full_window_request: None,
            paths,
            ctx: cc.egui_ctx.clone(),
            viewport,
            tx,
            rx,
            ime_composing: false,
            ime_busy: false,
            needs_focus: true,
            seen_focus: false,
            space_exempt: platform::SpaceExemptDecider::new(SPACE_EXEMPT_WINDOW),
            focus_pending_since: None,
            last_focused: None,
            scroll_to_selected: false,
            last_h: None,
        }
    }

    /// 每次唤起/隐藏后复位(product §1:重复唤起居输入清空,每次都是全新查询)。
    pub fn reset(&mut self) {
        self.input.clear();
        self.session.on_input("", Instant::now()); // Idle + 作废在途响应
        self.results.clear();
        self.total = 0;
        self.selected = 0;
        self.visible_rows = ROWS_INITIAL;
        self.status_note = None;
        self.ime_composing = false;
        self.ime_busy = false;
        self.needs_focus = true;
        self.scroll_to_selected = false;
        // 每次唤起重新「武装」失焦门:窗口常驻(隐藏走 orderOut 不销毁),
        // 但唤起瞬间尚未成 key(报 unfocused)时不隐藏,待观察到聚焦后失焦才隐藏。
        self.seen_focus = false;
        // 待决态与边沿基线一并复位(上一轮的失焦已经由隐藏动作了结)。
        self.focus_pending_since = None;
        self.last_focused = None;
        self.last_h = None;
    }

    /// 每帧驱动(悬浮条视口回调 `ui::draw_palette` 调用):防抖到期发查询;
    /// 收工作线程结果(根视口被完全遮挡、`logic` 不跑时仍由此驱动,Task 14)。
    pub fn tick(&mut self, now: Instant) {
        if let Some(fire) = self.session.pending_at(now) {
            self.spawn_query(fire);
        }
        while let Ok(res) = self.rx.try_recv() {
            self.apply(res);
        }
    }

    /// 发起查询:工作线程执行,结果经 mpsc 回投并唤醒悬浮条视口。
    fn spawn_query(&mut self, fire: Fire) {
        spawn_query_worker(
            self.paths.clone(),
            self.ctx.clone(),
            self.viewport,
            self.tx.clone(),
            fire,
            QUERY_LIMIT,
        );
    }

    /// 结果应用:代际校验(旧代丢弃)→ 命中集/失败分支 → 角标重算。
    fn apply(&mut self, res: QueryResult) {
        if !self.session.accept_response(res.generation) {
            return; // Task 11 代际防护:慢响应后到不得覆盖新结果
        }
        match res.payload {
            Payload::Hits {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                offline,
            } => {
                self.results = entries;
                self.total = total;
                self.elapsed_ms = elapsed_ms;
                self.stat_ms = stat_ms;
                self.selected = 0;
                self.visible_rows = ROWS_INITIAL;
                self.scroll_to_selected = false;
                if offline {
                    self.session.mark_offline(); // 结果来自落盘索引
                } else {
                    self.session.mark_total(total); // 0 → Empty;>0 → 有结果
                }
            }
            Payload::Failed { engine_down } => {
                self.results.clear();
                self.total = 0;
                if engine_down {
                    self.session.mark_engine_starting(); // daemon 不可连:引擎未就绪
                } else {
                    self.session.mark_error("query");
                }
            }
        }
        self.status_note = badge_note(&self.session.state, res.engine.as_ref());
    }

    /// 绘制悬浮条;返回交上层的动作。
    pub fn draw(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let ctx = ui.ctx().clone();
        self.track_ime(ui);

        // 键位:先于 TextEdit 消费——TextEdit 读到的事件表不再含这些键
        // (singleline 的 Enter=失焦、方向键=移光标 均在此截住)。
        // IME 组合中或本帧有提交时不消费:该帧键位归输入法——实测选字回车
        // 与提交事件同帧到达(Commit + Key(Enter)),不设防会误触发打开。
        let keys = if self.ime_composing || self.ime_busy {
            Keys::default()
        } else {
            Keys::consume(ui)
        };

        // ↑↓:移动选择(不改变输入);越界 clamp。
        if !self.results.is_empty() {
            if keys.down && self.selected + 1 < self.results.len() {
                self.selected += 1;
                self.scroll_to_selected = true;
            }
            if keys.up && self.selected > 0 {
                self.selected -= 1;
                self.scroll_to_selected = true;
            }
        }

        let note = self.status_note.clone();
        let hotkey_note = self.hotkey_note.clone();
        let input = &mut self.input;
        let mut at_bottom = false;
        let frame = egui::Frame::new()
            .fill(ui.visuals().window_fill)
            .stroke(ui.visuals().window_stroke)
            .corner_radius(egui::CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                // 内容宽 = 窗宽 - 边距 - 描边
                ui.set_width(PANEL_W - 22.0);
                // —— 输入行:输入框占满余宽,角标贴右 ——
                // 定高子区(不可用 with_layout:其子区吃满剩余高度,行被垂直居中
                // → 面板高度翻倍空半屏)。
                let row_w = ui.available_width();
                let input_resp = ui
                    .allocate_ui_with_layout(
                        egui::vec2(row_w, INPUT_H),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if let Some(note) = &note {
                                ui.label(egui::RichText::new(note).small().weak());
                                ui.add_space(6.0);
                            }
                            let w = ui.available_width();
                            ui.add_sized(
                                [w, INPUT_H],
                                egui::TextEdit::singleline(input)
                                    .id_salt("palette-input")
                                    .hint_text("搜索文件…"),
                            )
                        },
                    )
                    .inner;

                // —— 热键提示(Task 15;降级「已改用 X」/不可用;常驻不随唤起复位)——
                if let Some(note) = &hotkey_note {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(note)
                            .small()
                            .color(ui.visuals().warn_fg_color),
                    );
                }

                // —— 结果列表 / 空态 / 计数行 ——
                if self.results.is_empty() {
                    if matches!(self.session.state, State::Empty) {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new("无匹配项").weak());
                    }
                } else {
                    ui.add_space(6.0);
                    let rows = self.results.len().min(self.visible_rows);
                    let results = &self.results;
                    let selected = self.selected;
                    let scroll_sel = self.scroll_to_selected;
                    let mut clicked: Option<usize> = None;
                    let out = egui::ScrollArea::vertical()
                        .max_height(LIST_MAX_H) // 行数少时 auto_shrink 收缩到内容高
                        .id_salt("palette-list")
                        .show(ui, |ui| {
                            for (i, e) in results.iter().take(rows).enumerate() {
                                let is_sel = i == selected;
                                let bg = if is_sel {
                                    ui.visuals().selection.bg_fill
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                let row = egui::Frame::new()
                                    .fill(bg)
                                    .corner_radius(egui::CornerRadius::same(4))
                                    .inner_margin(egui::Margin::symmetric(6, 2))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 8.0;
                                            ui.label(egui::RichText::new(&e.name).strong());
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(dir_display(&e.path))
                                                        .small()
                                                        .weak(),
                                                )
                                                .truncate(),
                                            );
                                        });
                                    });
                                if row.response.interact(egui::Sense::click()).clicked() {
                                    clicked = Some(i);
                                }
                                if is_sel && scroll_sel {
                                    ui.scroll_to_rect(row.response.rect, None);
                                }
                            }
                        });
                    if let Some(i) = clicked {
                        self.selected = i;
                    }
                    // 滚到底 → 本地增量渲染(+20,至 K;不重查)
                    at_bottom = rows < self.results.len()
                        && out.state.clamped_offset().y + out.inner_rect.height()
                            >= out.content_size.y - 4.0;
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "共 {} 项 · 匹配 {:.1} ms · 读取 {:.1} ms",
                            self.total, self.elapsed_ms, self.stat_ms
                        ))
                        .small()
                        .weak(),
                    );
                }
                input_resp
            });

        if at_bottom {
            self.visible_rows += ROWS_STEP;
            ctx.request_repaint();
        }
        self.scroll_to_selected = false;

        let input_resp = frame.inner;
        // 组合中的预编辑文本不触发查询(IME 未上屏;提交/取消帧 composing 已复位,
        // 照常按最终文本查询)。
        if input_resp.changed() && !self.ime_composing {
            let now = Instant::now();
            self.session.on_input(&self.input, now);
            if self.input.is_empty() {
                // 输入清空 = Idle 清空态:立即清结果,不等响应(product §1)
                self.results.clear();
                self.total = 0;
                self.selected = 0;
                self.status_note = None;
            } else {
                // 防抖到期帧(空闲不排帧:隐藏/无输入时 CPU 仍 0%)
                ctx.request_repaint_after(DEBOUNCE);
            }
        }
        if self.needs_focus {
            input_resp.request_focus();
            self.needs_focus = false;
        }

        // 窗口高度随内容自适应(结果行数变化);变化才发,防每帧抖动。
        let h = frame.response.rect.height().max(INPUT_H);
        if self.last_h.is_none_or(|last| (last - h).abs() > 1.0) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(PANEL_W, h)));
            self.last_h = Some(h);
        }

        // —— 键位动作(在 TextEdit 处理完本帧事件之后:⌥↵ 取到最新输入词)——
        let mut action = None;
        if keys.enter {
            action = self.open_selected(false);
        } else if keys.cmd_enter {
            action = self.open_selected(true);
        } else if keys.alt_enter {
            // 打开完整窗口并带当前查询词(上层消费)
            self.full_window_request = Some(self.input.clone());
            action = Some(Action::FullWindow);
        } else if keys.esc {
            action = Some(Action::Hide);
        }

        // —— 失焦隐藏 + 切 Space 豁免(product §1 键位表 2026-10-16 定)——
        // 判定链:失焦(边沿)→ 若「鼠标键按下」即判点击导致、立即隐藏(点击是主动作,
        // 照常隐藏,不等宽限);否则进入待决,宽限后锚失焦时刻复判 Space 变更 → 豁免。
        // 宽限期内鼠标键按下同样立即隐藏(用户已点走)。ghost 态(豁免后的可见未聚焦)
        // 点击面板外也隐藏:该态下点其他应用不产生聚焦事件,只能靠鼠标键状态识别。
        // 边沿检测保证豁免后的常驻未聚焦不被反复起待决。
        let now = Instant::now();
        let focused = ctx.input(|i| i.viewport().focused);
        let lost_edge = self.last_focused == Some(true) && focused == Some(false);
        if focused != self.last_focused {
            log::info!(
                "悬浮条聚焦:{:?} → {focused:?}(t+{}ms)",
                self.last_focused,
                platform::monotonic_ms()
            );
            self.last_focused = focused;
        }
        match focused {
            Some(true) => {
                self.seen_focus = true;
                if self.focus_pending_since.take().is_some() {
                    log::info!("失焦待决解除:已重新聚焦(保持可见)");
                }
            }
            Some(false) if self.seen_focus => match self.focus_pending_since {
                // 仅「true→false」边沿进入待决:豁免后的常驻未聚焦不再触发
                // (否则常态排帧会反复起待决,豁免过一轮后又被隐藏——实机第二轮实证)。
                None if lost_edge => {
                    if platform::mouse_button_down() {
                        log::info!(
                            "失焦隐藏:点击导致(鼠标键按下,t+{}ms)",
                            platform::monotonic_ms()
                        );
                        action = Some(Action::Hide);
                    } else {
                        self.focus_pending_since = Some(now);
                        log::info!(
                            "失焦待决:{}ms 后复判(t+{}ms)",
                            FOCUS_HIDE_GRACE.as_millis(),
                            platform::monotonic_ms()
                        );
                        ctx.request_repaint_after(FOCUS_HIDE_GRACE);
                    }
                }
                None => {
                    // ghost 态:可见未聚焦(豁免后的常驻)。点面板外 = 用户走开 → 隐藏。
                    if platform::mouse_button_down() && !platform::palette_contains_mouse() {
                        log::info!("隐藏:ghost 态点击面板外(t+{}ms)", platform::monotonic_ms());
                        action = Some(Action::Hide);
                    }
                    // 低延迟轮询(mouse down 无事件送达本 app,只能主动查)。
                    if action.is_none() {
                        ctx.request_repaint_after(GHOST_POLL);
                    }
                }
                Some(t0) => {
                    // 待决期内鼠标键按下 = 用户已点走(含点目标 Space 的应用) → 立即隐藏。
                    if platform::mouse_button_down() {
                        log::info!(
                            "失焦隐藏:点击导致(待决期内,t+{}ms)",
                            platform::monotonic_ms()
                        );
                        action = Some(Action::Hide);
                    } else {
                        let elapsed = now.duration_since(t0);
                        if elapsed >= FOCUS_HIDE_GRACE {
                            // 锚在失焦时刻(t0)判:通知通常晚于失焦到达,判定又在通知之后。
                            if self.space_exempt.decide(platform::last_space_change(), t0) {
                                self.focus_pending_since = None;
                                log::info!(
                                    "失焦豁免:切 Space 变更同过渡(t+{}ms)",
                                    platform::monotonic_ms()
                                );
                            } else {
                                log::info!(
                                    "失焦隐藏:非 Space 变更(t+{}ms)",
                                    platform::monotonic_ms()
                                );
                                action = Some(Action::Hide);
                            }
                        } else {
                            // 宽限期内:保持,并把复判帧排到点(早到的帧不丢后续排帧,
                            // 否则复判要等下一轮常态排帧——实机首轮实证:迟 2s)。
                            ctx.request_repaint_after(FOCUS_HIDE_GRACE - elapsed);
                        }
                    }
                }
            },
            _ => {}
        }

        action
    }

    /// 打开选中项:reveal=false → 默认应用打开(目录 = 访达进入);
    /// reveal=true → 访达显示(⌘↵)。成功 → Some(Hide)(product §1:打开后自动隐藏)。
    fn open_selected(&mut self, reveal: bool) -> Option<Action> {
        let path = self.results.get(self.selected)?.path.clone();
        match open_path(&path, reveal) {
            Ok(()) => Some(Action::Hide),
            Err(msg) => {
                // 边界(product §边界):文件已删/打开失败 → 提示,不隐藏、不静默
                self.status_note = Some(msg);
                None
            }
        }
    }

    /// 输入法组合态跟踪:IME 组合中(候选窗开着)Enter/Esc 属输入法,应用不处理。
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

/// 本帧消费的键位(消费后 TextEdit 不再看到;顺序:先具体组合后裸键——
/// `Modifiers::matches_logically` 忽略 Alt,裸 Enter 判定会吞掉 ⌥↵)。
#[derive(Default)]
struct Keys {
    up: bool,
    down: bool,
    enter: bool,
    cmd_enter: bool,
    alt_enter: bool,
    esc: bool,
}

impl Keys {
    fn consume(ui: &egui::Ui) -> Self {
        ui.input_mut(|i| {
            let cmd_enter = i.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter);
            let alt_enter = i.consume_key(egui::Modifiers::ALT, egui::Key::Enter);
            let enter = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            let esc = i.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            let up = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            let down = i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            Self {
                up,
                down,
                enter,
                cmd_enter,
                alt_enter,
                esc,
            }
        })
    }
}

/// 发起一次查询的工作线程(`client::query` + 顺带 `client::status`),结果经 mpsc
/// 回投并定向唤醒 `viewport`。悬浮条与完整窗口共用(勿复制第二份;UI 线程绝不做
/// 阻塞查询——模块头注「查询执行模型」)。结果行至多 `limit` 条(协议上限内)。
pub(crate) fn spawn_query_worker(
    paths: DaemonPaths,
    ctx: egui::Context,
    viewport: egui::ViewportId,
    tx: mpsc::Sender<QueryResult>,
    fire: Fire,
    limit: u32,
) {
    let sort = sort_str(fire.sort);
    let desc = fire.desc;
    std::thread::spawn(move || {
        let payload = match client::query(&paths, &fire.query, limit, sort, desc) {
            Ok(out) => Payload::Hits {
                entries: out.entries,
                total: out.total,
                elapsed_ms: out.elapsed_ms,
                stat_ms: out.stat_ms,
                offline: out.offline_built_at.is_some(),
            },
            Err(err) => {
                // 失败分流:daemon 不可连(引擎拉起中/未运行)与查询级错误,角标不同
                let engine_down = client::connect(&paths).is_none();
                log::warn!("查询失败(engine_down={engine_down}):{err}");
                Payload::Failed { engine_down }
            }
        };
        let engine = match client::status(&paths) {
            Ok(StatusOutcome::Reported(r)) => Some(EngineStatus {
                state: r.state,
                entry_count: r.entry_count,
                gaps: r.gaps.len() as u32,
            }),
            Ok(StatusOutcome::NoIndex) => None,
            Err(err) => {
                log::warn!("引擎状态查询失败:{err}");
                None
            }
        };
        let _ = tx.send(QueryResult {
            generation: fire.generation,
            payload,
            engine,
        });
        // 跨线程唤醒:定向目标视口(悬浮条:根视口隐藏/遮挡时不被根窗节流拖住;
        // 完整窗口:根视口隐藏期 eframe 走 run_logic,状态照常落地,不破隐藏零帧)。
        ctx.request_repaint_of(viewport);
    });
}

/// 默认应用打开(reveal=false → 目录 = 访达进入)或访达显示(reveal=true,`open -R`)。
/// 悬浮条与完整窗口共用(勿复制第二份);失败返回可展示的提示(None 之外不静默)。
pub(crate) fn open_path(path: &str, reveal: bool) -> Result<(), String> {
    if std::fs::metadata(path).is_err() {
        return Err("文件不存在,索引可能待更新".into());
    }
    log::info!(
        "{}:{path}",
        if reveal {
            "访达显示(⌘↵)"
        } else {
            "默认应用打开"
        }
    );
    let mut cmd = std::process::Command::new("open");
    if reveal {
        cmd.arg("-R");
    }
    cmd.arg(path);
    match cmd.spawn() {
        Ok(mut child) => {
            // 收尸 + 失败留痕(open 非零退出 = 系统未打开该文件)
            let path = path.to_string();
            std::thread::spawn(move || match child.wait() {
                Ok(st) if !st.success() => log::warn!("open 退出异常({st}):{path}"),
                Ok(_) => {}
                Err(e) => log::warn!("等待 open 退出失败:{e}"),
            });
            Ok(())
        }
        Err(e) => Err(format!("打开失败:{e}")),
    }
}

/// 会话排序键 → 协议字符串(daemon `parse_sort_key` 接受形态;缺省 = relevance)。
pub(crate) fn sort_str(key: SortKey) -> Option<&'static str> {
    match key {
        SortKey::Relevance => None,
        SortKey::Name => Some("name"),
        SortKey::Size => Some("size"),
        SortKey::Mtime => Some("mtime"),
        SortKey::Path => Some("path"),
    }
}

/// 角标文案(product §7 映射,悬浮条列)。引擎状态优先(更新鲜);会话态兜底。
fn badge_note(state: &State, engine: Option<&EngineStatus>) -> Option<String> {
    if let Some(eng) = engine {
        match eng.state.as_str() {
            client::state::BUILDING => return Some(format!("建索引中 {}", eng.entry_count)),
            client::state::RECONCILING => return Some("补齐中".into()),
            client::state::DEGRADED => return Some("重扫中".into()),
            client::state::OFFLINE => return Some("离线索引".into()),
            _ => {} // ready:无角标;其余看会话态
        }
    }
    match state {
        State::EngineStarting => Some("引擎启动中".into()),
        State::Warming => Some("索引构建中".into()),
        State::Offline => Some("离线索引".into()),
        State::Error(_) => Some("查询失败".into()),
        State::Idle | State::Searching | State::Empty => None,
    }
}

/// 所在目录(展示用):去掉文件名后中间省略。
fn dir_display(path: &str) -> String {
    let dir = match path.rsplit_once('/') {
        Some((d, _)) => d,
        None => path,
    };
    elide_middle(dir, 56)
}

/// 中间省略:超长取「前一半…后一半」(保留首尾信息)。
fn elide_middle(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1) / 2;
    let head: String = s.chars().take(keep).collect();
    let tail: String = s.chars().skip(n - keep).collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elide_middle_keeps_head_and_tail() {
        let long = "/Users/x/这是一个很长的中文目录名字/子目录甲/子目录乙/子目录丙";
        let got = elide_middle(long, 20);
        assert!(got.chars().count() <= 21, "省略后超长:{got}");
        assert!(got.starts_with("/Users/x/"), "首段丢失:{got}");
        assert!(got.ends_with("子目录丙"), "尾段丢失:{got}");
        assert!(got.contains('…'));
        assert_eq!(elide_middle("/a/b", 20), "/a/b", "未超长不省略");
    }

    #[test]
    fn dir_display_strips_file_name() {
        assert_eq!(
            dir_display("/tmp/mce/Documents/季度报告.md"),
            "/tmp/mce/Documents"
        );
        assert_eq!(dir_display("no-slash"), "no-slash", "无分隔符原样");
    }

    #[test]
    fn sort_str_matches_daemon_parse() {
        assert_eq!(sort_str(SortKey::Relevance), None, "缺省 = relevance");
        assert_eq!(sort_str(SortKey::Name), Some("name"));
        assert_eq!(sort_str(SortKey::Size), Some("size"));
        assert_eq!(sort_str(SortKey::Mtime), Some("mtime"));
        assert_eq!(sort_str(SortKey::Path), Some("path"));
    }

    #[test]
    fn badge_note_maps_six_states() {
        let eng = |s: &str| EngineStatus {
            state: s.into(),
            entry_count: 12,
            gaps: 0,
        };
        assert_eq!(
            badge_note(&State::Searching, Some(&eng("building"))).as_deref(),
            Some("建索引中 12")
        );
        assert_eq!(
            badge_note(&State::Searching, Some(&eng("reconciling"))).as_deref(),
            Some("补齐中")
        );
        assert_eq!(
            badge_note(&State::Searching, Some(&eng("degraded"))).as_deref(),
            Some("重扫中")
        );
        assert_eq!(
            badge_note(&State::Searching, Some(&eng("offline"))).as_deref(),
            Some("离线索引")
        );
        assert_eq!(
            badge_note(&State::Searching, Some(&eng("ready"))),
            None,
            "就绪无角标"
        );
        // 引擎状态取不到 → 会话态兜底
        assert_eq!(
            badge_note(&State::EngineStarting, None).as_deref(),
            Some("引擎启动中")
        );
        assert_eq!(
            badge_note(&State::Warming, None).as_deref(),
            Some("索引构建中")
        );
        assert_eq!(
            badge_note(&State::Offline, None).as_deref(),
            Some("离线索引")
        );
        assert_eq!(
            badge_note(&State::Error("query".into()), None).as_deref(),
            Some("查询失败")
        );
        assert_eq!(badge_note(&State::Idle, None), None);
    }
}

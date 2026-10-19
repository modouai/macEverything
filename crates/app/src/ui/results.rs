//! 完整结果窗口(Task 17;product §2):搜索框 + 虚拟化表格(四列/列宽可拖/列头排序)
//! + 多选 + 状态栏骨架。
//!
//! 查询执行模型与悬浮条同构(palette.rs 模块注释「查询执行模型」):UI 线程不做
//! 阻塞查询,`tick`(App::logic 每帧驱动)在防抖到期后发工作线程,响应经 mpsc 回投
//! 并按代际号丢弃旧代。本状态与悬浮条**各自独立会话**(各自防抖/代际;product §8:
//! 悬浮条不排序,列头排序只属完整窗口)。
//!
//! ## 方向语义(引擎-UI 分工,见 engine::query::execute_sorted 注释)
//!
//! 方向进协议(Task 19b):引擎按顺序做「全量命中排序 → 按方向反转 → 取前 K」,
//! 因此 total > K 时取得的是全库真最大/最小 K 条。UI 只持界面意图 `sort_asc`
//! (列头箭头 ▲▼ 所示),发查询时经 `request_desc` 换算为协议 `desc`
//! (desc = 反转自然方向,mtime 自然方向为降序)。**不做本地反转**:截断集反转
//! 在 total > K 时语义崩坏(修复的误差本体)。列头每次点击(同列翻转/异列换键)
//! 都走 `on_sort_change` 同串重查(状态栏「排序中…」)——方向变了必须重查,
//! 响应总是按当前 `sort`/`sort_asc` 落序,状态单一来源。
//!
//! ## 复用(勿复制第二份)
//!
//! 查询消息类型与工作线程(`palette::{QueryResult, Payload, EngineStatus,
//! spawn_query_worker}`)、排序键 → 协议串(`palette::sort_str`)、打开动作
//! (`palette::open_path`)自 palette 复用。Task 18(原生右键菜单)已接:右键行 →
//! 标准表格语义收敛选中 → 原生 NSMenu(`platform::popup_menu`,阻塞弹出)→
//! 动作执行(`ui/menu_actions.rs`)。
//!
//! ## Task 19 状态栏补全(缺口面板 / sort_echo / 重启引擎)
//!
//! - 三段:计数(总数/耗时) · 索引状态(§7 完整窗口列,离线带生成时刻) · 缺口徽标;
//! - 缺口徽标(Task 4 视觉:圆角 6 + 自绘警示三角 +「N 个目录未收录」,窗口内
//!   不使用托盘 `tray::gaps_label` 的「⚠」前缀,文案本体与托盘同源)→ 点击展开
//!   面板:逐行「目录 + 原因」,数据 = status.gaps(与 CLI `status` 同源,
//!   展示口径同款:最多 20 行 + 余量计数);
//! - sort_echo 校验:请求了非 relevance 键而回显为空(旧 daemon)或异键 → 该响应
//!   的排序未生效,状态栏红字 +「重启引擎」按钮(stop → start,`client::stop` +
//!   `engine_launcher::ensure_engine`);重启完成清提示并按当前列/方向重查;
//! - 「排序中…」= `sorting_inflight`(fire 置位、当代响应落地清除,见 `apply`)。

use eframe::egui;
use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::Instant;

use crate::engine_launcher;
use crate::i18n;
use crate::i18n::format;
use crate::platform;
use crate::session::{Session, SortKey, State};
use crate::ui::font as ui_font;
use crate::ui::icons::{self, IconCache};
use crate::ui::menu_actions;
use crate::ui::palette::{self, EngineStatus, Payload, QueryResult};
use crate::ui::window_tokens::{self, FullWindowPalette};
use client::DaemonPaths;
use protocol::{Gap, HitEntry};

/// 查询上限(与悬浮条同口径:协议上限一次取满;product §2「展示前 K 条」的 K)。
const QUERY_LIMIT: u32 = client::RESULT_CAP;

/// 表头列(顺序与 product §2 一致:名称 / 大小 / 修改时间 / 路径)。
/// 文案在绘制时按当前语言取(见 `column_label`)。
const COLUMNS: [SortKey; 4] = [SortKey::Name, SortKey::Size, SortKey::Mtime, SortKey::Path];

/// 列索引(与 `COLUMNS` 同序;几何函数按此取位)。
const COL_NAME: usize = 0;
const COL_SIZE: usize = 1;
const COL_MTIME: usize = 2;
const COL_PATH: usize = 3;

/// 名称列弹性宽下限(§3.4「名称弹性」;窄窗不得挤没名称)。
const NAME_MIN_W: f32 = 84.0;
/// 大小 / 修改时间 / 路径列最小宽(与旧表同口径;拖拽 clamp 用)。
const SIZE_MIN_W: f32 = 60.0;
const MTIME_MIN_W: f32 = 110.0;
const PATH_MIN_W: f32 = 120.0;
/// 列边界拖拽热区半宽(§3.4「边界 ±4」)。
const DRAG_HOT_ZONE: f32 = 4.0;

/// 状态栏瞬时提示(绘制时按当前语言渲染)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusNote {
    EngineNotReady,
    QueryFailed,
    /// 原样展示的文本：外部/技术细节，或已在构造处按当前语言拼好细节的提示
    /// （如打开失败 `OpenError::text()`、重启失败前缀）。
    Raw(String),
}

impl StatusNote {
    fn text(&self) -> String {
        match self {
            Self::EngineNotReady => i18n::s().status_engine_not_ready.to_owned(),
            Self::QueryFailed => i18n::s().status_query_failed.to_owned(),
            Self::Raw(text) => text.clone(),
        }
    }
}

/// 完整窗口状态与查询会话(与悬浮条独立)。
pub struct ResultsState {
    pub session: Session,
    pub results: Vec<HitEntry>,
    pub total: u32,
    pub elapsed_ms: f64,
    pub stat_ms: f64,
    /// 当前排序键(初始 = Relevance:默认排序 = 匹配质量,product §2)。
    pub sort: SortKey,
    /// 界面展示方向(true = 该列升序;列头箭头 ▲▼ 依据)。落序由引擎按 `desc`
    /// 完成(见 `request_desc`),本字段不参与结果加工——只作显示与请求换算。
    pub sort_asc: bool,
    /// 大小 / 修改时间 / 路径三列固定宽(名称列吃余宽)。拖拽后跨帧保留,
    /// **不持久化**(重启回落默认 [96, 160, 300],visual.md §3.4)。
    col_fixed: [f32; 3],
    /// 列宽拖拽会话(按下点与起拖时显示宽;总位移从按下点起算,见 `draw_header`)。
    col_drag: Option<ColDragState>,
    /// 选中行号集合(protocol 条目无 id,行号即选择;结果整体替换时清空)。
    pub selection: BTreeSet<usize>,
    /// shift 范围选锚点(范围选不移动锚点,概念如 Finder)。
    pub anchor: Option<usize>,
    /// 排序/查询在途(状态栏「排序中…」;fire 置位,当代响应到达清除)。
    pub sorting_inflight: bool,
    /// 排序未生效:请求了非 relevance 键但响应回显为空(旧 daemon)或异键
    /// (离线路径恒回 relevance 亦落此)。状态栏红字 +「重启引擎」按钮。
    pub sort_stale: bool,
    /// 引擎重启在途(按钮点击后显示「重启中…」并禁重复发起)。
    pub engine_restarting: bool,
    /// 缺口面板展开态(徽标点击切换;缺口归零或结果会话更替时收起)。
    pub gaps_open: bool,
    pub input: String,
    /// 瞬时提示(打开失败/查询失败;显示于状态栏)。
    pub status_note: Option<StatusNote>,
    /// 提示是否为错误态(决定状态栏错误红;中性文本色留给非错误提示)。
    pub status_note_is_error: bool,
    /// 齿轮点击的「打开设置」请求(绘制内只置位;共享锁释放后由 App::ui 消费)。
    pub settings_requested: bool,
    /// 引擎状态快照(status 面;状态栏「索引状态」段)。
    engine: Option<EngineStatus>,
    /// 是否已取过一次引擎状态(区分「未检查」与「引擎未运行」)。
    engine_known: bool,
    /// 窗口唤起后待补查一次状态(常驻状态栏:无查询词也须显示)。
    status_refresh_pending: bool,
    /// 唤起后请求搜索框焦点(下一帧消费)。
    focus_input_pending: bool,
    /// 输入法组合中(预编辑不上屏不触发查询;模式与 PaletteState 同款)。
    ime_composing: bool,
    /// 上帧搜索框是否持焦(⌘A 归属:聚焦 = 输入框全选文本,未聚焦 = 表格全选行)。
    input_focused: bool,
    /// 结果表格是否持键盘焦点(点击过行;搜索框聚焦时清除;空格 Quick Look 的门槛)。
    table_focused: bool,
    paths: DaemonPaths,
    ctx: egui::Context,
    tx: mpsc::Sender<QueryResult>,
    rx: mpsc::Receiver<QueryResult>,
    /// 状态补查通道(独立小通道;无代际语义,与查询结果互不干扰)。
    status_tx: mpsc::Sender<Option<EngineStatus>>,
    status_rx: mpsc::Receiver<Option<EngineStatus>>,
    /// 引擎重启结果通道(sort_echo 未生效的修复动作;worker → tick)。
    restart_tx: mpsc::Sender<RestartOutcome>,
    restart_rx: mpsc::Receiver<RestartOutcome>,
}

/// 引擎重启失败状态栏文案：本地化前缀 + 可展示原因（原始链路只进日志）。
fn restart_failed_message(err: impl std::fmt::Display) -> String {
    format!("{}: {err}", i18n::s().engine_restart_failed)
}

/// 引擎重启结果(worker → tick);失败带可展示文案(落状态栏,不静默)。
enum RestartOutcome {
    Ok,
    Err(String),
}

impl ResultsState {
    /// `ctx`:查询线程回投后定向唤起 ROOT;实机走查测试用独立 `egui::Context` 构造
    /// (本文件测试 `live_results_window_walkthrough`)。
    pub fn new(ctx: egui::Context, paths: DaemonPaths) -> Self {
        let (tx, rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        let (restart_tx, restart_rx) = mpsc::channel();
        Self {
            session: Session::new(palette::DEBOUNCE),
            results: Vec::new(),
            total: 0,
            elapsed_ms: 0.0,
            stat_ms: 0.0,
            sort: SortKey::Relevance,
            sort_asc: true,
            col_fixed: [
                window_tokens::COL_SIZE_W,
                window_tokens::COL_MTIME_W,
                window_tokens::COL_PATH_W,
            ],
            col_drag: None,
            selection: BTreeSet::new(),
            anchor: None,
            sorting_inflight: false,
            sort_stale: false,
            engine_restarting: false,
            gaps_open: false,
            input: String::new(),
            status_note: None,
            status_note_is_error: false,
            settings_requested: false,
            engine: None,
            engine_known: false,
            // 启动即显示窗口(dev 形态):首帧补查状态 + 聚焦搜索框。
            status_refresh_pending: true,
            focus_input_pending: true,
            ime_composing: false,
            input_focused: false,
            table_focused: false,
            paths,
            ctx,
            tx,
            rx,
            status_tx,
            status_rx,
            restart_tx,
            restart_rx,
        }
    }

    /// 窗口被唤起(托盘「打开完整窗口」/ 悬浮条 ⌥↵):聚焦搜索框 + 补查引擎状态
    /// (product §2:状态栏常驻显示索引状态)。
    pub fn on_window_shown(&mut self) {
        self.focus_input_pending = true;
        self.status_refresh_pending = true;
        self.table_focused = false;
        // 每次唤起从干净状态开始:上次会话的瞬时提示(如查询失败)不得跨唤起驻留。
        self.status_note = None;
        self.status_note_is_error = false;
    }

    /// 语言切换时清掉一次性状态栏消息（重查/下次操作会重建）。
    pub fn clear_transient_messages(&mut self) {
        self.status_note = None;
        self.status_note_is_error = false;
    }

    /// 消费齿轮的「打开设置」请求(置位发生在结果绘制内,开窗放在共享锁释放后)。
    pub fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    /// 托盘缺口项(product §3):唤起完整窗口并展开缺口面板。
    /// 缺口清单经 `on_window_shown` 触发的状态补查落地(面板内容 = status.gaps)。
    pub fn open_gaps(&mut self) {
        self.gaps_open = true;
        self.on_window_shown();
    }

    /// 悬浮条 ⌥↵:把查询词灌入并立即查询(清旧结果——旧词结果不得在新词下滞留;
    /// 词为空 = 清空态,只开窗)。
    pub fn set_query(&mut self, q: &str) {
        self.input = q.to_string();
        let now = Instant::now();
        self.session.on_input(q, now); // 代际推进,在途旧响应作废
        self.results.clear();
        self.total = 0;
        self.selection.clear();
        self.anchor = None;
        self.status_note = None;
        self.status_note_is_error = false;
        self.sorting_inflight = false;
        self.sort_stale = false; // 旧结果作废 → 排序警示随旧结果清
        if !q.is_empty() {
            // 防抖到期帧(调用点在悬浮条 pass,须定向 ROOT)
            self.ctx
                .request_repaint_after_for(palette::DEBOUNCE, egui::ViewportId::ROOT);
        }
    }

    /// 每帧驱动(App::logic 调用;根窗隐藏期也经 run_logic 收取响应,状态不落后):
    /// 补查状态 → 防抖到期发查询 → 收三路回投(结果/状态/重启)。
    pub fn tick(&mut self, now: Instant) {
        // 在途防抖逐帧补排(与悬浮条同款;根因见 `Session::pending_delay`:
        // 同一 pass 里 0ms 立即重绘请求会吞掉 60ms 防抖请求)。
        if let Some(delay) = self.session.pending_delay(now) {
            self.ctx
                .request_repaint_after_for(delay, egui::ViewportId::ROOT);
        }
        if self.status_refresh_pending {
            self.status_refresh_pending = false;
            self.spawn_status();
        }
        if let Some(fire) = self.session.pending_at(now) {
            self.sorting_inflight = true;
            palette::spawn_query_worker(
                self.paths.clone(),
                self.ctx.clone(),
                egui::ViewportId::ROOT,
                self.tx.clone(),
                fire,
                QUERY_LIMIT,
            );
        }
        while let Ok(res) = self.rx.try_recv() {
            self.apply(res);
        }
        while let Ok(engine) = self.status_rx.try_recv() {
            self.engine = engine;
            self.engine_known = true;
        }
        while let Ok(outcome) = self.restart_rx.try_recv() {
            self.apply_restart(outcome);
        }
        // 缺口徽标与面板都读引擎快照(查询回投与状态补查两路都在此同步):
        // 归零即收起面板(徽标同源消失,面板不留悬空)。
        if self.gaps().is_empty() {
            self.gaps_open = false;
        }
    }

    /// 缺口清单(status.gaps;引擎快照为真源,徽标计数与面板逐行同读此处)。
    fn gaps(&self) -> &[Gap] {
        self.engine
            .as_ref()
            .map(|e| e.gaps.as_slice())
            .unwrap_or(&[])
    }

    /// 一次性引擎状态补查(独立小通道;失败留痕并置「未知」——不静默)。
    fn spawn_status(&mut self) {
        let paths = self.paths.clone();
        let ctx = self.ctx.clone();
        let tx = self.status_tx.clone();
        std::thread::spawn(move || {
            let engine = palette::engine_status(&paths);
            let _ = tx.send(engine);
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        });
    }

    /// 结果应用:代际校验(旧代丢弃)→ 落结果/失败分支 → 引擎状态并入。
    /// 结果已由引擎按请求方向落序(desc),此处原样接收、不做任何本地反转。
    fn apply(&mut self, res: QueryResult) {
        if !self.session.accept_response(res.generation) {
            return; // 慢响应后到不得覆盖新结果(Task 11 语义)
        }
        self.sorting_inflight = false;
        match res.payload {
            Payload::Hits {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                offline,
                sort_echo,
                requested_sort,
            } => {
                self.results = entries;
                self.total = total;
                self.elapsed_ms = elapsed_ms;
                self.stat_ms = stat_ms;
                self.selection.clear();
                self.anchor = None;
                self.status_note = None;
                self.status_note_is_error = false;
                // sort_echo 校验(tech 定案):请求了非 relevance 键而回显为空
                // (旧 daemon 忽略 sort)或异键 → 本响应排序未生效,不静默展示。
                // 离线路径恒回 relevance,请求列排序时同落此分支(重启引擎即修复,
                // 见 restart_engine 对「daemon 未在运行」的容错)。
                self.sort_stale = sort_not_applied(requested_sort, &sort_echo);
                if self.sort_stale {
                    log::warn!("排序未生效:请求 {requested_sort:?} 回显 {sort_echo:?}(旧引擎?)");
                }
                if offline {
                    self.session.mark_offline();
                } else {
                    self.session.mark_total(total);
                }
            }
            Payload::Failed { engine_down, code } => {
                self.results.clear();
                self.total = 0;
                self.selection.clear();
                self.anchor = None;
                self.sort_stale = false; // 结果已清,排序警示随旧结果作废
                if engine_down {
                    self.session.mark_engine_starting();
                    self.status_note = Some(StatusNote::EngineNotReady);
                    self.status_note_is_error = true;
                } else {
                    // 错误码分流(tech 六态表):warming = 索引未挂载 → 占位态,
                    // 红字换中性(状态栏索引状态段与空态提示承担呈现)。
                    self.session.mark_error(code.as_deref().unwrap_or("query"));
                    self.status_note = match self.session.state {
                        State::Warming => None,
                        _ => Some(StatusNote::QueryFailed),
                    };
                    self.status_note_is_error = self.status_note.is_some();
                }
            }
        }
        if let Some(engine) = res.engine {
            self.engine = Some(engine);
        }
        self.engine_known = true;
    }

    /// 绘制完整窗口(主视口)。`frame` 供原生右键菜单取窗(调用方 = App::ui,根视口);
    /// `icons` 与悬浮条共用的系统图标缓存(UTI 纹理跨视口复用)。
    /// 返回动作由上层不需要——打开/失败都就地落在状态栏。
    pub fn draw(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame, icons: &mut IconCache) {
        self.draw_impl(ui, Some(frame), icons);
    }

    /// 绘制主体(`frame` = None 时禁用原生右键菜单:实机走查测试无 eframe Frame)。
    fn draw_impl(
        &mut self,
        ui: &mut egui::Ui,
        frame: Option<&eframe::Frame>,
        icons: &mut IconCache,
    ) {
        let ctx = ui.ctx().clone();
        // drain worker 响应 + 限量上传纹理(取图/解码在工作线程,不阻塞本帧)。
        icons.begin_frame(&ctx);
        self.track_ime(ui);

        // Quick Look 系统侧关闭 -> 归还焦点;面板可见期间每 200ms 唤醒轮询。
        if let Some(frame) = frame {
            platform::quick_look_poll(frame);
            if platform::quick_look_visible() {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }

        // ⌘A:输入框未聚焦时 = 表格全选行(聚焦时归输入框「全选文本」,平台惯例)。
        if !self.input_focused
            && !self.ime_composing
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
        {
            self.select_all();
        }

        // 空格:表格有焦点(点击过行)且搜索框未聚焦 -> Quick Look 开关。
        // 搜索框聚焦时空格照常输入;忽略自动重复(长按只 toggle 一次)。
        let space_pressed = !self.input_focused
            && !self.ime_composing
            && self.table_focused
            && !self.selection.is_empty()
            && ui.input(|i| {
                i.events.iter().any(|ev| {
                    matches!(
                        ev,
                        egui::Event::Key {
                            key: egui::Key::Space,
                            pressed: true,
                            repeat: false,
                            modifiers,
                            ..
                        } if modifiers.is_none()
                    )
                })
            });
        if space_pressed {
            // 消费掉(含自动重复),避免传给其他控件。
            ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Space);
            });
        }

        let mut sort_click: Option<SortKey> = None;
        let mut row_click: Option<(usize, bool, bool)> = None;
        let mut open_row: Option<usize> = None;
        let mut context_click: Option<(usize, egui::Pos2)> = None;
        let mut gaps_clicked = false;
        let mut restart_clicked = false;
        let (shift_down, cmd_down) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));

        // 行间无缝(item_spacing.y = 0):选中/悬停底连成整块(表格式观感)。
        ui.spacing_mut().item_spacing.y = 0.0;

        // 整窗底色(先于所有分段):根窗标题栏透明化后,原生交通灯浮在内容之上,
        // 该区域必须是窗口底色而非清屏透明(否则交通灯后是一条黑带)。
        let palette = window_tokens::palette(ui.visuals().dark_mode);
        ui.painter()
            .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, palette.window_fill);

        // —— 标题栏(32):左侧留原生交通灯,右侧齿轮;中间为拖拽区 ——
        let titlebar = egui::Panel::top("results-titlebar")
            .exact_size(window_tokens::TITLEBAR_H)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(palette.window_fill))
            .show(ui, |ui| draw_titlebar(ui, &palette));
        if titlebar.inner {
            // 只置请求位:本帧仍持共享锁(results.draw 调用方),开窗命令与状态写入
            // 由 App::ui 在锁释放后经 `take_settings_request` 统一执行。
            self.settings_requested = true;
            log::info!("标题栏齿轮点击:请求打开设置窗口");
        }

        // —— 顶部:搜索行(52 + 1 分隔线,§3.1/§3.3) ——
        //
        // 搜索框底由本层绘制(整宽圆角矩形),TextEdit 只承担文字与原生光标:
        // egui 0.37 的 TextEdit 主题自带描边与聚焦变化,而视觉规格要求「无描边、
        // 无聚焦粗环」,故 frame 置 NONE、底/字/图标全取 window_tokens。
        let input_resp = egui::Panel::top("results-search")
            .exact_size(window_tokens::SEARCH_ROW_H + window_tokens::DIVIDER_H)
            .show_separator_line(false)
            .frame(egui::Frame::NONE.fill(palette.window_fill))
            .show(ui, |ui| {
                let panel = ui.max_rect();
                let row = egui::Rect::from_min_max(
                    panel.min,
                    egui::pos2(panel.right(), panel.top() + window_tokens::SEARCH_ROW_H),
                );
                let layout = search_row_layout(row);
                let painter = ui.painter().clone();

                // 浅灰圆角底,无描边、无聚焦粗环(§3.3)。
                painter.rect_filled(
                    layout.box_rect,
                    egui::CornerRadius::same(window_tokens::SEARCH_BOX_RADIUS),
                    palette.search_fill,
                );
                draw_search_icon(&painter, layout.icon_rect, palette.search_icon);

                // 空词 hint:自绘以取 token 色(TextEdit 的 hint_text 会被主题
                // `hint_text_color` 覆盖,无法走 token)。先于 TextEdit 绘制,
                // 光标随后叠在其上。
                if self.input.is_empty() {
                    painter.text(
                        egui::pos2(layout.text_rect.left(), layout.text_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        i18n::s().search_placeholder,
                        ui_font::id(window_tokens::FONT_QUERY),
                        palette.search_hint,
                    );
                }

                // 点击框内空白/图标 → 聚焦输入框。TextEdit 在本交互之后注册:
                // 同层命中测试后注册者优先,框内文字区域的点击仍归 TextEdit(定光标)。
                let box_resp = ui.interact(
                    layout.box_rect,
                    ui.scope_id().with("results-search-box"),
                    egui::Sense::click(),
                );
                let input_id = ui.make_persistent_id("results-input");
                // 原生闪烁光标用查询文本色(§4.2「查询文本 / 光标」同一 token)。
                ui.visuals_mut().text_cursor.stroke.color = palette.query_text;
                let resp = ui.place(
                    layout.text_rect,
                    egui::TextEdit::singleline(&mut self.input)
                        .id(input_id)
                        .font(ui_font::id(window_tokens::FONT_QUERY))
                        .text_color(palette.query_text)
                        .frame(egui::Frame::NONE)
                        .min_size(layout.text_rect.size())
                        // 文字垂直居中、左对齐(egui 0.37 TextEdit 主题默认贴顶)。
                        .align(egui::Align2::LEFT_CENTER),
                );
                if box_resp.clicked() && !resp.has_focus() {
                    resp.request_focus();
                }

                // 搜索行底 1px 分隔线(§3.1 分隔线 1);标题栏与搜索行之间不加线。
                painter.rect_filled(
                    egui::Rect::from_min_max(egui::pos2(panel.left(), row.bottom()), panel.max),
                    egui::CornerRadius::ZERO,
                    palette.divider,
                );
                resp
            })
            .inner;

        // —— 底部:状态栏(顶部 1px 分隔线 + 28 内容,§3.6)+ 缺口面板(展开时在上)——
        // min_size 固定状态行首帧高度(egui 面板尺寸跨帧记忆;缺省最小 20 会让
        // 首帧从 20 长到 29 时向下溢出)。缺口面板展开时面板按内容增高。
        egui::Panel::bottom("results-status")
            .show_separator_line(false)
            .min_size(window_tokens::STATUS_H + window_tokens::DIVIDER_H)
            .frame(egui::Frame::NONE.fill(palette.status_fill))
            .show(ui, |ui| {
                let gaps = self.gaps();
                if self.gaps_open && !gaps.is_empty() {
                    draw_gaps_panel(ui, gaps);
                }
                // 状态栏顶部 1px 分隔线(§3.1 分隔线 3;28 内容另计,§3.2 高度推导)。
                let (divider, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), window_tokens::DIVIDER_H),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .rect_filled(divider, egui::CornerRadius::ZERO, palette.divider);
                let (row, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), window_tokens::STATUS_H),
                    egui::Sense::hover(),
                );
                let action = draw_status_row(ui, row, self, &palette);
                gaps_clicked |= action.gaps_clicked;
                restart_clicked |= action.restart_clicked;
            });

        // —— 中部:表头(32 + 1 分隔线)+ 自定义虚拟化表格(§3.4 / §3.5) ——
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(palette.window_fill))
            .show(ui, |ui| {
                let area = ui.max_rect();
                let cols = col_layout(area.width(), self.col_fixed);
                let header_action = draw_header(
                    ui,
                    area,
                    &cols,
                    self.sort,
                    self.sort_asc,
                    &palette,
                    &mut self.col_drag,
                );
                if let Some(key) = header_action.sort_click {
                    sort_click = Some(key);
                }
                // 表头底 1px 分隔线(§3.1 分隔线 2)。
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(area.left(), area.top() + window_tokens::HEADER_H),
                        egui::vec2(area.width(), window_tokens::DIVIDER_H),
                    ),
                    egui::CornerRadius::ZERO,
                    palette.divider,
                );
                // 拖拽结果下一帧生效(本帧行已按旧宽排;差一帧不可见)。总位移从
                // 按下点起算、起点宽取按下帧投影后的显示宽(不是 stored),否则窄窗
                // 首帧会跳变;释放帧同样按最终指针位置应用一次(见 `draw_header`)。
                if let Some((drag, delta)) = header_action.drag {
                    self.col_fixed =
                        drag_fixed(drag.boundary, drag.start_fixed, delta, area.width());
                }
                let table_rect = egui::Rect::from_min_max(
                    egui::pos2(
                        area.left(),
                        area.top() + window_tokens::HEADER_H + window_tokens::DIVIDER_H,
                    ),
                    area.max,
                );
                if self.results.is_empty() {
                    draw_empty_state(ui, table_rect, &self.input, &self.session.state, &palette);
                    return;
                }
                let results: &[HitEntry] = &self.results;
                let selection: &BTreeSet<usize> = &self.selection;
                let home = std::env::var("HOME").ok();
                let mut table_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(table_rect)
                        .layout(egui::Layout::top_down(egui::Align::LEFT)),
                );
                // 行间无缝(虚拟化按固定行高 30 计算);滚动条浮在内容上、不占内容宽,
                // 表头与行因此同宽对齐。
                //
                // 已知偏差(Task 4 登记,交 Task 5 验收决定):egui 0.37 的 floating
                // 滚动条并非「滚动时才出现」——可见性由悬停态驱动(悬停区域即淡入,
                // dormant→active,见 scroll_area.rs 的 is_hovering_outer_rect 分支),
                // 滚动本身不改变可见性。本轮不做自定义淡入,避免替换 ScrollArea 的
                // 滚动/虚拟化路径。
                table_ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
                table_ui.spacing_mut().scroll.floating = true;
                egui::ScrollArea::vertical()
                    .id_salt("results-table")
                    .auto_shrink([false, false])
                    .max_height(table_rect.height())
                    .show_rows(
                        &mut table_ui,
                        window_tokens::ROW_H,
                        results.len(),
                        |ui, range| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for i in range {
                                let resp = draw_row(
                                    ui,
                                    &results[i],
                                    &cols,
                                    selection.contains(&i),
                                    home.as_deref(),
                                    &palette,
                                    icons,
                                );
                                if resp.double_clicked() {
                                    open_row = Some(i); // 双击 = 默认应用打开(product §2)
                                } else if resp.secondary_clicked() {
                                    // 右键(Task 18):记录行与点击点;选择收敛与弹菜单在
                                    // 表格闭包外做(借用结束)
                                    match resp.interact_pointer_pos() {
                                        Some(pos) => context_click = Some((i, pos)),
                                        None => log::warn!("右键点击行 {i} 缺位置信息,菜单不弹"),
                                    }
                                } else if resp.clicked() {
                                    row_click = Some((i, shift_down, cmd_down));
                                }
                            }
                        },
                    );
            });

        // —— 交互落地(表格闭包外:借用结束)——
        if let Some(key) = sort_click {
            self.on_header_click(key);
        }
        if let Some((i, shift, cmd)) = row_click {
            apply_click(&mut self.selection, &mut self.anchor, i, shift, cmd);
            self.table_focused = true;
        }
        if let Some(i) = open_row {
            self.open_row(i);
        }
        if let Some((i, pos)) = context_click {
            self.open_context_menu(i, pos, frame);
        }
        if space_pressed {
            if let Some(frame) = frame {
                let paths = self.selected_paths();
                if let Err(err) = platform::quick_look_toggle(&paths, frame) {
                    self.status_note = Some(StatusNote::Raw(err.to_string()));
                    self.status_note_is_error = true;
                }
            } else {
                log::warn!("空格快速查看:无 eframe Frame(测试路径),忽略");
            }
        }
        if gaps_clicked {
            self.gaps_open = !self.gaps_open;
            log::info!(
                "缺口徽标点击:{}缺口面板(共 {} 个目录)",
                if self.gaps_open { "展开" } else { "收起" },
                self.gaps().len()
            );
            // 面板增删发生在绘制之后:排下一帧刷出(点击帧内容为旧态)
            self.ctx.request_repaint();
        }
        if restart_clicked {
            self.restart_engine();
        }

        // —— 输入变更 → 防抖查询(预编辑文本不上屏不触发;与悬浮条同款)——
        if input_resp.changed() && !self.ime_composing {
            let now = Instant::now();
            self.session.on_input(&self.input, now);
            if self.input.is_empty() {
                // 清空即清结果(不等响应;旧代已被 on_input 作废)
                self.results.clear();
                self.total = 0;
                self.selection.clear();
                self.anchor = None;
                self.status_note = None;
                self.status_note_is_error = false;
                self.sort_stale = false;
            } else {
                ctx.request_repaint_after(palette::DEBOUNCE);
            }
        }
        if self.focus_input_pending {
            input_resp.request_focus();
            self.focus_input_pending = false;
        }
        self.input_focused = input_resp.has_focus();
        if self.input_focused {
            self.table_focused = false;
        }

        // —— 窗口内描边(最内层,压在所有分段之上;§4.2 window_stroke / §4.4) ——
        // 原生窗口负责圆角裁剪与投影;内容层按同半径在窗口内侧补 1px 描边。
        ui.painter().rect_stroke(
            ui.max_rect(),
            egui::CornerRadius::same(window_tokens::WINDOW_RADIUS),
            egui::Stroke::new(window_tokens::WINDOW_STROKE, palette.window_stroke),
            egui::StrokeKind::Inside,
        );
    }

    /// 列头点击:异列切键并按该键自然方向;同列「自然方向 → 反向 → 默认」
    /// (mtime 自然为降,故不能用 asc 布尔判断第几次点击)。三种状态都触发
    /// 同串重查(on_sort_change 在防抖窗内合并;响应按新方向落序)。
    fn on_header_click(&mut self, key: SortKey) {
        if self.sort != key {
            self.sort = key;
            self.sort_asc = natural_asc(key);
        } else if self.sort_asc == natural_asc(key) {
            // 第一次(自然方向)→ 第二次(反向)
            self.sort_asc = !natural_asc(key);
        } else {
            // 第三次:回到默认(匹配质量,表头无箭头)。
            self.sort = SortKey::Relevance;
            self.sort_asc = natural_asc(key);
        }
        log::info!(
            "列头点击:{key:?} → sort={:?}(asc={})",
            self.sort,
            self.sort_asc
        );
        self.session.on_sort_change(
            self.sort,
            request_desc(self.sort, self.sort_asc),
            Instant::now(),
        );
        self.ctx
            .request_repaint_after_for(palette::DEBOUNCE, egui::ViewportId::ROOT);
    }

    /// 重启引擎(排序未生效的修复动作;tech 定案:随包 CLI stop → start)。
    /// 阻塞(stop 往返 + 拉起 ≤3s 连通轮询)放工作线程;在途期间按钮显示「重启中…」。
    /// 停失败分两类:daemon 未在运行(离线场景)= 无需停,不阻断启动;其余如实留痕。
    fn restart_engine(&mut self) {
        if self.engine_restarting {
            log::warn!("重启引擎:已在途,忽略重复发起");
            return;
        }
        self.engine_restarting = true;
        log::info!("排序未生效:重启引擎(stop → start)");
        self.ctx.request_repaint(); // 置位发生在绘制之后:排下一帧刷出「重启中…」
        let paths = self.paths.clone();
        let ctx = self.ctx.clone();
        let tx = self.restart_tx.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            match client::stop(&paths) {
                Ok(n) => log::info!("重启引擎:已停 daemon(最终 {n} 条)"),
                Err(err) => log::info!("重启引擎:停止跳过({err})"),
            }
            let outcome = match engine_launcher::ensure_engine(&paths) {
                Ok(()) => {
                    log::info!("重启引擎:新引擎就绪({}ms)", t0.elapsed().as_millis());
                    RestartOutcome::Ok
                }
                Err(err) => {
                    log::warn!("重启引擎失败:{err:#}");
                    RestartOutcome::Err(restart_failed_message(err))
                }
            };
            let _ = tx.send(outcome);
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        });
    }

    /// 重启结果落地(tick 收取):成功 → 清红字 + 状态补查 + 按当前列/方向重查
    /// (新引擎接管后排序才真正生效);失败 → 红字保留 + 状态栏提示原因(不静默)。
    fn apply_restart(&mut self, outcome: RestartOutcome) {
        self.engine_restarting = false;
        match outcome {
            RestartOutcome::Ok => {
                self.sort_stale = false;
                // 新引擎状态校准(索引状态段),并按当前列/方向同串重查
                // (与列头点击同一路径:新引擎接管后排序才真正生效)
                self.status_refresh_pending = true;
                self.session.on_sort_change(
                    self.sort,
                    request_desc(self.sort, self.sort_asc),
                    Instant::now(),
                );
                self.ctx
                    .request_repaint_after_for(palette::DEBOUNCE, egui::ViewportId::ROOT);
            }
            RestartOutcome::Err(msg) => {
                self.status_note = Some(StatusNote::Raw(msg));
                self.status_note_is_error = true;
            }
        }
    }

    /// 右键菜单(Task 18):标准表格语义先把点击行并入选中(不在选中集 → 单选它),
    /// 按选中类型收敛菜单项(product §2 表),弹原生菜单并执行所选动作。
    /// 弹出阻塞(嵌套 run loop,期间本帧冻结——PoC-3 定论);执行失败落状态栏。
    fn open_context_menu(&mut self, row: usize, pos: egui::Pos2, frame: Option<&eframe::Frame>) {
        converge_selection(&mut self.selection, &mut self.anchor, row);
        self.table_focused = true;
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let items = menu_actions::menu_entries();
        log::info!(
            "右键菜单:选中 {} 行,弹出于 ({:.0},{:.0})",
            paths.len(),
            pos.x,
            pos.y
        );
        let Some(frame) = frame else {
            log::warn!("右键菜单:无 eframe Frame(测试路径),不弹出");
            return;
        };
        let choice = platform::popup_menu(&items, (pos.x as f64, pos.y as f64), frame);
        match choice {
            None => log::info!("右键菜单:取消(未选择)"),
            Some(tag) => match menu_actions::MenuAction::from_tag(tag) {
                Some(action) => {
                    let result = match action {
                        menu_actions::MenuAction::QuickLook => {
                            menu_actions::ensure_existing(&paths)
                                .and_then(|()| platform::quick_look_show(&paths, frame))
                        }
                        menu_actions::MenuAction::CopyFile => menu_actions::ensure_existing(&paths)
                            .and_then(|()| platform::copy_files(&paths)),
                        _ => action
                            .shell()
                            .map_or(Ok(()), |shell| menu_actions::execute(shell, &paths)),
                    };
                    match result {
                        Ok(()) => log::info!("右键菜单:执行 {action:?}({} 项)", paths.len()),
                        Err(err) => {
                            self.status_note = Some(StatusNote::Raw(err.to_string()));
                            self.status_note_is_error = true;
                        }
                    }
                }
                None => log::warn!("右键菜单:未知 tag {tag},忽略"),
            },
        }
        self.ctx.request_repaint();
    }

    /// 选中行 -> 路径集合(行号与结果同步维护)。
    fn selected_paths(&self) -> Vec<String> {
        self.selection
            .iter()
            .filter_map(|&i| self.results.get(i))
            .map(|e| e.path.clone())
            .collect()
    }

    /// 双击:默认应用打开(palette::open_path 复用;失败以本地化文案落状态栏)。
    fn open_row(&mut self, i: usize) {
        let Some(entry) = self.results.get(i) else {
            return;
        };
        if let Err(err) = palette::open_path(&entry.path, false) {
            self.status_note = Some(StatusNote::Raw(err.text()));
            self.status_note_is_error = true;
        }
    }

    /// ⌘A:全选当前结果 K 行(product §2「⌘A 全选」;K = 实际返回条数)。
    fn select_all(&mut self) {
        self.selection = (0..self.results.len()).collect();
        log::info!("⌘A 全选 {} 行", self.selection.len());
    }

    /// 输入法组合态跟踪(仅组合态:预编辑不触发查询;模式与 PaletteState::track_ime 同款,
    /// 完整窗口不吃 Enter/Esc 键位,故不需要 palette 的「本帧有 IME 事件」位)。
    fn track_ime(&mut self, ui: &egui::Ui) {
        let composing = ui.input(|i| {
            let mut composing = self.ime_composing;
            for ev in &i.events {
                match ev {
                    egui::Event::Ime(egui::ImeEvent::Preedit { text, .. }) => {
                        composing = !text.is_empty();
                    }
                    egui::Event::Ime(egui::ImeEvent::Commit(_)) => composing = false,
                    _ => {}
                }
            }
            composing
        });
        self.ime_composing = composing;
    }
}

/// 搜索行几何(§3.3,纯函数):行内左右各 16 得到整宽搜索框;框高 36 在 52 行内
/// 垂直居中;框内左右各 14、图标 16×16、图标与文本间距 10。坐标均为绝对坐标。
struct SearchRowLayout {
    /// 搜索框:窗口宽 − 32。
    box_rect: egui::Rect,
    /// 搜索图标:16×16,框内左内边距 14、垂直居中。
    icon_rect: egui::Rect,
    /// 查询文本区:图标右缘 + 10 到框右缘 − 14,纵向取满框高(文字居中)。
    text_rect: egui::Rect,
}

/// 搜索行几何(规格数值见 `window_tokens`,单测逐项钉住)。
fn search_row_layout(row_rect: egui::Rect) -> SearchRowLayout {
    let (_, pad_x) = window_tokens::SEARCH_BOX_PAD;
    let box_rect = egui::Rect::from_min_size(
        egui::pos2(
            row_rect.left() + window_tokens::CONTENT_PAD_X,
            row_rect.center().y - window_tokens::SEARCH_BOX_H / 2.0,
        ),
        egui::vec2(
            (row_rect.width() - 2.0 * window_tokens::CONTENT_PAD_X).max(0.0),
            window_tokens::SEARCH_BOX_H,
        ),
    );
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(
            box_rect.left() + pad_x,
            box_rect.center().y - window_tokens::SEARCH_ICON / 2.0,
        ),
        egui::Vec2::splat(window_tokens::SEARCH_ICON),
    );
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(
            icon_rect.right() + window_tokens::SEARCH_BOX_GAP,
            box_rect.top(),
        ),
        egui::pos2(box_rect.right() - pad_x, box_rect.bottom()),
    );
    SearchRowLayout {
        box_rect,
        icon_rect,
        text_rect,
    }
}

// ---------------------------------------------------------------------------
// 表头 / 表格几何(§3.4 / §3.5;数值真源 = window_tokens + 本文件下限常量)

/// 四列几何(纯函数):返回各列 `(x, 宽)`,坐标系原点 = 窗口左边。
///
/// 左右内边距 16、列间距 16;名称列吃余宽(下限 84),大小 / 修改时间 / 路径
/// 取 `fixed`(下限 60 / 110 / 120)。名称不足下限时从右往左收缩固定列
/// (路径 → 修改时间 → 大小),保证列不重叠、不越过右内边距。
fn col_layout(total_w: f32, fixed: [f32; 3]) -> [(f32, f32); 4] {
    let mut fixed = [
        fixed[0].max(SIZE_MIN_W),
        fixed[1].max(MTIME_MIN_W),
        fixed[2].max(PATH_MIN_W),
    ];
    let inner =
        (total_w - 2.0 * window_tokens::CONTENT_PAD_X - 3.0 * window_tokens::COLUMN_GAP).max(0.0);
    let mut name_w = inner - fixed.iter().sum::<f32>();
    if name_w < NAME_MIN_W {
        // 名称吃最小宽后仍不足:路径 → 修改时间 → 大小依次让宽到各自下限。
        let mut deficit = NAME_MIN_W - name_w;
        for (i, min) in [(2usize, PATH_MIN_W), (1, MTIME_MIN_W), (0, SIZE_MIN_W)] {
            let shrink = (fixed[i] - min).max(0.0).min(deficit);
            fixed[i] -= shrink;
            deficit -= shrink;
            if deficit <= 0.0 {
                break;
            }
        }
        name_w = (inner - fixed.iter().sum::<f32>()).max(NAME_MIN_W);
    }
    let mut x = window_tokens::CONTENT_PAD_X;
    let mut cols = [(0.0, 0.0); 4];
    cols[COL_NAME] = (x, name_w);
    x += name_w + window_tokens::COLUMN_GAP;
    for (i, w) in fixed.into_iter().enumerate() {
        cols[i + 1] = (x, w);
        x += w + window_tokens::COLUMN_GAP;
    }
    cols
}

/// 表头列边界(从左到右;热区居中于两列之间的 16px 间隙)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColBoundary {
    NameSize,
    SizeMtime,
    MtimePath,
}

/// 列宽拖拽会话:按下点 + 按下时实际显示的三列固定宽。
///
/// 位移从**按下点**起算(不是逐帧 `drag_delta`):快速拖动同帧释放时
/// `drag_delta` 归零/起拖阈值前位移会丢,边界落后指针。
#[derive(Clone, Copy, Debug, PartialEq)]
struct ColDragState {
    boundary: ColBoundary,
    /// 按下点 x(视口坐标)。
    start_x: f32,
    /// 按下时 `col_layout` 投影后的实际显示宽(窄窗不跳回 stored)。
    start_fixed: [f32; 3],
}

/// 拖拽后的固定列宽(纯函数):`delta` = 本帧指针横向位移(右为正)。
///
/// 名称列吃余宽,故边界两侧必须一增一减才能让边界跟随指针:
/// - 名称/大小边界调整大小列(右移 = 名称变宽、大小变窄);
/// - 大小/修改时间边界调整大小列(右移变大),修改时间列反向补偿;
/// - 修改时间/路径边界调整修改时间列(右移变大),路径列反向补偿。
///
/// 各列 clamp 最小宽;名称列不经这两条边界变化,始终保有下限。
fn drag_fixed(boundary: ColBoundary, fixed: [f32; 3], delta: f32, total_w: f32) -> [f32; 3] {
    let mut fixed = [
        fixed[0].max(SIZE_MIN_W),
        fixed[1].max(MTIME_MIN_W),
        fixed[2].max(PATH_MIN_W),
    ];
    let inner =
        (total_w - 2.0 * window_tokens::CONTENT_PAD_X - 3.0 * window_tokens::COLUMN_GAP).max(0.0);
    // 固定列可占的最大宽 = 总内容宽 − 名称下限;窗口极窄时保底三列最小宽之和。
    let budget = (inner - NAME_MIN_W).max(SIZE_MIN_W + MTIME_MIN_W + PATH_MIN_W);
    match boundary {
        ColBoundary::NameSize => {
            let max = (budget - fixed[1] - fixed[2]).max(SIZE_MIN_W);
            fixed[0] = (fixed[0] - delta).clamp(SIZE_MIN_W, max);
        }
        ColBoundary::SizeMtime => {
            // 大小列吃 delta,修改时间列反向补偿;可用位移受两列最小宽共同约束。
            let d = delta.min(fixed[1] - MTIME_MIN_W).max(SIZE_MIN_W - fixed[0]);
            fixed[0] += d;
            fixed[1] -= d;
        }
        ColBoundary::MtimePath => {
            // 修改时间列吃 delta,路径列反向补偿。
            let d = delta.min(fixed[2] - PATH_MIN_W).max(MTIME_MIN_W - fixed[1]);
            fixed[1] += d;
            fixed[2] -= d;
        }
    }
    fixed
}

/// 单行各内容矩形(绝对坐标):名称图标 / 名称文本 / 大小 / 修改时间 / 路径。
/// 名称文本起点 = 图标右缘 + 8(§3.5);四个 cell 共享行高、垂直居中绘制。
#[derive(Clone, Copy, Debug)]
struct RowCells {
    rect: egui::Rect,
    icon: egui::Rect,
    name: egui::Rect,
    size: egui::Rect,
    mtime: egui::Rect,
    path: egui::Rect,
}

fn row_cells(row: egui::Rect, cols: &[(f32, f32); 4]) -> RowCells {
    let cell = |i: usize| {
        egui::Rect::from_min_max(
            egui::pos2(row.left() + cols[i].0, row.top()),
            egui::pos2(row.left() + cols[i].0 + cols[i].1, row.bottom()),
        )
    };
    let icon = egui::Rect::from_min_size(
        egui::pos2(
            row.left() + window_tokens::CONTENT_PAD_X,
            row.center().y - window_tokens::ROW_ICON / 2.0,
        ),
        egui::Vec2::splat(window_tokens::ROW_ICON),
    );
    let name = egui::Rect::from_min_max(
        egui::pos2(icon.right() + window_tokens::ICON_NAME_GAP, row.top()),
        egui::pos2(cell(COL_NAME).right(), row.bottom()),
    );
    RowCells {
        rect: row,
        icon,
        name,
        size: cell(COL_SIZE),
        mtime: cell(COL_MTIME),
        path: cell(COL_PATH),
    }
}

/// 表头交互产出(表头闭包外落地;避免 `draw_header` 参数过多)。
#[derive(Default)]
struct HeaderAction {
    /// 列头点击的排序键(由调用方走 `on_header_click`)。
    sort_click: Option<SortKey>,
    /// 列边界拖拽:(拖拽会话, 自按下点起算的总横向位移)。
    drag: Option<(ColDragState, f32)>,
}

/// 列头文案(按当前语言;relevance 是默认排序而非列,无列头)。
fn column_label(key: SortKey) -> Option<&'static str> {
    let s = i18n::s();
    match key {
        SortKey::Name => Some(s.col_name),
        SortKey::Size => Some(s.col_size),
        SortKey::Mtime => Some(s.col_mtime),
        SortKey::Path => Some(s.col_path),
        SortKey::Relevance => None,
    }
}

/// 表头(§3.4):高 32、12pt、无竖分隔线;名称 / 大小 / 修改时间 / 路径四列,
/// 大小右对齐、其余左对齐;当前排序列文字后 12×12 chevron(↑ 升 / ↓ 降)。
/// 列边界 ±4 热区拖拽调宽(不占列头点击区;hover 显示横向缩放光标)。
fn draw_header(
    ui: &mut egui::Ui,
    area: egui::Rect,
    cols: &[(f32, f32); 4],
    sort: SortKey,
    sort_asc: bool,
    palette: &FullWindowPalette,
    drag: &mut Option<ColDragState>,
) -> HeaderAction {
    let header =
        egui::Rect::from_min_size(area.min, egui::vec2(area.width(), window_tokens::HEADER_H));
    let header_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(header)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    let painter = header_ui.painter().clone();
    let mut action = HeaderAction::default();
    // 按下帧的实际显示宽:窄窗下 stored 超预算时以投影宽起算,拖拽不跳变。
    let effective = [cols[COL_SIZE].1, cols[COL_MTIME].1, cols[COL_PATH].1];
    for (i, key) in COLUMNS.into_iter().enumerate() {
        let Some(label) = column_label(key) else {
            debug_assert!(false, "COLUMNS 不得包含无列头的排序键:{key:?}");
            continue;
        };
        let cell = egui::Rect::from_min_size(
            egui::pos2(area.left() + cols[i].0, header.top()),
            egui::vec2(cols[i].1, header.height()),
        );
        let resp = header_ui.interact(
            cell,
            header_ui.scope_id().with(("results-header", i)),
            egui::Sense::click(),
        );
        if resp.clicked() {
            action.sort_click = Some(key);
        }
        // 默认 relevance 无箭头;仅当前排序列显示。
        let arrow = (key == sort && sort != SortKey::Relevance).then_some(sort_asc);
        let arrow_w = if arrow.is_some() {
            window_tokens::SORT_ARROW + window_tokens::SORT_ARROW_GAP
        } else {
            0.0
        };
        let galley = egui::WidgetText::from(
            egui::RichText::new(label)
                .size(ui_font::size(window_tokens::FONT_TABLE))
                .color(palette.header_text),
        )
        .into_galley(
            &header_ui,
            Some(egui::TextWrapMode::Truncate),
            (cell.width() - arrow_w).max(0.0),
            egui::TextStyle::Body,
        );
        let text_area =
            egui::Rect::from_min_max(cell.min, egui::pos2(cell.right() - arrow_w, cell.bottom()));
        let align = if i == COL_SIZE {
            egui::Align2::RIGHT_CENTER
        } else {
            egui::Align2::LEFT_CENTER
        };
        let text_rect = align.align_size_within_rect(galley.size(), text_area);
        painter.galley(text_rect.min, galley, palette.header_text);
        if let Some(up) = arrow {
            let arrow_rect = egui::Rect::from_min_size(
                egui::pos2(
                    text_rect.right() + window_tokens::SORT_ARROW_GAP,
                    cell.center().y - window_tokens::SORT_ARROW / 2.0,
                ),
                egui::Vec2::splat(window_tokens::SORT_ARROW),
            );
            draw_sort_arrow(&painter, arrow_rect, up, palette.sort_arrow);
        }
    }
    // 表头列间竖线(2026-10-18 需求):1px、取 divider token,居中于列间距;
    // 上下各内缩 8(不顶头),只画在表头行内,结果行不画。
    let sep_y = (
        header.top() + window_tokens::HEADER_SEP_INSET_Y,
        header.bottom() - window_tokens::HEADER_SEP_INSET_Y,
    );
    for col in cols.iter().take(COLUMNS.len() - 1) {
        let x = area.left() + col.0 + col.1 + window_tokens::COLUMN_GAP / 2.0;
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x - window_tokens::DIVIDER_H / 2.0, sep_y.0),
                egui::pos2(x + window_tokens::DIVIDER_H / 2.0, sep_y.1),
            ),
            egui::CornerRadius::ZERO,
            palette.divider,
        );
    }
    // 拖拽热区:居中于列间 16px 空隙(与列头点击区不重叠,拖拽不会误触排序)。
    let boundaries = [
        ColBoundary::NameSize,
        ColBoundary::SizeMtime,
        ColBoundary::MtimePath,
    ];
    for (i, boundary) in boundaries.iter().enumerate() {
        let x = area.left() + cols[i].0 + cols[i].1 + window_tokens::COLUMN_GAP / 2.0;
        let zone = egui::Rect::from_center_size(
            egui::pos2(x, header.center().y),
            egui::vec2(2.0 * DRAG_HOT_ZONE, header.height()),
        );
        let resp = header_ui.interact(
            zone,
            header_ui.scope_id().with(("results-col-drag", i)),
            egui::Sense::click_and_drag(),
        );
        // 按下即记住起点(不等 drag_started 的 6px 阈值):快速拖动同帧释放
        // 时也能按最终指针位置应用,阈值前的位移不丢。
        if drag.is_none() && resp.is_pointer_button_down_on() {
            if let Some(start) = header_ui.input(|i| i.pointer.press_origin()) {
                *drag = Some(ColDragState {
                    boundary: *boundary,
                    start_x: start.x,
                    start_fixed: effective,
                });
                log::info!("列宽拖拽按下:{boundary:?} x={:.1}", start.x);
            }
        }
        if resp.hovered() || resp.dragged() {
            header_ui
                .ctx()
                .set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if let Some(state) = *drag {
            if state.boundary == *boundary {
                // 总位移 = 当前指针 x − 按下点 x(drag_delta 不是总位移)。
                let pos = resp
                    .interact_pointer_pos()
                    .or_else(|| header_ui.input(|i| i.pointer.latest_pos()));
                if let Some(pos) = pos {
                    action.drag = Some((state, pos.x - state.start_x));
                }
                let (released, down) =
                    header_ui.input(|i| (i.pointer.any_released(), i.pointer.any_down()));
                if released || !down {
                    *drag = None; // 释放帧已按最终位置应用;会话结束
                }
            }
        }
    }
    action
}

/// Lucide `chevron-up` / `chevron-down`(§5.2;12×12,线条绘制)。
fn draw_sort_arrow(painter: &egui::Painter, rect: egui::Rect, up: bool, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.5, color);
    let c = rect.center();
    let tip = egui::pos2(c.x, c.y + if up { -2.5 } else { 2.5 });
    let wing_y = c.y + if up { 2.5 } else { -2.5 };
    painter.line_segment([egui::pos2(c.x - 3.0, wing_y), tip], stroke);
    painter.line_segment([tip, egui::pos2(c.x + 3.0, wing_y)], stroke);
}

/// 数据行(§3.5):高 30、左右内边距 16;行底全宽(选中 token / 悬停 50%),
/// 名称 cell = 16×16 系统图标(兜底中性文档字形)+ 8 + 文件名 13pt;
/// 大小右对齐(目录「—」、文件缺失留空)、修改时间 / 路径 12pt。鼠标移动不改选中集。
fn draw_row(
    ui: &mut egui::Ui,
    entry: &HitEntry,
    cols: &[(f32, f32); 4],
    selected: bool,
    home: Option<&str>,
    palette: &FullWindowPalette,
    icons: &mut IconCache,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), window_tokens::ROW_H),
        egui::Sense::click(),
    );
    let cells = row_cells(rect, cols);
    let bg = if selected {
        palette.row_selected
    } else if resp.hovered() {
        palette.row_selected.gamma_multiply(0.5)
    } else {
        egui::Color32::TRANSPARENT
    };
    let painter = ui.painter();
    painter.rect_filled(cells.rect, egui::CornerRadius::ZERO, bg);
    match icons.icon(ui.ctx(), entry) {
        Some(id) => {
            painter.image(
                id,
                cells.icon,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => icons::draw_fallback(painter, cells.icon, palette.column_text),
    }
    draw_cell_text(
        ui,
        cells.name,
        &entry.name,
        window_tokens::FONT_FILENAME,
        palette.file_name,
        egui::Align2::LEFT_CENTER,
    );
    draw_cell_text(
        ui,
        cells.size,
        &size_cell(entry),
        window_tokens::FONT_TABLE,
        palette.column_text,
        egui::Align2::RIGHT_CENTER,
    );
    draw_cell_text(
        ui,
        cells.mtime,
        &mtime_text(entry.mtime),
        window_tokens::FONT_TABLE,
        palette.column_text,
        egui::Align2::LEFT_CENTER,
    );
    let path = display_path(&entry.path, home);
    draw_cell_text(
        ui,
        cells.path,
        &path,
        window_tokens::FONT_TABLE,
        palette.column_text,
        egui::Align2::LEFT_CENTER,
    );
    resp
}

/// 单元格单行文本(超宽截断加省略号;垂直居中,水平按 `align`)。
/// `size` 为 token 基准 pt,这里按全局字号倍率换算(几何不变)。
fn draw_cell_text(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    size: f32,
    color: egui::Color32,
    align: egui::Align2,
) {
    if rect.width() <= 0.0 || text.is_empty() {
        return;
    }
    let galley = egui::WidgetText::from(
        egui::RichText::new(text)
            .size(ui_font::size(size))
            .color(color),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    let text_rect = align.align_size_within_rect(galley.size(), rect);
    ui.painter().galley(text_rect.min, galley, color);
}

/// 搜索图标(§5.2 `search`,16×16):按 Lucide 24 网格比例画圆 + 手柄。
fn draw_search_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let s = rect.width() / 24.0;
    let stroke = egui::Stroke::new(2.0 * s, color);
    let center = rect.min + egui::vec2(11.0, 11.0) * s;
    painter.circle_stroke(center, 7.0 * s, stroke);
    painter.line_segment(
        [
            rect.min + egui::vec2(16.65, 16.65) * s,
            rect.min + egui::vec2(21.0, 21.0) * s,
        ],
        stroke,
    );
}

/// 标题栏(§3.1/§3.3):左侧 80px 留给原生交通灯(不绘制内容),右端齿轮按钮,
/// 其余区域为窗口拖拽区。返回齿轮是否被点击(调用方置「打开设置」请求)。
///
/// 交通灯是原生 NSWindow 按钮,浮在内容之上;此处只负责不遮挡它们的视觉。
fn draw_titlebar(ui: &mut egui::Ui, palette: &FullWindowPalette) -> bool {
    let bar = ui.max_rect();
    let gear = egui::Rect::from_min_size(
        egui::pos2(
            bar.right() - window_tokens::TITLEBAR_PAD_X - window_tokens::GEAR_SIZE,
            bar.top() + (window_tokens::TITLEBAR_H - window_tokens::GEAR_SIZE) / 2.0,
        ),
        egui::Vec2::splat(window_tokens::GEAR_SIZE),
    );

    // 拖拽区 = 交通灯预留区之后到齿轮左缘。左侧 80px 不接管:原生交通灯自带
    // 命中,自定义交互不覆盖其附近;双击与绿灯缩放走系统原生 zoom(见下)。
    let drag_rect = egui::Rect::from_min_max(
        egui::pos2(
            bar.left() + window_tokens::TRAFFIC_LIGHTS_RESERVE,
            bar.top(),
        ),
        egui::pos2(gear.left(), bar.bottom()),
    );
    let drag = ui.interact(
        drag_rect,
        ui.scope_id().with("results-titlebar-drag"),
        egui::Sense::click_and_drag(),
    );
    if drag.drag_started() {
        log::info!("标题栏拖拽:StartDrag");
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    // 双击标题栏与原生绿灯缩放都交给系统:实测双击的第二次 release 不进入 egui
    // (原生标题栏先接管并 zoom),egui 侧收不到完整 double-click,保留自定义
    // `ViewportCommand::Maximized` 分支只会是死代码(2026-10-17 临时日志实测)。

    let gear = ui.interact(
        gear,
        ui.scope_id().with("results-titlebar-settings"),
        egui::Sense::click(),
    );
    if gear.hovered() {
        ui.painter().rect_filled(
            gear.rect,
            egui::CornerRadius::same(window_tokens::GEAR_RADIUS),
            palette.title_hover_fill,
        );
    }
    let inset = (window_tokens::GEAR_SIZE - window_tokens::GEAR_ICON) / 2.0;
    draw_settings_icon(ui.painter(), gear.rect.shrink(inset), palette.title_icon);
    gear.on_hover_text(i18n::s().settings_tooltip).clicked()
}

/// Lucide `settings` 近似(16×16):外圈 + 内圈 + 8 个齿(visual.md §5.2 图标族)。
fn draw_settings_icon(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.4, color);
    let center = rect.center();
    painter.circle_stroke(center, 4.6, stroke);
    painter.circle_stroke(center, 1.8, stroke);
    for i in 0..8 {
        let angle = std::f32::consts::TAU * i as f32 / 8.0;
        let dir = egui::vec2(angle.cos(), angle.sin());
        painter.line_segment([center + dir * 5.4, center + dir * 7.0], stroke);
    }
}

/// 行点击 → 选择集更新(纯逻辑,可测;概念如 Finder):
/// shift = 从锚点到该行的范围选(覆盖旧选、锚点不动);⌘ = 切换单项;单击 = 清选单选。
fn apply_click(
    selection: &mut BTreeSet<usize>,
    anchor: &mut Option<usize>,
    row: usize,
    shift: bool,
    cmd: bool,
) {
    if shift {
        let start = anchor.unwrap_or(row);
        let (lo, hi) = if start <= row {
            (start, row)
        } else {
            (row, start)
        };
        selection.clear();
        selection.extend(lo..=hi);
    } else if cmd {
        if !selection.remove(&row) {
            selection.insert(row);
        }
        *anchor = Some(row);
    } else {
        selection.clear();
        selection.insert(row);
        *anchor = Some(row);
    }
}

/// 右键点击行的选中收敛(纯逻辑,可测):点击行不在选中集 → 清旧选、设为唯一选中
/// 并立锚(标准表格语义);已在选中集 → 保持多选不动(菜单动作作用于整组)。
fn converge_selection(selection: &mut BTreeSet<usize>, anchor: &mut Option<usize>, row: usize) {
    if !selection.contains(&row) {
        selection.clear();
        selection.insert(row);
        *anchor = Some(row);
    }
}

/// 界面方向(asc = 期望升序显示)→ 协议 desc(反转自然方向,见 protocol::Request::Query
/// 与 engine::query::execute_sorted 注释)。简洁规则:asc 与自然方向相同 → false。
/// Relevance 无方向概念:恒 false(引擎忽略,不发该字段)。
fn request_desc(key: SortKey, asc: bool) -> bool {
    match key {
        SortKey::Relevance => false,
        _ => natural_asc(key) != asc,
    }
}

/// 引擎各键的自然方向(true = 升序;desc 反转它,见模块注释「方向语义」)。
/// Relevance = 匹配质量序,无方向概念(不参与反转)。
fn natural_asc(key: SortKey) -> bool {
    !matches!(key, SortKey::Mtime | SortKey::Relevance)
}

/// 数值文案(原型口径):保留一位小数,整数值去掉 `.0`
/// (18 KB / 2.4 MB / 12 MB / 匹配 3 ms / 读取 12 ms)。
fn format_metric(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

/// 状态栏计数段:N 项 = 命中总数(total);返回条数 < total 时注「展示前 K 条」。
fn counts_text(total: u32, shown: usize, elapsed_ms: f64, stat_ms: f64) -> String {
    format::counts(
        total,
        shown,
        &format_metric(elapsed_ms),
        &format_metric(stat_ms),
    )
}

/// 状态栏左侧主文本(§3.6 / §3.7,纯函数):计数/耗时/截断/排序中/索引状态,
/// 各段统一用「 · 」连接(不用「|」分隔条)。Idle(空词)隐藏计量段,只留索引状态;
/// Empty(total=0 且有查询)只报「0 项」。
fn status_text(
    idle: bool,
    total: u32,
    shown: usize,
    elapsed_ms: f64,
    stat_ms: f64,
    index: Option<&str>,
    sorting: bool,
) -> String {
    let mut segments: Vec<String> = Vec::new();
    if !idle {
        if total == 0 {
            segments.push(format::items(0));
        } else {
            segments.push(counts_text(total, shown, elapsed_ms, stat_ms));
        }
        if sorting {
            segments.push(i18n::s().status_sorting.into());
        }
    }
    if let Some(note) = index {
        segments.push(note.into());
    }
    segments.join(" · ")
}

/// 窗口内缺口徽标文案(§3.6「N 个目录未收录」)。托盘项 `tray::gaps_label`
/// 带「⚠」前缀;窗口用自绘警示三角替代图标,文本不重复符号。
fn gap_badge_text(gaps: u32) -> String {
    format::gaps(gaps)
}

/// 状态栏一行的交互产出(绘制闭包外落地)。
#[derive(Default)]
struct StatusAction {
    gaps_clicked: bool,
    restart_clicked: bool,
}

/// 状态栏左区各段自然宽(0 = 不显示;由绘制侧量出的像素宽)。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct StatusWidths {
    status: f32,
    stale: f32,
    restart: f32,
    note: f32,
}

/// 状态栏各段矩形(纯几何;绝对坐标)。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct StatusRowLayout {
    status: Option<egui::Rect>,
    stale: Option<egui::Rect>,
    restart: Option<egui::Rect>,
    note: Option<egui::Rect>,
    badge: Option<egui::Rect>,
}

/// 状态栏几何(§3.6,纯函数):先算右侧缺口徽标占位,左侧状态区取剩余宽,
/// 两者之间留 `STATUS_GAP`(16)。
///
/// 左区内容放不下时依次压缩「状态文本 → 瞬时提示 → 警示文本」;重启按钮不压缩,
/// 因此其右缘恒不超过徽标左缘 − 16(长状态不得伸进徽标区)。
fn status_row_layout(
    row: egui::Rect,
    badge_w: f32,
    badge_h: f32,
    widths: StatusWidths,
) -> StatusRowLayout {
    let pad = window_tokens::CONTENT_PAD_X;
    let badge = (badge_w > 0.0).then(|| {
        egui::Rect::from_min_max(
            egui::pos2(row.right() - pad - badge_w, row.center().y - badge_h / 2.0),
            egui::pos2(row.right() - pad, row.center().y + badge_h / 2.0),
        )
    });
    // 左区右界:有徽标 = 徽标左缘 − 16;无徽标 = 右内边距。
    let left_right = badge.map_or(row.right() - pad, |b| b.left() - window_tokens::STATUS_GAP);
    let left = egui::Rect::from_min_max(
        egui::pos2(row.left() + pad, row.top()),
        egui::pos2(left_right.max(row.left() + pad), row.bottom()),
    );

    let present = [
        widths.status > 0.0,
        widths.stale > 0.0,
        widths.restart > 0.0,
        widths.note > 0.0,
    ];
    let gap_count = present.iter().filter(|p| **p).count().saturating_sub(1) as f32;
    let mut fitted = [widths.status, widths.stale, widths.restart, widths.note];
    let mut overflow = (fitted.iter().sum::<f32>() + gap_count * window_tokens::STATUS_GAP
        - left.width())
    .max(0.0);
    for index in [0usize, 3, 1] {
        let take = fitted[index].min(overflow);
        fitted[index] -= take;
        overflow -= take;
        if overflow <= 0.0 {
            break;
        }
    }

    let mut cursor = left.left();
    let mut first = true;
    let mut rects: [Option<egui::Rect>; 4] = [None; 4];
    for (index, width) in fitted.into_iter().enumerate() {
        if width <= 0.0 {
            continue;
        }
        if !first {
            cursor += window_tokens::STATUS_GAP;
        }
        let right = (cursor + width).min(left.right());
        if right <= cursor {
            break;
        }
        rects[index] = Some(egui::Rect::from_min_max(
            egui::pos2(cursor, left.top()),
            egui::pos2(right, left.bottom()),
        ));
        cursor = right;
        first = false;
    }

    StatusRowLayout {
        status: rects[0],
        stale: rects[1],
        restart: rects[2],
        note: rects[3],
        badge,
    }
}

/// 状态栏单段文本的自然宽(不换行;空串 = 0)。
fn status_text_width(ui: &egui::Ui, text: &str) -> f32 {
    if text.is_empty() {
        return 0.0;
    }
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            ui_font::id(window_tokens::FONT_STATUS),
            egui::Color32::PLACEHOLDER,
        )
        .size()
        .x
}

/// 状态栏单段文本(按 rect 宽截断;垂直居中)。
fn draw_status_text(ui: &egui::Ui, rect: egui::Rect, text: &str, color: egui::Color32) {
    if rect.width() <= 0.0 || text.is_empty() {
        return;
    }
    let galley = egui::WidgetText::from(
        egui::RichText::new(text)
            .size(ui_font::size(window_tokens::FONT_STATUS))
            .color(color),
    )
    .into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        rect.width(),
        egui::TextStyle::Body,
    );
    let pos = egui::pos2(rect.left(), rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, color);
}

/// 状态栏内容行(高 28,§3.6):左 = 状态文本(11pt status_text)+ 排序未生效
/// 错误色文字/重启动作 + 瞬时提示;右 = 缺口徽标(仅 gaps>0,绝对定位)。
/// 左右内边距 16、子元素间距 16。
fn draw_status_row(
    ui: &mut egui::Ui,
    row: egui::Rect,
    st: &ResultsState,
    palette: &FullWindowPalette,
) -> StatusAction {
    let gaps = st.gaps();
    let mut action = StatusAction::default();
    let error = palette.error_text;
    let index = index_note(st.engine.as_ref(), &st.session.state, st.engine_known);
    let text = status_text(
        st.input.is_empty(),
        st.total,
        st.results.len(),
        st.elapsed_ms,
        st.stat_ms,
        index.as_deref(),
        st.sorting_inflight,
    );
    let stale_text = st.sort_stale.then_some(i18n::s().engine_outdated_sort);
    let restart_text = st.sort_stale.then_some(if st.engine_restarting {
        i18n::s().restarting
    } else {
        i18n::s().restart_engine
    });
    let note = st.status_note.as_ref().map(StatusNote::text);
    let note = note.as_deref();
    // 打开/查询失败走错误红;非错误提示(预留)用中性文本色。
    let note_color = if st.status_note_is_error {
        error
    } else {
        palette.status_text
    };

    // 缺口徽标(右侧,绝对定位):文本 + 自绘警示三角。先量宽,左侧状态区
    // 只使用剩余宽,保证长状态不会伸进徽标区。
    let badge_text = (!gaps.is_empty()).then(|| gap_badge_text(gaps.len() as u32));
    let badge_galley = badge_text.as_deref().map(|text| {
        ui.painter().layout_no_wrap(
            text.to_owned(),
            ui_font::id(window_tokens::FONT_STATUS),
            palette.gap_badge_text,
        )
    });
    let badge_w = badge_galley.as_ref().map_or(0.0, |galley| {
        window_tokens::GAP_ICON
            + window_tokens::GAP_BADGE_GAP
            + galley.size().x
            + 2.0 * window_tokens::GAP_BADGE_PAD.1
    });
    let badge_h = badge_galley.as_ref().map_or(0.0, |galley| {
        (galley.size().y + 2.0 * window_tokens::GAP_BADGE_PAD.0)
            .max(window_tokens::GAP_ICON + 2.0 * window_tokens::GAP_BADGE_PAD.0)
    });
    let stale_w = stale_text.map_or(0.0, |t| status_text_width(ui, t));
    let restart_w = restart_text.map_or(0.0, |t| status_text_width(ui, t));
    let note_w = note.map_or(0.0, |t| status_text_width(ui, t));
    let layout = status_row_layout(
        row,
        badge_w,
        badge_h,
        StatusWidths {
            status: status_text_width(ui, &text),
            stale: stale_w,
            restart: restart_w,
            note: note_w,
        },
    );

    if let Some(rect) = layout.status {
        draw_status_text(ui, rect, &text, palette.status_text);
    }
    // 排序未生效(tech 定案:请求键与回显不符):错误色文字 + 重启动作(11pt)。
    if let (Some(rect), Some(label)) = (layout.stale, stale_text) {
        draw_status_text(ui, rect, label, error);
    }
    if let (Some(rect), Some(label)) = (layout.restart, restart_text) {
        if st.engine_restarting {
            draw_status_text(ui, rect, label, palette.status_text);
        } else {
            draw_status_text(ui, rect, label, error);
            let resp = ui.interact(
                rect,
                ui.scope_id().with("results-restart"),
                egui::Sense::click(),
            );
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            action.restart_clicked = resp.clicked();
        }
    }
    // 瞬时提示(打开失败/查询失败):11pt,按左区剩余宽截断。
    if let (Some(rect), Some(note)) = (layout.note, note) {
        draw_status_text(ui, rect, note, note_color);
    }

    // 徽标绝对定位后注册(命中优先于左侧内容;几何上两者已不重叠)。
    if let (Some(badge), Some(galley)) = (layout.badge, badge_galley) {
        let resp = ui.interact(
            badge,
            ui.scope_id().with("results-gap-badge"),
            egui::Sense::click(),
        );
        let fill = if resp.hovered() {
            palette.gap_badge_fill.gamma_multiply(1.5)
        } else {
            palette.gap_badge_fill
        };
        ui.painter().rect_filled(
            badge,
            egui::CornerRadius::same(window_tokens::GAP_BADGE_RADIUS),
            fill,
        );
        let icon = egui::Rect::from_min_size(
            egui::pos2(
                badge.left() + window_tokens::GAP_BADGE_PAD.1,
                badge.center().y - window_tokens::GAP_ICON / 2.0,
            ),
            egui::Vec2::splat(window_tokens::GAP_ICON),
        );
        draw_warning_triangle(ui.painter(), icon, palette.gap_badge_text);
        ui.painter().galley(
            egui::pos2(
                icon.right() + window_tokens::GAP_BADGE_GAP,
                badge.center().y - galley.size().y / 2.0,
            ),
            galley,
            palette.gap_badge_text,
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        action.gaps_clicked = resp.clicked();
    }
    action
}

/// 缺口徽标的警示三角(12×12,自绘;Lucide `triangle-alert` 风格:三角轮廓 +
/// 竖线 + 点)。窗口内用图标替代托盘文案的「⚠」前缀。
fn draw_warning_triangle(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.2, color);
    let center_x = rect.center().x;
    painter.add(egui::Shape::closed_line(
        vec![
            egui::pos2(center_x, rect.top() + 1.0),
            egui::pos2(rect.right() - 1.0, rect.bottom() - 1.5),
            egui::pos2(rect.left() + 1.0, rect.bottom() - 1.5),
        ],
        stroke,
    ));
    painter.line_segment(
        [
            egui::pos2(center_x, rect.top() + 4.5),
            egui::pos2(center_x, rect.top() + 7.5),
        ],
        stroke,
    );
    painter.circle_filled(egui::pos2(center_x, rect.top() + 9.3), 0.7, color);
}

/// sort_echo 校验(tech 定案,纯函数):仅「请求了非 relevance 键」时校验回显——
/// 缺省/relevance 不校验(旧 daemon 无 echo 字段亦兼容)。回显为空串(旧 daemon
/// 忽略 sort)或异键 = 排序未生效。desc 方向不在 echo 范围(Task 19b 定案),
/// 不比对:文案只说排序未生效,不声称方向。
fn sort_not_applied(requested: Option<&str>, echo: &str) -> bool {
    requested.is_some_and(|key| echo != key)
}

/// 缺口清单展示切片(与 CLI `status` 同口径:最多 20 行 + 余量计数)。
fn gaps_display(gaps: &[Gap]) -> (&[Gap], usize) {
    const MAX: usize = 20;
    let shown = gaps.len().min(MAX);
    (&gaps[..shown], gaps.len() - shown)
}

/// 缺口面板(状态栏上方:标题 + 逐行「目录 + 原因」;超 20 行给余量计数)。
/// 数据 = status.gaps(与 CLI `status` 同源;行格式同 CLI 口径)。
fn draw_gaps_panel(ui: &mut egui::Ui, gaps: &[Gap]) {
    let (rows, rest) = gaps_display(gaps);
    // 面板与状态行同向左内边距 16(状态栏面板 frame 为 NONE,需在此自排)。
    let pad = window_tokens::CONTENT_PAD_X;
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.add_space(pad);
        ui.label(
            egui::RichText::new(format::gaps_header(gaps.len()))
                .size(ui_font::size(window_tokens::FONT_STATUS))
                .weak(),
        );
    });
    egui::ScrollArea::vertical()
        .max_height(140.0)
        .id_salt("gaps-panel")
        .show(ui, |ui| {
            for g in rows {
                ui.horizontal(|ui| {
                    ui.add_space(pad);
                    ui.spacing_mut().item_spacing.x = window_tokens::GAP_BADGE_GAP;
                    ui.label(
                        egui::RichText::new(&g.path)
                            .size(ui_font::size(window_tokens::FONT_STATUS))
                            .color(ui.visuals().text_color()),
                    );
                    ui.label(
                        egui::RichText::new(format!("({})", g.reason))
                            .size(ui_font::size(window_tokens::FONT_STATUS))
                            .weak(),
                    );
                });
            }
        });
    if rest > 0 {
        ui.horizontal(|ui| {
            ui.add_space(pad);
            let rest = rest.to_string();
            ui.label(
                egui::RichText::new(i18n::fill(i18n::s().gaps_more, &[("n", rest.as_str())]))
                    .size(ui_font::size(window_tokens::FONT_STATUS))
                    .weak(),
            );
        });
    }
    ui.add_space(4.0);
}

/// 索引状态段(product §7 完整窗口列)。引擎快照优先;快照缺失(未运行/取不到)时
/// 会话态兜底(与悬浮条角标同口径:warming/引擎拉起中可见)。
/// 返回 None = 尚未取到(窗口刚开、状态补查未回),不占位。
fn index_note(engine: Option<&EngineStatus>, state: &State, known: bool) -> Option<String> {
    let s = i18n::s();
    if let Some(e) = engine {
        return Some(match e.state.as_str() {
            client::state::BUILDING => format::index_building_count(u64::from(e.entry_count)),
            client::state::READY => s.index_ready.into(),
            client::state::RECONCILING => s.index_reconciling.into(),
            client::state::DEGRADED => s.index_degraded.into(),
            client::state::OFFLINE => {
                let time = mtime_text(Some(e.built_at));
                i18n::fill(s.index_offline_since, &[("time", time.as_str())])
            }
            other => other.to_string(),
        });
    }
    match state {
        State::Warming => Some(s.index_building.into()),
        State::EngineStarting => Some(s.engine_starting.into()),
        State::Offline => Some(s.offline_index.into()), // 无快照:生成时刻不可得
        _ if known => Some(s.engine_not_running.into()),
        _ => None,
    }
}

/// 空态主/副文案(§2.7 空态文案表 + 完整窗口 §3.7;纯函数)。
/// 空词恒为 Idle 文案;Offline 有结果时照常渲染表格,无结果按 Empty;
/// Error 给出可操作副文案(不再只由状态栏承担)。
fn empty_state_text(input: &str, state: &State) -> (&'static str, &'static str) {
    let s = i18n::s();
    if input.is_empty() {
        return (s.empty_start, s.empty_start_sub);
    }
    match state {
        State::EngineStarting => (s.empty_engine_starting, s.empty_wait),
        State::Warming => (s.empty_warming, s.empty_warming_sub),
        State::Error(_) => (s.empty_search_failed, s.empty_search_failed_sub),
        // 首次查询尚无结果(防抖/在途)不误报「无匹配项」。
        State::Searching => (s.empty_searching, s.empty_wait),
        State::Empty | State::Offline | State::Idle => (s.empty_no_match, s.empty_no_match_sub),
    }
}

/// 空态(表格区,表头保留):主文案 14pt 文件名色 + 副文案 12pt 目录色,
/// 纵向整块居中、间距 4(§2.7 空态几何)。
fn draw_empty_state(
    ui: &egui::Ui,
    area: egui::Rect,
    input: &str,
    state: &State,
    palette: &FullWindowPalette,
) {
    let (main, sub) = empty_state_text(input, state);
    let painter = ui.painter();
    let main_galley = painter.layout_no_wrap(
        main.to_owned(),
        ui_font::id(window_tokens::FONT_EMPTY_TITLE),
        palette.file_name,
    );
    let sub_galley = painter.layout_no_wrap(
        sub.to_owned(),
        ui_font::id(window_tokens::FONT_TABLE),
        // 完整窗口的「所在目录色」= 路径列次要文本色(§4.2 column_text)。
        palette.column_text,
    );
    let main_h = main_galley.size().y;
    let block_h = main_h + window_tokens::EMPTY_TEXT_GAP + sub_galley.size().y;
    let top = area.center().y - block_h / 2.0;
    painter.galley(
        egui::pos2(area.center().x - main_galley.size().x / 2.0, top),
        main_galley,
        palette.file_name,
    );
    painter.galley(
        egui::pos2(
            area.center().x - sub_galley.size().x / 2.0,
            top + main_h + window_tokens::EMPTY_TEXT_GAP,
        ),
        sub_galley,
        palette.column_text,
    );
}

/// 大小显示(与 cli 的 human_size 同边界;自写简版,不引依赖)。
/// 缺失 → 空串(visual.md §3.5:元数据缺失留空,不显示占位符)。
fn size_text(size: Option<u64>) -> String {
    match size {
        Some(bytes) => human_size(bytes),
        None => String::new(),
    }
}

/// 大小列文案(§3.5):目录恒显示「—」(目录大小无意义);文件缺失留空。
fn size_cell(entry: &HitEntry) -> String {
    if entry.is_dir {
        "—".into()
    } else {
        size_text(entry.size)
    }
}

/// 路径展示(纯函数):HOME 前缀替换为 `~`;无 HOME / 非整段前缀时原样返回。
/// 按路径段匹配:`/Users/ab` 不会被 HOME=`/Users/a` 误改。
fn display_path(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_owned();
    };
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return path.to_owned();
    }
    if path == home {
        return "~".into();
    }
    match path.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    }
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{} GB", format_metric(bytes as f64 / GB as f64))
    } else if bytes >= MB {
        format!("{} MB", format_metric(bytes as f64 / MB as f64))
    } else if bytes >= KB {
        format!("{} KB", format_metric(bytes as f64 / KB as f64))
    } else {
        format!("{bytes} B")
    }
}

/// 修改时间显示:unix 秒 → "YYYY-MM-DD HH:MM"(**本地时区**,localtime_r;
/// Finder 同观感——cli 的 fmt_time 是 UTC 且带秒,窗口口径更贴近文件管理器)。
/// 缺失 → 空串(§3.5 留空);0 = 未取,沿用 cli 的「—」约定。
fn mtime_text(mtime: Option<u64>) -> String {
    match mtime {
        Some(secs) => fmt_mtime(secs),
        None => String::new(),
    }
}

fn fmt_mtime(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    // SAFETY: localtime_r 线程安全,结果写入调用方提供的 tm;仅失败时返回空指针。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let t = secs as libc::time_t;
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        log::warn!("localtime_r 失败(secs={secs}),显示占位");
        return "—".into();
    }
    format_ymd_hm(
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
    )
}

/// 分解时间格式化(纯函数;字段来自本地时区分解结果)。
fn format_ymd_hm(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> String {
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 原型口径:整数去 `.0`(18 KB / 12 MB),非整数保留一位(2.4 MB);
    /// 字节 `B` 恒整数。
    #[test]
    fn human_size_bounds() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(18 * 1024), "18 KB");
        assert_eq!(human_size(2048), "2 KB");
        assert_eq!(human_size(12 * 1024 * 1024), "12 MB");
        assert_eq!(human_size(5 * 1024 * 1024), "5 MB");
        assert_eq!(human_size((2.4 * (1024 * 1024) as f64) as u64), "2.4 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3 GB");
        assert_eq!(
            human_size((2.4 * (1024 * 1024 * 1024) as f64) as u64),
            "2.4 GB"
        );
    }

    /// ms 与大小共用同一数值策略:整数去 `.0`,非整数一位。
    #[test]
    fn format_metric_trims_integer_fraction() {
        assert_eq!(format_metric(18.0), "18");
        assert_eq!(format_metric(2.4), "2.4");
        assert_eq!(format_metric(2.04), "2");
        assert_eq!(format_metric(2.05), "2.1");
        assert_eq!(format_metric(0.0), "0");
    }

    #[test]
    fn size_and_mtime_none_render_empty() {
        assert_eq!(size_text(None), "", "元数据缺失留空(visual.md §3.5)");
        assert_eq!(mtime_text(None), "", "元数据缺失留空(visual.md §3.5)");
        assert_eq!(mtime_text(Some(0)), "—", "0 = 未取(与 cli fmt_time 同约定)");
    }

    /// 搜索行几何(§3.3):钉住 52 / 36 / 16 / 14 / 10 / 图标 16×16 的落位。
    #[test]
    fn search_row_geometry_matches_spec() {
        let row = egui::Rect::from_min_size(
            egui::pos2(100.0, 200.0),
            egui::vec2(1080.0, window_tokens::SEARCH_ROW_H),
        );
        let l = search_row_layout(row);

        // 搜索框:窗口宽 − 32,高 36,在 52 行内垂直居中。
        assert_eq!(l.box_rect.width(), 1048.0, "左右内边距各 16");
        assert_eq!(l.box_rect.height(), 36.0);
        assert_eq!(l.box_rect.left() - row.left(), 16.0);
        assert_eq!(row.right() - l.box_rect.right(), 16.0);
        assert_eq!(l.box_rect.top() - row.top(), 8.0, "行内垂直居中");
        assert_eq!(row.bottom() - l.box_rect.bottom(), 8.0);

        // 图标:16×16,框内左内边距 14,垂直居中。
        assert_eq!(l.icon_rect.left() - l.box_rect.left(), 14.0);
        assert_eq!((l.icon_rect.width(), l.icon_rect.height()), (16.0, 16.0));
        assert_eq!(l.icon_rect.center().y, l.box_rect.center().y);

        // 文本区:图标右缘 + 10,右端内边距 14,纵向取满框高。
        assert_eq!(l.text_rect.left() - l.icon_rect.right(), 10.0);
        assert_eq!(l.box_rect.right() - l.text_rect.right(), 14.0);
        assert_eq!(l.text_rect.top(), l.box_rect.top());
        assert_eq!(l.text_rect.bottom(), l.box_rect.bottom());
    }

    /// 表头 / 行几何(§3.4 / §3.5):1080 下名称 444、四列间距 16、右内边距 16。
    #[test]
    fn col_layout_matches_spec_at_default_width() {
        let cols = col_layout(1080.0, [96.0, 160.0, 300.0]);
        assert_eq!(
            cols,
            [(16.0, 444.0), (476.0, 96.0), (588.0, 160.0), (764.0, 300.0)]
        );
        assert_eq!(cols[COL_PATH].0 + cols[COL_PATH].1, 1064.0, "右内边距 16");
    }

    /// 720 最小窗宽:名称恰好 84(设计下限),四列仍留足内边距。
    #[test]
    fn col_layout_at_min_window_keeps_name_minimum() {
        let cols = col_layout(720.0, [96.0, 160.0, 300.0]);
        assert_eq!(cols[COL_NAME].1, 84.0);
        assert_eq!(cols[COL_PATH].0 + cols[COL_PATH].1, 704.0);
    }

    /// 窄宽与非法固定宽:固定列先 clamp 最小宽;名称不足时路径 → 修改时间 →
    /// 大小依次让宽,直到四列最小宽之和(窗口最小 720 下远未触底)。
    #[test]
    fn col_layout_clamps_fixed_and_shrinks_right_to_left() {
        let clamped = col_layout(720.0, [10.0, 10.0, 10.0]);
        assert_eq!(
            (
                clamped[COL_NAME].1,
                clamped[COL_SIZE].1,
                clamped[COL_MTIME].1
            ),
            (350.0, 60.0, 110.0)
        );
        assert_eq!(clamped[COL_PATH].1, 120.0);
        assert_eq!(clamped[COL_PATH].0 + clamped[COL_PATH].1, 704.0);

        let narrow = col_layout(500.0, [96.0, 160.0, 300.0]);
        assert_eq!(narrow[COL_NAME].1, 84.0, "名称保底");
        assert_eq!(narrow[COL_MTIME].1, 120.0, "修改时间只让到够用为止");
        assert_eq!(narrow[COL_PATH].1, 120.0, "路径先让到最小宽");
        assert_eq!(narrow[COL_PATH].0 + narrow[COL_PATH].1, 484.0);

        // 四列全部到达最小宽的总宽下限(32 + 48 + 84 + 60 + 110 + 120 = 454)。
        let floor = col_layout(454.0, [96.0, 160.0, 300.0]);
        assert_eq!(
            (
                floor[COL_NAME].1,
                floor[COL_SIZE].1,
                floor[COL_MTIME].1,
                floor[COL_PATH].1
            ),
            (84.0, 60.0, 110.0, 120.0)
        );
        assert_eq!(floor[COL_PATH].0 + floor[COL_PATH].1, 438.0);
    }

    /// 任意窗口宽(≥四列最小宽):列不重叠、右缘恰在右内边距上。
    #[test]
    fn col_layout_never_overlaps_or_overflows() {
        for total in [454.0, 500.0, 720.0, 1080.0, 1600.0] {
            for fixed in [
                [96.0, 160.0, 300.0],
                [60.0, 110.0, 120.0],
                [400.0, 400.0, 400.0],
            ] {
                let cols = col_layout(total, fixed);
                assert_eq!(cols[COL_NAME].0, 16.0, "左内边距 16");
                for i in 0..3 {
                    assert_eq!(
                        cols[i].0 + cols[i].1 + window_tokens::COLUMN_GAP,
                        cols[i + 1].0,
                        "列间距 16(total={total}, fixed={fixed:?})"
                    );
                }
                assert_eq!(
                    cols[COL_PATH].0 + cols[COL_PATH].1,
                    total - 16.0,
                    "右内边距 16(total={total})"
                );
                assert!(cols[COL_NAME].1 >= NAME_MIN_W);
            }
        }
    }

    /// 列宽拖拽:边界跟随指针,固定列一增一减(名称列吃余宽,只在名称/大小边界
    /// 变化);clamp 最小宽且名称始终 ≥84。
    #[test]
    fn drag_fixed_adjusts_left_fixed_column_and_clamps() {
        let base = [96.0, 160.0, 300.0];
        assert_eq!(
            drag_fixed(ColBoundary::NameSize, base, 10.0, 1080.0),
            [86.0, 160.0, 300.0]
        );
        assert_eq!(
            drag_fixed(ColBoundary::NameSize, base, -10.0, 1080.0),
            [106.0, 160.0, 300.0]
        );
        // 大小/修改时间边界:大小 +10,修改时间 −10(边界右移 10)。
        assert_eq!(
            drag_fixed(ColBoundary::SizeMtime, base, 10.0, 1080.0),
            [106.0, 150.0, 300.0]
        );
        // 修改时间/路径边界:修改时间 +10,路径 −10(边界右移 10)。
        assert_eq!(
            drag_fixed(ColBoundary::MtimePath, base, 10.0, 1080.0),
            [96.0, 170.0, 290.0]
        );
        // 最小宽 clamp。
        assert_eq!(
            drag_fixed(ColBoundary::NameSize, base, 1000.0, 1080.0),
            [60.0, 160.0, 300.0]
        );
        assert_eq!(
            drag_fixed(ColBoundary::SizeMtime, base, 1000.0, 1080.0),
            [146.0, 110.0, 300.0],
            "修改时间到最小宽即停"
        );
        assert_eq!(
            drag_fixed(ColBoundary::SizeMtime, base, -1000.0, 1080.0),
            [60.0, 196.0, 300.0],
            "大小到最小宽即停"
        );
        assert_eq!(
            drag_fixed(ColBoundary::MtimePath, base, 1000.0, 1080.0),
            [96.0, 340.0, 120.0],
            "路径到最小宽即停"
        );
        assert_eq!(
            drag_fixed(ColBoundary::MtimePath, base, -1000.0, 1080.0),
            [96.0, 110.0, 350.0],
            "修改时间到最小宽即停"
        );
        // 名称 ≥84:名称/大小边界左拖的上限受固定列预算约束(1080 下 916)。
        assert_eq!(
            drag_fixed(ColBoundary::NameSize, base, -1000.0, 1080.0),
            [456.0, 160.0, 300.0]
        );
        // 入参低于最小宽先 clamp(防御旧持久化值/窄窗)。
        assert_eq!(
            drag_fixed(ColBoundary::MtimePath, [10.0, 10.0, 10.0], 0.0, 720.0),
            [60.0, 110.0, 120.0]
        );
    }

    /// 窄窗投影态拖拽回归(审查发现):stored 固定宽超窄窗预算时,拖拽必须从
    /// 当前实际显示的宽度起算。720 + stored [456,160,300] 投影为
    /// [84,326,110,120];名称/大小边界右拖 10 → 名称 94、大小 316,
    /// 修改时间 / 路径保持 110 / 120(不跳回 stored)。
    #[test]
    fn drag_from_projected_layout_does_not_jump_in_narrow_window() {
        let stored = [456.0, 160.0, 300.0];
        let before = col_layout(720.0, stored);
        assert_eq!(before[COL_NAME].1, NAME_MIN_W);
        assert_eq!(
            (before[COL_SIZE].1, before[COL_MTIME].1, before[COL_PATH].1),
            (326.0, 110.0, 120.0),
            "窄窗投影:超预算固定列按 路径 → 修改时间 → 大小 让宽"
        );

        let start_fixed = [before[COL_SIZE].1, before[COL_MTIME].1, before[COL_PATH].1];
        let new_fixed = drag_fixed(ColBoundary::NameSize, start_fixed, 10.0, 720.0);
        let after = col_layout(720.0, new_fixed);
        assert_eq!(
            after[COL_NAME].1,
            before[COL_NAME].1 + 10.0,
            "名称随边界右移 +10"
        );
        assert_eq!(after[COL_SIZE].1, before[COL_SIZE].1 - 10.0, "大小反向 -10");
        assert_eq!(after[COL_MTIME].1, before[COL_MTIME].1, "修改时间不跳变");
        assert_eq!(after[COL_PATH].1, before[COL_PATH].1, "路径不跳变");
        assert_eq!(after[COL_PATH].0 + after[COL_PATH].1, 704.0, "右内边距 16");
    }

    /// 行几何:高 30、左内边距 16、图标 16×16 垂直居中、名称起点 = 图标右缘 + 8。
    #[test]
    fn row_cells_geometry_matches_spec() {
        let row = egui::Rect::from_min_size(
            egui::pos2(100.0, 200.0),
            egui::vec2(1080.0, window_tokens::ROW_H),
        );
        let cols = col_layout(1080.0, [96.0, 160.0, 300.0]);
        let c = row_cells(row, &cols);
        assert_eq!(c.rect, row);
        assert_eq!(row.height(), 30.0);
        assert_eq!(c.icon.left() - row.left(), 16.0);
        assert_eq!(c.icon.size(), egui::vec2(16.0, 16.0));
        assert_eq!(c.icon.center().y, row.center().y);
        assert_eq!(c.name.left() - c.icon.right(), 8.0);
        assert_eq!(c.name.left() - row.left(), 40.0, "16 + 图标 16 + 间距 8");
        assert_eq!(
            c.name.right(),
            row.left() + cols[COL_NAME].0 + cols[COL_NAME].1
        );
        assert_eq!(c.size.left() - row.left(), cols[COL_SIZE].0);
        assert_eq!(c.size.width(), cols[COL_SIZE].1);
        assert_eq!(c.mtime.left() - row.left(), cols[COL_MTIME].0);
        assert_eq!(c.path.left() - row.left(), cols[COL_PATH].0);
    }

    #[test]
    fn display_path_replaces_home_prefix() {
        assert_eq!(
            display_path("/Users/a/Documents/report.pdf", Some("/Users/a")),
            "~/Documents/report.pdf"
        );
        assert_eq!(display_path("/Users/a", Some("/Users/a")), "~");
        assert_eq!(display_path("/Users/a/", Some("/Users/a/")), "~/");
        // 按路径段匹配:同前缀但不同目录不得替换。
        assert_eq!(display_path("/Users/ab/x", Some("/Users/a")), "/Users/ab/x");
        // 无 HOME / 不匹配 / HOME 为空:原样。
        assert_eq!(display_path("/tmp/x", None), "/tmp/x");
        assert_eq!(display_path("/tmp/x", Some("/Users/a")), "/tmp/x");
        assert_eq!(display_path("/tmp/x", Some("")), "/tmp/x");
        assert_eq!(display_path("/tmp/x", Some("/")), "/tmp/x");
    }

    #[test]
    fn directory_size_cell_is_dash() {
        let file = HitEntry {
            name: "报告.pdf".into(),
            path: "/tmp/报告.pdf".into(),
            size: Some(2048),
            mtime: None,
            is_dir: false,
        };
        let dir = HitEntry {
            is_dir: true,
            size: Some(999),
            ..file.clone()
        };
        assert_eq!(size_cell(&file), "2 KB");
        assert_eq!(size_cell(&dir), "—", "目录大小恒为占位");
        let no_size = HitEntry {
            size: None,
            ..file.clone()
        };
        assert_eq!(size_cell(&no_size), "", "文件大小缺失留空,不加占位符");
    }

    /// 自定义表头 + 虚拟化行出帧:选中行底可见;1000 行只绘可见行(文本形状
    /// 数量不随总行数增长)。同时覆盖图标兜底路径(无 worker 缓存 → 中性字形)。
    #[test]
    fn custom_table_draws_selected_row_and_virtualizes() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        st.results = (0..1000)
            .map(|i| HitEntry {
                name: format!("区域B报告-{i:04}.pdf"),
                path: format!("/tmp/mce-test/区域B报告-{i:04}.pdf"),
                size: Some(2048),
                mtime: Some(1_700_000_000),
                is_dir: i == 0,
            })
            .collect();
        st.total = 1000;
        st.selection = BTreeSet::from([0]);
        let mut icons = IconCache::without_worker();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
        let palette = window_tokens::dark();
        let selected_bg = out
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == palette.row_selected))
            .count();
        assert_eq!(selected_bg, 1, "选中行底恰好一条");
        // 下界:800 高窗口至少绘出满屏行数(行底矩形高 = ROW_H),防止「只画
        // 1 行也通过虚拟化上界断言」的假绿。
        let visible_rows = out
            .shapes
            .iter()
            .filter(|s| {
                matches!(&s.shape, egui::Shape::Rect(r)
                    if (r.rect.height() - window_tokens::ROW_H).abs() < 0.01)
            })
            .count();
        let min_rows = ((800.0
            - window_tokens::TITLEBAR_H
            - (window_tokens::SEARCH_ROW_H + window_tokens::DIVIDER_H)
            - (window_tokens::HEADER_H + window_tokens::DIVIDER_H)
            - window_tokens::STATUS_H)
            / window_tokens::ROW_H)
            .floor() as usize;
        assert!(
            visible_rows >= min_rows,
            "800 高窗口至少绘出满屏 {min_rows} 行(实际 {visible_rows})"
        );
        let texts = out
            .shapes
            .iter()
            .filter(|s| matches!(&s.shape, egui::Shape::Text(_)))
            .count();
        assert!(
            texts < 200,
            "1000 行应只绘可见行(实际 {texts} 个文本形状;非虚拟化会 >4000)"
        );
        out.textures_delta.clear();
    }

    /// 表头列间竖线（2026-10-18 需求）：仅表头行内 3 条 1px 竖线，取 divider
    /// token，居中于列边界；上下内缩 8（不顶头），结果区不出现。
    #[test]
    fn header_column_separators_only_in_header_row() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        let mut icons = IconCache::without_worker();
        let screen = egui::vec2(1200.0, 800.0);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
        let palette = window_tokens::dark();
        let header_top =
            window_tokens::TITLEBAR_H + window_tokens::SEARCH_ROW_H + window_tokens::DIVIDER_H;
        let sep_h = window_tokens::HEADER_H - 2.0 * window_tokens::HEADER_SEP_INSET_Y;
        let mut separators: Vec<egui::Rect> = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Rect(r)
                    if r.fill == palette.divider
                        && (r.rect.width() - window_tokens::DIVIDER_H).abs() < 0.01
                        && (r.rect.height() - sep_h).abs() < 0.01 =>
                {
                    Some(r.rect)
                }
                _ => None,
            })
            .collect();
        separators.sort_by(|a, b| a.center().x.partial_cmp(&b.center().x).unwrap());
        let cols = col_layout(
            1200.0,
            [
                window_tokens::COL_SIZE_W,
                window_tokens::COL_MTIME_W,
                window_tokens::COL_PATH_W,
            ],
        );
        let expected: Vec<f32> = (0..3)
            .map(|i| cols[i].0 + cols[i].1 + window_tokens::COLUMN_GAP / 2.0)
            .collect();
        let expected_top = header_top + window_tokens::HEADER_SEP_INSET_Y;
        let expected_bottom =
            header_top + window_tokens::HEADER_H - window_tokens::HEADER_SEP_INSET_Y;
        assert_eq!(separators.len(), 3, "仅表头 3 条列间竖线");
        for (rect, want_x) in separators.iter().zip(expected.iter()) {
            assert!(
                (rect.center().x - want_x).abs() < 0.51,
                "竖线 x={} 期望 {want_x}",
                rect.center().x
            );
            assert!(
                (rect.top() - expected_top).abs() < 0.01
                    && (rect.bottom() - expected_bottom).abs() < 0.01,
                "竖线上下内缩 8、只出现在表头行(实际 {:?})",
                rect
            );
        }
        out.textures_delta.clear();
    }

    /// 表头拖拽接线(窄窗):720 宽 + stored 超预算时,在名称/大小边界右拖 10px,
    /// `draw_impl` 必须按投影宽归一化且边界跟手(守 call site 不退回 stored 起算)。
    #[test]
    fn header_drag_at_min_window_follows_pointer() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        st.col_fixed = [456.0, 160.0, 300.0];
        st.results = vec![HitEntry {
            name: "报告.pdf".into(),
            path: "/tmp/报告.pdf".into(),
            size: Some(2048),
            mtime: Some(1_700_000_000),
            is_dir: false,
        }];
        let mut icons = IconCache::without_worker();
        let screen = egui::vec2(720.0, 600.0);
        // 720 投影:名称 84(16..100),名称/大小间隙中心 x = 100 + 8 = 108;
        // 表头垂直中心 = 标题栏 32 + 搜索行 53 + 表头 16 = 101。
        let from = egui::pos2(108.0, 101.0);
        let to = from + egui::vec2(10.0, 0.0);
        let press = egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        };
        let release = egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        };
        // 先空跑一帧注册控件矩形(egui 交互用上一帧的 widget rects 命中)。
        run_frame_with_events(&ctx, &mut st, &mut icons, screen, vec![]);
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![egui::Event::PointerMoved(from), press],
        );
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![egui::Event::PointerMoved(to)],
        );
        run_frame_with_events(&ctx, &mut st, &mut icons, screen, vec![release]);
        assert_eq!(
            st.col_fixed,
            [316.0, 110.0, 120.0],
            "窄窗拖拽按投影宽归一化:名称 84→94、大小 326→316、时间/路径不跳"
        );
    }

    /// 快速拖拽:移动与释放落在同一帧时,按「按下点 → 释放点」总位移应用一次,
    /// 不得因 `drag_delta` 归零而丢位移(边界跟手)。
    #[test]
    fn header_drag_release_in_same_frame_keeps_total_displacement() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        st.results = vec![HitEntry {
            name: "报告.pdf".into(),
            path: "/tmp/报告.pdf".into(),
            size: Some(2048),
            mtime: Some(1_700_000_000),
            is_dir: false,
        }];
        let mut icons = IconCache::without_worker();
        let screen = egui::vec2(720.0, 600.0);
        // 默认固定宽在 720 下名称恰好 84;名称/大小间隙中心 x = 16 + 84 + 8 = 108。
        let from = egui::pos2(108.0, 101.0);
        let to = from + egui::vec2(10.0, 0.0);
        run_frame_with_events(&ctx, &mut st, &mut icons, screen, vec![]);
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![
                egui::Event::PointerMoved(from),
                egui::Event::PointerButton {
                    pos: from,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        // 同帧:移动到目标 + 释放。
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![
                egui::Event::PointerMoved(to),
                egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(
            st.col_fixed,
            [86.0, 160.0, 300.0],
            "释放帧按最终位置应用 10px:大小 96→86;不得丢位移"
        );
        assert!(st.col_drag.is_none(), "释放后拖拽会话结束");
    }

    /// 起拖阈值前的位移不丢:3px + 3px 两帧后释放(未达 egui 6px 拖拽阈值),
    /// 仍按按下点起算的 6px 应用(旧 `drag_delta` 实现会丢前段)。
    #[test]
    fn header_drag_counts_movement_before_threshold() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        st.results = vec![HitEntry {
            name: "报告.pdf".into(),
            path: "/tmp/报告.pdf".into(),
            size: Some(2048),
            mtime: Some(1_700_000_000),
            is_dir: false,
        }];
        let mut icons = IconCache::without_worker();
        let screen = egui::vec2(720.0, 600.0);
        let from = egui::pos2(108.0, 101.0);
        run_frame_with_events(&ctx, &mut st, &mut icons, screen, vec![]);
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![
                egui::Event::PointerMoved(from),
                egui::Event::PointerButton {
                    pos: from,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let mid = from + egui::vec2(3.0, 0.0);
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![egui::Event::PointerMoved(mid)],
        );
        let end = from + egui::vec2(6.0, 0.0);
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![egui::Event::PointerMoved(end)],
        );
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![egui::Event::PointerButton {
                pos: end,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(
            st.col_fixed,
            [90.0, 160.0, 300.0],
            "按下点起算总位移 6px:大小 96→90"
        );
    }

    /// 带原始输入事件的一帧(拖拽等指针交互测试)。
    fn run_frame_with_events(
        ctx: &egui::Context,
        st: &mut ResultsState,
        icons: &mut IconCache,
        screen: egui::Vec2,
        events: Vec<egui::Event>,
    ) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            events,
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, icons));
        out.textures_delta.clear();
    }

    #[test]
    fn fmt_mtime_shape() {
        // 本地时区依赖环境,精确值走实机走查;此处钉形态与占位行为。
        let got = fmt_mtime(1_700_000_000);
        assert_eq!(got.chars().count(), 16, "YYYY-MM-DD HH:MM:{got}");
        assert_eq!(&got[4..5], "-");
        assert_eq!(&got[7..8], "-");
        assert_eq!(&got[10..11], " ");
        assert_eq!(&got[13..14], ":");
        assert_eq!(format_ymd_hm(2026, 10, 16, 9, 3), "2026-10-16 09:03");
    }

    #[test]
    fn natural_directions_match_engine() {
        // 引擎定案:Name/Size/Path 升、Mtime 降(engine::query::execute_sorted 注释)
        assert!(natural_asc(SortKey::Name));
        assert!(natural_asc(SortKey::Size));
        assert!(!natural_asc(SortKey::Mtime));
        assert!(natural_asc(SortKey::Path));
    }

    /// 8 态表(四键 × 升/降)逐行钉:desc = asc 与自然方向不同
    /// (Name/Size/Path 自然升、Mtime 自然降;Task 19b 定案)。
    #[test]
    fn request_desc_encodes_ui_direction() {
        // 自然方向为升:asc 即自然,desc=false;降需反转,desc=true
        assert!(!request_desc(SortKey::Name, true), "Name 升 = 自然");
        assert!(request_desc(SortKey::Name, false), "Name 降 = 反转");
        assert!(!request_desc(SortKey::Size, true), "Size 升 = 自然");
        assert!(request_desc(SortKey::Size, false), "Size 降 = 反转");
        assert!(!request_desc(SortKey::Path, true), "Path 升 = 自然");
        assert!(request_desc(SortKey::Path, false), "Path 降 = 反转");
        // 自然方向为降(mtime):asc 与自然相反,desc=true;降即自然,desc=false
        assert!(
            request_desc(SortKey::Mtime, true),
            "Mtime 升 = 反转(自然为降)"
        );
        assert!(!request_desc(SortKey::Mtime, false), "Mtime 降 = 自然");
        // Relevance 无方向:恒 false(引擎忽略;不依赖 asc 取值)
        assert!(!request_desc(SortKey::Relevance, true));
        assert!(!request_desc(SortKey::Relevance, false));
    }

    /// 列头点击三态循环:异列按自然方向;同列升 → 降 → 默认匹配质量。
    #[test]
    fn header_click_cycles_asc_desc_default() {
        let ctx = egui::Context::default();
        let mut st = ResultsState::new(ctx, daemon_paths_for_test());
        assert_eq!(st.sort, SortKey::Relevance, "默认 = 匹配质量");

        // 名称:自然升 → 降 → 默认
        st.on_header_click(SortKey::Name);
        assert_eq!((st.sort, st.sort_asc), (SortKey::Name, true));
        st.on_header_click(SortKey::Name);
        assert_eq!((st.sort, st.sort_asc), (SortKey::Name, false));
        st.on_header_click(SortKey::Name);
        assert_eq!(st.sort, SortKey::Relevance, "第三次点同列回默认");

        // 修改时间:自然降 → 升 → 默认
        st.on_header_click(SortKey::Mtime);
        assert_eq!((st.sort, st.sort_asc), (SortKey::Mtime, false));
        st.on_header_click(SortKey::Mtime);
        assert_eq!((st.sort, st.sort_asc), (SortKey::Mtime, true));
        st.on_header_click(SortKey::Mtime);
        assert_eq!(st.sort, SortKey::Relevance, "第三次点同列回默认");

        // 换列:直接进入新列自然方向,不是回默认
        st.on_header_click(SortKey::Size);
        assert_eq!((st.sort, st.sort_asc), (SortKey::Size, true));
        st.on_header_click(SortKey::Mtime);
        assert_eq!((st.sort, st.sort_asc), (SortKey::Mtime, false));
    }

    #[test]
    fn selection_click_modes() {
        let mut sel = BTreeSet::new();
        let mut anchor = None;
        // 单击:清选单选 + 锚点
        apply_click(&mut sel, &mut anchor, 3, false, false);
        assert_eq!(sel, BTreeSet::from([3]));
        assert_eq!(anchor, Some(3));
        apply_click(&mut sel, &mut anchor, 7, false, false);
        assert_eq!(sel, BTreeSet::from([7]), "单击清掉旧选");
        // shift:锚点(7)→ 该行范围,覆盖旧选;锚点不动
        apply_click(&mut sel, &mut anchor, 4, true, false);
        assert_eq!(sel, BTreeSet::from([4, 5, 6, 7]), "范围含锚点与目标行");
        assert_eq!(anchor, Some(7), "范围选不动锚点");
        apply_click(&mut sel, &mut anchor, 9, true, false);
        assert_eq!(sel, BTreeSet::from([7, 8, 9]), "再次 shift 以原锚点重算");
        // ⌘:切换单项
        apply_click(&mut sel, &mut anchor, 8, false, true);
        assert_eq!(sel, BTreeSet::from([7, 9]), "⌘ 点选移出");
        apply_click(&mut sel, &mut anchor, 2, false, true);
        assert_eq!(sel, BTreeSet::from([2, 7, 9]), "⌘ 点选加入");
        assert_eq!(anchor, Some(2), "⌘ 点击移动锚点");
    }

    #[test]
    fn context_click_converges_selection() {
        let mut sel = BTreeSet::from([1, 2, 3]);
        let mut anchor = Some(1);
        converge_selection(&mut sel, &mut anchor, 3);
        assert_eq!(sel, BTreeSet::from([1, 2, 3]), "点击行已在选中集:保持多选");
        assert_eq!(anchor, Some(1), "多选保持时锚点不动");
        converge_selection(&mut sel, &mut anchor, 7);
        assert_eq!(sel, BTreeSet::from([7]), "点击行不在选中集:收敛为唯一选中");
        assert_eq!(anchor, Some(7));
    }

    #[test]
    fn shift_click_without_anchor_selects_single() {
        let mut sel = BTreeSet::new();
        let mut anchor = None;
        apply_click(&mut sel, &mut anchor, 5, true, false);
        assert_eq!(sel, BTreeSet::from([5]), "无锚点 shift = 单选且立锚");
        assert_eq!(anchor, None, "shift 不设锚点(概念:锚点由单击/⌘ 建立)");
    }

    #[test]
    fn counts_text_notes_truncation() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(
                counts_text(42, 42, 3.2, 1.1),
                "42 项 · 匹配 3.2 ms · 读取 1.1 ms",
                "未截断不加注"
            );
            assert_eq!(
                counts_text(18, 18, 3.0, 12.0),
                "18 项 · 匹配 3 ms · 读取 12 ms",
                "整数值去 .0(原型口径)"
            );
            assert_eq!(
                counts_text(8000, 1000, 12.5, 40.0),
                "8000 项 · 匹配 12.5 ms · 读取 40 ms · 展示前 1000 条",
                "K < total 注展示条数;非整数保留一位"
            );
        });
    }

    #[test]
    fn counts_text_pluralizes_in_english() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(counts_text(1, 1, 2.0, 3.0), "1 项 · 匹配 2 ms · 读取 3 ms");
        });
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(
                counts_text(1, 1, 2.0, 3.0),
                "1 item · Match 2 ms · Read 3 ms"
            );
            assert_eq!(
                counts_text(2, 1, 2.0, 3.0),
                "2 items · Match 2 ms · Read 3 ms · Showing first 1"
            );
        });
    }

    #[test]
    fn index_note_six_states() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let idle = State::Idle;
            let eng = |s: &str, built_at: u64| EngineStatus {
                state: s.into(),
                entry_count: 12,
                gaps: Vec::new(),
                built_at,
            };
            assert_eq!(
                index_note(Some(&eng(client::state::BUILDING, 0)), &idle, true).as_deref(),
                Some("索引构建中(12 条)")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::READY, 0)), &idle, true).as_deref(),
                Some("就绪")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::RECONCILING, 0)), &idle, true).as_deref(),
                Some("索引补齐中(结果可能不全)")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::DEGRADED, 0)), &idle, true).as_deref(),
                Some("索引重扫中(结果可能不全)")
            );
            // 离线带生成时刻(§7 完整窗口列「离线索引 · 生成于 …」;0 = 未取 → 占位)
            assert_eq!(
                index_note(Some(&eng(client::state::OFFLINE, 0)), &idle, true).as_deref(),
                Some("离线索引 · 生成于 —")
            );
            assert_eq!(
                index_note(
                    Some(&eng(client::state::OFFLINE, 1_700_000_000)),
                    &idle,
                    true
                ),
                Some(format!("离线索引 · 生成于 {}", fmt_mtime(1_700_000_000)))
            );
            assert_eq!(
                index_note(None, &idle, true).as_deref(),
                Some("引擎未运行"),
                "已查过且无状态 = 未运行/无索引"
            );
            assert_eq!(index_note(None, &idle, false), None, "未查过不占位");
        });
    }

    /// 快照缺失时会话态兜底(六态全出口:warming / 引擎拉起中 / 离线在状态栏可见)。
    #[test]
    fn index_note_falls_back_to_session_state() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(
                index_note(None, &State::Warming, true).as_deref(),
                Some("索引构建中"),
                "warming(索引未挂载)占位非错误"
            );
            assert_eq!(
                index_note(None, &State::EngineStarting, true).as_deref(),
                Some("引擎启动中")
            );
            assert_eq!(
                index_note(None, &State::Offline, true).as_deref(),
                Some("离线索引"),
                "无快照:生成时刻不可得,只报离线"
            );
            // 有快照时快照优先(会话态不覆盖)
            let eng = EngineStatus {
                state: client::state::READY.into(),
                entry_count: 1,
                gaps: Vec::new(),
                built_at: 0,
            };
            assert_eq!(
                index_note(Some(&eng), &State::Warming, true).as_deref(),
                Some("就绪")
            );
        });
    }

    /// 英文索引状态:构建条目数单复数、各快照态与会话态兜底逐项钉住。
    #[test]
    fn index_note_is_localized_in_english() {
        i18n::with_lang(i18n::Lang::En, || {
            let idle = State::Idle;
            let eng = |state: &str, entry_count: u32, built_at: u64| EngineStatus {
                state: state.into(),
                entry_count,
                gaps: Vec::new(),
                built_at,
            };
            assert_eq!(
                index_note(Some(&eng(client::state::BUILDING, 1, 0)), &idle, true).as_deref(),
                Some("Building index (1 entry)")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::BUILDING, 2, 0)), &idle, true).as_deref(),
                Some("Building index (2 entries)")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::READY, 2, 0)), &idle, true).as_deref(),
                Some("Ready")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::RECONCILING, 2, 0)), &idle, true).as_deref(),
                Some("Reconciling index (results may be incomplete)")
            );
            assert_eq!(
                index_note(Some(&eng(client::state::DEGRADED, 2, 0)), &idle, true).as_deref(),
                Some("Rescanning index (results may be incomplete)")
            );
            assert_eq!(
                index_note(
                    Some(&eng(client::state::OFFLINE, 2, 1_700_000_000)),
                    &idle,
                    true
                ),
                Some(format!(
                    "Offline index · Generated {}",
                    fmt_mtime(1_700_000_000)
                ))
            );
            assert_eq!(
                index_note(None, &State::Warming, true).as_deref(),
                Some("Building index")
            );
            assert_eq!(
                index_note(None, &State::EngineStarting, true).as_deref(),
                Some("Engine starting")
            );
            assert_eq!(
                index_note(None, &State::Offline, true).as_deref(),
                Some("Offline index")
            );
            assert_eq!(
                index_note(None, &idle, true).as_deref(),
                Some("Engine not running")
            );
            assert_eq!(index_note(None, &idle, false), None);
        });
    }

    /// 四列表头英文文案;relevance 是默认排序,不对应列头。
    #[test]
    fn column_label_is_localized() {
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(column_label(SortKey::Name), Some("Name"));
            assert_eq!(column_label(SortKey::Size), Some("Size"));
            assert_eq!(column_label(SortKey::Mtime), Some("Date Modified"));
            assert_eq!(column_label(SortKey::Path), Some("Path"));
            assert_eq!(column_label(SortKey::Relevance), None);
        });
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(column_label(SortKey::Name), Some("名称"));
            assert_eq!(column_label(SortKey::Size), Some("大小"));
            assert_eq!(column_label(SortKey::Mtime), Some("修改时间"));
            assert_eq!(column_label(SortKey::Path), Some("路径"));
        });
    }

    /// sort_echo 校验表(tech 定案):缺省/relevance 不校验(旧 daemon 兼容);
    /// 请求列键而回显空串(旧 daemon)或异键 → 排序未生效。
    #[test]
    fn sort_not_applied_only_for_column_requests() {
        assert!(!sort_not_applied(None, ""), "无排序/缺省 relevance:不校验");
        assert!(!sort_not_applied(None, "relevance"));
        assert!(!sort_not_applied(Some("size"), "size"), "回显相符");
        assert!(!sort_not_applied(Some("mtime"), "mtime"));
        assert!(
            sort_not_applied(Some("size"), ""),
            "空串 = 旧 daemon 忽略 sort"
        );
        assert!(sort_not_applied(Some("size"), "relevance"), "回显不符");
        assert!(sort_not_applied(Some("mtime"), "size"), "回显异键");
    }

    /// 缺口清单展示切片:与 CLI status 同口径(20 行封顶 + 余量计数)。
    #[test]
    fn gaps_display_caps_at_20_rows() {
        let g = |i: usize| Gap {
            path: format!("/root/{i}"),
            reason: "Permission denied".into(),
        };
        let all: Vec<Gap> = (0..25).map(g).collect();
        let (rows, rest) = gaps_display(&all);
        assert_eq!(rows.len(), 20, "前 20 行");
        assert_eq!(rest, 5, "余量计数");
        assert_eq!(rows[0].path, "/root/0");
        assert_eq!(rows[19].path, "/root/19");
        let few: Vec<Gap> = (0..3).map(g).collect();
        let (rows, rest) = gaps_display(&few);
        assert_eq!((rows.len(), rest), (3, 0), "不足 20 全展示");
    }

    /// 测试用命中响应(字段缺省值集中一处)。
    fn hits_resp(generation: u64, sort_echo: &str, requested: Option<&'static str>) -> QueryResult {
        QueryResult {
            generation,
            payload: Payload::Hits {
                entries: Vec::new(),
                total: 0,
                elapsed_ms: 0.0,
                stat_ms: 0.0,
                offline: false,
                sort_echo: sort_echo.into(),
                requested_sort: requested,
            },
            engine: None,
        }
    }

    /// 排序请求生命周期:代际丢弃不动在途;当代响应清「排序中…」并按回显落警示;
    /// 失败清警示(结果已清,旧排序无意义)。
    #[test]
    fn sorting_inflight_and_sort_echo_warning_lifecycle() {
        let ctx = egui::Context::default();
        let mut st = ResultsState::new(ctx, daemon_paths_for_test());
        let now = Instant::now();
        st.session.on_input("报告", now);
        let fire = st
            .session
            .pending_at(now + palette::DEBOUNCE)
            .expect("防抖到期应发起");
        st.sorting_inflight = true; // tick 的置位动作(线程不跑;排序中提示 = 此位)

        // 旧代响应:丢弃——排序中保持、警示不落
        st.apply(hits_resp(fire.generation - 1, "", Some("size")));
        assert!(st.sorting_inflight, "旧代丢弃,在途保持");
        assert!(!st.sort_stale, "旧代不落警示");

        // 当代响应 + 旧 daemon 空回显:排序中解除、警示亮起
        st.apply(hits_resp(fire.generation, "", Some("size")));
        assert!(!st.sorting_inflight, "当代响应清「排序中…」");
        assert!(st.sort_stale, "请求 size 回显空串 = 排序未生效");

        // 回显相符的响应:警示清除
        st.session.on_input("报告", now);
        let fire2 = st
            .session
            .pending_at(now + palette::DEBOUNCE * 2)
            .expect("第二次发起");
        st.apply(hits_resp(fire2.generation, "size", Some("size")));
        assert!(!st.sort_stale, "回显相符即清警示");

        // 失败:警示一并清(结果已清)
        st.apply(hits_resp(fire2.generation, "", Some("size")));
        assert!(st.sort_stale);
        st.session.on_input("报告", now);
        let fire3 = st
            .session
            .pending_at(now + palette::DEBOUNCE * 3)
            .expect("第三次发起");
        st.apply(QueryResult {
            generation: fire3.generation,
            payload: Payload::Failed {
                engine_down: false,
                code: Some("warming".into()),
            },
            engine: None,
        });
        assert!(!st.sort_stale, "失败响应不携带排序结论");
        assert!(
            matches!(st.session.state, State::Warming),
            "错误码分流到 Warming"
        );
        assert_eq!(st.status_note, None, "warming 占位非错误:不给红字");
        assert!(!st.status_note_is_error);
    }

    /// 重启动作落地:成功 → 清红字 + 按当前列/方向同串重查;失败 → 红字保留 + 提示原因。
    #[test]
    fn restart_outcome_clears_warning_and_refires() {
        let ctx = egui::Context::default();
        let mut st = ResultsState::new(ctx, daemon_paths_for_test());
        let now = Instant::now();
        st.session.on_input("报告", now);
        let fire = st
            .session
            .pending_at(now + palette::DEBOUNCE)
            .expect("防抖到期应发起");
        st.apply(hits_resp(fire.generation, "", Some("size")));
        assert!(st.sort_stale);

        // 在途:重复发起被忽略(按钮态)
        st.engine_restarting = true;
        st.restart_engine();
        assert!(st.engine_restarting, "在途不重复发起");

        // 成功落地:清红字,并按当前列/方向重查(列头点击同一路径)
        st.sort = SortKey::Size;
        st.sort_asc = true;
        st.apply_restart(RestartOutcome::Ok);
        assert!(!st.engine_restarting);
        assert!(!st.sort_stale, "重启成功清红字");
        assert!(st.status_refresh_pending, "重启后补查引擎状态");
        let refire = st
            .session
            .pending_at(now + palette::DEBOUNCE * 2)
            .expect("重启后同串重查");
        assert_eq!(refire.query, "报告");
        assert_eq!(refire.sort, SortKey::Size, "按当前列重查");
        assert!(!refire.desc, "Size 升 = 自然方向");

        // 失败落地:红字保留 + 原因可见(不静默)
        st.sort_stale = true;
        let failure = restart_failed_message("x");
        st.apply_restart(RestartOutcome::Err(failure.clone()));
        assert!(st.sort_stale, "失败不清红字(排序仍未生效)");
        assert!(!st.engine_restarting);
        assert_eq!(st.status_note, Some(StatusNote::Raw(failure)));
        assert!(st.status_note_is_error, "重启失败是错误态");
    }

    /// 齿轮真实点击:1200×800 帧内点击齿轮中心 → 置设置请求;消费一次即清
    /// (后续帧不得重复开窗)。
    #[test]
    fn gear_click_sets_and_take_consumes_settings_request() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        let mut icons = IconCache::without_worker();
        let screen = egui::vec2(1200.0, 800.0);
        // 齿轮中心:x = 屏宽 − 右内边距 − 齿轮半宽;y = 标题栏半高。
        let gear = egui::pos2(
            screen.x - window_tokens::TITLEBAR_PAD_X - window_tokens::GEAR_SIZE / 2.0,
            window_tokens::TITLEBAR_H / 2.0,
        );
        // 先空跑一帧注册控件矩形(egui 交互用上一帧的 widget rects 命中)。
        run_frame_with_events(&ctx, &mut st, &mut icons, screen, vec![]);
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![
                egui::Event::PointerMoved(gear),
                egui::Event::PointerButton {
                    pos: gear,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        run_frame_with_events(
            &ctx,
            &mut st,
            &mut icons,
            screen,
            vec![egui::Event::PointerButton {
                pos: gear,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(st.take_settings_request(), "齿轮点击应置设置请求");
        assert!(!st.take_settings_request(), "消费即清,不重复开窗");
    }

    /// 状态栏几何(§3.6):底 = 1px 分隔线 + 28 内容;右侧缺口徽标圆角 6、
    /// 右内边距 16、垂直居中;窗口内文案不带「⚠」(自绘三角替代)。
    #[test]
    fn status_bar_geometry_and_gap_chip() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
            st.input = "报告".into();
            st.engine = Some(EngineStatus {
                state: client::state::READY.into(),
                entry_count: 3,
                gaps: vec![
                    Gap {
                        path: "/private/a".into(),
                        reason: "Permission denied".into(),
                    },
                    Gap {
                        path: "/private/b".into(),
                        reason: "Permission denied".into(),
                    },
                ],
                built_at: 0,
            });
            st.engine_known = true;
            let mut icons = IconCache::without_worker();
            let screen = egui::vec2(1200.0, 800.0);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
            out.textures_delta.clear();
            let palette = window_tokens::dark();

            // 状态栏底:29 = 1 分隔线 + 28 内容,贴窗口底。
            let fill = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == palette.status_fill => Some(r.rect),
                    _ => None,
                })
                .expect("状态栏底");
            assert_eq!(
                fill.height(),
                window_tokens::STATUS_H + window_tokens::DIVIDER_H
            );
            assert_eq!(fill.bottom(), screen.y);
            let row_top = fill.top() + window_tokens::DIVIDER_H;

            // 顶部 1px 分隔线恰在状态行上方。
            let divider = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Rect(r)
                        if r.fill == palette.divider
                            && (r.rect.top() - fill.top()).abs() < 0.01
                            && (r.rect.height() - window_tokens::DIVIDER_H).abs() < 0.01 =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .expect("状态栏顶部分隔线");
            assert_eq!(divider.width(), screen.x);
            assert_eq!(divider.bottom(), row_top);

            // 缺口徽标:圆角 6、右内边距 16、在 28 行内垂直居中。
            let chip = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == palette.gap_badge_fill => Some(r),
                    _ => None,
                })
                .expect("缺口徽标");
            assert_eq!(
                chip.corner_radius,
                egui::CornerRadius::same(window_tokens::GAP_BADGE_RADIUS)
            );
            assert_eq!(chip.rect.right(), screen.x - window_tokens::CONTENT_PAD_X);
            assert_eq!(
                chip.rect.center().y,
                row_top + window_tokens::STATUS_H / 2.0
            );
            let texts: Vec<String> = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                    _ => None,
                })
                .collect();
            assert!(
                texts.iter().any(|t| t == "2 个目录未收录"),
                "窗口内徽标文案;实际 {texts:?}"
            );
            assert!(!texts.iter().any(|t| t.contains('⚠')), "不重复托盘前缀");
            out.textures_delta.clear();
        });
    }

    /// 720 宽 + 长状态 + 缺口徽标:徽标先占右侧宽,左区在其左侧 16 处截止;
    /// 重启按钮右缘不得越过徽标左缘 − 16。
    #[test]
    fn status_layout_at_720_keeps_restart_clear_of_badge() {
        let row = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(720.0, 28.0));
        let widths = StatusWidths {
            status: 520.0,
            stale: 140.0,
            restart: 52.0,
            note: 240.0,
        };
        let layout = status_row_layout(row, 116.0, 20.0, widths);
        let badge = layout.badge.expect("有缺口:徽标");
        assert_eq!(badge.right(), 720.0 - window_tokens::CONTENT_PAD_X);
        assert_eq!(badge.left(), 720.0 - 16.0 - 116.0);
        let restart = layout.restart.expect("排序未生效:重启按钮");
        assert!(
            restart.right() <= badge.left() - window_tokens::STATUS_GAP + 0.01,
            "重启按钮右缘 {} 须 ≤ 徽标左缘 − 16 ({})",
            restart.right(),
            badge.left() - window_tokens::STATUS_GAP
        );
        for rect in [layout.status, layout.stale, layout.restart, layout.note]
            .into_iter()
            .flatten()
        {
            assert!(rect.left() >= window_tokens::CONTENT_PAD_X - 0.01);
            assert!(
                rect.right() <= badge.left() - window_tokens::STATUS_GAP + 0.01,
                "左区内容 {rect:?} 不得伸进徽标区"
            );
        }
        // 长状态文本先被压缩(不得把重启按钮挤出去)。
        assert!(layout.status.expect("状态文本").width() < widths.status);
    }

    /// 无徽标:左区右界 = 右内边距(16),内容仍不越界。
    #[test]
    fn status_layout_without_badge_uses_right_padding() {
        let row = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(720.0, 28.0));
        let widths = StatusWidths {
            status: 900.0,
            stale: 0.0,
            restart: 0.0,
            note: 0.0,
        };
        let layout = status_row_layout(row, 0.0, 0.0, widths);
        assert!(layout.badge.is_none());
        let status = layout.status.expect("状态文本");
        assert_eq!(status.left(), window_tokens::CONTENT_PAD_X);
        assert_eq!(status.right(), 720.0 - window_tokens::CONTENT_PAD_X);
    }

    /// 720 宽实机形态出帧:长状态 + 排序未生效 + 重启按钮 + 长提示 + 缺口徽标,
    /// 重启文案（完整未截断）右缘在徽标左缘 − 16 之内;徽标保持右内边距 16。
    #[test]
    fn status_row_at_720_long_state_does_not_reach_badge() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
            st.input = "报告".into();
            st.total = 18;
            st.elapsed_ms = 3.0;
            st.stat_ms = 12.0;
            st.sort_stale = true;
            st.status_note = Some(StatusNote::Raw(
                "查询失败:无法连接引擎,请稍后重试并检查索引状态是否正常".into(),
            ));
            st.status_note_is_error = true;
            st.engine = Some(EngineStatus {
                state: client::state::READY.into(),
                entry_count: 3,
                gaps: vec![
                    Gap {
                        path: "/private/a".into(),
                        reason: "Permission denied".into(),
                    },
                    Gap {
                        path: "/private/b".into(),
                        reason: "Permission denied".into(),
                    },
                ],
                built_at: 0,
            });
            st.engine_known = true;
            let mut icons = IconCache::without_worker();
            let screen = egui::vec2(720.0, 600.0);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
            out.textures_delta.clear();
            let palette = window_tokens::dark();

            let chip = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.fill == palette.gap_badge_fill => Some(r.rect),
                    _ => None,
                })
                .expect("720 宽仍显示缺口徽标");
            assert_eq!(chip.right(), screen.x - window_tokens::CONTENT_PAD_X);

            let restart = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == "重启引擎" => {
                        Some(t.pos.x + t.galley.size().x)
                    }
                    _ => None,
                })
                .expect("排序未生效:重启按钮文案");
            assert!(
                restart <= chip.left() - window_tokens::STATUS_GAP + 0.01,
                "重启按钮右缘 {restart} 须 ≤ 徽标左缘 − 16 ({})",
                chip.left() - window_tokens::STATUS_GAP
            );

            // 左侧状态文本(被截断)同样止于徽标左侧 16。
            let status_right = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text().starts_with("18") => {
                        Some(t.pos.x + t.galley.size().x)
                    }
                    _ => None,
                })
                .fold(0.0f32, f32::max);
            assert!(status_right > 0.0, "状态文本上屏");
            assert!(
                status_right <= chip.left() - window_tokens::STATUS_GAP + 0.01,
                "长状态文本不得伸进徽标区"
            );
        });
    }

    /// 空态(§2.7):表头保留,主/副文案在表格区整块居中、间距 4。
    #[test]
    fn empty_state_is_centered_with_header_kept() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let ctx = egui::Context::default();
            ctx.set_visuals(egui::Visuals::dark());
            let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
            let mut icons = IconCache::without_worker();
            let screen = egui::vec2(1200.0, 800.0);
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));

            let text_shape = |needle: &str| {
                out.shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.text() == needle => Some(t),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("缺文案「{needle}」"))
            };
            let main = text_shape("输入关键词开始搜索");
            let sub = text_shape("可搜文件名，也可搜路径");
            let center_x = screen.x / 2.0;
            assert!((main.pos.x + main.galley.size().x / 2.0 - center_x).abs() < 0.5);
            assert!((sub.pos.x + sub.galley.size().x / 2.0 - center_x).abs() < 0.5);
            let gap = sub.pos.y - (main.pos.y + main.galley.size().y);
            assert!(
                (gap - window_tokens::EMPTY_TEXT_GAP).abs() < 0.01,
                "主副文案间距 4(实际 {gap})"
            );
            // 表头保留(结构不塌陷);Idle 隐藏计量,状态栏无「项」。
            for header in ["名称", "大小", "修改时间", "路径"] {
                let _ = text_shape(header);
            }
            assert!(
                !out.shapes.iter().any(|s| matches!(&s.shape,
                    egui::Shape::Text(t) if t.galley.text().contains(" 项"))),
                "Idle 状态栏隐藏计数"
            );
            out.textures_delta.clear();
        });
    }

    /// 窗口内描边(Task 4):整窗最内层 1px、圆角 12,取 window_stroke token。
    #[test]
    fn window_inner_stroke_uses_token() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        let mut icons = IconCache::without_worker();
        let screen = egui::vec2(1200.0, 800.0);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            ..Default::default()
        };
        let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
        let palette = window_tokens::dark();
        let stroke = out
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Rect(r)
                    if r.stroke.color == palette.window_stroke
                        && (r.stroke.width - window_tokens::WINDOW_STROKE).abs() < f32::EPSILON =>
                {
                    Some(r)
                }
                _ => None,
            })
            .expect("窗口内描边");
        assert_eq!(stroke.rect.min, egui::Pos2::ZERO);
        assert_eq!(stroke.rect.max, screen.to_pos2());
        assert_eq!(
            stroke.corner_radius,
            egui::CornerRadius::same(window_tokens::WINDOW_RADIUS)
        );
        assert_eq!(stroke.stroke_kind, egui::StrokeKind::Inside);
        out.textures_delta.clear();
    }

    /// 缺口面板:展开态随缺口归零自动收起;徽标/面板/红字/按钮出帧不 panic。
    #[test]
    fn gaps_panel_toggle_and_auto_close() {
        let ctx = egui::Context::default();
        let mut st = ResultsState::new(ctx.clone(), daemon_paths_for_test());
        let gap = |p: &str| Gap {
            path: p.into(),
            reason: "Permission denied (os error 13)".into(),
        };
        st.engine = Some(EngineStatus {
            state: client::state::READY.into(),
            entry_count: 3,
            gaps: vec![gap("/private/a"), gap("/private/b")],
            built_at: 0,
        });
        st.engine_known = true;
        assert_eq!(st.gaps().len(), 2, "徽标计数 = status.gaps");

        // 展开(徽标点击的落地;tray 缺口项 = open_gaps 同置位)+ 红字/按钮同帧
        st.gaps_open = true;
        st.sort_stale = true;
        run_frame(&ctx, &mut st);
        assert!(st.gaps_open, "帧绘制不动展开态");

        // 缺口归零 → tick 自动收起(徽标同源消失,面板不留悬空)
        st.engine = Some(EngineStatus {
            state: client::state::READY.into(),
            entry_count: 3,
            gaps: Vec::new(),
            built_at: 0,
        });
        st.tick(Instant::now());
        assert!(!st.gaps_open, "缺口归零即收起");
        run_frame(&ctx, &mut st);
    }

    /// warming(索引未挂载)端到端:桩 daemon 按线上协议回 `Error{code:"warming"}`。
    /// 真实 daemon 的该窗口只在进程启动到索引挂载之间(毫秒级,无法稳定复现),
    /// 桩锁全链:响应码 → QueryError.code → Payload::Failed → State::Warming →
    /// 占位文案(非红字),覆盖 Task 19 新增的错误码接线。
    #[test]
    fn warming_code_from_daemon_renders_placeholder() {
        use std::io::{Read, Write};

        let dir = std::env::temp_dir().join(format!("mce-warming-stub-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("建桩数据目录");
        let sock = dir.join("daemon.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).expect("绑定桩 socket");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let mut len = [0u8; 4];
                if s.read_exact(&mut len).is_err() {
                    continue;
                }
                let n = u32::from_le_bytes(len) as usize;
                let mut buf = vec![0u8; n];
                if s.read_exact(&mut buf).is_err() {
                    continue;
                }
                let Ok(req) = protocol::decode_request(&buf) else {
                    continue;
                };
                let resp = match req {
                    // 查询:索引未挂载(daemon.rs 同文案)
                    protocol::Request::Query { .. } => protocol::Response::Error {
                        code: "warming".into(),
                        message: "索引构建中,暂不可查".into(),
                    },
                    // 其余(状态补查):构建中快照
                    _ => protocol::Response::Status {
                        state: client::state::BUILDING.into(),
                        entry_count: 0,
                        gaps: Vec::new(),
                        io_error_count: 0,
                        built_at: 0,
                        index_file: "/stub/index.bin".into(),
                    },
                };
                let body = protocol::encode(&resp).expect("编码桩响应");
                let _ = s.write_all(&body);
                let _ = s.flush();
            }
        });

        let mut h = LiveHarness::new(DaemonPaths {
            data_dir: dir.clone(),
        });
        h.st.set_query("报告");
        h.settle();
        assert!(
            matches!(h.st.session.state, State::Warming),
            "warming 码 → Warming 占位态(实际 {:?})",
            h.st.session.state
        );
        assert_eq!(h.st.status_note, None, "占位非错误:不给红字");
        assert!(h.st.results.is_empty());
        // 状态栏文案:引擎快照(building)优先 → §7「索引构建中(N 条)」
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let note = index_note(h.st.engine.as_ref(), &h.st.session.state, h.st.engine_known);
            assert_eq!(note.as_deref(), Some("索引构建中(0 条)"));
        });
        h.frame(); // 文案上屏(出帧不 panic)

        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir(&dir);
    }

    /// 测试数据目录(不存在的路径:任何线程(spawn_status)在隔离下快速失败,不碰真实数据)。
    fn daemon_paths_for_test() -> DaemonPaths {
        DaemonPaths {
            data_dir: std::path::PathBuf::from("/nonexistent-mce-test"),
        }
    }

    /// 单帧绘制(无后端;纹理增量显式清空——epaint 要求:不处理须 clear)。
    fn run_frame(ctx: &egui::Context, st: &mut ResultsState) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1200.0, 800.0),
            )),
            ..Default::default()
        };
        let mut icons = IconCache::without_worker();
        let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
        out.textures_delta.clear();
    }

    /// 空态文案映射(visual §2.7/§3.7 + Task 4):主/副文案逐态钉住;
    /// Offline 无结果按 Empty;空词恒为 Idle 文案。
    #[test]
    fn empty_state_copy_maps_all_states() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let idle = ("输入关键词开始搜索", "可搜文件名，也可搜路径");
            assert_eq!(empty_state_text("", &State::Idle), idle);
            assert_eq!(empty_state_text("", &State::Empty), idle, "空词压过会话态");
            assert_eq!(
                empty_state_text("报告", &State::Empty),
                ("无匹配项", "换个关键词试试")
            );
            assert_eq!(
                empty_state_text("报告", &State::Offline),
                ("无匹配项", "换个关键词试试"),
                "离线无结果按 Empty"
            );
            assert_eq!(
                empty_state_text("报告", &State::EngineStarting),
                ("启动引擎中", "请稍候…")
            );
            assert_eq!(
                empty_state_text("报告", &State::Warming),
                ("索引构建中", "部分结果可能暂不可见")
            );
            assert_eq!(
                empty_state_text("报告", &State::Error("x".into())),
                ("搜索失败", "请重试或检查引擎状态")
            );
            assert_eq!(
                empty_state_text("报告", &State::Searching),
                ("搜索中…", "请稍候…")
            );
        });
    }

    #[test]
    fn empty_state_text_is_localized() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(
                empty_state_text("", &State::Idle),
                ("输入关键词开始搜索", "可搜文件名，也可搜路径")
            );
        });
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(
                empty_state_text("", &State::Idle),
                ("Type to search", "Search file names or paths")
            );
            assert_eq!(
                empty_state_text("abc", &State::Empty),
                ("No matches", "Try a different keyword")
            );
        });
    }

    /// 状态栏文本生成(§3.6/§3.7):连接符恒为「 · 」;Idle 隐藏计量;
    /// Empty 只报「0 项」;截断注「展示前 K 条」。
    #[test]
    fn status_text_idle_empty_and_connectors() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(
                status_text(true, 42, 42, 3.2, 1.1, Some("就绪"), true),
                "就绪",
                "Idle 隐藏计量与排序中,只留索引状态"
            );
            assert_eq!(
                status_text(true, 0, 0, 0.0, 0.0, None, false),
                "",
                "Idle 且状态未回:不占位"
            );
            assert_eq!(
                status_text(false, 0, 0, 0.0, 0.0, Some("就绪"), false),
                "0 项 · 就绪",
                "Empty 只报 0 项"
            );
            let normal = status_text(false, 42, 42, 3.2, 1.1, Some("就绪"), false);
            assert_eq!(normal, "42 项 · 匹配 3.2 ms · 读取 1.1 ms · 就绪");
            assert!(normal.contains(" · "), "段间用「 · 」连接");
            assert!(!normal.contains('|'), "不用「|」分隔条");
            assert_eq!(
                status_text(false, 8000, 1000, 12.5, 40.0, Some("就绪"), true),
                "8000 项 · 匹配 12.5 ms · 读取 40 ms · 展示前 1000 条 · 排序中… · 就绪"
            );
            assert_eq!(
                status_text(
                    false,
                    5,
                    5,
                    1.0,
                    2.0,
                    Some("离线索引 · 生成于 2026-10-16 09:03"),
                    false
                ),
                "5 项 · 匹配 1 ms · 读取 2 ms · 离线索引 · 生成于 2026-10-16 09:03",
                "离线生成时刻并入索引段"
            );
        });
    }

    /// 英文状态栏组合:Idle 只留索引段;计数/截断/排序中各段连接。
    #[test]
    fn status_text_is_localized_in_english() {
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(
                status_text(true, 42, 42, 3.2, 1.1, Some("Ready"), true),
                "Ready",
                "Idle 隐藏计量与排序中,只留索引状态"
            );
            assert_eq!(
                status_text(false, 0, 0, 0.0, 0.0, Some("Ready"), false),
                "0 items · Ready"
            );
            assert_eq!(
                status_text(false, 2, 1, 2.0, 3.0, Some("Ready"), true),
                "2 items · Match 2 ms · Read 3 ms · Showing first 1 · Sorting… · Ready"
            );
        });
    }

    /// 缺口徽标文案:窗口内不带「⚠」(自绘三角替代),与托盘文案同源。
    #[test]
    fn gap_badge_text_has_no_warning_glyph() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(gap_badge_text(3), "3 个目录未收录");
            assert_eq!(gap_badge_text(1), "1 个目录未收录");
            assert!(!gap_badge_text(12).contains('⚠'), "窗口内重复「⚠」即失格");
            assert_eq!(
                format!("⚠ {}", gap_badge_text(3)),
                crate::tray::gaps_label(3),
                "与托盘项文案同源(product §2/§3)"
            );
        });
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(gap_badge_text(1), "1 folder not indexed");
            assert_eq!(gap_badge_text(2), "2 folders not indexed");
            assert!(!gap_badge_text(12).contains('⚠'));
        });
    }

    /// 状态栏提示:类型化变体按当前语言渲染,Raw 原样保留外部/技术细节。
    #[test]
    fn status_note_text_is_localized() {
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(StatusNote::EngineNotReady.text(), "Engine not ready");
            assert_eq!(StatusNote::QueryFailed.text(), "Query failed");
            assert_eq!(
                StatusNote::Raw("external detail: x".into()).text(),
                "external detail: x"
            );
        });
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(StatusNote::EngineNotReady.text(), "引擎未就绪");
            assert_eq!(StatusNote::QueryFailed.text(), "查询失败");
        });
    }

    /// 重启失败状态栏前缀随语言切换（原始原因原样附后）。
    #[test]
    fn restart_failure_message_is_localized() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(restart_failed_message("boom"), "引擎重启失败: boom");
        });
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(
                restart_failed_message("boom"),
                "Failed to restart the engine: boom"
            );
        });
    }

    #[test]
    fn sort_column_defaults_plan_spec() {
        // plan Step 1:名称升/大小升/mtime 降/路径升——与引擎自然方向一致
        assert!(natural_asc(SortKey::Name));
        assert!(natural_asc(SortKey::Size));
        assert!(!natural_asc(SortKey::Mtime));
        assert!(natural_asc(SortKey::Path));
        // 列定义与 product §2 列序一致
        let keys: Vec<SortKey> = COLUMNS.to_vec();
        assert_eq!(
            keys,
            [SortKey::Name, SortKey::Size, SortKey::Mtime, SortKey::Path]
        );
    }

    // —— 实机走查(#[ignore]:需本机 daemon/落盘索引与真实数据面)——
    //
    // 跑法(daemon 在线或离线有落盘索引皆可;仓库约定同 engine/tests/fsevents_live.rs):
    //   cargo test -p macEverything-app -- --ignored live_results_window_walkthrough --nocapture
    // 隔离跑法(不碰真实索引;daemon 须以同一 HOME 启动,根经 MCE_ROOTS 指向合成目录):
    //   HOME=$TMP/home MCE_ROOTS=$TMP/root macEverything start
    //   HOME=$TMP/home ./target/debug/deps/macEverything_app-<hash> --ignored \
    //       live_results_window_walkthrough --nocapture
    // ⚠ 排序断言要求 daemon 含 Task 8b 的 sort + Task 19b 的 desc 支持:旧版 daemon
    //    静默忽略 sort(退 relevance)或忽略 desc(降序请求回升序,排序断言失败即
    //    此因);换当前构建起的 daemon 再跑。
    //
    // Task 19 新增两条走查(见各自注释的场景与前置):
    //   live_sort_echo_old_daemon_restart(旧 daemon → 红字 → 重启 → 生效)
    //   live_gaps_badge_and_states(缺口徽标/面板数据 + 离线文案)

    /// 隔离/真实共用的数据目录(HOME 派生;与 CLI/App 同规)。
    fn live_paths() -> DaemonPaths {
        let home = std::env::var_os("HOME").expect("HOME 未设置");
        DaemonPaths {
            data_dir: std::path::PathBuf::from(home)
                .join("Library/Application Support/macEverything"),
        }
    }

    /// 走查台(Task 19 两条走查共用):一帧 = tick + draw(无后端;1200×800 画布)。
    struct LiveHarness {
        ctx: egui::Context,
        st: ResultsState,
        icons: IconCache,
    }

    impl LiveHarness {
        fn new(paths: DaemonPaths) -> Self {
            let ctx = egui::Context::default();
            let st = ResultsState::new(ctx.clone(), paths);
            Self {
                ctx,
                st,
                icons: IconCache::without_worker(),
            }
        }

        fn frame(&mut self) {
            self.st.tick(Instant::now());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let Self { ctx, st, icons } = self;
            let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, icons));
            out.textures_delta.clear(); // 无后端消费;epaint 要求:不处理须 clear
        }

        /// 防抖到期 + 排帧至在途结束;返回是否观测到「排序中…」期。
        fn settle(&mut self) -> bool {
            std::thread::sleep(std::time::Duration::from_millis(90));
            let deadline = Instant::now() + std::time::Duration::from_secs(20);
            let mut saw = false;
            loop {
                self.frame();
                saw |= self.st.sorting_inflight;
                if !self.st.sorting_inflight {
                    return saw;
                }
                assert!(Instant::now() < deadline, "查询 20s 未回");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    /// Task 19 走查①(旧 daemon):排序请求 → 回显不符 → 红字警示 → 重启引擎
    /// (真实 stop → start)→ 警示清除且新引擎上排序真正生效(升序断言)。
    ///
    /// 跑法(隔离;PATH 首段须为**当前构建**的 target/debug,旧 daemon 由绝对路径
    /// 手动起——重启动作经 `ensure_engine` 从 PATH 取新引擎接管):
    /// ```text
    /// T=$(mktemp -d); mkdir -p $T/home $T/root
    /// 造样本:seq 1 30 | xargs -I{} touch $T/root/sample-report-{}.pdf
    /// HOME=$T/home MCE_ROOTS=$T/root <旧构建>/target/debug/macEverything start
    /// HOME=$T/home PATH=<本仓>/target/debug:$PATH \
    ///   cargo test -p macEverything-app -- --ignored live_sort_echo_old_daemon_restart --nocapture
    /// ```
    #[test]
    #[ignore = "实机走查:需隔离 HOME + MCE_ROOTS,且 daemon 为旧版(无 sort_echo)构建"]
    fn live_sort_echo_old_daemon_restart() {
        use std::time::Duration;

        let paths = live_paths();
        assert!(
            client::connect(&paths).is_some(),
            "旧版 daemon 须已按测试注释的跑法在隔离环境启动"
        );
        let mut h = LiveHarness::new(paths.clone());

        // 1) 查询 → 列头排序:请求 size,旧 daemon 回显空串 → 警示位亮起
        h.st.set_query("report");
        h.settle();
        assert!(
            h.st.total > 0,
            "隔离根应预置 sample-report-*.pdf(total={})",
            h.st.total
        );
        assert!(h.st.gaps().is_empty(), "无缺口环境:徽标不显示(gaps 为空)");
        h.st.on_header_click(SortKey::Size);
        assert!(h.settle(), "排序重查应有「排序中…」期");
        assert!(
            h.st.sort_stale,
            "旧 daemon(sort_echo 缺省空串)应触发「引擎版本过旧,排序未生效」"
        );
        println!("[实机] 旧 daemon:sort_stale=true(状态栏红字 + 重启按钮)");

        // 2) 重启引擎(按钮动作本身;真实 stop → start)
        h.st.restart_engine();
        let deadline = Instant::now() + Duration::from_secs(20);
        while h.st.engine_restarting {
            h.frame();
            assert!(Instant::now() < deadline, "重启 20s 未完成");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!h.st.sort_stale, "重启成功:红字应清除");
        println!("[实机] 重启完成:sort_stale=false");

        // 3) 新引擎上排序真正生效:重查(set_query 走全链路;冷启动窗口内重试)
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            h.st.set_query("report");
            h.settle();
            if !h.st.results.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "重启后 30s 内新引擎未出结果(状态:{:?})",
                h.st.session.state
            );
        }
        assert!(!h.st.sort_stale, "新引擎回显相符:无警示");
        let sizes: Vec<u64> = h.st.results.iter().filter_map(|e| e.size).collect();
        assert!(
            sizes.windows(2).all(|w| w[0] <= w[1]),
            "重启后 size 升序应成立:{:?}",
            &sizes[..sizes.len().min(5)]
        );
        println!(
            "[实机] 重启后排序生效:size 升序前 5 = {:?}",
            &sizes[..sizes.len().min(5)]
        );
    }

    /// Task 19 走查②(缺口 + 离线文案):隔离根含不可读目录 → status.gaps 非空 →
    /// 窗口快照/徽标/面板与 CLI `status` 逐行同源;停 daemon → 离线文案(生成于…)。
    ///
    /// 跑法(隔离):
    /// ```text
    /// T=$(mktemp -d); mkdir -p $T/home $T/root/locked
    /// 造样本:seq 1 5 | xargs -I{} touch $T/root/sample-report-{}.pdf
    /// chmod 000 $T/root/locked
    /// HOME=$T/home MCE_ROOTS=$T/root <当前构建>/target/debug/macEverything start
    /// HOME=$T/home ./target/debug/deps/macEverything_app-<hash> --ignored \
    ///   live_gaps_badge_and_states --nocapture
    /// ```
    #[test]
    #[ignore = "实机走查:需隔离 HOME + MCE_ROOTS(根内预置不可读目录)"]
    fn live_gaps_badge_and_states() {
        let paths = live_paths();
        assert!(
            client::connect(&paths).is_some(),
            "daemon 须已按测试注释的跑法启动"
        );
        let mut h = LiveHarness::new(paths.clone());

        // 1) 查询一次(状态随查询并入 + 徽标同帧同步)
        h.st.set_query("report");
        h.settle();

        // 2) 缺口清单:窗口快照 vs CLI status 独立查询逐行对照(同一数据面)
        let ui_gaps: Vec<Gap> = h.st.gaps().to_vec();
        assert!(!ui_gaps.is_empty(), "隔离根须预置不可读目录(chmod 000)");
        let cli_gaps = match client::status(&paths).expect("CLI 侧 status 查询") {
            client::StatusOutcome::Reported(r) => r.gaps,
            client::StatusOutcome::NoIndex => panic!("隔离环境应有索引"),
        };
        assert_eq!(ui_gaps.len(), cli_gaps.len(), "徽标计数 = CLI status 条数");
        for (ui_row, cli_row) in ui_gaps.iter().zip(&cli_gaps) {
            assert_eq!(ui_row.path, cli_row.path, "缺口目录逐行同源");
            assert_eq!(ui_row.reason, cli_row.reason, "缺口原因逐行同源");
        }
        println!(
            "[实机] 缺口 {} 个(与 CLI status 逐行一致):{:?}",
            ui_gaps.len(),
            ui_gaps
                .iter()
                .map(|g| (g.path.as_str(), g.reason.as_str()))
                .collect::<Vec<_>>()
        );

        // 3) 徽标点击/托盘项 → 展开面板:出帧不 panic;面板行 = gaps_display 切片
        h.st.open_gaps();
        assert!(h.st.gaps_open);
        h.frame();
        let (rows, rest) = gaps_display(h.st.gaps());
        assert_eq!(rows.len(), ui_gaps.len().min(20), "面板行数");
        assert_eq!(rows.len() + rest, ui_gaps.len(), "余量计数守恒");
        println!("[实机] 缺口面板展开:{} 行 + 余 {rest}", rows.len());

        // 4) 离线:停 daemon → 重查(直读落盘索引)→ 离线文案带生成时刻(§7)
        let n = client::stop(&paths).expect("stop daemon");
        println!("[实机] daemon 已停(最终 {n} 条),进入离线直读");
        h.st.set_query("report");
        h.settle();
        assert!(
            h.st.total > 0,
            "离线直读落盘索引应有命中(total={})",
            h.st.total
        );
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let note = index_note(h.st.engine.as_ref(), &h.st.session.state, h.st.engine_known)
                .expect("状态栏索引状态段");
            assert!(note.starts_with("离线索引"), "离线文案:{note}");
            assert!(note.contains("生成于"), "§7 离线列带生成时刻:{note}");
            println!("[实机] 状态栏索引段:{note}");
        });
    }

    /// 真机全链路:真实引擎查高频词 → 表格落数(draw 全渲染无 panic)→
    /// 列头排序(升/降/换列)→ 多选(单选/shift/⌘A)。
    #[test]
    #[ignore = "实机走查:需本机 macEverything 索引(隔离环境不跑)"]
    fn live_results_window_walkthrough() {
        use std::time::Duration;

        let home = std::env::var_os("HOME").expect("HOME 未设置");
        let paths = DaemonPaths {
            data_dir: std::path::PathBuf::from(home)
                .join("Library/Application Support/macEverything"),
        };
        let ctx = egui::Context::default();
        let mut st = ResultsState::new(ctx.clone(), paths);
        let mut icons = IconCache::without_worker();

        // 一帧 = logic(tick) + ui(draw);raw 输入给足画布(模拟 1200×800 窗口)。
        let mut frame = |st: &mut ResultsState| {
            st.tick(Instant::now());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None, &mut icons));
            // 无真实后端消费纹理增量;显式清空(epaint 要求:不处理须 clear)
            out.textures_delta.clear();
        };

        // 防抖到期(60ms)+ 查询线程回投:排帧至在途结束。
        // 返回是否观测到「排序中…」段(sorting_inflight=true 至少一帧——
        // 发起帧即置位,响应线程不可能先于该帧回投,故正常路径恒 true)。
        let mut settle = |st: &mut ResultsState| -> bool {
            std::thread::sleep(Duration::from_millis(90));
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut saw_inflight = false;
            loop {
                frame(st);
                saw_inflight |= st.sorting_inflight;
                if !st.sorting_inflight {
                    return saw_inflight;
                }
                assert!(Instant::now() < deadline, "查询 20s 未回");
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        // 1) 输入即搜:高频词 .pdf(实机百万级索引 → 数千命中)
        st.set_query(".pdf");
        assert!(
            settle(&mut st),
            "在途期状态栏应出现「排序中…」(sorting_inflight)"
        );
        assert!(st.total > 0, "「.pdf」应有命中(total={})", st.total);
        assert!(!st.results.is_empty(), "结果表应有行");
        // 名称含词(v1 匹配语义:文件名,大小写不敏感)
        let bad = st
            .results
            .iter()
            .find(|e| !e.name.to_lowercase().contains(".pdf"));
        assert!(bad.is_none(), "命中行名称应含 .pdf: {bad:?}");
        // 大小/修改时间随响应携带(daemon 并行读取):抽验非空
        let with_meta = st
            .results
            .iter()
            .filter(|e| e.size.is_some() && e.mtime.is_some())
            .count();
        assert!(with_meta > 0, "size/mtime 应随响应到达");
        println!(
            "[实机] .pdf:total={} 返回 K={} 匹配 {:.1}ms 读取 {:.1}ms",
            st.total,
            st.results.len(),
            st.elapsed_ms,
            st.stat_ms
        );

        // 2) 列头排序:大小(自然升)→ 再点(降)→ 换列 mtime(自然降)
        let sizes_of =
            |st: &ResultsState| -> Vec<u64> { st.results.iter().filter_map(|e| e.size).collect() };
        st.on_header_click(SortKey::Size);
        assert!(settle(&mut st), "排序重查在途期应出现「排序中…」");
        assert!(st.sort_asc, "首次点「大小」= 升序(小在前)");
        let asc = sizes_of(&st);
        assert!(
            asc.windows(2).all(|w| w[0] <= w[1]),
            "大小升序在前 5 项不成立:{:?}",
            &asc[..asc.len().min(5)]
        );
        assert!(
            !st.sort_stale,
            "当前构建 daemon 回显相符:不得误报「排序未生效」"
        );
        println!("[实机] 大小升序前 5:{:?}", &asc[..asc.len().min(5)]);

        st.on_header_click(SortKey::Size);
        settle(&mut st);
        assert!(!st.sort_asc, "再点「大小」= 降序");
        let desc = sizes_of(&st);
        assert!(
            desc.windows(2).all(|w| w[0] >= w[1]),
            "大小降序(反向)不成立"
        );
        println!("[实机] 大小降序前 5:{:?}", &desc[..desc.len().min(5)]);

        st.on_header_click(SortKey::Mtime);
        settle(&mut st);
        assert_eq!(st.sort, SortKey::Mtime);
        assert!(!st.sort_asc, "换列 mtime = 该列默认真方向(降,最新在前)");
        let mtimes: Vec<u64> = st.results.iter().filter_map(|e| e.mtime).collect();
        assert!(mtimes.windows(2).all(|w| w[0] >= w[1]), "mtime 降序不成立");
        println!("[实机] mtime 降序前 5:{:?}", &mtimes[..mtimes.len().min(5)]);

        // 3) 多选:单选 → shift 范围 → ⌘ 点选 → ⌘A 全选
        apply_click(&mut st.selection, &mut st.anchor, 3, false, false);
        assert_eq!(st.selection, std::collections::BTreeSet::from([3]));
        apply_click(&mut st.selection, &mut st.anchor, 10, true, false);
        assert_eq!(st.selection.len(), 8, "shift 范围 3..=10");
        apply_click(&mut st.selection, &mut st.anchor, 5, false, true);
        assert_eq!(st.selection.len(), 7, "⌘ 点选移出行 5");
        st.select_all();
        assert_eq!(st.selection.len(), st.results.len(), "⌘A 全选 K 行");
        println!("[实机] 多选:⌘A 全选 {} 行", st.selection.len());

        // 4) 大表全渲染一帧(虚拟化路径无 panic);结果集足够大才算「大结果集」
        frame(&mut st);
        assert!(
            st.results.len() >= 100 || st.total < 100,
            "实机 .pdf 应为大结果集(返回 K={} total={})",
            st.results.len(),
            st.total
        );

        // 5) 虚拟化代理指标:1000 行 vs 40 行(都超出可见窗口 ~26 行)。
        // 虚拟化(core::TableBody::rows 只画可见行)两者每帧绘制量相同、耗时应接近;
        // 非虚拟化实现会随总行数线性放大(此处应 ~25 倍)。debug 构建,数值偏悲观,
        // 只打印不断言(避免机器负载抖动造成假红)。
        let mut bench = |st: &mut ResultsState| -> f64 {
            let t = Instant::now();
            for _ in 0..20 {
                frame(st);
            }
            t.elapsed().as_secs_f64() * 1000.0 / 20.0
        };
        let big_rows = st.results.len();
        let big_ms = bench(&mut st);
        st.results.truncate(40); // 同“可见行数”对照:总行数 40 仍超出窗口
        let small_ms = bench(&mut st);
        println!(
            "[实机] 单帧绘制:{big_rows} 行 {big_ms:.2}ms / 40 行 {small_ms:.2}ms(可见行数同,比值 {:.2})",
            big_ms / small_ms.max(1e-6)
        );
    }
}

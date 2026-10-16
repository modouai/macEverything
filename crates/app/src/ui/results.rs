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
//! 动作执行(`ui/menu_actions.rs`);Task 19(缺口面板/sort_echo/状态栏补全)
//! 在本文件基础上加。

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use std::collections::BTreeSet;
use std::sync::mpsc;
use std::time::Instant;

use crate::platform;
use crate::session::{Session, SortKey, State};
use crate::ui::menu_actions;
use crate::ui::palette::{self, EngineStatus, Payload, QueryResult};
use client::{DaemonPaths, StatusOutcome};
use protocol::HitEntry;

/// 查询上限(与悬浮条同口径:协议上限一次取满;product §2「展示前 K 条」的 K)。
const QUERY_LIMIT: u32 = client::RESULT_CAP;
/// 数据行高(px;行间 item_spacing 置零,选中/斑马底连成整块)。
const ROW_H: f32 = 22.0;
/// 表头行高(px)。
const HEADER_H: f32 = 22.0;
/// 搜索行高(px;与悬浮条输入行一致)。
const INPUT_H: f32 = 26.0;

/// 表格列:(排序键, 标题, 初始宽, 最小宽);末列占余宽(不可拖)。
/// 顺序与 product §2 一致:名称 / 大小 / 修改时间 / 路径。
const COLUMNS: [(SortKey, &str, f32, f32); 4] = [
    (SortKey::Name, "名称", 260.0, 100.0),
    (SortKey::Size, "大小", 90.0, 60.0),
    (SortKey::Mtime, "修改时间", 150.0, 110.0),
    (SortKey::Path, "路径", 320.0, 120.0),
];

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
    /// 选中行号集合(protocol 条目无 id,行号即选择;结果整体替换时清空)。
    pub selection: BTreeSet<usize>,
    /// shift 范围选锚点(范围选不移动锚点,概念如 Finder)。
    pub anchor: Option<usize>,
    /// 排序/查询在途(状态栏「排序中…」;fire 置位,当代响应到达清除)。
    pub sorting_inflight: bool,
    pub input: String,
    /// 瞬时提示(打开失败/查询失败;显示于状态栏)。
    pub status_note: Option<String>,
    /// 缺口徽标:未收录目录数(0 = 不显示;点击展开面板 Task 19 接)。
    pub gaps_badge: Option<usize>,
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
    paths: DaemonPaths,
    ctx: egui::Context,
    tx: mpsc::Sender<QueryResult>,
    rx: mpsc::Receiver<QueryResult>,
    /// 状态补查通道(独立小通道;无代际语义,与查询结果互不干扰)。
    status_tx: mpsc::Sender<Option<EngineStatus>>,
    status_rx: mpsc::Receiver<Option<EngineStatus>>,
}

impl ResultsState {
    /// `ctx`:查询线程回投后定向唤起 ROOT;实机走查测试用独立 `egui::Context` 构造
    /// (本文件测试 `live_results_window_walkthrough`)。
    pub fn new(ctx: egui::Context, paths: DaemonPaths) -> Self {
        let (tx, rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        Self {
            session: Session::new(palette::DEBOUNCE),
            results: Vec::new(),
            total: 0,
            elapsed_ms: 0.0,
            stat_ms: 0.0,
            sort: SortKey::Relevance,
            sort_asc: true,
            selection: BTreeSet::new(),
            anchor: None,
            sorting_inflight: false,
            input: String::new(),
            status_note: None,
            gaps_badge: None,
            engine: None,
            engine_known: false,
            // 启动即显示窗口(dev 形态):首帧补查状态 + 聚焦搜索框。
            status_refresh_pending: true,
            focus_input_pending: true,
            ime_composing: false,
            input_focused: false,
            paths,
            ctx,
            tx,
            rx,
            status_tx,
            status_rx,
        }
    }

    /// 窗口被唤起(托盘「打开完整窗口」/ 悬浮条 ⌥↵):聚焦搜索框 + 补查引擎状态
    /// (product §2:状态栏常驻显示索引状态)。
    pub fn on_window_shown(&mut self) {
        self.focus_input_pending = true;
        self.status_refresh_pending = true;
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
        self.sorting_inflight = false;
        if !q.is_empty() {
            // 防抖到期帧(调用点在悬浮条 pass,须定向 ROOT)
            self.ctx
                .request_repaint_after_for(palette::DEBOUNCE, egui::ViewportId::ROOT);
        }
    }

    /// 每帧驱动(App::logic 调用;根窗隐藏期也经 run_logic 收取响应,状态不落后):
    /// 补查状态 → 防抖到期发查询 → 收两路回投。
    pub fn tick(&mut self, now: Instant) {
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
        // 缺口徽标跟随引擎快照:查询回投与状态补查两路都在此同步
        // (只开窗不查询时,补查也要能亮徽标)。
        self.gaps_badge = self.engine.as_ref().map(|e| e.gaps as usize);
    }

    /// 一次性引擎状态补查(独立小通道;失败留痕并置「未知」——不静默)。
    fn spawn_status(&mut self) {
        let paths = self.paths.clone();
        let ctx = self.ctx.clone();
        let tx = self.status_tx.clone();
        std::thread::spawn(move || {
            let engine = match client::status(&paths) {
                Ok(StatusOutcome::Reported(r)) => Some(EngineStatus {
                    state: r.state,
                    entry_count: r.entry_count,
                    gaps: r.gaps.len() as u32,
                }),
                Ok(StatusOutcome::NoIndex) => None,
                Err(err) => {
                    log::warn!("完整窗口状态补查失败:{err}");
                    None
                }
            };
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
            } => {
                self.results = entries;
                self.total = total;
                self.elapsed_ms = elapsed_ms;
                self.stat_ms = stat_ms;
                self.selection.clear();
                self.anchor = None;
                self.status_note = None;
                if offline {
                    self.session.mark_offline();
                } else {
                    self.session.mark_total(total);
                }
            }
            Payload::Failed { engine_down } => {
                self.results.clear();
                self.total = 0;
                self.selection.clear();
                self.anchor = None;
                if engine_down {
                    self.session.mark_engine_starting();
                    self.status_note = Some("引擎未就绪".into());
                } else {
                    self.session.mark_error("query");
                    self.status_note = Some("查询失败".into());
                }
            }
        }
        if let Some(engine) = res.engine {
            self.engine = Some(engine);
        }
        self.engine_known = true;
    }

    /// 绘制完整窗口(主视口)。`frame` 供原生右键菜单取窗(调用方 = App::ui,根视口)。
    /// 返回动作由上层不需要——打开/失败都就地落在状态栏。
    pub fn draw(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        self.draw_impl(ui, Some(frame));
    }

    /// 绘制主体(`frame` = None 时禁用原生右键菜单:实机走查测试无 eframe Frame)。
    fn draw_impl(&mut self, ui: &mut egui::Ui, frame: Option<&eframe::Frame>) {
        let ctx = ui.ctx().clone();
        self.track_ime(ui);

        // ⌘A:输入框未聚焦时 = 表格全选行(聚焦时归输入框「全选文本」,平台惯例)。
        if !self.input_focused
            && !self.ime_composing
            && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
        {
            self.select_all();
        }

        let mut sort_click: Option<SortKey> = None;
        let mut row_click: Option<(usize, bool, bool)> = None;
        let mut open_row: Option<usize> = None;
        let mut context_click: Option<(usize, egui::Pos2)> = None;
        let mut gaps_clicked = false;
        let (shift_down, cmd_down) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));

        // 行间无缝(item_spacing.y = 0):选中/斑马底连成整块(表格式观感)。
        ui.spacing_mut().item_spacing.y = 0.0;

        // —— 顶部:搜索框 ——
        let input_resp = egui::Panel::top("results-search")
            .show(ui, |ui| {
                ui.add_space(6.0);
                let resp = ui
                    .horizontal(|ui| {
                        ui.add_sized(
                            [ui.available_width(), INPUT_H],
                            egui::TextEdit::singleline(&mut self.input)
                                .id_salt("results-input")
                                .hint_text("搜索文件…"),
                        )
                    })
                    .inner;
                ui.add_space(6.0);
                resp
            })
            .inner;

        // —— 底部:状态栏骨架(N 项 · 匹配/读取 ms · 索引状态 | 排序中 | 缺口徽标 | 提示)——
        egui::Panel::bottom("results-status").show(ui, |ui| {
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(
                    egui::RichText::new(counts_text(
                        self.total,
                        self.results.len(),
                        self.elapsed_ms,
                        self.stat_ms,
                    ))
                    .small()
                    .weak(),
                );
                ui.separator();
                if let Some(note) = index_note(self.engine.as_ref(), self.engine_known) {
                    ui.label(egui::RichText::new(note).small().weak());
                }
                if self.sorting_inflight {
                    ui.label(egui::RichText::new("排序中…").small().weak());
                }
                let gaps = self.gaps_badge.unwrap_or(0);
                if gaps > 0 {
                    let warn = ui.visuals().warn_fg_color;
                    let badge = egui::Label::new(
                        egui::RichText::new(format!("⚠ {gaps} 个目录未收录"))
                            .small()
                            .color(warn),
                    )
                    .sense(egui::Sense::click());
                    if ui.add(badge).clicked() {
                        gaps_clicked = true;
                    }
                }
                if let Some(note) = &self.status_note {
                    let warn = ui.visuals().warn_fg_color;
                    ui.label(egui::RichText::new(note).small().color(warn));
                }
            });
            ui.add_space(3.0);
        });

        // —— 中部:结果表格(虚拟化:只渲染可见行)——
        egui::CentralPanel::default().show(ui, |ui| {
            if self.results.is_empty() {
                ui.add_space(12.0);
                if let Some(msg) = empty_hint(&self.session.state) {
                    ui.label(egui::RichText::new(msg).weak());
                }
                return;
            }
            let cur_sort = self.sort;
            let cur_asc = self.sort_asc;
            let results: &[HitEntry] = &self.results;
            let selection: &BTreeSet<usize> = &self.selection;

            let mut table = TableBuilder::new(ui)
                .id_salt("results-table")
                .sense(egui::Sense::click())
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for (i, (_, _, width, min_width)) in COLUMNS.iter().enumerate() {
                let column = if i == COLUMNS.len() - 1 {
                    // 末列占余宽(TableBuilder 语义:remainder 末列不可拖)
                    Column::remainder().at_least(*min_width).clip(true)
                } else {
                    Column::initial(*width)
                        .at_least(*min_width)
                        .clip(true)
                        .resizable(true)
                };
                table = table.column(column);
            }
            table
                .header(HEADER_H, |mut header| {
                    for (key, label, _, _) in COLUMNS {
                        header.col(|ui| {
                            let arrow = if cur_sort == key {
                                if cur_asc {
                                    " ▲"
                                } else {
                                    " ▼"
                                }
                            } else {
                                ""
                            };
                            let text = egui::RichText::new(format!("{label}{arrow}")).strong();
                            let resp = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
                            if resp.clicked() {
                                sort_click = Some(key);
                            }
                        });
                    }
                })
                .body(|body| {
                    body.rows(ROW_H, results.len(), |mut row| {
                        let i = row.index();
                        row.set_selected(selection.contains(&i));
                        let e = &results[i];
                        row.col(|ui| {
                            ui.add(egui::Label::new(&e.name).truncate());
                        });
                        row.col(|ui| {
                            // 数字右对齐(表格式读数习惯)
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(size_text(e.size));
                                },
                            );
                        });
                        row.col(|ui| {
                            ui.label(mtime_text(e.mtime));
                        });
                        row.col(|ui| {
                            let weak = egui::RichText::new(&e.path).weak();
                            ui.add(egui::Label::new(weak).truncate());
                        });
                        let resp = row.response();
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
                    });
                });
        });

        // —— 交互落地(表格闭包外:借用结束)——
        if let Some(key) = sort_click {
            self.on_header_click(key);
        }
        if let Some((i, shift, cmd)) = row_click {
            apply_click(&mut self.selection, &mut self.anchor, i, shift, cmd);
        }
        if let Some(i) = open_row {
            self.open_row(i);
        }
        if let Some((i, pos)) = context_click {
            self.open_context_menu(i, pos, frame);
        }
        if gaps_clicked {
            log::info!("缺口徽标点击:展开缺口清单(Task 19 接面板)");
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
            } else {
                ctx.request_repaint_after(palette::DEBOUNCE);
            }
        }
        if self.focus_input_pending {
            input_resp.request_focus();
            self.focus_input_pending = false;
        }
        self.input_focused = input_resp.has_focus();
    }

    /// 列头点击:同列翻转方向;异列切键并按该键默认方向;两者都触发同串重查
    /// (on_sort_change 在防抖窗内合并;响应按新方向由引擎落序)。
    fn on_header_click(&mut self, key: SortKey) {
        if self.sort == key {
            self.sort_asc = !self.sort_asc;
        } else {
            self.sort = key;
            self.sort_asc = natural_asc(key);
        }
        log::info!("列头点击:{key:?}(asc={})", self.sort_asc);
        self.session
            .on_sort_change(key, request_desc(key, self.sort_asc), Instant::now());
        self.ctx
            .request_repaint_after_for(palette::DEBOUNCE, egui::ViewportId::ROOT);
    }

    /// 右键菜单(Task 18):标准表格语义先把点击行并入选中(不在选中集 → 单选它),
    /// 按选中类型收敛菜单项(product §2 表),弹原生菜单并执行所选动作。
    /// 弹出阻塞(嵌套 run loop,期间本帧冻结——PoC-3 定论);执行失败落状态栏。
    fn open_context_menu(&mut self, row: usize, pos: egui::Pos2, frame: Option<&eframe::Frame>) {
        // 1. 选中收敛(标准表格语义)
        converge_selection(&mut self.selection, &mut self.anchor, row);
        // 2. 选中行 → 路径集合 + 文件/目录计数 → 菜单项(按选中类型收敛)
        let paths: Vec<String> = self
            .selection
            .iter()
            .filter_map(|&i| self.results.get(i))
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() {
            return; // 理论不可达(selection 行号只在 results 内维护,两者同步清空)
        }
        let (files, dirs) = menu_actions::count_types(&paths);
        let items = menu_actions::menu_items(files, dirs);
        if items.is_empty() {
            return; // 空选(理论不可达,见上)——不弹空菜单
        }
        let menu: Vec<(&str, i64)> = items.iter().map(|(title, a)| (*title, a.tag())).collect();
        log::info!(
            "右键菜单:选中 {} 行(文件 {files} · 目录 {dirs}),弹出于 ({:.0},{:.0})",
            paths.len(),
            pos.x,
            pos.y
        );
        // 3. 弹出原生菜单(阻塞;菜单关闭后返回所选 tag)
        let Some(frame) = frame else {
            log::warn!("右键菜单:无 eframe Frame(测试路径),不弹出");
            return;
        };
        let choice = platform::popup_menu(&menu, (pos.x as f64, pos.y as f64), frame);
        // 4. 执行(未知 tag 留痕;失败落状态栏,不静默)
        match choice {
            None => log::info!("右键菜单:取消(未选择)"),
            Some(tag) => match menu_actions::MenuAction::from_tag(tag) {
                Some(action) => match menu_actions::execute(action, &paths) {
                    Ok(()) => log::info!("右键菜单:执行 {action:?}({} 项)", paths.len()),
                    Err(err) => self.status_note = Some(err.to_string()),
                },
                None => log::warn!("右键菜单:未知 tag {tag},忽略"),
            },
        }
        // 阻塞期本帧未上屏:重绘刷出选中态 / 状态栏提示
        self.ctx.request_repaint();
    }

    /// 双击:默认应用打开(palette::open_path 复用;失败落状态栏,不静默)。
    fn open_row(&mut self, i: usize) {
        let Some(entry) = self.results.get(i) else {
            return;
        };
        if let Err(msg) = palette::open_path(&entry.path, false) {
            self.status_note = Some(msg);
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

/// 状态栏计数段:N 项 = 命中总数(total);返回条数 < total 时注「展示前 K 条」。
fn counts_text(total: u32, shown: usize, elapsed_ms: f64, stat_ms: f64) -> String {
    let mut s = format!("{total} 项 · 匹配 {elapsed_ms:.1} ms · 读取 {stat_ms:.1} ms");
    if (shown as u32) < total {
        s.push_str(&format!(" · 展示前 {shown} 条"));
    }
    s
}

/// 索引状态段(product §7 完整窗口列;skeleton 版——缺口面板/sort_echo 提示 Task 19 细化)。
/// 返回 None = 尚未取到(窗口刚开、状态补查未回),不占位。
fn index_note(engine: Option<&EngineStatus>, known: bool) -> Option<String> {
    match engine {
        Some(e) => Some(match e.state.as_str() {
            client::state::BUILDING => format!("索引构建中({} 条)", e.entry_count),
            client::state::READY => "就绪".into(),
            client::state::RECONCILING => "索引补齐中(结果可能不全)".into(),
            client::state::DEGRADED => "索引重扫中(结果可能不全)".into(),
            client::state::OFFLINE => "离线索引".into(),
            other => other.to_string(),
        }),
        None if known => Some("引擎未运行".into()),
        None => None,
    }
}

/// 空结果区提示(按会话态;Error 由状态栏 status_note 承担,不再重复)。
fn empty_hint(state: &State) -> Option<&'static str> {
    match state {
        State::Idle => Some("输入关键词开始搜索"),
        State::Searching => Some("搜索中…"),
        State::Empty => Some("无匹配项"),
        State::EngineStarting => Some("引擎未就绪…"),
        State::Warming => Some("索引构建中…"),
        State::Offline => Some("无匹配项(离线索引)"),
        State::Error(_) => None,
    }
}

/// 大小显示(与 cli 的 human_size 同边界;自写简版,不引依赖)。
fn size_text(size: Option<u64>) -> String {
    match size {
        Some(bytes) => human_size(bytes),
        None => "—".into(),
    }
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// 修改时间显示:unix 秒 → "YYYY-MM-DD HH:MM"(**本地时区**,localtime_r;
/// Finder 同观感——cli 的 fmt_time 是 UTC 且带秒,窗口口径更贴近文件管理器)。
fn mtime_text(mtime: Option<u64>) -> String {
    match mtime {
        Some(secs) => fmt_mtime(secs),
        None => "—".into(),
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

    #[test]
    fn human_size_bounds() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn size_and_mtime_none_show_placeholder() {
        assert_eq!(size_text(None), "—", "读取失败不留空");
        assert_eq!(mtime_text(None), "—");
        assert_eq!(mtime_text(Some(0)), "—", "0 = 未取(与 cli fmt_time 同约定)");
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
        assert_eq!(
            counts_text(42, 42, 3.2, 1.1),
            "42 项 · 匹配 3.2 ms · 读取 1.1 ms",
            "未截断不加注"
        );
        assert_eq!(
            counts_text(8000, 1000, 12.5, 40.0),
            "8000 项 · 匹配 12.5 ms · 读取 40.0 ms · 展示前 1000 条",
            "K < total 注展示条数"
        );
    }

    #[test]
    fn index_note_six_states() {
        let eng = |s: &str| EngineStatus {
            state: s.into(),
            entry_count: 12,
            gaps: 0,
        };
        assert_eq!(
            index_note(Some(&eng(client::state::BUILDING)), true).as_deref(),
            Some("索引构建中(12 条)")
        );
        assert_eq!(
            index_note(Some(&eng(client::state::READY)), true).as_deref(),
            Some("就绪")
        );
        assert_eq!(
            index_note(Some(&eng(client::state::RECONCILING)), true).as_deref(),
            Some("索引补齐中(结果可能不全)")
        );
        assert_eq!(
            index_note(Some(&eng(client::state::DEGRADED)), true).as_deref(),
            Some("索引重扫中(结果可能不全)")
        );
        assert_eq!(
            index_note(Some(&eng(client::state::OFFLINE)), true).as_deref(),
            Some("离线索引")
        );
        assert_eq!(
            index_note(None, true).as_deref(),
            Some("引擎未运行"),
            "已查过且无状态 = 未运行/无索引"
        );
        assert_eq!(index_note(None, false), None, "未查过不占位");
    }

    #[test]
    fn empty_hint_covers_states() {
        assert_eq!(empty_hint(&State::Idle), Some("输入关键词开始搜索"));
        assert_eq!(empty_hint(&State::Empty), Some("无匹配项"));
        assert_eq!(empty_hint(&State::EngineStarting), Some("引擎未就绪…"));
        assert_eq!(empty_hint(&State::Warming), Some("索引构建中…"));
        assert_eq!(empty_hint(&State::Offline), Some("无匹配项(离线索引)"));
        assert_eq!(empty_hint(&State::Error("x".into())), None, "错误走状态栏");
    }

    #[test]
    fn sort_column_defaults_plan_spec() {
        // plan Step 1:名称升/大小升/mtime 降/路径升——与引擎自然方向一致
        assert!(natural_asc(SortKey::Name));
        assert!(natural_asc(SortKey::Size));
        assert!(!natural_asc(SortKey::Mtime));
        assert!(natural_asc(SortKey::Path));
        // 列定义与 product §2 列序一致
        let keys: Vec<SortKey> = COLUMNS.iter().map(|c| c.0).collect();
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
    //    此因);换当前构建起的 daemon 再跑(Task 19 的 sort_echo 校验接上后
    //    「排序未生效」在界面上可见)。

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

        // 一帧 = logic(tick) + ui(draw);raw 输入给足画布(模拟 1200×800 窗口)。
        let frame = |st: &mut ResultsState| {
            st.tick(Instant::now());
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            };
            let mut out = ctx.run_ui(input, |ui| st.draw_impl(ui, None));
            // 无真实后端消费纹理增量;显式清空(epaint 要求:不处理须 clear)
            out.textures_delta.clear();
        };

        // 防抖到期(60ms)+ 查询线程回投:排帧至在途结束。
        let settle = |st: &mut ResultsState| {
            std::thread::sleep(Duration::from_millis(90));
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                frame(st);
                if !st.sorting_inflight {
                    return;
                }
                assert!(Instant::now() < deadline, "查询 20s 未回");
                std::thread::sleep(Duration::from_millis(20));
            }
        };

        // 1) 输入即搜:高频词 .pdf(实机百万级索引 → 数千命中)
        st.set_query(".pdf");
        settle(&mut st);
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
        settle(&mut st);
        assert!(st.sort_asc, "首次点「大小」= 升序(小在前)");
        let asc = sizes_of(&st);
        assert!(
            asc.windows(2).all(|w| w[0] <= w[1]),
            "大小升序在前 5 项不成立:{:?}",
            &asc[..asc.len().min(5)]
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
        let bench = |st: &mut ResultsState| -> f64 {
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

//! 悬浮条视图:输入即搜(防抖 60ms)+ 结果列表 + 状态角标 + 键位(product §1)。
//!
//! 查询执行模型(定案):UI 线程不做阻塞查询(socket 往返 + 按需 stat 会掉帧)。
//! 会话防抖到期 → 工作线程执行 `client::query` → mpsc 回投 `QueryResult` + 唤醒
//! UI;UI 每帧 try_recv,按代际号 `accept_response` 丢弃旧代(Task 11 语义)。
//! 角标状态取自工作线程顺带执行的 `client::status`(product §7 映射)。
//!
//! ## 面板高度(2026-10-17 定案:空态滞后 + 贴合内容 + 平滑动画)
//!
//! **空态滞后(主药)**:查询 0 命中且输入仍在进行(距末次输入 < `EMPTY_HOLD`)时
//! 不切「无匹配项」小态——打字/删除途中经过的 0 命中中间态(如「区域B换」)完全静默,
//! 保留上一轮结果与面板高度,停手后仍 0 命中才落定。用户实测「删最后一个字抖动
//! 严重」的直接来源即「0 命中塌 → 删字弹回」的逐键往返。
//!
//! 高度**贴合内容**(空态不留白;自然内容高 = 输入行 + 结果区 + 计数行),但窗口高
//! 不瞬跳——每帧向目标高做时间基指数逼近(`anim_step`;τ 主导缓动),动画期逐帧
//! 排帧直到收敛吸附。结果行数差 ≤ `DEAD_ROWS`(±2 行)的高频小波动**不动面板**
//! (死区);超出即**实时平滑跟随**(收缩不推迟——「打字期只长不缩 + 停手收敛」
//! 实机反馈「输入完成后整框抖一下」,2026-10-17 第四版拆除)。
//!
//! 面板背景**自绘**(占位 → 按可见高填实):egui `Frame` 的背景恒等于内容高,
//! 动画期「窗高 / 内容高」不一致,直接用 Frame 会出现两种伪影——背景画出窗外被裁
//! (底边直角、圆角丢失)或背景瞬贴内容(动画不可见)。
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
/// 高度动画:指数逼近时间常数(s)。每帧位移 = 差值 × (1 − e^(−dt/τ))——**缓动形状
/// 由 τ 主导**(2026-10-17 第四版:30ms 太快,实机观感「啪」地抖一下,放缓到 70ms)。
const H_EASE_TAU: f32 = 0.070;
/// 高度动画:速度上限(px/s;60fps 下 ≈15px/帧)。只在大差值前段生效(避免开帧
/// 大跨步),不构成「匀速冲」的形状;τ 主导缓动。原 30px/帧硬上限已删
/// (恒定步长就是「匀速冲」感的来源)。
const H_MAX_SPEED: f32 = 900.0;
/// 高度死区(±行):结果行数与上一承诺行数差 ≤ 此值 → 面板不动(8→7→6 这类高频
/// 小波动全静默,小差异以结果区留白吸收);超出即实时平滑跟随(收缩不推迟)。
const DEAD_ROWS: usize = 2;
/// 高度动画:收敛阈值(px)。差值小于它直接吸附目标并停排帧(吸附后精确相等;
/// 3px 已是目视不可辨的残差,顺带砍掉指数尾巴的最后 ~5 帧)。
const H_ANIM_EPS: f32 = 3.0;
/// 空态滞后窗口(主药;2026-10-17):响应 0 命中且距末次输入小于此值 → 输入仍在
/// 进行,不切「无匹配项」小态(保留上一轮结果与面板高度);停手后仍 0 命中才切。
/// 取值须大于连打/删除的键间隔(实测 ~120-150ms/键)并覆盖查询往返(~10ms)。
const EMPTY_HOLD: Duration = Duration::from_millis(400);
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
/// 2026-10-16 辉哥实机反馈「窗口偏小」:600 → 720。
pub const PANEL_W: f32 = 720.0;
/// 初始/复位窗口高(空态:仅输入行;也是面板高动画的起点)。
/// = 2×`PANEL_PAD_Y`(10) + `INPUT_H`(32) + 描边 2(描边画在面板矩形内侧,
/// 与自然高算式同一个 2,见 `draw` 自绘背景)= 54。2026-10-16:44 → 54(上下留白)。
pub const INITIAL_H: f32 = 54.0;

// ---------------------------------------------------------------------------
// 字号档(悬浮条用;全局基准 +2 号在 ui/mod.rs::install_base_text_styles)
//
// 单点定义:未来「自定义字号」设置项(v2.1 候选)改这几处常量即可。输入框与
// 结果行用比全局 Body(15)更大的放大档(辉哥 2026-10-16 定:输入框 17、
// 结果行 16、目录灰字 13、计数/角标 12)。
/// 输入框字号。
pub const FONT_INPUT: f32 = 17.0;
/// 结果行文件名。
pub const FONT_ROW: f32 = 16.0;
/// 结果行目录灰字。
pub const FONT_DIR: f32 = 13.0;
/// 计数行 / 状态角标 / 空态。
pub const FONT_META: f32 = 12.0;

/// 结果行/输入框字号(proportional;供 RichText/TextEdit 直接引用)。
fn font(size: f32) -> egui::FontId {
    egui::FontId::proportional(size)
}

/// 首查按协议上限请求一次(product §1 分页语义:不重查、不翻页请求)。
const QUERY_LIMIT: u32 = client::RESULT_CAP;
/// 列表初始渲染行数与滚到底增量(product §1:初始 20 行,滚到底 +20)。
const ROWS_INITIAL: usize = 20;
const ROWS_STEP: usize = 20;
/// 列表区最大高度(行数少时收缩到内容高;超出滚动)。
const LIST_MAX_H: f32 = 322.0;
/// 排版区放开上限(px):面板内容最大高 ≈ 内外边距 + 输入行 + 间距 + `LIST_MAX_H`
/// + 计数行 ≈ 410,取富余值。仅用于布局区(与窗高解耦),不作窗口高输出。
const PANEL_MAX_H: f32 = 4096.0;
/// 面板四周内边距(左右, 上下);`INITIAL_H` = 2×上下内边距 + 输入行高。
const PANEL_PAD_X: i8 = 14;
const PANEL_PAD_Y: i8 = 10;
/// 输入行(输入框)高度:17pt 文字 + 上下留白(文本内垂直居中)。
const INPUT_H: f32 = 32.0;
/// 面板圆角。
const PANEL_RADIUS: u8 = 12;
/// 结果行圆角(选中高亮块的柔和圆角)与行内边距(左右, 上下)。
const ROW_RADIUS: u8 = 6;
const ROW_PAD_X: i8 = 8;
const ROW_PAD_Y: i8 = 3;

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
        /// 实际生效的排序键回显(relevance/name/size/mtime/path;旧 daemon = 空串)。
        /// 完整窗口比对 `requested_sort` 识别「排序未生效」,防静默错排序。
        sort_echo: String,
        /// 本次请求下发的排序键(None = relevance/未请求);随响应携带,与回显比对。
        requested_sort: Option<&'static str>,
    },
    /// 查询失败;engine_down = daemon 不可连(拉起中/未运行);
    /// code = 引擎响应错误码(None = 连接/离线路径失败)——「warming」= 索引未挂载,
    /// 会话态进 Warming(tech 六态表),否则进 Error。
    Failed {
        engine_down: bool,
        code: Option<String>,
    },
}

/// 引擎状态(status 子命令数据面;悬浮条角标与完整窗口状态栏共用)。
pub struct EngineStatus {
    pub state: String,
    pub entry_count: u32,
    /// 权限缺口清单(status.gaps;完整窗口徽标与面板用,悬浮条不显示)。
    pub gaps: Vec<protocol::Gap>,
    /// 落盘索引生成时刻(unix 秒;0 = 未取)——离线状态栏「生成于 …」用。
    pub built_at: u64,
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
    /// 已下发的窗口高度(变化才发 InnerSize,防每帧抖动;探针也读它)。
    last_h: Option<f32>,
    /// 面板高动画:当前输出高(窗高单位,含内边距与描边;每帧向 `dead_h` 逼近)。
    /// `reset` 复位到 `INITIAL_H`(唤起首帧不缩放)。
    anim_h: f32,
    /// 死区承诺高(动画目标,窗高单位):行数差 ≤ `DEAD_ROWS` 时面板保持此高。
    dead_h: f32,
    /// 死区锚行数(承诺高对应的结果行数;None = 非列表态:空态/输入行等)。
    dead_rows: Option<usize>,
    /// 上帧动画推进时刻(时间基插值;`reset` 置为当下,故首帧 dt≈0 不抢跳)。
    anim_at: Option<Instant>,
    /// 动画留痕(起点高, 峰值单帧位移, 帧数, 起算时刻):收敛时汇总一行 info,
    /// 作为实机「每帧位移 ≤ `H_MAX_SPEED`×帧长」的机器可读证据(逐帧另有 debug 留痕)。
    anim_trace: Option<(f32, f32, u32, Instant)>,
    /// 最近一次输入变更时刻(空态滞后判定 `EMPTY_HOLD` 起算;仅输入帧更新)。
    last_input_at: Option<Instant>,
    /// 空态待决:输入进行中的 0 命中已静默(仍显示上一轮结果);停手后复判,
    /// 仍 0 命中才切「无匹配项」小态(见 `apply`/`draw`)。
    pending_empty: bool,
    /// 本视口 pass 计数与上帧起始(首帧成本定位:与「距上帧」联立可见
    /// tessellate/paint 耗时——我们的回调返回后才出图)。
    pass_seq: u64,
    last_pass_start: Option<Instant>,
    /// 最近一次唤起时刻(`on_shown` 置;唤起后 1.5s 内逐帧留痕,之后停)。
    show_at: Option<Instant>,
    /// 在途查询(代际号, 发起时刻):响应落地时记往返。
    query_sent: Option<(u64, Instant)>,
    /// 空串清空待决:IME 过渡帧(Preedit("") 清组合)的空串不是用户清空,
    /// 推迟到非过渡帧复判——预编辑帧不得清结果区(2026-10-16 抖动根因)。
    pending_empty_clear: bool,
}

impl PaletteState {
    /// `egui_ctx` 直传(不取 `eframe::CreationContext`):本类型只用其 Context,
    /// 且 headless 测试要能直接构造(打字高度探针,见 tests::probe)。
    pub fn new(
        egui_ctx: egui::Context,
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
            ctx: egui_ctx,
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
            anim_h: INITIAL_H,
            dead_h: INITIAL_H,
            dead_rows: None,
            anim_at: None,
            anim_trace: None,
            last_input_at: None,
            pending_empty: false,
            pass_seq: 0,
            last_pass_start: None,
            show_at: None,
            query_sent: None,
            pending_empty_clear: false,
        }
    }

    /// 唤起打点(首帧计时起点;`ui::palette_show` 调用)。
    pub fn on_shown(&mut self, now: Instant) {
        self.show_at = Some(now);
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
        // 高度动画复位到初始高:唤起首帧不缩放(窗口上一次的高度即使残留,
        // 也由首帧的 InnerSize 命令拉平;拉平期窗外区域全透明,不可见)。
        self.anim_h = INITIAL_H;
        self.dead_h = INITIAL_H;
        self.dead_rows = None;
        self.anim_at = Some(Instant::now());
        self.anim_trace = None;
        self.last_input_at = None;
        self.pending_empty = false;
        self.pending_empty_clear = false;
    }

    /// 每帧驱动(悬浮条视口回调 `ui::draw_palette` 调用):防抖到期发查询;
    /// 收工作线程结果(根视口被完全遮挡、`logic` 不跑时仍由此驱动,Task 14)。
    pub fn tick(&mut self, now: Instant) {
        // 在途防抖逐帧补排(根因见 `Session::pending_delay`:同帧 0ms 立即重绘请求
        // 会吞掉 60ms 防抖请求;不补排则要等下一次偶然重绘才发查询,实测 ~500ms)。
        if let Some(delay) = self.session.pending_delay(now) {
            self.ctx.request_repaint_after_for(delay, self.viewport);
        }
        if let Some(fire) = self.session.pending_at(now) {
            self.spawn_query(fire);
        }
        while let Ok(res) = self.rx.try_recv() {
            self.apply(res, now);
        }
    }

    /// 发起查询:工作线程执行,结果经 mpsc 回投并唤醒悬浮条视口。
    fn spawn_query(&mut self, fire: Fire) {
        self.query_sent = Some((fire.generation, Instant::now()));
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
    ///
    /// `now` = 本帧时刻(空态滞后判定用;`tick` 传入,不另取 `Instant::now()`)。
    fn apply(&mut self, res: QueryResult, now: Instant) {
        if !self.session.accept_response(res.generation) {
            return; // Task 11 代际防护:慢响应后到不得覆盖新结果
        }
        if let Some((g, t0)) = self.query_sent.take() {
            if g == res.generation {
                log::info!(
                    "查询往返:gen={g} {:.1}ms(输入→结果落地)",
                    t0.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        match res.payload {
            Payload::Hits {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                offline,
                ..
            } => {
                // —— 空态滞后(主药;2026-10-17)——
                // 输入进行中(距末次输入 < EMPTY_HOLD)的 0 命中**不落地**:打字中间态
                // (如「区域B换」)的 0 命中只静默,保留上一轮非空结果与高度;停手后仍
                // 0 命中才切「无匹配项」(draw 内复判)。否则「打→0 命中塌→删字弹回→
                // 再打」逐键往返,即用户实测「删最后一个字抖动严重」的直接来源。
                let typing = self
                    .last_input_at
                    .is_some_and(|t| now.saturating_duration_since(t) < EMPTY_HOLD);
                if empty_hold(total, typing, !self.results.is_empty()) {
                    self.pending_empty = true;
                    self.status_note = badge_note(&self.session.state, res.engine.as_ref());
                    log::info!(
                        "0 命中滞后:输入进行中(距末次输入 {}ms < {}ms),保留上一轮 {} 项与面板高度",
                        self.last_input_at
                            .map(|t| now.saturating_duration_since(t).as_millis())
                            .unwrap_or_default(),
                        EMPTY_HOLD.as_millis(),
                        self.results.len()
                    );
                    return;
                }
                self.pending_empty = false;
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
            Payload::Failed { engine_down, code } => {
                self.pending_empty = false;
                self.results.clear();
                self.total = 0;
                if engine_down {
                    self.session.mark_engine_starting(); // daemon 不可连:引擎未就绪
                } else {
                    // 错误码分流:warming = 索引未挂载 → 占位态(六态表),其余 → Error
                    self.session.mark_error(code.as_deref().unwrap_or("query"));
                }
            }
        }
        self.status_note = badge_note(&self.session.state, res.engine.as_ref());
    }

    /// 绘制悬浮条;返回交上层的动作。
    pub fn draw(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let ctx = ui.ctx().clone();
        // 首程诊断打点(唤起后 1.5s 内逐帧留痕,之后停;常态零开销):
        // pass# / 自唤起 / 距上帧(上帧含 tessellate+paint)/ 本帧回调体耗时 /
        // 输入事件到达时刻 / 字体图集布局耗时 / 本帧排帧因(谁叫的帧)。
        // 缘起:2026-10-16「唤起后首次出结果慢」定位(根因 = 防抖排帧被吞,见
        // `Session::pending_delay`;此处打点是定位手段,留住供后续回归对照)。
        let pass_t0 = Instant::now();
        self.pass_seq += 1;
        let since_last_ms = self
            .last_pass_start
            .map(|l| pass_t0.duration_since(l).as_secs_f64() * 1000.0);
        self.last_pass_start = Some(pass_t0);
        let since_show = self.show_at.map(|s| pass_t0.duration_since(s));
        if since_show.is_some() {
            // 输入事件到达打点(字到帧的延迟定位)。
            let evs: Vec<String> = ui.input(|i| {
                i.events
                    .iter()
                    .map(|e| match e {
                        egui::Event::Text(t) => format!("Text({t:?})"),
                        egui::Event::Ime(ev) => format!("Ime({ev:?})"),
                        egui::Event::Key { key, .. } => format!("Key({key:?})"),
                        _ => String::new(),
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            });
            if !evs.is_empty() {
                log::info!(
                    "悬浮条输入事件到达:t={}ms 事件={evs:?} 输入={:?}",
                    platform::monotonic_ms(),
                    self.input
                );
            }
            // 字体图集打点:显式按输入框字号布局一次(此刻才真正构建字形图集,
            // 含 system_fonts 中文回退),把「字体加载」从整体耗时里单列。
            let ft = Instant::now();
            ui.fonts_mut(|f| {
                let _ = f.layout_no_wrap(
                    "搜索文件…".to_owned(),
                    font(FONT_INPUT),
                    egui::Color32::WHITE,
                );
            });
            let causes: Vec<String> = ui
                .ctx()
                .repaint_causes()
                .iter()
                .map(|c| format!("{}:{} {}", c.file, c.line, c.reason))
                .collect();
            log::info!(
                "悬浮条首帧计时:pass#{} 自唤起 {:.1}ms,距上帧 {},字体布局打点 {:.1}ms,排帧因={causes:?}",
                self.pass_seq,
                since_show.map(|d| d.as_secs_f64() * 1000.0).unwrap_or_default(),
                since_last_ms
                    .map(|v| format!("{v:.1}ms"))
                    .unwrap_or_else(|| "-".into()),
                ft.elapsed().as_secs_f64() * 1000.0
            );
        }
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

        let now = Instant::now();

        // —— 面板背景占位(自绘;填实见帧尾)——
        // 背景矩形 = 本帧**可见高**矩形,而非自然内容高矩形:egui `Frame` 的背景
        // 恒等于内容高,动画期窗/内容两高不一致会出现「底边直角裁切」或「背景瞬贴
        // 内容(动画不可见)」两种伪影(模块头注「面板高度」)。占位先落笔、内容后画,
        // 遮挡关系正确(egui `Frame` 内部同法:先 add(Shape::Noop),末尾 set)。
        let panel_origin = ui.min_rect().left_top();
        // 真实窗高(可见高/裁剪用;须在放开布局区之前读)。
        let win_h = ui.max_rect().height();
        // 布局区与窗高解耦(先读窗高再放开):内容自然高必须只由内容决定——根 ui
        // 的布局区默认为窗矩形,而窗高又由内容高驱动(动画输出),不放即形成
        // 「窗高 → 内容高 → 窗高」反馈环:结果区被窗高钳住,面板只能一帧长几 px
        // 慢慢「爬」向目标(前一版实机高度大幅摆动/慢爬的根因之一)。取富余上限
        // 让排版一次到位;可见性仍由下面的裁剪矩形(窗 ∩ 动画矩形)决定。
        ui.set_max_height(PANEL_MAX_H);
        // 可见高 = min(动画高, 真实窗高):窗尺寸命令下一帧才落地(扩张期窗还矮),
        // 不取小则背景伸到窗外被裁 → 底边直角(圆角丢失)。窗高取不到(创建首帧)时
        // 以动画高为准。
        let clip_h = if win_h > 1.0 {
            self.anim_h.min(win_h)
        } else {
            self.anim_h
        };
        ui.set_clip_rect(egui::Rect::from_min_size(
            panel_origin,
            egui::vec2(PANEL_W, clip_h),
        ));
        let bg_idx = ui.painter().add(egui::Shape::Noop);
        let panel_fill = ui.visuals().window_fill;
        let panel_stroke = ui.visuals().window_stroke;

        let note = self.status_note.clone();
        let hotkey_note = self.hotkey_note.clone();
        let input = &mut self.input;
        let mut at_bottom = false;
        // 布局壳(内边距/宽度);填色描边归自绘背景,故 Frame 本体不带背景。
        let frame = egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(PANEL_PAD_X, PANEL_PAD_Y))
            .show(ui, |ui| {
                // 内容宽 = 窗宽 - 左右边距 - 描边(内容所在位置与自绘背景一致:
                // 背景矩形 0..PANEL_W,描边画在其内侧 1px)。
                ui.set_width(PANEL_W - 2.0 * PANEL_PAD_X as f32 - 2.0);
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
                                ui.label(egui::RichText::new(note).size(FONT_META).weak());
                                ui.add_space(6.0);
                            }
                            let w = ui.available_width();
                            ui.add_sized(
                                [w, INPUT_H],
                                egui::TextEdit::singleline(input)
                                    .id_salt("palette-input")
                                    .font(font(FONT_INPUT))
                                    // 文字垂直居中(同结果窗口;egui 0.37 主题的
                                    // TextEdit align2 默认 LEFT_TOP,add_sized 拉高后贴顶
                                    // —— 2026-10-16 实测两窗同病,一并显式居中)。
                                    .align(egui::Align2::LEFT_CENTER)
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
                            .size(FONT_META)
                            .color(ui.visuals().warn_fg_color),
                    );
                }

                // —— 结果列表 / 空态 / 计数行 ——
                // 结果只在响应落地时整体替换(上一轮结果保留到新响应,减少高度动画
                // 的目标变化次数);行数变化不再直接缩放窗口——高度由帧尾动画过渡
                // (模块头注「面板高度」)。
                if self.results.is_empty() {
                    if matches!(self.session.state, State::Empty) {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("无匹配项").size(FONT_META).weak());
                    }
                } else {
                    ui.add_space(8.0);
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
                                    selection_fill(ui.visuals())
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                let row = egui::Frame::new()
                                    .fill(bg)
                                    .corner_radius(egui::CornerRadius::same(ROW_RADIUS))
                                    .inner_margin(egui::Margin::symmetric(ROW_PAD_X, ROW_PAD_Y))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 10.0;
                                            ui.label(
                                                egui::RichText::new(&e.name)
                                                    .strong()
                                                    .size(FONT_ROW),
                                            );
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(dir_display(&e.path))
                                                        .size(FONT_DIR)
                                                        .color(dir_text_color(ui.visuals())),
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
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "共 {} 项 · 匹配 {:.1} ms · 读取 {:.1} ms",
                            self.total, self.elapsed_ms, self.stat_ms
                        ))
                        .size(FONT_META)
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

        // —— 高度动画:目标 = 自然高经「±2 行死区」过滤(贴合内容 + 高频噪点静默)——
        // 自然高 = 内容高 + 上下内边距 + 描边 2(描边画在面板矩形内侧,与窗口几何
        // 一致:窗高 = 自然高时面板恰好铺满窗)。
        let natural_h = (frame.response.rect.height() + 2.0 * panel_stroke.width).max(INPUT_H);
        // 死区(2026-10-17 第四版定案):行数差 ≤ `DEAD_ROWS` → 面板不动(高频小波动
        // 全静默);超出 → **实时平滑跟随**(收缩同样立即跟,不推迟到停手——「打字期
        // 只长不缩 + 停手收敛」实机反馈「输入完成后整框抖一下」,已拆除)。
        // 死区内取 max:增长不多走一步会把新行裁在窗外(截断比留白刺眼);收缩在
        // 同区内则保持原高(留白 ≤2 行,不可感知)。非列表态(空态/输入行)不设死区。
        let rows = if self.results.is_empty() {
            None
        } else {
            Some(self.results.len())
        };
        match (self.dead_rows, rows) {
            (Some(a), Some(b)) if a.abs_diff(b) <= DEAD_ROWS => {
                if natural_h > self.dead_h {
                    self.dead_h = natural_h; // 死区内增长照走(免裁切)
                }
            }
            _ => {
                self.dead_rows = rows;
                self.dead_h = natural_h;
            }
        }
        let target_h = self.dead_h;
        let dt = self
            .anim_at
            .map_or(Duration::ZERO, |t| now.saturating_duration_since(t));
        self.anim_at = Some(now);
        let prev_h = self.anim_h;
        self.anim_h = anim_step(prev_h, target_h, dt);
        let step = self.anim_h - prev_h;
        if step.abs() > 0.001 {
            let trace = self.anim_trace.get_or_insert((prev_h, 0.0, 0, now));
            trace.1 = trace.1.max(step.abs());
            trace.2 += 1;
            log::debug!(
                "面板高度动画:t={}ms {prev_h:.1} → {:.1}px(目标 {target_h:.1},dt {:.1}ms)",
                platform::monotonic_ms(),
                self.anim_h,
                dt.as_secs_f64() * 1000.0
            );
        }
        if self.anim_h == target_h {
            if let Some((start_h, peak, frames, t0)) = self.anim_trace.take() {
                // 收敛汇总一行(实机序列测量的机器可读证据;逐帧见 debug 留痕)
                log::info!(
                    "面板高度动画完成:{start_h:.0} → {target_h:.0}px,{frames} 帧 {:.0}ms,峰值单帧 {peak:.1}px",
                    t0.elapsed().as_secs_f64() * 1000.0
                );
            }
        } else {
            // 动画未收敛:补排下一帧(自驱到吸附为止,最长 ~300ms)。
            ctx.request_repaint_of(self.viewport);
        }

        // —— 背景填实:可见高矩形(底边 = 可见底边;圆角/描边恒在可见底边)——
        ui.painter().set(
            bg_idx,
            egui::Shape::Rect(egui::epaint::RectShape::new(
                egui::Rect::from_min_size(panel_origin, egui::vec2(PANEL_W, clip_h)),
                egui::CornerRadius::same(PANEL_RADIUS),
                panel_fill,
                panel_stroke,
                egui::StrokeKind::Inside,
            )),
        );

        let input_resp = frame.inner;
        // 组合中的预编辑文本不触发查询(IME 未上屏;提交/取消帧 composing 已复位,
        // 照常按最终文本查询)。
        if input_resp.changed() && !self.ime_composing {
            // 打点只覆盖「唤起后 1.5s」(首程诊断窗口);常态输入不逐键留痕,
            // 查询往返另有 `查询往返` 一行。
            if self.show_at.is_some() {
                log::info!(
                    "输入变更处理:t={}ms 输入={:?}(防抖 {}ms 起算)",
                    platform::monotonic_ms(),
                    self.input,
                    DEBOUNCE.as_millis()
                );
            }
            self.last_input_at = Some(now); // 空态滞后起算(见 EMPTY_HOLD)
            self.session.on_input(&self.input, now);
            if !self.input.is_empty() {
                // 防抖到期帧(空闲不排帧:隐藏/无输入时 CPU 仍 0%)
                ctx.request_repaint_after(DEBOUNCE);
            }
        }
        // 输入清空 = Idle 清空态:立即清结果,不等响应(product §1)。
        // 例外(2026-10-16 实机抖动根因):IME 过渡帧(Preedit("") 清组合)的空串不是
        // 「用户清空」——macOS 选字事件序为 Preedit("") → Commit("字")(egui-winit
        // 实测事件表),清组合那帧预编辑文本已移出输入框,此刻照清会把上一轮结果清掉,
        // 提交帧结果回来又弹开 → 一轮多余的高度往返(动画期表现为「缩半截再弹回」)。
        // 故过渡帧只置待决,非过渡帧复判:提交随即到达(输入非空)→ 取消;
        // 组合真被取消(仍空且无 IME 事件)→ 照清。
        let (do_clear, pending) = empty_clear_step(
            input_resp.changed(),
            self.ime_busy,
            self.input.is_empty(),
            self.pending_empty_clear,
        );
        if pending && !self.pending_empty_clear {
            log::info!(
                "空串清空待决:IME 过渡帧不清结果(现有 {} 项;提交到达即取消)",
                self.results.len()
            );
        }
        self.pending_empty_clear = pending;
        if do_clear {
            self.results.clear();
            self.total = 0;
            self.selected = 0;
            self.status_note = None;
            self.pending_empty = false; // 用户清空:直接落定,不留待决(product §1)
                                        // 高度不瞬跳:目标自然高变小(非列表态不设死区),由动画平滑收拢
                                        // 到输入行高(product §1)。
        }
        // —— 空态待决复判(主药后半):输入进行中的 0 命中已静默(仍显示上一轮
        // 结果与高度);输入停手(超 EMPTY_HOLD)后仍待决 → 真正切「无匹配项」小态,
        // 交由帧尾高度动画平滑收拢。——
        if self.pending_empty {
            let settled = self
                .last_input_at
                .is_none_or(|t| now.saturating_duration_since(t) >= EMPTY_HOLD);
            // 还须**无在途查询**(防抖窗口内 / 响应未回)才许落定:否则「区域B换」
            // (0 命中)静默后,刚删回「区域B」的那一帧就会把空态落定,随后 24 行的
            // 响应又弹回——实机实测的「塌-弹」往返(2026-10-17 首轮实机暴露)。
            let inflight = self.session.pending_delay(now).is_some() || self.query_sent.is_some();
            if settled && !inflight {
                self.pending_empty = false;
                self.results.clear();
                self.total = 0;
                self.selected = 0;
                self.session.mark_total(0); // → State::Empty
                log::info!("空态滞后落定:输入停手且无在途查询,切「无匹配项」");
            } else {
                // 复判帧排到点(早到的帧不丢后续排帧;同失焦宽限的排帧纪律);
                // 已停手但在途 → 0ms 连排,等响应落地那一帧复判。
                let left = if settled {
                    Duration::ZERO
                } else {
                    EMPTY_HOLD.saturating_sub(
                        now.saturating_duration_since(self.last_input_at.unwrap_or(now)),
                    )
                };
                ctx.request_repaint_after(left);
            }
        }
        if self.needs_focus {
            input_resp.request_focus();
            self.needs_focus = false;
        }

        // —— 窗口高度输出(动画值;动画期每帧发,收敛后只在变化时发)——
        // 尺寸命令异步落到窗口(下一帧才生效):每帧 30px 内的小步,合成器旧帧拉伸
        // 窗口里最多错一拍、≤30px;立即补排一帧把窗口压到最短(旧「一次跳到位」
        // 的大位移拉伸伪影即由此消除)。
        let h = self.anim_h;
        if self.anim_h != target_h || self.last_h.is_none_or(|last| (last - h).abs() > 0.5) {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(PANEL_W, h)));
            self.last_h = Some(h);
            ctx.request_repaint_of(self.viewport);
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

        self.pass_end(pass_t0);
        action
    }

    /// 帧尾计时结算(pass 体耗时 + 唤起后窗口期收尾)。
    fn pass_end(&mut self, pass_t0: Instant) {
        if let Some(since_show) = self.show_at {
            let d = pass_t0.elapsed();
            log::info!(
                "悬浮条首帧结算:pass#{} 自唤起 {:.1}ms,回调体 {:.1}ms",
                self.pass_seq,
                since_show.elapsed().as_secs_f64() * 1000.0,
                d.as_secs_f64() * 1000.0
            );
            if since_show.elapsed() > Duration::from_millis(1500) {
                self.show_at = None; // 常态停表(不再逐帧留痕)
            }
        }
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

/// 空串清空判定(纯逻辑;`draw` 输入处理节调用)。入参:`changed` = 本帧输入有变更;
/// `ime_busy` = 本帧有 IME 事件(预编辑/提交);`empty` = 输入当前为空串;
/// `pending` = 上帧留下的待决态。返回 (本帧是否执行清空, 新的待决态)。
///
/// IME 过渡帧(`ime_busy`)的空串只置待决、不清:macOS 选字事件序
/// `Preedit("") → Commit("字")`(egui-winit 实测表)里清组合那帧输入会短暂为空,
/// 照清会闪空面板(2026-10-16 实机抖动根因);下一帧提交到达(输入非空)即取消待决,
/// 组合真被取消(仍空且无 IME 事件)则照清。
fn empty_clear_step(changed: bool, ime_busy: bool, empty: bool, pending: bool) -> (bool, bool) {
    let pending = if changed { empty } else { pending };
    if pending && empty && !ime_busy {
        (true, false)
    } else {
        (false, pending)
    }
}

/// 空态滞后判定(纯逻辑;主药,`apply` 调用):响应 total=0 且输入仍在进行
/// (`typing` = 距末次输入 < `EMPTY_HOLD`)且现有结果非空 → 本次 0 命中静默,
/// 保留上一轮结果与面板高度(置 `pending_empty`,停手后复判)。
/// 现有结果已空时不拦(空态幂等,免待决无谓挂起)。
fn empty_hold(total: u32, typing: bool, has_results: bool) -> bool {
    total == 0 && typing && has_results
}

/// 面板高度动画步进(纯逻辑;模块头注「面板高度」):每帧向目标做时间基指数逼近
/// (`1 − e^(−dt/τ)`;τ 主导缓动形状),大差值前段再由速度上限(`H_MAX_SPEED × dt`)
/// 收一收开帧跨步。差值小于 `H_ANIM_EPS` 时直接吸附目标(精确相等,调用方据此停排帧)。
fn anim_step(cur: f32, target: f32, dt: Duration) -> f32 {
    let diff = target - cur;
    if diff.abs() < H_ANIM_EPS {
        return target;
    }
    // 长停帧(挂起恢复/系统忙)不按真实时长放大步长;上限 100ms 防 exp 溢出。
    let dt = dt.as_secs_f32().min(0.1);
    let k = 1.0 - (-dt / H_EASE_TAU).exp();
    let max_step = H_MAX_SPEED * dt;
    let step = (diff * k).clamp(-max_step, max_step);
    cur + step
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
                sort_echo: out.sort_echo,
                requested_sort: sort,
            },
            Err(err) => {
                // 失败分流:daemon 不可连(引擎拉起中/未运行)与查询级错误,角标不同
                let engine_down = client::connect(&paths).is_none();
                log::warn!(
                    "查询失败(engine_down={engine_down}, code={}):{}",
                    err.code.as_deref().unwrap_or("-"),
                    err.message
                );
                Payload::Failed {
                    engine_down,
                    code: err.code,
                }
            }
        };
        let engine = engine_status(&paths);
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

/// 引擎状态快照(查询线程顺带 / 完整窗口状态补查 / 重启后校准共用,勿复制第二份;
/// 失败留痕返回 None——不静默)。
pub(crate) fn engine_status(paths: &DaemonPaths) -> Option<EngineStatus> {
    match client::status(paths) {
        Ok(StatusOutcome::Reported(r)) => Some(EngineStatus {
            state: r.state,
            entry_count: r.entry_count,
            gaps: r.gaps,
            built_at: r.built_at,
        }),
        Ok(StatusOutcome::NoIndex) => None,
        Err(err) => {
            log::warn!("引擎状态查询失败:{err}");
            None
        }
    }
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

/// 所在目录(展示用):去掉文件名后中间省略(宽窗 720 下容 64 字符)。
fn dir_display(path: &str) -> String {
    let dir = match path.rsplit_once('/') {
        Some((d, _)) => d,
        None => path,
    };
    elide_middle(dir, 64)
}

/// 选中行高亮:主题 selection 色的柔和版(降不透明度),深浅主题通用——
/// 直接铺 `selection.bg_fill` 是饱和蓝块,抢结果文字(2026-10-16 实机反馈)。
fn selection_fill(visuals: &egui::Visuals) -> egui::Color32 {
    let c = visuals.selection.bg_fill;
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 60)
}

/// 结果行目录灰字:`text_color` 降不透明度到「可读但不抢」。
/// 不用 `weak()`:egui 默认 weak 档位过淡,叠加小字号后不可读(实机反馈)。
fn dir_text_color(visuals: &egui::Visuals) -> egui::Color32 {
    let c = visuals.text_color();
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 150)
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

    /// 高度动画步进(纯逻辑):单帧位移受速度上限约束、向目标单调收敛、收敛帧精确
    /// 吸附;dt=0(复位首帧)不动、长停帧不按真实时长放大——缓动形状由 τ 主导。
    #[test]
    fn anim_step_bounded_monotone_and_snaps() {
        let frame = Duration::from_millis(16); // 60fps
        let cap = H_MAX_SPEED * frame.as_secs_f32(); // 单帧位移上限(≈14.4px)
                                                     // 空态 → 大量结果(54 → 400):位移 ≤ 速度上限、单调递增、收敛帧 == 目标
        let mut h = 54.0;
        let mut peak = 0.0f32;
        let mut frames = 0;
        while h != 400.0 {
            let prev = h;
            h = anim_step(h, 400.0, frame);
            assert!(h > prev, "单调向目标:{prev} → {h}");
            peak = peak.max(h - prev);
            frames += 1;
            assert!(frames < 200, "必须收敛");
        }
        assert!(peak <= cap + 1e-3, "单帧位移超速度上限:{peak} > {cap}");
        assert!(
            frames >= 12,
            "缓动应有多帧观感(非一帧到位的瞬跳):{frames} 帧"
        );
        // 反向收缩同样有界且单调
        let mut h = 400.0;
        let mut peak = 0.0f32;
        while h != 79.0 {
            let prev = h;
            h = anim_step(h, 79.0, frame);
            assert!(h < prev, "单调向目标:{prev} → {h}");
            peak = peak.max(prev - h);
        }
        assert!(peak <= cap + 1e-3, "收缩单帧位移超上限:{peak}");
        // 差值小于阈值:一步吸附(停排帧,不留亚像素尾巴)
        assert_eq!(anim_step(54.0, 54.5, frame), 54.5);
        // 复位首帧(dt=0):不动(不抢跳)
        assert_eq!(anim_step(54.0, 400.0, Duration::ZERO), 54.0);
        // 长停帧(挂起恢复 3s):按 100ms 上限折算,不按真实 3s 放大
        let jump = anim_step(54.0, 400.0, Duration::from_secs(3)) - 54.0;
        assert!(
            jump <= H_MAX_SPEED * 0.1 + 1e-3,
            "长停帧步长应按 100ms 折算封顶:{jump}"
        );
        // 120Hz(8ms 帧):步长按速度上限折算,更细更平滑
        let hz120 = anim_step(54.0, 400.0, Duration::from_millis(8)) - 54.0;
        assert!(hz120 <= cap + 1e-3, "高刷屏步长更细:{hz120}");
    }

    // ------------------------------------------------------------------
    // 高度探针(headless;「屏幕录屏 + 抽帧量面板底边」的应用层同构版):
    // 逐帧注入输入法事件序(egui-winit 实测表形态:预编辑链 → Preedit("")+Commit
    // 选字),查询响应手工注入(不起工作线程),以 16ms 模拟帧间隔推进高度动画,
    // 记录每帧请求的窗口高度 `last_h`(= 屏上像素高 /2)与结果数。`--nocapture` 看序列表。
    // 用途:出现「单帧位移超速 / 非单调 / 收敛后不贴合自然高 / 停手后仍在动」时变红
    // (突变验证:给 anim_step 去掉钳制、或把窗口高改回内容高瞬跳,即红)。

    /// 探针帧长(模拟 60fps;动画是时间基插值,headless 逐帧 run 之间没有真实时长)。
    const PROBE_DT: Duration = Duration::from_millis(16);

    /// 造 N 条命中(只用于行数/高度,内容不参与布局高度)。
    fn hits(n: usize) -> Vec<HitEntry> {
        (0..n)
            .map(|i| HitEntry {
                name: format!("命中{i}.md"),
                path: format!("/tmp/mce-probe/Documents/子目录/命中{i}.md"),
                size: None,
                mtime: None,
            })
            .collect()
    }

    /// 跑一帧(注入事件 → draw),返回本帧请求的窗口高度。
    fn probe_frame(ctx: &egui::Context, st: &mut PaletteState, events: Vec<egui::Event>) -> f32 {
        // 模拟 16ms 帧间隔(动画时间基;dt 由上一帧时刻算出)。
        st.anim_at = Some(Instant::now() - PROBE_DT);
        // 窗高模拟为「上一帧请求的高」(尺寸命令下一帧落地):可见高取 min 的路径
        // 与实机一致;窗高只影响裁剪,不影响本帧请求的高(测量值)。
        let win_h = st.last_h.unwrap_or(900.0).max(INITIAL_H);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(PANEL_W, win_h),
            )),
            events,
            ..Default::default()
        };
        // 根 Ui 直驱(与 `ui::draw_palette` 的 deferred 视口回调同形态)。
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = st.draw(ui);
        });
        out.textures_delta.clear(); // headless:不做纹理上传,清掉免得 Drop 告警
        st.last_h.unwrap_or_default()
    }

    /// 连续走帧到高度收敛(动画吸附;上限防死循环),逐帧记入 seq。
    fn probe_settle(
        ctx: &egui::Context,
        st: &mut PaletteState,
        seq: &mut Vec<(String, String, usize, f32)>,
    ) {
        let mut prev = st.last_h;
        for _ in 0..120 {
            let h = probe_frame(ctx, st, vec![]);
            seq.push(("anim".into(), st.input.clone(), st.results.len(), h));
            if prev.is_some_and(|p| (p - h).abs() < 1e-3) {
                return; // 连续两帧同高 = 已吸附
            }
            prev = Some(h);
        }
        panic!("高度动画 120 帧内未收敛");
    }

    /// 一阶段:注入事件 → 走帧到收敛;返回本阶段逐帧高(首帧含事件帧)。
    fn probe_phase(
        ctx: &egui::Context,
        st: &mut PaletteState,
        seq: &mut Vec<(String, String, usize, f32)>,
        tag: &str,
        events: Vec<egui::Event>,
    ) -> Vec<f32> {
        let start = seq.len();
        let h = probe_frame(ctx, st, events);
        seq.push((tag.into(), st.input.clone(), st.results.len(), h));
        probe_settle(ctx, st, seq);
        seq[start..].iter().map(|r| r.3).collect()
    }

    /// 断言:相邻帧位移 ≤ 速度上限×帧长(≈14.4px @60fps),且整段单调(不来回抽)。
    fn assert_smooth(heights: &[f32], expect_up: bool) {
        // 上限按「模拟帧长 + 帧内耗时余量」折算(headless 帧的实测 dt ≈ 16-17ms)
        let cap = H_MAX_SPEED * (PROBE_DT + Duration::from_millis(3)).as_secs_f32() + 1e-3;
        for w in heights.windows(2) {
            let (a, b) = (w[0], w[1]);
            assert!(
                (a - b).abs() <= cap,
                "单帧位移超速:{a} → {b}(上限 {cap:.1}px;序列 {heights:?})"
            );
            if expect_up {
                assert!(b >= a, "扩张段必须单调不减:{a} → {b}");
            } else {
                assert!(b <= a, "收缩段必须单调不增:{a} → {b}");
            }
        }
    }

    /// 防抖点火 + 注入合成响应(当代代际号取真正 fire 的),不起工作线程。
    fn probe_respond(st: &mut PaletteState, rows: usize) {
        let fire = st
            .session
            .pending_at(Instant::now() + DEBOUNCE)
            .expect("有待发查询才注入响应");
        let entries = hits(rows);
        st.apply(
            QueryResult {
                generation: fire.generation,
                payload: Payload::Hits {
                    entries,
                    total: rows as u32,
                    elapsed_ms: 0.1,
                    stat_ms: 0.1,
                    offline: false,
                    sort_echo: String::new(),
                    requested_sort: None,
                },
                engine: None,
            },
            Instant::now(),
        );
    }

    /// 同串重发(模拟又敲了一键/引擎兜底重查):置新 pending 并把输入活跃时刻
    /// 推到当下(打字窗口内,死区/空态滞后生效)。
    fn probe_refire(st: &mut PaletteState) {
        st.session.on_input(&st.input.clone(), Instant::now());
        st.last_input_at = Some(Instant::now());
    }

    /// 模拟「输入停手」(末次输入已过 `EMPTY_HOLD`):空态待决复判/滞后判定按停手处理。
    fn probe_settle_input(st: &mut PaletteState) {
        st.last_input_at = Some(Instant::now() - EMPTY_HOLD - Duration::from_millis(50));
    }

    fn probe_commit(t: &str) -> egui::Event {
        egui::Event::Ime(egui::ImeEvent::Commit(t.into()))
    }

    /// 隔离索引实测命中数(2026-10-17,`macEverything query` 于 /tmp/mce-anim 词表):
    /// 湖=28、区域B=27、区域B报=24、区域B省=3、区域B换=0、zzqqxx=0——覆盖 0 命中与非 0
    /// 命中两类,并含「大波动」(28→3)与「往返穿越」(多↔0)。
    fn real_hits(q: &str) -> usize {
        match q {
            "湖" => 28,
            "区域B" => 27,
            "区域B报" => 24,
            "区域B省" => 3,
            _ => 0, // 区域B换 / zzqqxx / 空串等:0 命中
        }
    }

    /// 高度序列验收矩阵(headless;「屏幕录屏 + 抽帧量面板底边」的应用层同构版):
    /// 逐帧注入输入法事件序(预编辑链 → Preedit("")+Commit 选字),响应手工注入
    /// (**行数取隔离索引实测命中数**),以 16ms 模拟帧间隔推进高度动画,记录每帧
    /// 请求的窗口高 `last_h`。四个场景(0 命中冷启动 / 状态往返穿越 / 行数大波动 /
    /// 删字往返)逐段断言:单帧位移 ≤ 速度上限、同目标段单调、死区段零抽动、
    /// 死区零抽动、收敛精确贴合自然高(54 空输入 / 79 空态 / 317 八行 / 410 封顶)。
    /// 突变验证:去掉 `anim_step` 钳制、去掉空态滞后或死区,本测试即红。
    #[test]
    fn typing_height_sequence_is_smooth_and_fits_content() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        let mut seq: Vec<(String, String, usize, f32)> = Vec::new();
        // 预热两帧:首帧 request_focus,次帧起输入框持焦(IME 事件才归它)。
        for _ in 0..2 {
            let h = probe_frame(&ctx, &mut st, vec![]);
            seq.push(("warm".into(), st.input.clone(), st.results.len(), h));
        }
        assert_eq!(st.last_h, Some(INITIAL_H), "唤起/复位后停在输入行高");

        let backspace = || {
            vec![egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        // 打一个字符:提交 → 防抖响应(实测命中数)→ 收敛;返回本字两段的逐帧高。
        let type_char = |st: &mut PaletteState,
                         seq: &mut Vec<(String, String, usize, f32)>,
                         ch: &str|
         -> (Vec<f32>, Vec<f32>) {
            let framed = probe_phase(&ctx, st, seq, &format!("打字 {ch}"), vec![probe_commit(ch)]);
            let word = st.input.clone();
            let rows = real_hits(&word);
            probe_respond(st, rows);
            let resp = probe_phase(&ctx, st, seq, &format!("响应 {word}"), vec![]);
            (framed, resp)
        };

        // ================= S1 完全匹配不上的词(冷启动 0 命中)=================
        // 预期:无旧结果 → 空态立即落定,平滑塌到「无匹配项」行高(79);后续键零变化。
        let (_, first) = type_char(&mut st, &mut seq, "z");
        assert_smooth(&first, true); // 无旧结果:空态立即落定(平滑塌到 79)
        assert_eq!(*first.last().unwrap(), 79.0, "S1 首字落空态 79px:{first:?}");
        for ch in ["z", "q", "q", "x", "x"] {
            let (framed, resp) = type_char(&mut st, &mut seq, ch);
            assert!(
                framed
                    .iter()
                    .chain(resp.iter())
                    .all(|h| (h - 79.0).abs() < 0.5),
                "S1 空态持续期零抽动({ch}):{framed:?}/{resp:?}"
            );
        }
        for _ in 0..6 {
            probe_phase(&ctx, &mut st, &mut seq, "退格", backspace());
        }
        assert!(st.input.is_empty(), "S1 全删");
        let clear = probe_phase(&ctx, &mut st, &mut seq, "清空", vec![]);
        assert_smooth(&clear, false);
        assert_eq!(
            *clear.last().unwrap(),
            INITIAL_H,
            "清空回输入行高:{clear:?}"
        );

        // ================= S2 状态往返穿越(加字链,含 0 命中坠点)=================
        // 预期:28/27/24 行都封顶同高(零变化);降到 3 行时打字期死区不动;
        // 「区域B换」0 命中打字中滞后(结果仍 3、高度不动);停手 → 平滑落到空态。
        let (_, f) = type_char(&mut st, &mut seq, "湖"); // 28 行
        assert_smooth(&f, true);
        let h_cap = *f.last().unwrap();
        assert_eq!(h_cap, 410.0, "28 行封顶自然高:{f:?}");
        let (_, f) = type_char(&mut st, &mut seq, "北"); // 区域B 27 行
        assert!(
            f.iter().all(|h| (h - h_cap).abs() < 0.5),
            "27 行同封顶:{f:?}"
        );
        let (_, f) = type_char(&mut st, &mut seq, "报"); // 区域B报 24 行
        assert!(
            f.iter().all(|h| (h - h_cap).abs() < 0.5),
            "24 行同封顶:{f:?}"
        );
        // 删到「区域B」→ 再打「省」:区域B换?不——本链为 区域B报→(退格)区域B→(打)区域B省
        probe_phase(&ctx, &mut st, &mut seq, "退格 报", backspace());
        assert_eq!(st.input, "区域B");
        let (_, f) = type_char(&mut st, &mut seq, "省"); // 区域B省 3 行
        assert_eq!(st.results.len(), 3, "结果照常替换为 3 行");
        assert_smooth(&f, false);
        let h_3 = *f.last().unwrap();
        assert!(
            (h_3 - h_cap).abs() > 100.0,
            "S2 3 行(|28-3|=25 > 死区)实时平滑跟到 3 行自然高:{f:?}"
        );
        // 0 命中坠点:再打「换」(区域B省换 = 0 命中,仍在打字)→ 空态滞后
        let (_, f) = type_char(&mut st, &mut seq, "换");
        assert!(st.pending_empty, "0 命中打字中 → 空态待决");
        assert_eq!(st.results.len(), 3, "0 命中不落地:保留上一轮 3 行");
        assert!(f.iter().all(|h| (h - h_3).abs() < 0.5), "{f:?}");
        // 停手且无在途 → 复判落定:平滑落到空态行高
        probe_settle_input(&mut st);
        let down = probe_phase(&ctx, &mut st, &mut seq, "停手落定", vec![]);
        assert_smooth(&down, false);
        assert!(st.results.is_empty(), "停手后切空态");
        assert_eq!(*down.last().unwrap(), 79.0, "空态贴合:{down:?}");
        for _ in 0..4 {
            probe_phase(&ctx, &mut st, &mut seq, "退格", backspace());
        }
        assert!(st.input.is_empty());
        let clear = probe_phase(&ctx, &mut st, &mut seq, "清空", vec![]);
        assert_eq!(*clear.last().unwrap(), INITIAL_H, "清空回 54:{clear:?}");

        // ================= S3 行数大波动(28 行封顶 ↔ 3 行)=================
        // 预期:28 → 410;降到 3 行打字中不动(死区);停手平滑收拢到 3 行自然高。
        let (_, f) = type_char(&mut st, &mut seq, "湖"); // 28
        assert_eq!(*f.last().unwrap(), h_cap, "28 行封顶");
        let (_, _) = type_char(&mut st, &mut seq, "北");
        let (_, f) = type_char(&mut st, &mut seq, "省"); // 区域B省 3 行:大波动实时跟
        assert_smooth(&f, false);
        let h3b = *f.last().unwrap();
        assert!(h3b < 200.0 && h3b > 79.0, "28→3 行大波动实时平滑收拢:{f:?}");
        // 停手后零高度变化事件(验收口径「输入完成后无额外动作」)
        probe_settle_input(&mut st);
        let idle = probe_phase(&ctx, &mut st, &mut seq, "停手静置", vec![]);
        assert!(
            idle.iter().all(|h| (h - h3b).abs() < 0.5),
            "停手后不得再有高度动作:{idle:?}"
        );

        // ================= S3b 死区(±2 行静默;基准显式重置)=================
        // 基准重置:前段基准=3(区域B省响应),先以 8 行(差 5>2 出区)重置基准与承诺高,
        // 再从 8 往下验证——避免「5 相对 3 仍区内」把整段假设链带歪(曾致假红)。
        // 预期:8→7→6 行(差 ≤2)面板不动;4 行(差 4)起实时跟随。
        probe_refire(&mut st);
        probe_respond(&mut st, 8);
        let base = probe_phase(&ctx, &mut st, &mut seq, "合 8 行(重置基准)", vec![]);
        let h8 = *base.last().unwrap();
        for rows in [7, 6] {
            probe_refire(&mut st);
            probe_respond(&mut st, rows);
            let dz = probe_phase(
                &ctx,
                &mut st,
                &mut seq,
                &format!("合 {rows} 行(死区内)"),
                vec![],
            );
            assert_eq!(st.results.len(), rows, "结果照常替换");
            assert!(
                dz.iter().all(|h| (h - h8).abs() < 0.5),
                "死区内行数小波动不得动面板({rows} 行):{dz:?}"
            );
        }
        probe_refire(&mut st);
        probe_respond(&mut st, 4);
        let beyond = probe_phase(&ctx, &mut st, &mut seq, "合 4 行(出死区)", vec![]);
        assert_smooth(&beyond, false);
        assert!(
            (*beyond.last().unwrap() - h8).abs() > 40.0,
            "出死区即实时跟随(4 行比 8 行低 ~4 行高):{beyond:?}"
        );
        // 回打多行,清场
        probe_refire(&mut st);
        probe_respond(&mut st, 28);
        let _ = probe_phase(&ctx, &mut st, &mut seq, "合 28 行", vec![]);

        // ================= S4 删字往返(3 行 → 27 行 → 28 行 → 空)=================
        // 预期:删「省」回 27 行 → 实时平滑扩张回封顶(增量即时跟随);再删到 28 行零变化;
        // 删到空 → 清空立即回输入行高。
        probe_phase(&ctx, &mut st, &mut seq, "退格 省", backspace());
        assert_eq!(st.input, "区域B");
        probe_respond(&mut st, real_hits("区域B")); // 27 行
        let up = probe_phase(&ctx, &mut st, &mut seq, "删字回 27 行(实时扩张)", vec![]);
        assert_smooth(&up, true);
        assert_eq!(*up.last().unwrap(), h_cap, "27 行回到封顶:{up:?}");
        probe_phase(&ctx, &mut st, &mut seq, "退格 北", backspace());
        assert_eq!(st.input, "湖");
        probe_respond(&mut st, real_hits("湖")); // 28 行
        let flat = probe_phase(&ctx, &mut st, &mut seq, "删字回 28 行", vec![]);
        assert!(flat.iter().all(|h| (h - h_cap).abs() < 0.5), "{flat:?}");
        probe_phase(&ctx, &mut st, &mut seq, "退格 湖", backspace());
        assert!(st.input.is_empty());
        let clear = probe_phase(&ctx, &mut st, &mut seq, "空输入", vec![]);
        assert_smooth(&clear, false);
        assert_eq!(*clear.last().unwrap(), INITIAL_H, "清空回 54:{clear:?}");

        println!("—— 验收矩阵小结(预期 vs 实测;命中数取自隔离索引实测)——");
        println!("S1 zzqqxx(0 命中,冷启动)      预期 空态立即落定 79px        实测 79px");
        println!("S2 湖28→区域B27→区域B报24→区域B省3 预期 封顶段零变化/3 行打字期不动  实测 {h_cap:.0}/{h_cap:.0}/{h_cap:.0}/{h_cap:.0}");
        println!("S2 区域B省换(0 命中,打字中)   预期 结果仍 3、高不动 → 停手落 79  实测 79px");
        println!("S3 湖28→区域B省3(大波动)     预期 打字期不动 → 停手平滑收拢     实测 {h_3:.1}px");
        println!(
            "S4 区域B省3→(删)区域B27→湖28    预期 实时平滑扩张回封顶           实测 {h_cap:.0}px"
        );
        println!("S4 全删(空输入)             预期 立即回输入行高 54            实测 54px");
        println!("—— 逐帧高度序列(输入=当前词 · 结果=行数 · 窗高) ——");
        for (tag, input, rows, h) in &seq {
            println!("{tag:<26} 输入={input:<10} 结果={rows:<3} 窗高={h:.0}px");
        }
    }

    /// 空态滞后(主药):输入进行中的 0 命中静默,停手后/无旧结果才落空态。
    #[test]
    fn empty_hold_only_while_typing_with_old_results() {
        // 打字中 0 命中 + 有旧结果 → 拦(保留旧结果与高度)
        assert!(empty_hold(0, true, true));
        // 输入停手 → 不拦(切空态)
        assert!(!empty_hold(0, false, true));
        // 旧结果已空 → 不拦(空态幂等,免无谓待决)
        assert!(!empty_hold(0, true, false));
        // 有命中 → 永不拦
        assert!(!empty_hold(7, true, true));
    }

    /// 空串清空判定:IME 过渡帧不清(预编辑帧不动结果区),非过渡帧照清(product §1)。
    #[test]
    fn empty_clear_defers_on_ime_transition_frame() {
        // macOS 选字事件序(egui-winit 实测表):Preedit("") 清组合 → Commit("字")。
        // 帧1:清组合(输入被清空 + 本帧有 IME 事件)→ 不清,置待决。
        assert_eq!(empty_clear_step(true, true, true, false), (false, true));
        // 帧2:提交到达(输入非空)→ 待决取消,不清。
        assert_eq!(empty_clear_step(true, true, false, true), (false, false));
        // 组合真被取消(仍空且无 IME 事件)→ 复判执行清空。
        assert_eq!(empty_clear_step(false, false, true, true), (true, false));
        // 用户退格清空(无 IME 事件)→ 立即清(product §1:清空即清结果)。
        assert_eq!(empty_clear_step(true, false, true, false), (true, false));
        // 无变更且非空 → 不误清、不误置待决。
        assert_eq!(empty_clear_step(false, false, false, false), (false, false));
    }

    #[test]
    fn badge_note_maps_six_states() {
        let eng = |s: &str| EngineStatus {
            state: s.into(),
            entry_count: 12,
            gaps: Vec::new(),
            built_at: 0,
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

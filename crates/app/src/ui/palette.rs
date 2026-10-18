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
//! 高度**贴合内容**(空态不留白;自然内容高 = 输入行 + 结果区 + 计数行),过渡呈现为
//! **窗内平滑**(PoC-6 定案,2026-10-17,取证见 poc-results.md「PoC-6」):目标高变更即
//! 从当前绘制高起一段 `H_ANIM_MS`(120ms)smoothstep 动画(`h_ease`),动画全部在 egui
//! 窗内绘制,排帧节流到 ~120Hz。**窗口先在目标一侧就位**:增长立即撑到目标(1 次原生
//! resize),收缩等动画收敛再收(1 次)——静止时窗口 = 面板 + 形象条 70(Task 9),
//! 过渡期窗口瞬时大于面板(透明区)。
//! 配套平台层 `layerContentsPlacement=TopLeft`(mac.rs;resize 时旧帧钉左上、裁切而非拉伸)。
//! 对照实测:旧「每帧发 InnerSize」= 134–175 次 resize/次变化,即抖动的来源。
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

use crate::hotkey::HotkeyNote;
use crate::i18n;
use crate::platform;
use crate::session::{Fire, Session, SortKey, State};
use crate::ui::font as ui_font;
use crate::ui::icons;
use crate::ui::mascot;
use crate::ui::tokens as tk;
use client::{DaemonPaths, StatusOutcome};
use protocol::HitEntry;

/// 防抖窗口(tech「查询会话」:60ms)。
pub const DEBOUNCE: Duration = Duration::from_millis(60);
/// 高度过渡时长(ms;PoC-6 定案取值,visual.md §6.4「建议 120ms」)。
const H_ANIM_MS: u64 = 120;
/// 过渡期排帧节流(≈120Hz)。未节流的实测值 ≈1250–1400 帧/秒(PoC-6 附加发现,
/// 逐帧驱动窗口高时即以该频率冲击 WindowServer)。
const H_FRAME: Duration = Duration::from_millis(8);
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
/// 悬浮条窗口**最大**宽度（视口创建处的 ViewportBuilder 用；visual.md §2.2 = 700）。
/// 实际面板宽由 `PaletteState::panel_w` 决定（唤起时按屏回填，窄屏收窄）。
pub const PANEL_W: f32 = tk::PANEL_W;
/// 窄屏宽度下限（visual.md §1.1：`min(700, 可见宽−48)`，下限 480）。
pub const PANEL_MIN_W: f32 = crate::platform::PALETTE_MIN_W as f32;
/// 宽度策略单源：平台层上限 = UI 面板最大宽（防 700 两处漂移；Task 11）。
const _: () = assert!(crate::platform::PALETTE_MAX_W as f32 == tk::PANEL_W);
/// 初始/复位面板高 = 空态面板高（搜索行 + 空态区 + 页脚；visual.md §2.7 = 176）。
/// 也是面板高过渡的起点（唤起首帧不缩放）。窗口高另加 `MASCOT_STRIP`。
pub const INITIAL_H: f32 = tk::panel_h(tk::list_h(0));
/// 形象露出面板顶边的条高（窗口高 = 面板高 + 本值；§2.9 / Task 9）。
/// 单源 = token 层；`ui/mod.rs` 的平台调用与 `tokens` 编译期钉住同一值。
pub const MASCOT_STRIP: f32 = tk::MASCOT_STRIP;

// ---------------------------------------------------------------------------
// 字号（visual.md §4.3；数值真源 = token 层，本文件禁散落色值/字号）
/// 查询文本（21pt）。
const FONT_INPUT: f32 = tk::FONT_QUERY;
/// 结果行文件名（15pt）。
const FONT_ROW: f32 = tk::FONT_FILENAME;
/// 结果行所在目录（12pt）。
const FONT_DIR: f32 = tk::FONT_DIR;
/// 页脚标签 / 结果计数 / 动作提示（12pt）。
const FONT_META: f32 = tk::FONT_HINT;
/// 键帽 / 状态角标（11pt）。
const FONT_KEYCAP: f32 = tk::FONT_KEYCAP;
/// 空态主文案（14pt 基准；绘制时经全局倍率）。
const FONT_EMPTY_TITLE: f32 = tk::FONT_EMPTY_TITLE;
/// 状态角标 chip 最大宽（px）：超长错误文案不得挤爆搜索行。
const BADGE_MAX_W: f32 = 240.0;
/// 搜索行输入框下限（px）：角标挤压时的底线（与布局 `.max` 同源）。
const SEARCH_INPUT_MIN_W: f32 = 80.0;
/// 搜索行除角标外的固定占用（px；按右→左布局实际推进核算）：
/// 左右内边距 40 + 按钮 28 + 形象净空 128 + 三处 item_spacing 42
/// + 搜索图标 20 + 输入框下限 80 = 338。
const SEARCH_ROW_FIXED_W: f32 = 2.0 * tk::SEARCH_PAD_X
    + tk::BTN_SIZE
    + tk::MASCOT_CLEAR_W
    + 3.0 * tk::SEARCH_GAP
    + tk::SEARCH_ICON
    + SEARCH_INPUT_MIN_W;

/// 搜索行角标 chip 可用宽：面板宽 − 固定占用（clamp 到 [0, `BADGE_MAX_W`]）。
/// 窄屏（480）下为 142；超长错误文案按此收窄，不挤掉输入框下限。
fn search_row_badge_max_w(panel_w: f32) -> f32 {
    (panel_w - SEARCH_ROW_FIXED_W).clamp(0.0, BADGE_MAX_W)
}

/// 结果行/输入框字号（proportional；供 RichText/TextEdit 直接引用）。
///
/// 入参为 token 层的基准 pt；绘制与 `text_w` 测量共用本函数，全局倍率只换算一次。
fn font(size: f32) -> egui::FontId {
    ui_font::id(size)
}

/// 首查按协议上限请求一次（product §1 分页语义：不重查、不翻页请求）。
const QUERY_LIMIT: u32 = client::RESULT_CAP;
/// 列表初始渲染行数与滚到底增量（product §1：初始 20 行，滚到底 +20）。
const ROWS_INITIAL: usize = 20;
const ROWS_STEP: usize = 20;
/// 滚动区行区最大高 = 6 行（不含结果区上下内边距；超出滚动；visual.md §2.2
/// 「结果区高 = 行数×56 + (行数−1)×2 + 16」——16 即上下内边距，单独加）。
const LIST_MAX_H: f32 = tk::list_h(6) - 2.0 * tk::LIST_PAD_V;
/// 排版区放开上限（px）：仅用于布局区（与窗高解耦），不作窗口高输出。
const PANEL_MAX_H: f32 = 4096.0;
/// 面板圆角（f64 形态；平台层垫窗签名用，避免调用点多处 `as`）。
pub const PANEL_RADIUS_F64: f64 = tk::PANEL_RADIUS as f64;

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

/// 打开失效的原因（应用自产；文案按当前语言渲染）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpenError {
    /// 路径已不存在（索引陈旧）。
    FileMissing,
    /// 系统 `open` 启动失败；`detail` 为原始技术原因。
    Failed(String),
}

impl OpenError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::FileMissing => i18n::s().palette_file_missing.to_owned(),
            Self::Failed(detail) => format!("{}: {detail}", i18n::s().palette_open_failed),
        }
    }
}

/// 角标文案（绘制时按当前语言渲染）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BadgeText {
    Building(u64),
    Reconciling,
    Rescanning,
    OfflineIndex,
    EngineNotRunning,
    Warming,
    QueryFailed,
    Open(OpenError),
}

impl BadgeText {
    fn text(&self) -> String {
        let s = i18n::s();
        match self {
            Self::Building(n) => {
                let n = n.to_string();
                i18n::fill(s.badge_building, &[("n", n.as_str())])
            }
            Self::Reconciling => s.badge_reconciling.to_owned(),
            Self::Rescanning => s.badge_rescanning.to_owned(),
            Self::OfflineIndex => s.badge_offline_index.to_owned(),
            Self::EngineNotRunning => s.badge_engine_not_running.to_owned(),
            Self::Warming => s.badge_building_index.to_owned(),
            Self::QueryFailed => s.badge_query_failed.to_owned(),
            Self::Open(err) => err.text(),
        }
    }
}

/// 状态角标（visual.md §2.6）：文案 + 强调度（决定底色与文字色）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub text: BadgeText,
    pub tone: BadgeTone,
}

impl Badge {
    fn neutral(text: BadgeText) -> Self {
        Self {
            text,
            tone: BadgeTone::Neutral,
        }
    }

    fn warning(text: BadgeText) -> Self {
        Self {
            text,
            tone: BadgeTone::Warning,
        }
    }

    /// 瞬时错误（打开失败等；非索引态映射）。
    fn error(text: BadgeText) -> Self {
        Self {
            text,
            tone: BadgeTone::Error,
        }
    }
}

/// 角标强调度（visual.md §2.6 / §4.1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadgeTone {
    Neutral,
    Warning,
    Error,
}

/// 绘制产出的动作,交上层(`ui::App`)执行。
pub enum Action {
    /// 隐藏悬浮条(打开成功 / Esc / 失焦)。
    Hide,
    /// ⌥↵:打开完整窗口并带当前查询词(上层消费 `full_window_request` → `ResultsState::set_query`)。
    FullWindow,
}

/// headless 测试探针:搜索行关键矩形(输入/图标左缘、角标 chip 宽、面板左缘/顶边)。
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
struct SearchRowProbe {
    panel_left: f32,
    /// 面板顶边(窗口内 y;恒 = `MASCOT_STRIP`,形象锚点不随面板高度变化)。
    panel_top: f32,
    input_left: f32,
    input_right: f32,
    icon_left: f32,
    badge_w: f32,
    /// 「打开完整窗口」按钮整矩形(与形象/旋转包围盒的避让断言用)。
    button_rect: egui::Rect,
    /// 搜索行内容(角标或输入框)最右缘(避让断言用)。
    content_right: f32,
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
    /// 状态角标（§2.6 六态映射 + 强调度；打开失败等瞬时提示用 `Badge::error`）。
    pub status_note: Option<Badge>,
    /// 热键提示(降级/不可用;Task 15)。常驻:不随唤起复位、不被查询结果覆盖。
    pub hotkey_note: Option<HotkeyNote>,
    /// 列表渲染行数(初始 20,滚到底 +20,至 `results.len()`)。
    pub visible_rows: usize,
    /// ⌥↵ 置位:完整窗口待带入的查询词(`ui::draw_palette` 转交 `ResultsState::set_query`)。
    pub full_window_request: Option<String>,
    /// 结果行系统图标缓存(UTI 纹理 + 路径自定义图标;`reset` 清路径无自定义/
    /// 失败条目,类型纹理与类型失败记录保留)。
    icons: icons::IconCache,
    /// 吉祥物纹理与姿态(跨帧持有;`reset` 不清纹理)。
    mascot: mascot::Mascot,

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
    /// 当前面板宽(唤起时按屏幕回填;`PANEL_W` = 最大宽/非窄屏)。
    panel_w: f32,
    /// 测试探针:最近一帧搜索行关键矩形(headless 越界断言用;生产不编译)。
    #[cfg(test)]
    search_probe: Option<SearchRowProbe>,
    /// 已下发的**窗口**高度(`send_h` 记账 = 面板高 + `MASCOT_STRIP`;探针与测试读它)。
    last_h: Option<f32>,
    /// 面板高过渡:当前绘制高(面板高单位,含内边距与描边;窗口另加形象条)。
    /// `reset` 复位到 `INITIAL_H`(唤起首帧不缩放)。
    anim_h: f32,
    /// 进行中的高度过渡:(起点高, 目标高, 起始时刻);None = 已收敛。
    h_anim: Option<(f32, f32, Instant)>,
    /// 过渡留痕(起点高, 帧数, 起算时刻):收敛时汇总一行 info——实机「每次高度变化
    /// 只发 1 次原生 resize」的机器可读证据(配 `h_resizes`)。
    anim_trace: Option<(f32, u32, Instant)>,
    /// 会话内原生 resize 计数(证据与测试读它;每次高度变化应恰 +1)。
    h_resizes: u32,
    /// 最近一帧实测的**自然内容高**（三段之和，未走 `INITIAL_H` 兜底）。
    /// 测试用它钉住「内部版面精确等于设计推导」——只看窗口高会被兜底遮蔽
    /// （空态块若少占高，窗口仍是 176，页脚却整体上移）。
    natural_h: f32,
    /// 本帧**可见面板高**（draw 的 `clip_h`；平台垫窗按它裁磨砂，避免动画期
    /// 磨砂带伸出可见面板底）。`reset` 复位到 `INITIAL_H`。
    visible_panel_h: f32,
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
        hotkey_note: Option<HotkeyNote>,
    ) -> Self {
        let mut st = Self::with_icons(
            egui_ctx.clone(),
            paths,
            viewport,
            hotkey_note,
            icons::IconCache::new(),
            mascot::Mascot::new(),
        );
        // 启动期预热（仅生产构造路径）：窗口此时隐藏，避开首次唤起首帧的 PNG
        // 解码 + Lanczos 缩放。测试构造 `new_for_test` 传入不加载纹理的 Mascot。
        st.mascot.ensure_texture(&egui_ctx);
        st
    }

    /// 测试构造:注入无 worker 的图标缓存,避免测试向 NSWorkspace 发假路径。
    #[cfg(test)]
    fn new_for_test(
        egui_ctx: egui::Context,
        paths: DaemonPaths,
        viewport: egui::ViewportId,
        hotkey_note: Option<HotkeyNote>,
    ) -> Self {
        Self::with_icons(
            egui_ctx,
            paths,
            viewport,
            hotkey_note,
            icons::IconCache::without_worker(),
            // 单测不加载纹理：构造与绘制都不解码 PNG（姿态/几何由纯函数测试覆盖）。
            mascot::Mascot::without_texture(),
        )
    }

    fn with_icons(
        egui_ctx: egui::Context,
        paths: DaemonPaths,
        viewport: egui::ViewportId,
        hotkey_note: Option<HotkeyNote>,
        icons: icons::IconCache,
        mascot: mascot::Mascot,
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
            icons,
            mascot,
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
            panel_w: PANEL_W,
            #[cfg(test)]
            search_probe: None,
            last_h: None,
            anim_h: INITIAL_H,
            h_anim: None,
            anim_trace: None,
            h_resizes: 0,
            natural_h: INITIAL_H,
            visible_panel_h: INITIAL_H,
            last_input_at: None,
            pending_empty: false,
            pass_seq: 0,
            last_pass_start: None,
            show_at: None,
            query_sent: None,
            pending_empty_clear: false,
        }
    }

    /// 本帧可见面板高（draw 的 `clip_h` 快照；平台垫窗按它裁磨砂）。
    pub fn visible_panel_h(&self) -> f32 {
        self.visible_panel_h
    }

    /// 结果行图标缓存（完整窗口共用同一份：UTI 纹理键与悬浮条一致，
    /// 勿为完整窗口再开第二个 worker）。
    pub fn icons_mut(&mut self) -> &mut icons::IconCache {
        &mut self.icons
    }

    /// 唤起打点(首帧计时起点;`ui::palette_show` 调用)。
    pub fn on_shown(&mut self, now: Instant) {
        self.show_at = Some(now);
    }

    /// 唤起时按屏幕回填面板宽(平台层实际落地宽;防御 clamp 到 [480, PANEL_W])。
    /// 须在 `reset` 前调用——reset 内的 `send_h(INITIAL_H)` 以本宽下发。
    pub fn set_panel_width(&mut self, w: f32) {
        self.panel_w = w.clamp(PANEL_MIN_W, PANEL_W);
    }

    /// 每次唤起/隐藏后复位(product §1:重复唤起居输入清空,每次都是全新查询)。
    pub fn reset(&mut self) {
        self.input.clear();
        self.session.on_input("", Instant::now()); // Idle + 作废在途响应
        self.results.clear();
        // 图标:清掉上次结果集的「无自定义/读取失败」路径缓存(自定义纹理保留;
        // 超过 2048 条按插入序淘汰)。在途请求不取消,响应落地后照常入库。
        self.icons.on_reset();
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
        // 高度复位到初始高并**立即下发**:视口窗常驻(隐藏不销毁),高度可能停在上次的
        // 自然高;唤起首帧不缩放(拉平期窗外区域全透明,不可见)。
        // 纹理不清(形象跨帧缓存;素材不变)。
        self.anim_h = INITIAL_H;
        self.h_anim = None;
        self.anim_trace = None;
        self.visible_panel_h = INITIAL_H;
        self.send_h(INITIAL_H);
        self.last_input_at = None;
        self.pending_empty = false;
        self.pending_empty_clear = false;
    }

    /// 下发窗口高并记账(去重由调用点按策略判边沿;`h_resizes` 供证据与测试)。
    /// 入参 `h` = **面板高**；窗口高 = `h + MASCOT_STRIP`（形象条在面板上方），
    /// `last_h` 记窗口高（探针的 `screen_rect` 与平台窗高同口径）。
    fn send_h(&mut self, h: f32) {
        let win_h = h + MASCOT_STRIP;
        self.ctx.send_viewport_cmd_to(
            self.viewport,
            egui::ViewportCommand::InnerSize(egui::vec2(self.panel_w, win_h)),
        );
        self.last_h = Some(win_h);
        self.h_resizes += 1;
        // 尺寸命令异步落地:补排一帧让「落地之后」的帧再同步一次平台层——
        // 垫窗按 `panel.frame()` 对齐、顶边重锚同理;否则过渡结束不再排帧(L4),
        // 最后一次收缩落地的尺寸永远没人跟(实机:面板下方残留一条磨砂带)。
        self.ctx
            .request_repaint_after_for(Duration::from_millis(32), self.viewport);
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
        // 当前面板宽(唤起时按屏回填;窄屏收窄,最大 PANEL_W)。
        let panel_w = self.panel_w;
        // 图标:drain 工作线程响应 + 限量上传纹理(取图/解码在工作线程,不阻塞本帧)。
        self.icons.begin_frame(&ctx);
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
                    i18n::s().search_placeholder.to_owned(),
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

        // —— 窗口 = 形象条 + 面板(Task 9;§2.9)——
        // 窗口顶部预留 70 给形象条;面板从形象条下缘起画。先读**窗矩形**(可见高/
        // 裁剪基准,须在放开布局区之前),再推进游标给形象条留位。平台层把窗口顶
        // 上移 strip,故面板顶边距可见区顶仍是 110(舞台口径不变)。
        let window_rect = ui.max_rect();
        let win_h = window_rect.height();
        // 布局区与窗高解耦(先读窗高再放开):内容自然高必须只由内容决定——根 ui
        // 的布局区默认为窗矩形,而窗高又由内容高驱动(动画输出),不放即形成
        // 「窗高 → 内容高 → 窗高」反馈环:结果区被窗高钳住,面板只能一帧长几 px
        // 慢慢「爬」向目标(前一版实机高度大幅摆动/慢爬的根因之一)。取富余上限
        // 让排版一次到位;可见性仍由下面的裁剪矩形(窗 ∩ 动画矩形)决定。
        ui.set_max_height(PANEL_MAX_H);
        ui.add_space(MASCOT_STRIP);
        let panel_origin = ui.cursor().min;
        // —— 面板背景占位(自绘;填实见帧尾)——
        // 背景矩形 = 本帧**可见高**矩形,而非自然内容高矩形:egui `Frame` 的背景
        // 恒等于内容高,动画期窗/内容两高不一致会出现「底边直角裁切」或「背景瞬贴
        // 内容(动画不可见)」两种伪影(模块头注「面板高度」)。占位先落笔、内容后画,
        // 遮挡关系正确(egui `Frame` 内部同法:先 add(Shape::Noop),末尾 set)。
        // 面板可见高 = min(动画高, 真实窗高 − 形象条):窗尺寸命令下一帧才落地
        // (扩张期窗还矮),不取小则背景伸到窗外被裁 → 底边直角(圆角丢失)。
        // 窗高取不到(创建首帧)时以动画高为准。
        let clip_h = if win_h > 1.0 {
            self.anim_h.min(win_h - MASCOT_STRIP)
        } else {
            self.anim_h
        };
        // 平台垫窗按本帧可见面板高裁磨砂（动画期窗口先停在旧高，整窗垫窗会
        // 在可见面板下方露磨砂带）。
        self.visible_panel_h = clip_h;
        ui.set_clip_rect(egui::Rect::from_min_size(
            panel_origin,
            egui::vec2(panel_w, clip_h),
        ));
        let bg_idx = ui.painter().add(egui::Shape::Noop);
        // 设计 token（visual.md §四；本文件禁散落色值/字号）
        let t = tk::palette(ui.visuals().dark_mode);
        // 面板底 = 设计 token 的 alpha（深 `#191C21CC` / 浅 `#FFFFFFD6`），叠在垫窗
        // （平台层 NSVisualEffectView，PoC-7 定案）之上。若毛玻璃不可用（降级路径，
        // 见 visual.md §6.1 路线①），此处改 `t.panel_fill.to_opaque()` 走实底。
        let panel_fill = t.panel_fill;
        let panel_stroke = egui::Stroke::new(1.0, t.panel_stroke);

        let note = self.status_note.clone();
        let hotkey_note = self.hotkey_note.as_ref().map(HotkeyNote::text);
        // 页脚所需（借 `self.input` 可变之前取值，避免与闭包内的 `self` 借用冲突）
        let has_query = !self.input.is_empty();
        let total = self.total;
        let input = &mut self.input;
        let mut at_bottom = false;
        // 本帧「打开完整窗口」按钮被点击（画完在动作段消费）。
        let mut btn_full = false;
        // 测试探针：搜索行关键矩形（生产构建不编译）。
        #[cfg(test)]
        let mut search_probe: Option<SearchRowProbe> = None;
        // 布局壳（三段：搜索行 / 结果区 / 页脚；各段自带内边距，壳本身不加）。
        // 填色描边归自绘背景，故 Frame 本体不带背景。
        let frame = egui::Frame::new()
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                // 内容宽 = 面板宽（Task 11：描边画在内侧 1px，不再从内容宽里扣 2；
                // 右侧 20 内边距与按钮位置按设计稿落在面板右缘内侧）。
                ui.set_width(panel_w);
                // 三段高按 token 精确拼装：段间不留 item_spacing（否则整段多垫 N×4px）。
                ui.spacing_mut().item_spacing.y = 0.0;
                // —— 搜索行（高 64；右端为「打开完整窗口」按钮，右→左：
                //    按钮/形象净空/角标/输入/图标）——
                let row_w = ui.available_width();
                let input_resp = ui
                    .allocate_ui_with_layout(
                        egui::vec2(row_w, tk::SEARCH_H),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.spacing_mut().item_spacing.x = tk::SEARCH_GAP;
                            ui.add_space(tk::SEARCH_PAD_X); // 右端内边距（右→左，先右后左）
                                                            // 「打开完整窗口」按钮（28×28 圆角 7；静止态须一眼可见，§2.3）
                            let (btn_rect, btn_resp) = ui.allocate_exact_size(
                                egui::vec2(tk::BTN_SIZE, tk::BTN_SIZE),
                                egui::Sense::click(),
                            );
                            let fill = if btn_resp.is_pointer_button_down_on() {
                                t.btn_fill_hover.gamma_multiply(0.85)
                            } else if btn_resp.hovered() {
                                t.btn_fill_hover
                            } else {
                                t.btn_fill
                            };
                            ui.painter().rect_filled(
                                btn_rect,
                                egui::CornerRadius::same(tk::BTN_RADIUS),
                                fill,
                            );
                            draw_open_full_icon(ui.painter(), btn_rect, t.btn_icon);
                            if btn_resp
                                .on_hover_text(i18n::s().open_in_full_window)
                                .clicked()
                            {
                                btn_full = true;
                            }
                            // 形象净空（§2.9）：无角标也保留，保证输入框右缘
                            // 不进入形象区域（角标右缘距形象左缘 12）。
                            ui.add_space(tk::MASCOT_CLEAR_W);
                            // 状态角标（非 ready 才有；落在按钮左侧，§2.6）：
                            // 形制与键帽同族（圆角 4 / 上下 3 左右 6 / 11pt，
                            // 高 = KEYCAP_H），底色与文字色按强调度取 token。
                            // 可用宽随面板宽收窄（窄屏 480 下 142）：超长文案先按
                            // 实测宽截断，不挤掉输入框下限（见 `search_row_badge_max_w`）。
                            #[cfg(test)]
                            let mut badge_w = 0.0_f32;
                            #[cfg(test)]
                            let mut badge_right = 0.0_f32;
                            if let Some(badge) = &note {
                                let chip_max_w = search_row_badge_max_w(panel_w);
                                let text_max_w = (chip_max_w - 2.0 * tk::KEYCAP_PAD.1).max(0.0);
                                let badge_text = badge.text.text();
                                let text = badge_text_fit(ui, &badge_text, text_max_w);
                                let w = (text_w(ui, &text, FONT_KEYCAP) + 2.0 * tk::KEYCAP_PAD.1)
                                    .min(chip_max_w);
                                #[cfg(test)]
                                {
                                    badge_w = w;
                                }
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(w, tk::KEYCAP_H),
                                    egui::Sense::hover(),
                                );
                                #[cfg(test)]
                                {
                                    badge_right = rect.right();
                                }
                                let (fill, text_color) = badge_colors(badge.tone, &t);
                                let p = ui.painter();
                                p.rect_filled(
                                    rect,
                                    egui::CornerRadius::same(tk::KEYCAP_RADIUS),
                                    fill,
                                );
                                p.text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    &text,
                                    font(FONT_KEYCAP),
                                    text_color,
                                );
                            }
                            // 查询区（占满余宽 − 图标 − 间距 − 左端内边距；空词只有光标）
                            let w = (ui.available_width()
                                - tk::SEARCH_ICON
                                - tk::SEARCH_GAP
                                - tk::SEARCH_PAD_X)
                                .max(SEARCH_INPUT_MIN_W);
                            let input_resp = ui.add_sized(
                                [w, tk::SEARCH_H],
                                egui::TextEdit::singleline(input)
                                    .id_salt("palette-input")
                                    .font(font(FONT_INPUT))
                                    .text_color(t.query_text)
                                    // 裸输入（设计稿：无框无底，只有文字与光标）
                                    .frame(egui::Frame::NONE)
                                    // 文字垂直居中（同结果窗口；egui 0.37 主题的 TextEdit
                                    // align2 默认 LEFT_TOP，add_sized 拉高后贴顶）。
                                    .align(egui::Align2::LEFT_CENTER)
                                    .hint_text(""),
                            );
                            // 搜索图标（20×20；自绘，§5.2 `search`）
                            let (icon_rect, _) = ui.allocate_exact_size(
                                egui::vec2(tk::SEARCH_ICON, tk::SEARCH_ICON),
                                egui::Sense::hover(),
                            );
                            draw_search_icon(ui.painter(), icon_rect, t.search_icon);
                            #[cfg(test)]
                            {
                                search_probe = Some(SearchRowProbe {
                                    panel_left: panel_origin.x,
                                    panel_top: panel_origin.y,
                                    input_left: input_resp.rect.left(),
                                    input_right: input_resp.rect.right(),
                                    icon_left: icon_rect.left(),
                                    badge_w,
                                    button_rect: btn_rect,
                                    content_right: badge_right.max(input_resp.rect.right()),
                                });
                            }
                            ui.add_space(tk::SEARCH_PAD_X); // 左端内边距
                            input_resp
                        },
                    )
                    .inner;
                // 搜索行底分隔线（1px，§2.1）
                let sep = ui.available_rect_before_wrap();
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(sep.min, egui::vec2(panel_w - 2.0, tk::DIVIDER_H)),
                    0,
                    t.divider,
                );
                ui.add_space(tk::DIVIDER_H);

                // —— 热键提示(Task 15;降级「已改用 X」/不可用;常驻不随唤起复位)——
                if let Some(note) = &hotkey_note {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(note.as_str())
                            .size(ui_font::size(FONT_META))
                            .color(ui.visuals().warn_fg_color),
                    );
                }

                // —— 结果区 / 空态（中段）——
                // 结果只在响应落地时整体替换（上一轮结果保留到新响应，减少高度过渡的
                // 目标变化次数）；行数变化由窗内平滑过渡（模块头注「面板高度」）。
                if self.results.is_empty() {
                    // 空态两屏（§2.7）：空词 / 无匹配项；区高 72、纵向居中、主副间距 4
                    let (primary, secondary) = empty_text(&self.session.state);
                    // 先按 EMPTY_H 精确占位，再在块内居中放两行——`allocate_ui_with_layout`
                    // 只按内容实际用量推进游标（实测只占 51px），页脚会整体上移、下方留白。
                    let (empty_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), tk::EMPTY_H),
                        egui::Sense::hover(),
                    );
                    let mut empty_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(empty_rect)
                            .layout(egui::Layout::top_down(egui::Align::Center)),
                    );
                    empty_ui.add_space((tk::EMPTY_H - tk::EMPTY_TEXT_H) / 2.0);
                    empty_ui.spacing_mut().item_spacing.y = tk::EMPTY_GAP;
                    // 空态主文案（token 基准 14pt 经全局倍率；几何占位不变）。
                    empty_ui.label(
                        egui::RichText::new(primary)
                            .size(ui_font::size(FONT_EMPTY_TITLE))
                            .color(t.file_name),
                    );
                    empty_ui.label(
                        egui::RichText::new(secondary)
                            .size(ui_font::size(FONT_DIR))
                            .color(t.dir_text),
                    );
                } else {
                    ui.add_space(tk::LIST_PAD_V);
                    let rows = self.results.len().min(self.visible_rows);
                    // 两个字段分开借:结果只读、图标缓存可变,闭包内不触碰 `self`。
                    let results = &self.results;
                    let icons = &mut self.icons;
                    let selected = self.selected;
                    let scroll_sel = self.scroll_to_selected;
                    let mut clicked: Option<usize> = None;
                    let out = egui::ScrollArea::vertical()
                        .max_height(LIST_MAX_H) // 行数少时收缩到内容高
                        .id_salt("palette-list")
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = tk::ROW_GAP;
                            for (i, e) in results.iter().take(rows).enumerate() {
                                let is_sel = i == selected;
                                let texture = icons.icon(ui.ctx(), e);
                                let row = draw_result_row(
                                    ui,
                                    e,
                                    is_sel,
                                    &t,
                                    panel_origin.x,
                                    panel_w,
                                    texture,
                                );
                                if row.clicked() {
                                    clicked = Some(i);
                                }
                                if is_sel && scroll_sel {
                                    ui.scroll_to_rect(row.rect, None);
                                }
                            }
                        });
                    if let Some(i) = clicked {
                        self.selected = i;
                    }
                    // 滚到底 → 本地增量渲染（+20，至 K；不重查）
                    at_bottom = rows < self.results.len()
                        && out.state.clamped_offset().y + out.inner_rect.height()
                            >= out.content_size.y - 4.0;
                    ui.add_space(tk::LIST_PAD_V);
                }
                // 结果区底分隔线（1px）
                let sep = ui.available_rect_before_wrap();
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(sep.min, egui::vec2(panel_w - 2.0, tk::DIVIDER_H)),
                    0,
                    t.divider,
                );
                ui.add_space(tk::DIVIDER_H);
                // —— 页脚（高 38）——
                draw_footer(ui, has_query, total, &t);
                input_resp
            });
        #[cfg(test)]
        {
            self.search_probe = search_probe;
        }

        if at_bottom {
            self.visible_rows += ROWS_STEP;
            ctx.request_repaint();
        }
        self.scroll_to_selected = false;

        // —— 高度:目标 = 自然高(贴合内容;过渡为窗内平滑,PoC-6 定案)——
        // 自然高 = 三段内容高之和（搜索行 64 + 结果区 + 页脚 38 + 两条分隔线）。
        // 描边画在面板矩形内侧 1px、叠在内容首尾像素上——与设计稿「面板高 466 =
        // 64+1+362+1+38」同口径，故不再另加描边宽。
        self.natural_h = frame.response.rect.height();
        let natural_h = self.natural_h.max(INITIAL_H);
        // 目标变更(或首次)→ 从当前绘制高起一段新过渡(中途再变即在现值上重起,不叠段)。
        // 静止(未在过渡且已贴合)不起空过渡——否则每帧白走一段 54→54,日志刷屏。
        let target_changed = self
            .h_anim
            .map_or((self.anim_h - natural_h).abs() > 0.5, |(_, to, _)| {
                (to - natural_h).abs() > 0.5
            });
        if target_changed {
            self.h_anim = Some((self.anim_h, natural_h, now));
            self.anim_trace = Some((self.anim_h, 0, now));
        }
        // 步进:固定 120ms smoothstep(时间基;到点精确吸附并结算停排帧)。
        if let Some((from, to, t0)) = self.h_anim {
            self.anim_h = h_ease(from, to, now.saturating_duration_since(t0));
            if let Some(trace) = &mut self.anim_trace {
                trace.1 += 1;
            }
            if self.anim_h == to {
                self.h_anim = None;
                if let Some((start_h, frames, t0)) = self.anim_trace.take() {
                    // 收敛汇总一行(实机「每次变化只发 1 次原生 resize」的证据)
                    log::info!(
                        "面板高度过渡完成:{start_h:.0} → {to:.0}px,{frames} 帧 {:.0}ms,resize 累计 {} 次",
                        t0.elapsed().as_secs_f64() * 1000.0,
                        self.h_resizes
                    );
                }
            }
        }
        // 窗口高策略(PoC-6 定案):增长**立即撑到目标**(1 次原生 resize);收缩等动画
        // 收敛再收(1 次)。静止时窗口 = 面板 + 形象条;过渡期窗口瞬时大于面板
        // (透明区,不可见)。`last_h` 为 None(创建首帧)视作 0:走增长分支直接对齐。
        let window_target = natural_h + MASCOT_STRIP;
        let cur_win_h = self.last_h.unwrap_or(0.0);
        if window_target > cur_win_h + 0.5
            || (self.h_anim.is_none() && window_target < cur_win_h - 0.5)
        {
            self.send_h(natural_h);
        }
        if self.h_anim.is_some() {
            // 过渡期排帧(节流到 ~120Hz;未节流实测 ≈1250–1400 帧/秒,PoC-6 附加发现)。
            ctx.request_repaint_after_for(H_FRAME, self.viewport);
        }

        // —— 背景填实:可见高矩形(底边 = 可见底边;圆角/描边恒在可见底边)——
        ui.painter().set(
            bg_idx,
            egui::Shape::Rect(egui::epaint::RectShape::new(
                egui::Rect::from_min_size(panel_origin, egui::vec2(panel_w, clip_h)),
                egui::CornerRadius::same(tk::PANEL_RADIUS),
                panel_fill,
                panel_stroke,
                egui::StrokeKind::Inside,
            )),
        );

        // —— 形象(Task 9;§2.9 层级硬约束)——
        // 面板子树在 Frame 内且带面板 clip;此处先把裁剪恢复为**整个窗矩形**,
        // 形象才不被面板 clip 裁掉,且语义上是面板的兄弟节点。绘制顺序在面板
        // 背景之后(压住面板顶边);`Image::paint_at` 不占布局、不可交互。
        ui.set_clip_rect(window_rect);
        let typing = self
            .last_input_at
            .is_some_and(|t| now.saturating_duration_since(t) < mascot::TYPING_WINDOW);
        let t = ui.input(|i| i.time) as f32;
        self.mascot
            .draw(ui, &self.session.state, typing, panel_origin, panel_w, t);

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

        // —— 键位动作(在 TextEdit 处理完本帧事件之后:⌥↵ 取到最新输入词)——
        let mut action = None;
        if btn_full {
            // 搜索行右端按钮 ≡ ⌥↵（§2.3/§2.8）：打开完整窗口并带入当前查询词
            self.full_window_request = Some(self.input.clone());
            action = Some(Action::FullWindow);
        } else if keys.enter {
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
            Err(err) => {
                // 边界(product §边界):文件已删/打开失败 → 提示,不隐藏、不静默
                self.status_note = Some(Badge::error(BadgeText::Open(err)));
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

/// 高度过渡步进(纯逻辑;PoC-6 定案):`elapsed / H_ANIM_MS` 的 smoothstep——两端缓、
/// 中段快的对称形状;到点(≥ 时长)精确返回目标,调用方据此吸附并停排帧。
fn h_ease(from: f32, to: f32, elapsed: Duration) -> f32 {
    let p = (elapsed.as_secs_f32() / (H_ANIM_MS as f32 / 1000.0)).min(1.0);
    let e = p * p * (3.0 - 2.0 * p);
    if p >= 1.0 {
        to
    } else {
        from + (to - from) * e
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
/// 悬浮条与完整窗口共用(勿复制第二份);失败返回按当前语言渲染的错误。
pub(crate) fn open_path(path: &str, reveal: bool) -> Result<(), OpenError> {
    if std::fs::metadata(path).is_err() {
        return Err(OpenError::FileMissing);
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
        Err(e) => Err(OpenError::Failed(e.to_string())),
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

/// 状态角标（visual.md §2.6 六态映射）。引擎状态优先（更新鲜）；会话态兜底。
fn badge_note(state: &State, engine: Option<&EngineStatus>) -> Option<Badge> {
    if let Some(eng) = engine {
        match eng.state.as_str() {
            client::state::BUILDING => {
                return Some(Badge::neutral(BadgeText::Building(u64::from(
                    eng.entry_count,
                ))));
            }
            // 就绪不占位；但查询级错误不得被 ready 吞掉（落到会话态 → 「查询失败」）。
            client::state::READY if !matches!(state, State::Error(_)) => return None,
            client::state::RECONCILING => return Some(Badge::warning(BadgeText::Reconciling)),
            client::state::DEGRADED => return Some(Badge::warning(BadgeText::Rescanning)),
            client::state::OFFLINE => return Some(Badge::warning(BadgeText::OfflineIndex)),
            _ => {} // 未知引擎态：落到会话态
        }
    }
    match state {
        State::EngineStarting => Some(Badge::error(BadgeText::EngineNotRunning)),
        State::Warming => Some(Badge::neutral(BadgeText::Warming)),
        State::Offline => Some(Badge::warning(BadgeText::OfflineIndex)),
        State::Error(_) => Some(Badge::error(BadgeText::QueryFailed)),
        State::Idle | State::Searching | State::Empty => None,
    }
}

/// 角标强调度 → (底, 字)（visual.md §2.6 / §4.1）。
fn badge_colors(tone: BadgeTone, t: &tk::Palette) -> (egui::Color32, egui::Color32) {
    match tone {
        BadgeTone::Neutral => (t.badge_neutral_fill, t.badge_neutral_text),
        BadgeTone::Warning => (t.badge_warning_fill, t.badge_warning_text),
        BadgeTone::Error => (t.badge_error_fill, t.badge_error_text),
    }
}

/// 所在目录(展示用):去掉文件名后中间省略(最大宽 700 下容 64 字符)。
fn dir_display(path: &str) -> String {
    let dir = match path.rsplit_once('/') {
        Some((d, _)) => d,
        None => path,
    };
    elide_middle(dir, 64)
}

/// 文本宽（页脚键帽/标签排版与状态角标宽度测量共用）。
fn text_w(ui: &egui::Ui, s: &str, size: f32) -> f32 {
    ui.painter()
        .layout_no_wrap(s.to_owned(), font(size), egui::Color32::WHITE)
        .size()
        .x
}

/// 角标文本按实测宽度截断：超 `max_w` 时逐字符回退并补「…」。
/// 不按字符数硬编码——中文/数字/标点宽度不同。
fn badge_text_fit(ui: &egui::Ui, text: &str, max_w: f32) -> String {
    if text_w(ui, text, FONT_KEYCAP) <= max_w {
        return text.to_string();
    }
    let mut out = String::new();
    for ch in text.chars() {
        out.push(ch);
        let mut probe = out.clone();
        probe.push('…');
        if text_w(ui, &probe, FONT_KEYCAP) > max_w {
            out.pop();
            break;
        }
    }
    out.push('…');
    out
}

/// 搜索图标（§5.2 `search`；圆 + 手柄）。
fn draw_search_icon(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.6, color);
    let c = rect.center() - egui::vec2(1.5, 1.5);
    p.circle_stroke(c, 5.5, stroke);
    p.line_segment([c + egui::vec2(4.0, 4.0), c + egui::vec2(8.0, 8.0)], stroke);
}

/// 「打开完整窗口」按钮图标（§5.2 `maximize-2`；对角双箭头）。
fn draw_open_full_icon(p: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    let s = egui::Stroke::new(1.5, color);
    let c = rect.center();
    p.line_segment([c + egui::vec2(-1.0, 1.0), c + egui::vec2(5.0, -5.0)], s);
    p.line_segment([c + egui::vec2(1.0, -5.0), c + egui::vec2(5.0, -5.0)], s);
    p.line_segment([c + egui::vec2(5.0, -1.0), c + egui::vec2(5.0, -5.0)], s);
    p.line_segment([c + egui::vec2(1.0, -1.0), c + egui::vec2(-5.0, 5.0)], s);
    p.line_segment([c + egui::vec2(-1.0, 5.0), c + egui::vec2(-5.0, 5.0)], s);
    p.line_segment([c + egui::vec2(-5.0, 1.0), c + egui::vec2(-5.0, 5.0)], s);
}

/// 空态两屏文案（空词 / 无匹配项；随语言切换）。
fn empty_text(state: &State) -> (&'static str, &'static str) {
    if matches!(state, State::Empty) {
        (i18n::s().empty_no_match, i18n::s().empty_no_match_sub)
    } else {
        (i18n::s().empty_start, i18n::s().empty_start_sub)
    }
}

/// 选中行动作提示（§2.4）：文件「打开」、目录「进入」。
fn action_hint(is_dir: bool) -> &'static str {
    if is_dir {
        "进入"
    } else {
        "打开"
    }
}

/// 结果行（§2.4）：高 56 / 圆角 8 / 内边距 12 / 图标 32 裸放 / 文本列 15+12 /
/// 动作提示 12pt 仅选中行。悬停 = 选中态约 50% 强度；鼠标移动不改键盘选中。
fn draw_result_row(
    ui: &mut egui::Ui,
    e: &HitEntry,
    is_sel: bool,
    t: &tk::Palette,
    panel_x: f32,
    panel_w: f32,
    texture: Option<egui::TextureId>,
) -> egui::Response {
    // 占位分配（只为拿滚动区的 y 与点击响应）。
    let (slot, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), tk::ROW_H),
        egui::Sense::click(),
    );
    // 行矩形按**面板坐标系**显式定位：滚动区内容 ui 的原点会被 egui 左移、
    // 宽度含滚动条余量（实测 min.x=-14 / 宽 712），不能拿它当行定位基准。
    // 行宽随面板宽收窄（`panel_w − 2×LIST_SIDE_PAD`；最大宽即设计稿 684）。
    let rect = egui::Rect::from_min_size(
        egui::pos2(panel_x + tk::LIST_SIDE_PAD, slot.min.y),
        egui::vec2(panel_w - 2.0 * tk::LIST_SIDE_PAD, tk::ROW_H),
    );
    let bg = if is_sel {
        t.row_selected
    } else if resp.hovered() {
        t.row_selected.gamma_multiply(0.5)
    } else {
        egui::Color32::TRANSPARENT
    };
    // 行内绘制放子 Ui：`Ui::put` 会把**父**游标回退到放置矩形（实测每行被吃 6px，
    // 直接吃穿 §2.2 的行高推导）。子 Ui 的游标独立，父游标停在 `allocate_exact_size`。
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    row_ui
        .painter()
        .rect_filled(rect, egui::CornerRadius::same(tk::ROW_RADIUS), bg);
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.min.x + tk::ROW_PAD_X,
            rect.center().y - tk::ROW_ICON / 2.0,
        ),
        egui::vec2(tk::ROW_ICON, tk::ROW_ICON),
    );
    if let Some(id) = texture {
        row_ui.painter().image(
            id,
            icon_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        // 兜底字形两形态共用（16 / 32 按矩形缩放；visual.md §2.4）。
        icons::draw_fallback(row_ui.painter(), icon_rect, t.dir_text);
    }
    let name_c = if is_sel {
        t.file_name_selected
    } else {
        t.file_name
    };
    // 文本列（左对齐；`Ui::put` 是居中布局，不能用——改用定宽子 Ui 顶部左排）
    let tx = icon_rect.max.x + tk::ROW_INNER_GAP;
    let hint_w = if is_sel { tk::ACTION_HINT_W } else { 0.0 };
    let right = rect.max.x - tk::ROW_PAD_X - hint_w - 6.0;
    let col = egui::Rect::from_min_max(
        egui::pos2(tx, rect.center().y - 18.0),
        egui::pos2(right, rect.center().y + 18.0),
    );
    let mut col_ui = row_ui.new_child(
        egui::UiBuilder::new()
            .max_rect(col)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    col_ui.spacing_mut().item_spacing.y = tk::ROW_TEXT_GAP_Y;
    col_ui.add(
        egui::Label::new(
            egui::RichText::new(&e.name)
                .size(ui_font::size(FONT_ROW))
                .color(name_c),
        )
        .truncate()
        .sense(egui::Sense::hover()),
    );
    col_ui.add(
        egui::Label::new(
            egui::RichText::new(dir_display(&e.path))
                .size(ui_font::size(FONT_DIR))
                .color(t.dir_text),
        )
        .truncate()
        .sense(egui::Sense::hover()),
    );
    if is_sel {
        // 动作提示：文件「打开」/ 目录「进入」（协议 HitEntry.is_dir 分流）。
        row_ui.painter().text(
            egui::pos2(rect.max.x - tk::ROW_PAD_X, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            action_hint(e.is_dir),
            font(FONT_META),
            t.action_hint,
        );
    }
    resp
}

/// 页脚（§2.5）：高 38 / 内边距 16；左结果计数（空词隐藏），右四组键位提示
/// （右→左：隐藏 / 完整窗口 / 访达 / 打开；窄屏按「隐藏 > 完整窗口 > 访达」裁减，
/// 至少保留「打开」）。
fn draw_footer(ui: &mut egui::Ui, has_query: bool, total: u32, t: &tk::Palette) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), tk::FOOTER_H),
        egui::Sense::hover(),
    );
    let p = ui.painter();
    p.rect_filled(rect, 0, t.footer_fill);
    let groups: [(&str, &str); 4] = [
        ("隐藏", "esc"),
        ("完整窗口", "⌥↵"),
        ("访达", "⌘↵"),
        ("打开", "↵"),
    ];
    let mut cursor = rect.max.x - tk::FOOTER_PAD_X;
    for (gi, (label, key)) in groups.iter().enumerate() {
        let kw = text_w(ui, key, FONT_KEYCAP) + 2.0 * tk::KEYCAP_PAD.1;
        let lw = text_w(ui, label, FONT_META);
        let gw = lw + tk::KEYCAP_GAP + kw;
        let last = gi == groups.len() - 1;
        // 窄屏裁减：为左端计数预留 120px；「打开」无论如何保留
        if !last && cursor - gw < rect.min.x + 120.0 {
            break;
        }
        let chip = egui::Rect::from_center_size(
            egui::pos2(cursor - kw / 2.0, rect.center().y),
            egui::vec2(kw, tk::KEYCAP_H),
        );
        p.rect_filled(
            chip,
            egui::CornerRadius::same(tk::KEYCAP_RADIUS),
            t.keycap_fill,
        );
        p.text(
            chip.center(),
            egui::Align2::CENTER_CENTER,
            *key,
            font(FONT_KEYCAP),
            t.keycap_text,
        );
        p.text(
            egui::pos2(cursor - kw - tk::KEYCAP_GAP, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            *label,
            font(FONT_META),
            t.footer_label,
        );
        cursor -= gw + tk::FOOTER_GAP;
    }
    // 左：结果计数（空词隐藏；无匹配项显示「共 0 项」）
    if has_query {
        p.text(
            egui::pos2(rect.min.x + tk::FOOTER_PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("共 {total} 项"),
            font(FONT_META),
            t.footer_label,
        );
    }
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
    fn action_hint_dir_enters_file_opens() {
        assert_eq!(action_hint(true), "进入", "目录动作提示应为「进入」");
        assert_eq!(action_hint(false), "打开", "文件动作提示应为「打开」");
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

    /// 高度过渡步进(纯逻辑;PoC-6 定案):单调不过冲、到点精确吸附(不留亚像素尾巴)、
    /// 首帧(dt=0)不动、长停帧按总时长封顶一次到位——固定 120ms 的 smoothstep。
    #[test]
    fn h_ease_monotone_no_overshoot_snaps_at_duration() {
        let dur = Duration::from_millis(H_ANIM_MS);
        // 扩张 54 → 400:逐帧单调不降、不过冲;8 帧(≈120ms@15ms)内收敛
        let mut prev = 54.0;
        let mut last = 54.0;
        for i in 1..=8 {
            let h = h_ease(54.0, 400.0, Duration::from_millis(15 * i));
            assert!(h >= prev, "单调不降:{prev} → {h}");
            assert!(h <= 400.0, "不过冲:{h}");
            prev = h;
            last = h;
        }
        assert_eq!(last, 400.0, "第 8 帧(120ms)应精确到目标");
        // 到点即目标(含超过时长:挂起恢复等长停帧一次到位)
        assert_eq!(h_ease(54.0, 400.0, dur), 400.0);
        assert_eq!(h_ease(54.0, 400.0, dur + Duration::from_secs(3)), 400.0);
        // 首帧(dt=0)不动(复位首帧不抢跳)
        assert_eq!(h_ease(54.0, 400.0, Duration::ZERO), 54.0);
        // 中点 = 几何中点(smoothstep(0.5) = 0.5;对称形状)
        let mid = h_ease(54.0, 400.0, Duration::from_millis(H_ANIM_MS / 2));
        assert!((mid - 227.0).abs() < 1e-3, "中点应为几何中点:{mid}");
        // 收缩方向同形且到点吸附
        assert!(h_ease(400.0, 79.0, Duration::from_millis(60)) < 400.0);
        assert_eq!(h_ease(400.0, 79.0, dur), 79.0);
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

    /// 面板高 → 窗口高（探针 `last_h` 口径 = 面板 + 形象条 70）。
    fn win_h(panel_h: f32) -> f32 {
        panel_h + MASCOT_STRIP
    }

    /// 造 N 条命中(只用于行数/高度,内容不参与布局高度)。
    fn hits(n: usize) -> Vec<HitEntry> {
        (0..n)
            .map(|i| HitEntry {
                name: format!("命中{i}.md"),
                path: format!("/tmp/mce-probe/Documents/子目录/命中{i}.md"),
                size: None,
                mtime: None,
                is_dir: false,
            })
            .collect()
    }

    /// 跑一帧(注入事件 → draw),返回本帧请求的窗口高度与本帧视口命令。
    fn probe_frame_with_commands(
        ctx: &egui::Context,
        st: &mut PaletteState,
        events: Vec<egui::Event>,
    ) -> (f32, Vec<egui::ViewportCommand>) {
        // 模拟 16ms 帧间隔:把进行中的过渡起点回拨一帧(时间基插值 → 一帧推进一步)。
        if let Some((from, to, t0)) = st.h_anim {
            st.h_anim = Some((from, to, t0 - PROBE_DT));
        }
        // 窗高模拟为「上一帧请求的高」(尺寸命令下一帧落地):可见高取 min 的路径
        // 与实机一致;窗高只影响裁剪,不影响本帧请求的高(测量值)。
        // `last_h` 是**窗口高**(面板 + 形象条 70);创建首帧回落到初始窗高。
        let win_h = st
            .last_h
            .unwrap_or(INITIAL_H + MASCOT_STRIP)
            .max(INITIAL_H + MASCOT_STRIP);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(st.panel_w, win_h),
            )),
            events,
            ..Default::default()
        };
        // 根 Ui 直驱(与 `ui::draw_palette` 的 deferred 视口回调同形态)。
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = st.draw(ui);
        });
        out.textures_delta.clear(); // headless:不做纹理上传,清掉免得 Drop 告警
        let commands = out
            .viewport_output
            .get(&st.viewport)
            .map(|v| v.commands.clone())
            .unwrap_or_default();
        (st.last_h.unwrap_or_default(), commands)
    }

    /// 跑一帧,只取窗口高(多数探针用)。
    fn probe_frame(ctx: &egui::Context, st: &mut PaletteState, events: Vec<egui::Event>) -> f32 {
        probe_frame_with_commands(ctx, st, events).0
    }

    /// 连续走帧到高度收敛(过渡完成 + 窗高两帧一致;上限防死循环),逐帧记入 seq。
    fn probe_settle(
        ctx: &egui::Context,
        st: &mut PaletteState,
        seq: &mut Vec<(String, String, usize, f32)>,
    ) {
        let mut prev = st.last_h;
        for _ in 0..30 {
            let h = probe_frame(ctx, st, vec![]);
            seq.push(("anim".into(), st.input.clone(), st.results.len(), h));
            if st.h_anim.is_none() && prev == st.last_h {
                return; // 过渡完成且窗高两帧一致 = 已收敛
            }
            prev = st.last_h;
        }
        panic!("高度过渡 30 帧内未收敛");
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

    /// 断言:整段单调(不来回抽),且帧数在过渡时长内(120ms @16ms ≈ 8 帧 + 收敛余量)。
    fn assert_smooth(heights: &[f32], expect_up: bool) {
        for w in heights.windows(2) {
            let (a, b) = (w[0], w[1]);
            if expect_up {
                assert!(
                    b >= a - 1e-3,
                    "扩张段必须单调不减:{a} → {b}(序列 {heights:?})"
                );
            } else {
                assert!(
                    b <= a + 1e-3,
                    "收缩段必须单调不增:{a} → {b}(序列 {heights:?})"
                );
            }
        }
        assert!(
            heights.len() <= 12,
            "过渡应在 ~8 帧(120ms/16ms)内收敛,实际 {} 帧:{heights:?}",
            heights.len()
        );
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
    /// 推到当下(打字窗口内,空态滞后生效)。
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
    /// (**行数取隔离索引实测命中数**),以 16ms 模拟帧间隔推进高度过渡,记录每帧
    /// 请求的窗口高 `last_h`。四个场景(0 命中冷启动 / 状态往返穿越 / 行数大波动 /
    /// 删字往返)逐段断言:整段单调、帧数在 120ms 内、同目标段零抽动、
    /// 收敛精确贴合自然高(176 空态 / 292 三行 / 350 四行 / 408 五行 / 466 封顶)。
    /// 突变验证:把 `h_ease` 改回逐帧原生 resize、或去掉空态滞后,本测试即红。
    #[test]
    fn typing_height_sequence_is_smooth_and_fits_content() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
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
        assert_eq!(
            st.last_h,
            Some(win_h(INITIAL_H)),
            "唤起/复位后停在输入行窗高（面板 + 形象条）"
        );

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
        assert_eq!(
            *first.last().unwrap(),
            win_h(176.0),
            "S1 首字落空态面板 176px（窗高 {} = 176 + 70）:{first:?}",
            win_h(176.0)
        );
        for ch in ["z", "q", "q", "x", "x"] {
            let (framed, resp) = type_char(&mut st, &mut seq, ch);
            assert!(
                framed
                    .iter()
                    .chain(resp.iter())
                    .all(|h| (h - win_h(176.0)).abs() < 0.5),
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
            win_h(INITIAL_H),
            "清空回输入行窗高:{clear:?}"
        );

        // ================= S2 状态往返穿越(加字链,含 0 命中坠点)=================
        // 预期:28/27/24 行都封顶同高(自然高相同 → 零变化);降到 3 行即时平滑跟随;
        // 「区域B换」0 命中打字中滞后(结果仍 3、高度不动);停手 → 平滑落到空态。
        let (_, f) = type_char(&mut st, &mut seq, "湖"); // 28 行
        assert_smooth(&f, true);
        let h_cap = *f.last().unwrap();
        assert_eq!(
            h_cap,
            win_h(466.0),
            "28 行封顶面板高 466（窗高 + 70）:{f:?}"
        );
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
            "S2 3 行即时平滑跟到 3 行自然高:{f:?}"
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
        assert_eq!(*down.last().unwrap(), win_h(176.0), "空态贴合窗高:{down:?}");
        for _ in 0..4 {
            probe_phase(&ctx, &mut st, &mut seq, "退格", backspace());
        }
        assert!(st.input.is_empty());
        let clear = probe_phase(&ctx, &mut st, &mut seq, "清空", vec![]);
        assert_eq!(
            *clear.last().unwrap(),
            win_h(INITIAL_H),
            "清空回初始窗高:{clear:?}"
        );

        // ================= S3 行数大波动(28 行封顶 ↔ 3 行)=================
        // 预期:28 → 410;降到 3 行即时平滑收拢(不推迟到停手)。
        let (_, f) = type_char(&mut st, &mut seq, "湖"); // 28
        assert_eq!(*f.last().unwrap(), h_cap, "28 行封顶");
        let (_, _) = type_char(&mut st, &mut seq, "北");
        let (_, f) = type_char(&mut st, &mut seq, "省"); // 区域B省 3 行:大波动实时跟
        assert_smooth(&f, false);
        let h3b = *f.last().unwrap();
        assert_eq!(h3b, win_h(292.0), "28→3 行即时平滑收拢到 3 行窗高:{f:?}");
        // 停手后零高度变化事件(验收口径「输入完成后无额外动作」)
        probe_settle_input(&mut st);
        let idle = probe_phase(&ctx, &mut st, &mut seq, "停手静置", vec![]);
        assert!(
            idle.iter().all(|h| (h - h3b).abs() < 0.5),
            "停手后不得再有高度动作:{idle:?}"
        );

        // ================= S3b 行数小波动(无死区:每档同样走 120ms 平滑)=================
        // 预期:6→5→4→3 行每档都平滑跟随到各自自然高,且**每档恰 1 次原生 resize**
        // (8/7/6 行同封顶 466,无高度变化,不入本段)。
        probe_refire(&mut st);
        probe_respond(&mut st, 6);
        let base = probe_phase(&ctx, &mut st, &mut seq, "合 6 行", vec![]);
        let h8 = *base.last().unwrap();
        for rows in [5usize, 4, 3] {
            probe_refire(&mut st);
            probe_respond(&mut st, rows);
            let r0 = st.h_resizes;
            let dz = probe_phase(&ctx, &mut st, &mut seq, &format!("合 {rows} 行"), vec![]);
            assert_eq!(st.results.len(), rows, "结果照常替换");
            assert_smooth(&dz, false);
            assert_eq!(
                st.h_resizes - r0,
                1,
                "每档恰 1 次原生 resize({rows} 行):{dz:?}"
            );
            assert!(
                dz.iter().all(|h| *h <= h8 + 0.5),
                "收缩方向单调不增({rows} 行):{dz:?}"
            );
        }
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
        assert_eq!(
            *clear.last().unwrap(),
            win_h(INITIAL_H),
            "清空回初始窗高:{clear:?}"
        );

        println!("—— 验收矩阵小结(预期 vs 实测;命中数取自隔离索引实测;窗高 = 面板 + 70)——");
        println!(
            "S1 zzqqxx(0 命中,冷启动)      预期 空态立即落定 面板176/窗高{:.0} 实测 {:.0}",
            win_h(176.0),
            win_h(176.0)
        );
        println!("S2 湖28→区域B27→区域B报24→区域B省3 预期 封顶段零变化/3 行即时收拢   实测 {h_cap:.0}/{h_cap:.0}/{h_cap:.0}/{h_3:.0}");
        println!(
            "S2 区域B省换(0 命中,打字中)   预期 结果仍 3、高不动 → 停手落 面板176 实测 {:.0}",
            win_h(176.0)
        );
        println!("S3 湖28→区域B省3(大波动)     预期 即时平滑收拢(120ms)         实测 {h_3:.1}px");
        println!(
            "S4 区域B省3→(删)区域B27→湖28    预期 实时平滑扩张回封顶           实测 {h_cap:.0}px"
        );
        println!(
            "S4 全删(空输入)             预期 落空态面板高 176/窗高{:.0}   实测 {:.0}",
            win_h(INITIAL_H),
            win_h(INITIAL_H)
        );
        println!("—— 逐帧高度序列(输入=当前词 · 结果=行数 · 窗高) ——");
        for (tag, input, rows, h) in &seq {
            println!("{tag:<26} 输入={input:<10} 结果={rows:<3} 窗高={h:.0}px");
        }
    }

    /// 钉住：行数 → 面板高映射（visual.md §2.2 高度推导：0/1 行 176（72 空态区），
    /// 2 行起每多一行 +58，6 行封顶 466）。
    #[test]
    fn panel_height_per_row_count() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        let mut seq: Vec<(String, String, usize, f32)> = Vec::new();
        for _ in 0..2 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        // 空态（空词）：三段 = 64 + 1 + 72 + 1 + 38 = 176，**精确**（非兜底值）
        assert_eq!(
            st.natural_h, 176.0,
            "空态自然内容高应精确等于设计推导（页脚须贴底）"
        );
        probe_phase(&ctx, &mut st, &mut seq, "打字", vec![probe_commit("湖")]);
        let mut got: Vec<(usize, f32)> = Vec::new();
        for rows in [1usize, 2, 3, 4, 5, 6, 8] {
            probe_refire(&mut st);
            probe_respond(&mut st, rows);
            let f = probe_phase(&ctx, &mut st, &mut seq, &format!("合 {rows} 行"), vec![]);
            got.push((rows, *f.last().unwrap()));
        }
        for (rows, h) in &got {
            println!("rows={rows} → 面板高 {h}");
        }
        for (rows, h) in got {
            let expect = win_h(tk::panel_h(tk::list_h(rows.min(6))));
            assert_eq!(
                h, expect,
                "{rows} 行窗高（面板 + 形象条）不符 §2.2 推导（实测 {h}）"
            );
        }
        // 自然内容高 = 三段之和（最后一态：8 行封顶 466）
        assert_eq!(st.natural_h, 466.0, "末态自然内容高应为封顶 466");
    }

    /// 形象锚点(Task 9):面板顶边恒在窗口内 `MASCOT_STRIP`(70)处——形象条固定
    /// 在窗口顶部;面板高度变化(空态 176 → 5 行 350)不改变该锚点。
    /// 突变验证:去掉 `ui.add_space(MASCOT_STRIP)`,顶边变 0 即红。
    #[test]
    fn panel_top_stays_at_strip_across_height_changes() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        let mut seq: Vec<(String, String, usize, f32)> = Vec::new();
        for _ in 0..2 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        let empty_top = st.search_probe.expect("搜索行探针已记录").panel_top;
        assert_eq!(empty_top, MASCOT_STRIP, "空态面板顶边 = 形象条高");
        probe_phase(&ctx, &mut st, &mut seq, "打字", vec![probe_commit("湖")]);
        probe_respond(&mut st, 5);
        probe_phase(&ctx, &mut st, &mut seq, "响应 5 行", vec![]);
        assert!(st.natural_h > 176.0, "面板已增高");
        let tall_top = st.search_probe.expect("搜索行探针已记录").panel_top;
        assert_eq!(tall_top, MASCOT_STRIP, "面板增高后顶边不动（形象锚点不动）");
    }

    /// 宽度回填 clamp(平台层已按屏计算;UI 侧防御到 [480, PANEL_W])。
    #[test]
    fn set_panel_width_clamps_to_bounds() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx,
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        assert_eq!(st.panel_w, PANEL_W, "默认取最大宽");
        st.set_panel_width(480.0);
        assert_eq!(st.panel_w, 480.0);
        st.set_panel_width(700.0);
        assert_eq!(st.panel_w, 700.0);
        st.set_panel_width(300.0);
        assert_eq!(st.panel_w, 480.0, "低于下限按 480");
        st.set_panel_width(900.0);
        assert_eq!(st.panel_w, 700.0, "高于上限按 700");
    }

    /// 窄宽(480)headless 绘制:不 panic,自然高与最大宽一致(高度链与宽度解耦)。
    #[test]
    fn narrow_panel_width_keeps_natural_height() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        st.set_panel_width(480.0);
        for _ in 0..2 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        assert_eq!(st.panel_w, PANEL_MIN_W);
        assert_eq!(st.natural_h, 176.0, "窄宽空态自然高仍为设计推导 176");
        assert_eq!(
            st.last_h,
            Some(win_h(INITIAL_H)),
            "窗高下发不变（面板 + 形象条）"
        );
    }

    /// 角标可用宽随面板宽收窄:480 下 142,700 下取 240 上限,极窄不为负。
    #[test]
    fn search_row_badge_max_w_follows_panel_width() {
        assert_eq!(SEARCH_ROW_FIXED_W, 338.0, "固定占用须按布局实算");
        assert_eq!(search_row_badge_max_w(480.0), 142.0);
        assert_eq!(search_row_badge_max_w(700.0), BADGE_MAX_W);
        assert_eq!(search_row_badge_max_w(SEARCH_ROW_FIXED_W), 0.0);
        assert_eq!(search_row_badge_max_w(300.0), 0.0, "余量不足不为负");
    }

    /// 480 窄宽 + 长错误文案:角标按可用宽截断,输入框与搜索图标左缘不越界。
    #[test]
    fn narrow_panel_long_badge_keeps_search_row_inside() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        st.set_panel_width(480.0);
        for _ in 0..2 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        st.status_note = Some(Badge::error(BadgeText::Open(OpenError::Failed(
            "文件路径不存在或已被移动".repeat(8),
        ))));
        probe_frame(&ctx, &mut st, vec![]);
        let probe = st.search_probe.expect("搜索行布局探针应已记录");
        let max_chip_w = search_row_badge_max_w(PANEL_MIN_W);
        assert!(
            probe.badge_w > 0.0 && probe.badge_w <= max_chip_w + 0.01,
            "chip 宽 {} 超出可用宽 {max_chip_w}",
            probe.badge_w
        );
        let left_pad = probe.panel_left + tk::SEARCH_PAD_X;
        assert!(
            probe.icon_left >= left_pad - 0.01,
            "搜索图标左缘 {} 越出左内边距 {left_pad}",
            probe.icon_left
        );
        assert!(
            probe.input_left >= probe.icon_left + tk::SEARCH_ICON + tk::SEARCH_GAP - 0.01,
            "输入框左缘 {} 压住搜索图标(左缘 {})",
            probe.input_left,
            probe.icon_left
        );
    }

    /// 真实布局避让（700 / 480，长错误角标 = 最紧场景）：用探针实测的按钮矩形与
    /// 角标/输入右缘，断言形象矩形净空 ≥12；四动作极值的旋转包围盒不与两者相交。
    /// 突变验证：把 `MASCOT_CLEAR_W` 或形象右缘距改小，本测试即红。
    #[test]
    fn mascot_clears_real_layout_and_rotated_bounds_at_700_and_480() {
        for panel_w in [PANEL_W, PANEL_MIN_W] {
            let ctx = egui::Context::default();
            let mut st = PaletteState::new_for_test(
                ctx.clone(),
                DaemonPaths {
                    data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
                },
                egui::ViewportId::ROOT,
                None,
            );
            st.set_panel_width(panel_w);
            for _ in 0..2 {
                probe_frame(&ctx, &mut st, vec![]);
            }
            // 超长错误文案：角标占满可用宽（搜索行最紧）。
            st.status_note = Some(Badge::error(BadgeText::Open(OpenError::Failed(
                "文件路径不存在或已被移动".repeat(8),
            ))));
            probe_frame(&ctx, &mut st, vec![]);
            let p = st.search_probe.expect("搜索行探针应已记录");
            assert!(p.content_right >= p.input_right - 0.01);
            let origin = egui::pos2(p.panel_left, p.panel_top);
            let r = mascot::mascot_rect(origin, st.panel_w, 0.0);
            assert!(
                r.left() - p.content_right >= tk::MASCOT_BADGE_GAP - 0.01,
                "宽 {panel_w}:形象左缘 {} 与内容右缘 {} 净空不足 12",
                r.left(),
                p.content_right
            );
            assert!(
                p.button_rect.left() - r.right() >= tk::MASCOT_BADGE_GAP - 0.01,
                "宽 {panel_w}:形象右缘 {} 与按钮左缘 {} 净空不足 12",
                r.right(),
                p.button_rect.left()
            );
            for pose in [
                mascot::pose_for(&State::Idle, false, 1.8),     // 呼吸最高
                mascot::pose_for(&State::Searching, true, 0.0), // 前倾 2.5°
                mascot::pose_for(&State::EngineStarting, false, 0.4), // +3°
                mascot::pose_for(&State::EngineStarting, false, 1.2), // -3°
            ] {
                let bbox = mascot::rotated_bounds(
                    mascot::mascot_rect(origin, st.panel_w, pose.dy),
                    pose.angle_deg,
                );
                assert!(
                    !bbox.intersects(p.button_rect),
                    "宽 {panel_w}:姿态 {pose:?} 的旋转包围盒 {} 与按钮 {} 相交",
                    bbox,
                    p.button_rect
                );
                assert!(
                    bbox.left() > p.content_right,
                    "宽 {panel_w}:姿态 {pose:?} 的旋转包围盒左缘 {} 压住内容右缘 {}",
                    bbox.left(),
                    p.content_right
                );
            }
        }
    }

    /// 宽度回归网:set_panel_width(480) 后 reset 与高度变化下发的视口命令都是
    /// InnerSize(480, …)。突变复核:把 `send_h` 宽度改回 `PANEL_W` 即红。
    #[test]
    fn narrow_width_survives_reset_and_height_change_commands() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        for _ in 0..2 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        st.set_panel_width(480.0);
        st.reset();
        let (_, commands) = probe_frame_with_commands(&ctx, &mut st, vec![]);
        assert!(
            commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(
                480.0,
                win_h(INITIAL_H)
            ))),
            "reset 后首帧须按 480 宽下发:{commands:?}"
        );
        // 打字 → 注入 5 行响应 → 高度变化帧。
        probe_frame(&ctx, &mut st, vec![probe_commit("湖")]);
        probe_respond(&mut st, 5);
        let (h, commands) = probe_frame_with_commands(&ctx, &mut st, vec![]);
        let expect_h = win_h(tk::panel_h(tk::list_h(5)));
        assert_eq!(h, expect_h, "5 行自然高");
        assert!(
            commands.contains(&egui::ViewportCommand::InnerSize(egui::vec2(
                480.0, expect_h
            ))),
            "高度变化后仍须以 480 宽下发:{commands:?}"
        );
    }

    /// 窗口高策略(PoC-6 定案):增长 = 首帧即撑到目标(恰 1 次原生 resize);
    /// 收缩 = 过渡期窗高不动、收敛帧收拢(恰 1 次)——过渡期零逐帧尺寸命令。
    /// 突变验证:把窗口高改回逐帧发(旧实现),本测试即红。
    #[test]
    fn window_height_policy_one_resize_per_change() {
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        let mut seq: Vec<(String, String, usize, f32)> = Vec::new();
        for _ in 0..2 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        assert_eq!(
            st.last_h,
            Some(win_h(INITIAL_H)),
            "唤起/复位后停在输入行窗高（面板 + 形象条）"
        );

        // —— 增长:打字 → 注入 5 行响应 ——
        probe_phase(&ctx, &mut st, &mut seq, "打字", vec![probe_commit("湖")]);
        let r0 = st.h_resizes;
        probe_respond(&mut st, 5);
        let grow = probe_phase(&ctx, &mut st, &mut seq, "响应 5 行", vec![]);
        let h5 = *grow.last().unwrap();
        assert!(
            h5 > win_h(INITIAL_H) + 1.0,
            "5 行窗高应高于输入行窗高:{grow:?}"
        );
        assert_eq!(st.h_resizes - r0, 1, "增长恰 1 次原生 resize:{grow:?}");
        assert!(
            grow.iter().all(|h| (h - h5).abs() < 0.5),
            "增长首帧即撑到目标(窗口先撑,不逐帧爬):{grow:?}"
        );
        assert!(
            (h5 - st.natural_h - MASCOT_STRIP).abs() < 0.5,
            "窗口高恒为面板 + 形象条（面板高度变化时形象锚点不动）:h5={h5} natural={}",
            st.natural_h
        );
        // 收敛帧的绘制高是「步进前」的 anim_h（动画步进在布局之后），补一帧让
        // anim_h 吸附目标；垫窗输入应等于该帧实际可见面板高。
        probe_frame(&ctx, &mut st, vec![]);
        assert!(
            (st.visible_panel_h() - st.natural_h).abs() < 0.5,
            "收敛后垫窗输入 = 可见面板高（自然高）:visible={} natural={}",
            st.visible_panel_h(),
            st.natural_h
        );

        // —— 收缩:退格清空(清空即清结果,product §1)→ 过渡期窗高不动、收敛帧收拢 ——
        let backspace = || {
            vec![egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        };
        let r1 = st.h_resizes;
        let shrink = probe_phase(&ctx, &mut st, &mut seq, "退格清空", backspace());
        assert!(st.input.is_empty(), "退格清空");
        assert_eq!(st.h_resizes - r1, 1, "收缩恰 1 次原生 resize:{shrink:?}");
        assert_eq!(
            *shrink.last().unwrap(),
            win_h(INITIAL_H),
            "收拢回输入行窗高"
        );
        probe_frame(&ctx, &mut st, vec![]);
        assert!(
            (st.visible_panel_h() - st.natural_h).abs() < 0.5,
            "收缩收敛后垫窗输入 = 可见面板高（自然高）:visible={} natural={}",
            st.visible_panel_h(),
            st.natural_h
        );
        let changes = shrink
            .windows(2)
            .filter(|w| (w[0] - w[1]).abs() > 0.5)
            .count();
        assert_eq!(changes, 1, "收缩全程序列应只有 1 次窗高变化:{shrink:?}");
        assert!(
            (shrink[0] - h5).abs() < 0.5,
            "过渡期窗高保持旧值(不逐帧发尺寸命令):{shrink:?}"
        );
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
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let eng = |s: &str| EngineStatus {
                state: s.into(),
                entry_count: 12,
                gaps: Vec::new(),
                built_at: 0,
            };
            // 角标形态 = (文案, 强调度)；ready / 空态无角标。
            let note = |state: &State, engine: Option<&EngineStatus>| {
                badge_note(state, engine).map(|b| (b.text.text(), b.tone))
            };
            let some = |text: &str, tone: BadgeTone| Some((text.to_string(), tone));
            // —— 引擎六态（引擎优先，会话态无关）——
            assert_eq!(
                note(&State::Searching, Some(&eng("building"))),
                some("建索引中 12", BadgeTone::Neutral)
            );
            assert_eq!(
                note(&State::Searching, Some(&eng("ready"))),
                None,
                "就绪不占位"
            );
            // 就绪时非错误的会话态角标照样被抑制（证明 ready 分支起效）。
            assert_eq!(
                note(&State::EngineStarting, Some(&eng("ready"))),
                None,
                "就绪时不得显示会话态角标"
            );
            // 就绪但查询报错：错误不得被 ready 吞掉。
            assert_eq!(
                note(&State::Error("query".into()), Some(&eng("ready"))),
                some("查询失败", BadgeTone::Error)
            );
            assert_eq!(
                note(&State::Searching, Some(&eng("reconciling"))),
                some("补齐中", BadgeTone::Warning)
            );
            assert_eq!(
                note(&State::Searching, Some(&eng("degraded"))),
                some("重扫中", BadgeTone::Warning)
            );
            assert_eq!(
                note(&State::Searching, Some(&eng("offline"))),
                some("离线索引", BadgeTone::Warning)
            );
            // —— 引擎态取不到（或未知）→ 会话态兜底 ——
            assert_eq!(
                note(&State::EngineStarting, None),
                some("引擎未运行", BadgeTone::Error)
            );
            assert_eq!(
                note(&State::Warming, None),
                some("索引构建中", BadgeTone::Neutral)
            );
            assert_eq!(
                note(&State::Offline, None),
                some("离线索引", BadgeTone::Warning)
            );
            assert_eq!(
                note(&State::Error("query".into()), None),
                some("查询失败", BadgeTone::Error)
            );
            assert_eq!(note(&State::Idle, None), None);
            assert_eq!(note(&State::Searching, None), None);
            assert_eq!(note(&State::Empty, None), None);
            // 未知引擎态落到会话态（不误报 ready）。
            assert_eq!(
                note(&State::Warming, Some(&eng("unknown"))),
                some("索引构建中", BadgeTone::Neutral)
            );
        });
    }

    /// 英文角标:各状态文案逐条钉住(含会话态兜底),占位符不得残留。
    #[test]
    fn badge_note_is_localized_in_english() {
        i18n::with_lang(i18n::Lang::En, || {
            let eng = |s: &str| EngineStatus {
                state: s.into(),
                entry_count: 12,
                gaps: Vec::new(),
                built_at: 0,
            };
            let note = |state: &State, engine: Option<&EngineStatus>| {
                badge_note(state, engine).map(|b| b.text.text())
            };
            let cases = [
                (
                    &State::Searching,
                    Some(eng("building")),
                    Some("Building index 12"),
                ),
                (
                    &State::Searching,
                    Some(eng("reconciling")),
                    Some("Reconciling"),
                ),
                (&State::Searching, Some(eng("degraded")), Some("Rescanning")),
                (
                    &State::Searching,
                    Some(eng("offline")),
                    Some("Offline index"),
                ),
                (&State::EngineStarting, None, Some("Engine not running")),
                (&State::Warming, None, Some("Building index")),
                (&State::Offline, None, Some("Offline index")),
                (&State::Error("query".into()), None, Some("Query failed")),
            ];
            for (state, engine, expected) in &cases {
                let got = note(state, engine.as_ref());
                assert_eq!(got.as_deref(), *expected, "英文角标不符: {state:?}");
                if let Some(text) = &got {
                    assert!(!text.contains('{'), "未替换占位符: {text}");
                }
            }
        });
    }

    /// 空态两屏中英文案(悬浮条绘制与测试共用 `empty_text`)。
    #[test]
    fn empty_state_text_is_localized() {
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(
                empty_text(&State::Empty),
                ("No matches", "Try a different keyword")
            );
            assert_eq!(
                empty_text(&State::Idle),
                ("Type to search", "Search file names or paths")
            );
        });
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(empty_text(&State::Empty), ("无匹配项", "换个关键词试试"));
            assert_eq!(
                empty_text(&State::Idle),
                ("输入关键词开始搜索", "可搜文件名，也可搜路径")
            );
        });
    }

    #[test]
    fn full_window_tooltip_is_localized() {
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(i18n::s().open_in_full_window, "Open in Full Window (⌥↵)");
        });
    }

    /// 打开失败文案随语言切换；缺失路径直接产出 `FileMissing`（不 spawn `open`）。
    #[test]
    fn open_error_text_is_localized() {
        i18n::with_lang(i18n::Lang::En, || {
            assert_eq!(
                OpenError::FileMissing.text(),
                "File not found; the index may be out of date"
            );
            assert_eq!(
                OpenError::Failed("boom".into()).text(),
                "Failed to open: boom"
            );
            assert_eq!(
                BadgeText::Open(OpenError::FileMissing).text(),
                "File not found; the index may be out of date"
            );
        });
        i18n::with_lang(i18n::Lang::ZhHans, || {
            assert_eq!(OpenError::FileMissing.text(), "文件不存在,索引可能待更新");
            assert_eq!(OpenError::Failed("boom".into()).text(), "打开失败: boom");
            assert_eq!(
                BadgeText::Open(OpenError::FileMissing).text(),
                "文件不存在,索引可能待更新"
            );
        });
        assert_eq!(
            open_path("/definitely/not/here/mce-task6", false),
            Err(OpenError::FileMissing)
        );
    }

    /// 超长角标文案按实测宽截断：chip 宽 ≤ 上限且以「…」结尾；短文本原样。
    #[test]
    fn badge_text_fit_caps_chip_width() {
        let ctx = egui::Context::default();
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(PANEL_W, 200.0),
            )),
            ..Default::default()
        };
        let max_chip_w = search_row_badge_max_w(PANEL_W);
        let text_max_w = max_chip_w - 2.0 * tk::KEYCAP_PAD.1;
        let mut out = ctx.run_ui(raw, |ui| {
            let long = format!("打开失败:{}", "文件路径不存在或已被移动".repeat(8));
            let fitted = badge_text_fit(ui, &long, text_max_w);
            assert!(fitted.ends_with('…'), "超长文本应截断并补省略号:{fitted}");
            let chip_w = text_w(ui, &fitted, FONT_KEYCAP) + 2.0 * tk::KEYCAP_PAD.1;
            assert!(
                chip_w <= max_chip_w,
                "chip 宽 {chip_w} 超过上限 {max_chip_w}"
            );
            // 短文本不截断。
            assert_eq!(badge_text_fit(ui, "补齐中", text_max_w), "补齐中");
        });
        out.textures_delta.clear(); // headless：不做纹理上传，清掉免得 Drop 告警
    }

    /// 行点击命中:点击视觉行中心应选中该行(不偏上/偏下)。
    /// 面板顶 = 形象条 70；结果区首行 y = 70 + 搜索行 64 + 分隔线 1 + LIST_PAD_V 8。
    #[test]
    fn result_row_click_hits_its_visual_row() {
        use egui::{pos2, Event, Modifiers, PointerButton};
        let ctx = egui::Context::default();
        let mut st = PaletteState::new_for_test(
            ctx.clone(),
            DaemonPaths {
                data_dir: std::path::PathBuf::from("/tmp/mce-probe"),
            },
            egui::ViewportId::ROOT,
            None,
        );
        st.results = hits(3);
        st.total = 3;
        st.visible_rows = 3;
        // 走几帧让高度过渡收敛（点击时窗口/裁剪不再截断行）。
        for _ in 0..12 {
            probe_frame(&ctx, &mut st, vec![]);
        }
        let first_row_y = MASCOT_STRIP + tk::SEARCH_H + tk::DIVIDER_H + tk::LIST_PAD_V;
        let click = |st: &mut PaletteState, pos: egui::Pos2| {
            let mods = Modifiers::default();
            probe_frame(&ctx, st, vec![Event::PointerMoved(pos)]);
            probe_frame(
                &ctx,
                st,
                vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: mods,
                }],
            );
            probe_frame(
                &ctx,
                st,
                vec![Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: mods,
                }],
            );
        };
        for (row, x, dy) in [
            (1, PANEL_W / 2.0, tk::ROW_H / 2.0),
            (0, 120.0, tk::ROW_H / 2.0),
            (2, 650.0, tk::ROW_H / 2.0),
            (1, 300.0, 1.0),
            (1, 300.0, tk::ROW_H - 1.0),
        ] {
            st.selected = usize::MAX; // 清空选中，确保本次点击确实命中
            let start = first_row_y + row as f32 * (tk::ROW_H + tk::ROW_GAP);
            click(&mut st, pos2(x, start + dy));
            assert_eq!(
                st.selected, row,
                "点击第 {row} 行（x={x}, 行内 y={dy}）应选中该行"
            );
        }
    }
}

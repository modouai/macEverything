//! 跳转状态机(纯逻辑,design §3.4)及其平台执行层(Task 11)。
//! 步骤由「观测事件」驱动转移,产出「动作」;执行层把每个动作落实到
//! AX 直设/合成键/剪贴板。Go-to 框判定用「基线 + 焦点」双重复核
//! ([`is_goto_candidate`]/[`in_baseline`]);除 ⌘⇧G(此时尚无输入框)外,
//! 合成键前均自检 Go-to 框存在(消失则回报 [`Obs::SheetClosed`])。
//! 状态机部分不碰平台 API(单测全覆盖);执行层判定逻辑抽为纯函数
//! (`is_goto_candidate`/`in_baseline`/`fill_verdict`/`readback_until_match`/
//! `wait_until`/`outcome_of`)单独测试。
//!
//! post 目标(2026-11-05 实机结论):`CGEventPostToPid` 对保存面板不可靠
//! (事件未被宿主处理,⌘⇧G 3s 内无反应);统一走全局 HID post——每次合成前
//! 已用 `pid_still_front` 复核前台仍是宿主 app,投递目标正确。

use std::time::{Duration, Instant};

use super::ax_ffi::{self, attr, AXRef, CFStr};
use super::cg_events::{self, FLAG_CMD, FLAG_SHIFT, KEY_G, KEY_RETURN, KEY_V};
use super::{clipboard_restore, clipboard_set_text, clipboard_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// 尚未开始。
    Idle,
    /// ⌘⇧G 已发,等 sheet 出现。
    AwaitSheet,
    /// 填路径阶段(策略 A 直设,失败降级 B)。
    Fill,
    /// 确认阶段(已就位,等待 sheet 关闭/超时)。
    Confirm,
    Done,
    /// 用户中途干预(输入框消失)或超时:中止。
    Aborted,
    /// 策略 B 也失败:路径保留在剪贴板(不执行 RestoreClipboard),终态。
    /// 执行层据此映射 `NavigateOutcome::FellBackToClipboard`。
    FellBack,
}

/// 执行层回报的观测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obs {
    /// Go-to sheet 的路径输入框当前在 AX 树中。
    SheetOpened,
    /// 输入框消失(用户 Esc 关闭 sheet / 跳转完成)。
    SheetClosed,
    /// 策略 A 失败(直设/读回校验失败),降级策略 B。
    StrategyAFailed,
    /// 全流程超时。
    Timeout,
}

/// 状态机产出的下一步动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// 发 ⌘⇧G。
    SendGoToFolder,
    /// 策略 A:AX 直设 value(读回校验);提交由状态机统一发出的
    /// [`Act::PressReturn`] 完成。
    FillViaAx,
    /// 策略 B:剪贴板 set + ⌘V。
    PasteViaClipboard,
    /// 合成 Return(策略 A/B 统一的提交动作)。
    PressReturn,
    /// 恢复剪贴板(策略 B 参与时的收尾;Done/Aborted 均执行)。
    RestoreClipboard,
}

/// 状态机:策略 A 优先,A 失败当场降级 B。
pub struct NavMachine {
    step: Step,
    /// 策略 B 降级后为 true(收尾要恢复剪贴板;FellBack 例外,保留不恢复)。
    used_clipboard: bool,
}

impl NavMachine {
    pub fn new() -> Self {
        Self {
            step: Step::Idle,
            used_clipboard: false,
        }
    }

    /// 转移:返回 (下一步动作, 是否结束)。
    ///
    /// 执行契约:返回 `(Some(act), done)` 时,执行层**先执行 `act`,再按 `done`
    /// 决定是否停止**——`done = true` 且带 [`Act::RestoreClipboard`] 时该动作
    /// 必须执行。终态(Done/Aborted/FellBack)上任何再入都返回 `(None, true)`。
    pub fn on(&mut self, o: Obs) -> (Option<Act>, bool) {
        match (self.step, o) {
            // 终态可重复调用且总终止。
            (Step::Done | Step::Aborted | Step::FellBack, _) => (None, true),
            (Step::AwaitSheet, Obs::SheetOpened) => {
                self.step = Step::Fill;
                (Some(Act::FillViaAx), false)
            }
            (Step::AwaitSheet, Obs::Timeout | Obs::SheetClosed) => {
                self.step = Step::Aborted;
                (None, true)
            }
            // 策略 A 失败:首次降级策略 B;B 也失败则保留路径在剪贴板,终态。
            (Step::Fill, Obs::StrategyAFailed) => {
                if self.used_clipboard {
                    self.step = Step::FellBack;
                    (None, true)
                } else {
                    self.used_clipboard = true;
                    (Some(Act::PasteViaClipboard), false)
                }
            }
            // Fill 动作成功后执行层回报 SheetOpened(输入框仍在且已写入)推进 Confirm,
            // 提交统一由 PressReturn 完成。
            (Step::Fill, Obs::SheetOpened) => {
                self.step = Step::Confirm;
                (Some(Act::PressReturn), false)
            }
            (Step::Fill, Obs::SheetClosed | Obs::Timeout) => {
                self.step = Step::Aborted;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            (Step::Confirm, Obs::SheetClosed) => {
                self.step = Step::Done;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            // 确认阶段策略 A 失败:当场降级策略 B,回到 Fill 重新填写。
            (Step::Confirm, Obs::StrategyAFailed) => {
                self.used_clipboard = true;
                self.step = Step::Fill;
                (Some(Act::PasteViaClipboard), false)
            }
            // 确认阶段超时:剪贴板已参与 → FellBack(路径留在剪贴板,对应
            // design §5「路径已复制」出口);否则静默中止。
            (Step::Confirm, Obs::Timeout) => {
                if self.used_clipboard {
                    self.step = Step::FellBack;
                } else {
                    self.step = Step::Aborted;
                }
                (None, true)
            }
            _ => (None, false), // 未定义转移:保持原状(仅进入已知转移前的瞬时组合)
        }
    }

    /// 入口:显式开始,是发出动作的唯一入口(Idle 上的任何观测都不触发动作)。
    /// 返回 ⌘⇧G 动作;重复调用无副作用(已非 Idle 时返回 None)。
    pub fn start(&mut self) -> Option<Act> {
        if self.step != Step::Idle {
            return None;
        }
        self.step = Step::AwaitSheet;
        Some(Act::SendGoToFolder)
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// 策略 B 是否已参与(状态机单测读取;生产执行层按 `saved` 决定是否恢复
    /// 剪贴板,不读本方法)。
    #[allow(dead_code)] // 仅测试消费(执行层用 saved 判据)
    pub fn used_clipboard(&self) -> bool {
        self.used_clipboard
    }
}

/// 跳转结果(浮层接线层据此收尾;design §5 错误出口)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigateOutcome {
    /// 成功提交,面板已跳转。
    Done,
    /// 策略 B 也失败:路径保留在剪贴板,浮层显示手动粘贴提示。
    FellBackToClipboard,
    /// 中止(用户 Esc/超时/前台切换)。
    Aborted,
}

// ---------------------------------------------------------------------------
// 执行层:状态机动作 → AX 观测/合成键/剪贴板(后台线程调用;AX CF 层线程无关)

/// 单步等待上限(sheet 出现 / 提交后 sheet 关闭)。
const STEP_TIMEOUT: Duration = Duration::from_secs(3);
/// 轮询步进。
const POLL_STEP: Duration = Duration::from_millis(50);
/// 策略 B 读回校验的重试次数(初次之外最多再等 2 次)。
const PASTE_READBACK_RETRIES: u32 = 2;

/// Go-to 输入框判定(纯逻辑,三条件合取):
/// - `writable`:元素是可写 AXTextField;
/// - `in_baseline`:元素在 ⌘⇧G 前的基线集合中(保存面板文件名框等);
/// - `focused`:元素就是当前 `AXFocusedElement`。
///
/// 必须「可写 且 非基线 且 焦点」,三者任一不满足都不是 Go-to 框——
/// 防 ⌘⇧G 未生效时误中文件名框(覆盖文件名 + 意外保存)。
fn is_goto_candidate(writable: bool, in_baseline: bool, focused: bool) -> bool {
    writable && !in_baseline && focused
}

/// 候选是否落在基线集合(身份比较由调用方提供;AX 侧用 `cf_equal`,
/// 代理重建安全)。抽成泛型便于单测「候选 ∈ 基线 → 拒绝」。
fn in_baseline<T>(candidate: &T, baseline: &[T], eq: impl Fn(&T, &T) -> bool) -> bool {
    baseline.iter().any(|b| eq(b, candidate))
}

/// 当前 Go-to 输入框。
///
/// 快路径:取 `AXFocusedWindow` → 读窗口级 `AXFocusedElement` → 直接判
/// 「可写 且 非基线」(该元素本身就是焦点,三条件中的 `focused` 恒真)。
/// 每次判定只 2~3 次 AX IPC,避免深度 6 整树遍历在 50ms 轮询下的冗余开销。
/// 回退(防御):个别应用不暴露窗口级 `AXFocusedElement` 时才整树搜索,
/// 并要求「可写 + 非基线 + 与应用级焦点 CFEqual」三条件。
///
/// 注:探针 `locate_goto_field` 另有「焦点优先快路径 + FocusedStale 诊断」,
/// 执行层刻意不同构(不产出诊断);共享化留待 Task 15 收口。
/// 应用的全部候选窗口:焦点窗口优先 + AXWindows 全列表(重复无害)。
/// 实机(2026-11-05):Go-to sheet 弹出后输入框不在 `AXFocusedWindow` 的树里,
/// 而作为独立窗口出现在 `AXWindows` 中——只看焦点窗口会 3s 超时。
fn candidate_windows(app: &AXRef) -> Vec<AXRef> {
    let mut wins: Vec<AXRef> = Vec::new();
    if let Some(f) = ax_ffi::copy_attr(app, attr::FOCUSED_WINDOW) {
        wins.push(f);
    }
    if let Some(mut ws) = ax_ffi::copy_attr(app, attr::WINDOWS)
        .as_ref()
        .and_then(ax_ffi::array_elements)
    {
        wins.append(&mut ws);
    }
    wins
}

/// 找到宿主的保存面板窗口(与 detector 共用同一判定器,避免两套特征表)。
fn find_save_panel_window(app: &AXRef) -> Option<AXRef> {
    candidate_windows(app).into_iter().find(|win| {
        super::panel_detector::is_save_panel(&super::panel_detector::snapshot_of_window(win))
    })
}

/// 把保存面板重新设回宿主的 main/key 窗口并等待生效。
///
/// 点击非 key 的浮层会让本进程成为前台(见 `notify_frontmost` 注释);
/// `activate_app` 只把宿主 app 拉到前台,键盘焦点不一定回到保存面板 sheet
/// (实机 2026-11-06:激活成功但 ⌘⇧G 仍落空)。逐项尝试 AXRaise / AXMain /
/// AXFocused / app.AXFocusedWindow,再轮询 AXFocusedWindow 确认。
fn focus_save_panel(pid: i32) -> bool {
    let Some(app) = ax_ffi::app_element(pid) else {
        return false;
    };
    let Some(panel) = find_save_panel_window(&app) else {
        log::warn!("[navigator] 未找到保存面板窗口,无法恢复焦点 pid={pid}");
        return false;
    };
    let raised = ax_ffi::perform_action(&panel, "AXRaise");
    let main = ax_ffi::set_attr_bool(&panel, attr::MAIN);
    let focused = ax_ffi::set_attr_bool(&panel, attr::FOCUSED);
    let app_focus = ax_ffi::set_attr_element(&app, attr::FOCUSED_WINDOW, &panel);
    log::info!(
        "[navigator] 恢复面板焦点:raise={raised} main={main:?} focused={focused:?} app_focused_window={app_focus:?}"
    );
    let deadline = Instant::now() + Duration::from_millis(300);
    let confirmed = wait_until(deadline, || {
        ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW).is_some_and(|w| ax_ffi::cf_equal(&w, &panel))
    });
    log::info!("[navigator] 面板焦点确认 focused_window_is_panel={confirmed}");
    // 只认「确认读回焦点窗口 == 保存面板」:任一 setter 成功不代表焦点已转移,
    // 盲目发 ⌘⇧G 会落进宿主其它窗口(评审 2026-11-06)。
    confirmed
}

/// 交还键盘后的面板焦点恢复(design §10):与跳转前恢复同一实现(AXRaise/
/// AXMain/AXFocused/AXFocusedWindow + 读回确认)。阻塞;调用方在后台线程执行。
/// 失败 = false,调用方仅留痕——不发合成键、不碰跳转/屏蔽状态,用户再次
/// 点击面板即可(焦点恢复失败不构成任何功能性出口,见设计 §10.2)。
///
/// 串行化不变量(评审 #14):与 [`run_navigate`] 结构性互斥——恢复仅当浮层
/// 持 key 且可见时触发,而跳转自 commit 起浮层已 `visible=false` 并 orderOut,
/// 两条路径不可能并发;接线层的去重闸进一步保证同路径不并发自竞争。
pub fn restore_panel_focus(pid: i32) -> bool {
    focus_save_panel(pid)
}

fn goto_field(pid: i32, baseline: &super::PanelBaseline) -> Option<AXRef> {
    let app = ax_ffi::app_element(pid)?;
    let focused = ax_ffi::copy_attr(&app, attr::FOCUSED_ELEMENT);
    for win in candidate_windows(&app) {
        // Go-to sheet 必出现在基线之后的新窗口;基线窗口(保存面板自身)内的
        // 输入框一律不是候选——即使点击/切换目录导致字段引用重建、旧字段
        // 基线失效,也不会把文件名框误当 Go-to 框(评审 2026-11-06)。
        if in_baseline(&win, &baseline.windows, ax_ffi::cf_equal) {
            continue;
        }
        // 快路径:窗口级焦点元素(焦点元素即候选,只需「可写 + 非基线」)。
        if let Some(fe) = ax_ffi::copy_attr(&win, attr::FOCUSED_ELEMENT) {
            if is_goto_candidate(
                writable_text_field(&fe),
                in_baseline(&fe, &baseline.fields, ax_ffi::cf_equal),
                true,
            ) {
                return Some(fe);
            }
        }
        // 回退:窗口子树搜索(可写 + 非基线;应用级焦点读到时要求同一性复核,
        // 读不到则不以此否决——基线仍防文件名框误中)。
        if let Some(hit) = search_field(&win, 0, &|el| {
            is_goto_candidate(
                writable_text_field(el),
                in_baseline(el, &baseline.fields, ax_ffi::cf_equal),
                focused.as_ref().is_none_or(|f| ax_ffi::cf_equal(f, el)),
            )
        }) {
            return Some(hit);
        }
    }
    None
}

/// ⌘⇧G 前的基线:应用全部窗口 + 窗口内可写 AXTextField(主要是文件名框)。
/// 引用保活整个跳转过程;代理重建下身份比较走 CFEqual(见 [`in_baseline`])。
/// 窗口集是主判据:Go-to sheet 是新窗口,旧窗口内的字段一律排除。
fn baseline_text_fields(pid: i32) -> super::PanelBaseline {
    let Some(app) = ax_ffi::app_element(pid) else {
        return super::PanelBaseline {
            windows: Vec::new(),
            fields: Vec::new(),
        };
    };
    let windows = candidate_windows(&app);
    let mut fields = Vec::new();
    for win in &windows {
        collect_writable_fields(win, 0, &mut fields);
    }
    super::PanelBaseline { windows, fields }
}

/// 供接线层在浮层唤起期后台预采集(见 `platform::panel_baseline_collect`)。
pub fn baseline_for(pid: i32) -> super::PanelBaseline {
    baseline_text_fields(pid)
}

fn collect_writable_fields(el: &AXRef, depth: usize, out: &mut Vec<AXRef>) {
    if writable_text_field(el) {
        out.push(ax_ffi::clone_ref(el));
    }
    if depth >= 6 {
        return;
    }
    for c in ax_ffi::children(el) {
        collect_writable_fields(&c, depth + 1, out);
    }
}

/// 可写文本框谓词(Go-to 框与保存面板文件名框同满足;M0 探针同款)。
fn writable_text_field(el: &AXRef) -> bool {
    ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXTextField")
        && ax_ffi::attr_settable(el, attr::VALUE)
}

/// 带深度限的谓词搜索(命中即克隆引用返回;深度 6 与探针一致)。
fn search_field(el: &AXRef, depth: usize, pred: &dyn Fn(&AXRef) -> bool) -> Option<AXRef> {
    if pred(el) {
        return Some(ax_ffi::clone_ref(el));
    }
    if depth >= 6 {
        return None;
    }
    for c in ax_ffi::children(el) {
        if let Some(hit) = search_field(&c, depth + 1, pred) {
            return Some(hit);
        }
    }
    None
}

/// 前台 pid 仍为目标(pid 时效契约:合成键前以最新 pid 复核,防 pid 复用
/// 把组合键送进无关进程;同探针实现)。
fn pid_still_front(pid: i32) -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .is_some_and(|a| a.processIdentifier() == pid)
}

/// 轮询谓词(50ms 步进,deadline 到返回 false)。
fn wait_until(deadline: Instant, mut pred: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(POLL_STEP);
    }
    false
}

/// 读回校验结论(策略 A 与策略 B 的读回口径一致):读回值等于目标路径即成功,
/// 否则策略失败(由状态机决定降级/终态)。
fn fill_verdict(readback: Option<&str>, path: &str) -> Obs {
    if readback == Some(path) {
        Obs::SheetOpened
    } else {
        Obs::StrategyAFailed
    }
}

/// 单次读回采样(策略 B):重定位失败与「读到值/读不到值」必须区分——
/// 前者是输入框消失,后者只是本轮未读到(继续重试)。
#[derive(Debug)]
enum ReadbackSample {
    /// 输入框仍在;内含 AXValue(None = 属性读取失败/非字符串)。
    Value(Option<String>),
    /// 重定位失败 = 输入框已消失(用户 Esc / sheet 关闭)。
    FieldGone,
}

/// 读回校验结论(策略 B)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Readback {
    /// 读到目标路径。
    Matched,
    /// 重试耗尽仍未读到目标路径。
    Mismatched,
    /// 输入框消失(重定位失败)。
    FieldGone,
}

/// 读回校验(初次 + 最多 `retries` 次重试,每次重试前等 `interval`)。
/// 每次采样都由闭包**重新定位**输入框:⌘V 可能触发 AX 代理重建,复用首次
/// 的 AXRef 会让 3 次读回全失败而误判 FellBack。重定位失败立即返回
/// [`Readback::FieldGone`],由外层映射为 `SheetClosed`。
fn readback_until_match(
    path: &str,
    retries: u32,
    interval: Duration,
    mut sample: impl FnMut() -> ReadbackSample,
) -> Readback {
    for attempt in 0..=retries {
        if attempt > 0 {
            std::thread::sleep(interval);
        }
        match sample() {
            ReadbackSample::FieldGone => return Readback::FieldGone,
            ReadbackSample::Value(v) if v.as_deref() == Some(path) => return Readback::Matched,
            ReadbackSample::Value(_) => {}
        }
    }
    Readback::Mismatched
}

/// 终态 → (对外结果, 是否恢复剪贴板)。
///
/// 恢复判据 ≡ 状态机的 `RestoreClipboard` 契约:策略 B 参与过(`has_saved`)
/// 且终态为 Done/Aborted;`FellBack` 保留路径在剪贴板,不恢复。
/// 非终态意外进入(契约破坏)按中止处理,已保存则仍恢复。
fn outcome_of(step: Step, has_saved: bool) -> (NavigateOutcome, bool) {
    match step {
        Step::Done => (NavigateOutcome::Done, has_saved),
        Step::FellBack => (NavigateOutcome::FellBackToClipboard, false),
        _ => (NavigateOutcome::Aborted, has_saved),
    }
}

/// 发键前焦点诊断:前台 app 的 `AXFocusedWindow` 是否就是保存面板。
/// ⌘⇧G 落空的常见原因是浮层隐藏后键盘焦点没回到面板(合成键被送到宿主
/// 其它窗口),该日志用于区分「焦点错位」与「面板不响应」。
fn log_focus_diag(pid: i32) {
    let Some(app) = ax_ffi::app_element(pid) else {
        log::warn!("[navigator] 焦点诊断:app_element 读取失败 pid={pid}");
        return;
    };
    let focused = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW);
    let panel = find_save_panel_window(&app);
    let focused_is_panel = match (&focused, &panel) {
        (Some(f), Some(p)) => ax_ffi::cf_equal(f, p),
        _ => false,
    };
    let title_of = |w: &Option<AXRef>| {
        w.as_ref().map(|w| {
            let role = ax_ffi::attr_string(w, attr::ROLE).unwrap_or_default();
            let title = ax_ffi::attr_string(w, attr::TITLE).unwrap_or_default();
            format!("{role} {title:?}")
        })
    };
    let panel_state = panel.as_ref().map(|p| {
        (
            ax_ffi::attr_bool(p, attr::FOCUSED),
            ax_ffi::attr_bool(p, attr::MAIN),
        )
    });
    log::info!(
        "[navigator] 焦点诊断:focused_window={:?} 保存面板={:?} 焦点即面板={focused_is_panel} 面板 focused/main={panel_state:?}",
        title_of(&focused),
        title_of(&panel),
    );
}

/// 发 ⌘⇧G 并等待 Go-to 输入框出现(调用方已确认前台是宿主)。
fn send_goto_and_wait(pid: i32, baseline: &super::PanelBaseline, t_start: Instant) -> Obs {
    // 子 sheet 会让保存面板暂时从 AXWindows 消失:在跳转窗口内标记 detector,
    // 避免瞬态 NoPanel 误发 Closed(清掉 handled_pids → 浮层重现)。
    super::panel_detector::mark_navigating(Duration::from_secs(2));
    cg_events::post_combo(None, KEY_G, FLAG_CMD | FLAG_SHIFT);
    log::info!(
        "[navigator] 已发 ⌘⇧G,等待 Go-to 输入框(3s)t+{}ms",
        t_start.elapsed().as_millis()
    );
    let deadline = Instant::now() + STEP_TIMEOUT;
    // 双重复核(非基线 + 焦点一致)才认 sheet 已打开;
    // 3s 内不满足 → Timeout(⌘⇧G 未生效的正确出口,不碰文件名框)。
    if wait_until(deadline, || goto_field(pid, baseline).is_some()) {
        log::info!(
            "[navigator] Go-to 输入框已定位 t+{}ms",
            t_start.elapsed().as_millis()
        );
        Obs::SheetOpened
    } else {
        // 超时诊断:候选窗口与可写输入框概况(定位失败时一次性 dump)。
        if let Some(app) = ax_ffi::app_element(pid) {
            let wins = candidate_windows(&app);
            for (i, w) in wins.iter().enumerate().take(8) {
                log::warn!(
                    "[navigator] 诊断 win[{i}] role={:?} subrole={:?} title={:?}",
                    ax_ffi::attr_string(w, attr::ROLE),
                    ax_ffi::attr_string(w, attr::SUBROLE),
                    ax_ffi::attr_string(w, attr::TITLE)
                );
            }
            let mut fields = Vec::new();
            for w in &wins {
                collect_writable_fields(w, 0, &mut fields);
            }
            log::warn!(
                "[navigator] 诊断 可写输入框 {} 个,基线 {} 个/窗口 {} 个",
                fields.len(),
                baseline.fields.len(),
                baseline.windows.len()
            );
        }
        log::warn!("[navigator] 3s 内未定位 Go-to 输入框(⌘⇧G 未生效或 post 目标错误)");
        // 超时后的焦点快照:区分「焦点不在面板」与「焦点在面板但面板不响应
        // 合成键」(2026-11-07 QuickTime 打开面板实机排障入口)。
        log_focus_diag(pid);
        Obs::Timeout
    }
}

/// 完整跳转(阻塞;调用方在后台线程执行)。
///
/// 执行循环严格按状态机契约:先执行动作得到观测,再 `on(obs)` 转移;
/// `done = true` 时先按 [`outcome_of`] 执行剪贴板收尾,再返回终态。
/// `saved`:None = 从未保存;Some(None) = 保存时无文本;Some(Some(t)) = 原文。
///
/// `baseline` 在 `m.start()` 后、首个动作执行前取得并在整个跳转生命周期保活:
/// 后续每一步的「输入框仍在」自检都用「非基线 + 焦点一致」谓词,
/// 防把文件名框当成 Go-to 框。接线层传入浮层唤起期的后台预采集结果
/// (`prefetched`)以避开回车后的同步 AX 树遍历;None/空则当场同步采集兜底。
pub fn run_navigate(
    pid: i32,
    path: &str,
    prefetched: Option<super::PanelBaseline>,
) -> NavigateOutcome {
    let mut m = NavMachine::new();
    let mut saved: Option<Option<String>> = None;
    let Some(mut act) = m.start() else {
        return NavigateOutcome::Aborted;
    };
    // 端到端计时(日志 t+ms):定位「回车后明显分步」的耗时点。
    let t_start = Instant::now();
    // ⌘⇧G 前基线(首个动作执行前取得,整个跳转生命周期保活)。
    let (baseline, source) = match prefetched.filter(|b| !b.fields.is_empty()) {
        Some(b) => (b, "预采集命中"),
        None => (baseline_text_fields(pid), "同步采集"),
    };
    log::info!(
        "[navigator] 基线{source} {}ms",
        t_start.elapsed().as_millis()
    );
    if baseline.fields.is_empty() {
        // 空基线可能是瞬时 AX 读取失败:此时无法区分 Go-to 框与文件名框,
        // 保守中止(宁可不跳转,也不冒误覆盖文件名的风险)。
        log::warn!("[navigator] 基线为空(可写输入框未采到),保守中止 pid={pid}");
        return NavigateOutcome::Aborted;
    }
    log::info!(
        "[navigator] 开始 pid={pid} path={path} 基线 {} 个输入框/{} 个窗口 t+{}ms",
        baseline.fields.len(),
        baseline.windows.len(),
        t_start.elapsed().as_millis()
    );
    loop {
        // 1) 执行动作 → 观测。Restore 只作为 done 收尾动作执行(见 loop 末尾)。
        let obs = match act {
            Act::SendGoToFolder => {
                // cg_events 的 pid 时效契约:先复核前台,再合成 ⌘⇧G。
                // 二次跳转场景:用户点击非 key 的浮层会让本进程成为前台
                // (见 notify_frontmost 注释),此时不能直接中止——先显式
                // 激活宿主 app 并把前台还给它,再发键。
                if !pid_still_front(pid) {
                    log::info!(
                        "[navigator] 前台不是宿主(点击浮层致自身激活),请求恢复宿主 pid={pid} t+{}ms",
                        t_start.elapsed().as_millis()
                    );
                    let requested = super::activate_app(pid);
                    log::info!("[navigator] 宿主激活请求={requested},等待前台恢复(1s)");
                    let deadline = Instant::now() + Duration::from_secs(1);
                    if wait_until(deadline, || pid_still_front(pid)) {
                        log::info!(
                            "[navigator] 宿主已回到前台 t+{}ms",
                            t_start.elapsed().as_millis()
                        );
                        // 宿主被激活 ≠ 保存面板 sheet 拿回键盘焦点:显式恢复并
                        // 以读回确认作为发键门槛;确认失败宁可不跳,也不把
                        // ⌘⇧G 送进宿主其它窗口。
                        if focus_save_panel(pid) {
                            send_goto_and_wait(pid, &baseline, t_start)
                        } else {
                            log::warn!("[navigator] 保存面板焦点未恢复,中止本次跳转 pid={pid}");
                            Obs::SheetClosed
                        }
                    } else {
                        log::warn!("[navigator] 前台未恢复宿主,中止 pid={pid}");
                        Obs::SheetClosed
                    }
                } else {
                    send_goto_and_wait(pid, &baseline, t_start)
                }
            }
            Act::FillViaAx => match goto_field(pid, &baseline) {
                // Go-to 框消失(用户 Esc 关 sheet):按 sheet 关闭回报。
                None => {
                    log::warn!("[navigator] 策略 A:Go-to 框已消失");
                    Obs::SheetClosed
                }
                Some(field) => {
                    // 直设失败不单独短路:统一以读回值判定(设置未生效时读回仍是旧值)。
                    let _ = ax_ffi::set_attr(&field, attr::VALUE, &CFStr::new(path));
                    let verdict =
                        fill_verdict(ax_ffi::attr_string(&field, attr::VALUE).as_deref(), path);
                    match verdict {
                        Obs::SheetOpened => log::info!(
                            "[navigator] 策略 A:直设 value 成功 t+{}ms",
                            t_start.elapsed().as_millis()
                        ),
                        _ => log::warn!(
                            "[navigator] 策略 A:直设 value 读回不匹配,降级策略 B t+{}ms",
                            t_start.elapsed().as_millis()
                        ),
                    }
                    verdict
                }
            },
            Act::PasteViaClipboard => {
                // M6 契约:无论后续检查如何,先保存原剪贴板(None = 原本无文本)。
                // 仅在首次参与时拍摄,防未来 Confirm 降级 B 路径覆盖原始快照。
                if saved.is_none() {
                    saved = Some(clipboard_text());
                }
                if goto_field(pid, &baseline).is_none() {
                    Obs::SheetClosed
                } else if !clipboard_set_text(path) {
                    // 遗留(Task 15 实机评估):set 失败边角——此时路径并未进剪贴板,
                    // 但状态机会按策略失败降级/终态处理;实机复核提示文案是否失真。
                    log::warn!("[navigator] 策略 B:写剪贴板失败");
                    Obs::StrategyAFailed
                } else if !pid_still_front(pid) {
                    log::warn!("[navigator] 策略 B:⌘V 前复核前台已切换,中止");
                    Obs::SheetClosed
                } else {
                    // 遗留(Task 15 实机评估):策略 A 部分生效后走 B 的 ⌘V 追加风险
                    // (最坏得到「旧值+路径」文本,不误保存)。
                    cg_events::post_combo(None, KEY_V, FLAG_CMD);
                    log::info!("[navigator] 策略 B:已发 ⌘V,读回校验");
                    // 每次读回重新定位输入框(⌘V 可能触发 AX 代理重建)。
                    let outcome =
                        readback_until_match(path, PASTE_READBACK_RETRIES, POLL_STEP, || {
                            match goto_field(pid, &baseline) {
                                None => ReadbackSample::FieldGone,
                                Some(f) => {
                                    ReadbackSample::Value(ax_ffi::attr_string(&f, attr::VALUE))
                                }
                            }
                        });
                    match outcome {
                        Readback::Matched => {
                            log::info!("[navigator] 策略 B:粘贴读回成功");
                            Obs::SheetOpened
                        }
                        Readback::Mismatched => {
                            log::warn!("[navigator] 策略 B:粘贴读回不匹配");
                            Obs::StrategyAFailed
                        }
                        Readback::FieldGone => {
                            log::warn!("[navigator] 策略 B:粘贴期间 Go-to 框消失");
                            Obs::SheetClosed
                        }
                    }
                }
            }
            Act::PressReturn => {
                // 合成 Return 前自检 Go-to 框仍在(非基线 + 焦点一致)且前台仍是目标 pid。
                if goto_field(pid, &baseline).is_none() || !pid_still_front(pid) {
                    log::warn!("[navigator] Return 前自检失败(框消失/前台切换)");
                    Obs::SheetClosed
                } else {
                    cg_events::post_combo(None, KEY_RETURN, 0);
                    log::info!(
                        "[navigator] 已发 Return,等待 sheet 关闭(3s)t+{}ms",
                        t_start.elapsed().as_millis()
                    );
                    let deadline = Instant::now() + STEP_TIMEOUT;
                    if wait_until(deadline, || goto_field(pid, &baseline).is_none()) {
                        log::info!(
                            "[navigator] go-to sheet 已关闭(跳转提交)t+{}ms",
                            t_start.elapsed().as_millis()
                        );
                        Obs::SheetClosed
                    } else {
                        log::warn!(
                            "[navigator] Return 后 3s 内 sheet 未关闭 t+{}ms",
                            t_start.elapsed().as_millis()
                        );
                        Obs::Timeout
                    }
                }
            }
            // 防御:状态机契约保证 Restore 只出现在 done=true 的 next 上。
            // 非 done 路径出现即契约破坏——后台线程绝不 panic,记日志后中止。
            Act::RestoreClipboard => {
                log::error!(
                    "[navigator] RestoreClipboard 出现在非 done 动作路径(状态机契约破坏),中止"
                );
                return NavigateOutcome::Aborted;
            }
        };

        // 2) 状态机转移。
        let (next, done) = m.on(obs);
        if done {
            // M6 契约:先执行收尾动作(Restore),再按终态映射。
            // outcome_of 的恢复判据 ≡ 状态机 RestoreClipboard 契约
            // (Done/Aborted 且策略 B 参与);saved 为 None = 从未保存过 → 不恢复,
            // 避免误清用户剪贴板。
            // 遗留(后续单独任务):当前只做纯文本快照;`clipboard_restore(None)`
            // 走 clearContents 会连带清掉非文本项,需改多类型快照。
            let (outcome, restore) = outcome_of(m.step(), saved.is_some());
            if restore {
                if let Some(s) = &saved {
                    if !clipboard_restore(s.as_deref()) {
                        log::warn!("[navigator] 剪贴板恢复失败");
                    }
                }
            }
            log::info!(
                "[navigator] 结束 → {outcome:?} 总耗时 {}ms",
                t_start.elapsed().as_millis()
            );
            return outcome;
        }
        let Some(a) = next else {
            // 无动作且未 done:状态机保证不会走到(防御兜底,防死循环)。
            log::error!(
                "[navigator] 状态机在非终态返回无动作(契约破坏),中止 step={:?}",
                m.step()
            );
            return NavigateOutcome::Aborted;
        };
        act = a;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 已 start 的机器(AwaitSheet)。
    fn started() -> NavMachine {
        let mut m = NavMachine::new();
        assert_eq!(m.start(), Some(Act::SendGoToFolder));
        m
    }

    /// 主链:start → ⌘⇧G → sheet 出现 → 策略 A 填入 → 确认 → Done。
    /// 突变「AwaitSheet 上 SheetClosed 误入 Fill」时 esc 用例必红。
    #[test]
    fn happy_path_strategy_a() {
        let mut m = NavMachine::new();
        assert_eq!(m.start(), Some(Act::SendGoToFolder));
        assert_eq!(m.step(), Step::AwaitSheet);
        let (a, done) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::FillViaAx));
        assert!(!done);
        assert_eq!(m.step(), Step::Fill);
        // Fill 成功:输入框仍在 → 推进 Confirm + 统一 Return
        let (a, done) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::PressReturn));
        assert!(!done);
        assert_eq!(m.step(), Step::Confirm);
        assert!(!m.used_clipboard(), "策略 A 不碰剪贴板");
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, None, "策略 A 无剪贴板收尾");
        assert_eq!(m.step(), Step::Done);
    }

    /// 降级:A 失败 → 策略 B(剪贴板),收尾必 RestoreClipboard。
    #[test]
    fn degrade_to_strategy_b_restores_clipboard() {
        let mut m = started();
        m.on(Obs::SheetOpened); // AwaitSheet → Fill
        let (a, done) = m.on(Obs::StrategyAFailed);
        assert_eq!(a, Some(Act::PasteViaClipboard));
        assert!(!done);
        assert!(m.used_clipboard(), "降级后须记住剪贴板参与");
        assert_eq!(m.step(), Step::Fill, "降级 B 仍在 Fill");
        let (a, done) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::PressReturn));
        assert!(!done);
        assert_eq!(m.step(), Step::Confirm);
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, Some(Act::RestoreClipboard), "策略 B 收尾恢复剪贴板");
        assert_eq!(m.step(), Step::Done);
    }

    /// 用户 Esc 中止(AwaitSheet 与 Fill 两阶段):
    /// Fill 阶段且已降级时恢复剪贴板,否则无声退出(design §5 竞态防护的状态面)。
    #[test]
    fn user_esc_aborts_and_restores_when_clipboard_used() {
        // AwaitSheet 阶段 Esc:无动作退出
        let mut m = started();
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, None);
        assert_eq!(m.step(), Step::Aborted);

        // Fill 阶段(已降级)Esc:恢复剪贴板
        let mut m = started();
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed);
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, Some(Act::RestoreClipboard));
        assert_eq!(m.step(), Step::Aborted);
    }

    /// AwaitSheet 阶段超时:中止且无动作。
    #[test]
    fn await_sheet_timeout_aborts() {
        let mut m = started();
        let (a, done) = m.on(Obs::Timeout);
        assert!(done);
        assert_eq!(a, None);
        assert_eq!(m.step(), Step::Aborted);
    }

    /// start 幂等:非 Idle 状态重复 start 不动作。
    #[test]
    fn start_is_idempotent() {
        let mut m = NavMachine::new();
        assert_eq!(m.start(), Some(Act::SendGoToFolder));
        assert_eq!(m.start(), None);
    }

    /// start 是唯一入口:Idle 上任何观测都不产生动作、不改变状态
    /// (防执行层尚未 start 时收到过期观测而主动合成 ⌘⇧G)。
    #[test]
    fn idle_ignores_observations_until_started() {
        let mut m = NavMachine::new();
        for o in [Obs::Timeout, Obs::SheetClosed, Obs::SheetOpened] {
            assert_eq!(m.on(o), (None, false), "Idle 上观测不得产生动作");
            assert_eq!(m.step(), Step::Idle, "Idle 上观测不得改变状态");
        }
        assert_eq!(m.start(), Some(Act::SendGoToFolder));
        assert_eq!(m.step(), Step::AwaitSheet);
    }

    /// Fill 阶段超时必须终止(H1 回归钉):未降级静默中止,已降级恢复剪贴板。
    #[test]
    fn fill_timeout_terminates() {
        // 未降级:无动作中止
        let mut m = started();
        m.on(Obs::SheetOpened);
        let (a, done) = m.on(Obs::Timeout);
        assert!(done, "Fill 阶段超时必须终止");
        assert_eq!(a, None);
        assert_eq!(m.step(), Step::Aborted);

        // 已降级:恢复剪贴板后中止
        let mut m = started();
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed);
        let (a, done) = m.on(Obs::Timeout);
        assert!(done, "Fill 阶段超时必须终止");
        assert_eq!(a, Some(Act::RestoreClipboard));
        assert_eq!(m.step(), Step::Aborted);
    }

    /// 终态再入:Done/Aborted/FellBack 上任何观测都返回 (None, true) 且状态不变。
    #[test]
    fn terminal_states_always_terminate() {
        fn assert_terminal(m: &mut NavMachine, expected: Step) {
            for o in [
                Obs::SheetOpened,
                Obs::SheetClosed,
                Obs::StrategyAFailed,
                Obs::Timeout,
            ] {
                assert_eq!(m.on(o), (None, true), "终态 {:?} 上再入必须终止", expected);
                assert_eq!(m.step(), expected);
            }
        }

        let mut done = started();
        done.on(Obs::SheetOpened);
        done.on(Obs::SheetOpened);
        done.on(Obs::SheetClosed);
        assert_eq!(done.step(), Step::Done);
        assert_terminal(&mut done, Step::Done);

        let mut aborted = started();
        aborted.on(Obs::Timeout);
        assert_eq!(aborted.step(), Step::Aborted);
        assert_terminal(&mut aborted, Step::Aborted);

        let mut fell_back = started();
        fell_back.on(Obs::SheetOpened);
        fell_back.on(Obs::StrategyAFailed);
        fell_back.on(Obs::StrategyAFailed);
        assert_eq!(fell_back.step(), Step::FellBack);
        assert_terminal(&mut fell_back, Step::FellBack);
    }

    /// 策略 B 也失败:进入 FellBack 终态,不产出 RestoreClipboard(路径保留)。
    #[test]
    fn strategy_b_failure_falls_back_without_restoring() {
        let mut m = started();
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed); // 首次失败 → 降级 B
        let (a, done) = m.on(Obs::StrategyAFailed); // B 也失败
        assert!(done);
        assert_eq!(a, None, "FellBack 不恢复剪贴板(路径保留供用户粘贴)");
        assert!(m.used_clipboard());
        assert_eq!(m.step(), Step::FellBack);
    }

    /// 确认阶段策略 A 失败:当场降级 B,回到 Fill 发 ⌘V。
    #[test]
    fn confirm_strategy_failure_degrades_to_b() {
        let mut m = started();
        m.on(Obs::SheetOpened); // Fill(策略 A)
        m.on(Obs::SheetOpened); // Confirm
        assert_eq!(m.step(), Step::Confirm);
        assert!(!m.used_clipboard());
        let (a, done) = m.on(Obs::StrategyAFailed);
        assert!(!done);
        assert_eq!(a, Some(Act::PasteViaClipboard));
        assert!(m.used_clipboard(), "降级后记剪贴板参与");
        assert_eq!(m.step(), Step::Fill, "降级 B 回到 Fill");
    }

    /// 确认阶段超时两分支:剪贴板参与 → FellBack(路径留在剪贴板,不恢复);
    /// 未参与 → Aborted。
    #[test]
    fn confirm_timeout_paths() {
        // 策略 A(未参与剪贴板)→ Aborted 无动作
        let mut m = started();
        m.on(Obs::SheetOpened);
        m.on(Obs::SheetOpened); // Confirm
        let (a, done) = m.on(Obs::Timeout);
        assert!(done);
        assert_eq!(a, None);
        assert_eq!(m.step(), Step::Aborted);

        // 策略 B(剪贴板已写入)→ FellBack 无动作
        let mut m = started();
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed);
        m.on(Obs::SheetOpened); // Confirm
        let (a, done) = m.on(Obs::Timeout);
        assert!(done);
        assert_eq!(a, None, "FellBack 不恢复剪贴板");
        assert_eq!(m.step(), Step::FellBack);
        assert!(m.used_clipboard());
    }

    /// 读回校验结论:恰等目标路径才算成功,None/旧值/其他路径均为策略失败。
    /// 突变「只判 Some 不比值」→ 旧值用例必红。
    #[test]
    fn fill_verdict_requires_exact_readback() {
        assert_eq!(fill_verdict(Some("/tmp"), "/tmp"), Obs::SheetOpened);
        assert_eq!(
            fill_verdict(None, "/tmp"),
            Obs::StrategyAFailed,
            "读不到值 = 失败"
        );
        assert_eq!(
            fill_verdict(Some("/tmp/old"), "/tmp"),
            Obs::StrategyAFailed,
            "读回旧值 = 设置未生效"
        );
        assert_eq!(
            fill_verdict(Some(""), "/tmp"),
            Obs::StrategyAFailed,
            "空值不等于目标路径"
        );
    }

    /// Go-to 候选三条件:可写、非基线、焦点同时满足;任一缺失拒绝。
    /// 突变任一条件(如去掉「非基线」)→ 文件名框用例必红(防覆盖文件名)。
    #[test]
    fn goto_candidate_requires_writable_new_and_focused() {
        assert!(
            is_goto_candidate(true, false, true),
            "三条件齐备才是 Go-to 框"
        );
        assert!(!is_goto_candidate(false, false, true), "不可写拒绝");
        assert!(!is_goto_candidate(true, true, true), "基线内(文件名框)拒绝");
        assert!(!is_goto_candidate(true, false, false), "非焦点拒绝");
        assert!(!is_goto_candidate(false, true, false), "多条件缺失拒绝");
    }

    /// 基线身份比较走调用方谓词(AX 侧为 CFEqual);命中基线即拒绝。
    /// 突变「any → !any」→ 两条断言必红。
    #[test]
    fn in_baseline_uses_identity_predicate() {
        let baseline = [1u32, 2, 3];
        assert!(
            in_baseline(&2, &baseline, |a, b| a == b),
            "既有输入框在基线内"
        );
        assert!(
            !in_baseline(&4, &baseline, |a, b| a == b),
            "新增输入框不在基线内"
        );
        let empty: [u32; 0] = [];
        assert!(!in_baseline(&1, &empty, |a, b| a == b), "空基线永不命中");
    }

    /// 读回重试:初次 + 2 次重试内命中即成功(间隔 0 便于单测)。
    /// 每次采样都重新定位(闭包每次被调用);突变「重试次数少 1」→ calls 必红。
    #[test]
    fn readback_retries_until_match() {
        let mut calls = 0;
        let r = readback_until_match("/tmp", 2, Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                ReadbackSample::Value(Some("/tmp/old".to_owned()))
            } else {
                ReadbackSample::Value(Some("/tmp".to_owned()))
            }
        });
        assert_eq!(r, Readback::Matched, "第 3 次读回(初次 + 2 次重试)命中");
        assert_eq!(calls, 3);
    }

    /// 读回重试耗尽:总共尝试 retries + 1 次后放弃,返回 Mismatched。
    #[test]
    fn readback_gives_up_after_retries() {
        let mut calls = 0;
        let r = readback_until_match("/tmp", 2, Duration::ZERO, || {
            calls += 1;
            ReadbackSample::Value(None)
        });
        assert_eq!(r, Readback::Mismatched);
        assert_eq!(calls, 3, "初次 + 2 次重试后放弃");
    }

    /// 重定位失败(输入框消失)立即返回 FieldGone,不再重试。
    /// 突变「FieldGone 继续重试」→ calls 断言必红。
    #[test]
    fn readback_stops_when_field_gone() {
        let mut calls = 0;
        let r = readback_until_match("/tmp", 2, Duration::ZERO, || {
            calls += 1;
            if calls == 1 {
                ReadbackSample::Value(Some("/tmp/old".to_owned()))
            } else {
                ReadbackSample::FieldGone
            }
        });
        assert_eq!(r, Readback::FieldGone, "重定位失败即输入框消失");
        assert_eq!(calls, 2, "发现输入框消失后不再重试");
    }

    /// 轮询谓词:成立立即返回 true(不睡);deadline 已过直接 false。
    #[test]
    fn wait_until_respects_deadline() {
        assert!(
            wait_until(Instant::now() + Duration::from_secs(5), || true),
            "谓词立即成立"
        );
        assert!(
            !wait_until(Instant::now() - Duration::from_millis(1), || true),
            "deadline 已过不得再执行谓词"
        );
    }

    /// 终态映射:恢复仅在 Done/Aborted 且策略 B 参与(has_saved)时;
    /// FellBack 保留路径在剪贴板,绝不恢复;非终态防御映射为 Aborted。
    /// 突变「FellBack 也恢复」或「Done 忽略 has_saved」→ 对应断言必红。
    #[test]
    fn outcome_of_maps_terminal_and_restore() {
        assert_eq!(
            outcome_of(Step::Done, true),
            (NavigateOutcome::Done, true),
            "成功且策略 B 参与 → 恢复"
        );
        assert_eq!(
            outcome_of(Step::Done, false),
            (NavigateOutcome::Done, false),
            "策略 A 成功不碰剪贴板"
        );
        assert_eq!(
            outcome_of(Step::FellBack, true),
            (NavigateOutcome::FellBackToClipboard, false),
            "路径保留在剪贴板,不恢复"
        );
        assert_eq!(
            outcome_of(Step::Aborted, true),
            (NavigateOutcome::Aborted, true),
            "中止时恢复已保存的剪贴板"
        );
        assert_eq!(
            outcome_of(Step::Aborted, false),
            (NavigateOutcome::Aborted, false)
        );
        // 防御:非终态意外进入 → Aborted(不 panic);已保存则仍恢复。
        for step in [Step::Idle, Step::AwaitSheet, Step::Fill, Step::Confirm] {
            assert_eq!(outcome_of(step, false), (NavigateOutcome::Aborted, false));
            assert_eq!(outcome_of(step, true), (NavigateOutcome::Aborted, true));
        }
    }
}

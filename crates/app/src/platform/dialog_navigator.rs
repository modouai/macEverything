//! 跳转状态机(纯逻辑,design §3.4)及其平台执行层(Task 11)。
//! 步骤由「观测事件」驱动转移,产出「动作」;执行层把每个动作落实到
//! AX 直设/合成键/剪贴板,并在每次合成键前自检输入框存在(消失则回报
//! [`Obs::SheetClosed`])。状态机部分不碰平台 API(单测全覆盖);执行层
//! 的判定逻辑抽为纯函数(`fill_verdict`/`readback_until_match`/`wait_until`)
//! 单独测试。

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

    /// 策略 B 是否已参与(状态机单测读取;执行层按 `saved` 决定是否恢复剪贴板,
    /// 生产路径暂不读取本方法,Task 14 接线时再评估)。
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

/// 输入框是否在当前焦点窗口的 AX 树中(深度 6 浅层搜索,谓词同探针:
/// 可写 AXTextField)。探针的焦点一致/新增复核留在探针里(`panel_probe`
/// 保留 AXPress 对照实验),执行层只做存在性观测。
fn goto_field(pid: i32) -> Option<AXRef> {
    let app = ax_ffi::app_element(pid)?;
    let win = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW)?;
    search_field(&win, 0)
}

/// 可写文本框谓词(Go-to 框与保存面板文件名框同满足;M0 探针同款)。
fn writable_text_field(el: &AXRef) -> bool {
    ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXTextField")
        && ax_ffi::attr_settable(el, attr::VALUE)
}

/// 带深度限的谓词搜索(命中即克隆引用返回;深度 6 与探针一致)。
fn search_field(el: &AXRef, depth: usize) -> Option<AXRef> {
    if writable_text_field(el) {
        return Some(ax_ffi::clone_ref(el));
    }
    if depth >= 6 {
        return None;
    }
    for c in ax_ffi::children(el) {
        if let Some(hit) = search_field(&c, depth + 1) {
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

/// 读回校验(初次 + 最多 `retries` 次重试,每次重试前等 `interval`)。
/// 抽成闭包驱动便于单测重试次数/时序;实际读取 = AX VALUE。
fn readback_until_match(
    path: &str,
    retries: u32,
    interval: Duration,
    mut read: impl FnMut() -> Option<String>,
) -> bool {
    for attempt in 0..=retries {
        if attempt > 0 {
            std::thread::sleep(interval);
        }
        if read().as_deref() == Some(path) {
            return true;
        }
    }
    false
}

/// 完整跳转(阻塞;调用方在后台线程执行)。
///
/// 执行循环严格按状态机契约:先执行动作得到观测,再 `on(obs)` 转移;
/// `done = true` 时若带 [`Act::RestoreClipboard`] 先恢复剪贴板,再按终态映射。
/// `saved`:None = 从未保存;Some(None) = 保存时无文本;Some(Some(t)) = 原文。
pub fn run_navigate(pid: i32, path: &str) -> NavigateOutcome {
    let mut m = NavMachine::new();
    let mut saved: Option<Option<String>> = None;
    let Some(mut act) = m.start() else {
        return NavigateOutcome::Aborted;
    };
    loop {
        // 1) 执行动作 → 观测。Restore 只作为 done 收尾动作执行(见 loop 末尾)。
        let obs = match act {
            Act::SendGoToFolder => {
                // cg_events 的 pid 时效契约:先复核前台,再合成 ⌘⇧G。
                if !pid_still_front(pid) {
                    Obs::SheetClosed
                } else {
                    cg_events::post_combo(Some(pid), KEY_G, FLAG_CMD | FLAG_SHIFT);
                    let deadline = Instant::now() + STEP_TIMEOUT;
                    if wait_until(deadline, || goto_field(pid).is_some()) {
                        Obs::SheetOpened
                    } else {
                        Obs::Timeout
                    }
                }
            }
            Act::FillViaAx => match goto_field(pid) {
                // 输入框消失(用户 Esc 关 sheet):按 sheet 关闭回报。
                None => Obs::SheetClosed,
                Some(field) => {
                    // 直设失败不单独短路:统一以读回值判定(设置未生效时读回仍是旧值)。
                    let _ = ax_ffi::set_attr(&field, attr::VALUE, &CFStr::new(path));
                    fill_verdict(ax_ffi::attr_string(&field, attr::VALUE).as_deref(), path)
                }
            },
            Act::PasteViaClipboard => {
                // M6 契约:无论后续检查如何,先保存原剪贴板(None = 原本无文本)。
                saved = Some(clipboard_text());
                match goto_field(pid) {
                    None => Obs::SheetClosed,
                    Some(field) => {
                        if !clipboard_set_text(path) {
                            Obs::StrategyAFailed
                        } else if !pid_still_front(pid) {
                            Obs::SheetClosed
                        } else {
                            cg_events::post_combo(Some(pid), KEY_V, FLAG_CMD);
                            let matched = readback_until_match(
                                path,
                                PASTE_READBACK_RETRIES,
                                POLL_STEP,
                                || ax_ffi::attr_string(&field, attr::VALUE),
                            );
                            if matched {
                                Obs::SheetOpened
                            } else {
                                Obs::StrategyAFailed
                            }
                        }
                    }
                }
            }
            Act::PressReturn => {
                // 合成 Return 前自检输入框仍在 + 前台仍是目标 pid。
                if goto_field(pid).is_none() || !pid_still_front(pid) {
                    Obs::SheetClosed
                } else {
                    cg_events::post_combo(Some(pid), KEY_RETURN, 0);
                    let deadline = Instant::now() + STEP_TIMEOUT;
                    if wait_until(deadline, || goto_field(pid).is_none()) {
                        Obs::SheetClosed
                    } else {
                        Obs::Timeout
                    }
                }
            }
            // 防御:状态机契约保证 Restore 只出现在 done=true 的 next 上,
            // 非 done 路径出现即契约破坏(不可恢复),直接熔断。
            Act::RestoreClipboard => unreachable!(
                "RestoreClipboard 只作为 done 收尾动作返回(状态机契约);非 done 路径出现属实现错误"
            ),
        };

        // 2) 状态机转移。
        let (next, done) = m.on(obs);
        if done {
            // M6 契约:先执行收尾动作(Restore),再按终态映射。
            // saved 为 None = 从未保存过 → 不调用恢复,避免误清用户剪贴板。
            if let Some(Act::RestoreClipboard) = next {
                if let Some(s) = &saved {
                    let _ = clipboard_restore(s.as_deref());
                }
            }
            return match m.step() {
                Step::Done => NavigateOutcome::Done,
                Step::FellBack => NavigateOutcome::FellBackToClipboard,
                _ => NavigateOutcome::Aborted,
            };
        }
        let Some(a) = next else {
            // 无动作且未 done:状态机保证不会走到(防御兜底,防死循环)。
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

    /// 读回重试:初次 + 2 次重试内命中即成功(间隔 0 便于单测)。
    /// 突变「重试次数少 1」→ calls 断言必红。
    #[test]
    fn readback_retries_until_match() {
        let mut calls = 0;
        let ok = readback_until_match("/tmp", 2, Duration::ZERO, || {
            calls += 1;
            if calls < 3 {
                Some("/tmp/old".to_owned())
            } else {
                Some("/tmp".to_owned())
            }
        });
        assert!(ok, "第 3 次读回(初次 + 2 次重试)命中");
        assert_eq!(calls, 3);
    }

    /// 读回重试耗尽:总共尝试 retries + 1 次后放弃,返回 false。
    #[test]
    fn readback_gives_up_after_retries() {
        let mut calls = 0;
        let ok = readback_until_match("/tmp", 2, Duration::ZERO, || {
            calls += 1;
            None
        });
        assert!(!ok);
        assert_eq!(calls, 3, "初次 + 2 次重试后放弃");
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
}

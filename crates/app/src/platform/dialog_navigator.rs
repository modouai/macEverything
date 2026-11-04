//! 跳转状态机(纯逻辑,design §3.4):步骤由「观测事件」驱动转移,产出「动作」。
//! 执行层(Task 11)在每个动作执行后把观测结果回投;执行层在每次合成键前
//! 自检输入框存在(消失则回报 [`Obs::SheetClosed`])。
//! 本模块不碰 AX/CGEvent/剪贴板,可用单测全覆盖。
// Task 11 执行层接线后移除(当前动作/观测枚举尚无生产调用方)。
#![allow(dead_code)]

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

    pub fn used_clipboard(&self) -> bool {
        self.used_clipboard
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
}

//! 跳转状态机(纯逻辑,design §3.4):步骤由「观测事件」驱动转移,产出「动作」。
//! 执行层(Task 11)在每个动作执行后把观测结果回投;每步合成前要求
//! VerifyField 确认(防用户中途 Esc 关闭 sheet 后后续键误击面板主界面)。
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
}

/// 执行层回报的观测结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obs {
    /// Go-to sheet 的路径输入框出现在 AX 树。
    SheetOpened,
    /// 输入框消失(用户 Esc 关闭 sheet / 跳转完成)。
    SheetClosed,
    /// 策略 A 失败(直设/按钮不可用),降级策略 B。
    StrategyAFailed,
    /// 全流程超时。
    Timeout,
}

/// 状态机产出的下一步动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// 发 ⌘⇧G。
    SendGoToFolder,
    /// 策略 A:AX 直设 value + AXPress 前往。
    FillViaAx,
    /// 策略 B:剪贴板 set + ⌘V。
    PasteViaClipboard,
    /// 合成 Return。
    PressReturn,
    /// 执行层确认输入框仍在(每步合成前,design §3.4)。
    VerifyField,
    /// 恢复剪贴板(策略 B 参与时的收尾;Done/Aborted 均执行)。
    RestoreClipboard,
}

/// 状态机:策略 A 优先,A 失败当场降级 B。
pub struct NavMachine {
    pub step: Step,
    /// 策略 B 降级后为 true(收尾要恢复剪贴板)。
    used_clipboard: bool,
}

impl NavMachine {
    pub fn new() -> Self {
        Self {
            step: Step::Idle,
            used_clipboard: false,
        }
    }

    /// 转移:返回 (下一步动作, 是否结束)。结束 = 执行层停止循环。
    /// 入口约定:执行层以 [`Act::SendGoToFolder`] 起步(见 [`Self::start`]),
    /// 此后每次动作执行完毕回报一个 Obs。
    pub fn on(&mut self, o: Obs) -> (Option<Act>, bool) {
        match (self.step, o) {
            (Step::Idle, Obs::SheetOpened) | (Step::Idle, Obs::Timeout) => {
                // 入口驱动:开始 ⌘⇧G。
                self.step = Step::AwaitSheet;
                (Some(Act::SendGoToFolder), false)
            }
            (Step::AwaitSheet, Obs::SheetOpened) => {
                self.step = Step::Fill;
                (Some(Act::FillViaAx), false)
            }
            (Step::AwaitSheet, Obs::Timeout | Obs::SheetClosed) => {
                self.step = Step::Aborted;
                (None, true)
            }
            (Step::Fill, Obs::StrategyAFailed) => {
                self.used_clipboard = true;
                (Some(Act::PasteViaClipboard), false)
            }
            (Step::Fill, Obs::SheetClosed) => {
                self.step = Step::Aborted;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            // Fill 动作成功后执行层回报 SheetOpened(输入框仍在且已写入)推进 Confirm,
            // 或直接进入 Confirm(执行层以 PressReturn 观察闭合)。
            (Step::Fill, Obs::SheetOpened) => {
                self.step = Step::Confirm;
                (Some(Act::PressReturn), false)
            }
            (Step::Confirm, Obs::SheetClosed) => {
                self.step = Step::Done;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            (Step::Confirm, Obs::Timeout | Obs::StrategyAFailed) => {
                self.step = Step::Aborted;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            _ => (None, false), // 未定义转移:保持原状(执行层超时兜底)
        }
    }

    /// 入口:显式开始(等价于 `on(Obs::SheetOpened)` 的 Idle 分支,语义更直白)。
    /// 返回 ⌘⇧G 动作;重复调用无副作用(已非 Idle 时返回 None)。
    pub fn start(&mut self) -> Option<Act> {
        if self.step != Step::Idle {
            return None;
        }
        self.step = Step::AwaitSheet;
        Some(Act::SendGoToFolder)
    }

    pub fn used_clipboard(&self) -> bool {
        self.used_clipboard
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 主链:start → ⌘⇧G → sheet 出现 → 策略 A 填入 → 确认 → Done。
    /// 突变「AwaitSheet 上 SheetClosed 误入 Fill」时 esc 用例必红。
    #[test]
    fn happy_path_strategy_a() {
        let mut m = NavMachine::new();
        assert_eq!(m.start(), Some(Act::SendGoToFolder));
        let (a, done) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::FillViaAx));
        assert!(!done);
        // Fill 成功:输入框仍在 → 推进 Confirm + 发 Return
        let (a, done) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::PressReturn));
        assert!(!done);
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, None, "策略 A 无剪贴板收尾");
        assert_eq!(m.step, Step::Done);
    }

    /// 降级:A 失败 → 策略 B(剪贴板),收尾必 RestoreClipboard。
    #[test]
    fn degrade_to_strategy_b_restores_clipboard() {
        let mut m = NavMachine::new();
        m.start();
        m.on(Obs::SheetOpened); // AwaitSheet → Fill
        let (a, _) = m.on(Obs::StrategyAFailed);
        assert_eq!(a, Some(Act::PasteViaClipboard));
        assert!(m.used_clipboard(), "降级后须记住剪贴板参与");
        m.on(Obs::SheetOpened); // Confirm + Return
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, Some(Act::RestoreClipboard), "策略 B 收尾恢复剪贴板");
        assert_eq!(m.step, Step::Done);
    }

    /// 用户 Esc 中止(AwaitSheet 与 Fill 两阶段):
    /// Fill 阶段且已降级时恢复剪贴板,否则无声退出(design §5 竞态防护的状态面)。
    #[test]
    fn user_esc_aborts_and_restores_when_clipboard_used() {
        // AwaitSheet 阶段 Esc:无动作退出
        let mut m = NavMachine::new();
        m.start();
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, None);
        assert_eq!(m.step, Step::Aborted);

        // Fill 阶段(已降级)Esc:恢复剪贴板
        let mut m = NavMachine::new();
        m.start();
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed);
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, Some(Act::RestoreClipboard));
        assert_eq!(m.step, Step::Aborted);
    }

    /// AwaitSheet 阶段超时:中止且无动作。
    #[test]
    fn await_sheet_timeout_aborts() {
        let mut m = NavMachine::new();
        m.start();
        let (a, done) = m.on(Obs::Timeout);
        assert!(done);
        assert_eq!(a, None);
        assert_eq!(m.step, Step::Aborted);
    }

    /// start 幂等:非 Idle 状态重复 start 不动作。
    #[test]
    fn start_is_idempotent() {
        let mut m = NavMachine::new();
        assert_eq!(m.start(), Some(Act::SendGoToFolder));
        assert_eq!(m.start(), None);
    }

    /// 未定义转移保持原状:确认阶段的策略 A 失败不降级直接中止收尾。
    #[test]
    fn confirm_stage_strategy_failure_aborts_with_cleanup() {
        let mut m = NavMachine::new();
        m.start();
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed); // 降级 B
        m.on(Obs::SheetOpened); // Confirm
        let (a, done) = m.on(Obs::StrategyAFailed);
        assert!(done, "确认阶段失败 = 中止");
        assert_eq!(a, Some(Act::RestoreClipboard), "剪贴板参与过必须恢复");
        assert_eq!(m.step, Step::Aborted);
    }
}

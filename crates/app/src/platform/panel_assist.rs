//! 保存面板助手的跨平台数据类型与事件决策纯逻辑。
//!
//! 检测器(`panel_detector`)只在 macOS 实现;这里的事件枚举、Esc 屏蔽状态与
//! 「事件 + 状态 → 动作」决策不依赖 AX,UI 接线层与单测可在任意平台编译验证。

use std::collections::HashSet;

/// 检测事件(UI 接线层每帧 poll;mpsc 单消费)。
///
/// 重复语义:`Shown` 可能因多个通知(AXWindowCreated + AXFocusedWindowChanged +
/// on_signal 预扫)对同一面板重复发出;去重责任在接线层(浮层已显示则忽略),
/// detector 不做状态去重。
///
/// 关闭/切宿主语义:
/// - `HostChanged`:前台宿主切换(换绑成功且此前有面板归因)→ 接线层隐藏浮层,
///   但**保留 Dismissals**(Esc 屏蔽持续到该面板真正关闭,design §4);
/// - `Closed`:面板窗口销毁 → 隐藏浮层并**解除屏蔽**。
///
/// `Closed` 的宽口径(任意元素销毁即视为关闭)见 [`super::panel_detector`] 的
/// `on_ax_event` 现场注释。
#[derive(Debug, Clone, PartialEq)]
pub enum PanelEvent {
    /// 疑似保存面板出现。frame 为 AX 屏幕坐标 [x, y, w, h]:
    /// 原点在主屏左上角、y 向下(与 AppKit 左下原点相反——浮层定位换算在平台层做)。
    Shown { pid: i32, frame: [f64; 4] },
    /// 前台宿主切换(换绑成功且此前有面板归因)。接线层应隐藏浮层但保留 Esc 屏蔽。
    HostChanged { previous_pid: i32 },
    /// 面板窗口销毁。
    Closed,
}

/// 接线层对单个事件的决策(纯函数 [`decide`] 的输出)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 忽略(开关关闭/已显示/已屏蔽/daemon 离线)。
    Ignore,
    /// 打开浮层并定位到面板。
    Show,
    /// 隐藏浮层,但保留 Esc 屏蔽(宿主切换)。
    HideKeepBlock,
    /// 隐藏浮层并解除对该面板的屏蔽(面板销毁)。
    HideAndClear,
}

/// 事件 → 动作(纯函数;表驱动单测钉住全部分支)。
///
/// `Shown` 必须同时满足:助手开关开启、浮层未显示(重复 Shown 去重)、该宿主
/// 未被 Esc 屏蔽、daemon 在线;任一不满足即忽略。`HostChanged`/`Closed` 无条件
/// 走隐藏路径——可见性由状态机兜底,重复隐藏是幂等的。
pub fn decide(
    ev: &PanelEvent,
    enabled: bool,
    overlay_visible: bool,
    blocked: bool,
    daemon_online: bool,
) -> Decision {
    match ev {
        PanelEvent::Shown { .. } => {
            if enabled && !overlay_visible && !blocked && daemon_online {
                Decision::Show
            } else {
                Decision::Ignore
            }
        }
        PanelEvent::HostChanged { .. } => Decision::HideKeepBlock,
        PanelEvent::Closed => Decision::HideAndClear,
    }
}

/// Esc 屏蔽状态(design §4 不打扰规则:本面板存续内不再自动弹;面板关闭即清除)。
/// 键 = 宿主 pid(同一 app 同时至多一个模态保存面板)。
#[derive(Default)]
pub struct Dismissals {
    dismissed: HashSet<i32>,
}

impl Dismissals {
    pub fn dismiss(&mut self, pid: i32) {
        self.dismissed.insert(pid);
    }
    pub fn blocked(&self, pid: i32) -> bool {
        self.dismissed.contains(&pid)
    }
    pub fn clear(&mut self, pid: i32) {
        self.dismissed.remove(&pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown() -> PanelEvent {
        PanelEvent::Shown {
            pid: 42,
            frame: [100.0, 80.0, 640.0, 480.0],
        }
    }

    /// 决策表:Shown 四个门(开关/已显示/屏蔽/离线)任一不满足即忽略;
    /// HostChanged/Closed 无条件走隐藏路径。
    /// 突变「去掉任一门」→ 对应断言必红。
    #[test]
    fn decide_table() {
        // 全满足才弹。
        assert_eq!(decide(&shown(), true, false, false, true), Decision::Show);
        // 开关关闭。
        assert_eq!(
            decide(&shown(), false, false, false, true),
            Decision::Ignore
        );
        // 浮层已显示(重复 Shown 去重)。
        assert_eq!(decide(&shown(), true, true, false, true), Decision::Ignore);
        // 该宿主已被 Esc 屏蔽。
        assert_eq!(decide(&shown(), true, false, true, true), Decision::Ignore);
        // daemon 离线:不弹空浮层。
        assert_eq!(
            decide(&shown(), true, false, false, false),
            Decision::Ignore
        );
        // 多门同时不满足仍是忽略。
        assert_eq!(decide(&shown(), false, true, true, false), Decision::Ignore);

        let host = PanelEvent::HostChanged { previous_pid: 42 };
        assert_eq!(
            decide(&host, true, true, false, true),
            Decision::HideKeepBlock,
            "宿主切换须隐藏但保留屏蔽"
        );
        assert_eq!(
            decide(&host, false, false, true, false),
            Decision::HideKeepBlock,
            "宿主切换与开关/在线状态无关(幂等隐藏)"
        );

        assert_eq!(
            decide(&PanelEvent::Closed, true, true, false, true),
            Decision::HideAndClear,
            "面板销毁须隐藏并解除屏蔽"
        );
        assert_eq!(
            decide(&PanelEvent::Closed, false, false, false, false),
            Decision::HideAndClear
        );
    }

    /// Esc 屏蔽:dismiss 后本面板存续内 blocked,clear(面板关闭)后恢复。
    #[test]
    fn dismiss_blocks_until_cleared() {
        let mut d = Dismissals::default();
        assert!(!d.blocked(42));
        d.dismiss(42);
        assert!(d.blocked(42));
        d.clear(42);
        assert!(!d.blocked(42));
    }

    /// 屏蔽按宿主 pid 隔离:一个 app 的 Esc 不影响其他 app 的保存面板。
    #[test]
    fn dismissals_are_per_pid() {
        let mut d = Dismissals::default();
        d.dismiss(1);
        assert!(d.blocked(1));
        assert!(!d.blocked(2));
        d.clear(2); // 清除未屏蔽 pid 为 no-op
        assert!(d.blocked(1));
    }
}

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
/// - `Closed { pid }`:该 app 当前无保存面板 → 隐藏浮层并**按 pid 精确解除屏蔽**。
///   触发点有两种:① 前台 app 内窗口销毁(AX 销毁通知);② 换绑到该 app 后首扫
///   未命中——覆盖「面板在别的 app 前台期间关闭,detector 收不到销毁通知」的
///   屏蔽泄漏路径。事件携带 pid 而不是让接线层猜:宿主切换后归因可能已被其他
///   面板覆盖,「A 屏蔽 → 切 B → 切回 A」场景只有 pid 能正确清除 A 的屏蔽。
///
/// `Closed` 的宽口径(任意元素销毁即视为关闭)见 [`super::panel_detector`] 的
/// `on_ax_event` 现场注释;换绑首扫确认见 `rebind_and_scan`。
/// 两种触发点都幂等:重复 `Closed` 只重复 clear(无屏蔽 pid 为 no-op)。
#[derive(Debug, Clone, PartialEq)]
pub enum PanelEvent {
    /// 疑似保存面板出现。frame 为 AX 屏幕坐标 [x, y, w, h]:
    /// 原点在主屏左上角、y 向下(与 AppKit 左下原点相反——浮层定位换算在平台层做)。
    Shown { pid: i32, frame: [f64; 4] },
    /// 前台宿主切换(换绑成功且此前有面板归因)。接线层应隐藏浮层但保留 Esc 屏蔽。
    HostChanged { previous_pid: i32 },
    /// 该 app 当前无保存面板(面板销毁,或绑定后确认不存在;pid = 该 app 宿主)。
    Closed { pid: i32 },
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
    /// 隐藏浮层并解除 `pid` 的 Esc 屏蔽(该 app 当前无面板)。
    HideAndClear { pid: i32 },
}

/// 事件 → 动作(纯函数;表驱动单测钉住全部分支)。
///
/// `Shown` 必须同时满足:助手开关开启、浮层未显示(重复 Shown 去重)、该宿主
/// 未被 Esc 屏蔽、该面板未被本助手处理过(`handled`:commit 跳转后、面板关闭前
/// 不再自动弹)、daemon 在线;任一不满足即忽略。`HostChanged`/`Closed` 无条件
/// 走隐藏路径——可见性由状态机兜底,重复隐藏是幂等的。
pub fn decide(
    ev: &PanelEvent,
    enabled: bool,
    overlay_visible: bool,
    blocked: bool,
    handled: bool,
    daemon_online: bool,
) -> Decision {
    match ev {
        PanelEvent::Shown { .. } => {
            if enabled && !overlay_visible && !blocked && !handled && daemon_online {
                Decision::Show
            } else {
                Decision::Ignore
            }
        }
        PanelEvent::HostChanged { .. } => Decision::HideKeepBlock,
        PanelEvent::Closed { pid } => Decision::HideAndClear { pid: *pid },
    }
}

/// 有效开关 = 用户设置开启 **且** 辅助功能已授权。
///
/// 这是唯一真源:启动、设置切换、运行期授权跃迁都经此重算,避免三处各写
/// `save && trusted` 而产生偏差。
pub fn effective_enabled(save_panel_assist: bool, trusted: bool) -> bool {
    save_panel_assist && trusted
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

    /// 决策表:Shown 五个门(开关/已显示/屏蔽/已处理/离线)任一不满足即忽略;
    /// HostChanged/Closed 无条件走隐藏路径。
    /// 突变「去掉任一门」→ 对应断言必红。
    #[test]
    fn decide_table() {
        // 全满足才弹。
        assert_eq!(
            decide(&shown(), true, false, false, false, true),
            Decision::Show
        );
        // 开关关闭。
        assert_eq!(
            decide(&shown(), false, false, false, false, true),
            Decision::Ignore
        );
        // 浮层已显示(重复 Shown 去重)。
        assert_eq!(
            decide(&shown(), true, true, false, false, true),
            Decision::Ignore
        );
        // 该宿主已被 Esc 屏蔽。
        assert_eq!(
            decide(&shown(), true, false, true, false, true),
            Decision::Ignore
        );
        // commit 跳转后同一面板的重复 Shown(handled)不再弹。
        assert_eq!(
            decide(&shown(), true, false, false, true, true),
            Decision::Ignore
        );
        // daemon 离线:不弹空浮层。
        assert_eq!(
            decide(&shown(), true, false, false, false, false),
            Decision::Ignore
        );
        // 多门同时不满足仍是忽略。
        assert_eq!(
            decide(&shown(), false, true, true, true, false),
            Decision::Ignore
        );

        let host = PanelEvent::HostChanged { previous_pid: 42 };
        assert_eq!(
            decide(&host, true, true, false, false, true),
            Decision::HideKeepBlock,
            "宿主切换须隐藏但保留屏蔽"
        );
        assert_eq!(
            decide(&host, false, false, true, false, false),
            Decision::HideKeepBlock,
            "宿主切换与开关/在线状态无关(幂等隐藏)"
        );

        assert_eq!(
            decide(
                &PanelEvent::Closed { pid: 42 },
                true,
                true,
                false,
                true,
                true
            ),
            Decision::HideAndClear { pid: 42 },
            "面板销毁须隐藏并按事件 pid 精确解除屏蔽"
        );
        assert_eq!(
            decide(
                &PanelEvent::Closed { pid: 7 },
                false,
                false,
                false,
                false,
                false
            ),
            Decision::HideAndClear { pid: 7 },
            "pid 必须原样透传(宿主切换后归因可能已被其他面板覆盖)"
        );
    }

    /// 阻塞 1 语义:commit 后 handled 抑制重复 Shown;`Closed` 移除 handled 后
    /// 同一 pid 的新 Shown 重新允许(解除)。
    /// 突变「去掉 handled 门」→ 第一条 Ignore 断言必红。
    #[test]
    fn handled_suppresses_reshown_until_closed() {
        use std::collections::HashSet;

        let mut handled: HashSet<i32> = HashSet::new();
        handled.insert(42); // commit 时接线层插入
        assert_eq!(
            decide(&shown(), true, false, false, handled.contains(&42), true),
            Decision::Ignore,
            "commit 后的重复 Shown 不得复现浮层"
        );
        // Closed 到达:接线层 remove(pid)。
        handled.remove(&42);
        assert_eq!(
            decide(&shown(), true, false, false, handled.contains(&42), true),
            Decision::Show,
            "面板关闭后同 pid 的新 Shown 应重新允许"
        );
    }

    /// 有效开关 = 设置开启 && 已授权(唯一真源)。
    /// 突变「effective_enabled 去掉 trusted」→ (true,false) 用例必红。
    #[test]
    fn effective_enabled_table() {
        for (save, trusted, want) in [
            (false, false, false),
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert_eq!(
                effective_enabled(save, trusted),
                want,
                "effective_enabled(save={save}, trusted={trusted})"
            );
        }
    }

    /// 同一 pid 的 `Closed` 重复到达必须幂等:决策相同,接线层重复 clear 为
    /// no-op,且不得误清其他 pid 的屏蔽(跨 app 关闭路径会重复确认)。
    #[test]
    fn repeated_closed_is_idempotent_and_scoped() {
        let first = decide(
            &PanelEvent::Closed { pid: 5 },
            true,
            true,
            false,
            false,
            true,
        );
        let second = decide(
            &PanelEvent::Closed { pid: 5 },
            false,
            false,
            false,
            false,
            false,
        );
        assert_eq!(first, Decision::HideAndClear { pid: 5 });
        assert_eq!(first, second, "重复 Closed 决策不随开关/可见态变化");

        let mut d = Dismissals::default();
        d.dismiss(5);
        d.dismiss(6);
        // 接线层对每个 Closed 各 clear 一次;重复 clear 同一 pid 无害。
        d.clear(5);
        d.clear(5);
        assert!(!d.blocked(5));
        assert!(d.blocked(6), "不得误清其他 app 的屏蔽");
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

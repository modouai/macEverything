//! 保存面板检测:特征快照 dump(探针) + 两级判定(快速 subrole 白名单 →
//! 确认可编辑输入框/按钮组)。平台事件层接线在 Task 9。快照结构与 AX 树
//! 解耦,纯数据可序列化可测。
use super::ax_ffi::{self, attr, AXRef};

/// 窗口特征快照(判定输入;与 AX 树解耦,纯数据可序列化可测)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PanelSnapshot {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub title: Option<String>,
    /// 子树内可写 AXTextField 数(深度限 6)。
    pub editable_text_fields: usize,
    /// 子树内 AXButton 数。
    pub buttons: usize,
    /// 子树内文件浏览容器(AXScrollArea/AXSplitGroup)数。
    /// 当前仅取证字段,不参与 `is_save_panel`;特征表修订时评估是否纳入确认信号。
    pub browsers: usize,
}

/// 从 AX 窗口元素采集特征快照(浅层遍历;AX IPC 逐元素往返,探针/检测入口各一次)。
pub fn snapshot_of_window(win: &AXRef) -> PanelSnapshot {
    let mut s = PanelSnapshot {
        role: ax_ffi::attr_string(win, attr::ROLE),
        subrole: ax_ffi::attr_string(win, attr::SUBROLE),
        title: ax_ffi::attr_string(win, attr::TITLE),
        editable_text_fields: 0,
        buttons: 0,
        browsers: 0,
    };
    count_walk(win, 0, &mut s);
    s
}

fn count_walk(el: &AXRef, depth: usize, s: &mut PanelSnapshot) {
    match ax_ffi::attr_string(el, attr::ROLE).as_deref() {
        Some("AXTextField") if ax_ffi::attr_settable(el, attr::VALUE) => {
            s.editable_text_fields += 1
        }
        Some("AXButton") => s.buttons += 1,
        Some("AXScrollArea" | "AXSplitGroup") => s.browsers += 1,
        _ => {}
    }
    if depth >= 6 {
        return;
    }
    for c in ax_ffi::children(el) {
        count_walk(&c, depth + 1, s);
    }
}

/// M0 探针粗判:subrole 含 Dialog 或命中产品快速信号白名单即疑似
/// (取证用,宁滥勿缺——漏报比误报更伤取证)。白名单形态(尤其 AXFloatingWindow)
/// 不一定含 Dialog,故此处必须取并集,保证探针能对其 dump 快照并触发实验。
///
/// 与 [`is_save_panel`] 分工:本函数只看快速信号,用于探针决定是否触发实验;
/// 产品判定必须叠加确认信号(可编辑输入框 + 按钮组),零误报优先。
pub fn looks_like_panel(s: &PanelSnapshot) -> bool {
    s.subrole
        .as_deref()
        .is_some_and(|r| r.contains("Dialog") || SUBROLE_QUICK.contains(&r))
}

/// 快速信号:window subrole 白名单(设计推定 subrole 集合;M0 findings 回填后修订)。
/// 判定为「疑似」而非「确认」——确认必须叠加结构信号。
/// 注:AXStandardWindow 等普通窗口 subrole 不在列(防误报)。
const SUBROLE_QUICK: &[&str] = &["AXSystemDialog", "AXDialog", "AXFloatingWindow"];

/// 确认信号阈值:保存/打开面板必含可编辑文件名输入框与按钮组;
/// 普通对话框(如确认弹窗)无此组合。
const CONFIRM_EDITABLE_MIN: usize = 1;
const CONFIRM_BUTTONS_MIN: usize = 1;

/// 产品判定(设计 §3.1 两级):快速信号命中 **且** 确认信号满足。
/// 纯函数(无 AX 调用),特征表数据化供 macOS 版本漂移时修订。
#[allow(dead_code)] // Task 9 平台事件层接线后移除
pub fn is_save_panel(s: &PanelSnapshot) -> bool {
    let quick_ok = s
        .subrole
        .as_deref()
        .map(|r| SUBROLE_QUICK.contains(&r))
        .unwrap_or(false);
    quick_ok && s.editable_text_fields >= CONFIRM_EDITABLE_MIN && s.buttons >= CONFIRM_BUTTONS_MIN
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(subrole: &str, editable: usize, buttons: usize) -> PanelSnapshot {
        PanelSnapshot {
            role: Some("AXWindow".into()),
            subrole: Some(subrole.into()),
            title: Some("存储".into()),
            editable_text_fields: editable,
            buttons,
            browsers: 1,
        }
    }

    /// 契约:快速信号 + 确认信号双满足才判定;任一缺失为否。
    /// 突变「去掉 editable/button 确认(仅 subrole 判定)」→ 第三用例必红。
    #[test]
    fn quick_and_confirm_signals_both_required() {
        assert!(is_save_panel(&snap("AXSystemDialog", 1, 2)));
        assert!(
            !is_save_panel(&snap("AXStandardWindow", 1, 2)),
            "subrole 不中"
        );
        assert!(
            !is_save_panel(&snap("AXSystemDialog", 0, 2)),
            "无可写输入框"
        );
        assert!(!is_save_panel(&snap("AXSystemDialog", 1, 0)), "无按钮");
    }

    /// 标题不参与判定:打开面板标题为「打开」仍判定为是。
    /// 钉住不得按标题过滤;open/save 的区分不在判定函数职责内(设计 §3.2 触发范围:不区分)。
    #[test]
    fn title_not_part_of_judgment_open_title_still_matches() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.title = Some("打开".into());
        assert!(is_save_panel(&s));
    }

    /// subrole 缺失(个别 app 隐藏属性)→ 不判定,防误报。
    #[test]
    fn missing_subrole_never_matches() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.subrole = None;
        assert!(!is_save_panel(&s));
    }

    /// 探针粗判与产品判定的分工钉(Dialog 个案):粗判只看 subrole(取证宁滥),
    /// 产品判定必须双信号(零误报优先)。个案名不声称全局宽窄关系。
    #[test]
    fn probe_rough_judgment_wider_on_dialog_subroles() {
        let s = snap("AXDialog", 0, 0);
        assert!(looks_like_panel(&s), "粗判:subrole 命中即疑似");
        assert!(!is_save_panel(&s), "产品判定:缺确认信号不通过");
    }

    /// 不变量:探针覆盖面必须是产品快速信号白名单的超集——
    /// 白名单内每种形态(尤其 AXFloatingWindow)都必须能被 M0 探针 dump 快照并触发实验。
    #[test]
    fn probe_covers_every_product_quick_signal() {
        for subrole in SUBROLE_QUICK {
            assert!(looks_like_panel(&snap(subrole, 0, 0)), "{subrole}");
        }
    }

    /// 已登记风险:AXFloatingWindow + 可编辑输入框会命中产品判定,
    /// 而 Get Info/Inspector 窗口可能就是这种形态。
    /// M0 实测复核后决定:移除该白名单条目,或加严确认信号(例如让 browsers 参与判定)。
    #[test]
    fn floating_window_with_editable_field_currently_matches_pending_m0() {
        assert!(is_save_panel(&snap("AXFloatingWindow", 1, 1)));
    }
}

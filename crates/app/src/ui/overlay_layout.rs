//! 浮层定位纯几何(design §3.3,2026-11-05 修订):紧贴保存面板**下沿**——
//! 浮层顶 = 面板底(间隙 0),水平居中对齐面板;越界时两轴独立钳制进面板
//! 所在屏的安全区。输入输出均为 AppKit 屏幕坐标(frame = [x, y, w, h],
//! 原点左下、y 向上);AX 屏幕坐标(左上原点、y 向下)的换算由平台层完成。
//! 与 egui 无关:窗口位置由平台层 setFrame 设置。

/// 浮层 frame:panel = 保存面板 frame;screen = 面板**所在屏**的 **visibleFrame()**
/// (不含菜单栏/Dock),不得用 mainScreen().frame()——副屏选择与菜单栏/Dock 遮挡
/// 均由调用方负责。overlay_w/h = 期望浮层尺寸;屏幕安全区内缩 8。
///
/// **高度自适应**:面板下方到安全区下界的可用空间不足期望高度时,压缩浮层
/// 高度到可用值(下限 [`MIN_H`],保住输入框 + 两三行结果),始终紧贴面板下沿
/// 且不越屏;返回 frame 的高度是实际值,调用方须按返回尺寸设窗(不能只用
/// 期望尺寸 setFrameOrigin)。
///
/// 两轴独立钳制:仅越界的轴贴到安全区边缘,另一轴保持原定位;面板下沿低于
/// 安全区下界时,浮层取 [`MIN_H`] 并贴屏幕安全区下缘。
/// 屏幕比浮层窄/矮时取安全区左下角,让浮层溢出右/上侧(不 panic)。
///
/// # Panics
///
/// `screen` 原点(x/y)为 NaN 时 `f64::clamp` 会 panic;`panel`/`overlay` 尺寸
/// 为 NaN 则结果按 NaN 静默传播。调用方须保证有限值——detector 的
/// `valid_panel_frame` 已拦截 AX 读数。
pub fn overlay_frame(
    panel: [f64; 4],
    screen: [f64; 4],
    overlay_w: f64,
    overlay_h: f64,
) -> [f64; 4] {
    const EDGE: f64 = 8.0;
    let (sx, sy, sw, sh) = (screen[0], screen[1], screen[2], screen[3]);
    // 紧贴面板下沿:浮层顶 = 面板底(间隙 0),水平居中对齐面板。
    let x = panel[0] + panel[2] / 2.0 - overlay_w / 2.0;
    // 高度自适应:下方可用空间 = 面板底 → 安全区下界;不足则压缩,地板 MIN_H。
    let available = panel[1] - (sy + EDGE);
    let h = overlay_h.min(available.max(MIN_H));
    let y = panel[1] - h;
    // 钳制进屏幕安全区。屏幕比浮层窄/矮时 max 会小于 min,先抬到 min,
    // 否则 f64::clamp(min > max) 会 panic;此时浮层贴安全区左/下缘。
    let min_x = sx + EDGE;
    let max_x = (sx + sw - EDGE - overlay_w).max(min_x);
    let min_y = sy + EDGE;
    let max_y = (sy + sh - EDGE - h).max(min_y);
    let x = x.clamp(min_x, max_x);
    let y = y.clamp(min_y, max_y);
    [x, y, overlay_w, h]
}

/// 自适应高度下限:输入框 + 两三行结果(再小失去可用性;面板底已贴近屏幕底
/// 的退化场景浮层会叠在面板下部,仍优先保住「紧贴下沿」的方向感)。
const MIN_H: f64 = 120.0;

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> [f64; 4] {
        [0.0, 0.0, 1440.0, 900.0]
    }

    /// 紧贴面板下沿:浮层顶 = 面板底(间隙 0),水平居中对齐面板。
    /// 突变「去掉 -overlay_h(浮层叠在面板上)」或「去掉水平居中」→ 本用例必红。
    #[test]
    fn attaches_below_panel_bottom_tightly() {
        let f = overlay_frame([100.0, 400.0, 600.0, 400.0], screen(), 420.0, 300.0);
        let expected_x = 100.0 + 600.0 / 2.0 - 420.0 / 2.0; // 水平居中对齐面板
        assert_eq!([f[0], f[1]], [expected_x, 100.0]);
        assert_eq!(f[1] + f[3], 400.0, "浮层顶紧贴面板底(间隙 0)");
        assert_eq!([f[2], f[3]], [420.0, 300.0], "空间充足 = 期望全高");
    }

    /// 面板靠左、水平居中后左缘越界 → x 钳制到安全区左缘(留 8px);
    /// y 未越界保持紧贴。突变「删除 x 下界钳制」→ 本用例必红。
    #[test]
    fn clamps_x_when_panel_near_left_edge() {
        let f = overlay_frame([0.0, 400.0, 100.0, 300.0], screen(), 420.0, 300.0);
        assert_eq!([f[0], f[1]], [8.0, 100.0]);
    }

    /// 下方空间不足但 ≥ 高度下限:高度压缩到可用值,仍紧贴面板下沿且浮层底
    /// = 安全区下界(两端都贴)。突变「高度不压缩」→ 本用例必红。
    #[test]
    fn shrinks_height_when_space_below_is_tight() {
        let f = overlay_frame([100.0, 250.0, 600.0, 400.0], screen(), 420.0, 300.0);
        // 可用 = 250 - 8 = 242 ∈ [120, 300) → 高度压缩为 242,两端都贴。
        assert_eq!([f[0], f[1]], [190.0, 8.0]);
        assert_eq!(f[3], 242.0, "高度自适应:可用 242 → 高 242");
        assert_eq!(f[1] + f[3], 250.0, "压缩后仍紧贴面板下沿");
    }

    /// 面板底几乎贴着屏幕底(可用 < 高度下限)→ 取下限 120,浮层底部被安全区
    /// 下界钳住(叠在面板下部,保持下沿方向感)。突变「无下限」→ 本用例必红。
    #[test]
    fn min_height_floor_when_panel_hugs_screen_bottom() {
        let f = overlay_frame([100.0, 60.0, 600.0, 400.0], screen(), 420.0, 300.0);
        assert_eq!([f[0], f[1]], [190.0, 8.0]);
        assert_eq!(f[3], 120.0, "高度下限 120");
    }

    /// 面板靠右、水平居中后右缘越界 → x 钳制到安全区右缘(留 8px)。
    /// 突变「删除 x 上界钳制」→ 本用例必红。
    #[test]
    fn clamps_x_when_panel_near_right_edge() {
        let f = overlay_frame([1340.0, 400.0, 100.0, 300.0], screen(), 420.0, 300.0);
        assert_eq!([f[0], f[1]], [1012.0, 100.0]);
    }

    /// 负坐标屏幕(副屏在主屏左侧/下方):钳制以 screen 原点为界,不使用绝对 0。
    #[test]
    fn clamps_within_negative_origin_screen() {
        let screen = [-1920.0, -200.0, 1920.0, 1080.0];
        let f = overlay_frame([-400.0, -100.0, 600.0, 400.0], screen, 420.0, 300.0);
        // 可用 = -100 - (-192) = 92 < 下限 120 → 高度取下限,浮层底被安全区
        // 下界钳住(轻微叠面板下部,保住可用性)。
        assert!(f[0] >= -1920.0 + 8.0, "{f:?}");
        assert!(f[1] >= -200.0 + 8.0, "{f:?}");
        assert!(f[0] + 420.0 <= -1920.0 + 1920.0 - 8.0, "{f:?}");
        assert!(f[1] + f[3] <= -200.0 + 1080.0 - 8.0, "{f:?}");
        assert_eq!([f[0], f[1]], [-428.0, -192.0]);
        assert_eq!(f[3], 120.0);
    }

    /// 屏幕比浮层还小时不得 panic(f64::clamp 的 min > max 会 panic);
    /// 取安全区左下角,高度取下限。
    #[test]
    fn tiny_screen_does_not_panic() {
        let f = overlay_frame(
            [0.0, 0.0, 100.0, 100.0],
            [0.0, 0.0, 200.0, 150.0],
            420.0,
            300.0,
        );
        assert_eq!([f[0], f[1]], [8.0, 8.0]);
        assert_eq!(f[3], 120.0, "退化场景高度取下限");
    }
}

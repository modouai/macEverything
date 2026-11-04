//! 浮层定位纯几何(design §3.3):面板右侧优先 → 越界下方 → 再越界屏幕安全区钳制。
//! 输入输出均为 AppKit 屏幕坐标(frame = [x, y, w, h],原点左下、y 向上);
//! AX 屏幕坐标(左上原点、y 向下)的换算由平台层在 Task 13 完成。
//! 与 egui 无关:窗口位置由平台层 setFrame 设置。

/// 浮层 frame:panel = 保存面板 frame;screen = 面板**所在屏**的 **visibleFrame()**
/// (不含菜单栏/Dock),不得用 mainScreen().frame()——副屏选择与菜单栏/Dock 遮挡
/// 均由调用方负责。overlay_w/h = 浮层尺寸;边距:相邻面板 12、屏幕安全区内缩 8。
///
/// 两轴独立钳制:仅越界的轴贴到安全区边缘,另一轴保持原定位。
/// 屏幕比浮层窄/矮时取安全区左下角,让浮层溢出右/上侧(不 panic)。
///
/// # Panics
///
/// `screen` 原点(x/y)为 NaN 时 `f64::clamp` 会 panic;`panel`/`overlay` 尺寸
/// 为 NaN 则结果按 NaN 静默传播。调用方须保证有限值——detector 的
/// `valid_panel_frame` 已拦截 AX 读数。
#[allow(dead_code)] // Task 13 浮层接线后移除
pub fn overlay_frame(
    panel: [f64; 4],
    screen: [f64; 4],
    overlay_w: f64,
    overlay_h: f64,
) -> [f64; 4] {
    const GAP: f64 = 12.0;
    const EDGE: f64 = 8.0;
    let (sx, sy, sw, sh) = (screen[0], screen[1], screen[2], screen[3]);
    // 优先方案:贴面板右侧,顶部对齐(面板顶 = 浮层顶)。
    let mut x = panel[0] + panel[2] + GAP;
    let mut y = panel[1] + panel[3] - overlay_h;
    if x + overlay_w > sx + sw - EDGE {
        // 右侧放不下 → 面板下方,水平居中对齐面板。
        x = panel[0] + panel[2] / 2.0 - overlay_w / 2.0;
        y = panel[1] - GAP - overlay_h;
    }
    // 钳制进屏幕安全区。屏幕比浮层窄/矮时 max 会小于 min,先抬到 min,
    // 否则 f64::clamp(min > max) 会 panic;此时浮层贴安全区左/下缘。
    let min_x = sx + EDGE;
    let max_x = (sx + sw - EDGE - overlay_w).max(min_x);
    let min_y = sy + EDGE;
    let max_y = (sy + sh - EDGE - overlay_h).max(min_y);
    x = x.clamp(min_x, max_x);
    y = y.clamp(min_y, max_y);
    [x, y, overlay_w, overlay_h]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> [f64; 4] {
        [0.0, 0.0, 1440.0, 900.0]
    }

    /// 右侧放得下 → 面板右缘 + 12,顶部对齐(面板顶 = 浮层顶)。
    /// 突变「去掉右侧优先分支」→ 本用例必红。
    #[test]
    fn right_side_preferred() {
        let f = overlay_frame([100.0, 200.0, 600.0, 400.0], screen(), 420.0, 300.0);
        let x = 100.0 + 600.0 + 12.0;
        let y = 200.0 + 400.0 - 300.0; // 顶部对齐:浮层顶 = 面板顶
        assert_eq!([f[0], f[1]], [x, y]);
        assert_eq!([f[2], f[3]], [420.0, 300.0]);
    }

    /// 面板贴安全区顶部、右侧放得下 → 顶部对齐后仍须被 y 上界压回。
    /// 突变「删除 y 上界钳制」→ 本用例必红。
    #[test]
    fn clamps_against_top_edge() {
        let f = overlay_frame([100.0, 800.0, 600.0, 100.0], screen(), 420.0, 300.0);
        assert_eq!([f[0], f[1]], [712.0, 592.0]);
    }

    /// 右侧越界 → 面板下方(间隙 12),水平居中对齐面板。
    /// 突变「下方分支不居中」→ 本用例必红。
    #[test]
    fn falls_below_when_right_overflows() {
        let f = overlay_frame([900.0, 500.0, 600.0, 400.0], screen(), 420.0, 300.0);
        let cx = 900.0 + 600.0 / 2.0 - 420.0 / 2.0;
        let y = 500.0 - 12.0 - 300.0;
        assert_eq!([f[0], f[1]], [cx, y]);
    }

    /// 右侧越界落到面板下方,但下方水平居中后右缘仍越界 → x 上界钳制到
    /// 安全区右缘(留 8px);y 未越界,保持原定位。
    #[test]
    fn clamps_into_safe_area() {
        let f = overlay_frame([1100.0, 700.0, 400.0, 300.0], screen(), 420.0, 300.0);
        assert!(f[0] + 420.0 <= 1440.0 - 8.0, "右缘不出屏:{f:?}");
        assert!(f[1] >= 8.0, "下缘不出屏:{f:?}");
        assert!(f[0] >= 8.0, "左缘不出屏:{f:?}");
        assert!(f[1] + 300.0 <= 900.0 - 8.0, "上缘不出屏:{f:?}");
        assert_eq!([f[0], f[1]], [1012.0, 388.0]);
    }

    /// 负坐标屏幕(副屏在主屏左侧/下方):钳制以 screen 原点为界,不使用绝对 0。
    #[test]
    fn clamps_within_negative_origin_screen() {
        let screen = [-1920.0, -200.0, 1920.0, 1080.0];
        let f = overlay_frame([-400.0, -100.0, 600.0, 400.0], screen, 420.0, 300.0);
        assert!(f[0] >= -1920.0 + 8.0, "{f:?}");
        assert!(f[1] >= -200.0 + 8.0, "{f:?}");
        assert!(f[0] + 420.0 <= -1920.0 + 1920.0 - 8.0, "{f:?}");
        assert!(f[1] + 300.0 <= -200.0 + 1080.0 - 8.0, "{f:?}");
        assert_eq!([f[0], f[1]], [-428.0, -192.0]);
    }

    /// 屏幕比浮层还小时不得 panic(f64::clamp 的 min > max 会 panic);
    /// 取安全区左下角,让浮层溢出屏幕右侧/上方。
    #[test]
    fn tiny_screen_does_not_panic() {
        let f = overlay_frame(
            [0.0, 0.0, 100.0, 100.0],
            [0.0, 0.0, 200.0, 150.0],
            420.0,
            300.0,
        );
        assert_eq!([f[0], f[1]], [8.0, 8.0]);
        assert_eq!([f[2], f[3]], [420.0, 300.0]);
    }
}

//! 全局文本倍率（设置 · 字号）：只缩放文字，几何尺寸不变。
//!
//! 生产路径：启动时由 `App::new` 按 `settings.toml` 调用 [`set_scale`]；Task 5
//! 设置页改字号时再次调用并重装基础样式（`install_base_text_styles`）。各视口
//! 绘制期只读 [`scale`]。
//!
//! 测试路径：倍率真源是进程级原子，并行测试不得用 [`set_scale`] 制造非默认值；
//! 需要非默认倍率时用 [`with_test_scale`]（线程局部覆盖，闭包结束即还原）。

use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui;

/// 全局倍率（f32 位模式存放；0x3F80_0000 = 1.0f32）。
static SCALE_BITS: AtomicU32 = AtomicU32::new(0x3F80_0000);

// 测试期线程局部倍率覆盖（只影响当前测试线程；并行测试安全）。
#[cfg(test)]
thread_local! {
    static TEST_SCALE: std::cell::Cell<Option<f32>> = const { std::cell::Cell::new(None) };
}

/// 当前全局倍率。
///
/// 生产写入点仅两处：启动读取设置、Task 5 设置页改字号（均经 [`set_scale`]）。
/// `cfg(test)` 下若当前线程设置了 [`with_test_scale`] 覆盖，优先返回覆盖值。
pub fn scale() -> f32 {
    #[cfg(test)]
    {
        if let Some(scale) = TEST_SCALE.with(std::cell::Cell::get) {
            return scale;
        }
    }
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

/// 倍率口径收口：非有限值（NaN / ±∞）→ 1.0；其余 clamp 到 0.5–2.0。
pub fn sanitize_scale(s: f32) -> f32 {
    if s.is_finite() {
        s.clamp(0.5, 2.0)
    } else {
        1.0
    }
}

/// 设置全局倍率（与 `FontSize` 三档 0.9 / 1.0 / 1.15 同口径，越界收口）。
pub fn set_scale(s: f32) {
    SCALE_BITS.store(sanitize_scale(s).to_bits(), Ordering::Relaxed);
}

/// 纯倍率函数：`base × scale`。测试可直接调用（不触碰全局状态）。
pub fn scaled(base: f32, scale: f32) -> f32 {
    base * scale
}

/// 全局倍率下的字号（入参一律为 token 层的基准 pt）。
pub fn size(base: f32) -> f32 {
    scaled(base, scale())
}

/// 全局倍率下的 proportional 字体（token 基准 pt → FontId）。
pub fn id(base: f32) -> egui::FontId {
    egui::FontId::proportional(size(base))
}

/// 测试覆盖的还原守卫（闭包 panic 时也经 Drop 还原，防测试间污染）。
#[cfg(test)]
struct TestScaleGuard(Option<f32>);

#[cfg(test)]
impl Drop for TestScaleGuard {
    fn drop(&mut self) {
        TEST_SCALE.with(|cell| cell.set(self.0));
    }
}

/// 测试专用：闭包内把**当前线程**的倍率覆盖为 `scale`，结束后还原先前值。
///
/// 并行测试硬约束：需要非默认倍率的测试必须走这里，不得调用 [`set_scale`]
/// （后者写进程级原子，会影响同时运行的其他 UI 测试）。
#[cfg(test)]
pub(crate) fn with_test_scale<T>(scale: f32, f: impl FnOnce() -> T) -> T {
    let _guard = TestScaleGuard(TEST_SCALE.with(|cell| cell.replace(Some(sanitize_scale(scale)))));
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认倍率 1.0：只读默认原子，不写全局。
    #[test]
    fn default_scale_is_one() {
        assert_eq!(scale(), 1.0);
        assert_eq!(f32::from_bits(SCALE_BITS.load(Ordering::Relaxed)), 1.0);
    }

    /// 纯函数口径：1.0 / 1.15 / 0.9（与 `FontSize` 档位一致），不触碰全局状态。
    #[test]
    fn scaled_multiplies_base_by_factor() {
        assert_eq!(scaled(13.0, 1.0), 13.0);
        assert!((scaled(20.0, 1.15) - 23.0).abs() < 1e-5);
        assert!((scaled(11.0, 0.9) - 9.9).abs() < 1e-5);
    }

    /// 非默认倍率只经线程局部覆盖：闭包内生效，结束后回到原子默认值。
    #[test]
    fn size_and_id_follow_test_scale_then_restore() {
        with_test_scale(1.15, || {
            assert_eq!(size(10.0), 11.5);
            let id = id(10.0);
            assert_eq!(id.size, 11.5);
            assert_eq!(id.family, egui::FontFamily::Proportional);
        });
        assert_eq!(size(10.0), 10.0, "闭包结束必须还原全局倍率");
    }

    /// NaN / ±∞ 回落 1.0；正常值 clamp 到 0.5–2.0（纯函数，不写全局）。
    #[test]
    fn sanitize_scale_rejects_non_finite_and_clamps() {
        assert_eq!(sanitize_scale(1.0), 1.0);
        assert_eq!(sanitize_scale(1.15), 1.15);
        assert_eq!(sanitize_scale(0.1), 0.5);
        assert_eq!(sanitize_scale(3.0), 2.0);
        assert_eq!(sanitize_scale(f32::NAN), 1.0);
        assert_eq!(sanitize_scale(f32::INFINITY), 1.0);
        assert_eq!(sanitize_scale(f32::NEG_INFINITY), 1.0);
    }
}

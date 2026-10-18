//! 全局文本倍率（设置 · 字号）：只缩放文字，几何尺寸不变。

use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui;

/// 全局倍率（f32 位模式存放；0x3F80_0000 = 1.0f32）。
static SCALE_BITS: AtomicU32 = AtomicU32::new(0x3F80_0000);

/// 当前全局倍率（各视口共用；启动时由设置写入，运行期只读）。
pub fn scale() -> f32 {
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

/// 设置全局倍率（与 `FontSize` 三档 0.9 / 1.0 / 1.15 同口径；越界收口到 0.5–2.0）。
pub fn set_scale(s: f32) {
    SCALE_BITS.store(s.clamp(0.5, 2.0).to_bits(), Ordering::Relaxed);
}

/// 纯倍率函数：`base × scale`。测试直接调用，不触碰全局状态（并行安全）。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认倍率 1.0：只读，不改全局（并行 UI 测试共用同一份原子）。
    #[test]
    fn default_scale_is_one() {
        assert_eq!(scale(), 1.0);
        assert_eq!(f32::from_bits(SCALE_BITS.load(Ordering::Relaxed)), 1.0);
    }

    /// 纯函数口径：1.0 / 1.15（与 `FontSize` 档位一致），测试不触碰全局状态。
    #[test]
    fn scaled_multiplies_base_by_factor() {
        assert_eq!(scaled(13.0, 1.0), 13.0);
        assert!((scaled(20.0, 1.15) - 23.0).abs() < 1e-5);
        assert!((scaled(11.0, 0.9) - 9.9).abs() < 1e-5);
    }

    /// `size`/`id` 走当前全局倍率；测试只读不写，避免与其它 UI 测试竞态。
    #[test]
    fn size_and_id_follow_current_global_scale() {
        let base = 13.0;
        assert_eq!(size(base), scaled(base, scale()));
        let id = id(base);
        assert_eq!(id.size, size(base));
        assert_eq!(id.family, egui::FontFamily::Proportional);
    }
}

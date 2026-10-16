//! 平台能力 facade:悬浮条窗口的 macOS 补丁走这里。
//!
//! 真实实现集中在 `platform/mac.rs`(objc2;unsafe 逐处 SAFETY);
//! 生效配方与差分结论见 docs/specs/v2-ui/poc-results.md「必须的属性组合」,
//! 活样例见 scripts/poc/poc1b_nspanel/src/main.rs。
//!
//! 本任务(Task 9)只提供空壳,调用点在 `ui::App::logic`(每帧,
//! 与 PoC-1 续的「每帧核对、漂移即重施」同形态)。

pub mod mac;

/// 给悬浮条视口套平台补丁(NSPanel 改类、非激活样式位、层级与跨 Space 行为)。
///
/// 空壳:当前恒返回 `Ok(())`,不改变窗口;Task 14 填真实实现。
pub fn patch_palette_window(_frame: &eframe::Frame) -> anyhow::Result<()> {
    Ok(())
}

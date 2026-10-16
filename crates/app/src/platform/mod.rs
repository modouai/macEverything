//! 平台能力 facade:悬浮条窗口的 macOS 补丁走这里。
//!
//! 真实实现集中在 `platform/mac.rs`(objc2;unsafe 逐处 SAFETY);
//! 生效配方与差分结论见 docs/specs/v2-ui/poc-results.md「必须的属性组合」,
//! 活样例见 scripts/poc/poc1b_nspanel/src/main.rs。
//!
//! 非 macOS:同名空实现(本仓当前交付面仅 macOS;stub 只为保持 facade 可编译)。
//! 调用点:根视口 `logic`(每帧核对/唤起)、悬浮条视口回调(重锚)、`App::on_exit`(还原类)。

#[cfg(target_os = "macos")]
pub mod mac;

/// 悬浮条窗口标题(ui 的 ViewportBuilder 与平台层按标题定位原生窗,两处必须一致)。
pub const PALETTE_TITLE: &str = "palette";

/// 记录根窗句柄(创建期一次)。
pub fn attach_root(cc: &eframe::CreationContext<'_>) {
    #[cfg(target_os = "macos")]
    mac::attach_root(cc);
    #[cfg(not(target_os = "macos"))]
    let _ = cc;
}

/// 每帧核对:悬浮条窗首见即施补丁(五要素+两补丁);已接则核对漂移。
pub fn ensure_patched() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::ensure_patched()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
}

/// 唤起悬浮条:原生定位 + `orderFrontRegardless` + `makeKeyWindow`(不激活 app)。
pub fn show_palette() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::show_palette()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow::anyhow!("非 macOS:悬浮条原生唤起不可用"))
    }
}

/// 显示完整窗口(根窗):激活 app + 置前 + 成 key(常规 App 窗口语义)。
pub fn show_root_window() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::show_root_window()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow::anyhow!("非 macOS:根窗原生唤起不可用"))
    }
}

/// 高度变化时重锚(顶边钉住;悬浮条每次 pass 调用)。
pub fn reanchor() {
    #[cfg(target_os = "macos")]
    mac::reanchor();
}

/// 退出/销毁前还原窗口类(否则 winit drop → abort)。
pub fn revert_class() {
    #[cfg(target_os = "macos")]
    mac::revert_class();
}

/// 是否已接上悬浮条窗(诊断;未接上时调用方排下一帧完成接线)。
pub fn attached() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::attached()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

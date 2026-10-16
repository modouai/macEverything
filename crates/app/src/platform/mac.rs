//! macOS 平台补丁实现(objc2;unsafe 集中于此,SAFETY 逐处注释)。
//!
//! Task 14 填实现:悬浮条 NSWindow → 自定义 NSPanel 子类改类 +
//! `NSWindowStyleMaskNonactivatingPanel` 样式位 + Floating 层级 +
//! `CanJoinAllSpaces | FullScreenAuxiliary` 集合行为;显示走
//! `orderFrontRegardless`;关窗前 `object_setClass` 还原原类(否则退出即崩)。
//!
//! 验证结论(PoC-1 续,2026-10-16)与逐项差分见
//! docs/specs/v2-ui/poc-results.md;活样例:
//! scripts/poc/poc1b_nspanel/src/main.rs(`convert_and_configure`)。

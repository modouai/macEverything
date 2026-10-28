//! CLI 安装器(v2.5 Phase B:把 `macEverything` 命令行工具装进用户 PATH)。
//!
//! 本模块只承载纯逻辑与进程封装(可单测);安装动作/失败分类在 Task 7,
//! 设置页 UI 在 Task 8 接线。

pub mod path_env;

#[allow(unused_imports)]
// Task 8 接线后移除(重导出供安装动作/设置页取 PathSource,当前无消费点)
pub use path_env::PathSource;

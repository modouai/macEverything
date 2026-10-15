//! macEverything CLI:start/stop/query/status/exclude 五子命令(product §5)。
mod client;
mod daemon;
mod protocol;

use anyhow::{anyhow, Result};
use std::path::PathBuf;

fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME 未设置"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/macEverything"))
}

fn main() -> Result<()> {
    // 管道下游提前关闭(如 query ... | head)时按 Unix 惯例静默退出,而非 panic。
    // SAFETY:signal 注册无内存安全影响;进程生命周期内有效。
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = daemon::DaemonPaths {
        data_dir: data_dir()?,
    };
    match args.first().map(String::as_str) {
        Some("start") => daemon::serve(paths),
        Some("stop") => client::stop(&paths),
        Some("query") => {
            let q = args
                .get(1)
                .ok_or_else(|| anyhow!("用法: macEverything query \"关键词\""))?;
            client::query(&paths, q, 1000)
        }
        Some("status") => client::status(&paths),
        Some("exclude") => client::exclude(
            &paths,
            args.get(1).map(String::as_str).unwrap_or("list"),
            args.get(2).map(String::as_str),
        ),
        _ => {
            eprintln!(
                "macEverything — macOS 全盘文件名即时搜索\n\n\
                 用法:\n  \
                 macEverything start           启动常驻引擎(后台建索引)\n  \
                 macEverything stop            停止引擎(落盘后退出)\n  \
                 macEverything query \"关键词\"   查询(含语法:空格 AND、* ? 通配、path: case: whole:)\n  \
                 macEverything status          查看索引状态与权限缺口\n  \
                 macEverything exclude add|remove|list [绝对路径]  管理排除目录"
            );
            std::process::exit(2);
        }
    }
}

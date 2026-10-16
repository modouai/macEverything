//! macEverything CLI:start/stop/query/status/exclude 五子命令(product §5)。
mod client;
mod daemon;
mod protocol;

use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;

fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME 未设置"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/macEverything"))
}

/// 后台化:start 默认把引擎进程脱离终端(契约「后台运行」)。
/// 实现 = spawn 自身带 MCE_FOREGROUND=1,父进程退出;stdio 接 /dev/null
/// (日志走数据目录 daemon.log)。子进程由 launchd 接管,终端关闭不受影响。
/// 调试可用 `MCE_FOREGROUND=1 macEverything start` 前台跑。
fn spawn_background() -> Result<()> {
    let exe = std::env::current_exe().map_err(|e| anyhow!("定位自身可执行文件失败: {e}"))?;
    let mut child = std::process::Command::new(exe)
        .arg("start")
        .env("MCE_FOREGROUND", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("启动后台引擎失败: {e}"))?;
    // 短窗口探测:子进程秒退 = 单实例拒绝(已在运行)或启动失败——如实报出而非静默
    std::thread::sleep(std::time::Duration::from_millis(400));
    match child.try_wait() {
        Ok(Some(status)) => bail!(
            "引擎未启动(退出码 {status})。多半是已在运行(`macEverything status` 确认);\
             其他原因见数据目录 daemon.log"
        ),
        Ok(None) => {
            println!("引擎已在后台启动(pid {})。", child.id());
            println!("查看进度:macEverything status;查询:macEverything query \"关键词\"");
            Ok(())
        }
        Err(e) => bail!("检查后台进程失败: {e}"),
    }
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
        Some("start") => {
            if std::env::var_os("MCE_FOREGROUND").is_some() {
                daemon::serve(paths)
            } else {
                spawn_background()
            }
        }
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

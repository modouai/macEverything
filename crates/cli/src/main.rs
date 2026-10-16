//! macEverything CLI:start/stop/query/status/exclude 五子命令(product §5)。
//! 客户端能力(连接/离线降级/stat 读取)在 client crate;本文件只做参数分派与输出格式化。
mod daemon;

use anyhow::{anyhow, bail, Result};
use client::state;
use protocol::Gap;
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

/// 查询结果打印:离线来源先标注索引生成时刻(在线/离线共用同一结果格式)。
fn print_query(out: &client::QueryOutcome) {
    if let Some(built_at) = out.offline_built_at {
        println!(
            "[离线索引 · 生成于 {}] 执行 macEverything start 可获得实时结果",
            fmt_time(built_at)
        );
    }
    print_hits(&out.entries, out.total, out.elapsed_ms, out.stat_ms);
}

fn print_hits(entries: &[protocol::HitEntry], total: u32, elapsed_ms: f64, stat_ms: f64) {
    println!("共 {total} 项 · 匹配 {elapsed_ms:.1} ms · 读取信息 {stat_ms:.1} ms");
    for e in entries {
        // 契约:结果列表含名称、路径、大小、修改时间
        let size = e.size.map(human_size).unwrap_or_else(|| "-".into());
        let mtime = e.mtime.map(fmt_time).unwrap_or_else(|| "-".into());
        println!("  {mtime}  {size:>8}  {}  {}", e.name, e.path);
    }
    if total as usize > entries.len() {
        println!("(仅展示前 {} 条)", entries.len());
    }
}

fn print_status(
    state: &str,
    entry_count: u32,
    gaps: &[Gap],
    io_errors: u32,
    built_at: u64,
    index_file: &str,
    offline: bool,
) {
    let label = match state {
        state::BUILDING => "构建中",
        state::READY => "就绪",
        state::RECONCILING => "补齐中(停机窗口事件重放)",
        state::DEGRADED => "重扫中(全量兜底)",
        state::OFFLINE => "离线(daemon 未运行)",
        other => other,
    };
    println!("状态: {label}");
    println!("条目数: {entry_count}");
    println!("索引生成: {}", fmt_time(built_at));
    println!("索引文件: {index_file}");
    if io_errors > 0 {
        println!("IO 错误计数: {io_errors}");
    }
    if !gaps.is_empty() {
        println!("权限缺口({} 个目录,未收录):", gaps.len());
        for g in gaps.iter().take(20) {
            println!("  {}  ({})", g.path, g.reason);
        }
        if gaps.len() > 20 {
            println!("  … 其余 {} 个", gaps.len() - 20);
        }
    }
    if offline {
        println!("提示: 执行 macEverything start 恢复实时引擎。");
    }
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// unix 秒 → "YYYY-MM-DD HH:MM:SS UTC"(civil-from-days,Hinnant 算法;不引时区依赖)。
fn fmt_time(secs: u64) -> String {
    if secs == 0 {
        return "未知".into();
    }
    let days = (secs / 86400) as i64;
    let tod = secs % 86400;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

fn main() -> Result<()> {
    // 管道下游提前关闭(如 query ... | head)时按 Unix 惯例静默退出,而非 panic。
    // SAFETY:signal 注册无内存安全影响;进程生命周期内有效。
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = client::DaemonPaths {
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
        Some("stop") => {
            let n = client::stop(&paths)?;
            println!("已停止,最终条目数 {n}");
            Ok(())
        }
        Some("query") => {
            let q = args
                .get(1)
                .ok_or_else(|| anyhow!("用法: macEverything query \"关键词\""))?;
            let out = client::query(&paths, q, 1000)?;
            print_query(&out);
            Ok(())
        }
        Some("status") => {
            match client::status(&paths)? {
                client::StatusOutcome::Reported(r) => {
                    print_status(
                        &r.state,
                        r.entry_count,
                        &r.gaps,
                        r.io_error_count,
                        r.built_at,
                        &r.index_file,
                        r.offline,
                    );
                    if r.offline {
                        // 缺口清单只在 daemon 内存中:离线时显式说明,不静默留白
                        println!(
                            "权限缺口与 IO 错误计数:需 daemon 在线查看(执行 macEverything start)"
                        );
                    }
                }
                client::StatusOutcome::NoIndex => {
                    println!(
                        "状态: 无索引(daemon 未运行且无落盘索引);执行 macEverything start 开始建索引"
                    );
                }
            }
            Ok(())
        }
        Some("exclude") => {
            let out = client::exclude(
                &paths,
                args.get(1).map(String::as_str).unwrap_or("list"),
                args.get(2).map(String::as_str),
            )?;
            println!("排除目录({} 条):", out.excluded.len());
            for e in &out.excluded {
                println!("  {e}");
            }
            if !out.ok {
                bail!("{}", out.message.unwrap_or_else(|| "操作未生效".into()));
            }
            Ok(())
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_time_known_epochs() {
        assert_eq!(fmt_time(1_700_000_000), "2023-11-14 22:13:20 UTC");
        assert_eq!(fmt_time(86_400), "1970-01-02 00:00:00 UTC");
        assert_eq!(fmt_time(0), "未知");
    }

    #[test]
    fn human_size_bounds() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }
}

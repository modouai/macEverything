//! client:query/status/stop/exclude 的连接与离线降级(场景 5:daemon 不在时
//! query/status 直读落盘索引自答并标注,exclude/stop 报错提示)。
use crate::daemon::DaemonPaths;
use anyhow::{anyhow, bail, Result};
use engine::index::IndexView;
use engine::persist;
use protocol::{self, HitEntry, Request, Response};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// 结果上限(product 查询契约)。
const RESULT_CAP: u32 = 1000;

fn connect(paths: &DaemonPaths) -> Option<UnixStream> {
    let s = UnixStream::connect(paths.socket()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    Some(s)
}

fn call(stream: &mut UnixStream, req: &Request) -> Result<Response> {
    let bytes = protocol::encode(req)?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > protocol::MAX_MSG {
        bail!("响应超限({n} 字节)");
    }
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf)?;
    Ok(protocol::decode_response(&buf)?)
}

fn expect_ok(resp: Response) -> Result<Response> {
    match resp {
        Response::Error { message, .. } => bail!(message),
        other => Ok(other),
    }
}

pub fn stop(paths: &DaemonPaths) -> Result<()> {
    let Some(mut s) = connect(paths) else {
        bail!("daemon 未在运行(无需停止)");
    };
    match expect_ok(call(
        &mut s,
        &Request::Stop {
            v: protocol::PROTOCOL_VERSION,
        },
    )?)? {
        Response::Stop {
            ok: true,
            final_entry_count,
        } => {
            println!("已停止,最终条目数 {final_entry_count}");
            Ok(())
        }
        Response::Stop { ok: false, .. } => {
            bail!("daemon 已停止,但最终落盘失败(详情见数据目录 daemon.log)")
        }
        other => bail!("意外响应: {other:?}"),
    }
}

pub fn query(paths: &DaemonPaths, q: &str, limit: u32) -> Result<()> {
    let limit = limit.min(RESULT_CAP);
    if let Some(mut s) = connect(paths) {
        let resp = expect_ok(call(
            &mut s,
            &Request::Query {
                v: protocol::PROTOCOL_VERSION,
                query: q.into(),
                limit,
            },
        )?)?;
        match resp {
            Response::Query {
                entries,
                total,
                elapsed_ms,
                stat_ms,
            } => {
                print_hits(&entries, total, elapsed_ms, stat_ms);
                Ok(())
            }
            other => bail!("意外响应: {other:?}"),
        }
    } else {
        offline_query(paths, q, limit)
    }
}

fn offline_query(paths: &DaemonPaths, q: &str, limit: u32) -> Result<()> {
    let view = persist::load_mmap(&paths.index_file())
        .map_err(|e| anyhow!("daemon 未运行,且落盘索引不可用({e});先执行 macEverything start"))?;
    let plan = engine::query::parse(q).map_err(|_| anyhow!("空查询"))?;
    let out = engine::query::execute(view.as_ref(), &plan, limit as usize);
    let rows: Vec<(String, String)> = out
        .hits
        .iter()
        .map(|h| {
            (
                String::from_utf8_lossy(view.display_name(*h)).into_owned(),
                view.full_path_dyn(*h),
            )
        })
        .collect();
    // 与 daemon 在线路径共用同一并行按需读取:离线结果同样含 size/mtime
    // (product 查询契约不含「离线除外」豁免),口径与耗时字段一致。
    let (entries, stat_ms) = crate::daemon::stat_rows(rows);
    println!(
        "[离线索引 · 生成于 {}] 执行 macEverything start 可获得实时结果",
        fmt_time(view.meta.built_at)
    );
    print_hits(&entries, out.total, out.elapsed_ms, stat_ms);
    Ok(())
}

pub fn status(paths: &DaemonPaths) -> Result<()> {
    if let Some(mut s) = connect(paths) {
        match expect_ok(call(
            &mut s,
            &Request::Status {
                v: protocol::PROTOCOL_VERSION,
            },
        )?)? {
            Response::Status {
                state,
                entry_count,
                gaps,
                io_error_count,
                built_at,
                index_file,
                ..
            } => {
                print_status(
                    &state,
                    entry_count,
                    &gaps,
                    io_error_count,
                    built_at,
                    &index_file,
                    false,
                );
                Ok(())
            }
            other => bail!("意外响应: {other:?}"),
        }
    } else {
        offline_status(paths)
    }
}

fn offline_status(paths: &DaemonPaths) -> Result<()> {
    match persist::load_mmap(&paths.index_file()) {
        Ok(v) => {
            let live = v.entries().iter().filter(|e| e.is_live()).count() as u32;
            let gaps: Vec<protocol::Gap> = vec![];
            print_status(
                crate::daemon::state::OFFLINE,
                live,
                &gaps,
                0,
                v.meta.built_at,
                &paths.index_file().to_string_lossy(),
                true,
            );
            // 缺口清单只在 daemon 内存中:离线时显式说明,不静默留白
            println!("权限缺口与 IO 错误计数:需 daemon 在线查看(执行 macEverything start)");
            Ok(())
        }
        Err(_) => {
            println!("状态: 无索引(daemon 未运行且无落盘索引);执行 macEverything start 开始建索引");
            Ok(())
        }
    }
}

pub fn exclude(paths: &DaemonPaths, action: &str, arg: Option<&str>) -> Result<()> {
    let req = match action {
        "list" => Request::ExcludeList {
            v: protocol::PROTOCOL_VERSION,
        },
        "add" => {
            let p = arg.ok_or_else(|| anyhow!("用法: macEverything exclude add <绝对路径>"))?;
            Request::ExcludeAdd {
                v: protocol::PROTOCOL_VERSION,
                path: p.into(),
            }
        }
        "remove" => {
            let p = arg.ok_or_else(|| anyhow!("用法: macEverything exclude remove <绝对路径>"))?;
            Request::ExcludeRemove {
                v: protocol::PROTOCOL_VERSION,
                path: p.into(),
            }
        }
        other => bail!("未知动作 {other}(可用: add/remove/list)"),
    };
    let Some(mut s) = connect(paths) else {
        bail!("daemon 未在运行;排除变更需引擎在线,先执行 macEverything start");
    };
    match expect_ok(call(&mut s, &req)?)? {
        Response::Exclude {
            ok,
            excluded,
            message,
        } => {
            println!("排除目录({} 条):", excluded.len());
            for e in excluded {
                println!("  {e}");
            }
            if !ok {
                bail!("{}", message.unwrap_or_else(|| "操作未生效".into()));
            }
            Ok(())
        }
        other => bail!("意外响应: {other:?}"),
    }
}

fn print_hits(entries: &[HitEntry], total: u32, elapsed_ms: f64, stat_ms: f64) {
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
    gaps: &[protocol::Gap],
    io_errors: u32,
    built_at: u64,
    index_file: &str,
    offline: bool,
) {
    let label = match state {
        crate::daemon::state::BUILDING => "构建中",
        crate::daemon::state::READY => "就绪",
        crate::daemon::state::RECONCILING => "补齐中(停机窗口事件重放)",
        crate::daemon::state::DEGRADED => "重扫中(全量兜底)",
        crate::daemon::state::OFFLINE => "离线(daemon 未运行)",
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

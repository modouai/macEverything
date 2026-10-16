//! client:客户端承载层(ADR-7)——连接管理、socket 请求事务、状态常量、
//! 按需 stat 读取与离线降级(场景 5:daemon 不在时 query/status 直读落盘索引
//! 自答并标注,exclude/stop 报错提示)。
//! 本 crate 只回数据;格式化与打印由调用方(CLI/App)各自完成。
use anyhow::{anyhow, bail, Result};
use engine::index::IndexView;
use engine::persist;
use protocol::{self, Gap, HitEntry, Request, Response};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// 结果上限(product 查询契约)。
pub const RESULT_CAP: u32 = 1000;

/// 索引状态:daemon 四态(product §1 契约)+ 离线(客户端侧:daemon 未运行)。
pub mod state {
    pub const BUILDING: &str = "building";
    pub const READY: &str = "ready";
    pub const RECONCILING: &str = "reconciling";
    pub const DEGRADED: &str = "degraded";
    pub const OFFLINE: &str = "offline";
}

/// 运行时数据目录下的固定路径族(socket / 索引 / 配置 / 日志)。
#[derive(Clone)]
pub struct DaemonPaths {
    pub data_dir: PathBuf,
}

impl DaemonPaths {
    pub fn socket(&self) -> PathBuf {
        self.data_dir.join("daemon.sock")
    }
    pub fn index_file(&self) -> PathBuf {
        self.data_dir.join("index.bin")
    }
    pub fn config_file(&self) -> PathBuf {
        self.data_dir.join("config.toml")
    }
    pub fn log_file(&self) -> PathBuf {
        self.data_dir.join("daemon.log")
    }
}

/// 连接 daemon;连不上返回 None(调用方据此走离线降级)。
pub fn connect(paths: &DaemonPaths) -> Option<UnixStream> {
    let s = UnixStream::connect(paths.socket()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(30))).ok()?;
    s.set_write_timeout(Some(Duration::from_secs(5))).ok()?;
    Some(s)
}

/// 单次请求事务:编码 → 写 → 读长度头 → 读体 → 解码。
pub fn call(stream: &mut UnixStream, req: &Request) -> Result<Response> {
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

/// Error 响应转 anyhow 错误;其余响应原样返回。
pub fn expect_ok(resp: Response) -> Result<Response> {
    match resp {
        Response::Error { message, .. } => bail!(message),
        other => Ok(other),
    }
}

// —— 按需 stat 读取(daemon 在线查询与离线查询共用同一实现) ——

/// 读单个路径的 (size, mtime);不存在或不可读返回 (None, None)。
pub fn stat_of(path: &str) -> (Option<u64>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(m) => (
            Some(m.len()),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        ),
        Err(_) => (None, None),
    }
}

/// 结果行的 size/mtime 并行读取;返回 (条目, 读取耗时毫秒)。
/// daemon 在线查询与离线查询共用(product 契约口径一致)。
pub fn stat_rows(rows: Vec<(String, String)>) -> (Vec<HitEntry>, f64) {
    use rayon::prelude::*;
    let t = Instant::now();
    let entries: Vec<HitEntry> = rows
        .into_par_iter()
        .map(|(name, path)| {
            let (size, mtime) = stat_of(&path);
            HitEntry {
                name,
                path,
                size,
                mtime,
            }
        })
        .collect();
    (entries, t.elapsed().as_secs_f64() * 1000.0)
}

// —— 子命令数据面:query / status / stop / exclude ——

/// query 结果数据(在线应答与离线直读两种来源统一)。
#[derive(Debug)]
pub struct QueryOutcome {
    pub entries: Vec<HitEntry>,
    pub total: u32,
    pub elapsed_ms: f64,
    pub stat_ms: f64,
    /// 实际生效的排序键回显(relevance/name/size/mtime/path)。调用方比对请求键
    /// 与回显:不符(含旧 daemon 的空串)= 排序未生效,须提示而非静默展示错排序。
    pub sort_echo: String,
    /// Some(落盘索引生成时刻) = 离线直读落盘索引;None = daemon 在线应答。
    pub offline_built_at: Option<u64>,
}

/// 查询:daemon 在线走 socket;不在则直读落盘索引离线降级。
/// sort = 列排序键(name/size/mtime/path);None = relevance(CLI 与 v1 调用方语义)。
/// desc = 反转该键的自然方向(false = 自然方向,不发该字段——与 19b 前客户端同形);
/// 引擎在全量命中排序后按方向取前 K,total > K 时为真最大/最小 K 条。
pub fn query(
    paths: &DaemonPaths,
    q: &str,
    limit: u32,
    sort: Option<&str>,
    desc: bool,
) -> Result<QueryOutcome> {
    let limit = limit.min(RESULT_CAP);
    if let Some(mut s) = connect(paths) {
        let resp = expect_ok(call(
            &mut s,
            &Request::Query {
                v: protocol::PROTOCOL_VERSION,
                query: q.into(),
                limit,
                sort: sort.map(String::from),
                // false 不发送:None 与 false 引擎语义相同(自然方向),
                // 省字段让混跑期旧 daemon 收到的消息与旧客户端完全同形
                desc: desc.then_some(true),
            },
        )?)?;
        match resp {
            Response::Query {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                sort_echo,
            } => Ok(QueryOutcome {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                sort_echo,
                offline_built_at: None,
            }),
            other => bail!("意外响应: {other:?}"),
        }
    } else {
        offline_query(paths, q, limit)
    }
}

/// 离线查询:直读落盘索引自答(调用方须标注离线并给出索引生成时刻)。
/// 离线路径只做 relevance(无 daemon 侧排序轮 stat 缓存),回显如实为 "relevance":
/// 调用方请求了列排序时,可比对回显识别「排序未生效」。
pub fn offline_query(paths: &DaemonPaths, q: &str, limit: u32) -> Result<QueryOutcome> {
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
    let (entries, stat_ms) = stat_rows(rows);
    Ok(QueryOutcome {
        entries,
        total: out.total,
        elapsed_ms: out.elapsed_ms,
        stat_ms,
        sort_echo: "relevance".into(),
        offline_built_at: Some(view.meta.built_at),
    })
}

/// status 结果数据(在线应答或离线直读落盘索引)。
#[derive(Debug)]
pub struct StatusReport {
    pub state: String,
    pub entry_count: u32,
    pub gaps: Vec<Gap>,
    pub io_error_count: u32,
    pub built_at: u64,
    pub index_file: String,
    /// true = 离线直读落盘索引(daemon 未运行);缺口清单此时不可得。
    pub offline: bool,
}

/// status 数据面:在线应答,或离线降级。
#[derive(Debug)]
pub enum StatusOutcome {
    Reported(StatusReport),
    /// 离线且无落盘索引(从未建过索引)。
    NoIndex,
}

/// 状态:daemon 在线走 socket;不在则直读落盘索引离线降级。
pub fn status(paths: &DaemonPaths) -> Result<StatusOutcome> {
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
            } => Ok(StatusOutcome::Reported(StatusReport {
                state,
                entry_count,
                gaps,
                io_error_count,
                built_at,
                index_file,
                offline: false,
            })),
            other => bail!("意外响应: {other:?}"),
        }
    } else {
        offline_status(paths)
    }
}

/// 离线状态:直读落盘索引;不可用 = 无索引。
pub fn offline_status(paths: &DaemonPaths) -> Result<StatusOutcome> {
    match persist::load_mmap(&paths.index_file()) {
        Ok(v) => {
            let live = v.entries().iter().filter(|e| e.is_live()).count() as u32;
            let gaps: Vec<Gap> = vec![];
            Ok(StatusOutcome::Reported(StatusReport {
                state: state::OFFLINE.to_string(),
                entry_count: live,
                gaps,
                io_error_count: 0,
                built_at: v.meta.built_at,
                index_file: paths.index_file().to_string_lossy().into_owned(),
                offline: true,
            }))
        }
        Err(_) => Ok(StatusOutcome::NoIndex),
    }
}

/// 停止 daemon,返回最终条目数;落盘失败如实报错。
pub fn stop(paths: &DaemonPaths) -> Result<u32> {
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
        } => Ok(final_entry_count),
        Response::Stop { ok: false, .. } => {
            bail!("daemon 已停止,但最终落盘失败(详情见数据目录 daemon.log)")
        }
        other => bail!("意外响应: {other:?}"),
    }
}

/// 排除变更结果:ok=false 时 message 为可操作原因(调用方须如实报出)。
#[derive(Debug)]
pub struct ExcludeOutcome {
    pub ok: bool,
    pub excluded: Vec<String>,
    pub message: Option<String>,
}

/// 排除目录管理(list/add/remove);变更需引擎在线,离线直接报错。
pub fn exclude(paths: &DaemonPaths, action: &str, arg: Option<&str>) -> Result<ExcludeOutcome> {
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
        } => Ok(ExcludeOutcome {
            ok,
            excluded,
            message,
        }),
        other => bail!("意外响应: {other:?}"),
    }
}

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
    /// 用户主动 `stop` 的意图标记(CLI stop 成功后写入,CLI start 清除)。
    /// GUI 存活监控据此区分「崩溃退出」与「用户主动停止」,不把后者拉起。
    pub fn stop_intent_file(&self) -> PathBuf {
        self.data_dir.join("daemon.stop-intent")
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

/// 存活探测读超时:daemon 只回一个空 Pong,2s 足够;`connect` 的 30s 是给
/// 查询大响应用的,探测不适用(卡死的 daemon 不该让监控线程睡 30s)。
const PING_TIMEOUT: Duration = Duration::from_secs(2);

/// 最小存活探测:连接 → Ping → 期待 Pong。任何失败(连不上/超时/旧 daemon
/// 不识别 Ping 而断连/非 Pong 应答)→ false。
///
/// 必须发真实请求,不能 connect 后立即断开:后者会让 daemon 每轮记一条
/// 「连接处理失败」WARN(5s 一轮 ≈ 1.7 万行/天),而且探测连接若恰逢 Stop
/// 落盘,旧实现的「任意连接线程结束即 exit」会截断 `persist_final`。
/// daemon 侧同样只允许 Stop 所在线程退出进程(见 cli/daemon.rs `serve`)。
pub fn ping(paths: &DaemonPaths) -> bool {
    let Some(mut s) = connect(paths) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(PING_TIMEOUT));
    matches!(
        call(
            &mut s,
            &Request::Ping {
                v: protocol::PROTOCOL_VERSION
            }
        ),
        Ok(Response::Pong)
    )
}

// —— 按需 stat 读取(daemon 在线查询与离线查询共用同一实现) ——

/// 读单个路径的 (size, mtime, is_dir);不存在或不可读返回 (None, None, false)。
/// 一次 `metadata` 同时取三项,供 stat_rows 复用(不重复读文件系统)。
fn stat_meta(path: &str) -> (Option<u64>, Option<u64>, bool) {
    match std::fs::metadata(path) {
        Ok(m) => (
            Some(m.len()),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
            m.is_dir(),
        ),
        Err(_) => (None, None, false),
    }
}

/// 读单个路径的 (size, mtime);不存在或不可读返回 (None, None)。
pub fn stat_of(path: &str) -> (Option<u64>, Option<u64>) {
    let (size, mtime, _) = stat_meta(path);
    (size, mtime)
}

/// 结果行的 size/mtime/is_dir 并行读取;返回 (条目, 读取耗时毫秒)。
/// daemon 在线查询与离线查询共用(product 契约口径一致)。
pub fn stat_rows(rows: Vec<(String, String)>) -> (Vec<HitEntry>, f64) {
    use rayon::prelude::*;
    let t = Instant::now();
    let entries: Vec<HitEntry> = rows
        .into_par_iter()
        .map(|(name, path)| {
            let (size, mtime, is_dir) = stat_meta(&path);
            HitEntry {
                name,
                path,
                size,
                mtime,
                is_dir,
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
    /// 绝对方向回显(true = 降序, false = 升序, None = relevance 或旧 daemon)。
    pub desc_echo: Option<bool>,
    /// Some(落盘索引生成时刻) = 离线直读落盘索引;None = daemon 在线应答。
    pub offline_built_at: Option<u64>,
}

/// 查询失败(带响应错误码):UI 依码区分状态(如 `warming` = 索引未挂载,
/// 见 session.rs 六态表);CLI 按 message 展示。连接/离线路径失败无码(code=None)。
#[derive(Debug)]
pub struct QueryError {
    /// 引擎响应 Error.code;None = 连接建立/离线直读等无码失败。
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for QueryError {}

/// 查询:daemon 在线走 socket;不在则直读落盘索引离线降级。
/// sort = 列排序键(name/size/mtime/path);None = relevance(CLI 与 v1 调用方语义)。
/// desc = 反转该键的自然方向(false = 自然方向,不发该字段——与 19b 前客户端同形);
/// 引擎在全量命中排序后按方向取前 K,total > K 时为真最大/最小 K 条。
/// dir_only = 只返回目录(false 不过滤,与缺省同义,不发该字段——旧 daemon 兼容)。
pub fn query(
    paths: &DaemonPaths,
    q: &str,
    limit: u32,
    sort: Option<&str>,
    desc: bool,
    dir_only: bool,
) -> Result<QueryOutcome, QueryError> {
    let limit = limit.min(RESULT_CAP);
    let no_code = |e: anyhow::Error| QueryError {
        code: None,
        message: e.to_string(),
    };
    if let Some(mut s) = connect(paths) {
        let resp = call(
            &mut s,
            &Request::Query {
                v: protocol::PROTOCOL_VERSION,
                query: q.into(),
                limit,
                sort: sort.map(String::from),
                // false 不发送:None 与 false 引擎语义相同(自然方向),
                // 省字段让混跑期旧 daemon 收到的消息与旧客户端完全同形
                desc: desc.then_some(true),
                // false 不发送:None 与 false 引擎语义相同(不过滤),
                // 省字段让混跑期旧 daemon 收到的消息与旧客户端完全同形
                dir_only: dir_only.then_some(true),
                include_hidden: None,
            },
        )
        .map_err(no_code)?;
        match resp {
            Response::Query {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                sort_echo,
                desc_echo,
            } => Ok(QueryOutcome {
                entries,
                total,
                elapsed_ms,
                stat_ms,
                sort_echo,
                desc_echo,
                offline_built_at: None,
            }),
            // Error 不并进 expect_ok:错误码须上行到 UI(状态呈现按码分流)
            Response::Error { code, message } => Err(QueryError {
                code: Some(code),
                message,
            }),
            other => Err(QueryError {
                code: None,
                message: format!("意外响应: {other:?}"),
            }),
        }
    } else {
        offline_query(paths, q, limit, dir_only).map_err(no_code)
    }
}

/// 离线查询:直读落盘索引自答(调用方须标注离线并给出索引生成时刻)。
/// dir_only = 只返回目录;false 不过滤。
/// 离线路径只做 relevance(无 daemon 侧排序轮 stat 缓存),回显如实为 "relevance":
/// 调用方请求了列排序时,可比对回显识别「排序未生效」。
pub fn offline_query(
    paths: &DaemonPaths,
    q: &str,
    limit: u32,
    dir_only: bool,
) -> Result<QueryOutcome> {
    let view = persist::load_mmap(&paths.index_file())
        .map_err(|e| anyhow!("daemon 未运行,且落盘索引不可用({e});先执行 macEverything start"))?;
    let mut plan = engine::query::parse(q).map_err(|_| anyhow!("空查询"))?;
    plan.dir_only = dir_only;
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
        // 离线只做 relevance:无方向可言(与 sort_echo 同口径如实回显)
        desc_echo: None,
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

/// status 失败(带错误码);连接路径失败无码,离线索引不可读带 `index_unreadable`。
#[derive(Debug)]
pub struct StatusError {
    pub code: Option<String>,
    pub message: String,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StatusError {}

/// 状态:daemon 在线走 socket;不在则直读落盘索引离线降级。
/// `Response::Error` 的错误码须保留上行(UI/CLI 按码分流),不经 expect_ok。
pub fn status(paths: &DaemonPaths) -> Result<StatusOutcome, StatusError> {
    let no_code = |e: anyhow::Error| StatusError {
        code: None,
        message: e.to_string(),
    };
    if let Some(mut s) = connect(paths) {
        let resp = call(
            &mut s,
            &Request::Status {
                v: protocol::PROTOCOL_VERSION,
            },
        )
        .map_err(no_code)?;
        match resp {
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
            Response::Error { code, message } => Err(StatusError {
                code: Some(code),
                message,
            }),
            other => Err(StatusError {
                code: None,
                message: format!("意外响应: {other:?}"),
            }),
        }
    } else {
        offline_status(paths)
    }
}

/// 离线状态:直读落盘索引。索引文件不存在 = 无索引(从未建过);
/// 存在但读不出(损坏/权限)如实报 `index_unreadable`,不得伪装成无索引。
pub fn offline_status(paths: &DaemonPaths) -> Result<StatusOutcome, StatusError> {
    let index = paths.index_file();
    match std::fs::metadata(&index) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StatusOutcome::NoIndex),
        Err(e) => {
            return Err(StatusError {
                code: Some("index_unreadable".into()),
                message: format!("索引不可读({}): {e}", index.display()),
            })
        }
        Ok(_) => {}
    }
    match persist::load_mmap(&index) {
        Ok(v) => {
            let live = v
                .entry_segments()
                .iter()
                .map(|(_, s)| s.iter().filter(|e| e.is_live()).count() as u32)
                .sum::<u32>();
            let gaps: Vec<Gap> = vec![];
            Ok(StatusOutcome::Reported(StatusReport {
                state: state::OFFLINE.to_string(),
                entry_count: live,
                gaps,
                io_error_count: 0,
                built_at: v.meta.built_at,
                index_file: index.to_string_lossy().into_owned(),
                offline: true,
            }))
        }
        Err(e) => Err(StatusError {
            code: Some("index_unreadable".into()),
            message: format!("索引损坏或不可读({}): {e}", index.display()),
        }),
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

/// 排除目录管理(list/add/remove/reset);变更需引擎在线,离线直接报错。
/// `reset` 无参:恢复引擎内置默认排除项,一次重扫生效(旧 daemon 会拒绝,
/// 须以新 CLI 重启引擎)。
pub fn exclude(paths: &DaemonPaths, action: &str, arg: Option<&str>) -> Result<ExcludeOutcome> {
    let req = match action {
        "list" => Request::ExcludeList {
            v: protocol::PROTOCOL_VERSION,
        },
        "reset" => {
            if arg.is_some() {
                bail!("用法: macEverything exclude reset(无参数;恢复内置默认排除项)");
            }
            Request::ExcludeReset {
                v: protocol::PROTOCOL_VERSION,
            }
        }
        "add" => {
            let p = arg.ok_or_else(|| anyhow!("用法: macEverything exclude add <绝对路径>"))?;
            Request::ExcludeAdd {
                v: protocol::PROTOCOL_VERSION,
                path: p.into(),
            }
        }
        "remove" => {
            let p =
                arg.ok_or_else(|| anyhow!("用法: macEverything exclude remove <已存在的排除项>"))?;
            Request::ExcludeRemove {
                v: protocol::PROTOCOL_VERSION,
                path: p.into(),
            }
        }
        other => bail!("未知动作 {other}(可用: add/remove/list/reset)"),
    };
    let Some(mut s) = connect(paths) else {
        bail!("daemon 未在运行;排除变更需引擎在线,先执行 macEverything start");
    };
    // 旧 daemon 不认识新增的 exclude_reset 变体:解码失败后直接断连,客户端只会
    // 看到 EOF/解码错误。仅对 reset 附加可操作提示(离线/其他动作文案保持原样)。
    let resp = match call(&mut s, &req) {
        Ok(resp) => resp,
        Err(err) if action == "reset" => bail!(
            "{err:#};若引擎为旧版本,请停止并重新启动引擎(macEverything stop && macEverything start)"
        ),
        Err(err) => return Err(err),
    };
    match expect_ok(resp)? {
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

/// 查询运行中引擎的版本(升级路径偏差检测)。
/// 离线/失败/旧引擎(响应缺字段)→ `None`,调用方按「需重启」处理。
pub fn daemon_version(paths: &DaemonPaths) -> Option<String> {
    let mut s = connect(paths)?;
    match call(
        &mut s,
        &Request::Status {
            v: protocol::PROTOCOL_VERSION,
        },
    ) {
        Ok(resp) => match resp {
            Response::Status { daemon_version, .. } if !daemon_version.is_empty() => {
                Some(daemon_version)
            }
            _ => None,
        },
        Err(_) => None,
    }
}

/// 卷偏好结果(external-volumes):message 为落盘失败等可操作提示(成功为 None)。
#[derive(Debug)]
pub struct VolumePrefsOutcome {
    pub auto: bool,
    pub excluded: Vec<String>,
    pub message: Option<String>,
}

/// 卷偏好读/写共用管道;旧 daemon 不认识卷变体会直接断连,客户端只看到
/// EOF/解码错误——调用方(设置页)按操作失败如实展示。
fn volumes_call(paths: &DaemonPaths, req: Request) -> Result<VolumePrefsOutcome> {
    let Some(mut s) = connect(paths) else {
        bail!("daemon 未在运行;卷偏好需引擎在线,先执行 macEverything start");
    };
    // 版本偏差可操作提示:旧引擎不认识卷变体,解码失败即断连(客户端只看到
    // EOF/解码错误)。与 exclude reset 同款兜底文案。
    let resp = call(&mut s, &req).map_err(|err| {
        anyhow!(
            "{err:#};若引擎为旧版本,请重启引擎:macEverything stop && macEverything start(或托盘拉起)"
        )
    })?;
    match expect_ok(resp)? {
        Response::VolumesPrefs {
            auto,
            excluded,
            message,
        } => Ok(VolumePrefsOutcome {
            auto,
            excluded,
            message,
        }),
        other => bail!("意外响应: {other:?}"),
    }
}

/// 读当前卷偏好(设置页回显)。
pub fn volumes_prefs(paths: &DaemonPaths) -> Result<VolumePrefsOutcome> {
    volumes_call(
        paths,
        Request::VolumesPrefs {
            v: protocol::PROTOCOL_VERSION,
        },
    )
}

/// 写卷偏好(主开关 + 不索引卷名列表);daemon 落盘后卷监听 ≤3s 对账生效
/// (关主开关即清除全部卷条目,重开即立即索引放行卷)。
pub fn set_volume_prefs(
    paths: &DaemonPaths,
    auto: bool,
    excluded: Vec<String>,
) -> Result<VolumePrefsOutcome> {
    volumes_call(
        paths,
        Request::VolumesSet {
            v: protocol::PROTOCOL_VERSION,
            auto,
            excluded,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单连接桩 daemon:读一次请求(4 字节长度头 + JSON 体),回固定 JSON 响应。
    /// 句柄持有临时目录与后台线程,须绑定到变量活到测试结束。
    struct StubDaemon {
        _dir: tempfile::TempDir,
        _handle: std::thread::JoinHandle<()>,
    }

    fn stub_daemon(body: &[u8]) -> (DaemonPaths, StubDaemon) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let body = body.to_vec();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let n = u32::from_le_bytes(len) as usize;
            let mut req = vec![0u8; n];
            s.read_exact(&mut req).unwrap();
            let mut out = (body.len() as u32).to_le_bytes().to_vec();
            out.extend_from_slice(&body);
            s.write_all(&out).unwrap();
            s.flush().unwrap();
        });
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        (
            paths,
            StubDaemon {
                _dir: dir,
                _handle: handle,
            },
        )
    }

    /// 存活探测:daemon 回 Pong → true;无 daemon → false(不 panic、不阻塞)。
    #[test]
    fn ping_true_on_pong_and_false_without_daemon() {
        let (paths, _h) = stub_daemon(br#"{"msg":"pong"}"#);
        assert!(ping(&paths), "Pong 应答应判定为在线");

        let dir = tempfile::tempdir().unwrap();
        let absent = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        assert!(!ping(&absent), "socket 不存在应判定为不可连");
    }

    /// Error 应答(如旧 daemon 版本不匹配)不是 Pong:探测须判 false,
    /// 不能把「连上了但答错」当成健康。
    #[test]
    fn ping_false_on_error_response() {
        let (paths, _h) = stub_daemon(br#"{"msg":"error","code":"version","message":"bad"}"#);
        assert!(!ping(&paths));
    }

    /// 桩 daemon 变体:捕获请求体(供线路三态断言)。
    fn stub_daemon_capturing(
        body: &[u8],
    ) -> (
        DaemonPaths,
        StubDaemon,
        std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("daemon.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let body = body.to_vec();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap = captured.clone();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let n = u32::from_le_bytes(len) as usize;
            let mut req = vec![0u8; n];
            s.read_exact(&mut req).unwrap();
            *cap.lock().unwrap() = req;
            let mut out = (body.len() as u32).to_le_bytes().to_vec();
            out.extend_from_slice(&body);
            s.write_all(&out).unwrap();
            s.flush().unwrap();
        });
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        (
            paths,
            StubDaemon {
                _dir: dir,
                _handle: handle,
            },
            captured,
        )
    }

    /// 方向回显须原样读响应:真实 daemon 对 mtime + 协议 desc 位 true 回显
    /// 绝对方向 false(mtime 自然降序,desc 位反转后为升序)。桩故意回与请求位
    /// 相反的值——若实现拿请求 desc 冒充回显(而非读响应),此断言必红。
    #[test]
    fn query_passes_desc_echo_through() {
        let (paths, _h) = stub_daemon(
            br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0,"sort_echo":"mtime","desc_echo":false}"#,
        );
        let out = query(&paths, "x", 10, Some("mtime"), true, false).unwrap();
        assert_eq!(out.desc_echo, Some(false));
        assert_eq!(out.sort_echo, "mtime");
    }

    /// dir_only 线路三态钉:true 发送 `"dir_only":true`;false 不发送该键
    /// (None/Some(false) 引擎同义,省字段让混跑期报文与旧客户端同形)。
    /// 突变 `then_some(true)` → `then_some(false)` 时 true 分支必红。
    #[test]
    fn query_sends_dir_only_only_when_true() {
        for (dir_only, want_key) in [(true, true), (false, false)] {
            let (paths, _h, captured) = stub_daemon_capturing(
                br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0,"sort_echo":"relevance","desc_echo":null}"#,
            );
            let out = query(&paths, "x", 10, None, false, dir_only).unwrap();
            assert!(out.entries.is_empty());
            let body = String::from_utf8(captured.lock().unwrap().clone()).unwrap();
            assert_eq!(
                body.contains(r#""dir_only":true"#),
                want_key,
                "dir_only={dir_only} 时请求体: {body}"
            );
            if !want_key {
                assert!(
                    !body.contains("dir_only"),
                    "false 不得上线(三态收敛为缺省): {body}"
                );
            }
        }
    }

    /// status 失败须保留 daemon 错误码(UI/CLI 按码分流);不得经 expect_ok 丢码。
    #[test]
    fn status_keeps_daemon_error_code() {
        let (paths, _h) = stub_daemon(br#"{"msg":"error","code":"version","message":"bad"}"#);
        let err = status(&paths).unwrap_err();
        assert_eq!(err.code.as_deref(), Some("version"));
        assert_eq!(err.message, "bad");
    }

    /// 在线 status 成功应答须正确映射(重构掉 expect_ok 后的在线分支钉):
    /// offline 恒 false,状态/条目数/缺口逐字段透传。
    #[test]
    fn status_online_maps_reported_fields() {
        let (paths, _h) = stub_daemon(
            br#"{"msg":"status","state":"ready","entry_count":42,"gaps":[{"path":"/x","reason":"permission"}],"io_error_count":1,"built_at":123,"index_file":"/d/index.bin"}"#,
        );
        match status(&paths).unwrap() {
            StatusOutcome::Reported(r) => {
                assert!(!r.offline, "在线应答不得标离线");
                assert_eq!(r.state, "ready");
                assert_eq!(r.entry_count, 42);
                assert_eq!(r.gaps.len(), 1);
                assert_eq!(r.gaps[0].path, "/x");
                assert_eq!(r.gaps[0].reason, "permission");
                assert_eq!(r.io_error_count, 1);
                assert_eq!(r.built_at, 123);
                assert_eq!(r.index_file, "/d/index.bin");
            }
            StatusOutcome::NoIndex => panic!("在线成功应答不应是 NoIndex"),
        }
    }

    /// 索引文件从未存在 = 无索引(NoIndex),不是错误。
    #[test]
    fn offline_status_missing_index_is_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        match offline_status(&paths).unwrap() {
            StatusOutcome::NoIndex => {}
            other => panic!("期望 NoIndex,得到 {other:?}"),
        }
    }

    /// 落盘索引存在但损坏 ≠ 无索引:只有文件不存在才是 NoIndex,
    /// 损坏须带 index_unreadable 码如实报错(否则调用方谎报「从未建索引」)。
    #[test]
    fn offline_status_corrupt_index_is_not_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        std::fs::write(paths.index_file(), b"not-an-index").unwrap();
        let err = offline_status(&paths).unwrap_err();
        assert_eq!(err.code.as_deref(), Some("index_unreadable"));
    }

    /// stat 本身失败(ENOTDIR:data_dir 是普通文件)属「不可读」,
    /// 不得折叠成 NoIndex——只有 NotFound 才是无索引。
    #[test]
    fn offline_status_stat_error_is_not_no_index() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("not-a-dir");
        std::fs::write(&not_a_dir, b"x").unwrap();
        let paths = DaemonPaths {
            data_dir: not_a_dir,
        };
        let err = offline_status(&paths).unwrap_err();
        assert_eq!(err.code.as_deref(), Some("index_unreadable"));
    }

    /// remove 缺参提示须反映 E1 语义:已有相对名条目也可移除(不再写「绝对路径」)。
    /// 该分支在连接 daemon 前返回,不会触发真实 client/daemon。
    #[test]
    fn exclude_remove_usage_allows_existing_relative_names() {
        let paths = DaemonPaths {
            data_dir: PathBuf::from("/nonexistent-mace-test"),
        };
        let err = exclude(&paths, "remove", None).unwrap_err().to_string();
        assert!(err.contains("已存在的排除项"), "{err}");
        assert!(!err.contains("绝对路径"), "相对名条目也可移除:{err}");
    }

    /// reset 明确无参:带参数走用法错误;无参无 daemon 才走连接失败路径。
    /// 未知动作提示须把 reset 列为可用动作。请求构造由 protocol 往返测试覆盖。
    #[test]
    fn exclude_reset_usage_and_unknown_action_help() {
        let paths = DaemonPaths {
            data_dir: PathBuf::from("/nonexistent-mace-test"),
        };
        let err = exclude(&paths, "reset", Some("ignored"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("无参数"), "reset 不接受参数:{err}");
        assert!(!err.contains("daemon 未在运行"), "参数校验先于连接:{err}");

        let err = exclude(&paths, "reset", None).unwrap_err().to_string();
        assert!(err.contains("daemon 未在运行"), "{err}");
        assert!(!err.contains("未知动作"), "reset 不得被当成未知动作:{err}");
        assert!(
            !err.contains("若引擎为旧版本"),
            "离线错误不得附加旧 daemon 提示:{err}"
        );

        let err = exclude(&paths, "bogus", None).unwrap_err().to_string();
        assert!(
            err.contains("add/remove/list/reset"),
            "未知动作帮助须包含 reset:{err}"
        );
    }

    /// 旧 daemon 解码失败会直接断连(无响应):reset 客户端须把 EOF/IO 错误
    /// 转成可操作的「重启引擎」提示;桩服务读完请求即关闭模拟该行为。
    #[test]
    fn exclude_reset_old_daemon_error_adds_restart_hint() {
        use std::io::Read;

        let dir =
            std::env::temp_dir().join(format!("mce-exclude-reset-old-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("daemon.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut len = [0u8; 4];
            s.read_exact(&mut len).unwrap();
            let n = u32::from_le_bytes(len) as usize;
            let mut buf = vec![0u8; n];
            s.read_exact(&mut buf).unwrap();
            // 模拟旧 daemon 解码未知变体失败:不回响应直接关闭连接。
        });

        let paths = DaemonPaths {
            data_dir: dir.clone(),
        };
        let err = exclude(&paths, "reset", None).unwrap_err().to_string();
        handle.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            err.contains("若引擎为旧版本,请停止并重新启动引擎"),
            "旧 daemon 失败须给出可操作提示:{err}"
        );
    }

    /// stat_rows 必须从同一次 metadata 同时填 size/mtime/is_dir:
    /// 目录 true、文件 false、不存在路径缺省 false(size/mtime 为 None)。
    #[test]
    fn stat_rows_fills_is_dir_with_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, b"hello").unwrap();
        let missing = dir.path().join("missing.txt");

        let rows = vec![
            ("sub".to_string(), sub.to_string_lossy().into_owned()),
            ("file.txt".to_string(), file.to_string_lossy().into_owned()),
            (
                "missing.txt".to_string(),
                missing.to_string_lossy().into_owned(),
            ),
        ];
        let (entries, _) = stat_rows(rows);

        let sub = entries.iter().find(|e| e.name == "sub").unwrap();
        assert!(sub.is_dir, "目录应标为 is_dir=true");

        let file = entries.iter().find(|e| e.name == "file.txt").unwrap();
        assert!(!file.is_dir, "文件应标为 is_dir=false");
        assert_eq!(file.size, Some(5));
        assert!(file.mtime.is_some(), "文件应取到 mtime");

        let missing = entries.iter().find(|e| e.name == "missing.txt").unwrap();
        assert!(!missing.is_dir, "不存在路径缺省按文件");
        assert_eq!((missing.size, missing.mtime), (None, None));
    }
}

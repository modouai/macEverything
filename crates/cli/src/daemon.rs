//! daemon:引擎宿主——冷启动编排(秒开 → 物化 → 重放/重扫)+ 增量循环 + 服务循环。
//! 单实例:绑定 EADDRINUSE 时 connect 探测——连得上 = 已在运行;连不上 = 崩溃
//! 残留 stale socket,unlink 重绑。stop 经 socket 消息触发,响应在最终落盘后返回。
use crate::protocol::{self, Gap, HitEntry, Request, Response};
use anyhow::{bail, Context, Result};
use engine::config;
use engine::entry::NO_PARENT;
use engine::fsevents::{self, FseStream};
use engine::index::{Index, IndexView};
use engine::persist::{self, IndexMeta, SharedView};
use engine::reconcile;
use engine::watcher::{self, DirBatcher};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 停机时长超过此阈值且重放零事件 → 判重放不可信,转全量重扫(ADR-5 保守判定器)。
pub const REPLAY_DOWNTIME_THRESHOLD_SECS: u64 = 48 * 3600;
/// 落盘节流:距上次落盘超过此间隔且有变更 → 触发快照。
const FLUSH_INTERVAL: Duration = Duration::from_secs(30);
/// 落盘节流:累计变更超过此条数 → 立即触发快照。
const FLUSH_DIRTY_LIMIT: u64 = 10_000;
/// 事件合批静默窗(tech.md 增量管线)。
const BATCH_WINDOW: Duration = Duration::from_millis(100);

/// 状态四态(product §1 契约)。
pub mod state {
    pub const BUILDING: &str = "building";
    pub const READY: &str = "ready";
    pub const RECONCILING: &str = "reconciling";
    pub const DEGRADED: &str = "degraded";
    pub const OFFLINE: &str = "offline";
}

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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn default_roots() -> Result<Vec<PathBuf>> {
    let home = std::env::var_os("HOME").context("HOME 未设置,无法确定扫描根")?;
    Ok(vec![PathBuf::from(home), PathBuf::from("/Applications")])
}

enum IndexHolder {
    Empty,
    Shared(Box<SharedView>),
    Mutable(Index),
}

pub struct EngineState {
    holder: RwLock<IndexHolder>,
    state_name: Mutex<&'static str>,
    gaps: Mutex<Vec<Gap>>,
    io_errors: AtomicU32,
    built_at: AtomicU64,
    /// 事件流消费线程持续更新的水位,落盘时写入 meta.last_event_id。
    last_event_id: AtomicU64,
    stopping: AtomicBool,
    dirty: AtomicU64,
    last_flush: Mutex<Instant>,
    dir_map: Mutex<HashMap<PathBuf, u32>>,
    roots: Mutex<Vec<PathBuf>>,
    cfg: Mutex<config::Config>,
}

impl EngineState {
    fn new() -> Self {
        Self {
            holder: RwLock::new(IndexHolder::Empty),
            state_name: Mutex::new(state::BUILDING),
            gaps: Mutex::new(vec![]),
            io_errors: AtomicU32::new(0),
            built_at: AtomicU64::new(0),
            last_event_id: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            dirty: AtomicU64::new(0),
            last_flush: Mutex::new(Instant::now()),
            dir_map: Mutex::new(HashMap::new()),
            roots: Mutex::new(vec![]),
            cfg: Mutex::new(config::Config { excluded: vec![] }),
        }
    }

    fn set_state(&self, s: &'static str) {
        *self.state_name.lock().unwrap() = s;
    }
}

/// 单实例:绑定 EADDRINUSE → connect 探测;ECONNREFUSED = stale → unlink 重绑。
pub fn bind_single_instance(sock: &Path) -> Result<UnixListener> {
    match UnixListener::bind(sock) {
        Ok(l) => {
            // socket 权限 0600(仅本用户可连)
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600));
            Ok(l)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => match UnixStream::connect(sock) {
            Ok(_) => bail!("daemon 已在运行"),
            Err(_) => {
                std::fs::remove_file(sock).context("清理 stale socket 失败")?;
                Ok(UnixListener::bind(sock)?)
            }
        },
        Err(e) => Err(e).context("绑定 socket 失败"),
    }
}

// —— 查询执行(daemon 端,结果行按需 stat) ——

fn stat_of(path: &str) -> (Option<u64>, Option<u64>) {
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

fn run_query(view: &dyn IndexView, q: &str, limit: u32) -> Response {
    match engine::query::parse(q) {
        Ok(plan) => {
            let out = engine::query::execute(view, &plan, limit.min(1000) as usize);
            let entries: Vec<HitEntry> = out
                .hits
                .iter()
                .map(|h| {
                    let path = view.full_path_dyn(*h);
                    let name = String::from_utf8_lossy(view.display_name(*h)).into_owned();
                    let (size, mtime) = stat_of(&path);
                    HitEntry {
                        name,
                        path,
                        size,
                        mtime,
                    }
                })
                .collect();
            Response::Query {
                entries,
                total: out.total,
                elapsed_ms: out.elapsed_ms,
                offline: false,
                built_at: 0,
            }
        }
        Err(_) => Response::Error {
            code: "parse".into(),
            message: "空查询".into(),
        },
    }
}

fn with_view<R>(state: &EngineState, f: impl FnOnce(&dyn IndexView) -> R) -> Option<R> {
    let h = state.holder.read().unwrap();
    match &*h {
        IndexHolder::Shared(v) => Some(f(v.as_ref())),
        IndexHolder::Mutable(i) => Some(f(i)),
        IndexHolder::Empty => None,
    }
}

// —— 冷启动与增量 ——

/// 重建「绝对目录路径 → 条目号」映射(冷启动/重扫后一次性)。
/// O(n) 邻接表 + BFS;键为绝对路径,与 FSEvents 事件路径可直接比对。
fn rebuild_dir_map(state: &EngineState) {
    // 两个形态各司其职:根条目名是「字面配置根」(scanner 契约,用于匹配);
    // map 键用 canonical 形态——/tmp 等系统符号链接下 FSEvents 上报真实路径
    // (/private/tmp/...),canonicalize 让事件侧与索引侧同形。
    let literal_roots = state.roots.lock().unwrap().clone();
    let mut map: HashMap<PathBuf, u32> = HashMap::new();
    let h = state.holder.read().unwrap();
    let IndexHolder::Mutable(idx) = &*h else {
        return;
    };
    let n = idx.entries.len() as u32;
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for i in 0..n {
        let e = &idx.entries[i as usize];
        if e.parent != NO_PARENT && e.is_live() {
            children.entry(e.parent).or_default().push(i);
        }
    }
    let mut queue: std::collections::VecDeque<(PathBuf, u32)> = std::collections::VecDeque::new();
    for i in 0..n {
        let e = &idx.entries[i as usize];
        if e.parent != NO_PARENT || !e.is_live() || !e.is_dir() {
            continue;
        }
        // 根条目名 = 完整绝对路径(scanner 契约),按字面根匹配
        let name = String::from_utf8_lossy(idx.display_name_of(i)).into_owned();
        if let Some(r) = literal_roots.iter().find(|r| r.to_string_lossy() == name) {
            let key = r.canonicalize().unwrap_or_else(|_| r.clone());
            map.insert(key.clone(), i);
            queue.push_back((key, i));
        }
    }
    while let Some((abs, idx_p)) = queue.pop_front() {
        if let Some(kids) = children.get(&idx_p) {
            for &k in kids {
                if idx.entries[k as usize].is_dir() {
                    let mut p = abs.clone();
                    p.push(String::from_utf8_lossy(idx.display_name_of(k)).into_owned());
                    map.insert(p.clone(), k);
                    queue.push_back((p, k));
                }
            }
        }
    }
    *state.dir_map.lock().unwrap() = map;
}

/// 对单个绝对目录执行 diff 并维护 dir_map。锁序:holder(write) → dir_map。
fn diff_one(state: &EngineState, abs_dir: &Path) {
    // canonical 基址统一形态:map 键全 canonical;diff 的读取目录与子键拼接同基址。
    let base = abs_dir
        .canonicalize()
        .unwrap_or_else(|_| abs_dir.to_path_buf());
    let mut h = state.holder.write().unwrap();
    let IndexHolder::Mutable(idx) = &mut *h else {
        return;
    };
    let mut map = state.dir_map.lock().unwrap();
    // 直查(canonical 基址)覆盖 map 键形态;再退回字面键(历史插入/特殊场景)。
    let dir_idx = match map.get(&base).or_else(|| map.get(abs_dir)) {
        Some(&i) => i,
        None => return, // 未索引目录(排除内/根外/已删):忽略
    };
    if !idx.entries[dir_idx as usize].is_live() {
        map.remove(&base); // 目录已删(子树墓碑):清映射,不做 diff
        map.remove(abs_dir);
        return;
    }
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    match watcher::apply_dir_diff(idx, dir_idx, &base, &excludes) {
        Ok(stats) => {
            state
                .dirty
                .fetch_add((stats.added + stats.removed) as u64, Ordering::Relaxed);
            for k in idx.children_of(dir_idx) {
                if idx.entries[k as usize].is_dir() {
                    let mut p = base.clone();
                    p.push(String::from_utf8_lossy(idx.display_name_of(k)).into_owned());
                    map.insert(p, k);
                }
            }
        }
        Err(e) => {
            state.io_errors.fetch_add(1, Ordering::Relaxed);
            log::warn!("目录 diff 失败 {}: {e}", base.display());
        }
    }
}

fn build_meta(state: &EngineState) -> IndexMeta {
    let cfg = state.cfg.lock().unwrap().clone();
    IndexMeta {
        built_at: state.built_at.load(Ordering::Relaxed),
        last_event_id: state.last_event_id.load(Ordering::Relaxed),
        scan_roots: state
            .roots
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.to_string_lossy().into_owned().into_bytes())
            .collect(),
        excluded: cfg
            .excluded
            .iter()
            .map(|e| e.clone().into_bytes())
            .collect(),
    }
}

/// 快照落盘(原子替换);失败留痕不中断。
fn flush(state: &EngineState, paths: &DaemonPaths) {
    let h = state.holder.read().unwrap();
    if let IndexHolder::Mutable(idx) = &*h {
        let meta = build_meta(state);
        if let Err(e) = persist::write_atomic(idx, &meta, &paths.index_file()) {
            log::error!("落盘失败: {e}");
        } else {
            state.dirty.store(0, Ordering::Relaxed);
            *state.last_flush.lock().unwrap() = Instant::now();
        }
    }
}

fn should_flush(state: &EngineState) -> bool {
    let dirty = state.dirty.load(Ordering::Relaxed);
    if dirty == 0 {
        return false;
    }
    dirty >= FLUSH_DIRTY_LIMIT || state.last_flush.lock().unwrap().elapsed() >= FLUSH_INTERVAL
}

/// 事件消费循环(重放与增量共用):批量 diff + 落盘节流 + 降级判定。
/// `last_event_id` 水位持续更新,stop/落盘从这里取。
fn consume_events(
    state: Arc<EngineState>,
    paths: DaemonPaths,
    stream: FseStream,
    rx: std::sync::mpsc::Receiver<fsevents::FseEvent>,
    is_replay: bool,
) {
    let mut batcher = DirBatcher::new(BATCH_WINDOW, watcher::now_stub());
    let mut replay_events: u64 = 0;
    let mut stream_errored = false;

    loop {
        if state.stopping.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(ev) => {
                state.last_event_id.store(ev.id, Ordering::Relaxed);
                if fsevents::is_history_done(ev.flags) {
                    // 重放追平:转就绪并落盘一次(更新 last_event_id)
                    if is_replay {
                        state.set_state(state::READY);
                        flush(&state, &paths);
                    }
                    continue;
                }
                if fsevents::is_dropped(ev.flags) {
                    log::warn!("收到丢弃类标记,转全量重扫兜底");
                    degrade_and_rescan(&state, &paths);
                    continue;
                }
                replay_events += 1;
                if let Some(parent) = Path::new(&ev.path).parent() {
                    batcher.push(parent.to_string_lossy().into_owned());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                stream_errored = true;
                break;
            }
        }
        for dir in batcher.drain_ready() {
            diff_one(&state, Path::new(&dir));
        }
        if should_flush(&state) {
            flush(&state, &paths);
        }
    }

    // 重放零事件且停机超阈值 → 保守判定转重扫(唯一兜底,ADR-5)。
    if is_replay
        && !stream_errored
        && replay_events == 0
        && reconcile::needs_full_rescan(
            0,
            now_secs().saturating_sub(state.built_at.load(Ordering::Relaxed)),
            REPLAY_DOWNTIME_THRESHOLD_SECS,
        )
    {
        log::warn!("重放零事件且停机超阈值,转全量重扫");
        degrade_and_rescan(&state, &paths);
    }
    if stream_errored {
        log::warn!("事件流断开,转全量重扫兜底");
        degrade_and_rescan(&state, &paths);
    }
    if state.stopping.load(Ordering::Relaxed) {
        flush(&state, &paths);
    }
    drop(stream); // 停流(幂等)
}

/// 降级:全量重扫重建索引,替换 holder 并重落盘,回就绪。
fn degrade_and_rescan(state: &EngineState, paths: &DaemonPaths) {
    state.set_state(state::DEGRADED);
    let roots = state.roots.lock().unwrap().clone();
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    let (idx, gaps) = engine::scanner::scan_roots(&roots, &excludes, threads);
    state.built_at.store(now_secs(), Ordering::Relaxed);
    *state.gaps.lock().unwrap() = gaps
        .permission
        .iter()
        .map(|(p, r)| Gap {
            path: p.to_string_lossy().into_owned(),
            reason: r.clone(),
        })
        .collect();
    state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
    *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
    rebuild_dir_map(state);
    flush(state, paths);
    state.set_state(state::READY);
}

/// 冷启动编排(独立线程):有索引 → Shared 秒开 → 物化 → 重放;无/损坏 → 首扫。
fn cold_start(state: Arc<EngineState>, paths: DaemonPaths) -> Result<()> {
    let roots = default_roots()?;
    *state.roots.lock().unwrap() = roots.clone();
    let cfg = config::load(&paths.config_file());
    *state.cfg.lock().unwrap() = cfg;
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    let last_event_id = match persist::load_mmap(&paths.index_file()) {
        Ok(view) => {
            let last_id = view.meta.last_event_id;
            let built = view.meta.built_at;
            state.built_at.store(built, Ordering::Relaxed);
            state.last_event_id.store(last_id, Ordering::Relaxed);
            // 两段式:Shared 立即可查
            *state.holder.write().unwrap() = IndexHolder::Shared(view);
            state.set_state(state::RECONCILING);
            // 物化 → 可变索引
            let idx = {
                let h = state.holder.read().unwrap();
                match &*h {
                    IndexHolder::Shared(v) => v.materialize(),
                    _ => unreachable!("刚挂 Shared"),
                }
            };
            *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
            rebuild_dir_map(&state);
            last_id
        }
        Err(e) => {
            log::info!("无可用落盘索引({e}),首扫建索引");
            state.set_state(state::BUILDING);
            let (idx, gaps) = engine::scanner::scan_roots(&roots, &excludes, threads);
            state.built_at.store(now_secs(), Ordering::Relaxed);
            *state.gaps.lock().unwrap() = gaps
                .permission
                .iter()
                .map(|(p, r)| Gap {
                    path: p.to_string_lossy().into_owned(),
                    reason: r.clone(),
                })
                .collect();
            state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
            // 扫描完成时刻的水位:覆盖「扫描点 → 关机」的变更
            let water = fsevents::current_event_id();
            state.last_event_id.store(water, Ordering::Relaxed);
            *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
            rebuild_dir_map(&state);
            flush(&state, &paths);
            state.set_state(state::READY);
            // 首扫后无需重放,从 now 起监听
            fsevents::SINCE_NOW
        }
    };

    // 建流:重放自 last_event_id;无有效水位(0)→ 保守起步但做一次全量重扫兜底。
    let roots_str: Vec<String> = roots
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    let (stream, rx) = match fsevents::start_stream(
        &roots_str.iter().map(String::as_str).collect::<Vec<_>>(),
        last_event_id.max(1),
        0.1,
        fsevents::FILE_EVENTS,
    ) {
        Ok(v) => v,
        Err(e) => {
            log::error!("建流失败({e}),转全量重扫兜底模式");
            degrade_and_rescan(&state, &paths);
            return Ok(());
        }
    };
    let is_replay = last_event_id != fsevents::SINCE_NOW && last_event_id != 0;
    if is_replay {
        state.set_state(state::RECONCILING);
    }
    let st = state.clone();
    let p = paths.clone();
    std::thread::spawn(move || consume_events(st, p, stream, rx, is_replay));
    Ok(())
}

fn handle_conn(
    stream: &mut UnixStream,
    state: &Arc<EngineState>,
    paths: &DaemonPaths,
) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > protocol::MAX_MSG {
        bail!("消息超限({len} 字节)");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    let req: Request = protocol::decode_request(&buf).context("解码请求失败")?;
    let resp = dispatch(req, state, paths);
    let body = serde_json::to_vec(&resp)?;
    stream.write_all(&(body.len() as u32).to_le_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}

fn dispatch(req: Request, state: &Arc<EngineState>, paths: &DaemonPaths) -> Response {
    if req.version() != protocol::PROTOCOL_VERSION {
        return Response::Error {
            code: "version".into(),
            message: format!(
                "协议版本不识别({}),客户端与 daemon 版本不匹配",
                req.version()
            ),
        };
    }
    match req {
        Request::Query { query, limit, .. } => {
            match with_view(state, |v| run_query(v, &query, limit)) {
                Some(r) => r,
                None => Response::Error {
                    code: "warming".into(),
                    message: "索引构建中,暂不可查".into(),
                },
            }
        }
        Request::Status { .. } => {
            let name = *state.state_name.lock().unwrap();
            let entry_count = with_view(state, |v| {
                (0..v.entries().len() as u32)
                    .filter(|i| v.entries()[*i as usize].is_live())
                    .count() as u32
            })
            .unwrap_or(0);
            Response::Status {
                state: name.into(),
                entry_count,
                gaps: state.gaps.lock().unwrap().clone(),
                io_error_count: state.io_errors.load(Ordering::Relaxed),
                built_at: state.built_at.load(Ordering::Relaxed),
                index_file: paths.index_file().to_string_lossy().into_owned(),
                offline: false,
            }
        }
        Request::Stop { .. } => {
            state.stopping.store(true, Ordering::SeqCst);
            // 最终落盘完成后才响应(契约:响应即落盘已保证)
            let n = {
                let h = state.holder.read().unwrap();
                match &*h {
                    IndexHolder::Mutable(idx) => {
                        let meta = build_meta(state);
                        let _ = persist::write_atomic(idx, &meta, &paths.index_file());
                        idx.entry_count()
                    }
                    _ => 0,
                }
            };
            let _ = std::fs::remove_file(paths.socket());
            Response::Stop {
                ok: true,
                final_entry_count: n,
            }
        }
        Request::ExcludeList { .. } => Response::Exclude {
            ok: true,
            excluded: state.cfg.lock().unwrap().excluded.clone(),
        },
        Request::ExcludeAdd { path, .. } => {
            apply_exclude_change(state, paths, &path, ExcludeOp::Add);
            Response::Exclude {
                ok: true,
                excluded: state.cfg.lock().unwrap().excluded.clone(),
            }
        }
        Request::ExcludeRemove { path, .. } => {
            apply_exclude_change(state, paths, &path, ExcludeOp::Remove);
            Response::Exclude {
                ok: true,
                excluded: state.cfg.lock().unwrap().excluded.clone(),
            }
        }
    }
}

enum ExcludeOp {
    Add,
    Remove,
}

/// 排除变更:校验 → 更新配置 → 落盘配置 → 重扫使变更生效。
/// 运行时无定向增量(受影响范围重扫可作后续优化),重扫保证正确性;
/// 非法路径不改状态(响应仍带当前列表,调用方自行比对)。
fn apply_exclude_change(state: &Arc<EngineState>, paths: &DaemonPaths, path: &str, op: ExcludeOp) {
    if let Err(msg) = protocol::validate::exclude_path(path) {
        log::warn!("排除路径非法({msg}): {path}");
        return;
    }
    {
        let mut cfg = state.cfg.lock().unwrap();
        match op {
            ExcludeOp::Add => {
                if !cfg.excluded.iter().any(|e| e == path) {
                    cfg.excluded.push(path.to_string());
                }
            }
            ExcludeOp::Remove => cfg.excluded.retain(|e| e != path),
        }
    }
    let cfg = state.cfg.lock().unwrap().clone();
    if let Err(e) = config::save(&cfg, &paths.config_file()) {
        log::error!("配置保存失败: {e}");
    }
    degrade_and_rescan(state, paths);
}

/// 服务循环(主线程):accept → 每连接一线程;stop 处理后退出进程。
pub fn serve(paths: DaemonPaths) -> Result<()> {
    std::fs::create_dir_all(&paths.data_dir).context("创建数据目录失败")?;
    init_logger(&paths);
    let listener = bind_single_instance(&paths.socket())?;
    let state = Arc::new(EngineState::new());
    {
        let st = state.clone();
        let p = paths.clone();
        std::thread::spawn(move || {
            if let Err(e) = cold_start(st, p) {
                log::error!("冷启动失败: {e:#}");
            }
        });
    }
    for stream in listener.incoming() {
        if state.stopping.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut stream) = stream else { continue };
        let st = state.clone();
        let p = paths.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_conn(&mut stream, &st, &p) {
                log::warn!("连接处理失败: {e:#}");
            }
            // stop 已响应:落盘保证已达成,退出进程(accept 循环随之结束)
            if st.stopping.load(Ordering::SeqCst) {
                std::process::exit(0);
            }
        });
    }
    Ok(())
}

/// 极简日志:stderr + 数据目录 daemon.log(避免引入日志框架依赖)。
fn init_logger(paths: &DaemonPaths) {
    struct Dual(log::LevelFilter, Option<std::fs::File>);
    impl log::Log for Dual {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= self.0
        }
        fn log(&self, r: &log::Record) {
            if !self.enabled(r.metadata()) {
                return;
            }
            let line = format!("[{}] {}\n", r.level(), r.args());
            eprint!("{line}");
            if let Some(f) = &self.1 {
                let _ = (&*f).write_all(line.as_bytes());
            }
        }
        fn flush(&self) {}
    }
    if log::set_boxed_logger(Box::new(Dual(
        log::LevelFilter::Info,
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.log_file())
            .ok(),
    )))
    .is_ok()
    {
        log::set_max_level(log::LevelFilter::Info);
    }
}

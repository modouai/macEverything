//! daemon:引擎宿主——冷启动编排(秒开 → 物化 → 重放/重扫)+ 增量循环 + 服务循环。
//! 单实例:绑定 EADDRINUSE 时 connect 探测——连得上 = 已在运行;连不上 = 崩溃
//! 残留 stale socket,unlink 重绑。stop 经 socket 消息触发,响应在最终落盘后返回。
use anyhow::{bail, Context, Result};
use client::{stat_of, stat_rows, state, DaemonPaths};
use engine::config;
use engine::entry::NO_PARENT;
use engine::fsevents::{self, FseStream};
use engine::index::{Index, IndexView};
use engine::persist::{self, IndexMeta, SharedView};
use engine::query::{MetaRow, SortKey};
use engine::reconcile;
use engine::watcher::{self, DirBatcher};

mod volumes;
use protocol::{self, Gap, HitEntry, Request, Response};
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
/// 首扫流式聚合批大小:每批持写锁聚合后即释放,批间查询可见已扫部分(product 场景 2)。
const SCAN_CHUNK: usize = 50_000;
/// 重扫冷却:两次全量重扫的最小间隔(防「重扫→新 dropped→重扫」退化振荡;
/// 实测真实实例曾 420 次重复重扫,索引永在重扫、日志刷屏)。
const RESCAN_COOLDOWN: Duration = Duration::from_secs(30);

/// 事件是否已被全量重扫覆盖(≤ 重扫开始时刻的全局水位)。独立成函数便于单测
/// 钉住比较语义:覆盖点是「小于等于」——floor 本身的事件同属历史。
fn event_covered_by_rescan(ev_id: u64, rescan_floor: u64) -> bool {
    ev_id <= rescan_floor
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn default_roots() -> Result<Vec<PathBuf>> {
    // MCE_ROOTS(冒号分隔)覆盖扫描根:基准与隔离测试用;生产缺省 = 主目录 + /Applications。
    // 覆盖生效时留痕:该变量在生产路径同样可读,静默生效会让人误判索引范围。
    if let Some(v) = std::env::var_os("MCE_ROOTS") {
        let roots: Vec<PathBuf> = std::env::split_paths(&v).collect();
        if !roots.is_empty() {
            log::warn!(
                "MCE_ROOTS 覆盖扫描根(基准/测试用途): {:?}",
                roots
                    .iter()
                    .map(|r| r.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            );
            return Ok(roots);
        }
    }
    let home = std::env::var_os("HOME").context("HOME 未设置,无法确定扫描根")?;
    Ok(vec![PathBuf::from(home), PathBuf::from("/Applications")])
}

/// 扫描根校验:不存在的根不静默——记入缺口清单(status 可见)并留错误日志。
/// product 边界表:「扫描根不存在(如重命名了主目录)」须可操作提示。
fn invalid_roots_gaps(roots: &[PathBuf]) -> Vec<Gap> {
    let gaps: Vec<Gap> = roots
        .iter()
        .filter(|r| !r.is_dir())
        .map(|r| Gap {
            path: r.to_string_lossy().into_owned(),
            reason: "扫描根不存在或不可读,请检查路径(如主目录是否被重命名)".into(),
        })
        .collect();
    if !gaps.is_empty() {
        log::error!(
            "扫描根无效: {:?}",
            gaps.iter().map(|g| &g.path).collect::<Vec<_>>()
        );
    }
    gaps
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
    /// 接收水位:事件到达即 fetch_max 推进,只作批次水位快照用,不落盘。
    seen_event_id: AtomicU64,
    /// 已应用水位:一批目录 diff **全部完成后**才推进;落盘写 meta.last_event_id。
    /// 语义保证「落盘水位 ≤ 索引内容对应的事件」——重启重放必覆盖「已收未应用」
    /// 的变更(否则 stop/崩溃前 100ms 窗口内的变更永久丢失)。
    applied_event_id: AtomicU64,
    /// 重扫覆盖水位(全量重扫开始时取的全局事件 ID)。id ≤ 此值的事件已被重扫
    /// 内容覆盖,重扫完成后积压在通道里的陈旧事件(含其携带的丢弃标记)直接跳过。
    /// 不跳则死循环:重扫 10-30s 期间消费停摆 → 内核缓冲溢出产生新 dropped 标记
    /// → 又触发重扫(实测 420 次重复重扫,日志刷屏且索引永在重扫)。
    rescan_floor: AtomicU64,
    stopping: AtomicBool,
    dirty: AtomicU64,
    last_flush: Mutex<Instant>,
    /// 落盘串行锁:stop 落盘与消费线程退出落盘/节流落盘互斥(曾实测并发
    /// write_atomic 交错损坏索引;tmp 名唯一化之外的语义级双保险)。
    flush_lock: Mutex<()>,
    dir_map: Mutex<HashMap<PathBuf, u32>>,
    roots: Mutex<Vec<PathBuf>>,
    cfg: Mutex<config::Config>,
    /// 卷扫描世代:每次卷卸载推进,使在途扫描批次失效(防「拔出即重插」时
    /// 旧批次与新扫交错应用产生重复条目,见 volumes.rs scan_volume_into)。
    volume_scan_epoch: AtomicU64,
    /// 运行期挂载卷的按卷 FSEvents 流(external-volumes):卸载时显式 stop。
    /// 主流(HOME+/Applications)不在此列,由 consume_events 持有至退出。
    volume_streams: Mutex<HashMap<PathBuf, FseStream>>,
    /// 排除目录跨连接变更锁:add/remove/reset 的「校验 → 内存 → 落盘 → 重扫」
    /// 必须整体串行。`serve` 每连接一线程;没有此锁时 reset 与 CLI add/remove
    /// 交错会写出「配置含自定义条目、内存却已重置」的错位状态,并并发全量重扫。
    /// 只包变更序列,`ExcludeList` 读不加锁(避免被长重扫阻塞)。
    exclude_op_lock: Mutex<()>,
}

impl EngineState {
    fn new() -> Self {
        Self {
            holder: RwLock::new(IndexHolder::Empty),
            state_name: Mutex::new(state::BUILDING),
            volume_streams: Mutex::new(HashMap::new()),
            volume_scan_epoch: AtomicU64::new(0),
            gaps: Mutex::new(vec![]),
            io_errors: AtomicU32::new(0),
            built_at: AtomicU64::new(0),
            seen_event_id: AtomicU64::new(0),
            applied_event_id: AtomicU64::new(0),
            rescan_floor: AtomicU64::new(0),
            stopping: AtomicBool::new(false),
            dirty: AtomicU64::new(0),
            last_flush: Mutex::new(Instant::now()),
            flush_lock: Mutex::new(()),
            dir_map: Mutex::new(HashMap::new()),
            roots: Mutex::new(vec![]),
            cfg: Mutex::new(config::Config {
                excluded: vec![],
                auto_index_volumes: false,
                volumes_excluded: vec![],
            }),
            exclude_op_lock: Mutex::new(()),
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

/// sort 参数解析:字符串 → (列键, 回显串)。缺省/未知一律 relevance,回显即
/// 真相——调用方比对请求键与回显识别「排序未生效」(旧 daemon 回显空串),不静默错排序。
fn parse_sort_key(sort: Option<&str>) -> (SortKey, &'static str) {
    match sort.unwrap_or("relevance") {
        "name" => (SortKey::Name, "name"),
        "size" => (SortKey::Size, "size"),
        "mtime" => (SortKey::Mtime, "mtime"),
        "path" => (SortKey::Path, "path"),
        _ => (SortKey::Relevance, "relevance"),
    }
}

/// 绝对方向回显:NAME/SIZE/PATH 自然升序,MTIME 自然降序;relevance 无方向。
/// 协议 desc 位是「反转自然方向」,此处换算成绝对方向(降序 = true)。
fn desc_echo(key: SortKey, desc: bool) -> Option<bool> {
    match key {
        SortKey::Relevance => None,
        SortKey::Mtime => Some(!desc),
        SortKey::Name | SortKey::Size | SortKey::Path => Some(desc),
    }
}

/// 查询执行:匹配与排序在 holder 读锁内完成;结果行的 size/mtime 在**锁外**
/// 并行读取——product 契约要求按需读取「并行执行、单独计时随输出给出」,
/// 且 stat 不应持读锁阻塞增量 diff 的写锁获取。
/// Size/Mtime 列排序是上述锁外纪律的显式例外:完整排序语义要求候选集全量键,
/// 排序轮 meta_of 批量并行 stat 在读锁内(耗时随响应如实告知,tech.md 已标注);
/// 结果行落入缓存 map,组装轮零重查、不触碰锁外 stat 路径。
/// desc(定案,Task 19b)= 反转自然方向,透传引擎;缺省(旧客户端)等价 false。
fn run_query(state: &EngineState, q: &str, limit: u32, sort: Option<&str>, desc: bool) -> Response {
    let (key, key_str) = parse_sort_key(sort);
    let desc_echo = desc_echo(key, desc);
    let Ok(plan) = engine::query::parse(q) else {
        return Response::Error {
            code: "parse".into(),
            message: "空查询".into(),
        };
    };
    // 排序轮 stat 缓存:条目号 → (size, mtime)。meta_of 是 `&dyn Fn`,经 Mutex
    // 取内部可变性写入(不走 FnMut 捕获);响应组装(锁外)按条目号命中优先。
    let cache: Mutex<HashMap<u32, MetaRow>> = Mutex::new(HashMap::new());
    // 锁内:匹配 + 排序 + 结果行的名称/路径(纯内存拼接,微秒级)
    let Some((out, rows)) = with_view(state, |v| {
        let meta_fn = |ids: &[u32]| -> Vec<MetaRow> {
            use rayon::prelude::*;
            let metas: Vec<MetaRow> = ids
                .par_iter()
                .map(|&i| stat_of(&v.full_path_dyn(i)))
                .collect();
            let mut c = cache.lock().unwrap();
            for (&i, m) in ids.iter().zip(&metas) {
                c.insert(i, *m);
            }
            metas
        };
        let out =
            engine::query::execute_sorted(v, &plan, limit.min(1000) as usize, key, desc, &meta_fn);
        // 第四列 is_dir 取自索引条目:缓存命中与补 stat 两条组装路径都据此填值,
        // 不依赖 stat 是否发生(Relevance/Name/Path 排序下缓存为空)。
        let rows: Vec<(u32, String, String, bool)> = out
            .hits
            .iter()
            .map(|h| {
                (
                    *h,
                    String::from_utf8_lossy(v.display_name(*h)).into_owned(),
                    v.full_path_dyn(*h),
                    v.entries()[*h as usize].is_dir(),
                )
            })
            .collect();
        (out, rows)
    }) else {
        return Response::Error {
            code: "warming".into(),
            message: "索引构建中,暂不可查".into(),
        };
    };
    // 锁外:缓存命中优先,未命中行并行补 stat(慢速 syscall 不阻塞写锁)
    let (entries, complete_ms) = stat_with_cache(rows, &cache);
    Response::Query {
        entries,
        total: out.total,
        elapsed_ms: out.elapsed_ms,
        stat_ms: out.stat_ms + complete_ms,
        sort_echo: key_str.into(),
        desc_echo,
    }
}

/// 响应组装的结果行读取:排序轮缓存(条目号 → (size, mtime))命中优先,
/// 未命中行经 client::stat_rows 并行补 stat——Size/Mtime 排序下结果行在排序轮
/// 已全量 stat,零重查;Relevance/Name/Path 排序下缓存为空,与 v1 行为一致。
/// 返回 (条目, 补全轮读取耗时毫秒);响应 stat_ms = 排序轮 + 补全轮。
fn stat_with_cache(
    rows: Vec<(u32, String, String, bool)>,
    cache: &Mutex<HashMap<u32, MetaRow>>,
) -> (Vec<HitEntry>, f64) {
    let t = Instant::now();
    let mut meta: Vec<MetaRow> = Vec::with_capacity(rows.len());
    let mut miss: Vec<usize> = Vec::new();
    {
        let c = cache.lock().unwrap();
        for (i, (id, ..)) in rows.iter().enumerate() {
            match c.get(id) {
                Some(&m) => meta.push(m),
                None => {
                    // 占位;下方补 stat 后按位覆盖,不会留在结果里
                    miss.push(i);
                    meta.push((None, None));
                }
            }
        }
    }
    if !miss.is_empty() {
        let miss_rows: Vec<(String, String)> = miss
            .iter()
            .map(|&i| (rows[i].1.clone(), rows[i].2.clone()))
            .collect();
        let (filled, _) = stat_rows(miss_rows);
        for (&i, e) in miss.iter().zip(filled) {
            meta[i] = (e.size, e.mtime);
        }
    }
    let entries = rows
        .into_iter()
        .zip(meta)
        .map(|((_, name, path, is_dir), (size, mtime))| HitEntry {
            name,
            path,
            size,
            mtime,
            is_dir,
        })
        .collect();
    (entries, t.elapsed().as_secs_f64() * 1000.0)
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
/// 返回该目录本次的变更条数(added + removed),供抽样对拍判定用。
fn apply_and_track(
    state: &EngineState,
    idx: &mut Index,
    map: &mut HashMap<PathBuf, u32>,
    abs_dir: &Path,
    excludes: &[std::path::PathBuf],
) -> u32 {
    let Some(&dir_idx) = map.get(abs_dir) else {
        return 0;
    };
    if !idx.entries[dir_idx as usize].is_live() {
        map.remove(abs_dir); // 目录已删(子树墓碑):清映射,不做 diff
        return 0;
    }
    match watcher::apply_dir_diff(idx, dir_idx, abs_dir, excludes) {
        Ok(stats) => {
            state
                .dirty
                .fetch_add((stats.added + stats.removed) as u64, Ordering::Relaxed);
            for k in idx.children_of(dir_idx) {
                if idx.entries[k as usize].is_dir() {
                    let mut p = abs_dir.to_path_buf();
                    p.push(String::from_utf8_lossy(idx.display_name_of(k)).into_owned());
                    map.insert(p, k);
                }
            }
            stats.added + stats.removed
        }
        Err(e) => {
            state.io_errors.fetch_add(1, Ordering::Relaxed);
            log::warn!("目录 diff 失败 {}: {e}", abs_dir.display());
            0
        }
    }
}

/// 对单个目录应用 diff(未索引时先补链);返回该目录的变更条数。
fn diff_one(state: &EngineState, abs_dir: &Path) -> u32 {
    // canonical 基址统一形态:map 键全 canonical;diff 的读取目录与子键拼接同基址。
    let base = abs_dir
        .canonicalize()
        .unwrap_or_else(|_| abs_dir.to_path_buf());
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    let mut h = state.holder.write().unwrap();
    let IndexHolder::Mutable(idx) = &mut *h else {
        return 0;
    };
    let mut map = state.dir_map.lock().unwrap();
    let mut changed = 0u32;

    // 目标未索引:事件风暴下子目录事件可能先于其父目录的 diff 到达(新建目录树),
    // 直查必然 miss。从最近已索引祖先逐级向下补 diff(每级纳入其直接子项),
    // 再应用目标自身——否则新目录内容要等下次重放/重扫才可见。
    if !map.contains_key(&base) {
        let mut chain: Vec<PathBuf> = Vec::new(); // [目标, …, 未索引的第一级]
        let mut cur = base.clone();
        loop {
            let Some(parent) = cur.parent().map(Path::to_path_buf) else {
                return changed; // 根之外:忽略
            };
            chain.push(cur.clone());
            cur = parent;
            if map.contains_key(&cur) {
                break;
            }
        }
        for next in chain.iter().rev() {
            if engine::scanner::is_excluded(next, &excludes) {
                return changed; // 链上出现排除目录:不拉回索引
            }
            changed += apply_and_track(state, idx, &mut map, &cur, &excludes);
            if !map.contains_key(next) {
                return changed; // 祖先 diff 未纳入该级(根外/竞态删):放弃
            }
            cur = next.clone();
        }
        // 走完 chain 后 cur == base(目标自身尚未 diff)
    }

    changed + apply_and_track(state, idx, &mut map, &base, &excludes)
}

fn build_meta(state: &EngineState) -> IndexMeta {
    let cfg = state.cfg.lock().unwrap().clone();
    IndexMeta {
        built_at: state.built_at.load(Ordering::Relaxed),
        last_event_id: state.applied_event_id.load(Ordering::Relaxed),
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

/// 快照落盘(原子替换,flush_lock 串行化);失败留痕不中断。
fn flush(state: &EngineState, paths: &DaemonPaths) {
    let _guard = state.flush_lock.lock().unwrap();
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

/// 最终落盘(stop 路径):持 flush_lock 串行化;返回 (落盘成功, 条目数)。
fn persist_final(state: &EngineState, paths: &DaemonPaths) -> (bool, u32) {
    let _guard = state.flush_lock.lock().unwrap();
    let h = state.holder.read().unwrap();
    match &*h {
        IndexHolder::Mutable(idx) => {
            let meta = build_meta(state);
            match persist::write_atomic(idx, &meta, &paths.index_file()) {
                Ok(()) => (true, idx.entry_count()),
                Err(e) => {
                    log::error!("最终落盘失败: {e}");
                    (false, idx.entry_count())
                }
            }
        }
        _ => (true, 0),
    }
}

fn should_flush(state: &EngineState) -> bool {
    let dirty = state.dirty.load(Ordering::Relaxed);
    if dirty == 0 {
        return false;
    }
    dirty >= FLUSH_DIRTY_LIMIT || state.last_flush.lock().unwrap().elapsed() >= FLUSH_INTERVAL
}

/// 应用一批目录 diff,全部完成后推进已应用水位。
/// 水位快照在 diff **之前**取:本批目录对应的全部事件此刻必已入 batcher,
/// 快照覆盖它们且不含后续未入批事件(水位只落后、绝不超前于索引内容)。
fn apply_batch(state: &EngineState, dirs: Vec<String>, track_watermark: bool) {
    if dirs.is_empty() {
        return;
    }
    // 水位快照在 diff **之前**取:本批目录对应的全部事件此刻必已入 batcher,
    // 快照覆盖它们且不含后续未入批事件(水位只落后、绝不超前于索引内容)。
    // 卷流(volume_owned=false→track=false)不推进水位:多流共享见 volumes.rs。
    let batch_water = if track_watermark {
        Some(state.seen_event_id.load(Ordering::Relaxed))
    } else {
        None
    };
    for dir in &dirs {
        diff_one(state, Path::new(dir));
    }
    if let Some(w) = batch_water {
        state.applied_event_id.fetch_max(w, Ordering::Relaxed);
    }
    maybe_compact(state);
}

/// 墓碑/孤儿字节达阈值时整理(tech.md 风险表承诺:墓碑占比超 20% 或孤儿字节
/// 超 32MB 触发整理;原实现 0 调用方,长期常驻下内存无界增长)。
/// 整理重建条目号与 arena,dir_map 随之失效——必须同步重建,否则增量找不到目录。
fn maybe_compact(state: &EngineState) {
    let (before, after) = {
        let mut h = state.holder.write().unwrap();
        let IndexHolder::Mutable(idx) = &mut *h else {
            return;
        };
        if !idx.needs_compaction() {
            return;
        }
        let before = idx.entries.len();
        idx.compact();
        (before, idx.entries.len())
    };
    log::info!("索引整理:条目 {before} → {after},回收墓碑与 arena 孤儿字节");
    rebuild_dir_map(state);
}

/// 事件消费循环(重放与增量共用):批量 diff + 落盘节流 + 降级判定。
/// 水位语义:接收(`seen_event_id`)与已应用(`applied_event_id`)分离,
/// 落盘只取已应用水位(见 EngineState 字段注释)。
#[allow(clippy::too_many_arguments)]
fn consume_events(
    state: Arc<EngineState>,
    paths: DaemonPaths,
    stream: Option<FseStream>,
    rx: std::sync::mpsc::Receiver<fsevents::FseEvent>,
    is_replay: bool,
    replayed_from_disk: bool,
    volume_owned: bool,
) {
    // 主流由本线程持有,尾部 drop(stream) 即停流;卷流的流本体在
    // state.volume_streams,卸载时显式 stop 后此处为 None。
    let mut batcher = DirBatcher::new(BATCH_WINDOW, watcher::system_now());
    let mut replay_events: u64 = 0;
    let mut stream_errored = false;
    let mut last_rescan: Option<Instant> = None;

    loop {
        if state.stopping.load(Ordering::Relaxed) {
            break;
        }
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(ev) => {
                // 已被全量重扫覆盖的陈旧事件直接跳过。必须先于一切语义判定——
                // 重扫期间消费停摆、内核缓冲溢出产生的 dropped 标记就在其中,
                // 它们正是「重扫 → 又一批 dropped → 又重扫」死循环的燃料。
                if event_covered_by_rescan(ev.id, state.rescan_floor.load(Ordering::Relaxed)) {
                    continue;
                }
                // 卷流不写全局水位:多流共享会让 applied 越过主消费方实际进度,
                // 重启重放从该水位起步将漏掉主流未应用事件(见 volumes.rs 模块注释)。
                if !volume_owned {
                    state.seen_event_id.fetch_max(ev.id, Ordering::Relaxed);
                }
                if fsevents::is_history_done(ev.flags) {
                    if is_replay {
                        // 追平前强制应用窗口内 pending 事件:否则「就绪」早于索引追上,
                        // 紧随的查询会漏停机窗口内的变更(实机重放抽验曾因此为空)。
                        apply_batch(&state, batcher.drain_all(), !volume_owned);
                        // 判定一:零事件 + 停机超阈 → 重放不可信(spike 证实无可观测
                        // 信号,此为唯一确定性防线;原先该判定只在循环退出后执行,
                        // 常驻 daemon 下永不触发)。
                        let downtime =
                            now_secs().saturating_sub(state.built_at.load(Ordering::Relaxed));
                        if reconcile::needs_full_rescan(
                            replay_events,
                            downtime,
                            REPLAY_DOWNTIME_THRESHOLD_SECS,
                        ) {
                            log::warn!("重放零事件且停机超阈值,转全量重扫");
                            degrade_and_rescan(&state, &paths);
                            continue;
                        }
                        // 判定二:抽样对拍——日志滚动会「部分回放」(从保留窗口起点
                        // 开始、有事件但丢前段),零事件判定覆盖不到,抽样 diff 暴露差异。
                        // 仅对「来自落盘索引的停机重放」有意义;首扫后索引是全新全量的,
                        // 抽样只白付开销(且首扫窗口变更本就由本轮重放覆盖)。
                        if replayed_from_disk && sampling_finds_gap(&state, &paths) {
                            continue; // 已转 degraded,重扫完成后自行置 READY
                        }
                        state.set_state(state::READY);
                        flush(&state, &paths);
                    }
                    continue;
                }
                if fsevents::is_dropped(ev.flags) {
                    // 冷却双保险:rescan_floor 兜住重扫期间积压的旧标记,但重扫**之后**
                    // 新产生的 dropped(重扫自身引起的缓冲溢出)仍需限频,防退化振荡。
                    if let Some(t) = last_rescan {
                        if t.elapsed() < RESCAN_COOLDOWN {
                            log::warn!(
                                "重扫冷却期内再收丢弃标记({:.0}s 前刚重扫过),已忽略",
                                t.elapsed().as_secs_f64()
                            );
                            continue;
                        }
                    }
                    log::warn!("收到丢弃类标记,转全量重扫兜底");
                    degrade_and_rescan(&state, &paths);
                    last_rescan = Some(Instant::now());
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
        apply_batch(&state, batcher.drain_ready(), !volume_owned);
        if should_flush(&state) {
            flush(&state, &paths);
        }
    }

    // 退出前强制排空 pending:停止路径(或流断开)若跳过这一步,窗口内已收未应用的
    // 变更会因水位停在 diff 完成点而被正确重放,但此处仍排空以让落盘内容尽量新。
    apply_batch(&state, batcher.drain_all(), !volume_owned);

    // 降级判定(统一走 reconcile::should_degrade,契约见其 doc):
    // 流断开 / 保守判定(零事件且停机超阈)——信号路径已由 spike 排除(ADR-5)。
    // 非重放阶段(增量期)零事件是常态,传 1 使保守判定天然不触发。
    let replay_probe = if is_replay { replay_events } else { 1 };
    let stopping = state.stopping.load(Ordering::Relaxed);
    let downtime = now_secs().saturating_sub(state.built_at.load(Ordering::Relaxed));
    if !stopping
        && !volume_owned
        && reconcile::should_degrade(
            false,
            stream_errored,
            replay_probe,
            downtime,
            REPLAY_DOWNTIME_THRESHOLD_SECS,
        )
    {
        log::warn!("重放不可信(流断开或零事件长停机),转全量重扫");
        degrade_and_rescan(&state, &paths);
    }
    if stopping {
        flush(&state, &paths);
    }
    drop(stream); // 停流(幂等)
}

/// 重放追平后的抽样对拍兜底:spike 证实日志滚动的丢段无可观测信号,
/// 且过旧 sinceWhen 会「从保留窗口起点部分回放」(有事件,零事件判定覆盖不到)。
/// 抽样 N 个已索引目录做 diff:差异目录占比超阈值判重放不可信 → 全量重扫;
/// 少量差异(正常竞态)由抽样 diff 自身直接补上。
/// 返回 true 表示已转降级重扫。
fn sampling_finds_gap(state: &EngineState, paths: &DaemonPaths) -> bool {
    const SAMPLE_DIRS: usize = 100;
    const MISMATCH_PCT_LIMIT: usize = 5;
    let bases: Vec<PathBuf> = {
        let map = state.dir_map.lock().unwrap();
        if map.is_empty() {
            return false;
        }
        let keys: Vec<PathBuf> = map.keys().cloned().collect();
        if keys.len() <= SAMPLE_DIRS {
            keys
        } else {
            let step = keys.len() / SAMPLE_DIRS;
            (0..SAMPLE_DIRS).map(|i| keys[i * step].clone()).collect()
        }
    };
    let mut mismatched = 0usize;
    for b in &bases {
        if diff_one(state, b) > 0 {
            mismatched += 1;
        }
    }
    if mismatched * 100 / bases.len() > MISMATCH_PCT_LIMIT {
        log::warn!(
            "抽样对拍 {mismatched}/{} 个目录与磁盘不符,判重放不可信,转全量重扫",
            bases.len()
        );
        degrade_and_rescan(state, paths);
        return true;
    }
    false
}

/// 降级:全量重扫重建索引,替换 holder 并重落盘,回就绪。
/// 覆盖水位在扫描**之前**取(id > floor 的变更仍会被正常 diff,重扫未必包含)。
fn degrade_and_rescan(state: &EngineState, paths: &DaemonPaths) {
    state.set_state(state::DEGRADED);
    let floor = fsevents::current_event_id();
    let roots = state.roots.lock().unwrap().clone();
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    let (idx, gaps) = reconcile::full_rescan(&roots, &excludes, threads);
    state.built_at.store(now_secs(), Ordering::Relaxed);
    let mut gap_list = invalid_roots_gaps(&roots);
    gap_list.extend(gaps.permission.iter().map(|(p, r)| Gap {
        path: p.to_string_lossy().into_owned(),
        reason: r.clone(),
    }));
    *state.gaps.lock().unwrap() = gap_list;
    state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
    *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
    // 水位推进到覆盖点:重扫内容 = 截至 floor 的磁盘状态;floor 以下积压的陈旧
    // 事件(含触发本轮重扫的 dropped 标记)从此刻起被消费循环跳过。
    state.seen_event_id.fetch_max(floor, Ordering::Relaxed);
    state.applied_event_id.fetch_max(floor, Ordering::Relaxed);
    state.rescan_floor.fetch_max(floor, Ordering::Relaxed);
    rebuild_dir_map(state);
    flush(state, paths);
    state.set_state(state::READY);
}

/// 冷启动编排(独立线程):有索引 → Shared 秒开 → 物化 → 重放;无/损坏 → 首扫。
fn cold_start(state: Arc<EngineState>, paths: DaemonPaths) -> Result<()> {
    let cfg = config::load(&paths.config_file());
    *state.cfg.lock().unwrap() = cfg.clone();
    // 外接卷(external-volumes):挂载中的期望卷并入初始 roots——落盘索引路径
    // 由此获得流覆盖(停机窗口重放追平卷内漂移);首扫路径一并纳入首扫。
    let desired_volumes = volumes::desired_roots(&cfg);
    for v in &desired_volumes {
        log::info!("启动挂载卷并入扫描根: {}", v.display());
    }
    let desired_set: std::collections::HashSet<PathBuf> = desired_volumes.iter().cloned().collect();
    let mut roots = default_roots()?;
    roots.extend(desired_volumes.iter().cloned());
    *state.roots.lock().unwrap() = roots.clone();
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);

    // (建流起点水位, 是否来自落盘索引的停机重放——首扫无需抽样对拍)
    let (last_event_id, replayed_from_disk) = match persist::load_mmap(&paths.index_file()) {
        Ok(view) => {
            let last_id = view.meta.last_event_id;
            let built = view.meta.built_at;
            state.built_at.store(built, Ordering::Relaxed);
            state.applied_event_id.store(last_id, Ordering::Relaxed);
            state.seen_event_id.store(last_id, Ordering::Relaxed);
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
            // 启动清除:上次会话的卷前缀中,本次未挂载(或已停用)的移除——
            // 落盘索引不重扫,脏卷条目无自然消亡机制,必须在建流/就绪前清掉。
            volumes::purge_unmounted(&state, &desired_set);
            (last_id, true)
        }
        Err(e) => {
            log::info!("无可用落盘索引({e}),首扫建索引");
            state.set_state(state::BUILDING);
            // 水位在扫描**之前**取:扫描窗口内发生的变更,其事件 ID 均大于它,
            // 随后从该水位建流重放必覆盖(diff 幂等)。若取在扫描完成后,
            // 扫描读过该目录之后才出现的变更将永久缺失(无任何后续机制补回)。
            let water = fsevents::current_event_id();
            // 流式首扫(product 场景 2「建索引期间搜索」):先挂「仅含根」的索引,
            // 再按批聚合,批间释放写锁——首扫期间 query 返回已扫部分、status 可见进度。
            // (重扫路径不这样做:tech.md 要求「重扫期间旧索引可查」,新索引建成才替换。)
            let mut index = Index::new();
            let mut staging_map: HashMap<PathBuf, u32> = HashMap::new();
            engine::scanner::seed_roots(&mut index, &mut staging_map, &roots);
            *state.holder.write().unwrap() = IndexHolder::Mutable(index);
            let (recs, gaps) = engine::scanner::collect_records(&roots, &excludes, threads);
            state.built_at.store(now_secs(), Ordering::Relaxed);
            let mut gap_list = invalid_roots_gaps(&roots);
            gap_list.extend(gaps.permission.iter().map(|(p, r)| Gap {
                path: p.to_string_lossy().into_owned(),
                reason: r.clone(),
            }));
            *state.gaps.lock().unwrap() = gap_list;
            state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
            for chunk in recs.chunks(SCAN_CHUNK) {
                let mut h = state.holder.write().unwrap();
                if let IndexHolder::Mutable(idx) = &mut *h {
                    engine::scanner::apply_records(idx, &mut staging_map, chunk);
                }
            }
            state.applied_event_id.store(water, Ordering::Relaxed);
            state.seen_event_id.store(water, Ordering::Relaxed);
            rebuild_dir_map(&state);
            flush(&state, &paths);
            state.set_state(state::READY);
            // 从扫描前水位重放:补齐扫描窗口变更,追平后转纯实时
            (water, false)
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
    std::thread::spawn(move || {
        consume_events(
            st,
            p,
            Some(stream),
            rx,
            is_replay,
            replayed_from_disk,
            false,
        )
    });
    // 卷监听:挂载/卸载对账(主流已建;运行期新挂载的卷用按卷流,不写全局水位)
    {
        let st = state.clone();
        let p = paths.clone();
        std::thread::spawn(move || volumes::monitor_loop(st, p));
    }
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
    let body = encode_response(resp)?;
    stream.write_all(&(body.len() as u32).to_le_bytes())?;
    stream.write_all(&body)?;
    stream.flush()?;
    Ok(())
}

/// 响应写侧体积预算:client 读侧拒绝超过 MAX_MSG 的响应,1000 条深路径结果
/// (路径均值可达 1KB)会把合法查询整体打成「响应超限」失败。超限时按上限
/// 二分收缩 entries 后重序列化——total 保持真实,输出语义即「仅展示前 N 条」。
fn encode_response(resp: Response) -> serde_json::Result<Vec<u8>> {
    let mut body = serde_json::to_vec(&resp)?;
    if body.len() <= protocol::MAX_MSG {
        return Ok(body);
    }
    let Response::Query {
        mut entries,
        total,
        elapsed_ms,
        stat_ms,
        sort_echo,
        desc_echo,
    } = resp
    else {
        // 非查询响应不该接近上限(状态/排除列表都是小对象):原样返回交 client 判定
        return Ok(body);
    };
    while !entries.is_empty() && body.len() > protocol::MAX_MSG {
        entries.truncate(entries.len() / 2);
        body = serde_json::to_vec(&Response::Query {
            entries: entries.clone(),
            total,
            elapsed_ms,
            stat_ms,
            sort_echo: sort_echo.clone(),
            desc_echo,
        })?;
    }
    Ok(body)
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
        Request::Query {
            query,
            limit,
            sort,
            desc,
            ..
        } => run_query(state, &query, limit, sort.as_deref(), desc.unwrap_or(false)),
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
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            }
        }
        Request::Stop { .. } => {
            state.stopping.store(true, Ordering::SeqCst);
            // 最终落盘完成后才响应(契约:响应即落盘已保证);落盘失败如实 ok=false
            let (ok, n) = persist_final(state, paths);
            let _ = std::fs::remove_file(paths.socket());
            Response::Stop {
                ok,
                final_entry_count: n,
            }
        }
        Request::ExcludeList { .. } => Response::Exclude {
            ok: true,
            excluded: state.cfg.lock().unwrap().excluded.clone(),
            message: None,
        },
        Request::ExcludeAdd { path, .. } => exclude_response(state, paths, &path, ExcludeOp::Add),
        Request::ExcludeRemove { path, .. } => {
            exclude_response(state, paths, &path, ExcludeOp::Remove)
        }
        Request::ExcludeReset { .. } => reset_excludes(state, paths),
        Request::VolumesSet { auto, excluded, .. } => volumes_set(state, paths, auto, excluded),
        Request::VolumesPrefs { .. } => volumes_prefs_response(state),
    }
}

/// 卷偏好写入(external-volumes):卷名清洗(去空白/去空串/排序去重)→
/// 内存 + 落盘 → VolumeMonitor 下一轮(≤3s)对账生效(关主开关即清除全部卷,
/// 重开即立即索引挂载中的放行卷)。
fn volumes_set(
    state: &Arc<EngineState>,
    paths: &DaemonPaths,
    auto: bool,
    excluded: Vec<String>,
) -> Response {
    let mut cleaned: Vec<String> = excluded
        .iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    cleaned.sort();
    cleaned.dedup();
    let (auto_now, excluded_now, message) = {
        let mut c = state.cfg.lock().unwrap();
        c.auto_index_volumes = auto;
        c.volumes_excluded = cleaned.clone();
        match config::save(&c, &paths.config_file()) {
            Ok(()) => (auto, cleaned, None),
            Err(e) => {
                log::error!("卷偏好落盘失败: {e}");
                // 内存已生效(监听对账用内存态),落盘失败如实告知(重启会丢)
                (
                    c.auto_index_volumes,
                    c.volumes_excluded.clone(),
                    Some(format!("已临时生效但写入配置失败: {e}")),
                )
            }
        }
    };
    log::info!("卷偏好更新:auto={auto_now} excluded={excluded_now:?}");
    Response::VolumesPrefs {
        auto: auto_now,
        excluded: excluded_now,
        message,
    }
}

/// 卷偏好读(设置页回显)。
fn volumes_prefs_response(state: &Arc<EngineState>) -> Response {
    let c = state.cfg.lock().unwrap();
    Response::VolumesPrefs {
        auto: c.auto_index_volumes,
        excluded: c.volumes_excluded.clone(),
        message: None,
    }
}

/// 排除变更的统一应答:失败如实回 ok=false + 可操作原因(不再恒 ok=true)。
fn exclude_response(
    state: &Arc<EngineState>,
    paths: &DaemonPaths,
    path: &str,
    op: ExcludeOp,
) -> Response {
    let excluded = || state.cfg.lock().unwrap().excluded.clone();
    match apply_exclude_change(state, paths, path, op) {
        Ok(()) => Response::Exclude {
            ok: true,
            excluded: excluded(),
            message: None,
        },
        Err(msg) => Response::Exclude {
            ok: false,
            excluded: excluded(),
            message: Some(msg),
        },
    }
}

enum ExcludeOp {
    Add,
    Remove,
}

/// 恢复内置默认排除项(设置页「恢复默认」/ CLI `exclude reset`)。
///
/// 顺序有语义:先落盘默认配置,成功后才替换内存并重扫。保存失败时内存配置与
/// 索引都不变(返回当前列表 + 可操作原因)——不能先改内存再赌落盘成功。
/// 不持 `cfg` 锁跨越保存/重扫;成功后一次全量重扫使默认排除项同时生效。
fn reset_excludes(state: &Arc<EngineState>, paths: &DaemonPaths) -> Response {
    // 与 add/remove 共用变更锁:保存/内存替换/重扫整体串行,防配置与内存错位。
    let _op = state.exclude_op_lock.lock().unwrap();
    let defaults = config::default_config();
    if let Err(e) = config::save(&defaults, &paths.config_file()) {
        log::error!("恢复默认排除目录:配置保存失败: {e}");
        return Response::Exclude {
            ok: false,
            excluded: state.cfg.lock().unwrap().excluded.clone(),
            message: Some(format!("配置保存失败({e}),排除目录未改变")),
        };
    }
    *state.cfg.lock().unwrap() = defaults.clone();
    degrade_and_rescan(state, paths);
    Response::Exclude {
        ok: true,
        excluded: defaults.excluded,
        message: None,
    }
}

/// 排除路径校验(tech.md 契约):须为绝对路径且位于扫描根内,否则拒绝。
/// 两侧都做 canonical 兜底比较(/var → /private/var 符号链接形态统一)。
fn validate_exclude_in_roots(path: &str, roots: &[PathBuf]) -> Result<(), String> {
    protocol::validate::exclude_path(path)?;
    let canon = |x: &Path| x.canonicalize().unwrap_or_else(|_| x.to_path_buf());
    let p = canon(Path::new(path));
    if roots.iter().any(|r| p.starts_with(canon(r))) {
        return Ok(());
    }
    Err(format!(
        "路径不在扫描根内: {path}(扫描根: {});排除仅作用于扫描根内的目录",
        roots
            .iter()
            .map(|r| r.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    ))
}

/// 排除变更:校验 → 更新配置 → 落盘配置 → 重扫使变更生效。
/// Add 须为扫描根内绝对路径;Remove 只要求条目已存在——默认配置含
/// `node_modules`/`.git` 等相对名,设置页须能移除它们,且待移除条目可能
/// 已不在扫描根内(如根目录被重命名),故不做绝对路径/根内校验。
/// 运行时无定向增量(受影响范围重扫可作后续优化),重扫保证正确性;
/// 失败(路径非法/越界/条目不存在/配置落盘失败)返回可操作原因。
fn apply_exclude_change(
    state: &Arc<EngineState>,
    paths: &DaemonPaths,
    path: &str,
    op: ExcludeOp,
) -> Result<(), String> {
    // 跨连接串行化:校验/内存更新/落盘/重扫期间不允许另一 Exclude 变更穿插。
    let _op = state.exclude_op_lock.lock().unwrap();
    match op {
        ExcludeOp::Add => {
            let roots = state.roots.lock().unwrap().clone();
            validate_exclude_in_roots(path, &roots)?;
            let mut cfg = state.cfg.lock().unwrap();
            if !cfg.excluded.iter().any(|e| e == path) {
                cfg.excluded.push(path.to_string());
            }
        }
        ExcludeOp::Remove => {
            let mut cfg = state.cfg.lock().unwrap();
            if !cfg.excluded.iter().any(|e| e == path) {
                return Err(format!("条目不存在: {path}"));
            }
            cfg.excluded.retain(|e| e != path);
        }
    }
    let cfg = state.cfg.lock().unwrap().clone();
    if let Err(e) = config::save(&cfg, &paths.config_file()) {
        log::error!("配置保存失败: {e}");
        return Err(format!("配置保存失败({e}),变更未持久化,重启后将丢失"));
    }
    degrade_and_rescan(state, paths);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造:空索引 + 一条指向 tempdir 的根目录条目,dir_map 键为 canonical 形态。
    /// 卷偏好写入:内存态更新 + 配置落盘 + 回显清洗(external-volumes)。
    #[test]
    fn volumes_set_persists_and_echoes_cleaned() {
        let tmp = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let paths = DaemonPaths {
            data_dir: tmp.path().join("data"),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let resp = volumes_set(
            &state,
            &paths,
            false,
            vec![
                "  BackupU ".into(),
                String::new(),
                "BackupU".into(),
                "OD".into(),
            ],
        );
        let Response::VolumesPrefs { auto, excluded, .. } = resp else {
            panic!("应答应为 VolumesPrefs");
        };
        assert!(!auto, "主开关回显");
        assert_eq!(
            excluded,
            vec!["BackupU", "OD"],
            "清洗:去空白/去空串/排序去重"
        );
        {
            let c = state.cfg.lock().unwrap();
            assert!(
                !c.auto_index_volumes && c.volumes_excluded == excluded,
                "内存态生效"
            );
        }
        // 落盘可读回(旧文件 serde default 兜底语义同时被覆盖)
        let reloaded = config::load(&paths.config_file());
        assert!(!reloaded.auto_index_volumes);
        assert_eq!(reloaded.volumes_excluded, excluded);
        // 读应答
        let Response::VolumesPrefs { auto, excluded, .. } = volumes_prefs_response(&state) else {
            panic!("读应答应为 VolumesPrefs");
        };
        assert!(!auto && excluded == vec!["BackupU", "OD"]);
    }

    fn mounted_state(dir: &Path) -> Arc<EngineState> {
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.to_string_lossy().as_ref(), NO_PARENT, true);
        {
            let mut map = state.dir_map.lock().unwrap();
            map.insert(dir.canonicalize().unwrap(), root);
        }
        *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
        state
    }

    /// 水位的核心不变量钉:applied 只在 diff 完成后推进,「已收未应用」不得计入。
    /// 突变「事件到达即推进落盘水位」——即 stop 前 100ms 变更永久丢失的根因链——
    /// 时第一段断言必红。
    #[test]
    fn applied_watermark_lags_until_batch_applied() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let state = mounted_state(dir.path());

        // 事件到达:seen 推进,applied 不动
        state.seen_event_id.store(100, Ordering::Relaxed);
        assert_eq!(
            state.applied_event_id.load(Ordering::Relaxed),
            0,
            "未应用的接收水位不得进入落盘水位"
        );

        // 应用一批 → 水位追上,且 diff 真实生效(新文件入索引)
        apply_batch(
            &state,
            vec![dir.path().to_string_lossy().into_owned()],
            true,
        );
        assert_eq!(
            state.applied_event_id.load(Ordering::Relaxed),
            100,
            "批次应用后水位推进"
        );
        let h = state.holder.read().unwrap();
        let IndexHolder::Mutable(idx) = &*h else {
            panic!("应为可变索引")
        };
        assert_eq!(idx.entry_count(), 2, "根 + a.txt");
    }

    /// 空批次不推进水位:否则停止路径会把「窗口内未应用的 seen」写进落盘 meta,
    /// 重启重放即跳过这些变更。
    #[test]
    fn empty_batch_does_not_advance_watermark() {
        let state = EngineState::new();
        state.seen_event_id.store(7, Ordering::Relaxed);
        apply_batch(&state, vec![], true);
        assert_eq!(state.applied_event_id.load(Ordering::Relaxed), 0);
    }

    /// 水位取批次内最大 ID(tech.md:回放段事件 ID 非严格递增,水位必须取 max)。
    /// 乱序到达后 seen 恒为该批最大 ID,应用后 applied 直接落到最大值。
    #[test]
    fn seen_watermark_takes_batch_max() {
        let state = EngineState::new();
        for id in [100u64, 50, 200, 150] {
            state.seen_event_id.fetch_max(id, Ordering::Relaxed);
        }
        assert_eq!(state.seen_event_id.load(Ordering::Relaxed), 200);
    }

    /// 重扫覆盖跳过钉——死循环修复核心(实测真实实例 420 次重复重扫:
    /// 重扫期间消费停摆→内核缓冲溢出→新 dropped 标记→又重扫)。
    /// 突变比较符(≤ → <)或删除此检查,循环即可能回归。
    #[test]
    fn stale_events_below_rescan_floor_are_skipped() {
        assert!(event_covered_by_rescan(100, 100), "floor 本身属历史事件");
        assert!(event_covered_by_rescan(99, 100));
        assert!(
            !event_covered_by_rescan(101, 100),
            "floor 之后的变更是新事件"
        );
        assert!(!event_covered_by_rescan(1, 0), "未重扫过时无覆盖");
    }

    /// 重扫推进覆盖水位:degrade_and_rescan 后 floor 必须 ≥ 重扫开始时刻的全局
    /// 水位且已应用水位同步推进,否则积压事件不被跳过、死循环回归。
    #[test]
    fn rescan_advances_floor_and_watermarks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let state = EngineState::new();
        *state.roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        let paths = DaemonPaths {
            data_dir: dir.path().join("data"),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let before = fsevents::current_event_id();
        degrade_and_rescan(&state, &paths);
        let floor = state.rescan_floor.load(Ordering::Relaxed);
        assert!(floor >= before, "覆盖水位取重扫开始时刻");
        assert!(state.applied_event_id.load(Ordering::Relaxed) >= floor);
        assert_eq!(*state.state_name.lock().unwrap(), state::READY);
        assert!(paths.index_file().exists(), "重扫后落盘");
    }

    /// desc_echo 绝对方向映射钉:NAME/SIZE/PATH 自然升序,MTIME 自然降序,
    /// relevance 无方向;协议 desc 位 = 反转自然方向。
    /// 突变 MTIME 分支(!desc → desc)或其余列分支,表内断言必红。
    #[test]
    fn desc_echo_maps_protocol_bit_to_absolute_direction() {
        let cases: [(Option<&str>, bool, Option<bool>); 7] = [
            (None, false, None),
            (Some("name"), false, Some(false)),
            (Some("name"), true, Some(true)),
            (Some("mtime"), false, Some(true)),
            (Some("mtime"), true, Some(false)),
            (Some("size"), true, Some(true)),
            (Some("path"), true, Some(true)),
        ];
        for (sort, desc, want) in cases {
            let (key, _) = parse_sort_key(sort);
            assert_eq!(
                desc_echo(key, desc),
                want,
                "sort={sort:?} desc={desc} 应回显 {want:?}"
            );
        }
    }

    /// is_dir 组装路径钉:relevance 排序下排序轮 stat 缓存为空,两行都走锁外
    /// 补 stat(miss)路径;size 排序下两行在排序轮已 stat,走缓存命中路径。
    /// 两条路径的响应都必须保留索引条目的目录真值。
    #[test]
    fn query_response_keeps_is_dir_on_cache_hit_and_miss_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("hit_dir")).unwrap();
        std::fs::write(dir.path().join("hit_file.txt"), b"x").unwrap();
        let state = mounted_state(dir.path());
        {
            let mut h = state.holder.write().unwrap();
            let IndexHolder::Mutable(idx) = &mut *h else {
                panic!("应为可变索引")
            };
            let root = idx
                .entries()
                .iter()
                .position(|e| e.parent == NO_PARENT && e.is_dir())
                .expect("mounted_state 应含根目录条目") as u32;
            idx.insert("hit_dir", root, true);
            idx.insert("hit_file.txt", root, false);
        }

        for (sort, label) in [
            (None, "relevance(补 stat miss)"),
            (Some("size"), "size(缓存命中)"),
        ] {
            let resp = run_query(&state, "hit", 10, sort, false);
            let Response::Query { entries, .. } = resp else {
                panic!("{label} 应返回查询响应: {resp:?}");
            };
            assert_eq!(entries.len(), 2, "{label} 应命中目录与文件");
            let dir_hit = entries
                .iter()
                .find(|e| e.name == "hit_dir")
                .unwrap_or_else(|| panic!("{label} 缺目录命中"));
            assert!(dir_hit.is_dir, "{label}: 目录命中 is_dir 应为 true");
            let file_hit = entries
                .iter()
                .find(|e| e.name == "hit_file.txt")
                .unwrap_or_else(|| panic!("{label} 缺文件命中"));
            assert!(!file_hit.is_dir, "{label}: 文件命中 is_dir 应为 false");
        }
    }

    /// 排除路径校验钉(tech.md 契约:必须为扫描根内绝对路径,否则拒绝)。
    /// 突变「只查绝对路径不查根内」时根外断言必红。
    #[test]
    fn exclude_path_must_be_inside_scan_roots() {
        let roots = vec![PathBuf::from("/Users/t"), PathBuf::from("/Applications")];
        assert!(validate_exclude_in_roots("/Users/t/Library", &roots).is_ok());
        assert!(validate_exclude_in_roots("/Applications/Xcode.app", &roots).is_ok());
        assert!(
            validate_exclude_in_roots("/Users/t", &roots).is_ok(),
            "根本身合法"
        );
        // 根外绝对路径
        assert!(validate_exclude_in_roots("/etc", &roots).is_err());
        assert!(validate_exclude_in_roots("/System/Library", &roots).is_err());
        // 相对名(上游只放行绝对路径)
        assert!(validate_exclude_in_roots("node_modules", &roots).is_err());
        assert!(validate_exclude_in_roots("~/Library", &roots).is_err());
    }

    /// 排除变更夹具:临时扫描根(独立于数据目录)+ 初始配置。
    fn exclude_fixture(excluded: &[&str]) -> (tempfile::TempDir, Arc<EngineState>, DaemonPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scan");
        std::fs::create_dir(&root).unwrap();
        let state = Arc::new(EngineState::new());
        *state.roots.lock().unwrap() = vec![root];
        *state.cfg.lock().unwrap() = config::Config {
            excluded: excluded.iter().map(|e| (*e).to_string()).collect(),
            auto_index_volumes: true,
            volumes_excluded: vec![],
        };
        let paths = DaemonPaths {
            data_dir: tmp.path().join("data"),
        };
        (tmp, state, paths)
    }

    /// 移除只要求条目存在:默认配置里的相对名(node_modules/.git)及
    /// ~/Library 都是设置页可见条目,必须可移除,不得再走绝对路径/根内校验。
    /// 突变「Remove 仍先做 validate_exclude_in_roots」时本测试必红。
    #[test]
    fn exclude_remove_accepts_existing_relative_name() {
        let (_tmp, state, paths) = exclude_fixture(&["node_modules", "/abs/x"]);
        apply_exclude_change(&state, &paths, "node_modules", ExcludeOp::Remove)
            .expect("已存在的相对名条目应可移除");
        assert_eq!(
            state.cfg.lock().unwrap().excluded,
            vec!["/abs/x".to_string()]
        );
        assert_eq!(
            config::load(&paths.config_file()).excluded,
            vec!["/abs/x".to_string()],
            "移除结果应落盘"
        );
    }

    /// 移除不存在的条目:如实报「条目不存在」,内存与落盘配置均不变。
    #[test]
    fn exclude_remove_missing_entry_errors_without_change() {
        let (_tmp, state, paths) = exclude_fixture(&["node_modules", "/abs/x"]);
        config::save(&state.cfg.lock().unwrap(), &paths.config_file()).unwrap();
        let err = apply_exclude_change(&state, &paths, "/not/existing", ExcludeOp::Remove)
            .expect_err("不存在的条目应拒绝移除");
        assert_eq!(err, "条目不存在: /not/existing");
        assert_eq!(
            state.cfg.lock().unwrap().excluded,
            vec!["node_modules".to_string(), "/abs/x".to_string()],
            "失败不得改动内存配置"
        );
        assert_eq!(
            config::load(&paths.config_file()).excluded,
            vec!["node_modules".to_string(), "/abs/x".to_string()],
            "失败不得落盘"
        );
    }

    /// 恢复默认(E5):自定义条目清空 → 内存/落盘均为内置默认集,并按**新排除集**
    /// 重扫(原本被自定义条目剪枝的目录/文件重置后必须入索引)。状态从 BUILDING
    /// 推进到 READY;单次 reset 只触发这一次全量重扫,不为每条默认单独重扫。
    #[test]
    fn exclude_reset_restores_defaults_and_rescans_with_new_excludes() {
        let (_tmp, state, paths) = exclude_fixture(&["node_modules"]);
        let root = state.roots.lock().unwrap()[0].clone();
        // 自定义排除目录里放一个文件:重置前整棵子树被剪枝,重置后应被索引。
        let custom_dir = root.join("custom_excluded");
        std::fs::create_dir(&custom_dir).unwrap();
        let hidden_file = custom_dir.join("inside.txt");
        std::fs::write(&hidden_file, b"x").unwrap();
        let custom = custom_dir.to_string_lossy().into_owned();
        state.cfg.lock().unwrap().excluded.push(custom.clone());
        config::save(&state.cfg.lock().unwrap(), &paths.config_file()).unwrap();

        let resp = reset_excludes(&state, &paths);
        let defaults = config::default_config();
        match resp {
            Response::Exclude {
                ok,
                excluded,
                message,
            } => {
                assert!(ok, "恢复默认应成功:{message:?}");
                assert_eq!(excluded, defaults.excluded);
                assert!(message.is_none());
            }
            other => panic!("意外响应: {other:?}"),
        }

        let memory = state.cfg.lock().unwrap().excluded.clone();
        assert_eq!(memory, defaults.excluded, "内存配置须恢复默认排除项");
        assert!(
            !memory.iter().any(|e| e == &custom),
            "自定义条目必须消失:{memory:?}"
        );
        assert_eq!(
            config::load(&paths.config_file()).excluded,
            defaults.excluded,
            "默认配置须落盘"
        );
        assert!(
            *state.state_name.lock().unwrap() == state::READY,
            "恢复默认须完成一次重扫(状态 READY)"
        );
        let hidden_path = hidden_file.to_string_lossy().into_owned();
        let indexed = with_view(&state, |v| {
            (0..v.entries().len() as u32)
                .filter(|i| v.entries()[*i as usize].is_live())
                .any(|i| v.full_path_dyn(i) == hidden_path)
        })
        .expect("重扫后应挂载可变索引");
        assert!(
            indexed,
            "重扫必须使用新排除集:原自定义排除目录内的文件应被收录"
        );
    }

    /// 恢复默认先落盘:保存失败时内存配置不变、不触发重扫,并如实返回
    /// ok=false + 当前列表 + 可操作原因。
    #[test]
    fn exclude_reset_save_failure_keeps_memory_and_does_not_rescan() {
        let (_tmp, state, paths) = exclude_fixture(&["/custom/x", "node_modules"]);
        // 目标路径被目录占用 → save 的 rename 失败(与 settings 保存失败测同法)。
        std::fs::create_dir_all(paths.config_file()).unwrap();

        let resp = reset_excludes(&state, &paths);
        match resp {
            Response::Exclude {
                ok,
                excluded,
                message,
            } => {
                assert!(!ok, "保存失败不得报成功");
                assert_eq!(
                    excluded,
                    vec!["/custom/x".to_string(), "node_modules".to_string()],
                    "失败须返回当前(未改变)列表"
                );
                assert!(
                    message
                        .as_deref()
                        .is_some_and(|m| m.starts_with("配置保存失败(")),
                    "失败原因须可操作:{message:?}"
                );
            }
            other => panic!("意外响应: {other:?}"),
        }
        assert_eq!(
            state.cfg.lock().unwrap().excluded,
            vec!["/custom/x".to_string(), "node_modules".to_string()],
            "保存失败不得改动内存配置"
        );
        assert!(
            *state.state_name.lock().unwrap() == state::BUILDING,
            "保存失败不得触发重扫(状态保持原样)"
        );
    }

    /// 跨连接变更串行化回归:add 与 reset 都必须等待 `exclude_op_lock` 释放。
    /// started 信号保证线程已进入被调函数后再断言「超时未完成」,避免调度抖动
    /// 被误判成锁生效;若函数漏用锁,小夹具会立即完成而断言失败。
    #[test]
    fn exclude_mutations_serialize_across_threads() {
        use std::sync::mpsc::{self, RecvTimeoutError};
        use std::time::Duration;

        let (tmp, state, paths) = exclude_fixture(&["node_modules"]);
        config::save(&state.cfg.lock().unwrap(), &paths.config_file()).unwrap();
        let sub = tmp.path().join("scan").join("sub");
        std::fs::create_dir(&sub).unwrap();
        let sub = sub.to_string_lossy().into_owned();

        // add:持锁时线程必须阻塞,释放后成功。
        let guard = state.exclude_op_lock.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let st = state.clone();
        let p = paths.clone();
        let arg = sub.clone();
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx
                .send(apply_exclude_change(&st, &p, &arg, ExcludeOp::Add))
                .unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("add 线程应已启动");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_millis(150))
                .unwrap_err(),
            RecvTimeoutError::Timeout,
            "add 必须在排除变更锁内等待"
        );
        drop(guard);
        let added = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("释放锁后 add 应完成");
        handle.join().unwrap();
        assert!(added.is_ok(), "{added:?}");

        // reset:同样必须等待变更锁。
        let guard = state.exclude_op_lock.lock().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let st = state.clone();
        let p = paths.clone();
        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(reset_excludes(&st, &p)).unwrap();
        });
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("reset 线程应已启动");
        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_millis(150))
                .unwrap_err(),
            RecvTimeoutError::Timeout,
            "reset 必须在排除变更锁内等待"
        );
        drop(guard);
        let resp = done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("释放锁后 reset 应完成");
        handle.join().unwrap();
        match resp {
            Response::Exclude { ok, excluded, .. } => {
                assert!(ok);
                assert_eq!(excluded, config::default_config().excluded);
            }
            other => panic!("意外响应: {other:?}"),
        }
    }

    /// Add 契约不变:相对名/根外路径仍拒绝,仅根内绝对路径可加入。
    /// 突变「为支持 Remove 而放宽 Add 校验」时相对名/根外断言必红。
    #[test]
    fn exclude_add_still_requires_absolute_path_inside_scan_roots() {
        let (tmp, state, paths) = exclude_fixture(&[]);
        let err = apply_exclude_change(&state, &paths, "relative/x", ExcludeOp::Add)
            .expect_err("相对名不得加入");
        assert_eq!(err, "必须是绝对路径");
        let err = apply_exclude_change(&state, &paths, "/etc", ExcludeOp::Add)
            .expect_err("根外绝对路径不得加入");
        assert!(err.contains("路径不在扫描根内"));
        assert!(
            state.cfg.lock().unwrap().excluded.is_empty(),
            "被拒绝的 Add 不得改动配置"
        );
        assert!(!paths.config_file().exists(), "被拒绝的 Add 不得落盘");

        let inside = tmp.path().join("scan").join("sub");
        std::fs::create_dir(&inside).unwrap();
        let inside = inside.to_string_lossy().into_owned();
        apply_exclude_change(&state, &paths, &inside, ExcludeOp::Add)
            .expect("根内绝对路径应可加入");
        assert_eq!(state.cfg.lock().unwrap().excluded, vec![inside.clone()]);
        assert_eq!(
            config::load(&paths.config_file()).excluded,
            vec![inside],
            "加入结果应落盘"
        );
    }
}

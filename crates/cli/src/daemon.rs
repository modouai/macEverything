//! daemon:引擎宿主——冷启动编排(秒开直挂 Base → 重放/重扫)+ 增量循环 + 服务循环。
//! 单实例:绑定 EADDRINUSE 时 connect 探测——连得上 = 已在运行;连不上 = 崩溃
//! 残留 stale socket,unlink 重绑。stop 经 socket 消息触发,响应在最终落盘后返回。
use anyhow::{bail, Context, Result};
use client::{stat_of, stat_rows, state, DaemonPaths};
use engine::config;
use engine::dir_map::DirIndexMap;
use engine::entry::NO_PARENT;
use engine::fsevents::{self, FseStream};
use engine::index::{Index, IndexView};
use engine::persist::{self, IndexMeta, SharedView};
use engine::query::{MetaRow, SortKey};
use engine::reconcile;
use engine::watcher::{self, DirBatcher};

mod volumes;
use protocol::{self, Gap, HitEntry, Request, Response};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 停机时长超过此阈值且重放零事件 → 判重放不可信,转全量重扫(ADR-5 保守判定器)。
pub const REPLAY_DOWNTIME_THRESHOLD_SECS: u64 = 48 * 3600;
/// 落盘节流:距上次落盘超过此间隔且有变更 → 触发快照。
/// T4 放宽(tech.md §4.3):mmap Base 落地后落盘不再承担内存压力缓解(A1 由
/// 「常驻 = mmap Base + 小 Overlay」兜底),拉长间隔/提高批量以减少全量重写与
/// remap 频率;停机 flush 时机不变。
const FLUSH_INTERVAL: Duration = Duration::from_secs(120);
/// 落盘节流:累计变更超过此条数 → 立即触发快照(放宽理由同上)。
const FLUSH_DIRTY_LIMIT: u64 = 30_000;
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

/// Overlay 态的 Base 载体:mmap 直挂(冷启动后的常态,footprint≈0)或堆索引
/// (首扫/重扫产物挂载后;T4 替换段落盘 remap 后退役为 Mmap)。
enum BaseRef {
    Mmap(Box<SharedView>),
    Heap(Index),
}

impl BaseRef {
    fn as_view(&self) -> &dyn IndexView {
        match self {
            BaseRef::Mmap(v) => v.as_ref(),
            BaseRef::Heap(i) => i,
        }
    }

    /// 需要可变性的路径(前缀摘除/卷种子)按需物化(条目号 1:1,Overlay 账本
    /// 锚定的 base 条目号不受影响)。
    fn ensure_mutable(&mut self) {
        if let BaseRef::Mmap(v) = self {
            *self = BaseRef::Heap(v.materialize());
        }
    }
}

/// T3 增量态(daemon 常驻 = Base + Overlay,tech.md §4.2):增量 diff 全部写入
/// overlay,Base 只读驻留;compact 在 Overlay 活跃期跳过,重扫替换 holder 时
/// Overlay 整体丢弃(全量产物已覆盖一切增量)。
struct OverlaidState {
    base: BaseRef,
    overlay: engine::overlay::Overlay,
}

enum IndexHolder {
    Empty,
    /// 冷启动直挂(mmap 只读,零堆驻留);Overlay 恒空——首个 diff 即转 Overlaid。
    Shared(Box<SharedView>),
    /// 全量堆索引(首扫 staging/重扫产物/卷产物);Overlay 恒空。
    Mutable(Index),
    Overlaid(Box<OverlaidState>),
}

/// diff 入口的态转换(T3:diff 直写 Overlay,不再物化整个 Base——tech.md §4.3
/// 「ensure_mutable 退役」的 T3 形态):Shared/Mutable → Overlaid,已 Overlaid 原样。
/// 转换保持条目号 1:1(Shared 本就是视图,Mutable 平移为 Heap 载体),dir_map 有效。
fn ensure_overlay_ready(h: &mut IndexHolder) {
    match h {
        IndexHolder::Shared(_) | IndexHolder::Mutable(_) => {
            let taken = std::mem::replace(h, IndexHolder::Empty);
            *h = match taken {
                IndexHolder::Shared(v) => IndexHolder::Overlaid(Box::new(OverlaidState {
                    base: BaseRef::Mmap(v),
                    overlay: engine::overlay::Overlay::new(),
                })),
                IndexHolder::Mutable(idx) => IndexHolder::Overlaid(Box::new(OverlaidState {
                    base: BaseRef::Heap(idx),
                    overlay: engine::overlay::Overlay::new(),
                })),
                other => other,
            };
        }
        IndexHolder::Overlaid(_) | IndexHolder::Empty => {}
    }
}

/// 可变 base 槽位(持 holder 写锁期间取):Shared/Overlaid{Mmap} 需先经
/// [`ensure_mutable`] 物化。
fn mutable_base_slot(h: &mut IndexHolder) -> Option<&mut Index> {
    match h {
        IndexHolder::Mutable(idx) => Some(idx),
        IndexHolder::Overlaid(st) => match &mut st.base {
            BaseRef::Heap(idx) => Some(idx),
            BaseRef::Mmap(_) => None,
        },
        _ => None,
    }
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
    dir_map: Mutex<DirIndexMap>,
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
    /// 重扫互斥仲裁:自动源 `try_lock`(失败即弃——dropped/流断开类本就该忽略),
    /// exclude 源 `lock()`(阻塞等待)。锁覆盖扫描段 + 替换段全程:若只护扫描段,
    /// exclude 同步重扫完成后迟到的自动替换段会用更旧产物覆盖 holder 并落盘,
    /// 回退 exclude 成果。
    rescan_lock: Mutex<()>,
    /// 重扫进行中(自动与 exclude 路径都置):期间再收 dropped 直接忽略(替换段
    /// 的 rescan_floor 与重放会追平);diff 照常执行并记入 `rescan_dirty`。
    rescanning: AtomicBool,
    /// 重扫窗口内 diff 过的目录(canonical 形态)。重扫产出的新索引不含窗口内
    /// 变更——替换段完成后逐目录重放 diff 追平(diff 以磁盘为准,幂等)。
    rescan_dirty: Mutex<HashSet<PathBuf>>,
    /// 上次全量重扫完成时刻(防「重扫→新 dropped→重扫」振荡的第二道闸;原先
    /// 是 consume_events 局部变量,重扫移入后台线程后上移至 state)。
    last_rescan: Mutex<Option<Instant>>,
    /// diff 持久失败负缓存(canonical 目录路径)。EPERM/连续失败目录短路后续
    /// diff:EPERM 目录保留索引条目(与首扫「入索引 + 记 gaps + 空子树」语义
    /// 一致)但不再重试——摘除会让父 diff 重插,制造插-摘振荡。键用**路径**而非
    /// 条目号:compact 全量重排条目号,旧号会错杀无辜条目。进程内存活,重启
    /// 清零(用户补授权限后重启即自动重新索引)。
    perm_denied: Mutex<HashSet<PathBuf>>,
    /// 未知 errno 的连续失败计数(累计 3 次按持久失败入负缓存)。
    diff_fail_counts: Mutex<HashMap<PathBuf, u8>>,
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
            dir_map: Mutex::new(DirIndexMap::default()),
            roots: Mutex::new(vec![]),
            cfg: Mutex::new(config::Config {
                excluded: vec![],
                auto_index_volumes: false,
                volumes_excluded: vec![],
            }),
            exclude_op_lock: Mutex::new(()),
            rescan_lock: Mutex::new(()),
            rescanning: AtomicBool::new(false),
            rescan_dirty: Mutex::new(HashSet::new()),
            last_rescan: Mutex::new(None),
            perm_denied: Mutex::new(HashSet::new()),
            diff_fail_counts: Mutex::new(HashMap::new()),
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
                    v.entry(*h).is_dir(),
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
        IndexHolder::Overlaid(st) => Some(f(&st.overlay.composite_view(st.base.as_view()))),
        IndexHolder::Empty => None,
    }
}

// —— 冷启动与增量 ——

/// 需要可变 Base 的路径(卷种子/扫描/前缀摘除)的按需物化:Shared → Mutable;
/// Overlaid{Mmap} → Overlaid{Heap}(T3:Overlay 账本保留,base 物化条目号 1:1,
/// anchors/overlaid/dir_ids 锚定的条目号不受影响)。增量 diff 不经此路径——
/// diff 直写 Overlay(tech.md §4.3,ensure_mutable 的 diff 转换点退役)。
/// 全程持 holder 写锁:读者在锁上等待,唤醒后见到的已是可变态,无 Empty 观察窗。
/// 成本(实测,release):1.02M 条目 ~9ms,首次含 mmap 缺页 ~95ms;一次性。
fn ensure_mutable(state: &EngineState) {
    let mut h = state.holder.write().unwrap();
    match &mut *h {
        IndexHolder::Shared(_) => {
            let shared = std::mem::replace(&mut *h, IndexHolder::Empty);
            *h = match shared {
                IndexHolder::Shared(v) => IndexHolder::Mutable(v.materialize()),
                other => other,
            };
        }
        IndexHolder::Overlaid(st) => st.base.ensure_mutable(),
        _ => {}
    }
}

/// 重建「绝对目录路径 → 条目号」映射(冷启动/重扫后一次性)。
/// O(n) 邻接表 + BFS;键为绝对路径,与 FSEvents 事件路径可直接比对。
/// 读侧经 IndexView:Shared(直挂 Base)与 Mutable 皆可建——冷启动不再物化,
/// dir_map 直接从 mmap 视图建立;后续按需转可变时条目号 1:1,映射继续有效。
fn rebuild_dir_map(state: &EngineState) {
    // 两个形态各司其职:根条目名是「字面配置根」(scanner 契约,用于匹配);
    // map 键用 canonical 形态——/tmp 等系统符号链接下 FSEvents 上报真实路径
    // (/private/tmp/...),canonicalize 让事件侧与索引侧同形。
    let literal_roots = state.roots.lock().unwrap().clone();
    let mut map = DirIndexMap::default();
    let h = state.holder.read().unwrap();
    // 只按 Base 建:dir_map 是「绝对路径 → base 条目号」的寻址面,overlay 目录的
    // 寻址在 Overlay::dir_ids(纯 overlay 目录),两空间不混号。
    let view: &dyn IndexView = match &*h {
        IndexHolder::Shared(v) => v.as_ref(),
        IndexHolder::Mutable(i) => i,
        IndexHolder::Overlaid(st) => st.base.as_view(),
        IndexHolder::Empty => return,
    };
    // 分段枚举建邻接表 + 收集根条目号(段内下标 + 段起始号 = 条目号)
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut root_ids: Vec<u32> = Vec::new();
    for &(start, seg) in &view.entry_segments() {
        for (j, e) in seg.iter().enumerate() {
            let i = start + j as u32;
            if e.parent != NO_PARENT && e.is_live() {
                children.entry(e.parent).or_default().push(i);
            }
            if e.parent == NO_PARENT && e.is_live() && e.is_dir() {
                root_ids.push(i);
            }
        }
    }
    let mut queue: std::collections::VecDeque<(PathBuf, u32)> = std::collections::VecDeque::new();
    for i in root_ids {
        // 根条目名 = 完整绝对路径(scanner 契约),按字面根匹配
        let name = String::from_utf8_lossy(view.display_name(i)).into_owned();
        if let Some(r) = literal_roots.iter().find(|r| r.to_string_lossy() == name) {
            let key = r.canonicalize().unwrap_or_else(|_| r.clone());
            map.insert(&key, i);
            queue.push_back((key, i));
        }
    }
    while let Some((abs, idx_p)) = queue.pop_front() {
        if let Some(kids) = children.get(&idx_p) {
            for &k in kids {
                if view.entry(k).is_dir() {
                    let mut p = abs.clone();
                    p.push(String::from_utf8_lossy(view.display_name(k)).into_owned());
                    map.insert(&p, k);
                    queue.push_back((p, k));
                }
            }
        }
    }
    *state.dir_map.lock().unwrap() = map;
}

/// diff 目标解析:先 Base(dir_map,首扫收录的目录),后 Overlay(冷启动后新建的
/// 纯 overlay 目录)。两空间不混号:dir_map 值恒为 base 条目号,overlay 目录经
/// `Overlay::overlay_dir_id` 寻址(死键自愈)。
fn resolve_target(
    map: &DirIndexMap,
    overlay: &mut engine::overlay::Overlay,
    p: &Path,
) -> Option<engine::overlay::DiffTarget> {
    if let Some(b) = map.get(p) {
        return Some(engine::overlay::DiffTarget::Base(b));
    }
    overlay
        .overlay_dir_id(p)
        .map(engine::overlay::DiffTarget::OverlayDir)
}

/// 对单个绝对目录执行 diff(写侧入 Overlay,tech.md §4.2)并维护 dir_map。
/// 锁序:holder(write) → dir_map。返回该目录本次的变更条数(added + removed),
/// 供抽样对拍判定用(计数口径 = 真实漂移:overlay 旧子项集 vs listing;首 diff
/// 的统计旧集回退 base 子项名集,与 diff_dir 时代一致,无漂移返回 0)。
fn apply_and_track(
    state: &EngineState,
    base: &mut BaseRef,
    overlay: &mut engine::overlay::Overlay,
    map: &mut DirIndexMap,
    abs_dir: &Path,
    excludes: &[std::path::PathBuf],
) -> u32 {
    let Some(target) = resolve_target(map, overlay, abs_dir) else {
        return 0;
    };
    if let engine::overlay::DiffTarget::Base(d0) = target {
        // 「新索引 + 旧 map」竞态窗口(替换段 swap 与 rebuild_dir_map 之间,遗留
        // #12):map 残留的条目号对新索引可能**越界**。get+is_live 判活
        // (scanner.rs 同一模式):越界/已死键一律清映射短路,绝不按死号下标。
        // 突变「回退 entry(d0) 直下标」由 stale_dir_map_key 测试钉死。
        let live =
            base.as_view().entry_segments().iter().any(|&(s, seg)| {
                d0 >= s && seg.get((d0 - s) as usize).is_some_and(|e| e.is_live())
            });
        if !live {
            map.remove(abs_dir); // 目录已删(子树墓碑)或键失配:清映射,不做 diff
            return 0;
        }
    }
    // OverlayDir 目标的活性在 resolve_target 内已自愈(死键摘除返回 None)。
    let base_view = base.as_view();
    match overlay.apply_diff(base_view, target, abs_dir, excludes) {
        Ok(stats) => {
            state
                .dirty
                .fetch_add((stats.added + stats.removed) as u64, Ordering::Relaxed);
            // base 子目录寻址面照旧维护(base 空间陈旧副本仍作 overlay 层的
            // diff 目标入口,其 anchor 在 apply_diff 内续接);overlay 目录键由
            // apply_diff 内部落账。
            if let engine::overlay::DiffTarget::Base(d0) = target {
                for k in base_view.children_of(d0) {
                    if base_view.entry(k).is_dir() {
                        let mut p = abs_dir.to_path_buf();
                        p.push(String::from_utf8_lossy(base_view.display_name(k)).into_owned());
                        map.insert(&p, k);
                    }
                }
            }
            stats.added + stats.removed
        }
        Err(e) => {
            state.io_errors.fetch_add(1, Ordering::Relaxed);
            handle_diff_error(state, base, overlay, map, abs_dir, &e);
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
    // 持久失败负缓存:短路(含补链路径)。EPERM 目录保留索引条目但不再重试。
    // 重启清零——用户补授权限后重启即自动重新索引。
    if state.perm_denied.lock().unwrap().contains(&base) {
        return 0;
    }
    // 重扫窗口记账:替换段完成后重放追平。push 不持 holder 锁(在取锁之前);
    // 安全性由「push 恒先于同次 diff 的 apply,且 apply 持 holder 写锁、替换段
    // 挂 holder 也持写锁」的互斥保证:「先 push 后 drain → 基于旧索引 → 被重放」
    // 或「后 push → 基于新索引 → 不重放」,无第三种交错。
    if state.rescanning.load(Ordering::SeqCst) {
        state.rescan_dirty.lock().unwrap().insert(base.clone());
    }
    let excludes = reconcile::expand_excludes(&state.cfg.lock().unwrap().excluded);
    let mut h = state.holder.write().unwrap();
    // T3:diff 直写 Overlay,不再物化 Base(Shared/Mutable 首个 diff 转 Overlaid)。
    ensure_overlay_ready(&mut h);
    let IndexHolder::Overlaid(st) = &mut *h else {
        return 0; // Empty(首扫中):无可 diff 目标
    };
    // 解构改名:局部 `base` 历史语义是 canonical 目录路径,holder 侧载体用 holder_base
    let OverlaidState {
        base: holder_base,
        overlay,
    } = st.as_mut();
    let mut map = state.dir_map.lock().unwrap();
    let mut changed = 0u32;

    // 目标未索引:事件风暴下子目录事件可能先于其父目录的 diff 到达(新建目录树),
    // 直查必然 miss。从最近已索引祖先逐级向下补 diff(每级纳入其直接子项),
    // 再应用目标自身——否则新目录内容要等下次重放/重扫才可见。
    // 寻址两空间:base 目录在 dir_map,纯 overlay 目录在 overlay::dir_ids。
    if resolve_target(&map, overlay, &base).is_none() {
        let mut chain: Vec<PathBuf> = Vec::new(); // [目标, …, 未索引的第一级]
        let mut cur = base.clone();
        loop {
            let Some(parent) = cur.parent().map(Path::to_path_buf) else {
                return changed; // 根之外:忽略
            };
            chain.push(cur.clone());
            cur = parent;
            if resolve_target(&map, overlay, &cur).is_some() {
                break;
            }
        }
        for next in chain.iter().rev() {
            if engine::scanner::is_excluded(next, &excludes) {
                return changed; // 链上出现排除目录:不拉回索引
            }
            changed += apply_and_track(state, holder_base, overlay, &mut map, &cur, &excludes);
            if resolve_target(&map, overlay, next).is_none() {
                return changed; // 祖先 diff 未纳入该级(根外/竞态删):放弃
            }
            cur = next.clone();
        }
        // 走完 chain 后 cur == base(目标自身尚未 diff)
    }

    changed + apply_and_track(state, holder_base, overlay, &mut map, &base, &excludes)
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
/// Shared 态(dirty == 0,无本会话增量)无需重写:盘上索引即当前状态。
/// Overlaid 态(T4 正式形态):合并流式直写 + remap 换 Base + 清 Overlay,
/// 见 [`flush_merged`]。
fn flush(state: &EngineState, paths: &DaemonPaths) {
    let _guard = state.flush_lock.lock().unwrap();
    // Overlaid 分流:合并写需要「放读锁 → 取写锁」的锁形态,单独走 flush_merged
    if matches!(&*state.holder.read().unwrap(), IndexHolder::Overlaid(_)) {
        flush_merged(state, paths);
        return;
    }
    let h = state.holder.read().unwrap();
    let mut dirty_now = false;
    match &*h {
        IndexHolder::Mutable(idx) => {
            dirty_now = write_holder_index(idx, state, paths).is_ok();
        }
        IndexHolder::Shared(_) | IndexHolder::Empty => {}
        IndexHolder::Overlaid(_) => {} // 上方已分流
    }
    if dirty_now {
        state.dirty.store(0, Ordering::Relaxed);
        *state.last_flush.lock().unwrap() = Instant::now();
    }
}

/// Overlaid 态的合并 flush(T4,tech.md §4.3「落盘与 Base 替换」):
/// ① holder **读锁**内取 Base+Overlay 快照,条目流直写盘(查询不受阻,沿用
///   既有 flush 的同构锁形态);② 放读锁、加载新 mmap;③ holder **写锁**内
///   替换 Shared + 清 Overlay(旧 mmap 随 OverlaidState drop,munmap 只读映射
///   近零成本)。全程 flush_lock 串行域内。
///
/// **快照一致性**(为什么需要 epoch):②③ 之间「放读 → 取写」窗口内插入的
/// diff 会把改动写进即将被丢弃的 Overlay(丢更新)。替换前在写锁内复核
/// `Overlay::epoch`:世代前移则放弃本次替换(dirty 保留、last_flush 照刷以免
/// 密集重试),下轮 flush 重新合并——绝不丢弃窗口内增量。
fn flush_merged(state: &EngineState, paths: &DaemonPaths) {
    let written = {
        let h = state.holder.read().unwrap();
        let IndexHolder::Overlaid(st) = &*h else {
            return;
        };
        let epoch = st.overlay.epoch();
        let meta = build_meta(state);
        let ok = engine::overlay::write_merged_atomic(
            st.base.as_view(),
            &st.overlay,
            &meta,
            &paths.index_file(),
        )
        .map_err(|e| log::error!("合并落盘失败: {e}"))
        .is_ok();
        if ok {
            Some(epoch)
        } else {
            None
        }
    };
    let Some(epoch) = written else { return };
    match persist::load_mmap(&paths.index_file()) {
        Ok(view) => {
            if swap_base_if_fresh(state, view, epoch) {
                state.dirty.store(0, Ordering::Relaxed);
                log::info!("合并 flush 完成,Base 已 remap 为 mmap,Overlay 清零");
            } else {
                log::info!("合并写期间有新增量(世代前移),本轮不替换,下轮重试");
            }
            // 节流起点按「尝试过 flush」推进:失败重试不密集化
            *state.last_flush.lock().unwrap() = Instant::now();
        }
        Err(e) => log::error!("合并产物重载失败(保留增量态,下轮重试): {e}"),
    }
}

/// 写锁内复核快照世代并替换 Shared(替换即清 Overlay)。返回是否替换。
/// 「放读锁 → 取写锁」窗口内 diff 也必须拿写锁,故此刻的世代即终局判定:
/// 世代前移 = 快照过期(盘面不含窗口增量),保留 Overlaid 交下轮 flush 重新
/// 合并,绝不丢弃窗口内增量;形态已变(如重扫替换)同样不替换。
fn swap_base_if_fresh(state: &EngineState, view: Box<SharedView>, expected_epoch: u64) -> bool {
    let mut h = state.holder.write().unwrap();
    let fresh = match &*h {
        IndexHolder::Overlaid(st) => st.overlay.epoch() == expected_epoch,
        _ => false,
    };
    if fresh {
        *h = IndexHolder::Shared(view);
    }
    fresh
}

/// 单一 holder 内容的落盘(Mutable 索引或合成物化产物);失败留痕返回 Err。
fn write_holder_index(
    idx: &Index,
    state: &EngineState,
    paths: &DaemonPaths,
) -> std::io::Result<()> {
    let meta = build_meta(state);
    let layout = persist::EntryLayout::of(idx);
    match persist::write_atomic(&layout, idx.entries.iter(), &meta, &paths.index_file()) {
        Ok(()) => Ok(()),
        Err(e) => {
            log::error!("落盘失败: {e}");
            Err(e)
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
            let layout = persist::EntryLayout::of(idx);
            match persist::write_atomic(&layout, idx.entries.iter(), &meta, &paths.index_file()) {
                Ok(()) => (true, idx.entry_count()),
                Err(e) => {
                    log::error!("最终落盘失败: {e}");
                    (false, idx.entry_count())
                }
            }
        }
        IndexHolder::Overlaid(st) => {
            // T4:条目流直写(可见集),不再全量物化;条目数 = 流统计的活条目数。
            // stop 路径不做 remap(进程即将退出,盘上索引即下次冷启动的 Base)。
            let meta = build_meta(state);
            match engine::overlay::write_merged_atomic(
                st.base.as_view(),
                &st.overlay,
                &meta,
                &paths.index_file(),
            ) {
                Ok(n) => (true, n),
                Err(e) => {
                    log::error!("最终落盘失败: {e}");
                    // 近似计数(含桥 anchor;仅用于 stop 应答回报)
                    (
                        false,
                        live_count_of(&st.overlay.composite_view(st.base.as_view())),
                    )
                }
            }
        }
        // Shared(直挂 Base,本会话零增量):盘上索引即当前状态,不重写;
        // 活条目数直接按**已借用的 guard** 统计——不得经 live_count/with_view
        // 再取同一把 holder 锁:同线程「持读 + 写者排队 + 再读」在本平台实测
        // 不是返回 EDEADLK 而是**永久阻塞**(写者优先,见
        // holder_reentry_with_queued_writer_is_fatally_blocking 钉),后果为
        // stop 线程挂死、socket 残留、后续读者全部卡死。Empty(首扫中):0。
        IndexHolder::Shared(v) => (true, live_count_of(v.as_ref())),
        IndexHolder::Empty => (true, 0),
    }
}

/// 活条目数(分段枚举统计;入参为**已借用**的视图)。Shared 态无内存计数器
/// (mmap 侧墓碑数在 load 时即弃),与 Mutable 的 entry_count 走同一枚举面。
/// 调用方若已持 holder 锁,必须用本函数——status 汇报走 [`status_entry_count`]。
fn live_count_of(view: &dyn IndexView) -> u32 {
    view.entry_segments()
        .iter()
        .map(|(_, s)| s.iter().filter(|e| e.is_live()).count() as u32)
        .sum()
}

/// status 汇报的条目数(T3 近似口径,product.md §4.1):Overlaid 态 = 合成视图
/// 活条目 − 被抑制的 base 背书 anchor(anchor 是桥,不是增量条目);被屏蔽的
/// Base 陈旧条目**不预枚举**,仍计入近似值,落盘(物化去屏蔽)/重启后精确。
/// 一次锁内完成计数——不得经 with_view 再入 holder(见 persist_final 再入钉)。
fn status_entry_count(state: &EngineState) -> u32 {
    let h = state.holder.read().unwrap();
    match &*h {
        IndexHolder::Shared(v) => live_count_of(v.as_ref()),
        IndexHolder::Mutable(i) => i.entry_count(),
        IndexHolder::Overlaid(st) => {
            let view = st.overlay.composite_view(st.base.as_view());
            live_count_of(&view)
                .saturating_sub(st.overlay.suppressed_anchor_count(st.base.as_view()))
        }
        IndexHolder::Empty => 0,
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
/// **T3:Overlay 活跃时跳过整理**——compact 全量重排 Base 条目号,anchors/
/// overlaid/dir_ids 锚定的 base 条目号会全部失效;延迟到 T4 flush 合并
/// (清 Overlay)后再整理。
fn maybe_compact(state: &EngineState) {
    let (before, after) = {
        let mut h = state.holder.write().unwrap();
        match &mut *h {
            IndexHolder::Mutable(idx) => {
                if !idx.needs_compaction() {
                    return;
                }
                let before = idx.entries.len();
                idx.compact();
                (before, idx.entries.len())
            }
            IndexHolder::Overlaid(st) => {
                if !st.overlay.is_empty() {
                    return; // Overlay 活跃:整理会重排 base 条目号,账本失效
                }
                let BaseRef::Heap(idx) = &mut st.base else {
                    return; // Mmap base 只读,无墓碑增长形态
                };
                if !idx.needs_compaction() {
                    return;
                }
                let before = idx.entries.len();
                idx.compact();
                (before, idx.entries.len())
            }
            _ => return,
        }
    };
    log::info!("索引整理:条目 {before} → {after},回收墓碑与 arena 孤儿字节");
    rebuild_dir_map(state);
    engine::malloc_pressure::release_heap_pressure();
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
                            spawn_rescan(&state, &paths); // 后台重扫,消费循环继续 diff 旧索引
                            continue;
                        }
                        // 判定二:抽样对拍——日志滚动会「部分回放」(从保留窗口起点
                        // 开始、有事件但丢前段),零事件判定覆盖不到,抽样 diff 暴露差异。
                        // 仅对「来自落盘索引的停机重放」有意义;首扫后索引是全新全量的,
                        // 抽样只白付开销(且首扫窗口变更本就由本轮重放覆盖)。
                        if replayed_from_disk && sampling_finds_gap(&state, &paths) {
                            continue; // 已转 degraded(后台重扫),完成后自行置 READY
                        }
                        state.set_state(state::READY);
                        flush(&state, &paths);
                    }
                    continue;
                }
                if fsevents::is_dropped(ev.flags) {
                    // 重扫进行中直接忽略:替换段以 rescan_floor 跳过陈旧标记,窗口内
                    // 变更由 rescan_dirty 重放追平,这个 dropped 本身不携带信息。
                    if state.rescanning.load(Ordering::SeqCst) {
                        continue;
                    }
                    // 冷却双保险:rescan_floor 兜住重扫期间积压的旧标记,但重扫**之后**
                    // 新产生的 dropped(重扫自身引起的缓冲溢出)仍需限频,防退化振荡。
                    // 重扫已异步化(后台线程),消费不再停摆,本分支应趋于绝迹。
                    if let Some(t) = *state.last_rescan.lock().unwrap() {
                        if t.elapsed() < RESCAN_COOLDOWN {
                            log::warn!(
                                "重扫冷却期内再收丢弃标记({:.0}s 前刚重扫过),已忽略",
                                t.elapsed().as_secs_f64()
                            );
                            continue;
                        }
                    }
                    log::warn!("收到丢弃类标记,转全量重扫兜底");
                    spawn_rescan(&state, &paths); // 后台重扫,消费循环继续 diff 旧索引
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
        spawn_rescan(&state, &paths);
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
fn sampling_finds_gap(state: &Arc<EngineState>, paths: &DaemonPaths) -> bool {
    const SAMPLE_DIRS: usize = 100;
    const MISMATCH_PCT_LIMIT: usize = 5;
    let bases: Vec<PathBuf> = {
        let map = state.dir_map.lock().unwrap();
        if map.is_empty() {
            return false;
        }
        let keys: Vec<PathBuf> = map.keys().map(Path::to_path_buf).collect();
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
        spawn_rescan(state, paths); // 后台重扫,消费循环继续 diff 旧索引
        return true;
    }
    false
}

/// 降级:全量重扫重建索引,替换 holder 并重落盘,回就绪。
/// 覆盖水位在扫描**之前**取(id > floor 的变更仍会被正常 diff,重扫未必包含)。
///
/// 锁与并发(tech.md §1):`rescan_lock` 覆盖扫描段 + 替换段全程。自动源
/// `wait_for_lock=false`(try_lock 失败 = 重扫已在进行,放弃本次触发——dropped/
/// 流断开类本就该忽略);exclude 源 `wait_for_lock=true`(同步 `lock()`,响应
/// 语义「返回时排除项已生效」不变,最坏等一轮完整重扫)。`rescanning` 置位
/// 期间消费循环照常 diff(与替换段的 holder 写锁互斥)并记 `rescan_dirty`,
/// 替换段末尾重放追平窗口内变更。
fn degrade_and_rescan(state: &EngineState, paths: &DaemonPaths, wait_for_lock: bool) {
    let _guard = if wait_for_lock {
        Some(state.rescan_lock.lock().unwrap())
    } else {
        match state.rescan_lock.try_lock() {
            Ok(g) => Some(g),
            Err(_) => {
                log::info!("重扫已在进行,忽略本次自动触发");
                return;
            }
        }
    };
    state.set_state(state::DEGRADED);
    state.rescanning.store(true, Ordering::SeqCst);
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
    // 重扫替换段(T4 迁移,tech.md §4.3「P0 替换段衔接」收口):产物**直写落盘 →
    // remap mmap Base → 清 Overlay**,不挂堆上 Mutable(与首扫共用
    // mount_scan_product)。旧 Overlay 随整体替换丢弃——重扫产物为全量,窗口内
    // 变更由下方 drain 重放追平。
    mount_scan_product(state, paths, idx);
    // 水位推进到覆盖点:重扫内容 = 截至 floor 的磁盘状态;floor 以下积压的陈旧
    // 事件(含触发本轮重扫的 dropped 标记)从此刻起被消费循环跳过。
    state.seen_event_id.fetch_max(floor, Ordering::Relaxed);
    state.applied_event_id.fetch_max(floor, Ordering::Relaxed);
    state.rescan_floor.fetch_max(floor, Ordering::Relaxed);
    rebuild_dir_map(state);
    // 重扫窗口追平:先清 rescanning 再 drain——此后消费线程 diff 的对象已是新
    // 索引,无需再记集合;窗口内已记目录基于旧索引 diff 过,重放追平。push 与
    // drain 各持自己的 Mutex;push 不持 holder 锁,但恒先于同次 diff 的 apply
    // (apply 持 holder 写锁,与替换段挂 holder 互斥)——「push 在 drain 前 →
    // 基于旧索引 → 被 drain 带走重放」或「push 在 drain 后 → 基于新索引 →
    // 不重放」,无丢失。
    state.rescanning.store(false, Ordering::SeqCst);
    let replayed: Vec<PathBuf> = state.rescan_dirty.lock().unwrap().drain().collect();
    for dir in &replayed {
        diff_one(state, dir);
    }
    // 窗口重放后 Overlay 若仍为空(常态:窗口无变更),刚写盘的 mmap Base 即盘上
    // 真相——直接转 Shared 免一次全量合并写;有窗口变更则走 flush 合并。
    collapse_empty_overlay(state);
    flush(state, paths);
    // 重扫产物(recs 临时结构 + 旧索引)释放后,滞留的 Malloc Large 脏页
    // (实测 254MB)在此一次性归还内核。
    engine::malloc_pressure::release_heap_pressure();
    state.set_state(state::READY);
    *state.last_rescan.lock().unwrap() = Some(Instant::now());
}

/// 扫描产物挂载(T4,tech.md §4.3):全量扫描 Index → 条目流直写落盘 →
/// remap mmap Base(Shared),不留百万级堆索引常驻——**首扫与重扫替换段共用**
/// (P0 迁移的同一逻辑,tech.md §1.6 预告的切换点)。
///
/// 可见性(product 场景 2「建索引期间搜索」):产物先以 Mutable 挂回 holder
/// (查询持续可见,不出现 Empty 观察窗),写盘 + 重载完成后写锁内原位 swap
/// Shared——与 flush_merged 同形态。写盘/重载失败保持 Mutable(保守可用,下轮
/// flush 重试落盘),扫描成果不作废。
///
/// 锁序(tech.md §7 落盘串行):rescan_lock → flush_lock → holder,写+load 段
/// 整体在 flush_lock 内(与 flush_merged/persist_final 的 rename 互斥,防交错
/// rename 后把陈旧快照 mmap 成 Base);无反向获取点,无环。写盘期间持 holder
/// 读锁(内容与形态冻结,查询不受阻);放读 → 取写的窗口由下方**形态复核**
/// 兜住:diff 会把 Mutable 转 Overlaid(变体变更)→ 放弃 swap,产物已落盘,
/// 增量交 flush 合并收敛。窗口内 ENOENT 摘除是唯一同变体内容变更(µs 级),
/// 摘除对象磁盘已不存在,父目录下次 diff 幂等重墓碑,最终一致。
///
/// swap 落地 Shared 即清 dirty:盘面 = 内存真相(flush 的「Shared 态 dirty==0」
/// 前提;重扫路径后续 drain 重放会重新累积)。meta 取调用方当时的水位——崩溃
/// 窗口内盘上水位偏旧,重启重放幂等追平。
fn mount_scan_product(state: &EngineState, paths: &DaemonPaths, idx: Index) {
    *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
    let _flush_guard = state.flush_lock.lock().unwrap();
    let written = {
        let h = state.holder.read().unwrap();
        let IndexHolder::Mutable(idx) = &*h else {
            return; // 已被转 Overlaid(理论不可达:刚挂的 Mutable);flush 兜底
        };
        let meta = build_meta(state);
        let layout = persist::EntryLayout::of(idx);
        persist::write_atomic(&layout, idx.entries.iter(), &meta, &paths.index_file())
            .map_err(|e| log::error!("扫描产物落盘失败,保持内存索引: {e}"))
            .is_ok()
    };
    if !written {
        return;
    }
    match persist::load_mmap(&paths.index_file()) {
        Ok(view) => {
            let mut h = state.holder.write().unwrap();
            if let IndexHolder::Mutable(_) = &*h {
                *h = IndexHolder::Shared(view);
                drop(h);
                state.dirty.store(0, Ordering::Relaxed);
                // 索引已让位 mmap(不计 footprint),扫描与产物堆的脏页在此归还
                // (S2「死页归还」的同构落地点;实测 60 万条首扫滞留 ~60MB 不自行回落)
                engine::malloc_pressure::release_heap_pressure();
                log::info!("扫描产物已落盘并 remap 为 mmap Base");
            } else {
                log::info!("扫描产物已落盘;挂载形态已变,增量交 flush 合并收敛");
            }
            // 形态已变(Overlaid):盘面已是本产物,增量交 flush 合并收敛
        }
        Err(e) => log::error!("扫描产物重载失败,保持内存索引: {e}"),
    }
}

/// Overlaid{mmap Base, 空 Overlay} → Shared:刚写盘的 Base 即盘上真相,直挂免
/// 全量合并写(重扫替换段的窗口无变更常态路径)。写锁内的 Empty 瞬态无观察者
/// (与 ensure_overlay_ready 同一手法);Heap Base 不转(盘上内容与之无对应保证)。
/// 窗口有变更(Overlay 非空)则不动,交 flush 合并。落地 Shared 即清 dirty
/// (盘上即当前真相;否则 Shared 臂 flush 无操作且 dirty 滞留会让 should_flush
/// 恒真,消费循环空转)。
fn collapse_empty_overlay(state: &EngineState) {
    let mut h = state.holder.write().unwrap();
    let taken = std::mem::replace(&mut *h, IndexHolder::Empty);
    let collapsed = matches!(
        &taken,
        IndexHolder::Overlaid(st) if st.overlay.is_empty() && matches!(st.base, BaseRef::Mmap(_))
    );
    *h = match taken {
        IndexHolder::Overlaid(st) if st.overlay.is_empty() => match st.base {
            BaseRef::Mmap(v) => IndexHolder::Shared(v),
            base => IndexHolder::Overlaid(Box::new(OverlaidState {
                base,
                overlay: st.overlay,
            })),
        },
        other => other,
    };
    drop(h);
    if collapsed {
        state.dirty.store(0, Ordering::Relaxed);
    }
}

/// 自动源的重扫入口:后台线程执行(不阻塞消费循环);锁被占用即放弃。
fn spawn_rescan(state: &Arc<EngineState>, paths: &DaemonPaths) {
    let st = state.clone();
    let p = paths.clone();
    std::thread::spawn(move || degrade_and_rescan(&st, &p, false));
}

/// 记录持久失败目录:入负缓存(去重)+ gaps 记账(去重,上限 256 条)。
/// 返回是否新入负缓存(调用方据此决定是否留日志,避免重复刷屏)。
fn record_persistent_failure(state: &EngineState, abs_dir: &Path, reason: &str) -> bool {
    let is_new = state
        .perm_denied
        .lock()
        .unwrap()
        .insert(abs_dir.to_path_buf());
    if is_new {
        let mut gaps = state.gaps.lock().unwrap();
        let path_str = abs_dir.to_string_lossy().into_owned();
        if gaps.len() < 256 && !gaps.iter().any(|g| g.path == path_str) {
            gaps.push(Gap {
                path: path_str,
                reason: reason.into(),
            });
        }
    }
    is_new
}

/// diff 失败分派(tech.md §2):ENOENT → 磁盘已删,摘子树跟进 + Overlay 账本
/// 同步作废(purge_prefix——摘除子树内被锚定的 base 条目号若不清账,查询侧会把
/// 陈旧子树误判为有效);EPERM/EACCES → 保留条目(与首扫「入索引 + 记 gaps +
/// 空子树」语义一致,摘除会被父 diff 重插制造插-摘振荡,且不进 overlay)+ 负
/// 缓存短路;其他 errno 连续 3 次按持久失败处理。
fn handle_diff_error(
    state: &EngineState,
    base: &mut BaseRef,
    overlay: &mut engine::overlay::Overlay,
    map: &mut DirIndexMap,
    abs_dir: &Path,
    e: &std::io::Error,
) {
    match e.kind() {
        std::io::ErrorKind::NotFound => {
            // 摘除要写 base 墓碑:Mmap base 先按需物化(低频路径,条目号 1:1)
            base.ensure_mutable();
            let BaseRef::Heap(idx) = base else {
                unreachable!("ensure_mutable 后必为 Heap")
            };
            let removed = engine::scanner::remove_prefix(idx, map, abs_dir);
            let view: &dyn IndexView = idx;
            overlay.purge_prefix(view, abs_dir);
            state.dirty.fetch_add(removed as u64, Ordering::Relaxed);
        }
        std::io::ErrorKind::PermissionDenied => {
            if record_persistent_failure(
                state,
                abs_dir,
                "权限拒绝(如无完全磁盘访问权限),已停止重试;重启后自动重新索引",
            ) {
                log::warn!("目录 diff 权限拒绝 {},入负缓存", abs_dir.display());
            }
        }
        _ => {
            let mut counts = state.diff_fail_counts.lock().unwrap();
            let n = counts.entry(abs_dir.to_path_buf()).or_insert(0);
            *n += 1;
            if *n >= 3 {
                counts.remove(abs_dir);
                drop(counts);
                if record_persistent_failure(
                    state,
                    abs_dir,
                    "连续 diff 失败,已停止重试;重启后自动重新索引",
                ) {
                    log::warn!("目录 diff 连续失败 {},入负缓存: {e}", abs_dir.display());
                }
            }
        }
    }
}

/// 冷启动编排(独立线程):有索引 → 直挂 Shared 秒开(零物化)→ 重放;无/损坏 → 首扫。
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
            // 直挂 Shared(P3 后常驻即 Base,冷启动不再物化堆索引,tech.md §4.3):
            // 查询立即由 mmap 视图服务。首个可变性需求(增量 diff / 卷写入)经
            // ensure_mutable 按需物化——条目号 1:1,dir_map 与后续操作不受影响。
            *state.holder.write().unwrap() = IndexHolder::Shared(view);
            state.set_state(state::RECONCILING);
            rebuild_dir_map(&state);
            // 启动清除:上次会话的卷前缀中,本次未挂载(或已停用)的移除——
            // 落盘索引不重扫,脏卷条目无自然消亡机制,必须在建流/就绪前清掉。
            // (有待清卷时内部按需转可变;无卷变更则保持 Shared 常驻。)
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
            let mut staging_map = DirIndexMap::default();
            engine::scanner::seed_roots(&mut index, &mut staging_map, &roots);
            *state.holder.write().unwrap() = IndexHolder::Mutable(index);
            let (recs, table, gaps) = engine::scanner::collect_records(&roots, &excludes, threads);
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
                    engine::scanner::apply_records(idx, &mut staging_map, chunk, &table);
                }
            }
            state.applied_event_id.store(water, Ordering::Relaxed);
            state.seen_event_id.store(water, Ordering::Relaxed);
            // T4:首扫产物同样收敛为 mmap Base(与重扫替换段共用 mount_scan_product,
            // tech.md §4.3)——不留堆索引常驻;此刻水位已含 water,落盘 meta 一致。
            // mount_scan_product 自己完成「挂回 Mutable → 落盘 → swap Shared」,
            // 批间可见性与旧实现一致(product 场景 2)。
            let idx = {
                let mut h = state.holder.write().unwrap();
                match std::mem::replace(&mut *h, IndexHolder::Empty) {
                    IndexHolder::Mutable(idx) => idx,
                    IndexHolder::Empty => unreachable!("同线程不会放回 Empty"),
                    other => {
                        *h = other; // 形态异常(理论不可达):原样归还,guard 内不重入取锁
                        panic!("首扫流式聚合后 holder 应为 Mutable")
                    }
                }
            };
            mount_scan_product(&state, &paths, idx);
            rebuild_dir_map(&state);
            flush(&state, &paths); // Shared 态:盘上即刚写产物,无操作
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
            spawn_rescan(&state, &paths);
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
            let entry_count = status_entry_count(state);
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
    degrade_and_rescan(state, paths, true); // 同步:响应语义「返回时排除项已生效」
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
    degrade_and_rescan(state, paths, true); // 同步:响应语义「返回时排除项已生效」
    Ok(())
}

/// 数据目录 0700(索引/日志/配置均含全盘路径,属敏感数据);已存在目录也
/// 强制 0700——目录可能由旧版本以默认 0644 建过,每次启动收敛到位。
pub fn ensure_data_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true).mode(0o700);
    b.create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// 服务循环(主线程):accept → 每连接一线程;stop 处理后退出进程。
pub fn serve(paths: DaemonPaths) -> Result<()> {
    ensure_data_dir(&paths.data_dir).context("创建数据目录失败")?;
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
        open_log_file(&paths.log_file()),
    )))
    .is_ok()
    {
        log::set_max_level(log::LevelFilter::Info);
    }
}

/// 打开日志文件:新建按 0600;已存在文件先补 chmod 0600(旧版曾以默认 0644
/// 落盘,open 的 mode 只作用于新建,存量文件须显式收敛)。
/// 打开失败返回 None(stderr 仍在,日志降级不致命)。
fn open_log_file(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .ok()
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
            let key = dir.canonicalize().unwrap();
            map.insert(&key, root);
        }
        *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
        state
    }

    /// T2 桥接钉(tech.md §4.3 冷启动改造):冷启动直挂 Shared 后查询走 mmap
    /// 视图;首个增量 diff 经 ensure_mutable 按需物化——Shared → Mutable 条目号
    /// 1:1,已建 dir_map 继续有效(条目号稳定性是 T3 屏蔽判定的前提)。
    /// 突变「ensure_mutable 不物化」→ diff 静默丢弃(holder 仍 Shared、条目数
    /// 不变)本测必红。
    #[test]
    fn shared_holder_serves_queries_and_overlays_first_diff() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        // 建索引并落盘 → load_mmap → Shared 挂载(模拟冷启动直挂 Base)
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let a = idx.insert("a.txt", root, false);
        let b = idx.insert("b.txt", root, false);
        let idx_file = data.path().join("index.bin");
        let layout = persist::EntryLayout::of(&idx);
        persist::write_atomic(
            &layout,
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write().unwrap() =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        // Shared 态建 dir_map(rebuild 改读 IndexView,不再要求 Mutable;
        // 突变「rebuild 仍只认 Mutable 提前返回」→ map 被清空,断言必红)
        *state.roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);
        assert!(
            state
                .dir_map
                .lock()
                .unwrap()
                .contains_key(&dir.path().canonicalize().unwrap()),
            "Shared 态 rebuild_dir_map 应建立根映射"
        );
        // Shared 态查询直接可服务(冷启动秒开语义)
        match run_query(&state, "a.txt", 10, None, false) {
            Response::Query { total, .. } => assert_eq!(total, 1, "Shared 态应可查"),
            other => panic!("意外响应: {other:?}"),
        }
        // 磁盘新增 c.txt:首个 diff 转 Overlaid(直写 Overlay,不物化 Base),增量照常生效
        std::fs::write(dir.path().join("c.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1, "首个 diff 应生效");
        {
            let h = state.holder.read().unwrap();
            assert!(
                matches!(&*h, IndexHolder::Overlaid(_)),
                "diff 后应为 Overlaid 态(Base + Overlay),不再物化全量索引"
            );
        }
        // base 条目号 1:1 保持:a/b 仍在原号且为活条目(合成视图按空间分发,同号可达)
        with_view(&state, |v| {
            assert_eq!(v.display_name(a), b"a.txt".as_slice());
            assert_eq!(v.display_name(b), b"b.txt".as_slice());
            assert!(v.entry(a).is_live() && v.entry(b).is_live());
        });
        // overlay 增量可见、旧路径仍在(仅新增,无删除)
        match run_query(&state, "c.txt", 10, None, false) {
            Response::Query { total, .. } => assert_eq!(total, 1, "overlay 新增可见"),
            other => panic!("意外响应: {other:?}"),
        }
    }

    /// persist_final 在 Shared 态(stop 时的常态)的输出契约钉:不重写盘上索引
    /// (data_dir 不存在时仍 ok=true,写动作会因父目录缺失而失败),活条目数按
    /// 视图如实统计(墓碑不计)。注:本测**杀不死**「Shared 分支改回经
    /// with_view 再入取数」的突变——再入窗口在函数内部,外部无法向该窗口注入
    /// 写者(预先入队的写者同样卡死修复版的首取锁);再入的致命性由
    /// `holder_reentry_with_queued_writer_is_fatally_blocking` 平台钉收口,
    /// 再入的排除靠「已借用 guard 计数」的结构与评审。
    #[test]
    fn persist_final_shared_state_reports_live_count_without_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        std::fs::write(dir.path().join("b.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("a.txt", root, false);
        let b = idx.insert("b.txt", root, false);
        idx.remove(b); // 一条墓碑:活条目统计必须剔除
        let idx_file = data.path().join("index.bin");
        let layout = persist::EntryLayout::of(&idx);
        persist::write_atomic(
            &layout,
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write().unwrap() =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        // data_dir 指向不存在目录:Shared 分支不得尝试写(写即失败 ok=false)
        let paths = DaemonPaths {
            data_dir: data.path().join("absent"),
        };
        let (ok, n) = persist_final(&state, &paths);
        assert!(ok, "Shared 态不重写,盘上索引即当前状态");
        assert_eq!(n, 2, "根 + a.txt;墓碑 b 不计");
    }

    /// stop 与在途写者并发:persist_final 等写者完成后如实统计(正常阻塞语义),
    /// 不挂死。注:此为并发冒烟钉——sleep(5ms) 后写者通常已释放,本测**杀不死**
    /// 再入突变;再入致命性由 `holder_reentry_with_queued_writer_is_fatally_
    /// blocking` 平台钉收口(机制),再入的排除靠「已借用 guard 计数」的结构。
    #[test]
    fn persist_final_completes_under_pending_writer() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("a.txt", root, false);
        let idx_file = data.path().join("index.bin");
        let layout = persist::EntryLayout::of(&idx);
        persist::write_atomic(
            &layout,
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write().unwrap() =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        let st = state.clone();
        let writer = std::thread::spawn(move || {
            let _g = st.holder.write().unwrap(); // 短暂持写后释放
        });
        std::thread::sleep(std::time::Duration::from_millis(5));
        let paths = DaemonPaths {
            data_dir: data.path().join("absent"),
        };
        let (ok, n) = persist_final(&state, &paths);
        writer.join().unwrap();
        assert!(ok);
        assert_eq!(n, 2, "根 + a.txt");
    }

    /// 平台机制钉(本机 macOS 实测):同线程「持读锁 → 写者入队 → 再取读锁」
    /// **不是 EDEADLK 返回,而是永久阻塞**——后到的读按写者优先排队等写者,
    /// 写者又等本线程放锁,结构性互等,无调度可解。比 panic 更重:触发线程
    /// 挂死,后续所有读者排队卡死。persist_final 的计数因此必须走已借用 guard
    /// (live_count_of),不得再入 holder 锁。
    /// 形态:独立线程复现 A 持读 → B 请求写(sleep 200ms 确保入队)→ A 二次读,
    /// 主测 300ms 未返回即断言(阻塞是结构性的,判定无时间竞争;若平台放行
    /// 同线程递归读,探测须先放全部读锁再 join 写者,否则 join 自身永挂、
    /// 钉失去区分力——本测会变红提示重评 persist_final 再入风险)。
    /// 注:阻塞平台上探测线程组以挂死状态存活至测试进程退出,无副作用。
    #[test]
    fn holder_reentry_with_queued_writer_is_fatally_blocking() {
        let lock = Arc::new(std::sync::RwLock::new(()));
        let probe = {
            let lock = Arc::clone(&lock);
            std::thread::spawn(move || {
                let g1 = lock.read().unwrap(); // A 持读
                let w = {
                    let lock = Arc::clone(&lock);
                    std::thread::spawn(move || {
                        let _w = lock.write().unwrap(); // B 请求写 → 入队阻塞
                    })
                };
                std::thread::sleep(std::time::Duration::from_millis(200)); // 确保 B 入队
                let g2 = lock.read().unwrap(); // A 二次读 → 阻塞(平台若放行则返回)
                drop(g1);
                drop(g2); // 先放全部读锁再等写者,否则 join 永挂
                w.join().unwrap();
            })
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(probe.join());
        });
        let done = rx.recv_timeout(std::time::Duration::from_millis(300));
        assert!(
            matches!(done, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
            "同线程持读+写者入队+再取读应为致命阻塞;若探测已返回,平台行为已变,须重评 persist_final 的再入风险"
        );
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
        // T3:diff 写 Overlay,holder 转 Overlaid;status 近似计数扣掉桥 anchor
        // 后 = 根 + a.txt(被屏蔽 Base 条目不预枚举,一般情形为近似,此处恰精确)
        let h = state.holder.read().unwrap();
        assert!(
            matches!(&*h, IndexHolder::Overlaid(_)),
            "diff 后应为 Overlaid 态"
        );
        drop(h);
        assert_eq!(status_entry_count(&state), 2, "根 + a.txt");
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
        degrade_and_rescan(&state, &paths, true);
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
                .entries
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
            v.entry_segments().iter().any(|&(start, seg)| {
                seg.iter()
                    .enumerate()
                    .any(|(j, e)| e.is_live() && v.full_path_dyn(start + j as u32) == hidden_path)
            })
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

    /// 数据目录 0700、日志文件 0600:索引与日志都含全盘路径,属敏感数据。
    /// 突变「沿用 create_dir_all/OpenOptions 默认 0644」时两个断言必红。
    #[test]
    fn data_dir_is_0700_and_log_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        ensure_data_dir(&data).unwrap();
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o700
        );
        init_logger(&DaemonPaths {
            data_dir: data.clone(),
        });
        let log = data.join("daemon.log");
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    /// 收敛钉:已存在目录 0755、已存在日志 0644,经 ensure_data_dir + open_log_file
    /// 归位 0700/0600,且日志旧内容保留(append 打开,不截断)。
    /// 突变「删 ensure_data_dir 的 set_permissions」「删 open_log_file 的 open 前
    /// 补 chmod」——DirBuilder/open 的 mode 只作用于新建,此测必红。
    #[test]
    fn existing_dir_and_log_converge_to_0700_0600_with_content_kept() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = data.join("daemon.log");
        std::fs::write(&log, b"old-line\n").unwrap();
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o644)).unwrap();

        ensure_data_dir(&data).unwrap();
        assert_eq!(
            std::fs::metadata(&data).unwrap().permissions().mode() & 0o777,
            0o700,
            "已存在 0755 目录应收敛 0700"
        );

        open_log_file(&log)
            .expect("日志文件应可打开")
            .write_all(b"new-line\n")
            .unwrap();
        assert_eq!(
            std::fs::metadata(&log).unwrap().permissions().mode() & 0o777,
            0o600,
            "已存在 0644 日志应收敛 0600"
        );
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "old-line\nnew-line\n",
            "append 打开不得截断旧内容"
        );
    }

    // ---------- S1 失败治理与重扫窗口(tech.md §1/§2) ----------

    /// 构造:Mutable 索引含 root 与 subdirs(父先于子),dir_map 键为 canonical 形态。
    fn seeded_state(root: &std::path::Path, subdirs: &[&str]) -> Arc<EngineState> {
        let state = Arc::new(EngineState::new());
        let root = root.canonicalize().unwrap(); // 与 diff_one 基址同形态(/var→/private/var)
        let mut idx = Index::new();
        let mut map = DirIndexMap::default();
        let root_idx = idx.insert(&root.to_string_lossy(), NO_PARENT, true);
        map.insert(&root, root_idx);
        for sub in subdirs {
            let p = root.join(sub);
            let parent = p.parent().unwrap().to_path_buf();
            let pidx = map.get(&parent).expect("夹具父键已插入");
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let k = idx.insert(&name, pidx, true);
            map.insert(&p, k);
        }
        *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
        *state.dir_map.lock().unwrap() = map;
        state
    }

    /// EPERM 目录语义钉(tech.md §2):保留条目(摘除会被父 diff 重插 → 插-摘振荡)
    /// + 入负缓存 + gaps 记账;负缓存短路后不再重试(io_errors 不再涨、日志不再刷)。
    ///
    /// 突变验证:「EPERM 分支改为摘除」→ is_live 断言红;「无负缓存」→ 短路断言红。
    #[test]
    fn eperm_dir_keeps_entry_and_short_circuits() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("locked")).unwrap();
        let state = seeded_state(&root, &["locked"]);
        let locked = root.join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        assert_eq!(diff_one(&state, &locked), 0, "diff 失败返回 0 变更");
        {
            // T3:diff 路径转 Overlaid,base 经可变槽位读取(mounted_state 为 Heap 载体)
            let mut h = state.holder.write().unwrap();
            let idx = mutable_base_slot(&mut h).expect("holder 应持可变 base");
            let map = state.dir_map.lock().unwrap();
            let locked_idx = map.get(&locked).expect("locked 键应在");
            assert!(
                idx.entries[locked_idx as usize].is_live(),
                "EPERM 目录保留条目(与首扫「入索引+空子树+gaps」语义一致)"
            );
        }
        assert!(state.perm_denied.lock().unwrap().contains(&locked));
        assert_eq!(state.gaps.lock().unwrap().len(), 1, "gaps 记账一次");

        let mid = state.io_errors.load(Ordering::Relaxed);
        assert_eq!(diff_one(&state, &locked), 0);
        assert_eq!(
            state.io_errors.load(Ordering::Relaxed),
            mid,
            "负缓存短路:不再重试、不再计数"
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// ENOENT 目录语义钉:磁盘已删 → 摘子树 + 清映射(父 diff 不会重插,无振荡)。
    /// 突变「NotFound 分支改为保留」→ live 计数断言红。
    #[test]
    fn enoent_dir_diff_removes_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("gone").join("child")).unwrap();
        let state = seeded_state(&root, &["gone", "gone/child"]);
        let gone = root.join("gone");
        std::fs::remove_dir_all(&gone).unwrap();

        assert_eq!(diff_one(&state, &gone), 0);
        {
            let mut h = state.holder.write().unwrap();
            let idx = mutable_base_slot(&mut h).expect("holder 应持可变 base");
            let live = idx.entries.iter().filter(|e| e.is_live()).count();
            assert_eq!(live, 1, "仅剩根条目:gone 子树级联摘除");
        }
        assert!(
            !state.dir_map.lock().unwrap().contains_key(&gone),
            "dir_map 键同步清除"
        );
    }

    /// 未知 errno 钉:连续 3 次失败才入负缓存(阈值防偶发错误被永久短路)。
    /// 突变阈值(3 → 1)→ 前两次断言红。
    #[test]
    fn unknown_errno_three_strikes_enters_negative_cache() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        let state = seeded_state(tmp.path(), &["d"]);
        let e = std::io::Error::from_raw_os_error(5); // EIO,非 NotFound/EPERM
        {
            let mut h = state.holder.write().unwrap();
            ensure_overlay_ready(&mut h); // handle_diff_error 消费 BaseRef + Overlay
            let IndexHolder::Overlaid(st) = &mut *h else {
                panic!("holder 应为 Overlaid")
            };
            let OverlaidState { base, overlay } = st.as_mut();
            let mut map = state.dir_map.lock().unwrap();
            handle_diff_error(&state, base, overlay, &mut map, &dir, &e);
            handle_diff_error(&state, base, overlay, &mut map, &dir, &e);
            assert!(
                !state.perm_denied.lock().unwrap().contains(&dir),
                "前两次不入负缓存"
            );
            handle_diff_error(&state, base, overlay, &mut map, &dir, &e);
        }
        assert!(
            state.perm_denied.lock().unwrap().contains(&dir),
            "第三次入负缓存"
        );
        assert_eq!(state.gaps.lock().unwrap().len(), 1);
    }

    /// 负缓存键稳定性钉:键为 canonical 路径——条目号在 compact/重扫替换后失效,
    /// 错杀无辜条目会静默丢更新。突变「键改条目号」或「替换清空负缓存」
    /// → 对应断言红。(T3:Overlay 活跃期 compact 被禁止,故以重扫式替换再现
    /// 「条目号全变」场景——对负缓存而言二者等价。)
    #[test]
    fn negative_cache_survives_compact_and_spares_other_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("locked")).unwrap();
        std::fs::create_dir(root.join("live")).unwrap();
        let state = seeded_state(&root, &["locked", "live"]);
        let locked = root.join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        diff_one(&state, &locked);
        assert!(state.perm_denied.lock().unwrap().contains(&locked));

        {
            // T3:diff_one 后 holder 为 Overlaid(Overlay 活跃,直接 compact 会重排
            // 被锚定的 base 条目号,禁止)。等价钉法:重扫式替换 holder——条目号
            // 全部改变,负缓存若按条目号记键必然失效错杀,按路径记键则继续短路。
            let mut idx = Index::new();
            let mut map = DirIndexMap::default();
            let r = idx.insert(&root.to_string_lossy(), NO_PARENT, true);
            map.insert(&root, r);
            for sub in ["locked", "live"] {
                let p = root.join(sub);
                let k = idx.insert(sub, r, true);
                map.insert(&p, k);
            }
            *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
            *state.dir_map.lock().unwrap() = map;
        }
        let mid = state.io_errors.load(Ordering::Relaxed);
        diff_one(&state, &locked);
        assert_eq!(
            state.io_errors.load(Ordering::Relaxed),
            mid,
            "条目号全变后负缓存仍短路(键为路径;若失效则此 diff 失败计数上涨)"
        );

        std::fs::write(root.join("live").join("new.txt"), "x").unwrap();
        assert!(
            diff_one(&state, &root.join("live")) > 0,
            "其他目录内容漂移正常更新,不被负缓存错杀"
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// 重扫窗口追平语义钉:窗口内 diff 照常应用并记集合;替换段挂上「扫描时点
    /// 快照」(不含窗口变更)后,drain 重放把窗口变更补进新索引。
    /// 突变「不记 rescan_dirty」或「替换段不 drain」→ 末段 entry_count 断言红。
    #[test]
    fn rescan_replay_restores_window_changes_dropped_by_swap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir(root.join("d")).unwrap();
        let state = seeded_state(&root, &["d"]);
        let d = root.join("d");

        state.rescanning.store(true, Ordering::SeqCst);
        std::fs::write(d.join("x.txt"), "").unwrap();
        assert!(diff_one(&state, &d) > 0, "窗口内变更照常 diff(旧索引)");
        assert!(
            state.rescan_dirty.lock().unwrap().contains(&d),
            "窗口内 diff 记集合"
        );

        // 模拟重扫产物替换:全新索引 = 扫描开始时点快照(不含窗口内 x.txt)
        let mut idx = Index::new();
        let mut map = DirIndexMap::default();
        let r = idx.insert(&root.to_string_lossy(), NO_PARENT, true);
        map.insert(&root, r);
        let d_idx = idx.insert("d", r, true);
        map.insert(&d, d_idx);
        *state.holder.write().unwrap() = IndexHolder::Mutable(idx);
        *state.dir_map.lock().unwrap() = map;
        assert_eq!(status_entry_count(&state), 2, "替换后快照不含窗口变更");

        // 替换段追平:清标志(rescanning)→ drain → 逐目录重放
        state.rescanning.store(false, Ordering::SeqCst);
        let replayed: Vec<PathBuf> = state.rescan_dirty.lock().unwrap().drain().collect();
        assert_eq!(replayed.len(), 1);
        for dir in &replayed {
            diff_one(&state, dir);
        }
        // T3:重放 diff 转 Overlaid 写 Overlay;status 近似计数 = 根 + d + x.txt
        assert_eq!(
            status_entry_count(&state),
            3,
            "重放把窗口变更(x.txt)补进合成视图"
        );
    }

    // ---------- T4:flush 合并写 + P0 替换段迁移 ----------

    /// 节流常量钉(T4 放宽,memory-footprint tech.md §4.3):落盘不再承担内存
    /// 压力缓解(mmap Base 兜底 A1),放宽到 120s/30k 减少全量重写与 remap 频率。
    /// 突变「改回 30s/10k」或「注释与值脱节」此测必红。
    #[test]
    fn flush_throttle_constants_match_spec() {
        assert_eq!(FLUSH_INTERVAL, Duration::from_secs(120), "间隔 30s→120s");
        assert_eq!(FLUSH_DIRTY_LIMIT, 30_000, "条数 10k→30k");
    }

    /// P0 替换段迁移钉(T4):重扫替换段必须「直写落盘 → remap mmap Base → 清
    /// Overlay」,完成后 holder 为 Shared(mmap 直挂),绝不回退堆 Mutable
    /// (否则重新引入百万级堆驻留)。突变「替换段回退 *holder = Mutable(idx)」
    /// 或「collapse 漏调 + 窗口无变更」本断言红。
    #[test]
    fn rescan_replacement_mounts_mmap_base_not_heap_mutable() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "").unwrap();
        let state = Arc::new(EngineState::new());
        *state.roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        let paths = DaemonPaths {
            data_dir: dir.path().join("data"),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        // 预置 dirty(模拟窗口计数):mount 落地 Shared 必须清零,否则 Shared 态
        // flush 无操作 + should_flush 恒真 → 消费循环空转(评审必修 2 的 mount 侧)
        state.dirty.store(9, Ordering::Relaxed);
        degrade_and_rescan(&state, &paths, true);
        {
            let h = state.holder.read().unwrap();
            assert!(
                matches!(&*h, IndexHolder::Shared(_)),
                "重扫后应直挂 mmap Base(Shared),不得挂堆 Mutable"
            );
        }
        assert_eq!(
            state.dirty.load(Ordering::Relaxed),
            0,
            "mount 落地 Shared 清 dirty(窗口无重放变更的常态路径)"
        );
        // 重扫产物已落盘且可查(经 Shared 视图)
        assert!(paths.index_file().exists(), "重扫产物应已落盘");
        assert!(
            status_entry_count(&state) > 0,
            "Shared 态查询计数正常(dir_map/rebuild 链路完好)"
        );
    }

    /// 「新索引 + 旧 map」竞态窗口钉(遗留 #12):替换段 swap 与 rebuild_dir_map
    /// 之间,map 残留的条目号对新索引可能越界——必须按死键清映射短路,绝不
    /// 按死号下标(panic 即索引不可用)。突变「apply_and_track 回退 entry(d0)
    /// 直下标判活」本测 panic 红。
    #[test]
    fn stale_dir_map_key_outside_new_index_is_dropped_safely() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("stale");
        std::fs::create_dir(&stale).unwrap();
        let state = Arc::new(EngineState::new());
        // 小索引(仅 1 条 root),map 却残留指向 999 号(越界)的陈旧键
        let mut idx = Index::new();
        idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let idx_file = dir.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write().unwrap() =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        let stale_key = stale.canonicalize().unwrap();
        state.dir_map.lock().unwrap().insert(&stale_key, 999);
        // diff 走到越界键:清映射返回 0,不 panic
        assert_eq!(diff_one(&state, &stale), 0, "越界键按死键处理");
        assert!(
            !state.dir_map.lock().unwrap().contains_key(&stale_key),
            "陈旧键应被清出"
        );
    }

    /// flush 合并写主链路钉(T4,tech.md §4.3「落盘与 Base 替换」):Overlaid 态
    /// flush → 条目流直写落盘 → holder 替换 Shared + Overlay 清零 → 查询语义
    /// 不变(增量不丢)。突变「flush 后不替换/不清 Overlay」此测红;「替换丢
    /// 增量」由 whole:new.txt 断言红。
    #[test]
    fn merged_flush_remaps_base_and_clears_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("a.txt", root, false);
        let idx_file = data.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write().unwrap() =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        *state.roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);

        // 磁盘漂移 + diff → Overlaid(增量在 Overlay,未落盘)
        std::fs::write(dir.path().join("new.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1);
        assert!(matches!(
            &*state.holder.read().unwrap(),
            IndexHolder::Overlaid(_)
        ));

        // flush:合并直写 + remap + 清 Overlay
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };
        flush(&state, &paths);
        assert!(
            matches!(&*state.holder.read().unwrap(), IndexHolder::Shared(_)),
            "flush 后应为 Shared(remap 完成、Overlay 清零)"
        );
        // 内存查询:增量保留(合并写不得丢更新)
        match run_query(&state, "whole:new.txt", 10, None, false) {
            Response::Query { total, .. } => assert_eq!(total, 1, "flush 后增量保留"),
            other => panic!("意外响应: {other:?}"),
        }
        // 盘上证词:重新 mmap 载入(模拟下次冷启动)后增量已在索引里
        let reloaded = persist::load_mmap(&idx_file).unwrap();
        let out = engine::query::execute(
            reloaded.as_ref(),
            &engine::query::parse("whole:new.txt").unwrap(),
            10,
        );
        assert_eq!(out.total, 1, "增量已合并落盘");
    }

    /// epoch 复核行为钉(评审必修 4):flush 的「放读锁 → 取写锁」窗口内插入的
    /// diff 使世代前移 → 陈旧快照的替换必须被拒绝(holder 保留 Overlaid、窗口
    /// 增量查询可见);下轮 flush 重新合并落盘后替换成功且含该增量。
    /// 突变「swap_base_if_fresh 去掉世代复核(无条件替换)」:第一步即替换成功,
    /// 窗口增量随陈旧盘面丢失(查询 0),本测红。
    #[test]
    fn epoch_regression_defers_swap_and_next_flush_keeps_delta() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("a.txt", root, false);
        let idx_file = data.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write().unwrap() =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        *state.roots.lock().unwrap() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };

        // diff(new.txt)→ Overlaid;以当前世代为快照,合并落盘一份「不含窗口
        // 增量」的盘面(flush 读锁快照段的等价物)
        std::fs::write(dir.path().join("new.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1);
        let epoch_snapshot = match &*state.holder.read().unwrap() {
            IndexHolder::Overlaid(st) => st.overlay.epoch(),
            _ => panic!("diff 后应为 Overlaid"),
        };
        {
            let h = state.holder.read().unwrap();
            let IndexHolder::Overlaid(st) = &*h else {
                panic!("应为 Overlaid")
            };
            engine::overlay::write_merged_atomic(
                st.base.as_view(),
                &st.overlay,
                &build_meta(&state),
                &idx_file,
            )
            .unwrap();
        }
        let stale_view = persist::load_mmap(&idx_file).unwrap();

        // 「窗口内」插入 diff(c.txt)→ 世代前移
        std::fs::write(dir.path().join("c.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1);

        // 陈旧快照的替换必须被拒绝:保留增量态,查询可见
        assert!(
            !swap_base_if_fresh(&state, stale_view, epoch_snapshot),
            "世代前移必须放弃替换"
        );
        assert!(
            matches!(&*state.holder.read().unwrap(), IndexHolder::Overlaid(_)),
            "拒绝替换后保留增量态"
        );
        match run_query(&state, "whole:c.txt", 10, None, false) {
            Response::Query { total, .. } => {
                assert_eq!(total, 1, "窗口增量不得随陈旧快照替换丢失")
            }
            other => panic!("意外响应: {other:?}"),
        }

        // 下轮 flush:重新合并落盘(含 c.txt)→ 以当前世代替换成功 → Shared
        flush(&state, &paths);
        assert!(
            matches!(&*state.holder.read().unwrap(), IndexHolder::Shared(_)),
            "下轮 flush 应替换成功(世代一致)"
        );
        match run_query(&state, "whole:c.txt", 10, None, false) {
            Response::Query { total, .. } => assert_eq!(total, 1, "下轮合并后增量保留"),
            other => panic!("意外响应: {other:?}"),
        }
        let reloaded = persist::load_mmap(&idx_file).unwrap();
        let out = engine::query::execute(
            reloaded.as_ref(),
            &engine::query::parse("whole:c.txt").unwrap(),
            10,
        );
        assert_eq!(out.total, 1, "窗口增量已合并落盘");
    }

    /// collapse 的 dirty 清零钉(评审必修 2):窗口有变更而重放全零(常态)时,
    /// 落地 Shared 必须清 dirty——否则 Shared 臂 flush 无操作且 dirty 滞留,
    /// should_flush 恒真、消费循环空转。突变「collapse 不清 dirty」此测红。
    #[test]
    fn collapse_to_shared_clears_dirty() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let idx_file = dir.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        // Overlaid{Mmap, 空 Overlay} + 预置 dirty(模拟窗口计数滞留)
        *state.holder.write().unwrap() = IndexHolder::Overlaid(Box::new(OverlaidState {
            base: BaseRef::Mmap(persist::load_mmap(&idx_file).unwrap()),
            overlay: engine::overlay::Overlay::new(),
        }));
        state.dirty.store(7, Ordering::Relaxed);
        collapse_empty_overlay(&state);
        assert!(
            matches!(&*state.holder.read().unwrap(), IndexHolder::Shared(_)),
            "空 Overlay 的 mmap Base 应收敛 Shared"
        );
        assert_eq!(
            state.dirty.load(Ordering::Relaxed),
            0,
            "落地 Shared 清 dirty"
        );
    }
}

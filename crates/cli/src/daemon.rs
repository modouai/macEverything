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
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// —— 锁中毒策略 ——
//
// 后台线程的任何 panic 都经 `spawn_guarded` 记录后 `exit(1)`(fail-fast,GUI
// 存活监控负责拉起)。但线程 panic 到进程退出之间,以及未受看护的测试/主线程
// 路径,仍可能留下中毒锁;此处按「锁保护什么」二分:
//
// * 快照/配置/弱一致数据(state_name、gaps、roots、cfg、缓存、统计、流登记等)
//   → `lock_recover`:取 inner 继续,一次 panic 不得级联成全部查询失败。
// * 索引结构/寻址面/落盘串行域(holder、dir_map、flush_lock)→ `*_fail_fast`:
//   中毒意味着前持有者可能在半改状态中 panic,继续读写会静默污染索引或盘面;
//   记录后立即退出,由 GUI 拉起干净进程。
//
// rescan_lock / exclude_op_lock 只做串行化且已有显式清毒逻辑(见
// acquire_rescan_blocking / try_acquire_rescan),保持 recover。

/// 快照/配置类互斥锁的恢复入口:毒锁取 inner 继续(不 panic)。
pub(crate) fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 快照/配置类读写锁的恢复入口(生产只有 holder 用 RwLock,且属完整性锁;
/// 此处保留通用入口供测试/后续弱一致 RwLock 使用)。
#[cfg(test)]
fn read_recover<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
fn write_recover<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|e| e.into_inner())
}

/// 完整性锁中毒的 fail-fast 出口。日志自身也包一层 catch_unwind:诊断失败
/// 不得阻断退出路径。
fn fatal_lock_poison(what: &str) -> ! {
    let _ = std::panic::catch_unwind(|| {
        log::error!(
            "锁 `{what}` 中毒(前持有线程 panic),索引/落盘完整性不可保证;按 fail-fast 退出进程,等待 GUI 拉起"
        );
    });
    std::process::exit(1);
}

/// 同一键在窗口内只出一条,并在恢复时注明期间抑制了多少条。
/// 用途:记录目录级错误与连接错误——单个坏目录不配刷满整个日志窗口。
/// 定义置于文件前部:macro_rules 按文本顺序生效,consume_events 等早于
/// 日志实现段落使用它。
macro_rules! log_throttled {
    ($lvl:ident, $key:literal, $window:expr, $($arg:tt)*) => {{
        if let Some(suppressed) = $crate::daemon::throttle_allow($key, $window) {
            if suppressed == 0 {
                log::$lvl!($($arg)*);
            } else {
                log::$lvl!("{} [窗口内抑制 {suppressed} 条同类]", format_args!($($arg)*));
            }
        }
    }};
}

pub(crate) trait LockRecover<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        lock_recover(self)
    }
}

#[cfg(test)]
pub(crate) trait RwLockRecover<T> {
    fn read_recover(&self) -> RwLockReadGuard<'_, T>;
    fn write_recover(&self) -> RwLockWriteGuard<'_, T>;
}

#[cfg(test)]
impl<T> RwLockRecover<T> for RwLock<T> {
    fn read_recover(&self) -> RwLockReadGuard<'_, T> {
        read_recover(self)
    }

    fn write_recover(&self) -> RwLockWriteGuard<'_, T> {
        write_recover(self)
    }
}

pub(crate) trait LockFailFast<T> {
    fn lock_fail_fast(&self, what: &'static str) -> MutexGuard<'_, T>;
}

impl<T> LockFailFast<T> for Mutex<T> {
    fn lock_fail_fast(&self, what: &'static str) -> MutexGuard<'_, T> {
        match self.lock() {
            Ok(g) => g,
            Err(_) => fatal_lock_poison(what),
        }
    }
}

pub(crate) trait RwLockFailFast<T> {
    fn read_fail_fast(&self, what: &'static str) -> RwLockReadGuard<'_, T>;
    fn write_fail_fast(&self, what: &'static str) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockFailFast<T> for RwLock<T> {
    fn read_fail_fast(&self, what: &'static str) -> RwLockReadGuard<'_, T> {
        match self.read() {
            Ok(g) => g,
            Err(_) => fatal_lock_poison(what),
        }
    }

    fn write_fail_fast(&self, what: &'static str) -> RwLockWriteGuard<'_, T> {
        match self.write() {
            Ok(g) => g,
            Err(_) => fatal_lock_poison(what),
        }
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "非字符串 panic payload".to_owned()
    }
}

/// 后台线程统一看护:捕获 panic → 记录 → `exit(1)`(fail-fast)。
/// 替代裸 `thread::spawn`:任何后台线程静默死亡都会让 daemon「socket 照常
/// 应答、索引永不更新」,必须由进程退出 + GUI 存活监控收敛。
pub(crate) fn spawn_guarded<F>(name: &'static str, f: F)
where
    F: FnOnce() + Send + 'static,
{
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
                let msg = panic_payload_message(payload.as_ref());
                log::error!("后台线程 `{name}` panic: {msg};按 fail-fast 退出进程,等待 GUI 拉起");
                std::process::exit(1);
            }
        });
    if let Err(e) = spawned {
        log::error!("后台线程 `{name}` 启动失败: {e};按 fail-fast 退出进程");
        std::process::exit(1);
    }
}

/// 停机时长超过此阈值且重放零事件 → 判重放不可信,转全量重扫(ADR-5 保守判定器)。
pub const REPLAY_DOWNTIME_THRESHOLD_SECS: u64 = 48 * 3600;
/// 落盘节流:距上次落盘超过此间隔且有变更 → 触发快照。
/// T4 放宽(tech.md §4.3):mmap Base 落地后落盘不再承担内存压力缓解(A1 由
/// 「常驻 = mmap Base + 小 Overlay」兜底),拉长间隔/提高批量以减少全量重写与
/// remap 频率;停机 flush 时机不变。
const FLUSH_INTERVAL: Duration = Duration::from_secs(120);
/// 落盘节流:累计变更超过此条数 → 立即触发快照(放宽理由同上)。
const FLUSH_DIRTY_LIMIT: u64 = 30_000;
/// 落盘失败后的重试退避基值(I4):连续失败按 5s、10s、20s…… 指数退避,上限
/// `FLUSH_INTERVAL`。没有它时 `dirty >= FLUSH_DIRTY_LIMIT` 会在 150ms 消费
/// 循环里每轮都命中(写失败的 dirty 不清零)→ 全量遍历 + 写盘尝试打满 CPU/IO
/// (数据目录 ENOSPC/只读的实测形态)。
const FLUSH_RETRY_BASE: Duration = Duration::from_secs(5);
/// 退避指数的封顶位移(5s << 5 = 160s,再被 FLUSH_INTERVAL 截断)。
const FLUSH_RETRY_MAX_SHIFT: u32 = 5;
/// Overlay 规模上限:条目数或三段 arena 字节超限即触发合并(即便变更计数未到
/// 阈值)。目录 diff 会把整份 listing 收进 Overlay,大目录/事件风暴下条目数可
/// 远超 dirty 计数;无上限时单次增量可长到百万级(堆峰值与 flush 后死页都随之
/// 放大)。超限合并把常驻 Overlay 钉在几十 MB 量级。
const OVERLAY_FLUSH_ENTRIES: usize = 150_000;
const OVERLAY_FLUSH_BYTES: usize = 32 * 1024 * 1024;
/// 事件合批静默窗(tech.md 增量管线)。
const BATCH_WINDOW: Duration = Duration::from_millis(100);
/// 首扫流式聚合批大小:每批持写锁聚合后即释放,批间查询可见已扫部分(product 场景 2)。
const SCAN_CHUNK: usize = 50_000;
/// 重扫冷却:两次全量重扫的最小间隔(防「重扫→新 dropped→重扫」退化振荡;
/// 实测真实实例曾 420 次重复重扫,索引永在重扫、日志刷屏)。
const RESCAN_COOLDOWN: Duration = Duration::from_secs(30);
/// 丢弃标记触发的重扫去抖:事件流安静这么久才真正重扫。风暴期间重扫会与事件
/// 风暴互相加剧(重扫占 CPU/IO → 更多事件被丢 → 又一次重扫),实测这是
/// 「丢弃连刷 + 重扫振荡」的主因;先登记、等安静再跑。
const RESCAN_QUIET: Duration = Duration::from_secs(2);
/// 持续风暴下强制重扫的上限等待:事件一直不安静也必须收敛,不能无限待命。
const RESCAN_MAX_WAIT: Duration = Duration::from_secs(60);
/// 去抖重扫抢锁失败后的最小重试间隔:窗口内只允许一次尝试,防止每 150ms
/// 新建一个立刻退出的重扫线程(在途重扫的 drain/flush 段可长达数秒)。
const RESCAN_ATTEMPT_GAP: Duration = Duration::from_millis(500);

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
fn ensure_overlay_ready(h: &mut IndexHolder, spare: Option<engine::overlay::Overlay>) {
    match h {
        IndexHolder::Shared(_) | IndexHolder::Mutable(_) => {
            let taken = std::mem::replace(h, IndexHolder::Empty);
            let mut overlay = spare.unwrap_or_default();
            overlay.reset(); // 防御:调用方交来的 spare 必须干净(复用契约)
            overlay.reserve_baseline(); // 虚拟预留,避免大 listing 的增长换块死页
            *h = match taken {
                IndexHolder::Shared(v) => IndexHolder::Overlaid(Box::new(OverlaidState {
                    base: BaseRef::Mmap(v),
                    overlay,
                })),
                IndexHolder::Mutable(idx) => IndexHolder::Overlaid(Box::new(OverlaidState {
                    base: BaseRef::Heap(idx),
                    overlay,
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
    /// 连续落盘失败次数(I4):`should_flush` 据此做指数退避,防写失败时
    /// 「dirty ≥ 阈值 → 每 150ms 全量遍历 + 写盘」紧循环。成功清零。
    flush_failures: AtomicU32,
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
    /// 账本腐化自愈位:flush_merged 的合并写被腐化哨兵拒绝时置位,消费循环
    /// 消费后触发降级全量重扫(ADR-5「重扫=最终一致性闸门」——拒绝在腐化
    /// 账本上合并,但必须自愈而非无限拒绝)。经 flag 而非同步降级的理由:
    /// flush_merged 持 flush_lock,degrade_and_rescan 尾部 flush 重入自死锁;
    /// spawn_rescan 需要 Arc,消费循环持有。
    heal_requested: AtomicBool,
    /// 合并 flush 的可复用暂存:旧号→新号重映射表(百万级 ~6MB)+ 新增目录清单。
    /// 跨 flush 复用容量,避免在 macOS malloc Large 区反复「分配-释放」留下不
    /// 可折叠脏页(单次 flush 的 entries/offsets Vec 已改溢写后,这是唯一的大块)。
    merge_scratch: Mutex<MergeScratch>,
    /// 合并 flush 换基后回收的 Overlay(已 reset;页已标可回收),下次首 diff
    /// 直接复用——重放/事件风暴期长出的百万级 arena 不再每次 flush 释放成
    /// 不可折叠死页。
    spare_overlay: Mutex<Option<engine::overlay::Overlay>>,
    /// 卷全量扫描闸门(C1,见 [`VolumeScanGate`])。
    volume_scan_gate: Mutex<VolumeScanGate>,
    /// 事件流运行计数(保留窗口内的增量,日志每 60s 汇总一次)。
    /// 计数本身不是业务状态,中毒与否不影响索引,取 recover。
    run_stats: Mutex<RunStats>,
    /// 丢弃触发的重扫去抖(见 [`RescanDebounce`])。
    rescan_debounce: Mutex<RescanDebounce>,
}

/// 事件流运行计数。只用于日志归因:区分「事件量本身很大」与「系统在丢事件」,
/// 以及丢的是哪一类标记——这决定该调事件粒度还是调消费能力。
#[derive(Default)]
struct RunStats {
    events: u64,
    drops: u64,
    must_scan: u64,
    user_dropped: u64,
    kernel_dropped: u64,
    ids_wrapped: u64,
    last_report: Option<Instant>,
    reported_events: u64,
    reported_drops: u64,
    reported_must: u64,
    reported_user: u64,
    reported_kernel: u64,
    reported_wrapped: u64,
}

/// 记一条事件(含丢弃标记),供周期性汇总。
fn record_event_stats(state: &EngineState, ev: &fsevents::FseEvent) {
    let mut s = state.run_stats.lock_recover();
    s.events = s.events.saturating_add(1);
    let b = fsevents::drop_bits(ev.flags);
    if b.must_scan || b.user_dropped || b.kernel_dropped || b.ids_wrapped {
        s.drops = s.drops.saturating_add(1);
        s.must_scan += u64::from(b.must_scan);
        s.user_dropped += u64::from(b.user_dropped);
        s.kernel_dropped += u64::from(b.kernel_dropped);
        s.ids_wrapped += u64::from(b.ids_wrapped);
    }
}

/// 事件统计汇总窗口。
const STATS_WINDOW: Duration = Duration::from_secs(60);

/// 一次汇总窗口的增量(全部为窗口内差值)。
#[derive(Debug, Default, PartialEq, Eq)]
struct StatsDelta {
    events: u64,
    drops: u64,
    must_scan: u64,
    user_dropped: u64,
    kernel_dropped: u64,
    ids_wrapped: u64,
}

impl RunStats {
    /// 窗口到期时取增量并推进基线;未到期返回 None。空窗口返回全 0(由调用方
    /// 决定不出行)。提取为纯逻辑,便于单测「不重复计数 / 未到期不取」。
    fn take_delta(&mut self, now: Instant, window: Duration) -> Option<StatsDelta> {
        let due = self
            .last_report
            .is_none_or(|t| now.duration_since(t) >= window);
        if !due {
            return None;
        }
        let d = StatsDelta {
            events: self.events.saturating_sub(self.reported_events),
            drops: self.drops.saturating_sub(self.reported_drops),
            must_scan: self.must_scan.saturating_sub(self.reported_must),
            user_dropped: self.user_dropped.saturating_sub(self.reported_user),
            kernel_dropped: self.kernel_dropped.saturating_sub(self.reported_kernel),
            ids_wrapped: self.ids_wrapped.saturating_sub(self.reported_wrapped),
        };
        self.last_report = Some(now);
        self.reported_events = self.events;
        self.reported_drops = self.drops;
        self.reported_must = self.must_scan;
        self.reported_user = self.user_dropped;
        self.reported_kernel = self.kernel_dropped;
        self.reported_wrapped = self.ids_wrapped;
        Some(d)
    }
}

/// 每 60s 汇总一次事件/丢弃增量;窗口内无事件则不出行(空闲日志保持安静)。
/// 主流与卷流都可调用(计数器与 `last_report` 共享,同一窗口只出一行)。
fn maybe_log_stats(state: &EngineState) {
    let mut s = state.run_stats.lock_recover();
    let Some(d) = s.take_delta(Instant::now(), STATS_WINDOW) else {
        return;
    };
    drop(s);
    if d.events == 0 && d.drops == 0 {
        return;
    }
    log::info!(
        "事件统计({}s): events={} drops={} [must_scan={} user_dropped={} kernel_dropped={} ids_wrapped={}]",
        STATS_WINDOW.as_secs(),
        d.events,
        d.drops,
        d.must_scan,
        d.user_dropped,
        d.kernel_dropped,
        d.ids_wrapped
    );
}

#[derive(Default)]
struct MergeScratch {
    base_remap: Vec<u32>,
    new_dirs: Vec<(u32, Vec<u8>)>,
}

/// 丢弃触发的重扫去抖状态。语义见 [`RESCAN_QUIET`]/[`RESCAN_MAX_WAIT`]:
/// - `request`:登记待命并记录**最大的丢弃事件 ID**(重复丢弃不刷新起点);
/// - `note_event`:每条事件刷新安静窗的起点;
/// - `due`:安静期满或待命超过上限 → 可以尝试重扫;
/// - `covered_by(floor)`:重扫提交水位 floor 时,只有「丢弃 ID ≤ floor」的待命
///   才算被这轮重扫覆盖、可以核销;ID 更大的丢弃发生在扫描窗口内,必须保留,
///   否则会静默遗漏(评审 H1/M2)。
/// - `attempt_allowed`:抢重扫锁失败时限制重试频率,避免每 150ms 新建线程。
#[derive(Default)]
struct RescanDebounce {
    pending_since: Option<Instant>,
    /// 待命期间见到的最大丢弃事件 ID(未登记时为 0)。
    pending_max_id: u64,
    last_event_at: Option<Instant>,
    last_attempt: Option<Instant>,
}

impl RescanDebounce {
    /// 返回是否首次登记(调用方据此决定是否留日志,避免连刷)。
    fn request(&mut self, now: Instant, drop_event_id: u64) -> bool {
        self.pending_max_id = self.pending_max_id.max(drop_event_id);
        if self.pending_since.is_some() {
            return false;
        }
        self.pending_since = Some(now);
        true
    }

    fn note_event(&mut self, now: Instant) {
        self.last_event_at = Some(now);
    }

    fn due(&self, now: Instant, quiet: Duration, max_wait: Duration) -> bool {
        let Some(since) = self.pending_since else {
            return false;
        };
        if now.duration_since(since) >= max_wait {
            return true;
        }
        match self.last_event_at {
            Some(t) => now.duration_since(t) >= quiet,
            None => true,
        }
    }

    /// 重扫在 `floor` 处提交水位:覆盖该 ID 及更早的丢弃。返回是否核销了待命。
    fn covered_by(&mut self, floor: u64) -> bool {
        if self.pending_since.is_some() && self.pending_max_id <= floor {
            self.take();
            return true;
        }
        false
    }

    /// 抢锁失败后的重试节流:窗口内只允一次尝试。
    fn attempt_allowed(&mut self, now: Instant, gap: Duration) -> bool {
        if self
            .last_attempt
            .is_some_and(|t| now.duration_since(t) < gap)
        {
            return false;
        }
        self.last_attempt = Some(now);
        true
    }

    fn take(&mut self) {
        self.pending_since = None;
        self.pending_max_id = 0;
    }
}

/// 卷全量扫描闸门(C1)。卷扫描线程把卷内条目**直写 Base**;若同窗口内卷流
/// 消费线程把同卷目录 diff 进 Overlay(`overlaid.insert(V)`),扫描随后插入的
/// 深层 Base 条目父链上最近 overlaid 祖先是 V 且非 anchor → 查询/合并写按
/// 「被屏蔽陈旧 Base」丢弃,卷被截断为一级子项且不可自愈。
///
/// 闸门语义:扫描期间该卷前缀内的 diff 只登记(事件只是提示),扫描结束后
/// 按「重读磁盘」逐目录重放——既不丢窗口变更,也不与扫描写 Base 交错。
#[derive(Default)]
struct VolumeScanGate {
    /// 正在全量扫描的卷根(canonical 形态)。
    owner: Option<PathBuf>,
    /// 本次扫描的世代号:旧扫描收尾不得误清新扫描的闸/偷走其 pending。
    token: u64,
    /// 闸门期间被挡下的目录(canonical),扫描结束后重放 diff。
    pending: HashSet<PathBuf>,
}

/// 进入卷扫描闸门,返回本次 token。卷扫描由监听线程串行执行,正常不重入;
/// 上一轮若异常残留 owner,以新 token 直接接管(旧 end 因 token 不符不再动闸)。
fn begin_volume_scan(state: &EngineState, vol: &Path) -> u64 {
    let mut g = state.volume_scan_gate.lock_recover();
    g.token = g.token.wrapping_add(1);
    g.owner = Some(vol.to_path_buf());
    g.pending.clear();
    g.token
}

/// 退出卷扫描闸门:token 相符才释放 owner 并交出 pending;被新扫描接管时
/// 返回空(那批目录属于新扫描的窗口,不得由旧线程重放)。
fn end_volume_scan(state: &EngineState, token: u64) -> Vec<PathBuf> {
    let mut g = state.volume_scan_gate.lock_recover();
    if g.token != token {
        return Vec::new();
    }
    g.owner = None;
    g.pending.drain().collect()
}

/// 扫描闸门下 diff 的登记:目录位于当前扫描卷前缀内 → 只记录并返回 true。
fn gate_volume_diff(state: &EngineState, dir: &Path) -> bool {
    let mut g = state.volume_scan_gate.lock_recover();
    match &g.owner {
        Some(owner) if dir.starts_with(owner) => {
            g.pending.insert(dir.to_path_buf());
            true
        }
        _ => false,
    }
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
            flush_failures: AtomicU32::new(0),
            flush_lock: Mutex::new(()),
            dir_map: Mutex::new(DirIndexMap::default()),
            roots: Mutex::new(vec![]),
            cfg: Mutex::new(config::Config {
                excluded: vec![],
                auto_index_volumes: false,
                volumes_excluded: vec![],
                include_hidden: None,
            }),
            exclude_op_lock: Mutex::new(()),
            rescan_lock: Mutex::new(()),
            rescanning: AtomicBool::new(false),
            rescan_dirty: Mutex::new(HashSet::new()),
            last_rescan: Mutex::new(None),
            perm_denied: Mutex::new(HashSet::new()),
            diff_fail_counts: Mutex::new(HashMap::new()),
            heal_requested: AtomicBool::new(false),
            merge_scratch: Mutex::new(MergeScratch::default()),
            spare_overlay: Mutex::new(None),
            volume_scan_gate: Mutex::new(VolumeScanGate::default()),
            run_stats: Mutex::new(RunStats::default()),
            rescan_debounce: Mutex::new(RescanDebounce::default()),
        }
    }

    fn set_state(&self, s: &'static str) {
        *self.state_name.lock_recover() = s;
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
/// dir_only(保存面板助手,与 protocol 契约同源)= None/Some(false) 不过滤,
/// 仅 Some(true) 生效;缺省(旧客户端)为 None,与现状逐位一致。
/// include_hidden(隐藏项开关)= 请求级 Some 覆盖,None 跟随 daemon 配置。
fn run_query(
    state: &EngineState,
    q: &str,
    limit: u32,
    sort: Option<&str>,
    desc: bool,
    dir_only: bool,
    include_hidden: Option<bool>,
) -> Response {
    let (key, key_str) = parse_sort_key(sort);
    let desc_echo = desc_echo(key, desc);
    let Ok(mut plan) = engine::query::parse(q) else {
        return Response::Error {
            code: "parse".into(),
            message: "空查询".into(),
        };
    };
    plan.dir_only = dir_only;
    // 请求级覆盖优先;None 跟随 daemon 配置。配置读取只持锁一瞬,
    // 不跨 with_view(查询读锁)。
    let include_hidden =
        include_hidden.unwrap_or_else(|| state.cfg.lock_recover().include_hidden_effective());
    plan.include_hidden = include_hidden;
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
            let mut c = cache.lock_recover();
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
        let c = cache.lock_recover();
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
    let h = state.holder.read_fail_fast("holder");
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
    let mut h = state.holder.write_fail_fast("holder");
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
    let literal_roots = state.roots.lock_recover().clone();
    let h = state.holder.read_fail_fast("holder");
    // 只按 Base 建:dir_map 是「绝对路径 → base 条目号」的寻址面,overlay 目录的
    // 寻址在 Overlay::dir_ids(纯 overlay 目录),两空间不混号。
    let view: &dyn IndexView = match &*h {
        IndexHolder::Shared(v) => v.as_ref(),
        IndexHolder::Mutable(i) => i,
        IndexHolder::Overlaid(st) => st.base.as_view(),
        IndexHolder::Empty => return,
    };
    let map = build_dir_map(view, &literal_roots);
    // holder 读锁**仍持着**时安装 map(C2 窗口 2):换基(重扫替换/compact)必须
    // 先拿 holder 写锁,故旧视图的 map 绝无可能在「新 Base 已安装」之后才落到
    // dir_map——否则会以旧号覆盖新 map。锁序 holder(读) → dir_map,与
    // diff/换基的 holder(写) → dir_map 同序,无环。
    *state.dir_map.lock_fail_fast("dir_map") = map;
}

/// 从给定视图构建「绝对目录路径 → 条目号」映射(**纯函数**:不触碰 holder/
/// dir_map 锁)。供 [`rebuild_dir_map`] 与换基路径(必须「先构建、后与 holder
/// 同点原子提交」)共用——条目号只在该视图自身空间内有效。
fn build_dir_map(view: &dyn IndexView, literal_roots: &[PathBuf]) -> DirIndexMap {
    // 两个形态各司其职:根条目名是「字面配置根」(scanner 契约,用于匹配);
    // map 键用 canonical 形态——/tmp 等系统符号链接下 FSEvents 上报真实路径
    // (/private/tmp/...),canonicalize 让事件侧与索引侧同形。
    //
    // 顺序单趟建图(替代旧「邻接表 + BFS」):条目号契约保证父先于子,目录
    // 只需父目录的已建路径 + 自身 display 名即可拼接。旧实现为 145 万条目建
    // children: HashMap<u32, Vec<u32>>(百万级 push + 每父小 Vec)再 BFS 复制
    // PathBuf,冷启动/重扫一次留下 ~40MB 不可折叠死页;单趟版临时内存只有
    // 条目号→槽位表与一个路径缓冲,且按目录数精确预留 map 容量。
    let mut n_dirs = 0usize;
    let mut n_entries = 0usize;
    let mut path_bytes = 0usize;
    for &(start, seg) in &view.entry_segments() {
        n_entries += seg.len();
        for (j, e) in seg.iter().enumerate() {
            if e.is_live() && e.is_dir() {
                n_dirs += 1;
                path_bytes += view.dir_path(start + j as u32).len();
            }
        }
    }
    // arena 还需每个路径一个 '/' 的余量;精确预留免去倍增换块。
    let mut map = DirIndexMap::with_capacity(n_dirs, path_bytes + n_dirs);
    // 条目号 → map 池槽(u32::MAX = 未收录)。仅目录条目有值。
    let mut slots: Vec<u32> = vec![u32::MAX; n_entries];
    let mut path: Vec<u8> = Vec::with_capacity(256);
    for &(start, seg) in &view.entry_segments() {
        for (j, e) in seg.iter().enumerate() {
            let i = start + j as u32;
            if !e.is_live() || !e.is_dir() {
                continue;
            }
            if e.parent == NO_PARENT {
                // 根条目名 = 完整绝对路径(scanner 契约),按字面根匹配
                let name = String::from_utf8_lossy(view.display_name(i));
                if let Some(r) = literal_roots.iter().find(|r| r.to_string_lossy() == name) {
                    let key = r.canonicalize().unwrap_or_else(|_| r.clone());
                    slots[i as usize] = map.insert_get_slot(&key, i);
                }
                continue;
            }
            let parent_slot = slots[e.parent as usize];
            if parent_slot == u32::MAX {
                continue; // 父目录不在任一扫描根下(未收录)
            }
            path.clear();
            path.extend_from_slice(map.path_at(parent_slot).as_os_str().as_encoded_bytes());
            path.push(b'/');
            path.extend_from_slice(view.display_name(i));
            let key = Path::new(OsStr::from_bytes(&path));
            slots[i as usize] = map.insert_get_slot(key, i);
        }
    }
    // 槽位表为临时大块:清空后标可回收再 drop(冷启动/重扫一次约 5-6MB)。
    slots.clear();
    engine::malloc_pressure::release_reusable_pages(&mut slots);
    drop(slots);
    map
}

/// 换基的唯一提交点(holder + dir_map **同点替换**,锁序 holder → dir_map 与
/// diff_one/apply_and_track 一致,无环)。dir_map 必须由调用方预先从**即将安装
/// 的视图**构建:两条分开写(先换 holder、后 rebuild)会留下「新 Base + 旧 map」
/// 窗口,窗口内的 diff 用旧号打新索引——号可能恰好命中无关活条目,把磁盘
/// listing 挂到错父下,海量垃圾路径随下次合并写固化(实机:heal 重扫后条目数
/// 2.39M→2.04M 不降、index.bin 313MB 反复复写;毒 Base 规模下窗口被放大)。
fn install_holder_with_map(state: &EngineState, next: IndexHolder, map: DirIndexMap) {
    let mut h = state.holder.write_fail_fast("holder");
    *h = next;
    *state.dir_map.lock_fail_fast("dir_map") = map;
}

/// 索引健全性抽检的采样条数(冷启动 load 后一次,~256 次 stat,数 ms)。
const INDEX_POISON_SAMPLE: usize = 256;
/// 采样路径「不存在」占比超过此百分比即判索引中毒。
const INDEX_POISON_MISSING_PCT: usize = 20;

/// 抽检结果判定(纯函数,便于钉住边界)。
fn is_poison_sample(missing: usize, sampled: usize) -> bool {
    sampled >= 32 && missing * 100 / sampled > INDEX_POISON_MISSING_PCT
}

/// 索引健全性抽检:stride 采样活条目,stat 其完整路径,统计「路径不存在」
/// (ENOENT;ENOTDIR = 路径中某级被文件顶替,错挂的典型形态)占比。
///
/// 为什么需要主动判毒:历史版本 bug 的产物把真实 listing 挂到错父下,拼出的
/// 路径大量不存在(实测毒样本 43%),但条目在合成视图里「合法」——合并不一定
/// 报腐化,垃圾会被反复合并收编,永不消退。健康索引由目录 diff 维护,路径
/// 几乎全部存在(实测干净索引 0%);全量 stat 需数十秒,采样数 ms。
fn index_path_missing_sample(view: &dyn IndexView) -> (usize, usize) {
    let total: usize = view.entry_segments().iter().map(|(_, s)| s.len()).sum();
    if total == 0 {
        return (0, 0);
    }
    let step = (total / INDEX_POISON_SAMPLE).max(1);
    let mut missing = 0usize;
    let mut sampled = 0usize;
    let mut slot = 0usize;
    for &(start, seg) in &view.entry_segments() {
        for (j, e) in seg.iter().enumerate() {
            let this = slot;
            slot += 1;
            if !this.is_multiple_of(step) || !e.is_live() {
                continue;
            }
            sampled += 1;
            let p = view.full_path_dyn(start + j as u32);
            match std::fs::symlink_metadata(&p) {
                Ok(_) => {}
                Err(err)
                    if err.kind() == std::io::ErrorKind::NotFound
                        || err.kind() == std::io::ErrorKind::NotADirectory =>
                {
                    missing += 1;
                }
                // EPERM/EACCES(TCC 保护目录)不算「不存在」——条目本身合法
                Err(_) => {}
            }
        }
    }
    (missing, sampled)
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

/// dir_map 给出的 base 条目号在当前 Base 中仍是**活目录**?(C2:换基/整理后的
/// 陈旧 map 窗口里,旧号可能越界或指向无关的活条目。所有 Base 目标的判活必须
/// 走本函数——直接 `entry()` 下标越界即 panic,指向文件则会把 listing 挂到文件下。)
fn base_dir_target_live(view: &dyn IndexView, d0: u32) -> bool {
    view.entry_segments().iter().any(|&(s, seg)| {
        d0 >= s
            && seg
                .get((d0 - s) as usize)
                .is_some_and(|e| e.is_live() && e.is_dir())
    })
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
        // #12):map 残留的条目号对新索引可能**越界**。段内 get + is_live + is_dir
        // 判活(scanner.rs 同一模式):越界/已死/非目录键一律清映射短路,绝不按
        // 死号下标。突变「回退 entry(d0) 直下标」/「去掉 is_dir」由 C2 测试钉死。
        if !base_dir_target_live(base.as_view(), d0) {
            map.remove(abs_dir); // 目录已删(子树墓碑)、键失配或指向文件:清映射
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
            handle_diff_error(state, base, overlay, map, abs_dir, excludes, &e);
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
    // 卷扫描闸门(C1):该卷全量扫描期间,卷前缀内的 diff 只登记、不应用。
    // 扫描线程直写 Base;并发的 Overlay 收录会让扫描插入的深层 Base 条目
    // 因「最近 overlaid 祖先非 anchor」被合并 flush 静默丢弃(卷截断)。
    // 扫描结束后由 scan_volume_into 统一重放(重读磁盘,幂等)。
    if gate_volume_diff(state, &base) {
        return 0;
    }
    // 持久失败负缓存:短路(含补链路径)。EPERM 目录保留索引条目但不再重试。
    // 重启清零——用户补授权限后重启即自动重新索引。
    if state.perm_denied.lock_recover().contains(&base) {
        return 0;
    }
    // 重扫窗口记账:替换段完成后重放追平。push 不持 holder 锁(在取锁之前);
    // 安全性由「push 恒先于同次 diff 的 apply,且 apply 持 holder 写锁、替换段
    // 挂 holder 也持写锁」的互斥保证:「先 push 后 drain → 基于旧索引 → 被重放」
    // 或「后 push → 基于新索引 → 不重放」,无第三种交错。
    if state.rescanning.load(Ordering::SeqCst) {
        state.rescan_dirty.lock_recover().insert(base.clone());
    }
    let excludes = reconcile::expand_excludes(&state.cfg.lock_recover().excluded);
    let mut h = state.holder.write_fail_fast("holder");
    // T3:diff 直写 Overlay,不再物化 Base(Shared/Mutable 首个 diff 转 Overlaid)。
    // 形态转换时才取 spare(已是 Overlaid 时保持当前账本不动)。
    if matches!(&*h, IndexHolder::Shared(_) | IndexHolder::Mutable(_)) {
        let spare = state.spare_overlay.lock_recover().take();
        ensure_overlay_ready(&mut h, spare);
    }
    let IndexHolder::Overlaid(st) = &mut *h else {
        return 0; // Empty(首扫中):无可 diff 目标
    };
    // 解构改名:局部 `base` 历史语义是 canonical 目录路径,holder 侧载体用 holder_base
    let OverlaidState {
        base: holder_base,
        overlay,
    } = st.as_mut();
    let mut map = state.dir_map.lock_fail_fast("dir_map");
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
    build_meta_at(state, state.applied_event_id.load(Ordering::Relaxed))
}

/// 以**显式水位**构建落盘 meta(C3)。常规 flush 取当前 applied;扫描产物挂载
/// 必须取扫描开始时的 floor——扫描期间消费线程仍会把 id>floor 的事件 diff 进
/// 旧 Base 并推进 applied,但产物内容不含这些变更。若把当前 applied 写进新盘面,
/// 落盘+安装后、rescan_dirty 重放完成前崩溃/Stop,重启会从超前水位重放而跳过
/// 这些事件 → 索引永久缺条目。
fn build_meta_at(state: &EngineState, watermark: u64) -> IndexMeta {
    let cfg = state.cfg.lock_recover().clone();
    IndexMeta {
        built_at: state.built_at.load(Ordering::Relaxed),
        last_event_id: watermark,
        scan_roots: state
            .roots
            .lock_recover()
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
/// Shared 态(盘面即真相)无需重写;Mutable 态写盘后**收敛回 mmap Shared**
/// (I6:堆索引只是卷扫描/首扫的临时载体,不回收会长期驻留 ~100MB 级);
/// Overlaid 态(T4 正式形态)走合并流式写 + remap 换 Base,见 [`flush_merged`]。
///
/// I4:任何一次尝试之后都推进 `last_flush`;写失败额外累计 `flush_failures`,
/// 由 [`should_flush`] 做指数退避——否则 dirty 恒 ≥ 阈值时消费循环每 150ms
/// 触发一次全量遍历 + 写盘,数据目录 ENOSPC/只读下会打满 CPU/IO。
fn flush(state: &EngineState, paths: &DaemonPaths) {
    // 落盘串行域只覆盖写盘/换基;压力归还在锁释放后执行——8 轮
    // malloc_zone_pressure_relief 不需要与其它落盘路径互斥,持锁跑会无谓拉长
    // flush 串行段(消费循环的落盘节流、stop 落盘都在等这把锁)。
    let relieve = {
        let _guard = state.flush_lock.lock_fail_fast("flush_lock");
        // 先按当前形态选路;形态只可能被并发 diff 从 Mutable/Shared 转成
        // Overlaid,各分支都在自己的写锁内复核并安全降级。
        let form = {
            let h = state.holder.read_fail_fast("holder");
            match &*h {
                IndexHolder::Overlaid(_) => FlushForm::Overlaid,
                IndexHolder::Mutable(_) => FlushForm::Mutable,
                IndexHolder::Shared(_) | IndexHolder::Empty => FlushForm::Settled,
            }
        };
        match form {
            // 合并写需要「放读锁 → 取写锁」的锁形态,单独走 flush_merged
            FlushForm::Overlaid => flush_merged(state, paths),
            FlushForm::Mutable => {
                let converged = flush_mutable_converge(state, paths);
                *state.last_flush.lock_recover() = Instant::now();
                converged
            }
            FlushForm::Settled => {
                // 盘面即真相:滞留 dirty 只能是记账残留。持 holder 读锁清零——
                // diff/卷扫描的 dirty 增量都在 holder 写锁内,不会与本临界区
                // 交错,清掉的必是「已落盘」计数。I4 前提:否则 Shared 态
                // dirty 恒真会让 should_flush 每轮空转。
                let h = state.holder.read_fail_fast("holder");
                state.dirty.store(0, Ordering::Relaxed);
                state.flush_failures.store(0, Ordering::Relaxed);
                *state.last_flush.lock_recover() = Instant::now();
                drop(h);
                false
            }
        }
    };
    if relieve {
        engine::malloc_pressure::release_heap_pressure();
    }
}

/// [`flush`] 的形态选路(写锁内复核前的快照)。
#[derive(Clone, Copy)]
enum FlushForm {
    Overlaid,
    Mutable,
    Settled,
}

/// Mutable(Base 堆索引)的落盘 + 收敛(I6)。返回是否实际完成「写盘 → remap →
/// 换 Shared」(调用方据此归还堆压力)。
///
/// 全程持 holder **写锁**:Mutable 的在位写者(卷扫描批次/compact)同样要 holder
/// 写锁,写盘快照与 swap 之间不可能插入内容变更——否则 swap 会丢掉写入之后
/// 新应用的批次。代价是写盘期间查询短暂阻塞;Mutable 只在卷挂载/卸载与首扫
/// staging 等低频路径出现,可接受。
fn flush_mutable_converge(state: &EngineState, paths: &DaemonPaths) -> bool {
    let literal_roots = state.roots.lock_recover().clone();
    let mut h = state.holder.write_fail_fast("holder");
    let IndexHolder::Mutable(idx) = &*h else {
        // 形态已变(并发 diff 转 Overlaid):未写盘,增量交合并写;不算失败。
        return false;
    };
    let meta = build_meta(state);
    let layout = persist::EntryLayout::of(idx);
    if let Err(e) = persist::write_atomic(&layout, idx.entries.iter(), &meta, &paths.index_file()) {
        log::error!("落盘失败: {e}");
        // I4:失败退避(持写锁递增,不与 diff/扫描的 dirty 增量交错)。
        state.flush_failures.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    match persist::load_mmap(&paths.index_file()) {
        Ok(view) => {
            let map = build_dir_map(view.as_ref(), &literal_roots);
            // holder + dir_map 同点替换(见 install_holder_with_map 注)。
            let old = std::mem::replace(&mut *h, IndexHolder::Shared(view));
            if let IndexHolder::Mutable(mut old_idx) = old {
                old_idx.reset();
                old_idx.release_pages();
            }
            *state.dir_map.lock_fail_fast("dir_map") = map;
            state.dirty.store(0, Ordering::Relaxed);
            state.flush_failures.store(0, Ordering::Relaxed);
            true
        }
        Err(e) => {
            log::error!("落盘产物重载失败(保持内存索引,退避后重试): {e}");
            state.flush_failures.fetch_add(1, Ordering::Relaxed);
            false
        }
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
/// 返回是否实际尝试过合并写(调用方据此在 flush_lock 释放后归还堆压力)。
fn flush_merged(state: &EngineState, paths: &DaemonPaths) -> bool {
    // 重扫在途:holder 即将被替换,合并旧 Base 无意义——且毒 Base 下每次尝试都
    // 跑完全量遍历才撞腐化(实测 25s+),会把消费循环钉在写盘上。跳过即可,增量
    // 由重扫产物的 base 覆盖 + rescan_dirty 重放追平(重扫结束自有 flush)。
    if state.rescanning.load(Ordering::SeqCst) {
        return false;
    }
    // 合并写顺带产出旧号→新号表与新增目录清单(暂存跨 flush 复用,见字段注释)
    let mut scratch_guard = state.merge_scratch.lock_recover();
    let scratch = &mut *scratch_guard;
    let written = {
        let h = state.holder.read_fail_fast("holder");
        let IndexHolder::Overlaid(st) = &*h else {
            return false;
        };
        let epoch = st.overlay.epoch();
        let meta = build_meta(state);
        match engine::overlay::write_merged_atomic_collect(
            st.base.as_view(),
            &st.overlay,
            &meta,
            &paths.index_file(),
            &mut scratch.base_remap,
            &mut scratch.new_dirs,
        ) {
            Ok(_) => Some(epoch),
            Err(e) => {
                if engine::overlay::is_ledger_corruption(&e) {
                    // P0 实测第 N 形态:腐化哨兵 25 连刷使 flush 永不成功、
                    // Overlay 无限增长(1.40M→2.38M 合成读数)。止损 = 置自愈
                    // 位,由消费循环触发降级全量重扫(全新 Base 一次清零账本与
                    // 垃圾条目)。同时清 dirty + 推进节流起点:毒账本上重复全量
                    // 遍历毫无收益(实测 5839 次连刷),等下个积累窗口再试。
                    state.heal_requested.store(true, Ordering::SeqCst);
                    log::error!("合并写遇账本腐化,已置降级重扫自愈位: {e}");
                    state.dirty.store(0, Ordering::Relaxed);
                    state.flush_failures.fetch_add(1, Ordering::Relaxed);
                    *state.last_flush.lock_recover() = Instant::now();
                } else {
                    log::error!("合并落盘失败: {e}");
                    // I4:失败也推进节流起点并累计失败次数——否则 dirty 恒 ≥
                    // 阈值时每个 150ms 轮次都会重新触发全量遍历 + 写盘。
                    state.flush_failures.fetch_add(1, Ordering::Relaxed);
                    *state.last_flush.lock_recover() = Instant::now();
                }
                None
            }
        }
    };
    let Some(epoch) = written else { return false };
    match persist::load_mmap(&paths.index_file()) {
        Ok(view) => {
            // dir_map 必须与新 Base **同点提交**:合并写已产出旧号→新号表与
            // 新增目录清单,swap 写锁内直接增量修复现值/补键——与整表重建等价
            // (旧 Base 目录路径不变,只有未被留存者要摘、overlay 新增目录要补),
            // 但不再每次 flush 分配/释放百万级寻址表。原子性仍是硬约束:
            // 修复与 holder 替换同处 holder(write) → dir_map 的临界区内。
            if swap_base_if_fresh(state, view, epoch, &scratch.base_remap, &scratch.new_dirs) {
                state.dirty.store(0, Ordering::Relaxed);
                log::info!("合并 flush 完成,Base 已 remap 为 mmap,Overlay 清零");
            } else {
                log::info!("合并写期间有新增量(世代前移),本轮不替换,下轮重试");
                defer_stale_merge(state);
            }
            // 节流起点按「尝试过 flush」推进:失败重试不密集化
            *state.last_flush.lock_recover() = Instant::now();
            // 合并写的条目流/缓冲产物已释放;堆压力归还在 flush_lock 释放后
            // 由 flush 统一执行(见 flush 的块作用域)。
        }
        Err(e) => {
            log::error!("合并产物重载失败(保留增量态,下轮重试): {e}");
            // 节流起点照推进 + 失败退避(I4):dirty 已超批量阈值时,失败重试
            // 不得退化为消费循环逐轮密集全量重写。
            state.flush_failures.fetch_add(1, Ordering::Relaxed);
            *state.last_flush.lock_recover() = Instant::now();
        }
    }
    // 本轮重映射表用完即清:容量保留(跨 flush 复用),空页标可回收——下次
    // resize 重新缺页,不再以 dirty 计入 footprint。
    scratch.base_remap.clear();
    scratch.new_dirs.clear();
    engine::malloc_pressure::release_reusable_pages(&mut scratch.base_remap);
    engine::malloc_pressure::release_reusable_pages(&mut scratch.new_dirs);
    true
}

/// 写锁内复核快照世代并替换 Shared(替换即清 Overlay)。返回是否替换。
/// 「放读锁 → 取写锁」窗口内 diff 也必须拿写锁,故此刻的世代即终局判定:
/// 世代前移 = 快照过期(盘面不含窗口增量),保留 Overlaid 交下轮 flush 重新
/// 合并,绝不丢弃窗口内增量;形态已变(如重扫替换)同样不替换。
fn swap_base_if_fresh(
    state: &EngineState,
    view: Box<SharedView>,
    expected_epoch: u64,
    base_remap: &[u32],
    new_dirs: &[(u32, Vec<u8>)],
) -> bool {
    let mut h = state.holder.write_fail_fast("holder");
    let fresh = match &*h {
        IndexHolder::Overlaid(st) => st.overlay.epoch() == expected_epoch,
        _ => false,
    };
    if fresh {
        // dir_map 增量修复 + holder 同点替换(锁序 holder → dir_map,与 diff_one
        // 一致)。映射语义:dir_map 值恒为 base 条目号;base_remap 给出去留与
        // 新号,未留存者摘键;overlay 并入 Base 的新目录按合并号补键。
        // 两把锁同时在手时完成,查询/增量都看不到「新 Base + 旧 map」窗口。
        {
            let mut map = state.dir_map.lock_fail_fast("dir_map");
            map.remap(|old| match base_remap.get(old as usize) {
                Some(&nv) if nv != NO_PARENT => Some(nv),
                _ => None,
            });
            for (id, path) in new_dirs {
                map.insert(Path::new(OsStr::from_bytes(path)), *id);
            }
        }
        let old = std::mem::replace(&mut *h, IndexHolder::Shared(view));
        state.flush_failures.store(0, Ordering::Relaxed);
        // 回收旧 Overlay:清空保容量 + 空页标可回收,供下次增量复用。重放/
        // 事件风暴期 overlay 可涨到数十万条目,直接 drop 会让 macOS 滞留
        // 大块脏页(实测单次换基 ~100MB 级)。
        if let IndexHolder::Overlaid(st) = old {
            let OverlaidState { base, mut overlay } = *st;
            // Heap base(ENOENT 根级兜底物化残留)同样清空打标后再 drop。
            if let BaseRef::Heap(mut b) = base {
                b.reset();
                b.release_pages();
            }
            overlay.reset();
            overlay.release_pages();
            *state.spare_overlay.lock_recover() = Some(overlay);
        }
    }
    fresh
}

/// 陈旧快照(放弃替换)的收尾:盘面已含本批快照增量(写的就是含增量态),
/// 计入 dirty 的变更视为已落盘——清零让下次落盘等新变更积累(30k/120s)。
/// 否则 dirty 恒过阈值使全量合并写背靠背密集重试,而写期间的新 diff 又使
/// 世代前移,自激成风暴(P0 实测 23 次背靠背全量写的放大器之一)。
fn defer_stale_merge(state: &EngineState) {
    state.dirty.store(0, Ordering::Relaxed);
    state.flush_failures.store(0, Ordering::Relaxed);
    *state.last_flush.lock_recover() = Instant::now();
}

/// 最终落盘(stop 路径):持 flush_lock 串行化;返回 (落盘成功, 条目数)。
fn persist_final(state: &EngineState, paths: &DaemonPaths) -> (bool, u32) {
    let _guard = state.flush_lock.lock_fail_fast("flush_lock");
    let h = state.holder.read_fail_fast("holder");
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
    let h = state.holder.read_fail_fast("holder");
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

/// 连续失败的重试退避(I4):5s、10s、20s、40s、80s、120s(封顶正常间隔)。
fn flush_retry_backoff(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let shift = failures.saturating_sub(1).min(FLUSH_RETRY_MAX_SHIFT);
    FLUSH_RETRY_BASE
        .saturating_mul(1u32 << shift)
        .min(FLUSH_INTERVAL)
}

fn should_flush(state: &EngineState) -> bool {
    let dirty = state.dirty.load(Ordering::Relaxed);
    if dirty == 0 {
        return false;
    }
    let last = *state.last_flush.lock_recover();
    // I4:落盘连续失败时,阈值(dirty ≥ 30k)与 Overlay 规模闸都必须让位于
    // 退避窗口——否则写失败不清 dirty,消费循环每 150ms 重试一次全量遍历。
    let failures = state.flush_failures.load(Ordering::Relaxed);
    if failures > 0 && last.elapsed() < flush_retry_backoff(failures) {
        return false;
    }
    if dirty >= FLUSH_DIRTY_LIMIT || last.elapsed() >= FLUSH_INTERVAL {
        return true;
    }
    // 规模闸:变更计数不高但 Overlay 已长大(整份 listing 收录的目录多)时,
    // 同样立即合并,防单次增量无界增长。
    overlay_over_limit(state)
}

/// Overlay 是否已超过常驻规模上限(条目数或三段 arena 字节)。
fn overlay_over_limit(state: &EngineState) -> bool {
    let h = state.holder.read_fail_fast("holder");
    match &*h {
        IndexHolder::Overlaid(st) => {
            let (entries, arena_bytes) = st.overlay.footprint();
            entries >= OVERLAY_FLUSH_ENTRIES || arena_bytes >= OVERLAY_FLUSH_BYTES
        }
        _ => false,
    }
}

/// 应用一批目录 diff,全部完成后推进已应用水位。
/// 水位快照在 diff **之前**取:本批目录对应的全部事件此刻必已入 batcher,
/// 快照覆盖它们且不含后续未入批事件(水位只落后、绝不超前于索引内容)。
fn apply_batch(
    state: &EngineState,
    dirs: Vec<String>,
    track_watermark: bool,
    paths: Option<&DaemonPaths>,
) {
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
        // 批内规模闸:重放/事件风暴的单批可达数十万目录,等批末再 flush 会让
        // Overlay 先长到百万级(堆峰值与换基死页同步放大)。每个目录后检查
        // (读锁 + 两个长度比较,成本可忽略),超限即合并——单个大目录(实测
        // 29 万子项)也至少在下个目录前收口。水位在批末才推进,盘面水位落后
        // 于内容,安全。
        if let Some(p) = paths {
            if overlay_over_limit(state) {
                flush(state, p);
            }
        }
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
    let literal_roots = state.roots.lock_recover().clone();
    // compact 与 dir_map 重建**同一 holder 写锁临界区**(C2):compact 全量重排
    // base 条目号后旧 map 立即失效;若放锁再 rebuild,窗口内的并发 diff 会用旧号
    // 打新索引(号可能恰中无关活条目 → 错挂 + Overlay 污染),且 rebuild 可能在
    // 重扫换基之后用旧视图 map 覆盖新 map。build_dir_map 是纯函数,直接在锁内
    // 对已整理视图构建并同点安装(锁序 holder → dir_map,与 diff 一致)。
    let mut h = state.holder.write_fail_fast("holder");
    let (before, after) = match &mut *h {
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
    };
    {
        let view: &dyn IndexView = match &*h {
            IndexHolder::Mutable(idx) => idx,
            IndexHolder::Overlaid(st) => st.base.as_view(),
            _ => unreachable!("上方分支已限定 Mutable/Overlaid"),
        };
        let map = build_dir_map(view, &literal_roots);
        *state.dir_map.lock_fail_fast("dir_map") = map;
    }
    drop(h);
    log::info!("索引整理:条目 {before} → {after},回收墓碑与 arena 孤儿字节");
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
        // 丢弃去抖的触发点:每轮(≤150ms)检查一次,事件安静且冷却已过才真正重扫。
        maybe_fire_debounced_rescan(&state, &paths);
        match rx.recv_timeout(Duration::from_millis(150)) {
            Ok(ev) => {
                state
                    .rescan_debounce
                    .lock_recover()
                    .note_event(Instant::now());
                // 已被全量重扫覆盖的陈旧事件直接跳过。必须先于一切语义判定——
                // 重扫期间消费停摆、内核缓冲溢出产生的 dropped 标记就在其中,
                // 它们正是「重扫 → 又一批 dropped → 又重扫」死循环的燃料。
                // 统计也放在覆盖判定之后:积压的陈旧事件不是「新负载」,计入会
                // 夸大 events,失去「区分事件量 vs 消费能力」的用途。
                if event_covered_by_rescan(ev.id, state.rescan_floor.load(Ordering::Relaxed)) {
                    continue;
                }
                record_event_stats(&state, &ev);
                // 卷流不写全局水位:多流共享会让 applied 越过主消费方实际进度,
                // 重启重放从该水位起步将漏掉主流未应用事件(见 volumes.rs 模块注释)。
                if !volume_owned {
                    state.seen_event_id.fetch_max(ev.id, Ordering::Relaxed);
                }
                if fsevents::is_history_done(ev.flags) {
                    if is_replay {
                        // 追平前强制应用窗口内 pending 事件:否则「就绪」早于索引追上,
                        // 紧随的查询会漏停机窗口内的变更(实机重放抽验曾因此为空)。
                        apply_batch(&state, batcher.drain_all(), !volume_owned, Some(&paths));
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
                        if state.rescanning.load(Ordering::SeqCst) {
                            continue; // 重扫在途(判毒/抽样触发):就绪由重扫线程置
                        }
                        state.set_state(state::READY);
                        flush(&state, &paths);
                    }
                    continue;
                }
                if fsevents::is_dropped(ev.flags) {
                    // 在途重扫的 floor 取自扫描开始:其后的丢弃(id > floor)不在
                    // 该轮覆盖范围,同样要登记待命(covered_by 按 ID 精确核销),
                    // 不能因为「正在重扫」就丢掉这条信息。
                    if state.rescanning.load(Ordering::SeqCst) {
                        let newly = state
                            .rescan_debounce
                            .lock_recover()
                            .request(Instant::now(), ev.id);
                        if newly {
                            log::warn!(
                                "重扫在途收到丢弃类标记({}, id={}),登记待命(按 floor 覆盖判定)",
                                fsevents::drop_reason(ev.flags),
                                ev.id
                            );
                        }
                        continue;
                    }
                    // 不再当场全量重扫:风暴期间重扫会与事件风暴互相加剧
                    // (重扫占 CPU/IO → 更多事件被丢 → 又一次重扫)。先登记待命,
                    // 由 maybe_fire_debounced_rescan 在「事件安静 / 待命超上限 /
                    // 冷却已过」时触发一次。重复丢弃不刷新待命起点。
                    let newly = state
                        .rescan_debounce
                        .lock_recover()
                        .request(Instant::now(), ev.id);
                    if newly {
                        log::warn!(
                            "收到丢弃类标记({}, id={}, path={}, stream={}),登记去抖重扫(事件安静 {}s 后执行)",
                            fsevents::drop_reason(ev.flags),
                            ev.id,
                            ev.path,
                            if volume_owned { "volume" } else { "main" },
                            RESCAN_QUIET.as_secs()
                        );
                    } else {
                        log_throttled!(
                            warn,
                            "drop-pending",
                            RESCAN_COOLDOWN,
                            "丢弃标记持续,重扫已在待命(安静 {}s 或待命 {}s 后执行)",
                            RESCAN_QUIET.as_secs(),
                            RESCAN_MAX_WAIT.as_secs()
                        );
                    }
                    continue;
                }
                replay_events += 1;
                // 目录级事件:path 即变更所在目录,直接作为 diff 目标。若事件意外
                // 指向文件或已删除路径,diff_one 的 read_dir 会得到 ENOTDIR/ENOENT,
                // 由 handle_diff_error 的「按父目录收口」兜底——这里不逐事件 stat
                // (风暴期 8 万事件就是 8 万次额外系统调用)。
                if !ev.path.is_empty() {
                    batcher.push(ev.path);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                stream_errored = true;
                break;
            }
        }
        apply_batch(&state, batcher.drain_ready(), !volume_owned, Some(&paths));
        // 账本腐化自愈:flush_merged 无法同步降级(持 flush_lock 重入),经此位
        // 移交持 Arc 的消费循环;swap 语义保证并发置位不丢(重扫在途时 spawn
        // 内部 try_lock 弃权,在途重扫本身即重建)。
        if state.heal_requested.swap(false, Ordering::SeqCst) {
            log::warn!("账本腐化自愈:触发降级全量重扫(重扫=最终一致性闸门)");
            spawn_rescan(&state, &paths);
        }
        // 主流与卷流都可驱动汇总:计数器与 last_report 共享,同一窗口只出一行;
        // 主流断流后卷流仍能继续汇总(见 maybe_log_stats)。
        maybe_log_stats(&state);
        if should_flush(&state) {
            flush(&state, &paths);
        }
    }

    // 退出前强制排空 pending:停止路径(或流断开)若跳过这一步,窗口内已收未应用的
    // 变更会因水位停在 diff 完成点而被正确重放,但此处仍排空以让落盘内容尽量新。
    apply_batch(&state, batcher.drain_all(), !volume_owned, Some(&paths));

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
        let map = state.dir_map.lock_fail_fast("dir_map");
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
/// 阻塞获取重扫锁;中毒(前次重扫线程 panic)时清毒后取 inner——中毒不恢复
/// 会让自愈路径永久哑火(实机 heal 79 次零效果的可疑形态之一)。
fn acquire_rescan_blocking(lock: &Mutex<()>) -> std::sync::MutexGuard<'_, ()> {
    match lock.lock() {
        Ok(g) => g,
        Err(e) => {
            log::warn!("rescan_lock 中毒(前次重扫线程 panic),恢复后继续");
            lock.clear_poison();
            e.into_inner()
        }
    }
}

/// 非阻塞获取重扫锁:WouldBlock = 确在途(None);Poisoned = 前持有者已死,
/// 无在途写者 → 清毒后继续。不得把 Poisoned 吞成「在途」,否则自愈链断。
fn try_acquire_rescan(lock: &Mutex<()>) -> Option<std::sync::MutexGuard<'_, ()>> {
    match lock.try_lock() {
        Ok(g) => Some(g),
        Err(std::sync::TryLockError::WouldBlock) => None,
        Err(std::sync::TryLockError::Poisoned(e)) => {
            log::warn!("rescan_lock 中毒(前次重扫线程 panic),恢复后继续");
            lock.clear_poison();
            Some(e.into_inner())
        }
    }
}

/// 重扫进行中标志的兜底复位:重扫线程 panic 时否则永久为 true——消费循环会
/// 一直忽略 dropped、`flush_merged` 一直早退,索引再也不会落盘。
struct ResetRescanning<'a>(&'a AtomicBool);
impl Drop for ResetRescanning<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

fn degrade_and_rescan(state: &EngineState, paths: &DaemonPaths, wait_for_lock: bool) {
    // 中毒恢复:上一轮重扫线程若 panic,rescan_lock 永久 Poisoned——try_lock 的
    // Err 若一律当「在途」吞掉,自愈路径就此永久哑火(实机:heal 79 次零效果)。
    // Poisoned 意味着前持有者已死、无在途写者,取 inner 继续;WouldBlock 才是
    // 真在途。
    let _guard = if wait_for_lock {
        Some(acquire_rescan_blocking(&state.rescan_lock))
    } else {
        match try_acquire_rescan(&state.rescan_lock) {
            Some(g) => Some(g),
            None => {
                log::info!("重扫已在进行,忽略本次自动触发");
                return;
            }
        }
    };
    let before = with_view(state, |v| live_count_of(v));
    log::info!("重扫开始(当前条目数: {before:?})");
    state.set_state(state::DEGRADED);
    state.rescanning.store(true, Ordering::SeqCst);
    let _rescanning = ResetRescanning(&state.rescanning);
    let floor = fsevents::current_event_id();
    let roots = state.roots.lock_recover().clone();
    let excludes = reconcile::expand_excludes(&state.cfg.lock_recover().excluded);
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
    *state.gaps.lock_recover() = gap_list;
    state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
    // 重扫替换段(T4 迁移,tech.md §4.3「P0 替换段衔接」收口):产物**直写落盘 →
    // remap mmap Base → 清 Overlay**,不挂堆上 Mutable(与首扫共用
    // mount_scan_product)。旧 Overlay 随整体替换丢弃——重扫产物为全量,窗口内
    // 变更由下方 drain 重放追平。
    log::info!("扫描完成(磁盘产物条目数: {})", idx.entry_count());
    // 落盘水位 = floor(C3):产物只承诺覆盖扫描开始时刻的磁盘状态;窗口内
    // id>floor 的变更由下方 rescan_dirty 重放补入,但崩溃窗口不能依赖它。
    mount_scan_product(state, paths, idx, floor);
    log::info!(
        "重扫产物已挂载(条目数: {:?})",
        with_view(state, |v| live_count_of(v))
    );
    // 水位推进到覆盖点:重扫内容 = 截至 floor 的磁盘状态;floor 以下积压的陈旧
    // 事件(含触发本轮重扫的 dropped 标记)从此刻起被消费循环跳过。
    state.seen_event_id.fetch_max(floor, Ordering::Relaxed);
    state.applied_event_id.fetch_max(floor, Ordering::Relaxed);
    state.rescan_floor.fetch_max(floor, Ordering::Relaxed);
    // 待命核销:本轮重扫覆盖截至 floor 的磁盘状态,只有「丢弃 ID ≤ floor」的
    // 待命才被这轮覆盖;扫描窗口内新丢的事件(id > floor)保留,等下一轮。
    if state.rescan_debounce.lock_recover().covered_by(floor) {
        log::info!("本轮重扫已覆盖待命中的丢弃标记(floor={floor})");
    }
    // dir_map 已由 mount 与 Base 同点提交(勿在此二次 rebuild,否则重现窗口)
    // 重扫窗口追平:先清 rescanning 再 drain——此后消费线程 diff 的对象已是新
    // 索引,无需再记集合;窗口内已记目录基于旧索引 diff 过,重放追平。push 与
    // drain 各持自己的 Mutex;push 不持 holder 锁,但恒先于同次 diff 的 apply
    // (apply 持 holder 写锁,与替换段挂 holder 互斥)——「push 在 drain 前 →
    // 基于旧索引 → 被 drain 带走重放」或「push 在 drain 后 → 基于新索引 →
    // 不重放」,无丢失。
    state.rescanning.store(false, Ordering::SeqCst);
    let replayed: Vec<PathBuf> = state.rescan_dirty.lock_recover().drain().collect();
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
    *state.last_rescan.lock_recover() = Some(Instant::now());
    log::info!(
        "重扫完成(条目数: {:?})",
        with_view(state, |v| live_count_of(v))
    );
}

/// 扫描产物挂载(T4,tech.md §4.3):全量扫描 Index → 条目流直写落盘 →
/// remap mmap Base(Shared),不留百万级堆索引常驻——**首扫与重扫替换段共用**
/// (P0 迁移的同一逻辑,tech.md §1.6 预告的切换点)。
///
/// 可见性:重扫路径**不提前换 Base**——写盘与重载期间旧 Base 照常服务
/// (产品契约「重扫期间旧索引可查」),完成后写锁内把 holder 与 dir_map
/// **同点**替换为 mmap 新 Base(旧 map 只对旧号空间有效,提前换 Base 会让
/// 窗口内 diff 用旧号打新索引——毒 Base 事故的根因)。首扫路径复用在位
/// Mutable(holder 全程非 Empty),同样原子提交。写盘/重载失败:重扫以内存
/// 索引 + 其映射接管(必须换掉旧毒 Base);首扫保持 Mutable 并补建映射。
///
/// 锁序(tech.md §7 落盘串行):rescan_lock → flush_lock → holder,写+load 段
/// 整体在 flush_lock 内(与 flush_merged/persist_final 的 rename 互斥,防交错
/// rename 后把陈旧快照 mmap 成 Base);无反向获取点,无环。写盘期间持 holder
/// 读锁(内容与形态冻结,查询不受阻,compact 同样拿不到写锁);放读 → 取写的
/// 窗口由**形态复核**兜住:diff 会把 Mutable 转 Overlaid(变体变更)→ 放弃
/// swap,产物已落盘,增量交 flush 合并收敛。
///
/// 同变体内容变更只剩两处 µs 级窗口(放读锁 → 取写锁之间):
/// - `handle_diff_error` 的 ENOENT 摘除——摘除对象磁盘已不存在,父目录下次
///   diff 幂等重墓碑,最终一致;
/// - `maybe_compact` 的 Mutable 臂原地 compact——对扫描产物不可达:新产物零
///   墓碑/零孤儿字节,`needs_compaction` 恒假;且其写锁与本函数读锁互斥,
///   读锁段内无法运行。
///
/// swap 落地 Shared 即清 dirty:盘面 = 内存真相(flush 的「Shared 态 dirty==0」
/// 前提;重扫路径后续 drain 重放会重新累积)。meta 的落盘水位取**扫描开始时的
/// floor**(C3):扫描期间消费线程仍可能把 id>floor 的事件 diff 进旧 Base 并推进
/// applied,但产物内容不含它们——写当前 applied 会让重启跳过这些事件。取 floor
/// 只会多重放(幂等),绝不漏。
fn mount_scan_product(state: &EngineState, paths: &DaemonPaths, mut idx: Index, watermark: u64) {
    let literal_roots = state.roots.lock_recover().clone();
    let _flush_guard = state.flush_lock.lock_fail_fast("flush_lock");
    let n = idx.entry_count();
    let meta = build_meta_at(state, watermark);
    let layout = persist::EntryLayout::of(&idx);
    let written = persist::write_atomic(&layout, idx.entries.iter(), &meta, &paths.index_file())
        .map_err(|e| log::error!("扫描产物落盘失败,以内存索引接管: {e}"))
        .is_ok();
    if written {
        match persist::load_mmap(&paths.index_file()) {
            Ok(view) => {
                // 锁外构建映射(旧 Base 继续服务),写锁内与 holder 同点提交
                let map = build_dir_map(view.as_ref(), &literal_roots);
                install_holder_with_map(state, IndexHolder::Shared(view), map);
                state.dirty.store(0, Ordering::Relaxed);
                // 扫描堆索引已让位 mmap:先清空保容量并把空页标可回收再 drop,
                // 否则百万级 entries/arena 会滞留成不可折叠死页(单次重扫实测
                // ~150MB)。write_atomic 的内部整文件缓冲同样在写完后打标。
                idx.reset();
                idx.release_pages();
                drop(idx);
                engine::malloc_pressure::release_heap_pressure();
                log::info!("扫描产物已落盘并 remap 为 mmap Base(条目数: {n})");
                return;
            }
            Err(e) => log::error!("扫描产物重载失败,以内存索引接管: {e}"),
        }
    }
    // 落盘失败:扫描产物仍是全量干净态,以内存索引 + 其映射原子接管(旧毒 Base
    // 必须被替换——heal 的目标是「彻底重建」,不是「保留旧态」)
    let map = build_dir_map(&idx, &literal_roots);
    install_holder_with_map(state, IndexHolder::Mutable(idx), map);
    state.dirty.store(0, Ordering::Relaxed);
    log::warn!("扫描产物未能落盘,已以内存索引接管(条目数: {n})");
}

/// 在位落盘(holder 已持 Mutable 产物;首扫直接复用流水线挂载,无 Empty 观察
/// 窗):flush_lock 内写盘 → 重载 → 形态复核 swap Shared。
/// 锁序与窗口语义见 [`mount_scan_product`]。
fn mount_mutable_in_place(state: &EngineState, paths: &DaemonPaths) {
    let literal_roots = state.roots.lock_recover().clone();
    let _flush_guard = state.flush_lock.lock_fail_fast("flush_lock");
    let written = {
        let h = state.holder.read_fail_fast("holder");
        let IndexHolder::Mutable(idx) = &*h else {
            return; // 已被转 Overlaid(理论不可达:刚挂的 Mutable);flush 兜底
        };
        let meta = build_meta(state);
        let layout = persist::EntryLayout::of(idx);
        persist::write_atomic(&layout, idx.entries.iter(), &meta, &paths.index_file())
            .map_err(|e| log::error!("扫描产物落盘失败,保持内存索引: {e}"))
            .is_ok()
    };
    let mut swapped = false;
    if written {
        match persist::load_mmap(&paths.index_file()) {
            Ok(view) => {
                let map = build_dir_map(view.as_ref(), &literal_roots);
                let mut h = state.holder.write_fail_fast("holder");
                if let IndexHolder::Mutable(_) = &*h {
                    // holder + dir_map 同点替换(见 install_holder_with_map);
                    // 旧堆索引清空保容量 + 空页标可回收后再 drop(见
                    // mount_scan_product 同注:百万级 entries/arena 直接释放会
                    // 在 malloc Large 区留下不可折叠死页)。
                    let old = std::mem::replace(&mut *h, IndexHolder::Shared(view));
                    if let IndexHolder::Mutable(mut old_idx) = old {
                        old_idx.reset();
                        old_idx.release_pages();
                    }
                    *state.dir_map.lock_fail_fast("dir_map") = map;
                    swapped = true;
                }
                // 形态已变(Overlaid):盘面已是本产物,增量交 flush 合并收敛
            }
            Err(e) => log::error!("扫描产物重载失败,保持内存索引: {e}"),
        }
    }
    if swapped {
        state.dirty.store(0, Ordering::Relaxed);
        // 索引已让位 mmap(不计 footprint),扫描与产物堆的脏页在此归还
        // (S2「死页归还」的同构落地点;实测 60 万条首扫滞留 ~60MB 不自行回落)
        engine::malloc_pressure::release_heap_pressure();
        log::info!("扫描产物已落盘并 remap 为 mmap Base");
        return;
    }
    // 未换基(落盘/重载失败,或形态已变):确保 dir_map 与当前 holder 一致
    // ——首扫路径此前从未建过 map,cold_start 不再单独 rebuild。
    let map = {
        let h = state.holder.read_fail_fast("holder");
        match &*h {
            IndexHolder::Mutable(idx) => build_dir_map(idx, &literal_roots),
            _ => return,
        }
    };
    *state.dir_map.lock_fail_fast("dir_map") = map;
}

/// Overlaid{mmap Base, 空 Overlay} → Shared:刚写盘的 Base 即盘上真相,直挂免
/// 全量合并写(重扫替换段的窗口无变更常态路径)。写锁内的 Empty 瞬态无观察者
/// (与 ensure_overlay_ready 同一手法);Heap Base 不转(盘上内容与之无对应保证)。
/// 窗口有变更(Overlay 非空)则不动,交 flush 合并。落地 Shared 即清 dirty
/// (盘上即当前真相;否则 Shared 臂 flush 无操作且 dirty 滞留会让 should_flush
/// 恒真,消费循环空转)。
fn collapse_empty_overlay(state: &EngineState) {
    let mut h = state.holder.write_fail_fast("holder");
    let taken = std::mem::replace(&mut *h, IndexHolder::Empty);
    let (landed, collapsed) = match taken {
        IndexHolder::Overlaid(st) if st.overlay.is_empty() => match st.base {
            BaseRef::Mmap(v) => (IndexHolder::Shared(v), true),
            base => (
                IndexHolder::Overlaid(Box::new(OverlaidState {
                    base,
                    overlay: st.overlay,
                })),
                false,
            ),
        },
        other => (other, false),
    };
    *h = landed;
    drop(h);
    if collapsed {
        state.dirty.store(0, Ordering::Relaxed);
    }
}

/// 丢弃去抖的触发判定:待命 + 事件安静(或待命超上限)+ 冷却已过 + 无在途重扫。
/// 每轮消费循环调用一次(≤150ms),风暴期间只登记不执行。
///
/// **不在此处清待命**:重扫线程抢锁可能失败(在途重扫的 drain/flush 段仍在跑),
/// 提前清空会永久丢掉这条丢弃信息。核销由 `degrade_and_rescan` 在提交水位
/// floor 后按「丢弃 ID ≤ floor」完成;抢锁失败时待命保留,由
/// `attempt_allowed` 限制重试频率。
fn maybe_fire_debounced_rescan(state: &Arc<EngineState>, paths: &DaemonPaths) {
    let rescanning = state.rescanning.load(Ordering::SeqCst);
    let now = Instant::now();
    let cooldown_ok = state
        .last_rescan
        .lock_recover()
        .is_none_or(|t| now.duration_since(t) >= RESCAN_COOLDOWN);
    let mut d = state.rescan_debounce.lock_recover();
    let due = d.due(now, RESCAN_QUIET, RESCAN_MAX_WAIT);
    // 只有到期才消耗重试窗口;未到期不得推进 last_attempt。
    let attempt_ok = !due || d.attempt_allowed(now, RESCAN_ATTEMPT_GAP);
    let allowed = rescan_fire_allowed(rescanning, cooldown_ok, due, attempt_ok);
    drop(d);
    if !allowed {
        return;
    }
    log::info!("丢弃后事件已安静(或待命超上限),执行去抖重扫");
    spawn_rescan(state, paths);
}

/// 触发条件组合(纯函数,便于突变测试):无在途重扫、冷却已过、已到期、
/// 重试窗口允许。任一为假都不得触发。
fn rescan_fire_allowed(rescanning: bool, cooldown_ok: bool, due: bool, attempt_ok: bool) -> bool {
    !rescanning && cooldown_ok && due && attempt_ok
}

/// 自动源的重扫入口:后台线程执行(不阻塞消费循环);锁被占用即放弃。
fn spawn_rescan(state: &Arc<EngineState>, paths: &DaemonPaths) {
    let st = state.clone();
    let p = paths.clone();
    spawn_guarded("rescan", move || degrade_and_rescan(&st, &p, false));
}

/// 记录持久失败目录:入负缓存(去重)+ gaps 记账(去重,上限 256 条)。
/// 返回是否新入负缓存(调用方据此决定是否留日志,避免重复刷屏)。
fn record_persistent_failure(state: &EngineState, abs_dir: &Path, reason: &str) -> bool {
    let is_new = state
        .perm_denied
        .lock_recover()
        .insert(abs_dir.to_path_buf());
    if is_new {
        let mut gaps = state.gaps.lock_recover();
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

/// ENOENT 的常规收口:从被删目录逐级上溯到最近**已索引且存在**的祖先,对它做
/// 一次普通 diff。祖先 listing 里已无该子项 → Overlay 的删除方向逻辑屏蔽整棵
/// 子树(anchors 摘除 + purge_prefix)并摘除 dir_map 前缀键,与「父目录事件先到」
/// 完全同一语义,无需把 Base 物化成堆索引。返回变更加数;无已索引祖先(删除
/// 的是扫描根)返回 None,由调用方走物化兜底。
fn mask_deleted_via_parent(
    state: &EngineState,
    base: &BaseRef,
    overlay: &mut engine::overlay::Overlay,
    map: &mut DirIndexMap,
    abs_dir: &Path,
    excludes: &[std::path::PathBuf],
) -> Option<u32> {
    let mut cur = abs_dir.to_path_buf();
    loop {
        let parent = cur.parent()?.to_path_buf();
        if parent == cur {
            return None;
        }
        cur = parent;
        let view = base.as_view();
        let Some(target) = resolve_target(map, overlay, &cur) else {
            continue; // 该级未索引(如刚被删的中间目录):继续向上
        };
        // C2:resolve_target 只查 dir_map,不做判活——陈旧 map 窗口下 Base 号可能
        // 越界/指向非目录,apply_diff 内部 entry()/children_of() 会直接 panic。
        // 与 apply_and_track 复用同一判活校验;失配按键失效处理(清键,继续上溯,
        // 由更高层 listing 的一次屏蔽兜底)。
        if let engine::overlay::DiffTarget::Base(d0) = target {
            if !base_dir_target_live(view, d0) {
                map.remove(&cur);
                continue;
            }
        }
        match overlay.apply_diff(view, target, &cur, excludes) {
            Ok(stats) => {
                let mut changed = stats.added + stats.removed;
                // 与 apply_and_track 的寻址面维护同构:祖先 diff 后补登其 base
                // 目录子项键(随后 remove_prefix 清掉被删子树)。
                if let engine::overlay::DiffTarget::Base(d0) = target {
                    for k in view.children_of(d0) {
                        if view.entry(k).is_dir() {
                            let mut p = cur.clone();
                            p.push(String::from_utf8_lossy(view.display_name(k)).into_owned());
                            map.insert(&p, k);
                        }
                    }
                }
                let gone = map.remove_prefix(abs_dir);
                changed += gone;
                state.dirty.fetch_add(changed as u64, Ordering::Relaxed);
                return Some(changed);
            }
            // 祖先本身也已被删:继续上溯(整链由更高层的 listing 一次屏蔽)。
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }
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
    excludes: &[std::path::PathBuf],
    e: &std::io::Error,
) {
    match e.kind() {
        // NotADirectory:事件意外指向文件(目录级事件下不应出现,防御旧行为/异常
        // 流)。处置与 NotFound 相同——对父目录做一次普通 diff,listing 覆盖该
        // 文件/子树的增删;这样事件路径无需逐条 stat 也能安全喂给 diff_one。
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => {
            // 常规形态:被删目录仍有已索引祖先。对最近祖先做一次**普通 diff**
            // 即可——父目录 listing 里已无该子项,Overlay 按既有语义把整棵
            // Base 子树屏蔽(anchors/overlaid 账本),查询与合并写自然不再收录。
            // 这避免了为一次删除把整份 Base 物化成堆索引(实机 145 万条目
            // ≈150MB 堆,flush 换基后整块变不可折叠死页)的旧路径。
            if mask_deleted_via_parent(state, base, overlay, map, abs_dir, excludes).is_some() {
                return; // 变更加入 dirty 已由 helper 记账
            }
            // 兜底:删除的是扫描根本身(无已索引祖先可借 listing 屏蔽)——此时
            // 只能物化 Base 写墓碑(根级删除低概率,且条目号 1:1)。
            base.ensure_mutable();
            let BaseRef::Heap(idx) = base else {
                unreachable!("ensure_mutable 后必为 Heap")
            };
            let removed = engine::scanner::remove_prefix(idx, map, abs_dir);
            let view: &dyn IndexView = idx;
            overlay.purge_prefix(view, abs_dir);
            // 诊断+自愈:路径前缀清扫可能漏掉指向已 tombstone base 的桥(自愈
            // 兜底,并留痕供归因——这里是「anchor 的 base 目录未留存」的来源)。
            let stale = overlay.sweep_stale_anchors(view);
            if stale > 0 {
                log::warn!(
                    "ENOENT 摘除 {} 后清扫 {stale} 个残留桥(指向已墓碑 base 的 anchor)",
                    abs_dir.display()
                );
            }
            state.dirty.fetch_add(removed as u64, Ordering::Relaxed);
        }
        std::io::ErrorKind::PermissionDenied => {
            if record_persistent_failure(
                state,
                abs_dir,
                "权限拒绝(如无完全磁盘访问权限),已停止重试;重启后自动重新索引",
            ) {
                // 每目录只入负缓存一次,但 TCC 保护目录可能一次事件风暴扫出成百个
                // 子目录(实机 Photos Library);按 60s 窗口合并,保留一个样本路径。
                log_throttled!(
                    warn,
                    "diff-perm",
                    Duration::from_secs(60),
                    "目录 diff 权限拒绝 {},入负缓存",
                    abs_dir.display()
                );
            }
        }
        _ => {
            let mut counts = state.diff_fail_counts.lock_recover();
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
    // serve 已在接受连接前完成加载/迁移；此处只取快照，避免重读覆盖并发偏好写。
    let cfg = state.cfg.lock_recover().clone();
    // 外接卷(external-volumes):挂载中的期望卷并入初始 roots——落盘索引路径
    // 由此获得流覆盖(停机窗口重放追平卷内漂移);首扫路径一并纳入首扫。
    let desired_volumes = volumes::desired_roots(&cfg);
    for v in &desired_volumes {
        log::info!("启动挂载卷并入扫描根: {}", v.display());
    }
    let desired_set: std::collections::HashSet<PathBuf> = desired_volumes.iter().cloned().collect();
    let mut roots = default_roots()?;
    roots.extend(desired_volumes.iter().cloned());
    *state.roots.lock_recover() = roots.clone();
    let excludes = reconcile::expand_excludes(&state.cfg.lock_recover().excluded);
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
            // 索引健全性抽检:历史版本 bug 产物(路径错挂)的主动判毒。放在重放
            // **之前**——毒 Base 上重放会把数万事件逐个错挂、合并不报错但垃圾被
            // 反复收编(实机 churn 十余分钟)。判毒即后台全量重扫(重扫期间旧
            // Base 仍可查,产物就位后原子换基)。
            let (missing, sampled) = index_path_missing_sample(view.as_ref());
            if is_poison_sample(missing, sampled) {
                log::warn!(
                    "索引健全性抽检: {missing}/{sampled} 条路径不存在,判索引中毒(历史版本产物),转全量重扫彻底重建"
                );
                state.set_state(state::DEGRADED);
                spawn_rescan(&state, &paths);
            }
            *state.holder.write_fail_fast("holder") = IndexHolder::Shared(view);
            state.set_state(state::RECONCILING);
            rebuild_dir_map(&state);
            // 启动清除:上次会话的卷前缀中,本次未挂载(或已停用)的移除——
            // 落盘索引不重扫,脏卷条目无自然消亡机制,必须在建流/就绪前清掉。
            // (有待清卷时内部按需转可变;无卷变更则保持 Shared 常驻。)
            volumes::purge_unmounted(&state, &paths, &desired_set);
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
            *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(index);
            let (recs, table, gaps) = engine::scanner::collect_records(&roots, &excludes, threads);
            state.built_at.store(now_secs(), Ordering::Relaxed);
            let mut gap_list = invalid_roots_gaps(&roots);
            gap_list.extend(gaps.permission.iter().map(|(p, r)| Gap {
                path: p.to_string_lossy().into_owned(),
                reason: r.clone(),
            }));
            *state.gaps.lock_recover() = gap_list;
            state.io_errors.fetch_add(gaps.io_errors, Ordering::Relaxed);
            for chunk in recs.chunks(SCAN_CHUNK) {
                let mut h = state.holder.write_fail_fast("holder");
                if let IndexHolder::Mutable(idx) = &mut *h {
                    engine::scanner::apply_records(idx, &mut staging_map, chunk, &table);
                }
            }
            state.applied_event_id.store(water, Ordering::Relaxed);
            state.seen_event_id.store(water, Ordering::Relaxed);
            // T4:首扫产物同样收敛为 mmap Base(与重扫替换段共用 mount 的在位
            // 路径,tech.md §4.3)——不留堆索引常驻;此刻水位已含 water,落盘 meta
            // 一致。holder 自 apply 循环起恒为 Mutable,在位落盘无 Empty 观察窗。
            // mount 已把 dir_map 与 Base 同点提交;Shared 态 flush 为无操作
            mount_mutable_in_place(&state, &paths);
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
    // 会话边界行:日志带时间戳后,靠这一行切分「哪段属于哪次启动、重放自哪个水位」。
    log::info!(
        "daemon 启动: pid={} version={} data_dir={} roots={:?} since={} replayed_from_disk={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION"),
        paths.data_dir.display(),
        roots_str,
        last_event_id,
        replayed_from_disk,
    );
    let (stream, rx) = match fsevents::start_stream(
        &roots_str.iter().map(String::as_str).collect::<Vec<_>>(),
        last_event_id.max(1),
        0.1,
        // 目录级事件(不设 FileEvents):事件路径即变更所在目录,正是目录 diff
        // 管线需要的粒度。注意它**不减少事件条数**(实测 8 万次文件操作 ≈ 8 万条
        // 事件);风暴下的丢弃由回调热路径优化 + 去抖重扫兜底,别指望这个 flag。
        fsevents::DIR_EVENTS,
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
    // 消费线程是 daemon 的索引心跳:panic 若只死线程,进程仍应答 socket 但索引
    // 永不更新(静默返回旧索引)。看护层记录后 exit(1),由 GUI 存活监控拉起。
    spawn_guarded("consume-events", move || {
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
        spawn_guarded("volume-monitor", move || volumes::monitor_loop(st, p));
    }
    Ok(())
}

/// 处理一条连接。返回值语义:
/// - `Ok(true)`:本条连接处理的是 **Stop**(调用方负责退出进程);
/// - `Ok(false)`:其他请求,或对端在发请求前断开/超时(静默关闭);
/// - `Err`:真正的处理失败(仅非 Stop 路径;Stop 的响应写失败只留痕,仍须退出)。
///
/// Stop 的响应写失败也不能阻止退出:最终落盘已完成,若进程继续挂着
/// (stopping 已置位),CLI 收不到应答、socket 已删,daemon 会变成僵尸。
fn handle_conn(
    stream: &mut UnixStream,
    state: &Arc<EngineState>,
    paths: &DaemonPaths,
) -> Result<bool> {
    // 每步 io 带阶段标注:线上的「Invalid argument (os error 22)」无法从错误串
    // 定位 syscall,阶段化后一次出现即可锁定(read 长度/读体/写响应)。
    // EINVAL 定位(实测):对端在 accept 前已关闭(客户端 connect 后立即断开)时,
    // macOS 的 setsockopt(SO_RCVTIMEO) 返回 EINVAL——保持连接的客户端 0/100,
    // 即刻断开 257/300。这类连接注定读不到请求,静默关闭即可,不是错误。
    if let Err(e) = stream.set_read_timeout(Some(Duration::from_secs(5))) {
        if e.kind() == std::io::ErrorKind::InvalidInput {
            log::debug!("对端在 accept 前已关闭,连接直接关闭(SO_RCVTIMEO EINVAL)");
            return Ok(false);
        }
        return Err(e).context("设置读超时");
    }
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf) {
        Ok(()) => {}
        Err(e) if peer_gone(&e) => {
            // 健康探测/客户端在请求前断开或读超时:不是 daemon 故障。
            // 以 WARN 记录曾实测约 1.7 万行/天(5s 探测 × 全天)。
            log::debug!("连接在请求前关闭/超时({:?}),静默关闭", e.kind());
            return Ok(false);
        }
        Err(e) => return Err(e).context("读取消息长度"),
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > protocol::MAX_MSG {
        bail!("消息超限({len} 字节)");
    }
    let mut buf = vec![0u8; len];
    match stream.read_exact(&mut buf) {
        Ok(()) => {}
        Err(e) if peer_gone(&e) => {
            log::debug!("连接在读消息体时关闭/超时({:?}),静默关闭", e.kind());
            return Ok(false);
        }
        Err(e) => return Err(e).with_context(|| format!("读取消息体({len} 字节)")),
    }
    let req: Request = protocol::decode_request(&buf).context("解码请求失败")?;
    let is_stop = matches!(&req, Request::Stop { .. });
    let resp = dispatch(req, state, paths);
    let body = match encode_response(resp) {
        Ok(body) => body,
        Err(e) if is_stop => {
            // serde 编码小响应不会失败;真失败也不能让进程卡在 stopping 态。
            log::error!("Stop 响应编码失败(落盘已完成,仍退出): {e}");
            return Ok(true);
        }
        Err(e) => return Err(e).context("编码响应失败"),
    };
    let wrote = (|| -> Result<()> {
        stream
            .write_all(&(body.len() as u32).to_le_bytes())
            .context("写出响应长度")?;
        stream.write_all(&body).context("写出响应体")?;
        stream.flush().context("冲刷响应")?;
        Ok(())
    })();
    if is_stop {
        if let Err(e) = wrote {
            log::warn!("Stop 响应写出失败(落盘已完成,仍退出): {e:#}");
        }
        return Ok(true);
    }
    wrote?;
    Ok(false)
}

/// 连接线程的退出权限(纯函数,单测钉住 I1):**只有处理了 Stop 的连接**可以
/// 退出进程。`_stopping` 故意不参与判定——旧实现按全局状态判断,任意探测/查询
/// 连接恰逢 Stop 就会 `exit(0)`,截断 Stop 线程正在做的 `persist_final`
/// (大索引数百 ms)并吞掉 Stop 应答。保留该参数以便测试复现旧判据。
fn should_exit_process(handled_stop: bool, _stopping: bool) -> bool {
    handled_stop
}

/// 对端在请求完成前断开/超时(EOF、reset、SO_RCVTIMEO 到期):属于连接事件,
/// 不是 daemon 处理失败,不应刷 WARN。
fn peer_gone(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::WouldBlock
    )
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
            dir_only,
            include_hidden,
            v: _,
        } => run_query(
            state,
            &query,
            limit,
            sort.as_deref(),
            desc.unwrap_or(false),
            dir_only.unwrap_or(false),
            include_hidden,
        ),
        Request::Status { .. } => {
            let name = *state.state_name.lock_recover();
            let entry_count = status_entry_count(state);
            Response::Status {
                state: name.into(),
                entry_count,
                gaps: state.gaps.lock_recover().clone(),
                io_error_count: state.io_errors.load(Ordering::Relaxed),
                built_at: state.built_at.load(Ordering::Relaxed),
                index_file: paths.index_file().to_string_lossy().into_owned(),
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            }
        }
        // 存活探测:立即回,不触碰索引(Status 的 entry_count 是 O(条目数))。
        Request::Ping { .. } => Response::Pong,
        Request::Stop { .. } => {
            state.stopping.store(true, Ordering::SeqCst);
            // C3:Stop 落盘与重扫串行(rescan_lock → flush_lock,与
            // degrade_and_rescan 同序)。重扫在途时,新 Base 已按 floor 落盘但
            // 窗口事件尚在 rescan_dirty 待重放 / 待合并;此刻 persist_final 可能
            // 把「扫描产物 + 超前 applied 水位」写进盘面(或反之),留下重启后
            // 无法自愈的缺口。等一整轮重扫(含重放与最终 flush)结束再落盘。
            let _rescan = acquire_rescan_blocking(&state.rescan_lock);
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
            excluded: state.cfg.lock_recover().excluded.clone(),
            message: None,
        },
        Request::ExcludeAdd { path, .. } => exclude_response(state, paths, &path, ExcludeOp::Add),
        Request::ExcludeRemove { path, .. } => {
            exclude_response(state, paths, &path, ExcludeOp::Remove)
        }
        Request::ExcludeReset { .. } => reset_excludes(state, paths),
        Request::VolumesSet { auto, excluded, .. } => volumes_set(state, paths, auto, excluded),
        Request::VolumesPrefs { .. } => volumes_prefs_response(state),
        Request::HiddenSet { include, .. } => hidden_set(state, paths, include),
        Request::HiddenPrefs { .. } => hidden_prefs_response(state),
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
        let mut c = state.cfg.lock_recover();
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
    let c = state.cfg.lock_recover();
    Response::VolumesPrefs {
        auto: c.auto_index_volumes,
        excluded: c.volumes_excluded.clone(),
        message: None,
    }
}

/// 隐藏项偏好写入:更新内存 + 落盘;保存失败仍临时生效并如实告知。
fn hidden_set(state: &Arc<EngineState>, paths: &DaemonPaths, include: bool) -> Response {
    let message = {
        let mut c = state.cfg.lock_recover();
        c.include_hidden = Some(include);
        match config::save(&c, &paths.config_file()) {
            Ok(()) => None,
            Err(e) => {
                log::error!("隐藏项偏好落盘失败: {e}");
                Some(format!("已临时生效但写入配置失败: {e}"))
            }
        }
    };
    log::info!("隐藏项偏好更新:include={include}");
    Response::HiddenPrefs { include, message }
}

/// 隐藏项偏好读(设置页回显)。
fn hidden_prefs_response(state: &Arc<EngineState>) -> Response {
    Response::HiddenPrefs {
        include: state.cfg.lock_recover().include_hidden_effective(),
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
    let excluded = || state.cfg.lock_recover().excluded.clone();
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
/// `cfg` 锁覆盖「读-改-写」全程(HiddenSet/volumes_set 不得插入),重扫前释放;
/// 成功后一次全量重扫使默认排除项同时生效。
fn reset_excludes(state: &Arc<EngineState>, paths: &DaemonPaths) -> Response {
    let _op = state.exclude_op_lock.lock_recover();
    // 持 cfg 锁跨越读-改-写:HiddenSet/volumes_set 等配置写者不得插入本序列,
    // 否则重置会用陈旧默认值覆盖并发写入的搜索偏好。
    let mut cfg = state.cfg.lock_recover();
    let mut defaults = config::default_config();
    defaults.include_hidden = cfg.include_hidden;
    if let Err(e) = config::save(&defaults, &paths.config_file()) {
        log::error!("恢复默认排除目录:配置保存失败: {e}");
        return Response::Exclude {
            ok: false,
            excluded: cfg.excluded.clone(),
            message: Some(format!("配置保存失败({e}),排除目录未改变")),
        };
    }
    *cfg = defaults.clone();
    drop(cfg); // 不放锁调用重扫(重扫内部会取 cfg)
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
    let _op = state.exclude_op_lock.lock_recover();
    let roots = state.roots.lock_recover().clone();
    if matches!(&op, ExcludeOp::Add) {
        validate_exclude_in_roots(path, &roots)?;
    }
    // 持 cfg 锁跨越内存变更与落盘：HiddenSet/VolumesSet 不得在克隆→保存窗口
    // 插入并被陈旧副本覆盖（配置写者统一纪律）。
    let mut cfg = state.cfg.lock_recover();
    match op {
        ExcludeOp::Add => {
            if !cfg.excluded.iter().any(|e| e == path) {
                cfg.excluded.push(path.to_string());
            }
        }
        ExcludeOp::Remove => {
            if !cfg.excluded.iter().any(|e| e == path) {
                return Err(format!("条目不存在: {path}"));
            }
            cfg.excluded.retain(|e| e != path);
        }
    }
    if let Err(e) = config::save(&cfg, &paths.config_file()) {
        log::error!("配置保存失败: {e}");
        return Err(format!("配置保存失败({e}),变更未持久化,重启后将丢失"));
    }
    drop(cfg); // 不放锁调用重扫（重扫内部会取 cfg）
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
    // 上次会话崩溃/强杀残留的索引临时文件(tmp 与溢写段):与目标同目录、前缀
    // 精确匹配,启动即清扫(实机曾见 40MB 残留)。
    persist::cleanup_stale_index_tmp(&paths.data_dir);
    // 后台线程 panic 默认只写 stderr(随启动方式可能丢失)——统一经 log 留痕,
    // 排障时不再「静默死线程」。hook 只做诊断,退出由 spawn_guarded 的
    // catch_unwind 负责;hook 自身包 catch_unwind,避免诊断路径反过来阻断退出。
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            log::error!("线程 panic: {info}");
        }));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| default_hook(info)));
    }));
    let listener = bind_single_instance(&paths.socket())?;
    let state = Arc::new(EngineState::new());
    // 接受连接前完成配置读取/迁移：查询与偏好写都依赖 cfg，不能把 None/旧值
    // 暴露给冷启动窗口，也不能让 HiddenSet/reset 在迁移赋值前落地。
    *state.cfg.lock_recover() =
        config::load_migrating(&paths.config_file(), paths.index_file().exists());
    {
        let st = state.clone();
        let p = paths.clone();
        spawn_guarded("cold-start", move || {
            if let Err(e) = cold_start(st, p) {
                log::error!("冷启动失败: {e:#}");
            }
        });
    }
    // 不在 stopping 时 break:Stop 的最终落盘在连接线程里同步进行,主循环若因
    // 探测连接恰好到达而 break → serve 返回 → 进程退出会截断 persist_final
    // (索引落盘丢失、Stop 应答写不出去)。退出统一由**处理 Stop 的线程**在
    // 响应完成后执行;探测/查询线程即使与 Stop 并发也绝无退出权限。
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let st = state.clone();
        let p = paths.clone();
        spawn_guarded("conn", move || match handle_conn(&mut stream, &st, &p) {
            Ok(handled_stop) => {
                let stopping = st.stopping.load(Ordering::SeqCst);
                if should_exit_process(handled_stop, stopping) {
                    std::process::exit(0);
                }
            }
            Err(e) => log_throttled!(
                warn,
                "conn-error",
                Duration::from_secs(60),
                "连接处理失败: {e:#}"
            ),
        });
    }
    Ok(())
}

/// 日志文件上限与备份数:daemon.log + daemon.log.1..3。5MB × 4 是取证够用的
/// 窗口——旧实现无轮转,实机跑出 28MB 单文件且无时间戳,无法区分会话与阶段。
const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
const LOG_BACKUPS: usize = 3;
/// 相同消息的去重窗口:窗口内重复只计数,窗口到期或换消息时补一行「重复 N 次」。
/// 只对**完全相同**的消息生效;带路径/计数的消息由调用点用 `log_throttled!` 限频。
const LOG_DEDUP_WINDOW: Duration = Duration::from_secs(10);

/// 日志行时间戳(本地时区,毫秒)。`localtime_r` 线程安全;调用频率与日志量同级,
/// 不在热路径。
fn now_stamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    // SAFETY:localtime_r 只写调用方提供的 tm,不返回内部指针;tm 全零初始化,
    // 失败时保持零值(格式仍合法,只是年份为 1900)。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY:同上;`secs` 为 time_t 可表示范围内的当前秒数。
    unsafe { libc::localtime_r(&(now.as_secs() as libc::time_t), &mut tm) };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        now.subsec_millis()
    )
}

/// 相同消息折叠器:窗口内**同级别 + 同正文**的重复消息只计数;放下一条不同
/// 消息时返回累计抑制数与被折叠消息的级别(补报行沿用原级别,避免归因错误)。
#[derive(Default)]
struct RepeatFilter {
    last: Option<(log::Level, String, Instant)>,
    suppressed: u32,
}

enum Emit {
    /// 本行被折叠,不写。
    Skip,
    /// 写出本行;`suppressed` > 0 表示上一条消息被折叠过这么多次,
    /// `folded_level` 是被折叠消息的级别(用于补报行)。
    Write {
        suppressed: u32,
        folded_level: Option<log::Level>,
    },
}

impl RepeatFilter {
    fn observe(&mut self, level: log::Level, body: &str, now: Instant) -> Emit {
        if let Some((prev_level, prev, at)) = &self.last {
            if *prev_level == level && prev == body && now.duration_since(*at) < LOG_DEDUP_WINDOW {
                self.suppressed = self.suppressed.saturating_add(1);
                return Emit::Skip;
            }
        }
        let suppressed = std::mem::take(&mut self.suppressed);
        let folded_level = if suppressed > 0 {
            self.last.as_ref().map(|(l, _, _)| *l)
        } else {
            None
        };
        self.last = Some((level, body.to_string(), now));
        Emit::Write {
            suppressed,
            folded_level,
        }
    }
}

/// 日志文件写侧:按大小轮转;打开失败降级为 stderr-only(不致命)。
struct FileSink {
    path: PathBuf,
    file: Option<std::fs::File>,
    bytes: u64,
    /// 下一次尝试轮转的字节阈值。正常等于 `limit`;轮转失败时后移一个窗口,
    /// 避免每一行都重试 rename(见 `append`)。
    next_rotate_at: u64,
    limit: u64,
    repeat: RepeatFilter,
}

impl FileSink {
    fn new(path: PathBuf) -> Self {
        Self::with_limit(path, LOG_MAX_BYTES)
    }

    fn with_limit(path: PathBuf, limit: u64) -> Self {
        let file = open_log_file(&path);
        let bytes = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Self {
            path,
            file,
            bytes,
            next_rotate_at: limit,
            limit,
            repeat: RepeatFilter::default(),
        }
    }

    fn append(&mut self, line: &str) {
        if self.bytes >= self.next_rotate_at {
            // 先关句柄再改名;轮转失败不丢当前行:重开原文件继续追加。
            self.file = None;
            let rotated = rotate_log(&self.path).is_ok();
            self.file = open_log_file(&self.path);
            if rotated {
                self.bytes = 0;
                self.next_rotate_at = self.limit;
            } else {
                // 轮转失败(目录只读、备份位被占等):计数不得归零,否则实际
                // 文件会涨到 2×limit 才再尝试;后移一个窗口后重试。
                self.next_rotate_at = self.bytes.saturating_add(self.limit);
            }
        }
        if let Some(f) = &mut self.file {
            if f.write_all(line.as_bytes()).is_ok() {
                self.bytes = self.bytes.saturating_add(line.len() as u64);
            }
        }
    }
}

/// 备份路径:daemon.log → daemon.log.1(整体追加后缀,不替换扩展名)。
fn backup_path(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// 滚动:daemon.log.2 → .3、.1 → .2、daemon.log → .1,最老的丢弃。
fn rotate_log(path: &Path) -> std::io::Result<()> {
    for i in (1..LOG_BACKUPS).rev() {
        let from = backup_path(path, i);
        let to = backup_path(path, i + 1);
        if from.exists() {
            let _ = std::fs::remove_file(&to);
            std::fs::rename(&from, &to)?;
        }
    }
    let first = backup_path(path, 1);
    let _ = std::fs::remove_file(&first);
    if path.exists() {
        std::fs::rename(path, &first)?;
    }
    Ok(())
}

/// 键控限频:同一键在窗口内只放行一次,放行时返回窗口内累计抑制数。
struct ThrottleMap {
    entries: HashMap<&'static str, (Instant, u32)>,
}

impl ThrottleMap {
    fn allow(&mut self, key: &'static str, window: Duration, now: Instant) -> Option<u32> {
        match self.entries.get_mut(key) {
            Some((last, suppressed)) if now.duration_since(*last) < window => {
                *suppressed = suppressed.saturating_add(1);
                None
            }
            Some((last, suppressed)) => {
                let n = std::mem::take(suppressed);
                *last = now;
                Some(n)
            }
            None => {
                self.entries.insert(key, (now, 0));
                Some(0)
            }
        }
    }
}

fn throttle_allow(key: &'static str, window: Duration) -> Option<u32> {
    static THROTTLES: OnceLock<Mutex<ThrottleMap>> = OnceLock::new();
    let m = THROTTLES.get_or_init(|| {
        Mutex::new(ThrottleMap {
            entries: HashMap::new(),
        })
    });
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    g.allow(key, window, Instant::now())
}

/// 极简日志:stderr + 数据目录 daemon.log(不引日志框架)。每行带本地时间戳;
/// 文件超 5MB 轮转保留 3 份;完全相同的消息 10s 内折叠计数。
fn init_logger(paths: &DaemonPaths) {
    struct Dual(Mutex<FileSink>);
    impl log::Log for Dual {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::max_level()
        }
        fn log(&self, r: &log::Record) {
            if !self.enabled(r.metadata()) {
                return;
            }
            let body = format!("{}", r.args());
            let mut sink = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let Emit::Write {
                suppressed,
                folded_level,
            } = sink.repeat.observe(r.level(), &body, Instant::now())
            else {
                return;
            };
            if suppressed > 0 {
                // 补报行沿用**被折叠消息**的级别,而不是当前这条的级别。
                let note = format!(
                    "{} [{}] (上条消息在 {}s 内重复 {suppressed} 次)\n",
                    now_stamp(),
                    folded_level.unwrap_or(r.level()),
                    LOG_DEDUP_WINDOW.as_secs()
                );
                eprint!("{note}");
                sink.append(&note);
            }
            let line = format!("{} [{}] {body}\n", now_stamp(), r.level());
            eprint!("{line}");
            sink.append(&line);
        }
        fn flush(&self) {}
    }
    if log::set_boxed_logger(Box::new(Dual(Mutex::new(FileSink::new(paths.log_file()))))).is_ok() {
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
            let c = state.cfg.lock_recover();
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
            let mut map = state.dir_map.lock_fail_fast("dir_map");
            let key = dir.canonicalize().unwrap();
            map.insert(&key, root);
        }
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
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
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        // Shared 态建 dir_map(rebuild 改读 IndexView,不再要求 Mutable;
        // 突变「rebuild 仍只认 Mutable 提前返回」→ map 被清空,断言必红)
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);
        assert!(
            state
                .dir_map
                .lock_fail_fast("dir_map")
                .contains_key(&dir.path().canonicalize().unwrap()),
            "Shared 态 rebuild_dir_map 应建立根映射"
        );
        // Shared 态查询直接可服务(冷启动秒开语义)
        match run_query(&state, "a.txt", 10, None, false, false, None) {
            Response::Query { total, .. } => assert_eq!(total, 1, "Shared 态应可查"),
            other => panic!("意外响应: {other:?}"),
        }
        // 磁盘新增 c.txt:首个 diff 转 Overlaid(直写 Overlay,不物化 Base),增量照常生效
        std::fs::write(dir.path().join("c.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1, "首个 diff 应生效");
        {
            let h = state.holder.read_fail_fast("holder");
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
        match run_query(&state, "c.txt", 10, None, false, false, None) {
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
        *state.holder.write_fail_fast("holder") =
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
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        let st = state.clone();
        let writer = std::thread::spawn(move || {
            let _g = st.holder.write_fail_fast("holder"); // 短暂持写后释放
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
                let g1 = lock.read_recover(); // A 持读
                let w = {
                    let lock = Arc::clone(&lock);
                    std::thread::spawn(move || {
                        let _w = lock.write_recover(); // B 请求写 → 入队阻塞
                    })
                };
                std::thread::sleep(std::time::Duration::from_millis(200)); // 确保 B 入队
                let g2 = lock.read_recover(); // A 二次读 → 阻塞(平台若放行则返回)
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
            None,
        );
        assert_eq!(
            state.applied_event_id.load(Ordering::Relaxed),
            100,
            "批次应用后水位推进"
        );
        // T3:diff 写 Overlay,holder 转 Overlaid;status 近似计数扣掉桥 anchor
        // 后 = 根 + a.txt(被屏蔽 Base 条目不预枚举,一般情形为近似,此处恰精确)
        let h = state.holder.read_fail_fast("holder");
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
        apply_batch(&state, vec![], true, None);
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
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        let paths = DaemonPaths {
            data_dir: dir.path().join("data"),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        let before = fsevents::current_event_id();
        degrade_and_rescan(&state, &paths, true);
        let floor = state.rescan_floor.load(Ordering::Relaxed);
        assert!(floor >= before, "覆盖水位取重扫开始时刻");
        assert!(state.applied_event_id.load(Ordering::Relaxed) >= floor);
        assert_eq!(*state.state_name.lock_recover(), state::READY);
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
            let mut h = state.holder.write_fail_fast("holder");
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
            let resp = run_query(&state, "hit", 10, sort, false, false, None);
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

    /// dir_only 端到端钉(dispatch → unwrap_or → run_query → plan 置位):
    /// Some(true) 只回目录且 total 只计目录;None/Some(false) 与现状同义(不过滤)。
    /// 突变 run_query 的 `plan.dir_only = dir_only` 删除 → Some(true) 分支红;
    /// 突变 dispatch 的 `unwrap_or(false)` → `unwrap_or(true)` → None/Some(false) 分支红。
    #[test]
    fn dispatch_dir_only_returns_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("hit_dir")).unwrap();
        std::fs::write(dir.path().join("hit_file.txt"), b"x").unwrap();
        let state = mounted_state(dir.path());
        {
            let mut h = state.holder.write_fail_fast("holder");
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
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };

        for (dir_only, want_total, label) in [
            (Some(true), 1, "Some(true) 只回目录"),
            (None, 2, "None 不过滤"),
            (Some(false), 2, "Some(false) 不过滤"),
        ] {
            let resp = dispatch(
                Request::Query {
                    v: protocol::PROTOCOL_VERSION,
                    query: "hit".into(),
                    limit: 10,
                    sort: None,
                    desc: None,
                    dir_only,
                    include_hidden: None,
                },
                &state,
                &paths,
            );
            let Response::Query { entries, total, .. } = resp else {
                panic!("{label} 应返回查询响应: {resp:?}")
            };
            assert_eq!(total, want_total, "{label}: total 口径");
            assert_eq!(entries.len(), want_total as usize, "{label}: 条目数");
            if dir_only == Some(true) {
                assert!(
                    entries.iter().all(|e| e.is_dir),
                    "{label}: 不得回文件: {entries:?}"
                );
            }
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
        *state.roots.lock_recover() = vec![root];
        *state.cfg.lock_recover() = config::Config {
            excluded: excluded.iter().map(|e| (*e).to_string()).collect(),
            auto_index_volumes: true,
            volumes_excluded: vec![],
            include_hidden: None,
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
            state.cfg.lock_recover().excluded,
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
        config::save(&state.cfg.lock_recover(), &paths.config_file()).unwrap();
        let err = apply_exclude_change(&state, &paths, "/not/existing", ExcludeOp::Remove)
            .expect_err("不存在的条目应拒绝移除");
        assert_eq!(err, "条目不存在: /not/existing");
        assert_eq!(
            state.cfg.lock_recover().excluded,
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
    /// 并发 HiddenSet 的丢失更新由结构排除(cfg 锁覆盖读-改-写全程),不做易抖的
    /// 时序测试——本测只钉单线程语义。
    #[test]
    fn exclude_reset_restores_defaults_and_rescans_with_new_excludes() {
        let (_tmp, state, paths) = exclude_fixture(&["node_modules"]);
        let root = state.roots.lock_recover()[0].clone();
        // 自定义排除目录里放一个文件:重置前整棵子树被剪枝,重置后应被索引。
        let custom_dir = root.join("custom_excluded");
        std::fs::create_dir(&custom_dir).unwrap();
        let hidden_file = custom_dir.join("inside.txt");
        std::fs::write(&hidden_file, b"x").unwrap();
        let custom = custom_dir.to_string_lossy().into_owned();
        state.cfg.lock_recover().excluded.push(custom.clone());
        config::save(&state.cfg.lock_recover(), &paths.config_file()).unwrap();
        // 隐藏项开关须在「恢复默认」中保留(不得随默认配置被清空)。
        state.cfg.lock_recover().include_hidden = Some(true);
        config::save(&state.cfg.lock_recover(), &paths.config_file()).unwrap();

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

        let memory = state.cfg.lock_recover().excluded.clone();
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
        assert_eq!(
            state.cfg.lock_recover().include_hidden,
            Some(true),
            "恢复默认不得清空隐藏项开关(内存)"
        );
        assert_eq!(
            config::load(&paths.config_file()).include_hidden,
            Some(true),
            "恢复默认不得清空隐藏项开关(落盘)"
        );
        assert!(
            *state.state_name.lock_recover() == state::READY,
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
            state.cfg.lock_recover().excluded,
            vec!["/custom/x".to_string(), "node_modules".to_string()],
            "保存失败不得改动内存配置"
        );
        assert!(
            *state.state_name.lock_recover() == state::BUILDING,
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
        config::save(&state.cfg.lock_recover(), &paths.config_file()).unwrap();
        let sub = tmp.path().join("scan").join("sub");
        std::fs::create_dir(&sub).unwrap();
        let sub = sub.to_string_lossy().into_owned();

        // add:持锁时线程必须阻塞,释放后成功。
        let guard = state.exclude_op_lock.lock_recover();
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
        let guard = state.exclude_op_lock.lock_recover();
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
            state.cfg.lock_recover().excluded.is_empty(),
            "被拒绝的 Add 不得改动配置"
        );
        assert!(!paths.config_file().exists(), "被拒绝的 Add 不得落盘");

        let inside = tmp.path().join("scan").join("sub");
        std::fs::create_dir(&inside).unwrap();
        let inside = inside.to_string_lossy().into_owned();
        apply_exclude_change(&state, &paths, &inside, ExcludeOp::Add)
            .expect("根内绝对路径应可加入");
        assert_eq!(state.cfg.lock_recover().excluded, vec![inside.clone()]);
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
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        *state.dir_map.lock_fail_fast("dir_map") = map;
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
            let mut h = state.holder.write_fail_fast("holder");
            let idx = mutable_base_slot(&mut h).expect("holder 应持可变 base");
            let map = state.dir_map.lock_fail_fast("dir_map");
            let locked_idx = map.get(&locked).expect("locked 键应在");
            assert!(
                idx.entries[locked_idx as usize].is_live(),
                "EPERM 目录保留条目(与首扫「入索引+空子树+gaps」语义一致)"
            );
        }
        assert!(state.perm_denied.lock_recover().contains(&locked));
        assert_eq!(state.gaps.lock_recover().len(), 1, "gaps 记账一次");

        let mid = state.io_errors.load(Ordering::Relaxed);
        assert_eq!(diff_one(&state, &locked), 0);
        assert_eq!(
            state.io_errors.load(Ordering::Relaxed),
            mid,
            "负缓存短路:不再重试、不再计数"
        );
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// ENOENT 目录语义钉:磁盘已删 → 可见子树只剩根(Overlay 屏蔽,不再物化
    /// Base 写墓碑)+ dir_map 前缀键清除。突变「NotFound 分支改为保留」→ 可见
    /// 计数断言红。
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
            let h = state.holder.read_fail_fast("holder");
            let IndexHolder::Overlaid(st) = &*h else {
                panic!("首个 diff 后应为 Overlaid")
            };
            let view = st.overlay.composite_view(st.base.as_view());
            let visible = view
                .entry_segments()
                .iter()
                .flat_map(|(start, seg)| {
                    (0..seg.len() as u32).map(move |j| (start + j, &seg[j as usize]))
                })
                .filter(|(i, e)| e.is_live() && view.entry_valid(*i))
                .count();
            assert_eq!(visible, 1, "仅剩根条目:gone 子树被 Overlay 屏蔽");
        }
        assert!(
            !state.dir_map.lock_fail_fast("dir_map").contains_key(&gone),
            "dir_map 键同步清除"
        );
    }

    /// ENOENT 不得物化 mmap Base(实测:一次删除把 145 万条目整份物化 ≈150MB
    /// 堆,flush 换基后成不可折叠死页)。突变「NotFound 分支恢复
    /// base.ensure_mutable()」→ BaseRef 变体断言红。
    #[test]
    fn enoent_on_shared_base_keeps_mmap_base() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("gone").join("child")).unwrap();
        let mut idx = Index::new();
        let root_idx = idx.insert(root.to_string_lossy().as_ref(), NO_PARENT, true);
        let gone_idx = idx.insert("gone", root_idx, true);
        idx.insert("child", gone_idx, true);
        let idx_file = data.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        let state = Arc::new(EngineState::new());
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        *state.roots.lock_recover() = vec![root.clone()];
        rebuild_dir_map(&state);
        let gone = root.join("gone");
        std::fs::remove_dir_all(&gone).unwrap();

        assert_eq!(diff_one(&state, &gone), 0);
        {
            let h = state.holder.read_fail_fast("holder");
            let IndexHolder::Overlaid(st) = &*h else {
                panic!("首个 diff 后应为 Overlaid")
            };
            assert!(
                matches!(st.base, BaseRef::Mmap(_)),
                "ENOENT 摘除不得物化 mmap Base(改为父 listing 屏蔽)"
            );
            let view = st.overlay.composite_view(st.base.as_view());
            let visible = view
                .entry_segments()
                .iter()
                .flat_map(|(start, seg)| {
                    (0..seg.len() as u32).map(move |j| (start + j, &seg[j as usize]))
                })
                .filter(|(i, e)| e.is_live() && view.entry_valid(*i))
                .count();
            assert_eq!(visible, 1, "根之外全部屏蔽");
        }
        assert!(!state.dir_map.lock_fail_fast("dir_map").contains_key(&gone));
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
            let mut h = state.holder.write_fail_fast("holder");
            ensure_overlay_ready(&mut h, None); // handle_diff_error 消费 BaseRef + Overlay
            let IndexHolder::Overlaid(st) = &mut *h else {
                panic!("holder 应为 Overlaid")
            };
            let OverlaidState { base, overlay } = st.as_mut();
            let mut map = state.dir_map.lock_fail_fast("dir_map");
            handle_diff_error(&state, base, overlay, &mut map, &dir, &[], &e);
            handle_diff_error(&state, base, overlay, &mut map, &dir, &[], &e);
            assert!(
                !state.perm_denied.lock_recover().contains(&dir),
                "前两次不入负缓存"
            );
            handle_diff_error(&state, base, overlay, &mut map, &dir, &[], &e);
        }
        assert!(
            state.perm_denied.lock_recover().contains(&dir),
            "第三次入负缓存"
        );
        assert_eq!(state.gaps.lock_recover().len(), 1);
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
        assert!(state.perm_denied.lock_recover().contains(&locked));

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
            *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
            *state.dir_map.lock_fail_fast("dir_map") = map;
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
            state.rescan_dirty.lock_recover().contains(&d),
            "窗口内 diff 记集合"
        );

        // 模拟重扫产物替换:全新索引 = 扫描开始时点快照(不含窗口内 x.txt)
        let mut idx = Index::new();
        let mut map = DirIndexMap::default();
        let r = idx.insert(&root.to_string_lossy(), NO_PARENT, true);
        map.insert(&root, r);
        let d_idx = idx.insert("d", r, true);
        map.insert(&d, d_idx);
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        *state.dir_map.lock_fail_fast("dir_map") = map;
        assert_eq!(status_entry_count(&state), 2, "替换后快照不含窗口变更");

        // 替换段追平:清标志(rescanning)→ drain → 逐目录重放
        state.rescanning.store(false, Ordering::SeqCst);
        let replayed: Vec<PathBuf> = state.rescan_dirty.lock_recover().drain().collect();
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
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        let paths = DaemonPaths {
            data_dir: dir.path().join("data"),
        };
        std::fs::create_dir_all(&paths.data_dir).unwrap();
        // 预置 dirty(模拟窗口计数):mount 落地 Shared 必须清零,否则 Shared 态
        // flush 无操作 + should_flush 恒真 → 消费循环空转(评审必修 2 的 mount 侧)
        state.dirty.store(9, Ordering::Relaxed);
        degrade_and_rescan(&state, &paths, true);
        {
            let h = state.holder.read_fail_fast("holder");
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
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        let stale_key = stale.canonicalize().unwrap();
        state
            .dir_map
            .lock_fail_fast("dir_map")
            .insert(&stale_key, 999);
        // diff 走到越界键:清映射返回 0,不 panic
        assert_eq!(diff_one(&state, &stale), 0, "越界键按死键处理");
        assert!(
            !state
                .dir_map
                .lock_fail_fast("dir_map")
                .contains_key(&stale_key),
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
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);

        // 磁盘漂移 + diff → Overlaid(增量在 Overlay,未落盘)
        std::fs::write(dir.path().join("new.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1);
        assert!(matches!(
            &*state.holder.read_fail_fast("holder"),
            IndexHolder::Overlaid(_)
        ));

        // flush:合并直写 + remap + 清 Overlay
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };
        flush(&state, &paths);
        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Shared(_)
            ),
            "flush 后应为 Shared(remap 完成、Overlay 清零)"
        );
        // 内存查询:增量保留(合并写不得丢更新)
        match run_query(&state, "whole:new.txt", 10, None, false, false, None) {
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

    /// 重扫锁中毒恢复钉:前次重扫线程 panic 后 rescan_lock 中毒,try_lock 的
    /// Poisoned 若被当成「在途」吞掉,自愈路径永久哑火(实机 heal 79 次零效果
    /// 的形态)。突变「Poisoned 返回 None」本测红;在途判定必须仍准确。
    #[test]
    fn rescan_lock_poison_is_recovered_but_inflight_still_detected() {
        let lock = Arc::new(Mutex::new(()));
        let l2 = lock.clone();
        let _ = std::thread::spawn(move || {
            // 故意裸 lock:本测要制造真实中毒(不能用 lock_recover)
            let _g = l2.lock().unwrap();
            panic!("模拟重扫线程 panic");
        })
        .join();
        assert!(lock.is_poisoned(), "前提:锁已中毒");
        let g = try_acquire_rescan(&lock).expect("中毒必须恢复并取得锁");
        assert!(
            try_acquire_rescan(&lock).is_none(),
            "已持有 → 真在途,不得误判为可重入"
        );
        drop(g);
        assert!(try_acquire_rescan(&lock).is_some(), "释放后可再取");
    }

    /// 快照/配置类锁的恢复钉:前持有线程 panic → 锁中毒 → 后续 lock_recover
    /// 必须取 inner 继续,不得再次 panic(否则一次后台 panic 会把所有查询级联
    /// 打死)。突变「lock_recover 改回 .lock().unwrap()」本测红。
    #[test]
    fn lock_recover_survives_poison_and_keeps_data() {
        let m = Arc::new(Mutex::new(41u32));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("模拟持锁线程 panic");
        })
        .join();
        assert!(m.is_poisoned(), "前提:锁已中毒");
        assert_eq!(*m.lock_recover(), 41, "中毒后仍可读到 panic 前的数据");
        *m.lock_recover() += 1;
        assert_eq!(*m.lock_recover(), 42, "恢复后写入照常生效");
    }

    /// 看护层退出钉(子进程):后台线程 panic → 进程必须 exit(1),而不是只死
    /// 线程、daemon 继续应答旧索引。突变「去掉 spawn_guarded 的 catch_unwind +
    /// exit」→ 子进程走到末尾的 exit(99),父测红。
    #[test]
    fn guarded_thread_panic_exits_process() {
        const ENV: &str = "MCE_TEST_GUARDED_EXIT_CHILD";
        if std::env::var_os(ENV).is_some() {
            spawn_guarded("guard-test", || panic!("模拟消费线程 panic"));
            std::thread::sleep(Duration::from_secs(5));
            std::process::exit(99); // 未捕获到 panic 才会走到这里
        }
        let out = run_exit_child("daemon::tests::guarded_thread_panic_exits_process", ENV);
        assert_eq!(
            out.status.code(),
            Some(1),
            "后台线程 panic 应 fail-fast 退出(1);stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("模拟消费线程 panic"),
            "panic 信息应留痕: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 完整性锁 fail-fast 钉(子进程):holder/dir_map/flush_lock 中毒后继续
    /// 使用可能基于半改状态,必须退出而非恢复。突变「lock_fail_fast 改 recover」
    /// → 子进程走到 exit(99),父测红。
    #[test]
    fn integrity_lock_poison_exits_process() {
        const ENV: &str = "MCE_TEST_FAILFAST_EXIT_CHILD";
        if std::env::var_os(ENV).is_some() {
            let m = Arc::new(Mutex::new(()));
            let m2 = Arc::clone(&m);
            let _ = std::thread::spawn(move || {
                let _g = m2.lock().unwrap();
                panic!("模拟持完整性锁线程 panic");
            })
            .join();
            assert!(m.is_poisoned(), "前提:锁已中毒");
            let _g = m.lock_fail_fast("test-integrity-lock");
            std::process::exit(99); // fail-fast 未生效才会走到这里
        }
        let out = run_exit_child("daemon::tests::integrity_lock_poison_exits_process", ENV);
        assert_eq!(
            out.status.code(),
            Some(1),
            "完整性锁中毒应 fail-fast 退出(1);stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// 以当前测试二进制再跑单个用例(子进程),用于断言「进程退出」语义。
    fn run_exit_child(test_name: &str, env_key: &str) -> std::process::Output {
        let exe = std::env::current_exe().expect("测试二进制路径");
        std::process::Command::new(exe)
            .arg(test_name)
            .arg("--exact")
            .arg("--nocapture")
            .env(env_key, "1")
            .output()
            .expect("启动测试子进程")
    }

    /// 重扫中标志兜底复位钉:重扫线程 panic 时 rescanning 必须复位,否则
    /// 消费循环永久忽略 dropped、flush_merged 永久早退,索引再不落盘。
    /// 突变「ResetRescanning 的 Drop 不写 false」本测红。
    #[test]
    fn rescanning_guard_resets_flag_on_panic() {
        let flag = AtomicBool::new(true);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = ResetRescanning(&flag);
            panic!("模拟重扫线程 panic");
        }));
        assert!(r.is_err());
        assert!(!flag.load(Ordering::SeqCst), "panic 展开时必须兜底复位");
    }

    /// 判毒阈值边界钉:采样数下限 32(小样本不可判)、占比严格大于 20%。
    /// 突变「把 20 改成 >= 或去掉下限」→ 边界断言红。
    #[test]
    fn poison_threshold_boundaries() {
        assert!(!is_poison_sample(0, 0), "空索引不判毒");
        assert!(!is_poison_sample(31, 31), "样本不足 32 不判毒");
        assert!(!is_poison_sample(6, 32), "18.75% 不判毒");
        assert!(!is_poison_sample(6, 30), "样本不足:即便占比高也不判毒");
        assert!(is_poison_sample(7, 32), "21.9% 判毒");
        assert!(is_poison_sample(112, 256), "实测毒样本 43.7% 判毒");
    }

    /// 抽检端到端钉:真实目录树扫描出的索引 → 不判毒;同索引混入大量不存在
    /// 的路径(历史 bug 的错挂形态) → 判毒。突变「统计不存在的路径时恒 0」
    /// 或「路径不含 ENOTDIR」——后者用「中间级是文件」构造验证。
    #[test]
    fn path_existence_sample_separates_poison_from_healthy() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        std::fs::create_dir_all(root.join("d")).unwrap();
        for i in 0..20 {
            std::fs::write(root.join(format!("f{i}.txt")), "").unwrap();
            std::fs::write(root.join("d").join(format!("g{i}.txt")), "").unwrap();
        }
        let healthy = engine::scanner::scan_roots(&[root.to_path_buf()], &[], 1).0;
        let (missing, sampled) = index_path_missing_sample(&healthy);
        assert!(sampled >= 32, "样本量: {sampled}");
        assert_eq!(missing, 0, "健康索引路径全部存在");
        assert!(!is_poison_sample(missing, sampled));

        // 错挂形态:真实文件被当目录用(ENOTDIR),+ 纯不存在路径
        let mut poisoned = engine::scanner::scan_roots(&[root.to_path_buf()], &[], 1).0;
        let rid = (0..poisoned.entries.len() as u32)
            .find(|&i| poisoned.entries[i as usize].parent == NO_PARENT)
            .unwrap();
        let file = poisoned.children_of(rid)[0];
        for i in 0..40 {
            poisoned.insert(&format!("ghost{i}"), rid, false);
            poisoned.insert(&format!("x{i}"), file, false); // 挂到文件下 → ENOTDIR
        }
        let (missing, sampled) = index_path_missing_sample(&poisoned);
        assert!(missing > 0, "错挂路径必须计入");
        assert!(
            is_poison_sample(missing, sampled),
            "毒样本应判毒: {missing}/{sampled}"
        );
    }

    /// 存活探测:dispatch(Ping) 必须立即回 Pong,且不置 stopping(只有 Stop 能)。
    #[test]
    fn ping_dispatches_to_pong_without_stopping() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        let resp = dispatch(
            Request::Ping {
                v: protocol::PROTOCOL_VERSION,
            },
            &state,
            &paths,
        );
        assert_eq!(resp, Response::Pong);
        assert!(
            !state.stopping.load(Ordering::SeqCst),
            "Ping 不得改变停机状态"
        );
    }

    /// 退出权限只属于**处理了 Stop 的连接**,与全局 stopping 无关。
    /// 突变:改回 `_stopping`(旧实现)→ `(false, true)` 断言红——这正是
    /// 「探测连接撞 stopping 提前 exit、截断 persist_final」的根因。
    #[test]
    fn exit_permission_belongs_to_stop_connection_not_stopping_flag() {
        assert!(should_exit_process(true, true));
        assert!(should_exit_process(true, false));
        assert!(
            !should_exit_process(false, true),
            "探测/查询连接不得因 stopping 已置位而退出进程"
        );
        assert!(!should_exit_process(false, false));
    }

    /// 对端在请求前 EOF:静默关闭(Ok(false)),不上抛 WARN 级错误。
    /// 突变「EOF 仍上抛」→ 本测拿到 Err 即红。
    #[test]
    fn peer_eof_before_request_closes_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("eof.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let c = UnixStream::connect(&sock).unwrap();
        let (mut s, _) = l.accept().unwrap();
        // 只关写端(保持连接存活到 accept 之后,避免走 SO_RCVTIMEO EINVAL 分支)
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let state = Arc::new(EngineState::new());
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        let handled = handle_conn(&mut s, &state, &paths).expect("EOF 应静默关闭");
        assert!(!handled, "非 Stop 连接不得获得退出权限");
    }

    /// 退出权限只属于 Stop:**同一实现**对 Ping 返回 false、对 Stop 返回 true。
    /// 这是「探测连接撞 stopping 导致提前 exit」的结构性修复钉:旧实现按
    /// `state.stopping` 判定,任意并发连接到点即退出;新实现按请求类型判定。
    /// 突变「按 stopping 判定 / Stop 返回 false」→ 本测红。
    #[test]
    fn only_stop_connection_requests_process_exit() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };

        // ① Ping:Ok(false) + 线上 Pong
        {
            let sock = dir.path().join("ping.sock");
            let l = UnixListener::bind(&sock).unwrap();
            let mut c = UnixStream::connect(&sock).unwrap();
            let (mut s, _) = l.accept().unwrap();
            c.write_all(
                &protocol::encode(&Request::Ping {
                    v: protocol::PROTOCOL_VERSION,
                })
                .unwrap(),
            )
            .unwrap();
            c.flush().unwrap();
            let handled = handle_conn(&mut s, &state, &paths).unwrap();
            assert!(!handled, "Ping 连接不得请求退出");
            let mut len = [0u8; 4];
            c.read_exact(&mut len).unwrap();
            let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
            c.read_exact(&mut body).unwrap();
            assert_eq!(
                protocol::decode_response(&body).unwrap(),
                Response::Pong,
                "Ping 必须收到 Pong"
            );
        }

        // ② Stop:Ok(true) + Stop 应答 + stopping 置位(由 serve 负责随后 exit)
        {
            let sock = dir.path().join("stop.sock");
            let l = UnixListener::bind(&sock).unwrap();
            let mut c = UnixStream::connect(&sock).unwrap();
            let (mut s, _) = l.accept().unwrap();
            c.write_all(
                &protocol::encode(&Request::Stop {
                    v: protocol::PROTOCOL_VERSION,
                })
                .unwrap(),
            )
            .unwrap();
            c.flush().unwrap();
            let handled = handle_conn(&mut s, &state, &paths).unwrap();
            assert!(handled, "Stop 连接必须请求退出(落盘与应答已完成)");
            assert!(state.stopping.load(Ordering::SeqCst));
            let mut len = [0u8; 4];
            c.read_exact(&mut len).unwrap();
            let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
            c.read_exact(&mut body).unwrap();
            match protocol::decode_response(&body).unwrap() {
                Response::Stop {
                    ok,
                    final_entry_count,
                } => {
                    assert!(ok, "Empty 态落盘应为成功");
                    assert_eq!(final_entry_count, 0);
                }
                other => panic!("意外响应: {other:?}"),
            }
        }
    }

    /// 死连接静默关闭钉(实测 EINVAL 根因):对端在 accept 前已关闭时
    /// setsockopt(SO_RCVTIMEO) 返回 EINVAL,handle_conn 必须当作「连接已死」
    /// 静默返回 Ok,而不是 WARN 刷屏(实测 connect+立即关闭 257/300)。
    /// 突变「EINVAL 仍上抛」本测红(平台不返回 EINVAL 时测试自跳过)。
    #[test]
    fn dead_peer_connection_closes_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("s.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let c = UnixStream::connect(&sock).unwrap();
        drop(c); // 对端立即断开
        std::thread::sleep(Duration::from_millis(50));
        let (mut s, _) = l.accept().unwrap();
        if s.set_read_timeout(Some(Duration::from_secs(5))).is_ok() {
            return; // 平台不再返回 EINVAL:本钉无区分力(修复路径本身仍安全)
        }
        let state = Arc::new(EngineState::new());
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        assert!(
            handle_conn(&mut s, &state, &paths).is_ok(),
            "对端已关闭的连接应静默关闭"
        );
    }

    /// 腐化自愈的对照钉:非腐化 io 错误(磁盘/路径问题,重扫无益)不得置自愈
    /// 位——is_ledger_corruption 的判定边界(InvalidData + 精确前缀)。
    /// 突变「判定放宽为一切 InvalidData / 一切错误」→ 对照断言红。
    #[test]
    fn flush_merged_non_corruption_error_does_not_request_heal() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let mut map = DirIndexMap::default();
        let key = dir.path().canonicalize().unwrap();
        map.insert(&key, 0);
        *state.holder.write_fail_fast("holder") = IndexHolder::Overlaid(Box::new(OverlaidState {
            base: BaseRef::Heap(idx),
            overlay: engine::overlay::Overlay::new(),
        }));
        *state.dir_map.lock_fail_fast("dir_map") = map;
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        // 数据目录不存在:合并写 io NotFound(非腐化)
        let paths = DaemonPaths {
            data_dir: dir.path().join("absent"),
        };
        flush(&state, &paths);
        assert!(
            !state.heal_requested.load(Ordering::SeqCst),
            "普通 io 错误不得触发降级重扫(重扫无益)"
        );
    }

    /// 合并 flush 后 dir_map 必须随新 Base 重建(P0 回归钉,实测 310MB 膨胀的
    /// 根因):合并空间的条目号是**全新编号**,swap 成功后 dir_map 残留旧号——
    /// 旧号在新索引里恰好命中**无关活条目**时,#12 的「越界/死」判活拦不住
    /// (「活而错」),apply_diff 会把磁盘 listing 挂到错条目下:拼出大量错误
    /// 唯一路径(实测 1.4M → 237 万条),added 计数暴涨使 dirty 恒过 30k 阈值,
    /// 合并 flush 密集背靠背(实测 23 次)直至索引膨胀、连接饥饿挂起。
    /// 突变「swap 成功后不 rebuild_dir_map」本测两段断言红。
    #[test]
    fn merged_flush_rebuilds_dir_map_for_new_entry_ids() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        // v1 编号:root=0, gone=1, d=2, x.txt=3(gone 在 d 之前,合并后前移)
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("gone.txt", root, false);
        let d = idx.insert("d", root, true);
        idx.insert("x.txt", d, false);
        let idx_file = data.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);
        // 磁盘树与 v1 索引同构(扫描语义)
        std::fs::create_dir(dir.path().join("d")).unwrap();
        std::fs::write(dir.path().join("gone.txt"), b"").unwrap();
        std::fs::write(dir.path().join("d/x.txt"), b"").unwrap();
        let d_path = dir.path().join("d").canonicalize().unwrap();

        // 磁盘漂移:删 gone + 加 new.txt → diff(root) → flush 合并。
        // 合并可见集 = root, d, x.txt, new.txt(gone 被屏蔽不落盘)→
        // 新编号 root=0, d=1, x.txt=2, new.txt=3 —— d 的条目号 2→1。
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        std::fs::write(dir.path().join("new.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 2);
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };
        flush(&state, &paths);
        assert!(matches!(
            &*state.holder.read_fail_fast("holder"),
            IndexHolder::Shared(_)
        ));

        // 断言 A:dir_map 的 d 值必须等于**新索引**中 d 的条目号(1)。
        // 索引内路径为字面形态(夹具插入时的 dir.path()),不经 canonicalize
        let d_literal = dir.path().join("d").to_string_lossy().into_owned();
        let new_d_id = with_view(&state, |v| {
            (0..v
                .entry_segments()
                .iter()
                .map(|(_, s)| s.len() as u32)
                .sum::<u32>())
                .find(|&i| v.full_path_dyn(i) == d_literal)
        })
        .flatten()
        .expect("新索引应含 d");
        assert_ne!(new_d_id, 2, "夹具应保证编号前移(d: 2→{new_d_id})");
        assert_eq!(
            state.dir_map.lock_fail_fast("dir_map").get(&d_path),
            Some(new_d_id),
            "合并 swap 后 dir_map 必须重建(d 值应为新号)",
        );

        // 断言 B:d 内再漂移,diff 落在正确条目上(不得挂到 2 号 new.txt 下)。
        std::fs::write(d_path.join("y.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, &d_path), 1);
        match run_query(&state, "whole:y.txt", 10, None, false, false, None) {
            Response::Query { entries, total, .. } => {
                assert_eq!(total, 1);
                // 索引内路径为字面形态(root 条目名 = 夹具插入时的 dir.path())
                assert_eq!(
                    entries[0].path,
                    dir.path().join("d/y.txt").to_string_lossy(),
                    "y.txt 必须挂在 d 下(不得挂到错条目拼出错误路径)"
                );
            }
            other => panic!("意外响应: {other:?}"),
        }
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
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        rebuild_dir_map(&state);
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };

        // diff(new.txt)→ Overlaid;以当前世代为快照,合并落盘一份「不含窗口
        // 增量」的盘面(flush 读锁快照段的等价物)
        std::fs::write(dir.path().join("new.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 1);
        let epoch_snapshot = match &*state.holder.read_fail_fast("holder") {
            IndexHolder::Overlaid(st) => st.overlay.epoch(),
            _ => panic!("diff 后应为 Overlaid"),
        };
        {
            let h = state.holder.read_fail_fast("holder");
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
            !swap_base_if_fresh(&state, stale_view, epoch_snapshot, &[], &[]),
            "世代前移必须放弃替换"
        );
        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Overlaid(_)
            ),
            "拒绝替换后保留增量态"
        );
        // P0 加固钉:放弃替换的收尾必须清 dirty(盘面已含快照增量)——否则
        // dirty 恒过 30k 阈值使全量合并写背靠背密集重试(实测 23 次风暴的
        // 放大器)。突变「defer_stale_merge 不清 dirty」此断言红。
        state.dirty.store(5, Ordering::Relaxed);
        defer_stale_merge(&state);
        assert_eq!(
            state.dirty.load(Ordering::Relaxed),
            0,
            "放弃替换收尾清 dirty(防密集重写风暴)"
        );
        match run_query(&state, "whole:c.txt", 10, None, false, false, None) {
            Response::Query { total, .. } => {
                assert_eq!(total, 1, "窗口增量不得随陈旧快照替换丢失")
            }
            other => panic!("意外响应: {other:?}"),
        }

        // 下轮 flush:重新合并落盘(含 c.txt)→ 以当前世代替换成功 → Shared
        flush(&state, &paths);
        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Shared(_)
            ),
            "下轮 flush 应替换成功(世代一致)"
        );
        match run_query(&state, "whole:c.txt", 10, None, false, false, None) {
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
        *state.holder.write_fail_fast("holder") = IndexHolder::Overlaid(Box::new(OverlaidState {
            base: BaseRef::Mmap(persist::load_mmap(&idx_file).unwrap()),
            overlay: engine::overlay::Overlay::new(),
        }));
        state.dirty.store(7, Ordering::Relaxed);
        collapse_empty_overlay(&state);
        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Shared(_)
            ),
            "空 Overlay 的 mmap Base 应收敛 Shared"
        );
        assert_eq!(
            state.dirty.load(Ordering::Relaxed),
            0,
            "落地 Shared 清 dirty"
        );
    }

    // ---------- C1:卷全量扫描闸门 ----------

    /// 扫描闸门语义钉(C1):卷扫描期间该卷前缀内的 diff 只登记不应用——否则
    /// 卷流消费线程先 diff 会把目录收进 Overlay(holder 转 Overlaid),扫描随后
    /// 插入的深层 Base 条目因「最近 overlaid 祖先非 anchor」被合并写丢弃。
    /// 扫描结束后释放闸门并按登记目录重放,窗口变更不丢。
    /// 突变「diff_one 去掉闸门登记」→ 第一步 diff 即返回 1 且 holder 转 Overlaid,
    /// 两条断言必红。
    #[test]
    fn volume_scan_gate_defers_diffs_until_replay() {
        let dir = tempfile::tempdir().unwrap();
        let state = mounted_state(dir.path());
        let vol = dir.path().canonicalize().unwrap();
        let token = begin_volume_scan(&state, &vol);

        std::fs::write(dir.path().join("new.txt"), b"").unwrap();
        assert_eq!(diff_one(&state, dir.path()), 0, "闸门期间 diff 不得应用");
        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Mutable(_)
            ),
            "闸门期间不得转 Overlaid(否则扫描深层条目会被 overlaid 祖先屏蔽)"
        );

        let pending = end_volume_scan(&state, token);
        assert_eq!(pending.len(), 1, "被闸住的目录应登记");
        assert_eq!(diff_one(&state, &pending[0]), 1, "释放闸门后重放生效");
        match run_query(&state, "whole:new.txt", 10, None, false, false, None) {
            Response::Query { total, .. } => assert_eq!(total, 1, "重放后新文件可见"),
            other => panic!("意外响应: {other:?}"),
        }

        // 旧 token 不得清新扫描的闸门/偷走其 pending
        let t2 = begin_volume_scan(&state, &vol);
        let _ = end_volume_scan(&state, token);
        assert!(
            state.volume_scan_gate.lock_recover().owner.is_some(),
            "旧扫描收尾不得清掉新扫描的闸门"
        );
        let _ = end_volume_scan(&state, t2);
    }

    /// C1 + I6 集成钉:卷扫描把深层条目直写 Base,完成后落盘并收敛回 mmap
    /// Shared(堆索引回收);卷内所有层级(含深层文件)完整可查。
    /// 突变「scan 不 end gate / 不重放」由闸门单测覆盖;本测钉扫描结果完整性与
    /// 收敛形态(旧实现 flush 只写盘不换 Shared → 第一条断言红)。
    #[test]
    fn scan_volume_into_indexes_deep_tree_and_converges_shared() {
        let t = tempfile::tempdir().unwrap();
        let vol = t.path().join("vol");
        std::fs::create_dir_all(vol.join("a/b")).unwrap();
        std::fs::write(vol.join("a/b/c.txt"), b"").unwrap();
        std::fs::write(vol.join("top.txt"), b"").unwrap();
        let data = t.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        let paths = DaemonPaths { data_dir: data };
        let state = Arc::new(EngineState::new());
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(Index::new());
        *state.roots.lock_recover() = vec![vol.clone()];

        volumes::scan_volume_into(&state, &paths, &vol);

        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Shared(_)
            ),
            "I6:卷扫描落盘后必须收敛为 mmap Shared(回收堆索引)"
        );
        let disk = persist::load_mmap(&paths.index_file()).unwrap();
        for name in ["a", "b", "c.txt", "top.txt"] {
            let out = engine::query::execute(
                disk.as_ref(),
                &engine::query::parse(&format!("whole:{name}")).unwrap(),
                10,
            );
            assert_eq!(out.total, 1, "卷内 {name} 必须已落盘(深层条目不得被截断)");
        }
        assert!(
            state
                .dir_map
                .lock_fail_fast("dir_map")
                .contains_key(&vol.canonicalize().unwrap()),
            "收敛后 dir_map 必须与新 Base 同点重建(卷根可寻址)"
        );
    }

    // ---------- C2:compact 与 dir_map 同点提交 ----------

    /// C2 窗口钉:compact 重排条目号后,dir_map 必须在**同一 holder 写锁临界区**
    /// 内重建并安装。测试持 dir_map 锁迫使 compact 线程停在提交点:
    /// - 新实现:线程持 holder 写锁等待 dir_map → `try_read` 一直失败;
    /// - 旧实现(compact 放锁 → rebuild 再装 map):holder 已释放,`try_read` 成功。
    ///
    /// 突变「放锁后 rebuild」→ 100ms 后的 try_read 断言红。
    #[test]
    fn compact_commits_dir_map_under_holder_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("d")).unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("d", root, true);
        let gone = idx.insert("gone.txt", root, false);
        idx.remove(gone); // 墓碑 1/3 > 20%:触发整理
        assert!(idx.needs_compaction(), "夹具应满足整理阈值");
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        state
            .dir_map
            .lock_fail_fast("dir_map")
            .insert(Path::new("/stale/compacted/key"), 999);

        let dir_map_guard = state.dir_map.lock_fail_fast("dir_map");
        let st = state.clone();
        let handle = std::thread::spawn(move || maybe_compact(&st));
        let mut entered = false;
        for _ in 0..500 {
            if state.holder.try_read().is_err() {
                entered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(entered, "maybe_compact 应取得 holder 写锁");
        // compact(微秒级)早已完成;若实现先放 holder 再去等 dir_map,此刻可读。
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            state.holder.try_read().is_err(),
            "dir_map 提交前不得释放 holder 写锁(C2 窗口:旧 map 打新 Base)"
        );
        drop(dir_map_guard);
        handle.join().unwrap();

        let map = state.dir_map.lock_fail_fast("dir_map");
        let root_key = dir.path().canonicalize().unwrap();
        assert!(
            map.get(&root_key).is_some(),
            "整理后 dir_map 必须与新 Base 同点重建"
        );
        assert!(map.get(&root_key.join("d")).is_some(), "目录子键应重建");
        assert!(
            map.get(Path::new("/stale/compacted/key")).is_none(),
            "陈旧键应随整表重建消失"
        );
        drop(map);
        let h = state.holder.read_fail_fast("holder");
        let IndexHolder::Mutable(idx) = &*h else {
            panic!("整理后仍应为 Mutable");
        };
        assert_eq!(idx.entries.iter().filter(|e| e.is_live()).count(), 2);
    }

    /// C2:陈旧 map 键指向**活文件**时,不得按目录做 diff(apply_and_track 的
    /// 判活必须含 is_dir)。旧实现只判 is_live → 文件被当目录,listing 挂到文件
    /// 名下并污染 Overlay。突变「去掉 is_dir」→ diff 返回 1、holder 转 Overlaid,
    /// 本测两条断言红。
    #[test]
    fn stale_dir_map_key_pointing_to_live_file_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("realdir")).unwrap();
        std::fs::write(dir.path().join("realdir/new.txt"), b"").unwrap();
        std::fs::write(dir.path().join("f.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("realdir", root, true);
        let f = idx.insert("f.txt", root, false);
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        let root_key = dir.path().canonicalize().unwrap();
        let real_key = root_key.join("realdir");
        let mut map = DirIndexMap::default();
        map.insert(&root_key, root);
        map.insert(&real_key, f); // 陈旧/错位:目录键指向活文件
        *state.dir_map.lock_fail_fast("dir_map") = map;

        assert_eq!(
            diff_one(&state, &real_key),
            0,
            "指向文件的键不得按目录 diff"
        );
        assert!(
            !state
                .dir_map
                .lock_fail_fast("dir_map")
                .contains_key(&real_key),
            "错位键应被清出"
        );
        // diff 入口会先把 Shared/Mutable 转 Overlaid(形态转换与目标无关),
        // 但错位键必须**不产生任何增量**——Overlay 账本与条目均空。
        let h = state.holder.read_fail_fast("holder");
        match &*h {
            IndexHolder::Overlaid(st) => {
                assert!(st.overlay.is_empty(), "不得为错位键建 Overlay");
            }
            IndexHolder::Mutable(_) => {}
            _ => panic!("diff 后应为 Mutable 或 Overlaid"),
        }
    }

    /// C2:mask_deleted_via_parent 对 resolve_target 给出的 Base 号必须复用同一
    /// 套判活(越界/非目录)。旧实现直接 apply_diff → `entry(999)` 越界 panic。
    /// 突变「去掉校验」→ 本测 panic 红。
    #[test]
    fn mask_deleted_via_parent_ignores_out_of_range_base_id() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("d")).unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("d", root, true);
        let base = BaseRef::Heap(idx);
        let root_key = dir.path().canonicalize().unwrap();
        let d_key = root_key.join("d");
        let mut map = DirIndexMap::default();
        map.insert(&root_key, root);
        map.insert(&d_key, 999); // 换基窗口残留的越界号
        let mut overlay = engine::overlay::Overlay::new();

        let gone = d_key.join("deleted.txt");
        let out = mask_deleted_via_parent(&state, &base, &mut overlay, &mut map, &gone, &[]);
        assert!(out.is_some(), "应上溯到有效祖先完成屏蔽,而不是 panic");
        assert_ne!(map.get(&d_key), Some(999), "陈旧越界键不得保留");
    }

    // ---------- C3:扫描产物水位 ----------

    /// C3:扫描产物落盘 meta 的水位必须取**扫描开始时的 floor**,而不是扫描
    /// 期间被消费线程推进的 applied——否则崩溃重启从超前水位重放,跳过窗口事件。
    /// 突变「mount_scan_product 用 build_meta(当前 applied)」→ 断言 500≠100 红。
    #[test]
    fn mount_scan_product_writes_scan_floor_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        // 模拟:扫描期间消费线程把 applied 推到 500,但产物只覆盖 floor=100。
        state.applied_event_id.store(500, Ordering::Relaxed);
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        let mut idx = Index::new();
        idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };

        mount_scan_product(&state, &paths, idx, 100);

        let disk = persist::load_mmap(&paths.index_file()).unwrap();
        assert_eq!(
            disk.meta.last_event_id, 100,
            "扫描产物水位必须取 floor(崩溃窗口多重放,绝不漏事件)"
        );
        assert!(matches!(
            &*state.holder.read_fail_fast("holder"),
            IndexHolder::Shared(_)
        ));
    }

    /// C3:Stop 落盘必须与重扫串行(rescan_lock → flush_lock)。重扫在途时
    /// persist_final 不得抢先写盘;突变「Stop 不取 rescan_lock」→ Stop 立即完成,
    /// 第一条断言红。
    #[test]
    fn stop_waits_for_inflight_rescan() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let paths = DaemonPaths {
            data_dir: dir.path().to_path_buf(),
        };
        let rescan = state.rescan_lock.lock().unwrap();
        let st = state.clone();
        let p = paths.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let resp = dispatch(
                Request::Stop {
                    v: protocol::PROTOCOL_VERSION,
                },
                &st,
                &p,
            );
            let _ = tx.send(resp);
        });
        assert!(
            matches!(
                rx.recv_timeout(Duration::from_millis(150)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "重扫在途时 Stop 落盘必须等待(否则写入超前水位)"
        );
        drop(rescan);
        let resp = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("释放重扫锁后 Stop 应完成");
        assert!(matches!(resp, Response::Stop { ok: true, .. }));
        handle.join().unwrap();
    }

    // ---------- I6:Mutable → Shared 收敛 ----------

    /// I6:Mutable 落盘后必须 remap 回 Shared——否则插拔一次卷就把整份 Base
    /// 永久钉在堆上。突变「只有写盘、不换 Shared」→ 第一条断言红。
    #[test]
    fn mutable_flush_converges_to_shared_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"").unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        idx.insert("a.txt", root, false);
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        state.dirty.store(1, Ordering::Relaxed);
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };

        flush(&state, &paths);

        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Shared(_)
            ),
            "I6:Mutable 落盘后必须收敛 Shared(堆索引回收)"
        );
        assert_eq!(state.dirty.load(Ordering::Relaxed), 0);
        assert!(
            state
                .dir_map
                .lock_fail_fast("dir_map")
                .contains_key(&dir.path().canonicalize().unwrap()),
            "收敛时 dir_map 必须与新 Base 同点安装"
        );
        let disk = persist::load_mmap(&paths.index_file()).unwrap();
        let out = engine::query::execute(
            disk.as_ref(),
            &engine::query::parse("whole:a.txt").unwrap(),
            10,
        );
        assert_eq!(out.total, 1, "落盘内容完整");
    }

    /// I6:启动清除路径必须计 dirty + flush:否则 Shared→Mutable 物化后不写盘,
    /// 堆索引常驻且盘面残留未挂载卷条目。
    /// 突变「purge 不清 dirty/不 flush」→ holder 仍 Mutable、盘面仍含卷前缀,红。
    #[test]
    fn purge_unmounted_flushes_and_converges() {
        let data = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        let root = idx.insert("/Volumes/MceTestUnmounted", NO_PARENT, true);
        idx.insert("deep.txt", root, false);
        let idx_file = data.path().join("index.bin");
        persist::write_atomic(
            &persist::EntryLayout::of(&idx),
            idx.entries.iter(),
            &persist::IndexMeta::default(),
            &idx_file,
        )
        .unwrap();
        *state.holder.write_fail_fast("holder") =
            IndexHolder::Shared(persist::load_mmap(&idx_file).unwrap());
        state
            .dir_map
            .lock_fail_fast("dir_map")
            .insert(Path::new("/Volumes/MceTestUnmounted"), root);
        let paths = DaemonPaths {
            data_dir: data.path().to_path_buf(),
        };

        volumes::purge_unmounted(&state, &paths, &HashSet::new());

        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Shared(_)
            ),
            "启动清除后应收敛 Shared(回收物化堆索引)"
        );
        let disk = persist::load_mmap(&paths.index_file()).unwrap();
        let out = engine::query::execute(
            disk.as_ref(),
            &engine::query::parse("MceTestUnmounted").unwrap(),
            10,
        );
        assert_eq!(out.total, 0, "盘面不得残留未挂载卷条目");
    }

    // ---------- I4:落盘失败退避 ----------

    /// I4:落盘失败必须推进节流并计入失败次数(指数退避);否则 dirty 恒 ≥ 阈值
    /// 时消费循环每 150ms 重试全量遍历 + 写盘。
    /// 突变「失败分支不推进 last_flush / 不累计失败」→ 首轮失败后 should_flush
    /// 仍为 true,本测红。
    #[test]
    fn failed_merged_flush_backs_off_instead_of_tight_loop() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        *state.holder.write_fail_fast("holder") = IndexHolder::Overlaid(Box::new(OverlaidState {
            base: BaseRef::Heap(idx),
            overlay: engine::overlay::Overlay::new(),
        }));
        // 数据目录不存在:合并写 io NotFound(非腐化)
        let paths = DaemonPaths {
            data_dir: dir.path().join("absent"),
        };
        state.dirty.store(FLUSH_DIRTY_LIMIT, Ordering::Relaxed);

        assert!(should_flush(&state), "dirty 达阈值应触发首轮 flush");
        flush(&state, &paths);
        assert_eq!(
            state.flush_failures.load(Ordering::Relaxed),
            1,
            "失败次数应累计"
        );
        assert!(!should_flush(&state), "写失败后必须退避,不得紧循环重试");

        // 退避窗口(5s)过后允许重试
        *state.last_flush.lock_recover() = Instant::now() - Duration::from_secs(6);
        assert!(should_flush(&state), "退避窗口过后应重试");
        flush(&state, &paths);
        assert_eq!(state.flush_failures.load(Ordering::Relaxed), 2);
        // 连续失败退避翻倍(10s):6s 仍等待,11s 重试
        *state.last_flush.lock_recover() = Instant::now() - Duration::from_secs(6);
        assert!(!should_flush(&state), "连续失败退避应翻倍");
        *state.last_flush.lock_recover() = Instant::now() - Duration::from_secs(11);
        assert!(should_flush(&state));
    }

    /// I4:Mutable 写失败同样必须退避(旧实现只打日志,last_flush/dirty 均不动)。
    #[test]
    fn failed_mutable_flush_backs_off_instead_of_tight_loop() {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(EngineState::new());
        let mut idx = Index::new();
        idx.insert(dir.path().to_string_lossy().as_ref(), NO_PARENT, true);
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        *state.roots.lock_recover() = vec![dir.path().to_path_buf()];
        let paths = DaemonPaths {
            data_dir: dir.path().join("absent"),
        };
        state.dirty.store(FLUSH_DIRTY_LIMIT, Ordering::Relaxed);

        assert!(should_flush(&state));
        flush(&state, &paths);
        assert!(
            matches!(
                &*state.holder.read_fail_fast("holder"),
                IndexHolder::Mutable(_)
            ),
            "写失败保持 Mutable(内存即真相)"
        );
        assert_eq!(state.flush_failures.load(Ordering::Relaxed), 1);
        assert!(!should_flush(&state), "Mutable 写失败同样必须退避");
    }

    /// 相同消息在窗口内折叠:连续 3 条重复只出首条,计数留待下一条不同消息。
    /// 突变「窗口内不折叠」时实机日志会恢复刷屏,此测必红。
    #[test]
    fn repeat_filter_folds_identical_messages_within_window() {
        let mut f = RepeatFilter::default();
        let t0 = Instant::now();
        let warn = log::Level::Warn;
        assert!(matches!(
            f.observe(warn, "boom", t0),
            Emit::Write {
                suppressed: 0,
                folded_level: None
            }
        ));
        assert!(matches!(
            f.observe(warn, "boom", t0 + Duration::from_secs(1)),
            Emit::Skip
        ));
        assert!(matches!(
            f.observe(warn, "boom", t0 + Duration::from_secs(2)),
            Emit::Skip
        ));
        match f.observe(warn, "other", t0 + Duration::from_secs(3)) {
            Emit::Write {
                suppressed,
                folded_level,
            } => {
                assert_eq!(suppressed, 2, "补报累计折叠数");
                assert_eq!(folded_level, Some(warn), "补报沿用被折叠消息的级别");
            }
            Emit::Skip => panic!("不同消息必须写出"),
        }
    }

    /// 窗口到期后同一条消息重新放行(否则长期只出一条,丢失持续性证据);
    /// 放行时补报窗口内被折叠的次数。
    #[test]
    fn repeat_filter_reemits_after_window() {
        let mut f = RepeatFilter::default();
        let t0 = Instant::now();
        let info = log::Level::Info;
        assert!(matches!(
            f.observe(info, "boom", t0),
            Emit::Write {
                suppressed: 0,
                folded_level: None
            }
        ));
        assert!(matches!(
            f.observe(info, "boom", t0 + Duration::from_secs(1)),
            Emit::Skip
        ));
        match f.observe(info, "boom", t0 + LOG_DEDUP_WINDOW + Duration::from_secs(1)) {
            Emit::Write { suppressed, .. } => assert_eq!(suppressed, 1, "补报窗口内折叠数"),
            Emit::Skip => panic!("窗口外必须放行"),
        }
    }

    /// 同正文不同级别不得互相折叠:否则 error 会被 warn 吞掉,归因错误。
    /// 突变「dedup key 忽略级别」时此测必红。
    #[test]
    fn repeat_filter_does_not_fold_across_levels() {
        let mut f = RepeatFilter::default();
        let t0 = Instant::now();
        assert!(matches!(
            f.observe(log::Level::Warn, "boom", t0),
            Emit::Write {
                suppressed: 0,
                folded_level: None
            }
        ));
        match f.observe(log::Level::Error, "boom", t0 + Duration::from_secs(1)) {
            Emit::Write { suppressed, .. } => assert_eq!(suppressed, 0, "级别不同不得折叠"),
            Emit::Skip => panic!("error 不得被 warn 折叠"),
        }
    }

    /// 轮转保留 3 份:daemon.log → .1 → .2 → .3,第 4 次轮转丢弃最老的 .3。
    /// 突变「备份数少一」或「不删最老」时此测必红。
    #[test]
    fn rotate_log_keeps_configured_backups() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        for generation in 0..5 {
            std::fs::write(&log, format!("gen{generation}")).unwrap();
            rotate_log(&log).unwrap();
        }
        assert!(!log.exists(), "轮转后原文件被改名");
        assert_eq!(
            std::fs::read_to_string(backup_path(&log, 1)).unwrap(),
            "gen4"
        );
        assert_eq!(
            std::fs::read_to_string(backup_path(&log, 2)).unwrap(),
            "gen3"
        );
        assert_eq!(
            std::fs::read_to_string(backup_path(&log, 3)).unwrap(),
            "gen2"
        );
        assert!(!backup_path(&log, 4).exists(), "只保留 3 份备份");
    }

    /// FileSink 的阈值触发:超过 `next_rotate_at` 的那一行写出前轮转,计数归零。
    /// 突变「append 不检查阈值」时此测必红。
    #[test]
    fn file_sink_rotates_at_limit_and_resets_counter() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        let mut sink = FileSink::with_limit(log.clone(), 32);
        for _ in 0..3 {
            sink.append("0123456789\n"); // 11B × 3 = 33B,第 4 次写入前才轮转
        }
        assert!(!backup_path(&log, 1).exists(), "未检查前不轮转");
        sink.append("0123456789\n");
        assert!(backup_path(&log, 1).exists(), "超过上限必须轮转");
        assert_eq!(
            std::fs::metadata(backup_path(&log, 1)).unwrap().len(),
            33,
            "备份内容是轮转前的全部字节"
        );
        assert!(sink.bytes <= 32, "轮转后计数归零再写当前行");
    }

    /// 轮转失败不得把计数归零(否则实际文件会涨到 2×limit 才再尝试),
    /// 且重试窗口后移,避免每行一次 rename。
    #[test]
    fn file_sink_rotation_failure_keeps_counter() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("daemon.log");
        // 造一个不可覆盖的 .3(非空目录),让位移链在 rename(.2 → .3) 处失败。
        std::fs::write(backup_path(&log, 1), b"x").unwrap();
        std::fs::write(backup_path(&log, 2), b"x").unwrap();
        std::fs::create_dir(backup_path(&log, 3)).unwrap();
        std::fs::write(backup_path(&log, 3).join("block"), b"x").unwrap();

        let mut sink = FileSink::with_limit(log.clone(), 10);
        sink.append("0123456789\n"); // 11B
        sink.append("0123456789\n"); // 检查 11 >= 10 → 轮转失败,继续追加
        assert!(sink.bytes > 0, "轮转失败不得归零");
        assert!(
            sink.next_rotate_at > sink.limit,
            "失败后重试窗口后移,避免每行重试"
        );
        assert!(backup_path(&log, 3).is_dir(), "失败路径原样保留");
    }

    /// 统计增量按窗口取且只取一次:窗口内不重复计,空闲窗口返回全 0。
    /// 突变「直接输出总数」或「不推进基线」时此测必红。
    #[test]
    fn run_stats_delta_is_windowed() {
        let mut s = RunStats::default();
        let t0 = Instant::now();
        s.events = 5;
        s.drops = 2;
        s.must_scan = 2;
        let d = s.take_delta(t0, Duration::from_secs(60)).unwrap();
        assert_eq!(d.events, 5);
        assert_eq!(d.drops, 2);
        assert_eq!(d.must_scan, 2);
        assert!(
            s.take_delta(t0 + Duration::from_secs(1), Duration::from_secs(60))
                .is_none(),
            "窗口内不重复取"
        );
        s.events += 3;
        let d = s
            .take_delta(t0 + Duration::from_secs(61), Duration::from_secs(60))
            .unwrap();
        assert_eq!(d.events, 3, "只取窗口内增量");
        let d = s
            .take_delta(t0 + Duration::from_secs(122), Duration::from_secs(60))
            .unwrap();
        assert_eq!(d, StatsDelta::default(), "空闲窗口返回全 0");
    }

    /// 键控限频:窗口内只放行首条,放行时报告期间抑制数。
    #[test]
    fn throttle_map_allows_once_per_window() {
        let mut m = ThrottleMap {
            entries: HashMap::new(),
        };
        let t0 = Instant::now();
        assert_eq!(m.allow("k", Duration::from_secs(60), t0), Some(0));
        assert_eq!(
            m.allow("k", Duration::from_secs(60), t0 + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            m.allow("k", Duration::from_secs(60), t0 + Duration::from_secs(2)),
            None
        );
        assert_eq!(
            m.allow("k", Duration::from_secs(60), t0 + Duration::from_secs(61)),
            Some(2),
            "窗口到期放行并回报抑制数"
        );
        assert_eq!(
            m.allow(
                "other",
                Duration::from_secs(60),
                t0 + Duration::from_secs(62)
            ),
            Some(0),
            "不同键互不影响"
        );
    }

    /// 时间戳形状固定(毫秒 3 位):排序/机器解析依赖定宽。
    #[test]
    fn now_stamp_is_fixed_width() {
        let s = now_stamp();
        assert_eq!(s.len(), 23, "YYYY-MM-DD HH:MM:SS.mmm:{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], " ");
        assert_eq!(&s[19..20], ".");
    }

    /// 丢弃去抖:持续事件期间不触发,安静满窗口才触发。
    /// 突变「不检查 last_event_at」会让风暴中被立即重扫,此测必红。
    #[test]
    fn rescan_debounce_waits_for_quiet() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        let quiet = Duration::from_secs(2);
        assert!(d.request(t0, 7), "首次登记");
        d.note_event(t0 + Duration::from_millis(500));
        assert!(!d.due(
            t0 + Duration::from_millis(1500),
            quiet,
            Duration::from_secs(60)
        ));
        d.note_event(t0 + Duration::from_millis(2500));
        assert!(!d.due(
            t0 + Duration::from_millis(3000),
            quiet,
            Duration::from_secs(60)
        ));
        assert!(
            d.due(
                t0 + Duration::from_millis(4600),
                quiet,
                Duration::from_secs(60)
            ),
            "安静满 2s 后触发"
        );
    }

    /// 重复丢弃不刷新待命起点(否则持续风暴永不触发;上限等待是另一道闸)。
    #[test]
    fn rescan_debounce_repeated_requests_keep_first_instant() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        assert!(d.request(t0, 7));
        assert!(
            !d.request(t0 + Duration::from_secs(1), 8),
            "重复登记返回 false"
        );
        assert!(d.due(
            t0 + Duration::from_secs(3),
            Duration::from_secs(2),
            Duration::from_secs(60)
        ));
    }

    /// 持续风暴:事件不断刷新安静窗,但待命超过上限必须强制触发。
    /// 突变「去掉上限分支」会让索引在长风暴中永不收敛,此测必红。
    #[test]
    fn rescan_debounce_forces_after_max_wait() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        let quiet = Duration::from_secs(2);
        let max = Duration::from_secs(60);
        d.request(t0, 7);
        d.note_event(t0 + Duration::from_secs(59));
        assert!(
            !d.due(t0 + Duration::from_secs(59), quiet, max),
            "未到上限且刚有事件"
        );
        assert!(d.due(t0 + max, quiet, max), "待命满上限强制触发");
    }

    /// 触发后清待命;未登记时任何时刻都不触发。
    #[test]
    fn rescan_debounce_take_and_idle() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        assert!(!d.due(t0, Duration::from_secs(2), Duration::from_secs(60)));
        d.request(t0, 7);
        d.take();
        assert!(!d.due(
            t0 + Duration::from_secs(100),
            Duration::from_secs(2),
            Duration::from_secs(60)
        ));
    }

    /// 待命核销只认「丢弃 ID ≤ floor」:更大 ID 的丢弃发生在扫描窗口内,必须保留。
    /// 突变「无条件清待命」会让窗口内丢弃被静默遗漏,此测必红。
    #[test]
    fn rescan_debounce_covered_only_by_floor() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        d.request(t0, 42);
        assert!(!d.covered_by(41), "floor 小于丢弃 ID 不得核销");
        assert!(d.due(
            t0 + Duration::from_secs(3),
            Duration::from_secs(2),
            Duration::from_secs(60)
        ));
        assert!(d.covered_by(42), "floor 覆盖丢弃 ID 时核销");
        assert!(
            !d.due(
                t0 + Duration::from_secs(3),
                Duration::from_secs(2),
                Duration::from_secs(60)
            ),
            "核销后不再触发"
        );
    }

    /// 重复登记的丢弃 ID 取最大值:后续 covered_by 必须按最大值判定。
    #[test]
    fn rescan_debounce_keeps_max_drop_id() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        d.request(t0, 10);
        d.request(t0 + Duration::from_secs(1), 30);
        assert!(!d.covered_by(20), "最大 ID 30 > floor 20");
        assert!(d.covered_by(30));
    }

    /// 抢锁失败的重试节流:窗口内只允许一次尝试(防每 150ms 新建线程)。
    #[test]
    fn rescan_debounce_attempt_is_throttled() {
        let mut d = RescanDebounce::default();
        let t0 = Instant::now();
        let gap = Duration::from_millis(500);
        assert!(d.attempt_allowed(t0, gap), "首次允许");
        assert!(
            !d.attempt_allowed(t0 + Duration::from_millis(100), gap),
            "窗口内拒绝"
        );
        assert!(
            d.attempt_allowed(t0 + Duration::from_millis(600), gap),
            "窗口外允许"
        );
    }

    /// 触发条件组合:任一为假都不得触发(守卫被删时此测必红)。
    #[test]
    fn rescan_fire_requires_all_guards() {
        assert!(rescan_fire_allowed(false, true, true, true));
        assert!(
            !rescan_fire_allowed(true, true, true, true),
            "在途重扫不得触发"
        );
        assert!(
            !rescan_fire_allowed(false, false, true, true),
            "冷却中不得触发"
        );
        assert!(
            !rescan_fire_allowed(false, true, false, true),
            "未到期不得触发"
        );
        assert!(
            !rescan_fire_allowed(false, true, true, false),
            "重试窗口内不得触发"
        );
    }

    /// 查询遵循 daemon 配置，请求级 Some 可覆盖；total 同口径。
    #[test]
    fn run_query_honors_hidden_config_and_override() {
        let mut idx = Index::new();
        let root = idx.insert("/tmp/r", NO_PARENT, true);
        idx.insert(".hidden.txt", root, false);
        idx.insert("visible.txt", root, false);
        let state = Arc::new(EngineState::new());
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);
        state.cfg.lock_recover().include_hidden = Some(false);

        let total = |r: Response| match r {
            Response::Query { total, .. } => total,
            other => panic!("意外响应: {other:?}"),
        };
        assert_eq!(
            total(run_query(&state, "*", 10, None, false, false, None)),
            2,
            "配置 false：根 + visible.txt"
        );
        assert_eq!(
            total(run_query(&state, "*", 10, None, false, false, Some(true))),
            3,
            "请求级 true 覆盖配置"
        );

        // 升级用户方向:配置 true 时 None 跟随 true,请求级 false 可覆盖。
        state.cfg.lock_recover().include_hidden = Some(true);
        assert_eq!(
            total(run_query(&state, "*", 10, None, false, false, None)),
            3,
            "配置 true：三项全含"
        );
        assert_eq!(
            total(run_query(&state, "*", 10, None, false, false, Some(false))),
            2,
            "请求级 false 覆盖配置"
        );
    }

    /// HiddenSet 真方向回环:内存/落盘/同 dispatch 查询即时生效,HiddenPrefs
    /// 回显,请求级 Some(false) 仍可覆盖配置 true。
    #[test]
    fn hidden_set_roundtrip_drives_queries_and_echoes() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = DaemonPaths {
            data_dir: tmp.path().join("data"),
        };
        let mut idx = Index::new();
        let root = idx.insert("/tmp/r", NO_PARENT, true);
        idx.insert(".hidden.txt", root, false);
        idx.insert("visible.txt", root, false);
        let state = Arc::new(EngineState::new());
        *state.holder.write_fail_fast("holder") = IndexHolder::Mutable(idx);

        let query = |include_hidden| {
            dispatch(
                Request::Query {
                    v: protocol::PROTOCOL_VERSION,
                    query: "*".into(),
                    limit: 10,
                    sort: None,
                    desc: None,
                    dir_only: None,
                    include_hidden,
                },
                &state,
                &paths,
            )
        };
        let total = |r: Response| match r {
            Response::Query { total, .. } => total,
            other => panic!("意外响应: {other:?}"),
        };

        // false 方向:回显 + 内存 + 落盘 + 同 dispatch 查询立即生效。
        match dispatch(
            Request::HiddenSet {
                v: protocol::PROTOCOL_VERSION,
                include: false,
            },
            &state,
            &paths,
        ) {
            Response::HiddenPrefs { include, message } => {
                assert!(!include, "应回显 false");
                assert!(message.is_none(), "落盘成功不应带错误:{message:?}");
            }
            other => panic!("意外响应: {other:?}"),
        }
        assert_eq!(state.cfg.lock_recover().include_hidden, Some(false));
        assert_eq!(
            config::load(&paths.config_file()).include_hidden,
            Some(false),
            "偏好须落盘"
        );
        assert_eq!(total(query(None)), 2, "查询跟随配置 false");

        // true 方向:回显不得是改前默认(false)。
        match dispatch(
            Request::HiddenSet {
                v: protocol::PROTOCOL_VERSION,
                include: true,
            },
            &state,
            &paths,
        ) {
            Response::HiddenPrefs { include, message } => {
                assert!(include, "应回显真方向 true");
                assert!(message.is_none());
            }
            other => panic!("意外响应: {other:?}"),
        }
        assert_eq!(state.cfg.lock_recover().include_hidden, Some(true));
        assert_eq!(
            config::load(&paths.config_file()).include_hidden,
            Some(true),
            "true 偏好须落盘"
        );
        match dispatch(
            Request::HiddenPrefs {
                v: protocol::PROTOCOL_VERSION,
            },
            &state,
            &paths,
        ) {
            Response::HiddenPrefs { include, .. } => assert!(include, "读回应回显 true"),
            other => panic!("意外响应: {other:?}"),
        }
        assert_eq!(total(query(None)), 3, "查询跟随配置 true");
        assert_eq!(total(query(Some(false))), 2, "请求级 false 覆盖配置 true");
    }

    /// 落盘失败:内存仍临时生效,应答如实带 message。
    #[test]
    fn hidden_set_save_failure_still_applies_in_memory() {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        std::fs::write(&data, b"not a dir").unwrap();
        let paths = DaemonPaths { data_dir: data };
        let state = Arc::new(EngineState::new());
        match hidden_set(&state, &paths, true) {
            Response::HiddenPrefs { include, message } => {
                assert!(include, "内存已生效仍回 true");
                assert!(message.is_some(), "落盘失败须如实告知");
            }
            other => panic!("意外响应: {other:?}"),
        }
        assert_eq!(
            state.cfg.lock_recover().include_hidden,
            Some(true),
            "落盘失败仍临时生效"
        );
    }
}

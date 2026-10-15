//! Spike B:FSEvents sinceWhen 行为验证(tech.md ADR-5)。
//!
//! 回答一个问题:FSEvents 对「过旧 sinceWhen」是否有可观测信号。
//! 观测组:
//! ① `kFSEventStreamEventIdSinceNow` 建流 → 建 3 个文件 → 确认事件与 ID 递增;
//! ② `当前ID − 10亿` 重建流 → 观测历史事件、HistoryDone 时机、有无「日志已滚动」类
//!    flag、HistoryDone 后是否静默从 now 开始(以新建文件探针验证);
//! ②b 正面对照:`①首事件ID − 1` 重建流 → 应回放 ① 的事件,证明回放机制本身可用,
//!    从而区分 ② 的空结果到底是「日志已滚动」还是「该目录本无可回放历史」;
//! ③ 打印全部 flag 常量十六进制;各段事件逐条打印观测到的 flag。
//!
//! 跑法:`cargo run -p engine --example spike_fsevents`

use std::fs;
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use engine::fsevents::{current_event_id, start_stream, FseEvent, SINCE_NOW};
// fsevent-sys 的 flag 常量经 crate 根再导出(fsevent 模块私有)。
use fsevent_sys as fse;

/// 每文件级事件粒度。
const FILE_EVENTS: u32 = fse::kFSEventStreamCreateFlagFileEvents;
/// 等 HistoryDone 的上限(回放段结束标记;深回放实测可达 15s 级,留足余量)。
const HISTORY_DONE_TIMEOUT: Duration = Duration::from_secs(60);
/// ①(SinceNow)只验证「无 HistoryDone」,短等即可。
const SINCE_NOW_HISTORY_TIMEOUT: Duration = Duration::from_secs(5);
/// 静默判定:超过此时长无新事件即认为本段观测结束。
const QUIET: Duration = Duration::from_secs(3);

fn main() {
    let dir = std::env::temp_dir().join("mce-spike-fsevents");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("创建观察目录失败");
    let watch = fs::canonicalize(&dir).expect("canonicalize 失败");
    let watch_str = watch.to_str().expect("观察目录路径非 UTF-8");
    println!("观察目录 = {watch_str}");

    let obs1 = observe_since_now(watch_str, &dir);
    let obs1_events = obs1.as_ref().ok();
    if let Err(reason) = observe_too_old_since(watch_str, &dir, obs1_events) {
        println!("观测② 失败:{reason}");
    }
    if let Err(reason) = observe_positive_control(watch_str, &dir, obs1_events) {
        println!("观测②b 跳过:{reason}");
    }

    println!("\n=== 观测③ flag 常量表(十六进制)===");
    print_flag_table();
    println!("(各段观测到的 flag 已随事件逐条打印,见上)");

    let _ = fs::remove_dir_all(&dir);
    println!("\n清理完成:{dir:?} 已删除");
}

/// 观测①。返回文件段事件(供 ②/②b 做参照),失败返回原因。
fn observe_since_now(watch_str: &str, dir: &Path) -> Result<Vec<FseEvent>, String> {
    println!("\n=== 观测① SinceNow 建流 + 建 3 文件 ===");
    let base = current_event_id();
    println!("建流前 FSEventsGetCurrentEventId = {base} (0x{base:016x})");

    let t0 = Instant::now();
    let (mut stream, rx) = start_stream(&[watch_str], SINCE_NOW, 0.0, FILE_EVENTS)
        .map_err(|e| format!("建流失败:{e}"))?;
    let hist = recv_until_history_done(&rx, SINCE_NOW_HISTORY_TIMEOUT);
    println!(
        "HistoryDone 前事件数 = {},耗时 = {}ms",
        hist.len(),
        t0.elapsed().as_millis()
    );
    print_events("① 初始段", &hist);

    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(dir.join(name), b"fsevents-spike").map_err(|e| format!("建文件失败:{e}"))?;
    }
    println!("已建 3 个文件,等待事件…");
    let events = recv_quiet(&rx);
    print_events("① 文件段", &events);

    let ids: Vec<u64> = events
        .iter()
        .filter(|e| e.path.ends_with(".txt"))
        .map(|e| e.id)
        .collect();
    let increasing = ids.windows(2).all(|w| w[0] < w[1]);
    println!(
        "① 判定:txt 事件数 = {},ids = {ids:?},严格递增 = {increasing}",
        ids.len()
    );

    stream.stop();
    Ok(events)
}

/// 观测② 组:过旧 sinceWhen 的多点探测。任务规定「当前 ID 减 10 亿」;本机事件 ID
/// 总量不足 10 亿,saturating_sub 饱和到 0(= 自日志纪元起)。另加 −10⁷(中度过旧)、
/// since = 1 与 since = 当前ID÷2(数值上大概率低于日志起点的「陈旧区」),以
/// 「① ID 命中率 + 滚动/丢弃 flag 有无」共同判别行为。
fn observe_too_old_since(
    watch_str: &str,
    dir: &Path,
    obs1: Option<&Vec<FseEvent>>,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_id = current_event_id();
    let obs1_ids: Vec<u64> = obs1
        .map(|events| events.iter().map(|e| e.id).collect())
        .unwrap_or_default();
    println!("\n(② 组期望参照:① 的事件 id 集 = {obs1_ids:?};历史段命中即说明日志覆盖到该深度)");

    // 时间反查 API 探测(本机实测恒返回 0,不可用于定位日志起点,留作证据)。
    let dev = fs::metadata(dir)
        .map_err(|e| format!("取 dev_t 失败:{e}"))?
        .dev() as libc::dev_t;
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as f64;
    // CFAbsoluteTime 纪元 = 2001-01-01 00:00:00 GMT,早于 Unix 纪元 978307200 秒。
    let cf_now = unix_secs - 978_307_200.0;
    let last_id_days_ago = |days: f64| unsafe {
        fse::FSEventsGetLastEventIdForDeviceBeforeTime(dev, cf_now - days * 86_400.0)
    };
    println!(
        "(FSEventsGetLastEventIdForDeviceBeforeTime 探测:now = {},30 天前 = {},365 天前 = {},dev = {dev};恒 0 即该 API 在本机不可用)",
        last_id_days_ago(0.0),
        last_id_days_ago(30.0),
        last_id_days_ago(365.0),
    );

    let probes: [(String, u64); 4] = [
        (
            "观测② since = 当前ID−10亿".into(),
            now_id.saturating_sub(1_000_000_000),
        ),
        (
            "观测②a since = 当前ID−1000万".into(),
            now_id.saturating_sub(10_000_000),
        ),
        ("观测②d since = 1(最小非零 ID,陈旧区)".into(), 1),
        (
            "观测②e since = 当前ID÷2(数值上大概率低于日志起点)".into(),
            now_id / 2,
        ),
    ];
    for (label, since) in probes {
        observe_with_since(&label, watch_str, dir, since, &obs1_ids)?;
    }
    Ok(())
}

/// 观测②b:正面对照,since = ① 首个事件 ID − 1,应回放 ① 全部事件。
fn observe_positive_control(
    watch_str: &str,
    dir: &Path,
    obs1: Option<&Vec<FseEvent>>,
) -> Result<(), String> {
    let events = obs1.ok_or("① 无事件(或①失败)")?;
    let first_id = events.iter().map(|e| e.id).min().ok_or("① 事件集为空")?;
    let expect: Vec<u64> = events.iter().map(|e| e.id).collect();
    observe_with_since(
        "观测②b 正面对照 since = ①首事件ID−1",
        watch_str,
        dir,
        first_id - 1,
        &expect,
    )
}

/// ②/②b 共用骨架:`since` 建流 → 等 HistoryDone(记录历史段)→ 建 `probe.txt`
/// 探针验证 HistoryDone 后流是否存活 → 静默等待。`expect_ids` 是用于判定历史段
/// 命中情况的参照 ID 集(可为空)。
fn observe_with_since(
    label: &str,
    watch_str: &str,
    dir: &Path,
    since: u64,
    expect_ids: &[u64],
) -> Result<(), String> {
    println!("\n=== {label} ===");
    let now_id = current_event_id();
    let behind = now_id.saturating_sub(since);
    println!(
        "建流前 FSEventsGetCurrentEventId = {now_id} (0x{now_id:016x});sinceWhen = {since} (0x{since:016x}),落后 {behind} 个事件 ID"
    );

    let t0 = Instant::now();
    let (mut stream, rx) =
        start_stream(&[watch_str], since, 0.0, FILE_EVENTS).map_err(|e| format!("建流失败:{e}"))?;
    let hist = recv_until_history_done(&rx, HISTORY_DONE_TIMEOUT);
    println!(
        "HistoryDone 前事件数 = {},耗时 = {}ms",
        hist.len(),
        t0.elapsed().as_millis()
    );
    print_events(&format!("{label} 历史段"), &hist);

    const DROP_MASK: u32 = fse::kFSEventStreamEventFlagMustScanSubDirs
        | fse::kFSEventStreamEventFlagUserDropped
        | fse::kFSEventStreamEventFlagKernelDropped
        | fse::kFSEventStreamEventFlagEventIdsWrapped;
    let drop_events: Vec<(String, u32)> = hist
        .iter()
        .filter(|e| e.flags & DROP_MASK != 0)
        .map(|e| (e.path.clone(), e.flags))
        .collect();
    let hist_ids: Vec<u64> = hist.iter().map(|e| e.id).collect();
    let hit: Vec<u64> = expect_ids
        .iter()
        .filter(|id| hist_ids.contains(id))
        .copied()
        .collect();
    println!(
        "{label} 判定:历史段事件数 = {};含滚动/丢弃类 flag = {}({drop_events:?});参照 ID 命中 {}/{}",
        hist.len(),
        !drop_events.is_empty(),
        hit.len(),
        expect_ids.len()
    );

    fs::write(dir.join("probe.txt"), b"probe").map_err(|e| format!("建探针文件失败:{e}"))?;
    let after = recv_quiet(&rx);
    print_events(&format!("{label} 探针段"), &after);
    let probe = after.iter().find(|e| e.path.ends_with("probe.txt"));
    println!(
        "{label} 判定:HistoryDone 后流存活(收到 probe 事件)= {};probe 事件 id > sinceWhen = {}",
        probe.is_some(),
        probe.map(|e| e.id > since).unwrap_or(false)
    );

    stream.stop();
    Ok(())
}

/// 收事件直到出现 HistoryDone(含 HistoryDone 本身),或超时。
fn recv_until_history_done(rx: &Receiver<FseEvent>, timeout: Duration) -> Vec<FseEvent> {
    let deadline = Instant::now() + timeout;
    let mut got = Vec::new();
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            println!("!! 超时未等到 HistoryDone");
            break;
        }
        match rx.recv_timeout(remain) {
            Ok(e) => {
                let done = e.flags & fse::kFSEventStreamEventFlagHistoryDone != 0;
                got.push(e);
                if done {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                println!("!! 超时未等到 HistoryDone(本段事件已全部列出)");
                break;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    got
}

/// 收事件直到静默 `QUIET`。
fn recv_quiet(rx: &Receiver<FseEvent>) -> Vec<FseEvent> {
    let mut got = Vec::new();
    while let Ok(e) = rx.recv_timeout(QUIET) {
        got.push(e);
    }
    got
}

/// 逐条打印:id(十进制+十六进制)、flags(十六进制+解码)、路径。
fn print_events(label: &str, events: &[FseEvent]) {
    if events.is_empty() {
        println!("[{label}] (无事件)");
        return;
    }
    for (i, e) in events.iter().enumerate() {
        println!(
            "[{label}] #{i} id = {0} (0x{1:016x}) flags = 0x{2:08x} [{3}] path = {4}",
            e.id,
            e.id,
            e.flags,
            flag_names(e.flags),
            e.path
        );
    }
}

/// flag 位 → 名称;含未知位的残量以 hex 标注。
fn flag_names(flags: u32) -> String {
    if flags == 0 {
        return "None".into();
    }
    const TABLE: [(u32, &str); 23] = [
        (
            fse::kFSEventStreamEventFlagMustScanSubDirs,
            "MustScanSubDirs",
        ),
        (fse::kFSEventStreamEventFlagUserDropped, "UserDropped"),
        (fse::kFSEventStreamEventFlagKernelDropped, "KernelDropped"),
        (
            fse::kFSEventStreamEventFlagEventIdsWrapped,
            "EventIdsWrapped",
        ),
        (fse::kFSEventStreamEventFlagHistoryDone, "HistoryDone"),
        (fse::kFSEventStreamEventFlagRootChanged, "RootChanged"),
        (fse::kFSEventStreamEventFlagMount, "Mount"),
        (fse::kFSEventStreamEventFlagUnmount, "Unmount"),
        (fse::kFSEventStreamEventFlagItemCreated, "ItemCreated"),
        (fse::kFSEventStreamEventFlagItemRemoved, "ItemRemoved"),
        (
            fse::kFSEventStreamEventFlagItemInodeMetaMod,
            "ItemInodeMetaMod",
        ),
        (fse::kFSEventStreamEventFlagItemRenamed, "ItemRenamed"),
        (fse::kFSEventStreamEventFlagItemModified, "ItemModified"),
        (
            fse::kFSEventStreamEventFlagItemFinderInfoMod,
            "ItemFinderInfoMod",
        ),
        (
            fse::kFSEventStreamEventFlagItemChangeOwner,
            "ItemChangeOwner",
        ),
        (fse::kFSEventStreamEventFlagItemXattrMod, "ItemXattrMod"),
        (fse::kFSEventStreamEventFlagItemIsFile, "ItemIsFile"),
        (fse::kFSEventStreamEventFlagItemIsDir, "ItemIsDir"),
        (fse::kFSEventStreamEventFlagItemIsSymlink, "ItemIsSymlink"),
        (fse::kFSEventStreamEventFlagOwnEvent, "OwnEvent"),
        (fse::kFSEventStreamEventFlagItemIsHardlink, "ItemIsHardlink"),
        (
            fse::kFSEventStreamEventFlagItemIsLastHardlink,
            "ItemIsLastHardlink",
        ),
        (fse::kFSEventStreamEventFlagItemCloned, "ItemCloned"),
    ];
    let mut names: Vec<String> = Vec::new();
    let mut known_mask = 0u32;
    for (bit, name) in TABLE {
        known_mask |= bit;
        if flags & bit != 0 {
            names.push(name.to_owned());
        }
    }
    let unknown = flags & !known_mask;
    if unknown != 0 {
        names.push(format!("Unknown(0x{unknown:08x})"));
    }
    if names.is_empty() {
        return "UnknownBits".into();
    }
    names.join("|")
}

/// 观测③:打印 flag 常量表。
fn print_flag_table() {
    let table: [(&str, u32); 23] = [
        (
            "MustScanSubDirs",
            fse::kFSEventStreamEventFlagMustScanSubDirs,
        ),
        ("UserDropped", fse::kFSEventStreamEventFlagUserDropped),
        ("KernelDropped", fse::kFSEventStreamEventFlagKernelDropped),
        (
            "EventIdsWrapped",
            fse::kFSEventStreamEventFlagEventIdsWrapped,
        ),
        ("HistoryDone", fse::kFSEventStreamEventFlagHistoryDone),
        ("RootChanged", fse::kFSEventStreamEventFlagRootChanged),
        ("Mount", fse::kFSEventStreamEventFlagMount),
        ("Unmount", fse::kFSEventStreamEventFlagUnmount),
        ("ItemCreated", fse::kFSEventStreamEventFlagItemCreated),
        ("ItemRemoved", fse::kFSEventStreamEventFlagItemRemoved),
        (
            "ItemInodeMetaMod",
            fse::kFSEventStreamEventFlagItemInodeMetaMod,
        ),
        ("ItemRenamed", fse::kFSEventStreamEventFlagItemRenamed),
        ("ItemModified", fse::kFSEventStreamEventFlagItemModified),
        (
            "ItemFinderInfoMod",
            fse::kFSEventStreamEventFlagItemFinderInfoMod,
        ),
        (
            "ItemChangeOwner",
            fse::kFSEventStreamEventFlagItemChangeOwner,
        ),
        ("ItemXattrMod", fse::kFSEventStreamEventFlagItemXattrMod),
        ("ItemIsFile", fse::kFSEventStreamEventFlagItemIsFile),
        ("ItemIsDir", fse::kFSEventStreamEventFlagItemIsDir),
        ("ItemIsSymlink", fse::kFSEventStreamEventFlagItemIsSymlink),
        ("OwnEvent", fse::kFSEventStreamEventFlagOwnEvent),
        ("ItemIsHardlink", fse::kFSEventStreamEventFlagItemIsHardlink),
        (
            "ItemIsLastHardlink",
            fse::kFSEventStreamEventFlagItemIsLastHardlink,
        ),
        ("ItemCloned", fse::kFSEventStreamEventFlagItemCloned),
    ];
    for (name, value) in table {
        println!("  {name:20} = 0x{value:08x}");
    }
}

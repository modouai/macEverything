//! FSEvents 实机集成测试:需真实文件系统事件与 runloop,常规 `cargo test` 跳过。
//! 实机验证:`cargo test -p engine --test fsevents_live -- --ignored`
use engine::fsevents::{self, FseEvent};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// 在超时窗口内按路径后缀收集事件;每 500ms 窥视窗口避免长阻塞无法提前返回。
fn collect_event(
    rx: &Receiver<FseEvent>,
    name_suffix: &str,
    timeout: Duration,
) -> Option<FseEvent> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.min(Duration::from_millis(500))) {
            Ok(ev) => {
                if ev.path.ends_with(name_suffix) {
                    return Some(ev);
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

fn open_stream(paths: &[String], since: u64) -> (fsevents::FseStream, Receiver<FseEvent>) {
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    fsevents::start_stream(&refs, since, 0.05, fsevents::FILE_EVENTS).expect("建流失败")
}

/// 观测 ①(Task 3 spike 复刻为回归钉):SinceNow 建流 → 建文件 → 收到事件且 ID 递增。
#[test]
#[ignore = "需真实 FS 事件与 runloop,实机验证"]
fn live_events_from_since_now() {
    let dir = tempfile::tempdir().unwrap();
    let paths = vec![dir.path().to_string_lossy().into_owned()];
    let (mut stream, rx) = open_stream(&paths, fsevents::SINCE_NOW);
    std::thread::sleep(Duration::from_millis(300)); // 等 schedule/start 落定

    std::fs::write(dir.path().join("x.txt"), b"hi").unwrap();
    let ev = collect_event(&rx, "x.txt", Duration::from_secs(5)).expect("5s 内未收到 x.txt 事件");
    assert!(ev.id > 0, "事件 ID 应为正");
    assert!(
        stream.latest_event_id() >= ev.id,
        "流最新 ID({})应不小于末事件 ID({})",
        stream.latest_event_id(),
        ev.id
    );
    stream.stop();
    // stop 幂等
    stream.stop();
}

/// 观测 ③(目录级事件语义钉,生产建流参数):不设 FileEvents 时,一次文件写入
/// 上报的是**所在目录**,而非文件路径。生产增量管线依赖这一点(daemon::event_dir),
/// 若系统语义变化,此测先红。
#[test]
#[ignore = "需真实 FS 事件与 runloop,实机验证"]
fn live_dir_events_report_directories() {
    let dir = tempfile::tempdir().unwrap();
    let canon = dir.path().canonicalize().unwrap();
    let sub = canon.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let paths = [canon.to_string_lossy().into_owned()];
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let (mut stream, rx) =
        fsevents::start_stream(&refs, fsevents::SINCE_NOW, 0.05, fsevents::DIR_EVENTS)
            .expect("建流失败");
    std::thread::sleep(Duration::from_millis(300));

    std::fs::write(sub.join("x.txt"), b"hi").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_dir = false;
    let mut saw_file = false;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ev) => {
                let p = std::path::PathBuf::from(&ev.path);
                let p = p.canonicalize().unwrap_or(p);
                if p == sub {
                    saw_dir = true;
                    break;
                }
                if p.ends_with("x.txt") {
                    saw_file = true;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    stream.stop();
    assert!(saw_dir, "目录级事件应上报变更所在目录:{}", sub.display());
    assert!(!saw_file, "未设 FileEvents 时不应上报文件路径");
}

/// 压力观测:单目录 20k 次建/删,统计收到的事件数与丢弃标记。
/// 用途:区分「事件量本身大」与「回调消费不过来」——若同目录风暴仍逐事件上报,
/// 目录级 flag 并不能降低事件量,瓶颈在消费侧。
#[test]
#[ignore = "需真实 FS 事件与 runloop,实机验证"]
fn live_dir_event_volume_and_drops() {
    let dir = tempfile::tempdir().unwrap();
    let canon = dir.path().canonicalize().unwrap();
    let paths = [canon.to_string_lossy().into_owned()];
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let (mut stream, rx) =
        fsevents::start_stream(&refs, fsevents::SINCE_NOW, 0.05, fsevents::DIR_EVENTS)
            .expect("建流失败");
    std::thread::sleep(Duration::from_millis(300));

    let sub = canon.join("storm");
    std::fs::create_dir(&sub).unwrap();
    // 与真实风暴同形:先批量建、再批量删(两段各产生一轮事件),目录数可调,
    // 用于区分「事件总量」与「目录多样性」对 user_dropped 的影响。
    let dirs: usize = std::env::var("MCE_STORM_DIRS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let rounds: usize = std::env::var("MCE_STORM_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let per_dir: usize = std::env::var("MCE_STORM_PER_DIR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000);
    let writer = {
        let sub = sub.clone();
        std::thread::spawn(move || {
            for r in 0..rounds {
                let mut paths = Vec::with_capacity(dirs * per_dir);
                for d in 0..dirs {
                    let dir = sub.join(format!("d{d}"));
                    std::fs::create_dir_all(&dir).unwrap();
                    for i in 0..per_dir {
                        let p = dir.join(format!("f{r}_{i}"));
                        std::fs::write(&p, b"x").unwrap();
                        paths.push(p);
                    }
                }
                for p in &paths {
                    let _ = std::fs::remove_file(p);
                }
            }
        })
    };

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut events = 0u64;
    let mut drops = 0u64;
    let mut file_paths = 0u64;
    let mut samples: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(ev) => {
                events += 1;
                if samples.len() < 5 {
                    samples.push(format!("{} flags={:#x}", ev.path, ev.flags));
                }
                if fsevents::is_dropped(ev.flags) {
                    drops += 1;
                }
                if ev.path.ends_with("storm") {
                    // 目录级事件
                } else if ev.path.contains("/storm/") {
                    file_paths += 1;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    writer.join().unwrap();
    stream.stop();
    eprintln!("事件数={events} 丢弃={drops} 文件路径事件={file_paths}");
    for s in &samples {
        eprintln!("样本: {s}");
    }
    assert!(events > 0, "20k 次文件操作至少应产生一条事件");
}

/// 重放:以旧事件 ID 为 since_when,历史段应包含目标文件事件,并以 HistoryDone(0x10)收尾。
#[test]
#[ignore = "需真实 FS 事件与 runloop,实机验证"]
fn replay_since_old_id_yields_history_done() {
    let dir = tempfile::tempdir().unwrap();
    let before = fsevents::current_event_id();
    std::fs::write(dir.path().join("replay-probe.txt"), b"").unwrap();
    // 留出事件落日志的时间(重放读取的是持久化日志 .fseventsd)
    std::thread::sleep(Duration::from_millis(500));

    let paths = vec![dir.path().to_string_lossy().into_owned()];
    let (mut stream, rx) = open_stream(&paths, before);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut saw_probe = false;
    let mut saw_history_done = false;
    while !saw_history_done && Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(ev) => {
                if ev.path.ends_with("replay-probe.txt") {
                    saw_probe = true;
                }
                if fsevents::is_history_done(ev.flags) {
                    saw_history_done = true;
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(saw_probe, "重放段应含停机窗口内的历史事件");
    assert!(saw_history_done, "重放段应以 HistoryDone 标记收尾");
    stream.stop();
}

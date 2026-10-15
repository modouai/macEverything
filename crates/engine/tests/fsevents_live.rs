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

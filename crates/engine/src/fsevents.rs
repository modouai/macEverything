//! FSEvents C API 直调封装雏形(ADR-5)。
//!
//! 本文件为 Task 3 spike 产物:只做「stream 创建 + 回调 + 事件收集」最小封装,
//! 以经典 runloop 接线(`FSEventStreamScheduleWithRunLoop` + 专用线程 `CFRunLoopRun`)。
//! spike 以行为观测为先,封装形态待 watcher/replay 任务按需演化。
//!
//! 回调第 4 参 `eventPaths` 的语义(SDK FSEvents.h,`FSEventStreamCallback` 文档):
//! 创建流时设 `kFSEventStreamCreateFlagUseCFTypes`,则它是 `CFArrayRef`,
//! 元素为 `CFStringRef`;不设则是裸 `char **` 数组。本封装恒设该 flag,
//! 因此恒走 `CFArrayGetValueAtIndex` + `CFStringGetCString` 提取路径。
//!
//! 生命周期归属:runloop 线程拥有 stream 的 schedule/start/stop/invalidate/release;
//! 属主线程只负责唤醒 runloop 与回收回调上下文。拆除动作(含 `FSEventStreamInvalidate`)
//! 在流所调度的 runloop 线程上执行——spike 实测从属主线程跨线程 Invalidate 会触发
//! CoreFoundation 陷阱(SIGTRAP)。

use std::ffi::{c_char, CStr, CString};
use std::os::raw::c_void;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::{Builder, JoinHandle};

// fsevent-sys 的 `fsevent` 模块私有,全部项经 crate 根 `pub use` 再导出;
// `core_foundation` 模块本身公开。
use fsevent_sys as fs;
use fsevent_sys::core_foundation as cf;

/// 事件 ID(`FSEventStreamEventId`),全局单调递增,跨重启保持。
pub type EventId = u64;

/// 事件 flag 位集(`FSEventStreamEventFlags`),位义见 `fsevent_sys` 根的
/// `kFSEventStreamEventFlag*` 常量。
pub type EventFlags = u32;

/// 单条 FSEvents 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FseEvent {
    /// 事件路径(流创建时设了 `FileEvents` 则为具体文件,否则为目录)。
    pub path: String,
    /// flag 位集,原样透传。
    pub flags: EventFlags,
    /// 该事件的事件 ID。
    pub id: EventId,
}

/// `kFSEventStreamEventIdSinceNow`:建流时表示「从现在开始」。
pub const SINCE_NOW: EventId = fs::kFSEventStreamEventIdSinceNow;

/// 最近一条已生成的全局事件 ID(`FSEventsGetCurrentEventId`)。
#[must_use]
pub fn current_event_id() -> EventId {
    unsafe { fs::FSEventsGetCurrentEventId() }
}

struct CallbackCtx {
    tx: Sender<FseEvent>,
}

/// 裸指针跨线程转移的私有包装:仅用于把 stream / runloop 句柄移交或收回,
/// 句柄全程单线程独占,不并发共享。
#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

/// 回调:批量取事件逐条发往通道。字符串提取失败(理论不可达)以空路径留痕,
/// 不让单条事件异常中断整批。
extern "C" fn on_events(
    // fsevent-sys 的回调 typedef 首参为 FSEventStreamRef(*mut),C 头文件的
    // ConstFSEventStreamRef 在该 crate 里同指 c_void,随 crate 签名。
    _stream: fs::FSEventStreamRef,
    info: *mut c_void,
    num_events: usize,
    event_paths: *mut c_void,
    event_flags: *const fs::FSEventStreamEventFlags,
    event_ids: *const fs::FSEventStreamEventId,
) {
    let ctx = unsafe { &*(info as *const CallbackCtx) };
    let paths = event_paths as cf::CFArrayRef;
    for i in 0..num_events {
        let (path, flags, id) = unsafe {
            let value = cf::CFArrayGetValueAtIndex(paths, i as cf::CFIndex);
            let mut buf = [0 as c_char; 4096];
            let decoded = cf::CFStringGetCString(
                value as cf::CFStringRef,
                buf.as_mut_ptr(),
                buf.len() as cf::CFIndex,
                cf::kCFStringEncodingUTF8,
            );
            let path = if decoded {
                CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned()
            } else {
                String::new()
            };
            (path, *event_flags.add(i), *event_ids.add(i))
        };
        // 接收端已关闭(观测结束)时丢弃,不 panic 回调线程。
        let _ = ctx.tx.send(FseEvent { path, flags, id });
    }
}

/// 运行中的 FSEvents 流:封装 create → schedule(专用线程 runloop)→ start;
/// `stop()`/`Drop` 唤醒 runloop 后由该线程完成 Stop/Invalidate/Release,属主 join 后回收上下文。
pub struct FseStream {
    run_loop: cf::CFRunLoopRef,
    ctx: *mut CallbackCtx,
    worker: Option<JoinHandle<()>>,
}

// SAFETY:run_loop / ctx / worker 由本结构独占拥有,对外只暴露 `stop()`;
// `CFRunLoopStop` 官方文档保证可跨线程调用;ctx 在 join 回调线程之后才回收。
unsafe impl Send for FseStream {}

/// 建流失败(创建返回空句柄,或 `FSEventStreamStart` 失败)。
#[derive(Debug)]
pub struct StreamStartError(pub String);

impl std::fmt::Display for StreamStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FSEventStream 启动失败:{}", self.0)
    }
}

impl std::error::Error for StreamStartError {}

/// 创建并启动一条 FSEvents 流。
///
/// * `paths`:要监视的绝对路径(经 `std::fs::canonicalize` 归一后再传入为佳)。
/// * `since_when`:起始事件 ID;`SINCE_NOW` 表示从现在开始。
/// * `latency`:事件合批延迟(秒)。
/// * `extra_flags`:附加创建 flag(如 `kFSEventStreamCreateFlagFileEvents`);
///   `kFSEventStreamCreateFlagUseCFTypes` 由本函数强制附加,回调依赖它。
///
/// 返回事件接收端;接收端被丢弃后回调自动丢弃事件,不阻塞。
pub fn start_stream(
    paths: &[&str],
    since_when: EventId,
    latency: f64,
    extra_flags: fs::FSEventStreamCreateFlags,
) -> Result<(FseStream, Receiver<FseEvent>), StreamStartError> {
    let (tx, rx) = channel();
    // Box 回调上下文,裸指针交给 FSEventStreamContext.info;stop() 在 join 后回收。
    let ctx: *mut CallbackCtx = Box::into_raw(Box::new(CallbackCtx { tx }));

    let c_paths: Vec<CString> = paths
        .iter()
        .map(|p| CString::new(*p).expect("路径含 NUL"))
        .collect();
    let stream = unsafe {
        let array =
            cf::CFArrayCreateMutable(cf::kCFAllocatorDefault, 0, &cf::kCFTypeArrayCallBacks);
        for p in &c_paths {
            let cf_str = cf::CFStringCreateWithCString(
                cf::kCFAllocatorDefault,
                p.as_ptr(),
                cf::kCFStringEncodingUTF8,
            );
            cf::CFArrayAppendValue(array, cf_str);
            cf::CFRelease(cf_str);
        }
        let context = fs::FSEventStreamContext {
            version: 0,
            info: ctx.cast::<c_void>(),
            retain: None,
            release: None,
            copy_description: None,
        };
        fs::FSEventStreamCreate(
            cf::kCFAllocatorDefault,
            on_events,
            &context,
            array,
            since_when,
            latency,
            extra_flags | fs::kFSEventStreamCreateFlagUseCFTypes,
        )
    };
    if stream.is_null() {
        unsafe { drop(Box::from_raw(ctx)) };
        return Err(StreamStartError("FSEventStreamCreate 返回空句柄".into()));
    }

    // 专用线程跑 runloop:schedule + start 在该线程做,拆除也在该线程做;
    // 就绪(或失败)后回传 runloop 句柄。
    let (ready_tx, ready_rx) = channel::<Result<SendPtr, String>>();
    // 先整体绑定再进闭包:避免闭包按精确路径捕获裸指针字段,绕过 SendPtr 的 Send 实现。
    let stream_ptr = SendPtr(stream);
    let worker = Builder::new()
        .name("fsevents-runloop".into())
        .spawn(move || {
            let sp = stream_ptr;
            let stream = sp.0;
            let run_loop = unsafe { cf::CFRunLoopGetCurrent() };
            unsafe {
                fs::FSEventStreamScheduleWithRunLoop(stream, run_loop, cf::kCFRunLoopDefaultMode);
            }
            let started = unsafe { fs::FSEventStreamStart(stream) };
            if started == 0 {
                // start 失败:schedule 过的源要 invalidate,再 release,回报后退出。
                unsafe {
                    fs::FSEventStreamInvalidate(stream);
                    fs::FSEventStreamRelease(stream);
                }
                let _ = ready_tx.send(Err("FSEventStreamStart 返回 0".into()));
                return;
            }
            if ready_tx.send(Ok(SendPtr(run_loop))).is_err() {
                // 属主已放弃等待:自行收尾,防悬挂。
                unsafe {
                    cf::CFRunLoopStop(run_loop);
                }
            }
            // 阻塞至 stop() 调 CFRunLoopStop;退出后在源所在线程完成拆除。
            unsafe { cf::CFRunLoopRun() };
            unsafe {
                fs::FSEventStreamStop(stream);
                fs::FSEventStreamInvalidate(stream);
                fs::FSEventStreamRelease(stream);
            }
        })
        .expect("启动 fsevents-runloop 线程失败");

    match ready_rx
        .recv()
        .expect("fsevents-runloop 线程在回报就绪前退出")
    {
        Ok(run_loop) => Ok((
            FseStream {
                run_loop: run_loop.0,
                ctx,
                worker: Some(worker),
            },
            rx,
        )),
        Err(reason) => {
            worker.join().expect("fsevents-runloop 线程 join 失败");
            unsafe { drop(Box::from_raw(ctx)) };
            Err(StreamStartError(reason))
        }
    }
}

impl FseStream {
    /// 停流:唤醒 runloop,拆除由 runloop 线程完成后 join,再回收回调上下文。
    /// 幂等;`Drop` 复用。
    pub fn stop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        unsafe { cf::CFRunLoopStop(self.run_loop) };
        worker.join().expect("fsevents-runloop 线程 join 失败");
        unsafe { drop(Box::from_raw(self.ctx)) };
    }
}

impl Drop for FseStream {
    fn drop(&mut self) {
        self.stop();
    }
}

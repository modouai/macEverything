//! FSEvents C API 直调封装定稿(ADR-5):stream 生命周期(runloop 专用线程)、
//! 事件收集、`sinceWhen` 历史重放、丢弃标记判定与事件 ID 追踪。
//! 全部 unsafe FFI 集中于本文件;实机验证见 `tests/fsevents_live.rs`,
//! spike 实证结论(HistoryDone 触发条件、无可观测滚动信号)见 tech.md ADR-5。
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

/// `kFSEventStreamEventFlagHistoryDone`:历史重放段结束标记(Task 3 spike 实证
/// 值为 0x10 并落定;重放追平判据,ADR-5)。
pub const FLAG_HISTORY_DONE: EventFlags = fs::kFSEventStreamEventFlagHistoryDone;

/// 便捷组合:文件级事件 + 使用 CFTypes 路径数组(本封装恒设 UseCFTypes)。
/// 仅测试/示例使用:生产增量管线只要目录级事件(见 [`DIR_EVENTS`])。
pub const FILE_EVENTS: fs::FSEventStreamCreateFlags = fs::kFSEventStreamCreateFlagFileEvents;

/// 目录级事件(不请求 FileEvents):`path` 是变更所在的目录,正是目录 diff
/// 管线需要的粒度。
///
/// 注意它**不减少事件条数**:实测 8 万次文件操作仍产生约 8 万条事件(每条
/// path 都指向所在目录)。风暴下的丢事件要靠消费侧兜底——回调热路径复用缓冲、
/// 丢弃标记去抖重扫;不要指望换 flag 降低事件量。
pub const DIR_EVENTS: fs::FSEventStreamCreateFlags = 0;

/// 事件是否携带历史重放结束标记。
#[must_use]
pub fn is_history_done(flags: EventFlags) -> bool {
    flags & FLAG_HISTORY_DONE != 0
}

/// 事件是否携带「丢弃」类标记(事件被系统合并丢弃,调用方应转全量重扫兜底)。
/// 位组合见 `fsevent_sys`:MustScanSubDirs/UserDropped/KernelDropped/EventIdsWrapped。
#[must_use]
pub fn is_dropped(flags: EventFlags) -> bool {
    flags & DROP_MASK != 0
}

const DROP_MASK: EventFlags = fs::kFSEventStreamEventFlagMustScanSubDirs
    | fs::kFSEventStreamEventFlagUserDropped
    | fs::kFSEventStreamEventFlagKernelDropped
    | fs::kFSEventStreamEventFlagEventIdsWrapped;

/// 丢弃类标记的逐位拆解(诊断/计数用)。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DropBits {
    pub must_scan: bool,
    pub user_dropped: bool,
    pub kernel_dropped: bool,
    pub ids_wrapped: bool,
}

/// 拆出四种丢弃位。处置相同(全量重扫),区分是为了归因:
/// `must_scan` 多为系统合并、`kernel_dropped` 指向事件缓冲溢出(事件量超过流
/// 消费速度),两者混在一起时无法判断该调事件粒度还是调消费能力。
#[must_use]
pub fn drop_bits(flags: EventFlags) -> DropBits {
    DropBits {
        must_scan: flags & fs::kFSEventStreamEventFlagMustScanSubDirs != 0,
        user_dropped: flags & fs::kFSEventStreamEventFlagUserDropped != 0,
        kernel_dropped: flags & fs::kFSEventStreamEventFlagKernelDropped != 0,
        ids_wrapped: flags & fs::kFSEventStreamEventFlagEventIdsWrapped != 0,
    }
}

/// 丢弃类标记的位名组成:见 [`drop_bits`];无丢弃位返回 `<none>`。
#[must_use]
pub fn drop_reason(flags: EventFlags) -> String {
    let b = drop_bits(flags);
    let mut names = Vec::new();
    if b.must_scan {
        names.push("must_scan");
    }
    if b.user_dropped {
        names.push("user_dropped");
    }
    if b.kernel_dropped {
        names.push("kernel_dropped");
    }
    if b.ids_wrapped {
        names.push("ids_wrapped");
    }
    if names.is_empty() {
        "<none>".to_string()
    } else {
        names.join("|")
    }
}

/// 最近一条已生成的全局事件 ID(`FSEventsGetCurrentEventId`)。
#[must_use]
pub fn current_event_id() -> EventId {
    // SAFETY:无参只读查询,纯函数式 C API,无指针/生命周期涉及。
    unsafe { fs::FSEventsGetCurrentEventId() }
}

struct CallbackCtx {
    tx: Sender<FseEvent>,
}

/// 裸指针跨线程转移的私有包装:仅用于把 stream / runloop 句柄移交或收回,
/// 句柄全程单线程独占,不并发共享。
#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
// SAFETY:SendPtr 只承载「所有权转移」语义(创建线程 → runloop 线程、或回收),
// 不提供共享访问;两个句柄(stream/runloop)在任一时刻只被一个线程持有,
// 转移后原线程不再触碰。CF 对象本身的线程契约(如 CFRunLoopStop 可跨线程)
// 在调用点另行论证。
unsafe impl Send for SendPtr {}

/// 回调:批量取事件逐条发往通道。字符串提取失败(理论不可达)以空路径留痕,
/// 不让单条事件异常中断整批。
///
/// 性能约束:这条路径在风暴期按事件计费(实测 10 万次文件操作 ≈ 10 万次回调
/// 迭代)。转换缓冲必须**提到循环外**复用——旧实现每个事件在栈上开 4KB 并清零,
/// 单批 8 万事件即 320MB memset,直接拖慢消费速度并触发 `user_dropped`。
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
    // SAFETY:info 即 FSEventStreamContext.info,由 start_stream 以 Box::into_raw
    // 装入,在流存活期间恒有效;回调由 CF 在 stream 所属 runloop 线程串行调用,
    // 与属主线程回收 ctx 的时间窗被「Stop+Invalidate 完毕 → join」隔开,无别名冲突。
    let ctx = unsafe { &*(info as *const CallbackCtx) };
    let paths = event_paths as cf::CFArrayRef;
    // 回调串行执行(CF runloop 线程),循环外复用的栈缓冲不会跨线程共享。
    let mut buf = [0 as c_char; 4096];
    for i in 0..num_events {
        // SAFETY:eventPaths 为 CFArrayRef(创建流恒设 UseCFTypes,见模块头)、
        // eventFlags/eventIds 为长度 num_events 的 C 数组(框架契约),
        // i < num_events 时索引合法;CFArrayGetValueAtIndex 借出的值在本次回调内有效。
        //
        // `s` 是由 eventPaths 借出的 CFStringRef,归属该数组、回调期间不会被释放。
        // `CFStringGetCStringPtr` 返回的指针归 `s` 所有、NUL 结尾,在 `s` 生命周期内
        // 有效;仅在 `!is_null()` 分支解引用并立即拷贝。`CFStringGetCString` 失败时
        // 缓冲区内容不可读,故只在返回 true 的分支读 `buf`。
        let (path, flags, id) = unsafe {
            let value = cf::CFArrayGetValueAtIndex(paths, i as cf::CFIndex);
            let s = value as cf::CFStringRef;
            // 快路径:CFString 内部已是 UTF-8 时直接借用指针(零拷贝);
            // 失败再走定长缓冲(路径长度上限 4095,超长截断留痕为空串)。
            let ptr = cf::CFStringGetCStringPtr(s, cf::kCFStringEncodingUTF8);
            let path = if !ptr.is_null() {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            } else if cf::CFStringGetCString(
                s,
                buf.as_mut_ptr(),
                buf.len() as cf::CFIndex,
                cf::kCFStringEncodingUTF8,
            ) {
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
    /// 流句柄副本,仅供属主线程做只读查询(`latest_event_id`);拆除动作仍全部
    /// 在 runloop 线程执行。
    stream: *mut c_void,
    ctx: *mut CallbackCtx,
    worker: Option<JoinHandle<()>>,
}

// SAFETY:run_loop / ctx / worker 由本结构独占拥有,对外只暴露 `stop()`(排他借用)
// 与 `latest_event_id()`(只读查询:CFRunLoopStop 与 FSEventStreamGetLatestEventId
// 官方文档均保证可跨线程调用,后者为纯读);ctx 在 join 回调线程之后才回收。
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
    // 路径先转换:错误路径须在 ctx Box 化**之前**返回,否则裸指针无人回收。
    let c_paths: Vec<CString> = paths
        .iter()
        .map(|p| {
            CString::new(*p).map_err(|_| StreamStartError(format!("监视路径含 NUL 字节: {p:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Box 回调上下文,裸指针交给 FSEventStreamContext.info;stop() 在 join 后回收。
    let ctx: *mut CallbackCtx = Box::into_raw(Box::new(CallbackCtx { tx }));
    // SAFETY:全程在属主线程、ctx 已 Box 化且未移交;CFArray/CFString 的 Create 遵循
    // 「Create 归还调用方所有权」惯例——数组与临时字符串按次 Release,数组本身移交
    // FSEventStreamCreate(创建后 CF 持有引用,无需再 Release)。context 为栈上值,
    // 仅本次调用读取;info 裸指针在流存活期内由 stop() 回收,与回调的生命周期论证一致。
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
        let s = fs::FSEventStreamCreate(
            cf::kCFAllocatorDefault,
            on_events,
            &context,
            array,
            since_when,
            latency,
            extra_flags | fs::kFSEventStreamCreateFlagUseCFTypes,
        );
        // CF 惯例:被调用方按需自持,调用方引用必释——流存活与否都由流自己 retain,
        // 此处无条件释放数组(修 spike 版每流一次的数组泄漏)。
        cf::CFRelease(array);
        s
    };
    if stream.is_null() {
        // SAFETY:建流失败,ctx 从未交给任何流,无回调可能;独占回收唯一所有权。
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
            // SAFETY:在本线程(即将成为流所调度的 runloop 线程)内取当前 runloop,
            // 返回值在本线程存活期内恒有效。
            let run_loop = unsafe { cf::CFRunLoopGetCurrent() };
            // SAFETY:stream 由本线程独占(所有权已随 SendPtr 移交);ScheduleWithRunLoop
            // 要求在与后续 CFRunLoopRun 同线程调用,此处即该线程。
            unsafe {
                fs::FSEventStreamScheduleWithRunLoop(stream, run_loop, cf::kCFRunLoopDefaultMode);
            }
            // SAFETY:stream 已 schedule 到本线程 runloop,调用契约满足。
            let started = unsafe { fs::FSEventStreamStart(stream) };
            if started == 0 {
                // start 失败:schedule 过的源要 invalidate,再 release,回报后退出。
                // SAFETY:拆除序列(Invalidate→Release)按 Apple 契约在调度线程执行;
                // 流已在 schedule 后取得 Release 的所有权(创建时归调用方,此处消费)。
                unsafe {
                    fs::FSEventStreamInvalidate(stream);
                    fs::FSEventStreamRelease(stream);
                }
                let _ = ready_tx.send(Err("FSEventStreamStart 返回 0".into()));
                return;
            }
            if ready_tx.send(Ok(SendPtr(run_loop))).is_err() {
                // 属主已放弃等待:自行收尾,防悬挂。
                // SAFETY:CFRunLoopStop 官方文档保证可从任意线程调用(对尚未 Run 的 loop
                // 无害,下一次 Run 立即返回)。
                unsafe {
                    cf::CFRunLoopStop(run_loop);
                }
            }
            // 阻塞至 stop() 调 CFRunLoopStop;退出后在源所在线程完成拆除。
            // SAFETY:本线程专属的 runloop,无其他 owner。
            unsafe { cf::CFRunLoopRun() };
            // SAFETY:Stop/Invalidate/Release 三连按契约在调度线程执行;每次 start_stream
            // 只走到这里一次(worker 单次执行),无双重释放。
            unsafe {
                fs::FSEventStreamStop(stream);
                fs::FSEventStreamInvalidate(stream);
                fs::FSEventStreamRelease(stream);
            }
        })
        .expect("启动 fsevents-runloop 线程失败");

    let ready = match ready_rx.recv() {
        Ok(r) => r,
        Err(_) => {
            // worker 在回报前退出(panic):join 回收线程,再回收 ctx 与 stream,报错而非连带 panic。
            let joined = worker.join();
            if joined.is_err() {
                log::warn!("fsevents-runloop 线程在回报就绪前 panic");
            }
            // SAFETY:worker 已终结,回调不可能再发生;独占回收 ctx 唯一所有权。
            unsafe { drop(Box::from_raw(ctx)) };
            // stream 已创建但从未交给任何人:此路径极罕见(worker 仅做 CF 调用,无 panic 源)。
            // 只做 CFRelease 减引用(引用计数线程安全);不调用 Stop/Invalidate——
            // 跨线程 Invalidate 实测 SIGTRAP(tech.md spike 要点),宁可留 runloop source
            // 随线程消亡回收,不与崩溃风险对赌。
            // SAFETY:stream 由本次调用独占,无其他持有者;Release 只减引用计数。
            unsafe { fs::FSEventStreamRelease(stream) };
            return Err(StreamStartError(
                "fsevents-runloop 线程在回报就绪前退出".into(),
            ));
        }
    };
    match ready {
        Ok(run_loop) => Ok((
            FseStream {
                run_loop: run_loop.0,
                stream,
                ctx,
                worker: Some(worker),
            },
            rx,
        )),
        Err(reason) => {
            let _ = worker.join();
            // SAFETY:同 ready Err 分支——start 失败路径 worker 已终结,回调不可能发生。
            unsafe { drop(Box::from_raw(ctx)) };
            Err(StreamStartError(reason))
        }
    }
}

impl FseStream {
    /// 流当前已处理到的事件 ID(`FSEventStreamGetLatestEventId`)。
    /// 供实机测试与诊断查询;daemon 的落盘水位(ADR-5 重放起点)另由消费线程
    /// 维护的「已应用水位」承担(见 cli/daemon.rs 的 applied_event_id)。
    /// 流已拆除后返回 0。
    #[must_use]
    pub fn latest_event_id(&self) -> EventId {
        if self.worker.is_none() {
            return 0;
        }
        // SAFETY:stream 句柄在进程内全局注册表有效(Apple 文档:该 API 可在任意线程
        // 查询本进程创建的流);worker 尚未拆除(上面已判),句柄有效。
        unsafe { fs::FSEventStreamGetLatestEventId(self.stream) }
    }

    /// 停流:唤醒 runloop,拆除由 runloop 线程完成后 join,再回收回调上下文。
    /// 幂等;`Drop` 复用。worker panic 时记日志继续回收(不连带 panic)。
    pub fn stop(&mut self) {
        let Some(worker) = self.worker.take() else {
            return;
        };
        // SAFETY:CFRunLoopStop 官方文档保证可跨线程调用;run_loop 在流存活期内有效
        //(由 worker 持有并调度,stop 只在 join 前调用)。
        unsafe { cf::CFRunLoopStop(self.run_loop) };
        if worker.join().is_err() {
            log::error!("fsevents-runloop 线程 panic,stream 可能未完整释放");
        }
        // SAFETY:worker 已终结(join 返回),回调不可能再发生;独占回收 ctx。
        unsafe { drop(Box::from_raw(self.ctx)) };
    }
}

impl Drop for FseStream {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 单一位取值钉:每个丢弃位单独喂入时只报自己;多位组合按固定顺序拼接。
    /// 突变「少判一位」或「顺序改变」时必红——诊断字段的用途就是区分
    /// must_scan(系统合并)与 kernel_dropped(缓冲溢出)。
    #[test]
    fn drop_reason_names_each_bit() {
        assert_eq!(
            drop_reason(fs::kFSEventStreamEventFlagMustScanSubDirs),
            "must_scan"
        );
        assert_eq!(
            drop_reason(fs::kFSEventStreamEventFlagUserDropped),
            "user_dropped"
        );
        assert_eq!(
            drop_reason(fs::kFSEventStreamEventFlagKernelDropped),
            "kernel_dropped"
        );
        assert_eq!(
            drop_reason(fs::kFSEventStreamEventFlagEventIdsWrapped),
            "ids_wrapped"
        );
        assert_eq!(
            drop_reason(
                fs::kFSEventStreamEventFlagKernelDropped
                    | fs::kFSEventStreamEventFlagMustScanSubDirs
            ),
            "must_scan|kernel_dropped"
        );
    }

    /// 非丢弃位(如 ItemCreated)不得被误报;空集返回 `<none>`。
    #[test]
    fn drop_reason_ignores_non_drop_bits() {
        assert_eq!(
            drop_reason(fs::kFSEventStreamEventFlagItemCreated),
            "<none>"
        );
        assert_eq!(drop_reason(0), "<none>");
        assert!(!is_dropped(fs::kFSEventStreamEventFlagItemCreated));
        assert!(is_dropped(fs::kFSEventStreamEventFlagKernelDropped));
    }

    /// 逐位拆解与字符串归因一致:计数与日志必须同源。
    #[test]
    fn drop_bits_matches_reason() {
        let flags =
            fs::kFSEventStreamEventFlagKernelDropped | fs::kFSEventStreamEventFlagUserDropped;
        let b = drop_bits(flags);
        assert!(!b.must_scan);
        assert!(b.user_dropped);
        assert!(b.kernel_dropped);
        assert!(!b.ids_wrapped);
        assert_eq!(drop_reason(flags), "user_dropped|kernel_dropped");
    }
}

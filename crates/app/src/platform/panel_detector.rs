//! 保存面板检测:特征快照 dump(探针) + 两级判定(快速 subrole 白名单 →
//! 确认可编辑输入框/按钮组) + 产品事件层(NSWorkspace 前台切换通知驱动,
//! 专用 runloop 线程换绑 AXObserver,mpsc 输出 [`PanelEvent`])。
//! 快照结构与 AX 树解耦,纯数据可序列化可测。
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use super::ax_ffi::{self, attr, note, AXObserverWrap, AXRef};
// 事件定义在跨平台模块(UI 接线层与单测共用);此处只需导入事件类型。
use super::panel_assist::PanelEvent;

/// 窗口特征快照(判定输入;与 AX 树解耦,纯数据可序列化可测)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PanelSnapshot {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub title: Option<String>,
    /// 子树内可写 AXTextField 数(深度限 6)。
    pub editable_text_fields: usize,
    /// 子树内 AXButton 数。
    pub buttons: usize,
    /// 子树内文件浏览容器(AXScrollArea/AXSplitGroup)数。
    /// 当前仅取证字段,不参与 `is_save_panel`;特征表修订时评估是否纳入确认信号。
    pub browsers: usize,
}

/// 从 AX 窗口元素采集特征快照(浅层遍历;AX IPC 逐元素往返,探针/检测入口各一次)。
pub fn snapshot_of_window(win: &AXRef) -> PanelSnapshot {
    let mut s = PanelSnapshot {
        role: ax_ffi::attr_string(win, attr::ROLE),
        subrole: ax_ffi::attr_string(win, attr::SUBROLE),
        title: ax_ffi::attr_string(win, attr::TITLE),
        editable_text_fields: 0,
        buttons: 0,
        browsers: 0,
    };
    count_walk(win, 0, &mut s);
    s
}

fn count_walk(el: &AXRef, depth: usize, s: &mut PanelSnapshot) {
    match ax_ffi::attr_string(el, attr::ROLE).as_deref() {
        Some("AXTextField") if ax_ffi::attr_settable(el, attr::VALUE) => {
            s.editable_text_fields += 1
        }
        Some("AXButton") => s.buttons += 1,
        Some("AXScrollArea" | "AXSplitGroup") => s.browsers += 1,
        _ => {}
    }
    if depth >= 6 {
        return;
    }
    for c in ax_ffi::children(el) {
        count_walk(&c, depth + 1, s);
    }
}

/// M0 探针粗判:subrole 含 Dialog 或命中产品快速信号白名单即疑似
/// (取证用,宁滥勿缺——漏报比误报更伤取证)。白名单形态(尤其 AXFloatingWindow)
/// 不一定含 Dialog,故此处必须取并集,保证探针能对其 dump 快照并触发实验。
///
/// 与 [`is_save_panel`] 分工:本函数只看快速信号,用于探针决定是否触发实验;
/// 产品判定必须叠加确认信号(可编辑输入框 + 按钮组),零误报优先。
pub fn looks_like_panel(s: &PanelSnapshot) -> bool {
    s.subrole
        .as_deref()
        .is_some_and(|r| r.contains("Dialog") || SUBROLE_QUICK.contains(&r))
}

/// 快速信号:window subrole 白名单(设计推定 subrole 集合;M0 findings 回填后修订)。
/// 判定为「疑似」而非「确认」——确认必须叠加结构信号。
/// 注:AXStandardWindow 等普通窗口 subrole 不在列(防误报)。
const SUBROLE_QUICK: &[&str] = &["AXSystemDialog", "AXDialog", "AXFloatingWindow"];

/// 确认信号阈值:保存/打开面板必含可编辑文件名输入框与按钮组;
/// 普通对话框(如确认弹窗)无此组合。
const CONFIRM_EDITABLE_MIN: usize = 1;
const CONFIRM_BUTTONS_MIN: usize = 1;

/// 产品判定(设计 §3.1 两级):快速信号命中 **且** 确认信号满足。
/// 纯函数(无 AX 调用),特征表数据化供 macOS 版本漂移时修订。
pub fn is_save_panel(s: &PanelSnapshot) -> bool {
    let quick_ok = s
        .subrole
        .as_deref()
        .map(|r| SUBROLE_QUICK.contains(&r))
        .unwrap_or(false);
    quick_ok && s.editable_text_fields >= CONFIRM_EDITABLE_MIN && s.buttons >= CONFIRM_BUTTONS_MIN
}

// ---------------------------------------------------------------------------
// 产品事件层:NSWorkspace 前台切换通知 → 专用 runloop 线程换绑 AXObserver

/// 前台 pid(NSWorkspace 通知主线程写;detector 线程读)。
static FRONT_PID: AtomicI32 = AtomicI32::new(0);
/// 当前已展示面板的 pid(Shown 时写;Closed 时读取归因)。
static PANEL_PID: AtomicI32 = AtomicI32::new(0);
/// 一次性启动闸(幂等;mpsc Receiver 只可交出一次)。
static STARTED: AtomicBool = AtomicBool::new(false);
/// 自定义信号源 raw(detector 线程内 RAII 保活,这里只存指针供主线程 signal)。
static SIGNAL_SRC: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
/// detector 线程 runloop raw(主线程 CFRunLoopWakeUp 目标)。
static DETECTOR_RL: AtomicPtr<std::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());
/// 事件接收端断开的一次性标志(避免每个事件重复 warn/stop)。
static SEND_FAILED: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// 当前 observer(仅 detector 线程使用;AXObserverWrap 无 Send)。
    static OBSERVER: std::cell::RefCell<Option<AXObserverWrap>> =
        const { std::cell::RefCell::new(None) };
    /// 当前 observer 绑定的前台 pid(0 = 未绑定;M2 去抖,detector 线程独占)。
    static BOUND_PID: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    /// 事件发送端(接收端由接线层持有)。
    static TX: std::cell::RefCell<Option<mpsc::Sender<PanelEvent>>> =
        const { std::cell::RefCell::new(None) };
    /// 信号源 RAII 保活(detector 线程)。
    static SOURCE: std::cell::RefCell<Option<AXRef>> =
        const { std::cell::RefCell::new(None) };
}

/// 启动 detector(幂等;第二次调用返回 None)。返回事件接收端。
///
/// 启动握手:spawn 后在 `sync_channel(1)` 上等待 detector 线程完成全部初始化
/// (最长 1s);线程失败/超时 → None,不交出半死 receiver。
///
/// 停止契约:detector 常驻,不支持真正停止——设置关闭由接线层忽略事件即可
/// (detector 仅通知驱动、无轮询,空转成本可忽略)。若将来需要 stop,必须先
/// `CFRunLoopStop` 再清空 `SIGNAL_SRC`/`DETECTOR_RL`;反序会让主线程
/// `CFRunLoopWakeUp` 命中已释放 runloop。
pub fn start_detector() -> Option<mpsc::Receiver<PanelEvent>> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return None;
    }
    if !ax_ffi::is_process_trusted() {
        log::info!("保存面板检测未启动:辅助功能权限未授予");
        STARTED.store(false, Ordering::SeqCst); // 授权后允许再试(接线层按设置重试)
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<bool>(1);
    let spawned = std::thread::Builder::new()
        .name("panel-detector".to_owned())
        .spawn(move || runloop_thread(tx, ready_tx));
    match spawned {
        // JoinHandle 立即 drop = 分离线程(runloop 常驻到进程退出)。
        Ok(h) => drop(h),
        Err(e) => {
            log::error!("detector 线程创建失败:{e}");
            STARTED.store(false, Ordering::SeqCst);
            return None;
        }
    }
    match ready_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(true) => Some(rx),
        // 线程已 fail_init(复位 STARTED + 清原子),此处不再重复复位。
        Ok(false) => None,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // 线程在握手前意外退出(未走 fail_init):复位闸门允许重试。
            log::error!("detector 初始化线程提前退出");
            STARTED.store(false, Ordering::SeqCst);
            None
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // 仅超时保留 STARTED 闸门(防双实例);rx 丢弃后线程首次投递事件
            // 会触发 send 自停(M1)。
            log::error!("detector 初始化超时(1s)");
            None
        }
    }
}

/// 主线程调用(NSWorkspace 通知回调):前台 pid 写原子 + 唤醒 detector 线程换绑。
///
/// 承重不变量:必须先写 `FRONT_PID` 再读发布指针。若本次 signal 因指针尚未发布
/// 被跳过,detector 发布完成后的启动扫描必然能看到该 pid;两处 `SeqCst` 不得
/// 调换或降级(否则前台切换可能丢失)。
pub fn notify_frontmost(pid: i32) {
    FRONT_PID.store(pid, Ordering::SeqCst);
    // SAFETY:两个指针均为 detector 线程写入的进程生命周期 CF 对象 raw;
    // 未就绪时为 null,signal_and_wake 内部判空。
    ax_ffi::signal_and_wake(
        SIGNAL_SRC.load(Ordering::SeqCst),
        DETECTOR_RL.load(Ordering::SeqCst),
    );
}

fn runloop_thread(tx: mpsc::Sender<PanelEvent>, ready: mpsc::SyncSender<bool>) {
    TX.with(|c| *c.borrow_mut() = Some(tx));
    // 信号源(perform 在 detector 线程派发 → rebind 满足 AXObserver 同线程约束)
    let Some(src) = ax_ffi::signal_source_create(on_signal) else {
        log::error!("detector: 信号源创建失败,线程退出");
        fail_init();
        let _ = ready.send(false);
        return;
    };
    // observer 占位(创建成功前不 attach;失败时信号源随局部变量释放)
    let Some(obs) = ax_ffi::observer_create(0, on_ax_event) else {
        log::error!("detector: AXObserverCreate 失败(权限?),线程退出");
        fail_init();
        let _ = ready.send(false);
        return;
    };
    // 全部 fallible 初始化完成后,再挂 runloop 并发布跨线程指针。
    ax_ffi::attach_signal_source(&src);
    ax_ffi::attach_observer_to_current_runloop(&obs);
    let signal_raw = src.raw();
    let runloop_raw = ax_ffi::current_runloop_raw();
    // RAII 保活
    SOURCE.with(|c| *c.borrow_mut() = Some(src));
    OBSERVER.with(|c| *c.borrow_mut() = Some(obs));
    // 最后发布:notify_frontmost 在此之前读到 null 即安全跳过;发布后对象由
    // SOURCE 保活到 detector 线程结束。
    SIGNAL_SRC.store(signal_raw, Ordering::SeqCst);
    DETECTOR_RL.store(runloop_raw, Ordering::SeqCst);
    // 握手先于前台 app 的 AX 扫描:不绑定目标 app 的 AX 响应性;启动扫描产生的
    // 事件照常进入 channel。
    let _ = ready.send(true);
    // 处理启动前已经就位的前台 pid(通知可能早于线程就绪)
    let pid = FRONT_PID.load(Ordering::SeqCst);
    if pid != 0 {
        rebind_and_scan(pid);
    }
    // M1 预检:启动扫描期间接收端已断开时直接走停止路径,不进 runloop。
    if SEND_FAILED.load(Ordering::SeqCst) {
        shutdown_forever();
    }
    log::info!("detector: 运行");
    ax_ffi::run_current_runloop();
    shutdown_forever();
}

/// 停止路径(不返回):先清空跨线程指针,再进入无 panic 的 park 循环——线程与
/// 已发布 CF 对象保活到进程退出。已发布 raw 可能已被主线程加载,故不回收对象、
/// 也不结束线程;清指针之后的日志即使 panic,也不会留下对外可见的悬垂指针。
fn shutdown_forever() -> ! {
    SIGNAL_SRC.store(std::ptr::null_mut(), Ordering::SeqCst);
    DETECTOR_RL.store(std::ptr::null_mut(), Ordering::SeqCst);
    log::info!("detector: runloop 已停止(接收端断开),对象保活至进程退出");
    loop {
        std::thread::park();
    }
}

/// 初始化失败兜底:显式清空跨线程指针(防发布前遗留悬垂 raw)+ 回滚启动闸
/// 允许接线层重试(与线程创建失败路径一致)。
fn fail_init() {
    SIGNAL_SRC.store(std::ptr::null_mut(), Ordering::SeqCst);
    DETECTOR_RL.store(std::ptr::null_mut(), Ordering::SeqCst);
    STARTED.store(false, Ordering::SeqCst);
}

/// 信号源 perform(detector 线程):前台 pid 变更 → 换绑 + 立即扫一遍。
extern "C" fn on_signal(_info: *mut std::ffi::c_void) {
    let pid = FRONT_PID.load(Ordering::SeqCst);
    let bound = BOUND_PID.with(|c| c.get());
    if !needs_rebind(bound, pid) {
        return;
    }
    rebind_and_scan(pid);
}

/// 换绑观察者并首扫;绑定成功但当前 app 没有保存面板时发 `Closed { pid }`。
///
/// 收敛「面板在别的 app 前台期间关闭」的 Esc 屏蔽泄漏:detector 在非前台 app
/// 上收不到销毁通知,切回该 app 后这次首扫是唯一确认点——没有面板就按
/// 「该 app 当前无面板」解除屏蔽(接线层 clear 无屏蔽 pid 为 no-op)。
/// 命中面板时不发 Closed,保证「切走再切回同一面板仍开着」不会重新弹。
fn rebind_and_scan(pid: i32) {
    if !rebind_observer(pid) {
        return;
    }
    if !scan_focused(pid) {
        send(PanelEvent::Closed { pid });
    }
}

/// 是否需要换绑(bound = 当前 observer 绑定 pid,0 = 未绑定;front = 前台 pid,
/// 0 = 未知)。前台未变时既不换绑也不扫描(M2:去抖重复 signal)。
fn needs_rebind(bound: i32, front: i32) -> bool {
    front != 0 && bound != front
}

/// 重建 observer 并注册三类通知(本线程;refcon = pid 用于回调归因校验)。
/// 返回 false = 失败(调用方可重试);成功时更新绑定点并清空面板归因。
fn rebind_observer(pid: i32) -> bool {
    let Some(el) = ax_ffi::app_element(pid) else {
        log::warn!("detector: app element 创建失败 pid={pid}");
        return false;
    };
    let Some(obs) = ax_ffi::observer_create(pid, on_ax_event) else {
        log::warn!("detector: observer 创建失败 pid={pid}");
        return false;
    };
    let mut registered = 0usize;
    for n in [
        note::WINDOW_CREATED,
        note::FOCUSED_WINDOW_CHANGED,
        note::UI_ELEMENT_DESTROYED,
    ] {
        match ax_ffi::observer_add_notification(&obs, &el, n, pid as usize) {
            Ok(()) => registered += 1,
            Err(e) => log::debug!("detector: 通知注册失败 pid={pid} {n} err={e}"),
        }
    }
    // 三类通知全部被拒(隐藏 AX 树)时新 observer 无意义:不换绑、保留旧绑定,
    // 允许下次前台切换重试(至少一条成功才置 BOUND_PID)。
    if registered == 0 {
        log::warn!("detector: 三类通知全部注册失败 pid={pid},保留旧绑定等待重试");
        return false;
    }
    ax_ffi::attach_observer_to_current_runloop(&obs);
    OBSERVER.with(|c| {
        if let Some(old) = c.borrow_mut().take() {
            // H1:先真拆卸再释放——旧 source 不再挂 runloop,CFRelease 安全;
            // 不再保留退役池(每轮前台切换都会泄漏 observer + source)。
            ax_ffi::detach_observer_from_current_runloop(&old);
            drop(old);
        }
        *c.borrow_mut() = Some(obs);
    });
    BOUND_PID.with(|c| c.set(pid));
    // M4 契约:换绑即前台宿主变化。此前若有面板归因,先发 HostChanged 让接线层
    // 隐藏浮层但保留 Dismissals(Esc 屏蔽持续到该面板真正关闭,design §4);
    // 不发 Closed,随后清空归因。
    let previous = PANEL_PID.swap(0, Ordering::SeqCst);
    if previous != 0 {
        send(PanelEvent::HostChanged {
            previous_pid: previous,
        });
    }
    true
}

/// AXObserver 回调:销毁 → Closed 事件;其余 → 扫焦点窗口判定。
extern "C" fn on_ax_event(
    _obs: *mut std::ffi::c_void,
    _el: *mut std::ffi::c_void,
    notification: *const std::ffi::c_void,
    refcon: *mut std::ffi::c_void,
) {
    let pid = FRONT_PID.load(Ordering::SeqCst);
    // 归因兜底(H1):refcon = 注册通知时的 pid。换绑后旧 observer 的残留通知
    // (理论上已随 detach 消失)不得被误归因到新前台 app。
    if pid == 0 || refcon as usize as i32 != pid {
        return;
    }
    let n = ax_ffi::cfstring_to_string(notification).unwrap_or_default();
    if n == note::UI_ELEMENT_DESTROYED {
        // 宽口径(同探针):app 内任何元素销毁都按面板关闭处理——
        // 产品侧后果仅为 Closed 事件偏早(接线层隐藏浮层),可接受。
        // 旧归因值随事件交给接线层,用于按 pid 精确解除 Esc 屏蔽。
        let panel = PANEL_PID.swap(0, Ordering::SeqCst);
        if panel != 0 {
            send(PanelEvent::Closed { pid: panel });
        }
        return;
    }
    scan_focused(pid);
}

/// 扫描前台 app 的焦点窗口;命中保存面板并发出 `Shown` 时返回 true。
///
/// 返回 false 也可能是 AX 读取失败(无法区分「没有面板」与「读不到」),
/// 故只有换绑首扫路径把 false 当作「该 app 当前无面板」的确认点。
fn scan_focused(pid: i32) -> bool {
    let Some(app) = ax_ffi::app_element(pid) else {
        return false;
    };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        return false;
    };
    let snap = snapshot_of_window(&win);
    if !is_save_panel(&snap) {
        return false;
    }
    let Some((x, y)) = ax_ffi::attr_point(&win, attr::POSITION) else {
        return false;
    };
    let Some((w, h)) = ax_ffi::attr_size(&win, attr::SIZE) else {
        return false;
    };
    // M3:0/负/NaN/∞ 坐标或尺寸不是合法屏幕坐标(AX 布局未完成或读取异常),
    // 宁缺毋滥,避免把浮层定位到 (NaN,NaN) 或零尺寸。
    // 注:「尺寸非法时等后续通知重扫」是未验证假设,Task 15 实机确认。
    if !valid_panel_frame(x, y, w, h) {
        return false;
    }
    PANEL_PID.store(pid, Ordering::SeqCst);
    send(PanelEvent::Shown {
        pid,
        frame: [x, y, w, h],
    });
    true
}

/// 面板 frame 合法性:坐标/宽高必须有限,宽高必须为正(NaN/∞ 与 <=0 均拒绝)。
fn valid_panel_frame(x: f64, y: f64, w: f64, h: f64) -> bool {
    x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0
}

fn send(ev: PanelEvent) {
    TX.with(|c| {
        if let Some(tx) = c.borrow().as_ref() {
            if tx.send(ev).is_err() && !SEND_FAILED.swap(true, Ordering::SeqCst) {
                log::warn!("事件接收端已断开,detector 停止");
                // send 在 detector 线程(observer/signal 回调)上执行,可直接停。
                ax_ffi::stop_current_runloop();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(subrole: &str, editable: usize, buttons: usize) -> PanelSnapshot {
        PanelSnapshot {
            role: Some("AXWindow".into()),
            subrole: Some(subrole.into()),
            title: Some("存储".into()),
            editable_text_fields: editable,
            buttons,
            browsers: 1,
        }
    }

    /// 契约:快速信号 + 确认信号双满足才判定;任一缺失为否。
    /// 突变「去掉 editable/button 确认(仅 subrole 判定)」→ 第三用例必红。
    #[test]
    fn quick_and_confirm_signals_both_required() {
        assert!(is_save_panel(&snap("AXSystemDialog", 1, 2)));
        assert!(
            !is_save_panel(&snap("AXStandardWindow", 1, 2)),
            "subrole 不中"
        );
        assert!(
            !is_save_panel(&snap("AXSystemDialog", 0, 2)),
            "无可写输入框"
        );
        assert!(!is_save_panel(&snap("AXSystemDialog", 1, 0)), "无按钮");
    }

    /// 标题不参与判定:打开面板标题为「打开」仍判定为是。
    /// 钉住不得按标题过滤;open/save 的区分不在判定函数职责内(设计 §3.2 触发范围:不区分)。
    #[test]
    fn title_not_part_of_judgment_open_title_still_matches() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.title = Some("打开".into());
        assert!(is_save_panel(&s));
    }

    /// subrole 缺失(个别 app 隐藏属性)→ 不判定,防误报。
    #[test]
    fn missing_subrole_never_matches() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.subrole = None;
        assert!(!is_save_panel(&s));
    }

    /// 探针粗判与产品判定的分工钉(Dialog 个案):粗判只看 subrole(取证宁滥),
    /// 产品判定必须双信号(零误报优先)。个案名不声称全局宽窄关系。
    #[test]
    fn probe_rough_judgment_wider_on_dialog_subroles() {
        let s = snap("AXDialog", 0, 0);
        assert!(looks_like_panel(&s), "粗判:subrole 命中即疑似");
        assert!(!is_save_panel(&s), "产品判定:缺确认信号不通过");
    }

    /// 不变量:探针覆盖面必须是产品快速信号白名单的超集——
    /// 白名单内每种形态(尤其 AXFloatingWindow)都必须能被 M0 探针 dump 快照并触发实验。
    #[test]
    fn probe_covers_every_product_quick_signal() {
        for subrole in SUBROLE_QUICK {
            assert!(looks_like_panel(&snap(subrole, 0, 0)), "{subrole}");
        }
    }

    /// 已登记风险:AXFloatingWindow + 可编辑输入框会命中产品判定,
    /// 而 Get Info/Inspector 窗口可能就是这种形态。
    /// M0 实测复核后决定:移除该白名单条目,或加严确认信号(例如让 browsers 参与判定)。
    #[test]
    fn floating_window_with_editable_field_currently_matches_pending_m0() {
        assert!(is_save_panel(&snap("AXFloatingWindow", 1, 1)));
    }

    /// M2:仅当前台 pid 有效且变化时才换绑;相同 pid 的重复 signal 不得重复换绑/扫描。
    #[test]
    fn rebind_only_on_front_pid_change() {
        assert!(!needs_rebind(0, 0), "前台未知不触发");
        assert!(needs_rebind(0, 42), "首次绑定");
        assert!(!needs_rebind(42, 42), "前台未变不换绑");
        assert!(needs_rebind(42, 7), "前台变化换绑");
    }

    /// M3:坐标/尺寸为 NaN/∞,或宽高 <=0 时不可作为合法 frame
    /// (宁缺毋滥;「等后续通知重扫」待 Task 15 实机确认)。
    #[test]
    fn invalid_panel_frame_rejected() {
        assert!(valid_panel_frame(100.0, 80.0, 640.0, 480.0));
        // x/y 非法
        assert!(!valid_panel_frame(f64::NAN, 80.0, 640.0, 480.0));
        assert!(!valid_panel_frame(100.0, f64::INFINITY, 640.0, 480.0));
        // 宽高非正/非有限
        assert!(!valid_panel_frame(100.0, 80.0, 0.0, 480.0));
        assert!(!valid_panel_frame(100.0, 80.0, 640.0, 0.0));
        assert!(!valid_panel_frame(100.0, 80.0, -1.0, 480.0));
        assert!(!valid_panel_frame(100.0, 80.0, f64::NAN, 480.0));
        assert!(!valid_panel_frame(100.0, 80.0, f64::INFINITY, 480.0));
    }
}

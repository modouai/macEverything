//! 保存面板检测:特征快照 dump(探针) + 两级判定(快速 subrole 白名单 →
//! 确认可编辑输入框/按钮组) + 产品事件层(NSWorkspace 前台切换通知驱动,
//! 专用 runloop 线程换绑 AXObserver,mpsc 输出 [`PanelEvent`])。
//! 快照结构与 AX 树解耦,纯数据可序列化可测。
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
use std::sync::mpsc;

use super::ax_ffi::{self, attr, note, AXObserverWrap, AXRef};

/// 检测事件(UI 接线层每帧 poll;mpsc 单消费)。
///
/// 重复语义:`Shown` 可能因多个通知(AXWindowCreated + AXFocusedWindowChanged +
/// on_signal 预扫)对同一面板重复发出;去重责任在接线层(浮层已显示则忽略),
/// detector 不做状态去重。`Closed` 的宽口径(任意元素销毁即视为关闭)见
/// [`on_ax_event`] 现场注释。
#[derive(Debug, Clone, PartialEq)]
pub enum PanelEvent {
    /// 疑似保存面板出现。frame 为 AX 屏幕坐标 [x, y, w, h]:
    /// 原点在主屏左上角、y 向下(与 AppKit 左下原点相反——浮层定位换算在平台层做)。
    Shown { pid: i32, frame: [f64; 4] },
    /// 面板窗口销毁。
    Closed,
}

/// Esc 屏蔽状态(design §4 不打扰规则:本面板存续内不再自动弹;面板关闭即清除)。
/// 键 = 宿主 pid(同一 app 同时至多一个模态保存面板)。
#[derive(Default)]
#[allow(dead_code)] // Task 14 接线层使用(设计 §4 不打扰规则);单测已覆盖。
pub struct Dismissals {
    dismissed: HashSet<i32>,
}

#[allow(dead_code)] // Task 14 接线层使用;单测已覆盖。
impl Dismissals {
    pub fn dismiss(&mut self, pid: i32) {
        self.dismissed.insert(pid);
    }
    pub fn blocked(&self, pid: i32) -> bool {
        self.dismissed.contains(&pid)
    }
    pub fn clear(&mut self, pid: i32) {
        self.dismissed.remove(&pid);
    }
}

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

thread_local! {
    /// 当前 observer(仅 detector 线程使用;AXObserverWrap 无 Send)。
    static OBSERVER: std::cell::RefCell<Option<AXObserverWrap>> =
        const { std::cell::RefCell::new(None) };
    /// 退役 observer 保活池:重建时不 CFRelease 旧 observer(其 runloop source
    /// 可能仍挂在 runloop 上,释放后 source 归属未定义)。生命周期量级 = 前台
    /// 切换次数,同探针策略。
    static RETIRED: std::cell::RefCell<Vec<AXObserverWrap>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// 事件发送端(接收端由接线层持有)。
    static TX: std::cell::RefCell<Option<mpsc::Sender<PanelEvent>>> =
        const { std::cell::RefCell::new(None) };
    /// 信号源 RAII 保活(detector 线程)。
    static SOURCE: std::cell::RefCell<Option<AXRef>> =
        const { std::cell::RefCell::new(None) };
}

/// 启动 detector(幂等;第二次调用返回 None)。返回事件接收端。
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
    let spawned = std::thread::Builder::new()
        .name("panel-detector".to_owned())
        .spawn(move || runloop_thread(tx));
    match spawned {
        // JoinHandle 立即 drop = 分离线程(runloop 常驻到进程退出)。
        Ok(h) => {
            drop(h);
            Some(rx)
        }
        Err(e) => {
            log::error!("detector 线程创建失败:{e}");
            STARTED.store(false, Ordering::SeqCst);
            None
        }
    }
}

/// 主线程调用(NSWorkspace 通知回调):前台 pid 写原子 + 唤醒 detector 线程换绑。
pub fn notify_frontmost(pid: i32) {
    FRONT_PID.store(pid, Ordering::SeqCst);
    // SAFETY:两个指针均为 detector 线程写入的进程生命周期 CF 对象 raw;
    // 未就绪时为 null,signal_and_wake 内部判空。
    ax_ffi::signal_and_wake(
        SIGNAL_SRC.load(Ordering::SeqCst),
        DETECTOR_RL.load(Ordering::SeqCst),
    );
}

fn runloop_thread(tx: mpsc::Sender<PanelEvent>) {
    TX.with(|c| *c.borrow_mut() = Some(tx));
    // 信号源(perform 在 detector 线程派发 → rebind 满足 AXObserver 同线程约束)
    let Some(src) = ax_ffi::signal_source_create(on_signal) else {
        log::error!("detector: 信号源创建失败,线程退出");
        fail_init();
        return;
    };
    // observer 占位(创建成功前不 attach;失败时信号源随局部变量释放)
    let Some(obs) = ax_ffi::observer_create(0, on_ax_event) else {
        log::error!("detector: AXObserverCreate 失败(权限?),线程退出");
        fail_init();
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
    // 处理启动前已经就位的前台 pid(通知可能早于线程就绪)
    let pid = FRONT_PID.load(Ordering::SeqCst);
    if pid != 0 {
        rebind_observer(pid);
        scan_focused(pid);
    }
    log::info!("detector: 运行");
    ax_ffi::run_current_runloop();
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
    if pid == 0 {
        return;
    }
    rebind_observer(pid);
    scan_focused(pid);
}

/// 重建 observer 并注册三类通知(本线程;同探针形态,失败不致命)。
fn rebind_observer(pid: i32) {
    let Some(el) = ax_ffi::app_element(pid) else {
        log::warn!("detector: app element 创建失败 pid={pid}");
        return;
    };
    let Some(obs) = ax_ffi::observer_create(pid, on_ax_event) else {
        log::warn!("detector: observer 创建失败 pid={pid}");
        return;
    };
    for n in [
        note::WINDOW_CREATED,
        note::FOCUSED_WINDOW_CHANGED,
        note::UI_ELEMENT_DESTROYED,
    ] {
        if let Err(e) = ax_ffi::observer_add_notification(&obs, &el, n, 0) {
            log::debug!("detector: 通知注册失败 pid={pid} {n} err={e}");
        }
    }
    ax_ffi::attach_observer_to_current_runloop(&obs);
    OBSERVER.with(|c| {
        if let Some(old) = c.borrow_mut().take() {
            RETIRED.with(|r| r.borrow_mut().push(old)); // 保活(同探针:source 归属)
        }
        *c.borrow_mut() = Some(obs);
    });
}

/// AXObserver 回调:销毁 → Closed 事件;其余 → 扫焦点窗口判定。
extern "C" fn on_ax_event(
    _obs: *mut std::ffi::c_void,
    _el: *mut std::ffi::c_void,
    notification: *const std::ffi::c_void,
    _refcon: *mut std::ffi::c_void,
) {
    let pid = FRONT_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let n = ax_ffi::cfstring_to_string(notification).unwrap_or_default();
    if n == note::UI_ELEMENT_DESTROYED {
        // 宽口径(同探针):app 内任何元素销毁都按面板关闭处理——
        // 产品侧后果仅为 Closed 事件偏早(接线层隐藏浮层),可接受。
        let panel = PANEL_PID.swap(0, Ordering::SeqCst);
        if panel != 0 {
            send(PanelEvent::Closed);
        }
        return;
    }
    scan_focused(pid);
}

fn scan_focused(pid: i32) {
    let Some(app) = ax_ffi::app_element(pid) else {
        return;
    };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        return;
    };
    let snap = snapshot_of_window(&win);
    if !is_save_panel(&snap) {
        return;
    }
    let (x, y) = ax_ffi::attr_point(&win, attr::POSITION).unwrap_or((0.0, 0.0));
    let (w, h) = ax_ffi::attr_size(&win, attr::SIZE).unwrap_or((0.0, 0.0));
    PANEL_PID.store(pid, Ordering::SeqCst);
    send(PanelEvent::Shown {
        pid,
        frame: [x, y, w, h],
    });
}

fn send(ev: PanelEvent) {
    TX.with(|c| {
        if let Some(tx) = c.borrow().as_ref() {
            let _ = tx.send(ev);
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

    /// Esc 屏蔽:dismiss 后本面板存续内 blocked,clear(面板关闭)后恢复。
    #[test]
    fn dismiss_blocks_until_cleared() {
        let mut d = Dismissals::default();
        assert!(!d.blocked(42));
        d.dismiss(42);
        assert!(d.blocked(42));
        d.clear(42);
        assert!(!d.blocked(42));
    }

    /// 屏蔽按宿主 pid 隔离:一个 app 的 Esc 不影响其他 app 的保存面板。
    #[test]
    fn dismissals_are_per_pid() {
        let mut d = Dismissals::default();
        d.dismiss(1);
        assert!(d.blocked(1));
        assert!(!d.blocked(2));
        d.clear(2); // 清除未屏蔽 pid 为 no-op
        assert!(d.blocked(1));
    }
}

//! 保存面板检测:特征快照 dump(探针) + 两级判定(快速 subrole 白名单 →
//! 确认可编辑输入框/按钮组) + 产品事件层(NSWorkspace 前台切换通知驱动,
//! 专用 runloop 线程换绑 AXObserver,mpsc 输出 [`PanelEvent`])。
//! 快照结构与 AX 树解耦,纯数据可序列化可测。
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

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
    /// 参与 [`is_save_panel`] 结构指纹(≥3 在位置侧栏不可见时兜底,见
    /// `CONFIRM_BROWSERS_MIN`;2026-11-05 实机:真面板 r5-9,弹层 r≤1)。
    pub browsers: usize,
    /// 子树内「标准文件位置名」命中数(静态文本/按钮标题,见 [`LOCATION_NAMES`])。
    /// NSSavePanel 系面板必嵌位置侧栏(桌面/下载/文稿…),查找/密码/格式等
    /// 普通 sheet 不含——sheet 形态的结构级判别指纹(2026-11-05 实测引入)。
    pub location_hits: usize,
}

/// 标准文件位置名(中英;en 不分大小写)。覆盖 NSSavePanel 侧栏与「位置」
/// 弹层的可见标签;macOS 版本漂移时按实机特征表修订。
const LOCATION_NAMES: &[&str] = &[
    "桌面",
    "下载",
    "文稿",
    "文档",
    "最近使用",
    "最近项目",
    "隔空投送",
    "airdrop",
    "icloud",
    "desktop",
    "downloads",
    "documents",
    "recents",
    "applications",
    "应用程序",
    "我的mac",
    "my mac",
];

/// 值是否命中任一标准位置名(en 不分大小写)。
fn hits_location_name(v: &str) -> bool {
    let lower = v.to_lowercase();
    LOCATION_NAMES.iter().any(|n| lower.contains(n))
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
        location_hits: 0,
    };
    count_walk(win, 0, &mut s);
    s
}

fn count_walk(el: &AXRef, depth: usize, s: &mut PanelSnapshot) {
    match ax_ffi::attr_string(el, attr::ROLE).as_deref() {
        Some("AXTextField") if ax_ffi::attr_settable(el, attr::VALUE) => {
            s.editable_text_fields += 1
        }
        Some("AXButton") => {
            s.buttons += 1;
            // 按钮标题参与位置名匹配(「位置」弹层按钮/侧栏行)。
            for key in [attr::TITLE, "AXDescription"] {
                if let Some(v) = ax_ffi::attr_string(el, key) {
                    if hits_location_name(&v) {
                        s.location_hits += 1;
                        break;
                    }
                }
            }
        }
        Some("AXStaticText") => {
            if let Some(v) = ax_ffi::attr_string(el, attr::VALUE) {
                if hits_location_name(&v) {
                    s.location_hits += 1;
                }
            }
        }
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

/// M0 探针粗判:subrole 含 Dialog、命中产品快速信号三路(subrole 白名单 ∥
/// AXSheet 形态 ∥ 标准窗+标题命中面板名单)即疑似(取证用,宁滥勿缺——漏报
/// 比误报更伤取证)。必须与 [`is_save_panel`] 的快速信号保持超集关系(不变量
/// 测试 [`probe_covers_every_product_quick_signal` 钉住]),保证每种产品形态
/// 都能被 M0 探针 dump 快照并触发实验。
///
/// 与 [`is_save_panel`] 分工:本函数只看快速信号,用于探针决定是否触发实验;
/// 产品判定必须叠加确认信号,零误报优先。
pub fn looks_like_panel(s: &PanelSnapshot) -> bool {
    s.subrole
        .as_deref()
        .is_some_and(|r| r.contains("Dialog") || SUBROLE_QUICK.contains(&r))
        || s.role.as_deref() == Some(ROLE_QUICK_SHEET)
        || (s.role.as_deref() == Some(ROLE_QUICK_WINDOW)
            && s.subrole.as_deref() == Some(SUBROLE_QUICK_WINDOW)
            && s.title.as_deref().is_some_and(title_is_panel_name))
}

/// 快速信号一:window subrole 白名单(独立对话框/浮窗形态)。
/// 判定为「疑似」而非「确认」——确认必须叠加结构信号。
/// 注:AXStandardWindow 等普通窗口 subrole 不在列(防误报)。
const SUBROLE_QUICK: &[&str] = &["AXSystemDialog", "AXDialog", "AXFloatingWindow"];

/// 快速信号二:role = AXSheet(附着宿主的 sheet 形态)。实机(2026-11-05,
/// TextEdit/XMind/Edge 三家一致):保存面板以 sheet 附着宿主窗口,sheet 元素
/// 的 subrole 读不到(None),只有 role 可辨——subrole 白名单对此形态全漏。
const ROLE_QUICK_SHEET: &str = "AXSheet";

/// 快速信号三:AXStandardWindow + 标题精确命中面板名单。实机(2026-11-05,
/// XMind/第三方 app):打开面板以标准窗口形态呈现(e1 b15 r8 loc7),subrole
/// 防误报白名单不含 AXStandardWindow(普通文档窗口全是它)——分界在标题:
/// 面板窗口标题恒为打开/存储/另存为系,文档窗口标题是文件名。标题必须精确
/// 相等(en 不分大小写),防「打开xx.docx」类文档名误命中。
const ROLE_QUICK_WINDOW: &str = "AXWindow";
const SUBROLE_QUICK_WINDOW: &str = "AXStandardWindow";
const PANEL_TITLES: &[&str] = &[
    "打开",
    "存储",
    "存储为",
    "储存",
    "另存为",
    "保存",
    "open",
    "save",
    "save as",
    "open file",
    "save file",
];

/// 标题是否精确命中面板名单(en 不分大小写)。
fn title_is_panel_name(title: &str) -> bool {
    PANEL_TITLES.iter().any(|n| n.eq_ignore_ascii_case(title))
}

/// 确认信号阈值:保存/打开面板必含可编辑文件名输入框与按钮组(数量底线)。
/// 实测教训(2026-11-05):数量维度单独不构成判别力——WPS 查找框 AXDialog
/// e1 b9、查找和替换 e1 b12、下拉框 AXSheet e1 b1 全部能过数量关,必须叠加
/// 结构指纹(下),它才是真正的判别维度。
const CONFIRM_EDITABLE_MIN: usize = 1;
const CONFIRM_BUTTONS_MIN: usize = 1;

/// 结构指纹阈值:NSSavePanel 系面板必嵌位置侧栏(「桌面/下载/文稿…」静态
/// 文本,见 [`LOCATION_NAMES`])——查找/密码/打印/下拉等普通弹层不含;侧栏
/// 被宿主隐藏的极简面板以文件浏览容器 ≥3 兜底(实测真面板 r5-9,弹层 r≤1)。
const CONFIRM_LOCATION_MIN: usize = 1;
const CONFIRM_BROWSERS_MIN: usize = 3;

/// 产品判定(设计 §3.1 两级):快速信号(subrole 白名单 ∥ AXSheet 形态 ∥
/// 标准窗+标题精确命中面板名单)命中 **且** 数量确认 **且** 结构指纹。数量
/// 阈值只是底线,判别力在结构指纹——三条形态路径统一适用(2026-11-05 实机:
/// 查找/替换窗口形态与下拉框 sheet 形态的误报都只有数量信号,无指纹)。纯函数
/// (无 AX 调用),特征表数据化供 macOS 版本漂移时修订。
pub fn is_save_panel(s: &PanelSnapshot) -> bool {
    let quick_ok = s.role.as_deref() == Some(ROLE_QUICK_SHEET)
        || s.subrole
            .as_deref()
            .map(|r| SUBROLE_QUICK.contains(&r))
            .unwrap_or(false)
        || (s.role.as_deref() == Some(ROLE_QUICK_WINDOW)
            && s.subrole.as_deref() == Some(SUBROLE_QUICK_WINDOW)
            && s.title.as_deref().is_some_and(title_is_panel_name));
    let quantity_ok =
        s.editable_text_fields >= CONFIRM_EDITABLE_MIN && s.buttons >= CONFIRM_BUTTONS_MIN;
    let fingerprint_ok =
        s.location_hits >= CONFIRM_LOCATION_MIN || s.browsers >= CONFIRM_BROWSERS_MIN;
    quick_ok && quantity_ok && fingerprint_ok
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
/// 换绑重试已用预算(on_signal 真宿主切换时重置;排程消费,**detector 线程
/// 独占读写**——重置已从主线程 notify_frontmost 移入 on_signal,消除丢更新竞态)。
static REBIND_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
/// 换绑重试剩余 tick 数(0 = 无在途重试;常驻 timer 每 TICK_SECS 秒递减,
/// 到 1 时触发换绑。**detector 线程独占读写**:排程写入、tick 递减、换绑成功
/// 与真宿主切换时清零)。
static RETRY_TICKS_LEFT: AtomicU32 = AtomicU32::new(0);
/// 本助手跳转的「子 sheet 瞬态」窗口截止时刻。⌘⇧G 的「前往文件夹」子 sheet
/// 让保存面板暂时从 AXWindows 消失;仅在这段窗口内允许把「存在其他 sheet +
/// 无面板」判为 Unknown。超出窗口仍无面板 → NoPanel,避免宿主无关 sheet
/// 长期压制 Closed(评审 2026-11-06)。
static NAVIGATING_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

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
/// (通知驱动 + 一个 0.25s 常驻重试 tick timer,空闲期每次 tick 仅一次原子读,
/// 空转成本可忽略)。若将来需要 stop,必须先 `CFRunLoopStop` 再清空
/// `SIGNAL_SRC`/`DETECTOR_RL`;反序会让主线程 `CFRunLoopWakeUp` 命中已释放
/// runloop。timer 挂同一 runloop,随之停止、引用随进程存活。
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
    // 忽略自身进程:点击浮层/设置窗等我们的窗口会让前台变成 macEverything 本身
    // (实机:点击浮层 → 自身被激活 → 换绑 → 扫到无面板 → Closed → 浮层消失)。
    // 宿主面板仍在原 app 里,不能把「切到自己」当作宿主切换。
    if pid > 0 && pid as u32 == std::process::id() {
        return;
    }
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
    // 全部 fallible 初始化完成后,再挂 runloop 并发布跨线程指针。
    // observer 不预建:AXObserverCreate 需要真实目标 pid(占位 pid=0 会被拒绝),
    // 首个前台 pid(通知写入 FRONT_PID)到达时由 rebind_observer 创建并挂 runloop。
    ax_ffi::attach_signal_source(&src);
    let signal_raw = src.raw();
    let runloop_raw = ax_ffi::current_runloop_raw();
    // RAII 保活
    SOURCE.with(|c| *c.borrow_mut() = Some(src));
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
    // 常驻重试 timer(唯一、随进程存活、永不销毁;tick 倒数见 schedule_rebind_retry)。
    ax_ffi::timer_install_repeating(RETRY_TICK_SECS, on_retry_tick);
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
    // 真宿主切换 = 新的瞬态拒绝窗口:在此重置重试预算与在途倒数。重置原在
    // notify_frontmost(主线程),与 detector 线程的 load→store 存在丢更新竞态;
    // 移到本线程后 REBIND_ATTEMPTS/RETRY_TICKS_LEFT 全部单线程写。
    REBIND_ATTEMPTS.store(0, Ordering::SeqCst);
    RETRY_TICKS_LEFT.store(0, Ordering::SeqCst);
    rebind_and_scan(pid);
}

/// 焦点窗口扫描结果(三态)。
///
/// 只有「成功读到焦点窗口且判定不是保存面板」才是**确认无面板**,允许在换绑
/// 首扫时发 `Closed`;AX 读取失败/窗口无法定位属于**状态未知**,不得解除屏蔽。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanOutcome {
    /// 命中保存面板并已发出 `Shown`。
    Panel,
    /// 焦点窗口读取成功且 `is_save_panel == false` —— 确认该 app 当前无面板。
    NoPanel,
    /// AX 读取失败或面板存在但无法定位 —— 状态未知,不发事件。
    Unknown,
}

/// 换绑首扫结果是否应发 `Closed { pid }`(纯逻辑,单测钉住三态收敛)。
/// 双门:绑定成功(observer 可继续观察销毁)+ 扫描确认无面板;任一不满足
/// 都不得解除屏蔽(绑定失败时后续状态无人观察,屏蔽泄漏到面板关闭为止)。
fn should_send_closed_on_rebind(bound_ok: bool, outcome: ScanOutcome) -> bool {
    bound_ok && outcome == ScanOutcome::NoPanel
}

/// 换绑成功后是否应发 `HostChanged`(纯逻辑):仅当存在面板归因(previous≠0)
/// 且归因宿主 ≠ 新绑定宿主(真宿主切换)时成立。重试成功对同一 pid 的绑定
/// 不是宿主切换——「绑定失败也扫描」可能已把归因记到该 pid 上,归因仍有效,
/// 必须保留(评审 2026-11-05:否则对开着的面板误隐藏浮层并清空用户输入)。
/// 突变去掉 `previous != bound_pid` → `host_changed_only_on_real_host_switch` 必红。
fn host_changed_on_rebind(previous: i32, bound_pid: i32) -> bool {
    previous != 0 && previous != bound_pid
}

/// 重试节奏 = 常驻重复 timer(唯一、随进程存活、永不销毁)每 [`RETRY_TICK_SECS`]
/// 秒递减 [`RETRY_TICKS_LEFT`],到 1 时触发换绑。不做动态创建/销毁——2026-11-05
/// 实机教训:一次性 timer「回调内自我 invalidate + release」破坏 runloop 内部
/// 引用导致 UAF(CFRunLoopTimerCreate 内 SIGSEGV);常驻单 timer 是 M0 探针
/// 已长期验证的稳定形态。
const RETRY_TICK_SECS: f64 = 0.25;
/// 换绑重试 tick 档位:1/2/4/8/16 tick = 0.25s/0.5s/1s/2s/4s,覆盖约 8s 的
/// 瞬态拒绝恢复窗口(2026-11-05 Edge 实测)。
const REBIND_RETRY_DELAY_TICKS: [u32; 5] = [1, 2, 4, 8, 16];

/// 换绑重试档位(tick 数;attempt 从 0 计)。超预算返回 None:放弃本轮,等下次
/// 前台切换重置预算。实测(2026-11-05,Edge):注册可能在激活瞬间被目标 app
/// 瞬态拒绝,不重试 = 该 app 永久失联。
fn rebind_retry_delay_ticks(attempt: usize) -> Option<u32> {
    REBIND_RETRY_DELAY_TICKS.get(attempt).copied()
}

/// 换绑观察者并首扫;仅当「绑定成功 **且** 确认无面板」时发 `Closed { pid }`。
///
/// 收敛「面板在别的 app 前台期间关闭」的 Esc 屏蔽泄漏:detector 在非前台 app
/// 上收不到销毁通知,切回该 app 后这次首扫是唯一确认点——没有面板就按
/// 「该 app 当前无面板」解除屏蔽(接线层 clear 无屏蔽 pid 为 no-op)。
/// AX 读取失败不确认(否则会误清仍有效面板的屏蔽);命中面板也不发,保证
/// 「切走再切回同一面板仍开着」不会重新弹。
///
/// 绑定失败也扫描:面板可见性(`Shown`)只依赖 AX 读取,不依赖 observer——
/// 实机(2026-11-05,Edge)注册在激活瞬间被瞬态拒绝时,若不同时扫描,该 app
/// 在重试成功前完全失明。绑定失败时不发 `Closed`(AX 状态未证实,不得解除
/// 屏蔽),并安排延迟重试(见 [`schedule_rebind_retry`])。
fn rebind_and_scan(pid: i32) {
    let bound_ok = rebind_observer(pid);
    let outcome = scan_focused(pid);
    if should_send_closed_on_rebind(bound_ok, outcome) {
        send(PanelEvent::Closed { pid });
    }
    if !bound_ok {
        schedule_rebind_retry();
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
            // 实测教训(2026-11-05):错误码是定位瞬态拒绝的唯一线索,info 留痕。
            Err(e) => log::info!("detector: 通知注册失败 pid={pid} {n} err={e}"),
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
    // 绑定成功:重置重试预算与在途倒数(后续失败从第 0 档重新计)。
    REBIND_ATTEMPTS.store(0, Ordering::SeqCst);
    RETRY_TICKS_LEFT.store(0, Ordering::SeqCst);
    log::info!("detector: 已绑定前台 pid={pid}(通知 {registered}/3)");
    // M4 契约:HostChanged 只在「归因宿主 ≠ 新绑定宿主」(真宿主切换)时发出,
    // 让接线层隐藏浮层但保留 Dismissals(Esc 屏蔽持续到该面板真正关闭,design
    // §4),归因随发随清。例外(重试路径):「绑定失败也扫描」可能已把归因记到
    // 正在重绑的同一 pid 上——此刻绑定成功但宿主从未切换,归因仍有效,保留
    // 不动,否则会对开着的面板误隐藏浮层并清空用户输入(2026-11-05 评审收敛)。
    // PANEL_PID 仅 detector 线程读写,load-判断-store 无竞态。
    let previous = PANEL_PID.load(Ordering::SeqCst);
    if host_changed_on_rebind(previous, pid) {
        PANEL_PID.store(0, Ordering::SeqCst);
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
        // 实机收窄(2026-11-05):宽口径「任意元素销毁即面板关闭」在保存面板
        // 打开期间会被面板内部元素销毁(焦点切换/控件重建)频繁触发,形成
        // Shown→Closed→Shown 循环(浮层闪现即消失,实机复现)。改为销毁后重扫
        // 焦点窗口:仍在面板上则忽略;确认不再是面板(读取成功且判定否)才发
        // Closed。Unknown(读取失败)不解除——与扫描三态纪律一致。
        if PANEL_PID.load(Ordering::SeqCst) == pid && scan_focused(pid) == ScanOutcome::NoPanel {
            PANEL_PID.store(0, Ordering::SeqCst);
            send(PanelEvent::Closed { pid });
        }
        return;
    }
    // 同一 app 内的焦点变化:只扫描，不据未命中发 Closed（不做跨 app 收敛）。
    let _ = scan_focused(pid);
}

/// 标记「本助手正在执行一次跳转」:子 sheet 出现后的瞬态 NoPanel 只在
/// `window` 内不解除面板归因。由 navigator 后台线程在发 ⌘⇧G 前调用;
/// 不显式清除,截止时刻自然过期(避免漏清导致永久压制)。
pub fn mark_navigating(window: Duration) {
    let until = Instant::now() + window;
    let mut slot = match NAVIGATING_UNTIL.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    *slot = Some(until);
}

fn navigating_active() -> bool {
    let slot = match NAVIGATING_UNTIL.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    slot.is_some_and(|until| Instant::now() < until)
}

/// 「存在其他 sheet + 无面板」是否应视为瞬态(不解除面板归因)。
///
/// 只有本助手跳转期间的 ⌘⇧G 子 sheet 属于此列;宿主的无关 sheet(文档里
/// 另开的模态框等)不能长期压制 `Closed`,否则同 app 的下一个保存面板会被
/// `handled_pids` 永久吞掉(评审 2026-11-06)。
/// 突变改成 `saw_sheet` → `transient_sheet_only_during_navigation` 必红。
fn transient_sheet_blocks_close(saw_sheet: bool, navigating: bool) -> bool {
    saw_sheet && navigating
}

/// 扫描前台 app 的焦点窗口(三态,见 [`ScanOutcome`])。
///
/// 只有「焦点窗口读取成功且不是保存面板」返回 `NoPanel`;任何 AX 读取失败
/// 或面板无法定位都返回 `Unknown`,避免把读取故障误判成面板已关闭。
fn scan_focused(pid: i32) -> ScanOutcome {
    let Some(app) = ax_ffi::app_element(pid) else {
        return ScanOutcome::Unknown;
    };
    // 候选窗口:焦点窗口优先 + AXWindows 全列表。实机(2026-11-05):浮层拿到
    // 键盘焦点后保存面板不再是 AXFocusedWindow(但仍在 AXWindows 中),只看
    // 焦点窗口会把「面板仍开着」误判成 NoPanel → 点击浮层即消失。
    let mut candidates: Vec<AXRef> = Vec::new();
    let mut read_failed = false;
    match ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) {
        Some(f) => candidates.push(f),
        None => read_failed = true,
    }
    match ax_ffi::copy_attr(&app, attr::WINDOWS)
        .as_ref()
        .and_then(ax_ffi::array_elements)
    {
        Some(mut w) => candidates.append(&mut w),
        None => read_failed = true,
    }
    // 任一候选命中保存面板且可定位 → Panel;全部读取成功且未命中 → NoPanel;
    // 有候选但任一读取失败 → Unknown(保守,不解除屏蔽)。
    // 观测留痕(2026-11-05 教训:扫描零日志使「绑定成功但判定未见」无从排查):
    // NoPanel 时输出全部候选紧凑形态(面板在窗口列表里但形态不匹配是本轮实测
    // 漏报特征),命中面板时输出命中形态。
    let mut dialog_rejected: Option<PanelSnapshot> = None;
    let mut saw_sheet = false;
    let mut cand_forms: Vec<String> = Vec::with_capacity(candidates.len());
    for win in &candidates {
        let snap = snapshot_of_window(win);
        cand_forms.push(snap_summary(&snap));
        if !is_save_panel(&snap) {
            if snap.role.as_deref() == Some(ROLE_QUICK_SHEET) {
                saw_sheet = true;
            }
            if dialog_rejected.is_none() && looks_like_panel(&snap) {
                dialog_rejected = Some(snap.clone());
            }
            continue;
        }
        log::info!("detector: 面板命中 pid={pid} {}", snap_summary(&snap));
        let Some((x, y)) = ax_ffi::attr_point(win, attr::POSITION) else {
            return ScanOutcome::Unknown;
        };
        let Some((w, h)) = ax_ffi::attr_size(win, attr::SIZE) else {
            return ScanOutcome::Unknown;
        };
        // M3:0/负/NaN/∞ 坐标或尺寸不是合法屏幕坐标(AX 布局未完成或读取异常),
        // 宁缺毋滥,避免把浮层定位到 (NaN,NaN) 或零尺寸。
        if !valid_panel_frame(x, y, w, h) {
            return ScanOutcome::Unknown;
        }
        // 同一面板已归因(例如 commit 后 ⌘⇧G sheet 开关触发的通知):不再重复
        // 发 Shown;跨线程去重下沉,接线层的 handled 门负责语义收敛。
        if PANEL_PID.swap(pid, Ordering::SeqCst) == pid {
            return ScanOutcome::Panel;
        }
        send(PanelEvent::Shown {
            pid,
            frame: [x, y, w, h],
        });
        return ScanOutcome::Panel;
    }
    if candidates.is_empty() || read_failed {
        log::info!(
            "detector: 扫描未知态 pid={pid} candidates={} read_failed={read_failed}",
            candidates.len()
        );
        return ScanOutcome::Unknown;
    }
    if let Some(s) = dialog_rejected {
        log::info!(
            "detector: 疑似面板被确认信号否决 pid={pid} {}",
            snap_summary(&s)
        );
    }
    // 子 sheet(⌘⇧G 前往文件夹)在场时保存面板会暂时不可见:仅在本助手
    // 跳转窗口内判 Unknown,不得发 Closed(见 transient_sheet_blocks_close)。
    let navigating = navigating_active();
    if transient_sheet_blocks_close(saw_sheet, navigating) {
        log::info!(
            "detector: 扫描未知态 pid={pid} candidates={} saw_sheet={saw_sheet} navigating={navigating} forms={cand_forms:?}",
            candidates.len()
        );
        return ScanOutcome::Unknown;
    }
    log::info!(
        "detector: 扫描确认无面板 pid={pid} candidates={} saw_sheet={saw_sheet} forms={:?}",
        candidates.len(),
        cand_forms
    );
    ScanOutcome::NoPanel
}

/// 快照单行摘要(日志形态取证用;role/subrole/标题/可编辑框/按钮/浏览容器)。
fn snap_summary(s: &PanelSnapshot) -> String {
    format!(
        "{}/{} {:?} e{} b{} r{} loc{}",
        s.role.as_deref().unwrap_or("?"),
        s.subrole.as_deref().unwrap_or("?"),
        s.title.as_deref().unwrap_or(""),
        s.editable_text_fields,
        s.buttons,
        s.browsers,
        s.location_hits
    )
}

/// 面板 frame 合法性:坐标/宽高必须有限,宽高必须为正(NaN/∞ 与 <=0 均拒绝)。
fn valid_panel_frame(x: f64, y: f64, w: f64, h: f64) -> bool {
    x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0
}

// —— 换绑重试(瞬态拒绝的恢复路径;常驻 timer + tick 倒数,detector 线程) ——

/// 换绑失败后安排延迟重试(detector 线程调用):写剩余 tick 数,常驻 timer
/// 每 [`RETRY_TICK_SECS`] 秒递减,到 1 触发 [`on_retry_tick`] 换绑。预算档位
/// 见 [`rebind_retry_delay_ticks`],耗尽不再排程。重复排程 = 覆盖剩余计数
/// (最新失败的节奏接管,旧倒数作废,timer 本体永不动)。
fn schedule_rebind_retry() {
    let attempt = REBIND_ATTEMPTS.load(Ordering::SeqCst);
    let Some(ticks) = rebind_retry_delay_ticks(attempt) else {
        log::info!(
            "detector: 重试预算耗尽,等待下次前台切换(front pid={})",
            FRONT_PID.load(Ordering::SeqCst)
        );
        return;
    };
    REBIND_ATTEMPTS.store(attempt + 1, Ordering::SeqCst);
    RETRY_TICKS_LEFT.store(ticks, Ordering::SeqCst);
    log::info!(
        "detector: 换绑失败,{}ms 后重试(第 {}/{} 次)",
        (RETRY_TICK_SECS * f64::from(ticks) * 1000.0) as u64,
        attempt + 1,
        REBIND_RETRY_DELAY_TICKS.len()
    );
}

/// 常驻重试 timer 的 tick(detector 线程,每 [`RETRY_TICK_SECS`] 秒):
/// 倒数到 1 时按需重试换绑。无在途倒数即空转返回(一次原子读)。
extern "C" fn on_retry_tick(_info: *mut std::ffi::c_void) {
    let left = RETRY_TICKS_LEFT.load(Ordering::SeqCst);
    if left == 0 {
        return;
    }
    if left > 1 {
        RETRY_TICKS_LEFT.store(left - 1, Ordering::SeqCst);
        return;
    }
    RETRY_TICKS_LEFT.store(0, Ordering::SeqCst);
    let pid = FRONT_PID.load(Ordering::SeqCst);
    let bound = BOUND_PID.with(|c| c.get());
    if needs_rebind(bound, pid) {
        rebind_and_scan(pid);
    }
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
            location_hits: 0,
        }
    }

    /// 契约:快速信号 + 数量确认 + 结构指纹三者齐备才判定,任一缺失为否。
    /// 突变「去掉 editable/button 数量关」→ 第 3/4 用例红;「去掉指纹关」→
    /// 第 5 用例红(sheet_without_location_fingerprint_rejected 亦红)。
    #[test]
    fn quick_and_confirm_signals_both_required() {
        let mut full = snap("AXSystemDialog", 1, 2);
        full.location_hits = 1;
        assert!(is_save_panel(&full), "三信号齐备");
        assert!(
            !is_save_panel(&snap("AXStandardWindow", 1, 2)),
            "subrole 不中"
        );
        assert!(
            !is_save_panel(&snap("AXSystemDialog", 0, 2)),
            "无可写输入框"
        );
        assert!(!is_save_panel(&snap("AXSystemDialog", 1, 0)), "无按钮");
        assert!(
            !is_save_panel(&snap("AXSystemDialog", 1, 2)),
            "无结构指纹(loc0 r1)不得判定"
        );
    }

    /// 标题不参与判定:打开面板标题为「打开」仍判定为是。
    /// 钉住不得按标题过滤;open/save 的区分不在判定函数职责内(设计 §3.2 触发范围:不区分)。
    #[test]
    fn title_not_part_of_judgment_open_title_still_matches() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.location_hits = 1;
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

    /// 实测形态(2026-11-05,TextEdit/XMind/Edge 三家一致):保存面板以
    /// AXSheet 附着宿主窗口,subrole 读不到(None),确认信号 e3/b11、位置名
    /// 命中 ≥1(位置侧栏)。突变去掉 `role == ROLE_QUICK_SHEET` 路径 → 必红。
    #[test]
    fn sheet_form_without_subrole_matches_with_confirm() {
        let mut s = snap("AXSystemDialog", 3, 11);
        s.role = Some("AXSheet".into());
        s.subrole = None;
        s.location_hits = 1;
        s.browsers = 6;
        assert!(
            is_save_panel(&s),
            "AXSheet + 数量信号 + 位置指纹(实测漏报形态)"
        );
        s.buttons = 0;
        assert!(!is_save_panel(&s), "缺按钮仍否决(确认信号不豁免)");
        s.buttons = 11;
        s.editable_text_fields = 0;
        assert!(!is_save_panel(&s), "缺可编辑框仍否决(确认信号不豁免)");
        s.editable_text_fields = 3;
        s.role = Some("AXWindow".into());
        assert!(
            !is_save_panel(&s),
            "AXWindow 无 subrole 不判定(防误报守卫保留)"
        );
    }

    /// sheet 结构指纹(误报回归钉,2026-11-05 实机):数量阈值挡不住「查找框
    /// e2 b3 / 密码 sheet e2 b2 / 格式弹层」——它们无位置侧栏且浏览容器少。
    /// 突变去掉 `(location_hits >= 1 || browsers >= MIN)` → 用例必红。
    #[test]
    fn sheet_without_location_fingerprint_rejected() {
        let mut s = snap("AXSystemDialog", 2, 3);
        s.role = Some("AXSheet".into());
        s.subrole = None;
        s.location_hits = 0;
        s.browsers = 1;
        assert!(!is_save_panel(&s), "查找框形态(无位置指纹)不得判定");
        s.buttons = 2;
        s.editable_text_fields = 2;
        assert!(!is_save_panel(&s), "密码 sheet 形态不得判定");
        // 侧栏隐藏的极简面板兜底:数量满足 + 浏览容器 ≥3 仍判定。
        s.browsers = 3;
        assert!(is_save_panel(&s), "浏览容器 ≥3 兜底(极简保存面板)");
        // 位置指纹兜底:容器不足但有侧栏命中仍判定。
        s.browsers = 1;
        s.location_hits = 1;
        assert!(is_save_panel(&s), "位置指纹命中兜底");
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
        let mut sheet = snap("", 0, 0);
        sheet.role = Some("AXSheet".into());
        sheet.subrole = None;
        assert!(looks_like_panel(&sheet), "AXSheet role 形态");
        let mut window_form = snap("AXStandardWindow", 0, 0);
        window_form.title = Some("打开".into());
        assert!(looks_like_panel(&window_form), "标准窗+标题形态");
    }

    /// 已登记风险闭环(2026-11-05 实机):AXFloatingWindow + 可编辑输入框曾会
    /// 命中产品判定(Get Info/Inspector 误报嫌疑)——结构指纹挡住:此类窗口
    /// 无位置侧栏且容器少;带指纹的浮窗形态面板(如有)仍可放行。
    #[test]
    fn floating_window_without_fingerprint_rejected() {
        let mut s = snap("AXFloatingWindow", 1, 1);
        assert!(!is_save_panel(&s), "浮窗无指纹(等效 Get Info)不判定");
        s.location_hits = 1;
        assert!(is_save_panel(&s), "带指纹才放行");
    }

    /// 实机误报回归钉(2026-11-05):WPS 查找/查找和替换为窗口形态
    /// (AXDialog e1 b9/e1 b12),旧判定纯数量即放行误触发浮层;
    /// 结构指纹(无位置侧栏、无浏览容器)闭环。突变去掉指纹关 → 本用例必红。
    #[test]
    fn find_replace_dialog_window_form_rejected() {
        let mut s = snap("AXDialog", 1, 9);
        s.title = Some("查找".into());
        assert!(!is_save_panel(&s), "查找框(无指纹)不得判定");
        let mut r = snap("AXDialog", 1, 12);
        r.title = Some("查找和替换".into());
        assert!(!is_save_panel(&r), "查找和替换不得判定");
        // 真面板对照:另存为窗口形态有浏览容器(位置侧栏),放行。
        let mut save = snap("AXDialog", 3, 22);
        save.title = Some("另存为".into());
        save.browsers = 8;
        assert!(is_save_panel(&save), "另存为窗口形态放行");
    }

    /// 漏报回归钉(2026-11-05):部分 app 的打开面板以 AXStandardWindow 标准窗
    /// 形态呈现(XMind 实测 e1 b15 r8 loc7),仅「标准窗 + 标题命中面板名单 +
    /// 指纹」放行。突变去掉标题路径 → 本用例必红。
    #[test]
    fn standard_window_open_panel_matches_via_title() {
        let mut s = snap("AXStandardWindow", 1, 15);
        s.title = Some("打开".into());
        s.browsers = 8;
        s.location_hits = 7;
        assert!(is_save_panel(&s), "标准窗形态打开面板放行");
    }

    /// 标准窗标题路径防误报钉(2026-11-05 实机):普通文档/网页窗口也可能带
    /// 位置名指纹(标题栏路径/最近列表/页面内容),标题是唯一分界——必须
    /// 精确命中面板名单才放行。突变放宽标题匹配 → 用例必红。
    #[test]
    fn standard_window_documents_rejected_by_title_gate() {
        let mut ppt = snap("AXStandardWindow", 1, 42);
        ppt.title = Some("部委经营思路汇报".into());
        ppt.browsers = 4;
        ppt.location_hits = 7;
        assert!(!is_save_panel(&ppt), "文档窗口(带指纹无面板标题)不判定");
        let mut web = snap("AXStandardWindow", 5, 24);
        web.title = Some("搜索 - Microsoft 必应".into());
        web.browsers = 8;
        web.location_hits = 2;
        assert!(!is_save_panel(&web), "网页窗口不判定");
        let mut lookalike = snap("AXStandardWindow", 1, 15);
        lookalike.title = Some("打开指导书.docx".into());
        lookalike.browsers = 8;
        lookalike.location_hits = 7;
        assert!(
            !is_save_panel(&lookalike),
            "文档名含「打开」但非精确相等不判定"
        );
    }

    /// M4 三态收敛 × 绑定门:只有「绑定成功 **且** 确认无面板」才发 Closed;
    /// 绑定失败(状态无人观察)/AX 读取失败不得解除屏蔽,命中面板也不发。
    /// 突变 `should_send_closed_on_rebind` 去掉 `bound_ok &&` → 绑定失败用例必红。
    #[test]
    fn rebind_closed_only_on_bound_and_confirmed_no_panel() {
        assert!(!should_send_closed_on_rebind(true, ScanOutcome::Panel));
        assert!(should_send_closed_on_rebind(true, ScanOutcome::NoPanel));
        assert!(
            !should_send_closed_on_rebind(true, ScanOutcome::Unknown),
            "AX 读取失败不得解除屏蔽"
        );
        assert!(
            !should_send_closed_on_rebind(false, ScanOutcome::NoPanel),
            "绑定失败不得解除屏蔽"
        );
    }

    /// 子 sheet 瞬态只在「本助手跳转窗口内」压制 Closed:宿主无关 sheet
    /// 不得长期吞掉同 app 的下一个面板(否则 handled_pids 永久泄漏)。
    /// 突变改成 `saw_sheet` → 第二断言必红。
    #[test]
    fn transient_sheet_only_during_navigation() {
        assert!(transient_sheet_blocks_close(true, true));
        assert!(
            !transient_sheet_blocks_close(true, false),
            "非跳转期的其他 sheet 不得压制 Closed"
        );
        assert!(!transient_sheet_blocks_close(false, true));
        assert!(!transient_sheet_blocks_close(false, false));
    }

    /// 重试档位:5 档 tick(1/2/4/8/16 = 0.25s~4s),超预算 None(放弃,等
    /// 下次激活重置)。突变改 `REBIND_RETRY_DELAY_TICKS` 长度或值 → 用例必红。
    #[test]
    fn rebind_retry_schedule_five_steps_then_give_up() {
        assert_eq!(rebind_retry_delay_ticks(0), Some(1));
        assert_eq!(rebind_retry_delay_ticks(1), Some(2));
        assert_eq!(rebind_retry_delay_ticks(4), Some(16));
        assert_eq!(rebind_retry_delay_ticks(5), None, "预算耗尽必须放弃");
        assert_eq!(rebind_retry_delay_ticks(usize::MAX), None);
    }

    /// M2:仅当前台 pid 有效且变化时才换绑;相同 pid 的重复 signal 不得重复换绑/扫描。
    #[test]
    fn rebind_only_on_front_pid_change() {
        assert!(!needs_rebind(0, 0), "前台未知不触发");
        assert!(needs_rebind(0, 42), "首次绑定");
        assert!(!needs_rebind(42, 42), "前台未变不换绑");
        assert!(needs_rebind(42, 7), "前台变化换绑");
    }

    /// M4 契约收窄(2026-11-05 评审):HostChanged 只在真宿主切换时发出;
    /// 重试成功绑定「归因即同一 pid」不算宿主切换(归因保留,面板可能仍开着)。
    /// 突变去掉 `previous != bound_pid` → 用例必红。
    #[test]
    fn host_changed_only_on_real_host_switch() {
        assert!(!host_changed_on_rebind(0, 42), "无归因不发");
        assert!(
            host_changed_on_rebind(7, 42),
            "归因宿主 ≠ 新绑定宿主 = 真切换"
        );
        assert!(
            !host_changed_on_rebind(42, 42),
            "归因即新绑定宿主(重试路径)不发,保留归因"
        );
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

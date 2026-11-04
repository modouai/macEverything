//! M0 探针(save-panel-assist 风险点的实机取证工具;`MCE_PANEL_PROBE=1` 启用)。
//! 单一专用 runloop 线程:CF timer 每 500ms 轮询前台 pid → 变更即重建
//! AXObserver 并注册窗口通知 → 通知回调 dump 窗口特征快照 → 疑似保存面板
//! 触发跳转实验(策略 A → 失败转策略 B),全程 `log::info!` 取证。
//! 只做日志取证,不进产品路径;产品化 detector/navigator(Task 8-11)从本模块演化。
//! 验证点对应:②策略 A 直设 ③post-to-pid 有效性 ⑤输入框 AX 定位 ⑥按钮 press
//! vs Return ⑦AX 后台线程调用(实验线程即后台线程)。
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use super::ax_ffi::{self, attr, note, AXObserverWrap, AXRef, CFStr};
use super::cg_events::{self, FLAG_CMD, FLAG_SHIFT, KEY_G, KEY_RETURN, KEY_V};

/// 前台 pid(timer 轮询写入;AX 回调读取,同线程)。
static FRONT_PID: AtomicI32 = AtomicI32::new(0);
/// 探针运行标志(幂等启动)。
static PROBE_ON: AtomicBool = AtomicBool::new(false);
/// 疑似面板命中(同一面板只实验一次;面板销毁即重置)。
static PANEL_HIT: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// 当前 observer(本线程创建/挂载;与回调同线程,无 Send 需求)。
    static OBSERVER: RefCell<Option<AXObserverWrap>> = const { RefCell::new(None) };
    /// 退役 observer 保活池:重建时不 CFRelease 旧 observer(其 runloop source
    /// 可能仍挂在 runloop 上,释放后 source 归属未定义)。探针生命周期内存活,
    /// 量级 = 前台切换次数,泄漏可控;M0 取证工具可接受。
    static RETIRED: RefCell<Vec<AXObserverWrap>> = const { RefCell::new(Vec::new()) };
}

/// 探针入口(main.rs 按环境变量调用;幂等)。
pub fn start_probe() {
    if PROBE_ON.swap(true, Ordering::SeqCst) {
        return;
    }
    if !ax_ffi::is_process_trusted() {
        log::error!("探针未启动:辅助功能权限未授予(系统设置 → 隐私与安全性 → 辅助功能)");
        return;
    }
    let h = std::thread::Builder::new()
        .name("panel-probe".to_owned())
        .spawn(runloop_thread)
        .expect("探针线程创建失败");
    std::mem::forget(h); // 进程生命周期线程
}

fn runloop_thread() {
    // 首个 observer 绑 pid=0 占位(poll 到真实前台后重建);source 挂本线程 runloop。
    let obs = ax_ffi::observer_create(0, on_ax_event).expect("AXObserverCreate 失败(权限?)");
    ax_ffi::attach_observer_to_current_runloop(&obs);
    OBSERVER.with(|c| *c.borrow_mut() = Some(obs));
    install_poll_timer();
    log::info!("[probe] 运行:前台切换/面板窗口/跳转实验将写日志");
    ax_ffi::run_current_runloop();
}

// —— CF timer 轮询(本线程 runloop;符号在本模块声明,runloop 侧复用 ax_ffi) ——

/// CFRunLoopTimerContext 手工镜像(字段由 C 侧读取,Rust 侧只构造零值)。
#[repr(C)]
#[allow(dead_code)] // 字段全部由 CFRunLoopTimerCreate 读取,Rust 侧无读点
struct TimerContext {
    version: isize,
    info: *mut std::ffi::c_void,
    retain: usize,
    release: usize,
    describe: usize,
}

extern "C" {
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopTimerCreate(
        alloc: *mut std::ffi::c_void,
        fire_date: f64,
        interval: f64,
        tolerance: f64,
        order: isize,
        callout: extern "C" fn(*mut std::ffi::c_void),
        context: *mut TimerContext,
    ) -> *mut std::ffi::c_void;
    fn CFRunLoopAddTimer(
        rl: *mut std::ffi::c_void,
        t: *mut std::ffi::c_void,
        mode: *const std::ffi::c_void,
    );
}

extern "C" fn poll_callout(_ctx: *mut std::ffi::c_void) {
    poll_frontmost();
}

/// 安装 500ms 轮询 timer(常驻,随进程存活;context 零值 = 无 info)。
fn install_poll_timer() {
    // SAFETY:context 为本函数栈上零值结构,timer 创建后归 runloop 所有;
    // 本函数不 CFRelease(+1 有意交给 runloop,timer 常驻随 runloop 存活)。
    unsafe {
        let mut ctx = TimerContext {
            version: 0,
            info: std::ptr::null_mut(),
            retain: 0,
            release: 0,
            describe: 0,
        };
        let t = CFRunLoopTimerCreate(
            std::ptr::null_mut(),
            CFAbsoluteTimeGetCurrent() + 0.5,
            0.5,
            0.0,
            0,
            poll_callout,
            &mut ctx,
        );
        assert!(!t.is_null(), "CFRunLoopTimerCreate 失败");
        CFRunLoopAddTimer(ax_ffi::current_runloop_raw(), t, ax_ffi::default_mode_raw());
        // 不 CFRelease:+1 所有权有意交给 runloop(timer 常驻,随进程存活);
        // t 为裸指针(Copy),仅需 let _ 消去未用告警。
        let _ = t;
    }
}

/// 轮询:前台 pid 变化 → 重建 observer 换绑 + 立即扫一遍现有窗口。
fn poll_frontmost() {
    let Some(app) = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return;
    };
    let pid = app.processIdentifier();
    let prev = FRONT_PID.swap(pid, Ordering::SeqCst);
    if prev == pid {
        return;
    }
    PANEL_HIT.store(false, Ordering::SeqCst);
    log::info!(
        "[probe] 前台切换 pid={pid} name={:?}",
        app.localizedName().map(|n| n.to_string())
    );
    rebind_observer(pid);
    scan_existing_windows(pid);
}

/// 重建 observer 并注册三类窗口通知(全部在本 runloop 线程,AXObserverWrap 无 Send)。
fn rebind_observer(pid: i32) {
    let Some(el) = ax_ffi::app_element(pid) else {
        log::warn!("[probe] app element 创建失败 pid={pid}");
        return;
    };
    let obs = ax_ffi::observer_create(pid, on_ax_event).expect("AXObserverCreate 失败");
    for n in [
        note::WINDOW_CREATED,
        note::FOCUSED_WINDOW_CHANGED,
        note::UI_ELEMENT_DESTROYED,
    ] {
        match ax_ffi::observer_add_notification(&obs, &el, n, 0) {
            Ok(()) => log::info!("[probe] 通知已注册 pid={pid} {n}"),
            // 个别 app 拒绝 AX 观察(隐藏 AX 树):单条失败不致命,继续其余通知
            Err(e) => log::warn!("[probe] 通知注册失败 pid={pid} {n} err={e}"),
        }
    }
    ax_ffi::attach_observer_to_current_runloop(&obs);
    OBSERVER.with(|c| {
        // 旧 observer 保活进 RETIRED(见字段注释),新 observer 上位。
        if let Some(old) = c.borrow_mut().take() {
            RETIRED.with(|r| r.borrow_mut().push(old));
        }
        *c.borrow_mut() = Some(obs);
    });
}

/// 立即扫一遍焦点窗口(面板可能在换绑前已弹出)。
fn scan_existing_windows(pid: i32) {
    let Some(app) = ax_ffi::app_element(pid) else {
        return;
    };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        log::info!("[probe] pid={pid} 无焦点窗口");
        return;
    };
    inspect_window(pid, &win);
}

/// AXObserver 回调(静态;读 FRONT_PID 转发扫描)。参数按 C ABI 展开
/// (observer/element 为 *mut,CFStringRef 通知名为 *const)。
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
        PANEL_HIT.store(false, Ordering::SeqCst);
        log::info!("[probe] 面板销毁(pid={pid}),命中标志重置");
        return;
    }
    log::info!("[probe] AX 通知 pid={pid} {n}");
    let Some(app) = ax_ffi::app_element(pid) else {
        return;
    };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        return;
    };
    inspect_window(pid, &win);
}

/// 窗口检查:特征快照 dump + 疑似面板判定 + 触发跳转实验。
fn inspect_window(pid: i32, win: &AXRef) {
    let snap = super::panel_detector::snapshot_of_window(win);
    log::info!("[probe] 窗口快照 pid={pid} {snap:?}");
    if !super::panel_detector::looks_like_panel(&snap) {
        return;
    }
    if PANEL_HIT.swap(true, Ordering::SeqCst) {
        return; // 同一面板只实验一次
    }
    log::info!("[probe] 命中疑似保存面板,2s 后开始跳转实验 pid={pid}");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        run_jump_experiment(pid);
    });
}

// —— 跳转实验:策略 A → 失败转策略 B;目标 /tmp(存在可写,实验无害) ——

fn run_jump_experiment(pid: i32) {
    let target = "/tmp";
    log::info!("[probe] === 策略 A:⌘⇧G → AX 直设 value → AXPress 前往 ===");
    cg_events::post_combo(Some(pid), KEY_G, FLAG_CMD | FLAG_SHIFT);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let field = loop {
        if let Some(f) = find_goto_field(pid) {
            break Some(f);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let Some(field) = field else {
        log::warn!("[probe] ⌘⇧G 后 3s 内未定位到 Go-to 输入框(验证点⑤)");
        strategy_b(pid, target);
        return;
    };
    log::info!("[probe] Go-to 输入框已定位(验证点⑤ 通过)");
    let path_cf = CFStr::new(target);
    match ax_ffi::set_attr(&field, attr::VALUE, &path_cf) {
        Ok(()) => log::info!("[probe] AX set value 成功(验证点② 可行)"),
        Err(e) => {
            log::warn!("[probe] AX set value 失败 err={e}(验证点② 不可行,策略 B 兜底)");
            strategy_b(pid, target);
            return;
        }
    }
    match find_goto_button(pid) {
        Some(btn) => {
            let ok = ax_ffi::perform_action(&btn, "AXPress");
            log::info!("[probe] AXPress 前往按钮 ok={ok}(验证点⑥ 按钮路径)");
        }
        None => {
            log::warn!("[probe] 未找到前往按钮,回退合成 Return(验证点⑥ 键盘路径)");
            cg_events::post_combo(Some(pid), KEY_RETURN, 0);
        }
    }
    log::info!("[probe] === 策略 A 完成,人工核对面板是否已跳到 {target} ===");
}

/// 策略 B:剪贴板 save/set + ⌘V + Return + restore。
fn strategy_b(pid: i32, target: &str) {
    log::info!("[probe] === 策略 B:剪贴板 + ⌘V + Return ===");
    let saved = super::clipboard_text();
    super::clipboard_set_text(target);
    cg_events::post_combo(Some(pid), KEY_V, FLAG_CMD);
    std::thread::sleep(Duration::from_millis(300));
    cg_events::post_combo(Some(pid), KEY_RETURN, 0);
    std::thread::sleep(Duration::from_millis(300));
    let ok = super::clipboard_restore(saved.as_deref());
    log::info!("[probe] 策略 B 完成,剪贴板恢复 ok={ok};人工核对跳转与剪贴板");
}

/// 浅层谓词搜索:可写的 AXTextField(Go-to 输入框;深度限 6,AX IPC 有开销)。
fn find_goto_field(pid: i32) -> Option<AXRef> {
    let app = ax_ffi::app_element(pid)?;
    let win = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW)?;
    search(&win, 0, &|el| {
        ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXTextField")
            && ax_ffi::attr_settable(el, attr::VALUE)
    })
}

/// 「前往」按钮:AXButton 中 title/description 含 go/前往(大小写不敏感);
/// 全部按钮标题先打印(M0 人工核对本地化形态)再按谓词命中。
fn find_goto_button(pid: i32) -> Option<AXRef> {
    let app = ax_ffi::app_element(pid)?;
    let win = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW)?;
    let btns = collect(&win, 0, &|el| {
        ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXButton")
    });
    for b in &btns {
        log::info!(
            "[probe] 按钮 title={:?} desc={:?}",
            ax_ffi::attr_string(b, attr::TITLE),
            ax_ffi::attr_string(b, "AXDescription")
        );
    }
    btns.into_iter().find(|b| {
        [attr::TITLE, "AXDescription"].iter().any(|k| {
            ax_ffi::attr_string(b, k).is_some_and(|s| {
                let l = s.to_lowercase();
                l.contains("go") || l.contains("前往")
            })
        })
    })
}

/// 带深度限的谓词搜索(命中即克隆引用返回)。
fn search(el: &AXRef, depth: usize, pred: &dyn Fn(&AXRef) -> bool) -> Option<AXRef> {
    if pred(el) {
        return Some(ax_ffi::clone_ref(el));
    }
    if depth >= 6 {
        return None;
    }
    for c in ax_ffi::children(el) {
        if let Some(hit) = search(&c, depth + 1, pred) {
            return Some(hit);
        }
    }
    None
}

/// 带深度限的谓词收集。
fn collect(el: &AXRef, depth: usize, pred: &dyn Fn(&AXRef) -> bool) -> Vec<AXRef> {
    let mut out = Vec::new();
    walk(el, depth, pred, &mut out);
    out
}

fn walk(el: &AXRef, depth: usize, pred: &dyn Fn(&AXRef) -> bool, out: &mut Vec<AXRef>) {
    if pred(el) {
        out.push(ax_ffi::clone_ref(el));
    }
    if depth >= 6 {
        return;
    }
    for c in ax_ffi::children(el) {
        walk(&c, depth + 1, pred, out);
    }
}

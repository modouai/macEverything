//! M0 探针(save-panel-assist 风险点的开发取证工具;`MCE_PANEL_PROBE=1` 启用)。
//! 单一专用 runloop 线程:CF timer 每 500ms 轮询前台 pid → 变更即重建
//! AXObserver 并注册窗口通知 → 通知回调 dump 窗口特征快照 → 疑似保存面板
//! 触发跳转实验(策略 A → 失败转策略 B),全程 `log::info!` 取证。
//! 只做日志取证,不进产品路径;产品化 detector/navigator(Task 8-11)从本模块演化。
//! 验证点对应:②策略 A 直设 ③post-to-pid 有效性 ⑤输入框 AX 定位 ⑥按钮 press
//! vs Return ⑦AX 后台线程调用(实验线程即后台线程)。
//! 证据有效性约束:接受「Go-to 输入框」须焦点一致 + ⌘⇧G 后新增两道复核
//! (防误中文件名框假报⑤);每次合成键前复核前台 pid(见 cg_events 时效契约)。
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use super::ax_ffi::{self, attr, note, AXObserverWrap, AXRef, CFStr};
use super::cg_events::{self, FLAG_CMD, FLAG_SHIFT, KEY_G, KEY_RETURN, KEY_V};

/// 前台 pid(timer 轮询写入;AX 回调读取,同线程)。
static FRONT_PID: AtomicI32 = AtomicI32::new(0);
/// 探针运行标志(幂等启动)。
static PROBE_ON: AtomicBool = AtomicBool::new(false);
/// 疑似面板命中(同一面板只实验一次;app 元素上任何 UI_ELEMENT_DESTROYED 都
/// 重置——比「面板销毁」宽,取证宁滥)。
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
    let spawned = std::thread::Builder::new()
        .name("panel-probe".to_owned())
        .spawn(runloop_thread);
    match spawned {
        Ok(h) => {
            let _ = h; // 立即 drop = 分离线程(runloop 常驻到进程退出)
        }
        Err(e) => log::error!("探针线程创建失败:{e}"),
    }
}

fn runloop_thread() {
    // 首个 observer 绑 pid=0 占位(poll 到真实前台后重建);source 挂本线程 runloop。
    // 线程内 fatal 失败走 log::error + return(expect 的 stderr 在 Finder 启动
    // 形态不可见,取证工具必须留日志)。
    let Some(obs) = ax_ffi::observer_create(0, on_ax_event) else {
        log::error!("[probe] AXObserverCreate 失败(辅助功能权限未授予?),探针线程退出");
        return;
    };
    ax_ffi::attach_observer_to_current_runloop(&obs);
    OBSERVER.with(|c| *c.borrow_mut() = Some(obs));
    if !install_poll_timer() {
        log::error!("[probe] 轮询 timer 安装失败,探针线程退出(无轮询即失效)");
        return;
    }
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
/// 返回 false = 创建失败(调用方结束探针线程——无轮询即失效,继续跑 runloop
/// 只剩无事件源的 observer,无意义)。
fn install_poll_timer() -> bool {
    // SAFETY:context 为本函数栈上零值结构,timer 创建后归 runloop 所有;
    // CFRunLoopAddTimer 后 runloop 另持自身引用,本调用方持有的 +1 不再释放
    // 属有意常驻(timer 随进程存活,探针无拆卸路径)。
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
        if t.is_null() {
            log::error!("[probe] CFRunLoopTimerCreate 失败");
            return false;
        }
        CFRunLoopAddTimer(ax_ffi::current_runloop_raw(), t, ax_ffi::default_mode_raw());
        let _ = t;
    }
    true
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
    if !rebind_observer(pid) {
        // 换绑失败回滚前台 pid:下轮轮询重试,避免该 app 永久失联。
        FRONT_PID.store(prev, Ordering::SeqCst);
        return;
    }
    scan_existing_windows(pid);
}

/// 重建 observer 并注册三类窗口通知(全部在本 runloop 线程,AXObserverWrap 无 Send)。
/// 返回 false = 换绑失败(调用方回滚 FRONT_PID 重试)。
fn rebind_observer(pid: i32) -> bool {
    let Some(el) = ax_ffi::app_element(pid) else {
        log::warn!("[probe] app element 创建失败 pid={pid}");
        return false;
    };
    let Some(obs) = ax_ffi::observer_create(pid, on_ax_event) else {
        log::error!("[probe] AXObserverCreate 失败 pid={pid}(资源/权限),换绑中止");
        return false;
    };
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
    true
}

/// 立即扫一遍焦点窗口(面板可能在换绑前已弹出)。
fn scan_existing_windows(pid: i32) {
    let Some(app) = ax_ffi::app_element(pid) else {
        log::debug!("[probe] pid={pid} app element 不可用,跳过现有窗口扫描");
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
/// 归因注记:RETIRED 池中旧 observer 的残留通知仍会派发进本回调,读到的是
/// FRONT_PID 的**新**值——后果仅为日志归因错位,取证可接受。
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
        // app 元素上任何销毁通知都重置(比面板销毁宽,取证宁滥;见 PANEL_HIT 注)。
        PANEL_HIT.store(false, Ordering::SeqCst);
        log::info!("[probe] UI_ELEMENT_DESTROYED(pid={pid}),命中标志重置");
        return;
    }
    log::info!("[probe] AX 通知 pid={pid} {n}");
    let Some(app) = ax_ffi::app_element(pid) else {
        log::debug!("[probe] 通知 {n}:app element 不可用 pid={pid}");
        return;
    };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        log::debug!("[probe] 通知 {n}:无焦点窗口 pid={pid}");
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
    // 实验线程独立命名;线程内 panic 只写 stderr(Finder 启动形态不可见),
    // 故 catch_unwind 拦下留痕,并重置命中标志允许下一轮重试。
    match std::thread::Builder::new()
        .name("panel-probe-jump".to_owned())
        .spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            // SAFETY 口径:闭包只触静态与本地 Copy(pid),AssertUnwindSafe 无悬垂风险。
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_jump_experiment(pid);
            }));
            if r.is_err() {
                log::error!("[probe] 跳转实验 panic(细节见系统 panic 钩子),重置命中标志");
                PANEL_HIT.store(false, Ordering::SeqCst);
            }
        }) {
        Ok(h) => {
            let _ = h; // 分离线程(实验结束自然退出)
        }
        Err(e) => {
            log::error!("[probe] 实验线程创建失败:{e},重置命中标志");
            PANEL_HIT.store(false, Ordering::SeqCst);
        }
    }
}

// —— 跳转实验:策略 A → 失败转策略 B;目标 /tmp(存在可写,实验无害) ——

/// pid 时效复核(cg_events::post_combo 契约):实验捕获 pid 是否仍为前台。
/// 不等即中止——进程退出后 pid 被回收复用时,组合键会送进无关进程。
fn pid_still_front(pid: i32) -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .is_some_and(|app| app.processIdentifier() == pid)
}

/// 定位结果:命中(已过两道复核)/ 焦点仍在旧控件(附其 role,判 ③)/ 无候选。
enum Locate {
    Found(AXRef),
    FocusedStale(String),
    None,
}

/// 谓词:可写文本框(AXTextField 且 VALUE 可写)。Go-to 框与文件名框同满足,
/// 区分靠「焦点一致 + ⌘⇧G 后新增」两道复核,见 [`locate_goto_field`]。
fn writable_text_field(el: &AXRef) -> bool {
    ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXTextField")
        && ax_ffi::attr_settable(el, attr::VALUE)
}

/// ⌘⇧G 前已存在的可写文本框(clone_ref 保活,作 CFEqual 身份判定基线)。
/// AXUIElementCopyAttributeValue 每次可返回同一底层元素的新代理实例,裸 CF
/// 指针不等 ≠ 异元素——基线成员比对必须走 CFEqual(见 [`locate_goto_field`])。
fn baseline_text_fields(pid: i32) -> Vec<AXRef> {
    let Some(app) = ax_ffi::app_element(pid) else {
        return Vec::new();
    };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        return Vec::new();
    };
    collect(&win, 0, &writable_text_field)
}

/// Go-to 输入框定位(验证点⑤)带证据复核:
/// 首选 = app 当前焦点元素(`AXFocusedElement`)本身满足谓词且为 ⌘⇧G 后新增
/// (sheet 打开时焦点即其输入框);回退 = 全树谓词搜索,但命中还须与焦点元素
/// 同一且新增——防 ⌘⇧G 未生效时误中文件名框(覆盖用户文件名 + 假报⑤)。
/// 身份判定一律 CFEqual:代理重建下裸指针比较会漏判,CF 规范比较仍正确。
fn locate_goto_field(pid: i32, pre_fields: &[AXRef]) -> Locate {
    let Some(app) = ax_ffi::app_element(pid) else {
        return Locate::None;
    };
    let focused = ax_ffi::copy_attr(&app, attr::FOCUSED_ELEMENT);
    let focused_role = focused
        .as_ref()
        .and_then(|fe| ax_ffi::attr_string(fe, attr::ROLE));
    // 首选:焦点元素即 Go-to 框。
    if let Some(fe) = focused.as_ref() {
        if writable_text_field(fe) && !pre_fields.iter().any(|p| ax_ffi::cf_equal(p, fe)) {
            return Locate::Found(ax_ffi::clone_ref(fe));
        }
    }
    // 回退:全树谓词搜索,命中须与焦点同一且非基线成员。
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        return Locate::None;
    };
    let Some(cand) = search(&win, 0, &writable_text_field) else {
        // 全树无候选:焦点若仍停在旧文本框,即 ⌘⇧G 未生效的直接特征(③失败)。
        return match focused_role {
            Some(r) if r == "AXTextField" => Locate::FocusedStale(r),
            _ => Locate::None,
        };
    };
    let same_as_focus = focused.is_some_and(|fe| ax_ffi::cf_equal(&fe, &cand));
    let is_new = !pre_fields.iter().any(|p| ax_ffi::cf_equal(p, &cand));
    if same_as_focus && is_new {
        Locate::Found(cand)
    } else {
        Locate::FocusedStale(focused_role.unwrap_or_else(|| "<未知焦点>".to_owned()))
    }
}

fn run_jump_experiment(pid: i32) {
    let target = "/tmp";
    if !pid_still_front(pid) {
        log::warn!("[probe] 前台已切换,中止实验 pid={pid}");
        return;
    }
    // ⌘⇧G 前基线(见 baseline_text_fields):防误中文件名框的旧元素集合。
    let pre_fields = baseline_text_fields(pid);
    log::info!("[probe] === 策略 A:⌘⇧G → AX 直设 value → AXPress 前往 ===");
    cg_events::post_combo(Some(pid), KEY_G, FLAG_CMD | FLAG_SHIFT);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    // 轮询 3s:sheet 弹出有延迟,「焦点仍在旧控件」可能只是慢——到 deadline
    // 仍如此才判 ③失败;期间焦点若转到新增框即接受(验证点⑤)。
    let mut stale_focus: Option<String> = None;
    let field = loop {
        match locate_goto_field(pid, &pre_fields) {
            Locate::Found(f) => break Some(f),
            Locate::FocusedStale(role) => stale_focus = Some(role),
            Locate::None => {}
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let Some(field) = field else {
        // ③失败(post 未生效,焦点没动)与⑤失败(定位不到)在日志区分开。
        match stale_focus {
            Some(role) => log::warn!("[probe] ⌘⇧G 未生效(验证点③失败),焦点仍在 {role}"),
            None => log::warn!("[probe] ⌘⇧G 后 3s 内未定位到 Go-to 输入框(验证点⑤)"),
        }
        strategy_b(pid, target);
        return;
    };
    log::info!(
        "[probe] Go-to 输入框已定位(验证点⑤ 通过) EDITABLE={:?}(兑现 ax_ffi AXEditable 待验证项)",
        ax_ffi::attr_string(&field, attr::EDITABLE)
    );
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
            if pid_still_front(pid) {
                cg_events::post_combo(Some(pid), KEY_RETURN, 0);
            } else {
                log::warn!("[probe] 前台已切换,跳过 Return 合成");
            }
        }
    }
    log::info!("[probe] === 策略 A 完成,人工核对面板是否已跳到 {target} ===");
}

/// 策略 B:剪贴板 save/set + ⌘V + Return + restore(每个 post 点前复核 pid;
/// 中止路径也恢复剪贴板,不留脏状态)。
fn strategy_b(pid: i32, target: &str) {
    log::info!("[probe] === 策略 B:剪贴板 + ⌘V + Return ===");
    if !pid_still_front(pid) {
        log::warn!("[probe] 前台已切换,中止实验 pid={pid}");
        return;
    }
    let saved = super::clipboard_text();
    if !super::clipboard_set_text(target) {
        log::warn!("[probe] 剪贴板写纯文本失败,⌘V 可能粘贴旧内容");
    }
    cg_events::post_combo(Some(pid), KEY_V, FLAG_CMD);
    std::thread::sleep(Duration::from_millis(300));
    if !pid_still_front(pid) {
        log::warn!("[probe] 前台已切换,跳过 Return 合成并恢复剪贴板");
        let ok = super::clipboard_restore(saved.as_deref());
        log::info!("[probe] 剪贴板恢复 ok={ok}");
        return;
    }
    cg_events::post_combo(Some(pid), KEY_RETURN, 0);
    std::thread::sleep(Duration::from_millis(300));
    let ok = super::clipboard_restore(saved.as_deref());
    log::info!("[probe] 策略 B 完成,剪贴板恢复 ok={ok};人工核对跳转与剪贴板");
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

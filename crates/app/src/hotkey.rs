//! 全局热键:⌥Space 族唤起/隐藏切换 + ⌥⇧F 完整窗口直达(Task 15/21;
//! product §1「按下 ⌥Space(全局快捷键)→ 悬浮条出现;再按 ⌥Space 或 Esc 或失焦 →
//! 隐藏」、§2「全局快捷键 ⌥⇧F 直达」)。
//!
//! ## 默认键与降级链(PoC-2 定案)
//!
//! 三候选本机全部注册成功、无系统符号热键占用(PoC-2 结论表 #1/#2);辉哥确认
//! ⌥Space 无冲突 → 默认键 = ⌥Space,注册失败按 `candidates()` 顺序自动降级。
//! ⌥⇧F(product §2:字母键避开空格族的系统占用)独立注册;注册失败不降级
//! (完整窗口入口仍有托盘与悬浮条 ⌥↵),日志留痕。
//!
//! ## 事件泵为什么必须带唤醒(与 plan 示意的差异,证据链)
//!
//! 1. Carbon 热键事件进 channel 的前提是主线程 AppKit 事件循环在派发(PoC-2 隔离
//!    实验:裸进程裸泵收不到,`NSApplication` + nextEvent 拉取才收到)——eframe 的
//!    `NSApplication::run()` 天然满足,这一点与 PoC-2 结论一致;
//! 2. 但 `App::logic` 只在 **ROOT 视口出帧**时被调(eframe 0.37 `epi_integration.rs`
//!    `update` / `update_logic_only` 两处调用点),而出帧只由 repaint 请求驱动:
//!    `run.rs::check_redraw_requests` 在 `windows_next_repaint_times` /
//!    `windows_redraw_deadlines` 有到期项时才跑帧,空闲即显式 `ControlFlow::Wait`;
//! 3. 热键事件到达只唤醒 CFRunLoop 并交给 Carbon handler 写 channel,不产生任何
//!    repaint 请求(eframe 0.37 的 `WinitAppWrapper` 未实现 `about_to_wait`,winit 的
//!    `AboutToWait`/`NewEvents` 都不触发帧)→ 隐藏/空闲期只 try_recv 收不到 —— 事件
//!    会滞留在 channel 里直到下一帧(可能永不发生);
//! 4. 故事件泵线程除收事件外还负责唤醒(见下节)。
//!
//! ## 动作直调(主队列)与帧内兜底(Task 21;为什么不能只靠「叫帧」)
//!
//! 唤醒 UI 的传统做法是泵线程 `ctx.request_repaint_of(ROOT)`(eframe 的
//! `EventLoopProxy`,任意线程可调)。但产品形态两窗皆隐(Task 21 启动形态),而
//! eframe 对**不可见窗口**的跨线程 repaint 请求有硬性节流 ≥100ms
//! (`native/run.rs:27` `INVISIBLE_WINDOW_REPAINT_INTERVAL`;`run.rs:355-380` 的
//! `is_invisible_or_minimized` 分支)——等帧即破「唤起响应 <100ms」红线
//! (Task 15 遗留,Task 21 实测复现后修)。
//!
//! 故按下后**主队列直调动作**(`dispatch2::DispatchQueue::main().exec_async`;
//! PoC-4 native 路径同法:块在主线程执行、不等帧,对外可见中位 16.7ms):
//! - 悬浮条经原生 `orderFrontRegardless` 显示后窗口即 `isVisible`,随后的
//!   repaint 请求不再落进节流;
//! - 直调目标由 ui 层在共享态构造完成后回填(`attach`;install 时共享态尚不存在),
//!   回调实现即 `App` 帧内同一份逻辑(见 ui/mod.rs `toggle_palette_locked` 等);
//! - 回调拿不到共享锁(帧内持锁:如原生右键菜单弹出期的嵌套 run loop)时返回
//!   false → 动作入**兜底队列** + 叫帧,由 `App::logic` 的 `poll_action` 消费——
//!   该路径受上述 ≤100ms 节流,属罕见兜底,不静默丢弃。
//!
//! 线程模型与 CPU:线程阻塞在 crossbeam `recv`(park,非忙等);隐藏期无帧、无线程
//! 唤醒,CPU 0%(product §1 红线)。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

/// 候选热键(顺序 = 降级链;PoC-2 三候选注册 3/3 成功,默认取首个)。
/// ⌃Space / ⌃⌥Space = 系统输入法切换、⌘Space / ⌘⌥Space = Spotlight(本机实测占用),
/// 三候选均不与系统符号热键冲突(PoC-2 结论表 #2)。
///
/// 函数而非 const:keyboard-types 的 `Modifiers` 位或非 const 运算(bitflags 2.x)。
pub fn candidates() -> [(Modifiers, Code, &'static str); 3] {
    [
        (Modifiers::ALT, Code::Space, "⌥Space"),
        (Modifiers::ALT | Modifiers::SHIFT, Code::Space, "⌥⇧Space"),
        (
            Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT,
            Code::Space,
            "⌃⌥⇧Space",
        ),
    ]
}

/// 完整窗口直达键(product §2:⌥⇧F;显示名与注册同源,禁散落字符串)。
pub fn direct_key() -> (Modifiers, Code, &'static str) {
    (Modifiers::ALT | Modifiers::SHIFT, Code::KeyF, "⌥⇧F")
}

/// 全部候选注册失败时的悬浮条提示(托盘唤起 Task 16 接,先给文案)。
pub const UNAVAILABLE_NOTE: &str = "快捷键不可用,可从托盘唤起";

/// 热键动作(直调回调与兜底队列共用;ui 层按此分派到同一份实现)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// ⌥Space:唤起/隐藏切换(悬浮条)。
    TogglePalette,
    /// ⌥⇧F:完整窗口直达(product §2)。
    OpenResults,
}

/// 主队列直调目标:主线程执行,返回 true = 已即时处理(不再入兜底队列)。
pub type DispatchFn = Arc<dyn Fn(Action) -> bool + Send + Sync + 'static>;

/// 按 `candidates()` 顺序注册,首个成功即返回 `(命中的 HotKey, 显示名)`;
/// 全部失败 → `Err`(附逐个候选的失败原因)。
///
/// 必须在主线程调用(crate 文档:macOS 上 manager 须在主线程创建,且事件循环在跑)。
pub fn register_default(manager: &GlobalHotKeyManager) -> anyhow::Result<(HotKey, String)> {
    let mut failures = Vec::new();
    for (mods, code, name) in candidates() {
        let hotkey = HotKey::new(Some(mods), code);
        match manager.register(hotkey) {
            Ok(()) => return Ok((hotkey, name.to_string())),
            // 注册失败信息不含 OSStatus 码(PoC-2 差异 #3):「被占用」与其它 OS 错误
            // 无法区分,统一按注册失败降级。
            Err(err) => failures.push(format!("{name}: {err}")),
        }
    }
    Err(anyhow::anyhow!(
        "候选热键全部注册失败({})",
        failures.join(";")
    ))
}

/// 事件是否命中注册键的「按下」(Released 忽略:id 由 crate 计算勿自行拼,PoC-2 差异 #2)。
///
/// key repeat:macOS 走 Carbon,长按不产生重复 Pressed(PoC-2 实测),故不设去抖。
pub fn is_toggle_press(event: &GlobalHotKeyEvent, registered: HotKey) -> bool {
    event.state == HotKeyState::Pressed && event.id == registered.id()
}

/// 注册结果 → 悬浮条提示(纯逻辑):默认键正常 = 不打扰;降级/全失败 = 常驻提示。
fn note_for(registered: Option<&str>) -> Option<String> {
    let default_name = candidates()[0].2;
    match registered {
        Some(name) if name == default_name => None,
        Some(name) => Some(format!("{default_name} 不可用,已改用 {name}")),
        None => Some(UNAVAILABLE_NOTE.to_string()),
    }
}

/// 全局热键运行时(挂 `ui::App` 保活):
/// manager drop 即注销全部热键,须与进程同寿。
pub struct HotkeyRuntime {
    /// 注册状态的宿主;创建失败时为 None(热键不可用,其余功能照常)。
    _manager: Option<GlobalHotKeyManager>,
    /// 命中键显示名(日志用;None = 全候选失败)。
    name: Option<String>,
    /// 悬浮条提示(降级/不可用;默认键正常 = None)。
    note: Option<String>,
    /// 主队列直调目标(ui 层启动期回填;未回填时按下走兜底队列)。
    on_action: Arc<OnceLock<DispatchFn>>,
    /// 兜底队列(直调未竟的动作;`poll_action` 帧内消费)。
    deferred: Arc<Mutex<VecDeque<Action>>>,
}

impl HotkeyRuntime {
    /// 命中键显示名(未注册 = None)。
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// 悬浮条常驻提示(降级/不可用;默认键正常 = None)。
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// 回填主队列直调目标(ui 层在共享态构造完成后调用;进程内一次)。
    pub fn attach(&self, f: impl Fn(Action) -> bool + Send + Sync + 'static) {
        if self.on_action.set(Arc::new(f)).is_err() {
            log::warn!("热键直调目标重复回填(忽略后填)");
        }
    }
}

/// 注册热键并起事件泵线程(主线程调用;失败不中断 App,降级为提示 + 日志)。
pub fn install(ctx: &egui::Context) -> HotkeyRuntime {
    let on_action: Arc<OnceLock<DispatchFn>> = Arc::new(OnceLock::new());
    let deferred: Arc<Mutex<VecDeque<Action>>> = Arc::new(Mutex::new(VecDeque::new()));
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            log::error!("全局热键管理器创建失败(热键不可用):{err}");
            return HotkeyRuntime {
                _manager: None,
                name: None,
                note: note_for(None),
                on_action,
                deferred,
            };
        }
    };
    // 完整窗口直达键(⌥⇧F):失败只留痕,不降级(入口仍有托盘与悬浮条 ⌥↵)。
    let (dmods, dcode, dname) = direct_key();
    let direct = HotKey::new(Some(dmods), dcode);
    let direct = match manager.register(direct) {
        Ok(()) => {
            log::info!("完整窗口直达键注册成功:{dname}(id={})", direct.id());
            Some(direct)
        }
        Err(err) => {
            log::warn!("完整窗口直达键({dname})注册失败(入口仍有托盘与悬浮条 ⌥↵):{err}");
            None
        }
    };
    // ⌥Space 族(降级链):全失败只降级提示,不影响 ⌥⇧F 的命中。
    let (space, name, note) = match register_default(&manager) {
        Ok((hotkey, name)) => {
            log::info!("全局热键注册成功:{name}(id={})", hotkey.id());
            let note = note_for(Some(&name));
            if let Some(text) = &note {
                log::warn!("热键降级:{text}");
            }
            (Some(hotkey), Some(name), note)
        }
        Err(err) => {
            log::error!("全局热键不可用({UNAVAILABLE_NOTE}):{err:#}");
            (None, None, note_for(None))
        }
    };
    if space.is_some() || direct.is_some() {
        spawn_event_pump(
            ctx.clone(),
            space,
            direct,
            Arc::clone(&on_action),
            Arc::clone(&deferred),
        );
    }
    HotkeyRuntime {
        _manager: Some(manager),
        name,
        note,
        on_action,
        deferred,
    }
}

/// UI 帧内消费一条兜底动作(无则 None;直调路径正常时队列恒空)。
pub fn poll_action(runtime: &HotkeyRuntime) -> Option<Action> {
    let mut q = match runtime.deferred.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    q.pop_front()
}

/// 事件泵线程:阻塞收官方 channel(零轮询);命中注册键的 Pressed → 主队列直调动作。
///
/// `space` = ⌥Space 族命中键、`direct` = ⌥⇧F(各自 None = 该键未注册成功,不参与判定)。
fn spawn_event_pump(
    ctx: egui::Context,
    space: Option<HotKey>,
    direct: Option<HotKey>,
    on_action: Arc<OnceLock<DispatchFn>>,
    deferred: Arc<Mutex<VecDeque<Action>>>,
) {
    let spawned = std::thread::Builder::new()
        .name("hotkey-pump".to_string())
        .spawn(move || {
            // 静态 channel(PoC-2 用法:receiver 全局唯一,本线程是唯一消费者)。
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(event) = rx.recv() {
                let action = if space.is_some_and(|k| is_toggle_press(&event, k)) {
                    Some(Action::TogglePalette)
                } else if direct.is_some_and(|k| is_toggle_press(&event, k)) {
                    Some(Action::OpenResults)
                } else {
                    None
                };
                let Some(action) = action else { continue };
                // 直调块:`Context`/共享态句柄均 Send+Sync;块本身在主线程执行。
                let ctx = ctx.clone();
                let on_action = Arc::clone(&on_action);
                let deferred = Arc::clone(&deferred);
                dispatch2::DispatchQueue::main().exec_async(move || {
                    if let Some(f) = on_action.get() {
                        if f(action) {
                            log::info!("热键动作 {action:?}:主队列直调完成");
                            return;
                        }
                    }
                    // 兜底:直调目标未回填或共享锁被持(帧内拼锁/嵌套 run loop)→
                    // 入列 + 叫帧。不可见窗口的跨线程 repaint 请求被 eframe 节流
                    // ≥100ms,故该路径仅兜底,不静默丢弃。
                    log::info!("热键动作 {action:?}:转帧内兜底队列");
                    match deferred.lock() {
                        Ok(mut q) => q.push_back(action),
                        Err(poisoned) => poisoned.into_inner().push_back(action),
                    }
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                });
            }
        });
    match spawned {
        Ok(handle) => {
            log::info!("热键事件泵线程已启动(阻塞收事件,非轮询)");
            std::mem::drop(handle); // 分离:进程退出即终止,无汇合点
        }
        Err(err) => log::error!("热键事件泵线程启动失败(热键将无响应):{err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 降级链顺序与显示名(product §边界:⌥Space → ⌥⇧Space → ⌃⌥⇧Space)。
    #[test]
    fn candidates_degrade_chain_order() {
        let list = candidates();
        assert_eq!(list[0].2, "⌥Space", "默认键 = ⌥Space(PoC-2 定案)");
        assert_eq!(list[1].2, "⌥⇧Space");
        assert_eq!(list[2].2, "⌃⌥⇧Space");
        assert_eq!(list[0].0, Modifiers::ALT);
        assert!(list[1].0.contains(Modifiers::SHIFT));
        assert!(list[2].0.contains(Modifiers::CONTROL));
        for (_, code, _) in list {
            assert_eq!(code, Code::Space, "族内三候选均为 Space 键");
        }
    }

    /// 完整窗口直达键 = ⌥⇧F(product §2:字母键,避开空格族系统占用)。
    #[test]
    fn direct_key_is_alt_shift_f() {
        let (mods, code, name) = direct_key();
        assert_eq!(name, "⌥⇧F");
        assert_eq!(code, Code::KeyF);
        assert_eq!(mods, Modifiers::ALT | Modifiers::SHIFT);
    }

    /// 命中判定:只认注册键的 Pressed;Released(松开)与他键不触发。
    #[test]
    fn is_toggle_press_only_registered_pressed() {
        let registered = HotKey::new(Some(Modifiers::ALT), Code::Space);
        let pressed = GlobalHotKeyEvent {
            id: registered.id(),
            state: HotKeyState::Pressed,
        };
        let released = GlobalHotKeyEvent {
            id: registered.id(),
            state: HotKeyState::Released,
        };
        let other = GlobalHotKeyEvent {
            id: registered.id() ^ 0x1,
            state: HotKeyState::Pressed,
        };
        assert!(is_toggle_press(&pressed, registered));
        assert!(!is_toggle_press(&released, registered), "Released 忽略");
        assert!(!is_toggle_press(&other, registered), "他键不触发");
    }

    /// 兜底队列:空 = None;入列动作按先进先出逐条消费,取尽不再重复。
    #[test]
    fn poll_action_drains_deferred_fifo() {
        let runtime = HotkeyRuntime {
            _manager: None,
            name: None,
            note: None,
            on_action: Arc::new(OnceLock::new()),
            deferred: Arc::new(Mutex::new(VecDeque::new())),
        };
        assert_eq!(poll_action(&runtime), None, "队列空");
        {
            let mut q = runtime.deferred.lock().unwrap();
            q.push_back(Action::TogglePalette);
            q.push_back(Action::OpenResults);
        }
        assert_eq!(
            poll_action(&runtime),
            Some(Action::TogglePalette),
            "先入先出"
        );
        assert_eq!(poll_action(&runtime), Some(Action::OpenResults));
        assert_eq!(poll_action(&runtime), None, "取尽不再重复");
    }

    /// 提示文案:默认键正常不打扰;降级提示改用键;全失败提示托盘兜底。
    #[test]
    fn note_maps_registration_outcome() {
        assert_eq!(note_for(Some("⌥Space")), None, "默认键正常 = 无提示");
        assert_eq!(
            note_for(Some("⌥⇧Space")).as_deref(),
            Some("⌥Space 不可用,已改用 ⌥⇧Space")
        );
        assert_eq!(
            note_for(Some("⌃⌥⇧Space")).as_deref(),
            Some("⌥Space 不可用,已改用 ⌃⌥⇧Space")
        );
        assert_eq!(note_for(None).as_deref(), Some(UNAVAILABLE_NOTE));
    }
}

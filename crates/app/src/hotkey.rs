//! 全局热键:⌥Space 族唤起/隐藏切换(Task 15;product §1「按下 ⌥Space(全局快捷键)
//! → 悬浮条出现;再按 ⌥Space 或 Esc 或失焦 → 隐藏」)。
//!
//! ## 默认键与降级链(PoC-2 定案)
//!
//! 三候选本机全部注册成功、无系统符号热键占用(PoC-2 结论表 #1/#2);辉哥确认
//! ⌥Space 无冲突 → 默认键 = ⌥Space,注册失败按 `candidates()` 顺序自动降级。
//! plan 原稿曾把「事件泵」写成 UI 帧内 `GlobalHotKeyEvent::receiver().try_recv()`,
//! 实测接线后偏离为「后台线程阻塞收 + 唤醒 UI 帧」,理由见下节。
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
//! 4. 故事件泵线程除收事件外还负责**唤醒**:`ctx.request_repaint_of(ROOT)` 走
//!    eframe 的 `set_request_repaint_callback`(EventLoopProxy),任意线程可调、
//!    可唤醒 Wait 中的事件循环(PoC-4 结论 #5 已实证)。
//!
//! 线程模型与 CPU:线程阻塞在 crossbeam `recv`(park,非忙等);隐藏期无帧、无线程
//! 唤醒,CPU 0%(product §1 红线)。按下触发后:计数 +1(帧内消费,一帧至多切换一次,
//! 连按合并)+ 请求 ROOT 重绘 → `logic` 补跑 → `poll_toggle` 返回 true。
//!
//! 节流注记:ROOT 窗不可见时 eframe 对跨线程 repaint 请求有 100ms 节流
//! (`run.rs:27 INVISIBLE_WINDOW_REPAINT_INTERVAL`)。开发形态根窗可见不受影响;
//! 产品形态(两窗皆隐)的唤醒延迟上界 ≈ 100ms + 帧耗时,Task 17/20 接线时复测。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

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

/// 全部候选注册失败时的悬浮条提示(托盘唤起 Task 16 接,先给文案)。
pub const UNAVAILABLE_NOTE: &str = "快捷键不可用,可从托盘唤起";

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
    /// 事件泵计数(泵线程 fetch_add;`poll_toggle` 帧内消费)。
    counter: Option<Arc<AtomicU64>>,
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
}

/// 注册默认热键并起事件泵线程(主线程调用;失败不中断 App,降级为提示 + 日志)。
pub fn install(ctx: &egui::Context) -> HotkeyRuntime {
    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            log::error!("全局热键管理器创建失败(热键不可用):{err}");
            return HotkeyRuntime {
                _manager: None,
                name: None,
                note: note_for(None),
                counter: None,
            };
        }
    };
    match register_default(&manager) {
        Ok((hotkey, name)) => {
            log::info!("全局热键注册成功:{name}(id={})", hotkey.id());
            let counter = spawn_event_pump(ctx.clone(), hotkey);
            let note = note_for(Some(&name));
            if let Some(text) = &note {
                log::warn!("热键降级:{text}");
            }
            HotkeyRuntime {
                _manager: Some(manager),
                name: Some(name),
                note,
                counter: Some(counter),
            }
        }
        Err(err) => {
            log::error!("全局热键不可用({UNAVAILABLE_NOTE}):{err:#}");
            HotkeyRuntime {
                _manager: Some(manager),
                name: None,
                note: note_for(None),
                counter: None,
            }
        }
    }
}

/// UI 帧内消费(每帧至多切换一次):有待处理按下 → true 并清零。
pub fn poll_toggle(runtime: &HotkeyRuntime) -> bool {
    runtime
        .counter
        .as_ref()
        .is_some_and(|c| c.swap(0, Ordering::AcqRel) > 0)
}

/// 事件泵线程:阻塞收官方 channel(零轮询),命中注册键的 Pressed → 计数 + 唤醒 UI。
fn spawn_event_pump(ctx: egui::Context, registered: HotKey) -> Arc<AtomicU64> {
    let hits = Arc::new(AtomicU64::new(0));
    let pump_hits = Arc::clone(&hits);
    let spawned = std::thread::Builder::new()
        .name("hotkey-pump".to_string())
        .spawn(move || {
            // 静态 channel(PoC-2 用法:receiver 全局唯一,本线程是唯一消费者)。
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(event) = rx.recv() {
                if is_toggle_press(&event, registered) {
                    pump_hits.fetch_add(1, Ordering::AcqRel);
                    // 唤醒 ROOT 视口帧(App::logic 的宿主;模块注释「事件泵为什么
                    // 必须带唤醒」);窗口不可见时该帧退化为 logic-only。
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
            }
        });
    match spawned {
        Ok(handle) => {
            log::info!("热键事件泵线程已启动(阻塞收事件,非轮询)");
            std::mem::drop(handle); // 分离:进程退出即终止,无汇合点
        }
        Err(err) => log::error!("热键事件泵线程启动失败(热键将无响应):{err}"),
    }
    hits
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

    /// 帧内消费语义:无按下 = false;积压 N 次按下 = 一次 true 后清零(连按合并)。
    #[test]
    fn poll_toggle_consumes_pending_presses() {
        let counter = Arc::new(AtomicU64::new(0));
        let runtime = HotkeyRuntime {
            _manager: None,
            name: None,
            note: None,
            counter: Some(Arc::clone(&counter)),
        };
        assert!(!poll_toggle(&runtime), "无按下");
        counter.fetch_add(3, Ordering::AcqRel);
        assert!(poll_toggle(&runtime), "积压按下 → true");
        assert!(!poll_toggle(&runtime), "已消费,不重复触发");
    }

    /// 未注册(全失败/管理器创建失败)时 poll_toggle 恒 false(不误触发)。
    #[test]
    fn poll_toggle_without_registration_never_fires() {
        let runtime = HotkeyRuntime {
            _manager: None,
            name: None,
            note: Some(UNAVAILABLE_NOTE.to_string()),
            counter: None,
        };
        assert!(!poll_toggle(&runtime));
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

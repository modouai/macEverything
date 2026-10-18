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
//! ## 可重配置与启动偏好(设置 · 快捷键 Task H2)
//!
//! `settings.toml` 的 `palette_hotkey` / `window_hotkey` 存在时优先注册自定义键;
//! 校验失败或注册失败(被占用等)回落该槽默认键并留提示:
//! - 唤起槽回落 `candidates()` 降级链;
//! - 完整窗口槽回落 [`direct_key`]。
//!
//! 提示按槽拆分:唤起槽提示进 `HotkeySnapshot.note`(悬浮条常驻警告),
//! 完整窗口槽提示进 `HotkeySnapshot.window_note`(仅设置页展示),互不串台。
//!
//! 未配置(默认)时行为与旧版一致。两个槽的当前键存于 `Arc<Mutex<ActiveKeys>>`,
//! 事件泵每次事件读取**当前** id(不再捕获固定键);因此 [`HotkeyRuntime::commit`]
//! 换键后泵立即生效。注册/注销必须回主线程执行:install / `App::logic` 之外的
//! 设置 UI 不得直接调用 `suspend`/`resume`/`commit`(设置页通过命令队列交给 App)。
//!
//! ## 录制期与提交回滚(设置页录制的语义)
//!
//! - [`HotkeyRuntime::suspend`][]:注销两槽并记住旧键(`suspended`),泵读到的 active
//!   为空 → 录制按键不会触发旧动作;快照仍显示旧键(设置页录制中仍可看到另一槽)。
//! - [`HotkeyRuntime::resume`][]:取消录制,重新注册旧键;尽力恢复,失败留提示。
//! - [`HotkeyRuntime::commit`][]:校验白名单 + 拒绝与另一槽重复,再注册新键。
//!   - 挂起态(正常录制路径):**任何**错误(校验/重复/注册失败)都尽力恢复两个
//!     旧键并**退出挂起**(`suspended = None`);设置页失败后直接结束录制、旧键仍可用。
//!   - 非挂起态:同键提交 = 幂等成功;否则旧键保持注册,直接注册新键,成功后注销
//!     该槽旧键,失败不动旧键。
//!     成功统一清空两槽启动回退提示(执行提交即视为用户已确认当前键位)。
//! - [`HotkeyRuntime::reset_slot`][]:恢复该槽内置默认(录制中 Delete/Backspace)。
//!   唤起槽走 `candidates()` 首个成功项(降级时 `note` 如实提示),完整窗口槽走
//!   `direct_key()`。挂起态与 commit 同语义:先恢复另一槽旧键,再注册默认;
//!   任何失败都尽力恢复两个旧键并退出挂起。非挂起态先注册默认(旧键仍在),
//!   成功后才注销该槽旧键,失败不动旧键。默认键本身不需再校验/查重。
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
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::i18n;
use crate::settings::SettingsFile;
use crate::ui::hotkey_keys;

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

/// 持久热键提示（跨帧展示；语言切换时按当前语言重新渲染）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyNote {
    Fallback { default: String, used: String },
    Unavailable,
    ResumeFailed,
    SavedPaletteDefault { custom: String, default: String },
    SavedPaletteOther { custom: String, used: String },
    SavedPaletteUnavailable { custom: String },
    SavedWindowConflictDefault { custom: String, default: String },
    SavedWindowDefault { custom: String, default: String },
    SavedWindowConflictUnavailable { custom: String },
    SavedWindowUnavailable { custom: String },
}

impl HotkeyNote {
    pub fn text(&self) -> String {
        let s = i18n::s();
        match self {
            Self::Fallback { default, used } => i18n::fill(
                s.hotkey_fallback,
                &[("default", default.as_str()), ("name", used.as_str())],
            ),
            Self::Unavailable => s.hotkey_unavailable.to_owned(),
            Self::ResumeFailed => s.hotkey_resume_failed.to_owned(),
            Self::SavedPaletteDefault { custom, default } => i18n::fill(
                s.hotkey_saved_palette_default,
                &[("custom", custom.as_str()), ("name", default.as_str())],
            ),
            Self::SavedPaletteOther { custom, used } => i18n::fill(
                s.hotkey_saved_palette_other,
                &[("custom", custom.as_str()), ("name", used.as_str())],
            ),
            Self::SavedPaletteUnavailable { custom } => i18n::fill(
                s.hotkey_saved_palette_unavailable,
                &[
                    ("custom", custom.as_str()),
                    ("reason", s.hotkey_unavailable_reason),
                ],
            ),
            Self::SavedWindowConflictDefault { custom, default } => i18n::fill(
                s.hotkey_saved_window_conflict_default,
                &[("custom", custom.as_str()), ("name", default.as_str())],
            ),
            Self::SavedWindowDefault { custom, default } => i18n::fill(
                s.hotkey_saved_window_default,
                &[("custom", custom.as_str()), ("name", default.as_str())],
            ),
            Self::SavedWindowConflictUnavailable { custom } => i18n::fill(
                s.hotkey_saved_window_conflict_unavailable,
                &[
                    ("custom", custom.as_str()),
                    ("reason", s.window_hotkey_unavailable_reason),
                ],
            ),
            Self::SavedWindowUnavailable { custom } => i18n::fill(
                s.hotkey_saved_window_unavailable,
                &[
                    ("custom", custom.as_str()),
                    ("reason", s.window_hotkey_unavailable_reason),
                ],
            ),
        }
    }
}

/// 热键动作(直调回调与兜底队列共用;ui 层按此分派到同一份实现)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// ⌥Space:唤起/隐藏切换(悬浮条)。
    TogglePalette,
    /// ⌥⇧F:完整窗口直达(product §2)。
    OpenResults,
}

/// 可配置的两个热键槽(设置页录制与提交按此定位)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeySlot {
    /// 唤起/隐藏悬浮条。
    Palette,
    /// 完整窗口直达。
    Window,
}

/// 当前注册的热键(事件泵每事件读取)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ActiveKeys {
    palette: Option<HotKey>,
    window: Option<HotKey>,
}

impl ActiveKeys {
    fn get(self, slot: HotkeySlot) -> Option<HotKey> {
        match slot {
            HotkeySlot::Palette => self.palette,
            HotkeySlot::Window => self.window,
        }
    }

    fn set(&mut self, slot: HotkeySlot, hotkey: Option<HotKey>) {
        match slot {
            HotkeySlot::Palette => self.palette = hotkey,
            HotkeySlot::Window => self.window = hotkey,
        }
    }

    /// 另一槽的热键(重复校验用)。
    fn other(self, slot: HotkeySlot) -> Option<HotKey> {
        match slot {
            HotkeySlot::Palette => self.window,
            HotkeySlot::Window => self.palette,
        }
    }
}

/// 设置页展示/录制用的热键快照(纯数据;设置页消费)。
///
/// `palette_keys` / `window_keys` 是 macOS 键帽符号(⌃⌥⇧⌘ + 键名),槽位未注册时
/// 为空 `Vec`。回退提示按槽拆分:`note` = 唤起槽(悬浮条常驻警告只读它),
/// `window_note` = 完整窗口槽(仅设置页展示);正常均为 `None`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotkeySnapshot {
    /// 唤起槽已注册键。
    pub palette: Option<HotKey>,
    /// 完整窗口槽已注册键。
    pub window: Option<HotKey>,
    /// 唤起槽键帽展示(如 `["⌥", "Space"]`)。
    pub palette_keys: Vec<String>,
    /// 完整窗口槽键帽展示(如 `["⌥", "⇧", "F"]`)。
    pub window_keys: Vec<String>,
    /// 唤起槽启动回落/不可用提示(悬浮条警告;正常 = None)。
    pub note: Option<HotkeyNote>,
    /// 完整窗口槽启动回落/不可用提示(仅设置页;正常 = None)。
    pub window_note: Option<HotkeyNote>,
}

impl HotkeySnapshot {
    fn from_active(
        active: ActiveKeys,
        note: Option<HotkeyNote>,
        window_note: Option<HotkeyNote>,
    ) -> Self {
        Self {
            palette: active.palette,
            window: active.window,
            palette_keys: active
                .palette
                .map(hotkey_keys::display_keys)
                .unwrap_or_default(),
            window_keys: active
                .window
                .map(hotkey_keys::display_keys)
                .unwrap_or_default(),
            note,
            window_note,
        }
    }
}

/// 主队列直调目标:主线程执行,返回 true = 已即时处理(不再入兜底队列)。
pub type DispatchFn = Arc<dyn Fn(Action) -> bool + Send + Sync + 'static>;

/// 注册/注销抽象:生产实现包 `GlobalHotKeyManager`,单测用假实现验证纯规划
/// (不创建 manager、不碰系统热键,CI/无头环境可跑)。
trait KeyRegistrar {
    fn register(&mut self, hotkey: HotKey) -> Result<(), String>;
    fn unregister(&mut self, hotkey: HotKey) -> Result<(), String>;
}

struct ManagerRegistrar<'a>(&'a GlobalHotKeyManager);

impl KeyRegistrar for ManagerRegistrar<'_> {
    fn register(&mut self, hotkey: HotKey) -> Result<(), String> {
        self.0.register(hotkey).map_err(|err| err.to_string())
    }

    fn unregister(&mut self, hotkey: HotKey) -> Result<(), String> {
        self.0.unregister(hotkey).map_err(|err| err.to_string())
    }
}

/// 事件是否命中注册键的「按下」(Released 忽略:id 由 crate 计算勿自行拼,PoC-2 差异 #2)。
///
/// key repeat:macOS 走 Carbon,长按不产生重复 Pressed(PoC-2 实测),故不设去抖。
pub fn is_toggle_press(event: &GlobalHotKeyEvent, registered: HotKey) -> bool {
    event.state == HotKeyState::Pressed && event.id == registered.id()
}

/// 事件 + 当前 active 键 → 动作(泵线程纯判定;双槽未注册 = 无动作)。
fn action_for(event: &GlobalHotKeyEvent, keys: ActiveKeys) -> Option<Action> {
    if keys.palette.is_some_and(|key| is_toggle_press(event, key)) {
        Some(Action::TogglePalette)
    } else if keys.window.is_some_and(|key| is_toggle_press(event, key)) {
        Some(Action::OpenResults)
    } else {
        None
    }
}

/// 注册结果 → 悬浮条提示(纯逻辑):默认键正常 = 不打扰;降级/全失败 = 常驻提示。
fn note_for(registered: Option<&str>) -> Option<HotkeyNote> {
    let default_name = candidates()[0].2;
    match registered {
        Some(name) if name == default_name => None,
        Some(name) => Some(HotkeyNote::Fallback {
            default: default_name.to_owned(),
            used: name.to_owned(),
        }),
        None => Some(HotkeyNote::Unavailable),
    }
}

/// 键帽符号串(提示文案用;如 `⌃⌥K`)。
fn display_name(hotkey: HotKey) -> String {
    hotkey_keys::display_keys(hotkey).concat()
}

/// 校验 + 注册:来自设置文件/录制的键必须先过白名单,再交给系统注册。
fn register_checked<R: KeyRegistrar>(reg: &mut R, hotkey: HotKey) -> Result<(), String> {
    hotkey_keys::validate(hotkey)
        .map_err(|err| err.to_string())
        .and_then(|()| reg.register(hotkey))
}

struct DefaultPalette {
    hotkey: Option<HotKey>,
    name: Option<&'static str>,
}

/// 按 `candidates()` 顺序注册,首个成功即返回;全部失败 → None。
fn register_palette_default<R: KeyRegistrar>(reg: &mut R) -> DefaultPalette {
    register_palette_default_with(reg, None)
}

/// 同 [`register_palette_default`],但把已注册的 `already` 视为该候选成功
/// (非挂起态 reset:旧键仍注册,不得重复注册同一组合而误判失败)。
fn register_palette_default_with<R: KeyRegistrar>(
    reg: &mut R,
    already: Option<HotKey>,
) -> DefaultPalette {
    let mut failures = Vec::new();
    for (mods, code, name) in candidates() {
        let hotkey = HotKey::new(Some(mods), code);
        if already == Some(hotkey) {
            return DefaultPalette {
                hotkey: Some(hotkey),
                name: Some(name),
            };
        }
        match reg.register(hotkey) {
            Ok(()) => {
                return DefaultPalette {
                    hotkey: Some(hotkey),
                    name: Some(name),
                }
            }
            // 注册失败信息不含 OSStatus 码(PoC-2 差异 #3):「被占用」与其它 OS 错误
            // 无法区分,统一按注册失败降级。
            Err(err) => failures.push(format!("{name}: {err}")),
        }
    }
    log::error!("候选热键全部注册失败({})", failures.join(";"));
    DefaultPalette {
        hotkey: None,
        name: None,
    }
}

/// 自定义唤起键失败后的回落提示(区分「默认键」与「降级键」)。
fn palette_fallback_note(custom: HotKey, fallback_name: Option<&str>) -> HotkeyNote {
    let custom = display_name(custom);
    match fallback_name {
        Some(name) if name == candidates()[0].2 => HotkeyNote::SavedPaletteDefault {
            custom,
            default: name.to_owned(),
        },
        Some(name) => HotkeyNote::SavedPaletteOther {
            custom,
            used: name.to_owned(),
        },
        None => HotkeyNote::SavedPaletteUnavailable { custom },
    }
}

/// 完整窗口槽回落默认直键(⌥⇧F);`custom` 为 Some 表示是自定义键失败后的回落。
/// `conflict` = 自定义键与唤起槽相同(提示文案须区分「不可用」与「冲突」)。
fn register_window_default<R: KeyRegistrar>(
    reg: &mut R,
    custom: Option<HotKey>,
    conflict: bool,
) -> (Option<HotKey>, Option<HotkeyNote>) {
    let (mods, code, name) = direct_key();
    let hotkey = HotKey::new(Some(mods), code);
    match reg.register(hotkey) {
        Ok(()) => {
            let note = custom.map(|custom| {
                let custom = display_name(custom);
                if conflict {
                    HotkeyNote::SavedWindowConflictDefault {
                        custom,
                        default: name.to_owned(),
                    }
                } else {
                    HotkeyNote::SavedWindowDefault {
                        custom,
                        default: name.to_owned(),
                    }
                }
            });
            (Some(hotkey), note)
        }
        Err(err) => {
            log::warn!("完整窗口快捷键({name})注册失败(入口仍有托盘与悬浮条 ⌥↵): {err}");
            // 无自定义键时保持旧行为:只留日志不常驻提示;自定义回落失败才提示。
            let note = custom.map(|custom| {
                let custom = display_name(custom);
                if conflict {
                    HotkeyNote::SavedWindowConflictUnavailable { custom }
                } else {
                    HotkeyNote::SavedWindowUnavailable { custom }
                }
            });
            (None, note)
        }
    }
}

/// 启动偏好选择(纯规划器):自定义键有效且注册成功 → 用自定义;否则回落默认 + 提示;
/// 未配置 → 旧默认行为。两槽自定义键相同时第二槽(完整窗口)回落默认。
struct InitialPlan {
    active: ActiveKeys,
    /// 唤起槽提示(悬浮条警告;正常 = None)。
    note: Option<HotkeyNote>,
    /// 完整窗口槽提示(仅设置页;正常 = None)。
    window_note: Option<HotkeyNote>,
}

fn register_initial<R: KeyRegistrar>(reg: &mut R, file: &SettingsFile) -> InitialPlan {
    let note;

    let palette = match file.palette_hotkey {
        Some(custom) => match register_checked(reg, custom) {
            Ok(()) => {
                note = None;
                Some(custom)
            }
            Err(err) => {
                log::warn!(
                    "保存的唤起快捷键 {} 不可用,回落默认键: {err}",
                    display_name(custom)
                );
                let fallback = register_palette_default(reg);
                note = Some(palette_fallback_note(custom, fallback.name));
                fallback.hotkey
            }
        },
        None => {
            let fallback = register_palette_default(reg);
            note = note_for(fallback.name);
            fallback.hotkey
        }
    };

    let (window, window_note) = match file.window_hotkey {
        Some(custom) if palette != Some(custom) => match register_checked(reg, custom) {
            Ok(()) => (Some(custom), None),
            Err(err) => {
                log::warn!(
                    "保存的完整窗口快捷键 {} 不可用,回落默认键: {err}",
                    display_name(custom)
                );
                register_window_default(reg, Some(custom), false)
            }
        },
        Some(custom) => {
            // 同一组合不能用于两槽:启动时第二槽直接回落,不依赖 OS 判重。
            log::warn!(
                "保存的完整窗口快捷键 {} 与唤起键冲突,回落默认键",
                display_name(custom)
            );
            register_window_default(reg, Some(custom), true)
        }
        None => register_window_default(reg, None, false),
    };

    InitialPlan {
        active: ActiveKeys { palette, window },
        note,
        window_note,
    }
}

fn lock_active(active: &Arc<Mutex<ActiveKeys>>) -> MutexGuard<'_, ActiveKeys> {
    match active.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// 取消录制后的恢复结果(供 App 如实日志;分槽提示以快照为准)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeReport {
    /// 本次是否真的从挂起态恢复(false = manager 不可用或本就没有挂起)。
    pub resumed: bool,
    /// 唤起槽恢复成功(原本为空的槽视为成功)。
    pub palette_ok: bool,
    /// 完整窗口槽恢复成功(原本为空的槽视为成功)。
    pub window_ok: bool,
}

impl ResumeReport {
    /// manager 不可用:未做任何恢复。
    const UNAVAILABLE: Self = Self {
        resumed: false,
        palette_ok: true,
        window_ok: true,
    };

    /// 挂起态已恢复且两槽都成功(空槽视为成功)。
    pub fn fully_restored(self) -> bool {
        self.resumed && self.palette_ok && self.window_ok
    }
}

/// 全局热键运行时(挂 `ui::App` 保活):
/// manager drop 即注销全部热键,须与进程同寿。
pub struct HotkeyRuntime {
    /// 注册状态的宿主;创建失败时为 None(热键不可用,其余功能照常)。
    manager: Option<GlobalHotKeyManager>,
    /// 当前注册键(事件泵每事件读取此份;重配置后立即生效)。
    active: Arc<Mutex<ActiveKeys>>,
    /// 当前快照(设置页种子;commit/suspend 后更新)。
    snapshot: HotkeySnapshot,
    /// 录制期挂起的旧键(Some = 已注销、等待 resume/commit)。
    suspended: Option<ActiveKeys>,
    /// 主队列直调目标(ui 层启动期回填;未回填时按下走兜底队列)。
    on_action: Arc<OnceLock<DispatchFn>>,
    /// 兜底队列(直调未竟的动作;`poll_action` 帧内消费)。
    deferred: Arc<Mutex<VecDeque<Action>>>,
}

impl HotkeyRuntime {
    /// 唤起槽命中键显示名(未注册 = None;日志用)。
    pub fn name(&self) -> Option<String> {
        (!self.snapshot.palette_keys.is_empty()).then(|| self.snapshot.palette_keys.concat())
    }

    /// 唤起槽常驻提示(悬浮条警告;默认键正常 = None)。
    ///
    /// 完整窗口槽的回落提示在 [`HotkeySnapshot::window_note`],不得进入悬浮条。
    pub fn note(&self) -> Option<&HotkeyNote> {
        self.snapshot.note.as_ref()
    }

    /// 当前热键快照(设置页展示/录制用)。
    pub fn snapshot(&self) -> HotkeySnapshot {
        self.snapshot.clone()
    }

    /// 回填主队列直调目标(ui 层在共享态构造完成后调用;进程内一次)。
    pub fn attach(&self, f: impl Fn(Action) -> bool + Send + Sync + 'static) {
        if self.on_action.set(Arc::new(f)).is_err() {
            log::warn!("热键直调目标重复回填(忽略后填)");
        }
    }

    /// 进入录制:注销两槽并记住旧键(主线程;幂等)。
    ///
    /// 快照保持不变——设置页录制期间仍显示旧键,另一槽的键帽不消失。
    /// 返回 false = 热键运行时不可用(manager 创建失败),调用方日志须如实说明。
    pub fn suspend(&mut self) -> bool {
        let Some(manager) = self.manager.as_ref() else {
            return false;
        };
        if self.suspended.is_some() {
            return true;
        }
        // 先让泵看不到任何键,再走系统注销:关闭注销期间旧事件仍被派发的窗口。
        let mut active = *lock_active(&self.active);
        *lock_active(&self.active) = ActiveKeys::default();
        suspend_with(
            &mut ManagerRegistrar(manager),
            &mut active,
            &mut self.suspended,
        );
        true
    }

    /// 取消录制:重新注册挂起的旧键(主线程;无挂起 = 无操作)。
    ///
    /// 恢复失败按槽留提示(唤起槽进悬浮条警告,完整窗口槽仅设置页展示)。
    /// 返回本次是否真的从挂起态恢复以及各槽结果,供 App 日志如实描述。
    pub fn resume(&mut self) -> ResumeReport {
        let Some(manager) = self.manager.as_ref() else {
            return ResumeReport::UNAVAILABLE;
        };
        let was_suspended = self.suspended.is_some();
        let mut active = *lock_active(&self.active);
        let outcome = resume_with(
            &mut ManagerRegistrar(manager),
            &mut active,
            &mut self.suspended,
        );
        *lock_active(&self.active) = active;
        let (note, window_note) = resume_notes(
            self.snapshot.note.clone(),
            self.snapshot.window_note.clone(),
            outcome,
        );
        self.snapshot = HotkeySnapshot::from_active(active, note, window_note);
        ResumeReport {
            resumed: was_suspended,
            palette_ok: outcome.palette,
            window_ok: outcome.window,
        }
    }

    /// 提交录制结果(主线程;设置页命令队列调用)。
    ///
    /// 校验失败/与另一槽重复/系统注册失败 → `Err`(当前语言文案,可直接内联展示),
    /// 旧键保持可用;成功 → 更新快照并清空启动回退提示。
    pub fn commit(&mut self, slot: HotkeySlot, hotkey: HotKey) -> Result<HotkeySnapshot, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| i18n::s().hotkey_global_unavailable.to_owned())?;
        let mut active = *lock_active(&self.active);
        let result = plan_commit(
            &mut ManagerRegistrar(manager),
            &mut active,
            &mut self.suspended,
            slot,
            hotkey,
        );
        *lock_active(&self.active) = active;
        match result {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                Ok(self.snapshot.clone())
            }
            Err(err) => {
                // 状态已按回滚语义落定(设置页失败后退回 Idle,旧键为 active 真源);
                // 保留各槽启动回退提示,错误单独经 Err 展示。
                self.snapshot = HotkeySnapshot::from_active(
                    active,
                    self.snapshot.note.clone(),
                    self.snapshot.window_note.clone(),
                );
                Err(err)
            }
        }
    }

    /// 恢复该槽内置默认(主线程;录制中 Delete/Backspace 触发)。
    ///
    /// 唤起槽按 `candidates()` 首个成功项(降级时快照 `note` 如实提示),完整窗口槽
    /// 用 `direct_key()`。挂起态失败与 commit 同语义:尽力恢复两个旧键并退出挂起;
    /// 非挂起态失败时旧键保持注册。成功清空该槽启动回退提示(另一槽提示保留)。
    pub fn reset_slot(&mut self, slot: HotkeySlot) -> Result<HotkeySnapshot, String> {
        let manager = self
            .manager
            .as_ref()
            .ok_or_else(|| i18n::s().hotkey_global_unavailable.to_owned())?;
        let mut active = *lock_active(&self.active);
        let result = plan_reset(
            &mut ManagerRegistrar(manager),
            &mut active,
            &mut self.suspended,
            slot,
            self.snapshot.note.clone(),
            self.snapshot.window_note.clone(),
        );
        *lock_active(&self.active) = active;
        match result {
            Ok(snapshot) => {
                self.snapshot = snapshot;
                Ok(self.snapshot.clone())
            }
            Err(err) => {
                // 失败已回滚旧键(挂起态同时退出挂起);保留原有提示,错误单独返回。
                self.snapshot = HotkeySnapshot::from_active(
                    active,
                    self.snapshot.note.clone(),
                    self.snapshot.window_note.clone(),
                );
                Err(err)
            }
        }
    }
}

/// 注销两槽并挂起(纯状态;测试可复用)。
fn suspend_with<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    suspended: &mut Option<ActiveKeys>,
) {
    if suspended.is_some() {
        return;
    }
    for slot in [HotkeySlot::Palette, HotkeySlot::Window] {
        if let Some(hotkey) = active.get(slot) {
            if let Err(err) = reg.unregister(hotkey) {
                // 注销失败时该键仍可能被系统触发;active 置空后事件泵会忽略它。
                log::warn!(
                    "录制前注销快捷键 {} 失败(泵将忽略该键): {err}",
                    display_name(hotkey)
                );
            }
        }
    }
    *suspended = Some(*active);
    *active = ActiveKeys::default();
}

/// 各槽恢复结果;槽位原本为空视为成功(无键可恢复)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResumeOutcome {
    palette: bool,
    window: bool,
}

impl ResumeOutcome {
    const ALL_RESTORED: Self = Self {
        palette: true,
        window: true,
    };
}

/// 恢复挂起的旧键;失败槽仍尽力恢复其余。
fn resume_with<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    suspended: &mut Option<ActiveKeys>,
) -> ResumeOutcome {
    let Some(old) = suspended.take() else {
        return ResumeOutcome::ALL_RESTORED;
    };
    let mut restored = ActiveKeys::default();
    let mut outcome = ResumeOutcome::ALL_RESTORED;
    for slot in [HotkeySlot::Palette, HotkeySlot::Window] {
        if let Some(hotkey) = old.get(slot) {
            match reg.register(hotkey) {
                Ok(()) => restored.set(slot, Some(hotkey)),
                Err(err) => {
                    log::warn!("恢复快捷键 {} 失败: {err}", display_name(hotkey));
                    match slot {
                        HotkeySlot::Palette => outcome.palette = false,
                        HotkeySlot::Window => outcome.window = false,
                    }
                }
            }
        }
    }
    *active = restored;
    outcome
}

/// resume 后的分槽提示:成功槽保留原启动提示,失败槽改用恢复失败提示。
fn resume_notes(
    note: Option<HotkeyNote>,
    window_note: Option<HotkeyNote>,
    outcome: ResumeOutcome,
) -> (Option<HotkeyNote>, Option<HotkeyNote>) {
    let palette_note = if outcome.palette {
        note
    } else {
        Some(HotkeyNote::ResumeFailed)
    };
    let window_note = if outcome.window {
        window_note
    } else {
        Some(HotkeyNote::ResumeFailed)
    };
    (palette_note, window_note)
}

/// 恢复两个旧键(已注册的 `already_registered` 不重复注册);返回尽力恢复后的状态。
fn restore_old_keys<R: KeyRegistrar>(
    reg: &mut R,
    old: ActiveKeys,
    already_registered: Option<HotKey>,
) -> ActiveKeys {
    let mut restored = ActiveKeys::default();
    for slot in [HotkeySlot::Palette, HotkeySlot::Window] {
        let Some(old_key) = old.get(slot) else {
            continue;
        };
        if already_registered == Some(old_key) || reg.register(old_key).is_ok() {
            restored.set(slot, Some(old_key));
        } else {
            log::warn!("恢复旧快捷键 {} 失败", display_name(old_key));
        }
    }
    restored
}

/// 提交规划器(纯逻辑;注册器可注入)。
///
/// 挂起态（录制中）的**任何**错误——校验、与另一槽重复、系统注册失败——都会尽力
/// 恢复两个旧键、清空 `suspended` 并以旧键为 active 返回 `Err`(设置页直接退出录制,
/// 不会额外 resume)。非挂起态错误保留旧键注册。
fn plan_commit<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    suspended: &mut Option<ActiveKeys>,
    slot: HotkeySlot,
    hotkey: HotKey,
) -> Result<HotkeySnapshot, String> {
    // 先摘下挂起态:连校验/查重失败也必须恢复旧键并退出录制。
    let old = suspended.take();

    if let Err(err) = hotkey_keys::validate(hotkey) {
        return Err(rollback_suspended(reg, active, old, err.to_string()));
    }

    // 查重以「逻辑上的旧键」为准:录制挂起时 active 为空,旧键在 old。
    let logical = old.unwrap_or(*active);
    if logical.other(slot) == Some(hotkey) {
        return Err(rollback_suspended(
            reg,
            active,
            old,
            i18n::s().hotkey_duplicate.to_string(),
        ));
    }

    match old {
        Some(old) => {
            let other = old.other(slot);
            let mut other_registered = false;
            let outcome = (|| {
                // 录制期两槽都已注销:先把另一槽旧键注册回来,再注册新键。
                if let Some(other_key) = other {
                    reg.register(other_key).map_err(|err| {
                        let name = display_name(other_key);
                        format!(
                            "{}: {err}",
                            i18n::fill(
                                i18n::s().hotkey_restore_other_failed,
                                &[("name", name.as_str())]
                            )
                        )
                    })?;
                    other_registered = true;
                }
                reg.register(hotkey)
                    .map_err(|err| format!("{}: {err}", i18n::s().hotkey_register_failed))
            })();

            if let Err(err) = outcome {
                // 失败退出录制:尽力恢复两个旧键(已注册的 other 不重复注册)。
                *active = restore_old_keys(reg, old, other.filter(|_| other_registered));
                *suspended = None;
                return Err(err);
            }

            let mut next = old;
            next.set(slot, Some(hotkey));
            *active = next;
            *suspended = None;
            Ok(HotkeySnapshot::from_active(next, None, None))
        }
        None => {
            // 非录制直提:同键提交 = 幂等成功(该键仍在系统中,重复注册会失败)。
            if active.get(slot) == Some(hotkey) {
                return Ok(HotkeySnapshot::from_active(*active, None, None));
            }
            reg.register(hotkey)
                .map_err(|err| format!("{}: {err}", i18n::s().hotkey_register_failed))?;
            let old_key = active.get(slot);
            // 先更新 active 再注销旧键:旧事件在注销后仍被泵看到也不会再触发。
            active.set(slot, Some(hotkey));
            if let Some(old_key) = old_key {
                if let Err(err) = reg.unregister(old_key) {
                    // 旧键已不在 active,事件泵不会触发;仅留痕(可能仍占系统组合)。
                    log::warn!("注销旧快捷键 {} 失败: {err}", display_name(old_key));
                }
            }
            Ok(HotkeySnapshot::from_active(*active, None, None))
        }
    }
}

/// 挂起态提交错误落点:尽力恢复旧键并退出挂起(错误文案原样返回)。
fn rollback_suspended<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    old: Option<ActiveKeys>,
    err: String,
) -> String {
    if let Some(old) = old {
        *active = restore_old_keys(reg, old, None);
    }
    err
}

fn reset_failed(err: impl std::fmt::Display) -> String {
    format!("{}: {err}", i18n::s().hotkey_reset_failed)
}

/// 恢复默认成功后的分槽提示:唤起槽按默认链实际落点(`note_for` 暴露降级),
/// 完整窗口槽清空默认回落提示;另一槽原有提示保留。
fn reset_notes(
    slot: HotkeySlot,
    default_name: &str,
    note: Option<HotkeyNote>,
    window_note: Option<HotkeyNote>,
) -> (Option<HotkeyNote>, Option<HotkeyNote>) {
    match slot {
        HotkeySlot::Palette => (note_for(Some(default_name)), window_note),
        HotkeySlot::Window => (note, None),
    }
}

/// 恢复默认规划器(纯逻辑;注册器可注入)。
///
/// 挂起态:先恢复另一槽旧键,再注册默认;任何失败都尽力恢复两个旧键并退出挂起。
/// 非挂起态:旧键保持注册,默认键注册成功后才注销该槽旧键;失败不动旧键。
/// 默认键已在该槽注册(非挂起)按幂等成功处理,不重复注册。
fn plan_reset<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    suspended: &mut Option<ActiveKeys>,
    slot: HotkeySlot,
    note: Option<HotkeyNote>,
    window_note: Option<HotkeyNote>,
) -> Result<HotkeySnapshot, String> {
    match suspended.take() {
        Some(old) => plan_reset_suspended(reg, active, old, slot, note, window_note),
        None => plan_reset_active(reg, active, slot, note, window_note),
    }
}

/// 非挂起态恢复默认:注册默认(旧键仍在)→ 成功注销旧键;失败旧键保持。
fn plan_reset_active<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    slot: HotkeySlot,
    note: Option<HotkeyNote>,
    window_note: Option<HotkeyNote>,
) -> Result<HotkeySnapshot, String> {
    let old_key = active.get(slot);
    let (default, default_name) = match slot {
        HotkeySlot::Palette => {
            let fallback = register_palette_default_with(reg, old_key);
            match (fallback.hotkey, fallback.name) {
                (Some(hotkey), Some(name)) => (hotkey, name),
                _ => return Err(reset_failed("候选快捷键均不可用")),
            }
        }
        HotkeySlot::Window => {
            let (mods, code, name) = direct_key();
            let hotkey = HotKey::new(Some(mods), code);
            if old_key != Some(hotkey) {
                reg.register(hotkey).map_err(reset_failed)?;
            }
            (hotkey, name)
        }
    };

    if old_key != Some(default) {
        if let Some(old) = old_key {
            if let Err(err) = reg.unregister(old) {
                // 旧键已不在 active,事件泵不会触发;仅留痕(可能仍占系统组合)。
                log::warn!("注销旧快捷键 {} 失败: {err}", display_name(old));
            }
        }
    }
    active.set(slot, Some(default));
    let (next_note, next_window_note) = reset_notes(slot, default_name, note, window_note);
    Ok(HotkeySnapshot::from_active(
        *active,
        next_note,
        next_window_note,
    ))
}

/// 挂起态恢复默认:两槽旧键均已注销;先恢复另一槽旧键,再注册默认。
/// 失败(另一槽恢复失败/默认注册失败)退出挂起并尽力恢复两个旧键。
fn plan_reset_suspended<R: KeyRegistrar>(
    reg: &mut R,
    active: &mut ActiveKeys,
    old: ActiveKeys,
    slot: HotkeySlot,
    note: Option<HotkeyNote>,
    window_note: Option<HotkeyNote>,
) -> Result<HotkeySnapshot, String> {
    let other = old.other(slot);
    if let Some(other_key) = other {
        if let Err(err) = reg.register(other_key) {
            // 另一槽旧键都恢复不了:退出挂起,尽力恢复两个旧键。
            *active = restore_old_keys(reg, old, None);
            let name = display_name(other_key);
            return Err(format!(
                "{}: {err}",
                i18n::fill(
                    i18n::s().hotkey_restore_other_failed,
                    &[("name", name.as_str())]
                )
            ));
        }
    }

    let (default, default_name) = match slot {
        HotkeySlot::Palette => {
            let fallback = register_palette_default(reg);
            match (fallback.hotkey, fallback.name) {
                (Some(hotkey), Some(name)) => (hotkey, name),
                _ => {
                    // 默认链全部失败:另一槽旧键已恢复,补回本槽旧键并退出挂起。
                    *active = restore_old_keys(reg, old, other);
                    return Err(reset_failed("候选快捷键均不可用"));
                }
            }
        }
        HotkeySlot::Window => {
            let (mods, code, name) = direct_key();
            let hotkey = HotKey::new(Some(mods), code);
            match reg.register(hotkey) {
                Ok(()) => (hotkey, name),
                Err(err) => {
                    log::warn!("恢复默认快捷键 {name} 失败: {err}");
                    *active = restore_old_keys(reg, old, other);
                    return Err(reset_failed(err));
                }
            }
        }
    };
    let mut next = old;
    next.set(slot, Some(default));
    *active = next;
    let (next_note, next_window_note) = reset_notes(slot, default_name, note, window_note);
    Ok(HotkeySnapshot::from_active(
        next,
        next_note,
        next_window_note,
    ))
}

/// 注册热键并起事件泵线程(主线程调用;失败不中断 App,降级为提示 + 日志)。
///
/// `file` 提供启动偏好:两槽 Some → 优先注册自定义键,失败回落默认并留提示;
/// None → 保持旧默认行为(候选降级链 / ⌥⇧F)。
pub fn install(ctx: &egui::Context, file: &SettingsFile) -> HotkeyRuntime {
    let on_action: Arc<OnceLock<DispatchFn>> = Arc::new(OnceLock::new());
    let deferred: Arc<Mutex<VecDeque<Action>>> = Arc::new(Mutex::new(VecDeque::new()));
    let active: Arc<Mutex<ActiveKeys>> = Arc::new(Mutex::new(ActiveKeys::default()));

    let manager = match GlobalHotKeyManager::new() {
        Ok(manager) => manager,
        Err(err) => {
            log::error!("全局热键管理器创建失败(热键不可用):{err}");
            return HotkeyRuntime {
                manager: None,
                active,
                snapshot: HotkeySnapshot::from_active(ActiveKeys::default(), note_for(None), None),
                suspended: None,
                on_action,
                deferred,
            };
        }
    };

    let plan = register_initial(&mut ManagerRegistrar(&manager), file);
    match plan.active.palette {
        Some(hotkey) => log::info!(
            "唤起快捷键注册成功:{} (id={})",
            display_name(hotkey),
            hotkey.id()
        ),
        None => log::error!("全局热键不可用({:?})", HotkeyNote::Unavailable),
    }
    match plan.active.window {
        Some(hotkey) => log::info!(
            "完整窗口快捷键注册成功:{} (id={})",
            display_name(hotkey),
            hotkey.id()
        ),
        None => log::warn!("完整窗口快捷键未注册(入口仍有托盘与悬浮条 ⌥↵)"),
    }
    if let Some(note) = &plan.note {
        log::warn!("唤起快捷键回退提示:{note:?}");
    }
    if let Some(note) = &plan.window_note {
        log::warn!("完整窗口快捷键回退提示:{note:?}");
    }

    *lock_active(&active) = plan.active;
    let snapshot = HotkeySnapshot::from_active(plan.active, plan.note, plan.window_note);

    // manager 存在就起泵:即使启动时两槽都未注册,设置页录制后仍可 commit 新键。
    spawn_event_pump(
        ctx.clone(),
        Arc::clone(&active),
        Arc::clone(&on_action),
        Arc::clone(&deferred),
    );

    HotkeyRuntime {
        manager: Some(manager),
        active,
        snapshot,
        suspended: None,
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

/// 事件泵线程:阻塞收官方 channel(零轮询);命中**当前**注册键的 Pressed → 主队列直调。
///
/// 每事件读取 `active`(重配置后立即生效);线程不接触 manager(注册/注销只在主线程)。
fn spawn_event_pump(
    ctx: egui::Context,
    active: Arc<Mutex<ActiveKeys>>,
    on_action: Arc<OnceLock<DispatchFn>>,
    deferred: Arc<Mutex<VecDeque<Action>>>,
) {
    let spawned = std::thread::Builder::new()
        .name("hotkey-pump".to_string())
        .spawn(move || {
            // 静态 channel(PoC-2 用法:receiver 全局唯一,本线程是唯一消费者)。
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(event) = rx.recv() {
                let action = {
                    let keys = *lock_active(&active);
                    action_for(&event, keys)
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

    fn hotkey(mods: Modifiers, code: Code) -> HotKey {
        HotKey::new(Some(mods), code)
    }

    fn alt_space() -> HotKey {
        hotkey(Modifiers::ALT, Code::Space)
    }

    fn alt_shift_f() -> HotKey {
        hotkey(Modifiers::ALT | Modifiers::SHIFT, Code::KeyF)
    }

    fn custom_palette() -> HotKey {
        hotkey(Modifiers::CONTROL | Modifiers::ALT, Code::KeyK)
    }

    fn custom_window() -> HotKey {
        hotkey(Modifiers::SUPER | Modifiers::SHIFT, Code::KeyJ)
    }

    /// 假注册器:记录已注册键,可配置注册/注销失败(不创建系统 manager)。
    #[derive(Default)]
    struct FakeRegistrar {
        registered: Vec<HotKey>,
        fail_register: Vec<HotKey>,
        fail_unregister: Vec<HotKey>,
    }

    impl FakeRegistrar {
        fn failing(keys: &[HotKey]) -> Self {
            Self {
                fail_register: keys.to_vec(),
                ..Self::default()
            }
        }
    }

    impl KeyRegistrar for FakeRegistrar {
        fn register(&mut self, hotkey: HotKey) -> Result<(), String> {
            if self.fail_register.contains(&hotkey) {
                return Err("被占用".to_string());
            }
            if self.registered.contains(&hotkey) {
                return Err("重复注册".to_string());
            }
            self.registered.push(hotkey);
            Ok(())
        }

        fn unregister(&mut self, hotkey: HotKey) -> Result<(), String> {
            if self.fail_unregister.contains(&hotkey) {
                return Err("注销失败".to_string());
            }
            self.registered.retain(|registered| *registered != hotkey);
            Ok(())
        }
    }

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

    /// 泵判定读当前 active:双槽各自命中;Released/双槽空 → 无动作。
    #[test]
    fn pump_action_follows_active_keys() {
        let palette = alt_space();
        let window = alt_shift_f();
        let keys = ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        };
        let press = |hotkey: HotKey| GlobalHotKeyEvent {
            id: hotkey.id(),
            state: HotKeyState::Pressed,
        };
        let release = |hotkey: HotKey| GlobalHotKeyEvent {
            id: hotkey.id(),
            state: HotKeyState::Released,
        };

        assert_eq!(
            action_for(&press(palette), keys),
            Some(Action::TogglePalette)
        );
        assert_eq!(action_for(&press(window), keys), Some(Action::OpenResults));
        assert_eq!(action_for(&release(palette), keys), None, "Released 忽略");
        assert_eq!(
            action_for(&press(palette), ActiveKeys::default()),
            None,
            "双槽未注册时泵不得动作"
        );
    }

    /// 启动:自定义键有效且注册成功 → 直接用自定义,无提示。
    #[test]
    fn startup_prefers_valid_custom_hotkeys() {
        let palette = custom_palette();
        let window = custom_window();
        let file = SettingsFile {
            palette_hotkey: Some(palette),
            window_hotkey: Some(window),
            ..SettingsFile::default()
        };
        let mut reg = FakeRegistrar::default();

        let plan = register_initial(&mut reg, &file);

        assert_eq!(
            plan.active,
            ActiveKeys {
                palette: Some(palette),
                window: Some(window)
            }
        );
        assert_eq!(plan.note, None, "自定义键正常不提示");
        assert_eq!(reg.registered, vec![palette, window]);
    }

    /// 启动:自定义键注册失败 → 回落默认键 + 提示(两槽各一条)。
    #[test]
    fn startup_custom_failure_falls_back_with_note() {
        let palette = custom_palette();
        let window = custom_window();
        let file = SettingsFile {
            palette_hotkey: Some(palette),
            window_hotkey: Some(window),
            ..SettingsFile::default()
        };
        let mut reg = FakeRegistrar::failing(&[palette, window]);

        let plan = register_initial(&mut reg, &file);

        let (pmods, pcode, _) = candidates()[0];
        let (wmods, wcode, _) = direct_key();
        assert_eq!(
            plan.active.palette,
            Some(HotKey::new(Some(pmods), pcode)),
            "唤起键回落默认链首项"
        );
        assert_eq!(
            plan.active.window,
            Some(HotKey::new(Some(wmods), wcode)),
            "完整窗口键回落 ⌥⇧F"
        );
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let note = plan.note.expect("唤起键失败须留提示").text();
            assert!(note.contains("保存的唤起快捷键"), "{note}");
            assert!(note.contains("已回落默认 ⌥Space"), "{note}");
            let window_note = plan.window_note.expect("完整窗口键失败须留提示").text();
            assert!(
                window_note.contains("保存的完整窗口快捷键"),
                "{window_note}"
            );
            assert!(window_note.contains("已回落默认 ⌥⇧F"), "{window_note}");
            assert!(
                !note.contains("完整窗口"),
                "窗口提示不得混入悬浮条警告: {note}"
            );
        });
    }

    /// 启动:文件里持久化的越界键(Escape)不注册,回落默认 + 提示(H1 校验联动)。
    #[test]
    fn startup_invalid_persisted_key_falls_back() {
        let invalid = hotkey(Modifiers::ALT, Code::Escape);
        let file = SettingsFile {
            palette_hotkey: Some(invalid),
            ..SettingsFile::default()
        };
        let mut reg = FakeRegistrar::default();

        let plan = register_initial(&mut reg, &file);

        let (mods, code, _) = candidates()[0];
        assert_eq!(plan.active.palette, Some(HotKey::new(Some(mods), code)));
        assert!(!reg.registered.contains(&invalid), "越界键不得注册");
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let note = plan.note.expect("越界键须提示").text();
            assert!(note.contains("保存的唤起快捷键"), "{note}");
        });
    }

    /// 启动:无自定义键 → 旧默认行为(⌥Space + ⌥⇧F),无提示。
    #[test]
    fn startup_without_preferences_uses_defaults() {
        let mut reg = FakeRegistrar::default();

        let plan = register_initial(&mut reg, &SettingsFile::default());

        let (pmods, pcode, _) = candidates()[0];
        let (wmods, wcode, _) = direct_key();
        assert_eq!(
            plan.active,
            ActiveKeys {
                palette: Some(HotKey::new(Some(pmods), pcode)),
                window: Some(HotKey::new(Some(wmods), wcode)),
            }
        );
        assert_eq!(plan.note, None, "默认键正常不打扰");
        assert_eq!(plan.window_note, None);
    }

    /// 启动:两槽配置同一组合 → 完整窗口槽回落默认 + 冲突提示(仅设置页)。
    #[test]
    fn startup_duplicate_custom_slots_fall_back_second() {
        let shared = custom_palette();
        let file = SettingsFile {
            palette_hotkey: Some(shared),
            window_hotkey: Some(shared),
            ..SettingsFile::default()
        };
        let mut reg = FakeRegistrar::default();

        let plan = register_initial(&mut reg, &file);

        let (wmods, wcode, _) = direct_key();
        assert_eq!(plan.active.palette, Some(shared));
        assert_eq!(
            plan.active.window,
            Some(HotKey::new(Some(wmods), wcode)),
            "第二槽回落默认"
        );
        assert_eq!(plan.note, None, "唤起槽正常无悬浮条提示");
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let window_note = plan.window_note.expect("冲突须提示").text();
            assert!(
                window_note.contains("保存的完整窗口快捷键"),
                "{window_note}"
            );
            assert!(window_note.contains("与唤起键冲突"), "{window_note}");
        });
    }

    /// 快照:注册键转 macOS 键帽;未注册槽为空 vec;提示按槽保留。
    #[test]
    fn snapshot_displays_keys_and_empty_slots() {
        let palette = alt_space();
        let snapshot = HotkeySnapshot::from_active(
            ActiveKeys {
                palette: Some(palette),
                window: None,
            },
            Some(HotkeyNote::Unavailable),
            Some(HotkeyNote::ResumeFailed),
        );

        assert_eq!(snapshot.palette, Some(palette));
        assert_eq!(snapshot.window, None);
        assert_eq!(snapshot.palette_keys, ["⌥", "Space"]);
        assert!(snapshot.window_keys.is_empty());
        assert_eq!(snapshot.note, Some(HotkeyNote::Unavailable));
        assert_eq!(snapshot.window_note, Some(HotkeyNote::ResumeFailed));
    }

    /// 提交:只替换目标槽,另一槽保持注册;成功后清空启动回退提示。
    #[test]
    fn commit_preserves_other_slot_and_clears_note() {
        let old_palette = alt_space();
        let window = alt_shift_f();
        let new_palette = custom_palette();
        let mut active = ActiveKeys {
            palette: Some(old_palette),
            window: Some(window),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![old_palette, window],
            ..FakeRegistrar::default()
        };

        let snapshot = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            new_palette,
        )
        .expect("合法新键应提交成功");

        assert_eq!(active.palette, Some(new_palette));
        assert_eq!(active.window, Some(window), "另一槽保持");
        assert!(reg.registered.contains(&new_palette));
        assert!(!reg.registered.contains(&old_palette), "旧键须注销");
        assert!(reg.registered.contains(&window));
        assert!(suspended.is_none());
        assert_eq!(snapshot.palette, Some(new_palette));
        assert_eq!(snapshot.window, Some(window));
        assert_eq!(snapshot.palette_keys, ["⌃", "⌥", "K"]);
        assert_eq!(snapshot.window_keys, ["⌥", "⇧", "F"]);
        assert_eq!(snapshot.note, None, "成功后清空启动回退提示");
    }

    /// 提交:与另一槽相同 → 本地拒绝,注册态不变。
    #[test]
    fn commit_rejects_duplicate_with_other_slot() {
        let palette = alt_space();
        let window = alt_shift_f();
        let mut active = ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette, window],
            ..FakeRegistrar::default()
        };

        let err = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Window,
            palette,
        )
        .unwrap_err();
        assert!(err.contains("另一个功能"), "{err}");
        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(palette),
                window: Some(window)
            }
        );

        // 挂起态同样以旧键查重;拒绝时恢复旧键并退出挂起(设置页直接结束录制)。
        let mut reg = FakeRegistrar::default(); // 挂起 = 两键均已注销
        let mut active = ActiveKeys::default();
        let mut suspended = Some(ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        });
        let err = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Window,
            palette,
        )
        .unwrap_err();
        assert!(err.contains("另一个功能"), "{err}");
        assert!(suspended.is_none(), "失败须退出挂起");
        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(palette),
                window: Some(window)
            },
            "查重失败也须恢复旧键"
        );
        assert!(reg.registered.contains(&palette));
        assert!(reg.registered.contains(&window));
    }

    /// 提交(挂起态)校验失败(越界键):同样恢复旧键并退出挂起,不丢热键。
    #[test]
    fn commit_while_suspended_validation_error_restores_old_keys() {
        let palette = alt_space();
        let window = alt_shift_f();
        let invalid = hotkey(Modifiers::ALT, Code::Escape);
        let mut active = ActiveKeys::default();
        let mut suspended = Some(ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        });
        let mut reg = FakeRegistrar::default();

        let err = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            invalid,
        )
        .unwrap_err();

        assert!(err.contains("不支持该键"), "{err}");
        assert!(suspended.is_none(), "校验失败须退出挂起");
        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(palette),
                window: Some(window)
            },
            "校验失败也须恢复旧键"
        );
        assert!(reg.registered.contains(&palette));
        assert!(reg.registered.contains(&window));
    }

    /// 提交(非挂起)`同键` = 幂等成功,不得因重复注册失败而丢键。
    #[test]
    fn commit_same_key_without_suspend_is_noop() {
        let palette = alt_space();
        let window = alt_shift_f();
        let mut active = ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette, window],
            ..FakeRegistrar::default()
        };

        let snapshot = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            palette,
        )
        .expect("同键提交应幂等成功");

        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(palette),
                window: Some(window)
            }
        );
        assert_eq!(reg.registered, vec![palette, window]);
        assert_eq!(snapshot.palette, Some(palette));
    }

    /// 提交:非挂起态注册失败 → 旧键保持注册/active 不变。
    #[test]
    fn commit_failure_keeps_old_keys_active() {
        let old_palette = alt_space();
        let new_palette = custom_palette();
        let mut active = ActiveKeys {
            palette: Some(old_palette),
            window: None,
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![old_palette],
            fail_register: vec![new_palette],
            ..FakeRegistrar::default()
        };

        let err = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            new_palette,
        )
        .unwrap_err();

        assert!(err.contains("注册失败"), "{err}");
        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(old_palette),
                window: None
            },
            "失败不得动旧键"
        );
        assert_eq!(reg.registered, vec![old_palette]);
        assert!(suspended.is_none());
    }

    /// 提交(挂起态):重新注册另一槽旧键 + 新键;成功后退出挂起。
    #[test]
    fn commit_while_suspended_reregisters_other_and_new() {
        let old_palette = alt_space();
        let window = alt_shift_f();
        let new_palette = custom_palette();
        let mut active = ActiveKeys::default();
        let mut suspended = Some(ActiveKeys {
            palette: Some(old_palette),
            window: Some(window),
        });
        let mut reg = FakeRegistrar::default();

        let snapshot = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            new_palette,
        )
        .expect("挂起态提交应成功");

        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(new_palette),
                window: Some(window)
            }
        );
        assert!(suspended.is_none(), "成功后退出录制挂起");
        assert!(reg.registered.contains(&window));
        assert!(reg.registered.contains(&new_palette));
        assert!(!reg.registered.contains(&old_palette));
        assert_eq!(snapshot.palette, Some(new_palette));
    }

    /// 提交(挂起态)失败:退出挂起并尽力恢复两个旧键(设置页期望旧键仍可用)。
    #[test]
    fn commit_while_suspended_failure_restores_old_keys() {
        let old_palette = alt_space();
        let window = alt_shift_f();
        let new_palette = custom_palette();
        let mut active = ActiveKeys::default();
        let mut suspended = Some(ActiveKeys {
            palette: Some(old_palette),
            window: Some(window),
        });
        let mut reg = FakeRegistrar::failing(&[new_palette]);

        let err = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            new_palette,
        )
        .unwrap_err();

        assert!(err.contains("注册失败"), "{err}");
        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(old_palette),
                window: Some(window)
            },
            "失败须恢复旧键"
        );
        assert!(suspended.is_none(), "失败后退出挂起");
        assert!(reg.registered.contains(&old_palette));
        assert!(reg.registered.contains(&window));
        assert!(!reg.registered.contains(&new_palette), "新键须回滚");
    }

    /// 录制挂起/取消:注销全部键、幂等;resume 恢复旧键。
    #[test]
    fn suspend_and_resume_roundtrip_old_keys() {
        let palette = alt_space();
        let window = alt_shift_f();
        let old = ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        };
        let mut active = old;
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette, window],
            ..FakeRegistrar::default()
        };

        suspend_with(&mut reg, &mut active, &mut suspended);
        assert_eq!(active, ActiveKeys::default(), "录制期注销全部热键");
        assert!(reg.registered.is_empty());
        assert_eq!(suspended, Some(old));

        suspend_with(&mut reg, &mut active, &mut suspended);
        assert_eq!(suspended, Some(old), "重复 suspend 幂等");
        assert!(reg.registered.is_empty());

        assert_eq!(
            resume_with(&mut reg, &mut active, &mut suspended),
            ResumeOutcome::ALL_RESTORED
        );
        assert_eq!(active, old, "取消录制恢复旧键");
        assert!(suspended.is_none());
        assert!(reg.registered.contains(&palette));
        assert!(reg.registered.contains(&window));
    }

    /// 挂起/恢复兼容单槽与双空槽:无非空键可恢复时视为成功。
    #[test]
    fn suspend_and_resume_handles_missing_slots() {
        let palette = alt_space();
        let old = ActiveKeys {
            palette: Some(palette),
            window: None,
        };
        let mut active = old;
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette],
            ..FakeRegistrar::default()
        };

        suspend_with(&mut reg, &mut active, &mut suspended);
        assert_eq!(suspended, Some(old));
        assert_eq!(active, ActiveKeys::default());
        assert_eq!(
            resume_with(&mut reg, &mut active, &mut suspended),
            ResumeOutcome::ALL_RESTORED,
            "空槽不算恢复失败"
        );
        assert_eq!(active, old);

        // 双槽皆空:仍保持幂等挂起/恢复语义。
        let mut active = ActiveKeys::default();
        let mut suspended = None;
        suspend_with(&mut reg, &mut active, &mut suspended);
        assert_eq!(suspended, Some(ActiveKeys::default()));
        assert_eq!(active, ActiveKeys::default());
        assert_eq!(
            resume_with(&mut reg, &mut active, &mut suspended),
            ResumeOutcome::ALL_RESTORED
        );
        assert_eq!(active, ActiveKeys::default());
    }

    /// 录制前注销失败:仍进入挂起(active 置空),旧键残留但泵不再触发。
    #[test]
    fn suspend_unregister_failure_still_suspends() {
        let palette = alt_space();
        let old = ActiveKeys {
            palette: Some(palette),
            window: None,
        };
        let mut active = old;
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette],
            fail_unregister: vec![palette],
            ..FakeRegistrar::default()
        };

        suspend_with(&mut reg, &mut active, &mut suspended);

        assert_eq!(active, ActiveKeys::default(), "泵可见键必须置空");
        assert_eq!(suspended, Some(old));
        assert!(
            reg.registered.contains(&palette),
            "注销失败仅留痕(best effort),状态仍一致"
        );
    }

    /// 提交成功后注销旧键失败:新键仍生效,旧键残留但不在 active(泵不触发)。
    #[test]
    fn commit_unregister_failure_keeps_new_key_active() {
        let old_palette = alt_space();
        let window = alt_shift_f();
        let new_palette = custom_palette();
        let mut active = ActiveKeys {
            palette: Some(old_palette),
            window: Some(window),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![old_palette, window],
            fail_unregister: vec![old_palette],
            ..FakeRegistrar::default()
        };

        let snapshot = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            new_palette,
        )
        .expect("注销失败不阻断提交");

        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(new_palette),
                window: Some(window)
            }
        );
        assert!(reg.registered.contains(&new_palette));
        assert!(
            reg.registered.contains(&old_palette),
            "旧键残留(best effort)"
        );
        assert_eq!(snapshot.palette, Some(new_palette));
    }

    /// 挂起态提交:另一槽旧键恢复失败 → 退出挂起并尽力恢复(部分成功)。
    #[test]
    fn commit_while_suspended_other_failure_rolls_back_best_effort() {
        let palette = alt_space();
        let window = alt_shift_f();
        let new_window = custom_window();
        let old = ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        };
        let mut active = ActiveKeys::default();
        let mut suspended = Some(old);
        // 提交 Window 槽 → 先恢复 other(palette)失败。
        let mut reg = FakeRegistrar::failing(&[palette]);

        let err = plan_commit(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Window,
            new_window,
        )
        .unwrap_err();

        let name = display_name(palette);
        let expected = format!(
            "{}: 被占用",
            i18n::fill(
                i18n::s().hotkey_restore_other_failed,
                &[("name", name.as_str())]
            )
        );
        assert_eq!(err, expected);
        assert!(suspended.is_none(), "失败须退出挂起");
        assert_eq!(
            active,
            ActiveKeys {
                palette: None,
                window: Some(window)
            },
            "恢复失败的槽缺省,其余旧键尽力恢复"
        );
        assert!(!reg.registered.contains(&new_window), "新键不得残留");
        assert!(reg.registered.contains(&window));

        // 同一路径的英文文案（含 {name} 占位符替换）。
        i18n::with_lang(i18n::Lang::En, || {
            let mut active = ActiveKeys::default();
            let mut suspended = Some(old);
            let mut reg = FakeRegistrar::failing(&[palette]);
            let err = plan_commit(
                &mut reg,
                &mut active,
                &mut suspended,
                HotkeySlot::Window,
                new_window,
            )
            .unwrap_err();
            assert_eq!(
                err,
                format!("Failed to restore the other shortcut {name} (it may be in use): 被占用")
            );
            assert!(suspended.is_none(), "失败须退出挂起");
        });
    }

    /// resume 失败按槽留提示:成功槽保留原提示,失败槽用 `HotkeyNote::ResumeFailed`。
    #[test]
    fn resume_failure_marks_slot_note() {
        let palette = alt_space();
        let window = alt_shift_f();
        let old = ActiveKeys {
            palette: Some(palette),
            window: Some(window),
        };
        let mut active = old;
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette, window],
            fail_register: vec![window],
            ..FakeRegistrar::default()
        };

        suspend_with(&mut reg, &mut active, &mut suspended);
        let outcome = resume_with(&mut reg, &mut active, &mut suspended);

        assert_eq!(
            outcome,
            ResumeOutcome {
                palette: true,
                window: false
            }
        );
        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(palette),
                window: None
            }
        );
        assert!(suspended.is_none());

        let palette_note = HotkeyNote::Unavailable;
        let old_window_note = HotkeyNote::Fallback {
            default: "⌥Space".into(),
            used: "⌃⌥K".into(),
        };
        let (note, window_note) = resume_notes(
            Some(palette_note.clone()),
            Some(old_window_note.clone()),
            outcome,
        );
        assert_eq!(note, Some(palette_note.clone()), "成功槽保留原提示");
        assert_eq!(
            window_note,
            Some(HotkeyNote::ResumeFailed),
            "失败槽提示恢复失败"
        );

        let (note, window_note) = resume_notes(
            Some(palette_note.clone()),
            Some(old_window_note.clone()),
            ResumeOutcome::ALL_RESTORED,
        );
        assert_eq!(note, Some(palette_note));
        assert_eq!(window_note, Some(old_window_note));
    }

    /// 启动:唤起槽默认降级链全失败 → 唤起提示不可用;无自定义窗口键保持旧行为。
    #[test]
    fn startup_palette_default_total_failure_is_unavailable() {
        let mut failing = Vec::new();
        for (mods, code, _) in candidates() {
            failing.push(HotKey::new(Some(mods), code));
        }
        let (wmods, wcode, _) = direct_key();
        failing.push(HotKey::new(Some(wmods), wcode));
        let mut reg = FakeRegistrar::failing(&failing);

        let plan = register_initial(&mut reg, &SettingsFile::default());

        assert_eq!(plan.active, ActiveKeys::default());
        assert_eq!(plan.note, Some(HotkeyNote::Unavailable));
        assert_eq!(
            plan.window_note, None,
            "无自定义窗口键时保持旧行为(只留日志,不加提示)"
        );
    }

    /// 启动:窗口自定义键与默认直键都注册失败 → 仅窗口槽提示,不污染悬浮条。
    #[test]
    fn startup_window_custom_fallback_failure_is_window_only_note() {
        let custom = custom_window();
        let (wmods, wcode, _) = direct_key();
        let direct = HotKey::new(Some(wmods), wcode);
        let file = SettingsFile {
            window_hotkey: Some(custom),
            ..SettingsFile::default()
        };
        let mut reg = FakeRegistrar::failing(&[custom, direct]);

        let plan = register_initial(&mut reg, &file);

        assert!(plan.active.window.is_none());
        assert_eq!(plan.note, None, "唤起槽默认正常,悬浮条不得出现提示");
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let note = plan.window_note.expect("窗口槽须留提示").text();
            assert!(note.contains("保存的完整窗口快捷键"), "{note}");
        });
    }

    /// 无 manager:挂起/恢复如实报告不可用,不得让调用方误记「已挂起/已恢复」。
    #[test]
    fn runtime_without_manager_reports_unavailable_suspend_and_resume() {
        let mut runtime = HotkeyRuntime {
            manager: None,
            active: Arc::new(Mutex::new(ActiveKeys::default())),
            snapshot: HotkeySnapshot::default(),
            suspended: None,
            on_action: Arc::new(OnceLock::new()),
            deferred: Arc::new(Mutex::new(VecDeque::new())),
        };
        assert!(!runtime.suspend(), "无 manager 不得谎报已挂起");
        let report = runtime.resume();
        assert!(!report.resumed, "无 manager 不得谎报已恢复");
        assert!(!report.fully_restored());
        assert!(
            !runtime.resume().fully_restored(),
            "非挂起态重复恢复也不得视为完整恢复"
        );
        for (lang, expected) in [
            (i18n::Lang::ZhHans, "全局热键不可用"),
            (i18n::Lang::En, "Global shortcuts unavailable"),
        ] {
            i18n::with_lang(lang, || {
                assert_eq!(
                    runtime.reset_slot(HotkeySlot::Palette).unwrap_err(),
                    expected,
                    "无 manager 时恢复默认必须如实报错"
                );
                assert_eq!(
                    runtime
                        .commit(HotkeySlot::Palette, alt_space())
                        .unwrap_err(),
                    expected,
                    "无 manager 时提交必须如实报错"
                );
            });
        }
    }

    /// 兜底队列:空 = None;入列动作按先进先出逐条消费,取尽不再重复。
    #[test]
    fn poll_action_drains_deferred_fifo() {
        let runtime = HotkeyRuntime {
            manager: None,
            active: Arc::new(Mutex::new(ActiveKeys::default())),
            snapshot: HotkeySnapshot::default(),
            suspended: None,
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

    /// 持久提示在绘制时按当前语言渲染；全变体逐条钉住中英文案与占位符替换。
    #[test]
    fn hotkey_note_text_is_localized() {
        let cases = [
            (
                HotkeyNote::Fallback {
                    default: "⌥Space".into(),
                    used: "⌃⌥K".into(),
                },
                "⌥Space 不可用,已改用 ⌃⌥K",
                "⌥Space unavailable; using ⌃⌥K instead",
            ),
            (
                HotkeyNote::Unavailable,
                "快捷键不可用,可从托盘唤起",
                "Shortcut unavailable; use the menu bar instead",
            ),
            (
                HotkeyNote::ResumeFailed,
                "快捷键恢复失败,请重新录制",
                "Failed to restore shortcut; please record it again",
            ),
            (
                HotkeyNote::SavedPaletteDefault {
                    custom: "⌥D".into(),
                    default: "⌥Space".into(),
                },
                "保存的唤起快捷键 ⌥D 不可用,已回落默认 ⌥Space",
                "Saved Quick Search shortcut ⌥D is unavailable; reverted to default ⌥Space",
            ),
            (
                HotkeyNote::SavedPaletteOther {
                    custom: "⌥D".into(),
                    used: "⌃⌥K".into(),
                },
                "保存的唤起快捷键 ⌥D 不可用,已改用 ⌃⌥K",
                "Saved Quick Search shortcut ⌥D is unavailable; using ⌃⌥K instead",
            ),
            (
                HotkeyNote::SavedPaletteUnavailable {
                    custom: "⌥D".into(),
                },
                "保存的唤起快捷键 ⌥D 不可用,可从托盘唤起",
                "Saved Quick Search shortcut ⌥D is unavailable; use the menu bar instead",
            ),
            (
                HotkeyNote::SavedWindowConflictDefault {
                    custom: "⌥D".into(),
                    default: "⌥Space".into(),
                },
                "保存的完整窗口快捷键 ⌥D 与唤起键冲突,已回落默认 ⌥Space",
                "Full Window shortcut ⌥D conflicts with Quick Search; reverted to default ⌥Space",
            ),
            (
                HotkeyNote::SavedWindowDefault {
                    custom: "⌥D".into(),
                    default: "⌥Space".into(),
                },
                "保存的完整窗口快捷键 ⌥D 不可用,已回落默认 ⌥Space",
                "Full Window shortcut ⌥D is unavailable; reverted to default ⌥Space",
            ),
            (
                HotkeyNote::SavedWindowConflictUnavailable {
                    custom: "⌥D".into(),
                },
                "保存的完整窗口快捷键 ⌥D 与唤起键冲突,可从托盘打开",
                "Full Window shortcut ⌥D conflicts with Quick Search; open it from the menu bar",
            ),
            (
                HotkeyNote::SavedWindowUnavailable {
                    custom: "⌥D".into(),
                },
                "保存的完整窗口快捷键 ⌥D 不可用,可从托盘打开",
                "Full Window shortcut ⌥D is unavailable; open it from the menu bar",
            ),
        ];
        assert_eq!(cases.len(), 10, "须覆盖 HotkeyNote 全部变体");
        for (note, zh, en) in &cases {
            let zh_text = i18n::with_lang(i18n::Lang::ZhHans, || note.text());
            assert_eq!(&zh_text, zh);
            assert!(!zh_text.contains('{'), "未替换占位符: {zh_text}");
            let en_text = i18n::with_lang(i18n::Lang::En, || note.text());
            assert_eq!(&en_text, en);
            assert!(!en_text.contains('{'), "未替换占位符: {en_text}");
        }
    }

    /// 提示文案:默认键正常不打扰;降级提示改用键;全失败提示托盘兜底。
    #[test]
    fn note_maps_registration_outcome() {
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let text = |note: Option<HotkeyNote>| note.as_ref().map(HotkeyNote::text);
            assert_eq!(note_for(Some("⌥Space")), None, "默认键正常 = 无提示");
            assert_eq!(
                text(note_for(Some("⌥⇧Space"))).as_deref(),
                Some("⌥Space 不可用,已改用 ⌥⇧Space")
            );
            assert_eq!(
                text(note_for(Some("⌃⌥⇧Space"))).as_deref(),
                Some("⌥Space 不可用,已改用 ⌃⌥⇧Space")
            );
            assert_eq!(note_for(None), Some(HotkeyNote::Unavailable));
        });
    }

    fn palette_default() -> HotKey {
        let (mods, code, _) = candidates()[0];
        HotKey::new(Some(mods), code)
    }

    fn window_default() -> HotKey {
        let (mods, code, _) = direct_key();
        HotKey::new(Some(mods), code)
    }

    /// 非挂起 reset(唤起槽):注册默认、注销旧键、另一槽不动,提示清空。
    #[test]
    fn reset_active_palette_registers_default_and_unregisters_old() {
        let old = custom_palette();
        let window = custom_window();
        let mut active = ActiveKeys {
            palette: Some(old),
            window: Some(window),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![old, window],
            ..FakeRegistrar::default()
        };

        let snapshot = plan_reset(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            Some(HotkeyNote::Unavailable),
            Some(HotkeyNote::ResumeFailed),
        )
        .expect("默认键应注册成功");

        assert_eq!(
            active,
            ActiveKeys {
                palette: Some(palette_default()),
                window: Some(window),
            }
        );
        assert!(!reg.registered.contains(&old), "旧唤起键须注销");
        assert!(reg.registered.contains(&palette_default()));
        assert!(reg.registered.contains(&window), "另一槽保持注册");
        assert!(suspended.is_none());
        assert_eq!(snapshot.palette, Some(palette_default()));
        assert_eq!(snapshot.window, Some(window));
        assert_eq!(snapshot.note, None, "默认键正常须清空唤起槽提示");
        assert_eq!(
            snapshot.window_note,
            Some(HotkeyNote::ResumeFailed),
            "另一槽提示保留"
        );
    }

    /// 非挂起 reset(完整窗口槽):注册 ⌥⇧F、注销旧键,清窗口提示。
    #[test]
    fn reset_active_window_registers_direct_and_unregisters_old() {
        let palette = custom_palette();
        let old = custom_window();
        let mut active = ActiveKeys {
            palette: Some(palette),
            window: Some(old),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette, old],
            ..FakeRegistrar::default()
        };

        let snapshot = plan_reset(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Window,
            Some(HotkeyNote::ResumeFailed),
            Some(HotkeyNote::Unavailable),
        )
        .expect("⌥⇧F 应注册成功");

        assert_eq!(active.window, Some(window_default()));
        assert_eq!(active.palette, Some(palette), "另一槽不动");
        assert!(!reg.registered.contains(&old));
        assert!(reg.registered.contains(&window_default()));
        assert_eq!(snapshot.window, Some(window_default()));
        assert_eq!(
            snapshot.note,
            Some(HotkeyNote::ResumeFailed),
            "唤起槽提示保留"
        );
        assert_eq!(snapshot.window_note, None, "窗口槽提示清空");
    }

    /// 非挂起 reset 失败:默认键注册失败 → 旧键保持注册/active 不变并返回错误。
    #[test]
    fn reset_active_failure_keeps_old_key() {
        let old_palette = custom_palette();
        let old_window = custom_window();
        let mut active = ActiveKeys {
            palette: Some(old_palette),
            window: Some(old_window),
        };
        let mut suspended = None;
        let mut failures: Vec<HotKey> = Vec::new();
        for (mods, code, _) in candidates() {
            failures.push(HotKey::new(Some(mods), code));
        }
        let mut reg = FakeRegistrar {
            registered: vec![old_palette, old_window],
            fail_register: failures,
            ..FakeRegistrar::default()
        };

        let err = plan_reset(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            None,
            None,
        )
        .unwrap_err();

        assert!(err.contains("恢复默认快捷键失败"), "{err}");
        assert_eq!(active.palette, Some(old_palette), "失败不得动旧键");
        assert_eq!(active.window, Some(old_window));
        assert_eq!(
            reg.registered,
            vec![old_palette, old_window],
            "默认链全失败时旧键仍注册"
        );
        assert!(suspended.is_none());

        // 完整窗口槽同理:⌥⇧F 注册失败 → 旧窗口键保持。
        let direct = window_default();
        let mut active = ActiveKeys {
            palette: Some(old_palette),
            window: Some(old_window),
        };
        let mut reg = FakeRegistrar {
            registered: vec![old_palette, old_window],
            fail_register: vec![direct],
            ..FakeRegistrar::default()
        };
        let err = plan_reset(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Window,
            None,
            None,
        )
        .unwrap_err();
        assert!(err.contains("恢复默认快捷键失败"), "{err}");
        assert_eq!(active.window, Some(old_window));
        assert!(reg.registered.contains(&old_window));
        assert!(!reg.registered.contains(&direct));
    }

    /// 默认链降级:⌥Space 被占 → 用 ⌥⇧Space,快照 note 如实提示。
    #[test]
    fn reset_palette_degraded_default_exposes_note() {
        let old = custom_palette();
        let (mods, code, name) = candidates()[1];
        let fallback = HotKey::new(Some(mods), code);
        let mut active = ActiveKeys {
            palette: Some(old),
            window: None,
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![old],
            fail_register: vec![palette_default()],
            ..FakeRegistrar::default()
        };

        let snapshot = plan_reset(
            &mut reg,
            &mut active,
            &mut suspended,
            HotkeySlot::Palette,
            None,
            None,
        )
        .expect("降级候选应注册成功");

        assert_eq!(active.palette, Some(fallback));
        assert!(reg.registered.contains(&fallback));
        assert!(!reg.registered.contains(&old));
        i18n::with_lang(i18n::Lang::ZhHans, || {
            let note = snapshot.note.expect("降级须提示").text();
            assert!(note.contains(name), "{note}");
            assert!(note.contains("不可用,已改用"), "{note}");
        });
    }

    /// 已在该槽生效的默认键:reset 幂等成功,不重复注册(系统会判重复)。
    #[test]
    fn reset_when_default_already_active_is_idempotent() {
        let mut active = ActiveKeys {
            palette: Some(palette_default()),
            window: Some(window_default()),
        };
        let mut suspended = None;
        let mut reg = FakeRegistrar {
            registered: vec![palette_default(), window_default()],
            ..FakeRegistrar::default()
        };

        for slot in [HotkeySlot::Palette, HotkeySlot::Window] {
            let snapshot = plan_reset(&mut reg, &mut active, &mut suspended, slot, None, None)
                .expect("默认键已生效应幂等成功");
            assert_eq!(snapshot.palette, Some(palette_default()));
            assert_eq!(snapshot.window, Some(window_default()));
        }
        assert_eq!(
            reg.registered,
            vec![palette_default(), window_default()],
            "不得重复注册"
        );
    }

    /// 挂起态 reset(两槽分别):恢复另一槽旧键 + 注册默认,退出挂起。
    #[test]
    fn reset_while_suspended_reregisters_other_and_default() {
        let old_palette = custom_palette();
        let old_window = custom_window();
        let old = ActiveKeys {
            palette: Some(old_palette),
            window: Some(old_window),
        };

        for slot in [HotkeySlot::Palette, HotkeySlot::Window] {
            let mut active = ActiveKeys::default();
            let mut suspended = Some(old);
            let mut reg = FakeRegistrar::default();

            let snapshot = plan_reset(&mut reg, &mut active, &mut suspended, slot, None, None)
                .expect("挂起态恢复默认应成功");

            assert!(suspended.is_none(), "成功须退出挂起");
            match slot {
                HotkeySlot::Palette => {
                    assert_eq!(
                        active,
                        ActiveKeys {
                            palette: Some(palette_default()),
                            window: Some(old_window),
                        }
                    );
                    assert!(reg.registered.contains(&palette_default()));
                    assert!(reg.registered.contains(&old_window));
                    assert_eq!(snapshot.note, None);
                }
                HotkeySlot::Window => {
                    assert_eq!(
                        active,
                        ActiveKeys {
                            palette: Some(old_palette),
                            window: Some(window_default()),
                        }
                    );
                    assert!(reg.registered.contains(&old_palette));
                    assert!(reg.registered.contains(&window_default()));
                    assert_eq!(snapshot.window_note, None);
                }
            }
            assert_eq!(reg.registered.len(), 2, "恰好两槽各一键");
        }
    }

    /// 挂起态 reset 失败(两槽分别):退出挂起并尽力恢复两个旧键。
    #[test]
    fn reset_while_suspended_failure_restores_both_old_keys() {
        let old_palette = custom_palette();
        let old_window = custom_window();
        let old = ActiveKeys {
            palette: Some(old_palette),
            window: Some(old_window),
        };
        let mut palette_failures: Vec<HotKey> = Vec::new();
        for (mods, code, _) in candidates() {
            palette_failures.push(HotKey::new(Some(mods), code));
        }
        let cases = [
            (HotkeySlot::Palette, palette_failures),
            (HotkeySlot::Window, vec![window_default()]),
        ];

        for (slot, failures) in cases {
            let mut active = ActiveKeys::default();
            let mut suspended = Some(old);
            let mut reg = FakeRegistrar::failing(&failures);

            let err =
                plan_reset(&mut reg, &mut active, &mut suspended, slot, None, None).unwrap_err();

            assert!(err.contains("恢复默认快捷键失败"), "{err}");
            assert!(suspended.is_none(), "失败须退出挂起");
            assert_eq!(active, old, "失败须恢复两个旧键");
            assert!(reg.registered.contains(&old_palette));
            assert!(reg.registered.contains(&old_window));
            for failing in failures {
                assert!(
                    !reg.registered.contains(&failing),
                    "默认键不得残留: {failing:?}"
                );
            }
        }
    }
}

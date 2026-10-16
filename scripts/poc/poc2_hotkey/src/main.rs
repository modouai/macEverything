//! PoC-2：⌥Space 族全局热键注册 + 按下事件验证（中文输入法环境）。
//!
//! 判断依据（Evidence）：
//! - global-hotkey 0.6.4 的 macOS 后端 = Carbon `RegisterEventHotKey` +
//!   `InstallEventHandler(GetApplicationEventTarget())`，见
//!   `global-hotkey-0.6.4/src/platform_impl/macos/mod.rs` 的 `register` 与 `hotkey_handler`。
//! - 该 crate 文档（`global-hotkey-0.6.4/src/lib.rs`）明示：
//!   「On macOS, an event loop must be running on the main thread」。
//! - 本机隔离实验（2026-10-16，三例同机同注入方式）：
//!   ① 裸进程 + CFRunLoop::run_in_mode 泵 → 收不到热键事件；
//!   ② NSApplication 初始化 + CFRunLoop 泵 → 仍收不到；
//!   ③ NSApplication 初始化 + `nextEventMatchingMask:untilDate:inMode:dequeue:` 拉取 → 收到。
//!   故本 PoC 走 ③：初始化 NSApplication 后用 nextEvent 拉取并派发事件（eframe/winit 的
//!   `NSApplication::run()` 内部即此循环，产品接线时同一路径）。
//!
//! 运行：`cargo run --release`（拉依赖走 rsproxy 镜像参数，见报告复现步骤）。
//! 行为：注册 ⌥Space / ⌥⇧Space / ⌃⌥⇧Space 三个候选并打印 OK/FAIL；
//!       期间任何成功注册的组合被按下即打印 `>>> pressed: id=<n> <组合名>`；
//!       默认 60 秒后自动退出（环境变量 POC2_SECS 可覆盖，供自动化短跑）。

use std::io::Write;
use std::time::{Duration, Instant};

use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

/// 默认运行时长：给人工切输入法、轮流按键留足时间。
const RUN_SECS: u64 = 60;

/// 每片事件拉取的阻塞上限（毫秒）；到点即返回消费热键通道，保证按键提示低延迟。
const POLL_SLICE_MS: f64 = 50.0;

fn flush() {
    let _ = std::io::stdout().flush();
}

fn main() {
    let secs: u64 = std::env::var("POC2_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RUN_SECS);

    // 先初始化 NSApplication（事件目标就绪），再建热键管理器——顺序与验证过的实验一致。
    let mtm = MainThreadMarker::new().expect("必须在主线程运行");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();

    let manager = GlobalHotKeyManager::new().expect("GlobalHotKeyManager::new 失败");
    println!("== PoC-2 全局热键注册（global-hotkey 0.6.4 / Carbon RegisterEventHotKey）==");
    println!("候选：⌥Space、⌥⇧Space、⌃⌥⇧Space；注册成功的组合按下即打印 pressed 行。");
    println!("提示：验证输入法冲突时，先在可输入文本处切到中文（拼音）输入法，再依次按三个候选键；随后切英文再按一遍。");
    println!("{secs} 秒后自动退出（POC2_SECS 可覆盖时长）。");
    flush();

    let candidates = [
        (Modifiers::ALT, Code::Space, "⌥Space"),
        (Modifiers::ALT | Modifiers::SHIFT, Code::Space, "⌥⇧Space"),
        (
            Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT,
            Code::Space,
            "⌃⌥⇧Space",
        ),
    ];

    // id → 组合名，用于收到 pressed 事件后反查（id 由 HotKey::new 计算：mods 位 << 16 | key）。
    let mut id_to_name: Vec<(u32, &'static str)> = Vec::new();
    let mut ok_count = 0usize;
    for (mods, code, name) in candidates {
        let hotkey = HotKey::new(Some(mods), code);
        match manager.register(hotkey) {
            Ok(()) => {
                ok_count += 1;
                println!("OK   {name}  (id={})", hotkey.id());
                id_to_name.push((hotkey.id(), name));
            }
            Err(e) => println!("FAIL {name}: {e}"),
        }
    }
    println!("== 注册结果：{ok_count}/{} 成功 ==", candidates.len());
    println!("（现在开始监听，请按键；按 Ctrl+C 可提前退出）");
    flush();

    let rx = GlobalHotKeyEvent::receiver();
    let start = Instant::now();
    let mut presses = 0u32;
    while start.elapsed() < Duration::from_secs(secs) {
        // 拉取并派发 AppKit 事件：Carbon 热键事件经应用事件队列由 nextEvent 取出
        // （仅泵 CFRunLoop 收不到，见文件头隔离实验）。untilDate 保证无事件时最多阻塞 50ms。
        let deadline = NSDate::dateWithTimeIntervalSinceNow(POLL_SLICE_MS / 1000.0);
        while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&deadline),
            unsafe { NSDefaultRunLoopMode },
            true,
        ) {
            app.sendEvent(&event);
        }
        while let Ok(event) = rx.try_recv() {
            if event.state == HotKeyState::Pressed {
                let name = id_to_name
                    .iter()
                    .find(|(id, _)| *id == event.id)
                    .map(|(_, name)| *name)
                    .unwrap_or("未知组合");
                presses += 1;
                println!(">>> pressed: id={} {}  （第 {presses} 次）", event.id, name);
                flush();
            }
        }
    }
    println!("== 结束：共收到 {presses} 次按下 ==");
    flush();
}

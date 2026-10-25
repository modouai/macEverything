# macEverything v2.0 图形界面 — 实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给已验收的 v1 引擎装上双形态图形界面:悬浮条(⌥Space 唤起、即输即开)+ 完整结果窗口(Everything 主窗式、原生右键菜单)+ 菜单栏常驻 + 开机自启 + 引擎自动拉起。

**Architecture:** 先 PoC 验证 5 个平台赌注(跨 Space/热键/NSMenu/eframe 两红线/自启),再迁移 protocol/client 双共享 crate,新增 app crate(egui/eframe + objc2 平台补丁,unsafe 集中 platform/mac.rs);engine 唯一扩展 = execute_sorted(批量闭包注入元数据)。打包 = bundle.sh 手工组装 .app(随包 CLI)。

**Tech Stack:** Rust stable(Apple Silicon)、eframe/egui、objc2 + objc2-app-kit、global-hotkey、tray-icon、memchr(v1)、既有 engine/protocol/client。

**Product Spec:** `docs/specs/v2-ui/product.md`(产品已确认,三轮审查)
**Tech Spec:** `docs/specs/v2-ui/tech.md`(已确认,三轮审查)

**通用验证命令**(每个 Task 收尾跑,下文以 `<check>` 代称):

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

**提交纪律**:本仓可能有并行会话的进行中改动——**一律精确 `git add <具体文件>`,禁 `git add -A`**。

**验收环境**:真实数据目录(约 120 万条目)与隔离环境(`MCE_ROOTS` + 临时 HOME)并用;PoC 类任务在隔离环境跑,最终验收在真实环境。

---

## 阶段 A:PoC 前置(5 个赌注先验证,任一失败走降级路径)

### Task 1: PoC-1 — objc2 跨 Space + 浮动级别 + LSUIElement 焦点

**Files:**
- Create: `scripts/poc/poc1_window/Cargo.toml` + `src/main.rs`(独立 PoC crate,不进 workspace members;根 Cargo.toml `[workspace] exclude` 追加)

**Why:** ADR-8 的核心赌注——eframe 官方不提供窗口级别/跨 Space;须从 winit raw handle 走 objc2 补丁。失败 → 降级「当前 Space 可见」(product 边界表)。

- [ ] **Step 1: 写最小验证程序**

```toml
# scripts/poc/poc1_window/Cargo.toml
[package]
name = "poc1-window"
version = "0.0.0"
edition = "2021"

[dependencies]
eframe = { version = "0.29", default-features = false, features = ["glow", "default_fonts"] }
objc2 = "0.5"
objc2-app-kit = { version = "0.2", features = ["NSWindow", "NSApplication", "NSRunningApplication"] }
raw-window-handle = "0.6"
```

```rust
// scripts/poc/poc1_window/src/main.rs
//! PoC-1:eframe 无边框窗口 + objc2 补丁(浮动级别/跨 Space);F1 切换显示,ESC 退出。
use objc2::rc::Retained;
use objc2_app_kit::{NSCollectionBehavior, NSWindow, NSWindowLevel};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

fn patch_window(win: &eframe::egui::ViewportInfo) { /* 见 Step 2 说明:从 eframe 取 NSWindow 指针 */ }

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 400.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_transparent(true),
        ..Default::default()
    };
    eframe::run_native("poc1", opts, Box::new(|cc| {
        // 取 NSWindow 并施补丁:
        // let ptr = cc.window_handle()...  (eframe 0.29 提供 cc.window_handle())
        // let ns: &NSWindow = unsafe { &*(ptr.cast()) };
        // unsafe {
        //     ns.setLevel(NSWindowLevel::Floating);            // 或 Status 档,两档都试
        //     ns.setCollectionBehavior(NSCollectionBehavior::CanJoinAllSpaces
        //         | NSCollectionBehavior::FullScreenAuxiliary);
        // }
        Box::new(PocApp { visible: true })
    }))
}

struct PocApp { visible: bool }
impl eframe::App for PocApp {
    fn update(&mut self, ctx: &egui::Context, _f: &mut eframe::Frame) {
        egui::CentralPanel::default().frame(egui::Frame::none().fill(egui::Color32::from_black_alpha(220))).show(ctx, |ui| {
            ui.heading("PoC-1:切 Space / 全屏 App 上方观察本窗");
            ui.label("F1 隐藏/显示;ESC 退出");
        });
        // F1: ctx.input(|i| i.key_pressed(egui::Key::F1)) → ViewportCommand::Visible(toggle)
        // ESC: ctx.send_viewport_cmd(egui::ViewportCommand::Close)
    }
}
```

**Step 2: 接线细节(eframe 0.29 API 以实锁文档为准)**:`eframe::CreationContext` 提供窗口句柄(或经 `raw_window_handle` 从 `cc` 取 `AppKitWindowHandle`);`ns_window` 指针 → `&NSWindow`。**版本敏感:Doubt-Driven 纪律——先 `cargo doc -p eframe --no-deps` 或翻 crate 源码确认 API 名,禁凭记忆**。若 eframe 版本句柄 API 不同,直接改用 `objc2_app_kit::NSApplication::sharedApplication().windows()` 取窗口(也记录在 PoC 报告)。

**Step 3: 实机验证(逐项记录结果)**

1. 启动后切到另一个 Space(三指滑动)→ 窗口**仍在**可见 = 跨 Space ✓
2. 打开全屏应用(如浏览器全屏)→ 窗口**浮在其上** = 浮动级别 ✓
3. 窗口无标题栏圆角黑底 = 透明+无边框 ✓
4. 点窗口输入框(如加个 TextEdit 测试)→ 能聚焦输入 = LSUIElement 场景键盘焦点路径(完整验证留到 Task 13 打包后;此处确认一般焦点 OK)

**Step 4: 记录结论**——把每个子项的通过/失败/现象写入 `docs/specs/v2-ui/poc-results.md`(新建);失败项写明降级决策。**若跨 Space 失败 → 立即上报维护者**(产品目标 7 依赖它),不擅自改 spec。

**Step 5: 提交**

```bash
git add scripts/poc/poc1_window Cargo.toml docs/specs/v2-ui/poc-results.md
git commit -m "chore(poc): PoC-1 objc2 跨 Space/浮动级别/LSUIElement 焦点验证"
```

---

### Task 2: PoC-2 — ⌥Space 全局注册(中文输入法环境)

**Files:**
- Create: `scripts/poc/poc2_hotkey/`(同 PoC 目录约定)

**Why:** product 边界表——⌥Space 可能被输入法占用(本机已启用 SCIM 中文输入法);失败 → 备选位(⌥⇧Space / ⌃⌥⇧Space,⌃Space 族已排除)。

- [ ] **Step 1: 最小验证**

```toml
# scripts/poc/poc2_hotkey/Cargo.toml
[dependencies]
global-hotkey = "0.6"
```

```rust
// src/main.rs:注册 ⌥Space / ⌥⇧Space / ⌃⌥⇧Space 各试一次,打印注册成败;
// 成功的注册监听按下事件(callback 或 receiver 轮询),按下时 eprintln!("pressed: <组合>")。
// 运行 10 秒后退出。
use global_hotkey::{hotkey::{Code, HotKey, Modifiers}, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

fn main() {
    let manager = GlobalHotKeyManager::new().expect("manager");
    let candidates = [
        (Modifiers::ALT, Code::Space, "⌥Space"),
        (Modifiers::ALT | Modifiers::SHIFT, Code::Space, "⌥⇧Space"),
        (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SHIFT, Code::Space, "⌃⌥⇧Space"),
    ];
    for (m, c, name) in candidates {
        let hk = HotKey::new(Some(m), c);
        match manager.register(hk) {
            Ok(()) => println!("OK  {name}"),
            Err(e) => println!("FAIL {name}: {e}"),
        }
    }
    // 10 秒内轮询 GlobalHotKeyEvent::receiver(),按下即打印;结束后 exit(0)
    let rx = GlobalHotKeyEvent::receiver();
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < 10 {
        if let Ok(ev) = rx.try_recv() {
            if ev.state == HotKeyState::Pressed { println!("pressed id={}", ev.id); }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
```

- [ ] **Step 2: 实机验证**:运行后**切到中文输入法(拼音)状态**,依次按三个候选键——观察终端是否打印 `pressed`。同时验证:输入法候选窗弹出时按键是否仍触发(冲突的典型形态)。

- [ ] **Step 3: 记录结论到 `poc-results.md`**:哪个键可用、输入法状态下是否失效;若 ⌥Space 在中文态失效 → 默认键的最终选择写入报告(可能需在 App 里做「输入法态探测提示」——记录实测现象即可,方案 Task 15 定)。**若全候选失败 → 上报维护者**(需给更多候选取舍)。

- [ ] **Step 4: 提交**

```bash
git add scripts/poc/poc2_hotkey docs/specs/v2-ui/poc-results.md
git commit -m "chore(poc): PoC-2 ⌥Space 全局注册与中文输入法冲突验证"
```

---

### Task 3: PoC-3 — 原生 NSMenu 在 egui 主线程弹出

**Files:**
- Create: `scripts/poc/poc3_nsmenu/`

**Why:** 右键菜单用原生 NSMenu(产品要求);`popUpContextMenu` 是嵌套 run loop(阻塞),与 eframe 主循环的兼容性未验证。失败 → 独立线程弹出或非模态 API(tech 风险表)。

- [ ] **Step 1: 最小验证**

dependencies:`eframe`(同 PoC-1)+ `objc2` + `objc2-app-kit`(features 加 `NSMenu`, `NSMenuItem`, `NSEvent`)。

```rust
// src/main.rs 要点:
// 1. 一个 eframe 窗口(可静态文本 + eframe 区域右键触发)
// 2. 右键时(ctx.input 检测 secondary_clicked)构造 NSMenu:
//    let menu_title = NSString::from_str("");
//    let menu = NSMenu::initWithTitle(NSMenu::alloc(), &menu_title);
//    for (title, tag) in [("打开", 1), ("在访达中打开", 2), ("在访达中显示", 3), ("拷贝路径", 4)] {
//        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
//            NSMenuItem::alloc(), &NSString::from_str(title), None, &NSString::from_str(""));
//        item.setTag(tag);
//        menu.addItem(&item);
//    }
//    // 3. 在鼠标位置弹出(阻塞式):
//    let loc = NSEvent::mouseLocation();  // 屏坐标
//    menu.popUpContextMenu(None, &NSEvent::..., &view);   // 具体 API 以 objc2-app-kit 文档为准
//    // 4. 关闭后:读 menu.highlightedItem() 或经 NSMenuItem target/action 回调拿 tag,打印
//    // 5. 观察:弹出期间 egui 是否卡死/恢复、关闭后输入是否正常
```

**Doubt-Driven**:`popUpContextMenu_positioningView` / `popUpMenuPositioningItem_atLocation_inView` 的 objc2 绑定名与签名以 `objc2-app-kit` 实锁源码为准(禁凭记忆)。target/action 桥接若复杂,PoC 阶段可先**只验证弹出与阻塞行为**(读 highlightedItem 即可);回调桥接产品化在 Task 18。

- [ ] **Step 2: 实机验证**:右键弹出 → 菜单为**系统观感**(含键盘导航 ↑↓)→ 选择后 egui 窗口仍响应鼠标/输入(无卡死)→ 连续弹 5 次无泄漏崩溃。

- [ ] **Step 3: 记录结论 + 提交**

```bash
git add scripts/poc/poc3_nsmenu docs/specs/v2-ui/poc-results.md
git commit -m "chore(poc): PoC-3 原生 NSMenu 与 egui 主循环兼容性验证"
```

---

### Task 4: PoC-4 — eframe 两红线(隐藏 CPU 0% vs 唤起 <100ms)

**Files:**
- Create: `scripts/poc/poc4_idle/`

**Why:** product 性能红线 1/3 的拉扯——隐藏视口须零 CPU(`ControlFlow::Wait`),但唤起 <100ms 要求热路径就绪。ADR-8 单栈押注的核心风险。

- [ ] **Step 1: 最小验证**

```rust
// src/main.rs 要点:
// 1. eframe 单窗口,初始可见;按 F1 切 ViewportCommand::Visible(false/true) 循环
// 2. 隐藏分支:ctx.request_repaint 不调用(默认即 Wait);若样板有周期性 repaint,确认无处发起
// 3. 显示时在 update 首帧记录 Instant::now() 与「请求显示时刻」的时间差(近似唤起延迟)
// 4. main 里 spawn 一个线程每 2 秒 F1 自动 toggle(模拟唤起),打印每次切换耗时
```

- [ ] **Step 2: 实机测量**

```bash
# 运行后:
# a) 隐藏态持续 60 秒,`top -pid <pid> -l 60 | awk` 观察 CPU 列——须恒 0.0%
# b) 观察 30 次自动切换的间隔日志:显示侧延迟须 <100ms
```

- [ ] **Step 3: 记录**:隐藏态 CPU 实测值、切换延迟分布(中位/P99);若隐藏态 CPU 非 0 → 排查 repaint 源;若唤起 >100ms → 记录首帧成本,评估「keep-warm 预渲染」折中(隐藏但保留最小 repaint 的取舍要上报维护者)。

- [ ] **Step 4: 提交**

```bash
git add scripts/poc/poc4_idle docs/specs/v2-ui/poc-results.md
git commit -m "chore(poc): PoC-4 eframe 隐藏零 CPU 与唤起延迟两红线验证"
```

---

### Task 5: PoC-5 — SMAppService 自启(ad-hoc 签名)

**Files:**
- Create: `scripts/poc/poc5_autostart/`(Swift 或 Rust+objc2 均可;Swift 单文件最快)

**Why:** product §4 自启默认开启;v1 development.md 记录 ad-hoc 签名的自启可行性未验证。失败 → LaunchAgent plist 兜底(tech 风险表)。

- [ ] **Step 1: 最小验证(Swift 单文件,`swift scripts/poc/poc5_autostart/poc.swift`)**

```swift
import ServiceManagement
// 1. 注册:SMAppService.mainApp.register() —— 需要先有 .app bundle 形态!
//    无 bundle 直接跑会报错——这正是 PoC 的一部分:先做最小 .app 骨架:
//    mkdir -p /tmp/poc5.app/Contents/MacOS && cp <binary> ... && 写最小 Info.plist
// 2. 注册后:launchctl 检查 / print(SMAppService.mainApp.status.rawValue)
// 3. 注销:unregister();确认无残留(launchctl list | grep poc5)
```

**注意**:SMAppService.mainApp 要求当前进程即 .app bundle 内可执行——PoC 需先用 `scripts/poc/poc5_autostart/make-mini-app.sh` 组一个最小 .app(含 Info.plist:LSUIElement=true、CFBundleIdentifier=com.example.maceverything.poc)再运行其内可执行。整个流程本身就是 Task 20 打包的前哨验证。

- [ ] **Step 2: 验证三态**:注册成功(status == .enabled)→ `launchctl list` 可见 → 注销干净(无残留条目);**重启机器验证一次**(登录后 App 自动起来)——真实重启或 `sudo launchctl kickstart -k gui/$(id -u)/com.example.maceverything.poc` 近似验证,报告注明用的哪种。

- [ ] **Step 3: 记录结论 + 提交**

```bash
git add scripts/poc/poc5_autostart docs/specs/v2-ui/poc-results.md
git commit -m "chore(poc): PoC-5 SMAppService + ad-hoc 自启注册/注销/开机验证"
```

---

## 阶段 B:crate 迁移(protocol + client 双共享 crate)

### Task 6: protocol crate 成立 + cli 迁移

**Files:**
- Create: `crates/protocol/Cargo.toml`、`crates/protocol/src/lib.rs`(从 `crates/cli/src/protocol.rs` **原样搬迁**)
- Modify: 根 `Cargo.toml`(members + workspace.dependencies 无新增——protocol 依赖 serde/serde_json 已在)
- Modify: `crates/cli/Cargo.toml`(deps 加 `protocol = { path = "../protocol" }`;serde/serde_json 视 cli 其余使用保留)
- Modify: `crates/cli/src/main.rs`(`mod protocol;` 删;`crates/cli/src/protocol.rs` 删)
- Modify: `crates/cli/src/{daemon,client}.rs`(`use crate::protocol` → `use protocol`)

**Why:** ADR-7——协议为唯一契约真相,cli 与 app 共用。

- [ ] **Step 1: 迁移(机械)**

```bash
mkdir -p crates/protocol/src && git mv crates/cli/src/protocol.rs crates/protocol/src/lib.rs
```

```toml
# crates/protocol/Cargo.toml
[package]
name = "protocol"
version.workspace = true
edition.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
```

根 Cargo.toml members 追加 `"crates/protocol"`;cli 引用改 `use protocol::{...}`。

- [ ] **Step 2: `<check>` 全绿(含 protocol 原测试在 lib.rs 内保留运行)**

Run: `<check>`
Expected: 91 项全过(协议测试随迁,数量不变)。

- [ ] **Step 3: CLI 冒烟(隔离环境)**

```bash
mkdir -p /tmp/mce-mig/Documents && echo x > /tmp/mce-mig/Documents/迁移探针.md
HOME=/tmp/mce-mig MCE_ROOTS=/tmp/mce-mig ./target/debug/macEverything start  # 或 MCE_FOREGROUND=1
HOME=/tmp/mce-mig ./target/debug/macEverything query "迁移探针"   # 命中
HOME=/tmp/mce-mig ./target/debug/macEverything stop
```

- [ ] **Step 4: 提交**(精确 add)

```bash
git add crates/protocol crates/cli Cargo.toml Cargo.lock
git commit -m "refactor(protocol): 协议提升为共享 crate(ADR-7),cli 迁移引用"
```

---

### Task 7: client crate 成立(承载层)

**Files:**
- Create: `crates/client/Cargo.toml`、`crates/client/src/lib.rs`
- Modify: `crates/cli/Cargo.toml`(+ `client = { path = "../client" }`;cli 保留 daemon.rs)
- Modify: `crates/cli/src/{main,daemon}.rs`、`crates/cli/src/client.rs` 删除

**Why:** ADR-7——离线降级/连接/stat_rows/状态常量的唯一行为真相;app 经此依赖 engine。cli 无 lib target 的历史问题随之消解(承载层即 lib)。

- [ ] **Step 1: 搬迁内容清单**(从 cli 到 client,机械)

从 `crates/cli/src/client.rs` 迁入:
- `connect()` / `call()` / `expect_ok()`(socket 请求事务)
- `offline_query()` / `offline_status()` 的核心(输出打印部分留在 CLI 侧!承载层只回数据)——**重构**:承载层提供 `query(paths, q, limit, sort) -> Result<QueryOutcome 数据>`(在线/离线统一入口),CLI 与 App 各自格式化。
- `RESULT_CAP`

从 `crates/cli/src/daemon.rs` 迁入:
- `DaemonPaths`(socket/index_file/config_file/log_file 路径)
- `state` 常量模块
- `stat_of` / `stat_rows`(按需读取)

**重构纪律**:迁移是搬移+可见性调整,不改行为;凡需要拆的(如 offline_query 的打印分离)以「先搬后拆」两步提交,拆的步在 Task 9/app 接线时做——本任务只搬,CLI 行为零变化。

- [ ] **Step 2: cli 引用改 `use client::{DaemonPaths, ...}`;daemon.rs 保留服务宿主(EngineState/服务循环/冷启动)**

- [ ] **Step 3: `<check>` + CLI 冒烟(同上隔离配方)+ 提交**

```bash
git add crates/client crates/cli Cargo.toml Cargo.lock
git commit -m "refactor(client): 客户端承载层提升为共享 crate(ADR-7),cli 迁移"
```

---

### Task 8: 引擎排序扩展 execute_sorted(TDD)

**Files:**
- Modify: `crates/engine/src/query.rs`(+ `SortKey`/`SortOutcome`/`execute_sorted`)
- Test: 同文件 `#[cfg(test)]`
- Modify: `crates/engine/src/lib.rs`(如导出需要)

**Why:** product §8/tech「引擎扩展」——完整窗口列排序的唯一引擎改动;批量闭包注入保住 engine 纯逻辑。

- [ ] **Step 1: 失败测试(先写)**

```rust
#[cfg(test)]
mod sort_tests {
    use super::*;
    use crate::index::{Index, IndexView, NO_PARENT};
    use std::collections::HashMap;

    fn tree() -> Index {
        let mut i = Index::new();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("b.txt", d, false);   // size 30
        i.insert("a.txt", d, false);   // size 20
        i.insert("c.txt", d, false);   // size 10
        i
    }
    // 假 meta:名字→(size, mtime),条目号映射在闭包里经 view 解析?——
    // 简化:测试直接按条目号约定:1=a? 插入序:b(1) a(2) c(3)。用固定表。
    fn meta(ids: &[u32]) -> Vec<(Option<u64>, Option<u64>)> {
        let sizes = [30u64, 20, 10]; // 条目 1/2/3 依次
        ids.iter()
            .map(|&i| (Some(sizes[(i - 1) as usize]), Some(1000 + i as u64)))
            .collect()
    }

    #[test]
    fn sort_by_size_ascending() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let out = execute_sorted(&i, &plan, 10, SortKey::Size, &meta);
        let names: Vec<String> = out.hits.iter()
            .map(|h| String::from_utf8_lossy(i.display_name_of(*h)).into_owned()).collect();
        assert_eq!(names, vec!["c.txt", "a.txt", "b.txt"]); // 10 < 20 < 30
    }

    #[test]
    fn sort_by_mtime_and_limit() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let out = execute_sorted(&i, &plan, 2, SortKey::Mtime, &meta);
        assert_eq!(out.hits.len(), 2);
        assert_eq!(out.total, 3); // total 为截断前全量
        assert_eq!(i.display_name_of(out.hits[0]), b"c.txt"); // mtime 1003 最大 → 降序?—
        // 定案:mtime 排序为**降序**(最新在前)还是升序?
        // product 未定——本 plan 定案:mtime 一律**降序**(最新在前,文件管理器惯例);
        // size 默认升序,点列头方向切换在 UI 侧(状态栏见 tech)。
    }

    #[test]
    fn sort_by_name_display_order() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let out = execute_sorted(&i, &plan, 10, SortKey::Name, &meta);
        let names: Vec<Vec<u8>> = out.hits.iter().map(|h| i.display_name_of(*h).to_vec()).collect();
        assert_eq!(names, vec![b"a.txt".to_vec(), b"b.txt".to_vec(), b"c.txt".to_vec()]);
        // Name 排序不得调用 meta(零 stat):用一个 panics 的 meta 闭包验证
    }

    #[test]
    fn relevance_delegates_to_execute() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let base = execute(&i, &plan, 10);
        let sorted = execute_sorted(&i, &plan, 10, SortKey::Relevance, &|_| panic!("不得调用 meta"));
        assert_eq!(base.hits, sorted.hits);
        assert_eq!(base.total, sorted.total);
    }
}
```

- [ ] **Step 2: 跑失败** Run: `cargo test -p engine sort_tests` Expected: FAIL(未定义)

- [ ] **Step 3: 实现**

```rust
// query.rs 追加
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey { Relevance, Name, Size, Mtime, Path }

pub struct SortOutcome {
    pub hits: Vec<u32>,
    pub total: u32,
    pub elapsed_ms: f64,
    pub stat_ms: f64,
}

/// 按列排序查询。meta_of 为批量元数据注入(按入参顺序返回);Relevance 委托 execute。
pub fn execute_sorted(
    view: &dyn IndexView,
    plan: &QueryPlan,
    limit: usize,
    sort: SortKey,
    meta_of: &dyn Fn(&[u32]) -> Vec<(Option<u64>, Option<u64>)>,
) -> SortOutcome {
    if sort == SortKey::Relevance {
        let out = execute(view, plan, limit);
        return SortOutcome { hits: out.hits, total: out.total, elapsed_ms: out.elapsed_ms, stat_ms: 0.0 };
    }
    let t = std::time::Instant::now();
    // 1) 全量匹配(不分截断):复用 execute 的内部路径——实现:execute 取全量 limit=total?
    //    execute 的 limit 截断在排序后;直接调 execute(view, plan, u32::MAX as usize) 得全量顺序(质量序)
    let full = execute(view, plan, usize::MAX);
    let mut hits = full.hits;
    let total = full.total;
    // 2) 键计算
    let stat_t = std::time::Instant::now();
    let meta: Vec<(Option<u64>, Option<u64>)> = match sort {
        SortKey::Size | SortKey::Mtime => meta_of(&hits),
        _ => vec![],
    };
    let stat_ms = stat_t.elapsed().as_secs_f64() * 1000.0;
    // 3) 排序(等值键条目号决胜;确定性)
    match sort {
        SortKey::Size => {
            let idx: std::collections::HashMap<u32, (Option<u64>, Option<u64>)> =
                hits.iter().copied().zip(meta).collect();
            hits.sort_by(|a, b| {
                let ka = idx[a].0.unwrap_or(u64::MAX);
                let kb = idx[b].0.unwrap_or(u64::MAX);
                ka.cmp(&kb).then(a.cmp(b))
            });
        }
        SortKey::Mtime => {
            let idx: std::collections::HashMap<u32, (Option<u64>, Option<u64>)> =
                hits.iter().copied().zip(meta).collect();
            // 降序(最新在前)
            hits.sort_by(|a, b| {
                let ka = idx[a].1.unwrap_or(0);
                let kb = idx[b].1.unwrap_or(0);
                kb.cmp(&ka).then(a.cmp(b))
            });
        }
        SortKey::Name => {
            hits.sort_by(|a, b| {
                view.display_name(*a).cmp(view.display_name(*b)).then(a.cmp(b))
            });
        }
        SortKey::Path => {
            hits.sort_by(|a, b| {
                view.full_path_dyn(*a).cmp(&view.full_path_dyn(*b)).then(a.cmp(b))
            });
        }
        SortKey::Relevance => unreachable!(),
    }
    hits.truncate(limit);
    SortOutcome { hits, total, elapsed_ms: t.elapsed().as_secs_f64() * 1000.0, stat_ms }
}
```

注意:`execute(view, plan, usize::MAX)` 依赖 execute 的 select_nth 在 limit>len 时跳过(已有实现如此);若内部行为有差,以「先全量 collect 再本函数统一截断」为准调整,保持 Relevance 路径委托不变。

- [ ] **Step 4: 跑通过 + `<check>`**

- [ ] **Step 5: 提交**

```bash
git add crates/engine/src/query.rs
git commit -m "feat(engine): execute_sorted 按列排序(批量元数据注入,Relevance 委托原路径)"
```

### Task 8b: 协议与 daemon 接线 sort/sort_echo

**Files:**
- Modify: `crates/protocol/src/lib.rs`(`Request::Query` 加 `sort: Option<String>`;`Response::Query` 加 `sort_echo: String`)
- Modify: `crates/client/src/lib.rs`(查询请求构造带 sort;解析响应带 sort_echo)
- Modify: `crates/cli/src/daemon.rs`(run_query:sort 分派 execute_sorted;meta_of 批量并行 stat + 缓存 map;响应带 sort_echo)

**Why:** tech「协议变更」——sort 字段 + 回显防静默错排序;stat 缓存 map 兑现「无重查」。

- [ ] **Step 1: 协议测试先行(protocol crate 内)**

```rust
#[test]
fn query_sort_field_backward_compatible() {
    // 旧 JSON(无 sort/sort_echo)可解码:sort=None, sort_echo 缺省空串
    let old_req = br#"{"msg":"query","v":1,"query":"a","limit":10}"#;
    let r: Request = serde_json::from_slice(old_req).unwrap();
    assert!(matches!(r, Request::Query { sort: None, .. }));
    let old_resp = br#"{"msg":"query","entries":[],"total":0,"elapsed_ms":0.0,"stat_ms":0.0}"#;
    let p: Response = serde_json::from_slice(old_resp).unwrap();
    assert!(matches!(p, Response::Query { sort_echo, .. } if sort_echo.is_empty()));
}

#[test]
fn sort_echo_roundtrip() {
    let r = Request::Query { v: 1, query: "a".into(), limit: 10, sort: Some("size".into()) };
    // 编码→解码→sort 保留
}
```

- [ ] **Step 2: 实现协议字段(serde default);client 请求构造带 `sort: Option<String>`(None=relevance)**

- [ ] **Step 3: daemon run_query 分派**

```rust
fn run_query(view: &dyn IndexView, q: &str, limit: u32, sort: Option<&str>, cache: &mut HashMap<u32, (Option<u64>, Option<u64>)>) -> Response {
    let key = match sort.unwrap_or("relevance") { "name" => SortKey::Name, "size" => SortKey::Size, "mtime" => SortKey::Mtime, "path" => SortKey::Path, _ => SortKey::Relevance };
    let plan = match engine::query::parse(q) { Ok(p) => p, Err(_) => return Response::Error { code: "parse".into(), message: "空查询".into() } };
    // 批量 stat 闭包(内部 rayon;结果写缓存 map 供响应组装免重查)
    let meta_fn = |ids: &[u32]| -> Vec<(Option<u64>, Option<u64>)> {
        use rayon::prelude::*;
        let rows: Vec<(u32, (Option<u64>, Option<u64>))> = ids.par_iter().map(|&i| {
            let path = view.full_path_dyn(i);
            (i, stat_of(&path))
        }).collect();
        for (i, m) in &rows { cache.insert(*i, *m); }   // 借用问题定案:闭包捕获 &Mutex<HashMap>(内部可变性),见下「缓存填法」
        rows.into_iter().map(|(_, m)| m).collect()
    };
    // ... execute_sorted 调用、响应组装时 stat_rows 先查 cache
    let _ = cache;
    Response::Query { entries, total, elapsed_ms, stat_ms, offline: false, built_at: 0, sort_echo: key_str }
}
```

**缓存填法(定案,8b 实施定稿)**:`meta_of` 闭包捕获 `&Mutex<HashMap<u32, MetaRow>>`,每批 stat 后写入该 map(`&dyn Fn` 下经 Mutex 取内部可变性,免 FnMut 借用冲突;草稿的 `&mut cache` 形参作废)。排序完成后响应组装对 `out.hits` 行**查 map 命中优先**,未命中再经 `stat_rows` 并行补全——Size/Mtime 排序下结果行在排序轮已 stat(hits ⊆ 候选集),零重查;Relevance/Name/Path 下缓存为空、全部走补全轮,与 v1 行为一致。tech.md「stat 归属与去重」按此同步。

- [ ] **Step 4: 测试 + `<check>` + CLI 冒烟(老查询不变;`sort` 经 CLI 无入口,以 cargo test 与后续 App 验收为准)**

- [ ] **Step 5: 提交**

```bash
git add crates/protocol crates/client crates/cli docs/specs/v2-ui/tech.md
git commit -m "feat(protocol/client/daemon): sort 字段 + sort_echo 回显 + 排序 stat 缓存接线"
```

---

## 阶段 C:app crate 基础

### Task 9: app crate 骨架 + 双视口壳

**Files:**
- Create: `crates/app/Cargo.toml`、`src/main.rs`、`src/ui/mod.rs`
- Modify: 根 `Cargo.toml`(members + workspace.dependencies 加 eframe/egui/objc2 等——版本以 `cargo add` 实锁为准)

**Why:** 双形态的骨架:eframe 主循环 + 悬浮条视口 + 完整窗口视口(初始都可见便于开发,后按流程收)。

- [ ] **Step 1: 骨架**

```toml
# crates/app/Cargo.toml
[package]
name = "macEverything-app"
version.workspace = true
edition.workspace = true

[[bin]]
name = "macEverything-app"
path = "src/main.rs"

[dependencies]
client = { path = "../client" }
protocol = { path = "../protocol" }
eframe = { version = "0.29", default-features = false, features = ["glow", "default_fonts"] }
# 表格与虚拟化:自定义 `ScrollArea::show_rows`,不引 egui_extras(Task 17 定案)
objc2 = "0.5"
objc2-app-kit = { version = "0.2", features = ["NSWindow", "NSApplication", "NSMenu", "NSMenuItem", "NSEvent"] }
global-hotkey = "0.6"
tray-icon = "0.19"
anyhow = "1"
log = { workspace = true, features = ["std"] }
```

```rust
// src/main.rs(骨架:两个 viewport,主窗口=完整窗口,第二 viewport=悬浮条)
mod platform;
mod session;
mod ui;

fn main() -> anyhow::Result<()> {
    // SIGPIPE 等平台前置;日志(简化:stderr);单实例/引擎拉起在 Task 10 接
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("macEverything")
            .with_inner_size([900.0, 600.0])
            .with_visible(false),   // 初始都隐藏;开发期可置 true
        ..Default::default()
    };
    eframe::run_native("macEverything", native_options, Box::new(|cc| {
        ui::App::new(cc)   // 持有第二 viewport(悬浮条)的 builder 与状态
    })).map_err(|e| anyhow::anyhow!("eframe: {e}"))
}
```

`ui/mod.rs`:App 结构持有:`palette_visible: bool`、`results_visible: bool`、两视口的 egui::ViewportId;`update()` 里用 `ctx.show_viewport_immediate(ViewportId::from_hash_of("palette"), ViewportBuilder..., |ctx, _| palette::draw(ctx, &mut state))` 绘制悬浮条(Task 12 填充);主视口画完整窗口占位(Task 17 填充)。

**平台行为(Task 14 详接)**:本任务先只把 `platform::mac::patch_palette_window(cc)` 空实现挂上(返回 Ok,不做事),后续任务填。

- [ ] **Step 2: 编译 + 手工跑通(两空窗能显示/拖动)** + `<check>`

- [ ] **Step 3: 提交**

```bash
git add crates/app Cargo.toml Cargo.lock
git commit -m "feat(app): crate 骨架 + eframe 双视口壳(悬浮条/完整窗口空视图)"
```

---

### Task 10: 引擎定位/拉起 + app.sock 单实例

**Files:**
- Create: `crates/app/src/engine_launcher.rs`(定位三档 + spawn + 轮询)
- Create: `crates/app/src/single_instance.rs`(app.sock 监听 + 二次启动唤起)
- Modify: `crates/app/src/main.rs`(启动序列接线)

**Why:** product §5——app.sock 单实例(兑现「二次启动唤起悬浮条」)、引擎定位三档(随包优先,列搜过路径)。

- [ ] **Step 1: engine_launcher(纯逻辑部分 TDD:候选路径生成可测)**

```rust
/// 定位候选(顺序):① current_exe 同目录/macEverything ② PATH 各段/macEverything。
/// 返回「依次尝试的绝对路径列表」——可测(注入 exe_dir 与 path_var)。
pub fn engine_candidates(exe_dir: &Path, path_var: Option<&OsStr>) -> Vec<PathBuf> {
    let mut v = vec![exe_dir.join("macEverything")];
    if let Some(p) = path_var {
        for seg in std::env::split_paths(p) {
            v.push(seg.join("macEverything"));
        }
    }
    v
}

/// 拉起:对候选逐个「存在且可执行」检查→ spawn("start")→ 轮询 daemon.sock ≤3s。
/// 全失败 → Err(消息内含全部候选路径,逐行列)。
pub fn ensure_engine(paths: &client::DaemonPaths, exe_dir: &Path) -> anyhow::Result<()> { /* 实现:connect 先探测(已在跑直接 Ok);否则候选定位/spawn/轮询;失败 Err(列路径) */ }
```

测试:`engine_candidates` 顺序与去重;「全失败错误消息含全部候选」断言。

- [ ] **Step 2: single_instance**

```rust
/// App 启动时 bind app.sock:
/// - bind 成功 → 本进程持有,spawn 监听线程:收到 "show" 消息 → 置「唤起悬浮条」事件(经 mpsc 送 UI 线程)
/// - bind 失败(EADDRINUSE)→ connect 探测:连上 → 发 "show" 后 process::exit(0)(唤起已有实例);
///   拒连(stale)→ 删 sock 重绑。
pub enum Incoming { ShowPalette }
pub fn acquire(paths: &DaemonPaths) -> anyhow::Result<(SingleInstance, mpsc::Receiver<Incoming>)>
```

- [ ] **Step 3: main 接线**:`acquire` → `ensure_engine` → 起 UI;`Incoming::ShowPalette` → 置 palette_visible=true。

- [ ] **Step 4: 隔离冒烟**

```bash
HOME=/tmp/mce-app MCE_ROOTS=/tmp/mce-app ./target/debug/macEverything-app &   # 应自动拉起引擎(隔离数据目录)
sleep 3 && HOME=/tmp/mce-app ./target/debug/macEverything-app                # 二次启动:秒退并唤起已有实例(手动观察:第二进程退出码 0)
HOME=/tmp/mce-app ./target/debug/macEverything status                        # 引擎就绪
HOME=/tmp/mce-app ./target/debug/macEverything stop
```

- [ ] **Step 5: `<check>` + 提交**

```bash
git add crates/app
git commit -m "feat(app): 引擎定位三档/自动拉起 + app.sock 单实例通道"
```

---

### Task 11: session.rs 查询会话(纯逻辑 TDD)

**Files:**
- Create: `crates/app/src/session.rs`
- Test: 同文件 `#[cfg(test)]`

**Why:** tech「查询会话」——防抖 + 代际 + 六态状态机;UI 的输入手感与正确性核心,全部纯逻辑可测。

- [ ] **Step 1: 失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn t0() -> Instant { Instant::now() }

    #[test]
    fn debounce_coalesces_keystrokes() {
        let mut s = Session::new(Duration::from_millis(60));
        let base = t0();
        s.on_input("a", base);
        s.on_input("ab", base + Duration::from_millis(20));
        s.on_input("abc", base + Duration::from_millis(40));
        assert_eq!(s.pending_at(base + Duration::from_millis(80)), None, "窗口未到");
        let fire = s.pending_at(base + Duration::from_millis(110)).expect("窗口到应发起");
        assert_eq!(fire.query, "abc");
        assert_eq!(fire.generation, 1, "三次输入合并为一代");
    }

    #[test]
    fn stale_response_discarded() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = t0();
        s.on_input("a", base);
        let g1 = s.pending_at(base + Duration::from_millis(20)).unwrap().generation;
        s.on_input("ab", base + Duration::from_millis(30));
        let g2 = s.pending_at(base + Duration::from_millis(50)).unwrap().generation;
        assert!(g2 > g1);
        assert!(!s.accept_response(g1), "旧代响应丢弃");
        assert!(s.accept_response(g2), "当代响应接受");
    }

    #[test]
    fn empty_query_idles() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = t0();
        s.on_input("", base);
        assert_eq!(s.pending_at(base + Duration::from_millis(50)), None, "空词不发起");
        assert!(matches!(s.state(), State::Idle));
    }

    #[test]
    fn warming_maps_to_state() {
        let mut s = Session::new(Duration::from_millis(10));
        s.mark_response_error("warming");
        assert!(matches!(s.state(), State::Warming));
        s.mark_response_error("parse");
        assert!(matches!(s.state(), State::Error(_)));
    }

    #[test]
    fn sort_change_refires_same_query() {
        let mut s = Session::new(Duration::from_millis(10));
        let base = t0();
        s.on_input("a", base);
        let f1 = s.pending_at(base + Duration::from_millis(20)).unwrap();
        s.on_sort_change(SortKey::Size, base + Duration::from_millis(30));
        let f2 = s.pending_at(base + Duration::from_millis(50)).unwrap();
        assert_eq!(f2.query, f1.query);
        assert_ne!(f2.generation, f1.generation);
        assert_eq!(f2.sort, Some(SortKey::Size));
    }
}
```

- [ ] **Step 2: 实现(骨架)**

```rust
//! 查询会话:防抖 + 代际号 + 状态机(纯逻辑;时钟由调用方传入,测试可注入)。
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State { Idle, Searching, EngineStarting, Offline, Empty, Warming, Error(&'static str) }

// 定案:直接复用 engine::query::SortKey(app 经 client 承载层依赖 engine 已许可,
// ADR-7);协议层字符串 ↔ SortKey 的映射函数与本模块同放。
pub use engine::query::SortKey;

pub struct Fire { pub query: String, pub sort: SortKey, pub generation: u64 }

pub struct Session {
    debounce: Duration,
    last_input: Option<(String, Instant)>,
    generation: u64,
    state: State,
    sort: SortKey,
}
// 方法:new / on_input(更新 last_input,State=Idle 当空词)/ on_sort_change(改 sort 并触发)
// pending_at(now):窗口到期且有待发 → Fire(gen+=1, State=Searching) 否则 None
// accept_response(gen) -> bool;mark_response_error(code);mark_total_zero();set_state...
```

（具体实现按测试语义补全;`SortKey` 若直接复用 engine 类型则 app 依赖 engine——经 client 承载层已允许,报告说明选择。）

- [ ] **Step 3: 跑通过 + `<check>` + 提交**

```bash
git add crates/app/src/session.rs
git commit -m "feat(app): 查询会话——防抖/代际号/六态状态机(纯逻辑 TDD)"
```

---

### Task 12: 悬浮条 UI(输入+列表+角标+键位)

**Files:**
- Create: `crates/app/src/ui/palette.rs`
- Modify: `crates/app/src/ui/mod.rs`(接线 session → 绘制 → 响应)

**Why:** product §1——核心交互面;本任务先做「可见可搜可开」,平台行为(Task 14)与热键(Task 15)随后接。

- [ ] **Step 1: 绘制与键位**

```rust
// 关键结构与行为(完整实现按此规格):
pub struct PaletteState {
    pub input: String,
    pub results: Vec<HitEntry>,       // protocol::HitEntry
    pub selected: usize,
    pub total: u32,
    pub status_note: Option<String>,  // 角标文案(§7 映射:建索引中/补齐中/离线…)
    pub visible_rows: usize,          // 初始 20,滚到底 +20(至 K=results.len())
}

/// 绘制:圆角面板(egui::Frame radius+fill 按深浅色)+ TextEdit(自动聚焦)+ 列表(名称加粗/目录灰小字)。
/// 键位(TextEdit 不抢占时):↑↓ 移动(滚动保持可见)、↵ 打开选中(默认应用;目录=访达进入)、⌘↵ 访达显示、
/// ⌥↵ 事件发给上层(打开完整窗口并带 input)、Esc → 隐藏请求。
/// 打开动作:std::process::Command::new("open").arg(path).spawn()(文件);
///   目录经访达:open -R 是 reveal;目录「进入」= open <dir>。
/// 打开后:发 HideRequest 事件(悬浮条自动隐藏,product §1)。
```

- [ ] **Step 2: 输入中文验证(输入法 composition)**:中文输入法打字——**组合输入中回车不得触发打开**(egui TextEdit 的 IME 处理:`Key::Enter` 在 composition 期间忽略;若 egui 已正确处理则以实测为准,发现问题记 PoC 报告并在此任务修)。

- [ ] **Step 3: 隔离环境实机走查**:起 App(MCE_ROOTS 隔离)→ 悬浮条输入 → 结果出现(<150ms 手感)→ ↑↓/↵/⌘↵/Esc 逐键验证;`open` 打开的文件存在。

- [ ] **Step 4: `<check>` + 提交**

```bash
git add crates/app/src/ui
git commit -m "feat(app): 悬浮条 UI——输入即搜/列表/键位/打开动作"
```

---

### Task 13: 中文字体 + 深浅色主题

**Files:**
- Modify: `crates/app/src/ui/mod.rs`(字体加载 + 主题)
- Create: `crates/app/src/ui/theme.rs`

**Why:** egui 默认字体无 CJK(豆腐块);深浅色跟随系统是 v1 非目标里的「v2 做」。

- [ ] **Step 1: 字体加载(系统字体路径探测)**

```rust
/// 依次探测系统 CJK 字体(存在即用):/System/Library/Fonts/PingFang.ttc、
/// /System/Library/Fonts/STHeiti Light.ttc、用户目录思源等;失败 → 界面提示并回退默认(如实降级)。
/// 加载:FontDefinitions 注册为 Propo 与 Monospace 的 fallback 家族。
```

- [ ] **Step 2: 深浅色(自建探测——PoC-1 结论:winit 的 KVO 主题监听随 NSPanel 改类失效)**:每渲染帧(或低频节流)读系统外观经 objc2(`NSApp.effectiveAppearance.name` ← `NSAppearanceNameDarkAqua` 判定,或 `defaults read -g AppleInterfaceStyle` 子进程——**用 objc2 同进程查询,避免子进程**)→ 外观变化时 `ctx.set_visuals(Light/Dark)` 并重绘。

- [ ] **Step 3: 实机验证**:中文文件名列表无豆腐块;系统外观切换(设置或 `osascript` 模拟)→ 两窗口即时变色。

- [ ] **Step 4: `<check>` + 提交**

```bash
git add crates/app/src/ui
git commit -m "feat(app): 中文字体加载 + 深浅色跟随主题"
```

---

## 阶段 D:平台集成(悬浮条行为 / 热键 / 托盘)

### Task 14: 悬浮条平台行为产品化(PoC-1 成果落地)

**Files:**
- Create: `crates/app/src/platform/mac.rs`(objc2;unsafe 集中,SAFETY 逐处)
- Create: `crates/app/src/platform/mod.rs`(facade:非 macOS 空实现)
- Modify: `crates/app/src/ui/mod.rs`(唤起/隐藏流程接平台层)

**Why:** tech 平台行为表——跨 Space/浮动级别(**)PoC-1 续 spike 定稿的 NSPanel 改类五要素+两补丁产品化**,实现参考直接读 `scripts/poc/poc1b_nspanel/src/main.rs`(五要素:自定义子类覆写 canBecomeKeyWindow/NonactivatingPanel styleMask/Floating level/CanJoinAllSpaces|FullScreenAuxiliary/orderFrontRegardless;两必带补丁:makeFirstResponder 修键盘链路、**关窗前还原窗口类防 winit drop abort**))、失焦隐藏、多屏定位。

- [ ] **Step 1: 平台 API(facade)**

```rust
// platform/mod.rs
pub fn patch_palette_window(eframe_window: &eframe::Frame) -> anyhow::Result<()>;  // 跨 Space+浮动级别+activate 策略
pub fn mouse_screen_position() -> Option<(f64, f64)>;      // 鼠标所在屏 + 位置(NSEvent.mouseLocation)
pub fn screen_center_top(size: (f32, f32)) -> (f32, f32);  // 计算悬浮条坐标(屏可见区上 1/4)（已被 Task 11 取代：顶边固定 110）
pub fn activate_app();                                     // LSUIElement 键盘焦点(点击/唤起时)
```

mac.rs 实现:从 eframe Frame 取 window handle → NSWindow → `setLevel`/`setCollectionBehavior`(值抄 PoC-1 结论);`mouseLocation` + `NSScreen.screens` 计算目标屏;`NSApp.activateIgnoringOtherApps(true)`(唤起前置,保证输入焦点)。

- [ ] **Step 2: 唤起/隐藏流程**:`show_palette()` = 计算位置 → `ViewportCommand::OuterPosition(pos)` → `Visible(true)` → `activate_app()`(顺序敏感:先定位再显示,避免跳屏)。失焦隐藏:`update` 中检测悬浮条视口 `ctx.input(|i| i.viewport().focused)` 为 Some(false) → 隐藏。

- [ ] **Step 3: 实机验证(真实环境)**:跨 Space(F1 或开发入口唤起→切 Space 仍在)、全屏 App 上方、多屏(移动鼠标到副屏唤起→出现在副屏)、失焦隐藏、键盘输入焦点正常。

- [ ] **Step 4: `<check>` + 提交**

```bash
git add crates/app/src/platform crates/app/src/ui
git commit -m "feat(app): 悬浮条平台行为——跨 Space/浮动级别/失焦隐藏/多屏定位(unsafe 集中 platform/mac.rs)"
```

---

### Task 15: 全局热键接线(⌥Space + 备选)

**Files:**
- Create: `crates/app/src/hotkey.rs`
- Modify: `crates/app/src/main.rs`(注册与事件泵)

**Why:** product §1/PoC-2 结论——⌥Space 唤起/隐藏切换;失败走备选位与提示。

- [ ] **Step 1: 实现**

```rust
/// 按 PoC-2 结论选默认键(⌥Space 可用则用,否则按 PoC 实测的备选顺序);
/// 注册失败:返回 Err(候选备注),UI 弹持久提示条(「⌥Space 被占用,已改用 …」——v2.0 无设置页,
/// 备选由 PoC 时期定下的顺序自动降级;全部失败则提示「快捷键不可用,可从托盘图标唤起」)。
pub fn register_default(manager: &GlobalHotKeyManager) -> anyhow::Result<(HotKey, String)>;
/// 事件泵:GlobalHotKeyEvent::receiver() 在 UI 帧循环中 try_recv;Pressed → toggle palette。
pub fn poll_toggle(rx: &Receiver<GlobalHotKeyEvent>) -> bool;
```

- [ ] **Step 2: 实机**:真实环境 ⌥Space 唤起/再按隐藏/输入法状态下行为(与 PoC-2 一致);Esc/失焦隐藏后热键再唤起正常。

- [ ] **Step 3: `<check>` + 提交**

```bash
git add crates/app/src/hotkey.rs crates/app/src/main.rs
git commit -m "feat(app): 全局热键接线(默认键按 PoC-2 结论,失败降级提示)"
```

---

### Task 16: 托盘(菜单栏图标/菜单/六态)

**Files:**
- Create: `crates/app/src/tray.rs`
- Modify: `crates/app/src/main.rs`(托盘初始化 + 菜单事件)

**Why:** product §3——左键唤起/隐藏(切换)、右键菜单(打开搜索/打开完整窗口/开机自启/退出)、引擎状态六态首项、缺口项。

- [ ] **Step 1: 实现**

```rust
/// tray-icon 初始化:图标(先用内置简单位图,完整图标 asset 后补)+ 菜单:
///   [状态项:引擎:就绪/构建中/补齐中/重扫中/未运行(离线索引)/未运行]  (点击:未运行→启动引擎)
///   (有缺口时)⚠ N 个目录未收录                                        (点击→打开完整窗口+缺口面板)
///   ---
///   打开搜索 / 打开完整窗口 / ☑ 开机自启 / 退出
/// 事件:MenuEvent 经 mpsc 泵到 UI;左键(TrayIconEvent::Click 左键)→ toggle palette。
/// 状态项文案由 §7 映射表生成(单一函数 state_label(state) -> &str,可测)。
```

`state_label` 纯函数 + 单测(六态映射与 product §7 表逐字一致)。

- [ ] **Step 2: 实机**:左键切换、右键菜单各项、(自启项此任务只挂 UI,功能 Task 20 接)、退出只退 App(引擎仍在:`macEverything status` 可查)。

- [ ] **Step 3: `<check>` + 提交**

```bash
git add crates/app/src/tray.rs crates/app/src/main.rs
git commit -m "feat(app): 菜单栏托盘——左键切换/右键菜单/六态状态项/缺口入口"
```

## 阶段 E:完整结果窗口

### Task 17: 完整窗口 UI(表格/虚拟化/列排序/多选)

**Files:**
- Create: `crates/app/src/ui/results.rs`
- Modify: `crates/app/src/ui/mod.rs`

**Why:** product §2——Everything 主窗式浏览面;表格交互是 v2.0 的手感另一半。

- [ ] **Step 1: 结构**

```rust
pub struct ResultsState {
    pub session: session::Session,          // 与悬浮条独立会话(各自防抖/代际)
    pub results: Vec<HitEntry>,
    pub total: u32,
    pub elapsed_ms: f64,
    pub stat_ms: f64,
    pub sort: SortKey,
    pub sort_asc: bool,                     // 点列头切换
    pub selection: BTreeSet<u32>,           // protocol 条目无 id——用行号;选择 = 行号集合
    pub anchor: Option<usize>,              // shift 范围锚点
    pub sorting_inflight: bool,
}

/// 绘制:顶部搜索框 + 自定义虚拟化表格(`ScrollArea::show_rows`;列:名称/大小/修改时间/路径;列宽可拖)+ 底部状态栏。
/// 列头点击:同列切换 asc/desc;异列切换键并重置 asc(默认:名称升序,大小升序,mtime 降序,路径升序——与 plan Task 8 定案一致)。
/// 排序变更 → session.on_sort_change → 防抖窗内合并 → 重查(sorting_inflight=true,状态栏「排序中…」)。
/// 多选:单击(清选+选)、shift+单击(锚点范围)、⌘+单击(切换)、⌘A(全选可见 K 行);⌘ 检测:ctx.input modifiers。
/// 双击行:默认应用打开。
```

- [ ] **Step 2: 实机走查(隔离或真实)**:输入即搜;四列显示正确(大小/时间来自响应);点列头排序切换(升/降/换列);大结果集(如 `.pdf` 数千命中)滚动流畅;多选四式;关闭窗口不退出 App(托盘仍在)。

- [ ] **Step 3: `<check>` + 提交**

```bash
git add crates/app/src/ui
git commit -m "feat(app): 完整结果窗口——表格虚拟化/列排序/多选/状态栏骨架"
```

---

### Task 18: 原生右键菜单桥接 + 菜单动作

**Files:**
- Modify: `crates/app/src/platform/mac.rs`(+ NSMenu 构建/弹出/回调桥接)
- Create: `crates/app/src/ui/menu_actions.rs`(行→动作执行:打开/访达/拷贝)
- Modify: `crates/app/src/ui/results.rs`(右键触发)

**Why:** product §2 菜单四语义(按选中类型收敛)+ 产品要求原生菜单;PoC-3 已验证弹窗可行,本任务产品化。

- [ ] **Step 1: 菜单构建(按选中类型收敛,规格直抄 product 表)**

```rust
pub enum MenuAction { Open, OpenInFinderDir, RevealInFinder, CopyPaths }

/// 菜单项集合由选中行类型决定(纯逻辑可测):
pub fn menu_items(files: usize, dirs: usize) -> Vec<(&'static str, MenuAction)> {
    let mut v = vec![("打开", MenuAction::Open)];
    if files > 0 {
        v.push(("在访达中打开", MenuAction::OpenInFinderDir));  // 文件:打开所在目录
    }
    v.push(("在访达中显示", MenuAction::RevealInFinder));
    v.push(("拷贝路径", MenuAction::CopyPaths));
    v
}
// 测试:纯文件三(或四)项、纯目录三项(无「在访达中打开」)、混选四项——与 product 表逐行一致
```

- [ ] **Step 2: NSMenu 桥接(unsafe 集中 mac.rs)**:构建 → 弹出(PoC-3 验证过的方式)→ target/action 回调经 channel 送 UI 线程 → 执行动作。**回调阻塞注意**:弹出是模态,回调执行时机在关闭后;经 channel 回 UI 线程执行(不阻塞 runloop)。

- [ ] **Step 3: 动作执行**

```rust
/// Open:多选逐项 Command::new("open").arg(path)(后台 spawn);
/// OpenInFinderDir:open -R 的父目录?→ 打开所在目录不选中:Command::new("open").arg(parent);
/// RevealInFinder:open -R <path>;
/// CopyPaths:路径逐行 join("\n") → pbcopy(经 stdin,不走 shell 拼接——用 Command + stdin write)。
```

- [ ] **Step 4: 实机**:右键三类选中(文件/目录/混选)菜单项完全正确;四项动作逐项验证(访达定位/目录打开/剪贴板内容);连续操作无卡死。

- [ ] **Step 5: `<check>` + 提交**

```bash
git add crates/app/src/platform crates/app/src/ui
git commit -m "feat(app): 原生右键菜单桥接 + 四动作(按选中类型收敛)"
```

---

### Task 19: 状态栏补全(缺口面板 + sort_echo 提示 + 排序中)

**Files:**
- Modify: `crates/app/src/ui/results.rs`
- Modify: `crates/app/src/ui/palette.rs`(角标六态接线)

**Why:** 阻塞 3 的验收——状态呈现六态全出口;tech「sort_echo 防静默错排序」;tech 会话状态 Warming/Offline 可见。

- [ ] **Step 1: 实现**

```rust
/// 状态栏三段:N 项 · 匹配 X ms · 读取 Y ms | 索引状态(§7 映射) | 缺口徽标(有则显示)
/// 缺口徽标:点击 → 展开缺口面板(egui 折叠区:目录 + 原因列表,数据来自 status 响应 gaps)。
/// sort_echo 校验:响应的 sort_echo 与请求不符(空串=旧 daemon)→ 状态栏红字
///   「引擎版本过旧,排序未生效 [重启引擎]」——重启引擎按钮:经随包 CLI stop→start(engine_launcher 复用)。
/// 排序中:sorting_inflight=true 期间状态栏「排序中…」。
```

- [ ] **Step 2: 实机**:构造缺口(临时不可读目录→status 有 gaps→完整窗口徽标出现→展开清单与 CLI status 一致);Warming/Offline 状态文案出现;（sort_echo 旧 daemon 场景难以实机复现——单测覆盖回显不符分支）。

- [ ] **Step 3: `<check>` + 提交**

```bash
git add crates/app/src/ui
git commit -m "feat(app): 状态栏补全——缺口面板/sort_echo 提示与重启动作/排序中提示"
```

---

## 阶段 F:交付与验收

### Task 20: 开机自启接线 + bundle.sh 打包

**Files:**
- Create: `scripts/bundle.sh`
- Create/Modify: `crates/app/src/autostart.rs`(SMAppService 或 LaunchAgent,按 PoC-5 结论)
- Modify: `crates/app/src/tray.rs`(自启勾选项接线)

**Why:** product §4 交付形态——.app 组装(LSUIElement、CFBundleExecutable=macEverything-app、随包 CLI)+ 自启默认开。

- [ ] **Step 1: bundle.sh(照 PoC-5 的 make-mini-app 扩为正式版)**

```bash
#!/usr/bin/env bash
# 组装 macEverything.app:cargo build --release → 拷双二进制 → 生成 Info.plist(版本取自 Cargo.toml)。
# 结构:Contents/MacOS/macEverything-app(App) + Contents/MacOS/macEverything(CLI)+ Info.plist(LSUIElement)。
# 输出:release/macEverything.app(Task 21 验收用)。
set -euo pipefail
cargo build --release
APP=release/macEverything.app
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS"
cp target/release/macEverything-app "$APP/Contents/MacOS/"
cp target/release/macEverything     "$APP/Contents/MacOS/"
VERSION=$(grep '^version' Cargo.toml | head -1 | cut -d'"' -f2)
cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleExecutable</key><string>macEverything-app</string>
  <key>CFBundleIdentifier</key><string>com.example.maceverything</string>
  <key>CFBundleName</key><string>macEverything</string>
  <key>CFBundleShortVersionString</key><string>${VERSION}</string>
  <key>LSUIElement</key><true/>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
</dict></plist>
EOF
codesign --force --deep -s - "$APP"   # ad-hoc 签名
echo "打包完成: $APP"
```

- [ ] **Step 2: 定位顺序实机三验(product 验收)**:① 从 .app 启动 → 引擎定位命中随包 CLI(停掉引擎后 App 拉起成功,日志/行为验证);② 临时改名随包 CLI → 回退 PATH;③ PATH 移除 → 错误文案列出全部搜过路径。

- [ ] **Step 3: 自启接线**:autostart.rs 按 PoC-5 结论实现注册/注销/查询;托盘勾选项接线;启动时默认注册(产品定案)。

- [ ] **Step 4: `<check>` + 提交**

```bash
git add scripts/bundle.sh crates/app/src/autostart.rs crates/app/src/tray.rs
git commit -m "feat(release): bundle.sh 打包 + 开机自启接线(按 PoC-5 结论)"
```

---

### Task 21: 终验收(全链路 + 性能四项 + 真实重启)

**Files:**
- Modify: `docs/specs/v2-ui/poc-results.md`(验收记录追加)

**Why:** product「验收标准」逐条 + 性能四项红线的最终复测,全部在**真实环境**(约 120 万条目)。

- [ ] **Step 1: 全链路实机走查**(product 功能验收 13 条逐项打勾,记录到 poc-results.md)

覆盖:唤起/键位/打开隐藏/完整窗口四列排序/多选批量/原生菜单四语义/托盘/六态状态/缺口出口/⌥⇧F 直达/app.sock 二次启动/深浅色。

- [ ] **Step 2: 性能四项实测**

```bash
# ① 唤起延迟:热键按下(脚本发键或人工秒表)→ 可见 <100ms(PoC-4 的测量法)
# ② 输入到结果 <150ms:输入 5 个字符,结果出现耗时(录屏逐帧或打点)
# ③ 内存:`top -pid <app pid> -l 5` 的 MEM 列——<100MB
# ④ 隐藏 CPU:悬浮条+完整窗口全隐藏 60s,top CPU 恒 0.0%
```

- [ ] **Step 3: 服务校验(真实验收)**:改一个文件→悬浮条 1s 内可搜(增量);`stop` 引擎→App 自动拉起重现;**真实重启系统一次**→ 登录后 App 自动起、⌥Space 可搜(自启 + 自动拉起闭环)。

- [ ] **Step 4: 记录结果到 poc-results.md(逐项通过/偏差)+ 提交**

```bash
git add docs/specs/v2-ui/poc-results.md
git commit -m "docs: v2.0 终验收记录(全链路 + 性能四项 + 真实重启)"
```

---

### Task 22: 文档回填(README/usage/architecture + spec 状态)

**Files:**
- Modify: `README.md`(v2.0 特性/安装改为 .app 优先+bundle.sh、用法加 GUI)
- Modify: `docs/usage.md`(GUI 使用节:悬浮条键位/完整窗口/托盘/自启;CLI 节保留)
- Modify: `docs/architecture.md`(ADR-7/8 摘要 + app 层结构 + 平台补丁说明)
- Modify: `docs/specs/v2-ui/{product,tech}.md`(Status 回填;实现偏差清单如有)

**Why:** v1 惯例——实测回填,文档与实现零漂移。

- [ ] **Step 1: 四份文档更新**(内容按 Task 21 验收结果与实现偏差如实写;偏差清单照 v1 格式列)
- [ ] **Step 2: 自检命令链 + 提交**

```bash
<check>
git add README.md docs/usage.md docs/architecture.md docs/specs/v2-ui
git commit -m "docs: v2.0 文档回填——README/usage/architecture/GUI 用法与偏差清单"
```

---

## 覆盖矩阵

| product.md 章节 / 验收 | 对应 Task |
|---|---|
| §1 悬浮条(键位/分页/自动隐藏/失焦) | Task 12, 14, 15 |
| §2 完整窗口(列/排序/多选/原生菜单/状态栏/缺口) | Task 17, 18, 19 |
| §3 托盘(左键/右键/六态/缺口项) | Task 16 |
| §4 开机自启 | Task 5(PoC), 20 |
| §5 进程编排(app.sock/定位三档/退出语义) | Task 10, 20 |
| §6 外观(深浅色) | Task 13 |
| §7 状态呈现六态映射 | Task 11, 16, 19 |
| §8 引擎排序 | Task 8, 8b |
| 交付形态(.app/随包 CLI/定位顺序) | Task 20 |
| 验收-功能 13 条 | Task 21 |
| 验收-性能 4 条 | Task 4(PoC), 21 |
| 验收-体验 3 条 | Task 14, 21 |
| PoC-1..5 | Task 1-5 |
| ADR-7(protocol/client 迁移) | Task 6, 7 |
| ADR-8(eframe + objc2) | Task 1, 9, 14, 18 |
| 协议 sort/sort_echo | Task 8b |
| v1 回归(91 项) | Task 6, 7(迁移后全绿), 21 |

## plan 自检清单

- [x] 无 placeholder:PoC 任务给了最小代码 + 实机判据(探索类给骨架属合理);产品任务给了结构规格 + 关键实现;API 细节处标注「以实锁文档为准」(Doubt-Driven 纪律,非 placeholder)
- [x] product 每节/每验收条目都有对应 Task(覆盖矩阵)
- [x] 类型一致:SortKey/SortOutcome/execute_sorted 签名(8/8b)、HitEntry(protocol 迁移后 8b/12/17 引用)、DaemonPaths(client 迁移后 10/20 引用)、state 六态(11/16/19 同表)跨任务对齐
- [x] 提交纪律:每任务精确 add;并行会话安全
- [x] PoC 先行:5 个平台赌注全部在实现前验证,降级路径已在 product 边界表/tech 风险表登记


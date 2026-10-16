# macEverything v2.0 图形界面 — PoC 验证结果

> 记录阶段 A 五个 PoC 的实机验证结果，Task 2~5 完成后在本文件追加。
> 验收环境：macOS 27.0（Apple Silicon、arm64）、双 4K 显示器（主屏 x∈[0,1920)，副屏 x∈[1920,3840)，各 1920x1080 逻辑点）。
> 截图证据为本机 `/tmp/poc1-*.png`（重启后可能被系统清理）；窗口状态用 `CGWindowList` 清单（layer / onscreen / pid / bounds）取证。

---

## Task 1：PoC-1 — objc2 跨 Space + 浮动级别

**状态：BLOCKED → 已由文末「PoC-1 续：NSPanel 改类 spike」解除（2026-10-16：改类 NSPanel 后跨全屏 Space 通过）**（原始结论：跨 Space 在「他人全屏应用新建的 Space」场景不成立；「多桌面 Space」场景待人工三指滑动一测后定论。）

- **日期**：2026-10-16
- **实锁版本**：eframe / egui 0.37.0、objc2 0.6.5、objc2-app-kit 0.3.2、raw-window-handle 0.6.2（plan 示意的 eframe 0.29 已过时，API 变化见文末）
- **代码**：`scripts/poc/poc1_window/`（独立 crate，根 Cargo.toml `[workspace] exclude` 已追加）

### 结论表

| # | 验证项 | 结论 | 判定依据 |
| --- | --- | --- | --- |
| 0 | eframe 取 NSWindow + objc2 补丁施加 | ✅ 通过 | 主路径 `cc.window_handle() → NSView::window()` 取窗成功（回退路径已实现但未触发）；补丁后回读 `level=3`、`CanJoinAllSpaces=true`、`FullScreenAuxiliary=true`；每 500ms 核对漂移，长时运行漂移计数 0 |
| 1 | 跨 Space 可见（他人全屏应用新建的 Space） | ❌ 失败 | 全屏 Space 激活期间本窗不在屏（`CGWindowList` 无本窗条目、截图无本窗）；退出全屏后立即回屏。四种组合（Floating / Floating+Accessory / Status(25) / 无补丁对照组）全为「不在屏」 |
| 1b | 跨 Space 可见（多桌面 Space 间切换） | ⚠️ 未验证 | 本机每屏仅 1 个桌面 Space + 若干全屏 Space；三指滑动/快捷键注入无权限（System Events 未授权），需辉哥手动一测 |
| 2 | 浮动级别（浮在普通窗口 / 铺满屏幕窗口之上） | ✅ 通过 | 截图本窗压在所有普通窗口最前（其他应用主窗、Chrome、Edge、Finder 均被压住）；清单佐证本窗 `layer=3`，与铺满主屏的 `layer=0` 窗口同时 onscreen（层级更低故在其上）。对「他人全屏 Space」不成立（见 #1，同一根因） |
| 3 | 无标题栏 + 黑底半透明 | ✅ 通过 | 截图无标题栏/圆角装饰；像素取证：窗内 (30,32,35) 对窗下浅色内容、窗内 (35,33,32) 对窗下暖色横幅——回算 ≈0.137×底层色，即 alpha 220/255 的真半透明合成，非不透明黑 |
| 4 | 基本键盘焦点 | ✅ 通过（人工） | 辉哥实机按 F1/F2/L/ESC 均被本窗接收并生效（见日志） |
| 5 | 隐藏 / 自动再显示（`ViewportCommand::Visible` 双向） | ✅ 通过 | F1 隐藏后 3 秒自动 `Visible(true)` 回屏（后台线程定时；与产品里全局热键唤醒同构）；隐藏期 eframe 不跑 `ui`，逻辑走 `logic` 补跑 |

### 现象细节

1. **补丁施加（通过）**：启动日志 `补丁已施：取窗路径 = cc.window_handle() → NSView::window()；回读 level=3，behavior=CanJoinAllSpaces:true FullScreenAuxiliary:true`。窗口清单佐证 `layer=3 onscreen=true bounds=(200,200 600x400)`（`/tmp/poc1-16-d1-baseline.png`、`/tmp/poc1-03-cjk.png`）。
2. **跨 Space 失败（核心发现）**：把窗口放到主屏 (200,200)，随后让另一窗口进入系统全屏（macOS 原生全屏会新建 Space，`com.apple.spaces` 里 space 数 5→6 佐证）。全屏 Space 激活期间：
   - 窗口清单里**没有**本窗条目（同屏的全屏窗 `layer=0 onscreen=true bounds=(0,30 1920x1050)` 在列）；
   - 截图只见全屏窗，不见本窗（`/tmp/poc1-22-patched-fs.png`）；
   - 退出全屏后本窗立即回屏（清单 `layer=3 onscreen=true bounds=(200,200 …)`）。
   四种组合全数失败：同进程全屏窗、跨进程全屏窗、`POC1_ACCESSORY`（Accessory 激活策略）、`POC1_LEVEL=status`（25 档）。
3. **浮动级别（通过）**：本窗 layer=3/25 恒压普通窗口之上；12:49 实测截图本窗位于全屏铺满的 Chrome 页与 其他应用主窗之上（`/tmp/poc1-21-d1-float-over-cover.png`）。
4. **透明 + 无边框（通过）**：像素取证见结论表 #3；窗口尺寸 600x400 逻辑点与设定一致。
5. **隐藏/再显示（通过）**：日志 `隐藏（Visible(false)）…` → 3 秒后 `隐藏后自动再显示（Visible(true)）`；隐藏期窗口收不到键盘事件，用后台线程（`egui::Context` 是 Send+Sync，跨线程发 `ViewportCommand`）驱动再显示。
6. **补丁漂移**：长时运行 0 次漂移（level 与 collectionBehavior 均未被 winit/系统改写）。

### 根因与外证

- **实测差分矩阵**（全部已施补丁，回读行为值均正确）：
  | 窗口级别 | 激活策略 | 全员全屏 Space 内可见？ |
  | --- | --- | --- |
  | Floating(3) | Regular | ❌ |
  | Floating(3) | Accessory | ❌ |
  | Status(25) | Regular | ❌ |
  | 无补丁对照组 | Regular | ❌（预期失败） |
- **外证**：tauri-apps/tauri#11488（2024-10，macOS 15，close as not planned）——普通 `NSWindow` 的 `visibleOnAllWorkspaces` 无法覆盖他人全屏窗；社区结论是需把窗口改为 **NSPanel**（tauri-nspanel 路线，运行期改窗口类）或改应用激活策略；本机 macOS 27 实测「Accessory 策略」已不足以救活该路线。
- **机制推断**：macOS 的全屏 Space 只允许 NSPanel 类窗口加入；普通 NSWindow 即使带 `CanJoinAllSpaces | FullScreenAuxiliary` 也被排除（本机实测与外部证据一致）。
- **winit 已自带 Floating 档**：winit 0.30.13 `always_on_top` 走 `set_window_level(kCGFloatingWindowLevel=3)`（源码 `platform_impl/macos/window_delegate.rs:1533`）。即「浮动级别」的 Floating 档由 eframe 官方 API 即可覆盖，objc2 补丁在级别维度只在 Status 档有增量；补丁的真正增量是 collectionBehavior。

### 降级决策（待辉哥拍板，未擅自改 spec）

触发 tech 风险表预置的降级路径「当前 Space 可见，产品文档标注」。但本机实测显示该降级会削弱产品目标 7 的部分场景（用户切到任意全屏应用的 Space 时悬浮条不可见）。可选后续路线：

1. **运行期把 eframe 窗口改类为 NSPanel**（tauri-nspanel 同法：`object_setClass` 换类 + 非激活面板样式）——收益最大，但属非公开路径，需在 Task 9/14 前单独 spike 验证与 winit 的兼容性；
2. 原生 NSPanel 壳 + egui 渲染（ADR-8 已 Rejected：集成成本高）；
3. 接受降级「当前 Space 可见」。

补充：桌面多 Space 场景（1b）尚未定论，建议辉哥做一次 10 秒人工验证：`POC1_POS=200,200 ./target/debug/poc1-window` 启动后三指左右滑动切桌面 Space，观察窗口是否跟随（这是「跨 Space」最日常的用法；若该场景通过，产品可用性显著好于 #1 的结论）。

### 复现步骤

```bash
cd scripts/poc/poc1_window
# 本机 crates.io sparse index 极慢（实测 ~14KB/s），拉新依赖走 rsproxy：
cargo build --config 'source.crates-io.replace-with="rsproxy"' \
            --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'
POC1_POS=200,200 ./target/debug/poc1-window            # 常规运行（点窗口后 F1/F2/L/ESC）
POC1_FS_AUTO=1 ./target/debug/poc1-window              # 自动：3s 开全屏测试窗、15s 退全屏、18s 关窗
POC1_ACCESSORY=1 ...                                   # 附加 Accessory 激活策略（无 Dock 图标）
POC1_NO_PATCH=1 ...                                    # 对照组：跳过 objc2 补丁
POC1_LEVEL=status ...                                  # 起始用 Status(25) 档
```

观察方式：`screencapture -x -D 1 /tmp/poc1-*.png` 截图；窗口状态用 CoreGraphics `CGWindowListCopyWindowInfo(.optionOnScreenOnly)`（本机脚本 `/tmp/winlist.swift`，`layer/onscreen/pid/bounds` 四列）；Space 清单用 `defaults export com.apple.spaces`（`type=4` 即全屏 Space）。

### 与 plan 示意的接线差异（供 Task 9/14 产品化参考）

1. eframe 0.37 的 `App` trait 是 `logic(&mut self, &egui::Context, &mut Frame)` + `ui(&mut self, &mut egui::Ui, &mut Frame)`，**没有 `update`**（plan 示意代码是 0.29 写法）；窗口隐藏时 eframe 不跑 `ui`，只补跑 `logic`。
2. `egui::CentralPanel::show(&mut Ui, …)` 新签名；`egui::Frame::new()`（`Frame::none()` 已不存在）；`Context::wants_keyboard_input` 改名 `egui_wants_keyboard_input`；`ViewportCommand::{Visible, Close, Fullscreen}` 均存在。
3. 中文文本需 eframe `system_fonts` feature（仅 `default_fonts` 时 CJK 渲染为方框）；本 PoC 已加该 feature。
4. `CreationContext` 实现 `HasWindowHandle`（raw-window-handle 0.6）：`cc.window_handle()` → `RawWindowHandle::AppKit(ns_view)` → `NSView::window()` 取 NSWindow。plan Step 2 主路径成立；`NSApplication::sharedApplication(mtm).windows()` 回退路径已实现但未触发。
5. `NSWindowLevel` 是 `NSInteger` 类型别名，常量为静态量 `NSFloatingWindowLevel`（3）/ `NSStatusWindowLevel`（25），不是 `NSWindowLevel::Floating` 枚举；`NSWindowCollectionBehavior` 是 bitflags，`CanJoinAllSpaces | FullScreenAuxiliary` 可直接 `|` 组合。
6. macOS 原生全屏由 winit 的 `toggleFullScreen:` 实现（`WindowEvent` 侧用 `ViewportCommand::Fullscreen(bool)` 即可，新 Space 自动新建），本 PoC 的自动测试窗用的正是这条路径。

---

## PoC-1 续：NSPanel 改类 spike（poc1b_npanel）

**状态：✅ 通过**——运行期 `object_setClass` 把窗口改类为 NSPanel 后，悬浮条能在「他人全屏应用新建的 Space」里显示，且可交互、可退出。（Task 1 的 BLOCKED 解除路径成立。）

- **日期**：2026-10-16
- **实锁版本**：eframe / egui 0.37.0、objc2 0.6.5、objc2-app-kit 0.3.2、objc2-foundation 0.3.2、raw-window-handle 0.6.2、winit 0.30.13（winit 内部用 objc2 0.5.3，与本次 0.6.5 并存互不干扰）
- **代码**：`scripts/poc/poc1b_nspanel/`（独立 crate，根 Cargo.toml `[workspace] exclude` 已追加；含验证脚本 `verify.sh`）
- **路线来源（外证，非推断）**：tauri-nspanel（ahkohd/tauri-nspanel，同为 winit 底座）——`examples/fullscreen` README 原文「The panel must be non-activating to display over fullscreen windows」；本 spike 逐项复刻其组合，并给每项加差分开关实测必要性。

### 结论表

| # | 验证项 | 结论 | 判定依据 |
| --- | --- | --- | --- |
| 1 | 跨 Space 可见（他人全屏应用新建的 Space） | ✅ 通过 | 全屏 Space 激活期触发 show → `CGWindowList(.optionOnScreenOnly)` 出现本面板 `layer=3 onscreen=true bounds=(200,200 600x400)`，同屏并列他人全屏窗 `layer=0 …(0,30 1920x1050)`；截图双取证（`/tmp/poc1b-space-subclass-shown.png`：面板压在全屏窗内容之上，像素取证面板区均值 (38,38,38) 对全屏窗白底 (248,248,248)） |
| 2 | 对照组：不改类（复现 PoC-1） | ❌ 失败（预期） | 同流程 show 后 onscreen 清单无本面板；全清单（`.optionAll`）里本窗 `layer=3 onscreen=false`——窗口存在但在别的 Space（用户按热键「像没反应」）。像素取证：面板区均值 = 背景均值 (248,248,248) |
| 3 | 差分：非激活样式位是否必需 | **必需** | `POC1B_NONACTIVATING=0`（其余同）→ 面板不在全屏 Space（清单无、回归失败形态）；带 `NSWindowStyleMaskNonactivatingPanel`（styleMask 0xc→0x8c）才可见 |
| 4 | 差分：裸 NSPanel vs 自定义子类 | 跨 Space 两者皆可；**输入只有子类行** | `POC1B_MODE=plain`（直接换 `NSPanel`）：跨 Space ✅，但 `canBecomeKeyWindow=false` → `makeKeyWindow` 后 `isKeyWindow=false`，合成按键收不到（TextEdit 0 次）；自定义子类（覆写 `canBecomeKeyWindow`/`canBecomeMainWindow` → true，复刻 winit `WinitWindow` 原值）→ 输入 ✅ |
| 5 | 显示路径差分 | 两条皆可 | `orderFrontRegardless`（tauri-nspanel 原法）与 winit `ViewportCommand::Visible(true)`（`makeKeyAndOrderFront`）均能让面板出现在全屏 Space；两者显示后他人全屏窗仍在 onscreen（未切 Space） |
| 6 | 不抢激活（非激活面板语义） | ✅ 通过 | show（orderFrontRegardless + `makeKeyWindow` → `isKeyWindow=true`）后 `lsappinfo front` 仍是他人全屏 app（`poc1b-fs-actor`）；即在他人 Space 内可成为 key window 而不夺前台 |
| 7 | 输入（改类后） | ✅ 通过（合成按键；真实键盘待人工一测） | 进程内 `NSApp.postEvent` 合成 p/o/c → egui 收到 `Key(P)/Text(p)…` → TextEdit 落到 "poc"（`mode=subclass` 与 `mode=none` 两例一致；`plain` 例收不到，见 #4）。**前提**：改 styleMask 后必须补 `makeFirstResponder(contentView)`，否则合成按键收不到（实测；真实按键同一投递链，winit 源码注释同证，见「必须的属性组合」） |
| 8 | 隐藏/再显示双向 | ✅ 通过 | hide（`orderOut`）后清单转空；再 show 后清单回（全屏 Space 内往返皆成立） |
| 9 | 改类后他人全屏窗仍在原 Space（不扰动） | ✅ 通过 | 每次取证时他人全屏窗 `layer=0 onscreen=true (0,30 1920x1050)` 恒在 onscreen 清单 |
| 10 | 「跟随」场景：面板先可见，他人再全屏 | ✅ 通过 | 面板先在桌面 Space 显示，他人进全屏后清单里面板与全屏窗同时 onscreen（截图 `/tmp/poc1b-follow-in-fs.png`） |
| 11 | 退出（ESC / Close） | ⚠️ 需先还原类 | 改类后直接 `ViewportCommand::Close` → 退出即崩（`panic in a function that cannot unwind` → abort，见「崩溃与规避」）；**关窗前 `object_setClass` 还原原类**（tauri-nspanel `to_window` 同法）→ 干净退出 |
| 12 | eframe/winit 常规行为回归（桌面 Space） | ✅ 通过 | 桌面单跑：无边框+透明+置顶渲染正常、`layer=3`、hide/show 正常、漂移计数 0（level/collectionBehavior/styleMask 均未被 winit 改写）；TextEdit 可输入见 input-diff（#7） |

### 必须的属性组合（生效配方，缺一不可）

```text
1. 自定义类：NSPanel 子类（objc2 define_class! #[unsafe(super(NSPanel))] + #[thread_kind = MainThreadOnly]），
   覆写 canBecomeKeyWindow → true、canBecomeMainWindow → true（复刻 winit WinitWindow 原值，保证键盘可达）
   → object_setClass(window, Poc1bPanel)   // 实例大小实测 528==528，满足换类前提
2. setLevel(Floating(3))                       // 沿用 PoC-1
3. setStyleMask(原 mask | NonactivatingPanel)  ← 跨全屏的硬条件（实测缺此项即不在全屏 Space）
   ★ 随后必须 makeFirstResponder(contentView)：改 styleMask 会打断键盘投递
4. setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)   // 沿用 PoC-1
5. setFloatingPanel(true) / setBecomesKeyOnlyIfNeeded(false) / setHidesOnDeactivate(false)
6. 显示用 orderFrontRegardless（不激活、不切 Space）；winit Visible(true) 亦可
7. 时序：窗口创建后、显示前改类（本 spike 在 eframe 首个 Creator 闭包内，与 tauri setup 时序同构）
```

**两条实测踩坑（产品化时必带）**：

1. **改 styleMask 必须补 `makeFirstResponder(contentView)`**——winit 自己的 `WindowDelegate::set_style_mask` 带同一修法，源码注释：「If we don't do this, key handling will break (at least until the window is clicked again/etc.)」。实测：不补则合成按键/真实按键都进不了窗口（本 spike 第一版正是此症状）。
2. **关窗前必须把类还原**——见下。

### 崩溃与规避（改类后 Close 崩溃）

- **现象**：改类后走 `ViewportCommand::Close` 退出 → `panic in a function that cannot unwind` → `Abort trap: 6`。
- **定位**（backtrace）：`winit Window drop → Retained<WindowDelegate>::drop → objc_release → objc2(0.5.3) declared_ivars::dealloc::<WindowDelegate> → panic`，即**窗口析构路径**触发（delegate 析构里含 `removeObserver:forKeyPath:"effectiveAppearance"` 等窗口交互）。
- **规避**：关窗前 `object_setClass` 还原为改类前的类（tauri-nspanel `close_panel` → `to_window()` 同法）。三方对照实测：`subclass+还原` 干净退出 ✅；`subclass+不还原` 崩溃 ❌；`不改类` 干净退出 ✅（`POC1B_REVERT_ON_QUIT=0` 可复现崩溃）。

### 已知副作用：winit 的主题观察（KVO）失效

- 实测对象的换类前类是 **`NSKVONotifying_WinitWindow`**（不是 `WinitWindow`）：winit 为监听主题变化对窗口做了 KVO。
- winit 0.30.13 `window_delegate.rs` 对窗口 `addObserver:forKeyPath:"effectiveAppearance"`（触发 `WindowEvent::ThemeChanged`）。换类后 `NSKVONotifying_*` 子类被移除，**该 KVO 通知链路是否仍生效存疑**（未实测：需切系统深/浅色观察 egui 是否跟随）。
- 产品化建议：改类后自建主题探测（`NSDistributedNotificationCenter` 的 `AppleInterfaceThemeChangedNotification`，或轮询 `NSApp.effectiveAppearance`），不要依赖 winit 的 ThemeChanged。

### 降级体感（对照组实测，供权衡）

不改类时（复现 PoC-1）：他人全屏 Space 激活期触发 show，**窗口被 orderFront 到自己的 Space**——`.optionAll` 清单里可见本窗 `layer=3 onscreen=false`，当前全屏 Space 里既无清单条目也无截图痕迹。用户体感 = **按热键「像没反应」**（窗口在别处安静地显示了）。即降级「当前 Space 可见」在跨应用全屏场景下是「不可见 + 无反馈」，需产品决定是否补提示。
（本 spike 已证明 NSPanel 改类路线可用，故无需走该降级。）

### 复现步骤

```bash
cd scripts/poc/poc1b_nspanel
# 拉依赖走 rsproxy（本机 crates.io sparse index ~14KB/s）：
cargo build --config 'source.crates-io.replace-with="rsproxy"' \
            --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'
bash verify.sh smoke                        # 桌面单跑：换类 + 渲染 + 清单取证
bash verify.sh input-diff                   # 输入链路三例对照：subclass / plain / none
bash verify.sh space subclass               # 主测（他人全屏 Space，poc1b-fs-actor 充当他人应用）
bash verify.sh space none                   # 对照组（预期失败）
bash verify.sh space plain                  # 裸 NSPanel（跨 Space 可、输入不可）
bash verify.sh space subclass POC1B_NONACTIVATING=0   # 判 NonactivatingPanel 是否必需
bash verify.sh space subclass POC1B_SHOW=winit        # 显示路径差分
bash verify.sh follow                       # 面板先可见 → 他人全屏（跟随场景）
```

- 取证口径：`screencapture -x -D 1 /tmp/poc1b-*.png` 截图 + `/tmp/winlist.swift onscreen|all`（CGWindowList，`verify.sh` 缺脚本时自动重建）；前台应用用 `lsappinfo front`。
- 手动项（留给辉哥）：① 真键盘打字（本 spike 的输入验证走进程内合成事件，未覆盖 HID 路径）；② 系统深/浅色切换下主题跟随（见「已知副作用」）。

---

## Task 2：PoC-2 — ⌥Space 族全局热键注册（中文输入法环境）

**状态：自动化部分完成（三候选注册 3/3 成功 + 事件投递链经合成按键验证通过）；中文/英文输入法态下的真键盘按键实效「人工验证待辉哥执行」（步骤见本节文末，结论表 #4/#5 待回填）。**

- **日期**：2026-10-16
- **实锁版本**：global-hotkey 0.6.4（plan 示意 0.6 系，crates.io 当前最新为 0.8.0，本次锁 0.6.4）、objc2 0.6.5、objc2-app-kit 0.3.2、objc2-foundation 0.3.2；传递依赖 keyboard-types 0.7.0、crossbeam-channel 0.5.17
- **代码**：`scripts/poc/poc2_hotkey/`（独立 crate，根 Cargo.toml `[workspace] exclude` 已追加）

### 结论表

| # | 验证项 | 结论 | 判定依据 |
| --- | --- | --- | --- |
| 1 | 三候选注册（自动化） | ✅ 3/3 成功 | 原始输出见「现象细节」#1；组合↔id：⌥Space=65598、⌥⇧Space=33620030、⌃⌥⇧Space=34144318 |
| 2 | 系统符号热键占用盘点（原始数据） | ⌃Space / ⌃⌥Space / ⌘Space / ⌘⌥Space 占用；三候选空闲 | `defaults read com.apple.symbolichotkeys AppleSymbolicHotKeys`（53 条中涉 Space 的仅四条，均 enabled=1）：#60 输入法上一源=⌃Space、#61 下一源=⌃⌥Space、#64 Spotlight=⌘Space、#65 Spotlight 窗口=⌘⌥Space；三个候选无对应条目 |
| 3 | 事件投递链（自动化；合成按键注入） | ✅ 三候选均送达 | 隔离实验三例见「现象细节」#2；注入轮 ⌥⇧Space + ⌥Space×2 全数到达终端（3/3 打印 pressed） |
| 4 | 中文（拼音）输入法态下真键盘按键 | ⏳ 人工验证待辉哥执行 | 见文末「人工验证步骤」 |
| 5 | 英文输入法态下真键盘按键 | ⏳ 人工验证待辉哥执行 | 同上 |
| 6 | 前台应用/输入法吞键（旁证，未定性） | ⚠️ 观察到 1 例未送达 | 注入轮 A：⌥Space 首发送达失败（当时前台 = WPS Office，活动输入源 = 微信输入法拼音）；注入轮 B 同机同法对照 ⌥Space 2/2 送达。原因待 #4/#5 定性 |

### 现象细节

1. **注册输出（原始，逐字）**：
   ```
   == PoC-2 全局热键注册（global-hotkey 0.6.4 / Carbon RegisterEventHotKey）==
   OK   ⌥Space  (id=65598)
   OK   ⌥⇧Space  (id=33620030)
   OK   ⌃⌥⇧Space  (id=34144318)
   == 注册结果：3/3 成功 ==
   ```
   macOS 后端即 Carbon `RegisterEventHotKey`（成功返回 `noErr`）；失败时 crate 报「Unable to register hotkey: Space」，**不透出 OSStatus 码**。
2. **投递链隔离实验（三例；同机、同注入方式 = CGEvent 于 kCGHIDEventTap 合成 ⌃⌥⇧Space）**：

   | 例 | 进程形态 | 是否收到 |
   | --- | --- | --- |
   | ① | 裸进程 + `CFRunLoop::run_in_mode` 泵 | ❌ |
   | ② | `NSApplication` 初始化 + `CFRunLoop` 泵 | ❌ |
   | ③ | `NSApplication` 初始化 + `nextEventMatchingMask:untilDate:inMode:dequeue:` 拉取 | ✅ |
   结论：macOS 上 Carbon 热键事件必须经 AppKit 的 nextEvent 事件拉取才到本进程；eframe/winit 的 `NSApplication::run()` 内部即此循环，**Task 15 产品接线天然满足**。plan 示意的「裸 sleep 轮询 `GlobalHotKeyEvent::receiver()`」在 macOS 收不到事件——PoC 已按例③实现。
3. **合成按键口径与边界**：注入进程需「辅助功能」授权（本机 `AXIsProcessTrusted=true`、`CGPreflightPostEventAccess=true`）；注入事件在 HID 层进入会话流（session 级 CGEvent tap 可见，flags=alt+shift 正确）。合成注入足以验证投递链，**不能替真人切输入法**——输入法态与真键盘的组合行为以人工步骤为准。
4. **本机输入法事实（与任务描述的偏差）**：活动输入源实测为**微信输入法拼音**（`com.tencent.inputmethod.wetype.pinyin`，TIS 查询）；「已启用」列表另有系统 SCIM 简体（`com.apple.inputmethod.SCIM.ITABC`）、搜狗拼音、清见输入法等，均未选中。
5. **外证（风险参考）**：FB15168205（2024-09，macOS 15.0，Resolution: Fixed）——沙盒应用用 `RegisterEventHotKey` 注册「仅 Option 或 Option+Shift」修饰的热键在 15.0 曾完全失效；本机 macOS 27 实测注册与投递均正常，且 本应用非沙盒分发，暂不构成风险。

### 与 plan 示意的差异（供 Task 15 接线参考）

1. plan 的裸轮询循环收不到事件（见 #2），须有主线程事件循环；本 PoC 实现 = `NSApplication` 初始化 + nextEvent 拉取（每次最多阻塞 50ms），拉取片之间消费 `GlobalHotKeyEvent::receiver()`。
2. `HotKey::id()` 由 crate 计算（`mods.bits() << 16 | key`）；事件按 id 反查组合名，勿自行拼 id。
3. 注册失败信息不含 OSStatus 码；产品弹「被占用」提示时无法区分「被占用」与其它 OS 错误，只能统一按注册失败处理。

### 复现步骤

```bash
cd scripts/poc/poc2_hotkey
# 本机 crates.io sparse index 极慢（实测 ~14KB/s），拉依赖走 rsproxy：
cargo run --release --config 'source.crates-io.replace-with="rsproxy"' \
                       --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'
# 或先构建再直接运行（默认 60 秒自动退出；POC2_SECS=<秒> 覆盖时长，供自动化短跑）：
./target/release/poc2-hotkey
```

### 人工验证步骤（辉哥执行；中文/英文两态 × 三候选）

> 自动化部分无法替真人切输入法与按真键盘；以下步骤执行后把结果回填结论表 #4/#5。

1. 终端运行：`cd scripts/poc/poc2_hotkey && cargo run --release --config 'source.crates-io.replace-with="rsproxy"' --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'`，等出现「3/3 成功」与「现在开始监听」。
2. 切到任意**可输入文本**处（WPS 正文 / 备忘录 / 浏览器地址栏），光标点进输入框——输入法只在有文本焦点时才拦截按键。
3. 切到**中文（拼音）**输入法；可先在框里敲 1~2 个拼音字母进入组字（候选窗）态再按——拦截最可能发生在此态。
4. 依次按 ⌥Space、⌥⇧Space、⌃⌥⇧Space 各 1~2 次，记录终端是否出现 `>>> pressed: id=<n> <组合名>`。
5. 切**英文（ABC）**输入法，重复第 4 步。
6. 记录四格结果（中文态/英文态 × 触发与否），并附带记录「按键是否把字符输进了文本框」（⌥Space 在文本视图可能产生不间断空格）。
7. 60 秒自动退出；中途退出按 Ctrl+C。

### 默认键建议（据现证据；人工步骤执行后定稿）

- **⌥Space 两态均有效** → 按 product §1 用 ⌥Space，保留降级链（⌥⇧Space → ⌃⌥⇧Space）。
- **⌥Space 仅中文态失效** → 默认改 **⌥⇧Space**（本机实测注册与投递均通、无系统符号热键占用；与输入法自带快捷键的撞车面留待 Task 15 实机回归观察），⌃⌥⇧Space 作兜底。降级触发方式建议：注册成功但观察到「输入法态下按了没反应」时，Task 15 加一次可见提示并允许切备选。
- **三候选全部注册失败** → 回报辉哥扩池。本机实测三候选注册全 OK，该分支未触发。
- 旁证 #6（前台应用/输入法偶发吞键）提示：即使注册成功，唤起居也可能偶发无响应；Task 15 建议保留托盘唤起兜底，并在无响应场景给一次可见提示（v2.0 无设置页，提示即可）。

---

## Task 3：PoC-3 — 原生 NSMenu 在 egui 主线程弹出

**状态：通过（自动化 5 轮全过 + 浅色 / 深色 / 键盘导航截图留证）。plan 预置的降级路径（独立线程弹出、非模态 API）未触发、不需要。**

- **日期**：2026-10-16
- **实锁版本**：eframe / egui 0.37.0、objc2 0.6.5、objc2-app-kit 0.3.2、objc2-foundation 0.3.2、objc2-core-foundation 0.3.2、objc2-core-graphics 0.3.2、raw-window-handle 0.6.2
- **代码**：`scripts/poc/poc3_nsmenu/`（独立 crate，根 Cargo.toml `[workspace] exclude` 已追加）

### 结论表

| # | 验证项 | 结论 | 判定依据 |
| --- | --- | --- | --- |
| 1 | 右键 → 原生 NSMenu 弹出（系统观感） | ✅ 通过 | 5/5 轮弹出；浅色截图 `/tmp/poc3-r1-menu.png`；深色截图 `/tmp/poc3-dark-r1-menu.png`——`POC3_DARK=1` 把 app 有效外观置 darkAqua 后，菜单整体渲染为系统深色样式而周围其它 app 窗口仍为浅色，证明观感由 AppKit 按有效外观渲染（产品中该外观即系统外观），非自绘 |
| 2 | 键盘导航 ↑↓ | ✅ 通过 | 注入 ↓×4 后截图 `/tmp/poc3-r3-menu.png`：`拷贝路径` 带系统蓝色选中高亮；四轮 ↓×N + Return 的 `highlightedItem().tag()` = 1 / 2 / 4 / 3，与注入脚本全对上（前置：指针须先移出菜单，见「现象细节」#5） |
| 3 | Esc 取消 | ✅ 通过 | 轮 4 注入 Esc → `highlightedItem()` 为 None、状态栏显示「取消（未选择）」、菜单关闭 |
| 4 | 关闭后 egui 恢复（无卡死） | ✅ 通过 | 第 2~5 轮的右键均被 egui `secondary_clicked` 重新识别（输入管线若死则后续轮不会弹出）；收尾注入 a / b 键，egui 收到 `A, Text(a), B, Text(b)` |
| 5 | 连续弹 5 次无崩溃 / 无泄漏 | ✅ 通过 | 5 轮（含 1 轮取消）跑完、进程退出码 0；RSS 采样无增长（125088→124976KB 跨 24s；134912→135296KB 跨 16s 全流程） |
| 6 | 弹出期间 egui 渲染冻结 | ✅ 观测（符合预期，可接受） | 弹出调用期间主线程在嵌套 run loop 内，`ui()` 不返回、帧不更新；菜单开启期间抓的截图显示窗口内容仍是弹出前最后一帧。阻塞时长 = 菜单开启时长（每次弹出约 1.1~2.2s，含注入脚本的固定等待），非固定开销；关闭当帧即恢复 |
| 7 | `popUpMenuPositioningItem:atLocation:inView:` 返回值语义 | ⚠️ 不可依赖 | 24 次成功选择里 23 次 true、1 次 false（首轮）；6 次取消均 false。同场景结果不一致，**选择结果一律以 `highlightedItem()` 为准** |

### 现象细节

1. **弹出与定位（通过）**：`item=None` 时菜单左上角落在传入的 `location`（即鼠标点击点）。位置日志：点击点窗口底坐标 (320.0, 246.0) → 视图坐标 (320.0, 214.0)；窗口内容高 460，460−246=214，flipped 换算正确。
2. **冻结时长（观测）**：最终验证轮 5 次弹出阻塞 1592 / 1746 / 2168 / 1244 / 1957 ms。注入脚本在菜单开启后固定等 700ms 再发提交键，抓屏又占约 0.3~1.2s——阻塞时长基本等于菜单开启到关闭的全过程，即由用户决定。
3. **恢复（通过）**：菜单关闭后同一帧继续执行，状态栏（弹出次数 / 上次选择 / 累计阻塞）立即刷新；随后注入的 a / b 键到达 egui，界面计数同步更新（`/tmp/poc3-after-keys.png`）。
4. **返回值不稳定（观测）**：24 次成功选择里出现过 1 次 `popped=false`（自动化的首轮），取消轮 6/6 为 false——bool 与「是否选中」不对应，也无法与任何已知语义对齐；沿用 `highlightedItem()` 读取结果即可，勿用 bool 判定。
5. **指针悬停会预高亮菜单项（观测，接线必读）**：菜单左上角正好落在点击点，指针若停在菜单上，所在项会被预高亮——此时 ↓ 从第 2 项开始（实测踩到：↓×1 选中 tag=2）。产品里该行为与系统菜单一致、无需干预；自动化脚本须先把指针移出菜单再按 ↓（本 PoC 已加 `mouseMoved` 到菜单外 200ms 后再发 ↓，此后 5 轮结果稳定）。
6. **共享会话干扰（环境注记）**：本机与并行会话共用。收尾统计的 10 个按键里有 5 个非本脚本注入（`IO / Text(o) / S / Text(s) / Space / Text( )`，在注入脚本起跑前到达），判读时以日志中注入轮次对应的条目为准。

### API 实际用法与签名（与 plan 示意的差异）

objc2-app-kit 0.3.2 实际绑定（`src/generated/NSMenu.rs:166-175`）：

```rust
pub fn popUpMenuPositioningItem_atLocation_inView(
    &self,
    item: Option<&NSMenuItem>,
    location: NSPoint,
    view: Option<&NSView>,
) -> bool;
```

- plan 示意的 `menu.popUpContextMenu(None, &NSEvent::…, &view)` 实际是关联函数且需要真实 NSEvent：`NSMenu::popUpContextMenu_withEvent_forView(menu: &NSMenu, event: &NSEvent, view: &NSView)`（同文件 :146-148）。本 PoC 不需要合成 NSEvent，直接用 positioning 变体。
- `NSMenu` / `NSMenuItem` 均 `#[thread_kind = MainThreadOnly]`：`alloc` 需 `MainThreadMarker`，另需 `use objc2::MainThreadOnly;` 把 trait 引入作用域（否则 `NSMenu::alloc(mtm)` 编译不过）。这也意味着**菜单只能在主线程弹出**，「独立线程弹出」降级路线不成立。
- `NSMenuItem::initWithTitle_action_keyEquivalent` 是 `unsafe fn`（安全约定：selector 必须有效）；传 `None` 即无 action，PoC 阶段无需 target/action 桥接（回调桥接在 Task 18）。
- **无 action 的项默认被自动验证判为禁用**：必须 `menu.setAutoenablesItems(false)` + `item.setEnabled(true)`，否则项灰掉、`highlightedItem()` 无值。
- 读取选择：弹出返回后 `menu.highlightedItem()` → `Option<Retained<NSMenuItem>>`，取 `.tag()` / `.title()`；取消为 None。

位置计算（本次采用，免手工换算坐标）：

```rust
let window_point = window.mouseLocationOutsideOfEventStream();   // 窗口底坐标（原点 = 内容区左下）
let location = view.convertPoint_fromView(window_point, None);   // → 视图坐标（flipped，原点左上）
menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(&view));
```

实测视图坐标与 egui 坐标同值（同为逻辑点、原点左上；winit 的 view 为 flipped，winit-0.30.13 `view.rs:159-163`）。**Task 18 可直接用 egui 坐标定位菜单**（如 `i.pointer.interact_pos()`）；保留 AppKit 换算路径更稳（不依赖「view 恰好覆盖整个内容区」的假设，将来窗口加工具栏/内边距时仍正确）。

### 自动化方法与坑（供 Task 18 与后续 GUI PoC 复用）

- 注入方式：CGEvent HID 层合成事件（右键 / 左键 / ↑↓ / Return / Esc / 字母），需「辅助功能」授权（本机已具备；与 PoC-2 注入同源）。
- **后台启动的进程抢前台不可靠**：仅靠 `NSApplication::activate()` 反复重试，实测一次要等约 60 秒才成功（重试 602 次）。可靠做法：`window.orderFrontRegardless()` 把窗口压到最前，再合成一次左键点击——合成点击视为用户输入，首次尝试即激活成功。产品由用户点击唤起，不存在该问题。
- 抓屏用子进程 `screencapture -x`（约 0.3~1.2s，耗时计入菜单开启时长）。
- 键盘导航断言前先把指针移出菜单（`mouseMoved` 注入），否则指针悬停项被预高亮、↓×N 的落点整体偏移一项（见「现象细节」#5）。
- 关键判定信号用「egui 是否收到二次点击」而非注入端日志：注入成功不代表窗口收到，两者分开记录更易定位。

### 复现命令

```bash
cd scripts/poc/poc3_nsmenu
# 本机 crates.io sparse index 极慢（实测 ~14KB/s），拉新依赖走 rsproxy：
cargo build --release --config 'source.crates-io.replace-with="rsproxy"' \
            --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"'

# 手动：右键弹菜单（↑↓ 导航、Return 选择、Esc 取消；观感与导航以手动为准）
POC3_POS=200,200 ./target/release/poc3-nsmenu

# 自动化：5 轮注入（右键 → ↓×N → Return/Esc）+ 每轮菜单期抓屏 + 收尾输入 a/b
POC3_AUTO=1 POC3_POS=200,200 ./target/release/poc3-nsmenu

# 深色外观对照（验证菜单观感由 app 有效外观驱动；系统为浅色时亦可复现深色菜单）
POC3_AUTO=1 POC3_DARK=1 POC3_POS=200,200 ./target/release/poc3-nsmenu
```

证据截图在本机 `/tmp/`（与 PoC-1/2 同，重启后可能被系统清理）：`poc3-r1-menu.png`（浅色菜单）、`poc3-r3-menu.png`（↓×4 高亮「拷贝路径」）、`poc3-dark-r1-menu.png`（深色菜单）、`poc3-after-keys.png`（5 轮后状态 + 收尾按键计数）。

### 降级路径评估（本次未触发；留档供 Task 18 备用）

- plan 预置的两条降级本次都不需要：阻塞式主线程弹出与 eframe 主循环兼容良好——冻结时长 = 菜单开启时长，关闭即恢复。
- 「独立线程弹出」路线不成立：`NSMenu` 在 objc2 绑定里即 `MainThreadOnly`，AppKit 要求菜单在主线程跟踪。
- 「非模态 API」不存在等价物：`NSMenu` 没有非阻塞弹出接口。若产品形态要求菜单开启期间继续渲染（如动画），只能自绘菜单或把菜单放进独立进程，两者都放弃系统观感——不建议。
- 残留风险（低）：嵌套 run loop 期间 eframe 帧循环与 winit 事件派发暂停（窗口冻结即其表现）。本 PoC 未观察到其它副作用；长时开启菜单（>10s）与多层子菜单留待 Task 18 实机回归。

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

---

## Task 4：PoC-4 — eframe 两红线（隐藏 CPU 0% × 唤起 <100ms）

**状态：⚠️ 需折中——两条红线无法用「egui 原生显示命令」同时满足（隐藏 ✅ 0%，唤起 ❌ 对外可见中位 117ms）；显示改走 native 路径（主线程 `orderFrontRegardless`）后两线兼得（隐藏 0%、唤起对外可见中位 16.7ms）。产品接线建议见文末。**

- **日期**：2026-10-16
- **实锁版本**：eframe / egui 0.37.0、winit 0.30.13、objc2 0.6.5、objc2-app-kit 0.3.2、dispatch2 0.3.1、raw-window-handle 0.6.2
- **代码**：`scripts/poc/poc4_idle/`（独立 crate，根 Cargo.toml `[workspace] exclude` 已追加；测量脚本 `measure.sh`）
- **窗口形态**：600x400 置顶（`always_on_top`，悬浮条形态）、位置 (200,200)
- **两条口径（勿混）**：
  - **进程内**：`SHOW_LATENCY_MS` = 发出显示请求 t0 → 首帧 `ui()` 进入；`SHOW_LOGIC_MS` = t0 → 首个 `logic()`。不含热键系统投递（Task 2 覆盖）、不含帧内绘制（swap_buffers）、不含上屏。
  - **对外**：`VISIBLE_DELTA_MS` = t0（落盘锚点）→ CGWindowList 首次报本窗口 onscreen（外部轮询，1ms 间隔；含 WindowServer 标志更新延迟）。

### 结论表

| # | 验证项 | 结论 | 判定依据 |
| --- | --- | --- | --- |
| 1 | 隐藏态 CPU（60s） | ✅ 恒 0.0% | 12/12 采样 = 0.0%（`top -l 2 -s 1 -pid <pid> -stats cpu` 区间值）；累计 CPU 时间 0:00.17 → 0:00.17（60s 增量低于 ps 分辨率 10ms） |
| 2 | 隐藏期无唤醒源 | ✅ 零帧 | 打点：启动 0.35s 内 8 帧（首帧展出 + 隐藏命令 + macOS 遮挡事件），此后 **119.9s 零 PASS**，直到退出命令；事件循环空闲即 `ControlFlow::Wait` |
| 3 | 常显静止对照 | ✅ 0.0% | 常显 60s：12/12 = 0.0%，累计 0:00.19 → 0:00.19；启动后零帧（egui 按需重绘，无轮询） |
| 4 | 唤起延迟：egui 路径（30 轮） | ❌ 超线 | 进程内首帧中位 **132.74ms**（min 119.93 / P99=max 157.53）；对外可见中位 **117.48ms**（n=12，min 115.58 / max 131.62）——两条口径均 >100ms |
| 5 | 唤起延迟：native 路径（30 轮） | ✅ 达标 | 进程内首帧中位 **32.83ms**（min 21.04 / P99=max 56.19）；对外可见中位 **16.74ms**（n=12，min 13.97 / max 22.72） |
| 6 | 唤起「通道」成本 | egui 101ms vs native 1ms | SHOW_LOGIC：egui 路径中位 **101.12ms**（30 轮极差 1.0ms，钉在 100ms 节流上）；native 路径中位 **1.01ms**（0.64~5.47）⇒ 唤醒机制本身 ~1ms，100ms 全部来自节流 |
| 7 | keep-warm（隐藏下持续重绘）代价 | ⚠️ ~0.8% CPU 且不改善唤起 | 隐藏 + 每 100ms 申请重绘：采样 0.4~0.7%，累计 0:00.21 → 0:00.75（≈66s 内 0.54s ≈ **0.8%**）；实际帧率 4~9 帧/s（eframe 把隐藏视口重绘节流到 ≥100ms）；唤起仍走同一节流（节流按窗口可见性生效，与是否热身无关） |
| 8 | 首帧 ui 时窗口状态 | 两条路径皆「已可见」 | 30+30 轮 `win_visible_at_first_ui=1`：egui 路径的命令在节流帧末即应用（orderFront 先于首帧 ui）；native 路径主队列先行 |

### 现象细节

1. **隐藏态 CPU 原始采样**（`POC4_MODE=hide`，60s，每 ~5s 一点 × 12）：

   ```text
   sample[1..12] cpu=0.0%          （12/12 全 0.0）
   累计 CPU 时间：0:00.17 → 0:00.17
   PASS：t=0.007…0.348 共 8 帧（启动）→ 119.9s 零帧 → t=120.2 起 4 帧（退出命令）
   ```

2. **egui 路径单轮逐帧结构**（`ViewportCommand::Visible(true)`）：

   ```text
   SHOW_LOGIC_MS=100.759                      ← t0+100.8ms：节流到点后第一个 logic
   PASS seq=10 kind=logic win_visible=0 occluded=1 t=4.109   ← 仍隐藏；该帧末才应用 Visible(true) → orderFront
   PASS seq=11 kind=logic win_visible=1 occluded=1 t=4.126   ← 已在屏，但 egui 侧仍判遮挡 → 只跑 logic
   PASS seq=12 kind=logic win_visible=1 occluded=0 t=4.166
   SHOW_LATENCY_MS=157.529                    ← 遮挡态清除后的首帧 ui
   ```

   构成：100ms（eframe 节流）+ ~1.5ms（唤醒/命令应用）+ 15~55ms（等 macOS 遮挡状态变更通知，期间只跑 logic）。

3. **native 路径单轮逐帧结构**（主队列 `orderFrontRegardless` + `request_repaint`）：

   ```text
   NATIVE_MAIN_MS=0.346 round=2               ← 主队列 block 执行 orderFrontRegardless（含排队 0.3ms）
   SHOW_LOGIC_MS=0.693                        ← 0.7ms 即跑 logic（无节流）
   PASS seq=15/16 kind=logic win_visible=1 occluded=1       ← 窗口已在屏，等遮挡态清除
   SHOW_LATENCY_MS=32.702                     ← 遮挡态清除后的首帧 ui
   ```

4. **对外「窗口在屏」**（CGWindowList 轮询）：egui 路径 115.6~131.6ms；native 路径 14.0~22.7ms。native 路径下窗口 ~1ms 即在屏（内容为上一帧），新帧中位 ~33ms 到——悬浮条内容静态时用户无感。
5. **遮挡干扰（方法学坑，已处理）**：窗口被他人窗口盖住时 macOS 置 occluded，eframe 对遮挡视口只跑 `logic` 不跑 `ui` → 该轮首帧打点会失真（曾出现 30 轮里 4 轮 ui 未跑、被兜底记成 15s 假延迟）。本 PoC 已改为：① 窗口置顶（`always_on_top`，与悬浮条形态一致）；② 打点带 `occluded=` 字段；③ 上一轮未跑到首帧时显式打 `SHOW_MISSED`，不混入分布。

### 根因（源码级，实锁版本）

| 事实 | 出处 |
| --- | --- |
| 隐藏窗口的跨线程唤起被硬性节流 100ms（本 PoC 的核心发现） | eframe `native/run.rs:27`：`INVISIBLE_WINDOW_REPAINT_INTERVAL = 100ms`；`native/run.rs:373-380`：`UserEvent::RequestRepaint` 时若窗口不可见 → `when.max(now + 100ms)`（注释指明为 Windows 隐藏窗口高 CPU 之修，egui#7776） |
| 「不可见」判定 = winit `is_visible()`；`Visible(false)` 即 `orderOut` | winit `platform_impl/macos/window_delegate.rs:904-914`（`set_visible(false)=orderOut`、`set_visible(true)=makeKeyAndOrderFront`、`is_visible()=NSWindow.isVisible()`） |
| 隐藏/遮挡视口不跑 `ui`，只补跑 `App::logic` | eframe `glow_integration.rs:680`（`is_visible = info.visible().unwrap_or(true) || …`）+ egui `data/input/viewport_info.rs:98`（`visible()` 仅由 minimized/occluded 推导）；首帧 ui 额外等遮挡状态变更通知即由此而来 |
| 空闲即 `ControlFlow::Wait`（隐藏零 CPU 的机制） | eframe `native/run.rs:297-300` |
| 跨线程唤醒通道（eframe 内置 `EventLoopProxy`，后台线程可用） | eframe `glow_integration.rs:364-378`（`set_request_repaint_callback`） |

### 两线兼容性结论与折中建议

1. **结论**：egui 原生显示路径下两线互斥——「隐藏 0% CPU」与「唤起 <100ms」不可兼得；瓶颈是 eframe 给隐藏窗口的 100ms 唤醒节流（对「隐藏时不重绘」的代价补偿，见 `run.rs:373-380` 注释）。这是 ADR-8 单栈押注的实证风险点。
2. **建议（native 显示路径，已实测达标）**：隐藏仍用 `ViewportCommand::Visible(false)`；显示由热键线程 `DispatchQueue::main().exec_async` 投递主线程执行 `orderFrontRegardless()` 后 `ctx.request_repaint()`。隐藏态 CPU 仍 0%（未新增 repaint 源），唤起对外可见中位 16.7ms、进程内首帧中位 32.8ms。
   - 与 PoC-1b 结论一致：跨全屏 Space 与「不抢焦点」都要求 `orderFrontRegardless`（winit 的 `Visible(true)` 走 `makeKeyAndOrderFront`，会激活 app）。即**产品本来就必须走这条路径**，两红线的冲突随之消解。
   - 代价：`dispatch2 = "0.3"` 依赖 + 本 PoC 的 ~15 行接线（可整体搬运）。
3. **不建议 keep-warm**：量化代价 ~0.8% CPU（帧率被节流封顶在 4~9 帧/s），但唤起仍受同一节流支配 → 纯成本无收益。
4. **残留项（低）**：native 路径首帧 `ui` 仍等 macOS 遮挡状态变更（中位 ~33ms、P99 56ms）。悬浮条内容静态时用户无感；若将来要求「唤醒即新数据」，在 100ms 红线内仍有余量，但需按 P99≈56ms 留裕量，或向 eframe 提「隐藏窗口即时唤醒」需求。

### 复现步骤

```bash
cd scripts/poc/poc4_idle
bash measure.sh build            # 拉依赖走 rsproxy 镜像
bash measure.sh cpu-hide 60      # 隐藏态 CPU：12 点采样 + 累计 CPU + 帧打点
bash measure.sh show 15          # 常显对照 CPU
bash measure.sh warm 60          # keep-warm 代价
bash measure.sh cycle            # 30 轮 egui 路径延迟分布（约 92s；窗口会闪现 30 次并抢焦点）
bash measure.sh native           # 30 轮 native 路径延迟分布（不抢焦点）
bash measure.sh visible cycle    # 对外「窗口在屏」口径（外部轮询）
bash measure.sh visible native
```

### 与 plan 示意的差异

1. plan 示意 `eframe::App::update`；实锁 0.37 为 `logic()` + `ui()` 拆分（隐藏/遮挡视口只补跑 `logic`），本 PoC 的「首帧」打点正建立在该拆分上。
2. plan 示意 `top -pid <pid> -l 60`；实测取 `top -l 2 -s 1 -pid <pid> -stats cpu` 的**第二个样本**（区间值；第一样本是进程生命周期均值，会被启动期摊薄），并用 `ps -o time=` 累计 CPU 时间交叉校验。
3. plan 的「后台线程每 2~3 秒 toggle + 记录首帧」保留（3s/轮 = 隐藏 2.5s + 显示 0.5s，30 轮）；额外加了 t0 落盘锚点（`POC4_T0_FILE`）以支持外部「窗口在屏」口径，并按 plan Step 1 第 5 条实现 `POC4_MODE=show|hide|cycle`（另加 `native`/`warm` 差分）。

---

## Task 5：PoC-5 — SMAppService 自启（ad-hoc 签名）

**状态：✅ 通过——ad-hoc 签名的 .app 可成功 register，status 转 enabled(1)；open 与直接执行两种启动方式均有效；无需 sudo。SMAppService 可用，无需启用 LaunchAgent 兜底。重启实测留辉哥（终态已注册，见文末人工步骤）。**

- **日期**：2026-10-16
- **环境**：macOS 27.0（26A428）、Apple Silicon（arm64）、Swift 6.4（swiftc 1.168.6）
- **代码**：`scripts/poc/poc5_autostart/`（`poc5.swift` + `make-mini-app.sh` 组最小 .app + `verify.sh` 三态验证；非 cargo crate，根 Cargo.toml 无需改）
- **被测对象**：最小 .app（`CFBundleIdentifier=com.example.maceverything.poc5`、`CFBundleExecutable=poc5-binary`、`LSUIElement=true`，Swift 单文件），ad-hoc 签名：`codesign --force --deep -s -` → `Signature=adhoc`、`TeamIdentifier=not set`、`CDHash=fbc77b29…`

### 结论表

| # | 验证项 | 结论 | 判定依据 |
| --- | --- | --- | --- |
| 1 | ad-hoc 签名下 register | ✅ 可用 | `register: OK（无抛错）→ status=enabled(1)`；本机未出现任何审批弹窗、未用 sudo |
| 2 | 三态流转 | ✅ | `notFound(3)`（首次，LS 未认识）→ register → `enabled(1)` → unregister → `notRegistered(0)`；重复流转稳定复现（direct/open 两轮全绿） |
| 3 | 启动方式差异（open vs 直接执行） | 两者皆有效，无功能差异 | open：`ppid=1`（launchd 拉起）、`bundlePath=/private/tmp/poc5.app`（LS 解析 /tmp 符号链接）；直接执行：`ppid=<shell>`、`bundlePath=/tmp/poc5.app`。register/status/unregister 结果两模式逐项一致 |
| 4 | launchctl 观察注册态 | ❌ 看不到（机制使然） | 注册态下 `launchctl list` 与 `launchctl print gui/501` 均无 poc5 条目；运行的 App 才以 `application.<bundleid>.<n>` 出现（对照：本机 WeChat/Edge/Ghostty 等运行中 app 均有该形式条目） |
| 5 | sfltool dumpbtm（BTM 数据库） | ⚠️ 授权门禁，不可靠 | 首两次全量成功（2074 行，含 UID 501 段）；随后稳定挂起于 `SFAuthorization → AuthorizationCopyRights`（sample 栈：`mach_msg2_trap` 等 authd 回复）。sfltool 非交互不可用，留作人工检查手段 |
| 6 | kickstart 近似验证（plan Step 2） | ❌ 不适用 | `launchctl kickstart gui/501/application.com.example.maceverything.poc5` 与 `…/com.example.maceverything.poc5` 均报 `Could not find service ... in domain for user gui: 501`；注册态实测无 launchd 服务名可 kick |
| 7 | 未签名二进制 | 无法执行（arm64 内核门槛） | `codesign --remove-signature` 后 exec 直接 SIGKILL（exit=137）；最小 clang 二进制对照同样 137。ad-hoc 是硬性下限，「无签名 + SMAppService」场景不存在 |
| 8 | 无 bundle 直接跑（plan 假设"会报错"） | ⚠️ 不报错，且注册出独立条目 | bundle 外裸二进制（`bundleID=<nil>`）`register: OK → enabled(1)`；与 bundle 条目互相独立（一边 unregister 另一边仍 enabled）。产品侧须在 bundle 内调用并核对 bundle id |
| 9 | 二进制内容变化（模拟更新） | status 不失效 | 用构建戳改二进制（cdhash `fbc77b29`→`f5e528a2`），`status` 仍 `enabled(1)`；随后 unregister/register 均正常。「登录时 BTM 是否因 DR 不匹配拒启」需重启实测（未验证） |
| 10 | 条目身份键 | bundle id（非路径/cdhash） | 注册从 `/tmp/poc5.app` 迁到 `~/Applications/poc5.app`（unregister 旧 + register 新）后，两个路径副本 status 均报 `enabled(1)`；cdhash 变化不影响见 #9 |
| 11 | 同 bundle 第二可执行（他名） | 同一身份 | 把二进制复制为 `Contents/MacOS/maceverything-cli` 后运行，`bundleID` 仍解析为 `com.example.maceverything.poc5`——身份由 bundle 决定，与可执行名无关（product 的 App 主可执行 + CLI 同 bundle 场景成立） |

### 现象细节（原始输出）

**三态原始流转**（direct 模式；open 模式逐行一致，仅 ppid/bundlePath 不同）：

```text
[1] status   → status=notFound(3) raw=3            ← 本 PoC 首次运行的初始态：app 在 /tmp、LS 尚未认识
[2] register → register: OK（无抛错）→ status=enabled(1)
    status   → status=enabled(1) raw=1
[3] unregister → unregister: OK（无抛错）→ status=notRegistered(0)
    status   → status=notRegistered(0) raw=0
[4] register（复原）→ register: OK → status=enabled(1)     ← 终态=已启用
```

> 口径说明：`notFound(3)` 只出现在「LS 从未认识该 app」的首次；其后的完整轮次（direct/open 各一整轮）初始态均为 `notRegistered(0)`，上表其余各行即取自该两轮的逐行一致输出。

open 模式同构（`open /tmp/poc5.app --args status|register|unregister`）：

```text
[2026-10-16T07:07:15Z] pid=66457 ppid=1 起: cmd=status ... bundlePath=/private/tmp/poc5.app
[2026-10-16T07:07:17Z] pid=66477 ppid=1 register: OK（无抛错）→ status=enabled(1)
[2026-10-16T07:07:21Z] pid=66497 ppid=1 unregister: OK（无抛错）→ status=notRegistered(0)
```

- `notFound(3)` 只出现在「LS 从未认识该 app」时；经历一次 register 后不再出现（再 unregister 即 `notRegistered(0)`）。**产品侧读状态时 `notFound` 与 `notRegistered` 都应视作「未启用」**。
- open 模式有 2 秒等待：`open` 异步返回，进程输出只落日志文件（`/tmp/poc5.log`），stdout 不可见——这是 `open` 方式取证的唯一渠道。
- daemon 常驻可被 `ps aux | grep poc5-binary` 观察（`/tmp/poc5.app/Contents/MacOS/poc5-binary daemon 6`）；直接执行的进程不产生 launchctl `application.*` 条目。

**BTM 挂起取证**（`sample` 卡住的 sfltool）：

```text
-[SFAuthorization obtainWithRights:flags:environment:authorizedRights:error:]  (SecurityFoundation)
 AuthorizationCopyRights  (Security)
 _AuthorizationCopyRights_send_message  (Security)
 xpc_connection_send_message_with_reply_sync  (libxpc.dylib)
 ... mach_msg2_trap  (libsystem_kernel.dylib)      ← 等 authd 授权回复，无交互渠道即挂起
```

**注册前 BTM 基线**（`sfltool dumpbtm` 全量 2074 行，成功那次）：UID 501 段含 EdgeUpdater、fpprotect、ollama 等既有条目，**无 poc5/maceverything**。注册后的 BTM 原始 dump 因挂起未取得（见「遗留」）。

**边界实验原始行**：

```text
# 未签名（codesign --remove-signature 后）
$ /tmp/poc5-unsigned-*.app/Contents/MacOS/poc5-binary status
（无任何输出）exit=137            ← arm64 内核拒绝执行未签名二进制
$ codesign --verify --deep /tmp/poc5-unsigned-*.app
/tmp/poc5-unsigned-*.app: code object is not signed at all

# 无 bundle（build/ 裸二进制，bundleID=<nil>）
$ ./build/poc5-binary register
register: OK（无抛错）→ status=enabled(1)      ← 不报错！
$ /tmp/poc5.app/Contents/MacOS/poc5-binary unregister   # 只注销 bundle 条目
bundle 内 status: notRegistered(0)
bundle 外 status: enabled(1)                    ← 两条目独立

# cdhash 变化后（换二进制不改 bundle id）
CDHash=f5e528a2fbaa0ef7c4b59d350e894574e69cb32f（新）
status=enabled(1)                               ← 仍报启用

# kickstart 近似
$ launchctl kickstart -k gui/501/application.com.example.maceverything.poc5
Could not find service "application.com.example.maceverything.poc5" in domain for user gui: 501
```

### 机制与产品推论

1. **注册条件 = 在 .app bundle 内 + 有签名**：进程经 `NSBundle.main` 能解析出含 `CFBundleIdentifier` 的 bundle 即可，与「谁拉起」（LaunchServices / 终端 / shell）无关；Apple Silicon 上内核强制 ad-hoc 签名下限，签名形态（ad-hoc vs Developer ID）不影响本次注册。⇒ 产品（个人使用 ad-hoc，对外分发再补 Developer ID）的自启能力成立。
2. **登录项由 BTM 管理，不落 launchd**：macOS 13+ 的 `SMAppService` 登录项存于 BackgroundTaskManagement，`launchctl` 只在进程运行时可观察；因此**产品自启状态自查走 `SMAppService.mainApp.status`**，`launchctl`/`sfltool` 只作旁证。
3. **产品须防「注册错目标」**：`register()` 在 bundle 外不报错（结论表 #8），故 App 侧接线时应先校验 `Bundle.main.bundleIdentifier == "com.example.maceverything"` 再调 register。包内身份已实测无歧义：主可执行与 CLI 同 bundle 不同名时 `Bundle.main` 身份一致（#11），两条入口不会注册出两套条目；风险集中在「未打包的 CLI 裸跑」场景。
4. **更新后行为**：cdhash 改变不影响已注册条目的 status（#9）；重新打包但代码不变时 cdhash 可复现（同内容同哈希），无需重复注册；稳妥做法仍是 App 启动时 `status == .notRegistered/.notFound` 就补 register（幂等）。
5. **LaunchAgent 兜底（本次未启用，差异留档）**：plist 兜底不要求签名/bundle，任何二进制可跑；代价是需手写 plist、与 App 更新无联动、开关需自行 bootstrap/bootout。既然 ad-hoc 路线可用，**建议不作为主路径**；仅当重启验证发现 BTM 不拉起时再回退。

### 人工重启验证步骤（辉哥执行；脚本侧已完成到「终态已注册」）

终态：登录项已注册（`enabled(1)`），指向 `~/Applications/poc5.app`（cdhash `fbc77b29…`，与仓库源码一致）；该二进制无参数启动即进 daemon 600 秒，便于观察。

1. 重启 Mac，用辉哥账号登录。
2. 登录后 1 分钟内执行：`ps aux | grep poc5-binary` —— 预期出现 `~/Applications/poc5.app/Contents/MacOS/poc5-binary`（无参数）进程。
3. 双证据：`tail -5 /tmp/poc5.log` —— 预期新增一行 `cmd=<none>` 且 `ppid=1`（由 launchd 拉起）的启动记录。
4. 旁证：`launchctl list | grep -i poc5`（进程存活期间可见 `application.com.example.maceverything.poc5.<n>`）；「系统设置 → 通用 → 登录项与扩展」中应有 poc5 条目；首次登录若弹「允许在后台」类提示，点允许后重登再看。
5. 验证完清理（可选）：`~/Applications/poc5.app/Contents/MacOS/poc5-binary unregister`，然后 `~/Applications/poc5.app` 与 `/tmp/poc5.app` 移入废纸篓。
6. 若第 2/3 步未见进程：记为「BTM 未拉起」，此时再评估 LaunchAgent 兜底（结论表外的兜底路线见「机制与产品推论」第 5 条）。

> 注：plan 的 `sudo launchctl kickstart -k gui/$(id -u)/com.example.maceverything.poc5` 近似验证**不适用**（结论表 #6：注册态实测无 launchd 服务名可 kick），故本 PoC 只留真实重启一步。

### 复现步骤

```bash
cd scripts/poc/poc5_autostart
bash make-mini-app.sh                 # swiftc 编译 + 组 /tmp/poc5.app + ad-hoc 签名 + 验签
bash verify.sh direct                 # 三态全流程（直接执行 bundle 内二进制）
bash verify.sh open                   # 三态全流程（open 经 LaunchServices 拉起）
POC5_BTM=1 bash verify.sh direct      # 附：带看门狗尝试 sfltool dumpbtm（本机会挂起，8s 超时）
# 单命令：
/tmp/poc5.app/Contents/MacOS/poc5-binary register|status|unregister
open /tmp/poc5.app --args register    # 经 open 传参亦可
tail -f /tmp/poc5.log                 # 输出日志（open 方式唯一取证渠道）
```

### 与 plan 示意的差异

1. plan 假设「无 bundle 直接跑会报错」——实测**不报错**且注册出独立条目（结论表 #8），已改列为产品侧接线注意项。
2. plan 的 `sudo launchctl kickstart` 近似验证不可行（结论表 #6），真实重启为唯一验证；`sfltool dumpbtm` 非交互不可用（结论表 #5），本 PoC 的观察主通道改为进程内 `status` API + `launchctl` 旁证。
3. plan 的默认 `/tmp/poc5.app` 路径保留为脚本默认；为使重启验证不受 /tmp 清理影响，终态注册迁移到 `~/Applications/poc5.app`（product §交付形态的真实安装位置，结论表 #10 顺带取证）。
4. 额外记录：未签名对照（#7）、重建/改内容对照（#9）、条目身份键（#10）、同 bundle 第二可执行（#11）——四项均为 plan 外补充，服务产品接线。

### 遗留

1. **注册后的 BTM 原始 dump 未取得**（sfltool 授权门禁）；如需补证，辉哥在终端跑 `sudo sfltool dumpbtm | grep -i poc5`（会要求输入密码）或看「系统设置 → 登录项与扩展」。
2. **`requiresApproval(2)` 状态未在本机观察到**（首登审批路径）；重启验证时留意是否出现提示。
3. **DR 不匹配时的登录拉起行为未验证**（改二进制后再重启的场景，即「App 更新后登录项是否仍能拉起」）——本 PoC 终态已用与源码一致的 cdhash 注册，该场景待后续版本实测。

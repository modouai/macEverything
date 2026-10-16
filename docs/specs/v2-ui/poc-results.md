# macEverything v2.0 图形界面 — PoC 验证结果

> 记录阶段 A 五个 PoC 的实机验证结果，Task 2~5 完成后在本文件追加。
> 验收环境：macOS 27.0（Apple Silicon、arm64）、双 4K 显示器（主屏 x∈[0,1920)，副屏 x∈[1920,3840)，各 1920x1080 逻辑点）。
> 截图证据为本机 `/tmp/poc1-*.png`（重启后可能被系统清理）；窗口状态用 `CGWindowList` 清单（layer / onscreen / pid / bounds）取证。

---

## Task 1：PoC-1 — objc2 跨 Space + 浮动级别

**状态：BLOCKED**（跨 Space 在「他人全屏应用新建的 Space」场景不成立；「多桌面 Space」场景待人工三指滑动一测后定论。产品目标 7 依赖此项，降级/改路线需辉哥决策，未擅自改 spec。）

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

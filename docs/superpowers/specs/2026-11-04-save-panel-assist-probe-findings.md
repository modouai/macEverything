# save-panel-assist M0 探针结论

- 日期：2026-11-05（部分结论由产品链路实机取证回填；探针独立运行未执行）
- 环境：macOS 27（Darwin 27.0.0）；release bundle + `MCE_APP_LOG` 文件日志

> 状态：产品主链路已实机通过。下表标注「产品实机」的结论来自 2026-11-05 的
> 产品路径取证（TextEdit 保存面板 + winit 内部插桩），未经独立探针复核；
> 标记「探针待跑」的项仍需 `MCE_PANEL_PROBE=1` 独立取证。

## 操作步骤（探针独立取证，尚未执行）

1. `cargo build -p macEverything-app`（debug 未打包形态在当前环境无法启动 GUI，建议用 `release/macEverything.app/Contents/MacOS/macEverything-app` + `MCE_PANEL_PROBE=1`）
2. 前台保持探针 app，另一终端打开 TextEdit，⌘⇧S 打开另存为（sheet 形态）
3. 观察日志：前台切换 → 通知注册 → 窗口快照 → 命中疑似面板 → 2s 后跳转实验
4. 记录策略 A/B 结果、按钮标题 dump、剪贴板恢复情况
5. 换 VS Code「另存为」补独立窗口形态
6. 每轮实验前 `echo "clip-content" | pbcopy`，实验后 `pbpaste` 核对剪贴板恢复

## 结论表

| # | 验证点 | 结论 | 证据 |
|---|---|---|---|
| 1 | sheet 形态面板的 AX 通知行为 | 产品实机：`AXWindowCreated` / `AXFocusedWindowChanged` 均出现，扫描可命中（具体触发占比需探针复核） | detector 日志 + 命中后浮层正确显示 |
| 2 | 策略 A AX set value 可行性 | 产品实机：**可行**——直设 value 后读回校验通过，Return 提交成功 | `[navigator] 策略 A:直设 value 成功` → `Done` |
| 3 | CGEventPostToPid 对面板服务进程有效性 | 产品实机：**无效**——PostToPid 投递的 ⌘⇧G 未被 TextEdit 处理（3s 无 Go-to 输入框）；改全局 HID post 后立即生效 | `[navigator] 3s 内未定位 Go-to 输入框`（PostToPid）vs 全局 post 后 Go-to 弹窗出现 |
| 4 | 浮层 key 让还 | 产品实机：浮层 orderOut 后保存面板恢复键盘焦点，跳转后续输入正常 | 跳转成功后继续操作面板无异常 |
| 5 | ⌘⇧G 后输入框 AX 定位稳定性 | 产品实机：**输入框在独立 AXWindows 窗口**（不在 AXFocusedWindow 树）；遍历 AXWindows + 基线/焦点复核后稳定命中 | `[navigator] Go-to 输入框已定位` |
| 6 | 前往按钮 AXPress vs 合成 Return | 产品实机：合成 **Return** 提交有效（当前策略）；AXPress 对照实验仍需探针 | `[navigator] go-to sheet 已关闭(跳转提交)` |
| 7 | AX API 后台线程调用可行性 | 产品实机：navigator 后台线程 AX 调用正常（无崩溃、无错误） | 多次跳转成功 |
| 7b | IME（探针外补充） | 产品实机：浮层每帧 `request_focus` 会触发 egui `interrupt_ime()`，打断中文组合致选字 Commit 丢失；仅未聚焦时聚焦后修复 | winit 插桩：`insertText("文") hasMarked=false enabled=false` → 修复后上屏成功 |
| 8 | 打开面板(open)与保存面板(save)的特征差异 | 待实机（⌘O 路径未测） | |

> 备注:AXFloatingWindow 误报验证（Get Info / Inspector）：产品实机 Finder ⌘I 未触发浮层，误报风险暂未复现。

## 对 M1 的影响（已回填）

- 策略 A/B 默认值：**策略 A 优先，实测可用**（直设 + 统一 Return 提交）；策略 B 为降级路径（未在实机触发）
- 检测通知选型：NSWorkspace `DidActivateApplication` + AX `WindowCreated`/`FocusedWindowChanged`/`UIElementDestroyed`（销毁需三态重扫收敛）
- post 目标：**全局 HID post**（PostToPid 对保存面板无效）
- 「前往」按钮本地化形态：未取证（当前用 Return 提交，AXPress 对照待探针）

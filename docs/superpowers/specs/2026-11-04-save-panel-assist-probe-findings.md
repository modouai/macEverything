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

## 检测适配取证（2026-11-05 下午，实机漏报/误报专项）

背景：主链路通过后，实机发现 XMind/Edge/文本编辑保存面板不弹浮层、WPS
查找框/下拉框误触发。专项取证结论（产品日志 + `MCE_PANEL_AX_PROBE=<pid>`
诊断探针）：

**漏报侧（检测层两类形态全漏）**

| 形态 | 实测特征 | 例子 |
|---|---|---|
| sheet 附着 | `AXSheet`，**subrole 读不到(None)**，e3 b11 r5-9 | TextEdit/XMind/Edge 保存面板三家一致 |
| 标准窗口 | `AXWindow/AXStandardWindow` + 标题「打开」，e1 b10-15 r5-8 loc6-7 | XMind ⌘O、第三方 app 打开面板 |

subrole 白名单（AXSystemDialog/AXDialog/AXFloatingWindow）对上述两形态全漏。
XMind(Electron)/Edge(Chromium)/TextEdit 的 AX 能力本身正常（诊断探针实测
通知注册 err=0），排除「宿主不初始化 AX 服务」假设。

**误报侧（数量阈值不构成判别力）**

| 窗口 | 形态 | 数量信号 |
|---|---|---|
| WPS 查找框 | AXDialog e1 b9 | 过旧判定 |
| WPS 查找和替换 | AXDialog e1 b12 | 过旧判定 |
| WPS 表格下拉框 | AXSheet e1 b1 r1 | 过 sheet 数量阈值 |
| 文档/网页窗口 | AXStandardWindow e1-5 b24-42 r4-8 **loc2-7** | 带位置名命中！ |

**根本判别维度 = 结构指纹**：NSSavePanel 系面板必嵌位置侧栏（桌面/下载/
文稿…静态文本，XMind 面板实测 loc5-7），上述全部弹层/文档窗口 loc0 或仅
靠标题区分（面板窗口标题恒为打开/存储/另存为系）。最终判定：快速信号
（subrole 白名单 ∥ AXSheet ∥ 标准窗+标题精确命中）∧ 数量底线(e≥1,b≥1)
∧ 结构指纹(loc≥1 ∥ 浏览容器≥3)。

**换绑瞬态拒绝**：Edge 在激活瞬间被整批拒绝通知注册（旧代码不重试、不扫描、
无日志 → 该 app 永久失联，同 pid 数秒后空闲时注册成功）。修复：换绑失败
重试（常驻 timer + tick 倒数 0.25s~4s 五档）+ 失败也扫描 + 错误码留痕。

**FFI 教训（两次实机 SIGSEGV）**：`CFRunLoopTimerCreate` 第 4 参是整数
`flags`（CFOptionFlags），无 tolerance 参数——误声明为 f64 令 ARM64 整数
参数整体错位一格（整数走 x 寄存器/浮点走 v 寄存器），callout/context 读歪，
首次调用即段错误（寄存器指纹两次崩溃完全一致 = 确定性错误）。修 ax_ffi 与
panel_probe 两处；另「回调内 invalidate+release 自身 timer」路径一并移除，
改为常驻单 timer + tick 倒数。诊断工具：`MCE_PANEL_AX_PROBE=<pid>`
（一次性取证：基线注册错误码 → 写 manual-accessibility → 复测 + 窗口快照）。

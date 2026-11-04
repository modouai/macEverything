# save-panel-assist M0 探针结论

- 日期：<执行日期>
- 环境：macOS <版本>；探针二进制 target/debug/macEverything-app（MCE_PANEL_PROBE=1）

## 操作步骤

1. `MCE_PANEL_PROBE=1 ./target/debug/macEverything-app 2>&1 | tee /tmp/probe.log`
2. 前台保持探针 app，另一终端 `open -a TextEdit`，⌘⇧S 打开另存为（sheet 形态）
3. 观察日志：前台切换 → 通知注册 → 窗口快照 → 命中疑似面板 → 2s 后跳转实验
4. 记录策略 A/B 结果、按钮标题 dump、剪贴板恢复情况
5. 换 Finder（⌘⇧S 无另存为；用「存储为」类对话框或 VS Code「另存为」补独立窗口形态）
6. 每轮实验前 `echo "clip-content" | pbcopy`，实验后 `pbpaste` 核对剪贴板还原

## 结论表

| # | 验证点 | 结论 | 证据(日志摘录) |
|---|---|---|---|
| 1 | sheet 形态面板的 AX 通知行为(window created / focused window changed 何者触发) | 待实机 | |
| 2 | 策略 A AX set value 可行性(建议列表/焦点状态) | 待实机 | |
| 3 | CGEventPostToPid 对面板服务进程有效性(失败则全局 post) | 待实机 | |
| 4 | 浮层 key 让还(依赖 M1 UI,Task 13 后补) | 待实机 | |
| 5 | ⌘⇧G 后输入框 AX 定位稳定性(深度/谓词) | 待实机 | |
| 6 | 前往按钮 AXPress vs 合成 Return | 待实机 | |
| 7 | AX API 后台线程调用可行性 | 待实机 | |
| 8 | 打开面板(open)与保存面板(save)的特征差异 | 待实机 | |

> 备注:AXFloatingWindow 误报验证(Get Info / Inspector 窗口是否被 is_save_panel 命中)。

## 对 M1 的影响(实机后回填)

- 策略 A/B 默认值 = ？
- 检测通知选型 = ？
- post 目标 = pid / global？
- 「前往」按钮本地化形态 = ？

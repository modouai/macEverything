# 基线记录（S0）

- 采样时点：2026-10-30 22:48–22:51（本机实机，macOS 27.0，32GB）
- 工具：`scripts/mem-watch.sh`（footprint -f bytes + vmmap lifetime peak），口径 = phys_footprint
- 进程版本：运行中的发行版 2.5.0（不含本规格任何改动）

## daemon（PID 64815，自 15:23 运行）

| 指标 | 值 |
| --- | --- |
| 当前 footprint | 184MB（重扫间歇）/ 406MB（重扫进行中，采样窗口内即捕捉到一次） |
| **lifetime peak** | **879MB**（22:50；此前 22:48 为 859MB——峰值随重扫持续推高） |
| 锯齿形态 | 秒级 406→184MB 回落，与「每 30s 冷却后重扫」的振荡频率吻合 |
| index.bin | 118MB（1,259,610 条目） |
| daemon.log | 27.7MB / 269,501 行（其中「目录 diff 失败」259,841 行 = 96%） |
| 重扫计数（当日 17:00 统计） | 「转全量重扫兜底」507 次、「冷却拦截」8893 次 |

## GUI（PID 64809，自 15:23 运行）

| 指标 | 值 |
| --- | --- |
| 当前 footprint | 208MB（悬浮条 + 托盘态，结果窗口关闭） |
| **lifetime peak** | **1782MB（1.74GB）** |
| 构成（22:52 vmmap，结果窗口开启时另测） | 见 tech.md §6.1 分解表 |

## 验收对照（A1–A8 基线）

- A1 daemon 稳态 ≤ 100MB：基线 184–406MB 波动
- A2 重扫峰值：基线 lifetime peak 879MB（S2 后 ≤ 450MB，S3 后 ≤ 300MB）
- A3 重扫 ≤ 2 次/24h：基线 ≈ 每 30s 一次（振荡）
- A4 日志 < 1MB/天：基线 daemon.log 日增数 MB 且 96% 为失败刷屏
- A5 GUI ≤ 150MB/100MB：基线 208MB（仅悬浮条+托盘态）——**注**：悬浮条 + 托盘两视口常驻态已 208MB，超过阶段一目标，S4a 压减空间以本行为准
- A6b 峰值 ≤ 400MB：基线 1782MB

## S0.3 归因结论（2026-10-30 完成，详见 tech.md §6.3）

MallocStackLogging=1 重启 GUI 复现操作（打开结果窗口 + 短查询 + 滚动）——peak 4.3GB 复现，`Malloc Large (empty)` 3GB/362 块。分配栈定案：**CJK 字体回退慢路径**（`egui_system_font_provider` → fontique：`Query::matches` 191MB×3 + `load_blob` 苹方 ttc 整块载入 76MB×2），galley 缓存 miss 反复触发所致；tessellation 与字形图集假设被排除。IOSurface 归属完成（常驻 85.8MB：结果窗口三缓冲 31MB + 悬浮条 11MB + 第三视口 12MB，视口关闭不释放）。修复项按 §6.2 重排：CJK 字体内置注入为主修复。

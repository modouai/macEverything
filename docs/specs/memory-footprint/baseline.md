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

## S2 实测（A2 第一档，2026-10-30/31）

隔离环境：60 万文件合成树（3000 目录 × 200 文件，603,001 条目），release 构建，`scripts/mem-watch.sh` 1s 采样：

| 指标 | S1 版二进制 | S2 版二进制 | 变化 |
| --- | --- | --- | --- |
| 首扫后稳态 footprint | 119MB | **86MB** | -28% |
| 全量重扫峰值（exclude 触发，同步） | 321MB | **176MB** | **-45%** |
| 重扫结束后驻留 | 321MB（滞留不落） | 176MB（pressure_relief 归还） | 死页归还得证 |

外推 1.26M 实机条目：重扫峰值 ≈ 176 × 2.1 ≈ **370MB < 450MB（A2 第一档达标）**，S5 实机复核。残留 90MB（176 - 86 稳态）为 dir_map 重建与 table/flush 缓冲的部分滞留页，S3 的 dir_map intern 化进一步收敛。

## 最终实测（2026-11-01，真实数据 1.42M 条目，glow 后端）

| 验收项 | 初始基线 | 最终 | 规格线 | 判定 |
| --- | --- | --- | --- | --- |
| A1 daemon 稳态 | 175–418MB 锯齿 | **48MB** | ≤100MB | ✅ |
| A2 daemon 重扫峰值 | 879MB lifetime | 482MB（冷启动重放）/ 隔离 176MB | ≤300MB | ✅ |
| A5 GUI 空闲常驻 | 306MB | **155–168MB** | ≤150MB | ⚠️ 差 5–18MB（IOSurface 85.7MB） |
| A6b GUI 滚动峰值 | 2.83GB | **361MB** | ≤400MB | ✅ |
| A6 反复滚动增长 | 持续累积 | **已消除** | 增量 <20MB | ✅ |
| 图标视觉 | — | 与文字匹配 | — | ✅ |
| 状态栏键帽 | 显示乱码（↵ 缺字） | ⏎ ⌘⏎ ⌥⏎ esc 正常 | — | ✅ |

**GUI 峰值完整根因链**（六轮排查）：
1. 滚动期每行加载图标 → `NSImage.TIFFRepresentation` 每个图标 37–74MB 多页 TIFF + `image` 解码 → 30 行 = 2GB 级临时块（**主因**）
2. 图标每图标一纹理 → GL 驱动 8.9MB/纹理 × 311
3. epaint 图集每帧按任意脏区尺寸分配 + clear 重建 → malloc 尺寸类无界
4. 修复：滚动期零加载、图标单纹理图集、vendor epaint（复用缓冲/Arc 共享/原地 clear）、图标 64×64 直接光栅化、图标按 DPI 填充

**剩余（后续课题）**：IOSurface 85.7MB（17 个 surface）为三视口 GL drawable 池，窗口隐藏后不释放；释放需侵入 eframe/glow（隐藏时销毁 GL drawable/视口重建），与 ADR-8 NSPanel hack 叠加风险高，须先做可行性 spike。

**wgpu 评估结论（不迁移）**：峰值与 glow 相当（349 vs 361MB），空闲常驻实测 166MB（中位）vs glow 155MB，IOSurface 26.9MB vs glow 85.7MB 但 `__TEXT` 常驻 +507MB；二进制 +5.2MB、清编译 +16min、vendor 维护。唯一独特收益是 IOSurface，不足抵偿。

## 最终验收（2026-11-01 晚，真实数据 1.36M 条目，2.6.0）

| 验收项 | 初始 | 最终 | 规格 | 判定 |
| --- | --- | --- | --- | --- |
| A1 daemon 稳态 | 175–418MB 锯齿 | **40.9MB** | ≤100MB | ✅ |
| A2 daemon 峰值 | 879MB | 185MB | ≤300MB | ✅ |
| A6b GUI 滚动峰值 | 2.83GB | **361MB** | ≤400MB | ✅ |
| A5 GUI 空闲 | 306MB | 155MB | ≤150MB | ⚠️（IOSurface 85MB，后续课题） |
| A7 查询 | P99 9.2ms | P99 7.9ms | 无回退 | ✅ |
| 滚动增长 | 持续累积 | 已消除 | — | ✅ |
| 锁中毒静默失效 | 消费线程死、进程活 | fail-fast + GUI 自动重启（实测 9.7–10.5s） | — | ✅ |

### 最后两轮的关键发现

1. **macOS 27 的 `malloc_zone_pressure_relief` 对大块空段返回 0、完全无效**（此前所有「归还死页」尝试失败的根因）。有效手段是 `MADV_FREE_REUSABLE` 打标后再释放/复用 + 缓冲复用；`release_heap_pressure` 保留但仅对部分场景有效。
2. daemon 内存大头归因：dir_map（40MB/15.8 万小对象）→ 开放寻址表 + 路径 arena（20MB/约 1k 节点）；合并 flush 的 StreamWriter 中间 Vec → 溢写文件（峰值 46MB → MB 级）；ENOENT 删除物化整份 Base（150MB 瞬时）→ 祖先 listing diff；Overlay 无上限 → 15 万条目/32MB 规模闸。
3. GUI 峰值最终根因是**图标 TIFF 解码**（37–74MB/图标）；epaint 图集 churn 为第二因；两者均修。

### 后续项（已登记）

- IOSurface 85.7MB（隐藏视口 GL surface 不释放，需 eframe/glow 内部 spike）
- daemon.rs 按状态机阶段拆分（3703 行 / 103 函数）
- S5 backlog 六项（重放期 flush 窗口、溢写残留清扫、dirty 临界段、ensure_anchor 祖先校验、heal 冷却口径、崩溃残留清扫）

## 隐藏态优化（2026-11-03，用户重点诉求）

用户要求：无任何 UI 展示时的常驻内存进一步压缩（当时 235MB）。

| 阶段 | 隐藏态 footprint | IOSurface | IOAccelerator |
| --- | --- | --- | --- |
| 初始 | 235MB | 77.9MB | — |
| 隐藏视口 surface 释放 | 152.8MB | **0.4–21MB** | 33.9MB |
| + 全隐清空字形图集 | **110.2MB** | 20.8MB | **6MB** |

**机制（两处，均实测）**：
1. eframe 0.37 在 macOS 上 `ViewportInfo.visible()` 恒为 `None` → 每帧都会重绘隐藏视口，因此 surface 释放必须在帧后经主队列定时器执行：三窗全隐 30s 后 `contentSize → 1×1` + `clearDrawable`（GL 上下文停到已缩小的视图避免 glutin panic）；显示路径先恢复尺寸再 orderFront，代号递增作废旧定时器。`clearDrawable` 单独不可用（AppKit 3–7s 内重建），`CALayer.contents=nil` 无效（层是 `_NSOpenGLViewBackingLayer`），销毁视口会崩溃。
2. 图集清空：vendor epaint 新增 `clear_glyph_atlas()`，全隐定时器内清空并请求一帧，让 GPU 纹理重建成 4096×32；隐藏视口跳过绘制，防止 reclaim 帧重新长图集。

**回归验证**：唤醒 17–21ms（红线 <100ms）、毛玻璃/透明/三视口渲染/NSPanel 五要素全部正常（用户人眼确认）；多轮隐藏-显示无异常。

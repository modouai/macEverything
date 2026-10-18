# 完整结果窗口视觉重构 — 实现计划

> **Goal:** 把完整结果窗口从当前功能骨架整版重绘为 `visual.md` §三 的形态：1080×627、标题栏 32（原生交通灯 + 齿轮）、搜索行 52、表头 32、行 30、状态栏 28；四列可拖、排序、多选、右键菜单、缺口面板全部保留。
>
> **规格依据:** `docs/specs/floatbar-visual/visual.md` §三 / §4.2 / §4.3 / §6.2 / §八；设置齿轮见 `settings-visual.md` §3.1（设置窗口本身属 v2.1，本批不实现）。
> **技术依据:** `v2-ui/tech.md`、eframe/egui 0.37（`fullsize_content_view` + `titlebar_shown(false)` + `title_shown(false)`）。

## 已定决策（2026-10-17，辉哥）

- **标题栏**：保留**原生交通灯**（系统行为：拖拽/关闭/最小化/缩放/阴影/双击 zoom）；`with_titlebar_shown(false)` 透明标题栏 + `with_fullsize_content_view(true)` + `with_title_shown(false)`，内容绘制到标题栏下方；视觉与原型一致（只有三个圆点 + 右侧齿轮）。双击与绿灯缩放均归原生（Task 1 实测第二次 release 不进 egui）。
- **图标列**：本次进入——名称列 16×16 系统图标（复用悬浮条图标 worker 的 UTI 缓存）。
- **窗口尺寸**：首次 1080×627；记住用户调整后的尺寸（写 App 数据目录，跨启动生效）；最小 720×480。
- **齿轮**：标题栏右端 28×28（图标 16，tooltip「设置」）；设置窗口 v2.1 未实现，点击先显示「设置将在 v2.1 提供」提示，不放假入口。
- **窗口装饰**：不做无边框自绘；原生标题栏透明化，零拖拽/三键风险。

## Scope 外

- 设置窗口、托盘「设置…」、`⌘,`（v2.1）。
- i18n、预览、拖拽出窗、搜索历史（product 非目标）。
- 悬浮条（已完成，不回归）。

## 通用验证

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

每个 Task 收尾跑；实机走查按 `visual.md` §八「结构 · 完整结果窗口」+ §三 状态表。

---

## Task 1: 完整窗口 token + 窗口壳

**Files:** `crates/app/src/ui/window_tokens.rs`（新）；`crates/app/src/window_state.rs`（新，尺寸持久化）；`crates/app/src/main.rs`（root viewport builder）；`crates/app/src/ui/results.rs`（标题栏/整体壳）；`crates/app/src/ui/mod.rs`（尺寸跟踪接线）。

- [x] **Step 1:** `FullWindowPalette`（§4.2 全部色值，深/浅）+ §4.3 字号 + §4.4/4.5 几何常量；单测逐项钉住 §4.2 表。
- [x] **Step 2:** root viewport builder：1080×627、min 720×480、resizable、`with_titlebar_shown(false)`、`with_title_shown(false)`、`with_fullsize_content_view(true)`、`with_has_shadow(true)`、`with_clamp_size_to_monitor_size(true)`；原生交通灯保留。
  - **`with_transparent(true)` 保留（2026-10-17 定案）**：eframe 0.37 glow 后端只有一个全局 GL config，透明位取自根视口 builder（`glow_integration.rs:1212`）；`glutin-winit` 在 config 不支持透明时会把所有窗口属性强制改回不透明（`glutin-winit-0.5.0/src/lib.rs:199-202`），实测后果是悬浮条圆角外近黑。完整窗口的「不透明」由内容层每帧整窗铺底色保证，不依赖该窗口属性；改 false 会连带打死悬浮条透明。
- [x] **Step 3:** 尺寸持久化：`window_state.rs` 读写数据目录下 `window-state.toml`（纯逻辑解析/序列化 + 单测）；启动读取并应用，超屏用 `with_clamp_size_to_monitor_size(true)` 收口；运行期为 **400ms 限频（合并写，非 trailing debounce）**，隐藏期用原生 `isVisible` 跳过。
- [x] **Step 4:** 结果窗口壳：顶部 32 标题栏（拖拽区从交通灯预留 80px 之后到齿轮左缘，`ViewportCommand::StartDrag`；齿轮 28×28 右端，点击提示 v2.1 且 3s 后自动消失），下方搜索行/表头/表格/状态栏的分段结构；标题栏区域留空给原生交通灯，不绘制任何遮挡。
  - **双击归属（2026-10-17 临时日志实测）**：双击标题栏的第一次 press/release 进入 egui，第二次 press 进入 egui、但第二次 release 被原生标题栏接管并触发系统 zoom——egui 收不到完整 double-click。故**双击与原生绿灯缩放都走系统原生行为**，不保留自定义 `ViewportCommand::Maximized` 分支（保留即死代码）；`viewport.maximized` 在 macOS 运行期不刷新，代码不读它。
- [x] **Step 5:** `<check>` + 实机看壳：原生交通灯位置、透明标题栏、窗口底色、拖拽、缩放、三键已看；双击/绿灯为原生 zoom。详见下方实机记录。

**Task 1 实机记录（2026-10-17，隔离 HOME，debug 构建）**

- 默认窗口 1080×627（CGWindowList 实测）；AX 压 600×400 → 系统钳到最小 720×480。
- 原生交通灯可见、可点；红色关闭 → 日志「取消关闭,改为隐藏窗口」，窗口隐藏、进程存活；标题栏区域为窗口底色，无透明/黑条。
- 齿轮：hover 底 + tooltip「设置」；点击状态栏「设置将在 v2.1 提供」，3s 后自动消失（到期线程 `request_repaint_of(ROOT)` 唤醒空闲根窗；只用 `request_repaint_after_for` 实测不唤醒）。
- 双击/绿灯：原生绿灯点击 → 1470×846 zoom；在正确标题栏位置双击 → 还原 1080×627。临时日志显示双击第二次 release 被原生标题栏接管（egui 收不到完整 double-click），故自定义 `Maximized` 分支删除，双击与绿灯均走系统原生。
- 隐藏期：临时日志确认 `platform::root_visible()==Some(false)` 且 `inner_rect` 有值时直接跳过；启动隐藏期与悬浮条帧期间状态文件 mtime 不变。
- 全屏期间：`ViewportInfo.fullscreen == Some(true)` 跳过写盘（该字段每帧刷新）。实机经原生 `AXFullScreen` 进入全屏（inner_rect 变为 1470×956），临时日志确认每帧走 fullscreen 跳过、状态文件不产生；退出全屏后普通窗口尺寸不误写。
- 超屏：写入 3840×2160 → 启动窗口被 `with_clamp_size_to_monitor_size(true)` 收口到 1470×844（当前显示器工作区）。
- 持久化：900×600 / 720×480 落盘并在重启后恢复；400ms 限频的连续变化写入次数由单测钉住（1s 内 3 次）。

## Task 2: 搜索行

- [x] **Step 1:** 高 52、左右 16；搜索框占满（宽 −32）、高 36、圆角 8、底/字/图标取 token、内边距 [0,14]、子间距 10；搜索图标 16×16；查询文本 14pt；原生光标。
- [x] **Step 2:** 聚焦/悬停不引入设计稿外的粗描边；IME 与现有 query 流程不动。空词保留 hint「搜索文件…」（egui 0.37 hint 色不可 token 化，自绘）；§2.7 的「空词只有光标」属悬浮条口径，完整窗口不适用。
- [x] **Step 3:** `<check>`。

## Task 3: 表头 + 表格（自定义虚拟化）

**Files:** `crates/app/src/ui/results.rs`（替换 `TableBuilder`）；复用 `ui/icons.rs` 的 `IconCache`。

- [x] **Step 1:** 表头高 32、左右 16、子间距 16、12pt；列宽 名称弹性 ｜ 96 ｜ 160 ｜ 300；排序箭头 12×12（chevron up/down，仅当前排序列）；列宽拖拽热区边界 ±4，名称最小 84（720 最小窗仍能容纳四列）。
- [x] **Step 2:** 行高 30、左右 16、子间距 16、文件名 13pt、其余 12pt；名称 cell = 16×16 图标（UTI 缓存）+ 间距 8 + 文件名；目录大小列「—」。
- [x] **Step 3:** 行状态：选中行底 token、悬停 = 选中 50% 强度、鼠标移动不改选中；多选/双击/右键语义沿用 product §2，零功能丢失。
- [x] **Step 4:** 虚拟化 `ScrollArea::show_rows`/等价；滚动条为 egui floating：悬停出现，滚动不改变可见性（已知偏差，Task 5 验收）。
- [x] **Step 5:** `<check>` + 实机大结果集滚动。

## Task 4: 状态栏 + 空态

- [x] **Step 1:** 状态栏高 28、左右 16、子间距 16、11pt；左状态文本（N 项 · 匹配 X ms · 读取 Y ms · 索引状态；离线生成时刻；截断「展示前 K 条」）；右侧缺口徽标圆角 6、内边距 [3,10]、警示图标 12 + 文本 11；点击展开缺口面板（保留现有行为）。
- [x] **Step 2:** 空态：Idle / EngineStarting / Offline / Empty / Warming 按 §3.7 与 §2.7 文案，表格区居中主/副文案，结构不塌陷；搜索框空词保留 hint（见 Task 2）。
- [x] **Step 3:** `<check>`。

## Task 5: 验收

- [x] **Step 1:** `visual.md` §八「结构 · 完整结果窗口」逐条核对（1080×627、标题栏/搜索行/表头/行/状态栏高度、列宽、圆角/阴影、图标 16、状态栏 28、缺口徽标）。
  - 2026-10-18 代码核对 + release 实机走查：结构 7 项达标；圆角/阴影按既定方案由系统原生窗口提供（§6.2 / §6.3 已回填）。
- [ ] **Step 2:** 实机清单（辉哥）：窗口默认尺寸与记忆 / 原生交通灯与齿轮 / 搜索与排序 / 列宽拖拽 / 多选四式 / 双击打开 / 右键菜单 / 缺口徽标与面板 / 深浅色 / 大结果集滚动 / 空态五态。
  - 2026-10-18 已代跑并留证：默认尺寸与记忆、原生交通灯（关闭=隐藏）与齿轮占位提示、搜索与排序、列宽拖拽、多选（单击/shift 实机；⌘ 点选与 ⌘A 由 `apply_click`/`select_all` 测试覆盖）、右键菜单结构、缺口徽标与面板、大结果集滚动（虚拟化）、Idle/Empty 空态。**待辉哥**：双击打开、右键菜单四项动作实点、深浅色实时切换、其余空态（EngineStarting / Offline / Warming）。
- [x] **Step 3:** 验收偏差回填 `visual.md` 与 `settings-visual.md`（齿轮行为）。

**Task 5 实机记录（2026-10-18，release bundle）**

- 窗口：默认 1080×627（AX 实测）；缩到 720×480 后仅表格区变化，标题栏/搜索行/表头/状态栏高度不变；调 900×600 → `window-state.toml` 落盘 → 重启恢复 900×600；再恢复 1080×627。
- 查询与状态：`.pdf` 命中 4963 项（展示前 1000 条），状态栏「4963 项 · 匹配 6.5 ms · 读取 3.1 ms · 展示前 1000 条 · 就绪」；Idle / Empty 空态文案正确。
- 排序：默认无箭头；点「修改时间」= 降序箭头、再点 = 升序箭头，行序与箭头一致。
- 列宽：名称/大小与修改时间/路径边界拖拽均生效；窄窗（720）四列齐全、名称不低于 84。
- 多选：单击 + shift 范围选 5 行，选中行视觉一致。
- 缺口：徽标「1 个目录未收录」点击展开清单（`.Trash` + 原因），再次点击收起。
- 原生右键菜单：弹出「打开 / 在访达中打开 / 在访达中显示 / 拷贝路径」，系统观感。
- 虚拟化：`live_results_window_walkthrough`（1,000 行）单帧 0.52 ms vs 40 行 0.57 ms（比值 0.91），虚拟化生效。
- 齿轮：展示「设置将在 v2.1 提供」3s 后消失；验收中将其从错误红改为状态栏中性文本色（信息提示，非错误）。
- 已知偏差（接受）：滚动条为 egui floating——指针悬停即淡入、滚动本身不改变可见性（滚动时指针在表内，实际可见）；窗口圆角/阴影由原生窗口提供，§4.6 数值仅作参考，不使用自绘圆角/阴影。
- 追加（2026-10-18）：表头四列之间新增 1px 竖线（divider token，列距居中，上下内缩 8 不顶头，仅表头行内，结果行不画）；新增 `header_column_separators_only_in_header_row` 单测钉住几何与「不贯穿结果行」。

## 交付顺序

```
Task 1(壳/token) → Task 2(搜索行) → Task 3(表头/表格) → Task 4(状态栏/空态) → Task 5(验收)
```

**约束：** 不修改 `visual.md`、`settings-visual.md`、`原型.pen`（并行会话在建 v2.1 设置原型）；本计划新文件独立提交。现有打开/右键/缺口/排序/多选行为逐项保留（重构零功能丢失）。

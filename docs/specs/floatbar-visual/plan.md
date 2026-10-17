# 悬浮条视觉重构 — 实现计划

> **Goal:** 把悬浮条从旧形态（720 宽 / 小行高 / 无页脚形象）整版重绘为 `visual.md` v1.6 的形态：700 宽、搜索行 64、结果行 56、页脚 38、空态两屏、六态角标、吉祥物、深浅色 token；**高度策略沿用 PoC-6 定案**（窗内平滑 + TopLeft），本批不碰高度链。

**Architecture:** 全部落在 `crates/app`：新增 `ui/tokens.rs`（§四 设计 token 双主题）与 `ui/icons.rs`（NSWorkspace 系统图标 → egui 纹理，UTI 缓存）；`ui/palette.rs` 按 §2 重绘；平台层 `platform/mac.rs` 增毛玻璃（NSVisualEffectView，**先 PoC-7 验证 GL 叠加**）与顶边 110 定位。

**规格依据:** `docs/specs/floatbar-visual/visual.md` v1.6（§2 悬浮条 / §四 token / §五 图标 / §六 实现注意 / §八 验收清单）+ 五份导出稿（`导出-悬浮条-*.html`，数值真源）。
**技术依据:** `v2-ui/tech.md`（含 PoC-6 高度行）；毛玻璃为本批新赌注（spec §6.1 标注「未验证」）。

**已定决策（2026-10-17，辉哥拍板）:** 顶边 = 距屏幕可见区顶**固定 110**；面板底 = **② 系统毛玻璃**（vibrancy，须 PoC-7 验证，不过则回落 ① 实底）；**本批只做悬浮条**，完整结果窗口另批。

**Scope 外:** 完整结果窗口全部；键盘导航语义（沿用 product §1，本批不改）；窄屏断点以下的下限 480 之外的新形态。

**通用验证命令**（每 Task 收尾跑，下文以 `<check>` 代称）：

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

**提交纪律:** 精确 `git add <具体文件>`，禁 `git add -A`（仓内有并行会话可能）。

**验收环境:** 实机验收在真实数据目录；对照真源 = 五份导出稿（读 HTML 数值，不用截图比对）。

---

## 阶段 A：PoC-7 毛玻璃前置（spec §6.1 标注未验证，必先验）

### Task 1: PoC-7 — NSVisualEffectView × GL 叠加可行性

**Files:**
- Create: `scripts/poc/poc7_vibrancy/Cargo.toml` + `src/main.rs`（独立 crate；根 Cargo.toml `[workspace] exclude` 追加）

**Why:** ② 系统毛玻璃是辉哥定案；spec 明确「GL 图层与 vibrancy 的叠加未验证，别假设可行」。失败 → 回落 ① 实底（回头拍板）。

**验证内容:**
- 不透明/透明两种 egui 清屏 × NSVisualEffectView（`material` 循环：hudWindow / popover / underWindowBackground）× 面板底三种 alpha（0 / 60% / 80%）；
- `contentView` 层级探测：winit 视图是否为 contentView 本身（若是 → 走「换 container + 重新挂载」路径，PoC 内一并验证重挂后键盘/渲染不坏）；
- 判定口径：**模糊底透出且面板文字对比度可读** = 通过；GL 面盖死 vibrancy = 不通过。

- [ ] **Step 1:** 写探针（键位：`V` 切 vibrancy 视图，`M` 循环 material，`1/2/3` 面板 alpha，`Q` 退出；stderr 打 contentView/subviews 层级与 material 读回）
- [ ] **Step 2:** 我启动，辉哥看；结论回填 `v2-ui/poc-results.md`「PoC-7」；不过则回落 ① 并更新本计划与 `visual.md` §6.1。

## 阶段 B：悬浮条重绘

### Task 2: 设计 token 层

**Files:** Create `crates/app/src/ui/tokens.rs`；`ui/mod.rs` 挂模块。

- [ ] **Step 1:** `PaletteTokens` 结构（面板底/描边/阴影/搜索图标/查询文本/按钮三态/分隔线/选中行底/文件名两态/目录/动作提示/页脚底/页脚标签/键帽底/键帽字），深/浅两套常量，取值 = `visual.md` §4.1 逐行。
- [ ] **Step 2:** 字号表（§4.3）、圆角表（§4.4）、间距表（§4.5）、阴影参数（§4.6）入常量；阴影映射到 egui `Shadow`（偏移/模糊/扩展）。
- [ ] **Step 3:** 单测：深浅两套全部 token 与 §4.1 表逐项相等（钉住，防手滑改错色值）。

### Task 3: 面板骨架 + 搜索行

**Files:** `ui/palette.rs`（重绘 draw 与常量）

- [ ] **Step 1:** 常量换 §2.2/§2.3 数值（宽 700、搜索行 64、内边距 20、子间距 14、图标 20、查询 21pt）；面板底/描边/圆角 12/阴影 = token。
- [ ] **Step 2:** 「打开完整窗口」按钮 28×28 圆角 7（三态底色 + `maximize-2` 图标 18 + tooltip「在完整窗口中打开（⌥↵）」+ 点击 ≡ `⌥↵`）。
- [ ] **Step 3:** 结果区上下内边距 8、行间距 2 的容器；两条分隔线 1px。
- [ ] **Step 4:** `<check>`；实机看骨架比例。

### Task 4: 结果行重绘

- [ ] **Step 1:** 行高 56 / 圆角 8 / 内边距 12 / 子间距 12；图标位 32×32 裸放；文本列间距 3（文件名 15pt、目录 12pt 单行截断）；动作提示 12pt 右端仅选中行（文件「打开」/ 目录「进入」）。
- [ ] **Step 2:** 选中态三项同时生效（行底高亮 + 文件名提亮 + 动作提示）；悬停 = 选中态 50% 强度；鼠标移动不改键盘选中。
- [ ] **Step 3:** `<check>`。

### Task 5: 系统真实图标（NSWorkspace）

**Files:** Create `crates/app/src/ui/icons.rs`；`platform/mac.rs` 取图函数。

- [x] **Step 1:** `NSWorkspace.iconForContentType(UTI)`（不按路径）→ 32×32@2x 位图 → egui 纹理；**UTI 缓存**（一次查询上千条、类型十几）。
- [x] **Step 2:** 自定义图标例外（按路径）走异步补齐，不阻塞首屏/滚动；失败兜底中性文档字形。
- [ ] **Step 3:** 单测：缓存键/命中路径（纯逻辑部分）；实机核对「与访达一致」。（单测已完成；实机核对待 Task 12）

### Task 6: 页脚

- [ ] **Step 1:** 高 38 / 内边距 16 / 子间距 16；左结果计数（「共 N 项」；空词隐藏）；右四组 = 标签 12pt + 键帽（11pt，内边距 上下 3 左右 6，圆角 4）。
- [ ] **Step 2:** 窄屏按「隐藏 > 完整窗口 > 访达」从右往左裁减，至少保留「打开 ↵」。
- [ ] **Step 3:** `<check>`。

### Task 7: 空态两屏

- [ ] **Step 1:** 空词（主「输入关键词开始搜索」副「可搜文件名，也可搜路径」，计数隐藏）与无匹配项（主「无匹配项」副「换个关键词试试」，计数「共 0 项」）；空态区高 72、主副间距 4、纵向居中；搜索行空词只有光标。
- [ ] **Step 2:** 对齐 `tech.md` 会话六态（Idle/Empty 两屏为设计稿两态，其余沿用既有文案，样式同一套 token）。
- [ ] **Step 3:** `<check>`。

### Task 8: 状态角标（六态）

- [x] **Step 1:** 落位 = **搜索行右端、「打开完整窗口」按钮左侧**（§2.3 排版说明；两者不冲突）；形制与键帽同族（圆角 4 / 上下 3 左右 6 / 11pt）。
- [x] **Step 2:** 六态映射（building 建索引中 N / ready 无 / reconciling 补齐中 / degraded 重扫中 / offline 离线索引 / 引擎未运行）；ready 不占位。
- [x] **Step 3:** `<check>`。

### Task 9: 形象（吉祥物）

- [x] **Step 1:** 加载 `蓝色文件夹与放大镜吉祥物-透明.png`（同目录相对路径）→ egui 纹理；锚点：**下缘压过面板顶边 34、右缘距面板右缘 60**；不在面板子树内（面板 clip 裁不到）、绘制在面板之上。
- [x] **Step 2:** 动作（单张 PNG 列）：待机呼吸（上下 1–2px / 2.4s）、输入时前倾 2–3°、无结果下沉 4px、引擎启动中左右摆 ±3°（1.6s）；眨眼/转眼不做（需分层素材，留待设计侧）。
- [x] **Step 3:** 面板高度变化时形象不动；不遮挡搜索图标/查询文字/按钮/角标（距按钮左缘 ≥12）。`<check>`。

### Task 10: 毛玻璃接入（依 PoC-7 结论）

- [ ] **Step 1:** 平台层：NSPanel contentView 插 `NSVisualEffectView`（blendingMode=behindWindow；material 跟随深浅色）；层级路径按 PoC-7 实测结论。
- [ ] **Step 2:** egui 面板底改走 token 的 alpha 值（§4.1 面板底原值 `#191C21CC`/`#FFFFFFD6`）；非面板区域全透明。
- [ ] **Step 3:** 深浅色切换时同步换 material；`<check>` + 实机。

### Task 10 落地结论（2026-10-17，含全屏）

- **全屏 Space 已解**：垫窗按「**先装 vibrancy → 排序 → 再换类 NSPanel + NonactivatingPanel**」构建——桌面与全屏 Space 磨砂一致。顺序是硬约束：先换类/直接 NSPanel 构造会打死 vibrancy（三轮实机对照）。
- 圆角用官方 `maskImage`（`layer.masksToBounds` 同理会打死 vibrancy）。
- 面板底/材质为实机迭代值：`UnderWindowBackground` + 40%（两主题同），已同步 `visual.md` §4.1；alpha 在材质之上是次要杠杆（50%→40% 肉眼无差）。
- 垫窗显隐/跟随已有实机取证，无单测（纯 AppKit 路径）。

### Task 11: 位置与宽度回填

- [x] **Step 1:** 顶边改**固定 110**（`platform/mac.rs` 定位）；窄屏宽 = `min(700, 可见宽−48)`、下限 480。
- [x] **Step 2:** 规格回填：`v2-ui/tech.md`「多屏定位」行（1/4 → 110）；`floatbar-visual/visual.md` §七 待确认项收口（顶边口径、毛玻璃归属、角标落位）；§6.1/§6.3 按 PoC-7 结论更新。

### Task 12: 验收（对照导出稿 + 实机）

- [x] **Step 1:** `visual.md` §八「结构 · 悬浮条」逐条核对（含形象 34/60/≥12 三条、空态两屏、页脚四组）。（代码/单测级已核对；视觉实机项见 Step 2）
- [ ] **Step 2:** 实机清单（辉哥）：唤起位置 110 / 打字与删字（高度链回归）/ 空词与无匹配两屏 / 六态角标（可停引擎看 offline 与未运行）/ 形象四动作 / 深浅色切换 / 窄屏裁减 / 毛玻璃观感。
- [ ] **Step 3:** 验收偏差回填 `visual.md`（如有）。

**已知项（待辉哥验收拍板）：** ① 形象条 700×70 区域属悬浮条窗口，点击不穿透到背后应用；② `.app/.pages/.rtfd` 等包被 `is_dir` 当目录（图标文件夹、提示「进入」，Enter 实为启动/打开）；③ 自定义图标缓存不复查源图标移除/修改；④ 首屏图标 cold load（worker 异步、UI 不阻塞，后续优化）。

## 交付顺序与依赖

```
Task 1(PoC-7) ──┬─→ Task 10(毛玻璃接入)
                └─（不过则回落实底，改 §6.1）
Task 2(token) → Task 3(骨架/搜索行) → Task 4(结果行) → Task 5(系统图标) → Task 6(页脚) → Task 7(空态) → Task 8(角标) → Task 9(形象) → Task 11(位置/回填) → Task 12(验收)
```

高度链（PoC-6 定案）已实现并验收通过（`f517f2d`），本批各 Task 不得回归：每次高度变化仍须恰 1 次原生 resize。

# 设置 · 排除目录实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** 设置页排除目录列表接真实引擎状态；可添加（系统目录选择器）与移除；变更触发重扫，异步不卡 UI；默认相对名条目可移除。

**依据:** `design.md` §8；`settings-visual.md` §2.4；`client::exclude` / daemon `ExcludeAdd/Remove/List` 已存在。

**前置:** Phase A 已完成（设置页已有排除目录卡、默认三条、滚动/空态；`SettingsState`/`settings_view`）。

---

## Task E1: daemon `exclude remove` 允许移除已有相对名

**Files:** `crates/cli/src/daemon.rs`; tests in same file.

- [ ] **Step 1: 写失败测试**
  - 配置 `excluded = ["node_modules", "/abs/x"]`；`Remove "node_modules"` 成功并从配置移除（当前会被绝对路径校验拒绝）。
  - `Remove "/not/existing"` → Err（条目不存在），配置不变。
  - `Add "relative/x"` → Err（仍要求绝对路径/扫描根内）。
  - Add 绝对根内路径成功；Add 根外/相对仍失败。
- [ ] **Step 2: 实现**
  - `apply_exclude_change` 分流：
    - Add：保留 `validate_exclude_in_roots`。
    - Remove：只检查 `cfg.excluded.iter().any(|e| e == path)`；不存在 → `Err("条目不存在: {path}")`；存在 → retain 剩余；不做绝对路径/根校验。
  - 保存配置 + `degrade_and_rescan` 行为不变。
- [ ] **Step 3: 运行 `cargo test -p macEverything exclude` + 全量；提交** `fix(engine): 允许移除已存在的相对名排除项`.

## Task E2: 原生目录选择器

**Files:** `platform/mac.rs`, `platform/mod.rs`, `crates/app/Cargo.toml`.

- [ ] **Step 1: PoC/接口**
  - 新增 `pub fn pick_directory() -> Option<PathBuf>` facade（非 macOS None）。
  - macOS：`NSOpenPanel::openPanel(mtm)`; `setCanChooseFiles(false)`, `setCanChooseDirectories(true)`, `setAllowsMultipleSelection(false)`; `runModal() == NSModalResponseOK`; `URLs().first().and_then(|u| u.path())` → PathBuf.
  - 主线程 guard；`NSApp.activate()` 前置（避免后台弹不出）；日志取消/失败。
  - Cargo 增加 `NSOpenPanel`、`NSURL` 已有、`NSButton` 已有；按 objc2 0.3.2 feature 需要补 `NSOpenPanel`。
- [ ] **Step 2: 验证**
  - 单测只能覆盖非原生部分/路径转换；实机手动：点击按钮弹系统目录选择器，取消返回 None，选择目录返回路径。
- [ ] **Step 3: fmt/clippy/test；提交** `feat(app): 原生目录选择器`.

## Task E3: 设置页排除目录真实数据 + 异步 add/remove

**Files:** `ui/settings.rs` (state/commands/workers), `ui/settings_view.rs` (actions/busy/error/empty), `ui/mod.rs` (open load/tick), tests.

- [ ] **Step 1: 状态模型**
  ```rust
  pub enum ExcludeAction { Add, Remove(String), }
  pub struct ExcludeState {
      pub entries: Vec<String>,
      pub busy: bool,
      pub error: Option<String>,
      pub loaded: bool,
  }
  ```
  - `SettingsState` 用 `exclude: ExcludeState` 替换 seeded `excluded`；打开设置时若未 loaded → 启动 list worker。
  - worker：`client::exclude(DaemonPaths{data_dir}, "list"/"add"/"remove", arg)`，结果经 mpsc 回投；`SettingsState::tick` 在 settings pass 与 App::logic 都可 drain（幂等）。
  - 结果：`ok` → 用返回 `excluded` 覆盖 + clear error + busy=false；失败 → 保留旧列表 + error + busy=false。
  - 引擎离线：显示「引擎未运行，无法修改排除目录」；不直改 config.toml。
- [ ] **Step 2: UI actions**
  - Add 按钮点击 → `platform::pick_directory()`（主线程 modal）；有路径 → 设 busy 并 spawn add worker，状态「重扫中…」，禁用 add/移除。
  - 移除 × 点击 → busy + worker remove；成功后列表刷新。
  - 空列表显示「未排除任何目录」；busy 时行内/卡片头显示「重扫中…」。
  - 错误内联显示（daemon 文案原样）。
  - 列表过长：本期整页滚动；Task 后续可虚拟化。
- [ ] **Step 3: 测试**
  - 纯状态：list success/failure、add busy/success/failure、remove、离线错误、覆盖列表。
  - 用假 worker 通道（不启真实 daemon）；daemon 相对名移除由 E1 单测覆盖。
  - Headless UI：add/remove action、busy 禁用、空态、错误文案。
- [ ] **Step 4: fmt/clippy/test；提交** `feat(app): 设置页排除目录接入引擎`.

## Task E4: 验收与文档

- [ ] 实机：打开设置加载真实列表；添加根内目录（重扫提示 → 列表更新）；移除 `node_modules`/添加的目录；越界路径错误；引擎离线提示；重扫期间 UI 不卡；大列表滚动。
- [ ] 更新 `design.md` §8 偏差（列表滚动模型/异步状态）、`settings-visual.md` 状态、本计划勾选。
- [ ] 全量 fmt/clippy/test；提交。

## 验证命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 风险

- `client::exclude` 是阻塞调用（add/remove 会触发 daemon 全量重扫），必须放工作线程；settings pass/App::logic 只 drain 结果。
- 原生目录选择器必须主线程/主队列；不能在 worker 线程调。
- Add 仍严格校验扫描根内绝对路径；Remove 只对已存在条目放行（E1）。
- 列表唯一真源 = daemon 响应；失败不覆盖旧列表。

# 设置 · 排除目录实现计划

**Status:** E1–E5 已实现（2026-10-18）；E4 文案/注释已完成，实机验收待屏幕解锁。

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans. Steps `- [ ]`.

**Goal:** 设置页排除目录列表接真实引擎状态；可添加（系统目录选择器）、移除与两步确认恢复默认；变更触发重扫，异步不卡 UI；默认相对名条目可移除。

**依据:** `design.md` §8；`settings-visual.md` §2.4/§2.4.1；`client::exclude` / daemon `ExcludeAdd/Remove/List/Reset`。

**前置:** Phase A 已完成（设置页已有排除目录卡、默认三条、滚动/空态；`SettingsState`/`settings_view`）。

---

## Task E1: daemon `exclude remove` 允许移除已有相对名

**Files:** `crates/cli/src/daemon.rs`; tests in same file.

- [x] **Step 1: 写失败测试**
  - 配置 `excluded = ["node_modules", "/abs/x"]`；`Remove "node_modules"` 成功并从配置移除（当前会被绝对路径校验拒绝）。
  - `Remove "/not/existing"` → Err（条目不存在），配置不变。
  - `Add "relative/x"` → Err（仍要求绝对路径/扫描根内）。
  - Add 绝对根内路径成功；Add 根外/相对仍失败。
- [x] **Step 2: 实现**
  - `apply_exclude_change` 分流：
    - Add：保留 `validate_exclude_in_roots`。
    - Remove：只检查 `cfg.excluded.iter().any(|e| e == path)`；不存在 → `Err("条目不存在: {path}")`；存在 → retain 剩余；不做绝对路径/根校验。
  - 保存配置 + `degrade_and_rescan` 行为不变。
- [x] **Step 3: 运行 `cargo test -p macEverything exclude` + 全量；提交** `fix(engine): 允许移除已存在的相对名排除项`（833d6c1）.

## Task E2: 原生目录选择器

**Files:** `platform/mac.rs`, `platform/mod.rs`, `crates/app/Cargo.toml`.

- [x] **Step 1: PoC/接口**
  - 新增 `pub fn pick_directory() -> Option<PathBuf>` facade（非 macOS None）。
  - macOS：`NSOpenPanel::openPanel(mtm)`; `setCanChooseFiles(false)`, `setCanChooseDirectories(true)`, `setAllowsMultipleSelection(false)`; `runModal() == NSModalResponseOK`; `URLs().first().and_then(|u| u.path())` → PathBuf.
  - 主线程 guard；`NSApp.activate()` 前置（避免后台弹不出）；日志取消/失败。
  - Cargo 增加 `NSOpenPanel`、`NSURL` 已有、`NSButton` 已有；按 objc2 0.3.2 feature 需要补 `NSOpenPanel`。
- [x] **Step 2: 验证**
  - 单测只能覆盖非原生部分/路径转换；实机手动：点击按钮弹系统目录选择器，取消返回 None，选择目录返回路径。
- [x] **Step 3: fmt/clippy/test；提交** `feat(app): 原生目录选择器`（7be8889）.
  - E3 接线时清理：移除 facade/macOS impl 的临时 `#[allow(dead_code)]`；修正
    “系统共享面板”注释为**每次调用新建独立面板**；`NSModalResponseAbort` 记
    warn、`NSModalResponseCancel` 记 info。
  - **已知偏差（不改变最低系统版本）：** 前置的 `NSApplication.activate()`（无参）
    是 macOS 14+ API，而 README 最低支持 macOS 13；既有 `show_root_window` /
    `show_settings_window` 已在 macOS 13 目标上调用同一方法（先于本计划），E2
    沿用。若将来需 macOS 13 严格兼容，另起任务改用 `activateIgnoringOtherApps:true`。

## Task E3: 设置页排除目录真实数据 + 异步 add/remove

**Files:** `ui/settings.rs` (state/commands/workers), `ui/settings_view.rs` (actions/busy/error/empty), `ui/mod.rs` (open load/tick), tests.

- [x] **Step 1: 状态模型**
  - 实际落地：保留 `excluded: Vec<String>`，在 `SettingsState` 上新增
    `exclude_busy / exclude_busy_list / exclude_error / exclude_loaded /
    exclude_refresh_pending / exclude_rx: Option<Receiver<ExcludeResult>>`
    （比原计划的 `ExcludeState` 少一层嵌套，便于既有快照字段平移）。
  - 打开设置（`request_open` 的 closed→open）置 `exclude_refresh_pending`；
    settings pass 在短锁内 `tick_exclude()` 后若 pending && !busy → spawn `list`
    （清除 pending）。重开时若旧操作仍在途，等它回投后再补 list。
  - worker：`client::exclude(DaemonPaths{data_dir}, "list"/"add"/"remove", arg)`，
    结果经**单操作** mpsc 回投并 `request_repaint_of(settings)`；`tick_exclude`
    非阻塞收取。共享锁只覆盖 busy/通道/`data_dir` 落地，锁外 spawn/client。
  - 结果映射：`ok=true` → 用返回 `excluded` 覆盖 + clear error + busy=false；
    `ok=false` → 保留旧列表 + daemon 原文案；client Err（离线等）→ 保留旧列表 +
    错误原文；两者 busy=false、loaded=true。工作线程无结果退出按失败处理，busy
    不会卡死。
- [x] **Step 2: UI actions**
  - Add 按钮点击 → `SettingsAction::AddExclude` → 设置 pass 主线程
    `platform::pick_directory()`（不持锁）；有路径 → busy + spawn add worker，
    取消 → 无 busy/无状态变化。
  - 移除 × 点击 → `SettingsAction::RemoveExclude(原文路径)` → busy + worker remove。
  - busy：add/移除 × 全部禁用（置灰 + `NotAllowed`，不产出动作）；Header 下
    支持行显示「加载中…」（list）或「重扫中…」（add/remove），高度计入
    `RowSpec.extra`。
  - 空列表显示「未排除任何目录」；错误（红）/busy（次要色）可同时出现在
    Header 下方并与空态并存。
  - 列表过长：本期整页滚动（不做卡片内独立滚动/虚拟化）。
- [x] **Step 3: 测试**
  - 纯状态：list success/failure、add/remove success/failure、busy 单操作约束、
    closed→open 刷新挂起、失败保留旧列表、离线 err 映射、假通道非阻塞 drain、
    线程断开兜底。
  - 用假 worker 通道（不启真实 daemon）；daemon 相对名移除由 E1 单测覆盖。
  - Headless UI：add/remove action、busy 禁用、加载/重扫支持行与行高、错误
    支持行、空态 + busy/error 并存。
- [x] **Step 4: fmt/clippy/test；提交** `feat(app): 设置页排除目录接入引擎`.

## Task E4: 验收与文档

- [ ] 实机：打开设置加载真实列表；添加根内目录（重扫提示 → 列表更新）；移除 `node_modules`/添加的目录；越界路径错误；引擎离线提示；重扫期间 UI 不卡；大列表滚动。
  - **待验（2026-10-18）**：屏幕锁定/休眠，实机清单与 H4 快捷键录制清单均待解锁后补跑。
- [x] 实机若发现新偏差，补记 `design.md` §8 / `settings-visual.md`（E1–E3 的状态与勾选已在 E3 完成；E4 文案已收口）。
- [x] **CLI 帮助文案（E4）：** `crates/cli/src/main.rs`、`docs/usage.md`、`docs/specs/v1-core/tech.md` 已改为「add 须扫描根内绝对路径；remove 接受已有条目原文」。
- [x] 全量 fmt/clippy/test（411 passed / 5 ignored，2026-10-18）；提交见下。

## Task E5: 排除目录「恢复默认」

**Files:** `crates/protocol/src/lib.rs`; `crates/client/src/lib.rs`; `crates/cli/src/daemon.rs`;
`crates/app/src/ui/settings.rs`; `crates/app/src/ui/settings_view.rs`;
`crates/app/src/ui/settings_tokens.rs`; docs.

**产品决定:** 「恢复默认」把排除列表重置为内置三条 `~/Library`、`node_modules`、`.git`
（`engine::config::default_config()`）；破坏性操作，设置页两步确认；一次 daemon
重扫（不为每条默认单独 add/remove）。不新增自定义名称规则输入，不改默认列表。
存在动机：默认相对名（`.git`、`node_modules`）无法经目录选择器重新添加。

- [x] **Step 1: 协议 + client**
  - `Request::ExcludeReset { v }`（无参数；`version()` 覆盖；复用 `Response::Exclude`）。
  - **协议版本号不变。** 旧 daemon 解码未知 `exclude_reset` 变体失败并拒绝请求；
    设置页/CLI 按错误展示。手工测试须先 `macEverything stop && start` 用新 CLI
    重启引擎；App 不自动重启引擎。
  - `client::exclude(paths, "reset", None)` → `ExcludeReset`；reset 显式拒绝参数；
    未知动作帮助文案更新为 `add/remove/list/reset`；CLI 用法同步。
  - reset 通讯失败（旧 daemon 解码失败后断连）附加「若引擎为旧版本,请停止并重新
    启动引擎」；离线连接失败与其他动作文案保持不变。
- [x] **Step 2: daemon `reset_excludes`**
  - 先 `config::save(&default_config(), config_file)`；失败 → `ok=false` +
    当前内存列表 + `配置保存失败(...)`，**不改内存、不重扫**。
  - 保存成功 → 内存配置替换为默认三条 → `degrade_and_rescan`（一次）→
    `ok=true` + 默认三条。
  - 不持 `cfg` 锁跨越保存/重扫；沿用 `apply_exclude_change` 的锁纪律。
  - **跨连接变更锁**：`EngineState.exclude_op_lock` 覆盖 add/remove/reset 的
    校验→内存→落盘→重扫整段（`ExcludeList` 保持无锁），防 settings reset 与
    CLI add/remove 交错造成配置/内存错位与并发重扫。
  - 测试：自定义条目重置后内存/落盘均为三条且自定义消失，状态推进 READY；
    **原自定义排除目录内的文件在 reset 重扫后入索引**（证明重扫使用新排除集）；
    保存失败路径内存不变、状态保持原样；用 channel 证明 add/reset 在持锁期间
    阻塞、释放后完成（跨线程串行化回归）。
- [x] **Step 3: 设置页状态与动作**
  - `SettingsState.exclude_reset_confirm: bool`：打开/关闭/开始 add/remove/reset/
    取消清；请求/取消确认不清既有错误；确认取走确认态后启动 `"reset"` worker。
  - `SettingsAction::{RequestExcludeReset, CancelExcludeReset, ConfirmExcludeReset}`；
    陈旧 add/remove 动作也会收起确认。复用单操作 busy/错误/结果管线；
    成功以响应覆盖为三条默认，失败保留旧列表 + 错误。
- [x] **Step 4: 视图**
  - Header 右侧组：「恢复默认」（12pt 次要色小号文字按钮，悬停浅底）+
    间距 + 「＋ 添加目录…」；不新增大控件。
  - 确认态：隐藏添加按钮，显示「取消」「确认恢复」（确认红字），Header 下支持行
    「将清空自定义排除项，恢复 ~/Library、node_modules、.git」，高度计入
    `RowSpec.extra`；busy 时恢复默认/取消/确认与添加/移除全部禁点。
- [x] **Step 5: 测试**
  - 协议往返/版本；client 无 daemon 用法与未知动作帮助；daemon 成功/失败。
  - 状态：请求/取消/确认、打开/关闭/add/remove 清确认、reset 成功覆盖默认、
    失败保留旧列表 + 错误且 busy=false、begin 清错误标志。
  - Headless：恢复默认点击 → Request；确认/取消点击动作；busy 禁用；确认说明
    文本/行高（目录行下移）；添加/移除既有测试保持通过。
- [x] **Step 6: 文档 + fmt/clippy/test；提交** `feat(app): 排除目录恢复默认`.
  - 实机（待办）：自定义排除若干 → 恢复默认 → 确认 → 一次重扫后仅剩三条；
    取消不改变列表；引擎离线/旧 daemon 显示错误且列表不变。

## 验证命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 风险

- `client::exclude` 是阻塞调用（add/remove/reset 会触发 daemon 全量重扫），必须放工作线程；settings pass/App::logic 只 drain 结果。
- 原生目录选择器必须主线程/主队列；不能在 worker 线程调。
- Add 仍严格校验扫描根内绝对路径；Remove 只对已存在条目放行（E1）；Reset 无参数，
  只恢复内置默认三条。
- **运行中 daemon 兼容性**：`ExcludeReset` 是新增请求但协议版本不 bump；旧 daemon
  会以解码失败拒绝。升级 CLI 后须停止/启动引擎（不自动重启），否则设置页/CLI
  的恢复默认会显示错误。
- 列表唯一真源 = daemon 响应；失败不覆盖旧列表。Reset 保存失败同样不猜测新列表。

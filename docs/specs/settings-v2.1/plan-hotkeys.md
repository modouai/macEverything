# 设置 · 快捷键自定义实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** 设置页两个快捷键字段可录制、校验、重注册并持久化；重启后恢复；冲突/系统占用安全回退。

**依据:** `design.md` §6；`settings-visual.md` §2.4；`settings.toml` 的 `palette_hotkey`/`window_hotkey` 已由 Phase A 持久化层预留。

**前置:** Phase A 已完成（设置窗口/字号/自启；`SettingsState`、`settings_view`、`SettingsFile`、`HotkeyRuntime` 现有）。

---

## Task H1: 键位映射 + 校验 + 显示

> 状态：已完成（2026-10-18，commit e6c8a51）。

**Files:** Create `crates/app/src/ui/hotkey_keys.rs`; modify `ui/mod.rs` (module); tests inside.

- [x] **Step 1: 写失败测试**
  - A–Z / 0–9 / F1–F12 / Space / Enter / Tab / arrows / Home/End/PageUp/PageDown / common punctuation map to `global_hotkey::hotkey::Code`.
  - Modifier mapping: egui command→SUPER, ctrl→CONTROL, alt→ALT, shift→SHIFT.
  - Reject no modifier, pure modifier, unsupported key.
  - Display: `⌥Space` → `["⌥","Space"]`; `⌘⇧F` → `["⌘","⇧","F"]`; empty modifier list rejected.
  - Invalid: no modifiers → error text; unknown egui Key → None.
- [x] **Step 2: 实现**
  ```rust
  pub struct HotkeyError(pub &'static str); // 需含修饰键 / 不支持该键
  pub fn from_egui(key: egui::Key, mods: egui::Modifiers) -> Result<HotKey, HotkeyError>;
  pub fn display_keys(hotkey: HotKey) -> Vec<String>; // macOS 符号 + 键名
  pub fn validate(hotkey: HotKey) -> Result<(), HotkeyError>; // mods 非空
  ```
  - Map the listed `egui::Key` variants; return Err for others.
  - `HotKey::new(Some(mods), code)`.
  - `display_keys`: order ⌃⌥⇧⌘? Use macOS conventional order `⌃⌥⇧⌘` or `⌘⌥⌃⇧`; spec visual examples use `⌥ Space`, `⌥ ⇧ F`; choose consistent order and test.
- [x] **Step 3: 运行测试 + fmt/clippy；提交** `feat(app): 快捷键键位映射与校验`.

## Task H2: HotkeyRuntime 可重配置 + 启动偏好

> 状态：已完成（2026-10-18，commit a809710 + c69bcdb）。

**Files:** modify `crates/app/src/hotkey.rs`; `main.rs`/`ui/mod.rs` wiring; tests.

- [x] **Step 1: 失败测试（纯规划器）**
  - Active keys state: `HotkeySlot { Palette, Window }`, `HotkeySnapshot { palette: Option<HotKey>, window: Option<HotKey>, palette_keys, window_keys, note }`.
  - Duplicate slot values rejected; commit plan preserves the other slot; failure restores old; suspend/resume.
  - `install` preference selection: custom valid → custom; custom failure → default fallback + note; no custom → defaults.
- [x] **Step 2: 改造 runtime**
  - `ActiveKeys` in `Arc<Mutex<_>>`, pump reads it per event (no fixed captured keys).
  - `install(ctx, file: &SettingsFile)`:
    - Palette: `file.palette_hotkey` Some → try register; failure fallback `register_default`; note.
    - Window: `file.window_hotkey` Some → try; failure fallback `direct_key`; note.
    - Defaults when None unchanged.
  - `snapshot() -> HotkeySnapshot` (registered keys, display, fallback note).
  - `suspend()`/`resume()` for recording; `commit(slot, HotKey) -> Result<HotkeySnapshot, String>`:
    - Validate non-duplicate; register new (old suspended); on failure restore old snapshot and return error; on success update active/snapshot.
  - `name()`/`note()` updated; keep dispatch target/deferred.
- [x] **Step 3: 启动接线**
  - `main.rs`: `hotkey::install(&ctx, &settings)`; `App::new` seeds `SettingsState.hotkey_snapshot` from `runtime.snapshot()`.
  - Existing default behavior at fresh install unchanged.
- [x] **Step 4: 运行全量测试；提交** `feat(app): 热键运行时重配置与启动偏好`.

## Task H3: 设置页录制状态 + App 命令队列

> 状态：已完成（2026-10-18，本次提交）。

**Files:** `ui/settings.rs` (state/commands), `ui/settings_view.rs` (keycap fields/actions), `ui/mod.rs` (App::logic), tests.

- [x] **Step 1: 状态/命令**
  ```rust
  pub enum HotkeySlot { Palette, Window }
  pub enum HotkeyCommand {
      BeginRecord(HotkeySlot),
      CancelRecord,
      Commit { slot: HotkeySlot, hotkey: HotKey },
  }
  ```
  - `SettingsState` add: `hotkey_snapshot`, `recording: Option<HotkeySlot>`, `recording_ready: bool`, `hotkey_error: Option<String>`, `hotkey_commands: VecDeque<HotkeyCommand>`.
  - View action: data-bearing `SettingsViewAction::BeginRecord/Commit/Cancel`; settings pass enqueues commands + `ctx.request_repaint_of(ROOT)`.
  - App::logic drains:
    - BeginRecord → `hotkey.suspend()`, set recording/ready, clear error, repaint settings.
    - CancelRecord → `hotkey.resume()`, clear recording.
    - Commit → `hotkey.commit`; success → `file.palette_hotkey/window_hotkey = Some`, `settings::save(data_dir, file)`, update snapshot/recording/error; failure → error, clear recording (user retries).
  - `cancel_recording_pending` from close: App::logic calls `hotkey.resume()` and clears recording.
- [x] **Step 2: UI**
  - Keycap fields clickable: not recording → begin; recording same slot → cancel; other slot → cancel + begin.
  - While recording and `ready`: consume egui key events; Esc cancels; valid combo → commit action; invalid → inline error; show “按下快捷键… (Esc 取消)”.
  - Display from snapshot; error inline under the row; Save/persist via App.
  - Disable the other field/controls while recording? At least prevent overlapping; language/autostart can stay.
- [x] **Step 3: 测试**
  - State transitions begin/cancel/commit success/failure/close-cancel.
  - Headless: keycap click enqueues begin; key capture maps/commits; Esc cancels; duplicate rejected; error text drawn.
- [x] **Step 4: 运行全量；提交** `feat(app): 设置页快捷键录制`.

## Task H3 实现记录（与计划的差异）

- `hotkey_error` 落为 `Option<(HotkeySlot, String)>`：错误按槽显示在对应键帽行下方；
  回落提示（`note`/`window_note`）也按槽在错误下另起一行，行高按消息条数累加。
- 视图侧校验（无修饰键/不支持键）与本地查重**不结束录制**：红色内联错误保留，
  用户可直接重按或 Esc 取消。运行时 `commit` 失败（系统占用等）才结束录制——H2
  已在失败时恢复旧键并退出挂起。
- 保存失败（注册已成功）不回退键：键保持生效/展示，行内提示「快捷键已生效,但
  设置保存失败」；内存中的 `SettingsFile` 保留新键，后续保存会补写。
- 点击别处取消已实现：按下点落在录制字段之外才取消（同字段由抬起时的再次点击取消），
  避免「开始录制的那次按下」被误判为点外；Esc/⌘W/红点关闭同样取消。
- 录制期纯修饰键（⇧⌃⌥⌘）按下被消费但不报错（等待组合键主键）；所有按下事件都被
  消费，避免触发其它 UI。
- `PaletteState.hotkey_note` 在 begin/cancel/commit 后按运行时快照同步（悬浮条常驻
  警告随回落/恢复失败提示更新）。
- 已知限制：eframe/egui-winit 把「仅 Command」的 ⌘C/⌘X/⌘V 转成 Copy/Cut/Paste
  事件（无 `Event::Key`），录制层暂不能录入这三组；带额外修饰的组合可正常录制。

## Task H4: 验收与文档

- [ ] 实机手动回归（需真实 Carbon 注册与系统占用，自动化不可覆盖）：
  - [ ] 依次录制唤起槽与完整窗口槽：新键立即生效、重启后仍保持（`settings.toml` 两字段）。
  - [ ] 录制中按 Esc、再次点击字段、点击窗口其它位置、⌘W/红点关闭：均取消并恢复旧键。
  - [ ] 与另一槽相同组合：行内报「该快捷键已用于另一个功能」，不落盘、录制继续可重按。
  - [ ] 被系统/其它应用占用的组合：行内报注册失败，旧键恢复可用，文件不变。
  - [ ] 手工把 `settings.toml` 改成不可用/越界组合后重启：回落默认键，设置页对应行显示回落提示。
  - [ ] 录制期按旧全局组合不触发悬浮条/完整窗口（挂起生效）。
  - [ ] 保存失败（只读数据目录）时：新键仍生效，行内提示保存失败。
- [x] 更新 `design.md` §6 实现偏差（失败退出录制而不是停留 Recording，如实现如此）、`settings-visual.md` 状态、本计划勾选。
- [x] 全量 fmt/clippy/test；提交。

## 验证命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 风险

- 全局热键注册/注销只能在主线程；命令必须由 App::logic（主线程）执行，不能在 settings pass 直接调用 manager。
- 录制期间必须 suspend，否则按当前键会触发旧动作。
- `HotKey` 的 serde 规范形式为 `alt+Space` / `shift+alt+KeyF`；显示层用 macOS 符号，不写回显示串。
- 保存失败/注册失败必须恢复旧键，不能留下无热键或半注册状态。

# 设置 · 快捷键自定义实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development or superpowers:executing-plans. Steps use `- [ ]`.

**Goal:** 设置页两个快捷键字段可录制、校验、重注册并持久化；重启后恢复；冲突/系统占用安全回退。

**依据:** `design.md` §6；`settings-visual.md` §2.4；`settings.toml` 的 `palette_hotkey`/`window_hotkey` 已由 Phase A 持久化层预留。

**前置:** Phase A 已完成（设置窗口/字号/自启；`SettingsState`、`settings_view`、`SettingsFile`、`HotkeyRuntime` 现有）。

---

## Task H1: 键位映射 + 校验 + 显示

**Files:** Create `crates/app/src/ui/hotkey_keys.rs`; modify `ui/mod.rs` (module); tests inside.

- [ ] **Step 1: 写失败测试**
  - A–Z / 0–9 / F1–F12 / Space / Enter / Tab / arrows / Home/End/PageUp/PageDown / common punctuation map to `global_hotkey::hotkey::Code`.
  - Modifier mapping: egui command→SUPER, ctrl→CONTROL, alt→ALT, shift→SHIFT.
  - Reject no modifier, pure modifier, unsupported key.
  - Display: `⌥Space` → `["⌥","Space"]`; `⌘⇧F` → `["⌘","⇧","F"]`; empty modifier list rejected.
  - Invalid: no modifiers → error text; unknown egui Key → None.
- [ ] **Step 2: 实现**
  ```rust
  pub struct HotkeyError(pub &'static str); // 需含修饰键 / 不支持该键
  pub fn from_egui(key: egui::Key, mods: egui::Modifiers) -> Result<HotKey, HotkeyError>;
  pub fn display_keys(hotkey: HotKey) -> Vec<String>; // macOS 符号 + 键名
  pub fn validate(hotkey: HotKey) -> Result<(), HotkeyError>; // mods 非空
  ```
  - Map the listed `egui::Key` variants; return Err for others.
  - `HotKey::new(Some(mods), code)`.
  - `display_keys`: order ⌃⌥⇧⌘? Use macOS conventional order `⌃⌥⇧⌘` or `⌘⌥⌃⇧`; spec visual examples use `⌥ Space`, `⌥ ⇧ F`; choose consistent order and test.
- [ ] **Step 3: 运行测试 + fmt/clippy；提交** `feat(app): 快捷键键位映射与校验`.

## Task H2: HotkeyRuntime 可重配置 + 启动偏好

**Files:** modify `crates/app/src/hotkey.rs`; `main.rs`/`ui/mod.rs` wiring; tests.

- [ ] **Step 1: 失败测试（纯规划器）**
  - Active keys state: `HotkeySlot { Palette, Window }`, `HotkeySnapshot { palette: Option<HotKey>, window: Option<HotKey>, palette_keys, window_keys, note }`.
  - Duplicate slot values rejected; commit plan preserves the other slot; failure restores old; suspend/resume.
  - `install` preference selection: custom valid → custom; custom failure → default fallback + note; no custom → defaults.
- [ ] **Step 2: 改造 runtime**
  - `ActiveKeys` in `Arc<Mutex<_>>`, pump reads it per event (no fixed captured keys).
  - `install(ctx, file: &SettingsFile)`:
    - Palette: `file.palette_hotkey` Some → try register; failure fallback `register_default`; note.
    - Window: `file.window_hotkey` Some → try; failure fallback `direct_key`; note.
    - Defaults when None unchanged.
  - `snapshot() -> HotkeySnapshot` (registered keys, display, fallback note).
  - `suspend()`/`resume()` for recording; `commit(slot, HotKey) -> Result<HotkeySnapshot, String>`:
    - Validate non-duplicate; register new (old suspended); on failure restore old snapshot and return error; on success update active/snapshot.
  - `name()`/`note()` updated; keep dispatch target/deferred.
- [ ] **Step 3: 启动接线**
  - `main.rs`: `hotkey::install(&ctx, &settings)`; `App::new` seeds `SettingsState.hotkey_snapshot` from `runtime.snapshot()`.
  - Existing default behavior at fresh install unchanged.
- [ ] **Step 4: 运行全量测试；提交** `feat(app): 热键运行时重配置与启动偏好`.

## Task H3: 设置页录制状态 + App 命令队列

**Files:** `ui/settings.rs` (state/commands), `ui/settings_view.rs` (keycap fields/actions), `ui/mod.rs` (App::logic), tests.

- [ ] **Step 1: 状态/命令**
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
- [ ] **Step 2: UI**
  - Keycap fields clickable: not recording → begin; recording same slot → cancel; other slot → cancel + begin.
  - While recording and `ready`: consume egui key events; Esc cancels; valid combo → commit action; invalid → inline error; show “按下快捷键… (Esc 取消)”.
  - Display from snapshot; error inline under the row; Save/persist via App.
  - Disable the other field/controls while recording? At least prevent overlapping; language/autostart can stay.
- [ ] **Step 3: 测试**
  - State transitions begin/cancel/commit success/failure/close-cancel.
  - Headless: keycap click enqueues begin; key capture maps/commits; Esc cancels; duplicate rejected; error text drawn.
- [ ] **Step 4: 运行全量；提交** `feat(app): 设置页快捷键录制`.

## Task H4: 验收与文档

- [ ] 实机：录制两个槽、取消、与另一槽冲突、被系统占用、重启保持、保存键失效回退默认、录制期旧热键不触发、关闭窗口取消录制。
- [ ] 更新 `design.md` §6 实现偏差（失败退出录制而不是停留 Recording，如实现如此）、`settings-visual.md` 状态、本计划勾选。
- [ ] 全量 fmt/clippy/test；提交。

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

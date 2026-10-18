# 设置功能（v2.1 第一期 A：窗口 / 字号 / 自启）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 620×600 设置窗口、字号三档、开机自启与齿轮/托盘入口；设置窗口状态与持久化就绪。

**Architecture:** 第三个 eframe deferred viewport（与悬浮条同模式）；设置 UI 在 `ui/settings.rs`，持久化在 `settings.rs`，字号倍率在 `ui/font.rs`；自启复用 SMAppService，托盘与设置页双向同步。

**Tech Stack:** eframe/egui 0.37、objc2-app-kit、global-hotkey、serde/toml。

**范围说明:** 快捷键录制与排除目录是两个独立子系统，分别见 `plan-hotkeys.md` / `plan-exclusions.md`（待写）；它们依赖本计划交付的窗口、状态与持久化。

---

## Task 1: 设置视口骨架 + 入口（PoC）

**Files:**
- Create: `crates/app/src/ui/settings.rs`
- Modify: `crates/app/src/ui/mod.rs`（`Shared`、`App::ui`、`App::logic`、入口）
- Modify: `crates/app/src/ui/results.rs`（齿轮请求开窗；删除 v2.1 占位提示）
- Modify: `crates/app/src/tray.rs`（`OpenSettings` 改为开窗）
- Modify: `crates/app/Cargo.toml`、`crates/app/src/platform/*`（删除刚加的 NSAlert 占位）

- [ ] **Step 1: 写失败测试（窗口尺寸常量）**

`crates/app/src/ui/settings.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_size_matches_spec() {
        assert_eq!((SETTINGS_W, SETTINGS_H), (620.0, 600.0));
        assert_eq!(TITLEBAR_H, 44.0);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app window_size_matches_spec`
Expected: FAIL（`settings` 模块/常量不存在）

- [ ] **Step 3: 建最小模块与视口 builder**

`crates/app/src/ui/settings.rs`：

```rust
//! 设置窗口（v2.1）：620×600 固定、原生交通灯 + 自绘 44 标题栏。

use std::sync::{Arc, Mutex};

use eframe::egui;

use crate::ui::{lock_shared, Shared};

pub const SETTINGS_W: f32 = 620.0;
pub const SETTINGS_H: f32 = 600.0;
pub const TITLEBAR_H: f32 = 44.0;

pub fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("settings")
}

pub fn builder() -> egui::ViewportBuilder {
    egui::ViewportBuilder::default()
        .with_title("设置")
        .with_inner_size([SETTINGS_W, SETTINGS_H])
        .with_min_inner_size([SETTINGS_W, SETTINGS_H])
        .with_max_inner_size([SETTINGS_W, SETTINGS_H])
        .with_resizable(false)
        .with_title_shown(false)
        .with_titlebar_shown(false)
        .with_fullsize_content_view(true)
        .with_has_shadow(true)
        .with_visible(false)
}

#[derive(Default)]
pub struct SettingsState {
    pub open: bool,
}

/// 设置视口 pass（deferred 回调）。
pub fn draw(ui: &mut egui::Ui, shared: &Arc<Mutex<Shared>>) {
    let ctx = ui.ctx().clone();
    let mut s = lock_shared(shared);

    // 关闭=隐藏（子视口须自行消费 close_requested）。
    if ctx.input(|i| i.viewport().close_requested()) {
        s.settings.open = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
    }

    let palette = crate::ui::window_tokens::palette(ui.visuals().dark_mode);
    ui.painter()
        .rect_filled(ui.max_rect(), egui::CornerRadius::ZERO, palette.window_fill);

    // 44px 标题栏：原生交通灯在左，标题居中。
    let bar = egui::Rect::from_min_size(
        ui.max_rect().min,
        egui::vec2(ui.max_rect().width(), TITLEBAR_H),
    );
    ui.painter().text(
        bar.center(),
        egui::Align2::CENTER_CENTER,
        "设置",
        egui::FontId::proportional(13.0), // Task 3 接入全局字号后改 font::id(13.0)
        palette.file_name,
    );

    // 占位：标题栏下方整块窗口底色，后续任务填内容。
    ui.painter().rect_stroke(
        ui.max_rect(),
        egui::CornerRadius::same(crate::ui::window_tokens::WINDOW_RADIUS),
        egui::Stroke::new(1.0, palette.window_stroke),
        egui::StrokeKind::Inside,
    );
}
```

- [ ] **Step 4: 接线 `Shared` / 视口注册 / 打开函数**

`crates/app/src/ui/mod.rs`：

```rust
pub mod settings;
```

`Shared` 加：

```rust
pub settings: settings::SettingsState,
```

构造 `Shared` 时：

```rust
settings: settings::SettingsState::default(),
```

`App::ui` 末尾（悬浮条 viewport 之后）注册：

```rust
let shared = Arc::clone(&self.shared);
ctx.show_viewport_deferred(
    settings::viewport_id(),
    settings::builder(),
    move |ui, _class| settings::draw(ui, &shared),
);
```

新增函数：

```rust
fn open_settings(shared: &Arc<Mutex<Shared>>, ctx: &egui::Context) {
    lock_shared(shared).settings.open = true;
    ctx.send_viewport_cmd_to(
        settings::viewport_id(),
        egui::ViewportCommand::Visible(true),
    );
    ctx.send_viewport_cmd_to(settings::viewport_id(), egui::ViewportCommand::Focus);
    ctx.request_repaint_of(settings::viewport_id());
    ctx.request_repaint();
}
```

- [ ] **Step 5: 齿轮与托盘入口**

`results.rs`：`ResultsState` 加 `pub settings_requested: bool`（初始 false）。`draw_impl` 中齿轮命中时不再 `show_gear_note`，改为置位；新增：

```rust
pub fn take_settings_request(&mut self) -> bool {
    std::mem::take(&mut self.settings_requested)
}
```

`App::ui` 在 results 绘制锁释放后：

```rust
if self.shared().results.take_settings_request() {
    log::info!("齿轮:打开设置窗口");
    open_settings(&self.shared, &ctx);
}
```

`tray.rs` 的 `OpenSettings` 分支改为 `log::info!("托盘:打开设置");` + `open_settings(&self.shared, ctx);`（把 `open_settings` 放到 `ui::mod` 可见范围）。

删除：`GEAR_PENDING_NOTE` / `GEAR_NOTE_TTL` / `status_note_until` / `show_gear_note` 及其 tick 清理、线程唤醒；删除托盘 NSAlert 占位（`platform::show_info_alert`、`NSAlert`/`NSButton` features、相关 import）。保留 `status_note` 给错误提示。

- [ ] **Step 6: 运行全部验证**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: 全绿。

- [ ] **Step 7: 实机 PoC**

Run: `bash scripts/bundle.sh`，启动后 `⌥⇧F` 打开完整窗口 → 点齿轮；以及托盘「设置…」。
Expected:
- 设置窗口 620×600、原生交通灯、标题「设置」、圆角/底色正确；
- 关闭红点 → 窗口隐藏、App 不退；
- 再次点齿轮/托盘 → 同一实例重新出现，不新建窗口；
- 根窗隐藏时从托盘打开也可见（若不可见，记录并转平台层 `orderFrontRegardless` 方案）。

- [ ] **Step 8: 提交**

```bash
git add crates/app/src/ui/settings.rs crates/app/src/ui/mod.rs crates/app/src/ui/results.rs crates/app/src/tray.rs crates/app/Cargo.toml crates/app/src/platform
git commit -m "feat(app): 设置窗口骨架与齿轮/托盘入口"
```

---

## Task 2: 设置持久化 + 设计 token

**Files:**
- Create: `crates/app/src/settings.rs`
- Create: `crates/app/src/ui/settings_tokens.rs`
- Modify: `crates/app/Cargo.toml`（serde/toml/global-hotkey serde）

- [ ] **Step 1: 写失败测试**

`settings.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_and_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let p = path(dir.path());
        let f = SettingsFile::default();
        assert_eq!(f.font_size, FontSize::Standard);
        assert!(f.palette_hotkey.is_none());
        save(dir.path(), &f).unwrap();
        assert_eq!(load(dir.path()), f);

        std::fs::write(&p, "font_size = [").unwrap();
        assert_eq!(load(dir.path()), SettingsFile::default());
    }

    #[test]
    fn font_size_scales_match_spec() {
        assert_eq!(FontSize::Small.scale(), 0.9);
        assert_eq!(FontSize::Standard.scale(), 1.0);
        assert_eq!(FontSize::Large.scale(), 1.15);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app settings::tests`
Expected: FAIL（模块不存在）

- [ ] **Step 3: 实现 `settings.rs`**

```rust
//! v2.1 设置持久化：数据目录 `settings.toml`。
//! 字号与快捷键入此文件；自启/排除目录的真源分别是 SMAppService / 引擎配置。

use std::path::{Path, PathBuf};

use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "settings.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum FontSize {
    Small,
    #[default]
    Standard,
    Large,
}

impl FontSize {
    pub fn scale(self) -> f32 {
        match self {
            Self::Small => 0.9,
            Self::Standard => 1.0,
            Self::Large => 1.15,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Small => "小",
            Self::Standard => "标准",
            Self::Large => "大",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SettingsFile {
    #[serde(default)]
    pub font_size: FontSize,
    #[serde(default)]
    pub palette_hotkey: Option<HotKey>,
    #[serde(default)]
    pub window_hotkey: Option<HotKey>,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE_NAME)
}

pub fn load(data_dir: &Path) -> SettingsFile {
    let p = path(data_dir);
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
            log::warn!("设置解析失败({}),用默认值: {e}", p.display());
            SettingsFile::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SettingsFile::default(),
        Err(e) => {
            log::warn!("设置读取失败({}),用默认值: {e}", p.display());
            SettingsFile::default()
        }
    }
}

pub fn save(data_dir: &Path, f: &SettingsFile) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    let p = path(data_dir);
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, toml::to_string_pretty(f)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}
```

`Cargo.toml` 加：

```toml
serde.workspace = true
toml.workspace = true
global-hotkey = { version = "0.6", features = ["serde"] }

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 4: 实现 `settings_tokens.rs`**

按 `visual` §2.5 建 `SettingsPalette`（窗口底/描边/标题/分组标题/卡片底/分隔线/控件底/控件文字/选中段底/选中段文字/未选中段文字/开关开态/键帽底/键帽文字/目录路径/移除图标），深/浅两套 + `palette(is_dark)`；按 §2.2/2.4 建几何常量（`CONTENT_PAD=(20,24)`、`GROUP_GAP=20`、`CARD_RADIUS=10`、`ROW_H=44`、`EXCLUDE_HEADER_H=52`、`EXCLUDE_ROW_H=40`、`SEG_W=180`、`SEG_H=28`、`TOGGLE=(38,22)`、`FIELD=(152,28)` 等）。单测逐项钉住色值与尺寸。

- [ ] **Step 5: 运行测试并提交**

Run: `cargo test -p macEverything-app settings` / `cargo fmt --check`
Expected: PASS。

```bash
git add crates/app/src/settings.rs crates/app/src/ui/settings_tokens.rs crates/app/Cargo.toml Cargo.lock
git commit -m "feat(app): 设置持久化与视觉 token"
```

---

## Task 3: 字号倍率基础设施

**Files:**
- Create: `crates/app/src/ui/font.rs`
- Modify: `crates/app/src/ui/mod.rs`（加载设置、`install_base_text_styles`）
- Modify: `crates/app/src/ui/palette.rs`、`crates/app/src/ui/results.rs`（显式字号接入）
- Modify: `crates/app/src/main.rs`（启动读取 `SettingsFile`）

- [ ] **Step 1: 写失败测试**

`ui/font.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_affects_size_and_id() {
        set_scale(1.0);
        assert_eq!(size(13.0), 13.0);
        assert!((id(13.0).size - 13.0).abs() < f32::EPSILON);
        set_scale(1.15);
        assert!((size(13.0) - 14.95).abs() < 0.001);
        set_scale(1.0);
    }
}
```

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app font::tests`
Expected: FAIL。

- [ ] **Step 3: 实现 `ui/font.rs` 并接入基础字号**

```rust
//! 全局文本倍率（字号设置）：只缩放文字，几何尺寸不变。

use std::sync::atomic::{AtomicU32, Ordering};
use eframe::egui;

static SCALE_BITS: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0f32

pub fn scale() -> f32 {
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

pub fn set_scale(s: f32) {
    SCALE_BITS.store(s.clamp(0.5, 2.0).to_bits(), Ordering::Relaxed);
}

pub fn size(base: f32) -> f32 {
    base * scale()
}

pub fn id(base: f32) -> egui::FontId {
    egui::FontId::proportional(size(base))
}
```

`ui/mod.rs`：`install_base_text_styles` 的 11/15/15/20/15 改为 `font::size(...)`；`App::new` 先 `font::set_scale(settings.font_size.scale())`，再 `install_base_text_styles`。

- [ ] **Step 4: 机械替换全部显式字号**

Run: `rg -n "FontId::proportional|\.size\\(FONT_|\\.size\\(tk::FONT_" crates/app/src/ui`

全部改为 `font::id(...)` / `.size(font::size(...))`；`palette.rs` 的 `font()` 与 `text_w()` 内部使用 `font::id/size`，避免双重缩放。逐个跑现有测试确认无回归。

- [ ] **Step 5: 启动读取设置**

`main.rs` 读取 `let settings = settings::load(&paths.data_dir);`，传给 `App::new`；`App::new` 应用字号并按 settings 初始化设置状态（本任务先只应用字号）。`SettingsFile` 后续由 Task 5 使用。

- [ ] **Step 6: 运行全量测试并提交**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: 全绿。

```bash
git add crates/app/src/ui/font.rs crates/app/src/ui/mod.rs crates/app/src/ui/palette.rs crates/app/src/ui/results.rs crates/app/src/main.rs
git commit -m "feat(app): 全局字号倍率基础设施"
```

---

## Task 4: 设置窗口完整布局（静态）

**Files:**
- Modify: `crates/app/src/ui/settings.rs`（布局/控件）
- Modify: `crates/app/src/ui/settings_tokens.rs`（若需补 token）

- [ ] **Step 1: 写 headless 出帧测试**

用 `egui::Context::default()` + `run_ui` 调 `settings::draw_body_for_test(ui, ...)`（把绘制与 Shared 解耦的纯绘制函数），断言出现：
- 「外观」「通用」「索引」三个分组标题；
- 「字号」「开机自启」「语言」「唤起快捷键」「完整窗口快捷键」「排除目录」标签；
- 「小」「标准」「大」三段与「简体中文」；
- 不出现 panic。

- [ ] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app settings_layout_has_all_rows`
Expected: FAIL。

- [ ] **Step 3: 实现内容布局**

按 `visual` §2.2–§2.4 绘制：
- Titlebar 44（标题 13pt 居中）；
- Content padding `(20,24)`，分组 gap 20；
- 分组标题 12pt 次要色 + 卡片（圆角 10、卡片底、行间 1px 分隔线）；
- 行高 44；控件按 §2.4 尺寸；
- 语言下拉禁用态「简体中文」+ tooltip；
- 排除目录卡：Header 52（标题 + 「＋ 添加目录…」按钮）、目录行 40（文件夹图标 16 + 路径 13 + 移除 ×14）；
- 内容超过可视高时在卡片/内容区滚动，标题栏固定；
- 所有文本用 `font::size`，所有颜色/几何读 `settings_tokens`。

绘制用 `egui::Ui` 的 `allocate_ui_at_rect`/`allocate_exact_size` + painter；控件交互先只记录 hover/点击，行为在 Task 5/后续 plan 接。

- [ ] **Step 4: 运行测试并实机看**

Run: `cargo test -p macEverything-app settings_layout_has_all_rows && bash scripts/bundle.sh`
Expected: 三组五项齐全、深浅色正确、无裁切。

- [ ] **Step 5: 提交**

```bash
git add crates/app/src/ui/settings.rs crates/app/src/ui/settings_tokens.rs
git commit -m "feat(app): 设置窗口三组五项布局"
```

---

## Task 5: 开机自启 + 语言占位 + 字号选择生效

**Files:**
- Modify: `crates/app/src/ui/settings.rs`
- Modify: `crates/app/src/ui/mod.rs`（托盘自启同步）
- Modify: `crates/app/src/tray.rs`（勾选同步）

- [ ] **Step 1: 写失败测试（纯状态）**

为 `SettingsState` 增加 `autostart: Option<bool>`、`autostart_error: Option<String>`、`font_size: FontSize`，测试：
- `apply_font_size(FontSize::Large)` 后 `font::scale()==1.15`；
- `autostart_changed(Ok(true))` 更新为 true 且清错误；
- `autostart_changed(Err(msg))` 保持旧值并落错误。

- [ ] **Step 2: 实现自启读取/切换**

打开设置时读 `autostart::is_enabled()`；开关点击调 `autostart::apply_user_choice(&paths.data_dir, desired)`：
- 成功：更新 `settings.autostart`，写 `SettingsAction::AutostartChanged(desired)`；
- 失败：恢复旧值 + `autostart_error`。

`App::logic` drain `AutostartChanged` → `self.tray.set_autostart_checked(desired)`。托盘切换成功 → `self.shared().settings.autostart = Some(checked)`。

- [ ] **Step 3: 实现字号分段**

点击小/标准/大：
- `font::set_scale(font_size.scale())`；
- `install_base_text_styles(ctx)`；
- 写 `settings.toml`（`SettingsFile { font_size, ..current }`）；
- `ctx.request_repaint_of(ROOT + palette + settings)`。

- [ ] **Step 4: 语言占位**

下拉按钮禁用态；点击不改变值；hover tooltip「暂仅支持简体中文，语言切换将在后续版本提供」。

- [ ] **Step 5: 运行验证并提交**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
实机：字号三档切换两窗即时变化、重启保持；自启勾选与托盘一致。

```bash
git add crates/app/src/ui/settings.rs crates/app/src/ui/mod.rs crates/app/src/tray.rs crates/app/src/settings.rs
git commit -m "feat(app): 设置页字号与开机自启"
```

---

## Task 6: 收尾与文档

- [ ] 设置窗口居中（首次打开按光标所在屏）与置前；单实例。
- [ ] 关闭时取消录制（后续 hotkey plan 接入点），保持隐藏。
- [ ] 深浅色切换时设置窗口即时跟随（设置 pass 驱动 `ThemeFollow`）。
- [ ] 更新 `settings-visual.md` §3.1/§3.2 的实现状态；`design.md` 偏差回填。
- [ ] 实机清单（辉哥）逐项走查。
- [ ] 提交。

---

## 验证命令（每个 Task 收尾）

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 后续计划（不在本文件）

- `plan-hotkeys.md`：快捷键录制/冲突/重注册。
- `plan-exclusions.md`：排除目录（NSOpenPanel + daemon exclude + 相对名移除修复 + 异步重扫）。

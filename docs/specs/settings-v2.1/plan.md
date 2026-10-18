# 设置功能（v2.1 第一期 A：窗口 / 字号 / 自启）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 620×600 设置窗口、字号三档、开机自启与齿轮/托盘入口；设置窗口状态与持久化就绪。

**Architecture:** 第三个 eframe deferred viewport（与悬浮条同模式）；设置窗口/运行期状态/行为在 `ui/settings.rs`，纯视图在 `ui/settings_view.rs`（行模型在 `ui/settings_layout.rs`），持久化在 `settings.rs`，字号倍率在 `ui/font.rs`；自启复用 SMAppService，托盘与设置页双向同步。

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

- [x] **Step 1: 写失败测试（窗口尺寸常量）**

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

- [x] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app window_size_matches_spec`
Expected: FAIL（`settings` 模块/常量不存在）

- [x] **Step 3: 建最小模块与视口 builder**

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

- [x] **Step 4: 接线 `Shared` / 视口注册 / 打开函数**

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

- [x] **Step 5: 齿轮与托盘入口**

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

- [x] **Step 6: 运行全部验证**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: 全绿。

- [ ] **Step 7: 实机 PoC**

Run: `bash scripts/bundle.sh`，启动后 `⌥⇧F` 打开完整窗口 → 点齿轮；以及托盘「设置…」。
Expected:
- 设置窗口 620×600、原生交通灯、标题「设置」、圆角/底色正确；
- 关闭红点 → 窗口隐藏、App 不退；
- 再次点齿轮/托盘 → 同一实例重新出现，不新建窗口；
- 根窗隐藏时从托盘打开也可见（若不可见，记录并转平台层 `orderFrontRegardless` 方案）。

**状态（2026-10-18）:** 核心行为已由 controller 实机 PoC 验证（620×600、原生交通灯、红点隐藏、齿轮重开同一实例）；托盘在 ROOT 隐藏（同 Space）时打开设置已在 Task 6 实机验证；跨 Space 仍待验，留作缺口。

- [x] **Step 8: 提交**

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

- [x] **Step 1: 写失败测试**

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

- [x] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app settings::tests`
Expected: FAIL（模块不存在）——实际：编译失败，24 个未定义项错误。

- [x] **Step 3: 实现 `settings.rs`**

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

- [x] **Step 4: 实现 `settings_tokens.rs`**

按 `visual` §2.5 建 `SettingsPalette`（窗口底/描边/标题/分组标题/卡片底/分隔线/控件底/控件文字/选中段底/选中段文字/未选中段文字/开关开态/键帽底/键帽文字/目录路径/移除图标），深/浅两套 + `palette(is_dark)`；按 §2.2/2.4 建几何常量（`CONTENT_PAD=(20,24)`、`GROUP_GAP=20`、`CARD_RADIUS=10`、`ROW_H=44`、`EXCLUDE_HEADER_H=52`、`EXCLUDE_ROW_H=40`、`SEG_W=180`、`SEG_H=28`、`SWITCH_SIZE=(38,22)`、`FIELD=(152,28)` 等）。单测逐项钉住色值与尺寸。

- [x] **Step 5: 运行测试并提交**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: PASS。

验证记录（2026-10-18，review 修复后）：
- 三条命令全绿；app 全量 182 passed / 3 ignored。
- 本任务单测：`settings.rs` 5 个（默认值/往返/损坏、字号倍率、HotKey 规范序列化、部分文件+未知键、save tmp 清理与失败路径）；`settings_tokens.rs` 6 个（§2.5 色板、错误色跨模块比对、开关钮投影、几何、主题选择、`c()` 字节序）。

Review 后定稿差异：
- 设置窗口尺寸/chrome 单一来源：`settings_tokens` 拥有 `SETTINGS_W/H`、`TITLEBAR_H`；`WINDOW_RADIUS/WINDOW_STROKE/TRAFFIC_LIGHTS_RESERVE` 从 `window_tokens` re-export；`ui/settings.rs` 删除本地副本。
- 参考值（原生阴影/交通灯/标题栏留白）用 item-level `#[allow(dead_code)]` 标注；模块级 allow 在 Task 4/5 消费 token 接入后移除。
- `FILE_NAME` 私有；`save` 失败清理 tmp 并带路径上下文；`TOGGLE*` 命名统一为 `SWITCH_*`，新增 `switch_knob_shadow`。
- HotKey 序列化以 `alt+Space` / `shift+alt+KeyF` 规范形式钉住（design §3 已同步）。

```bash
git add Cargo.lock crates/app/Cargo.toml crates/app/src/main.rs crates/app/src/settings.rs crates/app/src/ui/mod.rs crates/app/src/ui/settings_tokens.rs
git commit -m "feat(app): 设置持久化与视觉 token"
```

---

## Task 3: 字号倍率基础设施

**Files:**
- Create: `crates/app/src/ui/font.rs`
- Modify: `crates/app/src/ui/mod.rs`（加载设置、`install_base_text_styles`）
- Modify: `crates/app/src/ui/palette.rs`、`crates/app/src/ui/results.rs`（显式字号接入）
- Modify: `crates/app/src/main.rs`（启动读取 `SettingsFile`）
- Modify: `crates/app/src/ui/settings.rs`（设置状态持 `SettingsFile`、标题接入倍率）

- [x] **Step 1: 写失败测试**

并行测试不得写进程级全局倍率（会与其它 UI 测试竞态）。非默认倍率用
`ui/font.rs` 中的线程局部覆盖 `with_test_scale`（闭包内生效、结束还原）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_and_id_follow_test_scale_then_restore() {
        with_test_scale(1.15, || {
            assert_eq!(size(10.0), 11.5);
            assert_eq!(id(10.0).size, 11.5);
        });
        assert_eq!(size(10.0), 10.0); // 闭包结束已还原
    }

    #[test]
    fn sanitize_scale_rejects_non_finite_and_clamps() {
        assert_eq!(sanitize_scale(f32::NAN), 1.0);
        assert_eq!(sanitize_scale(f32::INFINITY), 1.0);
        assert_eq!(sanitize_scale(0.1), 0.5);
        assert_eq!(sanitize_scale(3.0), 2.0);
    }
}
```

另测 `base_text_sizes(1.0/1.15)` 纯函数，以及安装后**两主题**五档字号与 family
（`install_base_text_styles_scales_both_themes`）。

- [x] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app font::tests`
Expected: FAIL。

- [x] **Step 3: 实现 `ui/font.rs` 并接入基础字号**

```rust
//! 全局文本倍率（设置 · 字号）：只缩放文字，几何尺寸不变。

use std::sync::atomic::{AtomicU32, Ordering};
use eframe::egui;

static SCALE_BITS: AtomicU32 = AtomicU32::new(0x3F80_0000); // 1.0f32

#[cfg(test)]
thread_local! {
    static TEST_SCALE: std::cell::Cell<Option<f32>> =
        const { std::cell::Cell::new(None) };
}

pub fn scale() -> f32 {
    #[cfg(test)]
    if let Some(s) = TEST_SCALE.with(std::cell::Cell::get) {
        return s;
    }
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

/// NaN / ±∞ → 1.0；其余 clamp 0.5–2.0。
pub fn sanitize_scale(s: f32) -> f32 {
    if s.is_finite() { s.clamp(0.5, 2.0) } else { 1.0 }
}

pub fn set_scale(s: f32) {
    SCALE_BITS.store(sanitize_scale(s).to_bits(), Ordering::Relaxed);
}

pub fn scaled(base: f32, scale: f32) -> f32 {
    base * scale
}

pub fn size(base: f32) -> f32 {
    scaled(base, scale())
}

pub fn id(base: f32) -> egui::FontId {
    egui::FontId::proportional(size(base))
}

#[cfg(test)]
pub(crate) fn with_test_scale<T>(scale: f32, f: impl FnOnce() -> T) -> T { /* 覆盖→f→还原 */ }
```

生产写入点仅两处：启动读取设置、Task 5 设置页改字号（后者还需重装基础样式）。
`ui/mod.rs`：`install_base_text_styles` 使用纯函数 `base_text_sizes(scale)` 的
11/15/15/20/15 基准 × 倍率；`App::new` 先 `font::set_scale(settings.font_size.scale())`，
再 `install_base_text_styles`。`ui/settings.rs` 标题改用 `font::id(TITLE_FONT)`。

- [x] **Step 4: 机械替换全部显式字号**

Run: `rg -n "FontId::proportional|\.size\(FONT_|\.size\(window_tokens::FONT_|\.size\(tk::FONT_|fn font\(" crates/app/src/ui`

全部改为 `font::id(...)` / `.size(font::size(...))`；`palette.rs` 的 `font()` 与
`text_w()` 内部使用 `font::id/size`，避免双重缩放；palette 空态 14pt 收进
`ui/tokens.rs::FONT_EMPTY_TITLE`。逐个跑现有测试确认无回归。

- [x] **Step 5: 启动读取设置**

`main.rs` 读取 `let settings = settings::load(&paths.data_dir);`，传给 `App::new`；
`App::new` 应用字号并用 settings 初始化设置状态（本任务只应用启动字号；
**运行期改字号仍属 Task 5**）。`SettingsState.file` 供 Task 4–5 读取。

- [x] **Step 6: 运行全量测试并提交**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: 全绿。

```bash
git add crates/app/src/ui/font.rs crates/app/src/ui/mod.rs crates/app/src/ui/palette.rs crates/app/src/ui/results.rs crates/app/src/main.rs crates/app/src/ui/settings.rs
git commit -m "feat(app): 全局字号倍率基础设施"
```

**验证记录（2026-10-18，复查修复后）：** fmt / clippy 通过；
`cargo test --workspace` 全绿：workspace 298 passed / 5 ignored，其中
`macEverything-app` 188 passed / 3 ignored（含 font 单测 4 个、基础样式端到端 1 个）。

---

## Task 4: 设置窗口完整布局（静态）

**Files:**
- Modify: `crates/app/src/ui/settings.rs`（布局/控件）
- Modify: `crates/app/src/ui/settings_tokens.rs`（若需补 token）

- [x] **Step 1: 写 headless 出帧测试**

用 `egui::Context::default()` + `run_ui` 调纯绘制函数（Task 5 重构后为
`settings_view::draw_body(ui, &SettingsData)`），断言出现：
- 「外观」「通用」「索引」三个分组标题；
- 「字号」「开机自启」「语言」「唤起快捷键」「完整窗口快捷键」「排除目录」标签；
- 「小」「标准」「大」三段与「简体中文」；
- 不出现 panic。

- [x] **Step 2: 运行确认失败**

Run: `cargo test -p macEverything-app settings_layout_has_all_rows`
Expected: FAIL。

- [x] **Step 3: 实现内容布局**

按 `visual` §2.2–§2.4 绘制：
- Titlebar 44（标题 13pt 居中）；
- Content padding `(20,24)`，分组 gap 20；
- 分组标题 12pt 次要色 + 卡片（圆角 10、卡片底、行间 1px 分隔线）；
- 行高 44；控件按 §2.4 尺寸；
- 语言下拉禁用态「简体中文」+ tooltip；
- 排除目录卡：Header 52（标题 + 「＋ 添加目录…」按钮）、目录行 40（文件夹图标 16 + 路径 13 + 移除 ×14）；
- 内容超过可视高时**整页纵向滚动**（标题栏固定；不做卡片内独立滚动，排除目录 Task 8 沿用整页）；
- 所有文本用 `font::size`，所有颜色/几何读 `settings_tokens`。

绘制用 `egui::Ui` 的 `allocate_exact_size` + painter；控件交互先只记录 hover/点击，行为在 Task 5/后续 plan 接。

- [x] **Step 4: 运行测试并实机看**

Run: `cargo test -p macEverything-app settings_layout_has_all_rows && bash scripts/bundle.sh`
Expected: 三组五项齐全、深浅色正确、无裁切。

- [x] **Step 5: 提交**

```bash
git add crates/app/src/ui/settings.rs crates/app/src/ui/settings_tokens.rs
git commit -m "feat(app): 设置窗口三组五项布局"
```

**验证记录（2026-10-18）：** headless 布局测试全绿；视觉由 controller 实机验证
（三组五项、深浅色、无裁切、620×600 固定窗）。Task 5 前置重构见下。

---

## Task 5: 开机自启 + 语言占位 + 字号选择生效

**Files:**
- Modify: `crates/app/src/ui/settings.rs`（视口/运行期状态/行为）
- Create: `crates/app/src/ui/settings_view.rs`（纯视图 + headless 测试）
- Create: `crates/app/src/ui/settings_layout.rs`（行模型/卡片高度）
- Modify: `crates/app/src/ui/mod.rs`（`pub(crate)` 字号样式/调色板 id、开窗查询自启、托盘同步仲裁）
- Modify: `crates/app/src/settings.rs`（移除模块级 dead_code allow）
- Modify: `crates/app/src/autostart.rs`（`apply_user_choice` 返回操作后真实 `Status`）
- Modify: `crates/app/src/tray.rs`（无需改动；沿用既有 `set_autostart_checked` API）
- Modify: `.gitignore`（忽略本地 `.claude/`）

- [x] **Step 1: 写失败测试（纯状态）**

为 `SettingsState` 增加 `autostart: Option<bool>`、`autostart_approval: bool`、
`autostart_error: Option<String>`、`file: SettingsFile`、`font_error: Option<String>`、
`tray_autostart_sync: Option<bool>`，测试：
- 字号纯状态转换：保存失败仍更新 `file.font_size` + `font_error`，成功清错误；
- 自启查询失败 → 未知（显示关态）+ 错误；`RequiresApproval` → 关态 + 审批提示；
- 切换成功按操作后真实 `Status` 落地（仅 Enabled 开），托盘同步只排实际值；
- 失败保持旧值 + 错误且不同步托盘；同帧托盘切换弃用设置页旧同步值；
- 快照 `SettingsData` 与运行期状态一致。

生产 `font::set_scale` 写进程级原子，单测不调用；非默认倍率视觉测试只用
`font::with_test_scale`，持久化路径单独用临时目录验证。

- [x] **Step 2: 实现自启读取/切换**

`open_settings` 仅在隐藏→显示时于共享锁外查询 `autostart::status()`；窗口已打开
不重复查询（保留失败切换的行内错误）。`RequiresApproval` → 关态 + 审批提示；
查询失败 → 未知 + 错误。开关点击在锁外调
`autostart::apply_user_choice(&data_dir, desired)`（返回操作后真实 `Status`）：
- 仅 `Enabled` 显示开，置 `tray_autostart_sync=Some(true)`；
- `RequiresApproval` 显示关 +「已注册,待系统设置「登录项」允许」，
  `tray_autostart_sync=Some(false)`；
- 启用后 `NotRegistered`/`NotFound` 显示关 + 内联错误、同步 false；
- 失败保持旧值 + `autostart_error`（如开发形态「未打包」），不同步托盘。

设置页与托盘共用 `SettingsState` 的同一状态策略；托盘路径只校准勾选不排队同步。
`App::logic` **先** `drain_tray`，再 drain `tray_autostart_sync`；若本帧托盘也切换了
自启，则丢弃设置页旧同步值（`take_tray_autostart_sync(tray_toggled)` 仲裁）。
设置页打开时请求其重绘；托盘失败复原勾选，审批中/未生效取消勾选。

- [x] **Step 3: 实现字号分段**

点击小/标准/大（出共享锁）：
- `font::set_scale(font_size.scale())`；
- `install_base_text_styles(ctx)`（`pub(crate)` 供设置模块复用）；
- 写 `settings.toml`（`SettingsFile { font_size, ..current }`）；失败只落
  `font_error`（字号行内联）+ `log::warn`，**保留已应用倍率**；
- 更新 `file.font_size` / 选中态；
- `ctx.request_repaint_of(ROOT + palette + settings)`。

- [x] **Step 4: 语言占位**

下拉按钮禁用态；点击不改变值；文字/chevron 用次要色（不只靠 hover 区分），
hover 光标 `NotAllowed` + tooltip「暂仅支持简体中文，语言切换将在后续版本提供」。

- [x] **Step 5: 运行验证并提交**

Run: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
实机：字号三档切换两窗即时变化、重启保持；自启勾选与托盘一致性**待 controller
复核**（本轮未实机验证托盘勾选）。

```bash
git add crates/app/src/ui/settings.rs crates/app/src/ui/settings_view.rs crates/app/src/ui/settings_layout.rs crates/app/src/ui/mod.rs crates/app/src/autostart.rs crates/app/src/settings.rs crates/app/src/ui/settings_tokens.rs .gitignore docs/specs/settings-v2.1
git commit -m "feat(app): 设置页字号与开机自启"
```

> 2026-10-18 修正：本条 `git add` 已更新为实际改动面——补 `autostart.rs`
>（`apply_user_choice` 返回真实 `Status`）与 `.gitignore`（`.claude/`），
> 移除未改动的 `tray.rs`。

**前置重构（Task 4 review → Task 5，2026-10-18）：**
- `ui/settings.rs` 拆为：`settings.rs`（视口 builder / `SettingsState` / 快照→锁外绘制→动作落地 / 隐藏）；
  `settings_view.rs`（`SettingsData`、纯 `draw_body`、分组/控件/图标与 headless 测试）；
  `settings_layout.rs`（`RowSpec` 行模型、`card_height`、`take_row`）。
- 高度与绘制共用同一行模型：卡片高 = Σ(行主体 + 行内错误行) + 行间分隔线；绘制
  末尾 `debug_assert` 卡片底 == 行游标。内联错误行增高不再导致卡片底/分隔线漂移。
- 滚动模型定案：**整页纵向滚动、44px 标题栏固定**；不做卡片内独立滚动。
  Task 8（排除目录真实列表）沿用整页滚动，列表变长只增高索引卡片；本期不做
  列表虚拟化（条目数量级低；如后续出现长列表再评估）。
- 运行期真源收进 `SettingsState`（含 `data_dir`/`excluded` 一次性种子/默认键帽），
  `SettingsData` 仅为出锁绘制快照（`from_state` 在 `settings.rs`，视图只依赖
  快照）；不再每帧克隆 `engine::config::default_config()`。
- 动作所有权：`settings_view::SettingsAction` 保持 `Copy` 即时动作（字号/自启），
  绘制后由设置 pass 锁外落地；Task 6 快捷键（带数据、主线程注册）与 Task 8
  排除目录（异步 daemon）改走 `Shared.settings` 的 App 动作队列，由 `App::logic`
  drain / 工作线程执行，不在视图回调内做阻塞或原生调用。
- 视觉修复：键帽组按原型在字段内水平居中；标题使用 `platform::SETTINGS_TITLE`；
  开关钮投影改 3 层递减 alpha 软影；排除目录空列表显示「未排除任何目录」。
- 删除 `crates/app/src/settings.rs` 模块级 `#![allow(dead_code)]`；仅保留
  `SettingsPalette::control_text` 的 item-level allow（规格 token，禁用态改次要色）。

**Review 修复（2026-10-18，follow-up）：**
- 自启按操作后真实 `Status` 落地：`RequiresApproval` 不再显示/同步为已启用，
  改为关态 +「已注册,待系统设置「登录项」允许」；启用后未注册为关态 + 错误。
  `apply_user_choice` 改为返回 `Status`；设置页与托盘共用同一策略方法。
- `App::logic` 改为先 `drain_tray`；本帧托盘切换自启时丢弃设置页旧同步
  （`take_tray_autostart_sync(tray_toggled)` 单元测试钉住）。
- `open_settings` 仅隐藏→显示时查询，已打开窗口保留失败切换的行内错误。
- `SettingsData::from_state` 移入 `ui/settings.rs`，视图不再依赖 `SettingsState`。
- 补充错误文案实际绘制断言与审批提示绘制测试；`.claude/` 加入 `.gitignore`。

**验证记录（2026-10-18）：** `cargo fmt --check` / `cargo clippy --workspace
--all-targets -- -D warnings` / `cargo test --workspace` 全绿。app 新增/更新测试：
设置状态/策略 9 个、行模型 2 个、headless 布局 10 个（含错误行增高与文案、
审批提示、空态、键帽居中、选中段填充、两处点击动作、大字号冒烟）；托盘勾选
实机复核仍待 controller。

---

## Task 6: 收尾与文档

**Files:**
- Modify: `crates/app/src/platform/{mod,mac}.rs`（设置窗原生居中/置前）
- Modify: `crates/app/src/ui/settings.rs`（首次定位记账/关闭取消录制 hook/主题跟随）
- Modify: `crates/app/src/ui/mod.rs`（开窗原生接线/托盘失败按实际值校准/关闭 hook 消费）
- Modify: `docs/specs/settings-v2.1/{plan,design}.md`、`docs/specs/floatbar-visual/settings-visual.md`

- [x] **Step 1: 原生居中/置前（仅首次）**
  - `platform::show_settings_window(center)`：按 `platform::SETTINGS_TITLE` 定位设置
    NSWindow；`center=true`（首次）按鼠标所在屏 `NSScreen.visibleFrame`（已扣菜单栏/
    Dock）居中 620×600，四周 clamp 8pt；放不下时保持**左上可见**（AppKit y 向上，
    原点 y = 可见区顶 − 窗高 − 8，底部允许溢出，标题栏/交通灯不得移出屏）。
    一律 `NSApp.activate` +（最小化态先 `deminiaturize`）`orderFrontRegardless` +
    `makeKeyWindow`；`show_root_window` 同步补 deminiaturize 守卫。
  - `SettingsState::request_open()` 首开返回 true 并记账 `positioned_once`；已打开/
    关闭后重开只置前，保留用户拖动位置。
  - 原生调用在 `Shared` 锁外；窗口未就绪 → 留痕并退回既有 egui `Visible(true)` +
    `Focus`；非 macOS facade 返回错误（同样走退回路径）。
  - ROOT 隐藏（同 Space）置前已实机验证；跨 Space 未加 `collectionBehavior`，待验。
  - 纯单测：`settings_origin`（主屏/副屏负坐标/小屏左上锚定）、窗口标题精确匹配。

- [x] **Step 2: 主题跟随（设置窗单独可见）**
  - 设置 pass 在 `settings.open` 时驱动 `Shared.theme.maybe_apply`，应用后立即重绘
    本视口，并按 `CHECK_INTERVAL` 排下一帧；隐藏后不排帧（L4）。
  - `open_settings` 在隐藏→显示时锁外探测 `theme::current_theme()` 并
    `check_now_with`，设置窗打开即用当前系统外观。

- [x] **Step 3: 关闭/录制 hook（不实现快捷键）**
  - 红点 / ⌘W → `SettingsState::mark_hidden()`：`open=false` +
    `cancel_recording_pending=true`；`App::logic` 取走并留痕。
  - headless 测试：⌘W 被消费/同帧只消费一次、裸 W 不触发；隐藏状态纯转换；
    `hide_with_command` 粘合测试（状态置隐藏 + 挂起取消录制 + `Visible(false)`）。

- [x] **Step 4: Task 5 review 可选加固**
  - 自启关闭成功（`desired=false, Ok(NotRegistered)`）单测：关态、无错误、托盘同步 OFF。
  - 托盘同帧切换失败：设置页已加载实际态时按 `s.settings.autostart` 校准；
    未知（设置页从未打开）回退点击前值——避免把托盘安装时读到的真实启用态误改成关。
    纯函数 `autostart_check_after_failure(known, checked_after_click)` 钉住。
  - Task 5 的 `git add` 命令已修正（补 `autostart.rs`/`.gitignore`，移除 `tray.rs`）。

- [x] **Step 5: 文档**
  - 本文下方实机记录与验收清单；`settings-visual.md` 状态/已知说明；
    `design.md` §5.1 居中/主题跟随与子视口标题栏 PoC 结论。

- [x] **Step 6: 验证与提交**
  - `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings &&
    cargo test --workspace` 全绿。
  - 提交 `feat(app): 设置窗口收尾（居中/主题/文档）`。

### 实机结果（controller，2026-10-18）

- 首次打开：设置窗在鼠标所在屏居中于 **(425,155)** 并置前（controller 实测坐标）。
- 移动窗口到 **(300,200)** → ⌘W 关闭 → 再次打开：位置保持 **(300,200)**，不重新居中。
- ROOT 隐藏且 App 隐藏时，托盘「设置…」把设置窗唤到 **(300,200)**（**同 Space**）。
- 关闭：设置窗口 **⌘W 隐藏**（App 不退出）。
- 字号：**Large 即时应用、重启保持、无裁切**；随后已恢复 **Standard**。
- 开机自启：开 / 关均生效（系统「登录项」条目增加 / 移除）；`autostart-off`
  用户意图标记写入后已恢复。
- **仍待验**：主题跟随（设置窗单独可见）、**跨 Space**、托盘「开机自启」勾选视觉。

### 手动验收清单（controller）

- [x] 首次打开（齿轮或托盘「设置…」）：设置窗在**鼠标所在屏**居中（可见区，扣菜单栏/
  Dock），且置前成 key（实测 (425,155)）。
- [x] 关闭后（可先拖动窗口）再打开：保留上次位置，只有首次打开居中（实测 (300,200)）。
- [x] ROOT 隐藏且 App 隐藏时，从托盘「设置…」打开：设置窗被唤到前台（**同 Space**，
  实测位置 (300,200)）。
- [ ] 设置窗最小化 → 托盘/齿轮恢复（`deminiaturize`；待 controller 实机验证）。
- [ ] 已打开时再点入口：同一实例置前，不新建、不重置位置。
- [ ] 系统浅色 ↔ 深色：设置窗口 2s 内跟随；ROOT/悬浮条都隐藏时同样生效。
- [ ] **跨 Space**：ROOT 与设置窗位于其他 Space 时的唤起行为待验（本期未加
  `collectionBehavior`，不擅自新增 Space 行为）。
- [x] 字号 小/标准/大：设置窗与另两窗即时变化、重启保持（Large 已验，Standard 已恢复）。
- [x] 开机自启开/关（设置页）：系统「登录项」条目增加 / 移除；`autostart-off` 标记
  写入后已恢复。审批态（若系统要求）显示关态 +
  「已注册,待系统设置「登录项」允许」。
- [ ] 托盘「开机自启」勾选视觉与设置页一致（**待 controller 复核**，沿用 Task 5 缺口）。
- [x] 红点 / ⌘W：隐藏设置窗、App 不退出；再次打开同一实例（⌘W 已验）。
- [x] 大字号（Large）：整页可滚动、标题栏固定、内容无裁切（已验）。

**验证记录（2026-10-18，quality review follow-up）：** `cargo fmt --check` /
`cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace`
全绿。测试覆盖：`settings_origin` 2 个（含小屏左上锚定）、标题精确匹配 1 个、
首开定位/关闭取消录制/⌘W/隐藏粘合各 1 个、自启关闭成功 1 个、托盘失败勾选校准 1 个。

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

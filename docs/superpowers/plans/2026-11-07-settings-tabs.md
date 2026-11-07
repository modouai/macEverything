# 设置页顶部图标标签 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把设置页从单页长列表改为 5 个顶部图标标签（通用 / 索引 / 高级 / 实验 / 关于），保持现有设置项语义与视觉 token，支持中英双语与运行期语言切换。

**Architecture:** 新增 `ui/settings_tabs.rs` 负责标签栏几何、命中与 5 个自绘图标；`settings_view.rs` 的 `draw_body` 改为「固定标签栏 + 按 `SettingsTab` 分发页面」；`SettingsState` 持有 `active_tab` 并在关闭重开时保留；所有标签/页面文案走 i18n catalog。

**Tech Stack:** Rust / eframe + egui 0.37 / 现有 `settings_tokens`、`settings_layout`、i18n catalog / headless egui 测试。

**Spec:** `docs/superpowers/specs/2026-11-07-settings-tabs-design.md`

**约定（全计划通用）:**
- 每个 Task 结束前跑 `cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings && cargo test -p macEverything-app`。
- 新增测试必须做突变验证：临时改坏实现确认测试变红，再改回。
- 提交粒度：每个 Task 一次 commit；提交信息用 `feat(app): ...` 前缀。
- 字体/尺寸测试使用 headless egui；实机验证用 `open release/macEverything.app`（不要命令行直启，避免继承终端 TCC 权限）。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/app/src/ui/settings_tabs.rs`（新增） | `SettingsTab` 枚举、标签栏绘制、命中区域、5 个自绘图标 |
| `crates/app/src/ui/settings_tokens.rs` | 新增标签栏 token 与 `tab_selected_fill` |
| `crates/app/src/i18n/catalog.rs` | 5 个标签中英文；快捷键子分组标题 |
| `crates/app/src/ui/settings_view.rs` | `draw_body` 分发、5 个页面绘制、`SettingsData.active_tab`、`SelectTab` 动作 |
| `crates/app/src/ui/settings.rs` | `SettingsState.active_tab`、快照、动作分发 |
| `crates/app/src/ui/mod.rs` | 注册 `settings_tabs` 模块 |
| `docs/specs/settings-v2.1/design.md` | 页面结构同步 |
| `docs/specs/floatbar-visual/settings-visual.md` | 标签栏 token/几何同步 |

---

## Task 1: Tokens、i18n 与 SettingsTab 枚举

**Files:**
- Modify: `crates/app/src/ui/settings_tokens.rs`
- Modify: `crates/app/src/i18n/catalog.rs`
- Create: `crates/app/src/ui/settings_tabs.rs`
- Modify: `crates/app/src/ui/mod.rs`

- [ ] **Step 1: 加 i18n 标签与子分组标题**

在 `catalog!` 中、`group_general` 附近加入：

```rust
    settings_tab_general: "通用" / "General",
    settings_tab_index: "索引" / "Index",
    settings_tab_advanced: "高级" / "Advanced",
    settings_tab_experimental: "实验" / "Experimental",
    settings_tab_about: "关于" / "About",
    group_shortcuts: "快捷键" / "Shortcuts",
```

- [ ] **Step 2: 加 token 与色板字段**

在 `settings_tokens.rs` 的几何常量区加入：

```rust
pub const TABBAR_H: f32 = 68.0;
pub const TAB_ITEM_W: f32 = 96.0;
pub const TAB_ITEM_H: f32 = 52.0;
pub const TAB_GAP: f32 = 4.0;
pub const TAB_ICON: f32 = 26.0;
pub const TAB_FONT: f32 = 11.0;
pub const TAB_PAD_X: f32 = 10.0;
pub const TAB_RADIUS: u8 = 9;
```

`SettingsPalette` 增加字段：

```rust
    /// 选中标签的浅色底（系统蓝低透明度）。
    pub tab_selected_fill: egui::Color32,
```

`dark()` 加 `tab_selected_fill: c(0x0A84FF1A)`，`light()` 加 `tab_selected_fill: c(0x007AFF1A)`。

把 `settings_palette_matches_spec_table` 的数组长度从 20 改成 21，并追加：

```rust
            (
                dark().tab_selected_fill,
                light().tab_selected_fill,
                0x0A84FF1A,
                0x007AFF1A,
                "标签选中底",
            ),
```

- [ ] **Step 3: 创建 settings_tabs.rs（先写失败测试）**

```rust
//! 设置页顶部标签栏：枚举、几何、命中与自绘图标。
//!
//! 只负责标签栏本身；页面绘制在 `settings_view`，状态编排在 `settings`。

use eframe::egui;

use crate::i18n;
use crate::ui::settings_tokens::{
    SettingsPalette, TAB_ICON, TAB_ITEM_H, TAB_ITEM_W, TAB_FONT, TAB_GAP, TAB_RADIUS, TABBAR_H,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SettingsTab {
    #[default]
    General,
    Index,
    Advanced,
    Experimental,
    About,
}

impl SettingsTab {
    pub const ALL: [Self; 5] = [
        Self::General,
        Self::Index,
        Self::Advanced,
        Self::Experimental,
        Self::About,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::General => i18n::s().settings_tab_general,
            Self::Index => i18n::s().settings_tab_index,
            Self::Advanced => i18n::s().settings_tab_advanced,
            Self::Experimental => i18n::s().settings_tab_experimental,
            Self::About => i18n::s().settings_tab_about,
        }
    }

    pub fn scroll_id(self) -> &'static str {
        match self {
            Self::General => "settings-page-general",
            Self::Index => "settings-page-index",
            Self::Advanced => "settings-page-advanced",
            Self::Experimental => "settings-page-experimental",
            Self::About => "settings-page-about",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn order_is_general_index_advanced_experimental_about() {
        assert_eq!(
            SettingsTab::ALL,
            [
                SettingsTab::General,
                SettingsTab::Index,
                SettingsTab::Advanced,
                SettingsTab::Experimental,
                SettingsTab::About,
            ]
        );
    }

    #[test]
    fn labels_cover_both_languages() {
        for (lang, expected) in [
            (
                Lang::ZhHans,
                ["通用", "索引", "高级", "实验", "关于"],
            ),
            (
                Lang::En,
                ["General", "Index", "Advanced", "Experimental", "About"],
            ),
        ] {
            crate::i18n::with_lang(lang, || {
                let labels: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.label()).collect();
                assert_eq!(labels, expected);
            });
        }
    }

    #[test]
    fn scroll_ids_are_unique() {
        let ids: Vec<&str> = SettingsTab::ALL.iter().map(|tab| tab.scroll_id()).collect();
        let mut dedup = ids.clone();
        dedup.sort_unstable();
        dedup.dedup();
        assert_eq!(dedup.len(), ids.len());
    }
}
```

- [ ] **Step 4: 注册模块并跑测试**

`crates/app/src/ui/mod.rs` 的 `pub mod settings_layout;` 后加：

```rust
pub mod settings_tabs;
```

运行：

```bash
cargo test -p macEverything-app ui::settings_tabs
```

预期：3 个测试通过（若 `with_lang` 不可见，确认它在 `i18n/mod.rs` 中为 `pub(crate)`）。

- [ ] **Step 5: 跑门禁并提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
cargo test -p macEverything-app ui::settings_tabs ui::settings_tokens
git add crates/app/src/ui/settings_tabs.rs crates/app/src/ui/settings_tokens.rs crates/app/src/i18n/catalog.rs crates/app/src/ui/mod.rs
git commit -m "feat(app): 设置页标签枚举、token 与 i18n"
```

---

## Task 2: 标签栏绘制、命中与图标

**Files:**
- Modify: `crates/app/src/ui/settings_tabs.rs`

- [ ] **Step 1: 先写几何/命中失败测试**

在 `settings_tabs` 测试模块加入：

```rust
    #[test]
    fn item_rects_are_centered_and_evenly_spaced() {
        let bar = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(620.0, TABBAR_H),
        );
        let rects = tab_item_rects(bar);
        assert_eq!(rects.len(), 5);
        let total = TAB_ITEM_W * 5.0 + TAB_GAP * 4.0;
        let expected_left = bar.center().x - total / 2.0;
        assert!((rects[0].left() - expected_left).abs() < 0.01);
        for pair in rects.windows(2) {
            assert!((pair[1].left() - pair[0].right() - TAB_GAP).abs() < 0.01);
        }
    }

    #[test]
    fn hit_test_maps_points_to_tabs() {
        let bar = egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(620.0, TABBAR_H),
        );
        let rects = tab_item_rects(bar);
        for (i, rect) in rects.iter().enumerate() {
            assert_eq!(hit_test(bar, rect.center()), Some(SettingsTab::ALL[i]));
        }
        assert_eq!(hit_test(bar, egui::pos2(2.0, bar.center().y)), None);
    }
```

- [ ] **Step 2: 跑测试确认失败（函数不存在）**

```bash
cargo test -p macEverything-app ui::settings_tabs
```

预期：编译失败，`tab_item_rects` / `hit_test` 未定义。

- [ ] **Step 3: 实现几何与命中**

在 `settings_tabs.rs` 加入：

```rust
/// 5 个标签项的矩形（相对标签栏居中）。
pub fn tab_item_rects(bar: egui::Rect) -> [egui::Rect; 5] {
    let total_w = TAB_ITEM_W * 5.0 + TAB_GAP * 4.0;
    let start_x = bar.center().x - total_w / 2.0;
    let top = bar.center().y - TAB_ITEM_H / 2.0;
    std::array::from_fn(|i| {
        egui::Rect::from_min_size(
            egui::pos2(start_x + i as f32 * (TAB_ITEM_W + TAB_GAP), top),
            egui::vec2(TAB_ITEM_W, TAB_ITEM_H),
        )
    })
}

pub fn hit_test(bar: egui::Rect, pos: egui::Pos2) -> Option<SettingsTab> {
    tab_item_rects(bar)
        .iter()
        .position(|rect| rect.contains(pos))
        .map(|i| SettingsTab::ALL[i])
}
```

- [ ] **Step 4: 跑几何测试**

```bash
cargo test -p macEverything-app ui::settings_tabs
```

预期：全部通过。

- [ ] **Step 5: 先写渲染失败测试**

```rust
    #[test]
    fn tab_bar_renders_five_labels_and_selected_background() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::light());
        let mut clicked = None;
        let out = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(620.0, TABBAR_H),
                )),
                ..Default::default()
            },
            |ui| {
                clicked = draw_tab_bar(ui, SettingsTab::Index, &crate::ui::settings_tokens::light());
            },
        );
        let labels: Vec<String> = out
            .shapes
            .iter()
            .flat_map(|clipped| text_shapes(&clipped.shape))
            .map(|text| text.galley.text().to_owned())
            .collect();
        for tab in SettingsTab::ALL {
            assert!(labels.iter().any(|label| label == tab.label()), "缺少标签 {}", tab.label());
        }
        assert!(clicked.is_none(), "无点击不应返回标签");
    }

    fn text_shapes(shape: &egui::Shape) -> Vec<egui::epaint::TextShape> {
        match shape {
            egui::Shape::Text(text) => vec![text.clone()],
            egui::Shape::Vec(shapes) => shapes.iter().flat_map(text_shapes).collect(),
            _ => Vec::new(),
        }
    }
```

- [ ] **Step 6: 跑测试确认失败（draw_tab_bar 未定义）**

```bash
cargo test -p macEverything-app ui::settings_tabs
```

- [ ] **Step 7: 实现 draw_tab_bar 与图标**

在 `settings_tabs.rs` 加入：

```rust
use crate::ui::font as ui_font;

pub fn draw_tab_bar(
    ui: &mut egui::Ui,
    active: SettingsTab,
    palette: &SettingsPalette,
) -> Option<SettingsTab> {
    let width = ui.available_width();
    let (bar, _) = ui.allocate_exact_size(egui::vec2(width, TABBAR_H), egui::Sense::hover());
    let painter = ui.painter().clone();
    painter.rect_filled(bar, egui::CornerRadius::ZERO, palette.window_fill);
    painter.hline(
        bar.x_range(),
        bar.bottom() - 0.5,
        egui::Stroke::new(1.0, palette.divider),
    );

    let mut clicked = None;
    for (i, tab) in SettingsTab::ALL.iter().enumerate() {
        let rect = tab_item_rects(bar)[i];
        let response = ui.interact(
            rect,
            ui.id().with(("settings-tab", i)),
            egui::Sense::click(),
        );
        let selected = *tab == active;
        let hovered = response.hovered();
        if selected {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(TAB_RADIUS),
                palette.tab_selected_fill,
            );
        } else if hovered {
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(TAB_RADIUS),
                palette.control_fill,
            );
        }
        let color = if selected {
            palette.switch_on
        } else {
            palette.text_secondary
        };
        let icon_center = egui::pos2(rect.center().x, rect.top() + 8.0 + TAB_ICON / 2.0);
        draw_tab_icon(&painter, *tab, icon_center, TAB_ICON, color);
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 12.0),
            egui::Align2::CENTER_CENTER,
            tab.label(),
            ui_font::id(TAB_FONT),
            color,
        );
        if response.clicked() {
            clicked = Some(*tab);
        }
        let _ = response.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    clicked
}

fn draw_tab_icon(
    painter: &egui::Painter,
    tab: SettingsTab,
    center: egui::Pos2,
    size: f32,
    color: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.7, color);
    let scale = size / 24.0;
    let p = |x: f32, y: f32| {
        egui::pos2(center.x + (x - 12.0) * scale, center.y + (y - 12.0) * scale)
    };
    match tab {
        SettingsTab::General => {
            for (y, knob_x) in [(7.0, 8.0), (12.0, 15.0), (17.0, 10.0)] {
                painter.line_segment([p(4.0, y), p(20.0, y)], stroke);
                painter.circle_filled(p(knob_x, y), 1.6 * scale, color);
            }
        }
        SettingsTab::Index => {
            painter.add(egui::Shape::line(
                vec![
                    p(3.0, 6.5),
                    p(9.0, 6.5),
                    p(11.0, 8.5),
                    p(20.0, 8.5),
                    p(20.0, 18.0),
                    p(3.0, 18.0),
                    p(3.0, 6.5),
                ],
                stroke,
            ));
            painter.circle_stroke(p(15.3, 13.3), 2.8 * scale, stroke);
            painter.line_segment([p(17.3, 15.3), p(19.5, 17.5)], stroke);
        }
        SettingsTab::Advanced => {
            let rect = egui::Rect::from_min_size(p(3.0, 4.5), egui::vec2(18.0 * scale, 15.0 * scale));
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same((2.5 * scale) as u8),
                stroke,
                egui::StrokeKind::Inside,
            );
            painter.add(egui::Shape::line(
                vec![p(7.0, 9.0), p(10.0, 12.0), p(7.0, 15.0)],
                stroke,
            ));
            painter.line_segment([p(12.5, 15.0), p(17.0, 15.0)], stroke);
        }
        SettingsTab::Experimental => {
            painter.line_segment([p(9.5, 3.0), p(14.5, 3.0)], stroke);
            painter.add(egui::Shape::line(
                vec![
                    p(10.5, 3.0),
                    p(10.5, 9.2),
                    p(5.4, 18.0),
                    p(5.8, 20.0),
                    p(7.5, 21.0),
                    p(16.5, 21.0),
                    p(18.2, 20.0),
                    p(18.6, 18.0),
                    p(13.5, 9.2),
                    p(13.5, 3.0),
                ],
                stroke,
            ));
            painter.line_segment([p(7.8, 15.0), p(16.2, 15.0)], stroke);
        }
        SettingsTab::About => {
            painter.circle_stroke(center, 9.0 * scale, stroke);
            painter.line_segment([p(12.0, 10.6), p(12.0, 17.0)], stroke);
            painter.circle_filled(p(12.0, 7.5), 0.9 * scale, color);
        }
    }
}
```

注意：`painter.rect_stroke` 在 egui 0.37 的签名是 `(rect, corner_radius, stroke, stroke_kind)`；若编译报错，按当前版本调整参数顺序。

- [ ] **Step 8: 跑测试**

```bash
cargo test -p macEverything-app ui::settings_tabs
```

预期：5 个测试通过。

- [ ] **Step 9: 清理已消费的临时 allow**

逐一删除 `settings_tokens.rs` 中 8 个 `TAB_*` 常量与 `tab_selected_fill` 的 `#[allow(dead_code)]`（Task 2 的 `draw_tab_bar`/`draw_tab_icon` 已引用）。同时尝试删除 `SettingsTab`/`impl SettingsTab` 上的 scoped allow；若生产 bin 仍判定未接入（调用方要到 Task 4 才出现），保留 scoped allow 并在 Task 4 删除。`group_shortcuts` 的 allow 保留到 Task 4。每删一项跑一次 `cargo clippy -p macEverything-app --all-targets -- -D warnings`，以编译器为准。

- [ ] **Step 10: 门禁 + 提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
git add crates/app/src/ui/settings_tabs.rs
git commit -m "feat(app): 设置页顶部标签栏与自绘图标"
```

---

## Task 3: 状态接线（active_tab / SelectTab / 快照）

**Files:**
- Modify: `crates/app/src/ui/settings_view.rs`
- Modify: `crates/app/src/ui/settings.rs`

- [ ] **Step 1: 先写失败测试**

在 `settings.rs` 的 tests 里加入：

```rust
    #[test]
    fn settings_default_tab_is_general_and_survives_reopen() {
        let mut state = test_state();
        assert_eq!(state.active_tab, crate::ui::settings_tabs::SettingsTab::General);
        state.active_tab = crate::ui::settings_tabs::SettingsTab::Advanced;
        // 关闭/打开只改 open；active_tab 是运行期字段，不随 open 复位。
        state.open = true;
        state.open = false;
        assert_eq!(
            state.active_tab,
            crate::ui::settings_tabs::SettingsTab::Advanced
        );
    }
```

- [ ] **Step 2: 跑测试确认失败（字段不存在）**

```bash
cargo test -p macEverything-app ui::settings::tests::settings_default_tab
```

- [ ] **Step 3: 实现状态**

`SettingsState` 增加：

```rust
    /// 设置页当前标签（运行期；重启回 General，不写入 settings.toml）。
    pub(crate) active_tab: crate::ui::settings_tabs::SettingsTab,
```

`SettingsState::new` 初始化：

```rust
            active_tab: crate::ui::settings_tabs::SettingsTab::General,
```

`SettingsData`（`settings_view.rs`）增加：

```rust
    /// 当前标签页。
    pub(crate) active_tab: SettingsTab,
```

顶部 import：

```rust
use crate::ui::settings_tabs::SettingsTab;
```

快照（`settings.rs` 的 `SettingsData { ... }`）增加：

```rust
            active_tab: state.active_tab,
```

所有现有 `SettingsData { ... }` 字面量（含测试辅助 `layout_test_data`）补 `active_tab: SettingsTab::General`；`SettingsData` 不实现 `Default`，必须显式列出。

`SettingsAction` 增加：

```rust
    /// 切换设置页标签。
    SelectTab(SettingsTab),
```

动作分发（`settings.rs` 的 match）加入：

```rust
        SettingsAction::SelectTab(tab) => {
            lock_shared(shared).settings.active_tab = tab;
            ctx.request_repaint_of(viewport_id());
        }
```

- [ ] **Step 4: 跑测试**

```bash
cargo test -p macEverything-app ui::settings
```

- [ ] **Step 5: 门禁 + 提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
git add crates/app/src/ui/settings.rs crates/app/src/ui/settings_view.rs
git commit -m "feat(app): 设置页 active_tab 状态与 SelectTab 动作"
```

---

## Task 4: draw_body 分发 + 通用页

**Files:**
- Modify: `crates/app/src/ui/settings_view.rs`

- [ ] **Step 1: 先写失败测试**

在 `settings_view` 的 tests 加入：

```rust
    #[test]
    fn general_page_has_five_rows_and_no_top_level_header() {
        let mut data = layout_test_data();
        data.active_tab = SettingsTab::General;
        let out = run_body(&data);
        let texts = all_texts(&out);
        let labels: Vec<&str> = texts.iter().map(|text| text.galley.text()).collect();
        for expected in [
            i18n::s().row_font_size,
            i18n::s().row_autostart,
            i18n::s().row_language,
            i18n::s().row_palette_hotkey,
            i18n::s().row_window_hotkey,
        ] {
            assert!(labels.contains(&expected), "通用页缺少 {expected:?}");
        }
        assert!(
            !labels.contains(&i18n::s().group_general),
            "顶层「通用」标题应移除"
        );
        assert!(labels.contains(&i18n::s().group_shortcuts), "缺少快捷键子分组");
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p macEverything-app general_page_has_five_rows
```

- [ ] **Step 3: 让 draw_card 支持无标题**

把 `draw_card` 的 `title: &str` 改为 `title: Option<&str>`，在内部：

```rust
    let card = draw_group_frame(ui, full_w, title, &specs, palette);
```

`draw_group_frame` 也改为 `title: Option<&str>`；当 `None` 时不画分组标题、卡片从当前游标开始：

```rust
fn draw_group_frame(
    ui: &mut egui::Ui,
    full_w: f32,
    title: Option<&str>,
    rows: &[RowSpec],
    palette: &SettingsPalette,
) -> egui::Rect {
    let inner_w = (full_w - 2.0 * CONTENT_PAD.1).max(0.0);
    if let Some(title) = title {
        let header_h = ui.fonts_mut(|fonts| fonts.row_height(&ui_font::id(GROUP_HEADER_FONT)));
        let (header_slot, _) =
            ui.allocate_exact_size(egui::vec2(full_w, header_h), egui::Sense::hover());
        let header = egui::Rect::from_min_size(
            egui::pos2(header_slot.left() + CONTENT_PAD.1, header_slot.top()),
            egui::vec2(inner_w, header_h),
        );
        draw_text(
            ui,
            header,
            title,
            GROUP_HEADER_FONT,
            palette.text_secondary,
            egui::Align2::LEFT_CENTER,
        );
        ui.add_space(GROUP_HEADER_GAP);
    }
    let card_h = card_height(rows);
    let (card_slot, _) = ui.allocate_exact_size(egui::vec2(full_w, card_h), egui::Sense::hover());
    let card = egui::Rect::from_min_size(
        egui::pos2(card_slot.left() + CONTENT_PAD.1, card_slot.top()),
        egui::vec2(inner_w, card_h),
    );
    ui.painter().rect_filled(
        card,
        egui::CornerRadius::same(CARD_RADIUS),
        palette.card_fill,
    );
    card
}
```

保留现有带标题调用的语义：把 `draw_card(..., i18n::s().group_volumes, ...)` 改成 `draw_card(..., Some(i18n::s().group_volumes), ...)`；无标题页面传 `None`。

- [ ] **Step 4: 拆分通用页**

把 `draw_general_group` 拆成两个函数：

```rust
fn draw_general_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let font_error = data.font_error.as_ref().map(SettingsError::text);
    let autostart_error = data.autostart_error.as_ref().map(AutostartError::text);
    let language_error = data.language_error.as_ref().map(LanguageError::text);
    let autostart_note = if autostart_error.is_none() && data.autostart_approval {
        Some(i18n::s().autostart_approval_note)
    } else {
        None
    };
    let has_message = autostart_error.is_some() || autostart_note.is_some();

    let rows = [
        GroupRow {
            kind: RowKind::FontSize,
            spec: RowSpec::new(ROW_H).with_extra(extra_for(font_error.is_some(), extra_h)),
            error: font_error.as_deref(),
            note: None,
            note2: None,
            hint: None,
        },
        GroupRow {
            kind: RowKind::Autostart,
            spec: RowSpec::new(ROW_H).with_extra(extra_for(has_message, extra_h)),
            error: autostart_error.as_deref(),
            note: autostart_note,
            note2: None,
            hint: None,
        },
        GroupRow {
            kind: RowKind::Language,
            spec: RowSpec::new(ROW_H).with_extra(extra_for(language_error.is_some(), extra_h)),
            error: language_error.as_deref(),
            note: None,
            note2: None,
            hint: None,
        },
    ];
    draw_card(ui, full_w, None, &rows, data, palette, actions);

    ui.add_space(GROUP_GAP);
    draw_shortcuts_page(ui, full_w, data, palette, extra_h, actions);
}

fn draw_shortcuts_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let palette_error = hotkey_error_for(data, HotkeySlot::Palette);
    let palette_note = data.hotkeys.note.as_ref().map(HotkeyNote::text);
    let palette_hint =
        (data.recording == Some(HotkeySlot::Palette)).then_some(i18n::s().recording_hint);
    let window_error = hotkey_error_for(data, HotkeySlot::Window);
    let window_note = data.hotkeys.window_note.as_ref().map(HotkeyNote::text);
    let window_hint =
        (data.recording == Some(HotkeySlot::Window)).then_some(i18n::s().recording_hint);

    let rows = [
        GroupRow {
            kind: RowKind::PaletteKeys,
            spec: RowSpec::new(ROW_H).with_extra(hotkey_messages_extra(
                palette_error,
                palette_note.as_deref(),
                palette_hint,
                extra_h,
            )),
            error: palette_error,
            note: palette_note.as_deref(),
            note2: None,
            hint: palette_hint,
        },
        GroupRow {
            kind: RowKind::WindowKeys,
            spec: RowSpec::new(ROW_H).with_extra(hotkey_messages_extra(
                window_error,
                window_note.as_deref(),
                window_hint,
                extra_h,
            )),
            error: window_error,
            note: window_note.as_deref(),
            note2: None,
            hint: window_hint,
        },
    ];
    draw_card(ui, full_w, Some(i18n::s().group_shortcuts), &rows, data, palette, actions);
}
```

- [ ] **Step 5: 重构 draw_body**

在 `settings_view.rs` 顶部补：

```rust
use crate::ui::settings_tabs::{draw_tab_bar, SettingsTab};
use crate::ui::settings_tokens::TABBAR_H;
```

把 `draw_body` 改成：

```rust
pub(crate) fn draw_body(ui: &mut egui::Ui, data: &SettingsData) -> BodyOutput {
    let window = ui.max_rect();
    let tab_top = window.top() + TITLEBAR_H;
    let tab_rect = egui::Rect::from_min_max(
        egui::pos2(window.left(), tab_top),
        egui::pos2(window.right(), tab_top + TABBAR_H),
    );
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(window.left(), tab_rect.bottom()),
        window.max,
    );
    let palette = settings_tokens::palette(ui.visuals().dark_mode);
    let mut actions = Vec::new();
    // 先取消录制，再处理标签点击：保证同一帧内动作顺序为 Cancel → SelectTab。
    if click_away_cancels_recording(ui, data) {
        actions.push(SettingsAction::CancelHotkeyRecord);
    }

    let mut tab_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(tab_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    if let Some(tab) = draw_tab_bar(&mut tab_ui, data.active_tab, &palette) {
        actions.push(SettingsAction::SelectTab(tab));
    }

    let mut body = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(content_rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT)),
    );
    body.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
    body.spacing_mut().scroll.floating = true;

    egui::ScrollArea::vertical()
        .id_salt(data.active_tab.scroll_id())
        .auto_shrink([false, false])
        .show(&mut body, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            let full_w = ui.available_width();
            let extra_h = support_extra_height(ui);
            ui.add_space(CONTENT_PAD.0);
            match data.active_tab {
                SettingsTab::General => {
                    draw_general_page(ui, full_w, data, &palette, extra_h, &mut actions);
                }
                SettingsTab::Index => {
                    draw_index_page(ui, full_w, data, &palette, extra_h, &mut actions);
                }
                SettingsTab::Advanced => {
                    draw_advanced_page(ui, full_w, data, &palette, extra_h, &mut actions);
                }
                SettingsTab::Experimental => {
                    draw_experimental_page(ui, full_w, data, &palette, extra_h, &mut actions);
                }
                SettingsTab::About => {
                    draw_about_page(ui, full_w, data, &palette, extra_h, &mut actions);
                }
            }
            ui.add_space(CONTENT_PAD.0);
        });
    BodyOutput { actions }
}
```

保留 `TITLEBAR_H` 偏移：标题栏由 `settings.rs` 在同一窗口内绘制，标签栏紧接标题栏下方（`window.top() + TITLEBAR_H`），内容区从标签栏底边开始。

- [ ] **Step 6: 实现其余页面的最小分发**

本 Task 需要所有页面可编译。加入以下函数（索引页在 Task 5 补外接磁盘；高级/实验页直接落地；关于页暂用现有 about 组）：

```rust
fn draw_index_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    draw_index_group(ui, full_w, data, palette, extra_h, actions);
}

fn draw_about_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    draw_about_group(ui, full_w, data, palette, extra_h, actions);
}

fn draw_advanced_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    draw_cli_page(ui, full_w, data, palette, extra_h, actions);
}

fn draw_cli_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let cli = cli_row(data);
    let cli_extra = extra_h * cli.line_count() as f32;
    let cli_error = cli.error.clone();
    let cli_note = cli.note.clone();
    let cli_note2 = cli.note2.clone();
    let cli_hint = cli.hint;
    let rows = [GroupRow {
        kind: RowKind::CliInstall(cli),
        spec: RowSpec::new(ROW_H).with_extra(cli_extra),
        error: cli_error.as_deref(),
        note: cli_note.as_deref(),
        note2: cli_note2.as_deref(),
        hint: cli_hint,
    }];
    draw_card(ui, full_w, None, &rows, data, palette, actions);
}

fn draw_experimental_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    draw_panel_assist_page(ui, full_w, data, palette, extra_h, actions);
}

fn draw_panel_assist_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    let panel_error = data.panel_assist_error.as_ref().map(SettingsError::text);
    let rows = [GroupRow {
        kind: RowKind::PanelAssist,
        spec: RowSpec::new(ROW_H)
            .with_extra(extra_for(panel_error.is_some(), extra_h))
            .with_extra(extra_h)
            .with_extra(extra_h),
        error: panel_error.as_deref(),
        note: Some(i18n::s().row_panel_assist_desc),
        note2: None,
        hint: Some(i18n::s().panel_assist_restart_hint),
    }];
    draw_card(ui, full_w, None, &rows, data, palette, actions);
}
```

- [ ] **Step 7: 跑测试**

```bash
cargo test -p macEverything-app general_page_has_five_rows
```

- [ ] **Step 8: 清理临时 allow 与废弃标题 key**

- 删除 `SettingsTab`/`impl SettingsTab` 上剩余的 scoped `#[allow(dead_code)]`（`draw_body` 已调用 `draw_tab_bar`）。
- 删除 `group_shortcuts` 的字段级 allow（`draw_shortcuts_page` 已消费）。
- 从 `catalog!` 删除 `group_general`（通用页不再画顶层标题）；更新所有引用它的测试断言（例如把「不得包含 group_general」改为断言新的子分组标题）。
- `group_index` / `group_about` 暂留到 Task 5/6 删除。
- 跑 `cargo clippy -p macEverything-app --all-targets -- -D warnings` 确认没有遗留 `allow(dead_code)` 掩盖漏接。

- [ ] **Step 9: 门禁 + 提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
cargo test -p macEverything-app ui::settings_view
git add crates/app/src/ui/settings_view.rs
git commit -m "refactor(app): 设置页标签分发与通用页"
```

---

## Task 5: 索引页（排除目录 + 外接磁盘）

**Files:**
- Modify: `crates/app/src/ui/settings_view.rs`

- [ ] **Step 1: 先写失败测试**

```rust
    #[test]
    fn index_page_has_exclusions_and_volumes_without_page_header() {
        let mut data = layout_test_data();
        data.active_tab = SettingsTab::Index;
        data.excluded = vec!["~/Library".into()];
        let out = run_body(&data);
        let labels: Vec<&str> = all_texts(&out)
            .iter()
            .map(|text| text.galley.text())
            .collect();
        assert!(labels.contains(&i18n::s().row_excluded));
        assert!(labels.contains(&i18n::s().group_volumes));
        assert!(!labels.contains(&i18n::s().group_index));
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p macEverything-app index_page_has_exclusions
```

- [ ] **Step 3: 实现索引页**

```rust
fn draw_index_page(
    ui: &mut egui::Ui,
    full_w: f32,
    data: &SettingsData,
    palette: &SettingsPalette,
    extra_h: f32,
    actions: &mut Vec<SettingsAction>,
) {
    draw_index_group(ui, full_w, data, palette, extra_h, actions);
    ui.add_space(GROUP_GAP);
    draw_volumes_group(ui, full_w, data, palette, extra_h, actions);
}
```

把 `draw_index_group` 的 `draw_card` 标题从 `i18n::s().group_index` 改为 `Some(i18n::s().row_excluded)`（子分组「排除目录」）；`draw_volumes_group` 保持 `Some(i18n::s().group_volumes)`。从 `catalog!` 删除 `group_index`，并更新引用它的测试断言。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p macEverything-app index_page_has_exclusions
```

- [ ] **Step 5: 门禁 + 提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
cargo test -p macEverything-app ui::settings_view
git add crates/app/src/ui/settings_view.rs crates/app/src/i18n/catalog.rs
git commit -m "refactor(app): 设置页索引标签页"
```

---

## Task 6: 高级 / 实验 / 关于页

**Files:**
- Modify: `crates/app/src/ui/settings_view.rs`

- [ ] **Step 1: 先写失败测试**

```rust
    #[test]
    fn advanced_page_only_has_cli_row() {
        let mut data = layout_test_data();
        data.active_tab = SettingsTab::Advanced;
        let labels: Vec<&str> = all_texts(&run_body(&data))
            .iter()
            .map(|text| text.galley.text())
            .collect();
        assert!(labels.iter().any(|l| l.contains("macEverything") || l.contains("命令行工具")));
        assert!(!labels.contains(&i18n::s().row_font_size));
    }

    #[test]
    fn experimental_page_has_panel_assist_row_only() {
        let mut data = layout_test_data();
        data.active_tab = SettingsTab::Experimental;
        let labels: Vec<&str> = all_texts(&run_body(&data))
            .iter()
            .map(|text| text.galley.text())
            .collect();
        assert!(labels.contains(&i18n::s().row_panel_assist));
        assert!(!labels.contains(&i18n::s().row_excluded));
    }

    #[test]
    fn about_page_has_version_and_auto_check_only() {
        let mut data = layout_test_data();
        data.active_tab = SettingsTab::About;
        let labels: Vec<&str> = all_texts(&run_body(&data))
            .iter()
            .map(|text| text.galley.text())
            .collect();
        assert!(labels.iter().any(|l| l.contains("v2.7.1")));
        assert!(labels.contains(&i18n::s().row_auto_check));
        assert!(!labels.contains(&i18n::s().row_language));
    }
```

- [ ] **Step 2: 跑测试确认失败**

```bash
cargo test -p macEverything-app advanced_page_only_has_cli_row
```

- [ ] **Step 3: 调整关于页卡片标题**

`draw_cli_page` / `draw_panel_assist_page` 已在 Task 4 Step 6 落地，本 Task 不重复实现。唯一改动：把 `draw_about_group` 的 `draw_card` 标题从 `Some(i18n::s().group_about)` 改为 `None`（页面内不再重复顶层标题）。

- [ ] **Step 4: 跑测试**

```bash
cargo test -p macEverything-app advanced_page_only_has_cli_row experimental_page_has_panel_assist_row_only about_page_has_version_and_auto_check_only
```

- [ ] **Step 5: 门禁 + 提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
cargo test -p macEverything-app ui::settings_view
git add crates/app/src/ui/settings_view.rs
git commit -m "refactor(app): 设置页高级/实验/关于标签页"
```

---

## Task 7: 测试重构、录制取消、滚动与多语言

**Files:**
- Modify: `crates/app/src/ui/settings_view.rs`（测试）
- Modify: `crates/app/src/ui/settings_tabs.rs`（测试）

- [ ] **Step 1: 加测试辅助**

在 `settings_view` 的 tests 模块：

```rust
    fn with_tab(mut data: SettingsData, tab: SettingsTab) -> SettingsData {
        data.active_tab = tab;
        data
    }
```

- [ ] **Step 2: 按页更新现有测试**

按下面映射，在构造 `SettingsData` 后套 `with_tab`（或直接 `data.active_tab = ...`）：

| 测试 | 目标标签 |
|---|---|
| `language_row_exposes_three_options_and_action` / `language_save_error_renders_one_extra_line` | General |
| `panel_assist_enable_prompt_modal_renders` | Experimental |
| `panel_assist_click_emits_toggle_action` / `panel_assist_untrusted_shows_auth_link` | Experimental |
| `cli_row_*` / `cli_error_row_adds_card_height` | Advanced |
| `empty_excluded_list_shows_placeholder_row` / `add_button_click_emits_add_exclude` / `reset_button_click_emits_reset_action` / `busy_disables_reset_action` / `remove_click_emits_exact_path` / `busy_disables_add_and_remove_actions` / `busy_support_line_distinguishes_list_and_rescan` / `exclude_error_and_busy_draw_with_empty_state` | Index |
| `auto_check_click_emits_toggle_action` / `update_action_button_emits_phase_specific_action` / `website_link_click_emits_open_website` | About |
| 字号/自启/语言/快捷键/键帽/录制相关测试 | General |

`settings_layout_has_all_rows` 改成逐页遍历：

```rust
    #[test]
    fn settings_layout_has_all_rows() {
        for tab in SettingsTab::ALL {
            let data = with_tab(layout_test_data(), tab);
            let out = run_body(&data);
            let labels: Vec<&str> = all_texts(&out)
                .iter()
                .map(|text| text.galley.text())
                .collect();
            let expected: &[&str] = match tab {
                SettingsTab::General => &[
                    "字号", "开机自启", "语言", "唤起快捷键", "完整窗口快捷键",
                ],
                SettingsTab::Index => &["排除目录", "外接磁盘"],
                SettingsTab::Advanced => &["命令行工具"],
                SettingsTab::Experimental => &["保存面板目录助手（实验）"],
                SettingsTab::About => &["版本", "启动时自动检查更新"],
            };
            for label in expected {
                assert!(labels.contains(label), "{tab:?} 缺少 {label:?}");
            }
        }
    }
```

`english_settings_page_covers_all_sections` 同样逐页断言英文标签；把原来的全量数组拆成每页子集。

- [ ] **Step 3: 录制取消顺序测试**

```rust
    #[test]
    fn tab_click_cancels_recording_before_selecting() {
        let mut data = layout_test_data();
        data.recording = Some(HotkeySlot::Palette);
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::light());
        // 首帧建立键帽字段临时 rect，供 click_away_cancels_recording 判定。
        let _ = run_frame(&ctx, &data, Vec::new());
        let bar = egui::Rect::from_min_size(
            egui::pos2(0.0, TITLEBAR_H),
            egui::vec2(SETTINGS_W, TABBAR_H),
        );
        let target = crate::ui::settings_tabs::tab_item_rects(bar)[1].center();
        let (_, press) = run_frame(&ctx, &data, vec![pointer_button(target, true)]);
        assert_eq!(
            press.actions.first(),
            Some(&SettingsAction::CancelHotkeyRecord),
            "按下标签应先取消录制: {:?}",
            press.actions
        );
        let (_, release) = run_frame(&ctx, &data, vec![pointer_button(target, false)]);
        assert_eq!(
            release.actions,
            vec![SettingsAction::SelectTab(SettingsTab::Index)],
            "释放标签应切换页"
        );
    }
```

测试模块顶部补：

```rust
use crate::ui::settings_tabs::tab_item_rects;
```

并把现有 `use crate::ui::settings_tokens::{SETTINGS_H, SETTINGS_W};` 改为：

```rust
use crate::ui::settings_tokens::{SETTINGS_H, SETTINGS_W, TABBAR_H, TITLEBAR_H};
```

如果 `SettingsAction` 未派生 `PartialEq`，先给它补 `#[derive(Debug, Clone, PartialEq, Eq)]` 再运行。

- [ ] **Step 4: 多语言宽度测试**

在 `settings_tabs` tests 加入：

```rust
    #[test]
    fn labels_fit_item_width_in_both_languages() {
        let ctx = egui::Context::default();
        for lang in [Lang::ZhHans, Lang::En] {
            crate::i18n::with_lang(lang, || {
                ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(620.0, TABBAR_H),
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        for tab in SettingsTab::ALL {
                            let width = ui
                                .painter()
                                .layout_no_wrap(
                                    tab.label().to_owned(),
                                    crate::ui::font::id(TAB_FONT),
                                    egui::Color32::PLACEHOLDER,
                                )
                                .size()
                                .x;
                            assert!(
                                width <= TAB_ITEM_W - 2.0 * crate::ui::settings_tokens::TAB_PAD_X,
                                "{:?} 标签 {:?} 超宽: {width}",
                                lang,
                                tab.label()
                            );
                        }
                    },
                );
            });
        }
    }
```

注意：默认测试字体不含中文字形时，中文按 tofu 宽度计算；若想更严格，在测试 ctx 上安装应用字体（需要把 `install_ui_fonts` 提升为 `pub(crate)`，或复用现有字体测试辅助）。实机验证仍会检查真实 SF Pro 下的英文标签。

- [ ] **Step 5: 滚动 id 与切页保留测试**

`SettingsTab::scroll_id()` 已测唯一。补一个状态测试：设置 `active_tab` 为 Index，模拟关闭/打开设置，断言仍为 Index（复用 Task 3 测试）。

- [ ] **Step 6: 跑全量测试并修复坐标**

```bash
cargo test -p macEverything-app
```

预期：所有测试通过。若失败，按上面映射检查是否漏设 `active_tab`；坐标点击失败时，确认目标行在当前页的 y 坐标内。

- [ ] **Step 7: 突变验证**

逐个新测试临时改坏对应实现，确认变红后改回（至少覆盖：标签顺序、标签点击、页面分发、录制取消顺序、i18n 宽度）。

- [ ] **Step 8: 门禁 + 提交**

```bash
cargo fmt --check && cargo clippy -p macEverything-app --all-targets -- -D warnings
cargo test -p macEverything-app
git add crates/app/src/ui/settings_view.rs crates/app/src/ui/settings_tabs.rs
git commit -m "test(app): 设置页标签页测试重构与多语言覆盖"
```

---

## Task 8: 文档同步、实机验证与最终门禁

**Files:**
- Modify: `docs/specs/settings-v2.1/design.md`
- Modify: `docs/specs/floatbar-visual/settings-visual.md`
- Modify: `docs/architecture.md`（设置页一行描述）

- [ ] **Step 1: 文档同步**

- `docs/specs/settings-v2.1/design.md`：把「设置页 = 单页三组」改为「5 个顶部图标标签 + 每页子分组」；列出标签顺序与页面内容。
- `docs/specs/floatbar-visual/settings-visual.md`：加入标签栏几何（68pt/96×52/26pt 图标/11pt 文字/4pt 间距）、选中态（系统蓝 + 10% 蓝底）、`tab_selected_fill` 深浅值。
- `docs/architecture.md`：设置页一行描述改为「5 标签（通用/索引/高级/实验/关于）+ 每页卡片」。

- [ ] **Step 2: 全量门禁**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

预期：全绿（app 测试数应 ≥ 原 618 + 新增测试数）。

- [ ] **Step 3: 构建与实机验证**

```bash
cargo build --release && bash scripts/bundle.sh
launchctl setenv MCE_APP_LOG /tmp/mce-settings-tabs.log
open release/macEverything.app
launchctl unsetenv MCE_APP_LOG
```

验证矩阵：
- 5 个标签顺序、图标、选中态；点击切换；
- 通用页一屏可见；索引页长列表滚动；
- 高级页 CLI 行；实验页授权链接/开关；关于页更新入口；
- 关闭设置再打开保留上次标签；重启回通用；
- 录制快捷键时点击标签先取消录制；
- 深浅主题、字号倍率；中英切换即时刷新且不截断。

- [ ] **Step 4: 提交**

```bash
git add docs/specs/settings-v2.1/design.md docs/specs/floatbar-visual/settings-visual.md docs/architecture.md
git commit -m "docs: 设置页标签信息架构同步"
```

---

## 自查记录

- **Spec 覆盖**：5 标签/顺序/页面划分（Task 1/4/5/6）、图标与几何（Task 1/2）、选中态/悬停（Task 2）、状态与保留（Task 3）、录制取消（Task 7）、滚动 id（Task 3/7）、多语言（Task 1/7）、测试/风险/文档（Task 7/8）。
- **占位符扫描**：无 TBD/TODO；每个实现步骤给出代码或精确改动点。
- **类型一致性**：`SettingsTab` 在 `settings_tabs` 定义，`SettingsData.active_tab` 与 `SettingsState.active_tab` 使用同一类型；`SettingsAction::SelectTab(SettingsTab)`；`scroll_id()` 由枚举提供。
- **顺序依赖**：Task 1（枚举/token/i18n）→ Task 2（标签栏）→ Task 3（状态）→ Task 4（分发/通用）→ Task 5（索引）→ Task 6（高级/实验/关于）→ Task 7（测试）→ Task 8（文档/实机）。

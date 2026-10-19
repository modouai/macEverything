# i18n 中英双语（v2.1）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让图形界面（三窗 + 托盘 + 原生菜单/弹窗）支持简体中文与 English，设置页语言项可用，切换立即生效并持久化。

**Architecture:** app crate 内新增 `i18n` 模块：宏生成中英两套编译期文案表，进程级 `AtomicU8` 保存当前语言；设置页写入语言并触发三视口重绘，托盘菜单在主线程显式 `set_text`；原生弹窗/右键菜单每次调用时取当前语言。

**Tech Stack:** Rust 2021、eframe/egui 0.37、objc2/objc2-foundation（NSLocale）、muda/tray-icon、serde/toml。

**依据:** `docs/specs/i18n-v2.1/design.md`

---

## 通用约定

- 每个任务收尾必须全绿：
  ```bash
  cargo fmt --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```
- 现有中文断言依赖默认语言 = 简体中文；英文用例一律：
  ```rust
  i18n::with_lang(i18n::Lang::En, || { /* 断言 */ });
  ```
- 禁止在测试里调用 `i18n::set_lang`（进程级状态会与并行测试竞态）。
- 日志、注释、CLI 输出不翻译；引擎/客户端返回的原始错误串在 GUI 中按技术细节原样展示。
- 新增文案一律先加进 `crates/app/src/i18n/catalog.rs` 的 `catalog!` 块，再在调用点取用；不允许在 UI 里留硬编码中英文。
- 每个改动的界面文件顶部加 `use crate::i18n;`（需要具体类型时再导入 `Lang`/`LanguageSetting`/`format`）。`settings_view.rs` 还需 `use super::settings::{AutostartError, ExcludeError, LanguageError, SettingsError};`，`settings_view.rs`/`palette.rs` 需要 `use crate::hotkey::HotkeyNote;`。

### 文件结构

| 文件 | 职责 |
|---|---|
| `crates/app/src/i18n/mod.rs`（新） | `Lang`、`LanguageSetting`、全局语言、系统语言解析、`with_lang` |
| `crates/app/src/i18n/catalog.rs`（新） | 宏 + 全部文案条目（中/英各一份） |
| `crates/app/src/i18n/format.rs`（新） | 带参数文案的类型化渲染 |
| `crates/app/src/settings.rs` | `settings.toml` 新增 `language` 字段 |
| `crates/app/src/main.rs` | 声明 `mod i18n`；启动时解析并设置语言 |
| `crates/app/src/platform/{mod,mac}.rs` | 读取系统首选语言；设置窗标题双语匹配 |
| `crates/app/src/ui/settings.rs` | 语言动作、保存、错误类型、托盘刷新标记 |
| `crates/app/src/ui/settings_view.rs` | 语言下拉、设置页全部文案、错误渲染 |
| `crates/app/src/ui/palette.rs` | 悬浮条文案、角标/热键提示类型化 |
| `crates/app/src/ui/results.rs` | 结果窗口文案、状态提示类型化 |
| `crates/app/src/tray.rs` | 托盘菜单文案、`relabel()` |
| `crates/app/src/ui/menu_actions.rs` | 右键菜单项文案 |
| `crates/app/src/hotkey.rs` | 持久热键提示类型化（`HotkeyNote`） |
| `crates/app/src/autostart.rs` | 状态说明文案映射（日志仍中文） |
| `crates/app/src/engine_launcher.rs` | 用户可见错误类型化 |
| `crates/app/src/ui/mod.rs` | 消费托盘刷新标记；启动语言接线 |

---

## Task 1: i18n 核心 + 启动语言

**Files:**
- Modify: `crates/app/Cargo.toml`
- Create: `crates/app/src/i18n/mod.rs`
- Create: `crates/app/src/i18n/catalog.rs`
- Modify: `crates/app/src/main.rs`
- Modify: `crates/app/src/settings.rs`
- Modify: `crates/app/src/platform/mod.rs`
- Modify: `crates/app/src/platform/mac.rs`
- Modify: `crates/app/src/ui/settings.rs`（仅标题取文案）

- [ ] **Step 1: 打开 NSLocale feature**

`crates/app/Cargo.toml` 的 `objc2-foundation` features 列表末尾追加：

```toml
    # i18n：NSLocale.preferredLanguages() 读取系统首选语言。
    "NSLocale",
```

- [ ] **Step 2: 写失败测试（语言解析与覆盖）**

`crates/app/src/i18n/mod.rs`（先只放测试与空壳亦可，下一步补实现）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_first_supported_language() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(resolve_system_language(&p(&["en-US"])), Lang::En);
        assert_eq!(resolve_system_language(&p(&["zh-Hans-CN"])), Lang::ZhHans);
        assert_eq!(resolve_system_language(&p(&["zh-Hant-TW"])), Lang::ZhHans);
        assert_eq!(resolve_system_language(&p(&["ja-JP"])), Lang::En);
        assert_eq!(resolve_system_language(&p(&["fr-FR", "en-US"])), Lang::En);
        assert_eq!(resolve_system_language(&p(&[])), Lang::ZhHans);
    }

    #[test]
    fn explicit_setting_wins_over_system() {
        let preferred = vec!["en-US".to_string()];
        assert_eq!(resolve_language(LanguageSetting::ZhHans, &preferred), Lang::ZhHans);
        assert_eq!(resolve_language(LanguageSetting::En, &preferred), Lang::En);
        assert_eq!(resolve_language(LanguageSetting::System, &preferred), Lang::En);
    }

    #[test]
    fn with_lang_overrides_then_restores() {
        assert_eq!(lang(), Lang::ZhHans);
        with_lang(Lang::En, || assert_eq!(lang(), Lang::En));
        assert_eq!(lang(), Lang::ZhHans);
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p macEverything-app i18n::tests`
Expected: FAIL（模块/类型不存在）。

- [ ] **Step 4: 实现 `i18n/mod.rs`**

```rust
//! 图形界面文案目录与全局语言（v2.1）。
//!
//! 只服务 app crate 的界面文案；CLI、日志、引擎/客户端原始错误不经过这里。
//! 当前语言是进程级原子（与 `ui/font.rs` 的字号倍率同模式）；测试用线程局部
//! `with_lang` 覆盖，避免并行测试竞态。

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Deserializer, Serialize};

pub mod catalog;

pub use catalog::{all_entries, s, strings, Strings};

/// 运行时语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Lang {
    ZhHans = 0,
    En = 1,
}

/// `settings.toml` 的语言设置（默认跟随系统）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LanguageSetting {
    #[default]
    System,
    ZhHans,
    En,
}

impl<'de> Deserialize<'de> for LanguageSetting {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(match raw.as_str() {
            "system" => Self::System,
            "zh-hans" => Self::ZhHans,
            "en" => Self::En,
            other => {
                log::warn!("未知语言设置 {other:?},按跟随系统处理");
                Self::System
            }
        })
    }
}

static LANG_BITS: AtomicU8 = AtomicU8::new(Lang::ZhHans as u8);

#[cfg(test)]
thread_local! {
    static TEST_LANG: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
}

/// 当前语言（测试线程内优先读线程局部覆盖）。
pub fn lang() -> Lang {
    #[cfg(test)]
    if let Some(lang) = TEST_LANG.with(std::cell::Cell::get) {
        return lang;
    }
    match LANG_BITS.load(Ordering::Relaxed) {
        1 => Lang::En,
        _ => Lang::ZhHans,
    }
}

/// 生产写入点仅两处：启动读取设置后、设置页切换语言后。
pub fn set_lang(lang: Lang) {
    LANG_BITS.store(lang as u8, Ordering::Relaxed);
}

/// 系统首选语言列表 → 语言：按顺序取第一个受支持项；无受支持项 → English；
/// 空列表（检测失败）→ 简体中文。
pub fn resolve_system_language(preferred: &[String]) -> Lang {
    for tag in preferred {
        let tag = tag.trim().to_ascii_lowercase();
        if tag.starts_with("zh") {
            return Lang::ZhHans;
        }
        if tag.starts_with("en") {
            return Lang::En;
        }
    }
    if preferred.is_empty() {
        Lang::ZhHans
    } else {
        Lang::En
    }
}

/// 设置项 → 实际语言；显式选择忽略系统列表。
pub fn resolve_language(setting: LanguageSetting, preferred: &[String]) -> Lang {
    match setting {
        LanguageSetting::System => resolve_system_language(preferred),
        LanguageSetting::ZhHans => Lang::ZhHans,
        LanguageSetting::En => Lang::En,
    }
}

#[cfg(test)]
pub(crate) fn with_lang<T>(lang: Lang, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<Lang>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_LANG.with(|cell| cell.set(self.0));
        }
    }
    let previous = TEST_LANG.with(|cell| cell.replace(Some(lang)));
    let _restore = Restore(previous);
    f()
}
```

- [ ] **Step 5: 实现 `i18n/catalog.rs`（宏 + 首条文案）**

```rust
//! 编译期文案目录：每条文案只声明一次，生成两套静态表。
//!
//! 宏保证两套表都填满 `Strings` 的全部字段——漏翻编译期报错。

macro_rules! catalog {
    ($( $field:ident : $zh:literal / $en:literal ),* $(,)?) => {
        #[derive(Debug, Clone, Copy)]
        pub struct Strings {
            $( pub $field: &'static str, )*
        }

        static ZH_HANS: Strings = Strings { $( $field: $zh, )* };
        static EN: Strings = Strings { $( $field: $en, )* };

        pub fn strings(lang: crate::i18n::Lang) -> &'static Strings {
            match lang {
                crate::i18n::Lang::ZhHans => &ZH_HANS,
                crate::i18n::Lang::En => &EN,
            }
        }

        pub fn s() -> &'static Strings {
            strings(crate::i18n::lang())
        }

        /// 测试清单（key, 中文, English）；仅供单测，生产不读。
        #[allow(dead_code)]
        pub fn all_entries() -> &'static [(&'static str, &'static str, &'static str)] {
            &[ $( (stringify!($field), $zh, $en), )* ]
        }
    };
}

catalog! {
    settings_title: "设置" / "Settings",
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(start) = rest.find('{') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else { break };
            out.push(&after[..end]);
            rest = &after[end + 1..];
        }
        out
    }

    #[test]
    fn entries_are_complete_and_placeholders_match() {
        for (key, zh, en) in all_entries() {
            assert!(!zh.trim().is_empty(), "{key} 中文为空");
            assert!(!en.trim().is_empty(), "{key} 英文为空");
            assert_eq!(zh.trim(), *zh, "{key} 中文首尾空白");
            assert_eq!(en.trim(), *en, "{key} 英文首尾空白");
            assert_eq!(placeholders(zh), placeholders(en), "{key} 中英占位符不一致");
        }
    }
}
```

- [ ] **Step 6: `settings.rs` 加 `language` 字段**

在 `use` 区加 `use crate::i18n::LanguageSetting;`，`SettingsFile` 改为：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SettingsFile {
    #[serde(default)]
    pub font_size: FontSize,
    #[serde(default)]
    pub language: LanguageSetting,
    #[serde(default)]
    pub palette_hotkey: Option<HotKey>,
    #[serde(default)]
    pub window_hotkey: Option<HotKey>,
}
```

在 `settings.rs` 的测试模块追加：

```rust
#[test]
fn language_defaults_and_unknown_value_falls_back_to_system() {
    let dir = tempfile::tempdir().unwrap();
    let p = path(dir.path());
    std::fs::write(&p, "font_size = \"large\"\nlanguage = \"klingon\"\n").unwrap();
    let f = load(dir.path());
    assert_eq!(f.language, LanguageSetting::System);
    assert_eq!(f.font_size, FontSize::Large, "未知语言不得拖垮其它字段");
}

#[test]
fn language_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let f = SettingsFile {
        language: LanguageSetting::En,
        ..SettingsFile::default()
    };
    save(dir.path(), &f).unwrap();
    assert_eq!(load(dir.path()).language, LanguageSetting::En);
}
```

- [ ] **Step 7: 平台层读取系统语言**

`crates/app/src/platform/mac.rs` 顶部 import 增加：

```rust
use objc2_foundation::NSLocale;
```

新增函数（放在现有平台函数附近）：

```rust
/// 系统首选语言（按偏好顺序；macOS `NSLocale.preferredLanguages`）。
pub fn preferred_languages() -> anyhow::Result<Vec<String>> {
    let langs = NSLocale::preferredLanguages();
    Ok(langs.iter().map(|s| s.to_string()).collect())
}
```

`crates/app/src/platform/mod.rs` 增加 facade：

```rust
/// 系统首选语言列表；非 macOS 返回错误（调用方回落到简体中文）。
pub fn preferred_languages() -> anyhow::Result<Vec<String>> {
    #[cfg(target_os = "macos")]
    {
        mac::preferred_languages()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(anyhow::anyhow!("非 macOS:无法读取系统语言"))
    }
}
```

- [ ] **Step 8: `main.rs` 启动接线**

顶部模块声明加：

```rust
mod i18n;
```

在 `let settings = settings::load(&paths.data_dir);` 之后追加：

```rust
// i18n：必须在任何视口/托盘创建之前设置语言（首帧与首次安装的菜单都取当前值）。
let preferred = platform::preferred_languages().unwrap_or_else(|err| {
    log::warn!("读取系统语言失败,按简体中文: {err}");
    Vec::new()
});
let lang = i18n::resolve_language(settings.language, &preferred);
i18n::set_lang(lang);
log::info!("启动语言:{lang:?}(设置 {:?})", settings.language);
```

- [ ] **Step 9: 设置窗标题改为取目录**

`crates/app/src/ui/settings.rs`：

- `builder()` 的 `.with_title(platform::SETTINGS_TITLE)` 改为 `.with_title(crate::i18n::s().settings_title)`。
- `draw` 中标题栏 `platform::SETTINGS_TITLE` 改为 `crate::i18n::s().settings_title`。
- 对应单测（`assert_eq!(b.title.as_deref(), Some(platform::SETTINGS_TITLE))`）改为：

```rust
assert_eq!(
    b.title.as_deref(),
    Some(crate::i18n::s().settings_title),
    "设置窗标题取当前语言"
);
```

- [ ] **Step 10: 跑全量验证并提交**

Run:
```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: 全绿。

```bash
git add crates/app/Cargo.toml Cargo.lock crates/app/src/i18n crates/app/src/main.rs crates/app/src/settings.rs crates/app/src/platform/mod.rs crates/app/src/platform/mac.rs crates/app/src/ui/settings.rs
git commit -m "feat(app): i18n 核心与启动语言"
```

---

## Task 2: 设置页语言下拉与保存

**Files:**
- Modify: `crates/app/src/i18n/catalog.rs`
- Modify: `crates/app/src/ui/settings_view.rs`
- Modify: `crates/app/src/ui/settings.rs`
- Modify: `crates/app/src/settings.rs`（移除 `FontSize::label`）

- [ ] **Step 1: 追加设置页文案**

在 `catalog!` 块中追加（保留已有 `settings_title`）：

```rust
    // 分组 / 行
    group_appearance: "外观" / "Appearance",
    group_general: "通用" / "General",
    group_index: "索引" / "Index",
    row_font_size: "字号" / "Text Size",
    row_autostart: "开机自启" / "Launch at Login",
    row_language: "语言" / "Language",
    row_palette_hotkey: "唤起快捷键" / "Quick Search Shortcut",
    row_window_hotkey: "完整窗口快捷键" / "Full Window Shortcut",
    row_excluded: "排除目录" / "Excluded Folders",
    // 字号档
    font_small: "小" / "Small",
    font_standard: "标准" / "Standard",
    font_large: "大" / "Large",
    // 语言选项
    language_system: "跟随系统" / "System Default",
    language_zh_hans: "简体中文" / "简体中文",
    language_en: "English" / "English",
    // 排除目录
    add_folder: "＋ 添加目录…" / "Add Folder…",
    restore_defaults: "恢复默认" / "Restore Defaults",
    excluded_empty: "未排除任何目录" / "No excluded folders",
    exclude_loading: "加载中…" / "Loading…",
    exclude_rescanning: "重扫中…" / "Rescanning…",
    // 快捷键录制
    recording_prompt: "按下快捷键…" / "Press shortcut…",
    recording_hint: "按 Esc 或点击别处取消,Backspace 恢复默认" / "Press Esc or click elsewhere to cancel; Backspace restores the default",
    recording_hint_hover: "按 Esc 或点击别处取消录制,Backspace 恢复默认" / "Press Esc or click elsewhere to cancel recording; Backspace restores the default",
    keycap_hover_record: "点击录制快捷键" / "Click to record shortcut",
    keycap_hover_rerecord: "点击改为录制此快捷键" / "Click to record a new shortcut",
    // 提示 / 错误
    autostart_approval_note: "已注册,待系统设置「登录项」允许" / "Registered. Allow it in System Settings > Login Items.",
    reset_confirm_title: "恢复默认排除目录？" / "Restore default excluded folders?",
    reset_confirm_message: "将清空自定义排除项，恢复 ~/Library、node_modules、.git。此操作会触发一次索引重扫。" / "This clears custom exclusions and restores ~/Library, node_modules, and .git. This will trigger a full index rescan.",
    reset_confirm_action: "恢复默认" / "Restore Defaults",
    settings_save_failed: "设置保存失败" / "Failed to save settings",
    language_save_failed: "语言已切换,但设置保存失败" / "Language changed, but the settings could not be saved",
    autostart_read_failed: "读取开机自启状态失败" / "Failed to read Launch at Login status",
    autostart_not_effective: "开机自启未生效" / "Launch at Login did not take effect",
    autostart_set_failed: "开机自启设置失败" / "Failed to set Launch at Login",
    exclude_failed_no_reason: "排除目录操作失败(daemon 未给出原因)" / "Excluded folders operation failed (the engine did not provide a reason)",
    exclude_worker_exited: "排除目录工作线程异常退出(无结果)" / "Excluded folders worker exited unexpectedly (no result)",
    exclude_worker_start_failed: "无法启动排除目录工作线程" / "Could not start the excluded folders worker",
    // 自启状态说明
    autostart_status_enabled: "已启用(系统设置「登录项」可见)" / "Enabled (visible in System Settings > Login Items)",
    autostart_status_requires_approval: "已注册,待用户在系统设置「登录项」中允许" / "Registered. Awaiting approval in System Settings > Login Items",
    autostart_status_not_registered: "未注册(已关闭)" / "Not registered (off)",
    autostart_status_not_found: "未找到条目(注册尚未生效)" / "Entry not found (registration not yet effective)",
```

- [ ] **Step 2: 写失败测试（语言行交互与错误行）**

`crates/app/src/ui/settings_view.rs` 测试模块追加（复用现有 `layout_test_data`、`run_frame`、`click_at`、`all_texts`、`find_text`、`text_center`）：

```rust
use crate::i18n;

fn click_language(ctx: &egui::Context, data: &SettingsData, label: &str) -> BodyOutput {
    // 1. 先出帧拿语言行控件中心（字段中心与键帽字段同一几何）。
    let (out, _) = run_frame(ctx, data, Vec::new());
    let field = keycap_field_center(&out, i18n::s().row_language);
    // 2. 点击字段打开 Popup（按下/抬起）。
    let _ = click_at(ctx, data, field);
    // 3. 再出一帧让 Popup 渲染，按文本命中选项。
    let (out, _) = run_frame(ctx, data, Vec::new());
    let pos = text_center(find_text(&all_texts(&out), label));
    // 4. 点击选项。
    click_at(ctx, data, pos)
}

#[test]
fn language_row_exposes_three_options_and_action() {
    i18n::with_lang(i18n::Lang::En, || {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());
        let data = layout_test_data();
        let out = click_language(&ctx, &data, "English");
        assert!(
            out.actions
                .contains(&SettingsAction::SetLanguage(i18n::LanguageSetting::En)),
            "点击 English 应产生 SetLanguage(En): {:?}",
            out.actions
        );
    });
}

#[test]
fn language_save_error_renders_one_extra_line() {
    let data = SettingsData {
        language: i18n::LanguageSetting::En,
        language_error: Some(LanguageError::SaveFailed),
        ..layout_test_data()
    };
    let out = run_body(&data);
    let texts = all_texts(&out);
    find_text(&texts, "语言已切换,但设置保存失败");
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p macEverything-app language_`
Expected: FAIL（字段/动作不存在）。

- [ ] **Step 4: 实现设置错误类型**

`crates/app/src/ui/settings.rs` 新增：

```rust
use crate::autostart::Status;
use crate::i18n::{self, LanguageSetting};

/// 设置保存失败（原始细节在构造处进日志，界面显示本地化文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsError {
    SaveFailed,
}

impl SettingsError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::SaveFailed => i18n::s().settings_save_failed.to_owned(),
        }
    }
}

/// 语言保存失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LanguageError {
    SaveFailed,
}

impl LanguageError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::SaveFailed => i18n::s().language_save_failed.to_owned(),
        }
    }
}

/// 开机自启相关错误（原始细节在构造处进日志）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutostartError {
    ReadFailed,
    NotEffective(Status),
    SetFailed,
}

impl AutostartError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::ReadFailed => i18n::s().autostart_read_failed.to_owned(),
            Self::NotEffective(status) => {
                format!("{}: {}", i18n::s().autostart_not_effective, status_text(*status))
            }
            Self::SetFailed => i18n::s().autostart_set_failed.to_owned(),
        }
    }
}

/// 自启状态说明（设置页行内错误使用；日志仍走 `Status::note()` 中文）。
pub(crate) fn status_text(status: Status) -> &'static str {
    match status {
        Status::Enabled => i18n::s().autostart_status_enabled,
        Status::RequiresApproval => i18n::s().autostart_status_requires_approval,
        Status::NotRegistered => i18n::s().autostart_status_not_registered,
        Status::NotFound => i18n::s().autostart_status_not_found,
    }
}

/// 排除目录错误（daemon 原始错误原样展示，其余本地化）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExcludeError {
    Daemon(String),
    NoReason,
    WorkerExited,
    StartFailed,
}

impl ExcludeError {
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Daemon(detail) => detail.clone(),
            Self::NoReason => i18n::s().exclude_failed_no_reason.to_owned(),
            Self::WorkerExited => i18n::s().exclude_worker_exited.to_owned(),
            Self::StartFailed => i18n::s().exclude_worker_start_failed.to_owned(),
        }
    }
}
```

> `Status` 若未派生 `Copy`，把 `status_text(*status)` 改为按值匹配（先 `clone()`）。

- [ ] **Step 5: 改 `SettingsState` 字段与动作**

`SettingsState` 字段类型改为：

```rust
pub(crate) autostart_error: Option<AutostartError>,
pub(crate) font_error: Option<SettingsError>,
pub(crate) language_error: Option<LanguageError>,
pub(crate) exclude_error: Option<ExcludeError>,
pub(crate) tray_relabel_pending: bool,
```

`SettingsState::new` 初始化加 `language_error: None, tray_relabel_pending: false`。

`SettingsAction` 加：

```rust
/// 语言下拉选择（立即应用 + 落盘）。
SetLanguage(LanguageSetting),
```

`apply_action` 加分支：

```rust
SettingsAction::SetLanguage(setting) => apply_language(shared, ctx, setting),
```

新增方法：

```rust
pub(crate) fn set_language_result(
    &mut self,
    setting: LanguageSetting,
    save_error: Option<LanguageError>,
) {
    self.file.language = setting;
    self.language_error = save_error;
}

pub(crate) fn request_tray_relabel(&mut self) {
    self.tray_relabel_pending = true;
}

pub(crate) fn take_tray_relabel_pending(&mut self) -> bool {
    std::mem::take(&mut self.tray_relabel_pending)
}

/// 语言切换时清掉一次性错误（跨帧常驻状态使用类型化文案，不清）。
pub(crate) fn clear_transient_messages(&mut self) {
    self.hotkey_error = None;
}
```

- [ ] **Step 6: 改状态写入点**

`exclude_result`：

```rust
fn exclude_result(outcome: anyhow::Result<client::ExcludeOutcome>) -> ExcludeResult {
    match outcome {
        Ok(outcome) if outcome.ok => ExcludeResult {
            entries: outcome.excluded,
            error: None,
        },
        Ok(outcome) => ExcludeResult {
            entries: Vec::new(),
            error: Some(match outcome.message {
                Some(message) => ExcludeError::Daemon(message),
                None => ExcludeError::NoReason,
            }),
        },
        Err(err) => {
            let detail = format!("{err:#}");
            log::warn!("排除目录 daemon 调用失败: {detail}");
            ExcludeResult {
                entries: Vec::new(),
                error: Some(ExcludeError::Daemon(detail)),
            }
        }
    }
}
```

同时把 `ExcludeResult.error` 类型改为 `Option<ExcludeError>`。

`tick_exclude`：daemon 错误分支 `Some(ExcludeError::Daemon(error))`；线程异常退出前先 `log::warn!`，再 `Some(ExcludeError::WorkerExited)`。

`exclude_spawn_failed`：参数改为 `String` 详情；先 `log::warn!`，再 `self.exclude_error = Some(ExcludeError::StartFailed)`。

`autostart_loaded`：`Err(err) => { log::warn!("读取开机自启状态失败: {err}"); self.autostart_error = Some(AutostartError::ReadFailed); }`。

`apply_autostart_status`：`self.autostart_error = if !enabled && status != Status::RequiresApproval && desired { Some(AutostartError::NotEffective(status)) } else { None };`

`apply_autostart_result`：`Err(message) => { log::warn!("开机自启设置失败: {message}"); self.autostart_error = Some(AutostartError::SetFailed); ... }`。

`set_font_size_result`：`save_error: Option<SettingsError>`。

- [ ] **Step 7: 实现 `apply_language`**

`crates/app/src/ui/settings.rs`：

```rust
/// 语言即时生效：解析 → 全局切换 → 落盘（失败仅内联留痕）→ 重绘三窗 + 托盘刷新标记。
fn apply_language(
    shared: &Arc<Mutex<Shared>>,
    ctx: &egui::Context,
    setting: LanguageSetting,
) {
    let preferred = platform::preferred_languages().unwrap_or_else(|err| {
        log::warn!("读取系统语言失败,按简体中文: {err}");
        Vec::new()
    });
    let lang = crate::i18n::resolve_language(setting, &preferred);
    crate::i18n::set_lang(lang);

    let (mut file, data_dir) = {
        let s = lock_shared(shared);
        (s.settings.file.clone(), s.settings.data_dir.clone())
    };
    file.language = setting;

    let save_error = store::save(&data_dir, &file).err().map(|err| {
        let detail = format!("{err:#}");
        log::warn!("语言 {setting:?} 已应用,但设置保存失败:{detail}");
        LanguageError::SaveFailed
    });

    {
        let mut s = lock_shared(shared);
        s.settings.set_language_result(setting, save_error);
        s.settings.clear_transient_messages();
        s.settings.request_tray_relabel();
        s.results.clear_transient_messages();
    }
    log::info!("设置语言 → {setting:?}({lang:?})");

    ctx.request_repaint_of(egui::ViewportId::ROOT);
    ctx.request_repaint_of(crate::ui::palette_viewport_id());
    ctx.request_repaint_of(viewport_id());
}
```

`ResultsState` 新增：

```rust
/// 语言切换时清掉一次性状态栏消息（重查/下次操作会重建）。
pub fn clear_transient_messages(&mut self) {
    self.status_note = None;
    self.status_note_is_error = false;
}
```

- [ ] **Step 8: 设置页渲染改用文案与错误类型**

`crates/app/src/ui/settings_view.rs`：

- 删除常量 `ADD_DIR_LABEL`、`RESET_LABEL`、`LANGUAGE_LABEL`、`LANGUAGE_TOOLTIP`、`EXCLUDED_EMPTY`、`EXCLUDE_LOADING`、`EXCLUDE_RESCANNING`、`AUTOSTART_APPROVAL_NOTE`、`RECORDING_PROMPT`、`RECORDING_HINT`。
- `SettingsData` 的错误字段与语言字段改为：

```rust
pub(crate) autostart_error: Option<AutostartError>,
pub(crate) font_error: Option<SettingsError>,
pub(crate) language: LanguageSetting,
pub(crate) language_error: Option<LanguageError>,
pub(crate) exclude_error: Option<ExcludeError>,
```

- `from_state` 改为：

```rust
autostart_error: state.autostart_error.clone(),
font_error: state.font_error.clone(),
language: state.file.language,
language_error: state.language_error.clone(),
exclude_error: state.exclude_error.clone(),
```

- 测试辅助 `layout_test_data()` 同步改为：

```rust
language: LanguageSetting::System,
language_error: None,
```

- 现有 headless 断言中的「简体中文」改为「跟随系统」（语言字段默认显示 `System Default` 的当前语言文案）；`settings_layout_has_all_rows` 的期望列表同步更新。

- 分组标题：`draw_card(ui, full_w, i18n::s().group_appearance, ...)`，通用/索引同理。
- 行标签：`字号` → `i18n::s().row_font_size`；自启/语言/两个快捷键同理。
- `draw_segmented` 内三档文案改用：

```rust
fn font_label(size: FontSize) -> &'static str {
    match size {
        FontSize::Small => i18n::s().font_small,
        FontSize::Standard => i18n::s().font_standard,
        FontSize::Large => i18n::s().font_large,
    }
}

// draw_segmented 内：
for (index, option) in options.into_iter().enumerate() {
    // ...
    draw_text(/* ... */, font_label(option), /* ... */);
}
```

- 错误/提示渲染：在 `draw_*_group` 内先把 typed error 转成 `String`，再 `as_deref()` 传给 `GroupRow.error`：

```rust
let autostart_error = data.autostart_error.as_ref().map(AutostartError::text);
let font_error = data.font_error.as_ref().map(SettingsError::text);
let language_error = data.language_error.as_ref().map(LanguageError::text);
let exclude_error = data.exclude_error.as_ref().map(ExcludeError::text);
```

- 语言行高度加入错误行：

```rust
GroupRow {
    kind: RowKind::Language,
    spec: RowSpec::new(ROW_H).with_extra(extra_for(language_error.is_some(), extra_h)),
    error: language_error.as_deref(),
    note: None,
    hint: None,
},
```

- 语言控件实现（替换原禁用版）：

```rust
fn language_label(setting: LanguageSetting) -> &'static str {
    match setting {
        LanguageSetting::System => i18n::s().language_system,
        LanguageSetting::ZhHans => i18n::s().language_zh_hans,
        LanguageSetting::En => i18n::s().language_en,
    }
}

fn draw_language_field(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    current: LanguageSetting,
    palette: &SettingsPalette,
    actions: &mut Vec<SettingsAction>,
) {
    let response = ui.interact(
        rect,
        ui.scope_id().with("settings-language"),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::CornerRadius::same(FIELD_RADIUS),
        palette.control_fill,
    );
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left() + FIELD_PAD_X, rect.top()),
        egui::pos2(rect.right() - FIELD_PAD_X - FIELD_CHEVRON, rect.bottom()),
    );
    draw_text(
        ui,
        text_rect,
        language_label(current),
        FIELD_FONT,
        palette.control_text,
        egui::Align2::LEFT_CENTER,
    );
    let chevron = egui::Rect::from_min_size(
        egui::pos2(
            rect.right() - FIELD_PAD_X - FIELD_CHEVRON,
            rect.center().y - FIELD_CHEVRON / 2.0,
        ),
        egui::Vec2::splat(FIELD_CHEVRON),
    );
    draw_chevron_down(painter, chevron, palette.control_text);
    let _ = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    egui::Popup::menu(&response).show(|ui| {
        ui.set_min_width(rect.width());
        for setting in [
            LanguageSetting::System,
            LanguageSetting::ZhHans,
            LanguageSetting::En,
        ] {
            if ui
                .selectable_label(current == setting, language_label(setting))
                .clicked()
            {
                actions.push(SettingsAction::SetLanguage(setting));
                ui.close();
            }
        }
    });
}
```

- 调用处改为 `draw_language_field(ui, control, data.language, palette, actions);`。
- 录制提示/键帽 hover/添加/恢复默认/空态/加载文案全部换目录条目。
- `settings.rs` 的 `RESET_CONFIRM_TITLE/MESSAGE/ACTION` 常量删除，`apply_action` 内改为：

```rust
if platform::confirm_action(
    i18n::s().reset_confirm_title,
    i18n::s().reset_confirm_message,
    i18n::s().reset_confirm_action,
) { /* 原逻辑 */ }
```

- `apply_font_size` 的保存错误改为：

```rust
let save_error = store::save(&data_dir, &file).err().map(|err| {
    let detail = format!("{err:#}");
    log::warn!("字号 {size:?} 已应用,但设置保存失败:{detail}");
    SettingsError::SaveFailed
});
```

- `apply_autostart` 的 `map_err` 仍返回 String 详情；`autostart_toggled` 内部转 typed。
- 删除 `crates/app/src/settings.rs` 中 `FontSize::label()` 与 `crates/app/src/ui/settings_view.rs:640` 的 `option.label()` 调用（改走 `font_label`）；`settings.rs` 的 `font_size_labels_match_spec` 测试改为断言 `scale()`，或删除该测试（文案断言已移到 headless 双语测试）。

- [ ] **Step 9: 跑测试并修断言**

Run: `cargo test -p macEverything-app settings`
Expected: 先失败（旧断言引用常量/字符串），逐项改为目录文案或 typed error 后通过。

- [ ] **Step 10: 全量验证并提交**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```bash
git add crates/app/src/i18n/catalog.rs crates/app/src/ui/settings.rs crates/app/src/ui/settings_view.rs crates/app/src/settings.rs
git commit -m "feat(app): 设置页语言下拉与即时切换"
```

---

## Task 3: 热键提示类型化 + 悬浮条文案

**Files:**
- Modify: `crates/app/src/hotkey.rs`
- Modify: `crates/app/src/ui/palette.rs`
- Modify: `crates/app/src/ui/settings_view.rs`（热键 note 渲染）
- Modify: `crates/app/src/ui/mod.rs`（构造快照处）
- Modify: `crates/app/src/i18n/catalog.rs`

- [ ] **Step 1: 追加文案**

`catalog!` 追加：

```rust
    // 悬浮条
    // 注：悬浮条空词按视觉规格只有光标（visual.md:276），没有可见 hint；
    // 此条供完整窗口搜索框（Task 4）使用；悬浮条的字体预热诊断也复用它。
    search_placeholder: "搜索文件…" / "Search files…",
    open_in_full_window: "在完整窗口中打开（⌥↵）" / "Open in Full Window (⌥↵)",
    empty_start: "输入关键词开始搜索" / "Type to search",
    empty_start_sub: "可搜文件名，也可搜路径" / "Search file names or paths",
    empty_no_match: "无匹配项" / "No matches",
    empty_no_match_sub: "换个关键词试试" / "Try a different keyword",
    // 角标
    badge_building: "建索引中 {n}" / "Building index {n}",
    badge_reconciling: "补齐中" / "Reconciling",
    badge_rescanning: "重扫中" / "Rescanning",
    badge_offline_index: "离线索引" / "Offline index",
    badge_engine_not_running: "引擎未运行" / "Engine not running",
    badge_building_index: "索引构建中" / "Building index",
    badge_query_failed: "查询失败" / "Query failed",
    // 热键提示（持久）
    hotkey_unavailable: "快捷键不可用,可从托盘唤起" / "Shortcut unavailable; use the menu bar instead",
    hotkey_unavailable_reason: "可从托盘唤起" / "use the menu bar instead",
    window_hotkey_unavailable_reason: "可从托盘打开" / "open it from the menu bar",
    hotkey_resume_failed: "快捷键恢复失败,请重新录制" / "Failed to restore shortcut; please record it again",
    hotkey_fallback: "{default} 不可用,已改用 {name}" / "{default} unavailable; using {name} instead",
    hotkey_saved_palette_default: "保存的唤起快捷键 {custom} 不可用,已回落默认 {name}" / "Saved Quick Search shortcut {custom} is unavailable; reverted to default {name}",
    hotkey_saved_palette_other: "保存的唤起快捷键 {custom} 不可用,已改用 {name}" / "Saved Quick Search shortcut {custom} is unavailable; using {name} instead",
    hotkey_saved_palette_unavailable: "保存的唤起快捷键 {custom} 不可用,{reason}" / "Saved Quick Search shortcut {custom} is unavailable; {reason}",
    hotkey_saved_window_conflict_default: "保存的完整窗口快捷键 {custom} 与唤起键冲突,已回落默认 {name}" / "Full Window shortcut {custom} conflicts with Quick Search; reverted to default {name}",
    hotkey_saved_window_default: "保存的完整窗口快捷键 {custom} 不可用,已回落默认 {name}" / "Full Window shortcut {custom} is unavailable; reverted to default {name}",
    hotkey_saved_window_conflict_unavailable: "保存的完整窗口快捷键 {custom} 与唤起键冲突,{reason}" / "Full Window shortcut {custom} conflicts with Quick Search; {reason}",
    hotkey_saved_window_unavailable: "保存的完整窗口快捷键 {custom} 不可用,{reason}" / "Full Window shortcut {custom} is unavailable; {reason}",
```

- [ ] **Step 2: 写失败测试（HotkeyNote 文案）**

`crates/app/src/hotkey.rs` 测试模块追加：

```rust
#[test]
fn hotkey_note_text_is_localized() {
    let note = HotkeyNote::Fallback {
        default: "⌥Space".into(),
        used: "⌃⌥K".into(),
    };
    assert_eq!(note.text(), "⌥Space 不可用,已改用 ⌃⌥K");
    i18n::with_lang(i18n::Lang::En, || {
        assert_eq!(note.text(), "⌥Space unavailable; using ⌃⌥K instead");
    });
    assert_eq!(
        HotkeyNote::Unavailable.text(),
        "快捷键不可用,可从托盘唤起"
    );
    i18n::with_lang(i18n::Lang::En, || {
        assert_eq!(
            HotkeyNote::Unavailable.text(),
            "Shortcut unavailable; use the menu bar instead"
        );
    });
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p macEverything-app hotkey_note_text_is_localized`
Expected: FAIL（`HotkeyNote` 不存在）。

- [ ] **Step 4: 实现 `HotkeyNote`**

`crates/app/src/hotkey.rs`：

```rust
/// 持久热键提示（跨帧展示；语言切换时按当前语言重新渲染）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyNote {
    Fallback { default: String, used: String },
    Unavailable,
    ResumeFailed,
    SavedPaletteDefault { custom: String, default: String },
    SavedPaletteOther { custom: String, used: String },
    SavedPaletteUnavailable { custom: String },
    SavedWindowConflictDefault { custom: String, default: String },
    SavedWindowDefault { custom: String, default: String },
    SavedWindowConflictUnavailable { custom: String },
    SavedWindowUnavailable { custom: String },
}

impl HotkeyNote {
    pub fn text(&self) -> String {
        let s = i18n::s();
        match self {
            Self::Fallback { default, used } => i18n::fill(
                s.hotkey_fallback,
                &[("default", default.as_str()), ("name", used.as_str())],
            ),
            Self::Unavailable => s.hotkey_unavailable.to_owned(),
            Self::ResumeFailed => s.hotkey_resume_failed.to_owned(),
            Self::SavedPaletteDefault { custom, default } => i18n::fill(
                s.hotkey_saved_palette_default,
                &[("custom", custom.as_str()), ("name", default.as_str())],
            ),
            Self::SavedPaletteOther { custom, used } => i18n::fill(
                s.hotkey_saved_palette_other,
                &[("custom", custom.as_str()), ("name", used.as_str())],
            ),
            Self::SavedPaletteUnavailable { custom } => i18n::fill(
                s.hotkey_saved_palette_unavailable,
                &[("custom", custom.as_str()), ("reason", s.hotkey_unavailable_reason)],
            ),
            Self::SavedWindowConflictDefault { custom, default } => i18n::fill(
                s.hotkey_saved_window_conflict_default,
                &[("custom", custom.as_str()), ("name", default.as_str())],
            ),
            Self::SavedWindowDefault { custom, default } => i18n::fill(
                s.hotkey_saved_window_default,
                &[("custom", custom.as_str()), ("name", default.as_str())],
            ),
            Self::SavedWindowConflictUnavailable { custom } => i18n::fill(
                s.hotkey_saved_window_conflict_unavailable,
                &[("custom", custom.as_str()), ("reason", s.window_hotkey_unavailable_reason)],
            ),
            Self::SavedWindowUnavailable { custom } => i18n::fill(
                s.hotkey_saved_window_unavailable,
                &[("custom", custom.as_str()), ("reason", s.window_hotkey_unavailable_reason)],
            ),
        }
    }
}
```

在 `i18n/mod.rs` 追加：

```rust
/// 模板填充：`{name}` 形式占位符；缺参数时保留原样（由目录单测保证成对）。
pub fn fill(template: &str, args: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) => {
                let key = &after[..end];
                match args.iter().find(|(k, _)| *k == key) {
                    Some((_, value)) => out.push_str(value),
                    None => {
                        out.push('{');
                        out.push_str(key);
                        out.push('}');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}
```

- [ ] **Step 5: 改快照与提示生成**

- `HotkeySnapshot.note/window_note` 类型改为 `Option<HotkeyNote>`。
- `PaletteState::new`/`with_icons`/`new_for_test` 的 `hotkey_note` 参数类型改为 `Option<HotkeyNote>`，字段同步。
- `from_active(active, note: Option<HotkeyNote>, window_note: Option<HotkeyNote>)`。
- `note_for` 返回 `Option<HotkeyNote>`：

```rust
fn note_for(registered: Option<&str>) -> Option<HotkeyNote> {
    let default_name = candidates()[0].2;
    match registered {
        Some(name) if name == default_name => None,
        Some(name) => Some(HotkeyNote::Fallback {
            default: default_name.to_owned(),
            used: name.to_owned(),
        }),
        None => Some(HotkeyNote::Unavailable),
    }
}
```

- `palette_fallback_note` 返回 `HotkeyNote`：

```rust
fn palette_fallback_note(custom: HotKey, fallback_name: Option<&str>) -> HotkeyNote {
    let custom = display_name(custom);
    match fallback_name {
        Some(name) if name == candidates()[0].2 => HotkeyNote::SavedPaletteDefault {
            custom,
            default: name.to_owned(),
        },
        Some(name) => HotkeyNote::SavedPaletteOther {
            custom,
            used: name.to_owned(),
        },
        None => HotkeyNote::SavedPaletteUnavailable { custom },
    }
}
```

- `register_window_default` 的两处 `note` 改为对应 `HotkeyNote` 变体。
- `WINDOW_UNAVAILABLE_NOTE`/`UNAVAILABLE_NOTE`/`RESUME_FAILED_NOTE` 常量删除；所有引用改为 `HotkeyNote` 变体或 `.text()`（仅日志可 `.text()`）。注意：完整窗口默认键失败且无自定义键时，原行为是只留日志、不常驻提示，因此 `HotkeyNote` 不设独立的 `WindowUnavailable` 变体。
- `HotkeyRuntime::note()` 签名改为 `pub fn note(&self) -> Option<&HotkeyNote> { self.snapshot.note.as_ref() }`。
- `ui/mod.rs` 中 `let hotkey_note = hotkey.note().map(str::to_string);` 改为 `let hotkey_note = hotkey.note().cloned();`。

- [ ] **Step 6: 悬浮条状态角标类型化**

`crates/app/src/ui/palette.rs`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum BadgeText {
    Building(u64),
    Reconciling,
    Rescanning,
    OfflineIndex,
    EngineNotRunning,
    Warming,
    QueryFailed,
    Raw(String),
}

impl BadgeText {
    fn text(&self) -> String {
        let s = i18n::s();
        match self {
            Self::Building(n) => {
                let n = n.to_string();
                i18n::fill(s.badge_building, &[("n", n.as_str())])
            }
            Self::Reconciling => s.badge_reconciling.to_owned(),
            Self::Rescanning => s.badge_rescanning.to_owned(),
            Self::OfflineIndex => s.badge_offline_index.to_owned(),
            Self::EngineNotRunning => s.badge_engine_not_running.to_owned(),
            Self::Warming => s.badge_building_index.to_owned(),
            Self::QueryFailed => s.badge_query_failed.to_owned(),
            Self::Raw(text) => text.clone(),
        }
    }
}
```

- `struct Badge { text: BadgeText, tone: BadgeTone }`。
- `Badge::neutral/warning/error` 构造参数改为 `BadgeText`。
- `Badge::error(msg)` 的调用点改为 `Badge::error(BadgeText::Raw(msg))`。
- `badge_note` 内所有 `format!`/字面量改为 `BadgeText` 变体。
- 绘制处 `badge_text_fit(ui, &badge.text, ...)` 改为 `let badge_text = badge.text.text(); badge_text_fit(ui, &badge_text, ...)`。
- `hotkey_note: Option<HotkeyNote>`；绘制处 `hotkey_note.as_ref().map(HotkeyNote::text)`。

- [ ] **Step 7: 悬浮条其余文案替换**

| 原字面量 | 替换为 |
|---|---|
| `"在完整窗口中打开（⌥↵）"` | `i18n::s().open_in_full_window` |
| `("无匹配项", "换个关键词试试")` | `(i18n::s().empty_no_match, i18n::s().empty_no_match_sub)` |
| `("输入关键词开始搜索", "可搜文件名，也可搜路径")` | `(i18n::s().empty_start, i18n::s().empty_start_sub)` |

> 悬浮条空词按视觉规格**只有光标、没有可见 hint**（`docs/specs/floatbar-visual/visual.md:276`），保持 `hint_text("")` 不变；`search_placeholder` 的可见使用点是完整窗口搜索框（Task 4），Task 3 中仅字体预热诊断引用它。

- [ ] **Step 8: 设置页热键 note 渲染**

`settings_view.rs` 中：

```rust
let palette_note = data.hotkeys.note.as_ref().map(HotkeyNote::text);
let window_note = data.hotkeys.window_note.as_ref().map(HotkeyNote::text);
```

`GroupRow.note` 用 `palette_note.as_deref()` / `window_note.as_deref()`。

- [ ] **Step 9: 跑测试并修断言**

Run: `cargo test -p macEverything-app hotkey`
Run: `cargo test -p macEverything-app palette`
Expected: 旧断言引用 `String`/常量处逐项改为 `HotkeyNote` 变体或 `.text()` 后通过。

英文宽度回归测试必须断言 `!galley.elided`（仅比较 `size().x <= 540` 无法发现截断：`Galley::size()` 会被截断宽度钳住，`text()` 仍返回完整原文），并覆盖 `SavedWindowConflictDefault` 与 `SavedWindowConflictUnavailable` 两个最长变体、使用最长键名 `⌃⌥⇧Space`。

- [ ] **Step 10: 全量验证并提交**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```bash
git add crates/app/src/hotkey.rs crates/app/src/ui/palette.rs crates/app/src/ui/settings_view.rs crates/app/src/ui/mod.rs crates/app/src/i18n
git commit -m "feat(app): 热键提示类型化与悬浮条双语"
```

---

## Task 4: 完整结果窗口文案 + 状态提示类型化

**Files:**
- Modify: `crates/app/src/i18n/catalog.rs`
- Create: `crates/app/src/i18n/format.rs`
- Modify: `crates/app/src/ui/results.rs`

- [ ] **Step 1: 追加文案**

`catalog!` 追加：

```rust
    // 列头 / 搜索
    col_name: "名称" / "Name",
    col_size: "大小" / "Size",
    col_mtime: "修改时间" / "Date Modified",
    col_path: "路径" / "Path",
    settings_tooltip: "设置" / "Settings",
    // 状态栏
    status_sorting: "排序中…" / "Sorting…",
    engine_outdated_sort: "引擎版本过旧,排序未生效" / "Engine is outdated; sorting is unavailable",
    restarting: "重启中…" / "Restarting…",
    restart_engine: "重启引擎" / "Restart Engine",
    status_engine_not_ready: "引擎未就绪" / "Engine not ready",
    status_query_failed: "查询失败" / "Query failed",
    // 缺口面板
    gaps_more: "… 其余 {n} 个" / "… {n} more",
    // 索引状态
    index_ready: "就绪" / "Ready",
    index_reconciling: "索引补齐中(结果可能不全)" / "Reconciling index (results may be incomplete)",
    index_degraded: "索引重扫中(结果可能不全)" / "Rescanning index (results may be incomplete)",
    index_offline_since: "离线索引 · 生成于 {time}" / "Offline index · Generated {time}",
    index_building: "索引构建中" / "Building index",
    engine_starting: "引擎启动中" / "Engine starting",
    offline_index: "离线索引" / "Offline index",
    engine_not_running: "引擎未运行" / "Engine not running",
    // 空态
    empty_engine_starting: "启动引擎中" / "Starting engine",
    empty_wait: "请稍候…" / "Please wait…",
    empty_warming: "索引构建中" / "Building index",
    empty_warming_sub: "部分结果可能暂不可见" / "Some results may be temporarily unavailable",
    empty_search_failed: "搜索失败" / "Search failed",
    empty_search_failed_sub: "请重试或检查引擎状态" / "Try again or check the engine status",
    empty_searching: "搜索中…" / "Searching…",
```

- [ ] **Step 2: 实现 `i18n/format.rs`**

```rust
//! 带参数文案（按当前语言渲染；中英共用同一组参数）。

use super::{lang, Lang};

/// "{n} 项" / "{n} item(s)"。
pub fn items(n: u32) -> String {
    match lang() {
        Lang::ZhHans => format!("{n} 项"),
        Lang::En if n == 1 => "1 item".to_owned(),
        Lang::En => format!("{n} items"),
    }
}

/// "N 个目录未收录" / "N folder(s) not indexed"。
pub fn gaps(n: u32) -> String {
    match lang() {
        Lang::ZhHans => format!("{n} 个目录未收录"),
        Lang::En if n == 1 => "1 folder not indexed".to_owned(),
        Lang::En => format!("{n} folders not indexed"),
    }
}

/// "展示前 N 条" / "Showing first N"。
pub fn showing_first(n: usize) -> String {
    match lang() {
        Lang::ZhHans => format!("展示前 {n} 条"),
        Lang::En => format!("Showing first {n}"),
    }
}

/// 状态栏计数段：`N 项 · 匹配 X ms · 读取 Y ms[ · 展示前 K 条]`。
pub fn counts(total: u32, shown: usize, match_ms: &str, read_ms: &str) -> String {
    let mut out = match lang() {
        Lang::ZhHans => format!("{} · 匹配 {match_ms} ms · 读取 {read_ms} ms", items(total)),
        Lang::En => format!("{} · Match {match_ms} ms · Read {read_ms} ms", items(total)),
    };
    if (shown as u32) < total {
        out.push_str(" · ");
        out.push_str(&showing_first(shown));
    }
    out
}

/// 缺口面板标题。
pub fn gaps_header(n: usize) -> String {
    match lang() {
        Lang::ZhHans => format!("权限缺口({n} 个目录,未收录):"),
        Lang::En if n == 1 => "Permission Gaps (1 folder not indexed):".to_owned(),
        Lang::En => format!("Permission Gaps ({n} folders not indexed):"),
    }
}

/// 索引构建条目数（英文单复数）。
pub fn index_building_count(n: u64) -> String {
    match lang() {
        Lang::ZhHans => format!("索引构建中({n} 条)"),
        Lang::En if n == 1 => "Building index (1 entry)".to_owned(),
        Lang::En => format!("Building index ({n} entries)"),
    }
}
```

> `format.rs` 中用到 `fill`/`s` 的条目在本任务 Step 1 已加入；若先编译失败属预期（TDD）。

- [ ] **Step 3: 写失败测试**

`results.rs` 测试模块追加：

```rust
#[test]
fn empty_state_text_is_localized() {
    assert_eq!(empty_state_text("", &State::Idle), ("输入关键词开始搜索", "可搜文件名，也可搜路径"));
    i18n::with_lang(i18n::Lang::En, || {
        assert_eq!(empty_state_text("", &State::Idle), ("Type to search", "Search file names or paths"));
        assert_eq!(
            empty_state_text("abc", &State::Empty),
            ("No matches", "Try a different keyword")
        );
    });
}

#[test]
fn counts_text_pluralizes_in_english() {
    assert_eq!(counts_text(1, 1, 2.0, 3.0), "1 项 · 匹配 2 ms · 读取 3 ms");
    i18n::with_lang(i18n::Lang::En, || {
        assert_eq!(counts_text(1, 1, 2.0, 3.0), "1 item · Match 2 ms · Read 3 ms");
        assert_eq!(
            counts_text(2, 1, 2.0, 3.0),
            "2 items · Match 2 ms · Read 3 ms · Showing first 1"
        );
    });
}
```

- [ ] **Step 4: 跑测试确认失败**

Run: `cargo test -p macEverything-app empty_state_text_is_localized`
Run: `cargo test -p macEverything-app counts_text_pluralizes_in_english`
Expected: FAIL（英文文案未接入）。

- [ ] **Step 5: `StatusNote` 类型化**

`results.rs`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusNote {
    EngineNotReady,
    QueryFailed,
    Raw(String),
}

impl StatusNote {
    fn text(&self) -> String {
        match self {
            Self::EngineNotReady => i18n::s().status_engine_not_ready.to_owned(),
            Self::QueryFailed => i18n::s().status_query_failed.to_owned(),
            Self::Raw(text) => text.clone(),
        }
    }
}
```

- `status_note: Option<StatusNote>`；绘制处 `self.status_note.as_ref().map(StatusNote::text)`。
- 赋值点：
  - `"引擎未就绪"` → `StatusNote::EngineNotReady`
  - `"查询失败"` → `StatusNote::QueryFailed`
  - `msg`/`err.to_string()`（重启失败、右键动作失败）→ `StatusNote::Raw(...)`
- `clear_transient_messages` 已清 `status_note`。

- [ ] **Step 6: 替换结果窗口文案**

| 原位置 | 原字面量 | 替换 |
|---|---|---|
| 列头 | `"名称"/"大小"/"修改时间"/"路径"` | `i18n::s().col_name` 等四项 |
| 搜索 hint | `"搜索文件…"` | `i18n::s().search_placeholder` |
| 状态栏 | `"0 项"` | `i18n::format::items(0)` |
| 状态栏 | `"排序中…"` | `i18n::s().status_sorting` |
| 状态栏 | `"{gaps} 个目录未收录"` | `i18n::format::gaps(gaps)` |
| 状态栏 | `"引擎版本过旧,排序未生效"` | `i18n::s().engine_outdated_sort` |
| 状态栏 | `"重启中…"/"重启引擎"` | `i18n::s().restarting` / `i18n::s().restart_engine` |
| 缺口面板 | `format!("权限缺口({} 个目录,未收录):", gaps.len())` | `i18n::format::gaps_header(gaps.len())` |
| 缺口面板 | `format!("… 其余 {rest} 个")` | `let rest = rest.to_string(); i18n::fill(i18n::s().gaps_more, &[("n", rest.as_str())])` |
| 索引状态 | 构建中/就绪/补齐/重扫/离线/构建中/启动中/离线/未运行 | 对应 `index_*` / `engine_*` 条目（构建计数用 `index_building_count`，离线时刻用 `fill(index_offline_since, ...)`） |
| 空态 | 全部六组主/副文案 | `i18n::s().empty_*` |
| 齿轮 tooltip | `"设置"` | `i18n::s().settings_tooltip` |

`counts_text` 改为：

```rust
fn counts_text(total: u32, shown: usize, elapsed_ms: f64, stat_ms: f64) -> String {
    i18n::format::counts(
        total,
        shown,
        &format_metric(elapsed_ms),
        &format_metric(stat_ms),
    )
}
```

`status_text` 中 `"0 项"` → `i18n::format::items(0)`，`"排序中…"` → `i18n::s().status_sorting`。

`empty_state_text` 返回目录条目：

```rust
fn empty_state_text(input: &str, state: &State) -> (&'static str, &'static str) {
    let s = i18n::s();
    if input.is_empty() {
        return (s.empty_start, s.empty_start_sub);
    }
    match state {
        State::EngineStarting => (s.empty_engine_starting, s.empty_wait),
        State::Warming => (s.empty_warming, s.empty_warming_sub),
        State::Error(_) => (s.empty_search_failed, s.empty_search_failed_sub),
        State::Searching => (s.empty_searching, s.empty_wait),
        State::Empty | State::Offline | State::Idle => (s.empty_no_match, s.empty_no_match_sub),
    }
}
```

- [ ] **Step 7: 跑测试并修断言**

Run: `cargo test -p macEverything-app results`
Expected: 旧断言改为目录文案/`StatusNote` 后通过。

- [ ] **Step 8: 全量验证并提交**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```bash
git add crates/app/src/i18n crates/app/src/ui/results.rs
git commit -m "feat(app): 完整窗口双语与状态提示类型化"
```

---

## Task 5: 托盘与原生层

**Files:**
- Modify: `crates/app/src/i18n/catalog.rs`
- Modify: `crates/app/src/tray.rs`
- Modify: `crates/app/src/ui/menu_actions.rs`
- Modify: `crates/app/src/ui/mod.rs`
- Modify: `crates/app/src/platform/mod.rs`
- Modify: `crates/app/src/platform/mac.rs`

- [ ] **Step 1: 追加文案**

```rust
    // 托盘
    tray_open_search: "打开搜索" / "Open Quick Search",
    tray_open_window: "打开完整窗口" / "Open Full Window",
    tray_autostart: "开机自启" / "Launch at Login",
    tray_settings: "设置…" / "Settings…",
    tray_quit: "退出" / "Quit",
    tray_state_building: "引擎:构建中" / "Engine: Building",
    tray_state_ready: "引擎:就绪" / "Engine: Ready",
    tray_state_reconciling: "引擎:补齐中" / "Engine: Reconciling",
    tray_state_degraded: "引擎:重扫中" / "Engine: Rescanning",
    tray_state_offline: "引擎:未运行(离线索引)" / "Engine: Not Running (Offline Index)",
    tray_state_not_running: "引擎:未运行" / "Engine: Not Running",
    // 右键菜单（v2.2/v2.3 最终形态）
    menu_open: "打开" / "Open",
    menu_show_in_finder: "在访达中显示" / "Show in Finder",
    menu_quick_look: "快速查看" / "Quick Look",
    menu_copy: "拷贝" / "Copy",
    menu_copy_file: "拷贝文件" / "Copy File",
    menu_copy_path: "拷贝路径" / "Copy Path",
    menu_copy_name: "拷贝文件名" / "Copy File Name",
    menu_file_missing: "文件不存在,索引可能待更新" / "File not found; the index may be out of date",
    menu_open_failed: "打开失败" / "Failed to open",
    menu_copy_failed: "拷贝路径失败" / "Failed to copy path",
    quick_look_failed: "无法预览该项目" / "Could not preview this item",
    copy_file_failed: "拷贝文件失败" / "Failed to copy file",
    copy_name_failed: "拷贝文件名失败" / "Failed to copy file name",
```

- [ ] **Step 2: 托盘文案与 `relabel`**

`tray.rs`：

- `state_label` 匹配分支改为目录条目：

```rust
fn state_label(state: &str) -> &'static str {
    let s = i18n::s();
    match state {
        client::state::BUILDING => s.tray_state_building,
        client::state::READY => s.tray_state_ready,
        client::state::RECONCILING => s.tray_state_reconciling,
        client::state::DEGRADED => s.tray_state_degraded,
        client::state::OFFLINE => s.tray_state_offline,
        _ => s.tray_state_not_running,
    }
}
```

- `gaps_label` 复用完整窗口的 plural 实现并加托盘前缀：

```rust
fn gaps_label(gaps: u32) -> String {
    format!("⚠ {}", crate::i18n::format::gaps(gaps))
}
```
- 安装时静态项文案改用目录条目。
- `TrayRuntime` 增加字段：

```rust
    open_search: MenuItem,
    open_window: MenuItem,
    settings: MenuItem,
    quit: MenuItem,
```

- `install` 中保存这些句柄到 runtime。
- 新增：

```rust
    /// 语言切换后原地刷新菜单文案（原生菜单不随 egui 重绘；主线程调用）。
    pub fn relabel(&mut self) {
        let s = i18n::s();
        self.open_search.set_text(s.tray_open_search);
        self.open_window.set_text(s.tray_open_window);
        self.autostart.set_text(s.tray_autostart);
        self.settings.set_text(s.tray_settings);
        self.quit.set_text(s.tray_quit);
        self.state_item.set_text(state_label(&self.applied.state));
        if let Some(item) = &self.gaps_item {
            item.set_text(gaps_label(self.applied.gaps));
        }
        log::info!("托盘菜单文案已按当前语言刷新");
    }
```

- [ ] **Step 3: `App::logic` 消费刷新标记**

在 `logic` 中 `if let Some(tray) = &mut self.tray { tray.sync(); }` 之前：

```rust
let relabel_tray = self.shared().settings.take_tray_relabel_pending();
if let Some(tray) = &mut self.tray {
    if relabel_tray {
        tray.relabel();
    }
    tray.sync();
}
```

同时删除 `settings.rs` 中 `take_tray_relabel_pending` 上的 `#[allow(dead_code)]`（本任务起有生产消费者）。

- [ ] **Step 4: 右键菜单文案**

> 历史说明：原计划为 `menu_items(files, dirs)` 按文件/目录计数收敛；v2.2/v2.3 已改为固定菜单，`menu_entries()` 无参，文件与目录同形（打开 / 在访达中显示 / 快速查看 / 分隔线 / 拷贝 ▸ 拷贝文件/拷贝路径/拷贝文件名）。「在访达中打开」及其执行分支已删除。

`menu_actions.rs` 的 `menu_entries()` 返回嵌套 `MenuEntry`：

```rust
pub fn menu_entries() -> Vec<MenuEntry> {
    let s = i18n::s();
    vec![
        MenuEntry::Item { title: s.menu_open, tag: MenuAction::Open.tag(), enabled: true },
        MenuEntry::Item { title: s.menu_show_in_finder, tag: MenuAction::RevealInFinder.tag(), enabled: true },
        MenuEntry::Item { title: s.menu_quick_look, tag: MenuAction::QuickLook.tag(), enabled: true },
        MenuEntry::Separator,
        MenuEntry::Submenu {
            title: s.menu_copy,
            items: vec![
                MenuEntry::Item { title: s.menu_copy_file, tag: MenuAction::CopyFile.tag(), enabled: true },
                MenuEntry::Item { title: s.menu_copy_path, tag: MenuAction::CopyPaths.tag(), enabled: true },
                MenuEntry::Item { title: s.menu_copy_name, tag: MenuAction::CopyFileName.tag(), enabled: true },
            ],
        },
    ]
}
```

错误文案（整批存在性校验前置；快速查看/拷贝文件由平台层执行）：

> 历史片段；最终实现见 `menu_actions.rs` 的 `ensure_exists` 与 `copy_to_clipboard(text, failure)`。

```rust
return Err(anyhow!("{}", i18n::s().menu_file_missing));
Err(e) => Err(anyhow!("{}: {e}", i18n::s().menu_open_failed)),
// 文本拷贝：failure = i18n::s().menu_copy_failed | i18n::s().copy_name_failed
.map_err(|e| anyhow!("{failure}: {e}"))?;
Ok(st) => Err(anyhow!("{failure} (pbcopy exit {st})")),
Err(e) => Err(anyhow!("{failure}: {e}")),
```

- [ ] **Step 5: 设置窗标题双语匹配**

`platform/mod.rs` 删除 `SETTINGS_TITLE` 常量，改为：

> 删除常量前先确认全仓引用：`grep -rn "SETTINGS_TITLE" crates/app/src`。`settings.rs` 的单测（builder title / 首次居中）与 `platform/mac.rs` 的查找/测试全部改为 `i18n::s().settings_title` 或 `settings_window_titles()`。

```rust
/// 设置窗可接受的两种原生标题（查找时同时匹配，避免切换瞬间竞态）。
pub fn settings_window_titles() -> [&'static str; 2] {
    [
        crate::i18n::strings(crate::i18n::Lang::ZhHans).settings_title,
        crate::i18n::strings(crate::i18n::Lang::En).settings_title,
    ]
}
```

`platform/mac.rs`：

- import 改为 `settings_window_titles`。
- `find_window_by_title(mtm, root, title)` 签名改为 `titles: &[&str]`，内部 `titles.iter().any(|t| window_title_matches(&actual, t))`。
- 所有调用传 `&settings_window_titles()` 或 `&[PALETTE_TITLE]`。
- 单测改为：

```rust
#[test]
fn window_title_lookup_accepts_both_settings_titles() {
    let titles = settings_window_titles();
    assert!(window_title_matches("设置", titles[0]));
    assert!(window_title_matches("Settings", titles[1]));
    assert!(!window_title_matches("设置窗口", titles[0]));
    assert!(!window_title_matches("", titles[0]));
}
```

- [ ] **Step 6: 原生确认弹窗取消按钮**

`platform/mac.rs` 的 `confirm_action` 内：

```rust
let cancel = alert.addButtonWithTitle(&NSString::from_str(crate::i18n::s().cancel));
```

> 调用方仍传标题/正文/动作按钮；取消按钮统一取当前语言。

- [ ] **Step 7: 跑测试并修断言**

Run: `cargo test -p macEverything-app tray`
Run: `cargo test -p macEverything-app platform`
Run: `cargo test -p macEverything-app menu_actions`
Expected: 旧断言改为目录文案后通过；托盘 relabel 由真实运行验收（原生菜单无法 headless）。

补充双语断言：
- `state_label` 六态与 `gaps_label(1/2)` 在 zh/en 下精确匹配（含托盘 `⚠ ` 前缀与英文单复数）。
- `menu_entries` 在 `with_lang(En)` 下返回 `["Open","Show in Finder","Quick Look"]` + `Copy` 子菜单 `["Copy File","Copy Path","Copy File Name"]`，中文默认断言保持。
- `window_title_lookup_accepts_both_settings_titles` 覆盖中/英标题。
- `reset_native_confirmation_copy_matches_contract` 增加 `cancel` 的中英断言。

- [ ] **Step 8: 全量验证并提交**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```bash
git add crates/app/src/i18n/catalog.rs crates/app/src/tray.rs crates/app/src/ui/menu_actions.rs crates/app/src/ui/mod.rs crates/app/src/platform/mod.rs crates/app/src/platform/mac.rs
git commit -m "feat(app): 托盘与原生层双语"
```

---

## Task 6: 剩余应用自产消息

**Files:**
- Modify: `crates/app/src/engine_launcher.rs`
- Modify: `crates/app/src/hotkey.rs`
- Modify: `crates/app/src/ui/hotkey_keys.rs`
- Modify: `crates/app/src/ui/palette.rs`（悬浮条打开失败文案）
- Modify: `crates/app/src/ui/results.rs`（打开失败 `OpenError::text()` 与重启前缀）
- Modify: `crates/app/src/i18n/catalog.rs`
- Modify: `crates/app/src/i18n/format.rs`

- [ ] **Step 1: 追加文案**

```rust
    // 引擎拉起（用户可见）
    engine_not_found: "未找到引擎可执行文件(macEverything),已搜索以下路径:" / "Could not find the macEverything engine. Searched:",
    engine_not_found_hint: "请将 macEverything 放到 App 同目录,或加入 PATH 后重试。" / "Place macEverything next to the app or add it to PATH, then try again.",
    engine_locate_app_failed: "定位 App 可执行文件失败" / "Could not locate the app executable",
    engine_start_failed: "启动引擎失败" / "Failed to start the engine",
    engine_start_timeout: "引擎进程已拉起,但未能连接引擎 socket" / "The engine process started but did not accept connections",
    engine_start_timeout_hint: "请查看 {log} 了解启动失败原因" / "See {log} for details",
    engine_restart_failed: "引擎重启失败" / "Failed to restart the engine",
    // 热键校验/运行时错误（一次性）
    hotkey_needs_modifier: "需包含修饰键" / "A modifier key is required",
    hotkey_unsupported: "不支持该键" / "Unsupported key",
    hotkey_register_failed: "注册失败(可能被系统或其他应用占用)" / "Registration failed (it may be in use by the system or another app)",
    hotkey_reset_failed: "恢复默认快捷键失败(可能被系统或其他应用占用)" / "Failed to restore the default shortcut (it may be in use by the system or another app)",
    hotkey_duplicate: "该快捷键已用于另一个功能" / "This shortcut is already used for another feature",
    hotkey_saved_but_save_failed: "快捷键已生效,但设置保存失败" / "Shortcut applied, but settings could not be saved",
    hotkey_reset_but_save_failed: "快捷键已恢复默认,但设置保存失败" / "Default shortcut restored, but settings could not be saved",
    hotkey_global_unavailable: "全局热键不可用" / "Global shortcuts unavailable",
    hotkey_restore_other_failed: "恢复另一快捷键 {name} 失败(可能被占用)" / "Failed to restore the other shortcut {name} (it may be in use)",
    hotkey_all_candidates_unavailable: "候选快捷键均不可用" / "No shortcut candidates are available",
    // 悬浮条结果行 / 页脚
    palette_action_enter: "进入" / "Enter",
    palette_action_open: "打开" / "Open",
    palette_footer_hide: "隐藏" / "Hide",
    palette_footer_full_window: "完整窗口" / "Full Window",
    palette_footer_finder: "访达" / "Finder",
    // 悬浮条打开失败（应用自产）
    palette_file_missing: "文件不存在,索引可能待更新" / "File not found; the index may be out of date",
    palette_open_failed: "打开失败" / "Failed to open",
```

- [ ] **Step 2: 热键校验错误类型化**

`ui/hotkey_keys.rs`：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyError {
    NeedsModifier,
    Unsupported,
}

impl HotkeyError {
    pub fn text(self) -> &'static str {
        match self {
            Self::NeedsModifier => i18n::s().hotkey_needs_modifier,
            Self::Unsupported => i18n::s().hotkey_unsupported,
        }
    }
}

impl std::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

impl std::error::Error for HotkeyError {}

const NO_MODIFIER: HotkeyError = HotkeyError::NeedsModifier;
const UNSUPPORTED_KEY: HotkeyError = HotkeyError::Unsupported;

pub fn validate(hotkey: HotKey) -> Result<(), HotkeyError> { /* 原逻辑，返回变体 */ }
```

调用方 `hotkey_keys::validate(...).map_err(|err| err.to_string())` 自动得到当前语言文案；测试默认中文保持通过。

- [ ] **Step 3: 热键运行时一次性错误改用目录**

`hotkey.rs`：

- `RESET_FAILED_PREFIX` → `i18n::s().hotkey_reset_failed`（使用处格式化为 `format!("{}: {err}", ...)`）。
- `DUPLICATE_HOTKEY_MESSAGE` → `i18n::s().hotkey_duplicate`（常量删除；`ui/settings_view` 查重处改为调用目录）。
- 注册失败文案（`"注册失败(可能被系统或其他应用占用): {err}"`）→ `format!("{}: {err}", i18n::s().hotkey_register_failed)`。
- manager 缺失文案 `"全局热键不可用"` → `i18n::s().hotkey_global_unavailable`。
- 回滚路径 `"恢复另一快捷键 {} 失败(可能被占用): {err}"` → `i18n::fill(i18n::s().hotkey_restore_other_failed, &[("name", &display_name(other_key))])` + `: {err}`。
- 候选全失败 `reset_failed("候选快捷键均不可用")` → `reset_failed(i18n::s().hotkey_all_candidates_unavailable)`。
- `ui/mod.rs` 的“已生效/已恢复默认但保存失败”改为目录条目。
- `BadgeText::Raw` 无生产消费者时删除该变体（布局测试改用 `BadgeText::Open(OpenError::Failed(long))`），不要保留 `#[allow(dead_code)]`。

- [ ] **Step 4: 引擎拉起错误类型化**

`engine_launcher.rs`：

```rust
#[derive(Debug)]
pub enum LaunchError {
    NotFound { tried: Vec<PathBuf> },
    LocateApp,
    Start,
    Timeout { log: PathBuf },
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { tried } => {
                let paths = tried
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                write!(f, "{}\n{paths}", i18n::s().engine_not_found)?;
                write!(f, "\n{}", i18n::s().engine_not_found_hint)
            }
            Self::LocateApp => f.write_str(i18n::s().engine_locate_app_failed),
            Self::Start => f.write_str(i18n::s().engine_start_failed),
            Self::Timeout { log } => {
                write!(f, "{}: ", i18n::s().engine_start_timeout)?;
                let log = log.display().to_string();
                f.write_str(&i18n::fill(
                    i18n::s().engine_start_timeout_hint,
                    &[("log", log.as_str())],
                ))
            }
        }
    }
}
impl std::error::Error for LaunchError {}
```

构造 `LaunchError` 时把原始细节（错误码、socket、日志路径）直接 `log::warn!`，不要放进变体字段（避免 dead_code 与中英混排）。

`ensure_engine` 返回 `anyhow::Result<()>` 可继续 `anyhow!(...)`/`.context(...)`；用户可见位置只显示 `Display`。原始细节进 `log`。

`results.rs` 重启失败前缀改为：

```rust
RestartOutcome::Err(format!("{}: {err}", i18n::s().engine_restart_failed))
```

并在 Task 6 的 `catalog!` 追加：

```rust
    engine_restart_failed: "引擎重启失败" / "Failed to restart the engine",
```

> 调用方展示 `{err:#}` 的位置改为 `{err}` 以走本地化 `Display`；日志保留 `{err:#}`。

- [ ] **Step 5: 悬浮条打开失败文案**

`crates/app/src/ui/palette.rs`：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
enum OpenError {
    FileMissing,
    Failed(String),
}

impl OpenError {
    fn text(&self) -> String {
        match self {
            Self::FileMissing => i18n::s().palette_file_missing.to_owned(),
            Self::Failed(detail) => {
                format!("{}: {detail}", i18n::s().palette_open_failed)
            }
        }
    }
}
```

- `BadgeText` 增加 `Open(OpenError)`；`BadgeText::text` 映射到 `error.text()`。
- `open_path` 返回 `Result<(), OpenError>`（文件不存在 → `FileMissing`；启动失败 → `Failed(detail)`），
  `open_selected` 改为 `Badge::error(BadgeText::Open(err))`。
- `BadgeText::Raw` 已删除（无生产消费者）；不要恢复该变体或 `#[allow(dead_code)]`。
- `results.rs` 的打开失败路径同样使用 `OpenError::text()`（或给 `OpenError` 实现 `Display`），
  不要继续把中文串塞进 `StatusNote::Raw`。
- 测试：`with_lang(En)` 下 `FileMissing` 文案为 `File not found; the index may be out of date`；
  中文默认下为「文件不存在,索引可能待更新」。
- **悬浮条结果行/页脚剩余文案**：
  - `action_hint(is_dir)` 改为目录 `i18n::s().palette_action_enter`、文件 `i18n::s().palette_action_open`。
  - 页脚四组标签改为 `palette_footer_hide` / `palette_footer_full_window` / `palette_footer_finder` / `palette_action_open`。
  - 页脚计数改为 `i18n::format::total_items(total)`；在 `i18n/format.rs` 新增：
    ```rust
    pub fn total_items(n: u32) -> String {
        match lang() {
            Lang::ZhHans => format!("共 {n} 项"),
            Lang::En if n == 1 => "Total: 1 item".to_owned(),
            Lang::En => format!("Total: {n} items"),
        }
    }
    ```
  - **结果行动作提示动态宽度**：`hint_w = if is_sel { text_w(ui, hint, FONT_META).max(tk::ACTION_HINT_W) } else { 0.0 }`，
    文本列右缘继续用 `hint_w`；不要只依赖固定 40px（Large 字号下 "Enter" 约 45px）。
  - **页脚布局重写为纯选择 + 统一绘制**：
    - 视觉序（左→右）：`[Open, Finder, Full Window, Hide]`；按此序做前缀选择，
      计数区优先保留，`Open` 永远保留。
    - 纯函数 `footer_visible_count(widths: &[f32], available: f32, gap: f32) -> usize`
      返回能放下的前缀组数；`available = panel_w - 2*FOOTER_PAD_X - count_w - FOOTER_GAP`
      （无计数时不含 count/gap）。
    - 绘制从右缘向左按选中组反序放置；计数在左。不要在循环里只对非末组做阈值判断。
    - 测试：用实测 `text_w` 在 `PANEL_MIN_W` 与 `PANEL_W` 下构造组宽，断言选中前缀 ≥1、
      计数矩形右缘与最左组矩形左缘不相交；`PANEL_W` 下四组全选。中文默认与英文都要覆盖。
  - 测试：`action_hint(true/false)`、页脚标签与 `total_items(0/1/2)` 的 zh/en 文案。
  - 英文宽度断言：`text_w(action_hint(true), FONT_META)` 在 1.0 与 1.15 下都不得超过动态预留宽度。
  - `open_path` 文档改为「由 `OpenError::text()` 在绘制时按当前语言渲染」。

- [ ] **Step 6: 跑测试并修断言**

Run: `cargo test -p macEverything-app engine_launcher`
Run: `cargo test -p macEverything-app hotkey_keys`
Run: `cargo test -p macEverything-app hotkey`
Run: `cargo test -p macEverything-app settings`
Run: `cargo test -p macEverything-app palette`
Run: `cargo test -p macEverything-app results`
Expected: 默认中文断言保持通过；英文用例用 `with_lang` 新增断言。

- [ ] **Step 7: 全量验证并提交**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```bash
git add crates/app/src/engine_launcher.rs crates/app/src/hotkey.rs crates/app/src/ui/hotkey_keys.rs crates/app/src/ui/palette.rs crates/app/src/ui/results.rs crates/app/src/ui/mod.rs crates/app/src/i18n
git commit -m "feat(app): 剩余自产消息双语"
```

---

## Task 7: 验收与文档

**Files:**
- Modify: `docs/specs/i18n-v2.1/plan.md`
- Modify: `docs/specs/i18n-v2.1/design.md`
- Modify: `docs/specs/settings-v2.1/design.md`
- Modify: `docs/specs/floatbar-visual/settings-visual.md`
- Modify: `README.md`（可选：界面支持中英双语一句）

- [ ] **Step 1: 英文布局 headless 冒烟**

在 `settings_view.rs` 追加：

```rust
#[test]
fn english_rows_fit_fixed_controls() {
    use crate::i18n;
    i18n::with_lang(i18n::Lang::En, || {
        let data = layout_test_data();
        let out = run_body(&data);
        let texts = all_texts(&out);
        for label in [
            "Launch at Login",
            "Quick Search Shortcut",
            "Full Window Shortcut",
            "Excluded Folders",
        ] {
            let text = find_text(&texts, label);
            // 标签列实际可用宽 372px（620 窗宽 − 2×24 内容内边距 − 2×16 行内边距
            // − 152 控件宽 − 16 标签/控件间距）。
            assert!(text.galley.size().x <= 372.0, "{label} 超宽: {:?}", text.galley.size());
        }
    });
}
```

- [ ] **Step 2: 全量命令**

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: 全绿。

- [ ] **Step 3: 实机验收（辉哥）**

```bash
bash scripts/bundle.sh
open release/macEverything.app
```

清单：
- [ ] 系统为英文时首启：三窗 + 托盘 + 右键菜单 + 恢复默认确认弹窗全英文。
- [ ] 设置页切「简体中文」：立即生效；重启保持；托盘菜单同步（无需重启）。
- [ ] 设置页切「English」：立即生效；重启保持。
- [ ] 切语言后托盘五项 + 状态项 + 缺口项即时更新；切换期间引擎状态/缺口变化仍以新语言显示。
- [ ] 英文设置窗可被托盘/齿轮原生定位（首次居中、最小化恢复、置前）。
- [ ] 系统中文 + 显式 English：重启后仍 English。
- [ ] 切「跟随系统」：按当前系统首选语言显示。
- [ ] 英文下设置页无裁切；结果窗口状态栏/空态/缺口面板无溢出；悬浮条提示正常截断。
- [ ] 语言切换后查询、选中、滚动、快捷键录制、字号、自启、排除目录均无回归。
- [ ] CLI 输出与日志仍为中文（预期）。

- [ ] **Step 4: 回填文档**

- `docs/specs/i18n-v2.1/design.md` Status → 已实现 + 实机记录。
- `docs/specs/settings-v2.1/design.md` 语言项从「禁用占位」改为「三选一下拉，立即生效」。
- `docs/specs/floatbar-visual/settings-visual.md` 状态行同步；删除「语言禁用」相关描述。
- `README.md` 图形界面对应位置补一句「界面支持简体中文/English（设置内切换，默认跟随系统）」。

- [ ] **Step 5: 提交**

```bash
git add docs/specs/i18n-v2.1 docs/specs/settings-v2.1/design.md docs/specs/floatbar-visual/settings-visual.md README.md
git commit -m "docs(i18n): 验收记录与文档回填"
```

---

## 自检清单（实现者）

- 每个新增文案都在 `catalog!` 中且被实际引用（无 dead field）。
- 所有 `with_lang` 测试结束自动还原；没有测试调用 `set_lang`。
- 原生层（托盘/右键菜单/确认弹窗/窗口标题）都有独立的刷新路径，不依赖 egui 重绘。
- 语言切换后不残留旧语言的应用自产消息；引擎/客户端原始错误除外。
- 提交前 `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` 全绿。

---

## 实现与验证记录（2026-10-18）

- Task 1（i18n 核心与启动语言）：`4f1cdf6`
- Task 2（设置页语言下拉与即时切换）：`60f8ace`
- Task 3（热键提示类型化与悬浮条双语）：`ffca756`（修复 `659f367`、`4da1baf`）
- Task 4（完整窗口双语与状态提示类型化）：`dc70f5a`（修复 `19c3d5d`）
- Task 5（托盘与原生层双语）：`65afab3`（修复 `d4d9ef6`）
- Task 6（剩余自产消息双语）：`1a641bf`、`59a31cf`、`0e08d12`、`b8df240`、`ad306cb`

验证：

- `cargo fmt --check`：通过。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test --workspace`：全绿；app 测试 **357 passed / 0 failed / 3 ignored**（含 Task 7 新增 `english_rows_fit_fixed_controls`）。
- Task 7 Step 1 的英文行宽断言按实际标签列取 **372px**（620 窗宽 − 2×24 内容内边距 − 2×16 行内边距 − 152 控件宽 − 16 标签/控件间距），并同时断言 `Galley::elided == false`，避免截断让宽度断言失去意义。

实机验收：**待辉哥**（Task 7 Step 3 清单；所有实机项均未勾选，不以 headless 结果代替实机）。

已知非阻塞项（来自审查）：

- 启动时 `ensure_engine` 早于 `i18n::set_lang`，仅影响 stderr 技术输出，不影响界面文案。
- `OpenError` 未实现 `Display`，行为不受影响。

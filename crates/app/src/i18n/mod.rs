//! 图形界面文案目录与全局语言（v2.1）。
//!
//! 只服务 app crate 的界面文案；CLI、日志、引擎/客户端原始错误不经过这里。
//! 当前语言是进程级原子（与 `ui/font.rs` 的字号倍率同模式）；测试用线程局部
//! `with_lang` 覆盖，避免并行测试竞态。

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Deserializer, Serialize};

pub mod catalog;

// `strings`/`Strings`/`all_entries` 是目录的对外入口（后续任务与单测使用）；
// Task 1 的生产路径只用到 `s`。
#[allow(unused_imports)]
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
        assert_eq!(
            resolve_language(LanguageSetting::ZhHans, &preferred),
            Lang::ZhHans
        );
        assert_eq!(resolve_language(LanguageSetting::En, &preferred), Lang::En);
        assert_eq!(
            resolve_language(LanguageSetting::System, &preferred),
            Lang::En
        );
    }

    #[test]
    fn with_lang_overrides_then_restores() {
        assert_eq!(lang(), Lang::ZhHans);
        with_lang(Lang::En, || assert_eq!(lang(), Lang::En));
        assert_eq!(lang(), Lang::ZhHans);
    }
}

//! 图形界面文案目录与全局语言（v2.1）。
//!
//! 只服务 app crate 的界面文案；CLI、日志、引擎/客户端原始错误不经过这里。
//! 当前语言是进程级原子（与 `ui/font.rs` 的字号倍率同模式）；测试用线程局部
//! `with_lang` 覆盖，避免并行测试竞态。

use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

use serde::de::{self, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

pub mod catalog;
pub mod format;

// `s` 是绘制路径的文案入口；`strings` 供平台层按语言取标题。
pub use catalog::{s, strings};

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
    /// 宽容解析：未知/非字符串值（数字、布尔、表、数组等）一律按跟随系统处理，
    /// 只留 `log::warn`，不让单个坏字段拖垮整个 `settings.toml`。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct LanguageVisitor;

        impl<'de> Visitor<'de> for LanguageVisitor {
            type Value = LanguageSetting;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("语言字符串 system、zh-hans 或 en")
            }

            fn visit_str<E>(self, raw: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(match raw {
                    "system" => LanguageSetting::System,
                    "zh-hans" => LanguageSetting::ZhHans,
                    "en" => LanguageSetting::En,
                    other => {
                        log::warn!("未知语言设置 {other:?},按跟随系统处理");
                        LanguageSetting::System
                    }
                })
            }

            fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                log::warn!("语言设置应为字符串,收到布尔值 {value},按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                log::warn!("语言设置应为字符串,收到整数 {value},按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                log::warn!("语言设置应为字符串,收到整数 {value},按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                log::warn!("语言设置应为字符串,收到浮点数 {value},按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                log::warn!("语言设置应为字符串,收到空值,按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                log::warn!("语言设置应为字符串,收到空值,按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                // 消费掉整个映射，避免反序列化器残留状态。
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                log::warn!("语言设置应为字符串,收到映射,按跟随系统处理");
                Ok(LanguageSetting::System)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                log::warn!("语言设置应为字符串,收到数组,按跟随系统处理");
                Ok(LanguageSetting::System)
            }
        }

        deserializer.deserialize_any(LanguageVisitor)
    }
}

static LANG_BITS: AtomicU8 = AtomicU8::new(Lang::ZhHans as u8);

/// 模板填充（单趟扫描）：`{name}` 形式占位符；缺参数时保留原样，
/// 替换值中的花括号不会被再次展开（由目录单测保证中英占位符成对）。
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

    #[test]
    fn fill_substitutes_all_args() {
        assert_eq!(
            fill(
                "{default} 不可用,已改用 {name}",
                &[("default", "⌥Space"), ("name", "⌃⌥K"),]
            ),
            "⌥Space 不可用,已改用 ⌃⌥K"
        );
    }

    #[test]
    fn fill_does_not_resubstitute_values() {
        assert_eq!(fill("{a}", &[("a", "{b}")]), "{b}");
        assert_eq!(fill("{a}-{b}", &[("a", "{b}"), ("b", "x")]), "{b}-x");
    }

    #[test]
    fn fill_keeps_missing_placeholder() {
        assert_eq!(fill("前缀 {missing} 后缀", &[]), "前缀 {missing} 后缀");
    }

    #[test]
    fn fill_keeps_unmatched_open_brace_tail() {
        assert_eq!(fill("前缀 {未闭合", &[]), "前缀 {未闭合");
        assert_eq!(fill("{a} 后缀 {未闭合", &[("a", "值")]), "值 后缀 {未闭合");
    }
}

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

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

/// 索引构建条目数。
pub fn index_building_count(n: u64) -> String {
    match lang() {
        Lang::ZhHans => format!("索引构建中({n} 条)"),
        Lang::En if n == 1 => "Building index (1 entry)".to_owned(),
        Lang::En => format!("Building index ({n} entries)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::with_lang;

    #[test]
    fn items_pluralizes_in_both_languages() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(items(1), "1 项");
            assert_eq!(items(2), "2 项");
        });
        with_lang(Lang::En, || {
            assert_eq!(items(1), "1 item");
            assert_eq!(items(2), "2 items");
        });
    }

    #[test]
    fn gaps_pluralizes_in_both_languages() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(gaps(1), "1 个目录未收录");
            assert_eq!(gaps(2), "2 个目录未收录");
        });
        with_lang(Lang::En, || {
            assert_eq!(gaps(1), "1 folder not indexed");
            assert_eq!(gaps(2), "2 folders not indexed");
        });
    }

    #[test]
    fn showing_first_matches_language() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(showing_first(1000), "展示前 1000 条");
        });
        with_lang(Lang::En, || {
            assert_eq!(showing_first(1000), "Showing first 1000");
        });
    }

    #[test]
    fn counts_composes_with_optional_truncation() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(counts(2, 2, "2", "3"), "2 项 · 匹配 2 ms · 读取 3 ms");
            assert_eq!(
                counts(2, 1, "2.5", "3"),
                "2 项 · 匹配 2.5 ms · 读取 3 ms · 展示前 1 条"
            );
        });
        with_lang(Lang::En, || {
            assert_eq!(counts(1, 1, "2", "3"), "1 item · Match 2 ms · Read 3 ms");
            assert_eq!(
                counts(2, 1, "2", "3"),
                "2 items · Match 2 ms · Read 3 ms · Showing first 1"
            );
        });
    }

    #[test]
    fn gaps_header_pluralizes_in_both_languages() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(gaps_header(1), "权限缺口(1 个目录,未收录):");
            assert_eq!(gaps_header(2), "权限缺口(2 个目录,未收录):");
        });
        with_lang(Lang::En, || {
            assert_eq!(gaps_header(1), "Permission Gaps (1 folder not indexed):");
            assert_eq!(gaps_header(2), "Permission Gaps (2 folders not indexed):");
        });
    }

    #[test]
    fn index_building_count_pluralizes_in_both_languages() {
        with_lang(Lang::ZhHans, || {
            assert_eq!(index_building_count(1), "索引构建中(1 条)");
            assert_eq!(index_building_count(2), "索引构建中(2 条)");
        });
        with_lang(Lang::En, || {
            assert_eq!(index_building_count(1), "Building index (1 entry)");
            assert_eq!(index_building_count(2), "Building index (2 entries)");
        });
    }
}

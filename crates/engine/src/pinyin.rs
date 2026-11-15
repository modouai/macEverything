//! 拼音查询展开:读音归一、音节切分、反查表与位图序列匹配。
//! 纯查询侧展开,索引/内存布局/落盘格式零改动(设计见
//! docs/superpowers/specs/2026-11-15-pinyin-search-design.md;技术规格见
//! docs/specs/pinyin-search/tech.md)。
#![cfg_attr(not(test), expect(dead_code))] // 公开接入前仅测试使用;接入后移除本属性
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;

use pinyin::ToPinyinMulti;

/// 单字的归一化读音集:去重、去声调、`ü` 写作 `v`(主流 IME 输入习惯)。
/// 上游 `plain()` 对 j/q/x/y 后的 ü 已写作 u(居 ju、去 qu、学 xue、鱼 yu),
/// 只有 lü/lüe/nü/nüe 保留 `ü`,这里统一转 v(绿 lv、女 nv)。
fn readings(c: char) -> Vec<String> {
    let Some(multi) = c.to_pinyin_multi() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for p in multi {
        let r = p.plain().replace('ü', "v");
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

const CJK_FIRST: u32 = 0x4E00;
const CJK_LAST: u32 = 0x9FFF;

/// 读音反查表 + 完整音节集。进程内首次拼音查询时构建一次,常驻约 500KB。
struct ReverseTable {
    /// 音节前缀 → 读音以它开头的全部汉字(升序去重)。前缀长度 1..=最长读音。
    prefixes: HashMap<Box<str>, Vec<char>>,
    /// 完整音节集(切分表)。由读音数据派生,与字符集天然一致。
    syllables: BTreeSet<Box<str>>,
}

static TABLE: OnceLock<ReverseTable> = OnceLock::new();

/// 全局反查表(懒构建;rayon 并发首次调用由 OnceLock 保证单次)。
fn table() -> &'static ReverseTable {
    TABLE.get_or_init(ReverseTable::build)
}

impl ReverseTable {
    fn build() -> Self {
        let mut prefixes: HashMap<Box<str>, Vec<char>> = HashMap::new();
        let mut syllables: BTreeSet<Box<str>> = BTreeSet::new();
        for cp in CJK_FIRST..=CJK_LAST {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            for reading in readings(c) {
                syllables.insert(reading.as_str().into());
                let mut buf = String::new();
                for ch in reading.chars() {
                    buf.push(ch);
                    prefixes.entry(buf.as_str().into()).or_default().push(c);
                }
            }
        }
        // 同字多读音会重复登记同一前缀:逐前缀排序去重。
        for chars in prefixes.values_mut() {
            chars.sort_unstable();
            chars.dedup();
        }
        Self {
            prefixes,
            syllables,
        }
    }

    fn has_syllable(&self, s: &str) -> bool {
        self.syllables.contains(s)
    }

    fn chars_for(&self, prefix: &str) -> Option<&[char]> {
        self.prefixes.get(prefix).map(Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    // 断言值对应 pinyin 0.11.0 数据；升级 crate 需复核。
    use super::*;

    #[test]
    fn readings_strip_tones_and_fold_umlaut() {
        assert_eq!(readings('绿'), vec!["lv", "lu"]);
        assert_eq!(readings('女'), vec!["nv", "ru"]);
        assert_eq!(readings('居'), vec!["ju", "ji"]);
    }

    #[test]
    fn polyphonic_chars_keep_all_readings() {
        assert_eq!(readings('长'), vec!["zhang", "chang"]);
        let xing = readings('行');
        assert!(xing.iter().any(|r| r == "xing"), "{xing:?}");
        assert!(xing.iter().any(|r| r == "hang"), "{xing:?}");
        assert_eq!(readings('数'), vec!["shu", "shuo"]);
    }

    #[test]
    fn cjk_basic_block_coverage_is_near_complete() {
        let covered = (0x4E00u32..=0x9FFF)
            .filter_map(char::from_u32)
            .filter(|&c| !readings(c).is_empty())
            .count();
        assert!(covered > 20_000, "covered={covered}");
    }

    #[test]
    fn reverse_table_maps_syllables_to_hanzi() {
        let t = table();
        let shu = t.chars_for("shu").unwrap();
        for c in ['数', '书', '树'] {
            assert!(shu.contains(&c), "{c} not in shu set");
        }
        assert!(!shu.contains(&'学'));
    }

    #[test]
    fn polyphonic_chars_land_in_every_reading_set() {
        let t = table();
        assert!(t.chars_for("chang").unwrap().contains(&'长'));
        assert!(t.chars_for("zhang").unwrap().contains(&'长'));
        for s in ["xing", "hang"] {
            assert!(t.chars_for(s).unwrap().contains(&'行'), "{s}");
        }
        assert!(t.chars_for("lv").unwrap().contains(&'绿'));
    }

    #[test]
    fn prefix_sets_cover_partial_syllables() {
        let t = table();
        assert!(t.chars_for("sh").unwrap().contains(&'数'));
        assert!(t.chars_for("s").unwrap().contains(&'书'));
        assert!(t.chars_for("shux").is_none());
    }

    #[test]
    fn complete_syllable_inventory_is_derived_from_readings() {
        let t = table();
        assert!(t.has_syllable("shu"));
        assert!(t.has_syllable("xue"));
        assert!(!t.has_syllable("shux"));
        assert!(!t.has_syllable("p"));
    }
}

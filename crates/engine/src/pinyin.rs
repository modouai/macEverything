//! 拼音查询展开:读音归一、音节切分、反查表与位图序列匹配。
//! 纯查询侧展开,索引/内存布局/落盘格式零改动(设计见
//! docs/superpowers/specs/2026-11-15-pinyin-search-design.md;技术规格见
//! docs/specs/pinyin-search/tech.md)。
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

/// 读音反查表 + 完整音节集。进程内首次拼音查询时构建一次,常驻约 400KB(shrink 后)。
struct ReverseTable {
    /// 音节前缀 → 读音以它开头的全部汉字(升序去重)。前缀长度 1..=最长读音;非 ASCII
    /// 前缀仅 `ê`(2 字节)。
    prefixes: HashMap<Box<str>, Vec<char>>,
    /// 完整音节集(切分表)。由读音数据派生,与字符集天然一致。
    syllables: BTreeSet<Box<str>>,
    /// 最长读音的字节长度(实测 6,如 shuang/chuang/zhuang);用于收敛切分扫描上界。
    max_reading_len: usize,
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
        let mut max_reading_len = 0usize;
        for cp in CJK_FIRST..=CJK_LAST {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            for reading in readings(c) {
                max_reading_len = max_reading_len.max(reading.len());
                if !syllables.contains(reading.as_str()) {
                    syllables.insert(reading.as_str().into());
                }
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
            // Vec 容量按插入次数增长,去重后普遍留有大量松弛;收缩压低常驻内存。
            chars.shrink_to_fit();
        }
        Self {
            prefixes,
            syllables,
            max_reading_len,
        }
    }

    fn has_syllable(&self, s: &str) -> bool {
        self.syllables.contains(s)
    }

    fn chars_for(&self, prefix: &str) -> Option<&[char]> {
        self.prefixes.get(prefix).map(Vec::as_slice)
    }
}

/// 单次查询词最多保留的切分方案数(防御组合爆炸)。
const MAX_PARSES: usize = 8;
/// 查询词字节上限:真实拼音查询远低于此;上限同时兜住递归深度与组合爆炸。
const MAX_WORD_LEN: usize = 64;
/// 单次切分的搜索步数上限:超出即整体放弃展开(回退纯字面),保证最坏耗时有界。
const MAX_SEARCH_STEPS: usize = 10_000;

/// 音节切分:返回全部可行切分(最多 `MAX_PARSES` 条),每条是音节子串序列。
/// 规则:完整音节全枚举 OR、最长优先;末段可为任意音节前缀;`'` 为强制分段边界
/// (边界内仍全枚举);无解返回空。
/// 仅接受 ASCII 且不超过 `MAX_WORD_LEN` 字节的词;搜索步数超过 `MAX_SEARCH_STEPS`
/// 即返回空(调用方回退纯字面),保证最坏耗时有界。
/// 枚举顺序为最长音节优先,截断时保留贪婪切分。
fn segment(word: &str) -> Vec<Vec<&str>> {
    if !word.is_ascii() || word.len() > MAX_WORD_LEN {
        return Vec::new();
    }
    let table = table();
    let mut steps = 0usize;
    let mut out: Vec<Vec<&str>> = vec![Vec::new()];
    for run in word.split('\'').filter(|r| !r.is_empty()) {
        let mut run_parses: Vec<Vec<&str>> = Vec::new();
        let mut cur: Vec<&str> = Vec::new();
        enumerate(run, table, 0, &mut cur, &mut run_parses, &mut steps);
        if steps >= MAX_SEARCH_STEPS || run_parses.is_empty() {
            return Vec::new();
        }
        let mut next: Vec<Vec<&str>> = Vec::new();
        for prefix in &out {
            for p in &run_parses {
                let mut joined = prefix.clone();
                joined.extend(p.iter().copied());
                next.push(joined);
                if next.len() >= MAX_PARSES {
                    break;
                }
            }
            if next.len() >= MAX_PARSES {
                break;
            }
        }
        out = next;
    }
    out.retain(|p| !p.is_empty());
    out
}

fn enumerate<'a>(
    run: &'a str,
    table: &ReverseTable,
    pos: usize,
    cur: &mut Vec<&'a str>,
    out: &mut Vec<Vec<&'a str>>,
    steps: &mut usize,
) {
    if out.len() >= MAX_PARSES {
        return;
    }
    if *steps >= MAX_SEARCH_STEPS {
        return;
    }
    *steps += 1;
    if pos == run.len() {
        if !cur.is_empty() {
            out.push(cur.clone());
        }
        return;
    }
    let tail = &run[pos..];
    // 末段:剩余串是某音节前缀、且本身不是完整音节(完整音节在下方整段消费,避免重复方案)。
    if tail.len() <= table.max_reading_len
        && !table.has_syllable(tail)
        && table.chars_for(tail).is_some()
    {
        let mut v = cur.clone();
        v.push(tail);
        out.push(v);
    }
    // 完整音节:最长优先。
    let max_end = (pos + table.max_reading_len).min(run.len());
    for end in (pos + 1..=max_end).rev() {
        if table.has_syllable(&run[pos..end]) {
            cur.push(&run[pos..end]);
            enumerate(run, table, end, cur, out, steps);
            cur.pop();
            if out.len() >= MAX_PARSES {
                return;
            }
        }
    }
}

const CJK_COUNT: usize = (CJK_LAST - CJK_FIRST + 1) as usize;
const BITMAP_WORDS: usize = CJK_COUNT.div_ceil(64);

/// CJK 基本区位图:一个音节字符集摊平为位图,membership O(1)。2.6KB/集。
struct CharBitmap {
    words: Box<[u64]>,
}

impl CharBitmap {
    fn from_chars(chars: &[char]) -> Self {
        let mut words = vec![0u64; BITMAP_WORDS].into_boxed_slice();
        for &c in chars {
            let cp = c as u32;
            if (CJK_FIRST..=CJK_LAST).contains(&cp) {
                let bit = (cp - CJK_FIRST) as usize;
                words[bit / 64] |= 1 << (bit % 64);
            }
        }
        Self { words }
    }

    fn contains(&self, c: char) -> bool {
        let cp = c as u32;
        if !(CJK_FIRST..=CJK_LAST).contains(&cp) {
            return false;
        }
        let bit = (cp - CJK_FIRST) as usize;
        self.words[bit / 64] & (1 << (bit % 64)) != 0
    }
}

/// 一个查询词的拼音匹配器:全部可行切分方案(每方案一组字符集位图)。
pub struct PinyinMatcher {
    alts: Vec<Vec<CharBitmap>>,
}

impl PinyinMatcher {
    /// 词级条件:仅 ASCII 小写字母与 `'`、至少一个字母、且能切分;否则 None(静默回退字面)。
    /// 大小写/`whole:`/通配符的整轮判定由调用方(query.rs)负责。
    pub fn compile(word: &[u8]) -> Option<Self> {
        let word = std::str::from_utf8(word).ok()?;
        if !word.bytes().any(|b| b.is_ascii_lowercase())
            || !word.bytes().all(|b| b.is_ascii_lowercase() || b == b'\'')
        {
            return None;
        }
        let table = table();
        let alts: Vec<Vec<CharBitmap>> = segment(word)
            .into_iter()
            .map(|parse| {
                parse
                    .iter()
                    .map(|syl| {
                        // segment 产出的音节必在反查表:派生同源。空集是数据不一致的信号,debug 下显式暴露。
                        debug_assert!(
                            table.chars_for(syl).is_some(),
                            "syllable {syl:?} missing from reverse table"
                        );
                        CharBitmap::from_chars(table.chars_for(syl).unwrap_or(&[]))
                    })
                    .collect()
            })
            .collect();
        if alts.is_empty() {
            None
        } else {
            Some(Self { alts })
        }
    }

    /// 命中起点(字节位)。全 ASCII 名称快速短路;多种切分任一命中即返回。
    pub fn hit(&self, name: &[u8]) -> Option<usize> {
        if name.is_ascii() {
            return None;
        }
        let s = std::str::from_utf8(name).ok()?;
        s.char_indices()
            .map(|(start, _)| start)
            .find(|&start| self.alts.iter().any(|alt| seq_matches(&s[start..], alt)))
    }
}

/// 从 hay 起点逐字符匹配一组位图;序列中断即失败。
fn seq_matches(hay: &str, alt: &[CharBitmap]) -> bool {
    // 不变量:每方案至少一段(segment 只保留非空方案,compile 拒绝空 alts)
    debug_assert!(!alt.is_empty());
    let mut idx = 0;
    for ch in hay.chars() {
        if !alt[idx].contains(ch) {
            return false;
        }
        idx += 1;
        if idx == alt.len() {
            return true;
        }
    }
    false
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

    #[test]
    fn prefix_lists_are_sorted_and_deduped() {
        let t = table();
        let l = t.chars_for("l").unwrap();
        // 绿读 lv/lu,同一字符经两个读音重复登记前缀 l;去重后应恰好一次。
        assert_eq!(l.iter().filter(|&&c| c == '绿').count(), 1);
        for prefix in ["l", "shu", "s", "z", "lv"] {
            let v = t.chars_for(prefix).unwrap();
            assert!(
                v.windows(2).all(|w| w[0] < w[1]),
                "{prefix}: 切片非严格升序"
            );
        }
    }

    #[test]
    fn segments_full_pinyin() {
        assert_eq!(segment("shu"), vec![vec!["shu"]]);
        assert_eq!(segment("shuxue")[0], vec!["shu", "xue"]);
    }

    #[test]
    fn partial_tail_syllable_is_supported() {
        assert_eq!(segment("shux"), vec![vec!["shu", "x"]]);
        assert_eq!(segment("sh"), vec![vec!["sh"]]);
    }

    #[test]
    fn ambiguous_pinyin_yields_all_parses() {
        let p = segment("fangan");
        assert!(p.contains(&vec!["fang", "an"]), "{p:?}");
        assert!(p.contains(&vec!["fan", "gan"]), "{p:?}");
        let x = segment("xian");
        assert!(x.contains(&vec!["xian"]), "{x:?}");
        assert!(x.contains(&vec!["xi", "an"]), "{x:?}");
    }

    #[test]
    fn parses_are_unique() {
        for w in [
            "shuxue",
            "xian",
            "fangan",
            "xi'an",
            "xianxian",
            "nihaoshijie",
        ] {
            let mut ps = segment(w);
            let n = ps.len();
            ps.sort();
            ps.dedup();
            assert_eq!(ps.len(), n, "{w} produced duplicate parses");
        }
    }

    #[test]
    fn apostrophe_is_hard_boundary() {
        let p = segment("xi'an");
        assert!(p.contains(&vec!["xi", "an"]), "{p:?}");
        // 边界内仍全枚举(an 可再切为 a+n),但没有任何方案跨过边界
        assert!(p.len() >= 2, "{p:?}");
        assert!(p.iter().all(|alt| alt.first() == Some(&"xi")), "{p:?}");
    }

    #[test]
    fn unsegmentable_words_return_empty() {
        assert!(segment("pdf").is_empty());
        assert!(segment("shuxq").is_empty());
    }

    #[test]
    fn parse_count_is_capped() {
        assert_eq!(segment("xianxianxianxian").len(), MAX_PARSES);
    }

    #[test]
    fn pathological_input_terminates_empty() {
        // 无解尾缀 + 高歧义前缀:步数预算必须兜住组合爆炸
        assert!(segment(&("xian".repeat(12) + "vv")).is_empty());
    }

    #[test]
    fn overlong_input_is_rejected() {
        assert!(segment(&"a".repeat(100_000)).is_empty());
    }

    #[test]
    fn non_ascii_input_is_rejected() {
        assert!(segment("学").is_empty());
        assert!(segment("shu学").is_empty());
    }

    #[test]
    fn apostrophe_edge_cases() {
        assert!(segment("'").is_empty());
        assert!(segment("''").is_empty());
        assert_eq!(segment("'shu'")[0], vec!["shu"]);
        assert_eq!(segment("shu''xue")[0], vec!["shu", "xue"]);
    }

    fn hit_of(name: &str, query: &str) -> Option<usize> {
        PinyinMatcher::compile(query.as_bytes())
            .unwrap_or_else(|| panic!("{query} should compile"))
            .hit(name.as_bytes())
    }

    #[test]
    fn full_pinyin_hits_chinese_name() {
        assert_eq!(hit_of("数学笔记.txt", "shuxue"), Some(0));
        assert_eq!(hit_of("季度报告书.docx", "shu"), Some("季度报告".len()));
    }

    #[test]
    fn ascii_names_never_pinyin_match() {
        assert_eq!(hit_of("shu.txt", "shu"), None);
    }

    #[test]
    fn partial_tail_matches_as_typing() {
        assert_eq!(hit_of("数学.pdf", "shux"), Some(0));
        assert_eq!(hit_of("数学.pdf", "shuxu"), Some(0));
    }

    #[test]
    fn japanese_kana_never_match() {
        assert_eq!(hit_of("しゅうがく.txt", "shu"), None);
    }

    #[test]
    fn non_han_chars_break_the_sequence() {
        assert_eq!(hit_of("数-学", "shuxue"), None);
    }

    #[test]
    fn polyphonic_readings_both_match() {
        assert_eq!(hit_of("长沙.txt", "chang"), Some(0));
        assert_eq!(hit_of("长沙.txt", "zhang"), Some(0));
    }

    #[test]
    fn ambiguous_segmentation_is_or() {
        assert_eq!(hit_of("方案.txt", "fangan"), Some(0));
        assert_eq!(hit_of("反感.txt", "fangan"), Some(0));
    }

    #[test]
    fn prefix_set_recall_covers_longer_syllables() {
        // 完整音节段也使用前缀集:shu 集含 shui/shuang/shuo 开头的字
        assert_eq!(hit_of("水果.txt", "shu"), Some(0));
        assert_eq!(hit_of("双方.txt", "shu"), Some(0));
        assert_eq!(hit_of("说话.txt", "shu"), Some(0));
    }

    #[test]
    fn ineligible_words_do_not_compile() {
        assert!(PinyinMatcher::compile(b"shu*").is_none());
        assert!(PinyinMatcher::compile(b"shu2").is_none());
        assert!(PinyinMatcher::compile(b"\x01").is_none()); // QMARK
        assert!(PinyinMatcher::compile(b"pdf").is_none());
        assert!(PinyinMatcher::compile(b"dot").is_none()); // 字母但无可行切分
        assert!(PinyinMatcher::compile(b"'").is_none());
        // 混合词:词内含非字母(数xue 的 UTF-8 字节)→ 不展开
        assert!(PinyinMatcher::compile("数xue".as_bytes()).is_none());
    }

    #[test]
    fn word_length_boundary_is_64_bytes() {
        assert!(!segment(&"a".repeat(64)).is_empty());
        assert!(segment(&"a".repeat(65)).is_empty());
    }

    #[test]
    fn hard_boundary_cannot_match_single_syllable_word() {
        // 先=xiān 在 xi 集,但 xi'an 要求其后紧跟 an 段,单个「先」不命中
        assert_eq!(hit_of("先.txt", "xi'an"), None);
    }

    #[test]
    fn multi_syllable_hit_at_nonzero_offset() {
        // 起点 0 的「书」在 shu 集但第二个字不在 xue 集;最早成功起点应为 3
        assert_eq!(hit_of("书数学", "shuxue"), Some("书".len()));
        // 多个起点均可命中时返回最早
        assert_eq!(hit_of("数数", "shu"), Some(0));
    }

    #[test]
    fn invalid_utf8_name_returns_none() {
        let m = PinyinMatcher::compile(b"shu").unwrap();
        assert_eq!(m.hit(b"\xff"), None);
    }

    #[test]
    fn apostrophe_query_matches_end_to_end() {
        assert_eq!(hit_of("西安.txt", "xi'an"), Some(0));
    }
}

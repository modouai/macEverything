//! 查询计划:解析(product.md §4 契约)→ 匹配内核与打分。
use crate::entry::{normalize_for_match, normalize_for_match_case_sensitive, NO_PARENT};
use crate::index::IndexView;
use memchr::memmem::Finder;
use rayon::prelude::*;
use unicode_normalization::is_nfc;

/// `?` 占位字节(匹配端按 UTF-8 字符跳过)。0x01 作为占位;含 U+0001 的文件名/查询串
/// 会被当通配符——控制字符输入属误用,行为文档化于此,不做运行时拒绝。
pub const QMARK: u8 = 0x01;

/// `*` 分段后的子串序列 + 首/尾锚:序列按序 AND,首段可钉开头、末段可钉结尾。
/// 锚只属于含通配符(`*`/`?`)的词:串首字面段钉开头、串尾字面段钉结尾;
/// 纯字面词无通配符,双锚皆 false,按子串匹配(「报告」须命中「季度报告.pdf」)。
/// 段默认经 normalize_for_match 归一(NFC + 小写折叠);`case:` 查询仅 NFC 归一、
/// 不做小写折叠(匹配端对 display 名做同一 NFC 归一后逐字节比对)。
#[derive(Debug, PartialEq)]
pub struct Pattern {
    pub parts: Vec<Vec<u8>>,
    /// 词含通配符且串首非 `*`:首段必须命中在名称开头(纯字面词恒 false)。
    pub prefix_anchor: bool,
    /// 词含通配符且串尾非 `*`:末段必须命中到名称结尾(`?` 消耗定长字符,同样产生尾锚;
    /// 纯字面词恒 false)。
    pub suffix_anchor: bool,
}

/// 一次查询的完整匹配计划:文件名词 + 路径词 + 两个全局开关。
#[derive(Debug)]
pub struct QueryPlan {
    pub name_terms: Vec<Pattern>,
    pub path_terms: Vec<Pattern>,
    pub case_sensitive: bool,
    pub whole_word: bool,
}

#[derive(thiserror::Error, Debug)]
#[error("空查询")]
pub struct ParseError;

/// 词的角色:`path:` 词匹配完整路径,其余词匹配文件名。
enum Word {
    Name(String),
    Path(String),
}

/// 解析查询为匹配计划。六语法(product.md §4):空格 AND、`*` 任意长、`?` 恰一字符、
/// `path:` 仅作用于紧跟一个词、`case:` 区分大小写、`whole:` 整词。
/// `case:`/`whole:` 是整轮开关:先收全部开关再统一编译,与书写位置无关。
pub fn parse(query: &str) -> Result<QueryPlan, ParseError> {
    let mut words: Vec<Word> = Vec::new();
    let mut case_sensitive = false;
    let mut whole_word = false;
    let mut tokens = query.split_whitespace();
    while let Some(tok) = tokens.next() {
        if let Some(rest) = tok.strip_prefix("case:") {
            case_sensitive = true;
            push_name(&mut words, rest);
        } else if let Some(rest) = tok.strip_prefix("whole:") {
            whole_word = true;
            push_name(&mut words, rest);
        } else if let Some(rest) = tok.strip_prefix("path:") {
            let word = if rest.is_empty() {
                tokens.next().ok_or(ParseError)?
            } else {
                rest
            };
            words.push(Word::Path(word.to_string()));
        } else {
            words.push(Word::Name(tok.to_string()));
        }
    }
    if words.is_empty() {
        return Err(ParseError);
    }
    let mut plan = QueryPlan {
        name_terms: Vec::new(),
        path_terms: Vec::new(),
        case_sensitive,
        whole_word,
    };
    for word in words {
        match word {
            Word::Name(w) => plan.name_terms.push(compile_pattern(&w, case_sensitive)),
            // 路径词恒折叠编译:路径池只存折叠形态,case: 只作用于文件名侧
            Word::Path(w) => plan.path_terms.push(compile_pattern(&w, false)),
        }
    }
    Ok(plan)
}

/// 前缀开关(case:/whole:)剥出的余量非空时是普通文件名词:`case:README` = 开关 + 词 README。
fn push_name(words: &mut Vec<Word>, rest: &str) {
    if !rest.is_empty() {
        words.push(Word::Name(rest.to_string()));
    }
}

/// 通配编译:`*` 任意长(分段),`?` 恰一字符(QMARK 占位,匹配端跳过一个 UTF-8 字符)。
/// 锚定:词含通配符时,串首非 `*` → 前锚、串尾非 `*` → 尾锚;纯字面词双锚皆 false。
/// 连续 `*` 折叠,不产出空段。parts 默认 NFC + 小写折叠;
/// `case:` 时仅 NFC、不折叠(与查询端 display 归一同管线)。
fn compile_pattern(term: &str, case_sensitive: bool) -> Pattern {
    let source = if case_sensitive {
        normalize_for_match_case_sensitive(term)
    } else {
        normalize_for_match(term)
    };
    let bytes = source.as_bytes();
    let mut parts: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for &b in bytes {
        match b {
            b'*' => {
                if !cur.is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
            }
            b'?' => cur.push(QMARK),
            _ => cur.push(b),
        }
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    // 锚只属于含通配符的词;纯字面词(无 * 无 ?)按子串,双锚皆 false
    let has_wildcard = bytes.contains(&b'*') || bytes.contains(&b'?');
    Pattern {
        parts,
        prefix_anchor: has_wildcard && !bytes.starts_with(b"*"),
        suffix_anchor: has_wildcard && !bytes.ends_with(b"*"),
    }
}

/// 一次执行的产出:hits 为打分排序截断后的条目号,total 为截断前全部命中数,
/// elapsed_ms 只计匹配 + 排序(product 契约:不含任何 stat)。
pub struct QueryOutcome {
    pub hits: Vec<u32>,
    pub total: u32,
    pub elapsed_ms: f64,
}

/// 执行查询:path 命中目录集 → rayon 分块候选过滤与文件名匹配(块内串行)→
/// 主线程 top-limit 选择(O(n) 划分 + 前 k ≤ limit 排序)。
pub fn execute(view: &dyn IndexView, plan: &QueryPlan, limit: usize) -> QueryOutcome {
    let t = std::time::Instant::now();
    let entries = view.entries();
    // 预编译:Finder 构建一次,百万条目匹配零重建(主开销消除)。
    let c_name: Vec<CPattern> = plan.name_terms.iter().map(CPattern::compile).collect();
    let c_path: Vec<CPattern> = plan.path_terms.iter().map(CPattern::compile).collect();

    // path 命中目录集:目录条目的路径池段命中全部 path_terms。池段是折叠形态,
    // 路径词在解析侧恒按折叠编译(case: 只作用于文件名侧),此处比对两侧同形态;
    // whole: 同样作用于路径侧(见 path_terms_match)。墓碑目录的池段可能是改名前
    // 的陈旧值,不参与;其子项必同为墓碑,无行为差。
    let dir_hits: Vec<bool> = if plan.path_terms.is_empty() {
        Vec::new()
    } else {
        (0..entries.len() as u32)
            .into_par_iter()
            .map(|idx| {
                let e = &entries[idx as usize];
                e.is_dir()
                    && e.is_live()
                    && path_terms_match(view.dir_path(idx), &c_path, plan.whole_word)
            })
            .collect()
    };

    // 候选行:(分数, 名长, 条目号)。case: 候选名 = NFC(display)(不折叠);
    // 否则 = match 名(arena 已折叠,零拷贝借用)。
    let mut rows: Vec<(u8, usize, u32)> = (0..entries.len() as u32)
        .into_par_iter()
        .filter_map(|idx| {
            let e = &entries[idx as usize];
            if !e.is_live() {
                return None;
            }
            if !plan.path_terms.is_empty() {
                // 命中目录的直接子项全取(含文件与子目录);文件路径不入池,
                // 以自身 match 名命中 path 词补「词落在文件名段」的情形。
                // 目录自身不按名字入选(命中目录本身不是结果,path 语义测已钉死)。
                // 已知限制:path 词不横跨目录段与文件名——目录池无文件名、
                // 文件只取自身名;tech.md「父路径+名称短拼」留待需求出现再落
                // (Task 21 回填 tech.md),现行为即已知限制。
                let under_hit_dir = e.parent != NO_PARENT && dir_hits[e.parent as usize];
                let name_hits_path =
                    !e.is_dir() && path_terms_match(view.match_name(idx), &c_path, plan.whole_word);
                if !under_hit_dir && !name_hits_path {
                    return None;
                }
            }
            let target: std::borrow::Cow<'_, [u8]> = if plan.case_sensitive {
                // NFC 直通零拷贝:display 名绝大多数为 ASCII/NFC,直接借用原始字节;
                // 仅真非 NFC 形态才归一分配(百万条目逐条 nfc().collect 是百万次小分配)。
                match std::str::from_utf8(view.display_name(idx)) {
                    Ok(s) if is_nfc(s) => std::borrow::Cow::Borrowed(view.display_name(idx)),
                    Ok(s) => {
                        std::borrow::Cow::Owned(normalize_for_match_case_sensitive(s).into_bytes())
                    }
                    // 索引侧名称来自 &str 入库,必为合法 UTF-8;此臂兜损坏数据
                    Err(_) => std::borrow::Cow::Owned(
                        normalize_for_match_case_sensitive(&String::from_utf8_lossy(
                            view.display_name(idx),
                        ))
                        .into_bytes(),
                    ),
                }
            } else {
                std::borrow::Cow::Borrowed(view.match_name(idx))
            };
            if !c_name.is_empty() && !c_name.iter().all(|pt| match_pattern(&target, pt)) {
                return None;
            }
            if plan.whole_word && !all_parts_word_bounded(&target, &c_name) {
                return None;
            }
            Some((score_of(&target, &c_name), target.len(), idx))
        })
        .collect();

    let total = rows.len() as u32;
    // top-limit 选择而非全量排序:select_nth O(n) 划出前 limit 段,再对 k ≤ limit 排序;
    // 全量 sort O(n log n) 计入 elapsed_ms,百万命中时单独击穿 P99 <10ms。
    // 比较子含 idx 决胜成全序,截断集与稳定排序一致(确定性不受不稳定划分影响)。
    if limit == 0 {
        rows.clear();
    } else if limit < rows.len() {
        rows.select_nth_unstable_by(limit - 1, cmp_rows);
        rows.truncate(limit);
    }
    rows.sort_by(cmp_rows);
    let hits = rows.into_iter().map(|(_, _, i)| i).collect();
    QueryOutcome {
        hits,
        total,
        elapsed_ms: t.elapsed().as_secs_f64() * 1000.0,
    }
}

/// 候选行全序比较:分数降序、名长升序、条目号升序决胜。
fn cmp_rows(a: &(u8, usize, u32), b: &(u8, usize, u32)) -> std::cmp::Ordering {
    b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
}

/// path 词对一段路径字节的匹配:全部 Pattern 命中;`whole:` 开启时各段还须整词出现
/// (case:/whole: 作用于整轮,tech.md——路径侧不得静默退化为子串)。
/// 预编译段:普通段携带 memchr Finder——`memmem::find` 每次调用都重建 Finder
/// (needle 预处理),百万条目逐条调用是查询主开销;预编译后搜索只做 SIMD 扫描。
/// QMARK 段与空段无 Finder(走逐字节/恒真)。
struct CPart {
    bytes: Vec<u8>,
    finder: Option<Finder<'static>>,
}

/// 预编译 Pattern(execute 入口一次;不改变公开 Pattern 契约)。
struct CPattern {
    parts: Vec<CPart>,
    prefix_anchor: bool,
    suffix_anchor: bool,
}

impl CPattern {
    fn compile(p: &Pattern) -> Self {
        Self {
            parts: p
                .parts
                .iter()
                .map(|b| CPart {
                    finder: if b.is_empty() || b.contains(&QMARK) {
                        None
                    } else {
                        Some(Finder::new(b).into_owned())
                    },
                    bytes: b.clone(),
                })
                .collect(),
            prefix_anchor: p.prefix_anchor,
            suffix_anchor: p.suffix_anchor,
        }
    }
}

fn path_terms_match(bytes: &[u8], path_terms: &[CPattern], whole: bool) -> bool {
    path_terms.iter().all(|pt| match_pattern(bytes, pt))
        && (!whole || all_parts_word_bounded(bytes, path_terms))
}

/// 单 Pattern 命中判定:parts 按序锚定扫描——首段(前锚时)必须命中在名称开头,
/// 末段(尾锚时)必须命中到名称结尾,中段依次 memmem 且位置递增;
/// 空段序列(纯 `*`)恒真。含 QMARK 的段逐字节比对,QMARK 跳过一个 UTF-8 字符。
fn match_pattern(name: &[u8], p: &CPattern) -> bool {
    if p.parts.is_empty() {
        return true;
    }
    let last = p.parts.len() - 1;
    let mut cursor = 0usize; // 上一段结束位置:下一段从此起搜,保证位置递增
    for (pi, part) in p.parts.iter().enumerate() {
        let head_anchored = pi == 0 && p.prefix_anchor;
        let tail_anchored = pi == last && p.suffix_anchor;
        let end = if head_anchored {
            // 前锚只试位置 0(QMARK 段也走 match_at)
            match_at(name, 0, &part.bytes)
        } else if let Some(f) = &part.finder {
            let hay = &name[cursor..];
            if tail_anchored {
                // 尾锚取最后一次出现:若它延伸不到结尾,更早的出现更不能
                f.find_iter(hay)
                    .last()
                    .map(|o| cursor + o + part.bytes.len())
            } else {
                f.find(hay).map(|o| cursor + o + part.bytes.len())
            }
        } else {
            // QMARK 段(compile 后 finder 为 None;空段不会出现在这里)
            find_with_qmark(name, cursor, &part.bytes, tail_anchored)
        };
        match end {
            Some(e) if !tail_anchored || e == name.len() => cursor = e,
            _ => return false,
        }
    }
    true
}

/// part 在 name 的 off 处恰好匹配时返回结束字节位;QMARK 跳过一个 UTF-8 字符
/// (按首字节判 1-4 字节长度)。
fn match_at(name: &[u8], off: usize, part: &[u8]) -> Option<usize> {
    let (mut h, mut p) = (off, 0usize);
    while p < part.len() {
        if h >= name.len() {
            return None;
        }
        if part[p] == QMARK {
            h += utf8_char_len(name[h]);
            p += 1;
        } else if name[h] == part[p] {
            h += 1;
            p += 1;
        } else {
            return None;
        }
    }
    Some(h)
}

/// 从 from 起按字符步进扫描含 QMARK 的段;need_end 时要求匹配延伸到名称结尾(尾锚)。
/// QMARK 消耗定长不定字节,结束位不可预知,只能逐位试。
fn find_with_qmark(name: &[u8], from: usize, part: &[u8], need_end: bool) -> Option<usize> {
    let mut off = from;
    loop {
        if let Some(end) = match_at(name, off, part) {
            if !need_end || end == name.len() {
                return Some(end);
            }
        }
        if off >= name.len() {
            return None;
        }
        off += utf8_char_len(name[off]);
    }
}

/// UTF-8 字符字节长度(按首字节);独立续字节属损坏输入,防御性按 1 步进。
fn utf8_char_len(b: u8) -> usize {
    match b {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// 打分:首个 name_term 的首 part 在名上的位置——前缀命中 3 > 词边界命中 2 > 子串命中 1。
/// 多词查询打分只取首词首段(其余词只参与命中判定;product/tech 未定义多词打分,
/// 以首词为准是已文档化的决策)。无名词(纯 path)或首 part 缺失(纯 `*`)一律 3,
/// 排序退化为按名长升序。
fn score_of(target: &[u8], name_terms: &[CPattern]) -> u8 {
    let Some(part) = name_terms.first().and_then(|p| p.parts.first()) else {
        return 3;
    };
    if match_at(target, 0, &part.bytes).is_some() {
        return 3;
    }
    if part.finder.is_some() && word_boundary_hit(target, part) {
        return 2;
    }
    1
}

/// `whole:` 语义:name_terms 每个非空、不含 QMARK 的 part 都须整词出现
/// (QMARK 段消耗定长不定字节,词边界不可判定,豁免)。
fn all_parts_word_bounded(target: &[u8], terms: &[CPattern]) -> bool {
    terms.iter().all(|p| {
        p.parts.iter().all(|part| {
            part.bytes.is_empty() || part.finder.is_none() || word_boundary_hit(target, part)
        })
    })
}

/// part 以整词边界出现在名中:命中位置 0 或前字节非词字符,尾部同理。
fn word_boundary_hit(name: &[u8], part: &CPart) -> bool {
    // 调用方(all_parts_word_bounded/score_of)已豁免空段与 QMARK 段;
    // finder 缺失即豁免场景,视为命中(与原实现的短路语义一致)。
    let Some(f) = &part.finder else {
        return true;
    };
    let mut from = 0usize;
    while let Some(o) = f.find(&name[from..]) {
        let pos = from + o;
        let end = pos + part.bytes.len();
        if (pos == 0 || !is_word_byte(name[pos - 1]))
            && (end == name.len() || !is_word_byte(name[end]))
        {
            return true;
        }
        from = pos + 1;
    }
    false
}

/// 词字符:ASCII 字母数字 + 下划线;非 ASCII 连续段(UTF-8 多字节)视为一个词。
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

#[cfg(test)]
mod match_tests {
    use super::*;
    use crate::entry::NO_PARENT;
    use crate::index::{Index, IndexView};

    fn sample() -> Index {
        let mut i = Index::new();
        let docs = i.insert("Documents", NO_PARENT, true);
        let proj = i.insert("projects", NO_PARENT, true);
        let _ = i.insert("季度报告.pdf", docs, false);
        let _ = i.insert("budget.xlsx", docs, false);
        let _ = i.insert("README.md", proj, false);
        i
    }

    #[test]
    fn substr_and_ordering() {
        let i = sample();
        let out = execute(&i, &parse("报告").unwrap(), 100);
        assert_eq!(out.total, 1);
        assert_eq!(i.display_name(out.hits[0]), "季度报告.pdf".as_bytes());
        assert!(out.elapsed_ms >= 0.0);
    }

    #[test]
    fn wildcard_anchors() {
        let i = sample();
        assert_eq!(execute(&i, &parse("*.pdf").unwrap(), 100).total, 1);
        assert_eq!(execute(&i, &parse("*eport*").unwrap(), 100).total, 0); // 测试树无 eport
        assert_eq!(execute(&i, &parse("季度*").unwrap(), 100).total, 1);
    }

    #[test]
    fn path_term_hits_dir_children() {
        let i = sample();
        // documents 折叠后匹配 Documents 目录的路径池段;命中目录的直接子项全取,
        // 目录自身不算命中(path: 的语义是「路径含词的东西」,测试钉死不含命中目录本身)
        let out = execute(&i, &parse("path:documents").unwrap(), 100);
        assert_eq!(out.total, 2); // Documents 下两个文件
    }

    #[test]
    fn case_and_whole() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("README.md", d, false);
        assert_eq!(execute(&i, &parse("readme").unwrap(), 100).total, 1); // 默认不敏感
        assert_eq!(execute(&i, &parse("case:readme").unwrap(), 100).total, 0); // case: 敏感
        assert_eq!(
            execute(&i, &parse("whole:readme.md").unwrap(), 100).total,
            1
        );
        assert_eq!(execute(&i, &parse("whole:read").unwrap(), 100).total, 0); // 整词不中
    }

    #[test]
    fn score_prefix_beats_substring() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        i.insert("报告.txt", d, false);
        i.insert("季度报告.txt", d, false);
        let out = execute(&i, &parse("报告").unwrap(), 100);
        assert_eq!(out.total, 2);
        assert_eq!(i.display_name(out.hits[0]), "报告.txt".as_bytes()); // 前缀命中排前
    }

    #[test]
    fn limit_caps_and_total_accurate() {
        let i = sample();
        let out = execute(&i, &parse("*").unwrap(), 2);
        assert_eq!(out.hits.len(), 2);
        assert_eq!(out.total, 5); // 截断前全部命中数
    }

    #[test]
    fn pure_path_query_works() {
        // 纯 path 查询可用:候选量 = 命中目录子项,不做百万全拼路径(Task 7 review 性能防线)
        let i = sample();
        let out = execute(&i, &parse("path:projects").unwrap(), 100);
        assert_eq!(out.total, 1);
        assert_eq!(i.display_name(out.hits[0]), b"README.md");
    }

    #[test]
    fn case_flag_leaves_path_terms_folded() {
        // path 词恒折叠编译:路径池只存折叠形态,case: 只作用于文件名侧——
        // `case: path:Documents` 与 `path:Documents` 同命中数(大小写不影响路径定位)
        let i = sample();
        assert_eq!(execute(&i, &parse("path:Documents").unwrap(), 100).total, 2);
        assert_eq!(
            execute(&i, &parse("case: path:Documents").unwrap(), 100).total,
            2
        );
    }

    #[test]
    fn case_nfc_but_sensitive() {
        // 索引侧 NFD 形态输入;case: 仅 NFC 归一、不做小写折叠:
        // 大小写敏感不中,NFD 输入与 NFC pattern 归一后相等 → 命中(别名互搜)
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("CAFE\u{301}.md", d, false); // NFD 形态的 CAFÉ.md
        assert_eq!(execute(&i, &parse("case:caf\u{e9}").unwrap(), 100).total, 0);
        assert_eq!(execute(&i, &parse("case:CAF\u{c9}").unwrap(), 100).total, 1);
    }

    #[test]
    fn whole_word_boundaries() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("init.txt", d, false);
        let _ = i.insert("README.md", d, false);
        assert_eq!(execute(&i, &parse("whole:init").unwrap(), 100).total, 1);
        assert_eq!(execute(&i, &parse("whole:read").unwrap(), 100).total, 0); // readme 的前缀,非整词
    }

    #[test]
    fn qmark_consumes_exactly_one_utf8_char() {
        // 契约:? 恰一字符,按首字节跳 1-4 字节——é(2 字节)与 A(1 字节)同为一字符;
        // 报告AB 多一字尾随、报告 缺字符,皆不中
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("报告A", d, false);
        let _ = i.insert("报告AB", d, false);
        let _ = i.insert("报告", d, false);
        let _ = i.insert("报告é", d, false);
        let out = execute(&i, &parse("报告?").unwrap(), 100);
        assert_eq!(out.total, 2);
        let names: Vec<Vec<u8>> = out
            .hits
            .iter()
            .map(|h| i.display_name(*h).to_vec())
            .collect();
        assert!(names.contains(&"报告A".as_bytes().to_vec()));
        assert!(names.contains(&"报告é".as_bytes().to_vec()));
    }

    #[test]
    fn suffix_anchor_takes_last_occurrence() {
        // 契约:尾锚看「命中到名称结尾」——首现位置(2)不达结尾,末现(6)达到 → 命中;
        // 若实现误用首现,此测必红
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("a.pdf.pdf", d, false);
        assert_eq!(execute(&i, &parse("*.pdf").unwrap(), 100).total, 1);
    }

    #[test]
    fn whole_flag_covers_path_terms() {
        // whole: 作用于整轮(tech.md):path 词在路径上同样整词边界,而非静默退化子串
        let i = sample();
        assert_eq!(execute(&i, &parse("path:documents").unwrap(), 100).total, 2);
        assert_eq!(
            execute(&i, &parse("whole: path:documents").unwrap(), 100).total,
            2
        );
        assert_eq!(
            execute(&i, &parse("whole: path:ment").unwrap(), 100).total,
            0
        ); // ment 处 docu|ment|s 词中,边界生效
    }

    #[test]
    fn part_order_follows_pattern_sequence() {
        // 段序锚定:parts 按序出现(a 先于 b)——突变「段搜索改全名搜索破坏段序」时必红
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("axb.txt", d, false);
        let _ = i.insert("bxa.txt", d, false);
        let out = execute(&i, &parse("*a*b*").unwrap(), 100);
        assert_eq!(out.total, 1);
        assert_eq!(i.display_name(out.hits[0]), b"axb.txt");
    }

    #[test]
    fn whole_word_checks_left_boundary() {
        // 左边界:reinit 的 init 处词中,非整词——突变「删左边界判断」时必红
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("reinit.txt", d, false);
        assert_eq!(execute(&i, &parse("whole:init").unwrap(), 100).total, 0);
    }

    #[test]
    fn limit_zero_returns_nothing_but_accurate_total() {
        let i = sample();
        let out = execute(&i, &parse("*").unwrap(), 0);
        assert!(out.hits.is_empty());
        assert_eq!(out.total, 5);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_terms_are_and() {
        let p = parse("季度 报告").unwrap();
        assert_eq!(p.name_terms.len(), 2);
        assert!(!p.case_sensitive);
        assert!(!p.whole_word);
        assert!(p.path_terms.is_empty());
    }

    #[test]
    fn wildcards_compile_to_anchored_parts() {
        let p = parse("*.pdf").unwrap();
        // * 展开为子串序列 + 尾锚:parts=[".pdf"], suffix_anchored
        assert_eq!(p.name_terms[0].parts, vec![b".pdf".to_vec()]);
        assert!(p.name_terms[0].suffix_anchor);
        assert!(!p.name_terms[0].prefix_anchor);

        // ? 恰一字符:QMARK 占位字节入段内;消耗定长字符后即结束 → 尾锚
        let p2 = parse("报告?").unwrap();
        let mut expected = "报告".as_bytes().to_vec();
        expected.push(QMARK);
        assert_eq!(p2.name_terms[0].parts, vec![expected]);
        assert!(p2.name_terms[0].suffix_anchor);
        assert!(p2.name_terms[0].prefix_anchor); // ? 前是字面「报告」→ 前锚
    }

    #[test]
    fn consecutive_stars_collapse_and_bare_star() {
        // 连续 * 折叠,不产空段;首尾皆字面 → 双锚
        let p = parse("a**b").unwrap();
        assert_eq!(p.name_terms[0].parts, vec![b"a".to_vec(), b"b".to_vec()]);
        assert!(p.name_terms[0].prefix_anchor);
        assert!(p.name_terms[0].suffix_anchor);

        // 纯 * → 空段序列 + 双锚皆 false(匹配端恒真,全量命中)
        let star = parse("*").unwrap();
        assert!(star.name_terms[0].parts.is_empty());
        assert!(!star.name_terms[0].prefix_anchor);
        assert!(!star.name_terms[0].suffix_anchor);
    }

    #[test]
    fn prefix_flags_scope() {
        let p = parse("path:合同 2026 case: whole:").unwrap();
        // path: 仅作用于「合同」;2026 是文件名词;case:/whole: 全局开关
        assert_eq!(p.path_terms.len(), 1);
        assert_eq!(p.path_terms[0].parts, vec!["合同".as_bytes().to_vec()]);
        assert_eq!(p.name_terms.len(), 1);
        assert!(p.case_sensitive);
        assert!(p.whole_word);
    }

    #[test]
    fn mid_wildcard_splits_ends_anchored() {
        // 本语法无转义:词中 * 就是通配符,按序切分(非字面);中段 * 自身不带锚,
        // 锚只属于首/末段——本例首尾皆字面,故双锚置位
        let p = parse("path:my*dir").unwrap();
        assert_eq!(p.path_terms[0].parts, vec![b"my".to_vec(), b"dir".to_vec()]);
        assert!(p.path_terms[0].prefix_anchor);
        assert!(p.path_terms[0].suffix_anchor);
    }

    #[test]
    fn empty_query_rejected() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
        assert!(parse("path:").is_err()); // path: 无词可带
        assert!(parse("case:").is_err()); // 只有开关,没有词
    }

    #[test]
    fn flag_prefix_glues_term() {
        // product §4 示例「case:README」:前缀开关 + 余量成词;case: 时词原样不折叠
        let p = parse("case:README").unwrap();
        assert!(p.case_sensitive);
        assert_eq!(p.name_terms.len(), 1);
        assert_eq!(p.name_terms[0].parts, vec![b"README".to_vec()]);
    }

    #[test]
    fn case_flag_is_query_wide() {
        // 整轮开关:case: 写在词后同样生效,先收全部开关再统一编译
        let p = parse("README case:").unwrap();
        assert!(p.case_sensitive);
        assert_eq!(p.name_terms[0].parts, vec![b"README".to_vec()]);
    }

    #[test]
    fn path_flag_consumes_next_word() {
        // path: 与词以空格分隔:仅作用于紧跟的一个词
        let p = parse("path: 合同 2026").unwrap();
        assert_eq!(p.path_terms.len(), 1);
        assert_eq!(p.path_terms[0].parts, vec!["合同".as_bytes().to_vec()]);
        assert_eq!(p.name_terms.len(), 1);
    }
}

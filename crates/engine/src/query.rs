//! 查询计划:解析(product.md §4 契约)→ 匹配(Task 8)。
use crate::entry::normalize_for_match;

/// `?` 占位字节(匹配端按 UTF-8 字符跳过);合法 UTF-8 输入不可能出现 0x01。
pub const QMARK: u8 = 0x01;

/// `*` 分段后的子串序列 + 首/尾锚:序列按序 AND,首段可钉开头、末段可钉结尾。
/// 段默认经 normalize_for_match 折叠;case: 查询原样保留(匹配端对 display 原始形态逐字节复核)。
#[derive(Debug, PartialEq)]
pub struct Pattern {
    pub parts: Vec<Vec<u8>>,
    /// 串首非 `*`:首段必须命中在名称开头。
    pub prefix_anchor: bool,
    /// 串尾非 `*`:末段必须命中到名称结尾(`?` 消耗定长字符,同样产生尾锚)。
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
            Word::Path(w) => plan.path_terms.push(compile_pattern(&w, case_sensitive)),
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
/// 锚定:串首非 `*` → 前锚,串尾非 `*` → 尾锚;连续 `*` 折叠,不产出空段。
fn compile_pattern(term: &str, case_sensitive: bool) -> Pattern {
    let source = if case_sensitive {
        term.to_string()
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
    Pattern {
        parts,
        prefix_anchor: !bytes.starts_with(b"*"),
        suffix_anchor: !bytes.ends_with(b"*"),
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
    fn mid_wildcard_splits_unanchored() {
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

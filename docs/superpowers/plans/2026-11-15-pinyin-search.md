# 拼音搜索（全拼）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 纯 ASCII 全拼查询（`shu`、`shuxue`、`shux`）命中中文文件名（数/书/数学），索引、内存布局与落盘格式零改动。

**Architecture:** 查询侧展开——engine 新增 `pinyin` 模块，把纯字母词切分为音节序列，反查「读音以该音节开头的汉字」位图，在文件名中做连续字符序列匹配；`query.rs` 的 `CPattern` 增加可选拼音分支，命中 = 字面 OR 拼音，评分沿用现有档位。

**Tech Stack:** Rust、`pinyin` crate 0.11（rust-pinyin，纯数据静态表，默认特性含 `heteronym`/`plain`）、rayon（既有）、`OnceLock` 懒构建。

**设计文档:** `docs/superpowers/specs/2026-11-15-pinyin-search-design.md`（行为契约以它为准）。

**前置:** 当前分支 `feat/pinyin-search`，设计文档已提交。

---

### Task 1: 规格档案（spec-first）

项目约定行为契约变更先写规格（CLAUDE.md「规格（spec-first）」）。本任务的三个文件是后续实现的契约来源，内容从设计文档派生，补实现细节。

**Files:**
- Create: `docs/specs/pinyin-search/product.md`
- Create: `docs/specs/pinyin-search/tech.md`
- Create: `docs/specs/pinyin-search/plan.md`

- [ ] **Step 1: 写 product.md（用户契约）**

必含小节：
1. 功能：全拼拼音搜索，默认开启，无开关。
2. 启用条件与命中语义：从设计文档 §3.1–§3.6 照搬为契约表格，逐条列出 `shu`/`shuxue`/`shux`/`xi'an`/`Shu`/`case:`/`whole:`/`shu*`/`path:shu`/`数xue`/`dot`/`pdf` 的期望行为（设计文档 §3.6 的表）。
3. 已知局限（设计文档 §8 的 6 条，逐条保留）。
4. 验收：`shu` 命中数/书/树；`shuxue` 命中数学；「数学.pdf」排在「季度报告书.docx」前；英文文件名的字面搜索行为不变；索引/落盘无变化。

- [ ] **Step 2: 写 tech.md（实现细节）**

必含小节：
1. 模块划分：`crates/engine/src/pinyin.rs`（读音归一 `readings`、反查表 `ReverseTable` + `OnceLock`、切分 `segment`、位图 `CharBitmap`、匹配器 `PinyinMatcher`）；`query.rs` 集成点（`CPattern.pinyin`、`CPattern::compile(p, pinyin_ok)`、`CPattern::hit`、`score_of` 拼音档位）。
2. 算法：读音归一（去声调、`ü`→`v`；实测上游 `plain()` 对 j/q/x/y 已写 `u`，只有 `lü/nü` 保留 `ü`）；反查表构建（遍历 U+4E00–U+9FFF，读全部读音的字符前缀）；切分（完整音节全枚举 OR，末段可前缀，`'` 硬边界，上限 8）；匹配（全 ASCII 短路、逐字符位图、起点扫描）。
3. 数据事实（实现时已实测，写进 tech 供后来者核对）：`pinyin 0.11`，`char::to_pinyin_multi()` 提供多读音；读音数据覆盖 CJK 基本区 20924/20992 字；完整音节 419 个（由读音数据派生）；反查前缀集 501 个、前缀-字符对 81926。
4. 性能预算与内存：设计文档 §5 原文。
5. 依赖：workspace + engine 增加 `pinyin = "0.11"`。
6. 测试策略：单元（pinyin.rs）+ query 集成 + 突变验证 + bench 拼音项。

- [ ] **Step 3: 写 plan.md（任务与验收）**

必含：指向本计划文件 `docs/superpowers/plans/2026-11-15-pinyin-search.md` 的任务编号表；三门禁命令；bench 验收口径（拼音 P99 < 10ms）；文档同步清单（usage.md / architecture.md / development.md / CLAUDE.md）。

- [ ] **Step 4: 提交**

```bash
git add docs/specs/pinyin-search/
git commit -m "docs: add pinyin search specs"
```

---

### Task 2: 依赖与读音归一

**Files:**
- Modify: `Cargo.toml`（workspace dependencies）
- Modify: `crates/engine/Cargo.toml`
- Create: `crates/engine/src/pinyin.rs`
- Modify: `crates/engine/src/lib.rs`

- [ ] **Step 1: 加依赖**

`Cargo.toml` 的 `[workspace.dependencies]` 追加一行（放在 `libc = "0.2"` 后）：

```toml
pinyin = "0.11"
```

`crates/engine/Cargo.toml` 的 `[dependencies]` 末尾追加：

```toml
pinyin.workspace = true
```

- [ ] **Step 2: 写失败的测试（新文件 + 模块声明）**

`crates/engine/src/lib.rs` 在 `pub mod persist;` 之后插入（保持字母序）：

```rust
pub mod pinyin;
```

创建 `crates/engine/src/pinyin.rs`：

```rust
//! 拼音查询展开:读音归一、音节切分、反查表与位图序列匹配。
//! 纯查询侧展开,索引/内存布局/落盘格式零改动(设计见
//! docs/superpowers/specs/2026-11-15-pinyin-search-design.md)。
use pinyin::ToPinyinMulti;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_strip_tones_and_fold_umlaut() {
        assert_eq!(readings('绿'), vec!["lv", "lu"]);
        assert_eq!(readings('女')[0], "nv");
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
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test -p engine pinyin::tests`
Expected: 编译错误 `cannot find function 'readings' in this scope`。

- [ ] **Step 4: 实现 `readings`**

在 `use pinyin::ToPinyinMulti;` 之后加入：

```rust
/// 单字的归一化读音集:去重、去声调、`ü` 写作 `v`(主流 IME 输入习惯)。
/// 上游 `plain()` 对 j/q/x/y 后的 ü 已写作 u(居 ju、去 qu、学 xue、鱼 yu),
/// 只有 lü/nü 保留 `ü`,这里统一转 v(绿 lv、女 nv)。
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
```

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test -p engine pinyin::tests`
Expected: 3 passed。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml Cargo.lock crates/engine/Cargo.toml crates/engine/src/lib.rs crates/engine/src/pinyin.rs
git commit -m "feat(engine): add pinyin dependency and reading normalization"
```

---

### Task 3: 反查表与音节表

**Files:**
- Modify: `crates/engine/src/pinyin.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `pinyin.rs` 的 `mod tests`：

```rust
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
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p engine pinyin::tests`
Expected: 编译错误 `cannot find function 'table'`。

- [ ] **Step 3: 实现反查表**

文件顶部 `use pinyin::ToPinyinMulti;` 之后加两行（项目风格：导入统一放顶部）：

```rust
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;
```

然后在 `readings` 之后加入：

```rust
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
            let Some(c) = char::from_u32(cp) else { continue };
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
        Self { prefixes, syllables }
    }

    fn has_syllable(&self, s: &str) -> bool {
        self.syllables.contains(s)
    }

    fn chars_for(&self, prefix: &str) -> Option<&[char]> {
        self.prefixes.get(prefix).map(Vec::as_slice)
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p engine pinyin::tests`
Expected: 7 passed。

- [ ] **Step 5: 突变验证（项目纪律）**

临时把 `build` 中 `for ch in reading.chars()` 改为 `for ch in reading.chars().skip(1)`（去掉单字母前缀），Run: `cargo test -p engine pinyin::tests`，Expected: `prefix_sets_cover_partial_syllables` 失败。还原改动，再跑一遍确认 7 passed。

- [ ] **Step 6: 提交**

```bash
git add crates/engine/src/pinyin.rs
git commit -m "feat(engine): build pinyin reverse table lazily"
```

---

### Task 4: 音节切分

**Files:**
- Modify: `crates/engine/src/pinyin.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `mod tests`：

```rust
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
    fn apostrophe_is_hard_boundary() {
        assert_eq!(segment("xi'an"), vec![vec!["xi", "an"]]);
    }

    #[test]
    fn unsegmentable_words_return_empty() {
        assert!(segment("pdf").is_empty());
        assert!(segment("shuxq").is_empty());
    }

    #[test]
    fn parse_count_is_capped() {
        assert!(segment("xianxianxianxian").len() <= MAX_PARSES);
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p engine pinyin::tests`
Expected: 编译错误 `cannot find function 'segment'`。

- [ ] **Step 3: 实现切分**

在 `ReverseTable::impl` 之后加入：

```rust
/// 单次查询词最多保留的切分方案数(防御组合爆炸)。
const MAX_PARSES: usize = 8;

/// 音节切分:返回全部可行切分(最多 `MAX_PARSES` 条),每条是音节子串序列。
/// 规则:完整音节全枚举 OR;末段可为任意音节前缀;`'` 为硬边界;无解返回空。
/// 枚举顺序为「最长音节优先」,截断时保留贪婪切分。
fn segment(word: &str) -> Vec<Vec<&str>> {
    let table = table();
    let mut out: Vec<Vec<&str>> = vec![Vec::new()];
    for run in word.split('\'').filter(|r| !r.is_empty()) {
        let mut run_parses: Vec<Vec<&str>> = Vec::new();
        let mut cur: Vec<&str> = Vec::new();
        enumerate(run, table, 0, &mut cur, &mut run_parses);
        if run_parses.is_empty() {
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
) {
    if out.len() >= MAX_PARSES {
        return;
    }
    if pos == run.len() {
        if !cur.is_empty() {
            out.push(cur.clone());
        }
        return;
    }
    let tail = &run[pos..];
    // 末段:剩余串是某音节前缀、且本身不是完整音节(完整音节在下方整段消费,避免重复方案)。
    if !table.has_syllable(tail) && table.chars_for(tail).is_some() {
        let mut v = cur.clone();
        v.push(tail);
        out.push(v);
    }
    // 完整音节:最长优先。
    for end in (pos + 1..=run.len()).rev() {
        if table.has_syllable(&run[pos..end]) {
            cur.push(&run[pos..end]);
            enumerate(run, table, end, cur, out);
            cur.pop();
            if out.len() >= MAX_PARSES {
                return;
            }
        }
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p engine pinyin::tests`
Expected: 13 passed。

- [ ] **Step 5: 突变验证**

临时把 `enumerate` 中的末段判断 `!table.has_syllable(tail) && table.chars_for(tail).is_some()` 改为 `table.chars_for(tail).is_some()`（完整音节也走末段前缀），Run: `cargo test -p engine pinyin::tests`，Expected: `segments_full_pinyin` 失败（出现重复方案）。还原，确认 13 passed。

- [ ] **Step 6: 提交**

```bash
git add crates/engine/src/pinyin.rs
git commit -m "feat(engine): segment pinyin words into syllables"
```

---

### Task 5: 位图与序列匹配器

**Files:**
- Modify: `crates/engine/src/pinyin.rs`

- [ ] **Step 1: 写失败的测试**

追加到 `mod tests`：

```rust
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
    fn ineligible_words_do_not_compile() {
        assert!(PinyinMatcher::compile(b"shu*").is_none());
        assert!(PinyinMatcher::compile(b"shu2").is_none());
        assert!(PinyinMatcher::compile(b"\x01").is_none()); // QMARK
        assert!(PinyinMatcher::compile(b"pdf").is_none());
        assert!(PinyinMatcher::compile(b"'").is_none());
        // 混合词:词内含非字母(数xue 的 UTF-8 字节)→ 不展开
        assert!(PinyinMatcher::compile("数xue".as_bytes()).is_none());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p engine pinyin::tests`
Expected: 编译错误 `cannot find type 'PinyinMatcher'`。

- [ ] **Step 3: 实现位图与匹配器**

在 `enumerate` 之后加入：

```rust
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
            || !word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'\'')
        {
            return None;
        }
        let table = table();
        let alts: Vec<Vec<CharBitmap>> = segment(word)
            .into_iter()
            .map(|parse| {
                parse
                    .iter()
                    .map(|syl| CharBitmap::from_chars(table.chars_for(syl).unwrap_or(&[])))
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
        for (start, _) in s.char_indices() {
            if self.alts.iter().any(|alt| seq_matches(&s[start..], alt)) {
                return Some(start);
            }
        }
        None
    }
}

/// 从 hay 起点逐字符匹配一组位图;序列中断即失败。
fn seq_matches(hay: &str, alt: &[CharBitmap]) -> bool {
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
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test -p engine pinyin::tests`
Expected: 21 passed。

- [ ] **Step 5: 突变验证**

临时把 `CharBitmap::contains` 的返回改为恒 `true`，Run: `cargo test -p engine pinyin::tests`，Expected: `japanese_kana_never_match` 与 `non_han_chars_break_the_sequence` 失败。还原，确认 21 passed。

- [ ] **Step 6: 提交**

```bash
git add crates/engine/src/pinyin.rs
git commit -m "feat(engine): match pinyin sequences via CJK bitmaps"
```

---

### Task 6: query.rs 接入

**Files:**
- Modify: `crates/engine/src/query.rs`
- Test: `crates/engine/src/query.rs` 的 `mod match_tests`

- [ ] **Step 1: 写失败的集成测试**

追加到 `query.rs` 的 `mod match_tests`：

```rust
    #[test]
    fn pinyin_hits_chinese_names_and_ranks_prefix_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        i.insert("数学笔记.txt", d, false);
        i.insert("季度报告书.txt", d, false);
        let out = execute(&i, &parse("shu").unwrap(), 100);
        assert_eq!(out.total, 2);
        assert_eq!(i.display_name(out.hits[0]), "数学笔记.txt".as_bytes()); // 拼音前缀档 3
    }

    #[test]
    fn pinyin_does_not_leak_into_case_whole_path_or_wildcard() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        i.insert("数学笔记.txt", d, false);
        assert_eq!(execute(&i, &parse("case:shu").unwrap(), 100).total, 0);
        assert_eq!(execute(&i, &parse("whole:shu").unwrap(), 100).total, 0);
        assert_eq!(execute(&i, &parse("path:shu").unwrap(), 100).total, 0);
        assert_eq!(execute(&i, &parse("shu*").unwrap(), 100).total, 0);
        // `*shu*` 编译后 parts 与纯字面词 shu 完全相同,必须靠 wildcard 标志排除
        assert_eq!(execute(&i, &parse("*shu*").unwrap(), 100).total, 0);
    }

    #[test]
    fn uppercase_still_expands_after_folding() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        i.insert("数学笔记.txt", d, false);
        assert_eq!(execute(&i, &parse("Shu").unwrap(), 100).total, 1);
    }

    #[test]
    fn pinyin_and_literal_hits_coexist() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        i.insert("shu.txt", d, false);
        i.insert("数学.txt", d, false);
        i.insert("季度报告书.txt", d, false);
        let out = execute(&i, &parse("shu").unwrap(), 100);
        assert_eq!(out.total, 3);
        // 字面前缀与拼音前缀同档 3,子串档 1;同档按名长升序
        assert_eq!(i.display_name(out.hits[0]), "shu.txt".as_bytes());
        assert_eq!(i.display_name(out.hits[2]), "季度报告书.txt".as_bytes());
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p engine pinyin_hits_chinese_names_and_ranks_prefix_first`
Expected: FAIL，`total` 为 0（尚未接入）。

- [ ] **Step 3: 接入 CPattern**

1）文件顶部 `use crate::index::IndexView;` 后加：

```rust
use crate::pinyin::PinyinMatcher;
```

2）`Pattern` 结构体（`query.rs:19`）加通配标志。原因：`*shu*` 编译时 `*` 被剥掉，`parts` 与纯字面词 `shu` 完全一致，拼音展开只能靠解析侧标注排除：

```rust
pub struct Pattern {
    pub parts: Vec<Vec<u8>>,
    pub prefix_anchor: bool,
    pub suffix_anchor: bool,
    /// 词含 `*`/`?`。`*` 在编译时被剥掉，`*shu*` 的 parts 与纯字面词相同，
    /// 拼音展开必须靠此标志排除通配词。
    pub wildcard: bool,
}
```

`compile_pattern`（`query.rs:137`）的构造改为：

```rust
    Pattern {
        parts,
        prefix_anchor: has_wildcard && !bytes.starts_with(b"*"),
        suffix_anchor: has_wildcard && !bytes.ends_with(b"*"),
        wildcard: has_wildcard,
    }
```

3）`CPattern` 结构体（`query.rs:470`）加字段与注释：

```rust
struct CPattern {
    parts: Vec<CPart>,
    prefix_anchor: bool,
    suffix_anchor: bool,
    /// 拼音匹配分支(纯字母词);`case:`/`whole:`/含通配符/`path:` 词不编译。
    pinyin: Option<PinyinMatcher>,
}
```

4）`CPattern::compile`（`query.rs:477`）改为：

```rust
    fn compile(p: &Pattern, pinyin_ok: bool) -> Self {
        let pinyin = if pinyin_ok && !p.wildcard && p.parts.len() == 1 {
            PinyinMatcher::compile(&p.parts[0])
        } else {
            None
        };
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
            pinyin,
        }
    }

    /// 词命中:字面 OR 拼音。
    fn hit(&self, name: &[u8]) -> bool {
        match_pattern(name, self)
            || self
                .pinyin
                .as_ref()
                .is_some_and(|m| m.hit(name).is_some())
    }
```

5）`execute` 编译入口（`query.rs:161-162`）改为：

```rust
    let pinyin_ok = !plan.case_sensitive && !plan.whole_word;
    let c_name: Vec<CPattern> = plan
        .name_terms
        .iter()
        .map(|p| CPattern::compile(p, pinyin_ok))
        .collect();
    let c_path: Vec<CPattern> = plan
        .path_terms
        .iter()
        .map(|p| CPattern::compile(p, false))
        .collect();
```

6）候选过滤（`query.rs:268`）改为：

```rust
                if !c_name.is_empty() && !c_name.iter().all(|pt| pt.hit(&target)) {
                    return None;
                }
```

7）`score_of`（`query.rs:592`）改为：

```rust
fn score_of(target: &[u8], name_terms: &[CPattern]) -> u8 {
    let Some(term) = name_terms.first() else {
        return 3;
    };
    let Some(part) = term.parts.first() else {
        return 3;
    };
    let mut score = 1;
    if match_at(target, 0, &part.bytes).is_some() {
        score = 3;
    } else if part.finder.is_some() && word_boundary_hit(target, part) {
        score = 2;
    }
    if score < 3 && term.pinyin.as_ref().is_some_and(|m| m.hit(target) == Some(0)) {
        score = 3;
    }
    score
}
```

- [ ] **Step 4: 运行全量测试**

Run: `cargo test -p engine`
Expected: 全部通过（含既有测试与 3 个新集成测试）。

- [ ] **Step 5: 突变验证**

临时把 `CPattern::hit` 改为只返回 `match_pattern(name, self)`（去掉拼音分支），Run: `cargo test -p engine pinyin_`，Expected: 2 个测试失败。还原，全量测试确认通过。

- [ ] **Step 6: 提交**

```bash
git add crates/engine/src/query.rs
git commit -m "feat(engine): wire pinyin matching into query execution"
```

---

### Task 7: 基准（合成树中文名 + 拼音 P99）

**Files:**
- Modify: `scripts/spike-readdir/src/main.rs:27-38`
- Modify: `scripts/bench.sh`

- [ ] **Step 1: 生成器加中文名**

`scripts/spike-readdir/src/main.rs` 的 `gen` 函数整体替换为：

```rust
fn gen(root: &std::path::Path, total: u64) {
    let _ = fs::remove_dir_all(root);
    // 中文名占比(百分比):拼音查询基准需要含中文名的合成树。
    // 默认 20;0 = 纯 ASCII(旧基线)。文件名按 100 一组,每组中文名取同一词。
    let cn_pct: u64 = std::env::var("MCE_GEN_CN_PCT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    const CN_NAMES: &[&str] = &[
        "数学笔记", "季度报告书", "数据分析", "项目文档", "会议记录", "照片备份", "通讯录",
        "音乐库",
    ];
    let dirs = total / 40; // 平均每目录 40 项
    for d in 0..dirs {
        let dir = root.join(format!("d{:07}", d / 500)).join(format!("s{:03}", d % 500));
        fs::create_dir_all(&dir).unwrap();
        for f in 0..40 {
            let n = d * 40 + f;
            let name = if n % 100 < cn_pct {
                format!(
                    "{}_{:07}.txt",
                    CN_NAMES[((n / 100) as usize) % CN_NAMES.len()],
                    n
                )
            } else {
                format!("file_{:07}.txt", n)
            };
            fs::write(dir.join(name), "").unwrap();
        }
    }
    println!("generated {} dirs under {}", dirs, root.display());
}
```

- [ ] **Step 2: bench.sh 加拼音查询项**

在「== 2. 查询 P99」段之后、「== 3. 增量时效」段之前插入：

```bash
echo "== 2b. 拼音查询 P99(200 次:shuxue/shux 交替,红线 P99 < 10ms) =="
# 预热:反查表 OnceLock 懒构建计入首查,不占 P99 样本
HOME="$BENCH_HOME" "$BIN" query "shuxue" >/dev/null 2>&1 || true
TIMES=$(mktemp /tmp/mce-qp.XXXXXX)
for i in $(seq 1 200); do
  if [ $((i % 2)) -eq 0 ]; then kw="shuxue"; else kw="shux"; fi
  HOME="$BENCH_HOME" "$BIN" query "$kw" 2>/dev/null \
    | head -1 | grep -oE '匹配 [0-9]+\.[0-9]+' | grep -oE '[0-9]+\.[0-9]+' >> "$TIMES" || true
done
python3 - "$TIMES" <<'PY'
import sys
xs = sorted(float(l) for l in open(sys.argv[1]) if l.strip())
if xs:
    n = len(xs)
    print(f"  拼音样本 {n} · 中位 {xs[n//2]:.1f} ms · P99 {xs[min(n-1, int(n*0.99))]:.1f} ms · 最大 {xs[-1]:.1f} ms")
PY
rm -f "$TIMES"
```

同时把脚本头部注释第 2 行「五项基准」改为「六项基准（新增拼音查询）」。

- [ ] **Step 3: 构建并实跑基准**

Run:

```bash
cargo build --release
bash scripts/bench.sh /tmp/mce-bench 1000000
```

Expected（判定口径）:
- 「2b. 拼音查询 P99」输出样本 200，P99 < 10ms；
- 「2. 查询 P99」与历史值同量级（合成树名字变化不影响 ASCII 关键词命中）；
- `shuxue` 实际命中：`target/release/macEverything query shuxue` 的「共 N 项」应约 2.5 万（1M × 20% × 1/8）。若为 0，检查合成树是否含中文名（`ls /tmp/mce-bench/d0000000/s000`）。

若 P99 超红线：先确认 `match_seq` 是否在纯 ASCII 名上被调用（应被 `is_ascii` 短路），再带着实测数字停下分析，不直接改预算。

- [ ] **Step 4: 提交**

```bash
git add scripts/spike-readdir/src/main.rs scripts/bench.sh
git commit -m "bench: cover pinyin queries with chinese filenames in synthetic tree"
```

---

### Task 8: 文档同步与三门禁终检

**Files:**
- Modify: `docs/development.md:43,99`
- Modify: `CLAUDE.md:31`
- Modify: `docs/usage.md`
- Modify: `docs/architecture.md`
- Modify: `docs/specs/pinyin-search/plan.md`

- [ ] **Step 1: 同步文档**

1）`docs/development.md` 第 43 行「五项基准(隔离 MCE_ROOTS)」→「六项基准(隔离 MCE_ROOTS;含拼音查询)」；第 99 行注释改为「六项:首扫/查询 P99/拼音 P99/增量/落盘/停机重放」，并在基准小节补一句：`MCE_GEN_CN_PCT`（默认 20）控制合成树中文名占比。
2）`CLAUDE.md` 第 31 行「（五项基准，自动隔离环境）」→「（六项基准，自动隔离环境）」。
3）`docs/usage.md` 查询语法小节新增「拼音匹配」：默认开启；纯字母词按全拼音节匹配中文名（`shu` → 数/书/树，`shuxue` → 数学，`shux` 支持未打完的末音节）；`case:`/`whole:`/含通配符的词、`path:` 词不展开；`'` 作音节硬边界（`xi'an`）；`ü` 用 `v` 输入（`lv` → 绿）。
4）`docs/architecture.md` 查询管线段落补一句：查询侧拼音展开（`pinyin.rs` + `CPattern.pinyin`），索引与落盘无变化。
5）`docs/specs/pinyin-search/plan.md` 标记实现完成（注明日期与最终门禁结果）。

- [ ] **Step 2: 三门禁**

Run:

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

Expected: 三条全绿。任何一条不绿：修到绿再提交。

- [ ] **Step 3: 实机冒烟（手动，非门禁）**

Run:

```bash
cargo build --release
export HOME=/tmp/mce-pinyin-smoke MCE_ROOTS=/tmp/mce-pinyin-smoke/root
rm -rf "$HOME" && mkdir -p "$MCE_ROOTS"
touch "$MCE_ROOTS/数学笔记.txt" "$MCE_ROOTS/季度报告书.txt" "$MCE_ROOTS/shu.txt"
target/release/macEverything start
target/release/macEverything query shu
target/release/macEverything query shuxue
target/release/macEverything stop
```

Expected: `query shu` 共 3 项（`shu.txt` 字面 + 两个中文名的拼音命中，中文前缀排前）；`query shuxue` 共 1 项（数学笔记）。结束后 `rm -rf /tmp/mce-pinyin-smoke`。

- [ ] **Step 4: 提交**

```bash
git add docs CLAUDE.md
git commit -m "docs: sync pinyin search usage, architecture and bench"
```

---

## 验收清单（对照设计文档）

| 设计条目 | 落地 |
|---|---|
| 仅全拼、默认开启 | Task 2–6；无配置面 |
| 切分（全枚举、`'` 边界、末段前缀、上限 8） | Task 4 |
| 多音字任一读音 | Task 2/3 测试钉死 |
| 任意位置命中 + 前缀档评分 | Task 5/6 |
| `case:`/`whole:`/通配符/`path:`/混合词不展开 | Task 6 集成测试 |
| 索引/内存/落盘零改动 | 仅 engine 查询层改动；parity 天然通过 |
| 性能 P99 < 10ms | Task 7 实测 |
| 突变验证 | Task 3/5/6 各一条 |
| spec-first 文档 | Task 1、Task 8 |

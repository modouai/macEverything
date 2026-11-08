//! 查询计划:解析(product.md §4 契约)→ 匹配内核与打分。
use crate::bundle::is_bundle_name;
use crate::entry::{normalize_for_match, normalize_for_match_case_sensitive, Entry, NO_PARENT};
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

/// 一次查询的完整匹配计划:文件名词 + 路径词 + 两个解析开关 + 调用方级目录过滤。
#[derive(Debug)]
pub struct QueryPlan {
    pub name_terms: Vec<Pattern>,
    pub path_terms: Vec<Pattern>,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// 仅目录(保存面板助手):候选层过滤非目录,total 同口径。
    /// parse 恒 false;由调用方(engine 外的 daemon/client)按协议位显式置位,
    /// 不在 parse 语法内。
    pub dir_only: bool,
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
        dir_only: false,
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
///
/// 条目遍历走分段枚举(tech.md §4.3 接口改造):视图产出若干连续 Entry 段
/// (单源一段,S3 双源合成视图两段),本函数不假设单一切片——各段再切成定长块
/// 作为 rayon 并行粒度,块内直切切片零逐条目虚调用,块间工作窃取并行。
pub fn execute(view: &dyn IndexView, plan: &QueryPlan, limit: usize) -> QueryOutcome {
    let t = std::time::Instant::now();
    // 预编译:Finder 构建一次,百万条目匹配零重建(主开销消除)。
    let c_name: Vec<CPattern> = plan.name_terms.iter().map(CPattern::compile).collect();
    let c_path: Vec<CPattern> = plan.path_terms.iter().map(CPattern::compile).collect();

    // 分段 → 定长块:(块起始条目号, 块切片)。16k 条/块:百万级单段切成 ~77 块,
    // 负载均衡足够;块内串行逐条(条目号 = 块起始号 + 块内下标)。
    const BLOCK: usize = 16 * 1024;
    let segments = view.entry_segments();
    // 分段稠密性自检(debug):trait 契约要求段按序无缝拼满 0..N-1(见
    // IndexView::entry_segments)——首段起于 0,且每段「起始号 + 段长」等于下
    // 一段起始号;违反即 dir_hits/父指针的全局下标访问失去意义(稀疏编号越界
    // 或错位),测试期先红。
    debug_assert!(
        segments.is_empty()
            || (segments[0].0 == 0
                && segments
                    .windows(2)
                    .all(|w| w[0].0 + w[0].1.len() as u32 == w[1].0)),
        "entry_segments 必须按序稠密覆盖条目号空间 0..N-1"
    );
    let mut blocks: Vec<(u32, &[Entry])> = Vec::new();
    for &(start, seg) in &segments {
        for (i, chunk) in seg.chunks(BLOCK).enumerate() {
            blocks.push((start + (i * BLOCK) as u32, chunk));
        }
    }

    // path 命中目录集:目录条目的路径池段命中全部 path_terms。池段是折叠形态,
    // 路径词在解析侧恒按折叠编译(case: 只作用于文件名侧),此处比对两侧同形态;
    // whole: 同样作用于路径侧(见 path_terms_match)。墓碑目录的池段可能是改名前
    // 的陈旧值,不参与;其子项必同为墓碑,无行为差。
    // dir_hits 与条目号全量对齐(块间并行产出后按块序拼接 = 条目号序)。
    // 块内 (start..) 前缀 id 序 zip 切片:块起始号按值进入迭代链,内层闭包只借
    // execute 栈帧上的 view/plan/编译产物,不持外层闭包局部量。
    // dir_hits 不做有效性判定(entry_valid):被屏蔽目录只影响其自身与子项的
    // 候选资格,子项在候选层逐条判定;此处仅为预过滤器,多放进来的失效目录
    // 不会产出结果,避免 path 查询对全量目录做上溯链判定。
    let dir_hits: Vec<bool> = if plan.path_terms.is_empty() {
        Vec::new()
    } else {
        blocks
            .par_iter()
            .flat_map_iter(|&(start, chunk)| {
                (start..).zip(chunk.iter()).map(|(idx, e)| {
                    e.is_dir()
                        && e.is_live()
                        && path_terms_match(view.dir_path(idx), &c_path, plan.whole_word)
                })
            })
            .collect()
    };

    // 候选行:((置顶位 | 分数) 组合键, 名长, 条目号)。置顶位占高 8 位,
    // 低 8 位为 score(1..3)——单次整数比较即完成两级排序,行大小不变(16B)。
    // case: 候选名 = NFC(display)(不折叠);
    // 否则 = match 名(arena 已折叠,零拷贝借用)。
    // entry_valid 为 T3 合成视图的屏蔽判定挂点(tech.md §4.2):Base 命中经
    // 条目号链上溯屏蔽陈旧子树,Overlay 命中抑制 base 背书 anchor 的双份;
    // 单源视图默认恒真,零开销。
    let mut rows: Vec<(u32, usize, u32)> = blocks
        .par_iter()
        .flat_map_iter(|&(start, chunk)| {
            (start..).zip(chunk.iter()).filter_map(|(idx, e)| {
                if !e.is_live() || !view.entry_valid(idx) {
                    return None;
                }
                // dir_only(保存面板助手):候选层滤掉非目录,rows 只剩目录,
                // total(rows 长度)自然同口径只计目录。
                if plan.dir_only && !e.is_dir() {
                    return None;
                }
                if !plan.path_terms.is_empty() {
                    // 命中目录的直接子项全取(含文件与子目录);文件路径不入池,
                    // 以自身 match 名命中 path 词补「词落在文件名段」的情形。
                    // 目录自身不按名字入选(命中目录本身不是结果,path 语义测已钉死)。
                    // 已知限制:path 词不横跨目录段与文件名——目录池无文件名、
                    // 文件只取自身名;tech.md「父路径+名称短拼」留待需求出现再落
                    // (Task 21 回填 tech.md),现行为即已知限制。
                    // parent_idx:T3 合成视图的父号重映射挂点——dir_hits 按全局
                    // 合成号对齐,Overlay 条目的 parent 是 overlay 空间号,直接
                    // 下标会命中无关槽位(path 查询随机丢失/误收)。
                    let parent = view.parent_idx(idx);
                    let under_hit_dir = parent != NO_PARENT && dir_hits[parent as usize];
                    let name_hits_path = !e.is_dir()
                        && path_terms_match(view.match_name(idx), &c_path, plan.whole_word);
                    if !under_hit_dir && !name_hits_path {
                        return None;
                    }
                }
                let target: std::borrow::Cow<'_, [u8]> = if plan.case_sensitive {
                    // NFC 直通零拷贝:display 名绝大多数为 ASCII/NFC,直接借用原始字节;
                    // 仅真非 NFC 形态才归一分配(百万条目逐条 nfc().collect 是百万次小分配)。
                    match std::str::from_utf8(view.display_name(idx)) {
                        Ok(s) if is_nfc(s) => std::borrow::Cow::Borrowed(view.display_name(idx)),
                        Ok(s) => std::borrow::Cow::Owned(
                            normalize_for_match_case_sensitive(s).into_bytes(),
                        ),
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
                Some((
                    ((ranks_first(view, idx, e.is_dir()) as u32) << 8)
                        | u32::from(score_of(&target, &c_name)),
                    target.len(),
                    idx,
                ))
            })
        })
        .collect();

    let total = rows.len() as u32;
    // top-limit 选择而非全量排序:select_nth O(n) 划出前 limit 段,再对 k ≤ limit 排序;
    // 全量 sort O(n log n) 计入 elapsed_ms,百万命中时单独击穿 P99 <10ms。
    // 比较子含 idx 决胜成全序,截断集与稳定排序一致(确定性不受不稳定划分影响)。
    // 置顶位为最高维度（候选收集时已编入高 8 位），组内按 score 降序、
    // 名长升序、条目号决胜。
    let cmp = |a: &(u32, usize, u32), b: &(u32, usize, u32)| {
        b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2))
    };
    if limit == 0 {
        rows.clear();
    } else if limit < rows.len() {
        rows.select_nth_unstable_by(limit - 1, &cmp);
        rows.truncate(limit);
    }
    rows.sort_by(&cmp);
    let hits = rows.into_iter().map(|(_, _, i)| i).collect();
    QueryOutcome {
        hits,
        total,
        elapsed_ms: t.elapsed().as_secs_f64() * 1000.0,
    }
}

/// 名字判定：目录且 display 名以 `.app` 结尾（ASCII 大小写不敏感，`case:` 的
/// 原大小写候选同判）。置顶与否还取决于祖先路径，见 `ranks_first`。
fn is_app_entry(is_dir: bool, display: &[u8]) -> bool {
    const SUFFIX: &[u8] = b".app";
    is_dir
        && display.len() >= SUFFIX.len()
        && display[display.len() - SUFFIX.len()..].eq_ignore_ascii_case(SUFFIX)
}

/// 默认相关度排序的最高维度：非包内的 `.app` 目录置顶。
/// 判定 = 目录 + display 名 `.app` 后缀 + 完整路径的祖先段无 bundle 组件。
///
/// 用 `dir_path` 而不是父链上溯：`CompositeView` 的桥接 anchor 父号是
/// `NO_PARENT`，`anchor_base` 才承载回 base 链的链接，按 `parent_idx` 上溯会在
/// 桥处断开——增量新建的深层 `.app` 会因此误判为非包内。写侧
/// `rewrite_dir_path` 已把两段路径拼进路径池，`dir_path` 是对合成视图正确的
/// 单一真相。路径解析仅对 `.app` 目录命中发生：非目录在 `is_dir` 处短路，
/// 普通条目零额外成本。
fn ranks_first(view: &dyn IndexView, idx: u32, is_dir: bool) -> bool {
    if !is_dir || !is_app_entry(is_dir, view.display_name(idx)) {
        return false;
    }
    let path = view.dir_path(idx);
    match path.iter().rposition(|&b| b == b'/') {
        Some(pos) => !path[..pos].split(|&b| b == b'/').any(is_bundle_name),
        None => true,
    }
}

/// 列排序键(完整窗口列头):Relevance = v1 质量序(委托 execute),
/// Name/Size/Mtime/Path = 四个可排序列。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Relevance,
    Name,
    Size,
    Mtime,
    Path,
}

/// execute_sorted 的产出:elapsed_ms 只计匹配 + 键计算 + 排序(裁决:不含 meta_of
/// 的 stat 耗时——该耗时独计 stat_ms),stat_ms 为包住 meta_of 的耗时(排序轮)。
/// 排序算出的元数据由 daemon 侧闭包自持(缓存 map,响应组装时免重查)——本结构不含 meta。
pub struct SortOutcome {
    pub hits: Vec<u32>,
    pub total: u32,
    pub elapsed_ms: f64,
    pub stat_ms: f64,
}

/// 一条元数据行:(大小, 修改时间);None = 该字段读取失败。
pub type MetaRow = (Option<u64>, Option<u64>);

/// 批量元数据闭包引用:入参为候选条目号切片,按序返回对应 MetaRow。
/// 签名成别名供 engine 入口与 daemon 侧实现共用(clippy type_complexity 门禁);
/// 显式带生命周期参数,容许捕获局部变量的闭包(daemon 侧缓存 map)。
pub type MetaOf<'a> = &'a dyn Fn(&[u32]) -> Vec<MetaRow>;

/// 按列排序查询。meta_of 为批量元数据注入:入参为候选条目号切片,按序返回
/// (大小, 修改时间);daemon 侧内部并行 stat(rayon),engine 单次顺序调用,无 Sync 约束。
/// 排序键形态(定案):Name/Path 取 display 原始形态 Unicode 码点序,不做本地化排序
/// (与 Finder 的拼音/区域序不同,已知局限)。等值键以条目号决胜(确定性,同 v1 约定)。
/// 方向(定案,Task 19b):desc = **反转该键的自然方向**,不是绝对升/降。自然方向
/// Size/Name/Path 升序(小/首字面/短路径在前)、Mtime 降序(最新在前,文件管理器惯例)。
/// desc 反转发生在**全量命中排序之后、截断之前**——total > K 时取真最大/最小 K 条
/// (修复「自然方向前 K 条反转」把截断集反转的误差);等值键组内条目号随反转逆序,
/// 同键组内顺序无用户可感知语义,接受(定案)。
/// Relevance 原样委托 execute(不调 meta_of、忽略 desc,回归钉)。
pub fn execute_sorted(
    view: &dyn IndexView,
    plan: &QueryPlan,
    limit: usize,
    sort: SortKey,
    desc: bool,
    meta_of: MetaOf<'_>,
) -> SortOutcome {
    if sort == SortKey::Relevance {
        let out = execute(view, plan, limit);
        return SortOutcome {
            hits: out.hits,
            total: out.total,
            elapsed_ms: out.elapsed_ms,
            stat_ms: 0.0,
        };
    }
    let t = std::time::Instant::now();
    // 全量匹配(不截断):execute 的 limit 截断在排序后,传 usize::MAX 时
    // limit < rows.len() 不成立、select_nth 跳过,得全量重排;截断由本函数统一做。
    let full = execute(view, plan, usize::MAX);
    let mut hits = full.hits;
    let total = full.total;
    // 键计算:仅 Size/Mtime 需要元数据(批量闭包注入);Name/Path 键来自 arena,零 stat。
    let stat_t = std::time::Instant::now();
    let meta: Vec<(Option<u64>, Option<u64>)> = match sort {
        SortKey::Size | SortKey::Mtime => meta_of(&hits),
        _ => vec![],
    };
    let stat_ms = stat_t.elapsed().as_secs_f64() * 1000.0;
    match sort {
        SortKey::Size => {
            let idx: std::collections::HashMap<u32, (Option<u64>, Option<u64>)> =
                hits.iter().copied().zip(meta).collect();
            hits.sort_by(|a, b| {
                let ka = idx[a].0.unwrap_or(u64::MAX); // 读取失败排最后(升序下最大)
                let kb = idx[b].0.unwrap_or(u64::MAX);
                ka.cmp(&kb).then(a.cmp(b))
            });
        }
        SortKey::Mtime => {
            let idx: std::collections::HashMap<u32, (Option<u64>, Option<u64>)> =
                hits.iter().copied().zip(meta).collect();
            hits.sort_by(|a, b| {
                let ka = idx[a].1.unwrap_or(0); // 读取失败排最后(降序下 0 最小)
                let kb = idx[b].1.unwrap_or(0);
                kb.cmp(&ka).then(a.cmp(b))
            });
        }
        SortKey::Name => hits.sort_by(|a, b| {
            view.display_name(*a)
                .cmp(view.display_name(*b))
                .then(a.cmp(b))
        }),
        SortKey::Path => hits.sort_by(|a, b| {
            view.full_path_dyn(*a)
                .cmp(&view.full_path_dyn(*b))
                .then(a.cmp(b))
        }),
        SortKey::Relevance => unreachable!("Relevance 已在入口委托 execute"),
    }
    // desc = 反转自然方向;先反转后截断,取的是全量命中里真最大/最小的 K 条
    // (先前 K 再反转 = 截断集反转,语义崩坏,Task 19b 修复的核心)。
    if desc {
        hits.reverse();
    }
    hits.truncate(limit);
    // 口径(裁决):elapsed_ms = 全程 − stat 轮耗时,即「匹配 + 键计算 + 排序」
    // 不含任何 stat;stat_ms 独计 meta_of 耗时(与 v1 的 elapsed 不含 stat 同口径)。
    let elapsed_ms = t.elapsed().as_secs_f64() * 1000.0 - stat_ms;
    SortOutcome {
        hits,
        total,
        elapsed_ms,
        stat_ms,
    }
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

    /// dir_only = true 时只返回目录;total 同步只计目录(截断前全量口径不变)。
    /// 突变「execute 去掉 dir_only 过滤」时文件命中回归必红。
    #[test]
    fn dir_only_filters_files_and_counts_total() {
        let i = sample();
        // "r" 恰命中 projects(目录)与 README.md(文件);Documents/budget/季度报告 无 r
        let mut plan = parse("r").unwrap();
        plan.dir_only = true;
        let out = execute(&i, &plan, 100);
        assert_eq!(out.total, 1, "只计目录");
        assert_eq!(i.display_name(out.hits[0]), b"projects");
        // 默认 plan(false)行为不变:文件照常命中。
        let all = execute(&i, &parse("r").unwrap(), 100);
        assert_eq!(all.total, 2, "缺省仍命中文件");
        assert!(
            all.hits.iter().any(|&h| i.display_name(h) == b"README.md"),
            "缺省不过滤:README.md 应在 hits 中"
        );
    }

    /// dir_only 与 path: 词组合:候选(命中目录的直接子项)中的文件同样被过滤。
    /// 突变「dir_only 过滤不作用于 path 候选」时 total 回归 2,必红。
    #[test]
    fn dir_only_filters_path_candidate_files() {
        let i = sample();
        let mut plan = parse("path:documents").unwrap(); // Documents 下两个文件
        plan.dir_only = true;
        assert_eq!(execute(&i, &plan, 100).total, 0, "子项全是文件,全被滤掉");
    }

    /// path: + dir_only 的保持侧正例:命中目录下的子目录存活(命中且 is_dir),
    /// 同层文件被滤除。内联树,不改共享 sample()(其计数被既有测试钉死)。
    /// 突变「dir_only 误杀目录或整段丢弃 path 候选」时必红。
    #[test]
    fn dir_only_keeps_subdirs_under_path_hit() {
        let mut i = Index::new();
        let docs = i.insert("Documents", NO_PARENT, true);
        let sub = i.insert("archive", docs, true);
        let _ = i.insert("notes.txt", docs, false);
        let mut plan = parse("path:documents").unwrap();
        plan.dir_only = true;
        let out = execute(&i, &plan, 100);
        assert_eq!(out.total, 1, "只剩子目录");
        assert_eq!(out.hits, vec![sub], "子目录存活");
        assert!(i.entry(sub).is_dir(), "存活项必须是目录");
    }

    /// 候选行保持 16B:置顶位与分数共享首个字段。rows 是每查询的命中全集,
    /// 字段膨胀会放大百万命中时的临时分配。
    #[test]
    fn candidate_row_stays_16_bytes() {
        assert_eq!(std::mem::size_of::<(u32, usize, u32)>(), 16);
    }

    fn names(i: &Index, hits: &[u32]) -> Vec<String> {
        hits.iter()
            .map(|&h| String::from_utf8_lossy(i.display_name_of(h)).into_owned())
            .collect()
    }

    /// APP 置顶(spec v1-core「匹配排序」):低分 APP(子串命中)排在所有非 APP
    /// (含高分前缀命中)之前;APP 组内与普通条目仍按打分/名长原序。
    /// 突变「比较子去掉 APP 维度」时必红。
    #[test]
    fn app_bundles_rank_first_in_relevance_order() {
        let mut i = Index::new();
        let d = i.insert("Applications", NO_PARENT, true);
        let _ = i.insert("zeta.app", d, true); // APP,子串命中(score 1)
        let _ = i.insert("alpha.txt", d, false); // 前缀命中(score 3)
        let out = execute(&i, &parse("a").unwrap(), 100);
        // 非 APP 组内:alpha.txt 与 Applications 同前缀分,名长升序(9 < 12)。
        assert_eq!(
            names(&i, &out.hits),
            vec!["zeta.app", "alpha.txt", "Applications"],
            "低分 APP 应置顶"
        );
    }

    /// APP 判定 = 目录 + display 名 `.app` 后缀(ASCII 大小写不敏感):
    /// 大写后缀仍置顶;非目录与普通目录不置顶。
    #[test]
    fn app_ranking_is_case_insensitive_and_dir_only() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("Tool.APP", d, true); // 大写后缀:仍判 APP
        let _ = i.insert("raider", d, true); // 目录但非 .app:不置顶
        let _ = i.insert("fake.app", d, false); // 非目录:不置顶
        let out = execute(&i, &parse("a").unwrap(), 100);
        // 非 APP 组内按打分同名长升序(raider 6 字节 < fake.app 8 字节)。
        assert_eq!(names(&i, &out.hits), vec!["Tool.APP", "raider", "fake.app"]);
    }

    /// `case:` 下候选名是 NFC 原大小写形态:APP 判定读 display 名,不依赖折叠形态。
    #[test]
    fn app_ranking_survives_case_sensitive_query() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("Apps.app", d, true); // case:a 前缀命中(score 3)
        let _ = i.insert("app.txt", d, false); // case:a 前缀命中(score 3),名更短
        let out = execute(&i, &parse("case:a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Apps.app", "app.txt"],
            "case: 下 APP 仍置顶"
        );
    }

    /// top-limit 截断与排序同一比较子:limit=1 保留的应是 APP,不是高分普通条目。
    #[test]
    fn app_first_survives_top_limit_truncation() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let _ = i.insert("zeta.app", d, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 1);
        assert_eq!(names(&i, &out.hits), vec!["zeta.app"]);
    }

    /// 嵌套在另一个 .app 内的 .app 不置顶：外层应用照常置顶，
    /// 包内 Helper 回到普通条目组按打分排序。
    /// 突变「ranks_first 去掉路径祖先判定」时必红。
    #[test]
    fn nested_app_inside_app_does_not_rank_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let contents = i.insert("Contents", outer, true);
        let _ = i.insert("Inner.app", contents, true);
        let _ = i.insert("alpha.txt", d, false); // 前缀命中(score 3)
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Outer.app", "alpha.txt", "Inner.app"],
            "包内 .app 不得置顶"
        );
    }

    /// 祖先为 .framework 的 .app 同样不置顶（判定用全 bundle 扩展名集合）。
    #[test]
    fn app_inside_framework_does_not_rank_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let fw = i.insert("Chromium.framework", d, true);
        let helpers = i.insert("Helpers", fw, true);
        let _ = i.insert("X.app", helpers, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["alpha.txt", "X.app", "Chromium.framework"],
            ".framework 内的 .app 不得置顶"
        );
    }

    /// 祖先名只是包含 .app 子串、并非 bundle 后缀：子 .app 仍置顶。
    #[test]
    fn app_under_non_bundle_lookalike_still_ranks_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let lookalike = i.insert("notes.app.txt", d, true);
        let _ = i.insert("Z.app", lookalike, true);
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Z.app", "notes.app.txt"],
            "祖先不是 bundle 时仍置顶"
        );
    }

    /// 祖先后缀大写同样识别为 bundle。
    #[test]
    fn nested_uppercase_app_does_not_rank_first() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let _ = i.insert("Inner.APP", outer, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Outer.app", "alpha.txt", "Inner.APP"],
            "大写 bundle 后缀的祖先同样生效"
        );
    }

    /// top-limit 截断使用同一比较子：包内 .app 不得挤掉普通高分条目。
    #[test]
    fn nested_app_does_not_survive_top_limit() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let contents = i.insert("Contents", outer, true);
        let _ = i.insert("Inner.app", contents, true);
        let _ = i.insert("alpha.txt", d, false);
        let out = execute(&i, &parse("a").unwrap(), 2);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Outer.app", "alpha.txt"],
            "截断集不得包含包内 .app"
        );
    }

    /// case: 查询下置顶判定读 display 原形态，嵌套判定同样生效。
    #[test]
    fn nested_app_does_not_rank_first_under_case_sensitive_query() {
        let mut i = Index::new();
        let d = i.insert("d", NO_PARENT, true);
        let outer = i.insert("Outer.app", d, true);
        let _ = i.insert("InnerA.app", outer, true); // 含大写 A，子串命中(score 1)
        let _ = i.insert("Another.txt", d, false); // 大写 A 前缀命中(score 3)
        let out = execute(&i, &parse("case:A").unwrap(), 100);
        assert_eq!(
            names(&i, &out.hits),
            vec!["Another.txt", "InnerA.app"],
            "case: 下包内 .app 仍不置顶"
        );
    }

    /// 双段合成视图测试替身(tech.md §4.3 接口改造的核心契约钉):
    /// 同一条目号空间拆成两段连续 Entry 段——S3「Base + Overlay 各自连续,
    /// 合成视图分段产出」的形态预演。名称/路径读取委托源索引(段拆分不动 arena)。
    struct SplitView<'a> {
        src: &'a Index,
        entries: Vec<Entry>,
        split_at: usize,
    }

    impl<'a> SplitView<'a> {
        fn new(src: &'a Index, split_at: usize) -> Self {
            Self {
                src,
                entries: src.entries.clone(),
                split_at,
            }
        }
    }

    impl IndexView for SplitView<'_> {
        fn entry_segments(&self) -> Vec<(u32, &[Entry])> {
            vec![
                (0, &self.entries[..self.split_at]),
                (self.split_at as u32, &self.entries[self.split_at..]),
            ]
        }
        fn entry(&self, idx: u32) -> &Entry {
            &self.entries[idx as usize]
        }
        fn match_name(&self, idx: u32) -> &[u8] {
            <Index as IndexView>::match_name(self.src, idx)
        }
        fn display_name(&self, idx: u32) -> &[u8] {
            <Index as IndexView>::display_name(self.src, idx)
        }
        fn dir_path(&self, idx: u32) -> &[u8] {
            <Index as IndexView>::dir_path(self.src, idx)
        }
    }

    /// 双段视图与单段视图查询逐位一致,且两段都被遍历、段内命中以全局条目号回填。
    /// 突变「execute 只扫首段」→ 第二段命中丢失必红;
    /// 突变「段内下标直报为条目号(不加速度偏移)」→ 第二段命中号错位必红。
    #[test]
    fn two_segment_view_queries_identically_to_single_slice() {
        let mut i = Index::new();
        let d = i.insert("docs", NO_PARENT, true); // 条目 0
        let _ = i.insert("alpha.txt", d, false); // 条目 1(段 1)
        let _ = i.insert("beta.txt", d, false); // 条目 2(段 2)
        let _ = i.insert("gamma.md", d, false); // 条目 3(段 2,不命中 *.txt)
        let full = execute(&i, &parse("*.txt").unwrap(), 10);
        assert_eq!(full.total, 2);

        let split = SplitView::new(&i, 2); // 段界切在第 2 条:两段各含一条命中
        let out = execute(&split, &parse("*.txt").unwrap(), 10);
        assert_eq!(out.total, full.total, "双段视图命中总数一致(两段都遍历)");
        assert_eq!(out.hits, full.hits, "命中条目号为全局号,顺序与单段一致");

        // 段 2 命中条目经全局号可正确取名(arena 委托 + 偏移对位)
        assert_eq!(split.display_name(out.hits[0]), b"beta.txt");
        assert_eq!(split.display_name(out.hits[1]), b"alpha.txt");

        // path: 词同样双段可查:dir_hits 按全局号对齐,段 2 条目的父判定不错位
        let path_full = execute(&i, &parse("path:docs").unwrap(), 10);
        let path_split = execute(&split, &parse("path:docs").unwrap(), 10);
        assert_eq!(path_split.total, path_full.total);
        assert_eq!(path_split.hits, path_full.hits);
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

#[cfg(test)]
mod sort_tests {
    use super::*;
    use crate::entry::NO_PARENT;
    use crate::index::Index;

    fn tree() -> Index {
        let mut i = Index::new();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("b.txt", d, false); // 条目 1
        i.insert("a.txt", d, false); // 条目 2
        i.insert("c.txt", d, false); // 条目 3
        i
    }
    /// 假 meta:条目 1/2/3 → size 30/20/10;mtime 1001/1002/1003。
    fn meta(ids: &[u32]) -> Vec<(Option<u64>, Option<u64>)> {
        let sizes = [30u64, 20, 10];
        ids.iter()
            .map(|&i| (Some(sizes[(i - 1) as usize]), Some(1000 + i as u64)))
            .collect()
    }

    #[test]
    fn sort_by_size_ascending() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let out = execute_sorted(&i, &plan, 10, SortKey::Size, false, &meta);
        let names: Vec<String> = out
            .hits
            .iter()
            .map(|h| String::from_utf8_lossy(i.display_name_of(*h)).into_owned())
            .collect();
        assert_eq!(names, vec!["c.txt", "a.txt", "b.txt"]); // size 10 < 20 < 30(升序)
        assert!(out.stat_ms >= 0.0, "stat_ms 为耗时毫秒,恒非负");
    }

    /// 口径钉(裁决):elapsed_ms 只计匹配 + 键计算 + 排序,不含 meta_of 的
    /// stat 耗时(该耗时独计 stat_ms)。meta 闭包睡 50ms:去掉减法(即回到
    /// 含 stat 的旧口径)时 elapsed_ms ≥ stat_ms,断言必红。
    #[test]
    fn elapsed_ms_excludes_meta_of_time() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let slow = |ids: &[u32]| -> Vec<(Option<u64>, Option<u64>)> {
            std::thread::sleep(std::time::Duration::from_millis(50));
            ids.iter().map(|_| (Some(1), Some(1))).collect()
        };
        let out = execute_sorted(&i, &plan, 10, SortKey::Size, false, &slow);
        assert!(
            out.stat_ms >= 40.0,
            "stat_ms 独计 meta_of 耗时(≥ 睡的 50ms):{}",
            out.stat_ms
        );
        assert!(
            out.elapsed_ms < out.stat_ms,
            "elapsed_ms({}) 不得含 stat({})",
            out.elapsed_ms,
            out.stat_ms
        );
    }

    /// 等值键按条目号决胜(确定性,同 v1 约定)。
    /// 前提钉:relevance 序为 [y, x](名短在前),与条目号序相反——删掉决胜
    /// 比较(`.then(a.cmp(b))`)时稳定排序保持 [y, x],本用例必红。
    #[test]
    fn equal_keys_tie_break_by_entry_id() {
        let mut i = Index::new();
        let d = i.insert("docs", NO_PARENT, true);
        let x = i.insert("xx.txt", d, false); // 条目号小;名长 → relevance 里排后
        let y = i.insert("y.txt", d, false); // 条目号大;名短 → relevance 里排前
        let plan = parse("*.txt").unwrap();
        assert_eq!(
            execute(&i, &plan, 10).hits,
            vec![y, x],
            "前提:relevance 序与条目号序相反"
        );
        // 同 size、同 mtime
        let same = |ids: &[u32]| -> Vec<(Option<u64>, Option<u64>)> {
            ids.iter().map(|_| (Some(100), Some(5))).collect()
        };
        let out = execute_sorted(&i, &plan, 10, SortKey::Size, false, &same);
        assert_eq!(out.hits, vec![x, y], "等值键按条目号决胜(确定性)");
        let out_m = execute_sorted(&i, &plan, 10, SortKey::Mtime, false, &same);
        assert_eq!(out_m.hits, vec![x, y], "mtime 等值同样按条目号决胜");
    }

    #[test]
    fn sort_by_mtime_is_descending() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let out = execute_sorted(&i, &plan, 2, SortKey::Mtime, false, &meta);
        assert_eq!(out.hits.len(), 2, "limit 截断");
        assert_eq!(out.total, 3, "total 为截断前全量");
        // 定案:mtime 降序(最新在前)——c.txt(1003) > a.txt(1002) > b.txt(1001)
        assert_eq!(i.display_name_of(out.hits[0]), b"c.txt");
        assert_eq!(i.display_name_of(out.hits[1]), b"a.txt");
    }

    #[test]
    fn sort_by_name_never_calls_meta() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let no_meta =
            |_: &[u32]| -> Vec<(Option<u64>, Option<u64>)> { panic!("Name 排序不得调 meta") };
        let out = execute_sorted(&i, &plan, 10, SortKey::Name, false, &no_meta);
        let names: Vec<Vec<u8>> = out
            .hits
            .iter()
            .map(|h| i.display_name_of(*h).to_vec())
            .collect();
        assert_eq!(
            names,
            vec![b"a.txt".to_vec(), b"b.txt".to_vec(), b"c.txt".to_vec()]
        );
    }

    #[test]
    fn sort_by_path_works() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let no_meta =
            |_: &[u32]| -> Vec<(Option<u64>, Option<u64>)> { panic!("Path 排序不得调 meta") };
        let out = execute_sorted(&i, &plan, 10, SortKey::Path, false, &no_meta);
        // 同一目录下,路径序 == 名称序(前缀相同)
        let names: Vec<Vec<u8>> = out
            .hits
            .iter()
            .map(|h| i.display_name_of(*h).to_vec())
            .collect();
        assert_eq!(
            names,
            vec![b"a.txt".to_vec(), b"b.txt".to_vec(), b"c.txt".to_vec()]
        );
    }

    #[test]
    fn relevance_delegates_to_execute() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let base = execute(&i, &plan, 10);
        let no_meta =
            |_: &[u32]| -> Vec<(Option<u64>, Option<u64>)> { panic!("Relevance 不得调 meta") };
        let sorted = execute_sorted(&i, &plan, 10, SortKey::Relevance, false, &no_meta);
        assert_eq!(base.hits, sorted.hits);
        assert_eq!(base.total, sorted.total);
    }

    /// 方向语义核心钉(修复误差本体):desc=true 取的是**全量命中里真最大的 K 条**,
    /// 不是「升序前 K 条反转」。5 条命中 limit=2:desc=false 取最小 2、desc=true 取
    /// 最大 2;若实现回退为「先前 K 再反转」,desc=true 会得最小两条的逆序(d.txt→a.txt),
    /// 断言必红。
    #[test]
    fn desc_true_takes_top_k_before_truncation() {
        let mut i = Index::new();
        let d = i.insert("docs", NO_PARENT, true);
        i.insert("a.txt", d, false); // 条目 1 → 10
        i.insert("b.txt", d, false); // 条目 2 → 50
        i.insert("c.txt", d, false); // 条目 3 → 30
        i.insert("d.txt", d, false); // 条目 4 → 20
        i.insert("e.txt", d, false); // 条目 5 → 40
        let sizes = [10u64, 50, 30, 20, 40];
        let meta = |ids: &[u32]| -> Vec<(Option<u64>, Option<u64>)> {
            ids.iter()
                .map(|&i| (Some(sizes[(i - 1) as usize]), Some(1000 + i as u64)))
                .collect()
        };
        let plan = parse("*.txt").unwrap();
        let names = |out: &SortOutcome| -> Vec<String> {
            out.hits
                .iter()
                .map(|h| String::from_utf8_lossy(i.display_name_of(*h)).into_owned())
                .collect()
        };
        let asc = execute_sorted(&i, &plan, 2, SortKey::Size, false, &meta);
        assert_eq!(asc.hits.len(), 2, "K=2 截断");
        assert_eq!(asc.total, 5, "total 为截断前全量");
        assert_eq!(names(&asc), vec!["a.txt", "d.txt"], "自然方向 = 最小的两条");
        let desc = execute_sorted(&i, &plan, 2, SortKey::Size, true, &meta);
        assert_eq!(desc.hits.len(), 2, "K=2 截断");
        assert_eq!(desc.total, 5, "total 为截断前全量");
        assert_eq!(
            names(&desc),
            vec!["b.txt", "e.txt"],
            "desc=true = 全库最大的两条(截断前排序;先前 K 再反转会得 d/a)"
        );
    }

    /// desc = 反转自然方向(非绝对升/降):mtime 自然方向为降序,desc=true 即升序
    /// (最旧在前);desc=false 与 19b 前行为逐位一致。
    #[test]
    fn desc_true_reverses_mtime_natural_direction() {
        let i = tree(); // 条目 1/2/3 = b/a/c → mtime 1001/1002/1003
        let plan = parse("*.txt").unwrap();
        let natural = execute_sorted(&i, &plan, 10, SortKey::Mtime, false, &meta);
        assert_eq!(
            natural
                .hits
                .iter()
                .map(|h| i.display_name_of(*h))
                .collect::<Vec<_>>(),
            vec![b"c.txt", b"a.txt", b"b.txt"],
            "自然方向 = 最新在前(降)"
        );
        let reversed = execute_sorted(&i, &plan, 10, SortKey::Mtime, true, &meta);
        assert_eq!(
            reversed
                .hits
                .iter()
                .map(|h| i.display_name_of(*h))
                .collect::<Vec<_>>(),
            vec![b"b.txt", b"a.txt", b"c.txt"],
            "desc=true = 升序(最旧在前)"
        );
    }

    /// Name/Path 的 desc=true 反转字面序(Path 同目录下与 Name 同序)。
    #[test]
    fn desc_true_reverses_name_and_path() {
        let i = tree(); // 自然 a < b < c
        let plan = parse("*.txt").unwrap();
        let no_meta = |_: &[u32]| -> Vec<(Option<u64>, Option<u64>)> {
            panic!("Name/Path 排序不得调 meta")
        };
        let name_desc = execute_sorted(&i, &plan, 10, SortKey::Name, true, &no_meta);
        assert_eq!(
            name_desc
                .hits
                .iter()
                .map(|h| i.display_name_of(*h))
                .collect::<Vec<_>>(),
            vec![b"c.txt", b"b.txt", b"a.txt"]
        );
        let path_desc = execute_sorted(&i, &plan, 10, SortKey::Path, true, &no_meta);
        assert_eq!(
            path_desc
                .hits
                .iter()
                .map(|h| i.display_name_of(*h))
                .collect::<Vec<_>>(),
            vec![b"c.txt", b"b.txt", b"a.txt"]
        );
    }

    /// Relevance 无方向概念:desc 被忽略,结果与 execute 逐位一致(委托回归钉)。
    #[test]
    fn relevance_ignores_desc() {
        let i = tree();
        let plan = parse("*.txt").unwrap();
        let base = execute(&i, &plan, 10);
        let no_meta =
            |_: &[u32]| -> Vec<(Option<u64>, Option<u64>)> { panic!("Relevance 不得调 meta") };
        let sorted = execute_sorted(&i, &plan, 10, SortKey::Relevance, true, &no_meta);
        assert_eq!(base.hits, sorted.hits, "desc=true 不得改变 relevance 序");
        assert_eq!(base.total, sorted.total);
    }
}

# 拼音搜索（全拼）技术规格

- 日期：2026-11-15
- 状态：已实现（2026-11-15）
- 产品契约：`product.md`
- 设计依据：`docs/superpowers/specs/2026-11-15-pinyin-search-design.md`

## 1. 模块划分与集成点

### 1.1 `crates/engine/src/pinyin.rs`（全部拼音逻辑内聚）

| 项 | 形态 | 职责 |
|---|---|---|
| `readings(c: char) -> Vec<String>` | 内部 | 单字归一化读音集：去声调、`ü`→`v`、去重保序 |
| `CJK_FIRST` / `CJK_LAST` | 常量 `0x4E00` / `0x9FFF` | 字符域（CJK 基本区） |
| `ReverseTable` | 内部 struct | `prefixes: HashMap<Box<str>, Vec<char>>`（读音前缀→升序去重字符）+ `syllables: BTreeSet<Box<str>>`（完整音节集） |
| `TABLE: OnceLock<ReverseTable>` + `table()` | 静态 | 进程内懒构建；rayon 并发首调由 `OnceLock` 保证单次 |
| `segment(word) -> Vec<Vec<&str>>` + `enumerate` | 内部 | 全枚举音节切分，`MAX_PARSES = 8` |
| `CharBitmap { words: Box<[u64]> }` | 内部 | CJK 基本区位图；`from_chars` / `contains`，2.6KB/集 |
| `PinyinMatcher { alts: Vec<Vec<CharBitmap>> }` | pub | 一个查询词的全部可行切分方案（每方案一组字符集位图） |
| `PinyinMatcher::compile(word: &[u8]) -> Option<Self>` | pub | 词级条件 + 切分 + 展开；`None` = 不展开（静默回退字面） |
| `PinyinMatcher::hit(&self, name: &[u8]) -> Option<usize>` | pub | 序列匹配，返回命中起点字节位 |
| `seq_matches(hay, alt)` | 内部 | 从一个起点逐字符匹配一组位图；序列中断即失败 |

- 实现 API 为 `PinyinMatcher::{compile, hit}`，序列匹配内部函数为 `seq_matches`。
- 位图每次查询按需从反查表切片构建（每音节微秒级）；反查表常驻。

### 1.2 `query.rs` 集成点

| 位置 | 变更 |
|---|---|
| `Pattern` | 新增 `pub wildcard: bool`：词含 `*`/`?`。由 `compile_pattern` 用 `has_wildcard` 填充 |
| `CPattern` | 新增 `pinyin: Option<PinyinMatcher>` |
| `CPattern::compile` | 签名改为 `compile(p: &Pattern, pinyin_ok: bool)`；拼音仅在 `pinyin_ok && !p.wildcard && p.parts.len() == 1` 时编译 |
| `CPattern::hit` | 新增：`match_pattern(name, self) \|\| self.pinyin.as_ref().is_some_and(\|m\| m.hit(name).is_some())` |
| `execute` 编译入口 | `pinyin_ok = !plan.case_sensitive && !plan.whole_word`（整轮开关）；`name_terms` 按它编译，`path_terms` 恒 `false` |
| 候选过滤 | `c_name.iter().all(\|pt\| pt.hit(&target))`（原为 `match_pattern` 直调） |
| `score_of` | 字面档 < 3 且 `term.pinyin.hit(target) == Some(0)` → 升到 3；中部命中不改变档位（保持字面 max 语义） |

**`wildcard` 字段的由来（必须保留）**：`*shu*` 编译时 `*` 被剥掉，`parts` 与纯字面词
`shu` 完全一致（`[b"shu"]`），仅凭 `parts` 形态无法区分通配词；拼音展开必须由解析侧
标注排除，否则 `*shu*` 会意外获得拼音召回。

`p.parts.len() == 1` 是防御性判断：只有 `*` 会拆出多段，`?` 以 QMARK 留在段内；
无通配符的词恒为单段，通配词已由 `wildcard` 排除。

```rust
pub struct Pattern {
    pub parts: Vec<Vec<u8>>,
    pub prefix_anchor: bool,
    pub suffix_anchor: bool,
    /// 词含 `*`/`?`。`*` 在编译时被剥掉，`*shu*` 的 parts 与纯字面词相同，
    /// 拼音展开必须靠此标志排除通配词。
    pub wildcard: bool,
}

struct CPattern {
    parts: Vec<CPart>,
    prefix_anchor: bool,
    suffix_anchor: bool,
    /// 拼音匹配分支（纯字母词）；`case:`/`whole:`/通配符/`path:` 词不编译。
    pinyin: Option<PinyinMatcher>,
}
```

- `Pattern` 是 `pub` 契约（对外结构），`CPattern` 是 `execute` 入口的内部预编译形态；
  拼音只在后者挂载，不改公开 `Pattern` 的匹配语义。
- `whole:` 的词边界判定（`all_parts_word_bounded`）不改：整轮 `whole:` 下拼音分支不存在。
- `path:` 词以 `pinyin_ok = false` 编译；唯一编译入口在 `execute`（`execute_sorted`
  委托 `execute`，无需改动）。
- 拼音匹配的输入 `target` 即字面匹配的同一份数据（折叠路径下为 `view.match_name(idx)`）：
  CJK 字节不受小写折叠影响，无需二次取名字。
- 打分对已命中候选二次调用 `hit`（只发生在通过匹配的条目上）；成本随命中集大小，
  已纳入 bench P99 口径。

## 2. 算法

### 2.1 读音归一（`readings`）

```
readings(c):
  multi = c.to_pinyin_multi()；无读音 → 空
  对每个 Pinyin p:
    r = p.plain().replace('ü', "v")
    去重保序收集
```

- `plain()` 已去声调并小写化。
- 实测上游对 j/q/x/y 后的 ü 已写作 `u`（居 ju、去 qu、学 xue、鱼 yu）；只有 `lü`/`nü`
  保留 `ü`（绿 lü、女 nü）。`replace` 统一转 `v`，与主流 IME 输入一致（`lv`/`nv`）。
- 多读音全部保留：`长` → zhang/chang，`行` → xing/hang/heng，`数` → shu/shuo。
- 上游原始读音条目上限 11（`欸`）；按字符做归一化去重（重复读音、同音形态）后
  上限 8（`擖`：ka/qia/jia/zha/gua/ye/ge/lie）。

### 2.2 反查表构建（`ReverseTable::build`，`OnceLock` 懒构建）

```
for cp in 0x4E00..=0x9FFF:
  c = char::from_u32(cp)
  for reading in readings(c):
    syllables.insert(reading)                  // 完整音节集由读音数据派生
    for 每个字符前缀 p of reading:              // 长度 1..=len(reading)
      prefixes[p].push(c)
构建后逐前缀 sort_unstable + dedup
```

- 完整音节集不手写：由读音数据派生，与字符集天然一致；`has_syllable` 直接查它。
- 单字多读音会把同一字符重复登记到同一前缀，构建后统一去重。
- 首次出现纯字母查询词（可含 `'`）时构建一次，纯内存计算，无 IO、无失败面；常驻约 400KB（shrink 后）。
- `chars_for(prefix)` 返回前缀字符切片；`None` = 该前缀不存在（切分失败路径之一）。

### 2.3 切分（`segment` / `enumerate`）

- `word.split('\'')` 切出若干 run，各 run 的可行切分做笛卡尔拼接；`'` 是强制分段边界：
  边界两侧各自单独切分，边界内仍全枚举，没有任何方案跨过边界（`xi'an` 至少产出
  `[xi][an]`，另有 `[xi][a][n]`——`a`、`n` 都是完整音节）。空 run 跳过。
- 每个 run 递归枚举，**完整音节全枚举、最长音节优先**：
  - 位置 `pos` 处，若剩余串是某读音前缀、且本身不是完整音节 → 以该前缀作末段产出一条
    切分（末尾未打完，如 `shux` 的 `[shu][x]`；`sh` 的 `[sh]`）。
  - 再枚举 `run[pos..end]` 为完整音节的全部 `end`（从长到短）并递归。
- 去重来源：剩余串本身是完整音节时只走「完整音节」分支，避免同一解析既作末段前缀又
  作完整音节重复产出。
- 任一 run 无可行切分 → 整个词不展开（返回空）。
- 方案数上限 `MAX_PARSES = 8`，按最长优先顺序截断，防御组合爆炸
  （`xianxianxianxian` 之类的多解词）。
- 输入防护：`segment` 只接受 ASCII 且 ≤64 字节的词（`MAX_WORD_LEN`）；整词切分搜索步数
  上限 10000（`MAX_SEARCH_STEPS`，跨 `'` 段共享），超限立即整体放弃展开。任一触发回退
  纯字面，不报错。
- 产物是音节子串序列；`PinyinMatcher::compile` 再把每段换成 `CharBitmap`。

### 2.4 匹配（`PinyinMatcher::hit` / `seq_matches`）

- `compile` 的词级条件：合法 UTF-8、只含 ASCII 小写字母与 `'`、至少一个字母；随后
  `segment` 有解才返回 `Some`，否则 `None`。小写与 NFC 由 `query.rs` 的
  `compile_pattern` 归一保证，compile 侧仍防御性校验字节集合。整轮条件（`case:`/
  `whole:`）与通配符、`path:` 排除由 `query.rs` 负责，pinyin 模块不感知这些语法。
- `hit`：`name.is_ascii()` 直接 `None`（全 ASCII 名 O(n) 扫描后直接判不命中，无位图测试）；否则按 `char_indices`
  逐字符起点扫描，每个起点依次试全部切分方案，首个命中即返回该起点字节位。
- `seq_matches` 从起点逐字符推进：第 i 字须 ∈ 第 i 段位图；中途任一字不在位图
  （非 CJK、标点、另一音节的字）即序列中断，换下一起点。
- 复杂度：`O(名字字符数 × 方案数 × 音节数)`，全 ASCII 名 O(n) 快速拒绝；
  位图 membership 为 O(1)。

### 2.5 降级（全部静默，无用户可见错误）

| 情形 | 行为 |
|---|---|
| 切分失败 | 纯字面匹配 |
| `case:` / `whole:` / 通配符 / 含数字或符号 / `path:` / 混合词 | 不展开 |
| 条目名称含非汉字字符 | 天然不命中，不误伤（序列中断） |
| 反查表构建 | 纯内存计算，无 IO 无失败面 |

## 3. 数据事实（`pinyin 0.11.0` 实测，供后来者核对）

测量方式与本实现的 `readings` 完全一致（`to_pinyin_multi` → `plain()` → `ü`→`v` →
按字符去重），遍历 U+4E00–U+9FFF：

| 项 | 值 | 说明 |
|---|---|---|
| 读音数据覆盖 | 20924 / 20992 字 | 68 字无读音（生僻/兼容形态） |
| 完整音节（ASCII） | 419 | 另有非 ASCII 读音 `ê`；合计 420。`ê` 不可能出现在 ASCII 查询词中 |
| 反查前缀 | 501 | 含 `ê`；ASCII 前缀 500。前缀长度 1..=6 字节 |
| 前缀-字符对 | 81926 | 每前缀去重后的字符总数 |
| 单字读音上限 | 原始 11 / 去重后 8 | 原始条目最多为 `欸`（11）；按字符归一化去重后最多为 `擖`（8） |
| 最长读音 | 6 字节 | shuang / chuang / zhuang |
| 原始读音含 `ü` | 123 条 | 集中在 lü/nü 系；j/q/x/y 后已被上游写作 `u` |

- 上游 API：`pinyin::ToPinyinMulti::to_pinyin_multi()` 返回 `Option<PinyinMulti>`
  （可迭代，多读音来自 `heteronym` 特性）；`Pinyin::plain()` 返回去声调的小写
  `&'static str`。默认特性已含 `plain` 与 `heteronym`，无需改特性开关。
- 音节表由读音数据派生（419 个 ASCII 音节），比教科书「410 个标准音节」多出
  den/tei/fiao/zhei 等数据中实际出现的读音，以及 m/n/ng/hm/hng/lo/yo 等叹词音节；
  这是刻意的：切分表与字符集必须取自同一份数据，否则会出现「能切分却查不到字」的空集。

## 4. 性能预算与内存（设计文档 §5）

- 编译期：切分 + 位图构建，单查询词微秒级，`execute` 一次。
- 匹配期：
  - 全 ASCII 条目名称：`is_ascii` 扫描后直接返回不命中（O(n) 快速拒绝，无位图测试）。
  - 含中文名：位图 membership 测试。1M 条目、CJK 名占比 20%、平均 15 字符、2 音节
    ≈ 600 万次位测试，rayon 并行下增量 <1ms。
  - 最坏（全 CJK、6 音节）为估算上限（并行 <10ms）；bench 实测覆盖 20% 与 100%
    中文名树、1–2 音节查询。
- 内存：索引内存布局零改动；查询侧反查表懒构建、常驻 ~400KB（shrink 后，首次出现
  纯字母查询词时触发构建，可展开与否均在 compile 入口取表）；查询期位图瞬态常规
  5–30KB，按段数计的真实上限 = 8 方案 × 每方案最多 64 音节 × 2.6KB ≈ 1.3MB
  （对抗性长词），随查询结束释放。
- 落盘：零变化。

派生核对：

- 单个 `CharBitmap` = 20992 位 / 64 × 8B = 2.6KB。
- 反查表主体 = 81926 个 `char`（4B）+ 每前缀 Vec 与键的开销 + 419 个音节键，
  与 ~400KB 同量级。
- 常规查询词 1–3 方案、2–4 音节，位图瞬态约 5–30KB；极端上界见预算条。

验收口径：`scripts/bench.sh` 增加拼音查询基准项（20% 中文名树与
`MCE_GEN_CN_PCT=100` 的 100% 中文名树各一次），常规 20% 树拼音 P99 < 10ms 方可通过；100% 极端树记录实测（中位 9.6ms / P99 13.7ms，复跑中位 11.5ms，不作红线）；
100% 中文名树上另记录 `query sh` 的单次匹配耗时（评分对每个命中二次调用
`PinyinMatcher::hit`）。任务与门禁见 `plan.md`。

### 测量结果（Task 7 基准，1M 条目）

来源：1M 条目合成树；daemon 合并了已挂载外置卷，实际条目约 1,030,134，其中 5,083 条
来自外置卷（既有行为）。

| 场景 | 首扫 | ASCII 查询 P99 | 拼音中位 / P99 | 备注 |
|---|---|---|---|---|
| 常规树（20% 中文名） | 0.70 s | 2.2 ms | 3.7 / 4.0 ms | `query shuxue` 25,000 命中；首个拼音查询 9.3 ms（反查表懒构建，基准预热已排除） |
| 极端树（100% 中文名） | 0.96 s | 2.9 ms | 9.6 / 13.7 ms（复跑 11.5/11.6） | `shuxue` 10.4/11.6 ms；`shux` 8.9/9.2 ms；`query sh` 375,108 命中、8.3–8.8 ms；人工极端分布，记录不设红线 |

## 5. 依赖

workspace 与 engine 各加一行，无其他新增：

```toml
# Cargo.toml [workspace.dependencies]
pinyin = "0.11"

# crates/engine/Cargo.toml [dependencies]
pinyin.workspace = true
```

- rust-pinyin 是纯数据静态表，零传递依赖、无 IO、无网络。
- 风险预案（设计 §4.4）：若上游多读音 API 无法支撑反查表，降级为仓库内生成表
  （Unihan 离线生成 + checkin 二进制，项目已有 vendor 先例）；实测 API 可用，
  预案不启用。

## 6. 测试策略

- **`pinyin.rs` 单元**
  - 读音归一：去声调、`ü`→`v`、多音字（数/长/行）。
  - 反查表：`shu` 集含数/书/树；多音字入每个读音集；前缀集；完整音节集派生。
  - 切分：全拼音、末段前缀、全枚举（完整音节全枚举、最长优先）、`'` 强制边界且
    边界内全枚举、无解回退、8 方案截断。
  - 匹配：名称开头/中部；全 ASCII 不匹配；日文假名不误伤；非汉字中断；多音字双命中；
    多切分 OR；前缀集召回（水/双/说）；不合格词不编译。
- **`query.rs` 集成**：拼音命中中文名且前缀先排；`case:`/`whole:`/`path:`/`*shu*`
  不泄漏；`Shu` 折叠后仍展开；目录名命中；字面与拼音共存及档位排序。
- **突变验证**（项目纪律，Task 3/4/5/6 各一条）
  - 反查表 `reading.chars().skip(1)`（去掉单字母前缀）→
    `prefix_sets_cover_partial_syllables` 变红。
  - 切分末段条件改坏 → `segments_full_pinyin` 变红。
  - `CharBitmap::contains` 恒真 → `japanese_kana_never_match` 与
    `non_han_chars_break_the_sequence` 变红。
  - `CPattern::hit` 去掉拼音分支 → 拼音集成测试变红。
- **bench**：20% 中文名树与 100% 中文名树（`MCE_GEN_CN_PCT=100`，不同 ROOT）各一次；
  `shuxue`/`shux` 交替 200 次，常规树 P99 < 10ms；100% 树上记录实测与 `query sh` 的单次匹配耗时
  （评分对每个命中二次调用 `PinyinMatcher::hit`）。
- **parity**：索引零改动，天然通过；不新增用例。
- **实机**：非门禁；隔离数据目录 `start → query shu / shuxue → stop` 各一条。

新增测试一律做突变验证，防止「恒绿假钉」（见 `docs/development.md`）。

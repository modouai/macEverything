# 拼音搜索（全拼）实施计划

- 日期：2026-11-15
- 状态：待实现
- 关联：`product.md` / `tech.md`
- 权威实施计划：`docs/superpowers/plans/2026-11-15-pinyin-search.md`（逐步步骤与代码模板以它为准；
  本文只记切片、门禁、验收口径与文档同步清单）

## 任务切片

| Task | 标题 | 主要交付物 | 完成判据 |
|---|---|---|---|
| 1 | 规格档案（spec-first） | `docs/specs/pinyin-search/{product,tech,plan}.md` | 三份契约先于代码合入 |
| 2 | 依赖与读音归一 | `Cargo.toml`、`crates/engine/Cargo.toml`、`engine/src/{lib,pinyin}.rs` | `readings` 3 条单测通过（多音字全保留、覆盖率 > 20000） |
| 3 | 反查表与音节表 | `engine/src/pinyin.rs` | 前缀/多音字/音节集 4 条单测；突变（去掉单字母前缀）必红 |
| 4 | 音节切分 | `engine/src/pinyin.rs` | 6 条切分单测（全拼音、全枚举、末段前缀、`'` 强制边界且边界内全枚举、无解、8 方案上限）；突变（末段判定）必红 |
| 5 | 位图与序列匹配器 | `engine/src/pinyin.rs` | 9 条匹配单测（开头/中部、ASCII 短路、末段输入中、假名不误伤、序列中断、多音字、多切分 OR、前缀集召回、不合格词）；突变（`contains` 恒真）必红 |
| 6 | query.rs 接入 | `engine/src/query.rs` | 5 条集成测试（拼音排序、`case:`/`whole:`/`path:`/通配符互斥、`Shu` 折叠、目录名命中、字面与拼音共存）；突变（去掉拼音分支）必红 |
| 7 | 基准（合成树中文名 + 拼音 P99） | `scripts/spike-readdir/src/main.rs`、`scripts/bench.sh` | 拼音 P99 < 10ms（§2） |
| 8 | 文档同步与三门禁终检 | 见 §3；`plan.md` 状态回填 | 三门禁全绿 + 实机冒烟通过 |

Task 编号与 superpowers 计划一一对应，不另起编号。

## 1. 三门禁（声明完成前必须全绿）

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- 每个 Task 合入前先跑与本 Task 相关的测试；Task 8 是最终全量门禁。
- 新增测试须做突变验证（临时改坏实现，确认测试变红），Task 3/4/5/6 各一条。
- 「逐键输入结果连续、不闪烁」为设计目标 4 的手动 GUI 核对项，不作自动门禁。
- 改动只落在 engine（新增 `pinyin` 模块、`query` 接入、新依赖）与 scripts/docs；
  daemon/增量逻辑未动，实机冒烟为手动项、非门禁。

## 2. bench 验收口径

```bash
cargo build --release
bash scripts/bench.sh /tmp/mce-bench 1000000                        # 常规树:20% 中文名
MCE_GEN_CN_PCT=100 bash scripts/bench.sh /tmp/mce-bench-cn 1000000  # 极端树:100% 中文名(不同 ROOT)
```

判定：

- **拼音 P99 < 10ms**（`bench.sh` 新增「2b. 拼音查询 P99」段）：200 次 `shuxue`/`shux`
  交替查询；反查表 `OnceLock` 懒构建计入预热、不占 P99 样本。两棵树各跑一次。
- 常规树「2. 查询 P99」按量级与历史值比较：合成树 20% 文件名换成中文后，约 20% 的
  随机 ASCII 关键词命中 0 项，P99 可能略降；不按绝对值逐位比较。
- 合成树中文名占比由 `MCE_GEN_CN_PCT` 控制，默认 20；0 = 纯 ASCII（旧基线）。
- `shuxue` 命中量约 2.5 万（1M × 20% × 1/8）；若为 0，先确认合成树含中文名。
- 100% 中文名树另记录 `query sh` 的单次匹配耗时（命中约 37.5 万：8 组中文名里
  「数学笔记」「季度报告书」「数据分析」含 `sh` 前缀字，占 3/8；评分对每个命中二次
  调用 `PinyinMatcher::hit`）；与两次 2b P99、常规树 ASCII P99 一并在 Task 8 记入
  `tech.md` §4。
- 超红线处理：先确认全 ASCII 名称被 `is_ascii` 短路，再带实测数字停下分析，
  不直接改预算。

bench 脚本头部与 `docs/development.md` 的「五项基准」描述同步改为六项
（首扫 / 查询 P99 / 拼音 P99 / 增量 / 落盘 / 停机重放）。

## 3. 文档同步清单（Task 8）

| 文件 | 改动 |
|---|---|
| `docs/usage.md` | 「搜索语法」新增「拼音匹配」：默认开启；纯字母词按全拼音节匹配中文条目名称（文件与文件夹；`shu` → 数/书/树，`shuxue` → 数学，`shux` 支持未打完的末音节）；`case:`/`whole:`/含通配符的词、`path:` 词不展开；`'` 作强制分段边界（边界内仍全枚举，`xi'an` 至少匹配西安）；`ü` 用 `v` 输入（`lv` → 绿） |
| `docs/architecture.md` | 查询管线段落补一句：查询侧拼音展开（`pinyin.rs` + `CPattern.pinyin`），索引与落盘无变化 |
| `docs/development.md` | 基准项「五项」→「六项」（含拼音查询）；补 `MCE_GEN_CN_PCT`（默认 20）控制合成树中文名占比 |
| `CLAUDE.md` | 常用命令 bench 行「（五项基准，自动隔离环境）」→「（六项基准，自动隔离环境）」 |
| `docs/specs/pinyin-search/tech.md` | Task 8 回填 §4 测量结果：两次基准（20% / 100% 中文名树）的 2b 拼音 P99、`query sh` 单次匹配耗时、常规树 ASCII P99 |
| `docs/specs/pinyin-search/plan.md` | 实现完成后标记状态、日期与最终门禁结果 |

文档同步时机按 `docs/development.md`「规格与文档」表：行为契约变更先写本规格，
实现合入时同步 usage / architecture / development / CLAUDE.md。

## 4. 验收口径

验收矩阵以实现计划 `docs/superpowers/plans/2026-11-15-pinyin-search.md` 的《验收清单
（对照设计文档）》为准；本文件只保留门禁（§1）、bench（§2）与文档同步（§3）。

## 5. 完成判定

- [ ] 三门禁全绿
- [ ] 常规树（20% 中文名）与极端树（`MCE_GEN_CN_PCT=100`）的拼音 P99 均 < 10ms；
      常规树 ASCII P99 与历史同量级（按量级比较）
- [ ] 100% 中文名树 `query sh` 单次匹配耗时已记录，并与其他测量一并回填 `tech.md` §4
- [ ] 实机冒烟：`query shu` 命中 `shu.txt` 与两个中文名（中文前缀排前）；
      `query shuxue` 命中「数学笔记」
- [ ] 逐键输入连续性经手动 GUI 核对（设计目标 4，非自动门禁）
- [ ] §3 文档同步完成
- [ ] 本节与文首状态回填实现日期与最终门禁结果

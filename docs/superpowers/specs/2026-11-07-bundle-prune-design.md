# 包内剪枝（bundle prune）设计

- 日期：2026-11-07（2026-11-07 复核修订）
- 状态：已确认，排在嵌套 App 排序修复（`2026-11-07-nested-app-rank-design.md`）之后实施
- 范围：engine 扫描/增量/持久化 + cli daemon 增量入口 + 配置；查询排序与 GUI 无改动
- 功能代号：bundle-prune

## 1. 背景与立论

索引语义目前是「磁盘上每个非符号链接条目一条」。`.app` 是目录，扫描器整棵遍历进去，包内的 Framework、Helper、嵌套子应用全部成为条目。

首屏污染的直接成因是排序把嵌套 `.app` 一并置顶，已由排序修复独立解决（O3）。剪枝的立论不建立在「首屏被污染」或「包内条目无搜索价值」这类判断上——后者不可验证，容易在评审中被顶回来。

**立论：索引粒度 = Finder 可见单位。**

product.md 问题陈述第 1 条把产品基线定为「Finder 里看得到的文件，Spotlight 搜不到」。Finder 中 bundle 是单个不可进入的项目：双击打开整个应用，不能浏览其内部。包内的 `Contents/`、`Frameworks/`、`MacOS/` 不是「Finder 里看得到的文件」，是应用的实现细节。把它们逐条索引进搜索面，等于把索引粒度定得比产品基线更细。

按这条立论，剪枝不是对搜索内容的取舍，而是把索引粒度校准到与 Finder 一致。它同时解决两件事：

- **体积与首扫**：包内条目通常占 `/Applications` 条目数的绝大部分，剪掉直接减少条目数、首扫时间与常驻内存。这是这套产品少有的同时改善「快」和内存的改动。
- **规则可解释**：判定原则是「Finder 双击打开整体、不进入」，而不是一份凭经验拟定的扩展名清单。

现有证据（2026-11-07 实机确认）：

```
/Applications/WPS Office.app/Contents/SharedSupport/kaideployserver.app
/Applications/WPS Office.app/Contents/SharedSupport/wpsbackend.app/Contents/SharedSupport/kaideployserver.app
```

两条是**不同路径**（顶层一份 + `wpsbackend.app` 内一份），不是重复条目。§1 初稿写的「出现两次」是同一产品两份拷贝，排除重复条目缺陷。

## 2. 方案谱系

| 方案 | 首屏污染 | 召回损失 | 索引体积/首扫 | 规则可解释性 |
|---|---|---|---|---|
| O3 只修排序（置顶只对非包内 `.app` 生效） | 解决 | 无 | 无收益 | 好 |
| O2 只剪「包中包」 | 解决 | 小 | 几乎无收益（包内文件大头在顶层 app 的 `Contents`） | 差（「包能进、包中包不行」） |
| O1 全局剪枝（本设计） | 解决 | 大 | 有收益 | 好 |

O3 先行且独立提交（见排序设计），解决首屏症状且零召回损失。O1 在 O3 之后按「粒度一致 + 体积收益」实施。O2 不单独实施：它同时丢失 O1 的体积收益和 O3 的语义清晰度。

**O1 与 O3 是叠加关系，不是替代。** O1 实施后，O3 仍然有效且必要：开关打开时索引恢复全量，排序修复保证包内 `.app` 不进置顶组。设计初稿写的「开关打开时完全恢复旧行为」不成立——那是恢复已知的排序缺陷。开关打开的正确语义是「恢复全量索引，排序仍不置顶包内 `.app`」。

## 3. 目标与非目标

### 目标

1. bundle 目录本身仍作为条目收录（可被搜到、可被列目录、可执行「打开」）。
2. bundle 目录内部不产生索引条目：首扫不进入，增量事件不追加。
3. 默认生效；配置项 `index_bundle_contents` 可恢复全量索引。
4. 所有写入路径共用同一判定：首扫（主根与卷扫描）、增量 diff、补链。
5. 旧索引在升级后自动重建，不残留包内条目。
6. 剪枝收益可量化：bench 受控树记录条目数、首扫时间、常驻内存的前后差。

### 非目标

- 不改变查询排序、结果上限、搜索语法。
- 本期不新增 GUI 开关（设置页「索引」标签只加常驻说明，开关排入后续迭代）。
- 不按 `Contents/Info.plist` 探测（需额外出 stat，首扫成本上升）。
- 不处理 `.xcworkspace` / `.xcodeproj` 这类「看起来像包、实际是用户工程目录」的扩展名（见 §4.1 清单）。

## 4. 行为契约

### 4.1 判定规则

路径的**任一组件**以 `BUNDLE_EXTENSIONS` 中某项结尾（ASCII 大小写不敏感），即视为 bundle 路径。

```
.app  .framework  .bundle  .plugin  .appex  .xpc  .kext  .prefpane  .qlgenerator
```

扩展名清单的判定原则：Finder 把它当单个项目、双击打开整体、不能浏览内部。清单按此原则一次定稿，后续新增按同一原则评审，不再逐项讨论。

有意不含：`.xcworkspace`、`.xcodeproj`、`.playground`、`.docset`、`.scnassets`——Finder 会按普通文件夹展开，内部文件可见。

判定函数（`engine/src/bundle.rs`，与排序修复共用）：

```rust
/// 条目名是否以 bundle 扩展名结尾（ASCII 大小写不敏感）。
pub fn is_bundle_name(display: &[u8]) -> bool;

/// 路径自身或任一祖先是否 bundle。
pub fn is_bundle_path(p: &Path) -> bool;

/// 路径的**严格祖先**是否 bundle（不含自身）。区分「bundle 本身」与「bundle 内部」。
pub fn is_inside_bundle(p: &Path) -> bool;
```

三个函数的职责边界：

| 判定 | 函数 | 用途 |
|---|---|---|
| 这个目录是 bundle 吗 | `is_bundle_name` | 首扫决定是否入队；增量决定事件归属 |
| 这个路径在包内（含自身）吗 | `is_bundle_path` | 排序置顶判定 |
| 这个路径在包内（不含自身）吗 | `is_inside_bundle` | 增量 diff 拦截、补链终止 |

### 4.2 首扫

- `classify_entry` 对目录新增分支：`is_bundle_name` 命中时返回 `Step::LeafDir { path, name }`（形态与 `Step::Dir` 相同，区别只在调用方不入队）。
- 串行臂：`index.insert(&name, parent_idx, true)`，**不** `queue.push_back`。
- 并行臂：与 `Step::Dir` 相同地登记 `DirTable` 取得自身 `dir_seq`、push `Rec::Dir`，**只是不 `queue.push_back`**。必须登记自身 seq：`apply_records` 用 `table.get(dir_seq)` 取自身原始路径来定位父目录并写 `dir_map` 键，沿用父 seq 会把 bundle 条目的路径写成父目录。
- 结果：bundle 目录条目在索引中，其子项全部缺席。

**显式扫描根豁免**：`scan_roots` 从根开始遍历，根自身不经过 `classify_entry`。用户通过 `MCE_ROOTS=/Applications/Safari.app` 显式指定 bundle 为根时，其内容照常索引；根之下再遇到的嵌套 bundle 仍剪枝。语义：显式指定根就是声明「我要这里面的内容」，比默认策略优先。

### 4.3 增量

三条规则，覆盖所有事件目标形态：

**R1 目标是普通目录**（`is_bundle_path(&base) == false`）：正常 diff。

**R2 目标是 bundle 自身**（`is_bundle_name(base 的文件名)`）：不 diff 自身（否则会把 `Contents` 收进索引），改 **diff 其父目录**。父目录的 listing 会正确增删该 bundle 条目，同时覆盖新建、删除、改名。

**R3 目标在 bundle 内部**（`is_inside_bundle(&base)`）：

- 若该 bundle 的条目在 `dir_map` 中存在 → `return 0`（稳态零成本）。后台应用的日志写入、自动更新会在包内任意深度产生事件，这些路径没有索引条目，走补链会把包内重新拉回索引。
- 若条目缺失 → diff 该 bundle 的父目录补录。这是自愈路径，不依赖父目录事件一定先到。

**补链终止**：补链循环在 `is_excluded(next, &excludes)` 的同点加 `is_inside_bundle(next)` 检查，命中即终止。`next` 是 bundle 自身时**不终止**——它的条目正是本级要收录的对象；终止条件针对「进入 bundle 内部」。

**listing 语义不变**：`read_dir_listing` 不感知 bundle。bundle 是父目录的普通子项（`is_dir = true`），父目录 diff 时照常增删该条目。增量本身不递归，不会因为 listing 中出现 bundle 就进入它。

**删除/改名/移动 bundle**：父目录 diff 处理条目增删，包内无条目需清理。

### 4.4 配置

`config.toml` 新增：

```toml
# 是否索引 .app/.framework 等 bundle 内部的文件（默认 false：只收录 bundle 本身）
index_bundle_contents = false
```

- `serde(default)`，缺省 `false`。
- `true` 时扫描/增量两侧跳过 bundle 判定，恢复全量索引。排序仍不置顶包内 `.app`（O3 独立生效）。
- **配置不热加载。修改前必须先 `macEverything stop`。** 排除目录命令（`exclude add/remove`、`reset_excludes`）会把内存中的 Config 整份回写落盘；运行中手改 `config.toml` 后执行任何排除操作，手改内容会被覆盖，表现为「改了没生效，还丢了」。usage 与设置页文案写清这条。

### 4.5 索引格式与升级

bundle 剪枝改变了「索引内容 = 磁盘条目全集」这一前提，旧索引里的包内条目不会自行消失。

- `persist::FORMAT_VERSION`：`1 → 2`。
- header 在 `excluded` 段之后新增 `bundle_prune: u8`（1 = 剪枝生效，0 = 未生效）。
- 读侧：版本 ≠ 2 → `LoadError::BadVersion` → daemon 弃用重建（既有路径）。
- `IndexMeta::default()` 改为手写实现，`bundle_prune = true`，与产品默认一致。测试夹具大量用 `IndexMeta::default()` 落盘，若沿用 `bool` 的 `false` 默认，任何走 `cold_start` 的测试都会撞上「落盘值 false ≠ 生产默认 true」的误判。

### 4.6 启动策略比对

版本相符但剪枝开关被翻转时，索引内容与配置不一致。判定做成纯函数，与 `needs_full_rescan`、`rescan_fire_allowed` 同形，放在 `engine` 侧便于单测：

```rust
/// 落盘索引的剪枝策略与当前配置是否不一致（不一致 → 需全量重扫）。
pub fn needs_policy_rescan(meta: &IndexMeta, cfg: &Config) -> bool {
    meta.bundle_prune != !cfg.index_bundle_contents
}
```

**比对范围只含 `bundle_prune`。**

- 不比 `scan_roots`：它是上次落盘时的根集合（含当时挂载的外接卷），启动时的 roots 由当前挂载状态重建。拔过盘就判「不同」，每次启动白跑全量重扫。
- 不比 `excluded`：排除列表的启动比对是另一个既有缺口（`meta.excluded` 全仓只写不读），本期不顺带修，单独立项。

调用点在 `cold_start` 加载索引成功的分支内，与索引健全性抽检并列：命中则记日志、置 `DEGRADED`、`spawn_rescan`，旧 Base 在重扫期间仍可查（既有语义）。`cold_start` 自身无单测，判定逻辑全部落在纯函数上。

### 4.7 升级代价

一次全量重扫。具体时长以 bench 实测为准，本文不引用未标注测量环境的数字。状态显示「重扫中」，旧索引在扫完前不可读（既有冷启动语义：格式版本不符时不挂载）。

## 5. 实现接入点

| 文件 | 改动 |
|---|---|
| `crates/engine/src/bundle.rs` | 新增：`BUNDLE_EXTENSIONS`、`is_bundle_name`、`is_bundle_path`、`is_inside_bundle`（排序修复已先落地，剪枝复用） |
| `crates/engine/src/scanner.rs` | `Step` 增加 `LeafDir`；串行/并行两臂各加分支；`scan_roots` / `collect_records` 接受 `prune_bundles: bool` |
| `crates/engine/src/reconcile.rs` | `full_rescan` 透传开关；新增 `needs_policy_rescan` |
| `crates/engine/src/config.rs` | 新增 `index_bundle_contents`，`default_config` 与往返测试同步 |
| `crates/engine/src/persist.rs` | `FORMAT_VERSION = 2`；`IndexMeta` 加 `bundle_prune: bool`、手写 `Default`；header 读写各一处 |
| `crates/cli/src/daemon.rs` | `diff_one` 按 R1/R2/R3 分支；补链终止加 `is_inside_bundle`；首扫/重扫调用传开关；`cold_start` 调用 `needs_policy_rescan` |
| `crates/cli/src/daemon/volumes.rs` | `scan_volume_into` 的 `collect_records` 传开关（初稿漏掉的扫描入口） |
| `crates/engine/tests/parity.rs` | 参考枚举同步剪枝语义，受控树加入 `.app`/`.framework`/`.kext` |

传参方式：`scan_roots(roots, excludes, threads, prune_bundles)` 增加一个 `bool`。当前调用点少，直接加参数；若后续再出现第二个策略位，再收敛为 `ScanPolicy`。

## 6. 测试计划

全部做突变验证（改坏实现确认测试变红）。

### 单元（engine）

1. `is_bundle_name`：`.app` / `.APP` / `.framework` 命中；`notes.app.txt`、`应用.app备份`、`Contents` 不命中。
2. `is_bundle_path` 与 `is_inside_bundle` 的分界：`/a/X.app` 前者 true 后者 false；`/a/X.app/Contents` 两者 true；`MyApp.appdata/x` 均 false。
3. 首扫：受控树 `t/X.app/Contents/Info.plist` → 索引含 `X.app`，不含 `Contents`、`Info.plist`。
4. 首扫开关：`prune_bundles = false` → 内部条目全部收录。
5. 显式根豁免：`scan_roots([t/X.app])` → 包内条目收录。
6. 串行/并行一致：同一棵树两臂条目集相等（扩展到含 bundle 的树）。
7. 增量 R2：目标为 `.app` 自身 → 父目录被 diff，bundle 条目增删正确，包内无条目。
8. 增量 R3：索引中已有 bundle 条目，目标为 `.app/Contents` → 返回 0，条目数不变。
9. 增量 R3 自愈：索引缺 bundle 条目，目标为 `.app/Contents` → diff 父目录补录条目。
10. 补链：索引中只有 `/Applications`，事件目标 `/Applications/Z.app/Contents/MacOS` → 补链收录 `Z.app`，止于包内，不收录内部条目。
11. 持久化：`bundle_prune = true` 落盘 → 读回为 true；版本 1 文件 → `BadVersion`。
12. `needs_policy_rescan`：策略一致 → false；翻转 → true；`scan_roots` 不同但策略一致 → false（突变「把 scan_roots 加进比对」此测必红）。
13. 配置：缺省 false；往返保留。

### 对拍（parity）

参考枚举实现同样的 bundle 剪枝，受控树含 `.app`、`.framework`、`.kext` 与普通同名目录，逐条比对条目集零差异。

### 实机冒烟

`HOME=/tmp/mce-dev MCE_ROOTS=...` 隔离环境下造 `A.app/Contents/B.app`，`start` → `query "B.app"` 应只命中 `A.app` 不命中内部；`status` 条目数符合预期。

### 量化验收

`scripts/bench.sh` 受控树加入一棵真实形态的 `.app`（`Contents/Frameworks/.../Helper.app` 多层）。记录剪枝前后的索引条目数、首扫时间、常驻内存，写进验收报告——这是本方案的主要正面收益，需要数字而不是断言。

## 7. 文档同步

| 文档 | 改动 |
|---|---|
| `docs/specs/v1-core/product.md` 问题陈述 §1 | 补「索引粒度 = Finder 可见单位」：bundle 在 Finder 中是单个项目，内部不单独收录 |
| `docs/specs/v1-core/product.md` 目标 7 | 对拍零差异的范围改为「同扫描根、同排除、同 bundle 规则」 |
| `docs/specs/v1-core/product.md` 目标 10 | 「100% 递归覆盖」改为「扫描根内所有 Finder 可见单位 100% 覆盖」 |
| `docs/specs/v1-core/product.md` 目标 12 | 实时性覆盖补一句：包内变更不产生索引条目，bundle 条目的增删改仍 ≤ 500 毫秒可见 |
| `docs/specs/v1-core/tech.md` | 扫描器与增量两节同步判定规则与三条增量规则 |
| `docs/usage.md` | 配置字段、改 config 前先 stop、已知边界（包内不可搜，开关可放开） |
| `crates/app/src/ui/settings_view.rs`（索引标签） | 常驻说明一句：「应用包（.app 等）内部不单独收录」；开关排入后续迭代 |
| `CLAUDE.md` 关键约定 | 「排除判定三条路径共用 `is_excluded`」补 bundle 判定同点共用；增补「卷扫描是第四条写入路径」 |
| `docs/architecture.md` | 扫描器一节补 bundle 剪枝与粒度立论 |

## 8. 风险与已定取舍

- **旧索引重建**：升级后首次启动有一次全量重扫。格式版本本来就承担这类语义变更。
- **混装窗口的离线读**：格式升到 2 后，旧 GUI 在 daemon 未运行时离线直读新索引会得到 `index_unreadable`（`client/src/lib.rs` 的 `load_mmap` 失败路径）；在线不受影响（`engine_launcher.rs` 已有版本偏差检测并自动重启引擎）。不额外处理，风险表留痕。
- **用户想搜包内文件**：两个逃生口——开关（需重启 daemon）与显式扫描根。文档写明。
- **`.bundle` 误伤**：非标准目录也可能叫 `*.bundle`，但 Finder 同样按包处理；开关可恢复。
- **bundle 目录的 `dir_map` 键**：父目录 diff 时登记，`diff_one` 按 R2/R3 拦截保证不 diff 内部。登记本身用于包内事件的自愈判定。
- **config.toml 手改与排除命令互相覆盖**：见 §4.4，文档与设置页文案双重提示。

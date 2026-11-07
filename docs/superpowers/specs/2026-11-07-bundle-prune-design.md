# 包内剪枝（bundle prune）设计

- 日期：2026-11-07
- 状态：暂缓（2026-11-07 复评：截图症状的直接成因是排序把嵌套 `.app` 一并置顶，属独立缺陷，先行修复；本方案待观察排序修复后的实际噪声再决定）
- 范围：engine 扫描/增量/持久化 + cli daemon 增量入口 + 配置；查询排序与 GUI 无改动
- 功能代号：bundle-prune

> **复评记录（2026-11-07）**：本文档初稿把「搜 `ai` 首屏 16 条中 11 条来自包内」当作剪枝的动机证据，这个归因不成立。首屏污染的成因是 product §4「App 包整体置顶」把嵌套 `.app` 也抬到最前；排序修复（`2026-11-07-nested-app-rank-design.md`）以零格式变更、零重扫、零召回损失解决该症状。剪枝仍可能因「包内文件本身无搜索价值、索引更小」而成立，但那是另一个动机，需重新论证后再实施。若实施，本文 §3.4 的「开关打开时完全恢复旧行为」同样需要改为「恢复全量索引，但排序仍不置顶包内 `.app`」。

## 1. 背景

`.app` 在索引里是普通目录，扫描器整棵遍历进去。结果是搜索结果里混入大量应用包内的实现细节：

- `/Applications/WPS Office.app/Contents/SharedSupport/.../kaideployserver.app`（嵌套子应用，出现两次）
- `/Applications/BaiduNetdisk_mac.app/Contents/Frameworks/.../BaiduNetdisk_mac Helper.app`（多个 Helper 变体）
- `/Applications/WorkBuddy.app/Contents/Frameworks/.../WorkBuddy Repair.app`

排序规则（product §4「App 包整体置顶」）让这些嵌套 `.app` 排在最前，正常应用反而被挤到后面。用户搜 `ai` 时首屏 16 条里 11 条来自包内。

规格里目前没有任何针对包内内容的策略，索引语义就是「磁盘上每个非符号链接条目一条」。

## 2. 目标与非目标

### 目标

1. `.app` 等 bundle 目录本身仍作为条目收录（可被搜到、可被列目录、可执行「打开」）。
2. bundle 目录内部不产生任何索引条目：首扫不进入，增量事件不追加。
3. 默认生效；配置项 `index_bundle_contents` 可恢复旧行为。
4. 三条写入路径（首扫 / 增量 diff / 补链）共用同一判定，不出现语义分叉。
5. 旧索引在升级后自动重建，不残留包内条目。

### 非目标

- 不改变查询排序、结果上限、搜索语法。
- 不新增 GUI 设置项（本期只走 `config.toml`）。
- 不按 `Contents/Info.plist` 探测（需额外出 stat，首扫成本上升）。
- 不处理 `.xcworkspace` / `.xcodeproj` 这类「看起来像包、实际是用户工程目录」的扩展名（见 §3.1 清单，刻意不含）。

## 3. 行为契约

### 3.1 判定规则

路径的**任一组件**满足「以 `BUNDLE_EXTENSIONS` 中某项结尾（ASCII 大小写不敏感）」，即视为 bundle 内路径。

`BUNDLE_EXTENSIONS`：

```
.app  .framework  .bundle  .plugin  .appex  .xpc  .kext  .prefpane  .qlgenerator
```

有意不含：`.xcworkspace`、`.xcodeproj`、`.playground`、`.docset`、`.scnassets`——这些是用户工程/资料目录，经常需要搜内部文件。

判定函数（engine）：

```rust
/// 目录名是否为 bundle（ASCII 大小写不敏感后缀匹配）。
pub fn is_bundle_name(name: &str) -> bool;

/// 路径是否位于 bundle 内部或自身即 bundle（逐组件调用 is_bundle_name）。
pub fn is_bundle_path(p: &Path) -> bool;
```

`is_bundle_path` 对 `.app` 自身返回 `true`。语义区分由调用点决定：判断「是否进入」用 `is_bundle_path(dir)`，判断「是否是 bundle 条目」用 `is_bundle_name(条目名)`。

### 3.2 首扫

- `classify_entry` 对目录新增分支：`is_bundle_name` 命中时返回 `Step::LeafDir { path, name }`（形态与 `Step::Dir` 相同，语义区别只在调用方不入队）。
- 串行臂：`index.insert(&name, parent_idx, true)`，**不** `queue.push_back`。
- 并行臂：与 `Step::Dir` 相同地登记 `DirTable` 取得自身 `dir_seq`、push `Rec::Dir`，**只是不 `queue.push_back`**。必须登记自身 seq：`apply_records` 用 `table.get(dir_seq)` 取自身原始路径来定位父目录并写 `dir_map` 键，沿用父 seq 会把 bundle 条目的路径写成父目录。
- 结果：bundle 目录条目在索引中，其子项全部缺席。

### 3.3 增量

- `read_dir_listing` 不变：bundle 是父目录的普通子项（`is_dir = true`），父目录 diff 时照常增删该条目。
- `diff_one` 入口（`crates/cli/src/daemon.rs`）：canonical 化后 `is_bundle_path(&base)` 为真时直接 `return 0`。理由：FSEvents 会在包内任意深度上报事件（后台应用的日志写入、自动更新），这些路径没有索引条目，走补链会把 bundle 内部重新拉回索引。
- 补链循环：`is_excluded(next, &excludes)` 的同点加 `is_bundle_path(next)` 检查，命中即终止补链。补链从 bundle 内的目标向上找到 `.app` 的父目录时，chain 会包含 `.app` 自身，此时必须停止，否则下一轮 `apply_and_track` 会 diff `.app` 并收录 `Contents`。
- 新建 `.app`：父目录 diff 收录 bundle 条目；随后到达的包内事件被 `diff_one` 入口丢弃。
- 删除/改名/移动 `.app`：父目录 diff 处理条目增删，bundle 内部无条目需清理。

### 3.4 配置

`config.toml` 新增：

```toml
# 是否索引 .app/.framework 等 bundle 内部的文件（默认 false：只收录 bundle 本身）
index_bundle_contents = false
```

- `serde(default)`，缺省 `false`。
- `true` 时完全恢复旧行为：`is_bundle_name` 判定在扫描/增量两侧被整体跳过。
- 配置在 daemon 启动时读取；修改需 `stop && start`。daemon 启动时与上次落盘索引的策略值比对，不一致触发全量重扫（见 §3.5）。

### 3.5 索引格式与升级

bundle 剪枝改变了「索引内容 = 磁盘条目全集」这一前提，旧索引里的包内条目不会自行消失。

- `persist::FORMAT_VERSION`：`1 → 2`。
- header 在 `excluded` 段之后新增 `bundle_prune: u8`（1 = 剪枝生效，0 = 未生效）。
- 读侧：版本 ≠ 2 → `LoadError::BadVersion` → daemon 弃用重建（既有路径）。
- 版本 = 2 但 `meta.bundle_prune` 与当前配置不符 → 同样触发全量重扫。这条覆盖「用户翻转开关后重启 daemon」的场景。
- 升级代价：一次全量重扫。24k 条目约 1 秒；百万级十几秒。状态显示「重扫中」，旧索引在扫完前不可用（既有冷启动语义）。

## 4. 实现接入点

| 文件 | 改动 |
|---|---|
| `crates/engine/src/scanner.rs` | 新增 `BUNDLE_EXTENSIONS`、`is_bundle_name`、`is_bundle_path`；`Step` 增加 `LeafDir`；串行/并行两臂的 match 各加分支；`scan_roots` / `collect_records` / `full_rescan` 接受 `prune_bundles: bool` |
| `crates/engine/src/watcher.rs` | 无改动（listing 语义不变）；`read_dir_listing` 的文档注释补一句 bundle 条目如常列出 |
| `crates/engine/src/reconcile.rs` | `full_rescan` 透传开关 |
| `crates/engine/src/config.rs` | 新增 `index_bundle_contents`，`default_config` 与往返测试同步 |
| `crates/engine/src/persist.rs` | `FORMAT_VERSION = 2`；`IndexMeta` 加 `bundle_prune: bool`；header 读写各一处 |
| `crates/cli/src/daemon.rs` | `diff_one` 入口拦截；补链循环加 bundle 检查；首扫/重扫调用传开关；启动时比对 `meta.bundle_prune` |
| `crates/engine/tests/parity.rs` | 参考枚举同步剪枝语义，受控树加入 `.app`/`.framework` |

传参方式：`scan_roots(roots, excludes, threads, prune_bundles)` 增加一个 `bool`。改动面最小的形态是让 `prune_bundles` 与 `excludes` 一起打包成一个 `ScanPolicy` 结构，但当前只有 4 个调用点，直接加参数即可；若后续再出现第二个策略位，再收敛。

## 5. 测试计划

全部做突变验证（改坏实现确认测试变红）。

### 单元（engine）

1. `is_bundle_name`：`.app` / `.APP` / `.framework` 命中；`notes.app.txt`、`应用.app备份`、`Contents` 不命中。
2. `is_bundle_path`：bundle 自身、内部深层路径命中；同前缀的非 bundle 目录（`MyApp.appdata/x`）不命中。
3. 首扫：受控树 `t/X.app/Contents/Info.plist` → 索引含 `X.app`，不含 `Contents`、`Info.plist`。
4. 首扫开关：`prune_bundles = false` → 内部条目全部收录。
5. 串行/并行一致：同一棵树两臂条目集相等（既有 parity 的串行-并行对照扩展到含 bundle 的树）。
6. 增量：索引中已有 bundle 条目；`diff_one` 对 `.app/Contents` 返回 0，索引条目数不变。
7. 增量新建：父目录 diff 收录 `Y.app`，随后 bundle 内 diff 不产生条目。
8. 补链：索引中只有 `/Applications`，事件目标 `/Applications/Z.app/Contents/MacOS` → 补链止于 `Z.app` 的父目录，索引增加 `Z.app` 一条，不增加内部条目。
9. 持久化：`bundle_prune = true` 落盘 → 读回 `meta.bundle_prune == true`；版本 1 文件 → `BadVersion`。
10. 配置：缺省 false；往返保留。

### 对拍（parity）

参考枚举实现同样的 bundle 剪枝，受控树含 `.app`、`.framework`、`.kext` 与普通同名目录，逐条比对条目集零差异。

### 实机冒烟

`HOME=/tmp/mce-dev MCE_ROOTS=...` 隔离环境下造 `A.app/Contents/B.app`，`start` → `query "B.app"` 应只命中 `A.app` 不命中内部；`status` 条目数符合预期。

## 6. 文档同步

| 文档 | 改动 |
|---|---|
| `docs/specs/v1-core/product.md` §1 | 排除语义补一条：bundle 内部默认不索引，配置可开 |
| `docs/usage.md` | 配置字段说明、搜索范围的已知边界 |
| `CLAUDE.md` 关键约定 | 「排除判定三条路径共用 `is_excluded`」补 bundle 判定同点共用 |
| `docs/architecture.md` | 扫描器一节补一句 bundle 剪枝 |

## 7. 风险与已定取舍

- **旧索引重建**：升级后首次启动有一次全量重扫。可接受，且格式版本本来就承担这类语义变更。
- **用户想搜包内文件**：开关是唯一退路，且翻转后必须重启 daemon（配置不热加载）。文档写明。
- **`.bundle` 误伤**：非标准目录也可能叫 `*.bundle`，但其内部条目通常同样无搜索价值；开关可恢复。
- **bundle 目录的 `dir_map` 键**：仍会登记（父目录 diff 时插入），但 `diff_one` 入口拦截保证它不会被 diff。登记本身无害，且删除/改名事件依赖父目录 diff 兜底。

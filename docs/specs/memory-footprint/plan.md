# 内存占用收敛 v1 实施计划

- 日期：2026-10-30
- 状态：待评审
- 关联：`product.md`（验收标准 A1–A8）、`tech.md`（P0–P5）

## 阶段总览

| 阶段 | 内容 | 主要落点 | 验收 |
| --- | --- | --- | --- |
| S0 | 基线与观测工具；GUI 峰值归因 | `scripts/mem-watch.sh`（新）、tech.md §6.3 | 基线数据入档 |
| S1 | P0 断振荡 + P1 失败治理 | `crates/cli/src/daemon.rs`、`crates/engine/src/watcher.rs` | A3、A4 |
| S2 | P2 峰值瘦身 + 死页归还 | `crates/engine/src/scanner.rs`、daemon/engine 平台模块 | A2 第一档（≤ 450MB） |
| S3 | P3 索引驻留 mmap 化 + dir_map intern | `crates/engine/src/{index,persist,query,watcher}.rs`、`crates/cli/src/daemon.rs` | A1、A2 第二档（≤ 300MB）、A7、A8 |
| S4 | P5 GUI 治理（阶段一确定性手段 → 阶段二归因驱动） | `crates/app/src/ui/mod.rs`、`platform/mac.rs` | A5、A6、A6b |
| S5 | 实机长跑验收 | 本机 48h | A1–A8 复核 |

依赖关系：S1/S2 相互独立，可并行；S3 是主工程，依赖 S1（否则重扫振荡反复重建 Overlay 语义混乱）；S4 独立。

## S0 基线与观测（0.5 天）

1. `scripts/mem-watch.sh <pid> [interval]`：循环采样 `footprint -p <pid>`（phys_footprint + 峰值），输出 CSV；验收统一用此脚本 + 活动监视器人工复核双口径。
2. 记录改动前基线：daemon 稳态/峰值、重扫次数（日志计数）、GUI 稳态/峰值（现值见 product.md §1）。
3. **GUI 全构成归因**（阶段二的数据前提，tech.md §6.3）：
   - `MallocStackLogging=1` 启动 GUI，复现完整结果窗口冷启动与万行滚动，`malloc_history` 定位 1.7GB 峰值分配源（嫌疑序：单帧 tessellation / 字形图集多轮扩容 / 图标批量解码）；
   - `vmmap -pages` 对稳态进程做 IOSurface 逐个归属（66 个 surface → 视口/图层/大小映射），确认 drawable 池与毛玻璃层占比；
   - `heap` 定位 49.7MB 活跃堆的异常块归属；
   - 全部结论回填 tech.md §6.3，并据此判定阶段二 ≤100MB 的可达路径或不可消除项清单。

## S1 断振荡 + 失败治理（1.5–2 天）

1. `EngineState` 增 `rescanning: AtomicBool`、`rescan_lock: Mutex<()>`、重扫完成通知通道（不携索引产物）、`rescan_dirty: Mutex<HashSet<PathBuf>>`（tech.md §1）。
2. **仅自动源**（633 重放零事件 / 661 dropped / 703 流断开 / 877 建流失败）异步化：重扫线程 `try_lock` 后在**同一 guard 内完成扫描段 + 替换段**（替换段含 drain `rescan_dirty` 重放；锁全程覆盖，tech.md §1.2）；`rescanning` 期间 dropped 忽略、diff 照常并记 `rescan_dirty`。**exclude 两处（1121/1184）保持同步**：`lock()` 阻塞等待进行中的自动重扫（含替换段）后锁内执行，`Response::Exclude` 时序语义不变（tech.md §1.5）。
3. `apply_and_track` Err 分支按 errno 分派：ENOENT → `remove_prefix` 摘除；EPERM/EACCES → **保留条目 + gaps + 负缓存**（`perm_denied: HashSet<PathBuf>`，canonical 路径键——条目号键会在 compact 重排后错杀无辜条目，tech.md §2；重启清零）；其他 → 3 次阈值入负缓存。
4. 测试（突变验证纪律）：
   - 单测：rescanning 期间 dropped 被忽略；替换段在重扫线程锁内执行后 `rescan_dirty` 目录重放；EPERM 目录负缓存短路（diff 零开销、条目保留）；EPERM 目录**不得摘除**（摘除则父 diff 重插 → 插摘振荡，突变验证）；**compact 触发后负缓存不错杀**（权限目录 diff 仍被短路，而同路径前缀的其他目录内容漂移正常更新）；exclude 同步路径在异步重扫进行中的阻塞等待行为。
   - 突变：还原同步重扫 → 构造 dropped 序列断言振荡重现（红）；负缓存键改回条目号 → compact 后内容漂移用例必红。
5. 验证：隔离环境（`HOME=/tmp/mce-dev` + 高频文件变动脚本模拟风暴）跑 1h：全量重扫 0 次、日志 < 1MB；实机冒烟脚本走查。

## S2 峰值瘦身 + 归还（0.5–1 天）

1. `Rec` 改 `dir_seq` 引用（tech.md §3，32B/条）；深排序 key 切 `dirs[dir_seq]`；`apply_records` 的 raw 键**直接取 `dirs[dir_seq]`**（保真非 UTF-8 名称，不拼接）；目录表 per-shard 本地 + `dir_seq` 全局编码（shard_idx << 20 | local_seq 或合并平移），延续无逐条全局锁。
2. `malloc_zone_pressure_relief` FFI 接入：重扫 flush 后、compact 后（daemon 侧）；GUI 侧接入见 S4。
3. 测试：`dir_seq`↔目录表对位单测（含跨 shard、非 UTF-8 目录名 raw 键等价）；bench 首扫 footprint 采样对比基线。
4. 验收：A2 第一档（S2 后峰值 ≤ 450MB，重扫后 10s 回落）。

## S3 索引 mmap 化（主工程，5–8 天）

按 tech.md §4 顺序：

1. **dir_map intern 化**先行（独立可回滚）：`HashMap<u64, u32>` + path_pool 二次比对；全量测试 + parity 绿。
2. **接口改造**（tech.md §4.3 表）：`IndexView::entries() -> &[Entry]` 单切片签名 → 迭代式/分段枚举（`query::execute` 及全部消费点同步改）；`write_atomic(&Index, ...)` 入参改条目流；`cold_start` 直挂 SharedView、`materialize()` 退役为 flush 合并内部步骤。
3. **Overlay 结构**：`Overlay { overlaid, anchors, index }` 落地；写侧 `apply_dir_diff` 改造（listing 与 `children_of(anchor)` 派生集比对产出 `DiffStats`，子项全集单一真相存于 overlay index，tech.md §4.2）。
4. **查询合成视图**：双源扫描 + 屏蔽判定（tech.md §4.2 条目号链 `valid(e)` 递归，与拼路径同一次上溯）；`full_path_composite` 单一拼路径入口；`query::execute` 接入。
5. **status/entry_count** 近似值；文案「约 N 条」。
6. **flush 合并写**：合成视图产出写侧条目流；Base 替换时序（flush_lock 域内 remap + holder 写锁瞬间持有）；节流参数 30s→120s、10k→30k。
7. **P0 替换段迁移**：S1 预留的切换点收口——重扫产物不挂堆上 `Mutable`，改走 flush 落盘（条目流直写）→ remap 新 Base → 清 Overlay（tech.md §4.3）。
8. **抽样对拍**改合成视图入口。
9. 测试（本项目最大测试面）：
   - parity 扩展：合成视图 vs 独立枚举零差异；新增场景——目录 diff 后立即查询（旧路径为空/新路径可见）、overlay 嵌套两层、子目录 diff 后父未再 diff、「有 anchor 但不在 overlaid」中间目录子树以 Base 为准（valid(e) 终止条件钉）、大写/Unicode 名称形态条目（结构化判定不受形态影响；若回归字符串校验必红）、EPERM 负缓存目录混合树对拍。
   - 突变：屏蔽判定漏判（去掉 `anchors.contains(e)` 终止或改走名称校验）→「diff 后查询旧路径为空」专项必红。
   - 崩溃恢复：Overlay 窗口内 kill -9 → 重启重放追平（现语义不变，回归验证）。
10. 验收：隔离环境全量测试绿；bench 五项劣化 ≤ 5%（A7）；实机稳态 footprint ≤ 100MB（A1，目标 60MB）；重扫峰值复核 A2 第二档 ≤ 300MB。

## S4 GUI 治理（1.5–2 天）

**S4a 阶段一（确定性手段，验收 ≤ 150MB）**：

1. 空闲 60s `pressure_relief`（`platform/mac.rs` unsafe 区）；悬浮条/结果窗口/托盘三视口统一接入。
2. 图标缓存上限 2048 → 128 LRU（`ui/icons.rs` `MAX_PATH_ICONS`）+ 确认淘汰路径上纹理与 CG Image 显式释放。
3. `max_texture_side` 4096；结果窗口中文长列表滚动回归（帧率肉眼可接受）。
4. S0 堆粗查的易修项。
5. 验收：A5 阶段一线、A6、A6b（若峰值修复项在归因后才可落地，A6b 留待 S4b）。

**S4b 阶段二（归因驱动，验收 ≤ 100MB）**：按 S0 归因结论逐项推进（隐藏视口 surface 释放、drawable 池收敛、字体缓存限制、每帧分配审计、峰值修复项）；若出现不可消除构成项，携实测数字回评审重新定线——不无限追。

## S5 实机长跑（48h，挂机观察）

1. 实机 48h：`mem-watch.sh` 采样 daemon/GUI；复核 A1（≤100MB）、A3（重扫 ≤2 次/24h）、A4（日志 <1MB/天）、A5（阶段线）、A6、A6b。
2. 实机冒烟脚本（development.md）全走查；`check_seo.py` 无涉（纯 Rust 改动）。
3. 全程三门禁绿收口；文档同步（architecture.md ADR-2 补 Base+Overlay 决策、development.md 陷阱表补 P0 窗口语义）。

## 风险与回滚

- S1/S2 各自独立 commit，可单独回滚。
- S3 分两步落地（intern 先行独立 commit）；Overlay 若评审发现屏蔽判定复杂度超预算，退路 = 「flush 提频至 15s + 物化索引保留」的中间态（footprint ~150MB，不达标但止血），退路不写入本规格验收。
- S0 归因若发现 GUI 峰值另有主因（非图集/tessellation），S4 范围按结论扩，验收线 A5/A6 不变。

# 内存占用收敛 v1 技术规格

- 日期：2026-10-30
- 状态：待评审
- 关联：`product.md`、`docs/architecture.md`（ADR-2 内存索引、ADR-5 降级重扫、ADR-6 并行扫描、ADR-8 GUI 平台层）、`docs/specs/v1-core/tech.md`

## 0. 根因链（实测证据，改动均锚定于此）

```
事件风暴（开发写盘 / .Trash 操作 / 自动更新器 / DMG 反复挂载）
  → FSEvents（FILE_EVENTS + 0.1s latency）内核缓冲溢出 → dropped 标记
  → degrade_and_rescan 在消费线程内同步执行（重扫 10–30s，rx 停摆）
  → channel（无界）与内核缓冲继续积压 → 新 dropped（> rescan_floor）
  → 30s 冷却过后再次重扫 → ∞   （daemon.rs:35-37 注释自曝病史：曾实测 420 次）

连锁：dropped 丢弃删除事件 → 死条目永留索引/dir_map
  → 每次相关事件白跑 diff（ENOENT/EPERM），日志刷屏 25.9 万行
  → 消费变慢 → 积压加剧 → 更多 dropped（正反馈第二环）

重扫峰值：Vec<Rec> 逐条拷贝完整父路径（126 万条 ≈ 200MB）
  + 新旧索引并存（物化 Index 113MB 常驻）+ sort 缓存 + dir_map 重建
  → 实测峰值 784MB；释放后 Malloc Large (empty) 254MB 脏页滞留
```

内存构成实测（vmmap/heap，daemon）：堆 175MB 稳态 = 物化索引 113MB + dir_map（HashMap<PathBuf,u32>，约 20 万目录）40MB + 运行时 ~20MB。GUI（phys_footprint 306.7MB，**不含 clean 库页，全部为可追究分配**）：Malloc 活跃堆 49.7MB、`Malloc Large (empty)` 死页 73MB、IOSurface 78MB（66 个，CAOpenGLLayer drawable 池，eframe glow 后端）、IOAccelerator 19MB、CG Image 18MB（结果行 app icons）、其余 ~68MB（CoreText 字体缓存、AppKit 对象、GL 兼容层内部）、峰值 1.7GB 未归因。

phys_footprint 口径关键事实：**clean file-backed 页不计入**。这是 P3 能达标而内存里仍有 113MB 索引数据的机制基础。

---

## 1. P0 断振荡：自动触发源的重扫移出消费线程

**现状**：`consume_events` 收到 `is_dropped` → `degrade_and_rescan` 同步执行（daemon.rs:648-663）。重扫期间 `rx` 无人消费；`rescan_floor` 只能跳过「重扫开始前」的陈旧事件，重扫**期间**新产生的事件与 dropped 无法免疫。

**触发源全景**（`degrade_and_rescan` 现有 6 个调用点，daemon.rs:633/661/703/877/1121/1184）：

| 调用点 | 触发源 | P0 处置 |
| --- | --- | --- |
| 633 | 重放零事件 + 停机超阈 | **异步化** |
| 661 | dropped 标记（振荡主角） | **异步化** |
| 703 | 流断开 / 保守判定 | **异步化** |
| 877 | 建流失败 | **异步化** |
| 1121 / 1184 | exclude 变更（reset / add·remove） | **保持同步**（见下） |

**设计**：自动源的重扫移入专用后台线程，**扫描段与替换段在同一把锁内完成**。

1. 自动路径的 `degrade_and_rescan` 改为：置 `state.rescanning: AtomicBool`，`try_lock` 拿到 `rescan_lock` 后 spawn 独立线程，**锁的 guard 由该线程持有直至替换段结束**：`reconcile::full_rescan`（纯磁盘遍历）→ 替换段（原函数后半：gaps、水位 `fetch_max(floor)`、`rebuild_dir_map`、`flush`、回 READY）→ drain `rescan_dirty` 逐目录 `diff_one` 重放——全部在重扫线程锁内执行，完成后向消费循环发通知消息（仅状态清理用，不携索引产物）。
2. **锁生命周期 = 扫描段 + 替换段全程**（评审缝隙修正）：若锁只护扫描段，存在时序窗口——exclude 同步重扫完成（新索引已挂 holder）→ 自动替换段后到，用更旧的自动产物覆盖 holder 并 flush 落盘，exclude 成果被回退。替换段纳入锁内后该窗口不存在；跨线程移交 guard 的复杂性随之消失。
3. **重扫窗口内的增量保真**：`rescanning == true` 期间消费循环照常 diff（基于旧索引，状态持续可查；与替换段的 holder 写锁天然互斥）；同时把本窗口内实际应用过 diff 的目录记入 `rescan_dirty: Mutex<HashSet<PathBuf>>`。重扫线程在锁内 drain 该集合逐目录重放——diff 以磁盘为准、天然幂等，新索引由此追平窗口内变更。`rescanning` 清零时机 = 替换段（挂新索引）完成之后、drain 之前——此后消费线程 diff 的对象已是新索引，无需记集合。
   - **daemon stop 与在途重扫**：stop 不 join 在途重扫线程（进程终止即终结）。正确性边界：stop 落盘与替换段落盘同受 `flush_lock` 互斥，不会交错损坏；最坏情形是重扫线程完成一次有效快照落盘后被终止，重启重放覆盖，无害。
4. **并发互斥**：单一 `rescan_lock: Mutex<()>` 仲裁两类路径。自动路径 `try_lock` 失败（重扫进行中）→ 放弃本次触发（dropped/流断开类本就该忽略；`rescanning` 期间再收 dropped 直接 `continue`，冷却常量保留为第二道闸）。exclude 路径同步 `lock()`——阻塞等待进行中的自动重扫（含替换段）完成后，再在锁内同步执行自己的重扫；最坏等待一轮完整重扫，与现状同量级（现状 exclude 自身同步重扫也是此量级）。
5. **exclude 路径保持同步的理由**：`exclude_op_lock` 的答复语义是「`Response::Exclude { ok: true }` 返回时排除项已生效」（daemon.rs:1108-1127：保存配置 → 替换内存 → 同步重扫 → 才回响应），GUI 设置页依赖此时序刷新。异步化会把「受理」伪装成「完成」，属对外可见行为变化；exclude 是用户显式低频操作，同步等待符合直觉。代价：exclude 重扫期间连接处理线程阻塞（现状已如此，不新增退化）。
6. 真重扫（自动源）仅剩触发场景：停机超阈零事件、流断开、建流失败、抽样对拍失败——频率天然极低（A3 验收 ≤ 2 次/24h）。

**风险**：替换段（`rebuild_dir_map` + flush 约 1–2s）在重扫线程执行，消费线程此刻的 diff 会在 holder 写锁上短暂阻塞；窗口 1–2s 内核缓冲不会溢出（对比现在 10–30s），可接受。S3 落地后替换段改走「flush → remap Base → 清 Overlay」（tech.md §4.3），不再物化堆索引。

---

## 2. P1 永久失败目录治理

**现状**：`apply_and_track`（daemon.rs:420-425）对 diff 失败只 `io_errors += 1`，映射与条目永留，无限重试。三类失败实测：`.Trash`/照片图库 = TCC 权限拒绝（永久不可成功）；`.example/*` 等已删目录 = ENOENT（删除事件被 dropped 丢弃后无人清理）；其余未知 errno 少量。

**设计**：`apply_and_track` 的 `Err` 分支按 errno 分派：

| errno | 动作 | 语义 |
| --- | --- | --- |
| ENOENT | `scanner::remove_prefix` 摘该目录子树 + 清 dir_map 键（幂等） | 磁盘已删 → 索引跟进删除；父目录后续 diff 不会重插（磁盘确无此项），无振荡 |
| EACCES / EPERM | **保留条目（子树为空）+ gaps 记账 + 负缓存**：`perm_denied: HashSet<PathBuf>`（canonical 目录路径）收录，后续 diff 请求对该目录短路返回 0 | 与首扫语义**一致**：首扫对 PermissionDenied 的处理是目录本体入索引（父 listing 收录）、`read_dir` 失败记 `gaps.permission`、子树为空（scanner.rs:6/107/185）。摘除反而制造插-摘振荡：父 diff 发现「磁盘有、索引无」→ 重插 → 自身 diff 又失败 → 又摘 → 无限循环 |
| 其他 | 连续失败计数（进程内 `HashMap<PathBuf, u8>`），同目录累计 3 次后按 EPERM 待遇入负缓存 + gaps | 防未知 errno 无限重试 |

- **负缓存键必须是路径而非条目号**：条目号在 `compact` 时全量重排（index.rs:322-368，父指针经 map 重映射），旧号 compact 后指向别的无辜条目 → 该目录 diff 被永久短路 → 内容漂移不更新，静默数据错误；且 P1 的 ENOENT 摘除与墓碑增多会频繁触发 `maybe_compact`（阈值仅墓碑占比 20%），非边缘场景。键用 canonical `PathBuf`（`apply_and_track` 手头即 `abs_dir`，零额外成本；权限目录数量极少，内存可忽略）。
- **负缓存失效语义**：进程内存活，重启清零——用户补授权限（完全磁盘访问）后重启 daemon 即自动重新索引；gaps 清单中提示「重启后生效」。不做运行期探活（权限恢复无事件信号，轮询徒增 IO）。
- gaps 条目格式复用 `invalid_roots_gaps` 的 `Gap { path, reason }`，reason 注明 errno 文案；status 已有 gaps 展示能力，无新协议面。去重上限 256 条。
- `diff_one` 补链路径（daemon.rs:445-469）同样受益：ENOENT 摘除后 `map` miss 不再对死目录补链；EPERM 负缓存使补链遇到的权限目录零开销跳过。
- 变更量计入 `dirty`，由既有落盘节流自然刷盘。

**验收锚点**：A4（日志增速）；`.Trash` 类路径零重复刷屏。

---

## 3. P2 重扫峰值瘦身 + 死页归还

**现状**：`collect_records` 的 `Rec::Dir { parent: PathBuf, name, raw: PathBuf }` / `Rec::File { parent: PathBuf, name }`（scanner.rs:150-154）逐条拷贝父路径；126 万条 → `Vec<Rec>` ≈ 200MB（结构体 72B/条 + 父路径串 ~100B/条）。`sort_by_cached_key` 另付 ~10MB 缓存（必要，保留）。

**设计**：`Rec` 改为目录引用 + 名称：

```rust
enum Rec {
    Dir  { dir_seq: u32, name: String },   // dir_seq = 该目录自身在目录表中的序号
    File { dir_seq: u32, name: String },   // dir_seq = 父目录在目录表中的序号
}
```

- `collect_records` 维护目录表 `dirs: Vec<PathBuf>`（队列出现过的目录原始路径，仅 ~20 万条 ≈ 20MB）。目录入队时先登记自身路径取得序号，再产出 Dir 记录引用之；File 记录引用父目录序号。
- **dir_map 键保真**：`apply_records` 聚合 Dir 记录时，raw 键**直接取 `dirs[dir_seq]`**（= 该目录自身的原始路径，classify_entry 的 `e.path()` 原始字节），无需任何拼接，天然保真非 UTF-8 名称（scanner.rs:141-143 注释记录的 raw 存在理由即此；按父路径 + lossy name 拼接会引入键漂移，明确不做）。深排序 key：Dir 用 `dirs[dir_seq]` 自身深度，File 用 `dirs[dir_seq]`（父）深度 + 1，拓扑序保证不变。
- **目录表并行化**：per-shard 线程本地 `Vec<PathBuf>` + `dir_seq` 全局编码为 `shard_idx << 20 | local_seq`（或合并时本地序号平移），延续「线程本地分片、无逐条全局锁」的既有设计（ADR-6）；不引入 Mutex 共享表。
- `Vec<Rec>` 结构体尺寸：`u32 + String + enum tag` 对齐后 **32B/条**（非 24B），126 万条数组 ~40MB；对照现状 72B/条 + 父路径串 ~100B/条（~190MB），临时峰值净降 ~150MB。

**峰值账（修正）**：S2 后单次重扫峰值 ≈ 双份索引 226MB（物化 113 + 重扫产物 113，S3 才消除）+ `Vec<Rec>` 40MB + 双 dir_map 80MB（旧 40 + 新建 staging 40）+ 写缓冲与滞留 ≈ **350–450MB**——故 A2 拆两档验收（product.md §3）：S2 后 ≤ 450MB，S3 后复核 ≤ 300MB。

**死页归还**：`degrade_and_rescan` 完成 flush 后、`maybe_compact` 完成后，调用一次
`malloc_zone_pressure_relief(malloc_default_zone(), 0, 1)`（libmalloc FFI，`unsafe` 单函数，集中放 engine 或 cli 的平台模块；GUI 侧同样接入，见 §6）。归还被 free 但滞留的大块脏页（实测 Malloc Large (empty) 254MB）。

**验证**：突变点 = `Rec` 恢复 parent 拷贝时 bench 首扫 footprint 采样应显著回升；单元测试钉 `dir_seq` 与目录表的对位（含跨 shard 场景、非 UTF-8 目录名的 raw 键等价）。

---

## 4. P3 索引驻留 mmap 化（达标主工程）

**现状**：冷启动 `load_mmap` → `SharedView`（mmap 只读，立即可查）→ `materialize()` 物化为 `Mutable Index`（113MB 堆：entries 30MB + 双名称池 83MB）→ 增量全程堆驻留 → flush 全量落盘。物化是增量的前提，却是 footprint 的主项。

**目标结构**：daemon 常驻 = **Base（mmap 只读，footprint≈0）+ Overlay（小内存增量）**；落盘时合并写全量 index.bin，重新 mmap 替换 Base，Overlay 清零。

### 4.1 为什么不是「Delta 全量物化」或「子树预枚举屏蔽」

- Base 无子树枚举能力（条目仅有 parent 上溯指针，无 child 索引；补 child 邻接表会膨胀文件且物化同等堆内存）。
- 因此屏蔽必须发生在**查询命中后的路径判定**，而非预枚举。这就是 Overlay 的目录级设计依据。

### 4.2 Overlay 设计

```rust
struct Overlay {
    /// 已被 diff 过的目录的 base 条目号集合（子项全集以 overlay index 中 anchor 的实际子项为准）
    overlaid: HashSet<u32>,
    /// base_dir_idx → overlay 内对应目录条目号（拼接 overlay 条目父链用）
    anchors: HashMap<u32, u32>,
    /// Overlay 自身的小型 Index（复用现有 Index 类型；parent 在 overlay 条目号空间内）
    index: Index,
}
```

**单一真相**：目录的直接子项全集只存于 overlay index（anchor 的实际子项），不另维护 children 映射——同一信息的两份表示会在二次 diff 时漂移。下次 diff 同目录时，旧子项集由 `index.children_of(anchor)` 动态派生（名称从 arena 读取），与磁盘 listing 比对产出 `DiffStats`。

**写侧（目录被 diff 时）**：
1. 目录 D（base 条目号 `d0`）diff：磁盘 listing 到手（`watcher::apply_dir_diff` 现有逻辑产 listing）。
2. Overlay 中确保存在 D 的 anchor 条目（parent 指向 overlay 内其 overlay 祖先的 anchor；D 尚无 overlay 祖先时 parent = 桥接占位，旁表记 `anchors[d0] = overlay_idx`）。
3. listing 与旧子项集（`children_of` 派生）比对得增删：删除的从 overlay index 移除（墓碑制），新增/变更的写入。
4. `overlaid.insert(d0)`：**Base 中 D 的直接子项从此作废**（查询侧屏蔽判定依据 `overlaid` 键集）。

**查询侧（`query::execute` 消费点）**：
1. 扫描两源：Base 全量 entries + Overlay index 全量 entries，Finder 匹配照旧。
2. **屏蔽判定（纯条目号链，零字符串比较）**——仅对 Base 命中条目 E，在与拼路径**同一次**父链上溯中顺带执行（拼路径本就逐级上溯，每级两次 HashMap 查询，零额外遍历）：

   ```
   valid(e)：                          # e 为 Base 条目号
     p = parent(e)
     p == NO_PARENT         → true     # 终止条件 1：到根，全链无 overlay → 有效
     overlaid.contains(p)   → anchors.contains(e)
                                        # 终止条件 2：最近祖先 p 被 diff，直接子项层以 listing 为准——
                                        # e 在 anchors（新 listing 中存活且为目录）→ e 子树转由 Base 裁定，e 有效；
                                        # e 不在 anchors（文件被 listing 取代 / 已删除）→ 无效
     其余                   → valid(p)  # p 未被 diff：p 的存活性由其父链裁定，递归上溯
   ```

   - **递归终止语义**：上溯途中遇到「有 anchor 但不在 `overlaid`（该目录从未被 diff）」的中间目录，即按 `valid(p) → true` 终止，该子树以 Base 为准——不要求更深层的 overlay 信息。
   - **不做名称逐段校验**：overlay index 中 anchor 的子项名称是 display 形态（磁盘 `file_name`），Base 拼路径用 match 形态（normalized），大小写/Unicode 归一差异会造成漏屏蔽——此即「结果含已删路径」风险的真正成因，结构化条目号判定从根上排除该类缺陷。
3. Overlay 条目拼路径：沿 overlay parent 链上溯至 anchor 根，切换到 base `dir_idx` 继续 base 链（两段拼接；实现为 `full_path_composite(idx)` 单一入口）。
4. 结果排序、limit、按需 stat 等下游不变。

**status 条目数**：`base.live_count + overlay.index.entry_count() − 被屏蔽 Base 条目数`。被屏蔽数未知（不预枚举），故显示为近似值（product.md §4.1 已声明协议值语义放宽）。

### 4.3 落盘与 Base 替换

- flush 逻辑不变（全量布局镜像写 index.bin）：合成视图一次性物化写出——实现为「Base + Overlay 顺序遍历产出写侧条目流」，复用现有 `persist` 写侧。
- 写毕 → 新 `Mmap` → `holder` 写锁内替换 `SharedView`、清 Overlay → 旧 mmap drop。全程在 `flush_lock` 串行域内（沿用既有锁纪律）。
- **节流参数放宽**：落盘不再承担内存压力缓解职责，`FLUSH_INTERVAL` 30s→120s、`FLUSH_DIRTY_LIMIT` 10k→30k（减少 113MB 全量重写与 remap 频率）；停机 flush 时机不变。
- 增量窗口内的崩溃恢复不变：水位语义（applied ≤ 内容）不受 Overlay 影响，重启重放覆盖未落盘变更。

**接口改造范围（评审补充，plan 工期依据）**——合成视图无法以现有签名零侵入实现：

| 现有接口 | 冲突 | 改造 |
| --- | --- | --- |
| `IndexView::entries() -> &[Entry]`（persist.rs:176，单连续切片） | 双源（Base + Overlay 各自连续）无法返回单一切片 | trait 拆分：查询消费点改迭代式 `for_each_entry` / 分段枚举，`query::execute`（query.rs:144）与全部消费点同步改 |
| `write_atomic(&Index, ...)`（persist.rs:44） | 写入对象从单一 Index 变为合成条目流 | 入参改条目流迭代器（Base 段 + Overlay 段顺序产出） |
| 冷启动 `materialize()` | P3 后常驻即 Base，不再物化 | 物化路径退役为 flush 合并的内部步骤；`cold_start` 直挂 SharedView |
| **P0 替换段衔接** | P0 现把重扫产物挂堆上 `Mutable`（daemon.rs:768），S3 后此举重新引入 113MB 堆驻留 | P0 替换段改走：扫描产物 → flush 落盘（条目流直写）→ remap 新 Base → 清 Overlay，全程不经堆物化；S1 实现时预留此切换点（S3 前临时挂 Mutable，S3 迁移） |

### 4.4 dir_map intern 化（并入本规格）

- 键从 `HashMap<PathBuf, u32>` 改为 `HashMap<u64, u32>`（键 = match 形态完整路径字节的 FxHash64），碰撞以 `path_pool` 字节二次比对消解（`SmallVec<u32>` 桶）。
- 路径字节仅存于 path_pool（该说法在 P3 后仅指 **Base 侧**；Overlay 目录的 intern 键由「overlay 父链段 + 自身段」双段拼接路径后 hash，存于 overlay index 自身的 path_pool），dir_map 由 ~40MB → **~12MB**。
- `diff_one`/补链/remove_prefix 逻辑不变，仅键编码与查表方式变；canonicalize 基址规则不变（`/tmp` 符号链接约定继续生效）。

### 4.5 内存账（1.26M 条目实机）

| 项 | 现状 | P3 后 |
| --- | --- | --- |
| 索引堆驻留 | 113MB | **0**（Base 为 clean file-backed mmap，不计 footprint） |
| Overlay | — | 日常 KB–数 MB（窗口内 diff 过的目录） |
| dir_map | ~40MB | ~12MB |
| 运行时 | ~20MB | ~20MB |
| **daemon 稳态 footprint** | **~175MB** | **~40–60MB** ✅ A1 |

### 4.6 风险表

| 风险 | 缓解 |
| --- | --- |
| 查询双源合并的命中序/去重错漏 | parity 对拍扩展：合成视图 vs 独立递归枚举，含「目录 diff 后立即查询」「overlay 嵌套两层」「子目录 diff 后父未再 diff」用例；突变验证屏蔽判定 |
| mask 判定漏屏蔽 → 结果含已删路径 | 对拍钉 + 「diff 后查询旧路径为空」专项测试 |
| flush 合并期间查询阻塞超时 | 合并遍历在 flush_lock 内、holder 写锁仅替换瞬间持有（现 flush 同构）；基准 A7 监控查询 P99 |
| mmap 替换瞬间的读一致性 | 沿用 holder RwLock；SharedView 生命周期与锁保护同构（现冷启动已走通 Shared 查询路径） |
| 条目数近似值误导 | GUI/status 文案「约 N 条」；flush 后自动精确 |
| 抽样对拍基准变更 | `sampling_finds_gap` 改走合成视图（与查询同一入口） |

---

## 5. P0–P4 与验收映射

| 改动 | 验收项 |
| --- | --- |
| P0 断振荡 | A3 |
| P1 失败治理 | A4 |
| P2 峰值瘦身 + pressure_relief | A2 第一档（S2 后 ≤ 450MB） |
| P3 mmap 化 + dir_map intern | A1（主）、A2 第二档（复核 ≤ 300MB）、A7、A8 |
| P5 GUI | A5（阶段线）、A6、A6b |
| 全程 | A8（三门禁 + 突变纪律） |

---

## 6. P5 GUI（306MB → 阶段一 ≤ 150MB → 阶段二 ≤ 100MB）

> 评审修订（2026-10-30）：初版「系统库硬下限 ~230MB」的判断**作废**——phys_footprint 不含 clean 库代码页，306MB 全部为可追究分配，逐项治理均有路径。分两阶段推进：阶段一为确定性手段（不需归因即可落地），阶段二凭 S0 归因数据冲 100MB。

### 6.1 构成分解（phys_footprint 306.7MB）

| 构成 | 大小 | 可压减性 |
| --- | --- | --- |
| `Malloc Large (empty)` 死页 | 73MB | ✅ 确定性：`pressure_relief` 归还（tessellation 每帧分配-释放滞留） |
| IOSurface | 78MB（66 个） | ⚠️ 阶段二：隐藏 viewport 释放 surface + drawable 池行为调查 |
| Malloc 活跃堆 | 49.7MB | ⚠️ 部分：结果行数据结构、字体数据、UI 状态 |
| CG Image | 18MB | ✅ 确定性：图标缓存上限 2048 → 128 LRU（现状 64×64 预乘 RGBA × 2048 条 FIFO，上界 ≈32MB）+ 释放路径确认 |
| IOAccelerator (graphics) | 19MB | ⚠️ 跟随 IOSurface/GPU 纹理治理 |
| 其余（CoreText 缓存、AppKit 对象、GL 兼容层内部） | ~68MB | ⚠️ 阶段二：S0 归因细分后逐项判定 |

渲染后端为 eframe 0.37 **glow**（OpenGL，经 `AppleMetalOpenGLRenderer` 兼容层桥接），IOSurface 66 个的主要嫌疑是 CAOpenGLLayer 的 drawable 池与图层树，非三个视口的固有开销。

### 6.2 阶段一（确定性手段，目标 ≤ 150MB）

1. **消灭 CJK 字体回退慢路径（峰值主修复，S0.3 归因定案）**：注入覆盖 CJK 的字体源使中文 galley 布局直接命中，`egui_system_font_provider`/fontique 的 fallback（`Family::resolve_slow` → `Query::matches` 191MB×3 + `load_blob` 苹方 ttc 整块载入 76MB×2，实测复现 peak 4.3GB）只在极生僻字形时偶发。实现取向：egui `FontInsert` 注入 CJK 覆盖字体（字形全集，~10MB 一次性内存）置于 Proportional/Monospace 高优先级；拉丁仍 SF Pro/SF Mono。`system_fonts` feature 的保留与否随实现评估（去掉则 fontique 不参与，需确认默认字体面覆盖）。
2. **死页归还**：viewport 空闲（无输入、无滚动）60s 后 `malloc_zone_pressure_relief`（`platform/mac.rs` unsafe 区）；对 `Malloc Large (empty)` 死页（峰值时实测 3GB/362 块滞留）。
3. **图标缓存治理**：现状已是 worker 回传 64×64 预乘 RGBA + 路径缓存上限 2048 条 FIFO 淘汰（`ui/icons.rs:54` `MAX_PATH_ICONS`），上界 64×64×4×2048 ≈ 32MB（与实测 CG Image 18MB 同量级）。治理：上限 2048 → 小上限（128）LRU + 确认缓存淘汰路径上 egui 纹理 handle 与 CG Image 的显式释放；预期 -15MB 以上。
4. **字形图集上限**：`max_texture_side` 8192→4096（单块重烘 256MB→64MB 上限），封顶保险（归因证实峰值主因不是图集，此项降级为防扩散）。
5. **堆粗查**：`heap` 定位活跃堆中的明显异常块，易修则修。

### 6.3 阶段二（归因驱动，目标 ≤ 100MB；S0.3 归因结论已回填）

**归因结论（2026-10-30，MallocStackLogging 复现实测）**：

- **峰值真凶**：结果窗口打开 + 中文文件名列表 → 每个 galley 布局遇 CJK 字形走 `egui_system_font_provider` → fontique 系统回退慢路径——`Query::matches` 单次分配 191MB×3、`SourceCache::load_blob` 把苹方 ttc 整块载入 76MB×2；galley 缓存 miss 反复触发，mmap 大块堆积，malloc 归还滞后 → `Malloc Large (empty)` 3GB/362 块滞留 → 复现 peak 4.3GB。**嫌疑序中的 tessellation 巨量与字形图集多轮扩容均被排除**（Malloc Large 活跃仅 12.8MB，纹理在 IOSurface/IOAccelerator 侧且未涨）。
- **IOSurface 归属（常驻态 85.8MB/28 个，结果窗口关闭也不释放）**：2160×1254 BGRA 10.4MB×3 = 结果窗口 drawable 三缓冲（IOGPUSurfaceMTL + AppKit 镜像）；1400×492 2.7MB×4 = 悬浮条；1240×1200 5.8MB×2 = 第三视口。滚动时 `NSIOSurfacePool nextUnusedSurface` 增配 18.8MB 块。处置：视口关闭显式释放 surface（egui viewport 生命周期钩子；drawable 池行为随实现调查）——预期常驻 -55MB。
- **CG Image**：8–18MB（图标，随查询内容浮动），§6.2.3 治理已覆盖。

**剩余探索项**：视口 surface 释放（上述）；CoreText 栅格化缓存驻留限制；每帧分配审计（style 克隆热点）。修复项落地后按数据决定是否继续冲 ≤ 100MB 或回评审定线。

### 6.4 阶段二出口条件

- 达成 ≤ 100MB：写入验收。
- 归因后存在不可消除构成项（GL 兼容层最低 drawable 池、系统字体缓存等，逐项附实测数字）：携数据回评审重新定线——**不事前拍定下限，也不无限追**。

**验收**：阶段一 A5（≤ 150MB）+ A6 + A6b；阶段二 A5（≤ 100MB，凭据驱动）。

---

## 7. 不变量（全改动共用）

- FSEvents 事件只是提示、以磁盘为准（项目核心设计，P0 重放目录集合依赖此语义）。
- 排除判定共用 `scanner::is_excluded`，三路径不另写判定。
- 落盘 `flush_lock` 串行化 + `write_atomic`；新增落盘路径同样串行。
- FSEvents 流拆除在所属 runloop 线程；`/tmp` canonicalize 对齐；重放追平语义不变。
- 协议消息面不变（字段值语义放宽仅「条目数」一处，product.md 已声明）。

# macEverything v1-core 技术规格

**Status:** 已确认(实现完成,实测回填;含审查修复轮)
**version:** 1.3.0
**prev_version:** 1.2.0
**updated:** 2026-10-16
**产品依据:** product.md v1.2.0(产品已确认)
**changes:**
- 新增 ADR-5:FSEvents C API 直调 + sinceWhen 停机重放(弃 notify 封装层)
- 新增 ADR-6:扫描器并行遍历(work-stealing),首扫预算收紧至 ≤ 5 秒
- 索引文件 header 增加 last_event_id;冷启动管线加重放步骤;依赖、风险、测试同步更新
- (1.2.0)实现完成实测回填:性能预算表五项实测数字;新增实现偏差清单(Entry 24B/
  折叠实现/聚合交织序/根名绝对路径/path 跨段限制);查询管线补 Finder 预编译
  (CPattern,主开销消除);增量补链与重放追平语义、落盘串行化入档
- (1.3.0)代码审查修复轮回填:落盘水位改「已应用水位」语义并配套退出排空;合批窗口
  自首事件起算;重放不可用判定加抽样对拍(补「部分回放」缺口);首扫流式聚合
  (场景 2 落地);结果行 stat 并行化 + stat_ms 字段;exclude 补扫描根内校验与
  失败原因;协议删死字段 offline/built_at;error.rs 空模块移除(错误类型按模块分置)

---

## 现状分析

绿地项目,无存量代码。技术现状与约束:

- 仓库为空(仅 specs);目标机 Apple Silicon + rustup stable。
- 引擎的全部外部行为依赖两项系统能力:目录遍历(std 文件系统调用)与文件变更事件(macOS FSEvents,经 `notify` crate 封装)。
- FSEvents 语义为粗粒度合并(官方文档确认):改名可能只报单边、事件可被系统合并丢弃(以 Rescan 标记告知)。因此**增量正确性只能建立在「目录级重扫 diff」上,事件只当触发信号**。此结论是本设计的地基。
- FSEvents 同时具备历史重放能力:`FSEventStreamCreate` 的 `sinceWhen` 参数可从系统持久化事件日志(`.fseventsd`)的指定事件 ID 重放历史——这是 macOS 上最接近 NTFS USN Journal 的机制,是停机窗口补齐的基石。日志有滚动保留期,重放不可用必须回退全量重扫。
- 原生事件源别无选择:kqueue 每目录占一个 fd(全盘目录数下不可扩展);APFS 不向用户态暴露文件表;USN 类日志不存在。**准确性与性能由架构层保证(并行遍历 + diff 唯一真相 + 重放/重扫),不依赖机制本身的精细语义。**
- 查询性能不受任何外部约束:索引完全自持,匹配内核可用指令级优化(memchr 系列 SIMD 子串搜索)。

## 架构变更

绿地无「变更」,以 ADR 形式记录四个关键决策:

### ADR-1 纯 Rust 双 crate,GUI 依赖零引入

- **Context**:v1 只验收引擎(快/准/全),图形界面 v2 再加;若 v1 引入 GUI 框架,验收面与依赖面同时膨胀。
- **Decision**:Cargo workspace 两 crate——`engine`(库,全部业务逻辑)+ `cli`(二进制,子命令分派与 daemon 通信)。v2 增加 `app` crate(egui),`engine` 不动。
- **Alternatives Considered**:单 crate(Rejected:引擎与命令行解析耦合,测试与 v2 复用不便);engine+cli+daemon 三 crate(Rejected:v1 daemon 只是 engine 的一种宿主形态,`cli` 内含 daemon 启动路径即可,不值得独立 crate)。
- **Consequences**:engine 对 GUI 零依赖,公开 API 即 v2 的接入面;代价是 daemon 代码放 cli 内,需纪律防止业务逻辑漏进 cli。

### ADR-2 索引 = 扁平数组 + 名称池,删除用墓碑

- **Context**:百万条目增删改查,父指针共享目录名省内存;swap-remove 会让「父目录编号」失效。
- **Decision**:条目定长结构 `Entry { name_start: u32, name_len: u32, parent: u32, flags: u8 }` 存 `Vec<Entry>`(下标即条目号);文件名连续存放于名称池(`Vec<u8>` arena)。并行维护两份 arena:`display`(原始字节)与 `match`(NFC 归一 + Unicode 大小写折叠,匹配专用;折叠覆盖整个 Unicode 范围,é/É 互搜,满足「默认大小写不敏感」产品语义)。删除置 `flags` 墓碑位;墓碑占比超阈值触发整理(重建数组,复用重扫或压缩遍历)。
- **Alternatives Considered**:HashMap<String, Node>(Rejected:每条目独立堆分配,百万级内存与缓存局部性双差);BTree/trie 定位(Rejected:SIMD 线性扫 20MB 级 arena 已 <10ms,定位结构反而增加构建与维护成本——与 product.md 目标 2/3 的预算分配一致)。
- **Consequences**:查询是顺序扫 arena(缓存友好);内存 ≈ 条目 16B + 名称双份 + 目录路径池(百万条目 ≈ 60MB + 路径池 10-20MB ≈ 70-80MB),远低于 500MB 红线;代价是墓碑整理与路径池维护逻辑需测试覆盖。

### ADR-3 落盘 = 镜像内存布局 + mmap 零拷贝加载

- **Context**:冷启动 <1s 是产品红线;反序列化路径(serde 类)百万条目需数百毫秒到秒级。
- **Decision**:索引文件布局与内存布局同构:`header(magic/version/built_at/entry_count/last_event_id/roots/excluded)` + `display_arena blob` + `match_arena blob` + `entries 定长数组`(整体 `#[repr(C)]`,16B/条)。加载用 `memmap2` 映射后直接转 `&[Entry]` 与 `&[u8]` 视图,零反序列化。写入 = 快照 + 临时文件 + `rename` 原子替换。
- **Alternatives Considered**:rkyv(Rejected:引依赖做「布局镜像」这件自己五行代码能做的事,且校验/兼容黑盒);SQLite(Rejected:加载走 B-tree 逐条读,冷启动目标直接破产)。
- **Consequences**:格式版本字段 + magic 校验兜住损坏/升级;daemon 运行中采用「mmap 只读视图 → 后台物化为可变索引」两段式,物化(拷贝 ~60MB,几十毫秒)完成后切可变、开始停机重放,冷启动全程可查。

### ADR-4 daemon 通信 = Unix domain socket + 长度前缀 JSON,同步线程模型

- **Context**:daemon 与 query/status/exclude 命令间需要本机 IPC;查询 QPS 极低(人手速),无高并发诉求。
- **Decision**:`std::os::unix::net::UnixListener` 同步线程,每连接一线程处理完即断;协议 = 4 字节小端长度 + UTF-8 JSON。socket 路径固定于应用数据目录,权限 0600。单实例判定:绑定遇 `EADDRINUSE` 时先 `connect` 探测——连接成功 = 已在运行(报错退出);`ECONNREFUSED` = 崩溃残留 stale socket,unlink 后重绑。
- **Alternatives Considered**:tokio + 异步框架(Rejected:为一个人手速场景引入运行时复杂度,违背 v1 最小依赖原则);gRPC/protobuf(Rejected:同上,量级失配)。
- **Consequences**:实现 ~200 行,无新依赖;JSON 序列化微秒级,不进性能预算关键路径。

### ADR-5 FSEvents C API 直调,以 sinceWhen 实现停机重放

- **Context**:`notify` 封装固定从「现在」开始监听,不暴露 `sinceWhen`,停机窗口只能全量重扫(成本 O(全盘) 且不随窗口缩小);FSEvents 框架本身支持从持久化日志(`.fseventsd`)的指定事件 ID 重放历史,是 macOS 上对标 USN Journal 的唯一机制。
- **Decision**:engine 直调 FSEvents C API(经 `fsevent-sys` 绑定),自建 stream 生命周期管理;每次落盘把**已应用水位**(一批目录 diff 全部完成后推进的最大事件 ID)写入索引文件 header 的 `last_event_id`;daemon 重启后以 `last_event_id` 为 `sinceWhen` 创建流,重放停机窗口事件(走与实时事件同一条目录 diff 管线),收到 HistoryDone 标记即追平转纯实时。**spike 结论(2026-10-15)**:过旧 sinceWhen 行为 = 从保留窗口起点全量回放并正常收尾(since = 0、1、当前ID÷2、当前ID−10⁷ 四点行为一致,回放段末尾收到 HistoryDone,其后流正常转实时);可观测信号 = 无(MustScanSubDirs / UserDropped / KernelDropped / EventIdsWrapped 全程未出现);回退设计按纯保守判定器定稿——重放零事件且停机时长超阈值(默认 48h 可配置)触发全量重扫;重放异常直接重扫。
- **Alternatives Considered**:继续用 notify + 全量重扫兜底(Rejected:浪费已知信息,重扫成本恒定不降);kqueue 自建事件源(Rejected:fd 预算撑不住全盘目录数);APFS 磁盘结构直读——即 mac 版 MFT(Rejected:FileVault 加密、SIP、格式无官方承诺随 macOS 版本漂移,风险收益完全失配,且只解决枚举不解决增量事件);「日志已滚动」信号判定器(Rejected:spike 证实信号不存在,无从订阅)。
- **Consequences**:engine 引入 ~200 行 unsafe FFI,集中于 `fsevents.rs` 单文件;停机补齐从 O(全盘) 降为 O(停机变更);回退判定不依赖未证信号,保守判定器保证「禁止静默遗漏」。丢弃标记(事件风暴下 FSEvents 客户端队列溢出)的处置为**去抖重扫**:登记待命,事件安静 2s 后执行一次,持续风暴 60s 强制,触发后 30s 冷却;回调按事件计费,转换缓冲必须循环外复用(旧实现每事件 4KB 栈清零,10 万事件压力下实测丢弃 2 条,优化后 0 条)。
- **spike 实测要点**(`crates/engine/examples/spike_fsevents.rs`,Apple Silicon + macOS 26,单卷验证):HistoryDone(flag 0x10)仅在 sinceWhen 为真实历史 ID 时出现,恒为回放段最后一条事件、flag 位只有 0x10、id 等于回放段最后事件 ID;SinceNow 建流不产生 HistoryDone——daemon 落盘的 last_event_id 恒为真实 ID,「收 HistoryDone 即追平」的设计成立。SinceNow 非精确切点:建流前发生、仍缓冲在 fseventsd 的变更会以大于当前 ID 的形式送达,故事件处理须幂等(目录 diff 天然幂等)。回放段事件 ID 非严格递增(同路径多事件合并、跨路径乱序),水位线必须取批内最大 ID。回放延迟随深度显著增长:since 取最近历史 ID 时回放段 13ms,since 取 0/1/当前ID÷2 等陈旧区时实测 4-14s(同深度各点波动大,首扫为主)。runloop 拆除(ScheduleWithRunLoop 自 macOS 13 标记 deprecated,仍可用)必须在流所调度的线程上执行,跨线程 Invalidate 实测 SIGTRAP。

### ADR-6 扫描器并行遍历(work-stealing)

- **Context**:首扫性能是「快」轴红线;macOS 无 MFT 可直读,串行 readdir 遍历百万文件 5-20 秒,比 Everything 慢一个量级。APFS 为 B+ 树写时复制文件系统,并发随机读扩展性极好,NVMe 深队列下多线程 readdir 近线性加速。
- **Decision**:并行扫描器:全局目录队列 + 每线程取目录遍历、子目录回灌队列(work-stealing),活跃线程数 = 物理核数(8-16);各线程条目写入分片缓冲,按队列顺序聚合后单线程入索引,保持 arena 布局确定性;权限缺口与 IO 错误线程本地聚合再合并。
- **Alternatives Considered**:串行遍历(Rejected:无法达到「惊艳」目标);`getattrlistbulk` 批量属性采集(Deferred:v1 首扫不采属性,该「遍历+属性合一」接口留 v2 列信息加速再评估);rayon `par_bridge` 递归(Rejected:目录树形状不规则,共享队列比 par_bridge 负载均衡好且行为可控)。
- **Consequences**:首扫预算从 ≤ 20s 收紧至 ≤ 5s(预期 1-3s,与 Everything MFT 直读同档);扫描器需并行正确性测试(多线程结果与串行一致、缺口合并无损)。

## 变更方案

### crate 与模块划分

```
macEverything/
├── Cargo.toml               # workspace
├── crates/
│   ├── engine/
│   │   └── src/
│   │       ├── lib.rs           # 公开 API:Engine 句柄(start/stop/query/status)
│   │       ├── entry.rs         # Entry 结构、flags(墓碑/目录位)、repr(C) 常量
│   │       ├── arena.rs         # 双 arena(display/match)构建与 NFC+小写折叠
│   │       ├── index.rs         # 内存索引:构建/增删改/目录 diff/子树级联/墓碑整理/目录路径池
│   │       ├── scanner.rs       # 并行遍历:work-stealing 目录队列/排除/符号链接跳过/缺口收集
│   │       ├── query.rs         # 语法解析(词法)→ 计划 → 匹配(memmem)→ 打分排序(path: 走目录路径池)
│   │       ├── persist.rs       # 落盘快照写、临时文件 + rename、mmap 加载与校验、last_event_id
│   │       ├── fsevents.rs      # FSEvents C API 直调封装:stream 生命周期、sinceWhen 重放、事件 ID 追踪
│   │       ├── watcher.rs       # 事件→目录集、100ms 合批(窗口自首事件起算)、重放调度、Rescan/日志滚动兜底
│   │       ├── reconcile.rs     # 兜底全量重扫入口:should_degrade 判定、full_rescan、排除列表展开
│   │       └── config.rs        # 排除列表读写(toml)
│   └── cli/
│       └── src/
│           ├── main.rs          # 子命令分派:start/stop/query/status/exclude
│           ├── daemon.rs        # UnixListener 服务循环、单实例、退出落盘
│           ├── protocol.rs      # 请求/响应 JSON 结构(与 engine 解耦的传输层类型)
│           └── client.rs        # query/status/exclude 的客户端连接逻辑 + 离线查询
├── docs/specs/v1-core/
├── scripts/
│   ├── bench.rs / bench.sh      # 合成百万树 + 五项基准(首扫/查询/增量/落盘/停机重放)
│   └── parity-check.sh          # find 对拍完整性
└── tests/                       # 集成测试:对拍、落盘回环、daemon 生命周期
```

### 关键机制

**扫描器**:并行遍历(ADR-6)——共享目录队列(work-stealing),活跃线程 = 物理核数;每线程迭代读目录(`read_dir`),子目录回灌队列;跳过符号链接与排除子树;`PermissionDenied` 单独分类进缺口清单(线程本地聚合),其他 IO 错误计数不中断。产出 = (分片条目集, 缺口清单),按队列顺序聚合入索引,保证 arena 布局确定性。

**查询管线**:
1. 解析:按空格分词;`path:`/`case:`/`whole:` 前缀提为计划级开关(`path:` 作用于其后续一个词;`case:`/`whole:` 作用于整轮);`*`/`?` 编译为「锚定子串序列」(如 `*.pdf` → [ends-with ".pdf"],退化为 memmem 序列 + 首/尾锚标记)。
2. 匹配:`rayon` 对 `entries` 分块,块内对每条目在 `match_arena` 对应切片跑锚定子串序列 AND。**Finder 预编译**(实现发现的关键优化):`memmem::find` 每次调用重建 Finder,百万条目逐条调用的重建开销是查询主开销(实测 P99 51ms);`CPattern` 在 execute 入口把每段编译为 `Finder::into_owned()`(memchr),块内只做 SIMD 扫描(P99 → 3.6ms)。`case:` 命中后回 `display_arena` 复核(NFC 直通零拷贝:`is_nfc` 判定,仅真非 NFC 形态才归一分配)。
3. `path:` 匹配走**目录路径池**:每个目录条目维护完整路径(归一化形态,含扫描根前缀)于独立 arena(`path_pool`,~20-40 万目录 ≈ 10-20MB),`path:` 词对池 memmem 直扫 <2ms;命中目录的直接子项即候选,文件项自身路径 = 父目录路径 + 名称(短串拼接判定)。目录改名/增删时增量管线同步维护受影响子树的池条目(重放与实时同一管线,风暴场景成本 O(子树))。此池消除 `path:` 单独成查询时「百万条目逐条拼路径」的性能破产路径。
4. 打分排序:App 包置顶(`is_dir` 且 display 名以 `.app` 结尾、ASCII 大小写不敏感;列头排序不受影响),组内按前缀命中(3)> 词边界命中(2)> 子串命中(1),同级按 `name_len` 升序;top-limit 选择(O(n) 划分 + 前 k ≤ limit 排序),比较子含 idx 决胜成全序,截断集与稳定排序一致。

**增量管线**:FSEvents 回调线程收事件 → 目录路径去重入集 → 100ms 合批(**窗口自首个 pending 事件起算**,到期即 drain,持续风暴不无限推迟)→ 每目录 `read_dir` 与索引 diff(该目录 `parent` 的直接子项)→ 增/删(墓碑)/改名(diff 天然表现为删 + 增,子树级联 = 目录条目下 `parent` 重挂 + 路径池级联重写)→ 变更计数进落盘节流。收到 RESCAN 标记、监听器错误或重放不可用判定 → 触发 `reconcile`(全量重扫,复用并行扫描器,后台线程)。

**停机重放**:daemon 启动 → mmap 加载读出 `last_event_id` → 以其为 `sinceWhen` 创建 FSEvents 流(ADR-5)→ 重放事件走与实时事件同一条目录 diff 管线 → 收到 HistoryDone 标记即追平,转纯实时。**不可用判定(spike 定稿,ADR-5)**:过旧 sinceWhen 无可观测信号,重放不可用认两路判定——①零事件且停机时长 > 阈值(默认 48h,配置项);②**抽样对拍**:追平后随机抽 100 个已索引目录做 diff,差异目录占比超 5% 判重放不可信(spike 证实过旧 sinceWhen 会「从保留窗口起点部分回放」,有事件但丢前段,零事件判定覆盖不到)。重放全程旧索引可查,状态「补齐中」;转重扫后状态「重扫中」。

**落盘节流**:变更计数器;worker 每轮检查「距上次落盘 ≥ 30s 或累计 ≥ 10000 条」→ 触发快照线程;**落盘水位取「已应用水位」**(消费线程应用完一批目录 diff 后的最大事件 ID,与「接收水位」分离)——保证落盘水位 ≤ 索引内容对应的事件,「已收未应用」的变更重启后必被重放覆盖;退出/停止前强制排空 pending。快照走 `<file>.tmp` → fsync → rename。`stop` 经 socket 消息触发同一快照路径,响应在落盘完成后返回;SIGTERM 走同路径兜底。

**冷启动**:`start` → persist 校验 magic/version → mmap → `IndexView::Shared` 立即可查 → 后台线程物化 `IndexView::Mutable`(从 mmap 拷贝)→ 物化完成即启动停机重放(sinceWhen = last_event_id,ADR-5)→ 重放毕重落盘并保持 `Mutable`;重放不可用才走全量重扫。查询代码只面对 `&[Entry] + &[u8]`,两视图同构,无分支扩散。**首扫流式**(product 场景 2):无落盘索引时先挂「仅含根」的可变索引,遍历与聚合分离(collect_records → 按 50k/批 apply_records),批间释放写锁——首扫期间 query 返回已扫部分、status 可见进度;首扫水位在扫描**之前**取,建流后重放补齐扫描窗口变更。

### 性能预算分配(对应 product.md 目标 1~6)

**实现完成实测(2026-10-16,Apple M5 + NVMe,合成百万树,`scripts/bench.sh`)**:

| 指标 | 预算分解与实测 |
|---|---|
| 首扫 100 万 ≤ 5s | 并行 readdir(ADR-6,物理核)+ 名称写入;不 stat 是达标前提。基线实测:单线程吞吐 ≈ 187 万条/s;端到端**实测 1.0-1.6s**(红线余量 3-5 倍) |
| 查询 P99 < 10ms | **Finder 预编译是关键**:`memmem::find` 每次调用重建 Finder(needle 预处理),百万条目逐条调用曾实测 P99 51ms;`CPattern` 预编译(memchr `Finder::into_owned`,execute 入口一次)后**实测 daemon 端 P99 2.9-3.6ms**(中位 ~2ms;系统噪声样本偶见 40ms,安静环境稳定) |
| 冷启动 < 1s | mmap 建映射毫秒级;物化 + 停机重放后台进行,不阻塞首查;实测秒开(轮询粒度 0.1s 内即就绪) |
| 停机重放 ≤ 2s(万级事件) | 重放走目录 diff 同管线;万级事件实测 0.03s 内追平(事件按目录聚合,单目录风暴合并为一次 diff)。**追平语义**:HistoryDone 前强制 drain 全部 pending 事件再置就绪 |
| 增量 ≤ 500ms | 100ms 防抖 + 目录 rescan + 应用;**实测 0.20-0.29s**(含查询往返,粒度 ~50ms);新目录事件先于父 diff 到达时经「补链」逐级向下 diff |
| 落盘不影响查询 | 独立线程 + 读锁快照;63MB 实测 0.05s;`flush_lock` 串行化 + tmp 名唯一化(pid+seq)防并发落盘互踩 |
| 空闲 CPU 0% | 无定时器;全部 await 事件(FSEvents 回调线程阻塞等待) |

**已知实现偏差**(与初始设计的差异,均已落代码注释):
1. `Entry` 为 24 字节(非 16):display/match arena 因折叠变长不平行,需各自偏移;百万条目内存 ~90-100MB(仍远低于 500MB 红线)。
2. 折叠用 std `str::to_lowercase()`(覆盖 é/É;ß 类特殊折叠除外)。
3. 并行聚合 = 线程本地分片按交织序上收 + 深度拓扑排序(跨 shard 的父先于子由排序保证):路径集合确定,arena 布局不保证字节级稳定;roots 不得重叠(debug 断言执法)。
4. 根条目名 = 扫描根完整绝对路径(full_path 输出即可直接使用的绝对路径)。
5. `path:` 词不横跨目录段与文件名(目录池无文件名、文件只取自身名);文件侧「父路径 + 名称短拼」留待需求出现。

## daemon 通信契约(适配表 3)

socket 路径:`~/Library/Application Support/macEverything/daemon.sock`,权限 0600;协议:4 字节小端长度 + UTF-8 JSON,长度上限 1 MB(超限断连);每消息首字段 `v`(协议版本,当前 1),daemon 不识别即拒绝;`exclude.add` 的 `path` 必须为扫描根内绝对路径,否则拒绝;`exclude.remove` 接受**已存在的条目原文**(含 `node_modules` 等相对名);`exclude.reset` 无参数,恢复内置默认四条。`exclude.add`/`remove`/`reset` 共用 daemon 变更锁(校验→内存→落盘→重扫整体串行),防跨连接交错。

| 消息 | 方向 | 请求字段 | 响应字段 | 说明 |
|---|---|---|---|---|
| query | client → daemon | `query: string`(原始语法串)、`limit: int ≤ 1000` | `entries: [{name, path, size, mtime}]`、`total: int`、`elapsed_ms: int` | `size`/`mtime` 懒 stat 填充 |
| stop | client → daemon | `{}` | `ok: bool`、`final_entry_count: int` | daemon 执行最终落盘快照后响应,随后进程退出;收到响应即落盘已保证 |
| status | client → daemon | `{}` | `state: building/ready/reconciling/degraded`、`entry_count: int`、`gaps: [{path, reason}]`、`io_error_count: int`、`built_at: int`、`index_file: string` | 状态契约唯一出口;daemon 不在时 client 读索引文件头自答并标 `offline: true` |
| exclude.add / exclude.remove | client → daemon | `path: string` | `ok: bool`、`excluded: [string]`、`message?: string` | 变更即生效并触发受影响范围重扫;失败(路径越界/落盘失败)回 ok=false + 可操作原因 |
| exclude.list | client → daemon | `{}` | `excluded: [string]` | 读操作,不加变更锁 |
| exclude.reset | client → daemon | `{}`(无参数) | `ok: bool`、`excluded: [string]`、`message?: string` | 恢复内置默认 `~/Library`/`node_modules`/`.git`/`~/.Trash`;先落盘默认配置再替换内存并一次重扫,保存失败不改内存;新增请求不 bump 协议版本,旧 daemon 解码失败拒绝,须停止/重启引擎 |

离线模式(query 时 daemon 不在):client 直接 mmap 索引文件执行同一查询管线,响应加 `offline: true` 与 `built_at`,CLI 输出标注「离线索引」。

## 技术实现决策表(适配表 5)

| 维度 | 决策 |
|---|---|
| crate 划分 | workspace = `engine`(库)+ `cli`(二进制);v2 增 `app`;业务逻辑禁入 `cli` |
| 依赖(engine) | fsevent-sys + core-foundation-sys(FSEvents 直调,ADR-5)、memchr(SIMD memmem)、rayon(查询分块并行)、unicode-normalization(NFC)+ unicode-case(全 Unicode 大小写折叠)、memmap2(加载)、serde+toml(配置)、thiserror+log(错误与日志);禁引 async 运行时 |
| 依赖(cli) | serde_json(协议)、anyhow(用户报错);引擎错误在边界转可读文案 |
| 存储层 | 内存 = 扁平 Entry 数组 + 双 arena;持久 = 同构布局文件 + mmap;删除 = 墓碑 + 阈值整理 |
| 并发模型 | watcher 线程 / 增量 worker / 快照线程 / `RwLock<IndexView>`;查询走 rayon 读锁;禁 shared mutable 无锁技巧。协议细节:写锁批量应用(单批 <10ms);快照取短读锁复制句柄后无锁序列化(arena 追加式,快照期间新写入不进本快照,由下次落盘补);墓碑整理走双缓冲重建,期间增量入暂存队列,重建完成原子 swap 后 100ms 内应用,暂存窗口不计入增量 ≤500ms 红线 |
| 进程通信 | Unix socket + 长度前缀 JSON;同步线程每连接一线程;单实例 = 绑定失败 |
| 错误处理 | 错误类型按模块分置(LoadError/StreamStartError/ScanGaps;统一 EngineError 无消费方,不建空抽象);缺口计数不中断;daemon 日志落文件(数据目录 daemon.log);禁 catch 静默 |
| 测试策略 | engine 全模块单测(TDD)+ 集成(落盘回环/对拍/daemon 生命周期)+ 基准脚本;CLI 实机验证 |

## 风险与应对

| 风险 | 应对 |
|---|---|
| FSEvents 语义粗粒度,丢事件 | 设计已内建:目录 diff 唯一真相 + RESCAN 标记/监听器失效触发全量重扫;增量时效验收(≤500ms)含风暴场景 |
| mmap 视图与运行中变更不一致 | 两段式:Shared(只读)期间增量暂存队列,物化完成后统一应用;重扫兜底 |
| 墓碑膨胀(长期高频删改) | 墓碑占比 > 20% 触发整理;rename/级联重写的 arena 孤儿字节同样进触发条件(orphan_pool_bytes > 32MB);整理走压缩遍历,毫秒级/万条,后台执行 |
| FFI 封装缺陷(自调 FSEvents C API) | unsafe 集中于 `fsevents.rs` 单文件;集成测试覆盖增删改/改名/RESCAN/重放/回退六类路径;`fsevent-sys` 绑定与 notify 同源,成熟度有背书 |
| `.fseventsd` 日志滚动(停机过久事件 ID 失效) | spike 证实滚动无可观测信号(ADR-5),不设信号路径;保守判定器(零事件长停机)触发即回退全量重扫;回退路径有测试;重扫期间旧索引可查,产品语义已定义 |
| 权限探测误判(把慢当拒绝) | 缺口分类只认 `PermissionDenied` 实际错误码;超时类错误计数但不进缺口清单 |
| socket 被第三方连接 | 0600 权限 + 数据目录内路径;协议无写操作,风险面仅查询 |
| 基准数字不可复现(SSD 缓存热) | 基准脚本:同一树跑 3 轮取中位,输出注明热缓存;合成树每次重建于临时目录 |
| 停机丢增量(补齐窗口) | 产品语义已接受(场景 1):秒开旧数据 + 重放补齐,补齐完成前 status 如实显示 reconciling;零事件长停机由保守判定器强制转重扫,杜绝静默遗漏 |

## 测试与验证

- **前置 spike(实现最先做)**:两项均已完成并回填——① sinceWhen 过旧行为(2026-10-15,结论见 ADR-5:无可观测信号,回退按纯保守判定器;程序 `crates/engine/examples/spike_fsevents.rs`);② readdir 吞吐基线(结论见性能预算表)。
- **单测(TDD)**:语法解析(六语法 + 非法输入)、匹配器(大小写/整词/归一化/通配锚定/全 Unicode 折叠)、索引(增删改/目录 diff/子树级联/墓碑整理/路径池维护)、persist(写读回环/坏 magic/版本不识别/原子替换/last_event_id 读写)、reconcile(差异应用)、arena(NFC 两种形态互搜)、scanner(并行聚合确定性:受控目录树下多线程结果与串行一致、缺口合并无损)、协议(长度上限/版本字段/path 校验/stale socket 重绑)。
- **集成**:合成树对拍(`find` 比对零差异)、落盘回环、daemon 生命周期(start 单实例/stop 落盘/杀进程恢复/离线查询)、停机重放(stop 后改文件 → start 重放补齐)、保守判定器(模拟长停机零重放 → 自动转全量重扫)、日志滚动回退(模拟 last_event_id 过旧 → 回退全量重扫)。
- **基准**:`scripts/bench.sh` 五项数字对照 product.md 目标;验收时实机跑。
- **命令链**:本地 `cargo fmt --check && cargo clippy -- -D warnings && cargo test` 全绿为声明完成前置。
- **实机验证**:`start` → 首扫 → Finder 增删改文件 → 500ms 内可查 → `stop` → 重启冷启动 <1s → `query` 对拍抽验 → 未授权完全磁盘访问时 `status` 缺口如实。

## 后续工作

- v2:`app` crate(egui 悬浮条 + 完整窗口 + 设置)、全局快捷键、i18n、登录自启、完全磁盘访问 GUI 引导;引入外置盘扫描时 `last_event_id` 需按卷拆分(现单值假设仅两扫描根同卷成立)。
- 引擎侧候选:regex:/size:/dm: 过滤器、外置盘扫描、`getattrlistbulk` 批量属性采集(列信息免懒 stat)、结果上限分页深化。

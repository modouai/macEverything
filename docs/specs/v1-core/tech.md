# macEverything v1-core 技术规格

**Status:** 写作中
**version:** 1.0.0
**updated:** 2026-10-15
**产品依据:** product.md v1.1.0(产品已确认)

---

## 现状分析

绿地项目,无存量代码。技术现状与约束:

- 仓库为空(仅 specs);目标机 Apple Silicon + rustup stable。
- 引擎的全部外部行为依赖两项系统能力:目录遍历(std 文件系统调用)与文件变更事件(macOS FSEvents,经 `notify` crate 封装)。
- FSEvents 语义为粗粒度合并(官方文档与 notify 源码确认):改名可能只报单边、事件可被系统合并丢弃(以 Rescan 标记告知)。因此**增量正确性只能建立在「目录级重扫 diff」上,事件只当触发信号**;全量重扫是唯一兜底。此结论是本设计的地基。
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
- **Decision**:条目定长结构 `Entry { name_start: u32, name_len: u32, parent: u32, flags: u8 }` 存 `Vec<Entry>`(下标即条目号);文件名连续存放于名称池(`Vec<u8>` arena)。并行维护两份 arena:`display`(原始字节)与 `match`(NFC 归一 + ASCII 小写折叠,匹配专用)。删除置 `flags` 墓碑位;墓碑占比超阈值触发整理(重建数组,复用重扫或压缩遍历)。
- **Alternatives Considered**:HashMap<String, Node>(Rejected:每条目独立堆分配,百万级内存与缓存局部性双差);BTree/trie 定位(Rejected:SIMD 线性扫 20MB 级 arena 已 <10ms,定位结构反而增加构建与维护成本——与 product.md 目标 2/3 的预算分配一致)。
- **Consequences**:查询是顺序扫 arena(缓存友好);内存 ≈ 条目 16B + 名称双份(百万 × 平均 20B × 2 ≈ 40MB + 16MB ≈ 60MB),远低于 500MB 红线;代价是墓碑整理逻辑需测试覆盖。

### ADR-3 落盘 = 镜像内存布局 + mmap 零拷贝加载

- **Context**:冷启动 <1s 是产品红线;反序列化路径(serde 类)百万条目需数百毫秒到秒级。
- **Decision**:索引文件布局与内存布局同构:`header(magic/version/built_at/entry_count/roots/excluded)` + `display_arena blob` + `match_arena blob` + `entries 定长数组`(整体 `#[repr(C)]`,16B/条)。加载用 `memmap2` 映射后直接转 `&[Entry]` 与 `&[u8]` 视图,零反序列化。写入 = 快照 + 临时文件 + `rename` 原子替换。
- **Alternatives Considered**:rkyv(Rejected:引依赖做「布局镜像」这件自己五行代码能做的事,且校验/兼容黑盒);SQLite(Rejected:加载走 B-tree 逐条读,冷启动目标直接破产)。
- **Consequences**:格式版本字段 + magic 校验兜住损坏/升级;daemon 运行中采用「mmap 只读视图 → 后台物化为可变索引」两段式,物化(拷贝 ~60MB,几十毫秒)完成后切可变、开始对账,冷启动全程可查。

### ADR-4 daemon 通信 = Unix domain socket + 长度前缀 JSON,同步线程模型

- **Context**:daemon 与 query/status/exclude 命令间需要本机 IPC;查询 QPS 极低(人手速),无高并发诉求。
- **Decision**:`std::os::unix::net::UnixListener` 同步线程,每连接一线程处理完即断;协议 = 4 字节小端长度 + UTF-8 JSON。socket 路径固定于应用数据目录,权限 0600。单实例 = socket 绑定失败即判定已运行。
- **Alternatives Considered**:tokio + 异步框架(Rejected:为一个人手速场景引入运行时复杂度,违背 v1 最小依赖原则);gRPC/protobuf(Rejected:同上,量级失配)。
- **Consequences**:实现 ~200 行,无新依赖;JSON 序列化微秒级,不进性能预算关键路径。

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
│   │       ├── index.rs         # 内存索引:构建/增删改/目录 diff/子树级联/墓碑整理
│   │       ├── scanner.rs       # 遍历器:扫描根/排除/符号链接跳过/权限缺口收集
│   │       ├── query.rs         # 语法解析(词法)→ 计划 → 匹配(memmem)→ 打分排序
│   │       ├── persist.rs       # 落盘快照写、临时文件 + rename、mmap 加载与校验
│   │       ├── watcher.rs       # notify 封装:事件→目录集、100ms 防抖、Rescan 兜底
│   │       ├── reconcile.rs     # 对账:重扫 diff 应用、差异计数
│   │       ├── config.rs        # 排除列表读写(toml)
│   │       └── error.rs         # EngineError(thiserror):含 PermissionGap 分类
│   └── cli/
│       └── src/
│           ├── main.rs          # 子命令分派:start/stop/query/status/exclude
│           ├── daemon.rs        # UnixListener 服务循环、单实例、退出落盘
│           ├── protocol.rs      # 请求/响应 JSON 结构(与 engine 解耦的传输层类型)
│           └── client.rs        # query/status/exclude 的客户端连接逻辑 + 离线查询
├── docs/specs/v1-core/
├── scripts/
│   ├── bench.rs / bench.sh      # 合成百万树 + 四项基准(首扫/查询/增量/落盘)
│   └── parity-check.sh          # find 对拍完整性
└── tests/                       # 集成测试:对拍、落盘回环、daemon 生命周期
```

### 关键机制

**扫描器**:手写迭代 DFS(`read_dir` 栈),每目录先读全量子项;跳过符号链接与排除子树;`PermissionDenied` 单独分类进缺口清单,其他 IO 错误计数不中断。产出 = (条目流, 缺口清单)。

**查询管线**:
1. 解析:按空格分词;`path:`/`case:`/`whole:` 前缀提为计划级开关(`path:` 作用于其后续一个词;`case:`/`whole:` 作用于整轮);`*`/`?` 编译为「锚定子串序列」(如 `*.pdf` → [ends-with ".pdf"],退化为 memmem 序列 + 首/尾锚标记)。
2. 匹配:`rayon` 对 `entries` 分块(每块 ≥ 10 万条),块内对每条目在 `match_arena` 对应切片跑 `memchr::memmem` 序列 AND;`path:` 词需沿 `parent` 链拼路径匹配(结果集小,逐条拼无性能风险);`case:` 命中后回 `display_arena` 复核大小写。
3. 打分排序:前缀命中(3)> 词边界命中(2)> 子串命中(1),同级按 `name_len` 升序;堆选前 N。

**增量管线**:watcher 线程收事件 → 目录路径去重入集 → 100ms 静默窗后交给增量 worker → 每目录 `read_dir` 与索引 diff(该目录 `parent` 的直接子项)→ 增/删(墓碑)/改名(diff 天然表现为删 + 增,子树级联 = 目录条目下 `parent` 重挂 + 路径懒惰求值)→ 变更计数进落盘节流。收到 `fsevent::EventFlags::RESCAN` 或监听器错误 → 触发 `reconcile`(全量重扫,复用扫描器,后台线程)。

**落盘节流**:变更计数器;worker 每轮检查「距上次落盘 ≥ 30s 或累计 ≥ 10000 条」→ 触发快照线程;快照 = `index.read()` 拍一致视图 → 写 `<file>.tmp` → fsync → rename。daemon SIGTERM/`stop` 走同一快照路径后退出。

**冷启动**:`start` → persist 校验 magic/version → mmap → `IndexView::Shared` 立即可查 → 后台线程物化 `IndexView::Mutable`(从 mmap 拷贝)→ 完成 + 对账启动 → 对账毕重落盘并保持 `Mutable`。查询代码只面对 `&[Entry] + &[u8]`,两视图同构,无分支扩散。

### 性能预算分配(对应 product.md 目标 1~6)

| 指标 | 预算分解 |
|---|---|
| 首扫 100 万 ≤ 20s | readdir + 名称写入,单线程 ~1.5 万条/s 下限即达标,实际 Apple Silicon 预期 5-10s;不 stat 是达标前提 |
| 查询 P99 < 10ms | memmem 吞吐 GB/s 级,40MB match_arena 单线程 <5ms;rayon 分块兜住最坏关键词分布 |
| 冷启动 < 1s | mmap 建映射毫秒级;物化 60MB 拷贝后台进行不阻塞首查 |
| 增量 ≤ 500ms | 100ms 防抖 + 目录 rescan(diff 毫秒级/目录)+ 应用;事件风暴时批量合并,仍受同一窗口约束 |
| 落盘不影响查询 | 独立线程 + 读锁快照;60MB 顺序写 <0.5s,期间查询持锁等待为微秒级 |
| 空闲 CPU 0% | 无定时器;全部 await 事件(FSEvents 回调线程阻塞等待) |

## daemon 通信契约(适配表 3)

socket 路径:`~/Library/Application Support/macEverything/daemon.sock`,权限 0600;协议:4 字节小端长度 + UTF-8 JSON。

| 消息 | 方向 | 请求字段 | 响应字段 | 说明 |
|---|---|---|---|---|
| query | client → daemon | `query: string`(原始语法串)、`limit: int ≤ 1000` | `entries: [{name, path, size, mtime}]`、`total: int`、`elapsed_ms: int` | `size`/`mtime` 懒 stat 填充 |
| status | client → daemon | `{}` | `state: building/ready/reconciling/degraded`、`entry_count: int`、`gaps: [{path, reason}]`、`built_at: int`、`index_file: string` | 状态契约唯一出口 |
| exclude.add / exclude.remove | client → daemon | `path: string` | `ok: bool`、`excluded: [string]` | 变更即生效并触发受影响范围重扫 |
| exclude.list | client → daemon | `{}` | `excluded: [string]` | |

离线模式(query 时 daemon 不在):client 直接 mmap 索引文件执行同一查询管线,响应加 `offline: true` 与 `built_at`,CLI 输出标注「离线索引」。

## 技术实现决策表(适配表 5)

| 维度 | 决策 |
|---|---|
| crate 划分 | workspace = `engine`(库)+ `cli`(二进制);v2 增 `app`;业务逻辑禁入 `cli` |
| 依赖(engine) | notify(FSEvents)、memchr(SIMD memmem)、rayon(分块并行)、unicode-normalization(NFC)、memmap2(加载)、serde+toml(配置)、thiserror+log(错误与日志);禁引 async 运行时 |
| 依赖(cli) | serde_json(协议)、anyhow(用户报错);引擎错误在边界转可读文案 |
| 存储层 | 内存 = 扁平 Entry 数组 + 双 arena;持久 = 同构布局文件 + mmap;删除 = 墓碑 + 阈值整理 |
| 并发模型 | watcher 线程 / 增量 worker / 快照线程 / `RwLock<IndexView>`;查询走 rayon 读锁;禁 shared mutable 无锁技巧 |
| 进程通信 | Unix socket + 长度前缀 JSON;同步线程每连接一线程;单实例 = 绑定失败 |
| 错误处理 | EngineError 分类(PermissionGap 独立);缺口计数不中断;daemon 日志落文件(数据目录 daemon.log);禁 catch 静默 |
| 测试策略 | engine 全模块单测(TDD)+ 集成(落盘回环/对拍/daemon 生命周期)+ 基准脚本;CLI 实机验证 |

## 风险与应对

| 风险 | 应对 |
|---|---|
| FSEvents 语义粗粒度,丢事件 | 设计已内建:目录 diff 唯一真相 + RESCAN 标记/监听器失效触发全量重扫;增量时效验收(≤500ms)含风暴场景 |
| mmap 视图与运行中变更不一致 | 两段式:Shared(只读)期间增量暂存队列,物化完成后统一应用;对账兜底 |
| 墓碑膨胀(长期高频删改) | 墓碑占比 > 20% 触发整理;整理走压缩遍历,毫秒级/万条,后台执行 |
| notify 版本行为差异(跨版本 FSEvents 封装变化) | Cargo.lock 锁版本;watcher 集成测试覆盖增删改/改名/RESCAN 五类事件 |
| 权限探测误判(把慢当拒绝) | 缺口分类只认 `PermissionDenied` 实际错误码;超时类错误计数但不进缺口清单 |
| socket 被第三方连接 | 0600 权限 + 数据目录内路径;协议无写操作,风险面仅查询 |
| 基准数字不可复现(SSD 缓存热) | 基准脚本:同一树跑 3 轮取中位,输出注明热缓存;合成树每次重建于临时目录 |
| 停机丢增量(对账窗口) | 产品语义已接受(场景 1):秒开旧数据 + 后台对账,对账完成前 status 如实显示 reconciling |

## 测试与验证

- **单测(TDD)**:语法解析(六语法 + 非法输入)、匹配器(大小写/整词/归一化/通配锚定)、索引(增删改/目录 diff/子树级联/墓碑整理)、persist(写读回环/坏 magic/版本不识别/原子替换)、reconcile(差异应用)、arena(NFC 两种形态互搜)。
- **集成**:合成树对拍(`find` 比对零差异)、落盘回环、daemon 生命周期(start 单实例/stop 落盘/杀进程恢复/离线查询)。
- **基准**:`scripts/bench.sh` 四项数字对照 product.md 目标;验收时实机跑。
- **命令链**:本地 `cargo fmt --check && cargo clippy -- -D warnings && cargo test` 全绿为声明完成前置。
- **实机验证**:`start` → 首扫 → Finder 增删改文件 → 500ms 内可查 → `stop` → 重启冷启动 <1s → `query` 对拍抽验 → 未授权完全磁盘访问时 `status` 缺口如实。

## 后续工作

- v2:`app` crate(egui 悬浮条 + 完整窗口 + 设置)、全局快捷键、i18n、登录自启、完全磁盘访问 GUI 引导。
- 引擎侧候选:regex:/size:/dm: 过滤器、外置盘扫描、增量 journal(避免对账全量重扫)、结果上限分页深化。

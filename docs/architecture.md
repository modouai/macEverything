# 架构总览

macEverything 的引擎架构:自建全盘文件名索引,不走 Spotlight。本文是**实现态**的架构说明;设计决策的完整推导过程见 `specs/v1-core/tech.md`(6 个 ADR)。

## 总体结构

```
┌──────────────────────────────┐        Unix socket         ┌─────────────────────┐
│  macEverything start (daemon) │◄──── 长度前缀 JSON ────────►│  query / status /   │
│  ┌────────────────────────┐  │                            │  stop / exclude     │
│  │ engine (纯逻辑库)        │  │                            │  (client 命令)       │
│  │  扫描 → 索引 → 查询      │  │                            └─────────────────────┘
│  │  FSEvents → diff → 索引  │  │                                     │
│  │  落盘 (mmap 同构布局)    │  │                              离线降级:daemon 不在
│  └────────────────────────┘  │                              时直读落盘索引自答
└──────────────────────────────┘
```

两个 crate:

| crate | 职责 | 关键模块 |
|---|---|---|
| `engine` | 全部业务逻辑,可独立测试 | entry / arena / index / query / scanner / persist / fsevents / watcher / reconcile / config |
| `cli` | 引擎宿主与命令行 | protocol(消息契约)/ daemon(宿主)/ client(五命令)/ main |

## 六大设计决策(实现态摘要)

### 1. 扁平索引 + 双名称池

百万条目的内存布局:

```
entries: [Entry; n]        24B/条:display 偏移/长度、match 偏移/长度、父条目号、标志位
display_arena: [u8]        原始文件名(展示与 full_path 用)
match_arena:   [u8]        NFC 归一 + 小写折叠后的文件名(匹配用,零拷贝直扫)
path_pool:     [u8]        目录完整路径(仅目录;path: 查询直扫,不做百万次路径拼接)
path_offsets:  [(u32,u32)] 按条目号全量对齐(目录真值,其余占位)
```

- **删除 = 墓碑位**(不移动数组,父指针稳定);墓碑占比 >20% 或 arena 孤儿字节 >32MB 触发整理(重建)。
- 目录改名级联重写子树路径池;`compact` 双阶段重建(先修父指针,再建路径池)。
- 遍历用深度拓扑排序保证「父条目号先于子」——并行扫描的分片交织不破坏不变量。

### 2. 查询:锚定子串序列 + Finder 预编译

语法在解析期编译为「锚定子串序列」(`*.pdf` → `[.pdf]`+尾锚;`报告?` → `[报告+QMARK]`),`path:`/`case:`/`whole:` 是轮级开关。

**关键优化**:`memchr::memmem::find` 每次调用重建 Finder(needle 预处理),百万条目逐条调用是主开销(实测 P99 51ms)。`CPattern` 在查询入口把每段编译为 `Finder::into_owned()`,块内只做 SIMD 扫描——**P99 3.6ms**。

- 并行:`rayon` 分块过滤,主线程 `select_nth_unstable` 取 top-limit(不做全量排序)。
- 打分:前缀命中 3 > 词边界 2 > 子串 1,同级按名长;total 为截断前全量命中数。
- 大小写不敏感覆盖整个 Unicode(折叠形态入 match arena);`case:` 走 display 侧 NFC 直通零拷贝。
- 耗时口径:仅匹配 + 排序;结果行的大小/修改时间按需 stat,不计入。

### 3. 落盘:内存布局镜像 + mmap 零反序列化

索引文件 = header(magic/版本/条目数/last_event_id/扫描根/排除列表/三池长度)+ 三池字节区 + path_offsets + **4 字节对齐填充** + entries 数组。布局与内存同构,加载即 `mmap` + 裸转型,**零反序列化,冷启动亚秒**。

- 写入 = 临时文件(pid+序号唯一名)+ fsync + `rename` 原子替换;崩溃/断电不损坏既有索引。
- 并发落盘由 daemon 的 `flush_lock` 串行化 + tmp 名唯一化双保护(曾实测并发写互相踩踏损坏索引)。
- 损坏/版本不识别 → 弃用重建,不中断服务。

### 4. FSEvents 直调:事件只当信号,目录 diff 才是真相

`notify` 封装不暴露 `sinceWhen`,engine 直调 FSEvents C API(unsafe 集中于 `fsevents.rs`,17 处全配 SAFETY):

- **事件语义不可信**(改名可能只报单边、事件可被合并丢弃)——收到任何事件只当「门铃」,重读该目录与索引 diff,以磁盘为唯一真相。
- **停机重放(mac 版 USN)**:落盘记录 `last_event_id`;重启以它为 `sinceWhen` 从系统事件日志位置重放停机窗口事件,秒级补齐。
- **兜底**:spike 实证「过旧 sinceWhen 无可观测信号」——重放零事件且停机 >48h(可配)一律转全量重扫(保守判定器,不赌信号);收到丢弃类标记或流断开同样降级。
- 拆除序列(Stop/Invalidate/Release)全部在流所属 runloop 线程执行(跨线程 Invalidate 实测 SIGTRAP)。

### 5. 并行扫描:work-sharing + 分片聚合

- 共享目录队列 + 线程本地分片(无逐条全局锁),活跃线程 = 物理核。
- 聚合:深度拓扑排序 + 两轮法(先目录后文件),路径集合确定、与串行全等(测试钉)。
- 排除语义双形态:绝对路径前缀(`~/Library`)+ 相对名组件匹配(`node_modules` 任意层级);首扫/增量/daemon 补链三条路径共用同一判定。
- 根条目名 = 扫描根完整绝对路径(结果路径可直接使用)。

### 6. 增量管线:批处理 → diff → 落盘节流

```
FSEvents 回调(专用线程 runloop)
   └─ 事件路径 → 父目录 → DirBatcher(100ms 静默窗去重)
        └─ drain → 每目录 apply_dir_diff(读磁盘 vs 索引,增/删/改名)
             ├─ 变更计数 → 落盘节流(30s 或 1 万条 → 快照)
             └─ 新目录 → dir_map 局部更新
```

- **补链**:事件风暴下子目录事件可能先于其父目录 diff 到达——miss 时从最近已索引祖先逐级向下补 diff。
- **重放追平**:HistoryDone 标记前强制 drain 全部 pending 事件再置「就绪」(否则就绪早于索引追上)。
- 排除目录不入 listing(防增量把首扫排除的子树拉回索引)。

## 生命周期

### 冷启动(有落盘索引)

```
load_mmap 校验 → Shared 视图(立即可查)→ 后台物化(拷贝为可变 Index)
  → 建 dir_map → 建流(since = last_event_id)→ 重放(状态「补齐中」)
  → HistoryDone → 追平(状态「就绪」)+ 落盘一次
```

### 首扫(无/损坏索引)

`building` → 并行扫描(`~/` + `/Applications`,百万文件 ~1.5s)→ 落盘 → `ready` → 建流(`SINCE_NOW`)→ 增量。

### 状态四态

| 状态 | 语义 | 成本 |
|---|---|---|
| building | 首扫进行中 | O(全盘) |
| reconciling | 停机窗口重放中(期间旧索引可查) | O(停机变更) |
| ready | 稳态增量 | — |
| degraded | 兜底全量重扫中 | O(全盘) |

### 进程模型

- 单实例:socket 绑定 `EADDRINUSE` → connect 探测——连得上 = 已在运行;拒连 = 崩溃残留 stale socket,unlink 重绑。
- `start` 后台化(spawn 自身脱离终端);`stop` 经 socket 消息触发,**最终落盘完成后才响应**。
- 日志:stderr + 数据目录 `daemon.log`;空闲 CPU 0%(纯事件驱动,无定时器)。

### IPC 契约

Unix socket(数据目录 `daemon.sock`,0600)+ 4 字节小端长度前缀 + JSON;消息含协议版本字段;长度上限 1MB;exclude 路径须绝对。daemon 不在时的离线降级:query/status 直读落盘索引并标注「离线」,exclude/stop 报错提示。

## 性能实测(百万文件,Apple M5 + NVMe,`scripts/bench.sh`)

| 指标 | 红线 | 实测 | 余量 |
|---|---|---|---|
| 首扫 | ≤ 5s | 1.0-1.6s | 3-5x |
| 查询 P99 | < 10ms | 2.9-3.6ms | 3x |
| 增量可搜 | ≤ 500ms | 0.2-0.3s | ~2x |
| 落盘(63MB) | 不拖查询 | 0.05s | — |
| 停机重放(万级事件) | ≤ 2s | 0.03s | — |
| 完整性对拍 | 零差异 | parity 测试通过 | — |

真实环境(123 万条目):5 个真实关键词查询全部 2.3-3.0ms;空闲 CPU 0%;常驻 ~90MB。

## 已知限制(如实挂账)

- `case: path:` 组合:路径侧恒折叠(case: 只作用于文件名侧)。
- `path:` 词不横跨目录段与文件名(文件侧「父路径 + 名称短拼」留待需求)。
- 非 UTF-8 文件名按 U+FFFD 替换收录(条目不丢,名称字节与磁盘非严格一致)。
- 冻结窗口:stop 后消费线程 ≤150ms 收尾期内的最后变更由下次启动重放兜底(最终一致)。
- 外置盘/网络盘不扫描;正则与 `size:`/`dm:` 过滤器属 v2 候选。

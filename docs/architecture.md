# 架构总览

macEverything 的引擎架构:自建全盘文件名索引,不走 Spotlight。本文是**实现态**的架构说明;设计决策的完整推导过程见 `specs/v1-core/tech.md`(6 个 ADR)与 `specs/v2-ui/tech.md`(v2.0 图形界面,ADR-7/8)。

## 总体结构

```
┌──────────────────────────────────────────────┐
│  macEverything.app(v2.0 GUI,eframe/egui)     │
│  悬浮条(⌥Space)/ 完整窗口(⌥⇧F)/ 托盘         │
│  全局热键 / 开机自启 / 引擎定位与拉起        │
└────────────────────────┬─────────────────────┘
                         │  crates/client(共享客户端承载层)
                         │  Unix socket · 长度前缀 JSON
┌────────────────────────▼─────────────────────┐
│  macEverything start(daemon)                 │
│  ┌────────────────────────────────────────┐  │
│  │  engine(纯逻辑库)                      │  │
│  │   扫描 → 索引 → 查询                   │  │
│  │   FSEvents → diff → 索引               │  │
│  │   落盘(mmap 同构布局)                  │  │
│  └────────────────────────────────────────┘  │
└──────────────────────────────────────────────┘
```

五个 crate:

| crate | 职责 | 关键模块 |
|---|---|---|
| `engine` | 全部业务逻辑,可独立测试 | entry / arena / index / query / scanner / persist / fsevents / watcher / reconcile / config |
| `protocol` | 协议契约(唯一真相:v1 从 cli 迁出,ADR-7) | 消息类型 / 编解码 / 校验 / 版本 |
| `client` | 客户端承载层(唯一行为真相:v1 从 cli 迁出,ADR-7) | DaemonPaths / 连接 / stat_rows / 状态常量 / 离线降级 |
| `cli` | 引擎宿主与命令行 | daemon(宿主)/ args(参数解析·排序换算·数据目录优先级)/ json(--json 输出)/ main |
| `app` | 图形界面(GUI,v2.0) | main(启动编排)/ session(查询会话)/ ui(双视口)/ platform(平台补丁)/ hotkey / tray / autostart / single_instance / engine_launcher / cli_install(命令行工具安装) |

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

## v2.0 增补:图形界面(实现态摘要)

设计推导见 `specs/v2-ui/tech.md`(ADR-7/ADR-8);逐项实测与终验收记录见 `specs/v2-ui/poc-results.md`。

### 层结构:app 是第二个客户端,不内嵌引擎

- 单 daemon 模型不变:app 与 CLI 同为 daemon 的客户端;app 依赖 engine 经 client 承载层发生(排序键等纯逻辑复用)。
- **双视口**:ROOT 视口 = 完整结果窗口;第二视口 = 悬浮条,采用 **deferred + 常驻**(独立渲染 pass——主窗被完全遮挡/不在当前 Space 时悬浮条照常工作;隐藏走 `orderOut` 不销毁,窗口唯一销毁点 = 进程退出)。
- 查询会话在 UI 侧纯逻辑化(防抖 60ms + 代际号防闪回 + 六态状态机);UI 线程不做阻塞查询(工作线程 + mpsc 回投,按代际丢弃旧响应)。

### ADR-7:协议与客户端承载层提升为共享 crate(protocol + client)

App 需要与 CLI 同款协议与客户端行为(连接 / 按需 stat / 离线降级 / 状态常量),照搬即复制且双漂移(契约与行为两处真相)。定案:提升为 `crates/protocol`(纯契约,零业务依赖)+ `crates/client`(客户端承载层);cli 与 app 共用,engine 零业务改动(仅 query 排序扩展)。

### ADR-8:悬浮条 = eframe/egui 单栈 + 运行期 NSPanel 改类

跨 Space(含他人全屏)、浮动级别在 winit 公开 API 无路径(PoC-1 实证:普通 NSWindow 四种组合全部失败,外证 tauri#11488)。定案:eframe 单技术栈 + 运行期 `object_setClass` 把 winit 窗口换成自定义 NSPanel 子类。生效组合**五要素缺一不可**:

1. 自定义子类覆写 `canBecomeKeyWindow` → true(裸 NSPanel 跨 Space 可,但收不到键盘)
2. `setStyleMask(原 mask | NonactivatingPanel)`(跨全屏硬条件)
3. `setLevel(Floating)`
4. `setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)`
5. 显示走 `orderFrontRegardless` / `makeKeyWindow`(成 key 而不夺前台)

两必带补丁:a) 改 styleMask 后补 `makeFirstResponder(contentView)`(否则键盘链路断);b) 关窗前还原窗口类(否则 winit drop → `panic in a function that cannot unwind` abort;视口常驻形态下还原点收敛到进程退出 `on_exit`)。unsafe 集中于 `crates/app/src/platform/mac.rs` 单模块。

配套实现要点(均源码级实证,详见 poc-results):

- **显示路径 native**:唤起必须走主线程 `orderFrontRegardless` + `makeKeyWindow`——eframe 对不可见窗口的跨线程唤起有 100ms 硬节流(`native/run.rs` 源码),winit `Visible(true)` 路径对外中位 117ms 破红线;native 路径对外可见中位 16.7ms,且与「不夺前台/跨全屏」本就同路。
- **热键动作主队列直调**:⌥Space/⌥⇧F 命中后经 `DispatchQueue.main` 在主线程序直调动作(与帧内同一份实现),不等帧——产品形态两窗皆隐,帧路受同一节流(实测旧路径 155ms);直调拿不到共享锁时入兜底队列由帧内消费,不静默丢。
- **切 Space 失焦豁免**:失焦沿 → 宽限期 800ms 待决 → Space 变更通知按「锚失焦时刻」判(实测通知多晚于失焦 265~550ms,曾漏判);点击导致的失焦经鼠标键状态当帧判出、立即隐藏。
- **主题跟随自建探测**:改类断掉 winit 的主题 KVO(`NSKVONotifying_WinitWindow` 被移除),改为渲染帧查询系统外观(`NSApp.effectiveAppearance`),仅在悬浮条可见期按节流检查。
- **排帧纪律**:只在悬浮条可见期排帧(含主题检查);全隐藏期不排帧,事件循环回 `Wait`——隐藏态 CPU 0% 的机制。

## v2.5 增补:CLI 集成(实现态摘要)

设计推导见 `specs/cli-integration-v2.5/design.md`。

- **数据目录覆盖**:解析优先级 `--data-dir` > `MCE_DATA_DIR` > `$HOME` 默认(`cli/args.rs`);env 为空/相对路径报错,不静默回落默认目录。目录 0700、`index.bin`/`config.toml`/`daemon.log` 0600;`start` 父进程预检目录可写后,给 spawn 的子进程显式回设 `MCE_DATA_DIR`。`daemon.sock`/索引/日志全部跟随目录——不同目录 = 不同实例。
- **App 拉起引擎固定目录**:`engine_launcher` spawn 时显式 `.env("MCE_DATA_DIR", paths.data_dir)`——外部同名变量(终端启动/`launchctl setenv`)不继承,App 永远连默认目录。
- **CLI JSON 契约**:`query/status --json` 输出 `schema_version = 1` 的单对象(`cli/json.rs`);字段、错误码与退出码(0/1/2)以 `docs/usage.md`「--json 输出契约」为入口。协议仅新增可选 `desc_echo` 回显,旧 daemon 缺省 `null`。
- **安装器**:`app` 的 `cli_install` 模块——登录 shell PATH 探测(`-lic` → `-lc` → 进程,超时终止进程组) + 分类状态机 + 原子安装/修复/卸载(只动自己装的符号链接,不覆盖其他来源);测试经 `test_fs` 注入文件系统,不触真实磁盘。

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

## 性能实测

引擎口径(百万文件,Apple M5 + NVMe,`scripts/bench.sh`):

| 指标 | 红线 | 实测 | 余量 |
|---|---|---|---|
| 首扫 | ≤ 5s | 1.0-1.6s | 3-5x |
| 查询 P99 | < 10ms | 2.9-3.6ms | 3x |
| 增量可搜 | ≤ 500ms | 0.2-0.3s | ~2x |
| 落盘(63MB) | 不拖查询 | 0.05s | — |
| 停机重放(万级事件) | ≤ 2s | 0.03s | — |
| 完整性对拍 | 零差异 | parity 测试通过 | — |

真实环境(123 万条目):5 个真实关键词查询全部 2.3-3.0ms;空闲 CPU 0%;引擎常驻 ~90MB。

图形界面口径(v2.0 终验收,真实索引 127.8 万条目,release + ad-hoc 签名):

| 指标 | 红线 | 实测 | 判定 |
|---|---|---|---|
| 悬浮条唤起 | < 100ms | 真 ⌥Space 5 轮中位 **95.4ms**(4/5 达标);App 内链路 0.7ms,残量在系统热键投递 ~51ms 与 WindowServer 合成 | 达标(边缘) |
| 输入到结果 | < 150ms | 中位 **~67ms**(8 轮 59~71ms) | 达标 |
| 隐藏态 CPU | 0% | 12/12 采样 0.0%(两窗皆隐 60s,累计 +10ms) | 达标 |
| 界面常驻内存 | < 100MB | **136M(两窗皆隐基线)/ 138M(使用后)** | **未达标**(栈基线,见已知限制) |
| 引擎自动拉起 | ≤ 3s 可搜 | 冷启动全链:随包 CLI 拉起 + 真热键真键盘出结果 | 达标 |
| 界面侧增量可搜 | ≤ 1s | 新建文件 → 可搜 267ms | 达标 |
| 全链路六项 | — | 冷启动 / 增量 / 离线 / 二次启动唤起 / 归零 / 自启态 6/6 通过 | 达标 |

## 已知限制(如实挂账)

- `case: path:` 组合:路径侧恒折叠(case: 只作用于文件名侧)。
- `path:` 词不横跨目录段与文件名(文件侧「父路径 + 名称短拼」留待需求)。
- 非 UTF-8 文件名按 U+FFFD 替换收录(条目不丢,名称字节与磁盘非严格一致)。
- 冻结窗口:stop 后消费线程 ≤150ms 收尾期内的最后变更由下次启动重放兜底(最终一致)。
- 外置盘/网络盘不扫描;正则与 `size:`/`dm:` 过滤器属 v2 候选。
- **界面常驻内存 ~136MB**(超「<100MB」设计红线):两窗皆隐的基线即 136M,与窗口使用无关——eframe + glow + glutin(两 GL 上下文)+ 系统 CJK 字体回退的栈级足迹。降本方向(未实施):字体图集按需裁剪 / 单视口化 / glow→wgpu 对照。
- **遮挡态退化**:悬浮条被其他窗口完全遮挡时,输入到结果退化到 ~500ms(eframe 对遮挡视口跳过 ui pass,防抖帧不发);悬浮条正常可见时不受影响。修法候选:`App::logic` 也驱动悬浮条 tick(遮挡/隐藏下 logic 仍跑)。
- **离线不排序**:daemon 不在时完整窗口列头排序不生效(结果恒按匹配质量序);状态栏红字提示并提供「重启引擎」动作。

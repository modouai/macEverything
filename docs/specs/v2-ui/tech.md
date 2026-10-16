# macEverything v2.0 图形界面 技术规格

**Status:** 写作中
**version:** 1.0.0
**updated:** 2026-10-16
**产品依据:** product.md v1.0.0(v2-ui)

---

## 现状分析

- v1 引擎已验收:查询 P99 3ms(140 万条目)、增量 ≤0.3s、mmap 秒开、停机重放;`engine` 为纯逻辑库(ADR-1 已为 v2 预留:「v2 增加 app crate(egui),engine 不动」)。
- CLI 已有完整 socket 契约实现(`crates/cli/src/protocol.rs`:Request/Response/编解码/版本/校验)与客户端逻辑(`client.rs`:连接/离线降级/输出)——v2 的 App 是**第二个客户端**,协议与客户端逻辑应复用而非复制。
- v1 验收暴露的教训对 v2 的约束:热路径避免每次重建结构(Finder 教训 → UI 侧同理:egui 每帧布局要避免大分配);事件/状态语义显式(batcher 窗口起算教训 → UI 的防抖与响应代际要显式建模)。
- 已知平台事实(本机实测):FSEvents/runloop 交互已封装良好;全局快捷键在 macOS 走 Carbon(`global-hotkey` crate,tauri 系);托盘 `tray-icon`(tauri 系)覆盖菜单栏图标+菜单;winit 的 `WindowExtMacOS` **不含**窗口级别/跨 Space 设置(官方文档查证于 2026-10)——这两项必须走 objc2。

## 架构变更

### ADR-7 protocol crate 提升 + app crate 新增

- **Context**:协议与客户端逻辑目前在 cli crate 内;App 需要同款契约。复制一份即产生双真相(契约漂移风险);App 内嵌 engine 则破坏「CLI/UI 共用一个 daemon」的进程模型(v1 ADR-1/4)。
- **Decision**:新建 `crates/protocol`(纯契约:消息类型/编解码/校验/版本,零业务依赖),cli 与 app 共同依赖;新建 `crates/app`(GUI,依赖 protocol + 领域无关的 UI 栈)。engine 零改动(除下述查询扩展)。
- **Alternatives Considered**:协议留 cli、app 依赖 cli 的 lib(Rejected:app 会连带拉进 daemon 宿主与整个 CLI 依赖);协议各持一份(Rejected:契约漂移);App 内嵌 engine 当 daemon(Rejected:双引擎实例/锁冲突/与 CLI 互斥)。
- **Consequences**:cli 需做一次机械迁移(协议文件搬家 + 引用改路径);`protocol` crate 成为唯一契约真相,变更走版本字段。

### ADR-8 悬浮条 = egui/eframe 渲染 + objc2 平台补丁

- **Context**:悬浮条需要 NSPanel 级行为(跨 Space 可见、浮动级别、失焦隐藏、多屏定位)。egui/eframe 官方覆盖无边框/透明/置顶/失焦事件/窗口定位;跨 Space 与窗口级别 winit 公开 API 不提供(文档查证)。原生 NSPanel 壳 + egui 渲染的集成成本高(自管 glow/wgpu surface)。
- **Decision**:eframe 单技术栈;窗口行为缺口经 winit raw handle(`ns_window()` → NSWindow 指针)用 objc2-app-kit 补丁:`setCollectionBehavior(CanJoinAllSpaces | FullScreenAuxiliary)`、`setLevel(NSFloatingWindowLevel)`、按需 `setActivationPolicy/Accessory`。unsafe 集中于 `platform/mac.rs` 单模块(与 v1 `fsevents.rs` 同纪律:SAFETY 注释逐处、失败可降级)。
- **Alternatives Considered**:原生 NSPanel 壳(Rejected:集成成本/双栈维护);SwiftUI 壳 + socket(Rejected:第二语言,双栈;v1 阶段已评过同类);纯 winit 公开 API(Rejected:能力缺口即产品红线)。
- **Consequences**:新增 objc2 依赖链(unsafe);PoC-1 先于界面实现验证;失败路径有明确降级(当前 Space 可见)。

## 变更方案

### crate 与模块划分

```
crates/
├── protocol/                ← 新建(从 cli 迁移,原样)
│   └── src/lib.rs             Request/Response/HitEntry/Gap/encode/decode/validate/PROTOCOL_VERSION
├── app/                     ← 新建
│   └── src/
│       ├── main.rs            启动编排:单实例锁 → daemon 探测/拉起 → 快捷注册 → 托盘 → eframe 主循环
│       ├── client.rs          socket 客户端(查询/状态;离线降级;响应代际)
│       ├── session.rs         查询会话:防抖(60ms)+ 代际号 + 排序状态(纯逻辑,可测)
│       ├── ui/
│       │   ├── mod.rs          App 壳:双视口(悬浮条/完整窗口)生命周期
│       │   ├── palette.rs      悬浮条视图:输入+列表+角标
│       │   └── results.rs      完整窗口:表格(虚拟化/列排序/多选)+右键菜单+状态栏
│       ├── platform/
│       │   ├── mod.rs          平台能力 facade(非 macOS 编译为空实现,便于测试)
│       │   └── mac.rs          objc2:跨 Space/浮动级别/激活策略/鼠标所在屏
│       ├── hotkey.rs           global-hotkey 注册与事件泵(失败可换备选位)
│       ├── tray.rs             tray-icon 图标/菜单/左键切换
│       └── autostart.rs        SMAppService 注册/注销/查询(或 LaunchAgent 兜底)
├── engine/                  唯一改动:query::execute_sorted(见下)
└── cli/                     删除本地 protocol 实现,改 `use protocol::*`
```

### 引擎扩展:execute_sorted

```rust
pub enum SortKey { Relevance, Name, Size, Mtime, Path }

pub fn execute_sorted(view: &dyn IndexView, plan: &QueryPlan, limit: usize, sort: SortKey) -> QueryOutcome
```

- `Relevance` = 现有 `execute`(委托,行为不变)。
- `Name`/`Path`:匹配集全量排序(键来自 arena,零 stat),取前 limit。
- `Size`/`Mtime`:**两段式**——先匹配得候选(≤ 全命中),对候选集 stat(按路径并行),再排序取前 limit。耗时口径与 v1 一致:「引擎耗时」只含匹配+排序;stat 单独计时随响应返回(`stat_ms` 字段),UI 状态栏展示两者。
- 排序稳定性:等值列用条目号决胜(确定性,同 v1 查询内约定)。
- 边界:limit=0 与全命中 0 条走既有路径。候选集全量 stat,不做抽样——完整排序语义要求全量键,耗时随响应如实告知(140 万条目全 stat 数百 ms 量级,仅点大小/时间列时触发)。

### 协议变更(向后兼容)

| 消息 | 字段 | 兼容性 |
|---|---|---|
| Request::Query | `sort: Option<String>`(serde default) | 旧客户端缺省 → relevance;旧 daemon 收到未知字段(serde 默认忽略)→ 仍按 relevance 返回 |
| Response::Query | `stat_ms: Option<f64>`(default) | 旧客户端忽略 |

### 悬浮条窗口行为(platform/mac.rs)

| 行为 | 实现 |
|---|---|
| 跨 Space | `NSWindow.setCollectionBehavior(CanJoinAllSpaces \| FullScreenAuxiliary)` |
| 浮动级别 | `NSWindow.setLevel(NSFloatingWindowLevel)`(值由 PoC 实测确定:floating / status 档位) |
| 失焦隐藏 | eframe `WindowEvent::Focused(false)` → `ViewportCommand::Visible(false)`(官方) |
| 多屏定位 | 唤起时取鼠标所在屏(NSEvent.mouseLocation 或 winit monitor)→ 计算居中偏上坐标 → `set_outer_position`(官方) |
| 无边框/透明/圆角 | `ViewportBuilder { decorations:false, transparent:true, ... }`(官方)+ egui 自绘圆角阴影 |
| 置顶 | `with_always_on_top`(官方) |
| 右键菜单 | **原生 NSMenu**(objc2):egui 收到右键 → 构建 NSMenu(NSMenuItem + target/action 桥回 Rust)→ `popUpContextMenu` 于鼠标位置;菜单项语义见 product §2。非自绘:观感/键盘导航/异屏缩放均随系统 |

### 进程编排

- **App 单实例**:数据目录文件锁(flock);二次启动 → 唤起已有实例悬浮条(经 socket 或通知,实现取简:检测锁存在即退出并提示)。
- **daemon 探测/拉起**:`UnixStream::connect(socket)` 探测;失败 → `Command::new("macEverything").arg("start")`(PATH 查找;缺失报可操作错误)→ 轮询连通(≤3s,250ms 间隔)→ 可用。
- **退出语义**:托盘「退出」只停 App(socket 不 send Stop);引擎常驻。
- **状态轮询**:App 对引擎状态的获取 = 每次查询响应捎带 + 空闲每 5s 一次 status(轻量;不引入订阅推送,v2.0 从简)。

### 查询会话(session.rs,可测纯逻辑)

```
输入事件 → debounce(60ms) → 发起查询(带代际号 n)
响应到达 → n < 当前代际?丢弃 : 渲染
排序变更 → 同代际机制重查(sort 参数变化,不重新输入)
```

- 代际号防「慢响应后到覆盖新结果」的闪回;离线/拉起中状态机显式(Idle/Searching/EngineStarting/Offline)。

## 技术实现决策表

| 维度 | 决策 |
|---|---|
| crate 划分 | protocol(契约)+ app(GUI);engine 零业务改动(仅 query 扩展);cli 机械迁移 |
| UI 技术栈 | eframe/egui 0.x(workspace 锁版本);egui_extras(表格/虚拟化);中文字体:内置 Noto Sans SC 子集或系统字体加载(避免豆腐块——实现期验证) |
| 平台补丁 | objc2 + objc2-app-kit(unsafe 集中 platform/mac.rs,SAFETY 逐处) |
| 托盘/快捷键/自启 | tray-icon / global-hotkey(tauri 系)/ SMAppService(经 objc2 或 launchd plist 兜底) |
| 进程编排 | 单实例 flock;daemon 探测=socket connect;拉起=spawn CLI;退出只退 App |
| 查询会话 | 防抖 60ms + 代际号 + 显式状态机(纯逻辑,单测覆盖) |
| 引擎扩展 | execute_sorted 两段式(stat 仅候选集);协议 sort 字段向后兼容 |
| 错误处理 | 每类失败给出可操作提示(引擎缺失/快捷键冲突/文件已删);禁静默 |
| 测试策略 | 引擎排序 TDD;会话纯逻辑 TDD;UI 实机验收清单;v1 全量回归 |

## 风险与应对

| 风险 | 应对 |
|---|---|
| PoC-1:跨 Space/浮动级别不生效 | 降级「当前 Space 可见」,产品文档标注;不阻塞其余功能 |
| PoC-2:⌥Space 与输入法冲突 | 注册失败弹窗 + 备选位(⌥⇧Space/⌃Space);以实机结果定默认 |
| PoC-3:NSMenu 阻塞式弹出与 egui 事件循环 | NSMenu 弹出走嵌套 run loop(阻塞当前线程)——在 eframe 主线程弹出需验证无卡死/输入错乱;不通过则菜单弹出移至独立线程或改用 popUpMenuPositioningItem 非模态 |
| 中文字体豆腐块(egui 默认字体无 CJK) | 启动时加载系统中文字体(苹方/思源),实现期首个验收项 |
| egui 隐藏视口仍在渲染(CPU 不为 0) | 隐藏时 `ControlFlow::Wait` + 不请求重绘;验收红线盯 `top` |
| 大命中集按大小/时间排序的 stat 风暴 | 仅在点列头时触发;候选集 stat 并行化(rayon);耗时随响应告知 |
| objc2 unsafe 缺陷 | 集中单文件 + SAFETY 注释 + PoC 期最小验证面;失败降级路径明确 |
| 并行会话(本仓另有改动在进行) | app 工作为全新目录,与 engine/cli 现改动零文件重叠;提交精确 add |

## 测试与验证

- **TDD(engine)**:`execute_sorted` —— 四列排序正确(含降序、稳定决胜、limit 边界)、sort=Relevance 与 v1 execute 逐字节同结果(回归钉);协议 sort 字段兼容(缺省/未知值)。
- **TDD(app 纯逻辑)**:session 防抖(注入时钟)、代际丢弃(乱序响应注入)、状态机转移。
- **v1 回归**:engine 79 项全绿;CLI 冒烟(迁移后 start/query/status/stop/离线)。
- **实机验收**(product「验收标准」逐条):双形态链路/多屏/全屏/批量/托盘/自启真实重启/性能四项;中文字体渲染为实现期首个验收项(egui 默认字体无 CJK,加载系统字体)。
- **PoC 验收(前置)**:P1 objc2 补丁(跨 Space 实测:切 Space 悬浮条仍在);P2 ⌥Space 注册(中文输入法环境实测);P3 NSMenu 原生弹出(egui 主线程弹菜单无卡死/输入错乱)。

## 后续工作(v2.1 边界)

设置页(排除目录/自启开关/语言选择 UI)、i18n 中英、快捷键自定义 UI、毛玻璃材质、完整窗口增强(打开方式子菜单/图标列)。

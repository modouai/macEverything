# macEverything v2.0 图形界面 技术规格

**Status:** 已确认(实现完成,实测回填;含终验收偏差清单)
**version:** 1.1.0
**prev_version:** 1.0.0
**updated:** 2026-10-16
**产品依据:** product.md v1.0.0(v2-ui;验收偏差见 product「验收偏差」节)
**changes:**
- (1.1.0)v2.0 实现完成回填:新增「实现偏差清单」五项(内存基线 136M / 遮挡态输入
  退化 / ⌥⇧F 补实现 / 热键主队列直调 / bundle 双可执行结构);终验收实测与逐项
  取证见 poc-results.md「Task 21」

---

## 现状分析

- v1 引擎已验收:查询 P99 3ms(真实环境约 120 万条目,2026-10-16 实测 1,197,010)、增量 ≤0.3s、mmap 秒开、停机重放;`engine` 为纯逻辑库(ADR-1 已为 v2 预留:「v2 增加 app crate(egui),engine 不动」)。
- CLI 已有完整 socket 契约实现(已提升为共享 crate `crates/protocol`:Request/Response/编解码/版本/校验)与客户端承载层(已提升为共享 crate `crates/client`:连接/离线降级/按需 stat 读取/状态常量)——v2 的 App 是**第二个客户端**,协议与客户端逻辑应复用而非复制。
- v1 验收暴露的教训对 v2 的约束:热路径避免每次重建结构(Finder 教训 → UI 侧同理:egui 每帧布局要避免大分配);事件/状态语义显式(batcher 窗口起算教训 → UI 的防抖与响应代际要显式建模)。
- 已知平台事实(本机实测):FSEvents/runloop 交互已封装良好;全局快捷键在 macOS 走 Carbon(`global-hotkey` crate,tauri 系);托盘 `tray-icon`(tauri 系)覆盖菜单栏图标+菜单;winit 的 `WindowExtMacOS` **不含**窗口级别/跨 Space 设置(官方文档查证于 2026-10)——这两项必须走 objc2。

## 架构变更

### ADR-7 协议与客户端承载层提升为共享 crate(protocol + client)

- **Context**:协议(`protocol.rs`)与客户端逻辑(`client.rs`+`DaemonPaths`/`stat_rows`/状态常量/离线降级)目前在 cli crate 内;App 需要同款能力。cli 无 lib target,照搬即复制——而「复制」的对象不止协议:离线降级依赖 `engine::persist`+`engine::query`,stat 补全、连接管理同理,复制面比协议大得多(契约与行为双漂移)。App 内嵌 engine 当 daemon 则破坏「CLI/UI 共用一个 daemon」进程模型(v1 ADR-1/4)。
- **Decision**:提升为**两个共享 crate**:
  - `crates/protocol`(纯契约:消息类型/编解码/校验/版本,零业务依赖);
  - `crates/client`(客户端承载层:`DaemonPaths`、socket 连接/请求、`stat_rows` 按需读取、状态常量、离线降级查询——依赖 `protocol` + `engine` 纯逻辑 API)。
  依赖关系:cli = daemon 宿主 + 子命令(依赖 client);app = GUI(依赖 client + UI 栈);**app 依赖 engine 经由 client 承载层发生**(engine 是纯逻辑库,不违反单 daemon 模型)。engine 零业务改动(仅下述查询扩展)。
- **Alternatives Considered**:协议留 cli、app 依赖 cli 的 lib(Rejected:cli 无 lib target 需先引入,且 app 会连带拉进 daemon 宿主代码);协议各持一份(Rejected:契约与离线/stat 行为双真相);App 内嵌 engine 当 daemon(Rejected:双引擎实例/锁冲突/与 CLI 互斥)。
- **Consequences**:cli 做一次机械迁移(protocol 与 client 相关代码搬家);`protocol` 为唯一契约真相、`client` 为唯一客户端行为真相;版本策略见「协议变更」节。

### ADR-8 悬浮条 = egui/eframe 渲染 + objc2 平台补丁(NSPanel 改类)

- **Context**:悬浮条需要 NSPanel 级行为(跨 Space 可见、浮动级别、失焦隐藏、多屏定位)。egui/eframe 官方覆盖无边框/透明/置顶/失焦事件/窗口定位;跨 Space 与窗口级别 winit 公开 API 不提供(文档查证)。**PoC-1 实测**:普通 NSWindow 即使设 `CanJoinAllSpaces|FullScreenAuxiliary`,`setLevel` 任意档,**进不了他人全屏 Space**(四组合对照 + CoreGraphics 清单取证;外证 tauri#11488)。
- **Decision**(PoC-1 续 spike 定稿,`scripts/poc/poc1b_nspanel`):eframe 单技术栈 + **运行期 NSPanel 改类**——`object_setClass` 把 winit 窗口换成自定义 NSPanel 子类;生效组合**五要素缺一不可**:①自定义子类覆写 `canBecomeKeyWindow`→true(裸 NSPanel 跨 Space 可但收不到键盘)②`setStyleMask(原 mask | NSWindowStyleMaskNonactivatingPanel)`(跨全屏硬条件)③`setLevel(Floating)` ④`setCollectionBehavior(CanJoinAllSpaces|FullScreenAuxiliary)` ⑤显示走 `orderFrontRegardless`/`makeKeyWindow`(成 key 而不夺前台)。**两必带补丁**:a) 改 styleMask 后补 `makeFirstResponder(contentView)`(否则键盘链路断,winit 源码注释同款修法);b) **关窗前还原窗口类**(否则 `ViewportCommand::Close` → winit Window drop → `panic in a function that cannot unwind` abort;spike 三方对照实证)。unsafe 集中于 `platform/mac.rs` 单模块。
- **Alternatives Considered**:普通 NSWindow 纯补丁(Rejected:PoC-1 实证不可行);原生 NSPanel 壳(Rejected:集成成本/双栈维护);SwiftUI 壳 + socket(Rejected:第二语言;v1 阶段已评过同类)。
- **Consequences**:跨全屏 Space 通过(双取证);**KVO 副作用**:winit 的主题监听(NSKVONotifying_WinitWindow)随换类失效——深浅色跟随**自建探测**(渲染帧查询系统外观),Task 13 按此实现;真键盘 HID 路径与 Chrome 全屏复核留人工验证(人工执行,poc-results 有步骤)。

## 变更方案

### crate 与模块划分

```
crates/
├── protocol/                ← 新建(从 cli 迁移)
│   └── src/lib.rs             Request/Response/HitEntry/Gap/encode/decode/validate/PROTOCOL_VERSION
├── client/                  ← 新建(承载层,从 cli 迁移并归置)
│   └── src/lib.rs             DaemonPaths / connect+call / stat_rows / state 常量 / 离线降级查询
│                              (依赖 protocol + engine;cli 与 app 的共同客户端面)
├── app/                     ← 新建(GUI)
│   └── src/
│       ├── main.rs            启动编排:app.sock 单实例 → 引擎定位(随包→PATH)/拉起 → 快捷注册 → 托盘 → eframe 主循环
│       ├── session.rs         查询会话:防抖(60ms)+ 代际号 + 排序状态 + 空词/warming/离线分支(纯逻辑,可测)
│       ├── ui/
│       │   ├── mod.rs          App 壳:双视口(悬浮条/完整窗口)生命周期 + 深浅色主题
│       │   ├── palette.rs      悬浮条视图:输入+列表+状态角标
│       │   └── results.rs      完整窗口:表格(虚拟化/列排序/多选)+原生菜单桥接+状态栏+缺口面板
│       ├── platform/
│       │   ├── mod.rs          平台能力 facade(非 macOS 编译为空实现,便于测试)
│       │   └── mac.rs          objc2:跨 Space/浮动级别/激活策略/鼠标所在屏/NSMenu 构建与弹出
│       ├── hotkey.rs           global-hotkey 注册与事件泵(失败可换备选位)
│       ├── tray.rs             tray-icon 图标/菜单/左键切换
│       └── autostart.rs        SMAppService 注册/注销/查询(LaunchAgent 兜底)
├── engine/                  唯一改动:query 排序支持(见下)
└── cli/                     迁移至 protocol/client crate;daemon 宿主与子命令保留
scripts/bundle.sh             ← 新增:.app 组装;CFBundleExecutable=macEverything-app(Contents/MacOS/),
                                随包 CLI=macEverything(同名覆盖与定位自命中双避免);LSUIElement=true
```

### 引擎扩展:排序查询

engine 保持纯逻辑(不直接碰文件系统)——元数据以**批量闭包注入**,并行归 daemon:

```rust
pub enum SortKey { Relevance, Name, Size, Mtime, Path }

/// meta_of:批量接口——入参为候选条目号切片,返回**按序对应**的 (大小, 修改时间)。
/// daemon 侧实现内部并行(rayon 铺开 stat),engine 单次顺序调用,无 Sync 约束。
/// 计分口径:elapsed_ms 只计匹配+键计算+排序(不含 stat);stat_ms 为 engine 包住 meta_of 调用的耗时。
/// 排序算出的元数据由 daemon 侧闭包自持(捕获缓存 map,供响应组装时免重查);SortOutcome 不含 meta。
pub fn execute_sorted(
    view: &dyn IndexView, plan: &QueryPlan, limit: usize,
    sort: SortKey, meta_of: &dyn Fn(&[u32]) -> Vec<(Option<u64>, Option<u64>)>,
) -> SortOutcome   // 新结构:{ hits, total, elapsed_ms, stat_ms: f64 }(QueryOutcome 保持 v1 不动)
```

- `Relevance` = 现有 `execute`(委托,行为不变,不调 meta_of)。
- `Name`/`Path`:匹配集全量排序(键来自 arena,零 stat),取前 limit。
- `Size`/`Mtime`:**两段式**——先匹配得候选,对候选集调 `meta_of`(daemon 侧批量并行 stat;测试侧 mock),再排序取前 limit。
- **stat 归属与去重(定案)**:daemon 的 meta_of 实现把批量 stat 结果**存入闭包捕获的 `&Mutex<HashMap<u32, MetaRow>>`**(每批 stat 后写入;`MetaOf` 是 `&dyn Fn`,经 Mutex 取内部可变性,不走 FnMut 捕获)。响应组装先查该 map,命中直接取、只对未命中行补 stat:Size/Mtime 排序下结果行已全量 stat(候选集包含截断后的 K 行),**零重查**;Relevance/Name/Path 下缓存为空、全部走补全轮,与 v1 行为一致。响应 `stat_ms` = 排序轮 meta_of 计时 + 补全轮计时(命中行零 syscall);补全轮沿用既有 v1 并行 stat 实现。
- 排序稳定性:等值键用条目号决胜(确定性,同 v1 查询内约定)。
- **排序键形态(定案)**:名称/路径键取 **display 原始形态 + Unicode 码点序**;不做本地化排序(与 Finder 的拼音/区域序不同,已知局限,文档标注)。中文与 ASCII 混排按码点,行为确定可复现。
- 边界:limit=0 与全命中 0 条走既有路径。候选集全量 stat,不做抽样——完整排序语义要求全量键,耗时随响应如实告知(约 120 万条目全 stat 数百 ms 量级,仅点大小/时间列时触发;UI 出「排序中…」状态)。

### 协议变更与版本策略

**版本策略(定案)**:纯**新增可选字段不 bump v**(v=1 保持);`sort_echo` 回显使旧 daemon 的降级可被 UI 识别——旧 daemon 忽略 `sort` 字段时,响应回显 `sort_echo:""`(=relevance),完整窗口发现请求键与回显不符即状态栏提示「引擎版本过旧,排序未生效」**并给「重启引擎」动作**(经随包 CLI `stop`→`start`,消除旧进程后由新版本接管),不静默展示错排序。同包同版(App 与随包引擎)为正常交付形态,跨版本混跑仅在用户手工混装时发生。

| 消息 | 变更 | 兼容性 |
|---|---|---|
| Request::Query | 新增 `sort: Option<String>`(serde default) | 旧客户端缺省 → relevance;旧 daemon 忽略未知字段 |
| Request::Query | 新增 `desc: Option<bool>`(serde default;**语义 = 反转自然方向**,非绝对升/降) | 缺省/false = 引擎自然方向(名升/大小升/mtime 降/路径升);旧 daemon 忽略 → 方向失效(Task 19 的 sort_echo 提示兜底) |
| Response::Query | 新增 `sort_echo: String`(serde default 空串) | 旧客户端忽略;空串 = 旧 daemon(实际按 relevance) |
| Response::Query | `HitEntry` 新增 `is_dir: bool`(serde default false) | 纯新增可选字段,不 bump v;旧客户端忽略该字段;旧 daemon 缺省 false(目录提示降级为「打开」,`open` 仍能进入);在线取索引真值、离线取当前 stat |

(`stat_ms` 为 v1 已在协议中的字段,本版不含变更。)

### 悬浮条窗口行为(platform/mac.rs)

> **Task 14 已落地**(实现与逐项实机取证见 poc-results.md「Task 14」):下表各行实现即现状;
> 新增一行「视口形态」——悬浮条改 **deferred 视口 + 常驻**(隐藏走 orderOut 不销毁;
> 独立渲染 pass,不随根视口是否被遮挡),解决 Task 12 遗留 L1/L2。

| 行为 | 实现 |
|---|---|
| 跨 Space(含他人全屏) | **NSPanel 改类五要素**(见 ADR-8;poc1b spike 定稿:`scripts/poc/poc1b_nspanel` 为可直接移植的实现参考);补丁施加时机 = 视口窗创建后首见(`NSApp.windows()` 按标题定位),可见期每帧漂移核对 |
| 视口形态 | **deferred 视口 + 常驻**(`show_viewport_deferred`,每根视口 pass 注册;builder 逐 pass 常量防重建);隐藏 = `Visible(false)`(winit `orderOut`,不销毁);窗口唯一销毁点 = 进程退出(改类还原补丁落 `App::on_exit`) |
| 浮动级别 | `setLevel(NSFloatingWindowLevel)`(PoC 实测;winit always_on_top 已自带 level 3,补丁增量在 collectionBehavior) |
| 失焦隐藏 | eframe 视口 `focused` → egui 命令 `Visible(false)`(winit orderOut)。实测(non-activating panel):`focused` 语义有效(winit 取 `windowDidBecomeKey/DidResignKey`)。**切 Space 豁免(2026-10-16 定,product §2;Task 14b 落地)**:`NSWorkspaceActiveSpaceDidChangeNotification`(NSWorkspace 通知中心,观察者 Retained 保活)记「序号+单调时刻」;**实测通知多数晚于失焦 265-550ms → 判据锚在失焦时刻**,宽限 `FOCUS_HIDE_GRACE=800ms`(覆盖最坏 +550;500ms 首版曾漏);**点击当帧判定**:`NSEvent.pressedMouseButtons` 查询(免权限)——点击失焦立即隐藏不受宽限影响;ghost 态 150ms 轮询兜底。常量:`SPACE_EXEMPT_WINDOW=500ms`/`GHOST_POLL=150ms` |
| **显示路径(定案)** | 显示必须走 **native `orderFrontRegardless()` + `makeKeyWindow()`**(主线程直接调用;唤起入口本就在主线程,无跨线程,故无需 DispatchQueue 投递)+ `ctx.request_repaint_of(悬浮条视口)`;**禁用 winit `Visible(true)` 唤起**——PoC-4 实测 eframe 0.37 对不可见窗口的跨线程唤起有 100ms 硬节流(`run.rs` 源码),唤起 132ms 超线;native 路径对外中位 **16.7ms**,且与 NSPanel 五要素(ADR-8)同路 |
| 多屏定位 | 唤起时取鼠标所在屏(`NSEvent.mouseLocation` + `NSScreen.visibleFrame`)→ 宽度 `min(700, 可见宽−48)`、下限 480;可见区水平居中、**面板**顶边距可见区顶**固定 110**(2026-10-17 定案,替代上 1/4);形象条 70(Task 9,`show_palette(top_inset)`):屏幕坐标 y 向下时窗口顶 = 面板顶 − 70,AppKit y 向上时窗口顶 y = 面板顶 y + 70;平台层原生 `setContentSize` + `setFrameOrigin`(不经 winit `set_outer_position`);高度随内容变化时「顶边钉住」重锚 |
| 高度变化呈现(**PoC-6 定案 2026-10-17**) | **窗内平滑**:窗口先在目标高一侧就位(增长先撑到目标、收缩等动画完成再收),面板高动画(120ms)全在 egui 窗内绘制;**静止时窗口 = 面板 + 形象条 70(Task 9;垫窗只覆盖面板,不含形象条)**,过渡期瞬时透明区约 0.12–0.2s;每次变化**只发 1 次原生 resize**(对照实测:逐帧驱动窗口高=134–175 次/变化,即抖动来源);配套 **`NSView.layerContentsPlacement=TopLeft`**(旧帧钉左上、裁切而非拉伸)。取证与判读见 poc-results.md「PoC-6」 |
| 形象条点击(**已知限制**,Task 9) | 形象条区域(窗口顶部 70px)属悬浮条窗口,点击落到窗口而非背后应用;ghost/失焦判定按整窗 frame,暂不区分条带。改为独立穿透窗口的架构代价大,不由本任务承担;待实机验收拍板(验收清单一节已列「实机确认是否接受」) |
| 包(package)语义(**已知项**,Task 9) | `.app/.pages/.rtfd` 等包被协议 `is_dir` 当目录:图标走文件夹、提示「进入」,但 `open` 实为启动/打开。待维护者验收拍板;可选修法:按扩展名或 `NSWorkspace.isFilePackageAtPath` 走文件语义 |
| 排帧纪律(L4) | 只在悬浮条可见期排帧(悬浮条 pass 自排 `CHECK_INTERVAL` 上限,含主题跟随检查);全隐藏期不排帧 → 事件循环回 Wait(隐藏态 CPU 0% 红线;Task 14 实测 60s 累计 CPU 零增量);唤起时 `ThemeFollow::check_now` 立即补查外观 |
| 根窗(完整窗口)唤起 | `NSApp.activate()` + `orderFrontRegardless` + `makeKeyAndOrderFront`;根窗 `collectionBehavior` 增加 `MoveToActiveSpace`,从悬浮条 ⌥↵ 进入时跟随当前 Space(2026-10-19 实机);`orderFrontRegardless` 保留「激活未完成也置顶」的历史保证 |
| 根窗关闭 | 取消关闭(`CancelClose`)+ 隐藏窗口(关闭窗口不退出 App,product §2);真退出走托盘「退出」(Task 16) |
| 无边框/透明/圆角 | `ViewportBuilder { decorations:false, transparent:true, ... }`(官方)+ egui 自绘圆角阴影 |
| 毛玻璃(**PoC-7 落地 2026-10-17**) | 「面板窗 + **下挂同级毛玻璃窗**」,构建顺序是硬约束:①建**普通 NSWindow**(borderless+`ignoresMouseEvents`+level/collectionBehavior 同面板);②装 `NSVisualEffectView`(blendingMode=BehindWindow,state=Active,material=`Popover`(2026-10-21 定版;曾用 `UnderWindowBackground`,其奶白层近乎不透明、叠面板底后「发实无玻璃感」,实机弃用),圆角用官方 `maskImage`);③`orderFrontRegardless` + `orderWindow:relativeTo:Below`(面板之下);④**最后**换类为 NSPanel 并补 `NonactivatingPanel`(跨他人全屏 Space 的硬条件;裸 NSPanel 收不到键盘,垫窗正合适)。**顺序反了(先换类/直接 NSPanel 构造)会打死 vibrancy**——浅深两色磨砂全失,三轮实机对照。圆角禁用 `layer.masksToBounds`(离屏通道同理会打死 vibrancy)。frame 每帧同步、显隐随面板(隐藏/切完整窗口当帧 orderOut)。**层级约束**:winit 视图即 contentView,禁换容器/加兄弟层(UB)。取证见 poc-results.md「PoC-7」与 floatbar-visual/plan.md「Task 10」 |
| 置顶 | `with_always_on_top`(官方) |
| 右键菜单 | **原生 NSMenu**(objc2):egui 收到右键 → 构建 NSMenu(NSMenuItem + target/action 桥回 Rust)→ `popUpContextMenu` 于鼠标位置;菜单项语义见 product §2。非自绘:观感/键盘导航/异屏缩放均随系统 |

### 进程编排(与 product §5 对齐)

- **App 单实例 = app.sock 通道**:App 启动时在数据目录监听 `app.sock`;二次启动连接它发「唤起悬浮条」消息后自行退出。socket 不可连(App 已退出残留)则清理后正常启动。
- **daemon 探测/拉起**:`UnixStream::connect(daemon.sock)` 探测;失败 → 按**引擎定位三档**找可执行文件(① `current_exe()` 同目录(随包 `Contents/MacOS/macEverything`)② PATH ③ 均失败 → 错误文案**列出实际搜过的全部路径**)→ spawn `start` → 轮询连通(≤3s,250ms 间隔)→ 可用。
- **退出语义**:托盘「退出」只停 App(不发 Stop);引擎常驻。
- **状态轮询**:查询响应捎带引擎状态;**轮询仅在至少一个窗口(悬浮条/完整窗口)可见时进行**(5s 间隔);两窗口均隐藏时停止轮询——与「隐藏时 CPU 0%」红线一致,状态在下次窗口可见或查询时刷新。

### 查询会话(session.rs,可测纯逻辑)

```
输入事件 → debounce(60ms) → 发起查询(带代际号 n)
响应到达 → n < 当前代际 → 丢弃 : 渲染
排序变更 → 同代际机制重查(sort 参数变化,不重新输入)
```

**状态机显式建模**(含 daemon 两个非结果响应分支):

| 状态 | 进入条件 | 界面 |
|---|---|---|
| Idle | 空词(含 ⌥⇧F 空词打开完整窗口) | 清空结果,不发起请求 |
| Searching | 防抖命中后发起 | 上一轮结果保留至新结果到达(整体替换,不闪) |
| EngineStarting | socket 不可连且拉起中 | 角标「启动引擎中」,请求排队重试 ≤3s |
| Offline | 拉起失败/daemon 退出且离线索引可用 | 结果来自离线索引,角标标注 |
| Empty | 响应 total=0 | 「无匹配项」空态 |
| Warming | 响应 error code=warming(索引未挂载) | 「索引构建中」占位(非错误样式);JSON 解析失败等其余 error 走错误态 |

- 代际号防「慢响应后到覆盖新结果」的闪回;`parse`(空词)已在 Idle 拦下,不到 daemon。

## 技术实现决策表

| 维度 | 决策 |
|---|---|
| crate 划分 | protocol(契约)+ client(客户端承载层:连接/stat/离线/状态常量)+ app(GUI)+ cli(daemon 宿主与子命令);engine 零业务改动(仅 query 排序扩展) |
| UI 技术栈 | eframe/egui 0.37(workspace 锁版本);表格 = 自定义虚拟化(`ScrollArea::show_rows`,不引 egui_extras);中文字体 = 系统回退(实现见 `crates/app/src/ui/mod.rs`「中文字体」) |
| 平台补丁 | objc2 + objc2-app-kit(unsafe 集中 platform/mac.rs,SAFETY 逐处) |
| 托盘/快捷键/自启 | tray-icon / global-hotkey(tauri 系)/ SMAppService(经 objc2 或 launchd plist 兜底) |
| 进程编排 | 单实例 = app.sock 通道(二次启动唤起悬浮条);daemon 探测=socket connect;拉起=引擎定位三档(随包→PATH→列搜过路径);退出只退 App |
| 查询会话 | 防抖 60ms + 代际号 + 六态状态机(Idle/Searching/EngineStarting/Offline/Empty/Warming,纯逻辑单测覆盖) |
| 交付形态 | .app bundle(LSUIElement=1)+ 随包 CLI;bundle.sh 手工组装;引擎定位随包优先、PATH 兜底、全失败列路径 |
| 引擎扩展 | execute_sorted 两段式(stat 仅候选集);协议 sort 字段向后兼容 |
| 错误处理 | 每类失败给出可操作提示(引擎缺失/快捷键冲突/文件已删);禁静默 |
| 测试策略 | 引擎排序 TDD;会话纯逻辑 TDD;UI 实机验收清单;v1 全量回归 |

## 风险与应对

| 风险 | 应对 |
|---|---|
| PoC-1:跨 Space/浮动级别 | **已定稿通过**(NSPanel 改类五要素+两补丁,spike 双取证);NSPanel 改类的复杂度即代价(KVO 主题链路断→自建探测;关窗还原类防 abort) |
| PoC-2:⌥Space 与输入法冲突 | 注册失败弹窗 + 备选位(⌥⇧Space / ⌃⌥⇧Space;⌃Space 族为系统输入法占用,不在候选池);以实机结果定默认 |
| PoC-3:NSMenu 阻塞式弹出与 egui 事件循环 | NSMenu 弹出走嵌套 run loop(阻塞当前线程)——在 eframe 主线程弹出需验证无卡死/输入错乱;不通过则菜单弹出移至独立线程或改用 popUpMenuPositioningItem 非模态 |
| PoC-4:eframe 隐藏 CPU 0% vs 唤起 <100ms | **已定稿通过**:隐藏 119.9s 零帧 CPU 恒 0;唤起走 native `orderFrontRegardless`(对外中位 16.7ms / P99 56ms,红线内);egui `Visible(true)` 路径因 eframe 100ms 硬节流弃用;keep-warm 实测 0.8% CPU 纯亏不采 |
| PoC-5:SMAppService + ad-hoc 签名自启 | **已定稿通过**:ad-hoc 可用、无需 sudo/审批;坑:①无 bundle 调 register 不报错且注册独立条目→产品入口须校验 bundle id;②条目身份键 = bundle id(路径/可执行名无关);③注册态不进 launchctl 服务表(由 BTM 管理,排障走系统设置登录项);未签名 arm64 二进制直接 SIGKILL(ad-hoc 是硬门槛);真实重启验证留人工 |
| LSUIElement(无 Dock 图标)与键盘焦点 | 菜单栏应用接收键盘输入前常需显式 `NSApp.activate`;悬浮条唤起与完整窗口打开时逐一处处理(PoC-1 同批验证) |
| .app 组装与随包引擎定位 | bundle.sh 组装后实机验证三种定位路径(随包命中/ PATH 兜底/全失败报错列路径) |
| 中文字体豆腐块(egui 默认字体无 CJK) | 启动时加载系统中文字体(苹方/思源),实现期首个验收项 |
| 大命中集按大小/时间排序的 stat 风暴 | 仅在点列头时触发;候选集 stat 并行化(rayon);耗时随响应告知;UI 出「排序中…」 |
| objc2 unsafe 缺陷 | 集中单文件 + SAFETY 注释 + PoC 期最小验证面;失败降级路径明确 |
| 并行会话(本仓另有改动在进行) | app 工作为全新目录,与 engine/cli 现改动零文件重叠;提交精确 add |

## 测试与验证

- **TDD(engine)**:`execute_sorted` —— 四列排序正确(含降序、稳定决胜、limit 边界)、sort=Relevance 与 v1 execute 逐字节同结果(回归钉);协议 sort 字段兼容(缺省/未知值)。
- **TDD(app 纯逻辑)**:session 防抖(注入时钟)、代际丢弃(乱序响应注入)、状态机六态转移(含空词/warming/offline 分支)。
- **v1 回归**:全仓 91 项(engine lib 77 + parity 2 + cli 12)全绿 + fsevents_live 2 项实机;CLI 冒烟(protocol/client 迁移后 start/query/status/stop/离线全链路)。
- **命令链(声明完成前置,承接 v1 口径)**:`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` 全绿;涉 App/打包的改动另跑实机验收清单。
- **实机验收**(product「验收标准」逐条):双形态链路/多屏/全屏/批量/托盘/自启真实重启/性能四项;中文字体渲染为实现期首个验收项(egui 默认字体无 CJK,加载系统字体)。
- **PoC 验收(前置)**:P1 objc2 补丁(跨 Space + 浮动级别 + LSUIElement 焦点,切 Space 实测);P2 ⌥Space 注册(中文输入法环境实测);P3 NSMenu 原生弹出(egui 主线程弹菜单无卡死/输入错乱);P4 eframe 隐藏 CPU 与唤起延迟(两红线兼容实测);P5 SMAppService + ad-hoc 自启注册/注销。

## 后续工作(v2.1 边界)

设置页(排除目录/自启开关/语言选择 UI)、i18n 中英、快捷键自定义 UI、毛玻璃材质、完整窗口增强(打开方式子菜单/图标列)。

## 实现偏差清单(v2.0 终验收回填)

**已知实现偏差**(与初始设计的差异,均已落代码注释;逐项取证见 poc-results.md「Task 21」):

1. **内存基线 136M(红线 <100MB 未达)**:两窗皆隐的基线即 136M——超预算与窗口使用无关,是 eframe + glow + glutin(两 GL 上下文)+ 系统 CJK 字体回退的栈级足迹(vmmap:Malloc Small dirty 58.5M、IOAccelerator 2.6M);旁证:PoC-3 单窗 demo 同口径 RSS 已 125~135MB。降本方向(未实施,建议单独立任务):egui 字体图集按需裁剪 / 单视口化 / glow→wgpu 对照。
2. **遮挡态输入→结果退化到 ~500ms**:悬浮条被其他窗口覆盖(occluded)时,eframe 对 occluded 视口跳过 ui pass(`glow_integration.rs` `show_ui=false`),防抖帧不发(实测 481~512ms)。非本版引入(既有结构),且发生在用户看不见悬浮条的期间;修法候选——`App::logic` 也驱动 `palette.tick`(logic 在遮挡/隐藏下仍跑),并让遮挡期排帧请求落到 ROOT,需单独验证(遮挡语义、双驱动去重)。
3. **⌥⇧F 为 Task 21 补实现**:product §2 与验收标准要求「完整窗口 ⌥⇧F 直达」,Task 15 只注册了 ⌥Space 族;Task 21 补齐(独立注册,失败只留痕不降级——完整窗口入口仍有托盘与悬浮条 ⌥↵)。「空词打开」的落地解释:⌥⇧F 与托盘入口同语义(不携带悬浮条查询词);悬浮条 ⌥↵ 才带词入窗。
4. **热键动作走「主队列直调」而非纯帧内消费**:eframe 对不可见窗口的跨线程 repaint 有 ≥100ms 硬节流,而产品形态两窗皆隐,纯「叫帧 → 帧内消费」路径实测 155ms、破唤起红线;命中后经 `DispatchQueue.main` 在主线程序直调动作(与帧内同一份实现),帧内只消费「直调拿不到共享锁」时的兜底队列(不静默丢)。
5. **bundle 双可执行结构**:App 主可执行定名 `macEverything-app`(CFBundleExecutable),引擎 CLI 同目录 `Contents/MacOS/macEverything`(随包)——两可执行同目录不同名,避免覆盖与「定位自命中」(App 按 `current_exe` 同目录找 CLI 时不会找到自己);bundle 标识 `com.example.maceverything` 为自启条目身份键,与路径、可执行名无关(PoC-5 实证)。

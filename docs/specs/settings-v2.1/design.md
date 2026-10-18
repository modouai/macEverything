# 设置功能（v2.1 第一期）— 设计

**Status:** Phase A 已实现（2026-10-18：窗口 / 字号 / 自启 / 语言占位）；快捷键 H1–H3 已实现（映射/运行时/设置页录制与持久化，2026-10-18）；排除目录 E1–E3 已实现（真实列表 + 异步 add/remove，2026-10-18；E4 实机验收待办）
**范围:** 设置窗口 + 非语言项（字号 / 开机自启 / 两个全局快捷键 / 排除目录）
**视觉依据:** `docs/specs/floatbar-visual/settings-visual.md`（下称 `visual`）
**不交付:** i18n（语言项先占位）、设置窗口的恢复默认/侧边栏/快捷键冲突弹窗、排除目录的定向增量重扫

---

## 一、已定决策

| 决策 | 值 |
|---|---|
| 设置窗口形态 | 第三个 eframe deferred viewport；620×600 固定、居中、单实例、关闭=隐藏 |
| 标题栏 | 原生交通灯 + 自绘 44px 标题「设置」；子视口透明标题栏若不可行，回落原生标题栏并回填偏差 |
| 字号 | 文本级全局倍率：小 0.9 / 标准 1.0 / 大 1.15；几何尺寸不变；立即生效 |
| 语言 | 禁用占位「简体中文（暂仅支持）」；不改任何文案 |
| 开机自启 | 复用 `autostart::apply_user_choice`（SMAppService），设置页与托盘勾选双向同步 |
| 快捷键 | 字段点击录制；必须含至少一个修饰键 + 非修饰键；Esc/点击他处取消；录制期临时注销全局热键；冲突/注册失败不落盘 |
| 排除目录 | 复用 daemon `exclude` 协议；添加走原生目录选择器；变更触发全量重扫，异步执行并显示「重扫中…」 |
| 数据源 | 字号/快捷键 → `settings.toml`；自启 → SMAppService；排除目录 → 引擎 `config.toml` |

---

## 二、架构

### 2.1 视口与状态

```
ROOT (完整结果窗口)
├── palette viewport (悬浮条, deferred)
└── settings viewport (设置, deferred)   ← 新增
```

- `ui::App::ui` 每帧注册 settings deferred viewport（builder 常量），与悬浮条同模式。
- `Shared` 新增 `settings: ui::settings::SettingsState`；状态含打开标志、录制状态、自启/排除目录的展示态与动作队列。
- 设置窗口的绘制回调在**自己的 pass** 中运行；根窗隐藏时也可渲染。
- 设置窗口关闭请求 → `settings.open = false` + `Visible(false)`；若在录制中，发 `CancelRecording`，由 App 恢复旧热键。

### 2.2 模块

| 文件 | 职责 |
|---|---|
| `crates/app/src/settings.rs`（新） | `SettingsFile`（字号/快捷键）读写、`FontSize`、原子保存、纯逻辑单测 |
| `crates/app/src/ui/settings.rs`（新） | 设置窗口视口、运行期状态（`SettingsState`）、字号/自启行为编排、快照→锁外绘制→动作落地 |
| `crates/app/src/ui/settings_view.rs`（新） | 纯视图：`SettingsData` 快照、分组/卡片/行控件绘制、headless 布局测试 |
| `crates/app/src/ui/settings_layout.rs`（新） | 行模型 `RowSpec`、卡片高度与行游标（高度/绘制单一真源） |
| `crates/app/src/ui/settings_tokens.rs`（新） | `visual` §2.5 色板 + §2.2/2.4 几何常量；深/浅两套 + 单测钉住 |
| `crates/app/src/ui/font.rs`（新） | 全局文本倍率 `AtomicU32` + `size(base)` / `id(base)`；启动读取设置 |
| `crates/app/src/hotkey.rs`（改） | 可重配置：活动键共享状态、录制期 suspend/resume、设置启动偏好 |
| `crates/app/src/ui/mod.rs`（改） | 注册设置视口、入口（齿轮/托盘）、动作队列落地、字号变更重装 text styles |
| `crates/app/src/tray.rs`（改） | 「设置…」改为打开窗口；自启勾选与设置页同步 |
| `crates/app/src/platform/mac.rs`（改） | 原生目录选择器 `pick_directory`；设置窗口居中/置前（如需要） |
| `crates/cli/src/daemon.rs`（改） | `exclude remove` 允许移除已存在的相对名条目（默认 `node_modules`/`.git`），Add 仍严格校验 |

### 2.3 动作队列（设置 → App）

设置回调不直接碰 `HotkeyRuntime`/`TrayRuntime`。视图动作 `settings_view::SettingsAction`
是带数据的非 `Copy` 枚举；字号/自启在设置 pass 绘制后立即落地，快捷键动作只翻译为
`SettingsState.hotkey_commands: VecDeque<HotkeyCommand>`，由 `App::logic` 在主线程消费：

```rust
enum SettingsAction {
    SetFontSize(FontSize),
    SetAutostart(bool),
    BeginHotkeyRecord(HotkeySlot),
    CancelHotkeyRecord,
    CommitHotkey { slot, hotkey },
    HotkeyError { slot, message: String },
}

enum HotkeyCommand {
    BeginRecord(HotkeySlot),
    CancelRecord,
    Commit { slot, hotkey },
}
```

`App::logic` 每帧按 FIFO 先执行命令、再消费关闭/⌘W 取消：
- 快捷键命令在主线程执行（manager 只能在主线程用），调用 `suspend/resume/commit`
  时**不持共享锁**，结果再以短锁写回 `settings.hotkey_snapshot` /
  `settings.recording` / `settings.hotkey_error`；
- 结果写回后 `request_repaint_of(SETTINGS_ID + ROOT + palette)`；
- 视图侧校验/查重错误（`HotkeyError`）不经过运行时，直接在设置 pass 落共享态并重绘；
- 设置窗口回调发命令后也 `request_repaint_of(ROOT)`；隐藏根窗的跨线程 repaint 有
  ≥100ms 节流，设置页操作可接受。
- `SettingsAction` 已不再是 `Copy`（可携带 `HotKey`/错误文案）；`HotkeyCommand`
  才是跨帧队列元素。排除目录的添加/移除**不走 App 命令队列**：设置 pass 在动作
  落地时短暂持锁置 busy/回投通道，随后直接在锁外启动工作线程（见 §8.5）；绘制
  回调不持锁、不阻塞，异步结果经 mpsc 回投下一 pass。

---

## 三、设置数据（`settings.toml`）

```toml
font_size = "standard"           # small | standard | large
palette_hotkey = "alt+Space"     # 缺省 = 内置默认（⌥Space 降级链）
window_hotkey = "shift+alt+KeyF" # 缺省 = 内置默认（⌥⇧F）
```

- `HotKey` 使用 `global-hotkey` 的 serde feature（字符串序列化）。0.6.4 的规范形式为
  小写修饰键、顺序固定 `shift+control+alt+super`，后接 `Code` 名（如 `alt+Space`、
  `shift+alt+KeyF`）；输入解析大小写不敏感（`Alt+Space` / `Alt+Shift+F` 均可读），
  保存时统一写回规范形式（两者 parse-compatible）。
- `SettingsFile` 只负责解析/保存，**不校验**裸键（无修饰键）或两个槽位冲突；
  校验与「冲突时不落盘」属于快捷键录制/注册流程（`plan-hotkeys.md`）。
- 原子写：同目录 tmp + rename，失败只 warn，不中断设置。
- 文件损坏 → 默认设置 + 留痕（不覆盖坏文件）。
- 字号：`FontSize → 0.9/1.0/1.15`，启动时在 `install_base_text_styles` 之前应用。
- 自启不写入此文件（SMAppService 为真源）；排除目录不写入（引擎配置为真源）。

---

## 四、字号

### 4.1 全局倍率

- `ui/font.rs`：`AtomicU32` 存 f32 bits；`set_scale` / `scale` / `size(base)` / `id(base)`。
- `install_base_text_styles` 的 11/15/15/20/15 改为 `base * scale`；字号变更时重新调用。
- 所有显式字号绘制/测量改走 `size`/`id`：
  - `palette.rs` 的 `font()` 与 `text_w()` 内部统一缩放；`.size(FONT_*)` 改为 `.size(font::size(FONT_*))`。
  - `results.rs` 的 `FontId::proportional(FONT_*)` 改为 `font::id(FONT_*)`，`.size(FONT_*)` 改为 `.size(font::size(FONT_*))`。
  - 设置窗口全部走 `font::size`。
- 几何（32/52/30/28、列宽、按钮尺寸）不变；大字号下设置内容区可滚动，标题栏固定。

### 4.2 生效路径

设置页点击分段 → `set_scale` → 重装 text styles → 写 `settings.toml`（立即写；
失败只落行内错误、保留已应用倍率）→ 请求 ROOT/palette/settings 重绘。

---

## 五、设置窗口 UI

### 5.1 窗口

- deferred viewport builder：620×600、不可缩放、标题「设置」、透明标题栏 + fullsize content、原生交通灯、阴影。
- 内容层每帧铺 `window_fill`；内描边 1px 圆角 12。
- **首次打开居中 + 置前（2026-10-18 实现）**：平台层按 `platform::SETTINGS_TITLE` 定位
  设置 NSWindow，在**鼠标所在屏**的 `NSScreen.visibleFrame`（已扣菜单栏 / Dock）内居中
  620×600，四周 clamp 8pt；窗口大于可见区时保持**左上可见**（AppKit y 向上：原点 y =
  可见区顶 − 窗高 − 8，底部允许溢出，标题栏/交通灯不得移出屏）。
  `NSApp.activate()` +（最小化态先 `deminiaturize`）`orderFrontRegardless()` +
  `makeKeyWindow()`。ROOT 隐藏（**同 Space**）已实机验证；**跨 Space 待验**
  （未额外设置 `collectionBehavior`，不擅自新增 Space 行为）。完整窗口唤起同步补
  `deminiaturize` 守卫。
- **只居中一次**：`SettingsState::request_open()` 首次打开返回居中请求并记账
  `positioned_once`；再次打开只置前，保留用户拖动后的位置。首次原生窗口未就绪时
  退回 egui `Visible(true)` + `Focus`，同样不再重试居中。
- **主题跟随**：设置窗口独立跟随系统深浅色；`settings.open` 期间由设置 pass 驱动
  `ThemeFollow::maybe_apply` 并按 `CHECK_INTERVAL` 排帧（ROOT/悬浮条都隐藏时是唯一
  驱动点），应用后立即重绘本视口；打开时锁外探测当前外观并 `check_now_with`，
  隐藏后停止排帧（L4）。
- **子视口标题栏 PoC（已验证）**：eframe deferred 子视口 + `with_titlebar_shown(false)`
  + `with_title_shown(false)` + `with_fullsize_content_view(true)` 下原生交通灯正常浮于
  自绘 44px 标题栏，标题栏拖拽 / 红点 / ⌘W 均可用；**无偏差**，不回落原生标题栏。
- **偏差记录**：首次打开若原生窗口尚未创建（视口首帧前，实际不可达），退回 egui
  默认位置且不重试居中——优先「不抢回用户拖动后的位置」。设置窗口位置不跨进程
  持久化（重启后首次打开重新居中），与本期范围一致。

### 5.2 内容

严格按 `visual` §2.2–§2.4：

```
Title Bar (44, 标题「设置」13pt 居中)
Content (padding 上下 20 / 左右 24, gap 20)
├── 外观: Header 12pt + Card(字号 Row 44)
├── 通用: Header + Card(自启 44 / 语言 44 / 唤起快捷键 44 / 完整窗口快捷键 44)
└── 索引: Header + Card(排除目录 Header 52 + 目录行 40 ×N)
```

- 卡片圆角 10、行间 1px 分隔线；控件按 §2.4 尺寸。
- 字号分段：容器 180×28、圆角 7、padding 2；选中段底色/文字按 token。
- 开关：38×22、圆角 11；开态系统蓝；钮 18×18 白 + 3 层递减 alpha 柔和投影。
- 语言：152×28 下拉按钮禁用态「简体中文」，tooltip「暂仅支持简体中文，语言切换将在后续版本提供」。
- 快捷键字段：152×28 键帽字段；录制中显示「按下快捷键…」，下方内联错误（冲突/注册失败/需修饰键）。
- 排除目录：Header 行「排除目录」+ 右侧「＋ 添加目录…」；目录行文件夹图标 16 + 路径 13 + 移除 ×14；空列表显示「未排除任何目录」。
- 滚动模型（2026-10-18 定案）：内容超出可视高时**整页纵向滚动**，44px 标题栏固定；
  排除目录列表随整页滚动，**不做卡片内独立滚动**（列表变长只增高索引卡片）。
- 所有文本走字号倍率；布局尺寸不随字号变。

---

## 六、快捷键

### 6.1 运行时改造

- `ActiveKeys { palette: Option<HotKey>, direct: Option<HotKey> }` 放 `Arc<Mutex<_>>`，事件泵每次事件读取当前 id，不再捕获固定键。
- `HotkeyRuntime` 暴露：
  - `snapshot() -> HotkeySnapshot`（当前键、显示名、按槽拆分的启动回退提示：
    唤起槽 `note` 供悬浮条警告，完整窗口槽 `window_note` 仅设置页展示）；
  - `suspend()` / `resume()`（录制期）；
  - `commit(slot, HotKey) -> Result<HotkeySnapshot, String>`（注册新键；挂起态任何
    失败都恢复旧键并退出挂起，成功清空回退提示）；
  - `reset_slot(slot) -> Result<HotkeySnapshot, String>`（恢复内置默认：唤起槽走
    `candidates()` 首个成功项、完整窗口槽走 `direct_key()`；挂起态与 commit 同
    语义，失败尽力恢复两个旧键并退出挂起；非挂起态先注册默认、成功后才注销旧键）。
- 启动：
  - 读取 `settings.toml`；自定义键优先注册；
  - 自定义键注册失败 → 回落该槽默认键（悬浮条仍走原降级链；完整窗口走 ⌥⇧F），在设置页显示「已回落默认」；
  - 默认键注册失败的行为保持现状。
- 事件泵与主队列直调/兜底队列机制不变。

### 6.2 录制状态机

```
Idle → (点击字段) Beginning → (App suspend 完成) Recording
Recording → (Esc/点击他处/关闭窗口) Cancelled → App resume → Idle
Recording → (Backspace 恢复默认) Resetting → App reset_slot
Resetting → 成功: Idle + 内置默认（settings.toml 字段清空）；失败: Idle + 内联错误（旧键已恢复）
Recording → (合法组合) Committing → App commit
Committing → 成功: Idle + 新键；失败: Idle + 内联错误（旧键已恢复，用户可重试）
```

- 合法组合：至少一个 ⌘/⌥/⌃/⇧ + 一个非修饰键；不支持纯修饰键、纯单键。
- 同一组合不能同时用于两个槽；冲突在本地拒绝。
- 系统占用/注册失败：App commit 失败 → 恢复旧键 → 内联「注册失败（可能被系统或其他应用占用）」。
- 显示名用 macOS 符号：⌘⌥⌃⇧ + 键名；默认键保持现有文案（⌥Space / ⌥⇧F）。
- 录制期间全局热键注销，确保按键进入设置窗口；App 记录旧键用于恢复。
- **恢复默认**：录制中 Backspace → `ResetHotkey(slot)` → `HotkeyCommand::Reset`
  → `reset_slot`；成功清空设置字段（跟随内置默认）并落盘，失败恢复旧键 + 内联错误。
  该路径用于 macOS 下 ⌥Space 不能可靠作为按键事件录制时回到内置默认。
- **⌥Space 已知限制**：释放版中 ⌥Space 可能不以 `Event::Key` 到达；录制层额外识别
  带 Alt 的 `Event::Text(" ")` / `Event::Text("\u{00A0}")` 并翻译为 ⌥Space。若平台
  既不交付 Key 也不交付 Text，只能用 Backspace 恢复默认；不全局拦截系统
  快捷键或文本输入。
- 启动时保存的键注册失败：回落默认 + 设置页警标；不自动改写文件（用户可重新录制）。

### 6.3 键位映射

- egui `Key` → `global_hotkey::hotkey::Code`：A–Z、0–9、F1–F12、Space、Enter、Tab、方向键、Home/End/PageUp/PageDown、常用标点。
- 修饰键：`command→SUPER`、`ctrl→CONTROL`、`alt→ALT`、`shift→SHIFT`。
- 不支持/未映射的键：忽略并提示「该键不支持」。
- 纯逻辑（映射、校验、显示格式化）单测钉住。

### 6.4 实现状态（2026-10-18）

- 设置页动作：`SettingsAction::{BeginHotkeyRecord, CancelHotkeyRecord, CommitHotkey,
  HotkeyError}`（已去掉 `Copy`，可携带 `HotKey`/错误文案）；设置 pass 只把前三个
  翻译为 `HotkeyCommand` 入 `SettingsState.hotkey_commands`，并请求 ROOT + 设置页
  重绘；`HotkeyError` 只落共享态错误并重绘设置页（§2.3）。
- `App::logic` 先 drain 命令（短锁取走），在共享锁外执行
  `suspend/resume/commit`，再短锁写回 `recording` / `hotkey_snapshot` /
  `hotkey_error`；设置文件保存在锁外。`Begin` 在窗口已关闭时丢弃；陈旧视图在
  同一槽已录制时再次 Begin → 按取消处理（恢复旧键），避免录制态无法退出。
- 录制 UI：空闲点击开始；`suspend()` 在主线程同步完成，字段立即显示
  「按下快捷键…」，不存在可渲染的「准备中」中间态（未保留 `recording_ready`）。
  行下显示可见提示「按 Esc 或点击别处取消,Backspace 恢复默认」；Esc / 再次点击字段 /
  点击字段外 / 失焦或最小化 / ⌘W/红点关闭均取消。失焦用焦点 `true→false` 跃迁
  判定（窗口刚打开、尚未获得焦点时的 `false` 不误取消；macOS 最小化同样表现为失焦）。
  `Begin` 落地时若已观测到失焦则直接丢弃，堵住「点击后瞬间切走」的竞态。
  所有按键事件被消费，不触发其它 UI。
- 键位采集：按键 `Event::Key` 经 `from_egui`；纯修饰键（⇧⌃⌥⌘）按下被消费但不
  报错。egui-winit 的剪贴板功能把「仅 Command」的 ⌘C/⌘X/⌘V 转成
  Copy/Cut/Paste/PasteImage 事件；录制层把它们翻译回 C/X/V，并使用
  `InputState.modifiers`（保留 Ctrl+Cmd+C 等额外修饰）。残留：⌘V 在剪贴板为空时
  egui-winit 不产生任何事件，故该次按键无法录入；不全局关闭剪贴板快捷键
  （会破坏搜索框的复制/粘贴）。
- ⌥Space 兜底（2026-10-18）：录制层识别带 Alt 的 `Event::Text(" ")` /
  `Event::Text("\u{00A0}")` 并翻译为 `(Space, modifiers)` 提交；录制期文本事件
  一律消费。若平台既不交付 Key 也不交付 Text，则直接录制仍不可行，
  **Backspace 恢复默认**为可靠路径。
- 恢复默认实现：`SettingsAction::ResetHotkey(slot)` → `HotkeyCommand::Reset(slot)`
  → `App::logic::reset_hotkey` → `HotkeyRuntime::reset_slot`；成功清空
  `SettingsFile.palette_hotkey`/`window_hotkey`（`None` = 跟随内置默认）并锁外保存、
  更新快照/悬浮条提示；失败由运行时恢复旧键并退出挂起，录制结束 + 按槽内联错误。
  唤起槽默认链降级时快照 `note` 显示「⌥Space 不可用,已改用 …」。Backspace
  不作为可录制键（不落白名单，捕获时优先解释为恢复默认）。
- 错误与提示按槽显示：`hotkey_error: Option<(HotkeySlot, String)>`；回落提示
  `note`/`window_note` 也显示在对应行下；录制提示另占一行。错误 + 提示 + 录制
  提示可同时存在，`RowSpec.extra` 随消息条数增高（卡片底/分隔线仍由同一游标推进）。
- 偏差：视图侧校验/查重错误**不退出录制**（可直接重按或 Esc）；运行时 `commit`
  失败退出录制（H2 已恢复旧键）；保存失败不回退键，只显示「快捷键已生效,但设置
  保存失败」，内存 `SettingsFile` 保留新键供后续保存补写。
- 默认展示完全来自 `HotkeyRuntime::snapshot()`（设置态不再硬编码键帽）；
  `PaletteState.hotkey_note` 随 begin/cancel/commit 后的快照同步。
- 自动化覆盖纯状态/命令队列/headless 视图/剪贴板事件/⌥Space 文本兜底/
  Backspace 恢复默认/运行时 reset 规划器/焦点取消/文件持久化；真实 Carbon 注册、
  系统占用、⌥Space 平台事件行为、跨重启与实机失焦需手动验收（清单见
  `plan-hotkeys.md` H4）。

---

## 七、开机自启

- 打开设置页时（仅隐藏→显示）在共享锁外查询 `autostart::status()`；窗口已打开时
  不重复查询，避免覆盖失败切换的行内错误。查询失败按未知/关态 + 错误文案。
- 开关显示按**操作后真实状态**，不由请求值反推：
  - 仅 `Status::Enabled` 显示开；
  - `Status::RequiresApproval`（已注册待允许，登录不会拉起）显示关 + 行内提示
    「已注册,待系统设置「登录项」允许」；
  - 启用后仍 `NotRegistered`/`NotFound` → 显示关 + 内联错误。
- 点击开关：锁外 `apply_user_choice(data_dir, desired)`（返回操作后 `Status`）；
  成功按上述真实状态更新 UI，并把**实际启用值**置入
  `tray_autostart_sync`（审批中/未生效 → false）；失败保持旧状态 + 内联错误。
- 托盘勾选：按同一策略更新 `Shared.settings.autostart`/错误；仅 `Enabled` 勾选，
  审批中/未生效取消勾选，注册失败复原。设置页打开时即时重绘。
- `App::logic` 先消费托盘动作，再 drain 设置页同步；本帧托盘也切换了自启时丢弃
  设置页旧同步值（托盘动作更新）。

---

## 八、排除目录

### 8.1 读取

- 打开设置页 → 工作线程 `client::exclude(paths, "list", None)`；结果写回 `excluded: Vec<String>`。
- 引擎离线：客户端返回错误 → Header 支持行**原样**显示 client 文案（当前为
  「daemon 未在运行;排除变更需引擎在线,先执行 macEverything start」）；不直接改
  `config.toml`。自动 list 刷新不清已展示的失败文案，直到一次成功 list。

### 8.2 添加

- 点击「添加目录…」→ 主线程 `platform::pick_directory()`（`NSOpenPanel`，只能选目录、单选）。
- 选定后启动工作线程 `client::exclude(paths, "add", Some(path))`；期间 `excluded_busy=true`，按钮/移除禁用，显示「重扫中…」。
- daemon 校验路径必须在扫描根内；失败原因原样展示。
- daemon 全量重扫完成后返回；成功用返回的 `excluded` 刷新列表。

### 8.3 移除

- 点 × → 工作线程 `client::exclude(paths, "remove", Some(path))`；同样 busy/重扫中。
- **daemon 修复**：`apply_exclude_change` 对 `Remove` 不再要求绝对路径（只要求条目存在）；默认的相对名 `node_modules` / `.git` 可移除。Add 仍严格校验扫描根。
- 移除失败（条目不存在/配置落盘失败）原样展示。

### 8.4 数据一致性

- 列表唯一真源 = daemon `ExcludeList` 返回；UI 不缓存第二份写源。
- 变更成功后以响应中的 `excluded` 覆盖；失败保留旧列表 + 错误。

### 8.5 实现偏差（2026-10-18，Task E3）

- **滚动模型**：排除目录列表随**整页滚动**（与 §六 的固定 44px 标题栏同款），
  不做卡片内独立滚动、不做虚拟化；列表变长只增高「索引」卡片。
- **异步状态**：`SettingsState` 同一时刻至多一个后台操作，经单操作 mpsc 回投；
  settings pass 在短锁内非阻塞 `tick_exclude()`。共享锁**不覆盖**
  `pick_directory()`、`client::exclude`（add/remove 会触发 daemon 全量重扫）与
  绘制；完成时由工作线程唤醒设置视口。
- **忙碌文案**：list（打开/重开设置）显示「加载中…」，add/remove（daemon 重扫）
  显示「重扫中…」；两者都禁用添加与移除 ×，并在 Header 下用支持行增高
  `RowSpec.extra`。错误（daemon 原文/离线）同样走 Header 支持行，可与 busy、
  空态「未排除任何目录」并存。
- **种子列表**：引擎默认条目只在首次**成功** list 前展示；失败时保留旧列表
  （首次即种子）+ 错误，不以空列表覆盖。
- **错误保留**：add/remove 失败文案在关窗/重开触发的自动 list 刷新期间保留，
  仅一次**成功** list 清空；刷新自身失败则替换为新错误（与 busy 支持行并存）。
- **目录选择器可用性**：前置 `NSApplication.activate()`（无参）为 macOS 14+ API，
  与 README 的 macOS 13 最低版本存在既有偏差（`show_root_window` /
  `show_settings_window` 早已如此）；本任务沿用，不改变最低系统版本。
- **已知限制（不在 E3 改代码）**：`client::connect` 读超时 30s，超大索引的全量
  重扫可能先报超时、而 daemon 稍后仍落盘成功；UI 此时保留旧列表 + 错误，重开
  可再取。原生选择器路径经 `to_string_lossy()` 转协议字符串，极端非 UTF-8 路径
  可能不可用。

### 8.6 恢复默认（E5，2026-10-18）

- **语义**：把 `excluded` 重置为内置三条 `~/Library`、`node_modules`、`.git`
  （`engine::config::default_config()`）；自定义条目全部清除。不新增名称规则输入，
  不改变默认列表。默认相对名（`.git`、`node_modules`）无法经目录选择器重新添加，
  这是本操作的直接动机。
- **两步确认（破坏性）**：Header 右侧常规态为「恢复默认」小号文字按钮 +
  「＋ 添加目录…」；点击「恢复默认」只展开确认，不启动 worker。确认态隐藏添加
  按钮，显示「取消」「确认恢复」，并在 Header 下显示支持行
  「将清空自定义排除项，恢复 ~/Library、node_modules、.git」；该行高度计入
  `RowSpec.extra`。确认/取消/打开/关闭/开始 add/remove/reset 均收起确认；
  请求与取消确认不清既有错误。
- **执行**：`SettingsAction::ConfirmExcludeReset` → `client::exclude(paths, "reset", None)`
  → daemon `Request::ExcludeReset` → `reset_excludes`。复用单操作 busy/错误管线
  （显示「重扫中…」、禁用全部排除控件）；成功以响应列表覆盖，失败保留旧列表 +
  错误。一次 reset 只触发一次 daemon 全量重扫，不为每条默认单独变更。
- **daemon 顺序**：先 `config::save(default_config(), config_file)`；保存失败 →
  `ok=false` + 当前列表 + `配置保存失败(...)`，内存与索引都不变、不重扫。保存成功
  才替换内存配置并 `degrade_and_rescan`。不持 `cfg` 锁跨越保存/重扫。
- **兼容性**：协议版本号不变；`exclude_reset` 是新增请求，旧 daemon 解码失败并
  拒绝。升级后须停止/启动引擎换用新 CLI（App 不自动重启）；设置页此时按错误展示。

---

## 九、入口

- 完整窗口齿轮：删除「设置将在 v2.1 提供」占位，改为 `settings_open(shared, ctx)`。
- 托盘「设置…」：删除 NSAlert 占位，改为 `settings_open`。
- 单实例：已打开则原生激活 + 置前（`deminiaturize` + `orderFrontRegardless` +
  `makeKeyWindow`；原生不可用时退回 `Visible(true)` + `Focus`）；不新建第二窗。
- 关闭：设置窗口的 `close_requested` → 取消关闭 + `Visible(false)` + 取消录制。
- 设置窗口打开不改变完整窗口显隐。

---

## 十、错误处理

| 场景 | 处理 |
|---|---|
| settings.toml 损坏/写失败 | 默认值/保留旧值 + `log::warn`；不 panic、不覆盖坏文件 |
| 热键注册失败 | 恢复旧键 + 内联错误；不落盘 |
| 录制中关闭/失去焦点 | 取消录制 + 恢复旧键 |
| 自启注册失败 | 开关复原 + 内联原因 |
| 排除目录读取失败 | 原样 client/daemon 错误内联展示；**保留旧列表**（首次为默认种子），不清空 |
| 排除目录变更失败 | 保留旧列表 + 原样原因（含 daemon 校验文案）；自动刷新不吞错误 |
| 目录选择器取消 | 无动作 |
| 设置窗口子视口标题栏 PoC 失败 | 回落原生标题栏，记录偏差 |

---

## 十一、测试

### 纯逻辑单测

- `settings.rs`：默认值、roundtrip、损坏文件降级、原子写、字号映射。
- `font.rs`：三档倍率、size/id、原子设置。
- 热键映射/格式化/合法性/重复检查；suspend/resume/commit 的**计划逻辑**（注册顺序、失败恢复）抽成纯函数测试。
- 排除目录状态机：加载/成功/失败/忙碌、daemon 相对名移除。
- 设置 UI 几何 token 单测；headless 出帧：三组五项存在、语言禁用、字号分段选中、录制字段、目录列表渲染不 panic。

### 实机（辉哥）

- 齿轮/托盘打开、单实例、620×600 居中、交通灯/标题/圆角。
- 字号三档即时生效、重启保持；两窗口文字变化、布局不塌。
- 自启开关与托盘双向同步、重启系统验证一次。
- 两个快捷键录制/取消/冲突/系统占用/重启保持；录制期旧热键不触发。
- 排除目录添加/移除、重扫提示、越界路径错误、默认相对名可移除。
- 深浅色、关闭=隐藏、再次打开恢复上次值。

---

## 十二、风险与先行验证

1. **子视口透明标题栏 + fullsize content + 原生交通灯**：先在 eframe 子视口做最小 PoC；不通过则用原生标题栏（~28px）并回填 `visual`。
2. **设置窗口跨根窗隐藏的渲染/命令时序**：先验证托盘打开（根窗隐藏）→ 设置窗口出现；必要时加平台层 `orderFrontRegardless`/`makeKeyWindow`。
3. **热键重注册的失败恢复**：用纯逻辑规划器 + 实机逐项验证；失败必须恢复旧键。
4. **排除目录全量重扫延迟**：异步 + 忙碌态；daemon 返回前 UI 不阻塞。
5. **字号全局替换面**：先扫完所有显式字号调用，避免遗漏造成“部分字变大”。
6. **根窗隐藏时的动作队列**：设置回调发动作后 `request_repaint_of(ROOT)`；实机验证根窗隐藏期 `App::logic` 仍被驱动（现有查询响应已依赖该路径）。若不可靠，改为在设置 pass 内直调 App 提供的回调。

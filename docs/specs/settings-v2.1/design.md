# 设置功能（v2.1 第一期）— 设计

**Status:** 设计已确认（2026-10-18），待实现
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
| `crates/app/src/ui/settings.rs`（新） | 设置窗口视口、布局、控件、录制状态机、排除目录/自启交互 |
| `crates/app/src/ui/settings_tokens.rs`（新） | `visual` §2.5 色板 + §2.2/2.4 几何常量；深/浅两套 + 单测钉住 |
| `crates/app/src/ui/font.rs`（新） | 全局文本倍率 `AtomicU32` + `size(base)` / `id(base)`；启动读取设置 |
| `crates/app/src/hotkey.rs`（改） | 可重配置：活动键共享状态、录制期 suspend/resume、设置启动偏好 |
| `crates/app/src/ui/mod.rs`（改） | 注册设置视口、入口（齿轮/托盘）、动作队列落地、字号变更重装 text styles |
| `crates/app/src/tray.rs`（改） | 「设置…」改为打开窗口；自启勾选与设置页同步 |
| `crates/app/src/platform/mac.rs`（改） | 原生目录选择器 `pick_directory`；设置窗口居中/置前（如需要） |
| `crates/cli/src/daemon.rs`（改） | `exclude remove` 允许移除已存在的相对名条目（默认 `node_modules`/`.git`），Add 仍严格校验 |

### 2.3 动作队列（设置 → App）

设置回调不直接碰 `HotkeyRuntime`/`TrayRuntime`，只写 `Shared.settings.actions`：

```rust
enum SettingsAction {
    BeginRecord(HotkeySlot),          // App: hotkey.suspend(), ready=true
    CancelRecord,                     // App: hotkey.resume()
    CommitHotkey { slot, hotkey },    // App: 尝试注册；成功持久化 + 更新快照
    AutostartChanged(bool),           // App: 同步托盘勾选
}
```

`App::logic` 每帧 drain：
- hotkey 动作在主线程处理（manager 只能在主线程用）；
- 结果写回 `settings.hotkey_snapshot` / `settings.hotkey_error`；
- 处理动作后 `request_repaint_of(SETTINGS_ID + ROOT)`，让设置窗口刷新「应用…/错误」。
- 设置窗口回调发动作后也 `request_repaint_of(ROOT)`；隐藏根窗的跨线程 repaint 有 ≥100ms 节流，设置页操作可接受。

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

设置页点击分段 → `set_scale` → 重装 text styles → 请求 ROOT/palette/settings 重绘 → 写 `settings.toml`（防抖/立即均可；本期立即写）。

---

## 五、设置窗口 UI

### 5.1 窗口

- deferred viewport builder：620×600、不可缩放、标题「设置」、透明标题栏 + fullsize content、原生交通灯、阴影。
- 内容层每帧铺 `window_fill`；内描边 1px 圆角 12。
- 首次打开居中于光标所在屏；再次打开只置前，不改位置。
- 设置窗口独立跟随系统深浅色；可见期由设置 pass 驱动主题探测（复用 `ThemeFollow`）。

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
- 开关：38×22、圆角 11；开态系统蓝；钮 18×18 白 + 1px 柔和投影。
- 语言：152×28 下拉按钮禁用态「简体中文」，tooltip「暂仅支持简体中文，语言切换将在后续版本提供」。
- 快捷键字段：152×28 键帽字段；录制中显示「按下快捷键…」，下方内联错误（冲突/注册失败/需修饰键）。
- 排除目录：Header 行「排除目录」+ 右侧「＋ 添加目录…」；目录行文件夹图标 16 + 路径 13 + 移除 ×14；列表超过卡片高度时在卡片内滚动。
- 所有文本走字号倍率；布局尺寸不随字号变。

---

## 六、快捷键

### 6.1 运行时改造

- `ActiveKeys { palette: Option<HotKey>, direct: Option<HotKey> }` 放 `Arc<Mutex<_>>`，事件泵每次事件读取当前 id，不再捕获固定键。
- `HotkeyRuntime` 暴露：
  - `snapshot() -> HotkeySnapshot`（当前键、显示名、启动回退提示）；
  - `suspend()` / `resume()`（录制期）；
  - `commit(slot, HotKey) -> Result<HotkeySnapshot, String>`（注册新键；失败恢复旧键）。
- 启动：
  - 读取 `settings.toml`；自定义键优先注册；
  - 自定义键注册失败 → 回落该槽默认键（悬浮条仍走原降级链；完整窗口走 ⌥⇧F），在设置页显示「已回落默认」；
  - 默认键注册失败的行为保持现状。
- 事件泵与主队列直调/兜底队列机制不变。

### 6.2 录制状态机

```
Idle → (点击字段) Beginning → (App suspend 完成) Recording
Recording → (Esc/点击他处/关闭窗口) Cancelled → App resume → Idle
Recording → (合法组合) Committing → App commit
Committing → 成功: Idle + 新键；失败: Recording + 内联错误（旧键已恢复）
```

- 合法组合：至少一个 ⌘/⌥/⌃/⇧ + 一个非修饰键；不支持纯修饰键、纯单键。
- 同一组合不能同时用于两个槽；冲突在本地拒绝。
- 系统占用/注册失败：App commit 失败 → 恢复旧键 → 内联「注册失败（可能被系统或其他应用占用）」。
- 显示名用 macOS 符号：⌘⌥⌃⇧ + 键名；默认键保持现有文案（⌥Space / ⌥⇧F）。
- 录制期间全局热键注销，确保按键进入设置窗口；App 记录旧键用于恢复。
- 启动时保存的键注册失败：回落默认 + 设置页警标；不自动改写文件（用户可重新录制）。

### 6.3 键位映射

- egui `Key` → `global_hotkey::hotkey::Code`：A–Z、0–9、F1–F12、Space、Enter、Tab、方向键、Home/End/PageUp/PageDown、常用标点。
- 修饰键：`command→SUPER`、`ctrl→CONTROL`、`alt→ALT`、`shift→SHIFT`。
- 不支持/未映射的键：忽略并提示「该键不支持」。
- 纯逻辑（映射、校验、显示格式化）单测钉住。

---

## 七、开机自启

- 打开设置页时查询 `autostart::is_enabled()`（失败按未启用 + 错误文案）。
- 点击开关：
  - `apply_user_choice(data_dir, desired)`；成功更新 UI + `SettingsAction::AutostartChanged(desired)` 同步托盘；
  - 失败恢复开关 + 内联错误（如开发形态「未打包」）。
- 托盘勾选自启成功 → 更新 `Shared.settings.autostart_enabled`（设置页打开时即时反映）。

---

## 八、排除目录

### 8.1 读取

- 打开设置页 → 工作线程 `client::exclude(paths, "list", None)`；结果写回 `excluded: Vec<String>`。
- 引擎离线：客户端返回错误 → 内联「引擎未运行，无法修改排除目录」；不直接改 `config.toml`。

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

---

## 九、入口

- 完整窗口齿轮：删除「设置将在 v2.1 提供」占位，改为 `settings_open(shared, ctx)`。
- 托盘「设置…」：删除 NSAlert 占位，改为 `settings_open`。
- 单实例：已打开则 `Visible(true)` + `Focus` + 置前；不新建第二窗。
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
| 排除目录读取失败 | 内联「引擎未运行/原因」，保留空列表状态 |
| 排除目录变更失败 | 保留旧列表 + 原样原因（含 daemon 校验文案） |
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

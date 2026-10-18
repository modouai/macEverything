# i18n 中英双语（v2.1）— 设计

**Status:** 已实现（2026-10-18），实机验收待辉哥（清单见 plan.md Task 7）
**范围:** 图形界面中英双语：悬浮条 / 完整结果窗口 / 设置窗口 / 托盘菜单 / 原生右键菜单与确认弹窗 / 应用自产状态与错误文案
**依据:** `docs/specs/v2-ui/product.md`（v2.1 交付「i18n 中英双语」）、`docs/specs/settings-v2.1/design.md`（语言项）、`docs/specs/floatbar-visual/settings-visual.md`（语言行形态）
**不交付:** 繁体中文/日韩等其它语言、CLI 与日志翻译、日期/数字区域化格式、引擎原始错误翻译

---

## 一、已定决策

| 决策 | 值 |
|---|---|
| 覆盖层 | 仅图形界面（含原生菜单/弹窗）；CLI、日志、引擎/客户端原始错误不翻译 |
| 语言集合 | 简体中文（zh-Hans）+ English |
| 设置项 | 语言行启用三选一下拉：跟随系统 / 简体中文 / English |
| 默认 | `settings.toml` 无 `language` → `system`；按系统首选语言解析 |
| 系统语言映射 | 按首选顺序取第一个受支持语言：`zh*` → 简体中文，`en*` → English；均无 → English；取不到/检测失败 → 简体中文 + 日志 |
| 生效时机 | 立即生效；三窗重绘、托盘菜单原地更新；不清空查询/选中/滚动位置 |
| 跟随系统的时机 | 启动时、用户选择「跟随系统」时解析；运行期不监听系统语言变化 |
| 持久化 | `settings.toml` 新增 `language = "system" \| "zh-hans" \| "en"`；保存失败仍本次生效 + 行内提示 |
| 技术方案 | 自建编译期文案目录（宏生成两套静态表），零新依赖 |
| 英文文案 | 由实现方起草，本文件附术语表；辉哥审阅定稿 |

---

## 二、架构

### 2.1 模块

新增仅 app crate 使用的 `crates/app/src/i18n/`：

| 文件 | 职责 |
|---|---|
| `mod.rs` | `Lang`、`LanguageSetting`、全局语言（`AtomicU8`）、`lang()/set_lang()`、`s()/strings(lang)`、系统语言解析纯函数、测试用 `with_lang` |
| `catalog.rs` | 宏一次性声明全部文案，生成 `Strings` 结构体、`ZH_HANS`/`EN` 静态表、`all_entries()` 测试清单 |
| `format.rs` | 带参数文案的类型化函数（如 `items(n)`、`gaps(n)`、`engine_state(..)`） |

依赖方向：`i18n` 只依赖标准库与 `log`；UI/托盘/平台层依赖 `i18n`，反向不依赖。`protocol`/`client`/`engine` 不感知 i18n。

### 2.2 文案目录

`catalog.rs` 以宏声明，每条文案只写一次：

```rust
catalog! {
    settings_title:     "设置"         / "Settings";
    search_placeholder: "搜索文件…"    / "Search files…";
    // ...
}
```

宏展开：

```rust
pub struct Strings { pub settings_title: &'static str, /* ... */ }

pub static ZH_HANS: Strings = Strings { settings_title: "设置", /* ... */ };
pub static EN: Strings = Strings { settings_title: "Settings", /* ... */ };

pub fn strings(lang: Lang) -> &'static Strings { /* ZH_HANS | EN */ }
pub fn s() -> &'static Strings { strings(lang()) }
pub fn all_entries() -> &'static [(&'static str, &'static str, &'static str)] { /* (key, zh, en) */ }
```

- 两套静态表必须填满结构体所有字段：**漏翻编译期报错**。
- `all_entries()` 供单测检查空串、占位符一致性。
- 调用点一律 `i18n::s().field`；新增语言只改 `i18n` 内部（`Lang`、解析、静态表），调用点不动。

### 2.3 全局语言与测试隔离

- `static LANG_BITS: AtomicU8`（`ZhHans = 0`、`En = 1`），沿用 `ui/font.rs` 的全局倍率模式。
- `set_lang` 仅在两个生产写入点调用：启动读取设置后、设置页切换语言后。
- 测试用线程局部覆盖 `with_lang(Lang, f)`；`lang()` 先读测试覆盖，再读原子。测试默认语言 = 简体中文，现有中文断言无需改动。
- 并行测试不得调用 `set_lang`（与 `font::set_scale` 同一纪律）。

### 2.4 系统语言解析

- 经 `objc2-foundation` 的 `NSLocale::preferredLanguages()` 读取 macOS 首选语言列表（新增 `NSLocale` feature）。
- 纯函数 `resolve_system_language(preferred: &[String]) -> Lang`：
  - 按顺序扫描，第一个以 `zh` 开头 → 简体中文；第一个以 `en` 开头 → English；
  - 均无 → English；
  - 空列表 → 简体中文。
- 平台调用失败 → 简体中文 + `log::warn`。
- 运行期不监听系统语言变化；改系统语言后需重启或重选「跟随系统」。

### 2.5 参数化文案

带变量的文案写成类型化函数，禁止在调用点手拼：

```rust
pub fn items(n: usize) -> String;            // "N 项" / "N items"
pub fn gaps(n: usize) -> String;             // "N 个目录未收录" / "N folders not indexed"
pub fn engine_state(state: EngineStateKey) -> String; // 托盘「引擎:…」/ "Engine: …"
pub fn hotkey_fallback(default: &str, used: &str) -> String;
// ...
```

- 函数内部按当前语言分支；两侧都用同一组参数。
- 模板占位符统一 `{name}` 形式，`all_entries()` 的单测比对中英占位符集合是否一致。

---

## 三、持久化与状态

### 3.1 `settings.toml`

```toml
language = "system"   # "system" | "zh-hans" | "en"
```

- `SettingsFile` 新增 `#[serde(default)] pub language: LanguageSetting`，默认 `System`。
- `LanguageSetting` 派生 `Serialize`；`Deserialize` 手写：未知值 → `System` + `log::warn`，**不连累同文件字号/快捷键字段**。
- 旧文件（无 `language`）→ `System`，行为与升级前一致（中文系统显示中文）。
- 保存沿用现有原子写入（tmp + rename）；失败返回 `anyhow::Result` 给调用方，由设置页内联提示。

### 3.2 启动

`main.rs` 读取 `SettingsFile` → `App::new`：

1. `let lang = resolve_language(settings.language);`
2. `i18n::set_lang(lang);`
3. 再安装字号样式、创建各视口 builder（窗口标题等取当前语言）。

顺序要求：`set_lang` 必须发生在**任何视口与托盘创建之前**，否则首帧/首次安装的菜单会取到默认中文。

### 3.3 设置页状态

- `SettingsData` 携带 `language_setting: LanguageSetting` 与 `language_error: Option<LanguageError>`（枚举，非已翻译串）。
- `SettingsAction` 新增 `SetLanguage(LanguageSetting)`；下拉点击只回传动作，设置在共享锁外落地。
- 落地顺序：解析目标 `Lang` → `i18n::set_lang` → `settings.file.language = setting` → 保存 → 失败置 `language_error` → 请求三视口重绘 → 置 `tray_relabel_pending`。

---

## 四、界面集成

### 4.1 设置窗口

- 语言行启用：152×28 下拉按钮，三项「跟随系统 / 简体中文 / English」；当前项打勾或高亮。
- 行标签、选项文案随当前语言。
- 保存失败：语言行下内联错误行（复用现有 `RowSpec.extra` 行高机制），文案「语言已切换，但设置保存失败」；下次启动回旧值。
- 切换语言不重置窗口位置、不重建窗口。

### 4.2 悬浮条与完整结果窗口

- 占位符、空态主/副文案、状态栏、列头、右键菜单、缺口面板全部改读 `i18n::s()`。
- 英文沿用现有截断/省略规则；状态栏按既有规则截断。
- 语言切换只触发重绘，不重发查询、不清空选中与滚动。

### 4.3 绘制数据原则

- 快照/状态只存枚举与数据，**不缓存已翻译字符串**；所有文案在出帧时取。
- **跨帧常驻**的应用自产提示（快捷键回落、自启审批、保存失败、排除目录失败）以类型化枚举 + 可选原始详情存储，绘制时取文案。
- **一次性**状态栏消息（操作成功的短暂提示等）在语言切换时清空重绘，避免中英混排。
- 引擎/客户端返回的原始错误串保持原样（技术细节），不拆解、不翻译。

---

## 五、原生层

### 5.1 托盘菜单

- 菜单项文案在安装时取当前语言；语言切换后必须显式刷新（原生菜单不随 egui 重绘）。
- `TrayRuntime` 保存各 `MenuItem`/`CheckMenuItem` 句柄（muda 项为 `Clone` 句柄）。
- `App::logic` 消费 `tray_relabel_pending`（主线程）调用 `relabel()`：
  - 对「打开搜索 / 打开完整窗口 / 开机自启 / 设置… / 退出」逐项 `set_text`；
  - 重新应用状态项与缺口项（`state_label`/`gaps_label` 取当前语言）；
  - 触发一次状态刷新，保证后续快照使用新语言。
- 工具提示保持品牌名 `macEverything`，不翻译。
- relabel 失败：日志告警，保留旧文案；内容窗口不受影响。

### 5.2 原生右键菜单与确认弹窗

- 右键菜单（打开 / 在访达中打开 / 在访达中显示 / 拷贝路径）与「恢复默认排除目录」确认弹窗均为**每次调用时新建**，直接从 `i18n::s()` 取文案，天然跟随当前语言。
- 弹窗按钮顺序与默认按钮语义不变：第一按钮「取消」为默认 Return，第二按钮「恢复默认」须显式点击。
- 注：AppKit 只对标题恰为 `Cancel` 的按钮自动绑定 Esc；英文界面下 `Cancel` 会自动获得 Esc（语义等同取消），中文「取消」仍不绑定——两者都只会取消，不会误触「恢复默认」。

### 5.3 原生窗口标题

- 设置窗原生标题随语言：`设置` ↔ `Settings`。
- 平台层按标题查找窗口时**同时接受两种语言的设置窗标题**（`settings_window_titles()`），避免切换瞬间「标题已变、查找仍用旧值」的竞态。
- 悬浮条内部标题 `palette` 与根窗标题 `macEverything` 不变（品牌/内部标识，不翻译）。
- 设置视口 builder 的 `with_title` 读当前语言；语言切换后随下一帧 builder patch 更新。
- `platform::SETTINGS_TITLE` 常量由 `i18n` 取文案的函数替代，平台层与相关单测同步更新。

---

## 六、错误处理

| 场景 | 行为 |
|---|---|
| 系统语言检测失败 | 简体中文 + `log::warn` |
| `settings.toml` 未知 `language` 值 | 按 `system` 处理 + `log::warn`；其它字段保留 |
| 设置保存失败 | 本次运行仍切换；语言行内联提示；下次启动回旧值 |
| 托盘 relabel 失败 | 日志告警；保留旧菜单文案；下次切换/重启自愈 |
| 文案目录漏项 | 编译错误 |
| 中英占位符不一致/空串 | 单测失败 |
| 设置窗标题查找 | 新旧两种标题都接受，无竞态 |

---

## 七、测试与验收

### 7.1 单测

- 语言解析表：`["en-US"]`、`["zh-Hans-CN"]`、`["zh-Hant-TW"]`、`["ja-JP"]`、`["fr-FR","en-US"]`、`[]`。
- `settings.toml`：缺字段 → `system`；未知值 → `system` 且字号/快捷键保留；往返保存；保存失败路径。
- 状态策略：切换成功清错误；保存失败保留已应用语言 + 错误；「跟随系统」解析结果正确。
- 目录完整性：`all_entries()` 全部非空、无首尾空格；中英占位符集合一致。
- 平台窗口标题匹配：中/英两种设置窗标题均可匹配，其它标题不匹配。

### 7.2 Headless UI

- 两语言下：设置页三组六项全部标签、语言下拉三项、字号三档；悬浮条占位/空态；结果窗口列头/状态/空态/缺口面板；右键菜单四项；托盘状态与缺口文案。
- 英文下设置页关键文本宽度不超过控件宽度；结果状态栏按既有规则截断不溢出。
- 现有中文断言默认 zh-Hans 保持通过；英文用例统一 `with_lang`。

### 7.3 实机验收

1. 英文系统首启：三窗 + 托盘 + 右键菜单 + 确认弹窗全英文。
2. 切「简体中文」：立即生效；重启保持；托盘菜单同步更新。
3. 切「跟随系统」：按当前系统首选语言解析。
4. 系统中文 + 显式选 English：重启后仍 English（不再跟随）。
5. 英文下无裁切/溢出；查询、排序、多选、快捷键、字号、自启、排除目录无回归。
6. CLI 与日志仍为中文（预期）。

### 7.4 收尾命令

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 八、英文术语表（待辉哥审阅）

| 中文 | English |
|---|---|
| 设置 | Settings |
| 语言 | Language |
| 跟随系统 | System Default |
| 简体中文 | 简体中文（语言名保留自身语言写法） |
| English | English |
| 字号 | Text Size |
| 小 / 标准 / 大 | Small / Standard / Large |
| 开机自启 | Launch at Login |
| 唤起快捷键 | Quick Search Shortcut |
| 完整窗口快捷键 | Full Window Shortcut |
| 排除目录 | Excluded Folders |
| 未排除任何目录 | No excluded folders |
| ＋ 添加目录… | Add Folder… |
| 恢复默认 | Restore Defaults |
| 加载中… | Loading… |
| 重扫中… | Rescanning… |
| 搜索文件… | Search files… |
| 名称 / 大小 / 修改时间 / 路径 | Name / Size / Date Modified / Path |
| 无匹配项 | No matches |
| 换个关键词试试 | Try a different keyword |
| 打开 | Open |
| 在访达中打开 | Open in Finder |
| 在访达中显示 | Show in Finder |
| 拷贝路径 | Copy Path |
| 取消 | Cancel |
| 退出 | Quit |
| 打开搜索 | Open Quick Search |
| 打开完整窗口 | Open Full Window |
| 权限缺口 | Permission Gaps |
| N 个目录未收录 | N folders not indexed |
| 引擎:就绪 | Engine: Ready |
| 引擎:构建中 | Engine: Building |
| 引擎:补齐中 | Engine: Reconciling |
| 引擎:重扫中 | Engine: Rescanning |
| 引擎:未运行(离线索引) | Engine: Not Running (Offline Index) |
| 引擎:未运行 | Engine: Not Running |
| 恢复默认排除目录？ | Restore default excluded folders? |
| 该快捷键已用于另一个功能 | This shortcut is already used for another feature. |

> 完整逐条文案在实现时落进 `catalog.rs`，本表只锁定关键术语；审阅时可直接改表。

---

## 九、实现拆分（高层，详见 plan.md）

1. **I1：i18n 核心** — 模块、宏、全局语言、系统解析、设置字段与兼容、单测。
2. **I2：设置面** — 设置页语言下拉、动作、保存失败策略、标签抽取。
3. **I3：悬浮条 + 结果窗口** — 全部用户可见文案抽取与双语布局。
4. **I4：托盘 + 原生层** — 菜单 relabel、右键菜单、确认弹窗、窗口标题与平台查找。
5. **I5：状态消息类型化** — 快捷键回落、错误/提示的类型化与切换清空策略。
6. **I6：英文文案定稿 + 验收** — 术语表回填、实机清单、文档状态更新。

---

## 十、风险与约束

- **文案量**：界面用户可见文案约 400–600 条，机械改动面大；按窗口/层拆任务，每步跑测试，避免一次性大爆炸。
- **配置兼容**：新增字段必须缺省可用；未知语言值不能拖垮字号/快捷键。
- **原生菜单**：托盘不随 egui 重绘，必须显式 relabel；只能在主线程做。
- **窗口标题查找**：必须接受新旧两种标题，否则语言切换瞬间可能定位不到设置窗。
- **测试并行**：语言是进程级状态，测试只能用线程局部覆盖，禁止 `set_lang`。
- **混合语言**：引擎/客户端原始错误与日志保持中文，属已接受的技术细节展示。
- **英文布局**：固定 620×600 设置窗与状态栏需专门验裁切；必要时只调英文文案长度，不改几何 token。
- **后续语言**：新增语言需补系统解析映射、静态表与术语表；调用点无需改动。

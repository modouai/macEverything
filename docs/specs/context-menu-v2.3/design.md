# 右键菜单 v2.3 设计：快速查看 + 拷贝增强

- 日期：2026-10-18
- 状态：设计草案（待评审）
- 依赖：`docs/specs/context-menu-v2.2/design.md`（删除「在访达中打开」）
- 说明：本文的菜单结构是**最终形态**；若 v2.2 与 v2.3 合并实施，以本文为准。

## 1. 背景与目标

搜索结果出来后的下一步，通常是「先看一眼内容」或「把文件/路径拿去做别的事」。当前只有
打开 / 在访达中显示 / 拷贝路径，缺少两个高频动作：

- **快速查看**：不打开文件，按空格就能预览。macOS 标准交互，搜索工具最自然的下一步。
- **拷贝增强**：除了完整路径，用户还需要拷贝文件本体（粘到 Finder/邮件/上传）和文件名。

本批只做这两项；打开方式、分享、移到废纸篓等暂不做（见 §12）。

目标：

1. 完整结果窗口支持快速查看：空格 + 右键项，多选进 Quick Look 画廊。
2. 「拷贝路径」升级为「拷贝 ▸」子菜单：拷贝文件 / 拷贝路径 / 拷贝文件名。
3. 顺带完成右键菜单的中英文案（i18n v2.1 Step 4），新增文案全部入目录。
4. 快速查看先过一个 PoC 门槛，验证通过再进正式实现。

## 2. 范围

**包含**

- 快速查看（`QLPreviewPanel`）：空格键、右键项、多选画廊、焦点返回。
- 拷贝子菜单：文件（NSPasteboard 文件 URL）、路径（既有）、文件名（新增）。
- 菜单桥接升级：支持子菜单与可靠的选择回传（target/action）。
- 右键菜单 i18n 落地（含既有三项）。
- 单测、实机验收、文档同步。

**不包含**

- 悬浮条（palette）的快速查看：它在失焦时自动隐藏，Quick Look 面板一抢焦点就会闪没。
  完整结果窗口先行，悬浮条留待单独评估。
- 打开方式子菜单、分享、移到废纸篓。
- 拷贝的全局快捷键（⌘C / ⌘⌥C 等）；本次只做菜单项与空格预览。
- 拖拽结果出窗口、AX 系统右键菜单。

## 3. 最终菜单

```
打开
在访达中显示
快速查看
———（分隔线）
拷贝 ▸
  拷贝文件
  拷贝路径
  拷贝文件名
```

| 菜单项 | 行为 | 多选 |
|---|---|---|
| 打开 | 文件=默认应用；目录=访达进入 | 逐项 |
| 在访达中显示 | 父目录中选中该项 | 逐项 |
| 快速查看 | 空格同义；打开 Quick Look 面板 | 画廊（全部选中项） |
| 拷贝文件 | 文件 URL 写入剪贴板 | 全部写入 |
| 拷贝路径 | 完整路径文本 | 逐行 |
| 拷贝文件名 | 文件名文本 | 逐行 |

- 子菜单标题「拷贝」无 tag、无 action，仅承载子菜单。
- `NSMenu` tag（进程内标识，无持久化）：

| 动作 | tag |
|---|---|
| 打开 `Open` | 1 |
| 在访达中显示 `RevealInFinder` | 2 |
| 拷贝路径 `CopyPaths` | 3 |
| 快速查看 `QuickLook` | 4 |
| 拷贝文件 `CopyFile` | 5 |
| 拷贝文件名 `CopyFileName` | 6 |

- 菜单每次右键重建，文案取当前语言（`i18n::s()`）。
- 选中 ≥1 行才弹出；空选不弹（既有约束）。

## 4. 快速查看

### 4.1 行为

- **触发**：
  - 结果表格获得键盘焦点、搜索框未聚焦、选中 ≥1 行时，按 **空格**；
  - 右键菜单「快速查看」。
- **多选**：把全部选中项作为预览项传给 `QLPreviewPanel`；面板内左右方向键切换。
- **开关语义**：
  - 空格：面板未显示 → 打开；已显示 → 关闭（Finder 同款 toggle）；
  - 菜单项：始终为当前选中项打开/更新面板并置于前台，不承担关闭语义。
- **键盘**：面板为 key 时，空格/Esc 关闭，方向键切换，由 Quick Look 自身处理；
  结果窗口不抢占这些键。
- **搜索框**：搜索框聚焦时空格照常输入空格，**不触发预览**。
- **焦点返回**：面板关闭后，结果窗口重新成为 key window；若系统未自动恢复，
  平台层显式激活 App 并 `makeKeyAndOrderFront` 结果窗口。
- **校验**：全部选中路径存在；任一缺失 → 沿用「文件不存在，索引可能待更新」，不打开面板。
- **目录/不支持的类型**：不做特殊处理，交给 Quick Look（文件夹会显示文件夹预览/图标）。
- **权限**：不需要任何 TCC 权限。

### 4.2 实现

- 依赖 `objc2-quick-look-ui = "0.3.2"`，关闭默认特性，只开
  `std`、`objc2-app-kit`、`QLPreviewPanel`、`QLPreviewItem`（`NSURL` 已原生符合
  `QLPreviewItem`）。
- 新增 `platform/mac_quicklook.rs`：
  - `QLPreviewPanel::sharedPreviewPanel(mtm)` 单例；设置 data source/delegate。
  - 用 `define_class!` 定义 data source 类，实现
    `numberOfPreviewItemsInPreviewPanel:` 与 `previewPanel:previewItemAtIndex:`；
    预览项直接返回 `NSURL`。
  - 选中 URL 保存在 data source 实例中（`Vec<Retained<NSURL>>`）；data source 与
    delegate 被面板弱引用，必须由平台层静态持有（主线程），直到面板关闭/进程退出。
  - 展示：设置 data source → `reloadData` → `makeKeyAndOrderFront`；关闭后清空 URL。
  - 焦点返回：监听面板关闭（`NSWindowWillCloseNotification` / delegate），
    重新激活结果窗口；具体路径由 PoC 决定（见 §4.3）。
  - 面板与 AppKit 调用全部主线程；unsafe 集中 + SAFETY 注释。
- 空格键处理在 `results.rs`：
  - 仅当结果表格获得焦点、搜索框未聚焦、选中非空；
  - 忽略按键自动重复（一次按下只 toggle 一次）；
  - 调用 `platform::quick_look(paths, frame)`；失败落状态栏。
- 右键项与空格共用同一平台入口。

### 4.3 PoC 门槛（实现前置）

Quick Look 是唯一有集成不确定性的部分，先做最小 PoC（不合并产品代码）：

1. LSUIElement + eframe 窗口下能正常显示 `QLPreviewPanel`。
2. 空格打开；面板为 key 后空格/Esc 能关闭；关闭后结果窗口恢复键盘焦点。
3. 多选画廊可显示全部项，左右方向键切换。
4. 搜索框聚焦时空格只输入空格，不触发预览。
5. 文件缺失/无预览类型不崩溃、行为可预期。

PoC 不通过时的退路（按优先级）：

- 显式实现 `QLPreviewPanelController` 的 responder 链接管（`acceptsPreviewPanelControl:` /
  `beginPreviewPanelControl:` / `endPreviewPanelControl:`）；
- 若仍不可靠，降级为 `qlmanage -p` 独立进程预览（体验差，默认不做）；
- 两者都不可接受则砍掉快速查看，只保留拷贝增强。

### 4.4 PoC 结果（2026-10-18，macOS 26）

实现：`scripts/poc/poc8_quicklook`（eframe 0.37 + objc2-quick-look-ui 0.3.2，Accessory 策略；
自动注入模式 `POC8_AUTO=1`，日志 `/tmp/poc8-quicklook.log`）。

实测结果：

| 验证项 | 结果 |
|---|---|
| 面板显示 | `panelVisible=true panelKey=true`；README.md 正常渲染 |
| 空格打开 / 空格关闭 | 均成功（面板自行处理关闭） |
| Esc 关闭 | 成功 |
| 关闭后焦点返回 | `appActive=true mainKey=true`；平台层 `restore_focus` 生效 |
| 多选画廊 | 3 项；→×2 使 index 0→1→2，← 回到 1 |
| 搜索框空格 | 输入 `" "` / `"  "`，未触发预览 |
| 进程退出 | 正常 |

未覆盖：缺失文件/无预览类型（产品前置校验会拦截缺失；无预览类型交给 Quick Look 自身，
留实机验收）。**结论：PoC 通过，可进入实现。**

## 5. 拷贝增强

### 5.1 拷贝文件

- 把选中文件的 `NSURL` 写入**通用剪贴板**：
  `NSPasteboard.generalPasteboard()` → `clearContents()` →
  `writeObjects(&NSArray<NSURL>)`。
- 语义 = Finder 的「拷贝」：粘贴到 Finder 会复制文件、粘贴到邮件会作为附件；
  **不复制文件内容**，只放文件引用。
- 多选：全部 URL 一次写入。
- `writeObjects` 返回 false → 状态栏「拷贝文件失败」。
- 需要给 `objc2-app-kit` 增加 `NSPasteboard` feature（其余复用现有 NSURL/NSArray）。
- 主线程执行（AppKit 剪贴板）。

### 5.2 拷贝路径

- 保持现状：完整路径逐行 join，`pbcopy`，无尾换行。

### 5.3 拷贝文件名

- 纯逻辑：对每个路径取 `Path::file_name()`，`to_string_lossy()`；
  `file_name()` 为 None（如根目录 `/`）时回退为完整路径。
- 多选逐行 join，无尾换行；`pbcopy`。
- 复用现有文本拷贝路径（`copy_to_clipboard`），错误文案改为可区分「拷贝路径失败 /
  拷贝文件名失败」。

### 5.4 前置校验

与既有动作一致：任一路径缺失则整批不执行并报「文件不存在，索引可能待更新」。
两个拷贝动作都不改变文件系统，但保持统一口径，避免部分成功。

## 6. 菜单桥接升级（子菜单与可靠选择）

现状：`platform::popup_menu` 创建无 action 的菜单项，弹出返回后读
`highlightedItem().tag()`。该方式在**子菜单**下不可靠（根菜单的 highlightedItem 可能是
子菜单父项，而不是叶子项），必须升级。

方案：

- 在平台层定义一个 Objective-C target 类，带 `menuItemChosen:` action；
  每个**叶子**菜单项设置 target/action，action 把 `sender.tag()` 写入进程内静态
  （`AtomicI64`，哨兵 = 未选择）。
- 弹出前重置哨兵；`popUpMenuPositioningItem` 返回后读取并清空。
- 子菜单父项只 `setSubmenu`，无 action；分隔线无 action；禁用项显式 `setEnabled(false)`
  （继续 `setAutoenablesItems(false)`）。
- target 实例由平台层静态持有（NSMenuItem 的 target 为弱引用）；菜单构建/销毁期间保持存活。
- `platform::MenuEntry` 扩展：

```rust
pub enum MenuEntry {
    Item { title: &'static str, tag: i64, enabled: bool },
    Separator,
    Submenu { title: &'static str, items: Vec<MenuEntry> },
}
```

- 回归要求：平铺项的选择、禁用项、取消/Esc 的行为与现状一致；子菜单叶子返回正确 tag。

## 7. 代码改造点

- `crates/app/src/ui/menu_actions.rs`
  - `MenuAction` 增加 `QuickLook` / `CopyFile` / `CopyFileName`（在 v2.2 三项基础上）。
  - `ShellAction` 扩展 `CopyFileName`（文本类）；`QuickLook` / `CopyFile` 为平台动作，
    不进入 shell plan。
  - `menu_entries()` 产出 §3 的嵌套结构；子菜单标题用 i18n。
  - 新增 `names_text(paths)` 纯函数与单测。
- `crates/app/src/ui/results.rs`
  - 空格键处理（表格焦点/搜索框未聚焦/非重复）。
  - 动作分派：`QuickLook` → `platform::quick_look`；`CopyFile` → `platform::copy_files`；
    其余 → `menu_actions::execute(ShellAction)`。
  - 菜单项构建改为嵌套 `MenuEntry`。
- `crates/app/src/platform/mod.rs`
  - `MenuEntry` 增加 `Submenu`；`popup_menu` 走 target/action。
  - 新增 facade：`quick_look(paths, frame)`、`copy_files(paths)`（非 macOS 返回不支持）。
- `crates/app/src/platform/mac.rs`
  - 菜单构建支持子菜单、target/action、分隔线、禁用项。
  - `copy_files`：NSPasteboard 写入 NSURL。
- `crates/app/src/platform/mac_quicklook.rs`（新增）
  - QLPreviewPanel 单例、data source/delegate、焦点返回。
- `crates/app/src/i18n/catalog.rs`
  - 新增：`menu_quick_look`、`menu_copy`、`menu_copy_file`、`menu_copy_path`、
    `menu_copy_name`、`quick_look_failed`、`copy_file_failed`、`copy_name_failed`；
    补齐既有 `menu_open` / `menu_show_in_finder`（Step 4 一并落地）。
- `crates/app/Cargo.toml`
  - `objc2-quick-look-ui`（features 见 §4.2）。
  - `objc2-app-kit` 增加 `NSPasteboard`。
- 注释与文档：results 模块头的右键说明、product/usage/i18n 文档。

## 8. 错误处理

| 场景 | 行为 |
|---|---|
| 路径缺失 | 沿用「文件不存在，索引可能待更新」，整批不执行 |
| 搜索框聚焦按空格 | 正常输入空格，不预览 |
| Quick Look 面板无法显示 | 状态栏「无法预览该项目」；PoC 兜底见 §4.3 |
| Quick Look 多选中部分文件缺失 | 整批前置校验拦截（与既有动作一致） |
| 剪贴板写入失败 | 状态栏「拷贝文件失败」 |
| 文本拷贝失败 | 状态栏「拷贝文件名失败 / 拷贝路径失败」 |
| 子菜单未选择（Esc/点外部） | 无动作（哨兵未命中） |
| 禁用项 | 不可选中；不触发 action |

## 9. 测试与验收

**单测（纯逻辑）**

- `menu_entries()`：顶层四项顺序（打开/在访达中显示/快速查看/分隔线/拷贝子菜单）、
  子菜单三项顺序与 tag、子菜单父项无 tag。
- tag 往返：1–6；0/未知不映射。
- `names_text`：单文件、多文件、根目录回退、空集。
- `plan`：CopyFileName 的 Pipe 载荷；既有三动作命令面不变。
- i18n 目录完整性（既有测试覆盖新 key 的中英非空与占位符）。

**实机验收**

- 空格：表格焦点、单选/多选、打开/关闭、Esc、方向键切换；关闭后焦点回到结果窗口。
- 搜索框聚焦时空格输入正常。
- 右键「快速查看」与空格打开面板；面板已打开时点菜单项 → 更新为当前选中项并置前（不关闭）。
- 拷贝文件：粘到 Finder（生成副本）、粘到邮件（附件）、多选全部。
- 拷贝路径：单/多选文本正确；拷贝文件名：单/多选、根目录回退。
- 缺失文件：整批拒绝并提示。
- 子菜单选择：每个叶子 tag 正确；取消不误触。
- 中英文界面下全部菜单/错误文案正确。
- 无新增 TCC 权限弹窗。

## 10. 风险与缓解

| 风险 | 说明 | 缓解 |
|---|---|---|
| QLPreviewPanel 集成 | 面板跟随 responder 链，egui 窗口可能无法正确接管/关闭 | §4.3 PoC 前置；必要时实现 QLPreviewPanelController；再不行降级或砍掉 |
| LSUIElement 焦点 | 无 Dock 图标 App 的激活/焦点返回可能异常 | PoC 专项验证；关闭时显式激活结果窗口 |
| 子菜单选择回传 | `highlightedItem` 在子菜单下不可靠 | target/action 升级 + 实机回归 |
| 文件 URL 剪贴板兼容性 | 个别旧 App 只认 legacy filenames 类型 | 先写现代 NSURL 并实测 Finder/邮件；不通过再补 legacy 类型 |
| 空格键冲突 | 搜索框空格输入 / 列表滚动 | 仅在表格聚焦且搜索框未聚焦时拦截；忽略自动重复 |
| 菜单 i18n 回归 | 本次把既有三项一并本地化 | 复用已有 i18n 测试与双语实机清单 |

## 11. 文档同步

- `docs/specs/v2-ui/product.md` §2：菜单表更新为 §3 最终形态；补充快速查看与拷贝语义；
  验收清单增加空格/多选画廊/剪贴板实测。
- `docs/usage.md`：完整窗口键位表增加空格 = 快速查看；右键菜单表更新；拷贝行为说明。
- `docs/specs/i18n-v2.1/design.md` / `plan.md`：右键菜单 Step 4 由本任务落地；
  文案表增补快速查看/拷贝/错误文案。
- `docs/specs/full-window-visual/plan.md`：菜单条目列表更新。
- `docs/specs/context-menu-v2.2/design.md`：标注最终菜单以 v2.3 为准（v2.2 仍是删除项的独立前置）。
- 历史 PoC 记录不回溯改写。

## 12. 非目标

- 悬浮条快速查看。
- 打开方式、分享、移到废纸篓、压缩、标签、显示简介。
- 拷贝的全局快捷键、拖拽结果出窗口。
- AX 系统右键菜单（v2.2 已否决）。

## 13. 决策记录

1. **快速查看只做完整结果窗口**：悬浮条失焦即隐藏，与 QL 面板抢焦点冲突；完整窗口是主工作区。
2. **空格 toggle + 右键项**：符合 macOS 标准；菜单项提供可发现性。
3. **拷贝做成子菜单**：避免顶层菜单膨胀；为将来的「打开方式 ▸」复用子菜单桥接。
4. **拷贝文件 = 文件 URL，不复制内容**：与 Finder 的「拷贝」语义一致，粘贴目标决定用途。
5. **菜单 i18n 随本任务落地**：新增菜单项必须双语，避免与既有 i18n 工作混排。
6. **Quick Look 先 PoC 再实现**：集成风险集中在 responder 链与焦点，不能靠纸面设计拍板。

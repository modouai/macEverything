# 右键菜单 v2.2 + v2.3 合并实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **实施状态（2026-10-19）：** Task 1–3 已实现并通过双审；Task 4 文档已回填；Task 4 Step 6 实机验收待人工确认。已完成步骤已勾选。
> **实施偏差（以代码为准）：** Quick Look 用 existing_panel 避免查询时创建面板；show 校验 isVisible；poll 在 App 非 active 时跳过，系统关闭时清空 URL。空格仅打开方向做存在性校验，关闭方向不校验；select_all 置 table_focused。菜单分派对 shell()==None 用 debug_assert! + log::error!。拷贝路径不校验存在性，其余动作整批校验。Task 2 为未接线入口加的 dead_code allow 已全部删除。下文代码块为计划稿，与最终实现有差异时以本节为准。

**Goal:** 把完整结果窗口的右键菜单落地为 v2.3 最终形态：删除「在访达中打开」，加入快速查看、拷贝文件/路径/文件名子菜单，并完成菜单 i18n。

**Architecture:** 纯逻辑（菜单结构、tag、文本载荷）留在 `ui/menu_actions.rs` 并可单测；AppKit 桥接（NSMenu 子菜单、target/action、QLPreviewPanel、NSPasteboard）集中在 `platform/`；`ui/results.rs` 只做焦点/选择语义与动作分派。

**Tech Stack:** Rust 2021、eframe/egui 0.37、objc2 0.6、objc2-app-kit 0.3.2、objc2-quick-look-ui 0.3.2。

---

## 文件结构

| 文件 | 职责 |
|---|---|
| `crates/app/src/platform/mod.rs` | 平台 facade：`MenuEntry` 类型、菜单/快速查看/拷贝文件入口；非 macOS stub |
| `crates/app/src/platform/mac.rs` | NSMenu 递归构建 + target/action、NSPasteboard 拷贝文件、暴露 `frame_view` |
| `crates/app/src/platform/mac_quicklook.rs`（新建） | QLPreviewPanel 单例、数据源、开关/轮询/焦点归还 |
| `crates/app/src/ui/menu_actions.rs` | 菜单结构、tag 映射、文本载荷、shell 执行计划 |
| `crates/app/src/ui/results.rs` | 空格键、表格焦点、菜单动作分派 |
| `crates/app/src/i18n/catalog.rs` | 新增/删除菜单文案 |
| `crates/app/Cargo.toml` | `objc2-quick-look-ui` 依赖、`NSPasteboard` feature |
| `docs/usage.md` 等 | 菜单与键位文档同步 |

---

## Task 1: 原生菜单桥接支持子菜单与 target/action

**Files:**
- Modify: `crates/app/src/platform/mod.rs`
- Modify: `crates/app/src/platform/mac.rs`
- Modify: `crates/app/src/ui/results.rs`

- [x] **Step 1: 在 facade 定义 `MenuEntry` 并改 `popup_menu` 签名**

`crates/app/src/platform/mod.rs`，在 `popup_menu` 前加类型：

```rust
/// 原生菜单项（支持分隔线与子菜单；`tag` 只用于叶子项）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    Item {
        title: &'static str,
        tag: i64,
        enabled: bool,
    },
    Separator,
    Submenu {
        title: &'static str,
        items: Vec<MenuEntry>,
    },
}
```

把 `popup_menu` 的 `items: &[(&str, i64)]` 改为 `items: &[MenuEntry]`，doc 更新为「叶子项经 target/action 回传 tag；子菜单父项无 action」。非 macOS stub 同步改签名：

```rust
pub fn popup_menu(
    items: &[MenuEntry],
    at_view_pos: (f64, f64),
    frame: &eframe::Frame,
) -> Option<i64> {
    #[cfg(target_os = "macos")]
    {
        mac::popup_menu(items, at_view_pos, frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (items, at_view_pos, frame);
        None
    }
}
```

- [x] **Step 2: 在 `mac.rs` 增加 target 类与选择落点**

在 `SpaceChange` 相关静态附近加：

```rust
/// 菜单选择 tag 的进程内落点（target/action 回传；-1 = 未选择）。
static MENU_CHOICE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
const NO_MENU_CHOICE: i64 = -1;

define_class!(
    // SAFETY(define_class 的 unsafe 契约):超类 NSObject 无子类化约束;
    // 无新增实例变量;方法签名与 AppKit action(single sender)一致;无 Drop。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MaceMenuTarget"]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(menuItemChosen:))]
        fn menu_item_chosen(&self, sender: Option<&NSMenuItem>) {
            let tag = sender.map_or(NO_MENU_CHOICE, |item| item.tag() as i64);
            MENU_CHOICE.store(tag, std::sync::atomic::Ordering::SeqCst);
        }
    }

    unsafe impl NSObjectProtocol for MenuTarget {}
);

impl MenuTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY:主线程刚分配、ivars 已置;本类未覆写 init,走 NSObject。
        unsafe { msg_send![super(this), init] }
    }
}
```

在 `use super::{...}` 里加入 `MenuEntry`。

- [x] **Step 3: 递归构建菜单并改写 `popup_menu`**

替换 `build_context_menu` 为：

```rust
/// 构建右键菜单（叶子项 target/action;子菜单父项只挂 submenu）。
fn build_context_menu(
    mtm: MainThreadMarker,
    items: &[MenuEntry],
    target: &MenuTarget,
) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
    // 无 action 的项默认被自动验证判禁用 -> 关掉自动验证并逐项显式启用。
    menu.setAutoenablesItems(false);
    fill_menu(mtm, &menu, items, target);
    menu
}

fn fill_menu(mtm: MainThreadMarker, menu: &NSMenu, items: &[MenuEntry], target: &MenuTarget) {
    for entry in items {
        match entry {
            MenuEntry::Item {
                title,
                tag,
                enabled,
            } => {
                // SAFETY:selector 由本模块实现(menuItemChosen:);标题/键等价串为
                // 调用内构造的 NSString 临时值,AppKit 拷贝入项。
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(title),
                        Some(objc2::sel!(menuItemChosen:)),
                        &NSString::from_str(""),
                    )
                };
                item.setTag(*tag as NSInteger);
                item.setEnabled(*enabled);
                // target 是弱引用:target 变量活到 popup_menu 的阻塞弹出返回。
                let target_obj: &AnyObject = target.as_super().as_ref();
                // SAFETY:target_obj 指向本次调用内存活的 MenuTarget;setTarget 不转移所有权。
                unsafe { item.setTarget(Some(target_obj)) };
                menu.addItem(&item);
            }
            MenuEntry::Separator => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            MenuEntry::Submenu { title, items } => {
                // SAFETY:同 Item,但子菜单父项不接 action(仅承载 setSubmenu)。
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &NSString::from_str(title),
                        None,
                        &NSString::from_str(""),
                    )
                };
                item.setEnabled(true);
                let submenu =
                    NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
                submenu.setAutoenablesItems(false);
                fill_menu(mtm, &submenu, items, target);
                item.setSubmenu(Some(&submenu));
                menu.addItem(&item);
            }
        }
    }
}
```

`popup_menu` 内改为：

```rust
    let view = frame_view(frame)?;
    MENU_CHOICE.store(NO_MENU_CHOICE, std::sync::atomic::Ordering::SeqCst);
    let target = MenuTarget::new(mtm);
    let menu = build_context_menu(mtm, items, &target);
    let location = NSPoint::new(at_view_pos.0, at_view_pos.1);
    // ...（鼠标换算与日志不变）
    let popped = menu.popUpMenuPositioningItem_atLocation_inView(None, location, Some(&view));
    let raw = MENU_CHOICE.load(std::sync::atomic::Ordering::SeqCst);
    let choice = (raw != NO_MENU_CHOICE).then_some(raw);
```

日志把 `items.len()` 改为 `"{} 个顶层项"`，并在末尾带上 `choice`。

- [x] **Step 4: 结果窗口临时适配新签名（本任务不改变菜单内容）**

`crates/app/src/ui/results.rs` 的 `open_context_menu` 中：

```rust
        let (files, dirs) = menu_actions::count_types(&paths);
        let menu_items = menu_actions::menu_items(files, dirs);
        if menu_items.is_empty() {
            return;
        }
        let items: Vec<platform::MenuEntry> = menu_items
            .into_iter()
            .map(|(title, action)| platform::MenuEntry::Item {
                title,
                tag: action.tag(),
                enabled: true,
            })
            .collect();
```

弹出调用改为 `platform::popup_menu(&items, (pos.x as f64, pos.y as f64), frame)`。

- [x] **Step 5: 编译与回归**

Run: `cargo test -p macEverything-app`
Expected: 现有测试全部通过（菜单内容仍是旧四项）。

Run: `cargo clippy -p macEverything-app --all-targets -- -D warnings`
Expected: 零警告。

- [x] **Step 6: Commit**

```bash
git add crates/app/src/platform/mod.rs crates/app/src/platform/mac.rs crates/app/src/ui/results.rs
git commit -m "refactor(menu): 原生菜单桥接支持子菜单与 target/action"
```

---

## Task 2: Quick Look 与拷贝文件平台能力

**Files:**
- Modify: `crates/app/Cargo.toml`
- Create: `crates/app/src/platform/mac_quicklook.rs`
- Modify: `crates/app/src/platform/mac.rs`
- Modify: `crates/app/src/platform/mod.rs`

- [x] **Step 1: 加依赖与 feature**

`crates/app/Cargo.toml`：

```toml
objc2-quick-look-ui = { version = "0.3.2", default-features = false, features = ["std", "objc2-app-kit", "QLPreviewPanel", "QLPreviewItem"] }
```

在 `objc2-app-kit` features 末尾加：

```toml
    # v2.3 拷贝文件:NSPasteboard.writeObjects(NSURL 数组)。
    "NSPasteboard",
```

- [x] **Step 2: 新建 `mac_quicklook.rs`**

`crates/app/src/platform/mac_quicklook.rs` 全文：

```rust
//! Quick Look 预览（v2.3 §4;PoC-8 验证路径）。
//!
//! QLPreviewPanel 是进程单例；dataSource 为弱引用，故由本模块 thread_local 保活。
//! 面板由系统自行关闭（空格/Esc/关闭按钮）时，`poll` 检测可见性变化并归还焦点。

use std::cell::{Cell, RefCell};

use anyhow::{anyhow, Result};
use objc2::ffi::NSInteger;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSObject, NSString, NSURL};
use objc2_quick_look_ui::{QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource};

use crate::i18n;

thread_local! {
    /// 预览项（主线程；data source 回调读取）。
    static QL_URLS: RefCell<Vec<Retained<NSURL>>> = const { RefCell::new(Vec::new()) };
    /// 面板对 dataSource 为弱引用，必须由本线程保活。
    static QL_DATA_SOURCE: RefCell<Option<Retained<QlDataSource>>> = const { RefCell::new(None) };
    /// 上一帧面板可见性（检测系统侧关闭）。
    static PANEL_WAS_VISIBLE: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    // SAFETY(define_class 的 unsafe 契约):超类 NSObject 无子类化约束;
    // 无新增实例变量;两个方法签名与 QLPreviewPanelDataSource 一致;无 Drop。
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MaceQLDataSource"]
    struct QlDataSource;

    unsafe impl QLPreviewPanelDataSource for QlDataSource {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        unsafe fn number_of_preview_items(
            &self,
            _panel: Option<&QLPreviewPanel>,
        ) -> NSInteger {
            QL_URLS.with(|urls| urls.borrow().len() as NSInteger)
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        unsafe fn preview_item_at_index(
            &self,
            _panel: Option<&QLPreviewPanel>,
            index: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            QL_URLS.with(|urls| {
                urls.borrow()
                    .get(index as usize)
                    .cloned()
                    .map(ProtocolObject::from_retained)
            })
        }
    }

    unsafe impl NSObjectProtocol for QlDataSource {}
);

impl QlDataSource {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY:主线程刚分配、ivars 已置;本类未覆写 init,走 NSObject。
        unsafe { msg_send![super(this), init] }
    }
}

fn shared_panel(mtm: MainThreadMarker) -> Option<Retained<QLPreviewPanel>> {
    // SAFETY:QLPreviewPanel 单例,主线程凭 mtm 访问。
    unsafe { QLPreviewPanel::sharedPreviewPanel(mtm) }
}

fn install_data_source(mtm: MainThreadMarker, panel: &QLPreviewPanel) -> Result<()> {
    QL_DATA_SOURCE.with(|slot| {
        if slot.borrow().is_none() {
            let data_source = QlDataSource::new(mtm);
            // SAFETY:data_source 立即转存 thread_local,活到进程退出;
            // 面板对 dataSource 为弱引用,这里的保活是协议要求。
            unsafe {
                panel.setDataSource(Some(ProtocolObject::from_ref(&*data_source)));
            }
            *slot.borrow_mut() = Some(data_source);
        }
    });
    Ok(())
}

/// 打开/更新面板（始终置前，不承担关闭语义；菜单项用）。
pub fn show(paths: &[String], _frame: &eframe::Frame) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(anyhow!("{}", i18n::s().quick_look_failed));
    };
    let Some(panel) = shared_panel(mtm) else {
        return Err(anyhow!("{}", i18n::s().quick_look_failed));
    };
    install_data_source(mtm, &panel)?;
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(p)))
        .collect();
    QL_URLS.with(|slot| *slot.borrow_mut() = urls);
    NSApplication::sharedApplication(mtm).activate();
    // SAFETY:主线程;数据源与 URL 已就位时 reloadData/设置索引是协议允许顺序。
    unsafe {
        panel.reloadData();
        panel.setCurrentPreviewItemIndex(0);
    }
    panel.makeKeyAndOrderFront(None);
    PANEL_WAS_VISIBLE.with(|v| v.set(true));
    log::info!(
        "快速查看:打开 {} 项 panelVisible={} panelKey={}",
        paths.len(),
        panel.isVisible(),
        panel.isKeyWindow()
    );
    Ok(())
}

/// 关闭面板并归还焦点（空格 toggle 用）。
pub fn close(frame: &eframe::Frame) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if let Some(panel) = shared_panel(mtm) {
        panel.orderOut(None);
    }
    PANEL_WAS_VISIBLE.with(|v| v.set(false));
    restore_focus(frame);
}

/// 空格语义：面板未显示 -> 打开；已显示 -> 关闭。
pub fn toggle(paths: &[String], frame: &eframe::Frame) -> Result<()> {
    if visible() {
        close(frame);
        Ok(())
    } else {
        show(paths, frame)
    }
}

/// 面板当前是否可见（非主线程/不可用 = false）。
pub fn visible() -> bool {
    MainThreadMarker::new()
        .and_then(shared_panel)
        .is_some_and(|panel| panel.isVisible())
}

/// 每帧轮询：系统侧关闭面板后归还结果窗口焦点。
pub fn poll(frame: &eframe::Frame) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(panel) = shared_panel(mtm) else {
        return;
    };
    let visible = panel.isVisible();
    let was = PANEL_WAS_VISIBLE.with(|v| v.replace(visible));
    if was && !visible {
        log::info!("快速查看:面板已由系统关闭,归还焦点");
        restore_focus(frame);
    }
}

fn restore_focus(frame: &eframe::Frame) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    NSApplication::sharedApplication(mtm).activate();
    if let Some(view) = super::mac::frame_view(frame) {
        if let Some(window) = view.window() {
            window.makeKeyAndOrderFront(None);
        }
    }
}
```

- [x] **Step 3: 暴露 `frame_view` 并注册模块**

`crates/app/src/platform/mac.rs`：

```rust
pub(super) fn frame_view(frame: &eframe::Frame) -> Option<Retained<NSView>> {
```

`crates/app/src/platform/mod.rs`：

```rust
#[cfg(target_os = "macos")]
pub mod mac;
#[cfg(target_os = "macos")]
pub mod mac_quicklook;
```

- [x] **Step 4: facade 增加快速查看与拷贝文件**

`platform/mod.rs`：

```rust
/// 打开/更新 Quick Look 面板（主线程；失败返回可展示文案）。非 macOS = 不支持。
pub fn quick_look_show(paths: &[String], frame: &eframe::Frame) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac_quicklook::show(paths, frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, frame);
        Err(anyhow::anyhow!("非 macOS:Quick Look 不可用"))
    }
}

/// 空格开关语义（未显示 -> 打开；已显示 -> 关闭）。
pub fn quick_look_toggle(paths: &[String], frame: &eframe::Frame) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac_quicklook::toggle(paths, frame)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (paths, frame);
        Err(anyhow::anyhow!("非 macOS:Quick Look 不可用"))
    }
}

/// 面板是否可见。
pub fn quick_look_visible() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac_quicklook::visible()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// 每帧轮询系统侧关闭并归还焦点。
pub fn quick_look_poll(frame: &eframe::Frame) {
    #[cfg(target_os = "macos")]
    mac_quicklook::poll(frame);
    #[cfg(not(target_os = "macos"))]
    let _ = frame;
}

/// 把文件 URL 写入通用剪贴板（主线程；空集 no-op）。非 macOS = 不支持。
pub fn copy_files(paths: &[String]) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::copy_files(paths)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = paths;
        Err(anyhow::anyhow!("非 macOS:文件剪贴板不可用"))
    }
}
```

- [x] **Step 5: 在 `mac.rs` 实现 `copy_files`**

导入补充：

```rust
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardWriting, ...};
use objc2_foundation::{NSArray, ...};
```

函数：

```rust
/// 把选中文件 URL 写入通用剪贴板（v2.3 §5.1；语义 = Finder「拷贝」）。
pub fn copy_files(paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    if MainThreadMarker::new().is_none() {
        return Err(anyhow!("{}", crate::i18n::s().copy_file_failed));
    }
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(p)))
        .collect();
    let objects: Vec<&ProtocolObject<dyn NSPasteboardWriting>> = urls
        .iter()
        .map(|url| ProtocolObject::from_ref(&**url))
        .collect();
    let array = NSArray::from_slice(&objects);
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    if pasteboard.writeObjects(&array) {
        log::info!("拷贝文件:{} 项已写入剪贴板", paths.len());
        Ok(())
    } else {
        Err(anyhow!("{}", crate::i18n::s().copy_file_failed))
    }
}
```

- [x] **Step 6: 编译**

Run: `cargo check -p macEverything-app`
Expected: 通过。

Run: `cargo test -p macEverything-app`
Expected: 现有测试全部通过。

- [x] **Step 7: Commit**

```bash
git add crates/app/Cargo.toml crates/app/src/platform
git commit -m "feat(menu): 接入 Quick Look 面板与文件剪贴板"
```

---

## Task 3: 菜单终态、动作接线与 i18n（TDD）

**Files:**
- Modify: `crates/app/src/ui/menu_actions.rs`
- Modify: `crates/app/src/i18n/catalog.rs`
- Modify: `crates/app/src/ui/results.rs`

- [x] **Step 1: 先写失败测试**

把 `crates/app/src/ui/menu_actions.rs` 的 `#[cfg(test)] mod tests` 整体替换为：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{with_lang, Lang};
    use crate::platform::MenuEntry;

    fn assert_item(entry: &MenuEntry, expected_title: &str, expected_tag: i64) {
        match entry {
            MenuEntry::Item {
                title,
                tag,
                enabled,
            } => {
                assert_eq!(*title, expected_title);
                assert_eq!(*tag, expected_tag);
                assert!(enabled);
            }
            other => panic!("期望菜单项,得到 {other:?}"),
        }
    }

    /// v2.3 §3 最终菜单:顶层四项 + 分隔线 + 拷贝子菜单。
    #[test]
    fn menu_entries_final_shape() {
        let items = menu_entries();
        assert_eq!(items.len(), 5);
        assert_item(&items[0], "打开", MenuAction::Open.tag());
        assert_item(
            &items[1],
            "在访达中显示",
            MenuAction::RevealInFinder.tag(),
        );
        assert_item(&items[2], "快速查看", MenuAction::QuickLook.tag());
        assert_eq!(items[3], MenuEntry::Separator);
        match &items[4] {
            MenuEntry::Submenu { title, items } => {
                assert_eq!(*title, "拷贝");
                assert_eq!(items.len(), 3);
                assert_item(&items[0], "拷贝文件", MenuAction::CopyFile.tag());
                assert_item(&items[1], "拷贝路径", MenuAction::CopyPaths.tag());
                assert_item(&items[2], "拷贝文件名", MenuAction::CopyFileName.tag());
            }
            other => panic!("期望拷贝子菜单,得到 {other:?}"),
        }
    }

    /// tag 与设计表一致:1/2/4/5/3/6。
    #[test]
    fn tag_roundtrip() {
        for a in [
            MenuAction::Open,
            MenuAction::RevealInFinder,
            MenuAction::QuickLook,
            MenuAction::CopyFile,
            MenuAction::CopyPaths,
            MenuAction::CopyFileName,
        ] {
            assert_eq!(MenuAction::from_tag(a.tag()), Some(a));
        }
        assert_eq!(MenuAction::Open.tag(), 1);
        assert_eq!(MenuAction::RevealInFinder.tag(), 2);
        assert_eq!(MenuAction::CopyPaths.tag(), 3);
        assert_eq!(MenuAction::QuickLook.tag(), 4);
        assert_eq!(MenuAction::CopyFile.tag(), 5);
        assert_eq!(MenuAction::CopyFileName.tag(), 6);
        assert_eq!(MenuAction::from_tag(0), None);
        assert_eq!(MenuAction::from_tag(7), None);
    }

    /// 文案随语言切换(原生菜单直接取这些标题)。
    #[test]
    fn menu_entries_localized_in_english() {
        with_lang(Lang::En, || {
            let items = menu_entries();
            assert_item(&items[0], "Open", 1);
            assert_item(&items[1], "Show in Finder", 2);
            assert_item(&items[2], "Quick Look", 4);
            match &items[4] {
                MenuEntry::Submenu { title, items } => {
                    assert_eq!(*title, "Copy");
                    assert_item(&items[0], "Copy File", 5);
                    assert_item(&items[1], "Copy Path", 3);
                    assert_item(&items[2], "Copy File Name", 6);
                }
                other => panic!("期望拷贝子菜单,得到 {other:?}"),
            }
        });
    }

    /// 文件名载荷:取末段;根目录回退完整路径;多选逐行无尾换行。
    #[test]
    fn names_text_line_joins_and_falls_back_to_path() {
        let p = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        assert_eq!(names_text(&p(&["/a/b.txt"])), "b.txt");
        assert_eq!(names_text(&p(&["/a/b.txt", "/c/d/e.pdf"])), "b.txt\ne.pdf");
        assert_eq!(names_text(&p(&["/"])), "/", "根目录无 file_name 时回退完整路径");
        assert_eq!(names_text(&[]), "");
    }

    /// 平台动作不在 shell 计划里。
    #[test]
    fn platform_actions_have_no_shell_plan() {
        assert_eq!(MenuAction::QuickLook.shell(), None);
        assert_eq!(MenuAction::CopyFile.shell(), None);
        assert_eq!(MenuAction::Open.shell(), Some(ShellAction::Open));
        assert_eq!(
            MenuAction::CopyFileName.shell(),
            Some(ShellAction::CopyFileName)
        );
    }

    /// 存在性前置校验:任一缺失即失败(Quick Look / 拷贝文件复用)。
    #[test]
    fn ensure_existing_rejects_missing() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        let err = ensure_existing(std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"), "错误文案应可展示:{err}");
        ensure_existing(&[]).unwrap();
    }

    /// shell 命令面:打开/显示/拷贝路径/拷贝文件名。
    #[test]
    fn plan_command_lines() {
        let base = std::env::temp_dir().join(format!("mce-menu-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("dir_a")).unwrap();
        let f1 = base.join("a.txt");
        let f2 = base.join("b.txt");
        let g1 = base.join("dir_a/c.txt");
        std::fs::write(&f1, b"x").unwrap();
        std::fs::write(&f2, b"x").unwrap();
        std::fs::write(&g1, b"x").unwrap();
        let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
        let run = |args: Vec<String>| Step::Run {
            program: "open",
            args,
        };

        assert_eq!(
            plan(ShellAction::Open, &[s(&f1)]).unwrap(),
            vec![run(vec![s(&f1)])]
        );
        assert_eq!(
            plan(ShellAction::Open, &[s(&f1), s(&f2)]).unwrap(),
            vec![run(vec![s(&f1)]), run(vec![s(&f2)])],
            "多选逐项打开"
        );
        assert_eq!(
            plan(ShellAction::RevealInFinder, &[s(&f1), s(&f2)]).unwrap(),
            vec![
                run(vec!["-R".to_string(), s(&f1)]),
                run(vec!["-R".to_string(), s(&f2)])
            ]
        );
        assert_eq!(
            plan(ShellAction::CopyPaths, &[s(&f1), s(&f2)]).unwrap(),
            vec![Step::Pipe {
                text: format!("{}\n{}", s(&f1), s(&f2))
            }]
        );
        assert_eq!(
            plan(ShellAction::CopyFileName, &[s(&f1), s(&f2)]).unwrap(),
            vec![Step::Pipe {
                text: "a.txt\nb.txt".to_string()
            }]
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// 存在性前置校验:陈旧索引路径 -> 可展示错误,且不发动作。
    #[test]
    fn execute_missing_path_fails_before_spawn() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        let err = execute(ShellAction::Open, std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"), "错误文案应可展示:{err}");
        let err = execute(ShellAction::RevealInFinder, std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
        let err = execute(ShellAction::CopyFileName, std::slice::from_ref(&missing)).unwrap_err();
        assert!(err.to_string().contains("文件不存在"));
    }

    /// 执行错误随语言切换(状态栏直接展示)。
    #[test]
    fn execute_missing_path_error_is_localized() {
        let missing = "/tmp/mce-menu-actions-不存在-000000".to_string();
        with_lang(Lang::En, || {
            let err = execute(ShellAction::Open, std::slice::from_ref(&missing)).unwrap_err();
            assert!(
                err.to_string()
                    .contains("File not found; the index may be out of date"),
                "错误文案应随语言切换:{err}"
            );
        });
    }

    /// 空集防御:不发无参 `open`(会新开 Finder 窗口),返回 Ok。
    #[test]
    fn execute_empty_paths_is_noop() {
        execute(ShellAction::Open, &[]).unwrap();
        execute(ShellAction::CopyPaths, &[]).unwrap();
    }
}
```

- [x] **Step 2: 跑测试确认失败**

Run: `cargo test -p macEverything-app menu_actions`
Expected: 编译失败（`menu_entries` / `ShellAction` / `names_text` 尚不存在）。

- [x] **Step 3: 重写 `menu_actions.rs` 的非测试部分**

完整替换为：

```rust
//! 右键菜单动作(v2.2/v2.3;product §2 菜单表)。
//!
//! 两段结构:
//!
//! - 纯逻辑(`menu_entries` / `names_text` / 存在性校验 / tag 映射,可测);
//! - shell 动作执行:`plan`(动作 -> 命令/载荷步骤,纯数据可测)产出计划,`execute`
//!   只做落地(open / pbcopy 数组参数调起,禁 shell 拼接——platform-compat 红线)。
//!
//! 菜单最终形态(v2.3 §3):打开 / 在访达中显示 / 快速查看 / 分隔线 / 拷贝 ▸
//! (拷贝文件 / 拷贝路径 / 拷贝文件名)。快速查看与拷贝文件是平台动作,不进 shell plan。

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Result};

use crate::i18n;
use crate::platform::MenuEntry;

/// 菜单动作(tag 为 NSMenu 进程内标识,无持久化)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// 打开:文件 = 默认应用打开;目录 = 访达进入。
    Open,
    /// 在访达中显示:父目录中选中该项(`open -R`;逐项)。
    RevealInFinder,
    /// 快速查看:打开/更新 Quick Look 面板。
    QuickLook,
    /// 拷贝文件:文件 URL 写入剪贴板。
    CopyFile,
    /// 拷贝路径:全部路径逐行复制。
    CopyPaths,
    /// 拷贝文件名:文件名逐行复制。
    CopyFileName,
}

impl MenuAction {
    /// NSMenu tag(设计表 v2.3 §3;避开 0——NSMenuItem 默认 tag)。
    pub fn tag(self) -> i64 {
        match self {
            Self::Open => 1,
            Self::RevealInFinder => 2,
            Self::CopyPaths => 3,
            Self::QuickLook => 4,
            Self::CopyFile => 5,
            Self::CopyFileName => 6,
        }
    }

    /// tag -> 动作(未知 tag = None;调用方留痕,不静默)。
    pub fn from_tag(tag: i64) -> Option<Self> {
        match tag {
            1 => Some(Self::Open),
            2 => Some(Self::RevealInFinder),
            3 => Some(Self::CopyPaths),
            4 => Some(Self::QuickLook),
            5 => Some(Self::CopyFile),
            6 => Some(Self::CopyFileName),
            _ => None,
        }
    }

    /// 平台动作返回 None,由 ui 直接调用 `platform` facade。
    pub fn shell(self) -> Option<ShellAction> {
        match self {
            Self::Open => Some(ShellAction::Open),
            Self::RevealInFinder => Some(ShellAction::RevealInFinder),
            Self::CopyPaths => Some(ShellAction::CopyPaths),
            Self::CopyFileName => Some(ShellAction::CopyFileName),
            Self::QuickLook | Self::CopyFile => None,
        }
    }
}

/// 可计划/落地的 shell 动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAction {
    Open,
    RevealInFinder,
    CopyPaths,
    CopyFileName,
}

/// v2.3 §3 菜单结构(每次右键重建;文案取当前语言)。
pub fn menu_entries() -> Vec<MenuEntry> {
    let s = i18n::s();
    vec![
        MenuEntry::Item {
            title: s.menu_open,
            tag: MenuAction::Open.tag(),
            enabled: true,
        },
        MenuEntry::Item {
            title: s.menu_show_in_finder,
            tag: MenuAction::RevealInFinder.tag(),
            enabled: true,
        },
        MenuEntry::Item {
            title: s.menu_quick_look,
            tag: MenuAction::QuickLook.tag(),
            enabled: true,
        },
        MenuEntry::Separator,
        MenuEntry::Submenu {
            title: s.menu_copy,
            items: vec![
                MenuEntry::Item {
                    title: s.menu_copy_file,
                    tag: MenuAction::CopyFile.tag(),
                    enabled: true,
                },
                MenuEntry::Item {
                    title: s.menu_copy_path,
                    tag: MenuAction::CopyPaths.tag(),
                    enabled: true,
                },
                MenuEntry::Item {
                    title: s.menu_copy_name,
                    tag: MenuAction::CopyFileName.tag(),
                    enabled: true,
                },
            ],
        },
    ]
}

/// 文件名载荷:取末段;根目录等无末段时回退完整路径;逐行 join 无尾换行。
pub fn names_text(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| {
            Path::new(p)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.clone())
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 整批存在性校验(Quick Look / 拷贝文件等平台动作执行前调用)。
pub fn ensure_existing(paths: &[String]) -> Result<()> {
    for p in paths {
        ensure_exists(p)?;
    }
    Ok(())
}

/// 执行计划的一步(纯数据;`plan` 产出、`execute` 落地)。
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Run {
        program: &'static str,
        args: Vec<String>,
    },
    Pipe { text: String },
}

/// 动作 -> 执行计划(纯逻辑,可测)。先整批前置校验再产出步骤。
fn plan(action: ShellAction, paths: &[String]) -> Result<Vec<Step>> {
    match action {
        ShellAction::Open => {
            ensure_existing(paths)?;
            Ok(paths
                .iter()
                .map(|p| Step::Run {
                    program: "open",
                    args: vec![p.clone()],
                })
                .collect())
        }
        ShellAction::RevealInFinder => {
            ensure_existing(paths)?;
            Ok(paths
                .iter()
                .map(|p| Step::Run {
                    program: "open",
                    args: vec!["-R".to_string(), p.clone()],
                })
                .collect())
        }
        ShellAction::CopyPaths => Ok(vec![Step::Pipe {
            text: paths.join("\n"),
        }]),
        ShellAction::CopyFileName => {
            ensure_existing(paths)?;
            Ok(vec![Step::Pipe {
                text: names_text(paths),
            }])
        }
    }
}

/// 执行 shell 动作(plan -> 逐步骤落地)。失败返回可展示提示。
pub fn execute(action: ShellAction, paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        log::warn!("菜单动作 {action:?}:路径集为空,忽略");
        return Ok(());
    }
    let steps = plan(action, paths)?;
    for step in &steps {
        match step {
            Step::Run { program, args } => spawn_run(program, args)?,
            Step::Pipe { text } => {
                let failure = match action {
                    ShellAction::CopyFileName => i18n::s().copy_name_failed,
                    _ => i18n::s().menu_copy_failed,
                };
                copy_to_clipboard(text, failure)?;
            }
        }
    }
    log::info!(
        "菜单动作 {action:?}:{} 项 / {} 步已发起",
        paths.len(),
        steps.len()
    );
    Ok(())
}

fn ensure_exists(path: &str) -> Result<()> {
    if std::fs::metadata(path).is_err() {
        return Err(anyhow!("{}", i18n::s().menu_file_missing));
    }
    Ok(())
}

fn spawn_run(program: &'static str, args: &[String]) -> Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    match cmd.spawn() {
        Ok(mut child) => {
            let what = format!("{program} {}", args.join(" "));
            std::thread::spawn(move || match child.wait() {
                Ok(st) if !st.success() => log::warn!("{program} 退出异常({st}):{what}"),
                Ok(_) => {}
                Err(e) => log::warn!("等待 {program} 退出失败:{e}"),
            });
            Ok(())
        }
        Err(e) => Err(anyhow!("{}: {e}", i18n::s().menu_open_failed)),
    }
}

fn copy_to_clipboard(text: &str, failure: &str) -> Result<()> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow!("{failure}: {e}"))?;
    let write_res = match child.stdin.take() {
        Some(mut stdin) => stdin.write_all(text.as_bytes()),
        None => Ok(()),
    };
    let wait_res = child.wait();
    if let Err(e) = write_res {
        return Err(anyhow!("{failure}: {e}"));
    }
    match wait_res {
        Ok(st) if st.success() => Ok(()),
        Ok(st) => Err(anyhow!("{failure} (pbcopy exit {st})")),
        Err(e) => Err(anyhow!("{failure}: {e}")),
    }
}
```

- [x] **Step 4: 更新 i18n 目录**

`crates/app/src/i18n/catalog.rs` 的右键菜单段替换为：

```rust
    // 右键菜单
    menu_open: "打开" / "Open",
    menu_show_in_finder: "在访达中显示" / "Show in Finder",
    menu_quick_look: "快速查看" / "Quick Look",
    menu_copy: "拷贝" / "Copy",
    menu_copy_file: "拷贝文件" / "Copy File",
    menu_copy_path: "拷贝路径" / "Copy Path",
    menu_copy_name: "拷贝文件名" / "Copy File Name",
    menu_file_missing: "文件不存在,索引可能待更新" / "File not found; the index may be out of date",
    menu_open_failed: "打开失败" / "Failed to open",
    menu_copy_failed: "拷贝路径失败" / "Failed to copy path",
    quick_look_failed: "无法预览该项目" / "Could not preview this item",
    copy_file_failed: "拷贝文件失败" / "Failed to copy file",
    copy_name_failed: "拷贝文件名失败" / "Failed to copy file name",
```

删除 `menu_open_in_finder` 与 `menu_locate_failed`。

- [x] **Step 5: 更新 `results.rs` 的焦点、空格与动作分派**

1）`ResultsState` 增加字段（`input_focused` 旁）：

```rust
    /// 结果表格是否持键盘焦点(点击过行;搜索框聚焦时清除;空格 Quick Look 的门槛)。
    table_focused: bool,
```

`new()` 中初始化为 `table_focused: false`。

2）`on_window_shown()` 中加：

```rust
        self.table_focused = false;
```

3）`draw_impl` 顶部（`track_ime` 后）加轮询：

```rust
        // Quick Look 系统侧关闭 -> 归还焦点;面板可见期间每 200ms 唤醒轮询。
        if let Some(frame) = frame {
            platform::quick_look_poll(frame);
            if platform::quick_look_visible() {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
```

4）⌘A 分支后加空格检测：

```rust
        // 空格:表格有焦点(点击过行)且搜索框未聚焦 -> Quick Look 开关。
        // 搜索框聚焦时空格照常输入;忽略自动重复(长按只 toggle 一次)。
        let space_pressed = !self.input_focused
            && !self.ime_composing
            && self.table_focused
            && !self.selection.is_empty()
            && ui.input(|i| {
                i.events.iter().any(|ev| {
                    matches!(
                        ev,
                        egui::Event::Key {
                            key: egui::Key::Space,
                            pressed: true,
                            repeat: false,
                            modifiers,
                            ..
                        } if modifiers.is_none()
                    )
                })
            });
        if space_pressed {
            // 消费掉(含自动重复),避免传给其他控件。
            ui.input_mut(|i| {
                i.consume_key(egui::Modifiers::NONE, egui::Key::Space);
            });
        }
```

5）交互落地段（`if let Some((i, shift, cmd)) = row_click` 内）加：

```rust
            self.table_focused = true;
```

右键分支同样在 `open_context_menu` 开头设 `self.table_focused = true;`。

6）空格动作放在菜单动作之后：

```rust
        if space_pressed {
            if let Some(frame) = frame {
                let paths = self.selected_paths();
                if let Err(err) = platform::quick_look_toggle(&paths, frame) {
                    self.status_note = Some(StatusNote::Raw(err.to_string()));
                    self.status_note_is_error = true;
                }
            } else {
                log::warn!("空格快速查看:无 eframe Frame(测试路径),忽略");
            }
        }
```

7）输入焦点更新处：

```rust
        self.input_focused = input_resp.has_focus();
        if self.input_focused {
            self.table_focused = false;
        }
```

8）`open_context_menu` 重写选中/菜单/分派部分：

```rust
        converge_selection(&mut self.selection, &mut self.anchor, row);
        self.table_focused = true;
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let items = menu_actions::menu_entries();
        log::info!(
            "右键菜单:选中 {} 行,弹出于 ({:.0},{:.0})",
            paths.len(),
            pos.x,
            pos.y
        );
        let Some(frame) = frame else {
            log::warn!("右键菜单:无 eframe Frame(测试路径),不弹出");
            return;
        };
        let choice = platform::popup_menu(&items, (pos.x as f64, pos.y as f64), frame);
        match choice {
            None => log::info!("右键菜单:取消(未选择)"),
            Some(tag) => match menu_actions::MenuAction::from_tag(tag) {
                Some(action) => {
                    let result = match action {
                        menu_actions::MenuAction::QuickLook => menu_actions::ensure_existing(&paths)
                            .and_then(|()| platform::quick_look_show(&paths, frame)),
                        menu_actions::MenuAction::CopyFile => menu_actions::ensure_existing(&paths)
                            .and_then(|()| platform::copy_files(&paths)),
                        _ => action
                            .shell()
                            .map_or(Ok(()), |shell| menu_actions::execute(shell, &paths)),
                    };
                    match result {
                        Ok(()) => log::info!("右键菜单:执行 {action:?}({} 项)", paths.len()),
                        Err(err) => {
                            self.status_note = Some(StatusNote::Raw(err.to_string()));
                            self.status_note_is_error = true;
                        }
                    }
                }
                None => log::warn!("右键菜单:未知 tag {tag},忽略"),
            },
        }
        self.ctx.request_repaint();
```

9）新增辅助方法（`select_all` 旁）：

```rust
    /// 选中行 -> 路径集合(行号与结果同步维护)。
    fn selected_paths(&self) -> Vec<String> {
        self.selection
            .iter()
            .filter_map(|&i| self.results.get(i))
            .map(|e| e.path.clone())
            .collect()
    }
```

- [x] **Step 6: 跑测试确认通过**

Run: `cargo test -p macEverything-app menu_actions`
Expected: 全部 PASS。

Run: `cargo test -p macEverything-app`
Expected: 全部 PASS。

Run: `cargo clippy -p macEverything-app --all-targets -- -D warnings`
Expected: 零警告。

- [x] **Step 7: Commit**

```bash
git add crates/app/src/ui/menu_actions.rs crates/app/src/ui/results.rs crates/app/src/i18n/catalog.rs
git commit -m "feat(menu): v2.2/v2.3 最终菜单、快速查看与拷贝增强"
```

---

## Task 4: 文档同步与验收

**Files:**
- Modify: `docs/usage.md`
- Modify: `docs/specs/v2-ui/product.md`
- Modify: `docs/specs/i18n-v2.1/design.md`
- Modify: `docs/specs/i18n-v2.1/plan.md`
- Modify: `docs/specs/full-window-visual/plan.md`
- Modify: `docs/specs/context-menu-v2.2/design.md`
- Modify: `docs/specs/context-menu-v2.3/design.md`

- [x] **Step 1: `docs/usage.md`**

完整窗口表格段替换为：

```markdown
原生右键菜单(**系统菜单**,文件与目录同形):

| 菜单项 | 行为 |
|---|---|
| 打开 | 文件 = 默认应用打开;目录 = 访达进入 |
| 在访达中显示 | 父目录中选中该项 |
| 快速查看 | 打开 Quick Look 预览;与空格同义 |
| 拷贝 ▸ 拷贝文件 | 文件 URL 写入剪贴板(粘到 Finder 复制、邮件作附件) |
| 拷贝 ▸ 拷贝路径 | 复制完整路径(多选逐行) |
| 拷贝 ▸ 拷贝文件名 | 复制文件名(多选逐行;根目录回退完整路径) |

- 多选逐项执行;快速查看打开多选画廊(方向键切换)。
- 表格有焦点(点击过行)且搜索框未聚焦时,按**空格**打开/关闭快速查看。
```

- [x] **Step 2: `docs/specs/v2-ui/product.md`**

§2 右键菜单表替换为同一张表（保留「多选」列语义，可写成「逐项 / 画廊 / 全部写入」）；删除「文件/目录 / 四项」描述。验收清单中「四项语义」改为：

```markdown
- [ ] 右键菜单为**原生 macOS 菜单**,最终形态(打开/在访达中显示/快速查看/拷贝文件/拷贝路径/拷贝文件名)逐项实测
- [ ] 空格在表格焦点下打开/关闭 Quick Look;搜索框聚焦时只输入空格
```

- [x] **Step 3: i18n 文档**

`docs/specs/i18n-v2.1/design.md`：删除「在访达中打开 / Open in Finder」行；菜单列表改为最终六项。`plan.md` Step 4 的文案表同步 Task 3 Step 4；删除 `menu_locate_failed` 与 `menu_items(files, dirs)` 的计数分支。

- [x] **Step 4: 其他计划文档**

`docs/specs/full-window-visual/plan.md` 第 93 行菜单描述改为最终菜单。`docs/specs/context-menu-v2.2/design.md` 状态改为「已实现(v2.3 合并落地)」；`docs/specs/context-menu-v2.3/design.md` 状态改为「已实现(2026-10-19)」，并在 §4.4 后追加实施记录：平台层 polling 方案、菜单 target/action、单测覆盖点。

- [x] **Step 5: 全量门禁**

Run: `cargo fmt --check`
Expected: 无输出。

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: 零警告。

Run: `cargo test --workspace`
Expected: 全部 PASS。

- [ ] **Step 6: 实机验收（人工清单）**

构建并运行：

```bash
bash scripts/bundle.sh
open release/macEverything.app
```

逐项确认：

- 空格:表格焦点单选/多选打开;再按空格关闭;Esc 关闭;方向键切换画廊;关闭后焦点回到结果窗口。
- 搜索框聚焦时空格只输入空格。
- 右键:六个叶子项 tag 正确;取消不误触;子菜单可展开。
- 拷贝文件:粘到 Finder 生成副本、粘到邮件为附件;多选全部写入。
- 拷贝路径/文件名:单/多选文本正确;根目录回退完整路径。
- 缺失文件:打开、在访达中显示、快速查看（打开方向）、拷贝文件、拷贝文件名整批拒绝并提示;拷贝路径不校验存在性,空格关闭方向不校验。
- 中英文界面下菜单与错误文案正确;无新增 TCC 权限弹窗。
- 长按空格只切换一次（忽略自动重复）。
- 面板已打开时点右键「快速查看」→ 更新为当前选中项并置前，不关闭。
- ⌘Tab 切走/切回不抢焦点；关闭后立即输入首键是否丢失（200ms 轮询延迟）。
- QL 打开时点击完整窗口红点或按 ⌘W（隐藏窗口）→ 面板随之关闭。
- QL 打开时最小化完整窗口 → 面板保持显示（已知限制，确认是否接受）。
- 反复打开/关闭 Quick Look 多次，共享面板复用正常。
- QL 打开期间改选/结果替换 → 面板保持旧内容（已知限制，确认可接受）。

- [x] **Step 7: Commit**

```bash
git add docs
git commit -m "docs(menu): v2.2/v2.3 验收记录与文档回填"
```

---

## 自检记录

- 设计 §3 菜单结构:Task 3 `menu_entries_final_shape` 覆盖。
- 设计 §4 快速查看(空格/菜单/多选/焦点/搜索框):Task 2 平台层 + Task 3 空格接线 + Task 4 实机清单覆盖。
- 设计 §5 拷贝增强(文件/路径/文件名/前置校验):Task 2 `copy_files` + Task 3 `names_text`/`ensure_existing` 覆盖。
- 设计 §6 子菜单桥接:Task 1 target/action + 递归构建覆盖。
- 设计 §8 错误处理:平台错误文案、缺失路径前置校验、取消哨兵均落实。
- 设计 §9 测试:纯逻辑单测在 Task 3;AppKit 行为留实机清单(Task 4)。
- 设计 §11 文档同步:Task 4 逐项覆盖。

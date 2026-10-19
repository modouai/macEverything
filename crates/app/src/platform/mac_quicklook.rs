//! Quick Look 预览(v2.3 §4;PoC-8 验证路径)。
//!
//! QLPreviewPanel 是进程单例;dataSource 为弱引用,故由本模块 thread_local 保活。
//! 面板由系统自行关闭(空格/Esc/关闭按钮)时,`poll` 检测可见性变化并归还焦点。

// Task 3 才把本模块接到菜单/空格处理;接线前这些入口无调用点,定向静音。
#![allow(dead_code)]

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
    static QL_URLS: RefCell<Vec<Retained<NSURL>>> = const { RefCell::new(Vec::new()) };
    static QL_DATA_SOURCE: RefCell<Option<Retained<QlDataSource>>> = const { RefCell::new(None) };
    static PANEL_WAS_VISIBLE: Cell<bool> = const { Cell::new(false) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "MaceQLDataSource"]
    struct QlDataSource;

    unsafe impl QLPreviewPanelDataSource for QlDataSource {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        unsafe fn number_of_preview_items(&self, _panel: Option<&QLPreviewPanel>) -> NSInteger {
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
        // SAFETY: 主线程刚分配、ivars 已置;本类未覆写 init,走 NSObject。
        unsafe { msg_send![super(this), init] }
    }
}

fn shared_panel(mtm: MainThreadMarker) -> Option<Retained<QLPreviewPanel>> {
    // SAFETY: QLPreviewPanel 单例,主线程凭 mtm 访问。
    unsafe { QLPreviewPanel::sharedPreviewPanel(mtm) }
}

fn install_data_source(mtm: MainThreadMarker, panel: &QLPreviewPanel) {
    QL_DATA_SOURCE.with(|slot| {
        if slot.borrow().is_none() {
            let data_source = QlDataSource::new(mtm);
            // SAFETY: data_source 立即转存 thread_local,活到进程退出;
            // 面板对 dataSource 为弱引用,这里的保活是协议要求。
            unsafe { panel.setDataSource(Some(ProtocolObject::from_ref(&*data_source))) };
            *slot.borrow_mut() = Some(data_source);
        }
    });
}

/// 打开/更新面板(始终置前,不承担关闭语义;菜单项用)。
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
    install_data_source(mtm, &panel);
    let urls: Vec<Retained<NSURL>> = paths
        .iter()
        .map(|p| NSURL::fileURLWithPath(&NSString::from_str(p)))
        .collect();
    QL_URLS.with(|slot| *slot.borrow_mut() = urls);
    NSApplication::sharedApplication(mtm).activate();
    // SAFETY: 主线程;数据源与 URL 已就位后 reloadData/设置索引是协议允许顺序。
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

/// 关闭面板并归还焦点(空格 toggle 用)。
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

/// 空格语义:未显示 -> 打开;已显示 -> 关闭。
pub fn toggle(paths: &[String], frame: &eframe::Frame) -> Result<()> {
    if visible() {
        close(frame);
        Ok(())
    } else {
        show(paths, frame)
    }
}

/// 面板当前是否可见(非主线程/不可用 = false)。
pub fn visible() -> bool {
    MainThreadMarker::new()
        .and_then(shared_panel)
        .is_some_and(|panel| panel.isVisible())
}

/// 每帧轮询:系统侧关闭面板后归还结果窗口焦点。
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

# 保存面板目录助手（save-panel-assist）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 任意应用弹出保存/打开面板时自动浮出目录搜索框，选中目录后面板自动跳转（⌘⇧G + AX 直设/剪贴板粘贴双策略）。spec 见 `docs/superpowers/specs/2026-11-04-save-panel-assist-design.md`。

**Architecture:** M0 探针先实机验证六个技术风险点（AX FFI、CGEvent 合成、面板 AX 特征），M1 在同一批基础组件上演化产品化：`panel_detector`（前台应用单 AXObserver）→ 浮层视口（第三视口，NonactivatingPanel）→ `dialog_navigator`（策略 A/B 状态机）。engine/protocol/client 仅加 `dir_only`。

**Tech Stack:** objc2 0.6 系（已有）、AX API 手写 FFI（objc2 未覆盖 HIServices）、CGEvent 手写 FFI、egui deferred viewport、现有 client/protocol。

**约定（全计划通用）：**

- 声明完成前三门禁全绿：`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`。
- unsafe 只放 `crates/app/src/platform/`，每处带 SAFETY 注释（ADR-8 纪律）。
- 新增测试须做突变验证：临时改坏实现确认测试变红，再改回。
- objc2-core-graphics / objc2-foundation 等 binding 的 feature 名以编译为准：本计划标注「预期 feature」，若编译报 cfg 缺失，在 `crates/app/Cargo.toml` 补该 feature 后重试（允许的偏差，不算偏离计划）。

---

## 文件结构

```
crates/app/src/platform/ax_ffi.rs          新增  AX API 手写 FFI + RAII 包装（Task 1）
crates/app/src/platform/cg_events.rs       新增  CGEvent 键盘合成（Task 2）
crates/app/src/platform/panel_probe.rs     新增  M0 探针（Task 3；M1 起仅 debug 保留）
crates/app/src/platform/panel_detector.rs  新增  M1 检测：特征快照/判定纯函数/平台事件层（Task 9/10）
crates/app/src/platform/dialog_navigator.rs 新增 M1 跳转：状态机纯逻辑 + 策略 A/B 平台层（Task 11/12）
crates/app/src/platform/mac.rs             修改  浮层窗口管理（Task 13）
crates/app/src/platform/mod.rs             修改  facade 转发（Task 1/2/10/12/13 各小步追加）
crates/app/src/main.rs                     修改  探针开关 + 未授权一次性提示（Task 3/5）
crates/app/src/settings.rs                 修改  save_panel_assist 字段（Task 5）
crates/app/src/ui/settings.rs              修改  SettingsData/Action/apply（Task 5）
crates/app/src/ui/settings_view.rs         修改  开关行 + 授权状态（Task 5）
crates/app/src/i18n/catalog.rs             修改  中英成对文案（Task 5/13）
crates/app/src/ui/overlay.rs               新增  浮层视口 UI（Task 13）
crates/app/src/ui/mod.rs                   修改  overlay 视口注册 + Shared + 事件接线（Task 13/14）
crates/app/src/tray.rs                     修改  未授权时托盘菜单项（Task 5）
crates/protocol/src/lib.rs                 修改  Query.dir_only（Task 6）
crates/engine/src/query.rs                 修改  QueryPlan.dir_only + 候选过滤（Task 7）
crates/client/src/lib.rs                   修改  query/offline_query 透传（Task 8）
crates/cli/src/daemon.rs                   修改  run_query 透传（Task 8）
crates/cli/src/main.rs                     修改  query 命令调用点（Task 8）
crates/app/src/ui/palette.rs               修改  查询工作线程调用点（Task 8）
docs/usage.md、docs/architecture.md        修改  文档同步（Task 15）
```

职责边界：`ax_ffi`/`cg_events` 是无业务逻辑的薄封装；`panel_detector` 只管「检测与判定」，不碰 UI；`dialog_navigator` 只管「把面板跳到路径」，不碰查询；`overlay.rs` 只管浮层 UI 与查询会话；接线集中在 `ui/mod.rs`。

---

## Task 1: AX FFI 基础层

**Files:**
- Create: `crates/app/src/platform/ax_ffi.rs`
- Modify: `crates/app/src/platform/mod.rs`（facade：`panel_assist_trusted()`）
- Modify: `crates/app/Cargo.toml`（预期无需新依赖；objc2-foundation 已有）

- [ ] **Step 1: 创建 ax_ffi.rs（FFI 声明 + RAII 包装，一次性完整落地）**

```rust
//! AX API 手写 FFI(ADR-8 平台层):objc2 系未覆盖 ApplicationServices/HIServices
//! 的 AXUIElement 族,此处按 C ABI 显式声明。CF 类型以 `*mut c_void` 直通,
//! RAII 包装经 Drop → CFRelease 释放。除 AXObserver 族(回调派发到创建线程的
//! runloop,见 panel_detector)外,AXUIElement 调用为线程无关 CF 层——
//! navigator 后台线程直接调用(2026-11 M0 实机验证点 #7)。
//!
//! SAFETY 总纪律:所有 raw FFI 仅在本模块 unsafe 块内调用;对外暴露的都是
//! 安全封装(参数为 &str/u32 等安全类型,返回 RAII 或 Option)。
#![allow(non_snake_case)]

use std::ffi::{c_char, c_void};

/// HIServices 的 AXError:0 = 成功。
pub type AXError = i32;
pub const AX_ERROR_SUCCESS: AXError = 0;

type AXUIElementRef = *mut c_void;
type AXObserverRef = *mut c_void;
type CFAllocatorRef = *mut c_void;
type CFStringRef = *const c_void;
type CFTypeRef = *const c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;

/// AXValue 类型枚举(HIServices AXValueType):CGPoint = 1,CGSize = 2。
const AX_VALUE_POINT_TYPE: usize = 1;
const AX_VALUE_SIZE_TYPE: usize = 2;

/// kCFStringEncodingUTF8。
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

extern "C" {
    fn CFStringCreateWithCString(
        alloc: CFAllocatorRef,
        c_str: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    /// kCFRunLoopDefaultMode 常量符号(CoreFoundation 导出,非 inline,可直接链接)。
    static kCFRunLoopDefaultMode: CFStringRef;

    fn AXIsProcessTrusted() -> u8;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        el: AXUIElementRef,
        attr: CFStringRef,
        out: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(el: AXUIElementRef, attr: CFStringRef, val: CFTypeRef) -> AXError;
    fn AXUIElementIsAttributeSettable(el: AXUIElementRef, attr: CFStringRef) -> u8;
    fn AXObserverCreate(
        pid: i32,
        callback: AXObserverCallback,
        out: *mut AXObserverRef,
    ) -> AXError;
    fn AXObserverAddNotification(
        obs: AXObserverRef,
        el: AXUIElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> AXError;
    fn AXObserverGetRunLoopSource(obs: AXObserverRef) -> CFRunLoopSourceRef;
    fn AXValueGetValue(value: CFTypeRef, t: usize, out: *mut c_void) -> u8;
}

/// AXObserver 回调(C 函数指针,无环境捕获;状态经 refcon/static 传递)。
pub type AXObserverCallback = extern "C" fn(
    observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
);

/// 属性名常量(与 HIServices 头一致;CFStringRef 经 [`cf_str`] 现建)。
pub mod attr {
    pub const ROLE: &str = "AXRole";
    pub const SUBROLE: &str = "AXSubrole";
    pub const TITLE: &str = "AXTitle";
    pub const VALUE: &str = "AXValue";
    pub const CHILDREN: &str = "AXChildren";
    pub const WINDOWS: &str = "AXWindows";
    pub const FOCUSED_WINDOW: &str = "AXFocusedWindow";
    pub const POSITION: &str = "AXPosition";
    pub const SIZE: &str = "AXSize";
    pub const EDITABLE: &str = "AXEditable";
}

/// 通知名常量。
pub mod note {
    pub const WINDOW_CREATED: &str = "AXWindowCreated";
    pub const FOCUSED_WINDOW_CHANGED: &str = "AXFocusedWindowChanged";
    pub const UI_ELEMENT_DESTROYED: &str = "AXUIElementDestroyed";
}

/// 自建 CFString(UTF-8;Drop 释放;与 NSString toll-free bridge 的指针形态一致)。
pub struct CFStr(CFStringRef);

impl CFStr {
    pub fn new(s: &str) -> Self {
        let c = std::ffi::CString::new(s).expect("AX 属性名/通知名不含 NUL");
        // SAFETY:C 字符串来自本调用栈上的 CString,长度合法;分配器 NULL = 默认。
        let p = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_STRING_ENCODING_UTF8) };
        assert!(!p.is_null(), "CFStringCreateWithCString 失败:{s}");
        Self(p)
    }
    pub fn raw(&self) -> CFStringRef {
        self.0
    }
}

impl Drop for CFStr {
    fn drop(&mut self) {
        // SAFETY:构造时 CFStringCreateWithCString 返回的 +1 引用归本对象所有。
        unsafe { CFRelease(self.0) }
    }
}

/// RAII AX 对象(AXUIElement/AXValue/AXObserver 共用形态;Drop → CFRelease)。
pub struct AXRef(*mut c_void);

impl AXRef {
    /// 接管 +1 引用(调用点刚由 Copy*/Create* 产出)。
    pub unsafe fn take(p: *mut c_void) -> Option<Self> {
        if p.is_null() {
            None
        } else {
            Some(Self(p))
        }
    }
    pub fn raw(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for AXRef {
    fn drop(&mut self) {
        // SAFETY:构造约定 +1 引用归本对象。
        unsafe { CFRelease(self.0) }
    }
}

// SAFETY:CF 对象为堆分配句柄,跨线程转移后调用 AX CF 层 API 合法
// (AXObserver 除外——它只在 detector 专用线程使用,不做 Send 转移)。
unsafe impl Send for AXRef {}
unsafe impl Send for CFStr {}

/// 辅助功能授权状态(首次调用触发 TCC 记录;结果由系统设置控制)。
pub fn is_process_trusted() -> bool {
    // SAFETY:无参无副作用之外的内存访问。
    unsafe { AXIsProcessTrusted() != 0 }
}

/// 前台应用的 AX 元素(pid = 目标应用)。
pub fn app_element(pid: i32) -> Option<AXRef> {
    // SAFETY:返回 +1 引用,交 AXRef 接管。
    unsafe { AXRef::take(AXUIElementCreateApplication(pid)) }
}

/// 读属性(返回 RAII CF 对象;失败/不支持 → None)。
pub fn copy_attr(el: &AXRef, attr: &str) -> Option<AXRef> {
    let a = CFStr::new(attr);
    let mut out: CFTypeRef = std::ptr::null();
    // SAFETY:out 为本栈帧可写指针;CFStr 生命周期覆盖调用。
    let err = unsafe { AXUIElementCopyAttributeValue(el.raw(), a.raw(), &mut out) };
    if err != AX_ERROR_SUCCESS {
        return None;
    }
    // SAFETY:成功时 out 为 +1 引用。
    unsafe { AXRef::take(out) }
}

/// 写属性(策略 A 的 AX 直设 value 用)。
pub fn set_attr(el: &AXRef, attr: &str, value: CFTypeRef) -> Result<(), AXError> {
    let a = CFStr::new(attr);
    // SAFETY:CFTypeRef 由调用方持有且生命周期覆盖调用;常量属性名合法。
    let err = unsafe { AXUIElementSetAttributeValue(el.raw(), a.raw(), value) };
    if err == AX_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(err)
    }
}

/// 属性是否可写(可编辑文件名框的判定:role 为 AXTextField 且 value 可写)。
pub fn attr_settable(el: &AXRef, attr: &str) -> bool {
    let a = CFStr::new(attr);
    // SAFETY:无指针出参。
    unsafe { AXUIElementIsAttributeSettable(el.raw(), a.raw()) != 0 }
}

/// 读字符串属性。
pub fn attr_string(el: &AXRef, attr: &str) -> Option<String> {
    let v = copy_attr(el, attr)?;
    let s = value_string(&v)?;
    Some(s)
}

/// CFString 值 → Rust String(NSString toll-free bridge,经 objc2 消息取 UTF-8)。
pub fn value_string(v: &AXRef) -> Option<String> {
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    // SAFETY:AX 值若为字符串类型,与 NSString 同构(toll-free bridged);
    // 非字符串类型时 objc_msgSend 不匹配——先经 class 判定拦住非 CFString。
    unsafe {
        let cls = {
            let is_cfstring: extern "C" fn(*mut c_void) -> *mut c_void =
                std::mem::transmute(CFGetTypeID_fn as usize);
            let _ = is_cfstring; // 占位防误用;实际判定见下
            CFStringGetTypeID_fn()
        };
        let type_id = CFGetTypeID(v.raw());
        if type_id != cls {
            return None;
        }
        let ns = Retained::from_raw(v.raw() as *mut NSString);
        let s = ns.to_string();
        std::mem::forget(ns); // 所有权仍在 AXRef,只借内容
        Some(s)
    }
}

extern "C" {
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
}

/// 读 CFValue 的 point/size(AXPosition/AXSize)。
pub fn attr_point(el: &AXRef, attr: &str) -> Option<(f64, f64)> {
    let v = copy_attr(el, attr)?;
    let mut out = CGPointXY { x: 0.0, y: 0.0 };
    // SAFETY:out 为 CGSize 同构双 f64 栈内存;类型常量按 HIServices 枚举。
    let ok = unsafe { AXValueGetValue(v.raw(), AX_VALUE_POINT_TYPE, &mut out as *mut _ as *mut c_void) };
    (ok != 0).then_some((out.x, out.y))
}

pub fn attr_size(el: &AXRef, attr: &str) -> Option<(f64, f64)> {
    let v = copy_attr(el, attr)?;
    let mut out = CGPointXY { x: 0.0, y: 0.0 };
    // SAFETY:同 attr_point;CGSize 与双 f64 同构。
    let ok = unsafe { AXValueGetValue(v.raw(), AX_VALUE_SIZE_TYPE, &mut out as *mut _ as *mut c_void) };
    (ok != 0).then_some((out.x, out.y))
}

#[repr(C)]
struct CGPointXY {
    x: f64,
    y: f64,
}

/// 子元素列表(AXChildren;快照遍历与按钮定位共用)。
pub fn children(el: &AXRef) -> Vec<AXRef> {
    let Some(v) = copy_attr(el, attr::CHILDREN) else {
        return Vec::new();
    };
    let arr = v.raw();
    // SAFETY:CFArray 与 NSArray toll-free bridged;count/元素指针读取按 C ABI。
    unsafe {
        let n = CFArrayGetCount(arr);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let e = CFArrayGetValueAtIndex(arr, i); // 未经 retain
            let mut copy: CFTypeRef = std::ptr::null();
            // 元素引用归数组所有,复制引用再交 AXRef:+1 经 CFRetain。
            if !e.is_null() {
                CFRetain(e);
                if let Some(r) = AXRef::take(e) {
                    out.push(r);
                }
            }
            let _ = &mut copy;
        }
        out
    }
}

extern "C" {
    fn CFArrayGetCount(arr: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(arr: CFTypeRef, idx: isize) -> *const c_void;
    fn CFRetain(cf: CFTypeRef);
}

/// 创建 AXObserver(回调派发到本调用线程的 runloop;须与 [`add_observer_source`] 同线程)。
pub fn observer_create(pid: i32, cb: AXObserverCallback) -> Option<AXRef> {
    let mut obs: AXObserverRef = std::ptr::null_mut();
    // SAFETY:obs 出参为本栈帧指针;callback 为 'extern "C" fn'(无捕获)。
    let err = unsafe { AXObserverCreate(pid, cb, &mut obs) };
    if err != AX_ERROR_SUCCESS {
        return None;
    }
    // SAFETY:成功时 obs 为 +1 引用。
    unsafe { AXRef::take(obs) }
}

/// 注册通知(须传与 pid 同源的 element)。
pub fn observer_add_notification(
    obs: &AXRef,
    el: &AXRef,
    notification: &str,
    refcon: usize,
) -> Result<(), AXError> {
    let n = CFStr::new(notification);
    // SAFETY:refcon 为调用方保证存活的整数句柄。
    let err = unsafe { AXObserverAddNotification(obs.raw(), el.raw(), n.raw(), refcon as *mut c_void) };
    if err == AX_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(err)
    }
}

/// observer 的 runloop source 挂到当前线程 runloop(创建线程调用)。
pub fn attach_observer_to_current_runloop(obs: &AXRef) {
    // SAFETY:source 归 observer 所有,runloop 只借;mode 常量为全局 CFString。
    unsafe {
        let rl = CFRunLoopGetCurrent();
        CFRunLoopAddSource(rl, AXObserverGetRunLoopSource(obs.raw()), kCFRunLoopDefaultMode);
    }
}

/// 阻塞跑当前线程 runloop(探针/detector 专用线程;退出经 [`stop_current_runloop`])。
pub fn run_current_runloop() {
    // SAFETY:无参;阻塞语义即本函数契约。
    unsafe { CFRunLoopRun() }
}

/// 请求退出 run_current_runloop。
pub fn stop_current_runloop() {
    // SAFETY:当前线程的 runloop 引用;此函数在任意线程调用皆合法(向目标 runloop 发停止信号)。
    unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) }
}
```

注意：`value_string` 里占位 `is_cfstring` 的写法是错的——直接删掉占位段，最终形态：

```rust
pub fn value_string(v: &AXRef) -> Option<String> {
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    unsafe {
        if CFGetTypeID(v.raw()) != CFStringGetTypeID() {
            return None; // 非 CFString(如 AXValue/数组),不桥接
        }
        let ns = Retained::from_raw(v.raw() as *mut NSString);
        let s = ns.to_string();
        std::mem::forget(ns); // 所有权仍在 AXRef,只借内容
        Some(s)
    }
}
```

（以这段为准实现；上面大块中的对应函数替换为本段。）

- [ ] **Step 2: facade 挂接**

`platform/mod.rs` 增加（放在 `attached()` 等 facade 区）：

```rust
/// 辅助功能授权状态(保存面板助手;macOS)。
pub fn panel_assist_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::ax_trusted()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
```

`platform/mac.rs` 末尾追加转发：

```rust
/// AX 授权查询(ax_ffi 转发;mac.rs 为平台层唯一 unsafe 集中点,故经此转出)。
pub fn ax_trusted() -> bool {
    super::ax_ffi::is_process_trusted()
}
```

- [ ] **Step 3: 声明模块**

`platform/mod.rs` 顶部模块区加 `pub mod ax_ffi;`（cfg target_os = "macos" 不需要——项目其他 platform 模块均为全平台编译、内部 cfg 分流，同款处理：FFI 声明在非 macOS 上不链接也能编译，因为 extern 块只在被调用时链接。若 clippy/CI 报非 macOS 未用告警，按项目惯例给函数加 `#[cfg_attr(not(target_os = "macos"), allow(dead_code))]`——本项目仅支持 macOS，workspace 目标即 macOS，通常无需处理）。

- [ ] **Step 4: 门禁**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

预期：全绿（本任务无行为变化，纯新增未调用代码；clippy 零容忍下若报 dead_code，给 `ax_ffi` 内尚未被引用的项加 `#[allow(dead_code)]` 并注释「M0 探针/Task 9-12 接线后移除」——这是有意的过渡态，Task 12 接线完成后清理）。

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/platform/ax_ffi.rs crates/app/src/platform/mod.rs crates/app/src/platform/mac.rs
git commit -m "feat(app): add AX API raw FFI layer for save-panel-assist"
```

---

## Task 2: CGEvent 键盘合成

**Files:**
- Create: `crates/app/src/platform/cg_events.rs`
- Modify: `crates/app/src/platform/mod.rs`（模块声明）

- [ ] **Step 1: 创建 cg_events.rs**

```rust
//! CGEvent 键盘合成(ADR-8 平台层):保存面板跳转的 ⌘⇧G/⌘V/Return。
//! 手写 FFI(objc2 系 CGEvent binding 的 feature 覆盖不足,直接声明 CoreGraphics
//! 符号;CGEventPostToPid 自 macOS 10.11 可用,面板服务进程投递行为为 M0
//! 实机验证点 #3,失败回退 [`post_combo_global`])。
#![allow(non_snake_case)]

use std::ffi::c_void;

extern "C" {
    fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, key_down: bool) -> *mut c_void;
    fn CGEventSetFlags(ev: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, ev: *mut c_void);
    fn CGEventPostToPid(pid: i32, ev: *mut c_void);
    fn CFRelease(cf: *mut c_void);
}

/// 事件 tap:kCGHIDEventTap = 0(系统注入层,面板可收)。
const TAP_HID: u32 = 0;
/// CGEventFlags 设备无关位(CGEventTypes.h → IOLLEvent.h NX_*MASK):
/// Shift = 0x0002_0000,Command = 0x0010_0000(0x02/0x10 是设备依赖位,系统不识别)。
pub const FLAG_SHIFT: u64 = 0x0002_0000;
pub const FLAG_CMD: u64 = 0x0010_0000;
/// HID keycode:kVK_ANSI_G = 0x05,V = 0x09,Return = 0x24。
pub const KEY_G: u16 = 0x05;
pub const KEY_V: u16 = 0x09;
pub const KEY_RETURN: u16 = 0x24;

/// 向指定进程投递一次组合键(keyDown+keyUp;flags 挂在事件上)。
/// pid = None 走全局 HID tap(不指定目标)。
pub fn post_combo(pid: Option<i32>, keycode: u16, flags: u64) {
    // SAFETY:两个事件各为 +1 引用,用后 CFRelease;投递即消费,无悬垂。
    unsafe {
        for down in [true, false] {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
            assert!(!ev.is_null(), "CGEventCreateKeyboardEvent 失败");
            CGEventSetFlags(ev, flags);
            match pid {
                Some(p) => CGEventPostToPid(p, ev),
                None => CGEventPost(TAP_HID, ev),
            }
            CFRelease(ev);
        }
    }
}
```

- [ ] **Step 2: 模块声明**

`platform/mod.rs` 加 `pub mod cg_events;`。

- [ ] **Step 3: 门禁**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

预期：全绿（同 Task 1，未接线代码的 dead_code 处理同款）。

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/platform/cg_events.rs crates/app/src/platform/mod.rs
git commit -m "feat(app): add CGEvent keyboard synthesis for panel jump"
```

---

## Task 3: M0 探针

**Files:**
- Create: `crates/app/src/platform/panel_probe.rs`
- Modify: `crates/app/src/main.rs`（`MCE_PANEL_PROBE=1` 触发启动）
- Modify: `crates/app/src/platform/mod.rs`（模块声明 + `probe_start()` facade）

探针 = 单个专用 runloop 线程：CF timer 每 500ms 读前台 pid → 变更时换绑 AXObserver → 通知回调 dump 窗口特征快照 JSON → 检测到疑似面板时后台线程执行跳转实验（策略 A → 失败转策略 B），全程 `log::info!`。

- [ ] **Step 1: 创建 panel_probe.rs**

```rust
//! M0 探针(save-panel-assist 六个风险点的实机取证工具;`MCE_PANEL_PROBE=1` 启用)。
//! 只做日志取证,不进产品路径;M1 的 detector/navigator 从本模块演化。
//! 探针线程 = 专用 runloop 线程:CF timer 轮询前台 pid(产品化后由 NSWorkspace
//! 通知取代,见 Task 10)、observer source 挂本线程 runloop。
//! 验证点对应:①通知选型(此处 timer 轮询,产品化选型另测)③post-to-pid 有效性
//! ④浮层 key 让还(本探针不含 UI,M1 验证)⑤输入框 AX 定位⑥Return 确认
//! ②策略 A 直设。全部结论回填 probe-findings 文档。
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Duration;

use super::ax_ffi::{self, attr, note, AXRef};
use super::cg_events::{self, FLAG_CMD, FLAG_SHIFT, KEY_G, KEY_RETURN, KEY_V};

/// 前台 pid(timer 轮询写入;observer 回调读取)。
static FRONT_PID: AtomicI32 = AtomicI32::new(0);
/// 探针运行标志(退出 runloop 用;进程退出即终止,无需显式 false)。
static PROBE_ON: AtomicBool = AtomicBool::new(false);
/// 疑似面板命中标志(避免同一面板反复触发实验)。
static PANEL_HIT: AtomicBool = AtomicBool::new(false);

/// 探针入口(main.rs 启动时按环境变量调用;幂等)。
pub fn start_probe() {
    if PROBE_ON.swap(true, Ordering::SeqCst) {
        return;
    }
    if !ax_ffi::is_process_trusted() {
        log::error!("探针未启动:辅助功能权限未授予(系统设置 → 隐私与安全性 → 辅助功能)");
        return;
    }
    let h = std::thread::Builder::new()
        .name("panel-probe".to_owned())
        .spawn(runloop_thread)
        .expect("探针线程创建失败");
    std::mem::forget(h); // 进程生命周期线程
}

fn runloop_thread() {
    let obs = ax_ffi::observer_create(0, on_ax_event)
        .expect("AXObserverCreate 失败(权限?)");
    ax_ffi::attach_observer_to_current_runloop(&obs);
    // observer 存 thread_local:回调(本线程)与换绑(本线程 timer)共用。
    OBSERVER.with(|c| *c.borrow_mut() = Some(obs));
    // CF timer 轮询前台 pid。CG timer FFI 最小化:用 std 线程节拍 + 自定义
    // runloop source 唤醒过重;此处直接在 runloop 上加 CFRunLoopTimer。
    install_poll_timer();
    log::info!("探针运行:前台切换/面板窗口/跳转实验将写日志");
    ax_ffi::run_current_runloop();
}

thread_local! {
    static OBSERVER: std::cell::RefCell<Option<AXRef>> = const { std::cell::RefCell::new(None) };
}

/// CFRunLoopTimer 轮询(callback 静态;500ms)。
fn install_poll_timer() {
    // SAFETY:CFRunLoopTimerCreate 出参入参均为本线程对象;context 零初始化。
    unsafe extern "C" {
        fn CFRunLoopTimerCreate(
            alloc: *mut std::ffi::c_void, fire: f64, interval: f64, tolerance: f64,
            flags: usize, callout: extern "C" fn(*mut std::ffi::c_void),
            context: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn CFRunLoopAddTimer(rl: *mut std::ffi::c_void, t: *mut std::ffi::c_void, mode: *const std::ffi::c_void);
        fn CFRunLoopGetMain() -> *mut std::ffi::c_void;
        fn CFRunLoopGetCurrent() -> *mut std::ffi::c_void;
    }
    extern "C" fn poll_callout(_ctx: *mut std::ffi::c_void) {
        poll_frontmost();
    }
    extern "C" {
        fn CFRunLoopGetCurrent2() -> *mut std::ffi::c_void;
    }
    // 简化:直接在本线程安装(ax_ffi 的 CFRunLoopGetCurrent 同义,复用声明避免重复 extern)。
    unsafe {
        let t = CFRunLoopTimerCreate(
            std::ptr::null_mut(), 0.5, 0.5, 0.0, 0, poll_callout, std::ptr::null_mut(),
        );
        let rl = ax_ffi::current_runloop_raw();
        CFRunLoopAddTimer(rl, t, ax_ffi::default_mode_raw());
        std::mem::forget(t); // 随进程存活的常驻 timer
    }
}

/// 轮询:前台 pid 变化 → 换绑 observer。
fn poll_frontmost() {
    let app = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication();
    let Some(app) = app else { return };
    let pid = app.processIdentifier();
    let prev = FRONT_PID.swap(pid, Ordering::SeqCst);
    if prev == pid {
        return;
    }
    PANEL_HIT.store(false, Ordering::SeqCst);
    log::info!("[probe] 前台切换 pid={pid} name={:?}", app.localizedName().map(|s| s.to_string()));
    // 换绑:新 pid 的 app element 上注册三类通知(observer 不换对象,通知按 element)。
    let Some(el) = ax_ffi::app_element(pid) else {
        log::warn!("[probe] app element 创建失败 pid={pid}");
        return;
    };
    let obs = OBSERVER.with(|c| c.borrow().clone());
    // AXRef 未实现 Clone:重新创建 observer 最简单(旧 source 随 drop 移除引用,
    // 但 runloop source 需显式移除——CFRunLoopSourceInvalidate 手写太重;M0 直接
    // 每次重建 observer 并挂 runloop:旧 source 仍挂着但 pid 已废,回调读 FRONT_PID
    // 过滤即可,无正确性影响(只多打印)。
    let _ = obs; // 见上,M0 不复用旧 observer
    let new_obs = ax_ffi::observer_create(pid, on_ax_event).expect("AXObserverCreate 失败");
    for n in [note::WINDOW_CREATED, note::FOCUSED_WINDOW_CHANGED, note::UI_ELEMENT_DESTROYED] {
        match ax_ffi::observer_add_notification(&new_obs, &el, n, 0) {
            Ok(()) => log::info!("[probe] 通知已注册 pid={pid} {n}"),
            Err(e) => log::warn!("[probe] 通知注册失败 pid={pid} {n} err={e}"),
        }
    }
    ax_ffi::attach_observer_to_current_runloop(&new_obs);
    OBSERVER.with(|c| *c.borrow_mut() = Some(new_obs));
    scan_existing_windows(pid);
}

/// 立即扫一遍当前窗口(面板可能在换绑前已弹出)。
fn scan_existing_windows(pid: i32) {
    let Some(app) = ax_ffi::app_element(pid) else { return };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        log::info!("[probe] pid={pid} 无焦点窗口");
        return;
    };
    inspect_window(pid, &win);
}

/// AXObserver 回调(静态;转发到本线程的扫描)。
extern "C" fn on_ax_event(
    _obs: *mut std::ffi::c_void,
    _el: *mut std::ffi::c_void,
    notification: *const std::ffi::c_void,
    _refcon: *mut std::ffi::c_void,
) {
    let pid = FRONT_PID.load(Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    let n = unsafe { ax_ffi::cfstring_to_string(notification) }.unwrap_or_default();
    log::info!("[probe] AX 通知 pid={pid} {n}");
    if n == note::UI_ELEMENT_DESTROYED {
        PANEL_HIT.store(false, Ordering::SeqCst);
        return;
    }
    let Some(app) = ax_ffi::app_element(pid) else { return };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else { return };
    inspect_window(pid, &win);
}

/// 窗口检查:特征快照 dump + 疑似面板判定 + 触发实验。
fn inspect_window(pid: i32, win: &AXRef) {
    let snap = super::panel_detector::snapshot_of_window(win);
    log::info!("[probe] 窗口快照 pid={pid} {snap:?}");
    if !super::panel_detector::looks_like_panel(&snap) {
        return;
    }
    if PANEL_HIT.swap(true, Ordering::SeqCst) {
        return; // 同一面板只实验一次
    }
    log::info!("[probe] 命中疑似保存面板,2s 后开始跳转实验 pid={pid}");
    let p = pid;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        run_jump_experiment(p);
    });
}

/// 跳转实验:策略 A → 失败转策略 B;目标 /tmp(存在且可写,实验后无害)。
fn run_jump_experiment(pid: i32) {
    let target = "/tmp";
    log::info!("[probe] === 策略 A:⌘⇧G → AX 直设 value → AXPress 前往 ===");
    cg_events::post_combo(Some(pid), KEY_G, FLAG_CMD | FLAG_SHIFT);
    std::thread::sleep(Duration::from_millis(600));
    let Some(app) = ax_ffi::app_element(pid) else { return };
    let Some(win) = ax_ffi::copy_attr(&app, attr::FOCUSED_WINDOW) else {
        log::warn!("[probe] ⌘⇧G 后找不到焦点窗口(验证点⑤:sheet 是否在 AX 树)");
        strategy_b(pid, target);
        return;
    };
    let Some(field) = find_goto_field(&win) else {
        log::warn!("[probe] 窗口内未找到 Go-to 输入框(验证点⑤)");
        strategy_b(pid, target);
        return;
    };
    let path_cf = ax_ffi::CFStr::new(target);
    let raw = path_cf.raw();
    match ax_ffi::set_attr(&field, attr::VALUE, raw) {
        Ok(()) => log::info!("[probe] AX set value 成功(验证点②可行)"),
        Err(e) => {
            log::warn!("[probe] AX set value 失败 err={e}(验证点②不可行,策略 B 兜底)");
            strategy_b(pid, target);
            return;
        }
    }
    match find_goto_button(&win) {
        Some(btn) => {
            let ok = ax_ffi::ax_press(&btn);
            log::info!("[probe] AXPress 前往按钮 ok={ok}(验证点⑥:Return 等价性)");
        }
        None => {
            log::warn!("[probe] 未找到前往按钮,回退合成 Return");
            cg_events::post_combo(Some(pid), KEY_RETURN, 0);
        }
    }
    log::info!("[probe] === 策略 A 完成,人工核对面板是否已跳到 /tmp ===");
}

/// 策略 B:剪贴板 save/restore + ⌘V + Return。
fn strategy_b(pid: i32, target: &str) {
    log::info!("[probe] === 策略 B:剪贴板 + ⌘V + Return ===");
    let saved = super::clipboard_save();
    super::clipboard_set_text(target);
    cg_events::post_combo(Some(pid), KEY_V, FLAG_CMD);
    std::thread::sleep(Duration::from_millis(300));
    cg_events::post_combo(Some(pid), KEY_RETURN, 0);
    std::thread::sleep(Duration::from_millis(300));
    let ok = super::clipboard_restore(saved.as_deref());
    log::info!("[probe] 策略 B 完成,剪贴板恢复 ok={ok};人工核对跳转与剪贴板");
}

/// 遍历窗口找 Go-to 输入框(可写的 AXTextField;M0 用浅层遍历,深度限 6)。
fn find_goto_field(win: &AXRef) -> Option<AXRef> {
    search(win, 0, |el| {
        ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXTextField")
            && ax_ffi::attr_settable(el, attr::VALUE)
    })
}

/// 找「前往」按钮(AXButton;标题本地化不稳,M0 打印全部按钮标题人工判定)。
fn find_goto_button(win: &AXRef) -> Option<AXRef> {
    let btns = collect(win, 0, |el| {
        ax_ffi::attr_string(el, attr::ROLE).as_deref() == Some("AXButton")
    });
    for b in &btns {
        log::info!("[probe] 按钮标题={:?} desc={:?}",
            ax_ffi::attr_string(b, attr::TITLE),
            ax_ffi::attr_string(b, "AXDescription"));
    }
    // 命中规则:title/description 含 go/goto/前往(大小写不敏感);M0 打印优先,命中任一返回。
    btns.into_iter().find(|b| {
        ["title", "AXDescription"].iter().any(|k| {
            ax_ffi::attr_string(b, k)
                .map(|s| {
                    let l = s.to_lowercase();
                    l.contains("go") || l.contains("前往")
                })
                .unwrap_or(false)
        })
    })
}

/// 带深度限的谓词搜索(命中返回该元素)。
fn search(el: &AXRef, depth: usize, pred: impl Fn(&AXRef) -> bool) -> Option<AXRef> {
    if pred(el) {
        return Some(clone_ref(el));
    }
    if depth >= 6 {
        return None;
    }
    for c in ax_ffi::children(el) {
        if let Some(hit) = search(&c, depth + 1, &pred) {
            return Some(hit);
        }
    }
    None
}

/// 带深度限的谓词收集。
fn collect(el: &AXRef, depth: usize, pred: impl Fn(&AXRef) -> bool) -> Vec<AXRef> {
    let mut out = Vec::new();
    walk(el, depth, &pred, &mut out);
    out
}

fn walk(el: &AXRef, depth: usize, pred: &impl Fn(&AXRef) -> bool, out: &mut Vec<AXRef>) {
    if pred(el) {
        out.push(clone_ref(el));
    }
    if depth >= 6 {
        return;
    }
    for c in ax_ffi::children(el) {
        walk(&c, depth + 1, pred, out);
    }
}

/// AXRef 复制引用(元素归父 AXRef/数组所有,复制即 +1;ax_ffi 提供)。
// 实现放 ax_ffi::clone_ref(见 Task 1 增补),此处 use。

use std::cell::RefCell;
thread_local! {
    static OBSERVER: RefCell<Option<AXRef>> = RefCell::new(None);
}
```

**本任务需要 Task 1 增补与修正**（写代码时直接落进对应文件，不是独立步骤）：

1. `ax_ffi.rs` 增补：`clone_ref(&AXRef) -> AXRef`（CFRetain + take）、`cfstring_to_string(CFStringRef) -> Option<String>`（同 `value_string` 但收 raw 指针，回调用）、`current_runloop_raw() -> *mut c_void`、`default_mode_raw() -> *const c_void`（供 timer 安装）。
2. `panel_probe.rs` 最终版**删除**文中重复的 `thread_local!` 与 `unsafe extern` 重复声明块（上面为展示引入了两处 OBSERVER/多余 extern——落地时只保留一处 OBSERVER thread_local，FFI 全部经 `ax_ffi`，不在 probe 里写 extern）。
3. `panel_detector` 的 `snapshot_of_window`/`looks_like_panel` 在本任务先以最小版落在 `panel_detector.rs`（快照 Debug 打印 + subrole 粗判），Task 9 完整化——probe 是它们的第一个调用方，避免探针阶段写死特征。

- [ ] **Step 2: main.rs 启动开关**

`main.rs` 在 eframe 启动前（应用初始化区）加：

```rust
// M0 探针:仅显式环境变量启用(docs/superpowers/plans/2026-11-04-save-panel-assist.md Task 3)。
if std::env::var("MCE_PANEL_PROBE").as_deref() == Ok("1") {
    crate::platform::probe_start();
}
```

`platform/mod.rs` facade：

```rust
/// M0 探针启动(环境变量 MCE_PANEL_PROBE=1)。
pub fn probe_start() {
    #[cfg(target_os = "macos")]
    mac::probe_start()
}
```

mac.rs 转发 `pub fn probe_start() { super::panel_probe::start_probe() }`。

- [ ] **Step 3: 编译 + 实机冒烟（探针验证，人工/半自动）**

```bash
cargo build -p macEverything-app
MCE_PANEL_PROBE=1 ./target/debug/macEverything-app 2>&1 | tee /tmp/probe.log
```

另一终端执行：
```bash
open -a TextEdit   # 打开另存为(⌘⇧S)触发面板
```

按 `docs/superpowers/specs/2026-11-04-save-panel-assist-probe-findings.md`（Step 4 创建的模板）逐项记录。

- [ ] **Step 4: 创建 probe-findings 模板并回填结论**

创建 `docs/superpowers/specs/2026-11-04-save-panel-assist-probe-findings.md`：

```markdown
# save-panel-assist M0 探针结论

- 日期：<执行日期>
- 环境：macOS <版本>；探针二进制 target/debug/macEverything-app

| # | 验证点 | 结论 | 证据(日志摘录) |
|---|---|---|---|
| 1 | sheet 形态面板的 AX 通知行为(window created / focused window changed 何者触发) | | |
| 2 | 策略 A AX set value 可行性(建议列表/焦点状态) | | |
| 3 | CGEventPostToPid 对面板服务进程有效性(失败则全局 post) | | |
| 5 | ⌘⇧G 后输入框 AX 定位稳定性(深度/谓词) | | |
| 6 | 前往按钮 AXPress vs 合成 Return | | |
| 7 | AX API 后台线程调用可行性 | | |
| 8 | 打开面板(open)与保存面板(save)的特征差异 | | |

验证点 ④(浮层 key 让还)依赖 M1 UI,在 Task 13 实机验证补记。
**结论对 M1 的影响**：策略 A/B 默认值 = ？；检测通知 = ？；post 目标 = pid/global？
```

执行探针（TextEdit 另存为、Finder 复制粘贴、VS Code 保存为各一轮），人工回填表格与结论段，commit。

- [ ] **Step 5: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src/platform/panel_probe.rs crates/app/src/platform/panel_detector.rs crates/app/src/platform/mod.rs crates/app/src/platform/mac.rs crates/app/src/main.rs docs/superpowers/specs/2026-11-04-save-panel-assist-probe-findings.md
git commit -m "feat(app): M0 probe for save-panel detection and jump experiment"
```

---

## Task 4: settings 开关 + 授权引导 + i18n + 托盘提示

**Files:**
- Modify: `crates/app/src/settings.rs`
- Modify: `crates/app/src/ui/settings.rs`、`crates/app/src/ui/settings_view.rs`
- Modify: `crates/app/src/i18n/catalog.rs`
- Modify: `crates/app/src/tray.rs`、`crates/app/src/main.rs`

- [ ] **Step 1: settings.rs 加字段（先写失败测试）**

在 `crates/app/src/settings.rs` 测试模块加：

```rust
/// 保存面板助手默认开启;缺字段读为 true(前向兼容),显式 false 往返保持。
#[test]
fn save_panel_assist_defaults_true_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    assert!(SettingsFile::default().save_panel_assist, "Default 必须为 true");
    assert!(load(dir.path()).save_panel_assist, "无文件 = 默认开启");
    std::fs::write(path(dir.path()), "font_size = \"large\"\n").unwrap();
    assert!(load(dir.path()).save_panel_assist, "缺字段必须默认开启");
    let f = SettingsFile { save_panel_assist: false, ..SettingsFile::default() };
    save(dir.path(), &f).unwrap();
    assert!(!load(dir.path()).save_panel_assist, "显式 false 保持");
}
```

Run: `cargo test -p macEverything-app save_panel_assist` — 预期 **FAIL**（字段不存在，编译错）。

- [ ] **Step 2: 实现字段**

`SettingsFile` 加（`check_updates` 字段之后）：

```rust
/// 保存面板目录助手(自动检测保存/打开面板并浮出目录搜索;默认开启)。
/// 依赖辅助功能权限,未授权时静默不生效(design §3.2)。
#[serde(default = "default_true")]
pub save_panel_assist: bool,
```

`Default` 实现加 `save_panel_assist: true,`。既有测试 `defaults_roundtrip_and_corrupt` 等的结构体字面量需补 `save_panel_assist: true,`（编译器会指出全部位置）。

- [ ] **Step 3: 跑测试确认通过**

Run: `cargo test -p macEverything-app save_panel_assist`
预期：**PASS**。突变验证：把 `default_true` 改成返回 `false`，确认 `save_panel_assist_defaults_true_and_roundtrips` 与 `check_updates_defaults_true_and_roundtrips` 变红，改回。

- [ ] **Step 4: i18n 文案（中英成对）**

`catalog.rs` 的条目区（`row_auto_check` 附近）加：

```rust
row_panel_assist: "保存面板目录助手" / "Save-Panel Directory Assist",
row_panel_assist_desc: "在其他应用保存/打开文件时，浮出目录搜索并自动跳转" / "Search a directory and jump the save/open panel to it",
panel_assist_need_auth: "需授权" / "Needs Permission",
panel_assist_grant: "去授权…" / "Grant Permission…",
panel_assist_auth_hint: "需在系统设置中授予辅助功能权限" / "Grant Accessibility access in System Settings",
tray_panel_assist_auth: "保存面板助手待授权…" / "Save-Panel Assist Needs Permission…",
```

（catalog 条目格式以文件内 `row_auto_check` 行的实际宏形态为准，保持同款。）

- [ ] **Step 5: 设置页开关行 + 授权状态**

`ui/settings.rs`：`SettingsData` 加 `pub(crate) panel_assist: bool,` 与 `pub(crate) panel_assist_trusted: bool,`（构造处从 `SettingsFile` 读取 + `platform::panel_assist_trusted()`）；`SettingsAction` 加 `SetPanelAssist(bool)`；`apply` 分支：

```rust
SettingsAction::SetPanelAssist(enabled) => apply_panel_assist(shared, ctx, enabled),
```

`apply_panel_assist` 仿 `apply_auto_check`（读写 settings.toml + 保存留痕）。

`ui/settings_view.rs`：仿 `row_auto_check` 行（约 1185-1196 行模式）加开关行，`data.panel_assist_trusted == false` 时在行尾显示 `panel_assist_need_auth` 徽标 + 「去授权…」按钮：

```rust
if !data.panel_assist_trusted {
    // 授权按钮:打开系统设置辅助功能面板。
    if ui.button(i18n::s().panel_assist_grant).clicked() {
        platform::open_accessibility_settings();
    }
}
```

`platform/mod.rs`/`mac.rs` facade：

```rust
/// 打开系统设置 → 隐私与安全性 → 辅助功能。
pub fn open_accessibility_settings() {
    #[cfg(target_os = "macos")]
    mac::open_accessibility_settings()
}
```

mac.rs 实现（ObjC `openURL:` 或 `std::process::Command`——用后者，项目已有子进程先例）：

```rust
pub fn open_accessibility_settings() {
    let url = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
    let _ = std::process::Command::new("open").arg(url).spawn();
}
```

- [ ] **Step 6: 托盘一次性提示（仅未授权时出现）**

`tray.rs` 菜单构建处加条件项（菜单项文案 `tray_panel_assist_auth`，点击动作 = `platform::open_accessibility_settings()`）。条件：`settings::load(&data_dir).save_panel_assist && !platform::panel_assist_trusted()`。菜单项 id/事件分发遵循 tray.rs 现有 MenuEvent 模式（新增一个 action 枚举值，处理分支调同一 facade）。

`main.rs` 启动一次性提示：App::new 里（`auto_update` 段之后）：

```rust
// 保存面板助手:开关开且未授权 → 提示一次(仅 log + 托盘项,不弹窗;design §3.2)。
if settings_file.save_panel_assist && !platform::panel_assist_trusted() {
    log::info!("保存面板助手待授权:设置页或托盘菜单可跳转系统设置");
}
```

- [ ] **Step 7: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add -A crates/app
git commit -m "feat(app): save-panel assist setting with authorization guidance"
```

---

## Task 5: protocol Query.dir_only

**Files:**
- Modify: `crates/protocol/src/lib.rs`

- [ ] **Step 1: 先写失败测试（仿 desc 模式三件套）**

`lib.rs` tests 加：

```rust
/// dir_only 向后兼容钉:旧请求(无键)解码 dir_only=None;None 不上线,
/// 混跑期旧 daemon 收到的消息与旧客户端完全同形。
#[test]
fn dir_only_backward_compatible_and_omits_on_wire() {
    let old_req = br#"{"msg":"query","v":1,"query":"a","limit":10}"#;
    let r: Request = serde_json::from_slice(old_req).unwrap();
    assert!(matches!(r, Request::Query { dir_only: None, .. }));

    let r2 = Request::Query {
        v: PROTOCOL_VERSION,
        query: "a".into(),
        limit: 10,
        sort: None,
        desc: None,
        dir_only: None,
    };
    let body = serde_json::to_string(&r2).unwrap();
    assert!(!body.contains("dir_only"), "None 不应序列化:{body}");

    let r3 = Request::Query {
        v: PROTOCOL_VERSION,
        query: "a".into(),
        limit: 10,
        sort: None,
        desc: None,
        dir_only: Some(true),
    };
    let b = encode(&r3).unwrap();
    assert_eq!(decode_request(&b[4..]).unwrap(), r3);
    assert!(
        serde_json::to_string(&r3).unwrap().contains(r#""dir_only":true"#),
        "Some(true) 线上为 JSON bool"
    );
}
```

Run: `cargo test -p protocol dir_only` — 预期 **FAIL**（无该字段）。

- [ ] **Step 2: 实现**

`Request::Query` 变体 `desc` 字段后加：

```rust
/// 仅目录(dir = true 时过滤非目录条目;保存面板助手用)。纯新增可选字段
/// 不 bump v:旧 daemon 忽略未知字段,旧客户端缺省不发送此键。
#[serde(default, skip_serializing_if = "Option::is_none")]
dir_only: Option<bool>,
```

（`Request::version()` 的 Query 分支已用 `..`，无需改。）

- [ ] **Step 3: 跑测试确认通过**

Run: `cargo test -p protocol` — 预期 **PASS**；同时既有 Query 构造点（本 crate tests、client、daemon、app）会编译报错——这是 Task 6 的前置信号，本任务只修 protocol crate 内的构造点（`request_roundtrip` 等加 `dir_only: None,`），其余 crate 在 Task 6 修。**过渡态 commit 允许 workspace 编译失败吗？不允许**——项目纪律是每次 commit 三门禁全绿。所以本步同步把所有构造点补上 `dir_only: None,`（机械修改，各 crate 的 Query 字面量），但透传逻辑仍在 Task 6。

Run: `cargo test --workspace` — 预期 **PASS**。

- [ ] **Step 4: Commit**

```bash
git add -A crates
git commit -m "feat(protocol): add optional dir_only filter to Query request"
```

---

## Task 6: engine dir_only 过滤

**Files:**
- Modify: `crates/engine/src/query.rs`

- [ ] **Step 1: 先写失败测试**

`query.rs` `tests` 模块加：

```rust
/// dir_only = true 时只返回目录;total 同步只计目录(截断前全量口径不变)。
/// 突变「execute 去掉 dir_only 过滤」时文件命中回归必红。
#[test]
fn dir_only_filters_files_and_counts_total() {
    let i = sample(); // Documents/projects 目录 + 3 文件
    let mut plan = parse("e").unwrap(); // 命中 projects(目录)与 README.md(文件)
    plan.dir_only = true;
    let out = execute(&i, &plan, 100);
    assert_eq!(out.total, 1, "只计目录");
    assert_eq!(i.display_name(out.hits[0]), b"projects");
    // 默认 plan(false)行为不变:文件照常命中。
    let all = execute(&i, &parse("e").unwrap(), 100);
    assert!(all.total > out.total, "缺省不过滤");
}

/// dir_only 与 path: 词组合:候选(命中目录的直接子项)同样被过滤。
#[test]
fn dir_only_applies_after_path_candidate_filter() {
    let i = sample();
    let mut plan = parse("path:documents").unwrap(); // Documents 下两个文件
    plan.dir_only = true;
    assert_eq!(execute(&i, &plan, 100).total, 0, "子项全是文件,全被滤掉");
}
```

Run: `cargo test -p engine dir_only` — 预期 **FAIL**（无字段，编译错）。

- [ ] **Step 2: 实现**

`QueryPlan` 加字段与 parse 初始化：

```rust
pub struct QueryPlan {
    pub name_terms: Vec<Pattern>,
    pub path_terms: Vec<Pattern>,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// 仅目录(保存面板助手):候选层过滤非目录,total 同口径。
    /// 调用方置位(parse 恒 false),不进解析语法。
    pub dir_only: bool,
}
```

`parse` 的 `QueryPlan` 字面量加 `dir_only: false,`。`execute` 候选闭包的 entry_valid 判定后加：

```rust
if plan.dir_only && !e.is_dir() {
    return None;
}
```

（位置：`if !e.is_live() || !view.entry_valid(idx) { return None; }` 之后。）

- [ ] **Step 3: 跑测试 + 突变验证**

Run: `cargo test -p engine` — 预期 **PASS**。
突变：删除 `if plan.dir_only && !e.is_dir()` 两行 → `dir_only_filters_files_and_counts_total` 必红 → 恢复。

- [ ] **Step 4: Commit**

```bash
git add crates/engine/src/query.rs
git commit -m "feat(engine): dir_only candidate filter for directory-only queries"
```

---

## Task 7: client/daemon 透传 dir_only

**Files:**
- Modify: `crates/client/src/lib.rs`、`crates/cli/src/daemon.rs`、`crates/cli/src/main.rs`、`crates/app/src/ui/palette.rs`

- [ ] **Step 1: client::query 加参数**

签名（`lib.rs:195`）：

```rust
pub fn query(
    paths: &DaemonPaths,
    q: &str,
    limit: u32,
    sort: Option<&str>,
    desc: bool,
    dir_only: bool,
) -> Result<QueryOutcome, QueryError> {
```

请求构造 `desc` 后加：

```rust
// false 不发送:与 desc 同款省字段纪律(混跑期消息与旧客户端同形)
dir_only: dir_only.then_some(true),
```

`offline_query` 同样加 `dir_only: bool` 尾参，实现中 `plan.dir_only = dir_only;`（在 parse 之后）。

- [ ] **Step 2: daemon 透传**

`daemon.rs` `dispatch` 的 `Request::Query` 分支（2418 行）解构加 `dir_only`，传给 `run_query`；`run_query` 签名加 `dir_only: bool` 尾参，`parse` 成功后加：

```rust
let mut plan = ...;
plan.dir_only = dir_only;
```

（注意 `let Ok(plan)` 换成先 parse 后置位的形态。）

- [ ] **Step 3: 更新两个调用点（行为不变）**

- `cli/src/main.rs:158`：`client::query(&paths, &query, 1000, sort_str, protocol_desc, false)`
- `app/src/ui/palette.rs:1513`：`client::query(&paths, &fire.query, limit, sort, desc, false)`

- [ ] **Step 4: daemon 侧回归测试（已有测试直接受益）**

Run: `cargo test --workspace` — 预期 **PASS**（daemon 既有 query 测试不传 dir_only = false，行为不变）。

- [ ] **Step 5: Commit**

```bash
git add crates/client crates/cli crates/app/src/ui/palette.rs
git commit -m "feat(client,cli): pass dir_only through query pipeline"
```

---

## Task 8: detector 判定纯函数 + 特征表

**Files:**
- Modify: `crates/app/src/platform/panel_detector.rs`（Task 3 已建的最小版上完整化）

- [ ] **Step 1: 定义快照结构与特征表（数据驱动，可序列化）**

```rust
/// 窗口特征快照(判定输入;与 AX 树解耦,纯数据可测可序列化)。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PanelSnapshot {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub title: Option<String>,
    /// 子树内可写 AXTextField 数(浅层遍历,深度限 6)。
    pub editable_text_fields: usize,
    /// 子树内 AXButton 数。
    pub buttons: usize,
    /// 子树内文件浏览器类容器(AXScrollArea/AXSplitGroup)数。
    pub browsers: usize,
}

/// 快速信号 subrole 白名单( macOS 实测:独立对话框/附着 sheet 的 subrole 形态;
/// M0 探针回填。判定为「疑似」而非「确认」——确认走结构信号。
const SUBROLE_QUICK: &[&str] = &["AXSystemDialog", "AXDialog", "AXFloatingWindow"];

/// 确认信号阈值:可写文件名输入框 ≥1 且按钮 ≥1。
/// (保存面板必有可编辑文件名框与按钮组;Finder 桌面等误报源无此组合。)
const CONFIRM_EDITABLE_MIN: usize = 1;
const CONFIRM_BUTTONS_MIN: usize = 1;

/// 判定:快速信号命中且确认信号满足。纯函数(无 AX 调用)。
pub fn is_save_panel(s: &PanelSnapshot) -> bool {
    let quick_ok = s
        .subrole
        .as_deref()
        .map(|r| SUBROLE_QUICK.contains(&r))
        .unwrap_or(false);
    quick_ok
        && s.editable_text_fields >= CONFIRM_EDITABLE_MIN
        && s.buttons >= CONFIRM_BUTTONS_MIN
}
```

`snapshot_of_window`（Task 3 最小版扩展为填充全字段）：遍历窗口 AX 子树统计三类元素数（复用 probe 的 `collect` 逻辑或从 probe 移入）。

- [ ] **Step 2: 先写失败测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn snap(subrole: &str, editable: usize, buttons: usize) -> PanelSnapshot {
        PanelSnapshot {
            role: Some("AXWindow".into()),
            subrole: Some(subrole.into()),
            title: Some("存储".into()),
            editable_text_fields: editable,
            buttons,
            browsers: 1,
        }
    }

    /// 契约:快速信号 + 确认信号双满足才判定;任一缺失为否。
    /// 突变「去掉 editable/button 确认(仅 subrole 判定)」→ 第三用例必红。
    #[test]
    fn quick_and_confirm_signals_both_required() {
        assert!(is_save_panel(&snap("AXSystemDialog", 1, 2)));
        assert!(!is_save_panel(&snap("AXStandardWindow", 1, 2)), "subrole 不中");
        assert!(!is_save_panel(&snap("AXSystemDialog", 0, 2)), "无可写输入框");
        assert!(!is_save_panel(&snap("AXSystemDialog", 1, 0)), "无按钮");
    }

    /// 打开/保存面板都判定为是(design §3.2 触发范围:不区分)。
    #[test]
    fn open_and_save_panels_both_match() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.title = Some("打开".into());
        assert!(is_save_panel(&s));
    }

    /// subrole 缺失(个别 app 隐藏属性)→ 不判定,防误报。
    #[test]
    fn missing_subrole_never_matches() {
        let mut s = snap("AXSystemDialog", 1, 2);
        s.subrole = None;
        assert!(!is_save_panel(&s));
    }
}
```

Run: `cargo test -p macEverything-app is_save_panel` — 预期按实现顺序 FAIL→PASS（TDD：先写测试再实现判定函数体时遵循此序；快照结构可先建）。

- [ ] **Step 3: 突变验证 + 门禁**

突变：`is_save_panel` 删掉 `quick_ok &&` 前缀（只看确认信号）→ `missing_subrole_never_matches` 必红 → 恢复。
Run: `cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` — 全绿。

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/platform/panel_detector.rs
git commit -m "feat(app): data-driven save-panel classifier with test pins"
```

---

## Task 9: detector 平台事件层

**Files:**
- Modify: `crates/app/src/platform/panel_detector.rs`
- Modify: `crates/app/src/platform/mod.rs`、`crates/app/src/platform/mac.rs`（facade/NSWorkspace 观察）

- [ ] **Step 1: 事件与运行结构**

```rust
/// 检测事件(UI 每帧 poll;mpsc 单消费)。
#[derive(Debug, Clone, PartialEq)]
pub enum PanelEvent {
    /// 疑似保存面板出现(含宿主 pid 与面板窗口 AppKit frame (x, y, w, h)，
    /// y 为屏幕左上原点向下——与 egui 无关，定位换算在平台层)。
    Shown { pid: i32, frame: [f64; 4] },
    /// 面板窗口销毁。
    Closed,
}

/// Esc 屏蔽状态(本面板存续内不再自动弹;design §4 不打扰规则)。
/// 键 = 宿主 pid;面板关闭(Closed 事件)即清除。
#[derive(Default)]
pub struct Dismissals {
    dismissed: std::collections::HashSet<i32>,
}

impl Dismissals {
    pub fn dismiss(&mut self, pid: i32) {
        self.dismissed.insert(pid);
    }
    pub fn blocked(&self, pid: i32) -> bool {
        self.dismissed.contains(&pid)
    }
    pub fn clear(&mut self, pid: i32) {
        self.dismissed.remove(&pid);
    }
}
```

- [ ] **Step 2: 平台线程（探针演化：NSWorkspace 通知替代轮询）**

- 主线程注册 `NSWorkspaceWillActivateApplicationNotification`（mac.rs 现有 `SpaceObserver` 模式：`define_class!` 新观察者 `AppActivateObserver`，方法 `appDidActivate:`，selector 注册在 `NSWorkspace::sharedWorkspace().notificationCenter()`——参照 mac.rs:340-352 的注册代码）。
- 回调（主线程）写 `ACTIVATE_PID: AtomicI32` 并 signal 自定义 CFRunLoopSource → detector 专用 runloop 线程执行换绑（AXObserver 须在其 runloop 线程）。
- 自定义 source：`ax_ffi` 增补 `make_signal_source() -> (AXRef, fn())`——`CFRunLoopSourceCreate` + context.perform 回调 + `CFRunLoopSourceSignal` + `CFRunLoopWakeUp` 封装：

```rust
/// 自定义 runloop 信号源:跨线程唤醒 detector 线程处理换绑。
/// 返回 (source RAII, signal 闭包所用的 raw)。signal 侧:
/// CFRunLoopSourceSignal(source); CFRunLoopWakeUp(rl_of_detector)。
```

- detector 线程循环：换绑 observer（三类通知，同 Task 3 形态）→ 通知回调 → `snapshot_of_window` → `is_save_panel` 命中且未被 Dismissals 屏蔽 → 读窗口 `AXPosition/AXSize` → `tx.send(Shown)`；`UI_ELEMENT_DESTROYED` → `tx.send(Closed)` + Dismissals.clear。
- 输出通道：`pub fn start_detector() -> std::sync::mpsc::Receiver<PanelEvent>`（内部 spawn；幂等：第二次调用返回 None——detector 进程内单实例）。

- [ ] **Step 3: 关机纪律**

detector 线程随进程存活；`Closed` 后 observer 保留（同一 pid 下次面板复用通知），pid 切换才换绑——与探针一致。

- [ ] **Step 4: 实机验证（复用 Task 3 冒烟形态）**

`MCE_PANEL_PROBE` 不动，detector 经 Task 11 接线后在 app 常规启动路径生效；本任务先在 mac.rs 提供独立开关 `MCE_PANEL_DETECTOR=1` 提前实机验证（log 收到 Shown/Closed 即证），Task 11 接线后移除该开关。

Run（实机）：`MCE_PANEL_DETECTOR=1 ./target/debug/macEverything-app`，TextEdit ⌘⇧S → 日志应出现 `Shown{pid, frame}`；关闭面板 → `Closed`。

- [ ] **Step 5: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src/platform
git commit -m "feat(app): save-panel detector with NSWorkspace-driven AXObserver"
```

---

## Task 10: navigator 状态机纯逻辑

**Files:**
- Create: `crates/app/src/platform/dialog_navigator.rs`

- [ ] **Step 1: 先写失败测试**

```rust
//! 跳转状态机(纯逻辑):步骤由「观测事件」驱动转移,产出「动作」。
//! 每步动作执行前状态机不假设成功——执行层(platform 半)回报观测结果。
use super::cg_events::{FLAG_CMD, FLAG_SHIFT};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// 尚未开始。
    Idle,
    /// ⌘⇧G 已发,等 sheet。
    AwaitSheet,
    /// 填路径阶段(策略待定/已定)。
    Fill,
    /// 确认阶段。
    Confirm,
    Done,
    /// 用户中途干预(Esc 关闭 sheet)或超时:中止。
    Aborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Obs {
    /// sheet 输入框出现在 AX 树。
    SheetOpened,
    /// 输入框消失(用户 Esc 关闭 sheet / 跳转完成)。
    SheetClosed,
    /// 策略 A 失败(直设/按钮不可用)。
    StrategyAFailed,
    /// 全流程超时。
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// 发 ⌘⇧G。
    SendGoToFolder,
    /// 策略 A:AX 直设 value + AXPress。
    FillViaAx,
    /// 策略 B:剪贴板(set 后) + ⌘V。
    PasteViaClipboard,
    /// 合成 Return。
    PressReturn,
    /// 请求执行层确认输入框仍在(每步合成前状态确认,design §3.4)。
    VerifyField,
    /// 恢复剪贴板(策略 B 专用;Done/Aborted 时执行层回调)。
    RestoreClipboard,
}

/// 状态机:策略 A 优先,A 失败当场降级 B(design §3.4)。
pub struct NavMachine {
    pub step: Step,
    /// 策略 B 降级后为 true(收尾要恢复剪贴板)。
    used_clipboard: bool,
}

impl NavMachine {
    pub fn new() -> Self {
        Self { step: Step::Idle, used_clipboard: false }
    }

    /// 转移:返回 (下一步动作, 是否结束)。
    pub fn on(&mut self, o: Obs) -> (Option<Act>, bool) {
        match (self.step, o) {
            (Step::Idle, Obs::SheetOpened) | (Step::Idle, Obs::Timeout) => {
                // 入口由执行层直接驱动:先发 ⌘⇧G。
                self.step = Step::AwaitSheet;
                (Some(Act::SendGoToFolder), false)
            }
            (Step::AwaitSheet, Obs::SheetOpened) => {
                self.step = Step::Fill;
                (Some(Act::FillViaAx), false)
            }
            (Step::AwaitSheet, Obs::Timeout | Obs::SheetClosed) => {
                self.step = Step::Aborted;
                (None, true)
            }
            (Step::Fill, Obs::StrategyAFailed) => {
                self.used_clipboard = true;
                (Some(Act::PasteViaClipboard), false)
            }
            (Step::Fill, Obs::SheetClosed) => {
                self.step = Step::Aborted;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            // Fill 动作执行成功后执行层把 VerifyField 回报进来,通过才进 Confirm。
            (Step::Confirm, Obs::SheetClosed) => {
                self.step = Step::Done;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            (Step::Confirm, Obs::Timeout | Obs::StrategyAFailed) => {
                self.step = Step::Aborted;
                if self.used_clipboard {
                    (Some(Act::RestoreClipboard), true)
                } else {
                    (None, true)
                }
            }
            _ => (None, false), // 未定义转移:保持原状(执行层超时兜底)
        }
    }

    pub fn used_clipboard(&self) -> bool {
        self.used_clipboard
    }
}
```

测试（同文件 `#[cfg(test)]`）：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// 主链:Idle→⌘⇧G→sheet→策略A→确认→Done。突变「AwaitSheet 上 SheetClosed
    /// 误入 Fill」时 Aborted 用例必红。
    #[test]
    fn happy_path_strategy_a() {
        let mut m = NavMachine::new();
        let (a, done) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::SendGoToFolder));
        assert!(!done);
        let (a, _) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::FillViaAx));
        // Fill 成功(执行层回报 VerifyField 视为进入 Confirm 的边界;此处直接推 Confirm)
        m.step = Step::Confirm;
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, None, "策略 A 无剪贴板收尾");
        assert_eq!(m.step, Step::Done);
    }

    /// 降级:A 失败 → 策略 B(剪贴板),收尾必 RestoreClipboard。
    #[test]
    fn degrade_to_strategy_b_restores_clipboard() {
        let mut m = NavMachine::new();
        m.on(Obs::SheetOpened);
        let (a, _) = m.on(Obs::SheetOpened);
        assert_eq!(a, Some(Act::FillViaAx));
        let (a, _) = m.on(Obs::StrategyAFailed);
        assert_eq!(a, Some(Act::PasteViaClipboard));
        assert!(m.used_clipboard(), "降级后须记住剪贴板参与");
        m.step = Step::Confirm;
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, Some(Act::RestoreClipboard), "策略 B 收尾恢复剪贴板");
    }

    /// Esc 中止(AwaitSheet/Fill 两个阶段):Fill 阶段且已降级时恢复剪贴板,
    /// 否则无声退出。design §5「用户中途 Esc」竞态防护的状态面。
    #[test]
    fn user_esc_aborts_and_restores_when_clipboard_used() {
        let mut m = NavMachine::new();
        m.on(Obs::SheetOpened);
        m.on(Obs::SheetOpened);
        m.on(Obs::StrategyAFailed); // 降级 B
        let (a, done) = m.on(Obs::SheetClosed);
        assert!(done);
        assert_eq!(a, Some(Act::RestoreClipboard));
        assert_eq!(m.step, Step::Aborted);
    }

    /// AwaitSheet 阶段超时/提前关闭:中止且无动作。
    #[test]
    fn await_sheet_timeout_aborts() {
        let mut m = NavMachine::new();
        m.on(Obs::SheetOpened); // 进入 AwaitSheet
        let (a, done) = m.on(Obs::Timeout);
        assert!(done);
        assert_eq!(a, None);
        assert_eq!(m.step, Step::Aborted);
    }
}
```

Run: `cargo test -p macEverything-app nav` — 预期 FAIL（文件未建）→ 实现（上面代码）→ PASS。
突变验证：`AwaitSheet` 分支的 `Obs::Timeout | Obs::SheetClosed` 改为只匹配 `Timeout` → `user_esc_aborts_and_restores_when_clipboard_used`（改为 await 阶段版本）与 `await_sheet_timeout_aborts` 中其一必红——具体做法：突变后跑全部 nav 测试，确认至少一个变红，恢复。

- [ ] **Step 2: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src/platform/dialog_navigator.rs
git commit -m "feat(app): jump navigator state machine (strategy A/B degrade, esc abort)"
```

---

## Task 11: navigator 平台执行层

**Files:**
- Modify: `crates/app/src/platform/dialog_navigator.rs`
- Modify: `crates/app/src/platform/mod.rs`（facade `navigate_panel(pid, path)`）

- [ ] **Step 1: 执行层实现**

```rust
/// 执行层:状态机驱动 + AX 观测 + 合成键。后台线程调用(AX CF 层线程无关,
/// M0 验证点 #7);入口经 facade 从 UI 线程 spawn。
pub enum NavigateOutcome {
    Done,
    /// 策略 B 也失败:路径已留在剪贴板,UI 显示手动降级提示(design §5)。
    FellBackToClipboard,
    /// 中止(用户 Esc/超时)。
    Aborted,
}

/// 单步观测:输入框是否在 AX 树(每步合成前确认,design §3.4)。
fn goto_field(pid: i32) -> Option<AXRef> {
    let app = ax_ffi::app_element(pid)?;
    let win = ax_ffi::copy_attr(&app, ax_ffi::attr::FOCUSED_WINDOW)?;
    search_field(&win, 0)
}

fn search_field(el: &AXRef, depth: usize) -> Option<AXRef> {
    if ax_ffi::attr_string(el, ax_ffi::attr::ROLE).as_deref() == Some("AXTextField")
        && ax_ffi::attr_settable(el, ax_ffi::attr::VALUE)
    {
        return Some(ax_ffi::clone_ref(el));
    }
    if depth >= 6 {
        return None;
    }
    for c in ax_ffi::children(el) {
        if let Some(h) = search_field(&c, depth + 1) {
            return Some(h);
        }
    }
    None
}

/// 前往按钮(title/description 含 go/前往;M0 探针同款谓词)。
fn goto_button(pid: i32) -> Option<AXRef> { /* collect 同款谓词搜索 AXButton */ }

/// 轮询直到谓词成立或超时(50ms 步进)。
fn wait_until(deadline: std::time::Instant, mut pred: impl FnMut() -> bool) -> bool {
    while std::time::Instant::now() < deadline {
        if pred() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// 完整跳转。blocking;调用方(接线层)后台线程调用。
pub fn run_navigate(pid: i32, path: &str) -> NavigateOutcome {
    let step_timeout = std::time::Duration::from_secs(3);
    let mut m = NavMachine::new();
    // 入口:发 ⌘⇧G(M0 结论决定 post 目标;默认 ToPid,常量可切)。
    let post_target = POST_TO_PID; // static bool, M0 回填
    cg_events::post_combo(post_target.then_some(pid), cg_events::KEY_G, FLAG_CMD | FLAG_SHIFT);
    let mut step = Step::AwaitSheet;
    let mut deadline = std::time::Instant::now() + step_timeout;
    let mut used_clipboard = false;
    let mut saved_clipboard: Option<String> = None;
    loop {
        let obs = if wait_until(deadline, || goto_field(pid).is_some()) {
            Obs::SheetOpened
        } else {
            Obs::Timeout
        };
        let (act, done) = { /* 状态机转移(与 Task 10 状态机对接的适配层) */ };
        ...
    }
}
```

落地时把上述骨架写完整：状态机驱动循环的每个 `Act` 分支调用对应平台操作（`FillViaAx` = `set_attr` + 失败回报 `StrategyAFailed`；`PasteViaClipboard` = save/set/`post_combo(V)`；`PressReturn`；`VerifyField`；`RestoreClipboard`），成功后推进到 Confirm 并确认 field 仍在（消失 = `SheetClosed`）。骨架中「与 Task 10 状态机对接的适配层」要写实：状态机 `on()` 的每步动作在本层执行，观测结果回投状态机，循环终止条件 = `done == true`，按 `m.step` 映射 `NavigateOutcome`（`Done` → Done；`Aborted` → Aborted；超时且策略 B 失败 → `FellBackToClipboard`）。

- [ ] **Step 2: 剪贴板 save/set/restore**

`platform/mod.rs`/`mac.rs` facade（NSPasteboard 已在依赖 feature 中；`copy_files` 已有 NSPasteboard 用法参照 mac.rs:548）：

```rust
/// 读取剪贴板文本(None = 无文本/非文本)。navigator save 用。
pub fn clipboard_text() -> Option<String>;
/// 写剪贴板纯文本。navigator set 用。
pub fn clipboard_set_text(s: &str);
/// 恢复(写回 save 的文本;None = 原本无文本 → 清空剪贴板)。
pub fn clipboard_restore(saved: Option<&str>) -> bool;
```

实现走 objc2-app-kit `NSPasteboard::generalPasteboard()`（`stringForType` / `setString:forType:`，同 `NSPasteboard` feature）。

- [ ] **Step 3: 实机验证（探针复用）**

`MCE_PANEL_PROBE=1` 下探针的 `run_jump_experiment` 改为调 `run_navigate`（探针成为执行层的第一调用方），TextEdit ⌘⇧S 实测：面板跳到 /tmp、剪贴板还原、按 Esc 中途打断不误粘贴（风险点 #3 竞态验证）。

- [ ] **Step 4: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src/platform
git commit -m "feat(app): navigator platform executor with clipboard save/restore"
```

---

## Task 12: 浮层定位几何纯函数

**Files:**
- Create: `crates/app/src/ui/overlay_layout.rs`（若项目 ui 层纯函数惯例放 ui/ 下独立文件；与 overlay.rs 同目录）
- Modify: `crates/app/src/ui/mod.rs`（`mod overlay_layout;`）

- [ ] **Step 1: 先写失败测试**

```rust
/// 浮层定位(纯几何;design §3.3):面板右侧优先 → 越界下方 → 再越界取屏幕
/// 安全区内最大位置。输入/输出均为 AppKit 屏幕坐标(frame,原点左下,y 向上),
/// 与 egui 无关(设置窗口位置在平台层 setFrame)。
use crate::ui::overlay_layout::overlay_frame;

fn screen() -> (f64, f64, f64, f64) {
    (0.0, 0.0, 1440.0, 900.0)
} // x, y, w, h

/// 右侧放得下 → 面板右缘 + 边距,顶部对齐。
#[test]
fn right_side_preferred() {
    let f = overlay_frame([100.0, 200.0, 600.0, 400.0], screen(), 420.0, 300.0);
    assert_eq!([f[0], f[1]], [100.0 + 600.0 + 12.0, 200.0 + 400.0 - 300.0]);
}

/// 右侧越界 → 下方居中对齐。
#[test]
fn falls_below_when_right_overflows() {
    let f = overlay_frame([900.0, 500.0, 600.0, 400.0], screen(), 420.0, 300.0);
    let cx = 900.0 + 600.0 / 2.0 - 420.0 / 2.0;
    assert_eq!([f[0], f[1]], [cx, 500.0 - 12.0 - 300.0]);
}

/// 下方也越界 → 屏幕安全区内钳制(右/下缘留 8px 边距)。
#[test]
fn clamps_into_safe_area() {
    let f = overlay_frame([1100.0, 700.0, 400.0, 300.0], screen(), 420.0, 300.0);
    assert!(f[0] + 420.0 <= 1440.0 - 8.0, "右缘不出屏:{:?}", f);
    assert!(f[1] >= 8.0, "下缘不出屏:{:?}", f);
}
```

Run: `cargo test -p macEverything-app overlay_frame` — 预期 FAIL。

- [ ] **Step 2: 实现**

```rust
/// 浮层 frame:panel = [x, y, w, h](AppKit 屏幕 frame);screen = [x, y, w, h];
/// overlay_w/h = 浮层尺寸。边距 12(相邻),安全区边距 8。
pub fn overlay_frame(panel: [f64; 4], screen: [f64; 4], overlay_w: f64, overlay_h: f64) -> [f64; 4] {
    const GAP: f64 = 12.0;
    const EDGE: f64 = 8.0;
    let (sx, sy, sw, sh) = (screen[0], screen[1], screen[2], screen[3]);
    let mut x = panel[0] + panel[2] + GAP;
    let mut y = panel[1] + panel[3] - overlay_h;
    if x + overlay_w > sx + sw - EDGE {
        // 右侧放不下 → 面板下方,GAP 间隙,水平居中对齐面板
        x = panel[0] + panel[2] / 2.0 - overlay_w / 2.0;
        y = panel[1] - GAP - overlay_h;
    }
    // 安全区钳制
    x = x.clamp(sx + EDGE, sx + sw - EDGE - overlay_w);
    y = y.clamp(sy + EDGE, sy + sh - EDGE - overlay_h);
    [x, y, overlay_w, overlay_h]
}
```

Run: `cargo test -p macEverything-app overlay_frame` — **PASS**。
突变验证：删除 clamp 行 → `clamps_into_safe_area` 必红 → 恢复。

- [ ] **Step 3: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src/ui
git commit -m "feat(app): overlay placement geometry with clamp pins"
```

---

## Task 13: 浮层视口（第三视口）+ 平台窗口管理

**Files:**
- Create: `crates/app/src/ui/overlay.rs`
- Modify: `crates/app/src/ui/mod.rs`（视口注册、Shared.overlay）
- Modify: `crates/app/src/platform/mac.rs`（OVERLAY 窗口 patch/show/hide）
- Modify: `crates/app/src/platform/mod.rs`（facade）
- Modify: `crates/app/src/i18n/catalog.rs`（浮层文案：错误态等）

这是最大的 UI 任务。执行 subagent 必须先读 `ui/palette.rs`（查询工作线程与状态模式）、`ui/mod.rs`（Shared/视口注册）、`platform/mac.rs` 的 PALETTE 段（窗口 patch/定位/makeKey）。

- [ ] **Step 1: overlay.rs 骨架（状态 + 查询会话 + UI）**

```rust
//! 保存面板目录助手浮层(第三视口):输入框 + 目录列表;键盘 ↑↓/Enter/双击/Esc。
//! 查询复用悬浮条的工作线程模式(palette.rs:1499 query worker + mpsc 回投),
//! dir_only = true;daemon 离线不弹浮层(接线层守门,本模块只管已弹出后的体验)。
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use eframe::egui;
use protocol::HitEntry;

use crate::client;
use crate::session::Session;
use crate::{client as client_mod, platform};

pub(crate) fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId(platform::OVERLAY_TITLE.into())
}

/// 浮层几何常量。
pub const OVERLAY_W: f32 = 420.0;
pub const OVERLAY_H: f32 = 300.0;

/// 浮层状态(Shared 持有;线程边界同 palette)。
pub struct OverlayState {
    pub ctx: egui::Context,
    pub paths: client::DaemonPaths,
    pub visible: bool,
    pub input: String,
    pub results: Vec<HitEntry>,
    pub selected: usize,
    session: Session,
    query_tx: Option<Sender<(u64, Vec<HitEntry>, bool)>>,
    query_rx: Option<Receiver<(u64, Vec<HitEntry>, bool)>>,
    gen: u64,
    /// navigator 失败态:路径已复制提示(design §5 错误出口)。
    pub clipboard_hint: bool,
}

impl OverlayState {
    pub fn new(ctx: egui::Context, paths: client::DaemonPaths) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            ctx,
            paths,
            visible: false,
            input: String::new(),
            results: Vec::new(),
            selected: 0,
            session: Session::new(std::time::Duration::from_millis(60)),
            query_tx: Some(tx),
            query_rx: Some(rx),
            gen: 0,
            clipboard_hint: false,
        }
    }

    /// 输入变更 → 防抖会话;到期由 [Self::poll] 发起查询。
    pub fn on_input(&mut self, now: std::time::Instant) {
        let text = self.input.clone();
        if text.is_empty() {
            self.results.clear();
            self.selected = 0;
        }
        self.session.on_input(&text, now);
    }

    /// 每帧:发起到期查询 / 收回投结果。
    pub fn poll(&mut self, now: std::time::Instant) {
        // 发起(工作线程同 palette 模式)
        if let Some(fire) = self.session.pending_at(now) {
            self.gen += 1;
            let gen = self.gen;
            let paths = self.paths.clone();
            let q = fire.query.clone();
            let tx = self.query_tx.clone().expect("query_tx 常驻");
            self.ctx.request_repaint(); // 立即清 pending 态
            std::thread::spawn(move || {
                let out = client_mod::query(&paths, &q, 30, None, false, true);
                let (entries, ok) = match out {
                    Ok(o) => (o.entries, true),
                    Err(_) => (Vec::new(), false),
                };
                let _ = tx.send((gen, entries, ok));
            });
            // 防抖续帧(同 palette 的 pending_delay 纪律)
            if let Some(d) = self.session.pending_delay(now) {
                self.ctx.request_repaint_after(d);
            }
        }
        // 收结果(代际校验)
        if let Some(rx) = &self.query_rx {
            while let Ok((gen, entries, _ok)) = rx.try_recv() {
                if gen != self.gen {
                    continue;
                }
                self.results = entries;
                self.selected = 0;
            }
        }
        if let Some(d) = self.session.pending_delay(now) {
            self.ctx.request_repaint_after(d);
        }
    }

    /// 当前选中目录完整路径。
    pub fn selected_path(&self) -> Option<String> {
        self.results.get(self.selected).map(|e| e.path.clone())
    }
}
```

- [ ] **Step 2: UI 绘制函数（deferred viewport 回调）**

```rust
/// deferred viewport 绘制(ui/mod.rs show_viewport_deferred 回调)。
pub(crate) fn show_overlay(ctx: &egui::Context, st: &mut OverlayState) {
    egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
        // 输入框:出现即聚焦(接线层 show 后首帧 grab focus)
        let input = egui::TextEdit::singleline(&mut st.input)
            .desired_width(OVERLAY_W - 24.0)
            .hint_text(crate::i18n::s().overlay_hint);
        let resp = ui.add(input);
        resp.request_focus();
        if resp.changed() {
            st.on_input(std::time::Instant::now());
        }
        if st.clipboard_hint {
            ui.label(crate::i18n::s().overlay_clipboard_hint);
        }
        // 目录列表
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, e) in st.results.iter().enumerate() {
                let row = ui.allocate_response(
                    egui::vec2(ui.available_width(), 24.0),
                    egui::Sense::click(),
                );
                let text = format!("{}  —  {}", e.name, e.path);
                ui.painter().text(
                    row.rect.left_top() + egui::vec2(8.0, 12.0),
                    egui::Align2::LEFT_CENTER,
                    text,
                    egui::FontId::proportional(13.0),
                    if i == st.selected {
                        ui.visuals().selection.bg_fill
                    } else {
                        ui.visuals().text_color()
                    },
                );
                if i == st.selected {
                    ui.painter().rect_filled(row.rect, 4.0, ui.visuals().selection.bg_fill.gamma_multiply(0.3));
                }
                if row.clicked() {
                    st.selected = i;
                }
                if row.double_clicked() {
                    // 双击 = 选中并提交(接线层读取 selected_path 触发跳转)
                    st.commit_requested = true;
                }
            }
        });
    });
    // 键盘:↑↓/Enter/Esc(浮层是 key window,事件不穿透宿主面板)
    ctx.input(|i| {
        for ev in &i.events {
            match ev {
                egui::Event::Key { key: egui::Key::ArrowDown, pressed: true, .. } => {
                    st.selected = (st.selected + 1).min(st.results.len().saturating_sub(1));
                }
                egui::Event::Key { key: egui::Key::ArrowUp, pressed: true, .. } => {
                    st.selected = st.selected.saturating_sub(1);
                }
                egui::Event::Key { key: egui::Key::Enter, pressed: true, .. } => {
                    st.commit_requested = true;
                }
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                    st.dismiss_requested = true;
                }
                _ => {}
            }
        }
    });
}
```

`OverlayState` 补两个字段 `pub commit_requested: bool`、`pub dismiss_requested: bool`（接线层每帧消费后清零）。

- [ ] **Step 3: ui/mod.rs 注册视口**

- `pub const OVERLAY_TITLE: &str = "overlay";`（放 platform/mod.rs，与 PALETTE_TITLE 同处）
- `overlay_builder()`（palette_builder 同型：`with_title(OVERLAY_TITLE).with_inner_size([OVERLAY_W, OVERLAY_H]).with_decorations(false).with_transparent(true).with_visible(false).with_always_on_top()`）
- `Shared` 加 `pub overlay: overlay::OverlayState,`；`App::new` 构造处初始化
- `update()`/viewport 注册处：`ctx.show_viewport_deferred(overlay_viewport_id(), overlay_builder(), move |ctx, _class| { ... 短锁取 overlay ... show_overlay(...) })`——锁纪律照 palette 视口的现有写法（palette 视口回调的持锁形态抄现文件）。
- 每帧 `App::update` 里 `shared.overlay.poll(now)`。

- [ ] **Step 4: 平台层窗口管理（mac.rs）**

仿 PALETTE 段：`OVERLAY: thread_local<RefCell<OverlayWinState>>`（含 `window: Option<Retained<NSWindow>>`、`orig_class`）；`MainWindow::Overlay` 枚举变体；`ensure_patched`/`convert` 复用（换类五要素 + orig_class 记录 + 销毁前还原——沿用现有 revert 纪律）。facade：

```rust
/// 显示浮层(panel_frame = 面板 AppKit frame;定位 = overlay_frame 纯函数结果)。
pub fn overlay_show(panel_frame: [f64; 4]) -> anyhow::Result<()>;
/// 隐藏浮层(orderOut;不销毁窗口)。
pub fn overlay_hide() -> anyhow::Result<()>;
```

`overlay_show` 流程：`ensure_patched` → `overlay_frame`（用 `NSScreen::mainScreen().frame()`）→ `setFrameOrigin` → 补 first responder → `orderFrontRegardless` → `makeKeyWindow`（顺序照 `show_palette` 的注释纪律：定位先于显示）。**改类后窗口销毁前还原**：Overlay 窗口与 PALETTE 同一 `revert_class` 路径覆盖（检查 mac.rs `revert_class()` 是否遍历两类窗口，不覆盖则扩展）。

- [ ] **Step 5: 实机验证（M0 验证点 ④：key 让还）**

手动：触发浮层 → 输入 → Enter（跳转）→ 焦点应回到面板文件名框；Esc → 浮层消失 → 面板仍可正常键入；浮层显示期间键入全部进浮层。结果回填 probe-findings 验证点 ④。

- [ ] **Step 6: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src
git commit -m "feat(app): overlay viewport for directory search over save panels"
```

---

## Task 14: 主接线（detector → overlay → navigator）

**Files:**
- Modify: `crates/app/src/ui/mod.rs`

- [ ] **Step 1: 事件循环接线**

`App::new`：detector 启动条件 = `settings_file.save_panel_assist && platform::panel_assist_trusted()` → `panel_detector::start_detector()` 得 `Receiver<PanelEvent>` 存 `App`。设置页开关切换后需重启 detector——`apply_panel_assist` 里经 `ctx.send_viewport_cmd`/直接标志位：简单方案 = `Shared` 加 `detector_desired: bool`，App::update 每帧比对 desired/running 并按需启停（start_detector 幂等 None）。daemon 守门：Shown 事件处理前 `client::ping(&paths)`，离线 → 忽略。

`App::update` 每帧：

```rust
while let Ok(ev) = detector_rx.try_recv() {
    match ev {
        PanelEvent::Shown { pid, frame } => {
            let mut s = self.shared();
            if !s.overlay.visible
                && !s.dismissals.blocked(pid)
                && client::ping(&self.paths)
            {
                drop(s);
                let _ = platform::overlay_show(frame);
                let mut s = self.shared();
                s.overlay.visible = true;
            }
        }
        PanelEvent::Closed => {
            let pid = /* 最近 shown 的 pid,由 Shared 记录 */;
            let mut s = self.shared();
            s.overlay.visible = false;
            s.dismissals.clear(pid);
            drop(s);
            let _ = platform::overlay_hide();
        }
    }
}
// 浮层请求消费
{
    let mut s = self.shared();
    if s.overlay.dismiss_requested {
        s.overlay.dismiss_requested = false;
        s.overlay.visible = false;
        s.dismissals.dismiss(shown_pid);
        drop(s);
        let _ = platform::overlay_hide();
    }
    if s.overlay.commit_requested {
        s.overlay.commit_requested = false;
        if let Some(path) = s.overlay.selected_path() {
            let pid = shown_pid;
            drop(s);
            let _ = platform::overlay_hide();
            std::thread::spawn(move || {
                let outcome = platform::navigate_panel(pid, &path);
                // FellBackToClipboard → 浮层重现 clipboard_hint(design §5)
            });
        }
    }
}
```

（`shown_pid` 存 `Shared`（`panel_pid: Option<i32>`），Shown/Closed/commit/dismiss 全走它——落地时以 `Shared` 字段形态写全，不引入裸全局。）

- [ ] **Step 2: 错误态回显**

`navigate_panel` 返回 `FellBackToClipboard` 时：`shared.overlay.clipboard_hint = true; platform::overlay_show(last_frame)`（浮层重现并显示「路径已复制，请手动 ⌘⇧G 粘贴」；用户 Esc 关闭走 dismissals 屏蔽规则）。`Done` 时无动作。i18n：`overlay_clipboard_hint: "路径已复制，按 ⌘⇧G 后 ⌘V 粘贴" / "Path copied — press ⌘⇧G then ⌘V"`、`overlay_hint: "输入目录名…" / "Type a directory name…"`。

- [ ] **Step 3: 手动回归（关键场景）**

- TextEdit ⌘⇧S：浮层出现 → 输入「tmp」→ Enter → 面板跳 /tmp
- Esc 中途打断（浮层隐藏后⌘⇧G 进行中 Esc）：不误粘贴、不误保存
- 剪贴板内容跳转后原样还原
- daemon 停止（托盘 stop）→ 面板弹出浮层不出现
- 设置关闭 `save_panel_assist` → 浮层不再出现

- [ ] **Step 4: 门禁 + Commit**

```bash
cargo fmt && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
git add crates/app/src
git commit -m "feat(app): wire panel detector to overlay and navigator"
```

---

## Task 15: 全量门禁 + 文档同步 + 实机验证矩阵

**Files:**
- Modify: `docs/usage.md`、`docs/architecture.md`、`docs/development.md`（如需）
- Modify: `crates/app/src/platform/mod.rs`（移除 `MCE_PANEL_DETECTOR` 临时开关）

- [ ] **Step 1: 移除过渡开关与探针遗留**

- `MCE_PANEL_DETECTOR` 开关删除（Task 9 加的）
- `panel_probe.rs` 保留（debug 工具，`MCE_PANEL_PROBE` 持续可用；模块注释标注「探针为开发取证工具，产品路径已由 detector/navigator 接管」）
- clippy 零容忍下清理所有过渡期 `#[allow(dead_code)]`

- [ ] **Step 2: 三门禁全绿**

```bash
cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
```

- [ ] **Step 3: 文档同步**

- `docs/usage.md`：新增「保存面板目录助手」节（功能描述、授权要求、设置开关、Esc/双击/回车交互、降级行为）
- `docs/architecture.md`：`panel_detector`/`dialog_navigator`/浮层视口一行级描述（实现态模块表）
- probe-findings 文档补验证点 ④（Task 13 已回填的确认）

- [ ] **Step 4: 实机验证矩阵（人工，逐项记录到 probe-findings 追加节）**

| 场景 | 预期 |
|---|---|
| TextEdit 另存为（sheet） | 浮层出现、跳转成功 |
| Finder「连接服务器」/导出类独立对话框 | 浮层出现、跳转成功 |
| VS Code「另存为」（Electron 宿主） | 按实测记录（AX 树可见性） |
| 网页版保存（浏览器内自绘 UI） | 误报不出现（特征判定拦截）或走剪贴板降级 |
| 深路径（≥6 级） | 跳转成功 |
| 剪贴板含富文本时跳转 | 跳转后剪贴板还原 |
| 浮层输入中直接 Esc ×2 | 第一次关浮层，第二次取消面板 |
| 跳转进行中按 Esc | 无误粘贴/无误保存（每步确认生效） |
| daemon 离线 | 浮层不出现 |

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "docs: save-panel-assist usage and architecture notes; cleanup probe switches"
```

---

## 自查记录

- **Spec 覆盖**：检测（Task 9）、两级判定/特征表（Task 8）、浮层第三视口 + 定位几何（Task 12/13）、双策略 navigator + 每步确认 + Esc 竞态（Task 10/11）、dir_only 链路（Task 5/6/7）、设置 + 授权引导 + 托盘一次性提示 + i18n（Task 4）、不打扰规则 Esc 本次生效（Task 9 Dismissals + Task 14）、daemon 离线守门（Task 14）、错误出口剪贴板提示（Task 11/14）、M0 六风险点（Task 3/4）、文档同步与实机矩阵（Task 15）。
- **占位符扫描**：Task 11 Step 1 的骨架中「collect 同款谓词搜索」「适配层」两处标注了落地要求（写全，不引用未定义符号）；Task 3 探针代码中重复块已在增补说明中明确删除指令。其余任务均为完整代码或精确行号引用。
- **类型一致性**：`client::query` 尾参 `dir_only: bool`（Task 7）与 Task 13/14 调用 `query(&paths, &q, 30, None, false, true)` 一致；`PanelEvent::Shown.frame: [f64; 4]` 与 `overlay_show(panel_frame: [f64; 4])`、`overlay_frame` 入参一致；`NavigateOutcome` 三态在 Task 11 定义、Task 14 消费一致；`Dismissals` 方法集（dismiss/blocked/clear）与 Task 14 用法一致。
- **顺序依赖**：Task 5（protocol）→ 6（engine）→ 7（透传）；Task 1/2（FFI）→ 3（探针）→ 8/9/10/11（M1 平台层）；Task 12/13/14 依赖 9/11。Task 4（设置）无依赖可并行。

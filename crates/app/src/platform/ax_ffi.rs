//! AX API 手写 FFI(ADR-8 平台层):objc2 系未覆盖 ApplicationServices/HIServices
//! 的 AXUIElement 族,此处按 C ABI 显式声明。CF 类型以 `*mut c_void` 直通,
//! RAII 包装经 Drop → CFRelease 释放。除 AXObserver 族(回调派发到创建线程的
//! runloop,见 panel_detector)外,AXUIElement 调用为线程无关 CF 层——
//! navigator 后台线程直接调用(M0 实机验证点)。
//!
//! SAFETY 总纪律:所有 raw FFI 仅在本模块 unsafe 块内调用;对外暴露的都是
//! 安全封装(参数为 &str/u32 等安全类型,返回 RAII 或 Option)。
//
// 当前整层尚未接线(保存面板助手后续任务使用),全部项判死代码属预期过渡态;
// 接线完成后移除下方 allow,恢复逐项死代码检查。
#![allow(non_snake_case)]
#![allow(dead_code)] // M0 探针/后续任务接线后移除

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

/// AXValue 类型枚举(HIServices AXValueType,32 位 enum):CGPoint = 1,CGSize = 2。
const AX_VALUE_POINT_TYPE: i32 = 1;
const AX_VALUE_SIZE_TYPE: i32 = 2;

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
    fn AXUIElementSetAttributeValue(
        el: AXUIElementRef,
        attr: CFStringRef,
        val: CFTypeRef,
    ) -> AXError;
    fn AXUIElementIsAttributeSettable(
        el: AXUIElementRef,
        attr: CFStringRef,
        out: *mut u8,
    ) -> AXError;
    fn AXObserverCreate(pid: i32, callback: AXObserverCallback, out: *mut AXObserverRef)
        -> AXError;
    fn AXObserverAddNotification(
        obs: AXObserverRef,
        el: AXUIElementRef,
        notification: CFStringRef,
        refcon: *mut c_void,
    ) -> AXError;
    fn AXObserverGetRunLoopSource(obs: AXObserverRef) -> CFRunLoopSourceRef;
    fn AXValueGetValue(value: CFTypeRef, t: i32, out: *mut c_void) -> u8;
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFArrayGetCount(arr: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(arr: CFTypeRef, idx: isize) -> *const c_void;
    /// CFBase.h 真实签名返回同一引用;调用点忽略返回值即可。
    fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
}

/// AXObserver 回调(C 函数指针,无环境捕获;状态经 refcon/static 传递)。
pub type AXObserverCallback = extern "C" fn(
    observer: AXObserverRef,
    element: AXUIElementRef,
    notification: CFStringRef,
    refcon: *mut c_void,
);

/// 属性名常量(与 HIServices 头一致)。
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
        let p = unsafe {
            CFStringCreateWithCString(std::ptr::null_mut(), c.as_ptr(), CF_STRING_ENCODING_UTF8)
        };
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
    // SAFETY:成功时 out 为 +1 引用(CFTypeRef 形态 *const,RAII 侧统一
    // *mut 句柄,仅指针可变性转换,不改变指向)。
    unsafe { AXRef::take(out as *mut c_void) }
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
    let mut out: u8 = 0;
    // SAFETY:out 为本栈帧可写 Boolean 出参;CFStr 生命周期覆盖调用。
    let err = unsafe { AXUIElementIsAttributeSettable(el.raw(), a.raw(), &mut out) };
    err == AX_ERROR_SUCCESS && out != 0
}

/// 读字符串属性。
pub fn attr_string(el: &AXRef, attr: &str) -> Option<String> {
    let v = copy_attr(el, attr)?;
    value_string(&v)
}

/// CFString 值 → Rust String(NSString toll-free bridge,经 objc2 取 UTF-8)。
pub fn value_string(v: &AXRef) -> Option<String> {
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    // SAFETY:CFTypeID 比对排除非 CFString(如 AXValue/数组)后才桥接;
    // CFString 与 NSString toll-free bridge,指针形态一致。
    unsafe {
        if CFGetTypeID(v.raw()) != CFStringGetTypeID() {
            return None; // 非 CFString,不桥接
        }
        // from_raw 接管 +1 引用后 forget,阻止其 Drop 再减 1——+1 所有权仍在
        // AXRef,由 AXRef::drop 的 CFRelease 释放;此处只借内容,计数平衡。
        let ns = Retained::from_raw(v.raw() as *mut NSString)?;
        let s = ns.to_string();
        std::mem::forget(ns);
        Some(s)
    }
}

/// CFStringRef(raw)→ Rust String(AXObserver 回调用;引用归调用方,只借内容)。
pub fn cfstring_to_string(p: *const c_void) -> Option<String> {
    use objc2::rc::Retained;
    use objc2_foundation::NSString;
    // SAFETY:同 value_string——先核对 CFTypeID;from_raw 接管 +1 后 forget,
    // 原引用计数归调用方,此处只借内容。
    unsafe {
        if p.is_null() || CFGetTypeID(p) != CFStringGetTypeID() {
            return None;
        }
        let ns = Retained::from_raw(p as *mut NSString)?;
        let s = ns.to_string();
        std::mem::forget(ns);
        Some(s)
    }
}

/// 读 CFValue 的 point/size(AXPosition/AXSize;CGSize 与双 f64 同构)。
pub fn attr_point(el: &AXRef, attr: &str) -> Option<(f64, f64)> {
    let v = copy_attr(el, attr)?;
    let mut out = XY { x: 0.0, y: 0.0 };
    // SAFETY:out 为 CGSize 同构双 f64 栈内存;类型常量按 HIServices 枚举。
    let ok = unsafe {
        AXValueGetValue(
            v.raw(),
            AX_VALUE_POINT_TYPE,
            &mut out as *mut _ as *mut c_void,
        )
    };
    (ok != 0).then_some((out.x, out.y))
}

pub fn attr_size(el: &AXRef, attr: &str) -> Option<(f64, f64)> {
    let v = copy_attr(el, attr)?;
    let mut out = XY { x: 0.0, y: 0.0 };
    // SAFETY:同 attr_point。
    let ok = unsafe {
        AXValueGetValue(
            v.raw(),
            AX_VALUE_SIZE_TYPE,
            &mut out as *mut _ as *mut c_void,
        )
    };
    (ok != 0).then_some((out.x, out.y))
}

#[repr(C)]
struct XY {
    x: f64,
    y: f64,
}

/// 子元素列表(AXChildren;快照遍历与按钮定位共用)。
pub fn children(el: &AXRef) -> Vec<AXRef> {
    let Some(v) = copy_attr(el, attr::CHILDREN) else {
        return Vec::new();
    };
    let arr = v.raw();
    // SAFETY:CFArray 与 NSArray toll-free bridged;元素引用归数组所有,
    // CFRetain 复制后交 AXRef 接管。
    unsafe {
        let n = CFArrayGetCount(arr);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let e = CFArrayGetValueAtIndex(arr, i); // 未经 retain
            if !e.is_null() {
                CFRetain(e);
                if let Some(r) = AXRef::take(e as *mut c_void) {
                    out.push(r);
                }
            }
        }
        out
    }
}

/// 复制 AXRef 引用(+1;谓词搜索命中时从遍历副本留下命中元素用)。
pub fn clone_ref(el: &AXRef) -> AXRef {
    // SAFETY:CFRetain +1 后交新 AXRef 接管,引用计数平衡。
    unsafe {
        CFRetain(el.raw());
        AXRef::take(el.raw()).expect("clone_ref: 非空元素")
    }
}

/// 创建 AXObserver(回调派发到本调用线程的 runloop;须与 attach 同线程)。
pub fn observer_create(pid: i32, cb: AXObserverCallback) -> Option<AXRef> {
    let mut obs: AXObserverRef = std::ptr::null_mut();
    // SAFETY:obs 出参为本栈帧指针;callback 为无捕获 C 函数指针。
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
    let err =
        unsafe { AXObserverAddNotification(obs.raw(), el.raw(), n.raw(), refcon as *mut c_void) };
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
        CFRunLoopAddSource(
            rl,
            AXObserverGetRunLoopSource(obs.raw()),
            kCFRunLoopDefaultMode,
        );
    }
}

/// 当前线程 runloop raw 指针(探针安装 CFRunLoopTimer 用)。
pub fn current_runloop_raw() -> *mut c_void {
    // SAFETY:无参。
    unsafe { CFRunLoopGetCurrent() }
}

/// kCFRunLoopDefaultMode raw(探针安装 timer 用)。
pub fn default_mode_raw() -> *const c_void {
    // SAFETY:全局常量符号。
    unsafe { kCFRunLoopDefaultMode }
}

/// 阻塞跑当前线程 runloop(探针/detector 专用线程)。
pub fn run_current_runloop() {
    // SAFETY:无参;阻塞语义即本函数契约。
    unsafe { CFRunLoopRun() }
}

/// 请求退出 run_current_runloop。
pub fn stop_current_runloop() {
    // SAFETY:当前线程 runloop 引用。
    unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) }
}

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
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRunLoopSourceCreate(
        alloc: CFAllocatorRef,
        order: isize,
        context: *mut CFRunLoopSourceContext,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopSourceSignal(src: CFRunLoopSourceRef);
    fn CFRunLoopWakeUp(rl: CFRunLoopRef);
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
    fn AXUIElementPerformAction(el: AXUIElementRef, action: CFStringRef) -> AXError;
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
    fn CFArrayGetTypeID() -> usize;
    fn CFArrayGetCount(arr: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(arr: CFTypeRef, idx: isize) -> *const c_void;
    fn CFEqual(a: CFTypeRef, b: CFTypeRef) -> u8;
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
    pub const FOCUSED_ELEMENT: &str = "AXFocusedElement";
    pub const POSITION: &str = "AXPosition";
    pub const SIZE: &str = "AXSize";
    /// 非公开常量(不在公开 HIServices 头),存在性待 M0 实机验证;
    /// 可编辑性判定以 role + attr_settable(VALUE) 为主路径。
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

/// RAII AX 对象(AXUIElement/AXValue 等 Copy*/Create* 产出;Drop → CFRelease)。
/// 不含 AXObserver——见下方 [`AXObserverWrap`](其不 impl Send,类型系统
/// 阻止跨线程转移误用)。
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

// SAFETY:CF 对象为堆分配句柄,跨线程转移后调用 AX CF 层 API 合法。
// AXObserver 不在此列:独立 AXObserverWrap 类型不 impl Send,跨线程转移
// 由类型系统阻止(observer 的 runloop source 须挂创建线程的 runloop)。
unsafe impl Send for AXRef {}
unsafe impl Send for CFStr {}

/// RAII AXObserver(Drop → CFRelease;**不 impl Send**):observer 的
/// runloop source 必须挂在创建线程的 runloop 上,回调也派发到该线程,
/// 故禁止 Send 转移——detector 只能在创建 observer 的专用线程上使用。
pub struct AXObserverWrap(*mut c_void);

impl AXObserverWrap {
    pub fn raw(&self) -> *mut c_void {
        self.0
    }
}

impl Drop for AXObserverWrap {
    fn drop(&mut self) {
        // SAFETY:构造约定 +1 引用归本对象。
        unsafe { CFRelease(self.0) }
    }
}

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

/// 读属性(返回 RAII CF 对象;失败/不支持 → None)。遍历热路径用,错误细节
/// 不透出;诊断需要区分错误码时用 [`copy_attr_err`]。
pub fn copy_attr(el: &AXRef, attr: &str) -> Option<AXRef> {
    copy_attr_err(el, attr).ok()
}

/// 读属性,错误码透出(probe/detector 诊断用:M0 需区分 CannotComplete
/// 与 AttributeUnsupported)。
pub fn copy_attr_err(el: &AXRef, attr: &str) -> Result<AXRef, AXError> {
    let a = CFStr::new(attr);
    let mut out: CFTypeRef = std::ptr::null();
    // SAFETY:out 为本栈帧可写指针;CFStr 生命周期覆盖调用。
    let err = unsafe { AXUIElementCopyAttributeValue(el.raw(), a.raw(), &mut out) };
    if err != AX_ERROR_SUCCESS {
        return Err(err);
    }
    // SAFETY:成功时 out 为 +1 引用(CFTypeRef 形态 *const,RAII 侧统一
    // *mut 句柄,仅指针可变性转换,不改变指向)。成功路径 out 非空为 API
    // 契约,take 不会返回 None。
    Ok(unsafe { AXRef::take(out as *mut c_void) }.expect("copy_attr_err: 成功路径 out 非空"))
}

/// 写属性(策略 A 的 AX 直设 value 用;模块纪律:对外只暴露安全类型,
/// 当前仅支持字符串值)。
pub fn set_attr(el: &AXRef, attr: &str, value: &CFStr) -> Result<(), AXError> {
    let a = CFStr::new(attr);
    // SAFETY:value 为调用方借用,生命周期覆盖调用;常量属性名合法。
    let err = unsafe { AXUIElementSetAttributeValue(el.raw(), a.raw(), value.raw()) };
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

/// 执行元素动作(AXPress 等;「前往」按钮确认用)。
pub fn perform_action(el: &AXRef, action: &str) -> bool {
    let a = CFStr::new(action);
    // SAFETY:CFStr 生命周期覆盖调用;无指针出参。
    let err = unsafe { AXUIElementPerformAction(el.raw(), a.raw()) };
    err == AX_ERROR_SUCCESS
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
    // SAFETY:先比对 CFTypeID 确认是 CFArray 再遍历(与 value_string 守卫
    // 模式一致);元素引用归数组所有,CFRetain 复制后交 AXRef 接管。
    unsafe {
        if CFGetTypeID(arr) != CFArrayGetTypeID() {
            return Vec::new(); // 非 CFArray(异常形态),按无子处理
        }
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

/// CF 身份判定(AXUIElementRef 代理可能被重建,裸指针比较不可靠;CF 规范做法)。
pub fn cf_equal(a: &AXRef, b: &AXRef) -> bool {
    // SAFETY:两参数均为存活 +1 引用,只读比较。
    unsafe { CFEqual(a.raw(), b.raw()) != 0 }
}

/// 创建 AXObserver(回调派发到本调用线程的 runloop;须与 attach 同线程)。
/// 返回的 [`AXObserverWrap`] 不 impl Send,只能留在本线程使用。
pub fn observer_create(pid: i32, cb: AXObserverCallback) -> Option<AXObserverWrap> {
    let mut obs: AXObserverRef = std::ptr::null_mut();
    // SAFETY:obs 出参为本栈帧指针;callback 为无捕获 C 函数指针。
    let err = unsafe { AXObserverCreate(pid, cb, &mut obs) };
    if err != AX_ERROR_SUCCESS {
        return None;
    }
    if obs.is_null() {
        return None; // 成功路径不应发生(API 契约),防御性兜底
    }
    // SAFETY:成功时 obs 为 +1 引用,交 AXObserverWrap 接管。
    Some(AXObserverWrap(obs))
}

/// 注册通知(须传与 pid 同源的 element;observer 与 element 同线程使用)。
pub fn observer_add_notification(
    obs: &AXObserverWrap,
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
pub fn attach_observer_to_current_runloop(obs: &AXObserverWrap) {
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

/// observer 的 runloop source 从当前线程 runloop 拆下(换绑时先拆再 CFRelease;
/// 否则旧 source 仍挂在 runloop 上,释放后归属未定义)。当前线程调用。
pub fn detach_observer_from_current_runloop(obs: &AXObserverWrap) {
    // SAFETY:source 归 observer 所有,runloop 只借;mode 为全局常量。
    unsafe {
        let rl = CFRunLoopGetCurrent();
        CFRunLoopRemoveSource(
            rl,
            AXObserverGetRunLoopSource(obs.raw()),
            kCFRunLoopDefaultMode,
        );
    }
}

/// CFRunLoopSourceContext(C 布局;仅 perform 有意义,其余置空)。
#[repr(C)]
pub struct CFRunLoopSourceContext {
    pub version: isize,
    pub info: *mut c_void,
    pub retain: usize,
    pub release: usize,
    pub copy_description: usize,
    pub equal: usize,
    pub hash: usize,
    pub schedule: usize,
    pub cancel: usize,
    pub perform: extern "C" fn(info: *mut c_void),
}

/// 创建自定义 runloop 信号源(跨线程唤醒 detector 线程;perform 在该线程派发)。
/// 返回 RAII 包装;调用方负责 CFRunLoopAddSource 到目标 runloop(见
/// [`attach_signal_source`])。
pub fn signal_source_create(perform: extern "C" fn(*mut c_void)) -> Option<AXRef> {
    let mut ctx = CFRunLoopSourceContext {
        version: 0,
        info: std::ptr::null_mut(),
        retain: 0,
        release: 0,
        copy_description: 0,
        equal: 0,
        hash: 0,
        schedule: 0,
        cancel: 0,
        perform,
    };
    // SAFETY:ctx 为本栈帧生命周期,CF 侧只读拷贝;返回 +1 引用交 AXRef。
    unsafe { AXRef::take(CFRunLoopSourceCreate(std::ptr::null_mut(), -1, &mut ctx)) }
}

/// 把信号源挂到当前线程 runloop。
pub fn attach_signal_source(src: &AXRef) {
    // SAFETY:source 生命周期由调用方(AXRef)保证;mode 为全局常量。
    unsafe {
        CFRunLoopAddSource(CFRunLoopGetCurrent(), src.raw(), kCFRunLoopDefaultMode);
    }
}

/// 唤醒目标 runloop 上的信号源(跨线程调用;两指针由调用方以 AtomicPtr 共享)。
pub fn signal_and_wake(source: *mut c_void, target_runloop: *mut c_void) {
    // SAFETY:非空指针由调用方保证指向存活 CF 对象(进程生命周期)。
    unsafe {
        if !source.is_null() {
            CFRunLoopSourceSignal(source);
        }
        if !target_runloop.is_null() {
            CFRunLoopWakeUp(target_runloop);
        }
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

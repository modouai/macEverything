//! CGEvent 键盘合成(ADR-8 平台层):保存面板跳转的 ⌘⇧G/⌘V/Return。
//! 手写 FFI(objc2 系 CGEvent binding 的 feature 覆盖不足,直接声明 CoreGraphics
//! 符号;CGEventPostToPid 自 macOS 10.11 可用,面板服务进程投递行为为 M0
//! 实机验证点,失败回退全局 post 形态)。
//!
//! SAFETY 总纪律:对外只暴露安全封装(post_combo 参数全安全类型);raw FFI
//! 收敛在单一 unsafe 块。
#![allow(non_snake_case)]
// 当前整层尚未接线(保存面板助手后续任务使用),全部项判死代码属预期过渡态;
// 接线完成后移除下方 allow,恢复逐项死代码检查。
#![allow(dead_code)] // M0 探针/后续任务接线后移除

use std::ffi::c_void;

extern "C" {
    fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, key_down: bool)
        -> *mut c_void;
    fn CGEventSetFlags(ev: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, ev: *mut c_void);
    fn CGEventPostToPid(pid: i32, ev: *mut c_void);
    // CFTypeRef = *const c_void(与 ax_ffi 同签名;extern 声明 crate 全局,签名不一致会触发
    // clashing_extern_declarations)。传 *mut c_void 实参自动协变到 *const。
    fn CFRelease(cf: *const c_void);
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
///
/// pid 时效性契约:`pid` 须为当前面板宿主进程;进程退出后 pid 被回收复用时,
/// 组合键会送进无关进程——调用方负责以最新 pid 调用(接线层在每次跳转前取前台 pid)。
pub fn post_combo(pid: Option<i32>, keycode: u16, flags: u64) {
    // SAFETY:CGEventCreateKeyboardEvent 成功时为 +1 引用,归本调用所有;post
    // 不消费所有权(仅投递),故必须由本调用 CFRelease,否则泄漏——无悬垂路径。
    // 创建失败仅 OOM 级,与 ax_ffi 的 CFString 创建同先例,断言即熔断。
    unsafe {
        for down in [true, false] {
            let ev = CGEventCreateKeyboardEvent(std::ptr::null_mut(), keycode, down);
            assert!(
                !ev.is_null(),
                "CGEventCreateKeyboardEvent 失败(仅 OOM 级;与 ax_ffi CFString 创建同先例)"
            );
            CGEventSetFlags(ev, flags);
            match pid {
                Some(p) => CGEventPostToPid(p, ev),
                None => CGEventPost(TAP_HID, ev),
            }
            CFRelease(ev);
        }
    }
}

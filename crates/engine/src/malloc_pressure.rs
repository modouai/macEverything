//! malloc 死页归还(峰值瘦身,tech.md §3):大块释放后 macOS libmalloc 滞留
//! 脏页不立即归还内核(vmmap 中表现为 `Malloc Large (empty)` 高位驻留,实测
//! daemon 重扫后 254MB、GUI 交互后 3GB),`malloc_zone_pressure_relief` 强制
//! 折叠。重扫 flush 后与 compact 后各调一次;GUI 空闲归还同用此入口。
#![allow(unsafe_code)]

/// 归还默认 malloc zone 的可折叠脏页(`flag=1` = 全力归还)。
/// 幂等且开销低(仅在存在滞留页时有工作可做),不需要节流。
#[cfg(target_os = "macos")]
pub fn release_heap_pressure() {
    use std::os::raw::{c_int, c_void};

    unsafe extern "C" {
        fn malloc_default_zone() -> *mut c_void;
        // 变长参数签名中我们只用到前两个固定参:len=0 表示整个 zone。
        fn malloc_zone_pressure_relief(zone: *mut c_void, len: usize, flag: c_int);
    }
    unsafe {
        malloc_zone_pressure_relief(malloc_default_zone(), 0, 1);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn release_heap_pressure() {}

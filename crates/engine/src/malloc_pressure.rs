//! malloc 死页归还(峰值瘦身,tech.md §3):大块释放后 macOS libmalloc 滞留
//! 脏页不立即归还内核(vmmap 中表现为 `Malloc Large (empty)` 高位驻留,实测
//! daemon 重扫后 254MB、GUI 交互后 3GB),`malloc_zone_pressure_relief` 强制
//! 折叠。重扫 flush 后与 compact 后各调一次;GUI 空闲归还同用此入口。
#![allow(unsafe_code)]

/// 归还默认 malloc zone 的可折叠脏页(`goal=0` = 尽力归还全部)。
/// 幂等且开销低(仅在存在滞留页时有工作可做),不需要节流。
///
/// 签名以系统头 `malloc/malloc.h` 为准:**两参数**
/// `size_t malloc_zone_pressure_relief(malloc_zone_t *zone, size_t goal)`;
/// 曾误声明为三参数(多一个 flag),按 ABI 多余参数被忽略,但那是不依
/// 赖实现细节的危险写法,已改为精确签名;返回值为实际归还字节数(仅
/// debug 日志用)。
#[cfg(target_os = "macos")]
pub fn release_heap_pressure() {
    use std::os::raw::c_void;

    unsafe extern "C" {
        fn malloc_default_zone() -> *mut c_void;
        fn malloc_zone_pressure_relief(zone: *mut c_void, goal: usize) -> usize;
    }
    // 迭代折叠:单次调用只处理当时可见的空闲段,Small zone 的碎片化段
    // 需要多轮才能收敛(实测单次后仍有 70MB+ 小对象死页)。最多 8 轮,
    // 返回 0 即收敛;每轮开销与剩余空闲段数成正比,收敛后近乎零成本。
    let zone = unsafe { malloc_default_zone() };
    let mut total = 0usize;
    for _ in 0..8 {
        let n = unsafe { malloc_zone_pressure_relief(zone, 0) };
        if n == 0 {
            break;
        }
        total += n;
    }
    if total > 0 {
        log::debug!("malloc pressure relief 归还 {} KB", total / 1024);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn release_heap_pressure() {}

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

/// 已清空但仍保留容量的 Vec:把可页对齐的整页标记为 `MADV_FREE_REUSABLE`,
/// 内核可立即回收物理页(不再计入 footprint),容量与指针不变,后续 push 会
/// 重新缺页为零页。
///
/// 为什么需要:macOS 27 上 `malloc_zone_pressure_relief` 对 malloc Large 释放
/// 出的空段实测返回 0(见 `release_heap_pressure` 注),而「清空 + 保留容量」
/// 复用的大缓冲若不打标,仍按 dirty 计入 footprint。仅对 `len == 0` 的 Vec
/// 调用(zero 后内容本来就不再被读)。
#[cfg(target_os = "macos")]
pub fn release_reusable_pages<T>(v: &mut Vec<T>) {
    use std::os::raw::c_void;

    if !v.is_empty() || v.capacity() == 0 {
        return;
    }
    let bytes = v.capacity().saturating_mul(std::mem::size_of::<T>());
    if bytes < 64 * 1024 {
        return;
    }
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as usize;
    let base = v.as_mut_ptr() as usize;
    let start = (base + page - 1) & !(page - 1);
    let end = (base + bytes) & !(page - 1);
    if end <= start {
        return;
    }
    // SAFETY:区间完全落在该 Vec 的已分配容量内且按页对齐;Vec 长度为 0,
    // 内容不再被读取。MADV_FREE_REUSABLE 只把整页标记为可回收/可复用。
    unsafe {
        libc::madvise(start as *mut c_void, end - start, libc::MADV_FREE_REUSABLE);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn release_reusable_pages<T>(_v: &mut Vec<T>) {}

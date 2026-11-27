//! Software prefetch hints for loops whose next reads are known ahead (chunk iteration).

/// Asks the CPU to start loading the cache line at `ptr`. Only a hint: it never reads in the
/// language sense, never faults (also for dangling or null pointers) and does nothing on
/// targets without a prefetch instruction.
#[inline(always)]
pub fn read<T>(ptr: *const T) {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `prefetcht0` is part of SSE, which every x86_64 CPU has; a prefetch does not
    // access memory architecturally and cannot fault, whatever the address.
    unsafe {
        std::arch::x86_64::_mm_prefetch::<{ std::arch::x86_64::_MM_HINT_T0 }>(ptr.cast());
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = ptr;
}

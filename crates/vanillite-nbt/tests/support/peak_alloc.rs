//! Per-thread peak allocation tracking for the exploit tests: a counting global allocator that
//! shows a hostile input was rejected without allocating what it asked for (SEC-03).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

pub struct Tracking;

thread_local! {
    static CURRENT: Cell<usize> = const { Cell::new(0) };
    static PEAK: Cell<usize> = const { Cell::new(0) };
}

fn grow(n: usize) {
    let _ = CURRENT.try_with(|c| {
        let now = c.get().saturating_add(n);
        c.set(now);
        let _ = PEAK.try_with(|p| p.set(p.get().max(now)));
    });
}

fn shrink(n: usize) {
    let _ = CURRENT.try_with(|c| c.set(c.get().saturating_sub(n)));
}

// SAFETY: every call is forwarded unchanged to the system allocator; the counters are
// const-initialised thread locals without destructors, so they never allocate.
unsafe impl GlobalAlloc for Tracking {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        grow(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        shrink(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        grow(new_size);
        shrink(layout.size());
        // SAFETY: same contract as the caller's.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Runs `f` and returns its result with the peak number of bytes it held allocated at once on
/// this thread.
pub fn peak<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CURRENT.with(Cell::get);
    PEAK.with(|p| p.set(base));
    let result = f();
    (result, PEAK.with(Cell::get).saturating_sub(base))
}

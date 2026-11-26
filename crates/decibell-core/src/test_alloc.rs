//! Test-only: the unit-test binary's global allocator counts allocations per
//! thread, so a test can assert that a hot path allocates nothing (the video
//! receive path's steady state, `media/video_recv.rs`). Per thread because
//! libtest runs tests in parallel: a test reads only its own thread's count.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    // Const-initialised and drop-free: reading it never allocates.
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAlloc;

fn count() {
    let _ = ALLOCATIONS.try_with(|c| c.set(c.get() + 1));
}

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Heap allocations (and reallocations) made on the calling thread so far.
pub fn allocations_on_this_thread() -> u64 {
    ALLOCATIONS.try_with(|c| c.get()).unwrap_or(0)
}

//! An allocator that counts what the current thread allocates, for the benches and the
//! "painting allocates nothing" test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

pub struct Counting;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<i64> = const { Cell::new(0) };
}

// SAFETY: every call is forwarded to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        let _ = BYTES.try_with(|bytes| bytes.set(bytes.get() + layout.size() as i64));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = BYTES.try_with(|bytes| bytes.set(bytes.get() - layout.size() as i64));
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        let _ = BYTES.try_with(|bytes| bytes.set(bytes.get() + new_size as i64 - layout.size() as i64));
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

/// (allocations, bytes still held) by this thread so far.
pub fn counters() -> (u64, i64) {
    (ALLOCATIONS.with(Cell::get), BYTES.with(Cell::get))
}

//! The scroll benchmark with allocations counted: the same run as `decibell --bench-scroll`,
//! in a binary whose global allocator counts every allocation of the process, so each frame
//! record carries how many the list's prepaint + paint made and how many the whole frame did.
//!
//! ```sh
//! cargo run --release -p decibell-chat --features fixtures --example chat_bench -- [seconds] [px/s …]
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn main() {
    let mut args = std::env::args().skip(1).filter_map(|arg| arg.parse::<f64>().ok());
    let seconds = args.next().unwrap_or(6.);
    let mut speeds: Vec<f64> = args.collect();
    if speeds.is_empty() {
        speeds = vec![1000., 3000.];
    }
    decibell_chat::bench::set_alloc_probe(|| ALLOCATIONS.load(Ordering::Relaxed));
    gpui_platform::application().with_assets(decibell_ui::Assets).run(move |cx| {
        decibell_ui::init(cx, decibell_theme::ThemeId::default()).expect("the theme and fonts");
        decibell_model::Model::init(cx, None);
        decibell_shell::init(cx);
        cx.spawn(async move |cx| {
            match decibell_chat::bench::run(cx.clone(), seconds, &speeds, None).await {
                Ok(report) => println!("{report:#}"),
                Err(error) => eprintln!("chat_bench: {error:#}"),
            }
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}

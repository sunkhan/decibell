//! Cost report: embedded size, first-use time, memory per loaded language,
//! throughput. `cargo run --release -p decibell-highlight --example measure`
//!
//! Each language is measured on its sample in tests/corpus/samples (the first
//! ~60 lines for the "snippet" figures: chat-sized). Memory is counted by a
//! wrapping allocator: live bytes after the first highlight (grammar tables
//! and the regexes that snippet needed) and after compiling every regex.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use decibell_highlight::{highlight_language, testing, Budget, Language};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        LIVE.fetch_add(layout.size(), Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        LIVE.fetch_add(new_size, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/samples"));
    let budget = Budget::UNLIMITED;
    println!(
        "{:<11} {:>8} {:>9} {:>10} {:>10} {:>10} {:>9} {:>9} {:>10}",
        "language", "embedded", "first use", "mem after", "all regex", "mem all", "snippet", "MB/s", "steps/byte"
    );
    println!(
        "{:<11} {:>8} {:>9} {:>10} {:>10} {:>10} {:>9} {:>9}",
        "", "bytes", "ms", "1st, KiB", "ms", "KiB", "µs", "(file)"
    );
    let (mut total_embedded, mut total_first, mut total_all, mut bytes, mut secs) = (0, 0usize, 0usize, 0usize, 0f64);
    let mut total_steps = 0u64;
    for &language in Language::all() {
        let name = language.name();
        let Ok(sample) = std::fs::read_to_string(dir.join(format!("{name}.txt"))) else {
            println!("{name:<11} {:>8}  (no sample)", language.embedded_size());
            total_embedded += language.embedded_size();
            continue;
        };
        let snippet: String = sample.lines().take(60).collect::<Vec<_>>().join("\n");
        let before = LIVE.load(Ordering::Relaxed);
        let t = Instant::now();
        let out = highlight_language(language, &snippet, &budget);
        let first = t.elapsed();
        drop(out);
        let mem_first = LIVE.load(Ordering::Relaxed) - before;

        // steady state on the snippet
        let n = 200;
        let t = Instant::now();
        for _ in 0..n {
            std::hint::black_box(highlight_language(language, &snippet, &budget));
        }
        let per_snippet = t.elapsed() / n;

        // throughput on the whole file
        let reps = (2_000_000 / sample.len().max(1)).max(3);
        let t = Instant::now();
        for _ in 0..reps {
            std::hint::black_box(highlight_language(language, &sample, &budget));
        }
        let dt = t.elapsed().as_secs_f64();
        let mbps = (sample.len() * reps) as f64 / dt / 1e6;
        bytes += sample.len() * reps;
        secs += dt;

        let steps = testing::steps(language, &sample);
        total_steps += steps;
        let t = Instant::now();
        testing::compile_all(language).unwrap();
        let all = t.elapsed();
        let mem_all = LIVE.load(Ordering::Relaxed) - before;

        println!(
            "{:<11} {:>8} {:>9.2} {:>10.0} {:>10.2} {:>10.0} {:>9.0} {:>9.2} {:>10.1}",
            name,
            language.embedded_size(),
            first.as_secs_f64() * 1e3,
            mem_first as f64 / 1024.0,
            all.as_secs_f64() * 1e3,
            mem_all as f64 / 1024.0,
            per_snippet.as_secs_f64() * 1e6,
            mbps,
            steps as f64 / sample.len() as f64
        );
        total_embedded += language.embedded_size();
        total_first += mem_first;
        total_all += mem_all;
    }
    println!(
        "total embedded {} bytes; all sampled languages loaded: {:.0} KiB after first use (sub-languages loaded by an earlier row count there), {:.0} KiB with every regex compiled; overall {:.2} MB/s; {} regex steps for one pass over every sample",
        total_embedded,
        total_first as f64 / 1024.0,
        total_all as f64 / 1024.0,
        bytes as f64 / secs / 1e6,
        total_steps
    );
}

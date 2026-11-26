//! `parse` over the 10,000-message fixture: ns per message, heap allocations per message, and
//! the retained size of the parsed documents.
//!
//!   cargo run --release -p decibell-richtext --example bench
//!   … --example bench -- fuzz        (the 60,000 fuzz strings instead)

#[path = "../tests/common/mod.rs"]
mod common;

use decibell_richtext::{parse, plain, ParseOptions};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size(), Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n, Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static A: Counting = Counting;

fn main() {
    let which = std::env::args().nth(1).unwrap_or_else(|| "fixture".into());
    let msgs = match which.as_str() {
        "fuzz" => common::fuzz(),
        _ => common::fixture(),
    };
    let opts = ParseOptions::default();
    let n = msgs.len();
    let bytes: usize = msgs.iter().map(String::len).sum();

    // Warm the thread-local scratch.
    for m in &msgs {
        std::hint::black_box(parse(m, &opts));
    }

    let rounds: usize = std::env::var("BENCH_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let mut docs = Vec::with_capacity(n);
    let mut best = f64::MAX;
    let mut allocs_per_msg = 0.0;
    for r in 0..rounds {
        docs.clear();
        let a0 = ALLOCS.load(Relaxed);
        let t = Instant::now();
        for m in &msgs {
            docs.push(parse(m, &opts));
        }
        let ns = t.elapsed().as_nanos() as f64 / n as f64;
        best = best.min(ns);
        if r == 0 {
            allocs_per_msg = (ALLOCS.load(Relaxed) - a0) as f64 / n as f64;
        }
    }
    let spans: usize = docs.iter().map(|d| d.spans().len()).sum();
    let heap: usize = docs.iter().map(|d| std::mem::size_of_val(d.spans())).sum();
    let with_alloc = docs.iter().filter(|d| !d.spans().is_empty()).count();
    println!("{which}: {n} messages, {:.1} bytes avg", bytes as f64 / n as f64);
    println!("parse: {best:.0} ns/message (best of {rounds} rounds, {:.1} MB/s)", bytes as f64 / n as f64 / best * 1e3);
    println!("allocations: {allocs_per_msg:.3}/message ({with_alloc} of {n} documents own a span array)");
    println!(
        "documents: {:.2} spans/message, Span = {} bytes, Document = {} bytes inline + {:.0} bytes heap/message",
        spans as f64 / n as f64,
        std::mem::size_of::<decibell_richtext::Span>(),
        std::mem::size_of::<decibell_richtext::Document>(),
        heap as f64 / n as f64
    );

    // The other per-message calls the list makes.
    let t = Instant::now();
    let mut k = 0usize;
    for (m, d) in msgs.iter().zip(&docs) {
        k += d.embeds(m).len() + d.lone_link(m).map_or(0, |_| 1) + decibell_richtext::jumbo_size(d).map_or(0, |_| 1) as usize;
    }
    println!("embeds + lone link + jumbo: {:.0} ns/message ({k})", t.elapsed().as_nanos() as f64 / n as f64);
    let a0 = ALLOCS.load(Relaxed);
    let t = Instant::now();
    for m in &msgs {
        std::hint::black_box(plain::preview(m));
    }
    println!(
        "preview line: {:.0} ns/message, {:.2} allocations/message",
        t.elapsed().as_nanos() as f64 / n as f64,
        (ALLOCS.load(Relaxed) - a0) as f64 / n as f64
    );
}

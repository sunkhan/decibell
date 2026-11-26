//! The steady state allocates nothing on our side: frames go from the producer through the hub,
//! the queue and FFmpeg's software decoder to a published picture with zero Rust allocations
//! per frame, anywhere in the process.
//!
//! A counting global allocator sees every Rust allocation of every thread: the feeder (a
//! stand-in for the core's receive thread, recycling its buffers as the core's pool does), the
//! hub, the decoder thread. FFmpeg and dav1d allocate through libc's malloc directly and are
//! not counted: per packet FFmpeg makes an `AVBufferRef` for our buffer and one for its own
//! reference to it (docs/video-design.md §5.5). The GPU paths add nothing per frame on our
//! side either (they reuse one command buffer, one fence and five slots), which a headless
//! test cannot show; `tools/streamgen/malloc-count.c` counts them in the real binary.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use decibell_video::VideoHub;
use decibell_video::dbv::{FeedOptions, Feeder};

struct Counting;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn wait_for(mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(30), "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn no_rust_allocation_per_frame_in_the_steady_state() {
    let dir = std::env::var_os("DECIBELL_VIDEO_STREAMS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/streams"));
    for name in ["1080p60_h264.dbv", "1080p60_hevc.dbv", "1080p60_av1.dbv"] {
        let path = dir.join(name);
        if !path.is_file() {
            eprintln!(
                "skipped: {} is missing (tools/streamgen/make-streams.sh)",
                path.display()
            );
            return;
        }
        let hub = VideoHub::headless();
        let stream = hub.stream(name);
        let _viewer = stream.viewer();
        // 200 frames at 60 fps. The first 80 warm everything up: the keyframe is the largest
        // frame, so the feeder's recycled buffers reach their final size at once.
        let feeder = Feeder::start(
            hub.clone(),
            name,
            &path,
            FeedOptions {
                frames: Some(200),
                unpaced: false,
            },
        )
        .unwrap();
        wait_for(|| stream.stats().published >= 80);
        let (frames_before, allocations_before) = (
            stream.stats().published,
            ALLOCATIONS.load(Ordering::Relaxed),
        );
        wait_for(|| stream.stats().published >= 180);
        let (frames, allocations) = (
            stream.stats().published - frames_before,
            ALLOCATIONS.load(Ordering::Relaxed) - allocations_before,
        );
        wait_for(|| feeder.finished());
        let stats = stream.stats();
        println!("{name}: {allocations} Rust allocations over {frames} frames");
        assert_eq!(
            stats.dropped, 0,
            "{name}: frames were shed, the window is not steady"
        );
        assert_eq!(
            allocations, 0,
            "{name}: {allocations} allocations over {frames} frames"
        );
        assert_eq!(decibell_video::packet_copies(), 0);
        assert_eq!(decibell_video::packet_regrows(), 0);
    }
}

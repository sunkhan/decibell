//! The steady state allocates nothing on our side: frames go from the producer through the hub,
//! the queue and FFmpeg's software decoder to a published picture with zero Rust allocations
//! per frame, anywhere in the process.
//!
//! A counting global allocator sees every Rust allocation of every thread: the feeder (a
//! stand-in for the core's receive thread, recycling its buffers as the core's pool does), the
//! hub, the decoder thread. FFmpeg and dav1d allocate through libc's malloc directly and are
//! not counted: per packet FFmpeg makes an `AVBufferRef` for our buffer and one for its own
//! reference to it (docs/video-design.md §5.5). The GPU paths (Vulkan Video, NVDEC, VA-API,
//! the software upload) add nothing per frame on our side either: each reuses one command
//! buffer, one fence and five slots, NVDEC its CUDA imports of those slots and one semaphore,
//! VA-API its imports of the decoder's surfaces. A headless test cannot show that: all of
//! them write into images of the renderer's own Vulkan device, which exists only with a
//! window. `tools/streamgen/malloc-count.c` counts them in the real binary
//! (docs/video-design.md §11.4 has each path's allocations per frame).
//!
//! What can be shown here beyond the steady state: that a decoder failure and the reopen that
//! follows (the path a fallback from one hardware decoder to the next takes through the hub)
//! leave it allocation-free again.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use decibell_video::{DecodeBackend, Fault, VideoHub};
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

/// The counter is the process's: one test at a time.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn wait_for(mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < Duration::from_secs(30), "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn no_rust_allocation_per_frame_in_the_steady_state() {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
        // The decoder thread is gone before the next stream (or the next test) is counted.
        drop(_viewer);
        wait_for(|| hub.decoder_threads() == 0);
    }
}

/// After a decoder failed and the next one was opened (what a fallback is to the hub: gate,
/// keyframe request, a new decoder, the reason kept in the stats), the steady state costs no
/// allocation again, and reading the stats (the reason is shared, not copied) costs none.
#[test]
fn no_rust_allocation_per_frame_after_a_decoder_was_replaced() {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let dir = std::env::var_os("DECIBELL_VIDEO_STREAMS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/streams"));
    let path = dir.join("1080p60_h264.dbv");
    if !path.is_file() {
        eprintln!("skipped: {} is missing", path.display());
        return;
    }
    let hub = VideoHub::headless();
    // The order as a forced one, as `DECIBELL_VIDEO_DECODER=software` sets it.
    hub.set_decoder_order(Some(&[DecodeBackend::Software]));
    let stream = hub.stream("replaced");
    let _viewer = stream.viewer();
    // The file has a keyframe every 240 frames: the decoder that fails at frame 100 is
    // replaced at frame 240.
    let feeder = Feeder::start(
        hub.clone(),
        "replaced",
        &path,
        FeedOptions {
            frames: Some(440),
            unpaced: false,
        },
    )
    .unwrap();
    wait_for(|| stream.stats().published >= 100);
    stream.inject(Fault::DecodeError);
    wait_for(|| stream.stats().decoder_opens == 2);
    let reopened_at = stream.stats().published;
    wait_for(|| stream.stats().published >= reopened_at + 60);
    let (frames_before, allocations_before) = (
        stream.stats().published,
        ALLOCATIONS.load(Ordering::Relaxed),
    );
    wait_for(|| stream.stats().published >= frames_before + 100);
    let (frames, allocations) = (
        stream.stats().published - frames_before,
        ALLOCATIONS.load(Ordering::Relaxed) - allocations_before,
    );
    wait_for(|| feeder.finished());
    let stats = stream.stats();
    println!("after a replaced decoder: {allocations} Rust allocations over {frames} frames");
    assert_eq!((stats.errors, stats.decoder_opens), (1, 2));
    assert_eq!(stats.backend, Some(DecodeBackend::Software));
    assert_eq!(
        allocations, 0,
        "{allocations} allocations over {frames} frames after a decoder was replaced"
    );
    drop(_viewer);
    wait_for(|| hub.decoder_threads() == 0);
}

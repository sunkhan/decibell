//! Steady-state playback allocates nothing on our side: packets from the demuxer through the
//! hub's queue and the software decoder to a published picture, sound from the decoder through
//! the resampler and the ring to the sink, with zero Rust allocations per picture or per audio
//! block, in every thread of the process.
//!
//! FFmpeg allocates through libc's malloc and is not counted (the demuxer makes a buffer per
//! packet; the decoders reuse theirs): the same line `decibell-video/tests/alloc.rs` draws.
//! The GPU paths add nothing per picture by construction (docs/video-design.md §0.3).

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use common::*;
use decibell_media::{Phase, TestOutput};
use decibell_video::VideoHub;

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

/// Plays from `from` seconds and counts allocations over the following `seconds`.
fn steady(player: &decibell_media::Player, from: i64, seconds: u64) -> (u64, u64, i64) {
    wait_for("the warm-up", Duration::from_secs(30), || player.position() >= from * 1_000_000);
    let shown = player.stats().shown;
    let position = player.position();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_secs(seconds));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    (allocations, player.stats().shown - shown, player.position() - position)
}

#[test]
fn a_video_with_sound_plays_without_allocating() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let hub = VideoHub::headless();
    let player = open_bytes(asset("sync60.mp4"), Some(&hub), "alloc", TestOutput::null(RATE), true);
    wait_phase(&player, Phase::Playing, Duration::from_secs(30));
    // Past the first keyframe interval's warm-up (queues, the backlog, the resampler's buffers
    // reach their sizes), then four seconds: 100 pictures, 400 audio blocks, 188 AAC frames.
    let (allocations, pictures, played) = steady(&player, 3, 4);
    println!("video + sound: {allocations} Rust allocations over {pictures} pictures and {:.2} s of sound", played as f64 / 1e6);
    assert!(pictures >= 95, "{pictures} pictures in four seconds");
    assert_eq!(allocations, 0);
}

#[test]
fn sound_alone_plays_without_allocating() {
    let _guard = ONE_AT_A_TIME.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let player = open_bytes(asset("sync60.mp4"), None, "alloc-audio", TestOutput::null(RATE), true);
    wait_phase(&player, Phase::Playing, Duration::from_secs(30));
    let (allocations, _, played) = steady(&player, 2, 4);
    println!("sound alone: {allocations} Rust allocations over {:.2} s", played as f64 / 1e6);
    assert!(played > 3_800_000);
    assert_eq!(allocations, 0);
}

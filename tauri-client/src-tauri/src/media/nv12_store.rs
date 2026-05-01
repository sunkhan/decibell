//! Per-stream latest-frame slot for the Linux watch pipeline.
//!
//! Linux can't decode video in WebKitGTK (no WebCodecs), so the Rust side
//! decodes via ffmpeg+NVDEC/VAAPI/SW and parks the freshest NV12 frame
//! here. The renderer (`LinuxStreamVideoPlayer.tsx`) pulls one frame per
//! `requestAnimationFrame` tick via the `pull_video_frame_yuv` Tauri
//! command — display-locked, no event-spam, no buffering past one frame.
//!
//! Why a global singleton, not AppState:
//!   * Only one set of streams exists per app instance — there is no
//!     per-channel or per-engine multiplicity to model.
//!   * Plumbing it through `VoiceEngine::start → run_video_recv_thread`
//!     would cost three signature changes for zero benefit.
//!   * The Tauri command needs lock-free access; an Arc inside a Mutex'd
//!     AppState would force every frame pull to take the AppState lock,
//!     which is the very thing the AppState-locking memory warns against.
//!
//! Why latest-only (not a queue):
//!   * Display refresh paces consumption. Buffering past one frame just
//!     adds latency and memory for no perceptual gain.
//!   * Frames the renderer skips are still decoded so P-frame references
//!     stay correct — the drop happens *after* decode, before render.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use crate::media::video_decoder::Nv12Frame;

/// Per-streamer slot: one latest frame, replaced on every decode.
type Slot = Arc<RwLock<Option<Nv12Frame>>>;

/// Singleton frame store. Initialised lazily on first publish/pull.
static STORE: OnceLock<RwLock<HashMap<String, Slot>>> = OnceLock::new();

fn store() -> &'static RwLock<HashMap<String, Slot>> {
    STORE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Get-or-create the slot for a streamer, then write the frame in.
/// Drops the previous frame (latest-only). Cheap on the hot path —
/// the read lock is taken only when the slot already exists; the write
/// lock to insert a new slot is one-time per streamer.
pub fn publish(streamer_username: &str, frame: Nv12Frame) {
    let slot = {
        if let Some(s) = store().read().ok().and_then(|g| g.get(streamer_username).cloned()) {
            s
        } else {
            let mut g = match store().write() {
                Ok(g) => g,
                Err(_) => return, // poisoned — drop the frame, store is dead
            };
            g.entry(streamer_username.to_string())
                .or_insert_with(|| Arc::new(RwLock::new(None)))
                .clone()
        }
    };
    if let Ok(mut w) = slot.write() {
        *w = Some(frame);
    }
}

/// Snapshot the latest frame for a streamer. Returns `None` when there's
/// no slot yet (no decode has happened) or when the latest sequence is
/// `<= last_seen_sequence` (renderer already drew it).
///
/// We take a clone here rather than handing back an Arc<RwLock> guard so
/// the IPC command can format the response without holding the slot
/// lock across an await. The clone is one Vec dup per pull — the cost is
/// dominated by the IPC copy that follows, not this.
pub fn pull_if_newer(streamer_username: &str, last_seen_sequence: u64) -> Option<Nv12Frame> {
    let slot = store().read().ok()?.get(streamer_username).cloned()?;
    let g = slot.read().ok()?;
    let f = g.as_ref()?;
    if f.sequence <= last_seen_sequence {
        return None;
    }
    Some(Nv12Frame {
        width: f.width,
        height: f.height,
        sequence: f.sequence,
        timestamp_us: f.timestamp_us,
        y_plane: f.y_plane.clone(),
        uv_plane: f.uv_plane.clone(),
    })
}

/// Drop a streamer's slot when they stop streaming or the watcher leaves.
/// Frees the per-stream NV12 buffer (~3MB at 1080p) immediately rather
/// than letting it linger until the next publish.
pub fn forget(streamer_username: &str) {
    if let Ok(mut g) = store().write() {
        g.remove(streamer_username);
    }
}

/// Drop every slot — called on voice disconnect. Keeps RAM tidy when the
/// user leaves a channel they were watching multiple streams in.
pub fn clear_all() {
    if let Ok(mut g) = store().write() {
        g.clear();
    }
}

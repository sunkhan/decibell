//! The addon's `decibell_core::events::EventSink`: three ThreadsafeFunctions
//! installed once by `init(opts, bus, stream_bus, stream_thumbnail_bus)`.
//!
//! - `bus`: every named event (a typed `CoreEvent`, turned into its JSON by
//!   `CoreEvent::to_json`), as the `{name, payload}` envelope the
//!   renderer-side `listen(name, handler)` shim reads (Tauri's
//!   `app.emit(name, payload)` shape).
//! - `stream_bus`: encoded video frames as Node Buffers plus a small
//!   metadata bag, bounded (8 in flight) with shedding and a discontinuity
//!   flag on the next frame after a shed one.
//! - `stream_thumbnail_bus`: per-stream JPEG thumbnails as Node Buffers.
//!
//! The core decides *what* to emit (and, for frames, whether anyone on
//! screen wants them); this module only owns the napi transport.

use std::sync::OnceLock;

use decibell_core::events::{CoreEvent, EventSink, StreamFrame, StreamThumbnail};
use decibell_core::media::frame_pool::FrameBuf;
use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::{JsFunction, JsUnknown};

type Bus = ThreadsafeFunction<serde_json::Value, ErrorStrategy::Fatal>;
type StreamBus = ThreadsafeFunction<StreamFrame, ErrorStrategy::Fatal>;
type StreamThumbnailBus = ThreadsafeFunction<StreamThumbnail, ErrorStrategy::Fatal>;

pub struct NapiSink {
    bus: Bus,
    stream_bus: StreamBus,
    stream_thumbnail_bus: StreamThumbnailBus,
}

impl NapiSink {
    /// Turn the three JS callbacks into ThreadsafeFunctions. Sync, on the JS
    /// thread: the JsFunction handles must become TSFNs before any await.
    pub fn new(
        bus: JsFunction,
        stream_bus: JsFunction,
        stream_thumbnail_bus: JsFunction,
    ) -> Result<Self> {
        Ok(Self {
            bus: create_bus(bus)?,
            stream_bus: create_stream_bus(stream_bus)?,
            stream_thumbnail_bus: create_stream_thumbnail_bus(stream_thumbnail_bus)?,
        })
    }
}

impl EventSink for NapiSink {
    /// NonBlocking: if the renderer-side queue saturates the call is
    /// dropped silently. For the named events this is fine; encoded-video
    /// frames bypass this bus entirely.
    fn event(&self, event: CoreEvent) {
        let (name, payload) = event.to_json();
        let mut envelope = serde_json::Map::new();
        envelope.insert("name".to_string(), serde_json::Value::String(name.to_string()));
        envelope.insert("payload".to_string(), payload);
        self.bus.call(
            serde_json::Value::Object(envelope),
            ThreadsafeFunctionCallMode::NonBlocking,
        );
    }

    fn stream_frame(&self, mut frame: StreamFrame) {
        if !reserve_stream_slot() {
            // Queue full (busy/stalled renderer): shed this frame — it is freed
            // here — and flag the streamer so its next queued frame tells the
            // player the reference chain broke.
            mark_discontinuity(&frame.key);
            note_stream_frame_drop();
            return;
        }
        // Unmark before the call; if the call itself fails, re-mark so the
        // flag rides the next frame instead. The name is only cloned on this
        // (rare) path.
        let remark = if take_discontinuity(&frame.key) {
            frame.discontinuity = true;
            Some(frame.key.clone())
        } else {
            None
        };
        let status = self.stream_bus.call(frame, ThreadsafeFunctionCallMode::NonBlocking);
        if status != napi::Status::Ok {
            // Only reachable while the TSFN is closing (shutdown) — the slot
            // reservation above keeps the napi queue from ever filling.
            release_stream_slot();
            if let Some(name) = remark {
                mark_discontinuity(&name);
            }
            note_stream_frame_drop();
        }
    }

    fn stream_thumbnail(&self, thumb: StreamThumbnail) {
        self.stream_thumbnail_bus
            .call(thumb, ThreadsafeFunctionCallMode::NonBlocking);
    }

    fn stream_frame_sink_removed(&self, key: &str) {
        let _ = take_discontinuity(key);
    }
}

/// Convert the JS-side bus callback into a TSFN. The JS side passes a
/// function `(env) => void` that broadcasts to every BrowserWindow.
fn create_bus(callback: JsFunction) -> Result<Bus> {
    callback.create_threadsafe_function(
        0,
        |ctx: ThreadSafeCallContext<serde_json::Value>| -> Result<Vec<JsUnknown>> {
            // serde-json feature on napi gives us Env::to_js_value for
            // any Serialize. Turns the envelope into a real JS object
            // instead of round-tripping through a JSON string.
            let v: JsUnknown = ctx.env.to_js_value(&ctx.value)?;
            Ok(vec![v])
        },
    )
}

// ── Stream frame bus ──────────────────────────────────────────────
// A dedicated TSFN carries the raw bytes as Node Buffer (zero-copy view
// over a Vec) plus a small metadata bag, so the renderer's WebCodecs
// decoder gets the bitstream directly.

/// Frames handed to the stream TSFN that JS hasn't picked up yet. We
/// reserve a slot here BEFORE calling the TSFN and refuse the call when
/// all are taken, instead of letting napi report QueueFull: napi-rs 2
/// boxes the value with `Box::into_raw` before `napi_call_threadsafe_function`
/// and never reclaims it on failure, so every QueueFull drop leaked the
/// whole encoded frame. With the cap mirrored here the napi queue can't
/// overflow and a drop is ours to account for (the frame is freed).
const STREAM_QUEUE_CAP: usize = 8;
static STREAM_IN_FLIGHT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn reserve_stream_slot() -> bool {
    use std::sync::atomic::Ordering;
    let mut n = STREAM_IN_FLIGHT.load(Ordering::Acquire);
    loop {
        if n >= STREAM_QUEUE_CAP {
            return false;
        }
        match STREAM_IN_FLIGHT.compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(cur) => n = cur,
        }
    }
}

fn release_stream_slot() {
    use std::sync::atomic::Ordering;
    // Saturating: a slot is only ever released once per reservation, but
    // never let a logic slip wrap the counter and wedge every later frame.
    let mut n = STREAM_IN_FLIGHT.load(Ordering::Acquire);
    while n > 0 {
        match STREAM_IN_FLIGHT.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return,
            Err(cur) => n = cur,
        }
    }
}

/// The JS callback receives a single object:
/// `{ key, codec, keyframe, timestamp, data: Uint8Array,
///    description: Uint8Array | null, discontinuity: boolean }`.
fn create_stream_bus(callback: JsFunction) -> Result<StreamBus> {
    // Bounded queue. With the previous unbounded queue (0), NonBlocking
    // could never report saturation, and a busy renderer buffered encoded
    // frames in main-process memory without limit, turning a stall into
    // permanent added stream latency. 8 frames ≈ 130ms at 60fps: enough to
    // ride out a GC pause, small enough that a real stall sheds frames and
    // stays live. A shed frame breaks the decoder's reference chain, so the
    // next frame queued for that streamer carries `discontinuity: true`
    // (see NapiSink::stream_frame) — the player then gates deltas on a
    // keyframe and requests one instead of smearing until the next natural IDR.
    callback.create_threadsafe_function(
        STREAM_QUEUE_CAP,
        |ctx: ThreadSafeCallContext<StreamFrame>| -> Result<Vec<JsUnknown>> {
            // Popped off the napi queue — free the slot first so an error
            // below can't strand it.
            release_stream_slot();
            let env = &ctx.env;
            let mut obj = env.create_object()?;
            obj.set_named_property("key", env.create_string(&ctx.value.key)?)?;
            obj.set_named_property("codec", env.create_uint32(ctx.value.codec as u32)?)?;
            obj.set_named_property("keyframe", env.get_boolean(ctx.value.keyframe)?)?;
            obj.set_named_property("timestamp", env.create_int64(ctx.value.timestamp)?)?;
            obj.set_named_property(
                "discontinuity",
                env.get_boolean(ctx.value.discontinuity)?,
            )?;
            // Buffer::with_data hands V8 the Vec's backing allocation
            // directly — no copy (see frame_bytes).
            let data_buf = env.create_buffer_with_data(frame_bytes(ctx.value.data))?;
            obj.set_named_property("data", data_buf.into_raw())?;
            match ctx.value.description {
                Some(d) => {
                    let desc_buf = env.create_buffer_with_data(d)?;
                    obj.set_named_property("description", desc_buf.into_raw())?;
                }
                None => {
                    obj.set_named_property("description", env.get_null()?)?;
                }
            }
            Ok(vec![obj.into_unknown()])
        },
    )
}

/// A frame's bytes as the `Vec` V8 adopts. The addon never enables the core's
/// frame pool, so `data` is a plain `Vec` (the reassembly buffer, the decrypt's
/// output, or the encoder's buffer for the self-preview) and `into_vec` hands
/// that very allocation on, as before `FrameBuf` existed.
fn frame_bytes(data: FrameBuf) -> Vec<u8> {
    data.into_vec()
}

/// Frames discarded because the JS-side TSFN queue was saturated (a
/// busy/stalled renderer). Voice counts its drops; without this, video
/// drops were invisible and "stream stutters" had no diagnosable trail.
static STREAM_FRAME_DROPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Not yet surfaced in the stats event — kept for the wiring when
/// voice_connection_stats grows a video section.
#[allow(dead_code)]
pub fn stream_frame_drop_count() -> u64 {
    STREAM_FRAME_DROPS.load(std::sync::atomic::Ordering::Relaxed)
}

fn note_stream_frame_drop() {
    let n = STREAM_FRAME_DROPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    // First drop and every 100th after — visible in logs without
    // becoming its own hot path during a sustained renderer stall.
    if n == 1 || n % 100 == 0 {
        log::warn!("stream frame bus saturated — {} frames dropped so far", n);
    }
}

// ── Discontinuity marks ──
// Usernames that lost a frame to the bounded queue and whose next queued
// frame must carry `discontinuity: true`. The AtomicBool is a lock-free
// "set is non-empty" fast path so the common case never touches the mutex.
// Per streamer key there is a single producer thread (the voice event bridge
// for remote streams, the encoder thread for our own), so mark → take for
// one name always happens in program order on one thread.
static DISCONTINUITY_ANY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn discontinuity_set() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static SET: OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SET.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn mark_discontinuity(username: &str) {
    let mut set = discontinuity_set().lock().unwrap_or_else(|e| e.into_inner());
    if !set.contains(username) {
        set.insert(username.to_string());
    }
    DISCONTINUITY_ANY.store(true, std::sync::atomic::Ordering::Release);
}

/// Remove `username`'s mark, returning whether it was marked.
fn take_discontinuity(username: &str) -> bool {
    if !DISCONTINUITY_ANY.load(std::sync::atomic::Ordering::Acquire) {
        return false;
    }
    let mut set = discontinuity_set().lock().unwrap_or_else(|e| e.into_inner());
    let was = set.remove(username);
    if set.is_empty() {
        DISCONTINUITY_ANY.store(false, std::sync::atomic::Ordering::Release);
    }
    was
}

// ── Stream thumbnail bus ──────────────────────────────────────────
// Per-stream JPEG thumbnails ship the raw bytes as a Node Buffer so the
// renderer can wrap them in a blob: URL with no transcoding.

fn create_stream_thumbnail_bus(callback: JsFunction) -> Result<StreamThumbnailBus> {
    callback.create_threadsafe_function(
        0,
        |ctx: ThreadSafeCallContext<StreamThumbnail>| -> Result<Vec<JsUnknown>> {
            let env = &ctx.env;
            let mut obj = env.create_object()?;
            obj.set_named_property(
                "ownerUsername",
                env.create_string(&ctx.value.owner_username)?,
            )?;
            // f64: a uid fits exactly, and the renderer gets a number, not
            // a BigInt.
            obj.set_named_property("ownerUid", env.create_double(ctx.value.owner_uid as f64)?)?;
            // Buffer::with_data hands V8 the Vec's backing allocation
            // directly — no copy on the way out.
            let data_buf = env.create_buffer_with_data(ctx.value.data)?;
            obj.set_named_property("data", data_buf.into_raw())?;
            Ok(vec![obj.into_unknown()])
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use decibell_core::media::frame_pool;
    use decibell_core::media::video_packet::UdpVideoPacket;
    use decibell_core::media::video_receiver::VideoReceiver;

    /// No extra copy on the way to V8: what the core delivers is handed on as
    /// the same allocation (the pool stays off in the addon).
    #[test]
    fn frames_reach_v8_as_the_allocation_the_core_filled() {
        assert!(!frame_pool::global().is_enabled());
        // A frame reassembled by the core's receive path, as the sink gets it.
        let mut rx = VideoReceiver::new();
        let mut frames = Vec::new();
        for i in 0..3u16 {
            let chunk = vec![i as u8 + 1; 1200];
            frames.extend(rx.process_packet(&UdpVideoPacket::new("42", 0, i, 3, true, &chunk)));
        }
        assert_eq!(frames.len(), 1);
        let data = frames.pop().unwrap().data;
        assert!(!data.is_pooled());
        let (ptr, len) = (data.as_ptr(), data.len());
        let v = frame_bytes(data);
        assert_eq!((v.as_ptr(), v.len()), (ptr, len));

        // The Linux self-preview's encoder buffer, moved in.
        let encoded = vec![9u8; 4096];
        let ptr = encoded.as_ptr();
        let v = frame_bytes(FrameBuf::from(encoded));
        assert_eq!(v.as_ptr(), ptr);
    }
}

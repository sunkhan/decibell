//! Video send pipeline (PR8 — minimal, post-FFmpeg-removal).
//!
//! The renderer's `WebCodecs.VideoEncoder` produces encoded chunks and
//! ships them to native via the `send_video_frame` command. This module
//! owns the per-stream send-side state (frame id counter) and
//! packetises chunks onto the media UDP socket using
//! `video_packet::UdpVideoPacket`. No FEC and no NACK on the send side
//! yet — those land in a follow-up if loss-resilience becomes a problem
//! in practice; receiver-side FEC + NACK request still works for the
//! incoming path (see `video_receiver.rs`).

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use super::media_socket::MediaSocket;
use super::video_packet::{UdpVideoPacket, PACKET_TYPE_VIDEO, UDP_MAX_PAYLOAD};

/// Per-stream send context. One of these per active outgoing stream.
/// Cheap to construct — just an atomic counter and a clone of the UDP
/// socket handle. The socket lives in an `ArcSwap` so it can be
/// hot-swapped when the stream follows the user into a new voice channel
/// (which spins up a fresh VoiceEngine with a new media socket) without
/// tearing down the capture/encoder pipeline.
pub struct VideoSender {
    socket: ArcSwap<MediaSocket>,
    sender_id: String,
    next_frame_id: AtomicU32,
    /// Community channel: the MLS epoch keys; each frame is sealed once
    /// before chunking (`frame_crypto::seal_video_frame`) and sent as
    /// VIDEO_SEALED. None on the P2P path (the socket itself is sealed).
    ring: Option<super::frame_crypto::SharedKeyRing>,
    /// Random per stream so a restarted frame counter never reuses a
    /// nonce within an epoch.
    stream_salt: u32,
}

impl VideoSender {
    pub fn new(
        socket: Arc<MediaSocket>,
        sender_id: String,
        ring: Option<super::frame_crypto::SharedKeyRing>,
    ) -> Self {
        Self {
            socket: ArcSwap::from(socket),
            sender_id,
            next_frame_id: AtomicU32::new(0),
            ring,
            stream_salt: super::frame_crypto::random_salt(),
        }
    }

    /// Re-point the sender at a new media socket. Used when the stream is
    /// carried into a new voice channel — the frame id counter keeps
    /// advancing so the receiver treats it as the same continuous stream.
    pub fn set_socket(&self, socket: Arc<MediaSocket>) {
        self.socket.store(socket);
    }

    /// Packetise an encoded frame and emit it onto the media socket.
    /// Returns `(packets_ok, packets_err)`.
    pub fn send_frame(
        &self,
        codec_byte: u8,
        is_keyframe: bool,
        data: &[u8],
    ) -> (u32, u32) {
        let frame_id = self.next_frame_id.fetch_add(1, Ordering::Relaxed);
        // Encrypted channel: seal the whole frame first. No epoch keys yet
        // (or quarantined) → the frame is dropped, never sent in the clear.
        let sealed;
        let (data, packet_type): (&[u8], u8) = match &self.ring {
            Some(r) => {
                match super::frame_crypto::seal_video_frame(
                    &r.load(),
                    self.stream_salt,
                    frame_id,
                    is_keyframe,
                    codec_byte,
                    data,
                ) {
                    Some(s) => {
                        sealed = s;
                        (&sealed, super::frame_crypto::PACKET_TYPE_VIDEO_SEALED)
                    }
                    None => return (0, 1),
                }
            }
            None => (data, PACKET_TYPE_VIDEO),
        };
        let chunks: Vec<&[u8]> = data.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;
        let mut ok = 0u32;
        let mut err = 0u32;
        // Snapshot the current socket once per frame (cheap Arc load).
        let socket = self.socket.load();
        for (i, chunk) in chunks.iter().enumerate() {
            let mut pkt = UdpVideoPacket::new_with_codec(
                &self.sender_id,
                frame_id,
                i as u16,
                total,
                is_keyframe,
                codec_byte,
                chunk,
            );
            pkt.packet_type = packet_type;
            match socket.send(&pkt.to_bytes()) {
                Ok(_) => ok += 1,
                Err(_) => err += 1,
            }
        }
        (ok, err)
    }
}

/// Hot-path frame sink. The renderer's WebCodecs.VideoEncoder pumps
/// encoded chunks at 60–120 fps via the `send_video_frame` napi
/// command; that command used to grab the global AppState mutex on
/// every frame just to look up `s.video_engine`, which serialised the
/// encoder hot path against every other tokio task touching state.
///
/// Instead we cache the active sender in this static slot at
/// `start_screen_share` time; `send_video_frame` reads it via a
/// dedicated short-held `Mutex` (uncontended in practice — only the
/// start/stop commands ever write) and skips AppState entirely.
///
/// `send_video_frame` runs on the Electron MAIN thread (sync napi fn
/// behind ipcMain.handle), so it no longer seals + packetises + sendto()s
/// there: each sink owns a `decibell-video-send` thread fed by a bounded
/// queue. The napi fn only copies the bytes and enqueues; the single
/// consumer keeps frame order. Native encoder threads still call
/// `VideoSender::send_frame` directly on their own threads.
struct FrameSink {
    sender: Arc<VideoSender>,
    /// None only if the sender thread failed to spawn — frames are then
    /// sent synchronously (the pre-queue behaviour).
    tx: Option<SyncSender<QueuedFrame>>,
    /// Shed bookkeeping. Only `send_video_frame` (one producer, the main
    /// thread) touches it, so the lock is uncontended.
    shed: Mutex<ShedState>,
}

/// One renderer-encoded frame on its way to the sender thread. `data` is
/// already wire-shaped (HEVC/AV1 keyframe description prefix applied).
struct QueuedFrame {
    codec: u8,
    keyframe: bool,
    data: Vec<u8>,
}

#[derive(Default)]
struct ShedState {
    /// A frame was shed: every later delta references it, so deltas are
    /// dropped (never sent broken) until a keyframe makes it into the queue.
    awaiting_keyframe: bool,
    last_keyframe_request: Option<Instant>,
}

/// ~130 ms at 60 fps. The sender thread only falls this far behind when
/// the socket stalls; shedding then keeps the stream live instead of
/// queueing latency (and memory) without bound.
const SEND_QUEUE_DEPTH: usize = 8;
/// While shedding, re-ask the renderer encoder for a keyframe at most this
/// often (the first request, or its answer, may itself be shed).
const KEYFRAME_RETRY: Duration = Duration::from_millis(500);

fn frame_sink_slot() -> &'static Mutex<Option<Arc<FrameSink>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<FrameSink>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Spawn the sender thread for one sink. It exits on its own once the
/// sink (the only `SyncSender`) is dropped and the queue is drained —
/// never joined, so dropping the last sink Arc can't block whichever
/// thread happens to hold it (possibly the Electron main thread).
fn spawn_send_thread(sender: Arc<VideoSender>) -> Option<SyncSender<QueuedFrame>> {
    let (tx, rx) = sync_channel::<QueuedFrame>(SEND_QUEUE_DEPTH);
    let spawned = std::thread::Builder::new()
        .name("decibell-video-send".to_string())
        .spawn(move || {
            while let Ok(f) = rx.recv() {
                sender.send_frame(f.codec, f.keyframe, &f.data);
            }
        });
    match spawned {
        Ok(_) => Some(tx),
        Err(e) => {
            log::warn!("[video-send] sender thread spawn failed ({e}); sending inline");
            None
        }
    }
}

/// Install the active sender into the slot. Called from
/// `start_screen_share` after constructing the engine.
pub fn set_frame_sink(sender: Arc<VideoSender>) {
    let tx = spawn_send_thread(sender.clone());
    let sink = Arc::new(FrameSink {
        sender,
        tx,
        shed: Mutex::new(ShedState::default()),
    });
    *frame_sink_slot().lock().expect("frame sink mutex poisoned") = Some(sink);
}

/// Clear the slot — but only if it still holds `sender`. Engine teardown
/// runs on a blocking thread *after* `stop_screen_share` has already
/// returned, so a new stream (next call, or stop→start in the same one)
/// can install its own sender before the old engine's Drop gets here; an
/// unconditional clear then wiped the live sink and every frame the
/// renderer pumped afterwards vanished — the watcher sat on "loading"
/// forever. Scoping the clear to the owning sender makes drop order
/// irrelevant.
pub fn clear_frame_sink_if(sender: &Arc<VideoSender>) -> bool {
    let mut slot = frame_sink_slot().lock().expect("frame sink mutex poisoned");
    match slot.as_ref() {
        Some(cur) if Arc::ptr_eq(&cur.sender, sender) => {
            *slot = None;
            true
        }
        _ => false,
    }
}

/// Read the active sender (Arc clone is ~atomic refcount bump).
#[cfg(test)]
pub fn current_frame_sink() -> Option<Arc<VideoSender>> {
    frame_sink_slot()
        .lock()
        .expect("frame sink mutex poisoned")
        .as_ref()
        .map(|s| s.sender.clone())
}

/// Outcome of `submit_renderer_frame`.
#[derive(Debug, PartialEq, Eq)]
pub enum Submit {
    /// Handed to the sender thread (or sent inline as a fallback).
    Queued,
    /// No active stream — a benign post-stop race, dropped.
    NoSink,
    /// Shed: the queue was full, or a delta arrived while waiting for a
    /// keyframe after an earlier shed.
    Shed,
}

/// Hand one renderer-encoded frame to the active sink's sender thread.
/// `build` produces the wire bytes (it copies out of the JS buffer) and
/// is only called when the frame is actually going to be queued. Never
/// blocks: a full queue sheds the frame, drops later deltas until a
/// keyframe is queued, and asks the renderer encoder for that keyframe
/// via the existing `keyframe_requested` event (StreamCapture.forceKeyframe).
pub fn submit_renderer_frame(
    codec: u8,
    keyframe: bool,
    build: impl FnOnce() -> Vec<u8>,
) -> Submit {
    let Some(sink) = frame_sink_slot()
        .lock()
        .expect("frame sink mutex poisoned")
        .clone()
    else {
        return Submit::NoSink;
    };
    let Some(tx) = sink.tx.as_ref() else {
        sink.sender.send_frame(codec, keyframe, &build());
        return Submit::Queued;
    };

    let mut shed = sink.shed.lock().unwrap_or_else(|e| e.into_inner());
    if shed.awaiting_keyframe && !keyframe {
        maybe_request_keyframe(&mut shed);
        return Submit::Shed;
    }
    match tx.try_send(QueuedFrame {
        codec,
        keyframe,
        data: build(),
    }) {
        Ok(()) => {
            if keyframe {
                shed.awaiting_keyframe = false;
            }
            Submit::Queued
        }
        Err(TrySendError::Full(_)) => {
            if !shed.awaiting_keyframe {
                log::warn!("[video-send] send queue full — shedding until the next keyframe");
                shed.awaiting_keyframe = true;
                shed.last_keyframe_request = None;
            }
            maybe_request_keyframe(&mut shed);
            Submit::Shed
        }
        Err(TrySendError::Disconnected(f)) => {
            // Sender thread gone (it only exits on a panic while the sink
            // is live) — keep the stream going inline.
            sink.sender.send_frame(f.codec, f.keyframe, &f.data);
            Submit::Queued
        }
    }
}

fn maybe_request_keyframe(shed: &mut ShedState) {
    let now = Instant::now();
    if shed
        .last_keyframe_request
        .is_some_and(|t| now.duration_since(t) < KEYFRAME_RETRY)
    {
        return;
    }
    shed.last_keyframe_request = Some(now);
    // Same event a watcher's PLI raises; useVoiceEvents routes it to
    // StreamCapture.forceKeyframe() on this (renderer-encode) path. Gated
    // out of unit tests: the napi TSFN symbols only resolve inside Node.
    #[cfg(not(test))]
    crate::events::send("keyframe_requested", serde_json::Value::Null);
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::media_socket::MediaSocket;
    use std::net::UdpSocket;

    fn sender() -> Arc<VideoSender> {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(sock.local_addr().unwrap()).unwrap();
        Arc::new(VideoSender::new(Arc::new(MediaSocket::plain(sock, "me")), "me".into(), None))
    }

    // The sink slot is process-global and cargo runs tests in parallel.
    static SLOT_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn late_teardown_never_clears_a_newer_sink() {
        let _g = SLOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old = sender();
        let new = sender();
        set_frame_sink(old.clone());
        // The next stream installs its sender before the old engine's Drop runs.
        set_frame_sink(new.clone());
        assert!(!clear_frame_sink_if(&old), "old engine must not clear the live sink");
        assert!(Arc::ptr_eq(&current_frame_sink().unwrap(), &new));
        // The owning engine clears its own sink.
        assert!(clear_frame_sink_if(&new));
        assert!(current_frame_sink().is_none());
    }

    #[test]
    fn renderer_frames_go_out_on_the_sender_thread() {
        let _g = SLOT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(rx.local_addr().unwrap()).unwrap();
        let s = Arc::new(VideoSender::new(
            Arc::new(MediaSocket::plain(sock, "me")),
            "me".into(),
            None,
        ));
        set_frame_sink(s.clone());
        assert_eq!(submit_renderer_frame(1, true, || vec![7u8; 100]), Submit::Queued);
        let mut buf = vec![0u8; 65536];
        let n = rx.recv(&mut buf).expect("frame never left the sender thread");
        assert!(n > 0);
        assert_eq!(buf[0], PACKET_TYPE_VIDEO);
        assert!(clear_frame_sink_if(&s));
        // A post-stop frame is a benign no-op and is never even built.
        assert_eq!(
            submit_renderer_frame(1, false, || panic!("built a frame with no sink")),
            Submit::NoSink
        );
    }
}

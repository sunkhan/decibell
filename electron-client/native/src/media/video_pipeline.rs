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

use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::{AtomicU32, Ordering};

use arc_swap::ArcSwap;

use super::media_socket::MediaSocket;
use super::video_packet::{UdpVideoPacket, UDP_MAX_PAYLOAD};

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
}

impl VideoSender {
    pub fn new(socket: Arc<MediaSocket>, sender_id: String) -> Self {
        Self {
            socket: ArcSwap::from(socket),
            sender_id,
            next_frame_id: AtomicU32::new(0),
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
        let chunks: Vec<&[u8]> = data.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;
        let mut ok = 0u32;
        let mut err = 0u32;
        // Snapshot the current socket once per frame (cheap Arc load).
        let socket = self.socket.load();
        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = UdpVideoPacket::new_with_codec(
                &self.sender_id,
                frame_id,
                i as u16,
                total,
                is_keyframe,
                codec_byte,
                chunk,
            );
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
/// Instead we cache an `Arc<VideoSender>` in this static slot at
/// `start_screen_share` time; `send_video_frame` reads it via a
/// dedicated short-held `Mutex` (uncontended in practice — only the
/// start/stop commands ever write) and skips AppState entirely.
fn frame_sink_slot() -> &'static Mutex<Option<Arc<VideoSender>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<VideoSender>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Install the active sender into the slot. Called from
/// `start_screen_share` after constructing the engine.
pub fn set_frame_sink(sender: Arc<VideoSender>) {
    *frame_sink_slot().lock().expect("frame sink mutex poisoned") = Some(sender);
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
        Some(cur) if Arc::ptr_eq(cur, sender) => {
            *slot = None;
            true
        }
        _ => false,
    }
}

/// Read the active sender (Arc clone is ~atomic refcount bump). The
/// caller releases the slot mutex immediately and does the send work
/// without any lock held.
pub fn current_frame_sink() -> Option<Arc<VideoSender>> {
    frame_sink_slot()
        .lock()
        .expect("frame sink mutex poisoned")
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::media_socket::MediaSocket;
    use std::net::UdpSocket;

    fn sender() -> Arc<VideoSender> {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(sock.local_addr().unwrap()).unwrap();
        Arc::new(VideoSender::new(Arc::new(MediaSocket::plain(sock, "me")), "me".into()))
    }

    #[test]
    fn late_teardown_never_clears_a_newer_sink() {
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
}

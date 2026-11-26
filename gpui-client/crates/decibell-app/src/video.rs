//! The video crate wired to the model and the core (docs/model.md M3.2, docs/video-design.md §0):
//!
//! - the core's received frames (its receive thread, or its encoder for our own preview) go
//!   straight into the hub (`StreamFrameSink` → `VideoHub::push_frame`), never through the UI
//!   thread;
//! - the hub asks the core for keyframes (`request_stream_keyframe` for a watched stream,
//!   `force_keyframe` for our own preview) and switches the core's per-stream frame sink as the
//!   first viewer of a stream appears and the last goes (`set_stream_frame_sink`), so the hub
//!   receives what is displayed and nothing else;
//! - what the hub says about a stream (first picture, fallback to software, errors) goes to the
//!   voice feature (`decibell_voice::video_event`);
//! - the decode capabilities: the hub's probe (the driver's answers, no FFmpeg context) is the
//!   codec store's decoder probe, and once the core has booted they are shipped to the core so
//!   the first voice join advertises them. The native *encoder* probe is not run here: it opens
//!   encoder sessions, so the voice feature runs it when Go Live first opens or the first voice
//!   session starts (`decibell_voice::codecs::ensure_caps`).

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use decibell_core::api::streaming;
use decibell_core::events::StreamFrame;
use decibell_model::live::voice::codecs::{CodecCap, DecoderProbe, VideoCodec, decode_ceiling, with_h264_fallback};
use decibell_model::{Model, core};
use decibell_video::{Codec, DecodePath, HubCallbacks, VideoHub};
use futures::future::LocalBoxFuture;
use gpui::App;

/// Our uid, for the keyframe route (our own preview's frames come from our encoder).
static OWN_UID: AtomicI64 = AtomicI64::new(0);

struct HubSink(VideoHub);

impl decibell_model::pump::StreamFrameSink for HubSink {
    fn frame(&self, frame: StreamFrame) {
        self.0.push_frame(frame);
    }

    fn removed(&self, key: &str) {
        self.0.sink_removed(key);
    }
}

/// The codec store's decoder probe: the hub's per-codec answer.
struct HubProbe(VideoHub);

pub fn decode_caps(hub: &VideoHub) -> Vec<CodecCap> {
    hub.capabilities()
        .into_iter()
        .filter_map(|support| {
            let path = support.path?;
            let codec = match support.codec {
                Codec::H264 => VideoCodec::H264Hw,
                Codec::Hevc => VideoCodec::H265,
                Codec::Av1 => VideoCodec::Av1,
            };
            let (w, h, fps) = decode_ceiling(codec);
            let (max_width, max_height) = support.max_size.map(|(mw, mh)| (mw.min(w), mh.min(h))).unwrap_or((w, h));
            Some(CodecCap { codec, max_width, max_height, max_fps: fps, hardware: path == DecodePath::Hardware })
        })
        .collect()
}

impl DecoderProbe for HubProbe {
    fn probe(&self) -> LocalBoxFuture<'static, Vec<CodecCap>> {
        let caps = decode_caps(&self.0);
        Box::pin(async move { caps })
    }
}

fn request_keyframe(key: &str) {
    let Ok(uid) = key.parse::<i64>() else { return };
    if !core::is_running() {
        return;
    }
    let own = OWN_UID.load(Ordering::Relaxed);
    if own > 0 && uid == own {
        core::spawn_detached(async {
            let _ = streaming::force_keyframe().await;
        });
    } else {
        core::spawn_detached(async move {
            let _ = streaming::request_stream_keyframe(streaming::RequestStreamKeyframeArgs { uid }).await;
        });
    }
}

fn interest(key: &str, on: bool) {
    if core::is_running() {
        streaming::set_stream_frame_sink(streaming::SetStreamFrameSinkArgs { key: key.to_string(), enabled: on });
    }
}

/// Install the glue (with the core: the app; not for screens, which have no core).
pub fn install(model: &Model, cx: &mut App) {
    let hub = VideoHub::global(cx);
    model.sink().set_stream_frame_sink(Some(Box::new(HubSink(hub.clone()))));
    hub.set_callbacks(HubCallbacks {
        request_keyframe: Box::new(request_keyframe),
        interest: Box::new(interest),
        event: Box::new(decibell_voice::video_event),
    });
    let probe = Rc::new(HubProbe(hub.clone()));
    model.live.voice.codecs.update(cx, |c, _| c.set_decoder_probe(probe));
    // Our uid for the keyframe route, and the decode caps once the core has booted (so the first
    // voice join advertises them; cheap: the driver's answers, no FFmpeg context, no encoder).
    let sent = Rc::new(AtomicBool::new(false));
    cx.observe(&model.auth, move |auth, cx| {
        let auth = auth.read(cx);
        OWN_UID.store(auth.uid, Ordering::Relaxed);
        if auth.booted && core::is_running() && !sent.swap(true, Ordering::Relaxed) {
            let caps = with_h264_fallback(decode_caps(&hub));
            let values: Vec<streaming::CodecCapValue> = caps
                .iter()
                .map(|c| streaming::CodecCapValue { codec: c.codec.wire(), max_width: c.max_width, max_height: c.max_height, max_fps: c.max_fps })
                .collect();
            core::spawn_detached(async move {
                let _ = streaming::set_decoder_caps(streaming::SetDecoderCapsArgs { decoder_caps: values }).await;
            });
        }
    })
    .detach();
}

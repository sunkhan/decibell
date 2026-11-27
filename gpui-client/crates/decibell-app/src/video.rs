//! The video crate wired to the model and the core (docs/model.md M3.2, docs/video-design.md §0):
//!
//! - the core's received frames (its receive thread, or its encoder for our own preview) go
//!   straight into the hub (`StreamFrameSink` → `VideoHub::push_frame`), never through the UI
//!   thread;
//! - the hub asks the core for keyframes (`request_stream_keyframe` for a watched stream,
//!   `force_keyframe` for our own preview) and switches the core's per-stream frame sink as the
//!   first viewer of a stream appears and the last goes (`set_stream_frame_sink`), so the hub
//!   receives what is displayed and nothing else;
//! - what the hub says about a stream (first picture, fallback to software, errors, a stall) goes
//!   to the voice feature (`decibell_voice::video_event`);
//! - **the decode capabilities are the hub's probe** (docs/video-design.md §0.5): each decoder
//!   of the machine's order decodes a test frame and the first that returns the right picture is
//!   what the codec's streams use and what the client advertises. It runs on a thread of its own
//!   once the window's first frame is on screen (never on the way to it), is kept per GPU and
//!   driver in `<user data>/video-decoders.txt`, and is the model's `DecoderProbe`: the model
//!   ships its answer to the core, Settings → Codecs lists it, "Refresh" runs it again from
//!   nothing, and a replaced GPU device or a decoder given up mid-stream asks again. Until the
//!   first frame the model gets the listed answer (what the driver and the FFmpeg build say,
//!   nothing opened), so a voice join in the first moments still advertises something.
//!   The native *encoder* probe is not run here: it opens encoder sessions, so whoever needs the
//!   caps first asks the model (`actions::voice::init_codec_caps`: the first voice session, Go
//!   Live, Settings → Codecs).

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use decibell_core::api::streaming;
use decibell_core::events::StreamFrame;
use decibell_model::live::voice::codecs::{
    CodecCap, DecodeReport, DecoderInfo, DecoderProbe, SkippedDecoder, VideoCodec, decode_ceiling,
};
use decibell_model::{Model, actions, core};
use decibell_video::{Codec, CodecSupport, DecodePath, HubCallbacks, StreamEvent, VideoHub};
use futures::StreamExt as _;
use futures::future::LocalBoxFuture;
use gpui::{App, Window};

/// Our uid, for the keyframe route (our own preview's frames come from our encoder).
static OWN_UID: AtomicI64 = AtomicI64::new(0);
/// The window's first frame reached the screen: the probe may open decoders from now on.
static FIRST_FRAME: AtomicBool = AtomicBool::new(false);

/// The probe's result between runs, in the user data directory.
const CAPABILITIES_FILE: &str = "video-decoders.txt";

struct HubSink(VideoHub);

impl decibell_model::pump::StreamFrameSink for HubSink {
    fn frame(&self, frame: StreamFrame) {
        self.0.push_frame(frame);
    }

    fn removed(&self, key: &str) {
        self.0.sink_removed(key);
    }
}

fn model_codec(codec: Codec) -> VideoCodec {
    match codec {
        Codec::H264 => VideoCodec::H264Hw,
        Codec::Hevc => VideoCodec::H265,
        Codec::Av1 => VideoCodec::Av1,
        // File-only codecs (attachment playback) are never probed or advertised: the callers
        // walk `Codec::ALL`.
        Codec::Vp8 | Codec::Vp9 => {
            debug_assert!(false, "a file-only codec reached the model");
            VideoCodec::H264Hw
        }
    }
}

/// The hub's answer in the model's terms. A codec is advertised when any decoder of the order
/// decodes it, software included, at the renderer's ceiling (`decoderProbe.ts` advertises a
/// software-only codec the same way, with `hardware: false`); a hardware decoder's own size
/// limit lowers the ceiling.
pub fn decode_report(hub: &VideoHub, support: [CodecSupport; 3]) -> DecodeReport {
    let mut report = DecodeReport { probed: support.iter().all(|s| s.probed), ..Default::default() };
    for s in support {
        let (Some(path), Some(backend)) = (s.path, s.backend) else { continue };
        let codec = model_codec(s.codec);
        let (w, h, fps) = decode_ceiling(codec);
        let (max_width, max_height) = s.max_size.map(|(mw, mh)| (mw.min(w), mh.min(h))).unwrap_or((w, h));
        let hardware = path == DecodePath::Hardware;
        report.caps.push(CodecCap { codec, max_width, max_height, max_fps: fps, hardware });
        report.decoders.push(DecoderInfo { codec, label: backend.display_name(s.codec).into(), hardware, probed: s.probed });
    }
    report.skipped = hub
        .skipped_decoders()
        .into_iter()
        .map(|s| SkippedDecoder {
            codec: model_codec(s.codec),
            label: s.backend.display_name(s.codec).into(),
            reason: s.reason.as_ref().into(),
        })
        .collect();
    report
}

/// The model's decoder probe: the hub's.
struct HubProbe(VideoHub);

impl DecoderProbe for HubProbe {
    fn probe(&self, force: bool) -> LocalBoxFuture<'static, DecodeReport> {
        let hub = self.0.clone();
        Box::pin(async move {
            if !force && !FIRST_FRAME.load(Ordering::Acquire) {
                // On the way to the first frame nothing is opened: the listed answer.
                return decode_report(&hub, hub.capabilities());
            }
            let (tx, rx) = futures::channel::oneshot::channel();
            let done = move |support| {
                let _ = tx.send(support);
            };
            if force {
                hub.reprobe_capabilities(done);
            } else {
                hub.probe_capabilities(done);
            }
            let support = match rx.await {
                Ok(support) => support,
                Err(_) => hub.capabilities(),
            };
            decode_report(&hub, support)
        })
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
    if let Ok(dir) = core::default_user_data_dir() {
        hub.set_capabilities_cache(dir.join(CAPABILITIES_FILE));
    }
    model.sink().set_stream_frame_sink(Some(Box::new(HubSink(hub.clone()))));

    // What the hub says that changes the capabilities: the GPU device was replaced (the first
    // decoder opened after a loss is on the new one), or a decoder was given up. One wake of the
    // UI thread per such event, none otherwise.
    let (again_tx, mut again_rx) = futures::channel::mpsc::unbounded::<()>();
    let lost = Arc::new(AtomicBool::new(false));
    hub.set_callbacks(HubCallbacks {
        request_keyframe: Box::new(request_keyframe),
        interest: Box::new(interest),
        event: Box::new(move |key, event| {
            let again = match &event {
                StreamEvent::DeviceLost => {
                    lost.store(true, Ordering::Release);
                    false
                }
                StreamEvent::DecoderOpened { .. } => lost.swap(false, Ordering::AcqRel),
                StreamEvent::BackendFailed { .. } => true,
                _ => false,
            };
            if again {
                let _ = again_tx.unbounded_send(());
            }
            decibell_voice::video_event(key, event);
        }),
    });
    cx.spawn(async move |cx| {
        while again_rx.next().await.is_some() {
            while again_rx.try_recv().is_ok() {}
            cx.update(actions::voice::load_decode_caps);
        }
    })
    .detach();

    let probe = Rc::new(HubProbe(hub));
    model.live.voice.codecs.update(cx, |c, _| c.set_decoder_probe(probe));
    // Our uid for the keyframe route, and the decode caps once the core has booted (its caps are
    // in memory: every start): the probe's when it has answered, the listed ones until then.
    let sent = Rc::new(AtomicBool::new(false));
    cx.observe(&model.auth, move |auth, cx| {
        let auth = auth.read(cx);
        OWN_UID.store(auth.uid, Ordering::Relaxed);
        if auth.booted && core::is_running() && !sent.swap(true, Ordering::Relaxed) {
            actions::voice::load_decode_caps(cx);
        }
    })
    .detach();
}

/// Tell the hub whether `window` is shown, now and at each change (minimised, suspended by the
/// compositor: `Window::is_visible`): a stream seen only in a hidden window stops decoding and is
/// asked for a keyframe when the window comes back (`StreamVideoPlayer.tsx`'s
/// `visibilitychange`). The stream's sound is the core's and goes on.
pub fn follow_window_visibility(window: &mut Window, cx: &mut App) {
    let hub = VideoHub::global(cx);
    let handle = window.window_handle();
    hub.set_window_visible(handle, window.is_visible());
    window
        .observe_window_visibility(move |visibility, _, _| hub.set_window_visible(handle, visibility.is_visible()))
        .detach();
}

/// Run the capability probe once `window`'s first frame is on screen (the app's main window):
/// it opens real decoders on the renderer's device, which must not compete with the way to the
/// first frame. A next-frame callback after a present is the closest GPUI has to "presented"
/// (as `decibell_harness::perf` uses it).
pub fn probe_after_first_frame(window: &Window) {
    probe_attempt(window, 0);
}

fn probe_attempt(window: &Window, attempt: u32) {
    window.on_next_frame(move |window, cx| {
        if window.frames_presented() == 0 && attempt < 8 {
            probe_attempt(window, attempt + 1);
            return;
        }
        if !FIRST_FRAME.swap(true, Ordering::AcqRel) {
            actions::voice::load_decode_caps(cx);
            crate::crash::note_gpu();
        }
    });
}

#[cfg(test)]
mod tests {
    use decibell_video::DecodeBackend;

    use super::*;

    #[test]
    fn a_codec_is_advertised_when_anything_decodes_it_and_names_its_decoder() {
        let hub = VideoHub::headless();
        let support = |codec, backend: Option<DecodeBackend>, max_size| CodecSupport {
            codec,
            path: backend.map(DecodeBackend::path),
            backend,
            probed: true,
            max_size,
        };
        let report = decode_report(
            &hub,
            [
                support(Codec::H264, Some(DecodeBackend::VulkanVideo), Some((4096, 4096))),
                support(Codec::Hevc, None, None),
                support(Codec::Av1, Some(DecodeBackend::Software), None),
            ],
        );
        assert!(report.probed);
        // Hardware H.264 at the renderer's ceiling (the driver takes more), software AV1 at the
        // same ceiling as the renderer advertises a software-only codec, no H.265 at all.
        assert_eq!(
            report.caps,
            [
                CodecCap { codec: VideoCodec::H264Hw, max_width: 3840, max_height: 2160, max_fps: 120, hardware: true },
                CodecCap { codec: VideoCodec::Av1, max_width: 3840, max_height: 2160, max_fps: 120, hardware: false },
            ]
        );
        let labels: Vec<&str> = report.decoders.iter().map(|d| d.label.as_ref()).collect();
        assert_eq!(labels, ["Vulkan Video", "Software (dav1d)"]);
        // A hardware decoder with a smaller limit lowers the ceiling.
        let small = decode_report(&hub, [support(Codec::Hevc, Some(DecodeBackend::Vaapi), Some((1920, 1088))); 3]);
        assert_eq!((small.caps[0].max_width, small.caps[0].max_height), (1920, 1088));
        assert_eq!(small.decoders[0].label.as_ref(), "VA-API");
    }
}

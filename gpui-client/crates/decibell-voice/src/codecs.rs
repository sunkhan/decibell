//! Encode capabilities for Go Live: when to probe, what the probe found (backend, label, a
//! software encoder's pixel-rate ceiling), and which presets this machine can encode.
//!
//! The native encoder probe opens real encoder sessions, so it never runs at app start: the first
//! time the Go Live dialog opens, or the first voice session starts, [`ensure_caps`] runs the
//! model's `init_codec_caps` (decode caps, the encoder probe off the UI thread, both shipped to
//! the core) and reads the core's cached probe with the details the model drops
//! (`cached_native_encoders`: the backend and its label, `max_pixel_rate`).

use std::sync::atomic::{AtomicBool, Ordering};

use decibell_model::live::voice::VideoCodec;
use decibell_model::live::voice::go_live::{StreamResolution, StreamSettings};
use decibell_model::{Model, actions};
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};

/// One native encoder as the core probed it (`NativeEncoderCap`).
#[derive(Debug, Clone, PartialEq)]
pub struct EncoderCap {
    pub codec: VideoCodec,
    pub hardware: bool,
    /// "nvenc", "amf", "qsv", "vaapi", "software".
    pub backend: SharedString,
    /// "NVENC (NVIDIA)", … "Software (SVT-AV1)".
    pub label: SharedString,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    /// Software only: width × height × fps allowed; 0 = no limit beyond `max_*`.
    pub max_pixel_rate: u32,
}

#[derive(Default)]
pub struct Encoders {
    pub caps: Vec<EncoderCap>,
    pub loaded: bool,
    /// Set by tests and screens: later probes don't replace these caps.
    pinned: bool,
}

struct Handle(Entity<Encoders>);
impl Global for Handle {}

impl Encoders {
    pub fn global(cx: &mut App) -> Entity<Encoders> {
        if let Some(h) = cx.try_global::<Handle>() {
            return h.0.clone();
        }
        let entity = cx.new(|_| Encoders::default());
        cx.set_global(Handle(entity.clone()));
        entity
    }

    pub fn set(&mut self, caps: Vec<EncoderCap>, cx: &mut Context<Self>) {
        if self.pinned {
            return;
        }
        if !self.loaded || self.caps != caps {
            self.caps = caps;
            self.loaded = true;
            cx.notify();
        }
    }

    /// Fix the caps (a test's fake probe answer): later probes don't replace them.
    pub fn pin(&mut self, caps: Vec<EncoderCap>, cx: &mut Context<Self>) {
        self.pinned = false;
        self.set(caps, cx);
        self.pinned = true;
    }

    /// Only software encoders work here (every cap is software): presets above the CPU's ceiling
    /// are greyed out.
    pub fn software_only(&self) -> bool {
        !self.caps.is_empty() && self.caps.iter().all(|c| !c.hardware)
    }

    /// Whether this machine can encode `resolution` at `fps` in any probed codec. "Source" is
    /// judged at the 1440p stand-in the renderer used for its pre-flight check.
    pub fn can_encode(&self, resolution: StreamResolution, fps: u32) -> bool {
        if !self.loaded || self.caps.is_empty() {
            return true;
        }
        let (w, h) = match resolution.request_size() {
            (0, 0) => (2560, 1440),
            size => size,
        };
        self.caps.iter().any(|c| {
            let rate = w as u64 * h as u64 * fps as u64;
            c.max_fps >= fps
                && c.max_width >= w
                && c.max_height >= h
                && (c.hardware || c.max_pixel_rate == 0 || rate <= c.max_pixel_rate as u64)
        })
    }

    /// The encoder a stream of `settings` would start on: the first probed for its codec.
    pub fn planned(&self, settings: &StreamSettings) -> Option<&EncoderCap> {
        let codec = settings.start_codec();
        self.caps.iter().find(|c| c.codec == codec).or_else(|| {
            // Auto falls through to whatever works (the core does the same when not enforced).
            (settings.enforced_codec == VideoCodec::Unknown).then(|| self.caps.first()).flatten()
        })
    }
}

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Probe the codec capabilities once per process, when first needed (see the module doc).
pub fn ensure_caps(cx: &mut App) {
    if REQUESTED.swap(true, Ordering::AcqRel) {
        return;
    }
    if !decibell_model::core::is_running() {
        REQUESTED.store(false, Ordering::Release);
        return;
    }
    actions::voice::init_codec_caps(false, cx);
    load_encoders(cx);
}

/// Read the core's cached probe (the process's result, its disk cache, or a probe now: all off
/// the UI thread) into [`Encoders`].
pub fn load_encoders(cx: &mut App) {
    let encoders = Encoders::global(cx);
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    cx.spawn(async move |cx| {
        let probed = decibell_core::api::streaming::cached_native_encoders().await;
        let caps: Vec<EncoderCap> = match probed {
            Ok(list) => list
                .into_iter()
                .map(|c| EncoderCap {
                    codec: VideoCodec::from_wire(c.codec),
                    hardware: c.hardware,
                    backend: c.backend.into(),
                    label: c.label.into(),
                    max_width: c.max_width,
                    max_height: c.max_height,
                    max_fps: c.max_fps,
                    max_pixel_rate: c.max_pixel_rate,
                })
                .collect(),
            Err(e) => {
                log::warn!("[voice] encoder probe: {e}");
                Vec::new()
            }
        };
        cx.update(|cx| encoders.update(cx, |e, cx| e.set(caps, cx)));
    })
    .detach();
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    encoders.update(cx, |e, cx| e.set(Vec::new(), cx));
}

/// Whether the model's codec store has caps yet (the Advanced codec row lists them).
pub fn model_caps_loaded(cx: &App) -> bool {
    cx.try_global::<Model>().is_some_and(|m| m.live.voice.codecs.read(cx).loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(codec: VideoCodec, hardware: bool, rate: u32) -> EncoderCap {
        EncoderCap {
            codec,
            hardware,
            backend: "x".into(),
            label: "x".into(),
            max_width: 3840,
            max_height: 2160,
            max_fps: if hardware { 120 } else { 60 },
            max_pixel_rate: rate,
        }
    }

    #[test]
    fn software_ceilings_grey_presets_out() {
        let mut e = Encoders::default();
        assert!(e.can_encode(StreamResolution::P1080, 60), "no probe yet: nothing greyed");
        e.caps = vec![cap(VideoCodec::Av1, false, 1920 * 1080 * 30)];
        e.loaded = true;
        assert!(e.software_only());
        assert!(!e.can_encode(StreamResolution::P1080, 60), "Smooth");
        assert!(!e.can_encode(StreamResolution::Source, 30), "Sharp (judged at 1440p)");
        assert!(e.can_encode(StreamResolution::Source, 5), "Slides");
        assert!(e.can_encode(StreamResolution::P720, 30), "Data saver");
        assert!(!e.can_encode(StreamResolution::P720, 120), "past the software encoder's fps");
        e.caps.push(cap(VideoCodec::H264Hw, true, 0));
        assert!(!e.software_only());
        assert!(e.can_encode(StreamResolution::P1080, 120));
    }

    #[test]
    fn the_planned_encoder_follows_the_codec() {
        let e = Encoders { caps: vec![cap(VideoCodec::H264Hw, true, 0), cap(VideoCodec::Av1, true, 0)], loaded: true, pinned: false };
        let mut s = StreamSettings::default();
        assert_eq!(e.planned(&s).map(|c| c.codec), Some(VideoCodec::H264Hw));
        s.enforced_codec = VideoCodec::Av1;
        assert_eq!(e.planned(&s).map(|c| c.codec), Some(VideoCodec::Av1));
        s.enforced_codec = VideoCodec::H265;
        assert_eq!(e.planned(&s), None, "an enforced codec nothing encodes");
    }
}

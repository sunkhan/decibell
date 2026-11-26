//! Codec negotiation on the client side (`utils/codecMap.ts`, `codecToasts.ts`,
//! `canWatchStream.ts`, `decoderProbe.ts`, `encoderProbe.ts`'s native path,
//! `stores/codecSettingsStore.ts`).
//!
//! What this client can **decode** comes from the video crate ([`DecoderProbe`], the app
//! installs it with `CodecCaps::set_decoder_probe`): a real probe (each decoder decodes a test
//! frame), run by the app once its first frame is up and kept by the video crate per GPU and
//! driver, so the model keeps no cache of its own for it. What it can **encode** comes from
//! the core's `probe_native_encoders` (native encode is the only path here: no WebCodecs),
//! with the backend that encodes each codec, its label and its limits ([`EncoderCap`]), cached
//! in `ui.json` (`Layout::extra`, key [`ENCODER_CAPS_KEY`]) like the renderer's
//! `localStorage`. Both are shipped to the core (which advertises them in every
//! `JoinVoiceRequest`) and re-probed by Settings → Codecs' refresh. The advertised encode
//! list keeps only codecs this client can also decode (the streamer's own preview).
//!
//! One action loads them, `actions::voice::init_codec_caps`: off the UI thread, never at
//! start (the encoder probe opens encoder sessions), one load at a time whoever asks.

use std::rc::Rc;

use futures::future::LocalBoxFuture;
use gpui::{Context, SharedString};

use super::streams::StreamInfo;

/// `VideoCodec` (the wire's enum byte).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    #[default]
    Unknown,
    H264Hw,
    H264Sw,
    H265,
    Av1,
}

impl VideoCodec {
    pub fn from_wire(v: i32) -> Self {
        match v {
            1 => Self::H264Hw,
            2 => Self::H264Sw,
            3 => Self::H265,
            4 => Self::Av1,
            _ => Self::Unknown,
        }
    }

    pub fn wire(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::H264Hw => 1,
            Self::H264Sw => 2,
            Self::H265 => 3,
            Self::Av1 => 4,
        }
    }

    /// For badges, tooltips and toasts (`videoCodecHumanName`).
    pub fn human_name(self) -> &'static str {
        match self {
            Self::Av1 => "AV1",
            Self::H265 => "H.265",
            Self::H264Hw => "H.264",
            Self::H264Sw => "H.264 (software)",
            Self::Unknown => "Unknown",
        }
    }
}

/// One codec this client can encode or decode, with its ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecCap {
    pub codec: VideoCodec,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub hardware: bool,
}

/// One native encoder as the core probed it (`NativeEncoderCap`): the codec's ceiling, and
/// which backend encodes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderCap {
    pub codec: VideoCodec,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub hardware: bool,
    /// "nvenc", "amf", "qsv", "vaapi", "software".
    pub backend: SharedString,
    /// What to show: "NVENC (NVIDIA)", … "Software (SVT-AV1)".
    pub label: SharedString,
    /// Software only: width × height × fps allowed; 0 = no limit beyond `max_*`.
    #[serde(default)]
    pub max_pixel_rate: u32,
}

impl EncoderCap {
    /// The ceiling alone: what is advertised.
    pub fn cap(&self) -> CodecCap {
        CodecCap {
            codec: self.codec,
            max_width: self.max_width,
            max_height: self.max_height,
            max_fps: self.max_fps,
            hardware: self.hardware,
        }
    }
}

/// The decoder a codec's streams get on this machine, for Settings → Codecs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoderInfo {
    pub codec: VideoCodec,
    /// "Vulkan Video", "NVDEC", "VA-API", "Software (dav1d)", "Software (FFmpeg)".
    pub label: SharedString,
    pub hardware: bool,
    /// The decoder decoded a test frame (else: what the driver and the FFmpeg build list).
    pub probed: bool,
}

/// A decoder that was tried for a codec and is not used, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedDecoder {
    pub codec: VideoCodec,
    pub label: SharedString,
    pub reason: SharedString,
}

/// What the decoder probe found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecodeReport {
    /// The codecs to advertise (without the H.264 fallback: the model adds it).
    pub caps: Vec<CodecCap>,
    pub decoders: Vec<DecoderInfo>,
    pub skipped: Vec<SkippedDecoder>,
    /// From a real probe: `false` while only the listed answer is known (no window yet).
    pub probed: bool,
}

/// `ui.json` key of the encoder probe's cache (the renderer's
/// `decibell.native_encoder_caps.v2`; v3 carries the backend, label and pixel-rate ceiling).
pub const ENCODER_CAPS_KEY: &str = "native_encoder_caps.v3";
/// Earlier caches, removed when met: the decode caps (the video crate keeps its own probe
/// result per GPU and driver now) and the encoder caps without their backends.
pub const STALE_CAPS_KEYS: [&str; 2] = ["decoder_caps.v6", "native_encoder_caps.v2"];

/// The decode ceiling (spec §3.2, `DECODE_CEILING`); matches the core's encode ceilings so
/// codec selection doesn't downgrade a 120 fps streamer when a viewer joins.
pub fn decode_ceiling(codec: VideoCodec) -> (u32, u32, u32) {
    let _ = codec;
    (3840, 2160, 120)
}

/// H.264 decode is always advertised (spec §3.3): the LCD picker always has a converging
/// codec. Appended, as software, when the probe didn't find it.
pub fn with_h264_fallback(mut caps: Vec<CodecCap>) -> Vec<CodecCap> {
    if !caps.iter().any(|c| c.codec == VideoCodec::H264Hw) {
        let (w, h, fps) = decode_ceiling(VideoCodec::H264Hw);
        caps.push(CodecCap { codec: VideoCodec::H264Hw, max_width: w, max_height: h, max_fps: fps, hardware: false });
    }
    caps
}

/// Only codecs this client can also decode (`probeNativeEncoders`' intersection): the
/// streamer's own preview, and watchers on the same platform, must be able to show it.
pub fn decodable_encoders(encode: Vec<CodecCap>, decode: &[CodecCap]) -> Vec<CodecCap> {
    encode.into_iter().filter(|e| decode.iter().any(|d| d.codec == e.codec)).collect()
}

/// Whether this client can watch `stream` (`canWatchStream`): the enforced codec (the
/// streamer's lock) and the current one must be decodable. Empty caps mean "not loaded
/// yet", never "nothing": fail open.
pub fn can_watch(stream: &StreamInfo, decode: &[CodecCap]) -> Result<(), SharedString> {
    if decode.is_empty() {
        return Ok(());
    }
    let has = |c: VideoCodec| c == VideoCodec::Unknown || decode.iter().any(|d| d.codec == c);
    if stream.enforced_codec != VideoCodec::Unknown && !has(stream.enforced_codec) {
        return Err(format!(
            "Cannot decode {} — streamer has locked this codec.",
            stream.enforced_codec.human_name()
        )
        .into());
    }
    if stream.current_codec != VideoCodec::Unknown && !has(stream.current_codec) {
        return Err(format!(
            "Cannot decode {} — your hardware/browser doesn't support it.",
            stream.current_codec.human_name()
        )
        .into());
    }
    Ok(())
}

/// `StreamCodecChangeReason`.
pub mod change_reason {
    pub const WATCHER_JOINED_LOW_CAPS: i32 = 1;
    pub const LIMITING_WATCHER_LEFT: i32 = 2;
    pub const STREAMER_INITIATED: i32 = 3;
}

fn res_label(w: u32, h: u32, fps: u32) -> String {
    let r = match (w, h) {
        (3840, 2160) => "4K".to_string(),
        (2560, 1440) => "1440p".to_string(),
        (1920, 1080) => "1080p".to_string(),
        (1280, 720) => "720p".to_string(),
        _ => format!("{w}×{h}"),
    };
    format!("{r}{fps}")
}

/// The "Codec changed" toast's text (`buildCodecToast`), streamer-side or viewer-side.
pub fn codec_change_text(
    reason: i32,
    codec: VideoCodec,
    width: u32,
    height: u32,
    fps: u32,
    streamer_name: &str,
    we_stream: bool,
) -> Option<String> {
    let codec = codec.human_name();
    let res = res_label(width, height, fps);
    Some(match reason {
        change_reason::WATCHER_JOINED_LOW_CAPS if we_stream => format!("Switched to {codec} at {res} so a viewer can watch."),
        change_reason::WATCHER_JOINED_LOW_CAPS => format!("{streamer_name} switched to {codec} ({res})."),
        change_reason::LIMITING_WATCHER_LEFT if we_stream => format!("Restored to {codec} at {res}."),
        change_reason::LIMITING_WATCHER_LEFT => format!("{streamer_name} restored to {codec} ({res})."),
        change_reason::STREAMER_INITIATED if we_stream => format!("Codec changed to {codec} at {res}."),
        change_reason::STREAMER_INITIATED => format!("{streamer_name} switched to {codec} ({res})."),
        _ => return None,
    })
}

/// What the video crate implements: which codecs this machine decodes and with which
/// decoder, probed off the UI thread. `force`: decode the test frames again whatever is
/// known (Settings → Codecs' refresh). The model adds the H.264 fallback itself.
pub trait DecoderProbe: 'static {
    fn probe(&self, force: bool) -> LocalBoxFuture<'static, DecodeReport>;
}

/// Settings → Codecs (`codecSettingsStore`).
#[derive(Default)]
pub struct CodecCaps {
    pub use_av1: bool,
    pub use_h265: bool,
    /// What we encode (probed, decodable, before the toggles: the core applies those).
    pub encode: Vec<CodecCap>,
    /// What we decode (with the H.264 fallback).
    pub decode: Vec<CodecCap>,
    /// Every encoder the core probed, with its backend, label and limits (also those whose
    /// codec this client cannot decode: `encode` leaves them out).
    pub encoders: Vec<EncoderCap>,
    /// The decoder each codec's streams get, and the ones given up with the reason.
    pub decoders: Vec<DecoderInfo>,
    pub skipped_decoders: Vec<SkippedDecoder>,
    /// `decode` comes from the decoder probe (the app runs it after its first frame).
    pub decode_probed: bool,
    /// Encode and decode are both loaded (`init_codec_caps` finished once).
    pub loaded: bool,
    /// A load is running.
    pub refreshing: bool,
    /// A forced load was asked for while one was running: it runs next.
    pub(crate) force_pending: bool,
    pub(crate) decoder_probe: Option<Rc<dyn DecoderProbe>>,
}

impl CodecCaps {
    /// The app installs the video crate's probe once.
    pub fn set_decoder_probe(&mut self, probe: Rc<dyn DecoderProbe>) {
        self.decoder_probe = Some(probe);
    }

    pub fn set_toggles(&mut self, use_av1: bool, use_h265: bool, cx: &mut Context<Self>) {
        if (self.use_av1, self.use_h265) != (use_av1, use_h265) {
            self.use_av1 = use_av1;
            self.use_h265 = use_h265;
            cx.notify();
        }
    }

    pub fn set_caps(&mut self, encode: Vec<CodecCap>, decode: Vec<CodecCap>, cx: &mut Context<Self>) {
        if (&self.encode, &self.decode, self.loaded) != (&encode, &decode, true) {
            self.encode = encode;
            self.decode = decode;
            self.loaded = true;
            cx.notify();
        }
    }

    /// Whether a load asked for now should start: one runs at a time. A load that is
    /// already running takes every caller (a forced one is remembered and runs after it); a
    /// plain one is not needed once the caps are loaded. `true`: the caller starts it
    /// (`refreshing` is set).
    pub fn begin_load(&mut self, force: bool, cx: &mut Context<Self>) -> bool {
        if self.refreshing {
            self.force_pending |= force;
            return false;
        }
        if self.loaded && !force {
            return false;
        }
        self.refreshing = true;
        cx.notify();
        true
    }

    /// The running load ended. `true`: a forced one was asked for meanwhile and starts now.
    pub fn end_load(&mut self, cx: &mut Context<Self>) -> bool {
        self.set_refreshing(false, cx);
        std::mem::take(&mut self.force_pending)
    }

    /// The decoder probe's answer: what we decode (with the H.264 fallback), with which
    /// decoder, what was given up. The advertised encode list follows (decodable only).
    pub fn set_decode(&mut self, report: DecodeReport, cx: &mut Context<Self>) {
        let decode = with_h264_fallback(report.caps);
        let encode = decodable_encoders(self.encoders.iter().map(EncoderCap::cap).collect(), &decode);
        let encode = if self.encoders.is_empty() { self.encode.clone() } else { encode };
        if (&self.decode, &self.decoders, &self.skipped_decoders, self.decode_probed, &self.encode)
            != (&decode, &report.decoders, &report.skipped, report.probed, &encode)
        {
            self.decode = decode;
            self.decoders = report.decoders;
            self.skipped_decoders = report.skipped;
            self.decode_probed = report.probed;
            self.encode = encode;
            cx.notify();
        }
    }

    /// The encoder probe's answer; `encode` keeps the ones we can also decode. Marks the
    /// caps loaded.
    pub fn set_encoders(&mut self, encoders: Vec<EncoderCap>, cx: &mut Context<Self>) {
        let encode = decodable_encoders(encoders.iter().map(EncoderCap::cap).collect(), &self.decode);
        if (&self.encoders, &self.encode, self.loaded) != (&encoders, &encode, true) {
            self.encoders = encoders;
            self.encode = encode;
            self.loaded = true;
            cx.notify();
        }
    }

    /// The backend that encodes `codec` ("NVENC (NVIDIA)"), when the probe said.
    pub fn encoder_label(&self, codec: VideoCodec) -> Option<&SharedString> {
        self.encoders.iter().find(|e| e.codec == codec).map(|e| &e.label).filter(|l| !l.is_empty())
    }

    /// The decoder `codec`'s streams get ("Vulkan Video"), when the probe said.
    pub fn decoder_label(&self, codec: VideoCodec) -> Option<&SharedString> {
        self.decoders.iter().find(|d| d.codec == codec).map(|d| &d.label)
    }

    pub fn set_refreshing(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.refreshing != on {
            self.refreshing = on;
            cx.notify();
        }
    }

    /// The codecs the Go Live picker offers: the encoders minus those the toggles
    /// switched off.
    pub fn offered(&self) -> Vec<VideoCodec> {
        self.encode
            .iter()
            .map(|c| c.codec)
            .filter(|c| match c {
                VideoCodec::Av1 => self.use_av1,
                VideoCodec::H265 => self.use_h265,
                _ => true,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(codec: VideoCodec) -> CodecCap {
        CodecCap { codec, max_width: 3840, max_height: 2160, max_fps: 120, hardware: true }
    }

    fn stream(current: VideoCodec, enforced: VideoCodec) -> StreamInfo {
        StreamInfo {
            stream_id: "2_screen".into(),
            owner_uid: 2,
            owner_name: "bob".into(),
            has_audio: false,
            width: 1920,
            height: 1080,
            fps: 60,
            current_codec: current,
            enforced_codec: enforced,
            watcher_count: 0,
        }
    }

    #[test]
    fn watching_needs_the_enforced_and_current_codec() {
        let h264 = [cap(VideoCodec::H264Hw)];
        assert!(can_watch(&stream(VideoCodec::Av1, VideoCodec::Unknown), &[]).is_ok(), "caps not loaded: fail open");
        assert!(can_watch(&stream(VideoCodec::H264Hw, VideoCodec::Unknown), &h264).is_ok());
        assert_eq!(
            can_watch(&stream(VideoCodec::H264Hw, VideoCodec::Av1), &h264).unwrap_err().as_ref(),
            "Cannot decode AV1 — streamer has locked this codec."
        );
        assert_eq!(
            can_watch(&stream(VideoCodec::H265, VideoCodec::Unknown), &h264).unwrap_err().as_ref(),
            "Cannot decode H.265 — your hardware/browser doesn't support it."
        );
    }

    #[test]
    fn caps_fallback_and_intersection() {
        let decode = with_h264_fallback(vec![cap(VideoCodec::Av1)]);
        assert_eq!(decode.len(), 2);
        assert!(!decode[1].hardware);
        assert_eq!(with_h264_fallback(decode.clone()).len(), 2);
        let encode = decodable_encoders(vec![cap(VideoCodec::H265), cap(VideoCodec::Av1), cap(VideoCodec::H264Hw)], &decode);
        assert_eq!(encode.iter().map(|c| c.codec).collect::<Vec<_>>(), [VideoCodec::Av1, VideoCodec::H264Hw]);
        let caps = CodecCaps { use_av1: false, use_h265: true, encode, ..Default::default() };
        assert_eq!(caps.offered(), [VideoCodec::H264Hw]);
    }

    use gpui::AppContext as _;

    fn encoder(codec: VideoCodec, label: &str) -> EncoderCap {
        EncoderCap {
            codec,
            max_width: 3840,
            max_height: 2160,
            max_fps: 120,
            hardware: true,
            backend: "nvenc".into(),
            label: label.into(),
            max_pixel_rate: 0,
        }
    }

    #[gpui::test]
    fn one_load_at_a_time_and_a_forced_one_is_not_lost(cx: &mut gpui::TestAppContext) {
        let caps = cx.new(|_| CodecCaps::default());
        caps.update(cx, |c, cx| {
            assert!(c.begin_load(false, cx), "nothing loaded: the first caller starts");
            assert!(c.refreshing);
            assert!(!c.begin_load(false, cx), "a second caller joins the running load");
            assert!(!c.begin_load(true, cx), "a forced one waits for it");
            c.set_encoders(vec![encoder(VideoCodec::H264Hw, "NVENC (NVIDIA)")], cx);
            assert!(c.end_load(cx), "and runs next");
            assert!(c.begin_load(true, cx));
            assert!(!c.end_load(cx));
            assert!(!c.begin_load(false, cx), "loaded: nothing to do");
            assert!(!c.refreshing);
        });
    }

    #[gpui::test]
    fn the_advertised_encoders_follow_what_decodes(cx: &mut gpui::TestAppContext) {
        let caps = cx.new(|_| CodecCaps::default());
        caps.update(cx, |c, cx| {
            let report = |codecs: &[VideoCodec]| DecodeReport {
                caps: codecs.iter().map(|c| cap(*c)).collect(),
                decoders: codecs
                    .iter()
                    .map(|c| DecoderInfo { codec: *c, label: "Vulkan Video".into(), hardware: true, probed: true })
                    .collect(),
                skipped: vec![SkippedDecoder { codec: VideoCodec::Av1, label: "NVDEC".into(), reason: "no CUDA".into() }],
                probed: true,
            };
            c.set_decode(report(&[VideoCodec::H265]), cx);
            // H.264 is always decoded (the fallback), as software when the probe had none.
            assert_eq!(c.decode.iter().map(|c| (c.codec, c.hardware)).collect::<Vec<_>>(), [(VideoCodec::H265, true), (VideoCodec::H264Hw, false)]);
            assert!(!c.loaded, "the encoders are not loaded yet");
            c.set_encoders(
                vec![encoder(VideoCodec::Av1, "NVENC (NVIDIA)"), encoder(VideoCodec::H265, "NVENC (NVIDIA)"), encoder(VideoCodec::H264Hw, "NVENC (NVIDIA)")],
                cx,
            );
            assert!(c.loaded);
            assert_eq!(c.encode.iter().map(|c| c.codec).collect::<Vec<_>>(), [VideoCodec::H265, VideoCodec::H264Hw]);
            assert_eq!(c.encoders.len(), 3, "the probe's whole answer stays");
            assert_eq!(c.encoder_label(VideoCodec::Av1).map(|l| l.as_ref()), Some("NVENC (NVIDIA)"));
            assert_eq!(c.decoder_label(VideoCodec::H265).map(|l| l.as_ref()), Some("Vulkan Video"));
            assert_eq!(c.decoder_label(VideoCodec::H264Hw), None, "the fallback names no decoder");
            // A later probe (a refresh, a new device) that decodes AV1 too: AV1 is offered.
            c.set_decode(report(&[VideoCodec::H265, VideoCodec::Av1, VideoCodec::H264Hw]), cx);
            assert_eq!(c.encode.iter().map(|c| c.codec).collect::<Vec<_>>(), [VideoCodec::Av1, VideoCodec::H265, VideoCodec::H264Hw]);
            assert_eq!(c.skipped_decoders.len(), 1);
        });
    }

    #[test]
    fn the_encoder_cache_round_trips() {
        let caps = vec![EncoderCap { hardware: false, backend: "software".into(), max_pixel_rate: 62_208_000, ..encoder(VideoCodec::Av1, "Software (SVT-AV1)") }];
        let json = serde_json::to_value(&caps).unwrap();
        assert_eq!(json[0]["label"], "Software (SVT-AV1)");
        assert_eq!(json[0]["maxPixelRate"], 62_208_000);
        assert_eq!(serde_json::from_value::<Vec<EncoderCap>>(json).unwrap(), caps);
        // A v2 entry (no backend) does not read as v3: the key changed for that.
        assert!(serde_json::from_value::<Vec<EncoderCap>>(serde_json::json!([{"codec": "av1", "maxWidth": 1, "maxHeight": 1, "maxFps": 1, "hardware": true}])).is_err());
    }

    #[test]
    fn codec_change_wording() {
        assert_eq!(
            codec_change_text(1, VideoCodec::H264Hw, 1920, 1080, 60, "bob", false).unwrap(),
            "bob switched to H.264 (1080p60)."
        );
        assert_eq!(
            codec_change_text(2, VideoCodec::Av1, 2560, 1440, 120, "bob", true).unwrap(),
            "Restored to AV1 at 1440p120."
        );
        assert_eq!(codec_change_text(3, VideoCodec::H265, 1000, 500, 30, "", true).unwrap(), "Codec changed to H.265 at 1000×50030.");
        assert_eq!(codec_change_text(0, VideoCodec::H265, 1, 1, 1, "", true), None);
    }
}

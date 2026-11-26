//! Codec negotiation on the client side (`utils/codecMap.ts`, `codecToasts.ts`,
//! `canWatchStream.ts`, `decoderProbe.ts`, `encoderProbe.ts`'s native path,
//! `stores/codecSettingsStore.ts`).
//!
//! What this client can **decode** comes from the video crate ([`DecoderProbe`], the app
//! installs it with `CodecCaps::set_decoder_probe`); what it can **encode** from the
//! core's `probe_native_encoders` (native encode is the only path here: no WebCodecs).
//! Both are cached in `ui.json` (`Layout::extra`, keys [`DECODER_CAPS_KEY`] /
//! [`ENCODER_CAPS_KEY`]) like the renderer's `localStorage`, shipped to the core (which
//! advertises them in every `JoinVoiceRequest`), and re-probed by Settings → Codecs'
//! refresh. The encode list keeps only codecs this client can also decode (the
//! streamer's own preview).

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

/// `ui.json` keys of the probe caches (the renderer's `decibell.decoder_caps.v6` and
/// `decibell.native_encoder_caps.v2`).
pub const DECODER_CAPS_KEY: &str = "decoder_caps.v6";
pub const ENCODER_CAPS_KEY: &str = "native_encoder_caps.v2";

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

/// What the video crate implements: which codecs this machine decodes (hardware or
/// software), probed off the UI thread. The model adds the H.264 fallback itself.
pub trait DecoderProbe: 'static {
    fn probe(&self) -> LocalBoxFuture<'static, Vec<CodecCap>>;
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
    pub loaded: bool,
    pub refreshing: bool,
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

//! Go Live settings (`voiceStore.streamSettings`, `features/voice/goLive/presets.ts`,
//! `loadSettings.ts`' stream block, `streamAudioFilter.ts`).
//!
//! The values live in `Settings` (the core's `config.json`, the `stream_*` fields): this
//! module reads them with the renderer's validation and defaults ([`StreamSettings::of`])
//! and writes them back ([`StreamSettings::store`]), so there is no second copy. Only the
//! cursor option is session state (`OwnStream::include_cursor`), as in the renderer.

use decibell_core::config::AppSettings;

use super::codecs::VideoCodec;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamResolution {
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "720p")]
    P720,
    /// The source's own size (announced as 1440p until capture reports it).
    Source,
}

impl StreamResolution {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "1080p" => Some(Self::P1080),
            "720p" => Some(Self::P720),
            "source" => Some(Self::Source),
            _ => None,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::P1080 => "1080p",
            Self::P720 => "720p",
            Self::Source => "source",
        }
    }

    /// The size asked of the encoder (`resolveDimensions`); "Source" asks for 0×0, the
    /// source's own size within the encoder's ceiling.
    pub fn request_size(self) -> (u32, u32) {
        match self {
            Self::P720 => (1280, 720),
            Self::P1080 => (1920, 1080),
            Self::Source => (0, 0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Source",
            other => other.as_setting(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamQuality {
    High,
    Medium,
    Low,
    Custom,
}

impl StreamQuality {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Custom => "custom",
        }
    }
}

/// How the ticked stream-audio apps are read (`StreamAudioMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamAudioMode {
    /// Only the ticked apps.
    Selected,
    /// Everything but the ticked apps.
    AllExcept,
    All,
}

impl StreamAudioMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "selected" => Some(Self::Selected),
            "all_except" => Some(Self::AllExcept),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::AllExcept => "all_except",
            Self::All => "all",
        }
    }
}

pub const VALID_FPS: [u32; 5] = [120, 60, 30, 15, 5];
pub const VALID_AUDIO_BITRATES: [u32; 2] = [128, 192];
/// A corrupt config can't balloon the app picker.
pub const MAX_AUDIO_APPS: usize = 256;

/// Canonical app identity, as the core's `normalize_identity`: trimmed, lowercase, no
/// trailing ".exe".
pub fn normalize_app_id(raw: &str) -> String {
    let s = raw.trim().to_lowercase();
    s.strip_suffix(".exe").map(str::to_string).unwrap_or(s)
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamSettings {
    pub resolution: StreamResolution,
    pub fps: u32,
    pub quality: StreamQuality,
    pub video_bitrate_kbps: u32,
    pub share_audio: bool,
    pub audio_bitrate_kbps: u32,
    pub audio_mode: StreamAudioMode,
    pub audio_apps: Vec<String>,
    /// Unknown = Auto.
    pub enforced_codec: VideoCodec,
}

impl Default for StreamSettings {
    fn default() -> Self {
        StreamSettings {
            resolution: StreamResolution::P1080,
            fps: 60,
            quality: StreamQuality::High,
            video_bitrate_kbps: 10_000,
            share_audio: false,
            audio_bitrate_kbps: 128,
            audio_mode: StreamAudioMode::All,
            audio_apps: Vec::new(),
            enforced_codec: VideoCodec::Unknown,
        }
    }
}

impl StreamSettings {
    /// The saved settings, each field whitelist-checked; anything invalid or missing
    /// keeps its default (`loadSettings`).
    pub fn of(s: &AppSettings) -> Self {
        let d = StreamSettings::default();
        let mut apps: Vec<String> = Vec::new();
        for app in s.stream_audio_apps.iter().flatten().map(|a| normalize_app_id(a)) {
            if !app.is_empty() && !apps.contains(&app) {
                apps.push(app);
            }
        }
        apps.truncate(MAX_AUDIO_APPS);
        StreamSettings {
            resolution: s.stream_resolution.as_deref().and_then(StreamResolution::parse).unwrap_or(d.resolution),
            fps: s.stream_fps.filter(|f| VALID_FPS.contains(f)).unwrap_or(d.fps),
            quality: s.stream_quality.as_deref().and_then(StreamQuality::parse).unwrap_or(d.quality),
            video_bitrate_kbps: s
                .stream_video_bitrate_kbps
                .filter(|b| *b > 0 && *b < 200_000)
                .unwrap_or(d.video_bitrate_kbps),
            share_audio: s.stream_share_audio.unwrap_or(d.share_audio),
            audio_bitrate_kbps: s
                .stream_audio_bitrate_kbps
                .filter(|b| VALID_AUDIO_BITRATES.contains(b))
                .unwrap_or(d.audio_bitrate_kbps),
            audio_mode: s.stream_audio_mode.as_deref().and_then(StreamAudioMode::parse).unwrap_or(d.audio_mode),
            audio_apps: if s.stream_audio_apps.is_some() { apps } else { d.audio_apps },
            enforced_codec: s
                .stream_enforced_codec
                .filter(|c| *c <= 4)
                .map(|c| VideoCodec::from_wire(c as i32))
                .unwrap_or(d.enforced_codec),
        }
    }

    /// Write these back into the settings blob (`saveSettings`' stream block).
    pub fn store(&self, s: &mut AppSettings) {
        s.stream_resolution = Some(self.resolution.as_setting().to_string());
        s.stream_fps = Some(self.fps);
        s.stream_quality = Some(self.quality.as_setting().to_string());
        s.stream_video_bitrate_kbps = Some(self.video_bitrate_kbps);
        s.stream_share_audio = Some(self.share_audio);
        s.stream_audio_bitrate_kbps = Some(self.audio_bitrate_kbps);
        s.stream_audio_mode = Some(self.audio_mode.as_setting().to_string());
        s.stream_audio_apps = Some(self.audio_apps.clone());
        s.stream_enforced_codec = Some(self.enforced_codec.wire());
    }

    /// The codec asked of the encoder: the enforced one, else H.264 (`handleGoLive`).
    pub fn start_codec(&self) -> VideoCodec {
        if self.enforced_codec == VideoCodec::Unknown { VideoCodec::H264Hw } else { self.enforced_codec }
    }

    /// Every 5 fps stream (Slides, or 5 fps under Advanced) is encoded for detail.
    pub fn detail(&self) -> bool {
        self.fps == 5
    }

    /// The preset these settings match, if any (the dialog shows "custom" otherwise).
    pub fn preset(&self) -> Option<&'static StreamPreset> {
        STREAM_PRESETS
            .iter()
            .find(|p| p.resolution == self.resolution && p.fps == self.fps && p.quality == self.quality)
    }

    /// Apply a preset (`presetSettings`).
    pub fn apply_preset(&mut self, p: &StreamPreset) {
        self.resolution = p.resolution;
        self.fps = p.fps;
        self.quality = p.quality;
        self.video_bitrate_kbps = step_bitrate(p.resolution, p.quality);
    }

    /// "Smooth · 1080p · 60 fps · with sound", beside the Go Live button. `sound`: the
    /// switch is shown and on.
    pub fn summary(&self, sound: bool) -> String {
        let mut parts = vec![match self.preset() {
            Some(p) => format!("{} · {}", p.label, p.spec),
            None => format!("Custom · {} · {} fps", self.resolution.label(), self.fps),
        }];
        if sound {
            parts.push("with sound".into());
        }
        parts.join(" · ")
    }
}

/// Video bitrate (kbps) of a quality step; "Source" gets the larger column.
pub fn step_bitrate(resolution: StreamResolution, quality: StreamQuality) -> u32 {
    let big = resolution == StreamResolution::Source;
    match quality {
        StreamQuality::Low => if big { 6000 } else { 3000 },
        StreamQuality::Medium => if big { 12_000 } else { 6000 },
        _ => if big { 20_000 } else { 10_000 },
    }
}

pub struct StreamPreset {
    pub id: &'static str,
    pub label: &'static str,
    /// The renderer's icon name (`gamepad`, `text`, `presentation`, `gauge`).
    pub icon: &'static str,
    pub spec: &'static str,
    pub blurb: &'static str,
    pub resolution: StreamResolution,
    pub fps: u32,
    pub quality: StreamQuality,
}

pub static STREAM_PRESETS: [StreamPreset; 4] = [
    StreamPreset {
        id: "smooth",
        label: "Smooth",
        icon: "gamepad",
        spec: "1080p · 60 fps",
        blurb: "Fluid motion at 1080p and 60 fps. Best for games and video.",
        resolution: StreamResolution::P1080,
        fps: 60,
        quality: StreamQuality::High,
    },
    StreamPreset {
        id: "sharp",
        label: "Sharp",
        icon: "text",
        spec: "Full res · 30 fps",
        blurb: "Your screen's full resolution at 30 fps, so text and code stay crisp.",
        resolution: StreamResolution::Source,
        fps: 30,
        quality: StreamQuality::Medium,
    },
    StreamPreset {
        id: "slides",
        label: "Slides",
        icon: "presentation",
        spec: "Full res · 5 fps",
        blurb: "Your screen's full resolution at 5 fps, with every frame encoded for detail. Best for presentations.",
        resolution: StreamResolution::Source,
        fps: 5,
        quality: StreamQuality::Medium,
    },
    StreamPreset {
        id: "saver",
        label: "Data saver",
        icon: "gauge",
        spec: "720p · 30 fps",
        blurb: "720p at 30 fps, a third of Smooth's bandwidth. For slow connections.",
        resolution: StreamResolution::P720,
        fps: 30,
        quality: StreamQuality::Low,
    },
];

/// `formatBitrate`.
pub fn format_bitrate(kbps: u32) -> String {
    if kbps < 1000 {
        return format!("{kbps} kbps");
    }
    let mbps = kbps as f64 / 1000.0;
    if kbps.is_multiple_of(1000) { format!("{mbps:.0} Mbps") } else { format!("{mbps:.1} Mbps") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_settings_are_validated_field_by_field() {
        let mut s = AppSettings {
            stream_resolution: Some("4k".into()),
            stream_fps: Some(5),
            stream_quality: Some("medium".into()),
            stream_video_bitrate_kbps: Some(500_000),
            stream_audio_bitrate_kbps: Some(160),
            stream_audio_mode: Some("all_except".into()),
            stream_audio_apps: Some(vec![" Firefox.EXE ".into(), "firefox".into(), "".into(), "Spotify".into()]),
            stream_enforced_codec: Some(9),
            ..AppSettings::default()
        };
        let st = StreamSettings::of(&s);
        assert_eq!(st.resolution, StreamResolution::P1080, "unknown → default");
        assert_eq!(st.fps, 5);
        assert_eq!(st.quality, StreamQuality::Medium);
        assert_eq!(st.video_bitrate_kbps, 10_000);
        assert_eq!(st.audio_bitrate_kbps, 128);
        assert_eq!(st.audio_mode, StreamAudioMode::AllExcept);
        assert_eq!(st.audio_apps, ["firefox", "spotify"]);
        assert_eq!(st.enforced_codec, VideoCodec::Unknown);
        assert!(st.detail());
        // Round trip.
        let mut back = StreamSettings::default();
        back.apply_preset(&STREAM_PRESETS[1]);
        back.store(&mut s);
        let again = StreamSettings::of(&s);
        assert_eq!(again.preset().map(|p| p.id), Some("sharp"));
        assert_eq!(again.video_bitrate_kbps, 12_000);
        assert_eq!(again.summary(true), "Sharp · Full res · 30 fps · with sound");
    }

    #[test]
    fn presets_and_formatting() {
        let mut st = StreamSettings::default();
        assert_eq!(st.preset().map(|p| p.id), Some("smooth"));
        st.fps = 15;
        assert!(st.preset().is_none());
        assert_eq!(st.summary(false), "Custom · 1080p · 15 fps");
        assert_eq!(step_bitrate(StreamResolution::P720, StreamQuality::Low), 3000);
        assert_eq!(format_bitrate(800), "800 kbps");
        assert_eq!(format_bitrate(12_000), "12 Mbps");
        assert_eq!(format_bitrate(2_500), "2.5 Mbps");
        assert_eq!(normalize_app_id("  Discord.exe"), "discord");
        assert_eq!(StreamSettings::default().start_codec(), VideoCodec::H264Hw);
    }
}

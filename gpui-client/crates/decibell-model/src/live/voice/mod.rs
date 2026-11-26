//! Voice channels, streams and the audio settings around them (`stores/voiceStore.ts`,
//! `voiceStatsStore.ts`, `audioDevicesStore.ts`, `streamAudioAppsStore.ts`,
//! `codecSettingsStore.ts`, the logic files of `features/voice/`).
//!
//! The renderer's one `voiceStore` is split here by how often each part changes and who
//! shows it (docs/model.md §4 and the "M3" section):
//!
//! | Entity | Changes | Notifies | Keyed event |
//! |---|---|---|---|
//! | `session` ([`VoiceSession`]) | join / leave / switch, our mute / deafen, MLS state, engine error | on change | — |
//! | `participants` ([`Participants`]) | the connected channel's roster or a participant's flags | on change | — |
//! | `rosters` ([`VoiceRosters`]) | any voice channel of any community | per batch | `Changed(keys)` |
//! | `speaking` ([`Speaking`]) | anyone starts / stops talking (high rate) | on change | `SpeakingChanged { uid }` |
//! | `input_level` ([`InputLevel`]) | ~16 Hz while a meter is shown | on change | — |
//! | `stats` ([`VoiceStats`]) | every ~2 s in a session | per sample | — |
//! | `streams` ([`StreamPresence`]) | stream start / stop / codec / viewers anywhere | on change | `StreamsChanged { key }` |
//! | `thumbnails` ([`Thumbnails`]) | a poster every ~15 s per stream | per poster | `ThumbnailChanged(uid)` |
//! | `watching` ([`Watching`]) | watch / focus / mini player / fullscreen | on change | — |
//! | `stream_views` ([`StreamViews`]) | a player mounts / unmounts | never | — |
//! | `own_stream` ([`OwnStream`]) | Go Live, stop, failure | on change | — |
//! | `devices` ([`AudioDevices`]) | a refresh (a view's first watch, a device change while watched) | on change | — |
//! | `stream_audio_apps` ([`StreamAudioApps`]) | a refresh that found a change | on change | — |
//! | `codecs` ([`CodecCaps`]) | a probe, a toggle | on change | — |

pub mod codecs;
pub mod devices;
pub mod gain;
pub mod go_live;
pub mod rosters;
pub mod session;
pub mod speaking;
pub mod stats;
pub mod streams;

use std::rc::Rc;

use gpui::{App, AppContext as _, Entity};

pub use codecs::{CodecCap, CodecCaps, DecodeReport, DecoderInfo, EncoderCap, SkippedDecoder, VideoCodec};
pub use devices::{AudioDevices, StreamAudioApps};
pub use rosters::{VoiceKey, VoiceRosters, VoiceUserState};
pub use session::{Participants, VoiceSession};
pub use speaking::{InputLevel, Speaking};
pub use stats::VoiceStats;
pub use streams::{OwnStream, StreamPresence, StreamViews, Thumbnails, Watching};

/// The voice entities (handles; cheap to clone).
#[derive(Clone)]
pub struct Voice {
    pub session: Entity<VoiceSession>,
    pub participants: Entity<Participants>,
    pub rosters: Entity<VoiceRosters>,
    pub speaking: Entity<Speaking>,
    pub input_level: Entity<InputLevel>,
    pub stats: Entity<VoiceStats>,
    pub streams: Entity<StreamPresence>,
    pub thumbnails: Entity<Thumbnails>,
    pub watching: Entity<Watching>,
    pub stream_views: Entity<StreamViews>,
    pub own_stream: Entity<OwnStream>,
    pub devices: Entity<AudioDevices>,
    pub stream_audio_apps: Entity<StreamAudioApps>,
    pub codecs: Entity<CodecCaps>,
}

impl Voice {
    pub(crate) fn new(cx: &mut App) -> Voice {
        Voice {
            session: cx.new(|_| VoiceSession::default()),
            participants: cx.new(|_| Participants::default()),
            rosters: cx.new(|_| VoiceRosters::default()),
            speaking: cx.new(|_| Speaking::default()),
            input_level: cx.new(|_| InputLevel::new(Rc::new(crate::actions::voice::set_level_reporting))),
            stats: cx.new(|_| VoiceStats::default()),
            streams: cx.new(|_| StreamPresence::default()),
            thumbnails: cx.new(|_| Thumbnails::default()),
            watching: cx.new(|_| Watching::default()),
            stream_views: cx.new(|_| StreamViews::default()),
            own_stream: cx.new(|_| OwnStream::default()),
            devices: cx.new(|_| {
                AudioDevices::new(Rc::new(crate::actions::voice::list_audio_devices))
                    .with_watching(Rc::new(crate::actions::voice::watch_audio_devices))
            }),
            stream_audio_apps: cx.new(|_| StreamAudioApps::new(Rc::new(crate::actions::voice::list_stream_audio_apps))),
            codecs: cx.new(|_| CodecCaps::default()),
        }
    }
}

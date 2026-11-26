//! Playback of video and audio attachments for the Decibell GPUI client: the engine behind the
//! players in a message's card. Design, the codec matrix and the measurements:
//! `gpui-client/docs/media-design.md`.
//!
//! ```ignore
//! let player = Player::open(Open {
//!     source: Box::new(reader.blocking()),          // decibell_attachments::media::MediaReader
//!     video: Some(VideoHub::global(cx)),            // None: an audio attachment
//!     key: format!("media:{attachment_id}"),
//!     output: decibell_media::system_output(),      // the default device; its own stream
//!     start_at: 0, play: true, volume: 1.0, muted: false,
//!     on_change: Box::new(move || /* wake the UI: player.status() changed */),
//! });
//! VideoView::new(player.viewer().unwrap())          // in the card's render()
//! player.pause(); player.seek(12_000_000); player.set_volume(0.5, false);
//! player.position()                                 // microseconds, for the time display
//! drop(player);                                     // fades out, closes everything
//! ```
//!
//! # What exists when
//!
//! | State | Threads | Decoders | Device |
//! |---|---|---|---|
//! | No player | none | none | none |
//! | Playing | `decibell-media` (control: demux, audio decode; asleep unless a queue has room), `decibell-video-<n>` (asleep until the next picture is due), the device's callback thread | one video, one audio | one output stream |
//! | Paused | the first two, parked | kept (the next picture is in hand) | closed 250 ms after the fade-out |
//! | Ended | `decibell-media`, parked; the video thread leaves after 3 s | audio kept, video freed (the last picture stays) | closed |
//!
//! # The modules
//!
//! [`source`] (bytes in: a custom AVIO over `Read + Seek`), `demux` (FFmpeg's demuxers),
//! `audio` (decode and resample), [`render`] (the ring, the envelope that makes every
//! transition silent, the audio clock), [`output`] (the device), [`player`] (the control
//! thread, the video clock, commands).

mod audio;
mod demux;
mod ffi;
pub mod output;
pub mod player;
pub mod render;
pub mod source;

pub use demux::OpenError;
pub use output::{AudioOutput, DefaultDevice, Recording, TestOutput, system_output};
pub use player::{Open, Phase, PlayStats, Player, Status, VideoInfo};
pub use source::Source;

/// What a file is, read from its header without decoding anything: for a card that wants a
/// duration or a picture size the upload did not record.
#[derive(Clone, Debug, PartialEq)]
pub struct Probe {
    /// Microseconds; 0 when the container does not say.
    pub duration: i64,
    pub video: Option<(decibell_video::Codec, u32, u32)>,
    pub audio: Option<String>,
    pub container: String,
}

/// Opens `source` far enough to say what it is. Blocking (reads the header through `source`).
pub fn probe(source: Box<dyn Source>) -> Result<Probe, OpenError> {
    let demuxer = demux::Demuxer::open(source, Default::default())?;
    Ok(Probe {
        duration: demuxer.duration,
        video: demuxer
            .video
            .as_ref()
            .map(|video| (video.codec, video.width, video.height)),
        audio: demuxer.audio.as_ref().map(|audio| audio.codec.clone()),
        container: demuxer.container.clone(),
    })
}

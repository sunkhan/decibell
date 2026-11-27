//! The seam for the persistent media players (`PersistentVideoLayer` / `PersistentAudioLayer`):
//! another crate's work. This crate draws the cards at rest and raises the request; the player
//! serves it.
//!
//! What a player gets:
//! - **Which**: `model.chat.video` / `model.chat.audio`'s `ActiveMedia` (attachment id, community,
//!   channel, file name, a video's natural size). One video and one audio play at a time.
//! - **Where** (video): [`VideoHosts`] — for every video surface drawn this frame, its window
//!   bounds by attachment id (the poster box: a standalone card's top part, a grid cell). The
//!   player overlays its picture on exactly that box, clipped to the list's viewport, and follows
//!   it as the list scrolls; when the box isn't drawn (scrolled away, another channel), it keeps
//!   playing hidden, as the renderer's layer did. The card stops drawing its play button while
//!   its attachment is the active one.
//! - **Bytes**: `decibell_attachments::media::MediaReader::new(attachments, &server.wire(), id)`
//!   (`.blocking()` for a decoder thread): plaintext by range, sealed files opened, held to the
//!   download cap. The process's `Attachments` is reached with
//!   `decibell_model::chat::composer::CoreAttachmentService::with_attachments`.
//! - **Posters**: a captured frame goes into `model.chat.video_versions` (`bump`) and the player's
//!   own store; the card draws the server thumbnail until then.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use decibell_model::chat::viewers::ActiveMedia;
use decibell_model::chat::{Attachment, Message};
use decibell_model::{Model, ServerId};
use gpui::{App, Bounds, Global, Pixels};

/// Where each video surface was drawn last (window coordinates), by attachment id.
#[derive(Default)]
pub struct VideoHosts {
    pub bounds: RefCell<HashMap<i64, Bounds<Pixels>>>,
    /// Self-tests: only what this window draws is recorded (here and by [`probe`]). A fixture
    /// check drives a window of its own while the app's main window shows the same fixture
    /// channel: both draw the same cards, and the one drawn last would win.
    pub only: std::cell::Cell<Option<gpui::WindowId>>,
}

/// Whether what `window` draws is recorded (see [`VideoHosts::only`]).
fn recorded(window: &gpui::Window, cx: &App) -> bool {
    cx.try_global::<VideoHosts>()
        .and_then(|hosts| hosts.only.get())
        .is_none_or(|only| only == window.window_handle().window_id())
}

/// A child for a card's element (positioned `relative()`) whose bounds a self-test needs, when
/// probes are on: `decibell_harness::probes::probe`, for the window the test drives only.
pub fn probe(key: impl Into<gpui::SharedString>) -> Option<impl gpui::IntoElement> {
    use gpui::Styled as _;
    if !decibell_harness::probes::enabled() {
        return None;
    }
    let key = key.into();
    Some(
        gpui::canvas(
            move |bounds, window, cx| {
                if recorded(window, cx) {
                    decibell_harness::probes::record(key.clone(), bounds);
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full(),
    )
}

impl Global for VideoHosts {}

/// Records the box `attachment_id`'s surface was drawn in (called while painting the card).
pub fn host_drawn(attachment_id: i64, bounds: Bounds<Pixels>, window: &gpui::Window, cx: &mut App) {
    if recorded(window, cx) {
        cx.default_global::<VideoHosts>().bounds.borrow_mut().insert(attachment_id, bounds);
    }
}

fn media(server: ServerId, message: &Message, a: &Attachment) -> ActiveMedia {
    ActiveMedia {
        attachment_id: a.id,
        server,
        channel: message.channel.clone(),
        filename: a.filename.clone(),
        width: a.width,
        height: a.height,
    }
}

/// The play button of a video card: the active video becomes this one.
pub fn play_video(server: ServerId, message: &Arc<Message>, a: &Attachment, bounds: Bounds<Pixels>, cx: &mut App) {
    cx.default_global::<VideoHosts>().bounds.borrow_mut().insert(a.id, bounds);
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let active = media(server, message, a);
    model.chat.video.update(cx, |video, cx| video.set_active(Some(active), cx));
}

/// The play button of an audio card: toggles the playing one, else makes this one active.
pub fn play_audio(server: ServerId, message: &Arc<Message>, a: &Attachment, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let is_active = model.chat.audio.read(cx).active.as_ref().is_some_and(|x| x.attachment_id == a.id);
    if is_active {
        // A player that could not open or decode its file is opened again (not in the
        // renderer, whose button is disabled once its card has an error).
        if let Some(playback) = crate::playback::Playback::global(cx)
            && playback.read(cx).audio_error(a.id).is_some()
        {
            playback.update(cx, |playback, cx| playback.retry_audio(cx));
            return;
        }
        // The player toggles; here the mirrored state flips so the card follows at once.
        let playing = model.chat.audio.read(cx).playing;
        model.chat.audio.update(cx, |audio, cx| audio.set_playback(Some(!playing), None, None, cx));
        return;
    }
    let active = media(server, message, a);
    model.chat.audio.update(cx, |audio, cx| audio.set_active(Some(active), cx));
}

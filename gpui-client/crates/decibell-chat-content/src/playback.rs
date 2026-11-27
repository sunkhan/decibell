//! The persistent players (`PersistentVideoLayer.tsx`, `PersistentAudioLayer.tsx`,
//! `audioController.ts`, `videoPlaybackState.ts`, `audioPlaybackState.ts`): one video and one
//! audio attachment play at a time, owned here and not by the message row that started them,
//! so playback goes on while the row is scrolled away, trimmed or re-created.
//!
//! * **What plays** is the model's: `chat.video.active` / `chat.audio.active` (the cards set
//!   them, `crate::player`). This entity follows both: a new active attachment gets a
//!   `decibell_media::Player`, the old one's is dropped (its sound fades out). Leaving the
//!   channel clears both, as the renderer's `clearChannel` does, and forgets the channel's
//!   "Paused at" positions (five per channel are kept while it is open).
//! * **Where the picture is drawn**: in the card itself. The card's surface asks
//!   [`video_stage`] for its element; while the row is not drawn the player is told
//!   ([`Playback::set_visible`]) and decodes without copying or presenting a picture.
//! * **Frames**: a picture costs one present of the kept scene (the video crate's presenter),
//!   never a render. The list is redrawn for the controls only: four times a second while
//!   they show a running time (the audio card always, the video's while the pointer is on
//!   it), and not at all while a video plays with its controls hidden. The controls come and go
//!   in 300 ms (`decibell_ui::motion::transition`): the list is redrawn while that value moves
//!   and never after.
//! * **Timers**, each with a live reason: that 250 ms tick, the three seconds after which a
//!   resting pointer hides the video's controls (and, in fullscreen, the cursor), and the
//!   220 ms a single click waits for a second one.

use std::io::{Read, Seek, SeekFrom};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use decibell_attachments::Attachments;
use decibell_attachments::media::{BlockingMediaReader, MediaReader};
use decibell_media::{Open, Phase, Player, Status};
use decibell_model::chat::Attachment;
use decibell_model::chat::viewers::{ActiveMedia, MediaMenuKind};
use decibell_model::{ChannelId, Model, ServerId, Uid};
use decibell_ui::motion::{self, Transition};
use decibell_ui::{ActiveTheme as _, Icon, IconName, title_tooltip};
use decibell_video::{VideoHub, VideoView};
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, ClickEvent, Context, Corners, DispatchPhase, Entity, FocusHandle, Global, HitboxBehavior, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, ScrollEvent, SharedString, Subscription, Task, Window,
    canvas, div, prelude::*, px,
};

use crate::style::{ContentStyle, label_in};

/// The time display's and the audio card's refresh (the renderer's `timeupdate` cadence).
const TICK: Duration = Duration::from_millis(250);
/// A resting pointer hides the video's controls after this (`resetIdleTimer`).
const IDLE: Duration = Duration::from_millis(3000);
/// A single click toggles play this long after it, unless a second one came (`handleSingleClick`).
const CLICK: Duration = Duration::from_millis(220);
/// The controls and the fullscreen download button come and go with
/// `transition-all duration-300 ease-out` (Tailwind's `ease-out`).
const FADE: Transition = Transition::new(300, motion::EASE_OUT);
/// `translate-y-2`: how far they slide while they fade.
const SLIDE: f32 = 8.;
/// Positions kept per channel (`MAX_PER_CHANNEL`).
const RESUME_PER_CHANNEL: usize = 5;

/// Gives a player its bytes: the attachment service's range reader, or a fixture's memory.
pub type SourceProvider = Rc<dyn Fn(ServerId, i64, &App) -> Option<Box<dyn decibell_media::Source>>>;

/// Which bar a drag belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drag {
    VideoSeek,
    VideoVolume,
    AudioSeek,
    AudioVolume,
}

struct Playing {
    media: ActiveMedia,
    player: Player,
    status: Status,
}

pub struct Playback {
    video: Option<Playing>,
    audio: Option<Playing>,
    /// (channel, attachment, microseconds): where each was left, newest last.
    resume: Vec<(ChannelId, i64, i64)>,
    hover: bool,
    idle: bool,
    drag: Option<Drag>,
    pub fullscreen: bool,
    video_visible: bool,
    audio_visible: bool,
    focus: FocusHandle,
    /// The audio cards' focus (`tabIndex={-1}` on `AudioItem`'s wrapper): one handle, tracked by
    /// the card that was pressed last.
    audio_focus: FocusHandle,
    audio_focused: Option<i64>,
    /// The audio attachment whose player failed, with why: its card is taller by that line.
    audio_failed: Option<(i64, SharedString)>,
    /// How opaque the video's controls were drawn last (self-tests).
    controls_drawn: std::cell::Cell<f32>,
    ticker: Option<Task<()>>,
    idle_timer: Option<Task<()>>,
    click_timer: Option<Task<()>>,
    provider: Option<SourceProvider>,
    changed: futures::channel::mpsc::UnboundedSender<()>,
    channel: Option<ChannelId>,
    /// The open DM conversation (`model.dm.list.active_peer`).
    conversation: Option<Uid>,
    volumes: (f64, bool, f64, bool),
    /// The saved name of the device players open their sound on ([`output_device`]).
    output: Option<String>,
    /// (channel, attachment, picture): the frame each played video stopped on, newest last
    /// (`setVideoPoster`; as many per channel as positions, gone with the channel).
    posters: Vec<(ChannelId, i64, decibell_video::Still)>,
    /// The channel that was left last: nothing of it is kept, whatever stops after the change.
    left: Option<ChannelId>,
    _pump: Task<()>,
    _subscriptions: Vec<Subscription>,
}

struct Handle(Entity<Playback>);
impl Global for Handle {}

/// The attachment service's reader, made on the player's thread when the service is there.
struct Lazy {
    pending: Option<std::sync::mpsc::Receiver<Arc<Attachments>>>,
    reader: Option<BlockingMediaReader>,
    server: String,
    id: i64,
}

impl Lazy {
    fn reader(&mut self) -> std::io::Result<&mut BlockingMediaReader> {
        if self.reader.is_none() {
            let service = self
                .pending
                .take()
                .and_then(|pending| pending.recv().ok())
                .ok_or_else(|| std::io::Error::other("Not connected"))?;
            self.reader = Some(MediaReader::new(service, &self.server, self.id).blocking());
        }
        self.reader.as_mut().ok_or_else(|| std::io::Error::other("Not connected"))
    }
}

impl Read for Lazy {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.reader()?.read(buffer)
    }
}

impl Seek for Lazy {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.reader()?.seek(to)
    }
}

fn core_source(server: ServerId, id: i64) -> Box<dyn decibell_media::Source> {
    let (tx, rx) = std::sync::mpsc::channel();
    decibell_model::chat::composer::CoreAttachmentService::with_attachments(move |service| {
        let _ = tx.send(service);
    });
    Box::new(Lazy {
        pending: Some(rx),
        reader: None,
        server: server.wire(),
        id,
    })
}

/// The output device attachment sound plays on, by its saved name (`None`: the system's
/// default). Owner, 2026-11-26: attachments are routed like other people's stream audio: the
/// separate stream-output device when that option is on in Settings → Audio, else the voice
/// output device (`set_separate_stream_output` / `set_output_device` in the core's pipeline).
/// Never Chromium's "default output whatever the settings say" (`electron-quirks.md` row 19).
pub fn output_device(cx: &App) -> Option<String> {
    let model = cx.try_global::<Model>()?;
    let settings = model.settings.read(cx).get();
    if settings.separate_stream_output {
        settings.stream_output_device.clone()
    } else {
        settings.output_device.clone()
    }
}

fn volumes(cx: &App) -> (f64, bool, f64, bool) {
    cx.try_global::<Model>().map_or((1., false, 1., false), |model| {
        let settings = model.settings.read(cx).get();
        (
            settings.media_video_volume.unwrap_or(1.),
            settings.media_video_muted,
            settings.media_audio_volume.unwrap_or(1.),
            settings.media_audio_muted,
        )
    })
}

impl Playback {
    /// Creates the players' owner and makes it follow the model. Once, from the crate's install.
    pub fn init(cx: &mut App) {
        if cx.try_global::<Handle>().is_some() {
            return;
        }
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let entity = cx.new(|cx: &mut Context<Playback>| {
            let (changed, mut changes) = futures::channel::mpsc::unbounded::<()>();
            // Every player's "my status changed" lands here: one wake for however many.
            let pump = cx.spawn(async move |this, cx| {
                while changes.next().await.is_some() {
                    while changes.try_recv().is_ok() {}
                    if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                        break;
                    }
                }
            });
            let subscriptions = vec![
                cx.observe(&model.chat.video, |this, _, cx| this.follow_video(cx)),
                cx.observe(&model.chat.audio, |this, _, cx| this.follow_audio(cx)),
                cx.observe(&model.nav, |this, nav, cx| {
                    let channel = nav.read(cx).active_channel.clone();
                    this.channel_changed(channel, cx);
                }),
                cx.observe(&model.dm.list, |this, list, cx| {
                    let peer = list.read(cx).active_peer;
                    this.conversation_changed(peer, cx);
                }),
                cx.observe(&model.settings, |this, _, cx| this.volumes_changed(cx)),
            ];
            Playback {
                video: None,
                audio: None,
                resume: Vec::new(),
                hover: false,
                idle: false,
                drag: None,
                fullscreen: false,
                video_visible: true,
                audio_visible: true,
                focus: cx.focus_handle(),
                audio_focus: cx.focus_handle(),
                audio_focused: None,
                audio_failed: None,
                controls_drawn: std::cell::Cell::new(0.),
                ticker: None,
                idle_timer: None,
                click_timer: None,
                provider: None,
                changed,
                channel: model.nav.read(cx).active_channel.clone(),
                conversation: model.dm.list.read(cx).active_peer,
                volumes: volumes(cx),
                output: output_device(cx),
                posters: Vec::new(),
                left: None,
                _pump: pump,
                _subscriptions: subscriptions,
            }
        });
        cx.set_global(Handle(entity));
    }

    pub fn global(cx: &App) -> Option<Entity<Playback>> {
        cx.try_global::<Handle>().map(|handle| handle.0.clone())
    }

    /// Fixtures and self-tests: where players read from instead of the attachment service.
    pub fn set_source_provider(&mut self, provider: SourceProvider) {
        self.provider = Some(provider);
    }

    fn open(&mut self, media: &ActiveMedia, video: bool, cx: &mut Context<Self>) -> Playing {
        let source = self
            .provider
            .as_ref()
            .and_then(|provider| provider(media.server, media.attachment_id, cx))
            .unwrap_or_else(|| core_source(media.server, media.attachment_id));
        let start_at = self
            .resume
            .iter()
            .find(|(channel, id, _)| *channel == media.channel && *id == media.attachment_id)
            .map_or(0, |entry| entry.2);
        let (video_volume, video_muted, audio_volume, audio_muted) = self.volumes;
        let changed = self.changed.clone();
        let player = Player::open(Open {
            source,
            video: video.then(|| VideoHub::global(cx)),
            // Never a uid: the hub's live streams are keyed by those.
            key: format!("media:{}:{}", media.server.wire(), media.attachment_id),
            output: decibell_media::device_output(self.output.clone()),
            start_at,
            play: true,
            volume: if video { video_volume } else { audio_volume } as f32,
            muted: if video { video_muted } else { audio_muted },
            on_change: Box::new(move || {
                let _ = changed.unbounded_send(());
            }),
        });
        let status = player.status();
        Playing {
            media: media.clone(),
            player,
            status,
        }
    }

    /// Keeps where a player was, for "Paused at" and the next play of the same attachment.
    fn remember(&mut self, playing: &Playing) {
        let position = match playing.status.phase {
            Phase::Ended | Phase::Failed(_) | Phase::Loading => 0,
            _ => playing.player.position(),
        };
        let (channel, id) = (playing.media.channel.clone(), playing.media.attachment_id);
        if self.left.as_ref() == Some(&channel) {
            // Stopped by leaving its channel (`clearChannel` drops the channel's entries).
            self.resume.retain(|entry| entry.0 != channel);
            self.posters.retain(|entry| entry.0 != channel);
            return;
        }
        // The frame a video stopped on becomes its card's poster (`PersistentVideoLayer`'s
        // unmount capture): the slot the picture is in, kept; nothing is read back.
        if playing.status.video.is_some()
            && playing.status.has_picture
            && let Some(still) = playing.player.still()
        {
            self.posters.retain(|entry| !(entry.0 == channel && entry.1 == id));
            self.posters.push((channel.clone(), id, still));
            if self.posters.iter().filter(|entry| entry.0 == channel).count() > RESUME_PER_CHANNEL
                && let Some(oldest) = self.posters.iter().position(|entry| entry.0 == channel)
            {
                self.posters.remove(oldest);
            }
        }
        self.resume.retain(|entry| !(entry.0 == channel && entry.1 == id));
        self.resume.push((channel.clone(), id, position));
        let kept = self.resume.iter().filter(|entry| entry.0 == channel).count();
        if kept > RESUME_PER_CHANNEL
            && let Some(oldest) = self.resume.iter().position(|entry| entry.0 == channel)
        {
            self.resume.remove(oldest);
        }
    }

    fn follow_video(&mut self, cx: &mut Context<Self>) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let active = model.chat.video.read(cx).active.clone();
        if self.video.as_ref().map(|playing| &playing.media) == active.as_ref() {
            return;
        }
        if let Some(old) = self.video.take() {
            self.remember(&old);
        }
        self.hover = false;
        self.idle = false;
        self.drag = None;
        self.video_visible = true;
        self.video = active.map(|media| self.open(&media, true, cx));
        if self.video.is_none() && self.fullscreen {
            self.fullscreen = false;
        }
        self.timers(cx);
        cx.notify();
    }

    fn follow_audio(&mut self, cx: &mut Context<Self>) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let (active, wanted) = {
            let audio = model.chat.audio.read(cx);
            (audio.active.clone(), audio.playing)
        };
        if self.audio.as_ref().map(|playing| &playing.media) != active.as_ref() {
            if let Some(old) = self.audio.take() {
                self.remember(&old);
            }
            self.audio_visible = true;
            self.audio = active.map(|media| self.open(&media, false, cx));
            self.mirror_audio(cx);
            self.audio_failure(cx);
        } else if let Some(playing) = &self.audio
            && playing.player.is_playing() != wanted
            && !matches!(playing.status.phase, Phase::Loading | Phase::Failed(_))
        {
            // The card's play button flipped the mirrored state (`audioToggle`).
            playing.player.toggle();
        }
        self.timers(cx);
    }

    /// Leaving a channel stops what plays in it (`clearChannel`).
    fn channel_changed(&mut self, channel: Option<ChannelId>, cx: &mut Context<Self>) {
        if channel == self.channel {
            return;
        }
        let left = std::mem::replace(&mut self.channel, channel);
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        model.chat.video.update(cx, |video, cx| video.set_active(None, cx));
        model.chat.audio.update(cx, |audio, cx| audio.set_active(None, cx));
        if let Some(left) = &left {
            self.resume.retain(|entry| entry.0 != *left);
            self.posters.retain(|entry| entry.0 != *left);
        }
        self.left = left;
    }

    /// Another DM conversation was opened (or the open one closed): what plays stops, as on a
    /// channel change. A conversation holds no attachments, so there is no position of its own
    /// to forget.
    fn conversation_changed(&mut self, peer: Option<Uid>, cx: &mut Context<Self>) {
        if peer == self.conversation {
            return;
        }
        self.conversation = peer;
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        model.chat.video.update(cx, |video, cx| video.set_active(None, cx));
        model.chat.audio.update(cx, |audio, cx| audio.set_active(None, cx));
    }

    /// Keeps `audio_failed` in step with the audio player; the card's height follows it.
    fn audio_failure(&mut self, cx: &mut Context<Self>) {
        let failed = self.audio.as_ref().and_then(|audio| match &audio.status.phase {
            Phase::Failed(reason) => Some((audio.media.attachment_id, SharedString::from(reason.clone()))),
            _ => None,
        });
        if failed != self.audio_failed {
            self.audio_failed = failed;
            decibell_chat::invalidate_slots(cx);
            cx.notify();
        }
    }

    /// Settings → Audio changed which device attachment sound belongs on (the device, the
    /// separate-stream-output switch, or the device behind it): what plays moves there, without
    /// a pop and without the picture waiting (`Player::set_output`).
    fn output_changed(&mut self, cx: &mut Context<Self>) {
        let now = output_device(cx);
        if now == self.output {
            return;
        }
        self.output = now;
        for playing in self.video.iter().chain(&self.audio) {
            playing.player.set_output(decibell_media::device_output(self.output.clone()));
        }
    }

    fn volumes_changed(&mut self, cx: &mut Context<Self>) {
        self.output_changed(cx);
        let now = volumes(cx);
        if now == self.volumes {
            return;
        }
        self.volumes = now;
        if let Some(video) = &self.video {
            video.player.set_volume(now.0 as f32, now.1);
        }
        if let Some(audio) = &self.audio {
            audio.player.set_volume(now.2 as f32, now.3);
        }
        cx.notify();
    }

    /// A player's status changed (its thread said so).
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if let Some(video) = &mut self.video {
            let status = video.player.status();
            if status != video.status {
                video.status = status;
                changed = true;
            }
        }
        if let Some(audio) = &mut self.audio {
            let status = audio.player.status();
            if status != audio.status {
                audio.status = status;
                changed = true;
            }
        }
        if changed {
            self.mirror_audio(cx);
            self.audio_failure(cx);
            self.timers(cx);
            cx.notify();
        }
    }

    /// The audio card draws from the model's mirrored state (`activeAudioStore`).
    fn mirror_audio(&mut self, cx: &mut Context<Self>) {
        let Some(audio) = &self.audio else { return };
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let playing = audio.status.phase == Phase::Playing || (audio.status.phase == Phase::Loading && audio.player.is_playing());
        let time = audio.player.position() as f64 / 1e6;
        let duration = audio.status.duration as f64 / 1e6;
        model
            .chat
            .audio
            .update(cx, |store, cx| store.set_playback(Some(playing), Some(time), Some(duration), cx));
    }

    fn video_playing(&self) -> bool {
        self.video.as_ref().is_some_and(|video| video.status.phase == Phase::Playing)
    }

    /// `controlsVisible`: paused, scrubbing, or the pointer moved on the video lately.
    fn controls_visible(&self) -> bool {
        !self.video_playing() || self.drag == Some(Drag::VideoSeek) || (self.hover && !self.idle)
    }

    /// Starts and stops the tick with its reason.
    fn timers(&mut self, cx: &mut Context<Self>) {
        let video = self.video_playing() && (self.video_visible || self.fullscreen) && self.controls_visible();
        let audio = self.audio_visible && self.audio.as_ref().is_some_and(|audio| audio.status.phase == Phase::Playing);
        if video || audio || self.drag.is_some() {
            if self.ticker.is_none() {
                self.ticker = Some(cx.spawn(async |this, cx| {
                    loop {
                        cx.background_executor().timer(TICK).await;
                        if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                            break;
                        }
                    }
                }));
            }
        } else {
            self.ticker = None;
        }
        if !self.video_playing() {
            self.idle_timer = None;
            self.idle = false;
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.mirror_audio(cx);
        if self.video.is_some() && self.controls_visible() {
            cx.notify();
        }
    }

    /// The pointer moved on the video (`resetIdleTimer`).
    fn poke(&mut self, cx: &mut Context<Self>) {
        let shown = self.controls_visible();
        self.hover = true;
        self.idle = false;
        if self.video_playing() {
            self.idle_timer = Some(cx.spawn(async |this, cx| {
                cx.background_executor().timer(IDLE).await;
                let _ = this.update(cx, |this, cx| {
                    this.idle = true;
                    this.idle_timer = None;
                    if this.fullscreen && this.video_playing() && this.drag.is_none() {
                        // `cursor-none` while playing, hovered and idle.
                        cx.hide_cursor_until_mouse_moves();
                    }
                    this.timers(cx);
                    cx.notify();
                });
            }));
        }
        if shown != self.controls_visible() {
            self.timers(cx);
            cx.notify();
        }
    }

    fn set_hover(&mut self, hover: bool, cx: &mut Context<Self>) {
        if hover {
            return self.poke(cx);
        }
        if self.hover {
            self.hover = false;
            self.timers(cx);
            cx.notify();
        }
    }

    /// The list says which rows it draws: a player whose row is not among them shows nothing.
    pub fn set_visible(&mut self, video: Option<bool>, audio: Option<bool>, cx: &mut Context<Self>) {
        if let Some(visible) = video
            && visible != self.video_visible
        {
            self.video_visible = visible;
            if let Some(playing) = &self.video {
                playing.player.set_visible(visible || self.fullscreen);
            }
            self.timers(cx);
        }
        if let Some(visible) = audio
            && visible != self.audio_visible
        {
            self.audio_visible = visible;
            self.timers(cx);
            if visible {
                self.mirror_audio(cx);
            }
        }
    }

    pub fn active_ids(&self) -> (Option<i64>, Option<i64>) {
        (
            self.video.as_ref().map(|playing| playing.media.attachment_id),
            self.audio.as_ref().map(|playing| playing.media.attachment_id),
        )
    }

    /// Where an attachment that is not playing was left ("Paused at 0:06"), microseconds.
    pub fn paused_at(&self, channel: &ChannelId, attachment: i64) -> Option<i64> {
        self.resume
            .iter()
            .find(|entry| entry.0 == *channel && entry.1 == attachment && entry.2 > 0)
            .map(|entry| entry.2)
    }

    /// Why the audio attachment cannot be played, if it is the active one and failed.
    pub fn audio_error(&self, attachment: i64) -> Option<SharedString> {
        self.audio_failed.as_ref().filter(|failed| failed.0 == attachment).map(|failed| failed.1.clone())
    }

    /// The audio attachment whose player failed, and why (the card's "Failed to load" line).
    pub fn failed_audio(&self) -> Option<&(i64, SharedString)> {
        self.audio_failed.as_ref()
    }

    /// Opens the failed audio attachment's player again (its play button, Space on its card).
    pub fn retry_audio(&mut self, cx: &mut Context<Self>) {
        if let Some(old) = self.audio.take() {
            self.audio = Some(self.open(&old.media, false, cx));
            self.mirror_audio(cx);
            self.audio_failure(cx);
            self.timers(cx);
            cx.notify();
        }
    }

    /// The audio cards' focus handle, and the card that tracks it.
    pub fn audio_focus(&self) -> (&FocusHandle, Option<i64>) {
        (&self.audio_focus, self.audio_focused)
    }

    /// A press on an audio card: the keys go to it (`onMouseDownCapture` → `focus()`).
    pub fn focus_audio(&mut self, attachment: i64, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.audio_focus, cx);
        if self.audio_focused != Some(attachment) {
            self.audio_focused = Some(attachment);
            cx.notify();
        }
    }

    /// Whether the video's controls are wanted (`controlsVisible`), and how opaque they were
    /// drawn last: 1 shown, 0 gone, in between while they fade (self-tests).
    pub fn controls(&self) -> (bool, f32) {
        (self.video.is_some() && self.controls_visible(), self.controls_drawn.get())
    }

    /// The playing video's position and duration, microseconds (self-tests).
    pub fn video_times(&self) -> (i64, i64) {
        self.video.as_ref().map_or((0, 0), |video| (video.player.position(), video.status.duration))
    }

    pub fn video_phase(&self) -> Option<Phase> {
        self.video.as_ref().map(|video| video.status.phase.clone())
    }

    /// The playing video's counters and what the player reports of the file (`media_perf`).
    pub fn video_report(&self) -> Option<(decibell_media::PlayStats, Status)> {
        self.video.as_ref().map(|video| (video.player.stats(), video.player.status()))
    }

    pub fn seek_video_to(&mut self, micros: i64, cx: &mut Context<Self>) {
        self.seek_video(micros, cx);
    }

    fn toggle_video(&mut self, cx: &mut Context<Self>) {
        if let Some(video) = &self.video {
            video.player.toggle();
        }
        self.timers(cx);
    }

    fn seek_video(&mut self, micros: i64, cx: &mut Context<Self>) {
        if let Some(video) = &self.video {
            video.player.seek(micros);
            cx.notify();
        }
    }

    /// `audioSeek`.
    pub fn seek_audio(&mut self, ratio: f32, cx: &mut Context<Self>) {
        if let Some(audio) = &self.audio
            && audio.status.duration > 0
        {
            audio.player.seek((ratio.clamp(0., 1.) as f64 * audio.status.duration as f64) as i64);
            self.mirror_audio(cx);
        }
    }

    fn set_fullscreen(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.video.is_none() && on {
            return;
        }
        self.fullscreen = on;
        if window.is_fullscreen() != on {
            window.toggle_fullscreen();
        }
        if let Some(video) = &self.video {
            video.player.set_visible(on || self.video_visible);
        }
        self.timers(cx);
        cx.notify();
    }

    /// The video card's keys (`handleKey`): space, the arrows (5 s), F, and Escape in fullscreen.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(video) = &self.video else { return false };
        match event.keystroke.key.as_str() {
            "space" => self.toggle_video(cx),
            key @ ("left" | "right") => {
                let delta = if key == "right" { 5_000_000 } else { -5_000_000 };
                let target = (video.player.position() + delta).clamp(0, video.status.duration.max(0));
                self.seek_video(target, cx);
            }
            "f" => self.set_fullscreen(!self.fullscreen, window, cx),
            "escape" if self.fullscreen => self.set_fullscreen(false, window, cx),
            _ => return false,
        }
        true
    }

    fn click(&mut self, event: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.click_count() >= 2 {
            // The second click cancels the first one's toggle and goes fullscreen instead.
            self.click_timer = None;
            let on = !self.fullscreen;
            self.set_fullscreen(on, window, cx);
        } else if self.click_timer.is_none() {
            self.click_timer = Some(cx.spawn(async |this, cx| {
                cx.background_executor().timer(CLICK).await;
                let _ = this.update(cx, |this, cx| {
                    this.click_timer = None;
                    this.toggle_video(cx);
                });
            }));
        }
    }

    fn retry_video(&mut self, cx: &mut Context<Self>) {
        if let Some(old) = self.video.take() {
            self.video = Some(self.open(&old.media, true, cx));
            cx.notify();
        }
    }
}

fn clock(micros: i64) -> String {
    let seconds = (micros.max(0) / 1_000_000) as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// The frame a played video stopped on, for its card at rest (`getCachedVideo(…).posterUrl`).
pub fn poster(channel: &ChannelId, attachment: i64, cx: &App) -> Option<decibell_video::Still> {
    let playback = Playback::global(cx)?.read(cx);
    playback
        .posters
        .iter()
        .find(|entry| entry.0 == *channel && entry.1 == attachment)
        .map(|entry| entry.2.clone())
}

/// "Paused at 0:06" for a video card's bar, when that attachment was left part-way.
pub fn paused_label(channel: &ChannelId, attachment: i64, cx: &App) -> Option<String> {
    let at = Playback::global(cx)?.read(cx).paused_at(channel, attachment)?;
    (at >= 1_000_000).then(|| format!("Paused at {}", clock(at)))
}

/// Where on a bar the pointer is, 0.0 to 1.0.
type OnRatio = Rc<dyn Fn(f32, &mut Window, &mut App)>;
/// A wheel step over a bar: up or down.
type OnWheel = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// A bar that reports where on it the pointer is while the button is held (the scrub and
/// volume tracks: `onScrubDown` and friends follow the mouse over the whole window).
fn track(
    kind: Drag,
    playback: Entity<Playback>,
    on_ratio: OnRatio,
    on_wheel: Option<OnWheel>,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            window.set_cursor_style(gpui::CursorStyle::PointingHand, &hitbox);
            let ratio = move |x: Pixels| ((x - bounds.left()) / bounds.size.width).clamp(0., 1.);
            {
                let (playback, hitbox, on_ratio) = (playback.clone(), hitbox.clone(), on_ratio.clone());
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                        return;
                    }
                    playback.update(cx, |playback, cx| {
                        playback.drag = Some(kind);
                        playback.timers(cx);
                    });
                    window.capture_pointer(hitbox.id);
                    on_ratio(ratio(event.position.x), window, cx);
                    cx.stop_propagation();
                });
            }
            {
                let (playback, on_ratio) = (playback.clone(), on_ratio.clone());
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && playback.read(cx).drag == Some(kind) {
                        on_ratio(ratio(event.position.x), window, cx);
                    }
                });
            }
            {
                let playback = playback.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || playback.read(cx).drag != Some(kind) {
                        return;
                    }
                    window.release_pointer();
                    playback.update(cx, |playback, cx| {
                        playback.drag = None;
                        playback.timers(cx);
                        cx.notify();
                    });
                });
            }
            if let Some(on_wheel) = on_wheel.clone() {
                let hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                        return;
                    }
                    let delta = event.delta.pixel_delta(px(20.)).y;
                    if delta == px(0.) {
                        return;
                    }
                    // The volume, not the chat's scroll position.
                    cx.stop_propagation();
                    on_wheel(delta > px(0.), window, cx);
                });
            }
        },
    )
    .absolute()
    .inset_0()
}

fn set_video_volume(volume: f64, cx: &mut App) {
    // `handleVolumeMouse`: zero mutes, anything else unmutes.
    let volume = volume.clamp(0., 1.);
    if let Some(model) = cx.try_global::<Model>().cloned() {
        model.settings.update(cx, |settings, cx| {
            settings.change(
                |values| {
                    values.media_video_volume = Some(volume);
                    values.media_video_muted = volume == 0.;
                },
                cx,
            )
        });
    }
}

/// `audioSetVolume`: works before anything plays.
pub fn set_audio_volume(volume: f64, cx: &mut App) {
    let volume = volume.clamp(0., 1.);
    if let Some(model) = cx.try_global::<Model>().cloned() {
        model.settings.update(cx, |settings, cx| {
            settings.change(
                |values| {
                    values.media_audio_volume = Some(volume);
                    values.media_audio_muted = volume == 0.;
                },
                cx,
            )
        });
    }
}

/// The audio card's scrub track (only the active attachment's, once its duration is known).
pub fn audio_scrub(attachment: i64, cx: &App) -> Option<AnyElement> {
    let playback = Playback::global(cx)?;
    let state = playback.read(cx);
    let audio = state.audio.as_ref().filter(|audio| audio.media.attachment_id == attachment)?;
    if audio.status.duration <= 0 {
        return None;
    }
    let target = playback.clone();
    Some(
        track(
            Drag::AudioSeek,
            playback,
            Rc::new(move |ratio, _, cx| target.update(cx, |playback, cx| playback.seek_audio(ratio, cx))),
            None,
        )
        .into_any_element(),
    )
}

/// The audio card's volume track: a drag sets it, the wheel steps it by 5 %.
pub fn audio_volume_track(cx: &App) -> Option<AnyElement> {
    let playback = Playback::global(cx)?;
    Some(
        track(
            Drag::AudioVolume,
            playback,
            Rc::new(|ratio, _, cx| set_audio_volume(ratio as f64, cx)),
            Some(Rc::new(|up, _, cx| {
                let (_, _, volume, muted) = volumes(cx);
                let shown = if muted { 0. } else { volume };
                set_audio_volume(shown + if up { 0.05 } else { -0.05 }, cx);
            })),
        )
        .into_any_element(),
    )
}

fn icon_button(id: &'static str, icon: IconName, title: &'static str, on_click: impl Fn(&mut Window, &mut App) + 'static, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    let white = gpui::white();
    div()
        .id(id)
        .size_8()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme.radius_sm)
        .cursor_pointer()
        .hover(|style| style.bg(theme.color_surface_active))
        .tooltip(title_tooltip(title))
        // `text-white/80`: the renderer's own literal on the black scrim.
        .child(Icon::new(icon).size(px(16.)).color(gpui::Hsla { a: 0.8, ..white }))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_, window, cx| {
            cx.stop_propagation();
            on_click(window, cx)
        })
}

/// The dot at a bar's fill end, there while the pointer is on the bar (`group-hover:opacity-100`
/// on `h-3 w-3 -translate-x-1/2 rounded-full border-2 border-accent`; `glow` is the scrub
/// thumbs' `shadow-[0_0_6px_color-mix(in_srgb,var(--color-accent)_30%,transparent)]`). The bar
/// is `.group(group)`.
pub fn thumb(ratio: f32, size: f32, fill: gpui::Hsla, glow: bool, group: impl Into<SharedString>, cx: &App) -> gpui::Div {
    let accent = cx.theme().color_accent;
    div()
        .absolute()
        .left(gpui::relative(ratio.clamp(0., 1.)))
        .ml(px(-size / 2.))
        .size(px(size))
        .rounded_full()
        .border_2()
        .border_color(accent)
        .bg(fill)
        .when(glow, |thumb| {
            thumb.shadow(vec![gpui::BoxShadow {
                color: gpui::Hsla { a: accent.a * 0.3, ..accent },
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(6.),
                spread_radius: px(0.),
                inset: false,
            }])
        })
        .opacity(0.)
        .group_hover(group, |style| style.opacity(1.))
}

fn bar(ratio: f32, group: &'static str, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let white = gpui::white();
    div()
        .group(group)
        .relative()
        .h_3()
        .flex()
        .items_center()
        // `bg-white/15` on the scrim, the fill in the accent.
        .child(div().absolute().left_0().right_0().h(px(4.)).rounded_full().bg(gpui::Hsla { a: 0.15, ..white }))
        .child(div().absolute().left_0().h(px(4.)).w(gpui::relative(ratio.clamp(0., 1.))).rounded_full().bg(theme.color_accent))
        .child(thumb(ratio, 12., gpui::black(), true, group, cx))
}

/// What the video card's surface draws while its attachment is the playing one: the picture
/// and, over it, the controls. `None`: this attachment is not playing (the card draws its
/// poster and play button).
#[allow(clippy::too_many_arguments)]
pub fn video_stage(
    attachment: i64,
    menu: Option<(ServerId, Attachment)>,
    corners: Corners<Pixels>,
    fullscreen: bool,
    style: &Rc<ContentStyle>,
    window: &mut Window,
    cx: &mut App,
) -> Option<AnyElement> {
    let playback = Playback::global(cx)?;
    {
        let state = playback.read(cx);
        state.video.as_ref().filter(|video| video.media.attachment_id == attachment)?;
        if state.fullscreen != fullscreen {
            // The picture is in the fullscreen layer; the card keeps its box black meanwhile.
            return Some(div().size_full().bg(gpui::black()).into_any_element());
        }
    }
    // `transition-all duration-300 ease-out`: 1 shown, 0 gone. Asked for on every draw of the
    // stage, so the value is kept while the controls are away; it asks for the next frame only
    // while it moves.
    let wanted = playback.read(cx).controls_visible();
    let shown = motion::transition_flag(window, cx, (if fullscreen { "media-controls-full" } else { "media-controls" }, attachment as u64), wanted, FADE);
    let state = playback.read(cx);
    state.controls_drawn.set(shown);
    let video = state.video.as_ref().filter(|video| video.media.attachment_id == attachment)?;
    let theme = cx.theme();
    let black = gpui::black();
    let playing = state.video_playing() || (video.status.phase == Phase::Loading && video.player.is_playing());
    let server = video.media.server;
    let filename = video.media.filename.clone();
    let duration = video.status.duration;
    let position = video.player.position();
    let progress = if duration > 0 { position as f32 / duration as f32 } else { 0. };
    let (volume, muted, _, _) = state.volumes;
    let shown_volume = if muted { 0. } else { volume as f32 };
    let focus = state.focus.clone();
    let failed = match &video.status.phase {
        Phase::Failed(reason) => Some(SharedString::from(reason.clone())),
        _ => None,
    };
    let picture = video.player.viewer().map(|viewer| VideoView::new(viewer).fit(decibell_video::Fit::Contain).corner_radii(corners).backdrop(black));
    let is_fullscreen = state.fullscreen;

    let mut stage = div()
        .id(("media-stage", attachment as u64))
        .track_focus(&focus)
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(black)
        .rounded_tl(corners.top_left)
        .rounded_tr(corners.top_right)
        .rounded_bl(corners.bottom_left)
        .rounded_br(corners.bottom_right)
        // The wrapper's `overflow-hidden`: the controls slide in from under its edge.
        .overflow_hidden()
        .cursor_pointer()
        .children(picture.map(|picture| div().absolute().inset_0().child(picture)));
    {
        let playback = playback.clone();
        let focus = focus.clone();
        stage = stage.on_mouse_down(MouseButton::Left, move |_, window, cx| {
            // `onMouseDownCapture`: the keys go to the player that was clicked.
            window.focus(&focus, cx);
            cx.stop_propagation();
            let _ = &playback;
        });
    }
    {
        let playback = playback.clone();
        stage = stage.on_mouse_move(move |_, _, cx| playback.update(cx, |playback, cx| playback.poke(cx)));
    }
    {
        let playback = playback.clone();
        stage = stage.on_hover(move |hovered: &bool, _, cx| playback.update(cx, |playback, cx| playback.set_hover(*hovered, cx)));
    }
    {
        let playback = playback.clone();
        stage = stage.on_key_down(move |event, window, cx| {
            if playback.update(cx, |playback, cx| playback.key(event, window, cx)) {
                cx.stop_propagation();
            }
        });
    }
    if let Some(reason) = failed {
        // Not in the renderer (its `<video>` reloads on an error and otherwise stays black):
        // here the card says why and a click tries again.
        let playback = playback.clone();
        return Some(
            stage
                .child(
                    div()
                        .relative()
                        .max_w(gpui::relative(0.85))
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(label_in(reason, &style.scrim, decibell_text::Wrapping::TRUNCATE, theme.color_text_secondary))
                        .child(label_in("Click to try again", &style.scrim, decibell_text::Wrapping::NOWRAP, theme.color_text_muted)),
                )
                .on_click(move |_, _, cx| playback.update(cx, |playback, cx| playback.retry_video(cx)))
                .into_any_element(),
        );
    }
    {
        let playback = playback.clone();
        stage = stage.on_click(move |event, window, cx| playback.update(cx, |playback, cx| playback.click(event, window, cx)));
    }
    if let Some((server, a)) = menu {
        stage = stage.on_mouse_down(MouseButton::Right, move |event, _, cx| {
            cx.stop_propagation();
            crate::menu::show_for_attachment(server, &a, event.position, MediaMenuKind::Video, cx);
        });
    }
    if shown <= 0. {
        return Some(stage.into_any_element());
    }
    let slide = px(SLIDE * (1. - shown));
    if fullscreen {
        // `right-4 top-4`, fullscreen only: `h-10 w-10 rounded-sm bg-black/70 text-white/85
        // hover:bg-black/85 hover:text-white`, sliding down into place.
        let white = gpui::white();
        stage = stage.child(
            div()
                .id("media-download")
                .group("media-download")
                .absolute()
                .right_4()
                .top(px(16.) - slide)
                .size_10()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .bg(gpui::Hsla { a: 0.7, ..black })
                .hover(|style| style.bg(gpui::Hsla { a: 0.85, ..black }))
                .opacity(shown)
                .cursor_pointer()
                .tooltip(title_tooltip("Download"))
                .child(Icon::new(IconName::ViewerDownload).size(px(16.)).color(gpui::Hsla { a: 0.85, ..white }).group_hover("media-download", white))
                .children(crate::player::probe("media-download"))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    // `handleDownload`: the attachment as the loaded messages have it, else
                    // by the playing one's name.
                    let target = crate::actions::Target {
                        server: Some(server),
                        attachment_id: Some(attachment),
                        filename: filename.clone(),
                        mime: None,
                        url: None,
                        page_url: None,
                    };
                    crate::actions::save(&target, decibell_attachments::AttachmentKind::Video, false, cx);
                }),
        );
    }

    let seek = {
        let target = playback.clone();
        track(
            Drag::VideoSeek,
            playback.clone(),
            Rc::new(move |ratio, _, cx| {
                target.update(cx, |playback, cx| {
                    let duration = playback.video.as_ref().map_or(0, |video| video.status.duration);
                    playback.seek_video((ratio as f64 * duration as f64) as i64, cx);
                })
            }),
            None,
        )
    };
    let volume_track = track(
        Drag::VideoVolume,
        playback.clone(),
        Rc::new(|ratio, _, cx| set_video_volume(ratio as f64, cx)),
        Some(Rc::new(|up, _, cx| {
            let (volume, muted, _, _) = volumes(cx);
            let shown = if muted { 0. } else { volume };
            set_video_volume(shown + if up { 0.05 } else { -0.05 }, cx);
        })),
    );
    let toggle = {
        let playback = playback.clone();
        move |_: &mut Window, cx: &mut App| playback.update(cx, |playback, cx| playback.toggle_video(cx))
    };
    let mute = |_: &mut Window, cx: &mut App| {
        if let Some(model) = cx.try_global::<Model>().cloned() {
            model.settings.update(cx, |settings, cx| settings.change(|values| values.media_video_muted = !values.media_video_muted, cx));
        }
    };
    let full = {
        let playback = playback.clone();
        move |window: &mut Window, cx: &mut App| {
            playback.update(cx, |playback, cx| {
                let on = !playback.fullscreen;
                playback.set_fullscreen(on, window, cx)
            })
        }
    };
    let time = format!("{} / {}", clock(position), clock(duration));
    // The scrim takes no clicks (`pointer-events-none`): only the bars and the buttons do.
    let controls = div()
        .absolute()
        .left_0()
        .right_0()
        // `translate-y-2 opacity-0` ↔ `translate-y-0 opacity-100`.
        .bottom(-slide)
        .opacity(shown)
        .flex()
        .flex_col()
        .px(px(14.))
        .pb(px(10.))
        .pt_10()
        .rounded_bl(corners.bottom_left)
        .rounded_br(corners.bottom_right)
        // `bg-gradient-to-t from-black/85 via-black/50 to-transparent`.
        .bg(gpui::linear_gradient(
            0.,
            gpui::linear_color_stop(gpui::Hsla { a: 0.85, ..black }, 0.),
            gpui::linear_color_stop(gpui::Hsla { a: 0., ..black }, 1.),
        ))
        .child(bar(progress, "media-seek-bar", cx).mb(px(10.)).child(seek))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon_button("media-play", if playing { IconName::ContentPause } else { IconName::ContentPlay }, if playing { "Pause" } else { "Play" }, toggle, cx))
                .child(label_in(time, &style.scrim, decibell_text::Wrapping::NOWRAP, theme.color_text_secondary))
                .child(
                    div()
                        .ml_auto()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(icon_button(
                            "media-mute",
                            if muted { IconName::ContentVolumeMuted } else { IconName::ContentVolume },
                            if muted { "Unmute" } else { "Mute" },
                            mute,
                            cx,
                        ))
                        .child(bar(shown_volume, "media-volume-bar", cx).w_20().child(volume_track)),
                )
                .child(icon_button(
                    "media-fullscreen",
                    IconName::StageExpand,
                    if is_fullscreen { "Exit fullscreen" } else { "Fullscreen" },
                    full,
                    cx,
                )),
        );
    Some(stage.child(controls).into_any_element())
}

/// The fullscreen layer: the playing video over the whole window (`position: fixed; inset: 0`).
pub struct FullscreenLayer {
    playback: Option<Entity<Playback>>,
    _subscription: Option<Subscription>,
}

impl FullscreenLayer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let playback = Playback::global(cx);
        let subscription = playback.as_ref().map(|playback| cx.observe(playback, |_, _, cx| cx.notify()));
        FullscreenLayer {
            playback,
            _subscription: subscription,
        }
    }
}

impl Render for FullscreenLayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(playback) = self.playback.clone() else {
            return div().into_any_element();
        };
        let state = playback.read(cx);
        let Some(video) = state.video.as_ref().filter(|_| state.fullscreen) else {
            return div().into_any_element();
        };
        let (attachment, style) = (video.media.attachment_id, ContentStyle::get(cx, 1.));
        let stage = video_stage(attachment, None, Corners::default(), true, &style, window, cx);
        div()
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::black())
            .children(stage)
            .into_any_element()
    }
}

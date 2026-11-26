//! What the video hub says about a stream, brought to the UI thread: the first picture (the
//! spinner goes), a size change, a fallback to software, a decoder error, a stall ("Reconnecting…"
//! over the last picture until the next one). The hub calls
//! [`video_event`] from its decoder threads (the app installs it as `HubCallbacks::event`); the
//! events cross over one channel drained by one foreground task, and [`StreamStates`] emits a
//! keyed event per stream so only the views showing it redraw.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use decibell_model::Uid;
use decibell_video::{DecodePath, StreamEvent};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global};

/// The hub's events, keyed by stream, from any thread to the UI's drain task.
type EventTx = UnboundedSender<(String, StreamEvent)>;
static TX: OnceLock<Mutex<Option<EventTx>>> = OnceLock::new();

/// The hub's event callback (`HubCallbacks::event`): any thread, never blocks.
pub fn video_event(key: &str, event: StreamEvent) {
    match &event {
        StreamEvent::FellBackToSoftware { reason } => log::warn!("[video] {key}: hardware decode failed, software from here: {reason}"),
        StreamEvent::DecoderError { reason } => log::warn!("[video] {key}: decoder error: {reason}"),
        StreamEvent::DeviceLost => log::warn!("[video] {key}: GPU device lost"),
        StreamEvent::DecoderOpened { codec, path, backend } => {
            log::info!("[video] {key}: {} decoder ({}, {})", codec.label(), backend.label(), path.label())
        }
        StreamEvent::BackendFailed { backend, reason } => log::warn!("[video] {key}: {} gave up, trying the next decoder: {reason}", backend.label()),
        StreamEvent::Stalled => log::warn!("[video] {key}: no picture for {} s, reconnecting", decibell_video::STALLED_AFTER.as_secs()),
        StreamEvent::Resumed => log::info!("[video] {key}: pictures again"),
        StreamEvent::Picture { .. } => {}
    }
    if let Some(tx) = TX.get().and_then(|m| m.lock().ok()).and_then(|g| g.clone()) {
        let _ = tx.unbounded_send((key.to_string(), event));
    }
}

/// What the views know about each displayed stream.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StreamState {
    /// The newest picture's size, once one is up (the spinner goes).
    pub picture: Option<(u32, u32)>,
    pub path: Option<DecodePath>,
    /// The stream had a picture and has had no new one for `decibell_video::STALLED_AFTER`
    /// (`StreamVideoPlayer`'s `stalled`): the views draw [`crate::scrim::reconnecting`] over it.
    pub stalled: bool,
}

#[derive(Default)]
pub struct StreamStates {
    by_uid: HashMap<Uid, StreamState>,
}

/// A stream's state changed (the views showing `uid` redraw).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamStateChanged(pub Uid);

impl EventEmitter<StreamStateChanged> for StreamStates {}

struct Handle(Entity<StreamStates>);
impl Global for Handle {}

impl StreamStates {
    pub fn global(cx: &mut App) -> Entity<StreamStates> {
        if let Some(h) = cx.try_global::<Handle>() {
            return h.0.clone();
        }
        let entity = cx.new(|_| StreamStates::default());
        cx.set_global(Handle(entity.clone()));
        entity
    }

    pub fn get(&self, uid: Uid) -> StreamState {
        self.by_uid.get(&uid).copied().unwrap_or_default()
    }

    fn apply(&mut self, uid: Uid, event: &StreamEvent, cx: &mut Context<Self>) {
        let state = self.by_uid.entry(uid).or_default();
        let before = *state;
        match event {
            StreamEvent::Picture { width, height } => state.picture = Some((*width, *height)),
            StreamEvent::DecoderOpened { path, .. } => state.path = Some(*path),
            StreamEvent::FellBackToSoftware { .. } => state.path = Some(DecodePath::Software),
            StreamEvent::Stalled => state.stalled = true,
            StreamEvent::Resumed => state.stalled = false,
            // A new decoder waits for a keyframe: the last picture stays on screen meanwhile.
            // (A decoder given up for the next in the order is the same: `DecoderOpened` follows.)
            StreamEvent::DecoderError { .. } | StreamEvent::BackendFailed { .. } | StreamEvent::DeviceLost => {}
        }
        if *state != before {
            cx.emit(StreamStateChanged(uid));
        }
    }

    /// A view stopped showing the stream and nothing else does: forget it, so the next viewer
    /// waits for its own first picture.
    pub fn forget(&mut self, uid: Uid, cx: &mut Context<Self>) {
        if self.by_uid.remove(&uid).is_some() {
            cx.emit(StreamStateChanged(uid));
        }
    }
}

/// Start draining the hub's events (once, from [`crate::init`]).
pub fn install(cx: &mut App) {
    let cell = TX.get_or_init(|| Mutex::new(None));
    let (tx, mut rx) = unbounded::<(String, StreamEvent)>();
    if let Ok(mut slot) = cell.lock() {
        *slot = Some(tx);
    }
    let states = StreamStates::global(cx);
    cx.spawn(async move |cx| {
        while let Some(first) = rx.next().await {
            // Everything queued meanwhile goes in the same update: one wake per burst.
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            cx.update(|cx| {
                states.update(cx, |s, cx| {
                    for (key, event) in &batch {
                        if let Ok(uid) = key.parse::<Uid>() {
                            s.apply(uid, event, cx);
                        }
                    }
                })
            });
        }
    })
    .detach();
}

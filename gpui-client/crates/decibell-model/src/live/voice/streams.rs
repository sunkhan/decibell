//! Screen streams (`voiceStore`'s streaming half): who is live where across every
//! connected community, the posters, what we watch and show large, the streams on screen
//! (frame sinks), and our own stream.
//!
//! | Entity | Holds | Changes | Observed by |
//! |---|---|---|---|
//! | [`StreamPresence`] | every live stream by owner (`streamsByUser`), a DM call's streams | stream start / stop / codec / viewer count anywhere | the sidebar's LIVE badges (keyed event), the stage, the profile popup |
//! | [`Thumbnails`] | the posters, as encoded JPEG behind a GPUI `Image` handle + a version | every ~15 s per stream while the voice view wants them | stream tiles (keyed event) |
//! | [`Watching`] | watched streams, the focused one, the mini player's, fullscreen | user actions | the stage, the mini player, the app's window (fullscreen) |
//! | [`StreamViews`] | how many views show each stream (frame sinks) | a player mounting / unmounting | nobody: never notifies |
//! | [`OwnStream`] | our stream: idle / starting / live, the Go Live error, session options | Go Live, stop, failures | the user panel, the Go Live dialog |
//!
//! Stream thumbnails stay plaintext by decision (CLAUDE.md "voice channels are
//! MLS-encrypted").

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use decibell_core::events::StreamInfoPayload;
use gpui::{Context, EventEmitter, SharedString};

use super::codecs::VideoCodec;
use super::rosters::VoiceKey;
use crate::ids::{ChannelId, ServerId, Uid};

/// One live stream (`StreamInfo`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfo {
    pub stream_id: SharedString,
    pub owner_uid: Uid,
    /// The payload's name (fallback; views draw the users store's).
    pub owner_name: SharedString,
    pub has_audio: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub current_codec: VideoCodec,
    /// Unknown = not enforced.
    pub enforced_codec: VideoCodec,
    pub watcher_count: u32,
}

impl From<&StreamInfoPayload> for StreamInfo {
    fn from(s: &StreamInfoPayload) -> Self {
        StreamInfo {
            stream_id: s.stream_id.clone().into(),
            owner_uid: s.owner_uid,
            owner_name: s.owner_username.clone().into(),
            has_audio: s.has_audio,
            width: s.resolution_width,
            height: s.resolution_height,
            fps: s.fps,
            current_codec: VideoCodec::from_wire(s.current_codec),
            enforced_codec: VideoCodec::from_wire(s.enforced_codec),
            watcher_count: s.watcher_count,
        }
    }
}

/// Where a user's stream is live (`StreamLocation`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StreamLocation {
    pub server: ServerId,
    pub channel: ChannelId,
    pub info: StreamInfo,
}

/// The streams of a channel changed (or of the DM call: `key` none).
#[derive(Debug, Clone, PartialEq)]
pub struct StreamsChanged {
    pub key: Option<VoiceKey>,
}

#[derive(Default)]
pub struct StreamPresence {
    by_user: HashMap<Uid, StreamLocation>,
    /// Each channel's streams in the server's order (`activeStreams` keeps the list as it came).
    order: HashMap<VoiceKey, Vec<Uid>>,
    /// A DM call's streams (announced over central: STREAM_START / STREAM_STOP).
    call: Vec<StreamInfo>,
}

impl EventEmitter<StreamsChanged> for StreamPresence {}

impl StreamPresence {
    /// Is `uid` live anywhere, and where (the profile popup's O(1) lookup).
    pub fn of_user(&self, uid: Uid) -> Option<&StreamLocation> {
        self.by_user.get(&uid)
    }

    /// The streams live in one channel.
    pub fn in_channel<'a>(&'a self, key: &'a VoiceKey) -> impl Iterator<Item = &'a StreamInfo> + 'a {
        self.by_user.values().filter(move |l| l.server == key.0 && l.channel == key.1).map(|l| &l.info)
    }

    /// The session's streams (`activeStreams`): the connected channel's, or the call's.
    pub fn session(&self, connected: Option<&VoiceKey>, in_call: bool) -> Vec<StreamInfo> {
        if in_call {
            return self.call.clone();
        }
        let Some(key) = connected else { return Vec::new() };
        let mut list: Vec<StreamInfo> = self.in_channel(key).cloned().collect();
        // The server's order (`activeStreams`), the stream id where it is unknown.
        let order = self.order.get(key);
        let rank = |s: &StreamInfo| order.and_then(|o| o.iter().position(|u| *u == s.owner_uid)).unwrap_or(usize::MAX);
        list.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.stream_id.cmp(&b.stream_id)));
        list
    }

    pub fn all(&self) -> impl Iterator<Item = (&Uid, &StreamLocation)> {
        self.by_user.iter()
    }

    /// A `stream_presence_updated`: the channel's full stream list. Owners no longer
    /// listed there go; the listed ones are (re)placed. Returns the owners listed.
    pub fn apply(&mut self, key: VoiceKey, streams: Vec<StreamInfo>, cx: &mut Context<Self>) -> HashSet<Uid> {
        let owners: HashSet<Uid> = streams.iter().map(|s| s.owner_uid).collect();
        let order: Vec<Uid> = streams.iter().map(|s| s.owner_uid).collect();
        let before = self.by_user.len();
        let mut changed = self.order.get(&key) != Some(&order);
        self.order.insert(key.clone(), order);
        self.by_user.retain(|uid, l| !(l.server == key.0 && l.channel == key.1 && !owners.contains(uid)));
        changed |= self.by_user.len() != before;
        for info in streams {
            let location = StreamLocation { server: key.0, channel: key.1.clone(), info };
            if self.by_user.get(&location.info.owner_uid) != Some(&location) {
                self.by_user.insert(location.info.owner_uid, location);
                changed = true;
            }
        }
        if changed {
            cx.emit(StreamsChanged { key: Some(key) });
            cx.notify();
        }
        owners
    }

    /// A DM call's stream started (ours or the peer's).
    pub fn call_started(&mut self, info: StreamInfo, cx: &mut Context<Self>) {
        self.call.retain(|s| s.owner_uid != info.owner_uid);
        self.call.push(info);
        cx.emit(StreamsChanged { key: None });
        cx.notify();
    }

    /// A DM call's stream stopped. Returns whether it was listed.
    pub fn call_stopped(&mut self, owner: Uid, cx: &mut Context<Self>) -> bool {
        let before = self.call.len();
        self.call.retain(|s| s.owner_uid != owner);
        let had = self.call.len() != before;
        if had {
            cx.emit(StreamsChanged { key: None });
            cx.notify();
        }
        had
    }

    pub fn clear_call(&mut self, cx: &mut Context<Self>) {
        if !self.call.is_empty() {
            self.call.clear();
            cx.emit(StreamsChanged { key: None });
            cx.notify();
        }
    }

    /// Drop one community's streams (all, or only those of channels not in `keep`).
    pub fn clear_server(&mut self, server: ServerId, keep: Option<&HashSet<ChannelId>>, cx: &mut Context<Self>) {
        let mut keys: Vec<VoiceKey> = Vec::new();
        self.by_user.retain(|_, l| {
            let drop = l.server == server && !keep.is_some_and(|k| k.contains(&l.channel));
            if drop {
                keys.push((l.server, l.channel.clone()));
            }
            !drop
        });
        keys.sort();
        keys.dedup();
        self.order.retain(|k, _| !(k.0 == server && !keep.is_some_and(|keep| keep.contains(&k.1))));
        for key in &keys {
            cx.emit(StreamsChanged { key: Some(key.clone()) });
        }
        if !keys.is_empty() {
            cx.notify();
        }
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.by_user.clear();
        self.order.clear();
        self.call.clear();
        cx.emit(StreamsChanged { key: None });
        cx.notify();
    }
}

// ── Posters ─────────────────────────────────────────────────────────────────

/// A stream's poster: the JPEG as the streamer sent it, behind GPUI's `Image` handle
/// (bytes + format + a content id; GPUI decodes it when a view draws it with `img()`),
/// and a version that moves with every new poster.
#[derive(Clone)]
pub struct Thumbnail {
    pub image: Arc<gpui::Image>,
    pub version: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbnailChanged(pub Uid);

#[derive(Default)]
pub struct Thumbnails {
    by_uid: HashMap<Uid, Thumbnail>,
    next_version: u64,
    /// Views that want posters (the voice view): they are kept only while one does
    /// (`useStreamThumbnails` mounted).
    wanted: usize,
    /// Streams already fetched on demand this channel visit.
    pub(crate) requested: HashSet<SharedString>,
}

impl EventEmitter<ThumbnailChanged> for Thumbnails {}

impl Thumbnails {
    pub fn get(&self, uid: Uid) -> Option<&Thumbnail> {
        self.by_uid.get(&uid)
    }

    pub fn wanted(&self) -> bool {
        self.wanted > 0
    }

    pub fn len(&self) -> usize {
        self.by_uid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_uid.is_empty()
    }

    pub(crate) fn retain(&mut self) -> bool {
        self.wanted += 1;
        self.wanted == 1
    }

    /// The last view that wanted posters went: what it fetched on demand is forgotten, so the
    /// next visit fetches again a stream whose fetch came back empty (`useStreamThumbnails`'
    /// `requested` set lives as long as its mount).
    pub(crate) fn release(&mut self) {
        self.wanted = self.wanted.saturating_sub(1);
        if self.wanted == 0 {
            self.requested.clear();
        }
    }

    pub fn set(&mut self, uid: Uid, jpeg: Vec<u8>, cx: &mut Context<Self>) {
        self.next_version += 1;
        let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Jpeg, jpeg));
        self.by_uid.insert(uid, Thumbnail { image, version: self.next_version });
        cx.emit(ThumbnailChanged(uid));
        cx.notify();
    }

    /// Leaving voice (or a new channel visit): every poster and fetch mark goes.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.requested.clear();
        if self.by_uid.is_empty() {
            return;
        }
        for uid in std::mem::take(&mut self.by_uid).into_keys() {
            cx.emit(ThumbnailChanged(uid));
        }
        cx.notify();
    }
}

// ── Watching ────────────────────────────────────────────────────────────────

#[derive(Default, Debug, Clone, PartialEq)]
pub struct Watching {
    /// Streams we subscribed to (each its own decoder), oldest first.
    watching: Vec<Uid>,
    /// The one the stage shows large (`fullscreenStream`).
    pub focused: Option<Uid>,
    /// The one loaded in the persistent player (`pipStream`), kept warm on the grid.
    pub pip: Option<Uid>,
    /// The window is fullscreen for a stream (`isStreamFullscreen`): the app applies it.
    pub fullscreen: bool,
}

impl Watching {
    pub fn list(&self) -> &[Uid] {
        &self.watching
    }

    pub fn is_watching(&self, uid: Uid) -> bool {
        self.watching.contains(&uid)
    }

    pub fn add(&mut self, uid: Uid, cx: &mut Context<Self>) {
        if !self.watching.contains(&uid) {
            self.watching.push(uid);
            cx.notify();
        }
    }

    /// Stop showing `uid`: not watched, not focused, not in the mini player. The window leaves
    /// fullscreen with the focused stream (`CallStage`'s "the focused stream ended or was
    /// unwatched while fullscreen → come back"; the voice view's paths did it by hand).
    pub fn remove(&mut self, uid: Uid, cx: &mut Context<Self>) {
        let before = self.clone();
        self.watching.retain(|u| *u != uid);
        if self.focused == Some(uid) {
            self.focused = None;
            self.fullscreen = false;
        }
        if self.pip == Some(uid) {
            self.pip = None;
        }
        if *self != before {
            cx.notify();
        }
    }

    pub fn set_focused(&mut self, uid: Option<Uid>, cx: &mut Context<Self>) {
        if self.focused != uid {
            self.focused = uid;
            cx.notify();
        }
    }

    pub fn set_pip(&mut self, uid: Option<Uid>, cx: &mut Context<Self>) {
        if self.pip != uid {
            self.pip = uid;
            cx.notify();
        }
    }

    pub fn set_fullscreen(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.fullscreen != on {
            self.fullscreen = on;
            cx.notify();
        }
    }

    /// A channel switch or a move: watches, focus and the mini player go (the server
    /// dropped our watches when we left the old channel).
    pub fn clear_watches(&mut self, cx: &mut Context<Self>) {
        if !self.watching.is_empty() || self.focused.is_some() || self.pip.is_some() {
            self.watching.clear();
            self.focused = None;
            self.pip = None;
            cx.notify();
        }
    }

    pub fn reset(&mut self, cx: &mut Context<Self>) {
        if *self != Watching::default() {
            *self = Watching::default();
            cx.notify();
        }
    }
}

// ── Streams on screen ───────────────────────────────────────────────────────

/// How many views draw each stream: the core forwards frames only for streams some view
/// shows (`set_stream_frame_sink`, keyed by the streamer's uid in decimal), so the video
/// hub receives what is displayed and nothing else. A view retains on mount and releases
/// on unmount (`actions::voice::show_stream` / `hide_stream`). Never notifies.
#[derive(Default)]
pub struct StreamViews {
    counts: HashMap<Uid, usize>,
}

impl StreamViews {
    /// Returns true for the first view of `uid` (the sink must be switched on).
    pub fn retain(&mut self, uid: Uid) -> bool {
        let n = self.counts.entry(uid).or_insert(0);
        *n += 1;
        *n == 1
    }

    /// Returns true when the last view of `uid` went (the sink must be switched off).
    pub fn release(&mut self, uid: Uid) -> bool {
        match self.counts.get_mut(&uid) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                self.counts.remove(&uid);
                true
            }
            None => false,
        }
    }

    pub fn shown(&self, uid: Uid) -> usize {
        self.counts.get(&uid).copied().unwrap_or(0)
    }

    pub fn keys(&self) -> impl Iterator<Item = Uid> + '_ {
        self.counts.keys().copied()
    }
}

// ── Our own stream ──────────────────────────────────────────────────────────

/// What our live stream encodes and where it is announced.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LiveStream {
    pub codec: VideoCodec,
    /// The codec the stream was announced locked to (`StartStreamReq.enforced_codec`):
    /// Advanced's pinned codec, or `Unknown` for Auto. A carry to another channel
    /// re-announces the same.
    pub enforced: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub has_audio: bool,
    /// The native encoder in use, as the core names it (`StartScreenShareResult.encoder`:
    /// "NVENC (NVIDIA)", … "Software (SVT-AV1)"); empty when it didn't say.
    pub encoder: SharedString,
    /// The community channel it is announced in (none in a DM call). A stream taken
    /// along on a switch moves with us.
    pub server: Option<ServerId>,
    pub channel: Option<ChannelId>,
}

#[derive(Debug, Clone, PartialEq, Default, serde::Serialize)]
#[serde(tag = "phase", rename_all = "lowercase")]
pub enum OwnStreamPhase {
    #[default]
    Idle,
    /// Go Live pressed; the core is opening capture and the encoder.
    Starting,
    Live(LiveStream),
}

pub struct OwnStream {
    pub phase: OwnStreamPhase,
    /// The Go Live dialog's error (what `start_screen_share` refused with).
    pub error: Option<SharedString>,
    /// Embed the cursor (session-only, not persisted: the renderer's default on).
    pub include_cursor: bool,
    /// Windows: the source the picker chose (the portal picks on Linux).
    pub source_id: Option<String>,
    /// Bumped by every start: a start that finds a newer one when its answer lands
    /// is stale ("Stream ended while starting").
    pub(crate) attempt: u64,
}

impl Default for OwnStream {
    fn default() -> Self {
        OwnStream { phase: OwnStreamPhase::Idle, error: None, include_cursor: true, source_id: None, attempt: 0 }
    }
}

impl OwnStream {
    /// `isStreaming`.
    pub fn is_live(&self) -> bool {
        matches!(self.phase, OwnStreamPhase::Live(_))
    }

    pub fn live(&self) -> Option<&LiveStream> {
        match &self.phase {
            OwnStreamPhase::Live(l) => Some(l),
            _ => None,
        }
    }

    pub fn set_phase(&mut self, phase: OwnStreamPhase, cx: &mut Context<Self>) {
        if self.phase != phase {
            self.phase = phase;
            cx.notify();
        }
    }

    pub fn set_error(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        if self.error != error {
            self.error = error;
            cx.notify();
        }
    }

    pub fn set_include_cursor(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.include_cursor != on {
            self.include_cursor = on;
            cx.notify();
        }
    }

    pub fn set_source(&mut self, source: Option<String>, cx: &mut Context<Self>) {
        if self.source_id != source {
            self.source_id = source;
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[test]
    fn a_new_visit_of_the_voice_view_fetches_again_what_came_back_empty() {
        let mut t = Thumbnails::default();
        assert!(t.retain());
        t.requested.insert("s1".into());
        // A second view (the call stage) holds it: the mark stays.
        assert!(!t.retain());
        t.release();
        assert!(t.requested.contains("s1"));
        // The last view goes: the next mount asks again.
        t.release();
        assert!(t.requested.is_empty());
        assert!(!t.wanted());
    }

    fn info(owner: Uid) -> StreamInfo {
        StreamInfo {
            stream_id: format!("{owner}_screen").into(),
            owner_uid: owner,
            owner_name: "x".into(),
            has_audio: false,
            width: 1920,
            height: 1080,
            fps: 60,
            current_codec: VideoCodec::H264Hw,
            enforced_codec: VideoCodec::Unknown,
            watcher_count: 0,
        }
    }

    #[gpui::test]
    fn a_channels_list_replaces_only_that_channel(cx: &mut TestAppContext) {
        let p = cx.new(|_| StreamPresence::default());
        let lounge: VoiceKey = (ServerId::from_central(1), "lounge".into());
        let squad: VoiceKey = (ServerId::from_central(1), "squad".into());
        p.update(cx, |p, cx| {
            p.apply(lounge.clone(), vec![info(2), info(3)], cx);
            p.apply(squad.clone(), vec![info(4)], cx);
            p.apply(lounge.clone(), vec![info(3)], cx);
        });
        p.read_with(cx, |p, _| {
            assert!(p.of_user(2).is_none());
            assert_eq!(p.of_user(3).unwrap().channel.as_ref(), "lounge");
            assert_eq!(p.of_user(4).unwrap().channel.as_ref(), "squad");
            assert_eq!(p.session(Some(&lounge), false).len(), 1);
            assert_eq!(p.session(Some(&squad), false)[0].owner_uid, 4);
            assert!(p.session(None, false).is_empty());
        });
        let keep: HashSet<ChannelId> = ["squad".into()].into();
        p.update(cx, |p, cx| p.clear_server(ServerId::from_central(1), Some(&keep), cx));
        p.read_with(cx, |p, _| assert!(p.of_user(3).is_none() && p.of_user(4).is_some()));
    }

    #[gpui::test]
    fn the_session_keeps_the_servers_order(cx: &mut TestAppContext) {
        let p = cx.new(|_| StreamPresence::default());
        let lounge: VoiceKey = (ServerId::from_central(1), "lounge".into());
        p.update(cx, |p, cx| p.apply(lounge.clone(), vec![info(3), info(2)], cx));
        p.read_with(cx, |p, _| {
            let order: Vec<Uid> = p.session(Some(&lounge), false).iter().map(|s| s.owner_uid).collect();
            assert_eq!(order, [3, 2], "carol's (listed first) before bob's, not by stream id");
        });
    }

    #[gpui::test]
    fn removing_a_watch_drops_its_focus(cx: &mut TestAppContext) {
        let w = cx.new(|_| Watching::default());
        w.update(cx, |w, cx| {
            w.add(2, cx);
            w.add(3, cx);
            w.add(2, cx);
            w.set_focused(Some(2), cx);
            w.set_pip(Some(2), cx);
            w.remove(2, cx);
        });
        w.read_with(cx, |w, _| {
            assert_eq!(w.list(), &[3]);
            assert_eq!((w.focused, w.pip), (None, None));
        });
    }

    #[test]
    fn frame_sinks_count_views() {
        let mut v = StreamViews::default();
        assert!(v.retain(2));
        assert!(!v.retain(2));
        assert!(!v.release(2));
        assert!(v.release(2));
        assert!(!v.release(2), "an unbalanced release is ignored");
    }
}

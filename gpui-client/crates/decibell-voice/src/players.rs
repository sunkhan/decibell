//! The stream players (`StreamPipManager.tsx`, `streamPipHost.ts`): one decoder per stream, any
//! number of [`VideoView`]s on it.
//!
//! The renderer rendered one persistent `StreamVideoPlayer` and moved its DOM node between the
//! grid tile, the focused view and the mini-player so the decoder survived the moves. Here the
//! video hub already shares one decoder between every [`Viewer`] of a stream, so each place just
//! holds a viewer; what has to survive the moves is that some viewer always exists while the
//! stream is "loaded" (`watching.pip`): this global holds one for it, exactly as
//! `StreamPipManager` kept its player mounted. Loading rules (`StreamPipManager`): focusing a
//! stream loads it; a stream no longer watched or no longer live is unloaded.
//!
//! Also here, because they follow the same stores: the window's fullscreen follows
//! `watching.fullscreen` (the model leaves applying it to the app), and a stream that is no longer
//! live anywhere is removed from the hub (its decoder and textures freed).

use std::collections::HashSet;

use decibell_model::{Model, Uid};
use decibell_video::{VideoHub, Viewer};
use gpui::{App, AppContext as _, Context, Entity, Global, Subscription};

pub struct Players {
    /// The loaded stream and the viewer that keeps its decoder alive across views.
    pip: Option<(Uid, Viewer)>,
    /// Owners live anywhere at the last look (to remove the ones that ended from the hub).
    live: HashSet<Uid>,
    fullscreen: bool,
    _subscriptions: Vec<Subscription>,
}

struct Handle(Entity<Players>);
impl Global for Handle {}

/// A viewer of `uid`'s stream: while it lives the hub decodes the stream (one decoder for every
/// viewer of it). Create the new viewer before dropping the old one when a stream moves.
pub fn viewer(uid: Uid, cx: &mut App) -> Viewer {
    VideoHub::global(cx).stream(&uid.to_string()).viewer()
}

impl Players {
    /// Installed once by [`crate::init`]: creates nothing until a stream is loaded.
    pub fn install(cx: &mut App) {
        if cx.has_global::<Handle>() {
            return;
        }
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let entity = cx.new(|cx: &mut Context<Players>| {
            let voice = &model.live.voice;
            let subscriptions = vec![
                cx.observe(&voice.watching, |this, _, cx| this.follow(cx)),
                cx.observe(&voice.streams, |this, _, cx| this.follow(cx)),
                cx.observe(&voice.session, |this, _, cx| this.follow(cx)),
            ];
            Players { pip: None, live: HashSet::new(), fullscreen: false, _subscriptions: subscriptions }
        });
        cx.set_global(Handle(entity));
    }

    /// The loaded stream (`pipStream`), if its viewer is held.
    pub fn loaded(cx: &App) -> Option<Uid> {
        cx.try_global::<Handle>().and_then(|h| h.0.read(cx).pip.as_ref().map(|(uid, _)| *uid))
    }

    fn follow(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let (connected, in_call) = {
            let s = voice.session.read(cx);
            (s.connected(), s.call_peer.is_some())
        };
        let session: HashSet<Uid> =
            voice.streams.read(cx).session(connected.as_ref(), in_call).iter().map(|s| s.owner_uid).collect();
        let (focused, pip, watched, fullscreen) = {
            let w = voice.watching.read(cx);
            (w.focused, w.pip, w.list().to_vec(), w.fullscreen)
        };
        // Focusing a stream loads it into the persistent player.
        let mut next = pip;
        if let Some(focused) = focused
            && next != Some(focused)
        {
            next = Some(focused);
        }
        // A stream no longer watched or no longer live is unloaded.
        if let Some(uid) = next
            && !(watched.contains(&uid) && session.contains(&uid))
        {
            next = None;
        }
        if next != pip {
            voice.watching.update(cx, |w, cx| w.set_pip(next, cx));
        }
        match next {
            Some(uid) if self.pip.as_ref().map(|(u, _)| *u) != Some(uid) => {
                // The new viewer first, then the old one goes. It is never drawn itself (the
                // places that show the stream hold their own): a keeper, so a hidden window still
                // pauses the stream.
                let viewer = VideoHub::global(cx).stream(&uid.to_string()).keeper();
                self.pip = Some((uid, viewer));
            }
            None => self.pip = None,
            _ => {}
        }

        // Streams that ended anywhere: their hub entries go (decoder, textures).
        let all: HashSet<Uid> = {
            let streams = voice.streams.read(cx);
            let mut all: HashSet<Uid> = streams.all().map(|(uid, _)| *uid).collect();
            all.extend(streams.session(None, true).iter().map(|s| s.owner_uid));
            all
        };
        let ended: Vec<Uid> = self.live.difference(&all).copied().collect();
        if !ended.is_empty() {
            let hub = VideoHub::global(cx);
            for uid in ended {
                hub.remove(&uid.to_string());
            }
        }
        self.live = all;

        // The window follows `watching.fullscreen` (StreamViewPanel's `appWindow.setFullscreen`).
        if fullscreen != self.fullscreen {
            self.fullscreen = fullscreen;
            cx.defer(move |cx| {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| {
                        if window.is_fullscreen() != fullscreen {
                            window.toggle_fullscreen();
                        }
                    });
                }
            });
        }
    }
}

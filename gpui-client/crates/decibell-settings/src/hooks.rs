//! What the app hands Settings, and the requests Settings makes of features drawn elsewhere.
//!
//! - The app's version (About shows it when the updater reports none: never the renderer's
//!   hard-coded fallback).
//! - The updater's buttons ([`UpdateHooks`]): "Check now", "Restart", "Open release page". The
//!   updater is a later task; without hooks the buttons do nothing (and the model's
//!   `UpdateStore` reads "disabled", so About says so).
//! - The out-of-voice mic test ([`crate::MicTest`]): the core has no capture outside a voice
//!   session yet; [`crate::UnavailableMicTest`] until it does.
//! - The picture editor's way in ([`PictureRequests`]): Account's picture and its button ask for
//!   it; the avatar editor (another crate) subscribes and opens over Settings.
//! - A fixed clock for screens ([`SettingsHooks::now`]): "Last active 3 h ago" must be the same
//!   picture on every run.

use std::rc::Rc;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString};

use crate::mic_test::{MicTest, UnavailableMicTest};

/// The updater's buttons (`window.decibell.update.*`).
pub trait UpdateHooks: 'static {
    fn check(&self, cx: &mut App);
    fn quit_and_install(&self, cx: &mut App);
    fn open_release_page(&self, cx: &mut App);
}

pub struct SettingsHooks {
    /// The app's version (`env!("CARGO_PKG_VERSION")` of `decibell-app`).
    pub app_version: SharedString,
    pub update: Option<Rc<dyn UpdateHooks>>,
    pub mic_test: Rc<dyn MicTest>,
    /// Unix seconds that stand for "now" (screens); `None`: the system clock.
    pub now: Option<i64>,
    pub pictures: Entity<PictureRequests>,
    /// What the screens show that only interaction or a core would (see [`Staged`]).
    pub staged: Staged,
}

impl Global for SettingsHooks {}

/// State the screens stage in views as they are built (no core, no input): the rename dialog
/// open, the first keybind recording, a level on the mic meter, the encoders' backends.
#[derive(Default, Clone)]
pub struct Staged {
    pub open_rename: bool,
    pub record_first: bool,
    pub meter_level: Option<f32>,
    pub encoder_labels: Option<std::collections::HashMap<decibell_model::live::voice::codecs::VideoCodec, SharedString>>,
}

/// Installed by [`crate::init`] (and by the screens) if the app hasn't yet.
pub(crate) fn ensure(cx: &mut App) {
    if cx.has_global::<SettingsHooks>() {
        return;
    }
    let pictures = cx.new(|_| PictureRequests);
    cx.set_global(SettingsHooks {
        app_version: SharedString::new_static(env!("CARGO_PKG_VERSION")),
        update: None,
        mic_test: Rc::new(UnavailableMicTest),
        now: None,
        pictures,
        staged: Staged::default(),
    });
}

impl SettingsHooks {
    pub fn get(cx: &mut App) -> &SettingsHooks {
        ensure(cx);
        cx.global::<SettingsHooks>()
    }

    pub fn update(cx: &mut App, f: impl FnOnce(&mut SettingsHooks)) {
        ensure(cx);
        f(cx.global_mut::<SettingsHooks>());
    }

    /// The app's version (`decibell-app`'s, set by the app).
    pub fn set_app_version(version: impl Into<SharedString>, cx: &mut App) {
        let version = version.into();
        Self::update(cx, |h| h.app_version = version);
    }

    pub fn set_update_hooks(hooks: Rc<dyn UpdateHooks>, cx: &mut App) {
        Self::update(cx, |h| h.update = Some(hooks));
    }

    pub fn set_mic_test(test: Rc<dyn MicTest>, cx: &mut App) {
        Self::update(cx, |h| h.mic_test = test);
    }

    /// Unix seconds, from the fixed clock when one is set.
    pub fn now(cx: &App) -> i64 {
        if let Some(h) = cx.try_global::<SettingsHooks>()
            && let Some(now) = h.now
        {
            return now;
        }
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }
}

/// What Account asks of the picture editor (`AccountTab.tsx`: the picture button and "Change
/// picture" open the file dialog, then `AvatarCropperModal`). The editor subscribes to this
/// entity (`SettingsHooks::pictures`) and draws over Settings; nothing here depends on it.
pub struct PictureRequests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PictureEvent {
    /// Pick a file for our avatar and open the editor on it.
    ChangeAvatar,
}

impl EventEmitter<PictureEvent> for PictureRequests {}

impl PictureRequests {
    pub fn request(&mut self, event: PictureEvent, cx: &mut Context<Self>) {
        log::debug!("[settings] picture request {event:?}");
        cx.emit(event);
    }
}

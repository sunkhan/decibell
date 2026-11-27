//! The avatar editor, joined to Settings → Account (`AccountTab.tsx` + `AvatarCropperModal.tsx`):
//! the settings crate asks for it (`SettingsHooks::pictures`: the picture button and "Change
//! picture" open the file dialog, a picture dropped on the card opens the editor on it), the
//! picture-editor crate draws it, and neither depends on the other. This overlay (registered by
//! `registry::join_features`, after Settings' own, so it draws over the Settings window) holds the
//! editor while it is open and drops it when it closes, with everything it decoded.
//!
//! Save uploads through the model (`actions::presence::upload_avatar`: a JPEG of at most 1 MiB,
//! central's refusal texts, the renderer's "Couldn't save your picture." otherwise); the editor
//! reads "Saving…" until central answers and shows a refusal itself. On success the core announces
//! our new version (`avatar_changed`), so every `UserAvatar` follows through the avatars store, and
//! Account shows the saved JPEG until the store has fetched it (`PictureEvent::AvatarSaved`).

use std::path::PathBuf;
use std::sync::Arc;

use decibell_model::live::presence::DisplayStatus;
use decibell_model::{Model, actions};
use decibell_picture_editor::picture::looks_like_picture;
use decibell_picture_editor::{EditorOptions, PictureEditor, PictureEditorEvent, PictureSource};
use decibell_settings::{PictureEvent, SettingsHooks};
use decibell_ui::Presence;
use gpui::{
    AppContext as _, Context, Entity, IntoElement, PathPromptOptions, Render, SharedString, Subscription, Task, Window, div,
    prelude::*,
};

/// Called by `registry::join_features`.
pub fn join(cx: &mut gpui::App) {
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| AvatarEditorHost::new(window, cx)).into());
}

pub struct AvatarEditorHost {
    editor: Option<Entity<PictureEditor>>,
    /// The avatar version the open editor would replace (Account's stand-in waits for another).
    before: SharedString,
    _requests: Subscription,
    _editor_events: Option<Subscription>,
    _upload: Option<Task<()>>,
}

thread_local! {
    /// The open editor, for the self-test (`avatar-editor`).
    static SHOWN: std::cell::RefCell<Option<gpui::WeakEntity<PictureEditor>>> = const { std::cell::RefCell::new(None) };
}

/// The avatar editor on screen, if one is open (self-tests).
pub fn shown_editor() -> Option<Entity<PictureEditor>> {
    SHOWN.with(|s| s.borrow().as_ref().and_then(|w| w.upgrade()))
}

impl AvatarEditorHost {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pictures = SettingsHooks::get(cx).pictures.clone();
        let requests = cx.subscribe_in(&pictures, window, |this: &mut Self, _, event: &PictureEvent, window, cx| match event {
            PictureEvent::ChangeAvatar => this.choose(window, cx),
            PictureEvent::AvatarDropped(paths) => {
                // `pictureFromList`: the first file that looks like a picture.
                if let Some(path) = paths.iter().find(|p| p.to_str().is_some_and(looks_like_picture)) {
                    this.open(PictureSource::Path(path.clone()), window, cx);
                }
            }
            PictureEvent::AvatarSaved { .. } => {}
        });
        Self { editor: None, before: SharedString::default(), _requests: requests, _editor_events: None, _upload: None }
    }

    /// The file dialog (`<input type="file" accept={PICTURE_ACCEPT}>`), then the editor.
    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
            filters: decibell_picture_editor::picture_filters(),
        });
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path): Option<PathBuf> = paths.into_iter().next() else { return };
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| this.open(PictureSource::Path(path), window, cx));
            });
        })
        .detach();
    }

    pub fn open(&mut self, source: PictureSource, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor.is_some() {
            return;
        }
        let model = Model::global(cx).clone();
        let uid = model.auth.read(cx).uid;
        let fallback = model.auth.read(cx).username.clone().unwrap_or_default();
        let name = model.name_of(uid, &fallback, cx);
        // `useOwnDisplayStatus()`: our status as others see it (invisible draws offline).
        let status = match model.live.presence.own.read(cx).display() {
            DisplayStatus::Online => Presence::Online,
            DisplayStatus::Away => Presence::Away,
            DisplayStatus::Dnd => Presence::Dnd,
            DisplayStatus::Offline | DisplayStatus::Invisible => Presence::Offline,
        };
        self.before = model.live.presence.avatars.read(cx).get(uid).map(|e| e.version.clone()).unwrap_or_default();
        let editor = cx.new(|cx| PictureEditor::new(source, EditorOptions::avatar(uid, name, status), window, cx));
        self._editor_events = Some(cx.subscribe(&editor, |this: &mut Self, editor, event: &PictureEditorEvent, cx| match event {
            PictureEditorEvent::Save(jpeg) => this.save(jpeg.clone(), editor, cx),
            PictureEditorEvent::Closed { saved } => {
                if let Some(jpeg) = saved {
                    let pictures = SettingsHooks::get(cx).pictures.clone();
                    let event = PictureEvent::AvatarSaved { jpeg: jpeg.clone(), before: this.before.clone() };
                    pictures.update(cx, |p, cx| p.request(event, cx));
                }
                this.editor = None;
                this._editor_events = None;
                this._upload = None;
                SHOWN.with(|s| s.borrow_mut().take());
                cx.notify();
            }
        }));
        SHOWN.with(|s| *s.borrow_mut() = Some(editor.downgrade()));
        self.editor = Some(editor);
        cx.notify();
    }

    /// `onSave`: upload, then tell the editor how it went (it closes on success, shows the
    /// refusal otherwise).
    fn save(&mut self, jpeg: Arc<Vec<u8>>, editor: Entity<PictureEditor>, cx: &mut Context<Self>) {
        let upload = actions::presence::upload_avatar((*jpeg).clone());
        self._upload = Some(cx.spawn(async move |_, cx| {
            let result = upload.await;
            if let Err(e) = &result {
                log::warn!("[avatar] save refused: {e}");
            }
            editor.update(cx, |e, cx| e.save_done(result, cx));
        }));
    }
}

impl Render for AvatarEditorHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // The editor is a modal: it draws its own backdrop over everything.
        div().children(self.editor.clone())
    }
}

/// The self-test: development builds only (the ship flavour carries no test code).
#[cfg(feature = "dev")]
mod selftest;
#[cfg(feature = "dev")]
pub use selftest::{checks, selftests};

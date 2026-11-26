//! Change password (`ChangePasswordModal.tsx`; spec `2026-10-19-devices-and-password-design.md`):
//! central checks the current password (a wrong one counts as a failed sign-in) and signs every
//! other device out; this one stays signed in and the core saves the new password for the
//! automatic sign-in. Open while `model.live.devices.password` is (Account's "Change password"
//! opens it): an overlay of its own, over the Settings window.

use std::rc::Rc;

use decibell_model::live::devices::MIN_PASSWORD_CHARS;
use decibell_model::{Model, actions};
use decibell_text::input::InputState;
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*};

use super::{DialogParts, dialog, field, submit_on_enter};

pub struct PasswordOverlay {
    open: bool,
    current: Entity<InputState>,
    new: Entity<InputState>,
    repeat: Entity<InputState>,
    /// The flow's error and busy flag as last shown (kept through the fade-out).
    error: Option<SharedString>,
    busy: bool,
    _subscriptions: Vec<Subscription>,
}

impl PasswordOverlay {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let masked = |placeholder: SharedString, window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::password(window, cx).placeholder(placeholder))
        };
        let current = masked("Current password".into(), window, cx);
        let new = masked(format!("New password (at least {MIN_PASSWORD_CHARS} characters)").into(), window, cx);
        let repeat = masked("New password again".into(), window, cx);
        let flow = cx.observe_in(&model.live.devices, window, |this, devices, window, cx| {
            let flow = devices.read(cx).password.clone();
            let open = flow.is_some();
            let (error, busy) = flow.map_or((this.error.clone(), false), |f| (f.error, f.busy));
            if open && !this.open {
                // A fresh dialog (the renderer clears the fields on every open), the first field
                // focused.
                for input in [&this.current, &this.new, &this.repeat] {
                    input.update(cx, |input, cx| input.clear(cx));
                }
                this.current.update(cx, |input, cx| input.focus(window, cx));
            }
            if open != this.open || error != this.error || busy != this.busy {
                this.open = open;
                this.error = error;
                this.busy = busy;
                cx.notify();
            }
        });
        let subscriptions = vec![
            flow,
            submit_on_enter(&current, Self::submit, cx),
            submit_on_enter(&new, Self::submit, cx),
            submit_on_enter(&repeat, Self::submit, cx),
        ];
        let mut this = Self { open: false, current, new, repeat, error: None, busy: false, _subscriptions: subscriptions };
        if let Some(flow) = model.live.devices.read(cx).password.clone() {
            this.open = true;
            this.error = flow.error;
            this.busy = flow.busy;
            this.current.update(cx, |input, cx| input.focus(window, cx));
        }
        this
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if !self.open || self.busy {
            return;
        }
        let value = |input: &Entity<InputState>| input.read(cx).value().to_string();
        let (current, new, repeat) = (value(&self.current), value(&self.new), value(&self.repeat));
        actions::devices::change_password(current, new, repeat, cx);
    }
}

impl Render for PasswordOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let weak = cx.weak_entity();
        let submit: super::Handler = Rc::new(move |_: &mut Window, cx: &mut App| {
            if let Some(v) = weak.upgrade() {
                v.update(cx, |v, cx| v.submit(cx));
            }
        });
        let inputs = vec![
            field("password-current", &self.current, busy),
            field("password-new", &self.new, busy),
            field("password-repeat", &self.repeat, busy),
        ];
        let cancel = |_: &mut Window, cx: &mut App| {
            let devices = Model::global(cx).live.devices.clone();
            devices.update(cx, |d, cx| d.close_password(cx));
        };
        div().child(dialog(
            DialogParts {
                id: "password-dialog",
                open: self.open,
                title: "Change password".into(),
                body: "Your other devices will be signed out and need the new password. This device stays signed in. Your encryption passphrase doesn't change.".into(),
                inputs,
                error: self.error.clone(),
                extra: None,
                busy,
                submit_label: if busy { "Changing…".into() } else { "Change password".into() },
                danger: false,
                in_settings: true,
                on_cancel: Rc::new(cancel),
                on_submit: submit,
            },
            window,
            cx,
        ))
    }
}

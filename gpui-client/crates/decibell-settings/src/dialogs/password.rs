//! Change password (`ChangePasswordModal.tsx`; spec `2026-10-19-devices-and-password-design.md`):
//! central checks the current password (a wrong one counts as a failed sign-in) and signs every
//! other device out; this one stays signed in and the core saves the new password for the
//! automatic sign-in. Open while `model.live.devices.password` is (Account's "Change password"
//! opens it): an overlay of its own, over the Settings window.

use std::rc::Rc;

use decibell_model::live::devices::MIN_PASSWORD_CHARS;
use decibell_model::{Model, actions};
use gpui::{App, Context, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*};

use super::{DialogParts, dialog};
use crate::fields::{Input, InputBox, InputLook};

pub struct PasswordOverlay {
    open: bool,
    pub current: Input,
    pub new: Input,
    pub repeat: Input,
    /// The flow's error and busy flag as last shown (kept through the fade-out).
    error: Option<SharedString>,
    busy: bool,
    _flow: Subscription,
}

impl PasswordOverlay {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let weak = cx.weak_entity();
        crate::handles::update(cx, |h| h.password = Some(weak));
        let flow = cx.observe_in(&model.live.devices, window, |this, devices, window, cx| {
            let flow = devices.read(cx).password.clone();
            let open = flow.is_some();
            let (error, busy) = flow.map_or((this.error.clone(), false), |f| (f.error, f.busy));
            if open && !this.open {
                // A fresh dialog (the renderer clears the fields on every open), the first field
                // focused.
                this.current.clear();
                this.new.clear();
                this.repeat.clear();
                this.current.focus.focus(window, cx);
            }
            if open != this.open || error != this.error || busy != this.busy {
                this.open = open;
                this.error = error;
                this.busy = busy;
                cx.notify();
            }
        });
        let mut this = Self {
            open: false,
            current: Input::new(cx),
            new: Input::new(cx),
            repeat: Input::new(cx),
            error: None,
            busy: false,
            _flow: flow,
        };
        if let Some(flow) = model.live.devices.read(cx).password.clone() {
            this.open = true;
            this.error = flow.error;
            this.busy = flow.busy;
        }
        this
    }

    /// The fields as typing would set them (the self-tests' way in until the editor lands).
    pub fn set_fields(&mut self, current: &str, new: &str, repeat: &str, cx: &mut Context<Self>) {
        self.current.set(current);
        self.new.set(new);
        self.repeat.set(repeat);
        cx.notify();
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let (current, new, repeat) = (self.current.text().to_string(), self.new.text().to_string(), self.repeat.text().to_string());
        actions::devices::change_password(current, new, repeat, cx);
    }
}

impl Render for PasswordOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let busy = self.busy;
        let weak = cx.weak_entity();
        let submit: super::Handler = Rc::new(move |_: &mut Window, cx: &mut App| {
            if let Some(v) = weak.upgrade() {
                v.update(cx, |v, cx| v.submit(cx));
            }
        });
        let inputs = vec![
            InputBox::new("password-current", &self.current, InputLook::Dialog)
                .placeholder("Current password")
                .masked()
                .disabled(busy)
                .on_enter(super::enter(&submit))
                .into_any_element(),
            InputBox::new("password-new", &self.new, InputLook::Dialog)
                .placeholder(format!("New password (at least {MIN_PASSWORD_CHARS} characters)"))
                .masked()
                .disabled(busy)
                .on_enter(super::enter(&submit))
                .into_any_element(),
            InputBox::new("password-repeat", &self.repeat, InputLook::Dialog)
                .placeholder("New password again")
                .masked()
                .disabled(busy)
                .on_enter(super::enter(&submit))
                .into_any_element(),
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
                on_cancel: Rc::new(cancel),
                on_submit: submit,
            },
            cx,
        ))
    }
}

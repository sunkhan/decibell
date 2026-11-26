//! The E2EE passphrase dialog (`features/e2ee/PassphraseModal.tsx`): the one dialog behind every
//! key operation — set up, unlock, change the passphrase, reset — open while
//! `model.live.e2ee.passphrase` is. Anything opens it through the model: the first "locked" report
//! of a sign-in, Settings → Privacy, a voice join without keys ("no keys → no voice"), an
//! encrypted DM while locked.
//!
//! The core does the work (Argon2id off the UI thread) and pushes the new status; this collects
//! the passphrase and shows the core's refusal inline. While it works the dialog says "Working…"
//! and can't be cancelled (as the renderer: a key operation half done must finish): a static
//! label, no animation, so nothing draws while Argon2 runs. "Set up" is offered when the account
//! has no keys anywhere (`not_set_up`: a fresh identity); a backup found on central makes the
//! status `locked`, and the dialog then unlocks it.

use std::rc::Rc;

use decibell_model::live::e2ee::{MIN_PASSPHRASE_CHARS, PassphraseFlow, PassphraseMode};
use decibell_model::{Model, actions};
use decibell_text::input::InputState;
use decibell_ui::ActiveTheme as _;
use gpui::{App, AppContext as _, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*};

use super::{DialogParts, dialog, field, submit_on_enter};
use crate::rows::TextLink;

pub struct PassphraseOverlay {
    /// The flow as last shown; the mode is kept through the fade-out (the renderer's frozen copy).
    flow: Option<PassphraseFlow>,
    mode: PassphraseMode,
    passphrase: Entity<InputState>,
    confirm: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

/// The first field's placeholder in `mode`.
fn placeholder(mode: PassphraseMode) -> SharedString {
    if mode == PassphraseMode::Unlock {
        SharedString::new_static("Passphrase")
    } else {
        format!("New passphrase (at least {MIN_PASSPHRASE_CHARS} characters)").into()
    }
}

impl PassphraseOverlay {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let initial = model.live.e2ee.read(cx).passphrase.clone();
        let mode = initial.as_ref().map_or(PassphraseMode::Setup, |f| f.mode);
        let passphrase = cx.new(|cx| InputState::password(window, cx).placeholder(placeholder(mode)));
        let confirm = cx.new(|cx| InputState::password(window, cx).placeholder("Confirm passphrase"));
        let flow = cx.observe_in(&model.live.e2ee, window, |this, e2ee, window, cx| {
            let flow = e2ee.read(cx).passphrase.clone();
            this.follow(flow, window, cx);
        });
        let subscriptions =
            vec![flow, submit_on_enter(&passphrase, Self::submit, cx), submit_on_enter(&confirm, Self::submit, cx)];
        // Open already (the first locked report of a sign-in can come before the main layout
        // builds its overlays): the passphrase has the focus, as the renderer's autoFocus gives it.
        if initial.is_some() {
            passphrase.update(cx, |input, cx| input.focus(window, cx));
        }
        Self { flow: initial, mode, passphrase, confirm, _subscriptions: subscriptions }
    }

    fn follow(&mut self, flow: Option<PassphraseFlow>, window: &mut Window, cx: &mut Context<Self>) {
        if flow == self.flow {
            return;
        }
        // Opened, or switched to another mode (unlock → "Forgot your passphrase?" → reset): a
        // fresh form with the passphrase focused.
        let fresh = flow.as_ref().is_some_and(|f| self.flow.as_ref().is_none_or(|old| old.mode != f.mode));
        if let Some(f) = &flow
            && f.mode != self.mode
        {
            self.passphrase.update(cx, |input, cx| input.set_placeholder(placeholder(f.mode), cx));
        }
        if fresh {
            for input in [&self.passphrase, &self.confirm] {
                input.update(cx, |input, cx| input.clear(cx));
            }
            self.passphrase.update(cx, |input, cx| input.focus(window, cx));
        }
        if let Some(f) = &flow {
            self.mode = f.mode;
        }
        self.flow = flow;
        cx.notify();
    }

    pub fn submit(&mut self, cx: &mut Context<Self>) {
        if self.flow.as_ref().is_none_or(|f| f.busy) {
            return;
        }
        let (passphrase, confirm) = (self.passphrase.read(cx).value().to_string(), self.confirm.read(cx).value().to_string());
        actions::e2ee::submit_passphrase(passphrase, confirm, cx);
    }
}

impl Render for PassphraseOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.mode;
        let open = self.flow.is_some();
        let busy = self.flow.as_ref().is_some_and(|f| f.busy);
        let error = self.flow.as_ref().and_then(|f| f.error.clone());
        let weak = cx.weak_entity();
        let submit: super::Handler = Rc::new(move |_: &mut Window, cx: &mut App| {
            if let Some(v) = weak.upgrade() {
                v.update(cx, |v, cx| v.submit(cx));
            }
        });
        let mut inputs = vec![field("passphrase-input", &self.passphrase, busy)];
        if m.needs_confirm() {
            inputs.push(field("passphrase-confirm", &self.confirm, busy));
        }
        let theme = cx.theme();
        let extra = (m == PassphraseMode::Unlock).then(|| {
            div()
                .flex()
                .child(
                    TextLink::new(
                        "passphrase-forgot",
                        "Forgot your passphrase? Reset your keys",
                        theme.color_text_muted,
                        theme.color_text_secondary,
                    )
                    .disabled(busy)
                    .on_click(|_, _, cx| {
                        let e2ee = Model::global(cx).live.e2ee.clone();
                        e2ee.update(cx, |e, cx| e.open(PassphraseMode::Reset, cx));
                    }),
                )
                .into_any_element()
        });
        let cancel = |_: &mut Window, cx: &mut App| {
            let e2ee = Model::global(cx).live.e2ee.clone();
            e2ee.update(cx, |e, cx| e.cancel(cx));
        };
        div().child(dialog(
            DialogParts {
                id: "passphrase-dialog",
                open,
                title: m.title().into(),
                body: m.body().into(),
                inputs,
                error,
                extra,
                busy,
                submit_label: if busy { "Working…".into() } else { m.confirm_label().into() },
                danger: m.danger(),
                in_settings: false,
                on_cancel: Rc::new(cancel),
                on_submit: submit,
            },
            window,
            cx,
        ))
    }
}

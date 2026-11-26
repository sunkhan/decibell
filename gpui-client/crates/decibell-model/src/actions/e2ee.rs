//! The E2EE flows (`features/e2ee/PassphraseModal.tsx`' submit, `usePeerE2ee.ts`,
//! `useE2eeEvents.ts`' first pull). The core does the work and pushes the new status;
//! this collects the passphrase, shows errors inline and the success toast.

use decibell_core::api;
use decibell_core::api::e2ee::E2eePeerInfo;
use gpui::{App, SharedString};

use crate::Model;
use crate::call::call;
use crate::error::UiError;
use crate::ids::Uid;
use crate::live::e2ee::{PassphraseFlow, PassphraseMode, validate_passphrase};

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// Pull the status once (a status pushed before the model listened would be lost).
pub fn pull_status(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    call(cx, api::e2ee::e2ee_get_status(), move |result, cx| {
        let Ok(s) = result else { return };
        if s.supported || s.status != "unavailable" {
            let p = decibell_core::events::E2eeStatusPayload {
                supported: s.supported,
                status: s.status,
                key_id: s.key_id,
                fingerprint: s.fingerprint,
            };
            model.live.e2ee.update(cx, |e, cx| e.apply_status(&p, cx));
        }
    });
}

/// The dialog's confirm button. Validation errors show inline and send nothing; while
/// the core works the dialog can't be cancelled; success toasts and closes it; a refusal
/// shows inline (without "Error: ").
pub fn submit_passphrase(passphrase: String, confirm: String, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(flow) = model.live.e2ee.read(cx).passphrase.clone() else { return };
    if flow.busy {
        return;
    }
    let mode = flow.mode;
    if let Err(error) = validate_passphrase(mode, &passphrase, &confirm) {
        model.live.e2ee.update(cx, |e, cx| e.set_flow(Some(PassphraseFlow { mode, busy: false, error: Some(error) }), cx));
        return;
    }
    model.live.e2ee.update(cx, |e, cx| e.set_flow(Some(PassphraseFlow { mode, busy: true, error: None }), cx));
    let args = api::e2ee::PassphraseArgs { passphrase };
    let done = move |result: Result<(), UiError>, cx: &mut App| match result {
        Ok(()) => {
            let (title, body) = mode.success();
            model.toasts.update(cx, |t, cx| t.success(title, body.map(SharedString::new_static), cx));
            model.live.e2ee.update(cx, |e, cx| e.set_flow(None, cx));
        }
        Err(e) => model.live.e2ee.update(cx, |s, cx| {
            s.set_flow(Some(PassphraseFlow { mode, busy: false, error: Some(e.plain()) }), cx)
        }),
    };
    match mode {
        PassphraseMode::Setup => call(cx, api::e2ee::e2ee_setup(args), done),
        PassphraseMode::Unlock => call(cx, api::e2ee::e2ee_unlock(args), done),
        PassphraseMode::Change => call(cx, api::e2ee::e2ee_change_passphrase(args), done),
        PassphraseMode::Reset => call(cx, api::e2ee::e2ee_reset(args), done),
    }
}

/// A peer's encryption state as the core sees it: keys published, their fingerprint, the
/// conversation's safety number (`usePeerE2ee`). Ask again when our status or the peer's
/// change marker moves. The core caches the bundle per session.
pub fn peer_info(uid: Uid, cx: &mut App, done: impl FnOnce(Result<E2eePeerInfo, UiError>, &mut App) + 'static) {
    let Some(model) = model(cx) else { return };
    let name = model.name_of(uid, &SharedString::default(), cx).to_string();
    call(cx, api::e2ee::e2ee_peer_info(api::e2ee::PeerInfoArgs { username: name, uid: Some(uid) }), done);
}

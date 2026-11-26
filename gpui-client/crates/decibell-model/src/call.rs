//! UI → core: calling `decibell_core::api` from the UI thread.
//!
//! Every `api` future can be awaited on GPUI's foreground executor: the core runs its
//! body on its own runtime and the answer wakes the awaiting task, so the UI thread
//! never blocks. The common shape — call, then update a store or show the error — is
//! one line:
//!
//! ```ignore
//! call(cx, api::servers::request_server_list(), |result, cx| { … });
//! call_or_toast(cx, "Couldn't leave server", api::community::leave_server(args));
//! ```
//!
//! Inside an entity or view, capture `cx.weak_entity()` in the closure, or use
//! `cx.spawn(async move |this, cx| { let r = api::…(args).await; this.update(cx, …) })`
//! when the task should die with the view.
//!
//! Most calls only queue a request: they succeed once it is sent, and the answer
//! arrives later as an event (the function's doc in `decibell_core::api` says which).

use std::future::Future;

use decibell_core::api::CoreError;
use gpui::{App, SharedString};

use crate::Model;
use crate::error::UiError;
use crate::stores::overlays::{CertMismatchNotice, Retry};

/// Run a core call; `done` gets its result on the UI thread, always after `call` has
/// returned (so `done` may update the entity that made the call). Detached: it finishes
/// even if the view that started it is gone. With the core not running (before
/// `Model::start_core` finished, after shutdown) `done` gets [`UiError::not_running`].
pub fn call<T: 'static>(
    cx: &mut App,
    request: impl Future<Output = Result<T, CoreError>> + 'static,
    done: impl FnOnce(Result<T, UiError>, &mut App) + 'static,
) {
    if !crate::core::is_running() {
        cx.defer(move |cx| done(Err(UiError::not_running()), cx));
        return;
    }
    cx.spawn(async move |cx| {
        let result = request.await.map_err(UiError::from);
        cx.update(|cx| done(result, cx));
    })
    .detach();
}

/// Run a core call whose answer is an event; a failure to send it is shown
/// ([`show_error`]).
pub fn call_or_toast<T: 'static>(
    cx: &mut App,
    title: &'static str,
    request: impl Future<Output = Result<T, CoreError>> + 'static,
) {
    call(cx, request, move |result, cx| {
        if let Err(e) = result {
            show_error(&e, title, cx);
        }
    });
}

/// The one way a failed call is shown: a certificate that no longer matches its pin
/// opens the re-trust prompt (with nothing to retry: pass a retry through
/// [`offer_retrust`] when there is one); anything else is an error toast with the
/// core's message as its body.
pub fn show_error(error: &UiError, title: &'static str, cx: &mut App) {
    if offer_retrust(error, None, cx) {
        return;
    }
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let body = Some(error.plain()).filter(|b| !b.is_empty());
    model.toasts.update(cx, |t, cx| t.error(SharedString::new_static(title), body, cx));
}

/// `handleCertMismatch(err, retry)`: when `error` is a pin failure, open the re-trust
/// prompt and return true (the caller then shows nothing else).
pub fn offer_retrust(error: &UiError, retry: Option<Retry>, cx: &mut App) -> bool {
    let Some(cert) = error.cert.clone() else { return false };
    let Some(model) = cx.try_global::<Model>().cloned() else { return false };
    model
        .overlays
        .update(cx, |o, cx| o.set_cert_mismatch(Some(CertMismatchNotice { cert, retry }), cx));
    true
}

//! End-to-end encryption (`features/e2ee/useE2eeEvents.ts`).

use decibell_core::events::{E2eePeerChangedPayload, E2eeStatusPayload};
use gpui::{App, SharedString};

use crate::Model;

/// The core's status for this account on this device. The first "locked" of a sign-in
/// opens the unlock prompt; becoming "ready" tells the DM model to reload
/// (`E2eeEvent::BecameReady`).
pub fn e2ee_status_changed(model: &Model, p: &E2eeStatusPayload, cx: &mut App) {
    model.live.e2ee.update(cx, |e, cx| e.apply_status(p, cx));
}

/// A pinned peer's identity changed (they reset their keys).
pub fn e2ee_peer_changed(model: &Model, p: &E2eePeerChangedPayload, cx: &mut App) {
    if p.uid > 0 {
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        model.live.e2ee.update(cx, |e, cx| e.mark_peer_changed(p.uid, at, cx));
    }
    let name = model.name_of(p.uid, &SharedString::from(p.username.clone()), cx);
    model.toasts.update(cx, |t, cx| {
        t.warning(
            "Encryption keys changed",
            Some(
                format!("{name}'s encryption keys changed. If that's unexpected, compare safety numbers in their profile.")
                    .into(),
            ),
            cx,
        )
    });
}

//! End-to-end encryption status and the passphrase flows (`stores/e2eeStore.ts`,
//! `features/e2ee/useE2eeEvents.ts`, `PassphraseModal.tsx`, `usePeerE2ee.ts`). All crypto
//! is in the core; this mirrors what it reports and drives the one dialog behind every
//! key operation.
//!
//! - The core decides the status for this account on this device (after every sign-in,
//!   setup, unlock, reset, a remote rotation of our keys, and sign-out) and pushes it.
//!   The first "locked" report of a sign-in opens the unlock prompt, once.
//! - Becoming "ready" emits [`E2eeEvent::BecameReady`]: the DM model reloads (every sealed
//!   row that arrived before the keys held a placeholder: M2's `dmStore.invalidateAllHistory`
//!   + `request_dm_conversations` + the open conversation's latest page).
//! - The passphrase dialog ([`PassphraseFlow`]) is its own overlay here, not
//!   `Overlays::active_modal`: Settings → Privacy opens it over the Settings modal.
//!   Strict voice ("no keys → no voice") opens it too.
//! - A pinned peer's identity changing is a warning toast and a per-session marker
//!   (`changed_peers`) the DM banner and profile read; safety numbers are fetched on
//!   demand (`actions::e2ee::peer_info`).
//!
//! **Rhythm**: a handful of changes per session. The DM panel, Settings → Privacy, the
//! voice join path and the dialog observe it.

use std::collections::HashMap;

use decibell_core::events::E2eeStatusPayload;
use gpui::{Context, EventEmitter, SharedString};

use crate::ids::Uid;

/// A passphrase must have at least this many characters (code points).
pub const MIN_PASSPHRASE_CHARS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum E2eeStatus {
    /// Signed out, or the core is still loading the keys after sign-in.
    #[default]
    Unavailable,
    NotSetUp,
    /// Keys exist (on central, as a passphrase backup) but this device hasn't unlocked them.
    Locked,
    Ready,
}

impl E2eeStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "not_set_up" => Self::NotSetUp,
            "locked" => Self::Locked,
            "ready" => Self::Ready,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PassphraseMode {
    Setup,
    Unlock,
    Change,
    Reset,
}

impl PassphraseMode {
    pub fn title(self) -> &'static str {
        match self {
            Self::Setup => "Set up end-to-end encryption",
            Self::Unlock => "Unlock encrypted messages",
            Self::Change => "Change encryption passphrase",
            Self::Reset => "Reset encryption keys",
        }
    }

    pub fn body(self) -> &'static str {
        match self {
            Self::Setup => "Your direct messages will be encrypted on this device so only you and the person you're talking to can read them — not even the server. Choose a passphrase to protect your keys: you'll need it to read encrypted messages on another device. A lost passphrase cannot be recovered.",
            Self::Unlock => "Enter your encryption passphrase to read and send encrypted messages on this device.",
            Self::Change => "Choose a new passphrase. Your keys and message history are unaffected; other devices will need the new passphrase to unlock.",
            Self::Reset => "This creates new keys. Messages sent to your old keys can no longer be read on a device that doesn't have them, and people you message will see that your keys changed. Only do this if you've lost your passphrase.",
        }
    }

    pub fn confirm_label(self) -> &'static str {
        match self {
            Self::Setup => "Turn on encryption",
            Self::Unlock => "Unlock",
            Self::Change => "Change passphrase",
            Self::Reset => "Reset keys",
        }
    }

    /// The dialog asks for the passphrase twice.
    pub fn needs_confirm(self) -> bool {
        !matches!(self, Self::Unlock)
    }

    pub fn danger(self) -> bool {
        matches!(self, Self::Reset)
    }

    /// The success toast (title, body).
    pub fn success(self) -> (&'static str, Option<&'static str>) {
        match self {
            Self::Setup => ("Encryption is on", Some("Your direct messages are now end-to-end encrypted.")),
            Self::Unlock => ("Encryption unlocked", None),
            Self::Change => ("Passphrase changed", None),
            Self::Reset => ("Encryption keys reset", None),
        }
    }
}

/// The passphrase dialog's state while it is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassphraseFlow {
    pub mode: PassphraseMode,
    /// The core is working (Argon2id takes a moment): the dialog can't be cancelled.
    pub busy: bool,
    /// Shown inline (validation, or the core's refusal without "Error: ").
    pub error: Option<SharedString>,
}

/// Check what was typed before anything is sent (`submit`'s checks).
pub fn validate_passphrase(mode: PassphraseMode, passphrase: &str, confirm: &str) -> Result<(), SharedString> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(format!("Use at least {MIN_PASSPHRASE_CHARS} characters.").into());
    }
    if mode.needs_confirm() && confirm != passphrase {
        return Err("The passphrases don't match.".into());
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
pub enum E2eeEvent {
    /// The keys became usable on this device: the DM model reloads its sealed rows.
    BecameReady,
    /// A pinned peer's identity changed (they reset their keys).
    PeerChanged(Uid),
}

#[derive(Default)]
pub struct E2ee {
    pub supported: bool,
    pub status: E2eeStatus,
    pub key_id: u32,
    /// Our identity fingerprint when ready.
    pub fingerprint: SharedString,
    /// The core reported a status for this sign-in (the first "locked" opens the prompt).
    pub reported: bool,
    /// Peers whose pinned identity changed this session → when (unix seconds).
    pub changed_peers: HashMap<Uid, i64>,
    /// The DM panel's "set up encryption" nudge, dismissed for this session.
    pub setup_nudge_dismissed: bool,
    pub passphrase: Option<PassphraseFlow>,
}

impl EventEmitter<E2eeEvent> for E2ee {}

impl E2ee {
    /// A status from the core (`applyStatus`).
    pub fn apply_status(&mut self, p: &E2eeStatusPayload, cx: &mut Context<Self>) {
        let first = !self.reported;
        let was_ready = self.status == E2eeStatus::Ready;
        let status = E2eeStatus::parse(&p.status);
        self.supported = p.supported;
        self.status = status;
        self.key_id = p.key_id;
        self.fingerprint = p.fingerprint.clone().into();
        // "unavailable" is also what sign-out reports: the next sign-in gets its prompt.
        self.reported = status != E2eeStatus::Unavailable;
        if status == E2eeStatus::Unavailable {
            self.changed_peers.clear();
            self.setup_nudge_dismissed = false;
            self.passphrase = None;
        }
        if status == E2eeStatus::Locked && first {
            self.open(PassphraseMode::Unlock, cx);
        }
        if status == E2eeStatus::Ready && !was_ready {
            cx.emit(E2eeEvent::BecameReady);
        }
        cx.notify();
    }

    pub fn mark_peer_changed(&mut self, uid: Uid, at: i64, cx: &mut Context<Self>) {
        self.changed_peers.insert(uid, at);
        cx.emit(E2eeEvent::PeerChanged(uid));
        cx.notify();
    }

    pub fn dismiss_peer_change(&mut self, uid: Uid, cx: &mut Context<Self>) {
        if self.changed_peers.remove(&uid).is_some() {
            cx.notify();
        }
    }

    pub fn dismiss_setup_nudge(&mut self, cx: &mut Context<Self>) {
        if !self.setup_nudge_dismissed {
            self.setup_nudge_dismissed = true;
            cx.notify();
        }
    }

    /// Open the passphrase dialog (fresh: no error, not busy).
    pub fn open(&mut self, mode: PassphraseMode, cx: &mut Context<Self>) {
        self.passphrase = Some(PassphraseFlow { mode, busy: false, error: None });
        cx.notify();
    }

    /// Cancel: not while the core works.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.passphrase.as_ref().is_some_and(|f| !f.busy) {
            self.passphrase = None;
            cx.notify();
        }
    }

    pub(crate) fn set_flow(&mut self, flow: Option<PassphraseFlow>, cx: &mut Context<Self>) {
        if self.passphrase != flow {
            self.passphrase = flow;
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn status(s: &str) -> E2eeStatusPayload {
        E2eeStatusPayload { supported: true, status: s.into(), key_id: 1, fingerprint: "ab".into() }
    }

    #[gpui::test]
    fn the_first_locked_report_opens_unlock_once_and_ready_reloads(cx: &mut TestAppContext) {
        let e = cx.new(|_| E2ee::default());
        let events = Rc::new(RefCell::new(Vec::new()));
        let _sub = cx.update(|cx| {
            let events = events.clone();
            cx.subscribe(&e, move |_, ev: &E2eeEvent, _| events.borrow_mut().push(ev.clone()))
        });
        e.update(cx, |e, cx| e.apply_status(&status("locked"), cx));
        e.read_with(cx, |e, _| assert_eq!(e.passphrase.as_ref().map(|f| f.mode), Some(PassphraseMode::Unlock)));
        // Dismissed, then locked reported again: no second prompt.
        e.update(cx, |e, cx| {
            e.cancel(cx);
            e.apply_status(&status("locked"), cx);
        });
        e.read_with(cx, |e, _| assert!(e.passphrase.is_none()));
        e.update(cx, |e, cx| e.apply_status(&status("ready"), cx));
        e.update(cx, |e, cx| e.apply_status(&status("ready"), cx));
        assert_eq!(*events.borrow(), vec![E2eeEvent::BecameReady], "only the edge");
        // Sign-out resets the session flags; the next sign-in prompts again.
        e.update(cx, |e, cx| {
            e.mark_peer_changed(3, 100, cx);
            e.dismiss_setup_nudge(cx);
            e.apply_status(&status("unavailable"), cx);
        });
        e.read_with(cx, |e, _| assert!(e.changed_peers.is_empty() && !e.setup_nudge_dismissed && !e.reported));
        e.update(cx, |e, cx| e.apply_status(&status("locked"), cx));
        e.read_with(cx, |e, _| assert!(e.passphrase.is_some()));
    }

    #[gpui::test]
    fn a_busy_dialog_cannot_be_cancelled(cx: &mut TestAppContext) {
        let e = cx.new(|_| E2ee::default());
        e.update(cx, |e, cx| {
            e.open(PassphraseMode::Setup, cx);
            e.set_flow(Some(PassphraseFlow { mode: PassphraseMode::Setup, busy: true, error: None }), cx);
            e.cancel(cx);
        });
        e.read_with(cx, |e, _| assert!(e.passphrase.is_some()));
    }

    #[test]
    fn passphrase_checks() {
        assert_eq!(validate_passphrase(PassphraseMode::Unlock, "short", "").unwrap_err().as_ref(), "Use at least 10 characters.");
        assert!(validate_passphrase(PassphraseMode::Unlock, "ünïcödé-10", "").is_ok(), "code points, not bytes");
        assert_eq!(
            validate_passphrase(PassphraseMode::Setup, "long enough!", "long enough?").unwrap_err().as_ref(),
            "The passphrases don't match."
        );
        assert!(validate_passphrase(PassphraseMode::Reset, "long enough!", "long enough!").is_ok());
    }
}

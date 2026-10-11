//! User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md).
//!
//! Central owns the chosen status; native only keeps what it must replay on
//! its own: whether communities should report us offline (Invisible — sent
//! in every COMMUNITY_AUTH_REQ, so a reconnect never shows us online), and
//! the auto-away idle flag (session-only on central, so it is re-sent after
//! a central reconnect). Lock-free so the connect paths never take AppState.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::net::proto::PresenceStatus;

static HIDDEN: AtomicBool = AtomicBool::new(false);
static IDLE: AtomicBool = AtomicBool::new(false);

pub fn hidden() -> bool {
    HIDDEN.load(Ordering::SeqCst)
}

/// Returns the previous value.
pub fn set_hidden(hidden: bool) -> bool {
    HIDDEN.swap(hidden, Ordering::SeqCst)
}

pub fn idle() -> bool {
    IDLE.load(Ordering::SeqCst)
}

/// Returns the previous value.
pub fn set_idle(idle: bool) -> bool {
    IDLE.swap(idle, Ordering::SeqCst)
}

/// Logout: the next account starts visible and active.
pub fn reset() {
    HIDDEN.store(false, Ordering::SeqCst);
    IDLE.store(false, Ordering::SeqCst);
}

/// Wire value → the renderer's name. INVISIBLE only ever describes the
/// local user (others who are invisible are simply absent).
pub fn status_name(raw: i32) -> &'static str {
    match PresenceStatus::try_from(raw) {
        Ok(PresenceStatus::PresenceAway) => "away",
        Ok(PresenceStatus::PresenceDnd) => "dnd",
        Ok(PresenceStatus::PresenceInvisible) => "invisible",
        _ => "online",
    }
}

pub fn parse_status(name: &str) -> Option<PresenceStatus> {
    match name {
        "online" => Some(PresenceStatus::PresenceOnline),
        "away" => Some(PresenceStatus::PresenceAway),
        "dnd" => Some(PresenceStatus::PresenceDnd),
        "invisible" => Some(PresenceStatus::PresenceInvisible),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_round_trip() {
        for name in ["online", "away", "dnd", "invisible"] {
            let s = parse_status(name).expect("known status");
            assert_eq!(status_name(s as i32), name);
        }
        assert_eq!(parse_status("busy"), None);
        // An unknown wire value from a newer central reads as online.
        assert_eq!(status_name(42), "online");
    }
}

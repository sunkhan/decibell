//! Identity types. Users are uids everywhere (CLAUDE.md "Backend identity is the uid");
//! a community is an integer here although its wire form is a string.

use std::sync::{Arc, Mutex};

use gpui::SharedString;

/// A central account uid. `0` = nobody / unknown (system messages, deleted senders, and
/// "ourselves" in a few media payloads); negative = a community's placeholder for a member
/// row without a central account, unique only inside that community.
pub type Uid = i64;

/// True for a real central account (the only uids the users store names).
pub fn is_account(uid: Uid) -> bool {
    uid > 0
}

/// A channel's id: a string on the wire, unique only within its community.
pub type ChannelId = SharedString;

/// A community.
///
/// On the wire community payloads carry `server_id: String` — central's id in decimal, or
/// `"host:port"` while a connection opened from an invite has not been re-keyed yet
/// (`CommunityAuthResponded`) — and central's own payloads carry an `i32`. Here both are
/// one integer: central's id as it is (positive), and a provisional negative id for a
/// `"host:port"` key, handed out in order and stable for the process. `0` is "none".
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default)]
pub struct ServerId(i64);

/// The `"host:port"` keys behind provisional ids: index `n` holds id `-(n + 1)`. Touched
/// on the UI thread only (the lock is never contended); a handful of entries per session.
static PROVISIONAL: Mutex<Vec<Arc<str>>> = Mutex::new(Vec::new());

impl ServerId {
    pub const NONE: ServerId = ServerId(0);

    /// From a community payload's `server_id` (or any string the core echoes back).
    pub fn from_wire(text: &str) -> ServerId {
        if text.is_empty() {
            return ServerId::NONE;
        }
        if let Ok(n) = text.parse::<i64>()
            && n > 0
        {
            return ServerId(n);
        }
        let mut keys = PROVISIONAL.lock().unwrap_or_else(|e| e.into_inner());
        let index = match keys.iter().position(|k| &**k == text) {
            Some(index) => index,
            None => {
                keys.push(Arc::from(text));
                keys.len() - 1
            }
        };
        ServerId(-(index as i64) - 1)
    }

    /// From a central payload's numeric id (`ServerInfo::id`, the server-picture events).
    pub fn from_central(id: i32) -> ServerId {
        ServerId(id as i64)
    }

    /// The string the core's API takes as `server_id`.
    pub fn wire(self) -> String {
        if self.0 >= 0 {
            return self.0.to_string();
        }
        let keys = PROVISIONAL.lock().unwrap_or_else(|e| e.into_inner());
        keys.get((-self.0 - 1) as usize)
            .map(|k| k.to_string())
            .unwrap_or_default()
    }

    /// Central's id, for the calls that take it as a number (`fetch_server_picture`).
    /// `None` for a provisional id.
    pub fn central(self) -> Option<i32> {
        (self.0 > 0).then(|| i32::try_from(self.0).ok()).flatten()
    }

    pub fn is_none(self) -> bool {
        self.0 == 0
    }

    /// A connection opened from an invite, not yet re-keyed onto central's id.
    pub fn is_provisional(self) -> bool {
        self.0 < 0
    }

    /// For `dump`s and logs.
    pub fn get(self) -> i64 {
        self.0
    }
}

impl std::fmt::Display for ServerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.wire())
    }
}

impl serde::Serialize for ServerId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.wire())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn central_ids_are_themselves() {
        let id = ServerId::from_wire("42");
        assert_eq!(id, ServerId::from_central(42));
        assert_eq!(id.wire(), "42");
        assert_eq!(id.central(), Some(42));
        assert!(!id.is_provisional());
        assert!(ServerId::from_wire("").is_none());
    }

    #[test]
    fn host_port_keys_get_stable_provisional_ids() {
        let a = ServerId::from_wire("203.0.113.9:8082");
        let b = ServerId::from_wire("203.0.113.10:8082");
        assert!(a.is_provisional() && b.is_provisional());
        assert_ne!(a, b);
        assert_eq!(a, ServerId::from_wire("203.0.113.9:8082"));
        assert_eq!(a.wire(), "203.0.113.9:8082");
        assert_eq!(a.central(), None);
        // "0" and negative numbers are not central ids.
        assert!(ServerId::from_wire("0").is_provisional());
    }
}

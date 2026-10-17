//! Central's record of where each community lives: id → host:port, as
//! central reported it (login memberships, the server directory, invite
//! resolution). Central is pinned, so this is the trusted answer to "which
//! address does community N have?".
//!
//! Used when a community's COMMUNITY_AUTH_RES names its central id: a
//! connection opened under a synthetic "host:port" id (an invite join) may
//! move onto that id only when central puts that id at this very host:port
//! (see `community::rekey_decision`). A community's own claim about its id
//! is otherwise untrusted.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static DIR: OnceLock<Mutex<HashMap<i64, (String, u16)>>> = OnceLock::new();

fn dir() -> &'static Mutex<HashMap<i64, (String, u16)>> {
    DIR.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Remember the address central reported for community `id`. Ignores
/// ids / ports that can't name a community.
pub fn record(id: i64, host: &str, port: i64) {
    let Ok(port) = u16::try_from(port) else { return };
    if id <= 0 || host.is_empty() || port == 0 {
        return;
    }
    dir()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, (host.to_ascii_lowercase(), port));
}

/// The address central reported for community `id`, if any.
pub fn lookup(id: i64) -> Option<(String, u16)> {
    dir().lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
}

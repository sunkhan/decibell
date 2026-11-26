//! Invisible is the account's (multi-device): following it on this device's
//! community connections. The status commands themselves are the addon's
//! (`commands/presence.rs`).

use crate::net::connection::build_packet;
use crate::net::proto::{packet, PresenceVisibility};
use crate::state;

/// Another device of this account changed its status (OWN_PRESENCE,
/// multi-device): Invisible is the account's, so this device's communities
/// follow it too.
pub async fn apply_hidden(hidden: bool) {
    if crate::presence::set_hidden(hidden) != hidden {
        tell_communities_hidden(hidden).await;
    }
}

/// Entering or leaving Invisible: every connected community stops (or
/// resumes) reporting us online. New connections carry it in their auth.
pub async fn tell_communities_hidden(hidden: bool) {
    let state_arc = state::shared();
    let sends: Vec<_> = {
        let s = state_arc.lock().await;
        s.communities
            .values()
            .filter_map(|c| {
                c.connection_write_tx().map(|tx| {
                    let data = build_packet(
                        packet::Type::PresenceVisibility,
                        packet::Payload::PresenceVisibility(PresenceVisibility { hidden }),
                        None,
                    );
                    (tx, data)
                })
            })
            .collect()
    };
    for (tx, data) in sends {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await;
    }
}

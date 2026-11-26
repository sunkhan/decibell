//! Connection state, apart from content: central's link (the "Reconnecting…" banner,
//! `uiStore.connectionStatus`) and which communities are connected or still connecting
//! (`chatStore.connectedServers`, `pendingMembershipServerIds`).

use std::collections::HashSet;

use gpui::Context;

use crate::ids::ServerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CentralStatus {
    #[default]
    Connected,
    Reconnecting,
    /// In the renderer's type; nothing sets it there either.
    Disconnected,
}

#[derive(Default)]
pub struct Connections {
    pub central: CentralStatus,
    connected: HashSet<ServerId>,
    /// Auto-rejoin: communities the core is connecting to because the sign-in reply
    /// listed them. Each is cleared by its `community_auth_responded`.
    pending: HashSet<ServerId>,
}

impl Connections {
    pub fn set_central(&mut self, status: CentralStatus, cx: &mut Context<Self>) {
        if self.central != status {
            self.central = status;
            cx.notify();
        }
    }

    pub fn is_connected(&self, server: ServerId) -> bool {
        self.connected.contains(&server)
    }

    pub fn is_pending(&self, server: ServerId) -> bool {
        self.pending.contains(&server)
    }

    pub fn connected(&self) -> impl Iterator<Item = ServerId> + '_ {
        self.connected.iter().copied()
    }

    pub fn pending(&self) -> impl Iterator<Item = ServerId> + '_ {
        self.pending.iter().copied()
    }

    pub fn add_connected(&mut self, server: ServerId, cx: &mut Context<Self>) {
        if self.connected.insert(server) {
            cx.notify();
        }
    }

    pub fn remove_connected(&mut self, server: ServerId, cx: &mut Context<Self>) {
        if self.connected.remove(&server) {
            cx.notify();
        }
    }

    /// Replace the pending set (`memberships_received`).
    pub fn set_pending(&mut self, servers: impl IntoIterator<Item = ServerId>, cx: &mut Context<Self>) {
        self.pending = servers.into_iter().collect();
        cx.notify();
    }

    pub fn remove_pending(&mut self, server: ServerId, cx: &mut Context<Self>) {
        if self.pending.remove(&server) {
            cx.notify();
        }
    }

    /// Sign-out: the community sets go. The banner's state is left as it is, as the
    /// renderer's `resetForLogout` leaves `uiStore.connectionStatus`.
    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        self.connected.clear();
        self.pending.clear();
        cx.notify();
    }
}

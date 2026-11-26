//! Central's link: the "Connection lost. Reconnecting…" banner
//! (`hooks/useCentralConnectionStatus.ts`). The community half of the same two events
//! is in `servers.rs`.

use decibell_core::events::ConnectionEventPayload;
use gpui::App;

use crate::Model;
use crate::stores::connections::CentralStatus;

pub fn lost(model: &Model, p: &ConnectionEventPayload, cx: &mut App) {
    if p.server_type == "central" {
        model.connections.update(cx, |c, cx| c.set_central(CentralStatus::Reconnecting, cx));
    }
}

pub fn restored(model: &Model, p: &ConnectionEventPayload, cx: &mut App) {
    if p.server_type == "central" {
        model.connections.update(cx, |c, cx| c.set_central(CentralStatus::Connected, cx));
    }
}

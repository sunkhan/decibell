//! Requests the shell raises for dialogs a feature crate draws, and the shell's own pending
//! leave. One small global entity: an overlay that answers a request observes it.

use decibell_model::stores::community::ChannelKind;
use decibell_model::stores::overlays::modal;
use decibell_model::{ChannelId, Model, ServerId};
use gpui::{App, AppContext as _, Context, Entity, Global, SharedString};

/// "Create channel" from the channel list (`CreateChannelModal`'s props): the dialog is the
/// server-menus feature's. It clears the request when it closes.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateChannelRequest {
    pub server: ServerId,
    pub kind: ChannelKind,
    pub category: Option<ChannelId>,
    /// The category header's "+": the dialog offers text / voice.
    pub allow_type_choice: bool,
}

#[derive(Default)]
pub struct ShellRequests {
    pub create_channel: Option<CreateChannelRequest>,
    /// The community the "Leave server" confirmation is about (with its name, as the renderer
    /// keeps it while the dialog shows).
    pub leave: Option<(ServerId, SharedString)>,
}

struct Handle(Entity<ShellRequests>);

impl Global for Handle {}

impl ShellRequests {
    /// The entity (made on first use).
    pub fn entity(cx: &mut App) -> Entity<ShellRequests> {
        if let Some(handle) = cx.try_global::<Handle>() {
            return handle.0.clone();
        }
        let entity = cx.new(|_| ShellRequests::default());
        cx.set_global(Handle(entity.clone()));
        entity
    }

    /// The server menu's "Leave Server": remember which and open the confirmation.
    pub fn leave_server(server: ServerId, cx: &mut App) {
        let Some(model) = cx.try_global::<Model>().cloned() else { return };
        let name = model.servers.read(cx).get(server).map(|s| s.name.clone()).unwrap_or_else(|| "this server".into());
        Self::entity(cx).update(cx, |r, cx| {
            r.leave = Some((server, name));
            cx.notify();
        });
        model.overlays.update(cx, |o, cx| o.open_modal(modal::LEAVE_SERVER_CONFIRM, cx));
    }

    pub fn clear_create_channel(&mut self, cx: &mut Context<Self>) {
        if self.create_channel.take().is_some() {
            cx.notify();
        }
    }
}

/// Ask for the create-channel dialog.
pub fn create_channel(server: ServerId, kind: ChannelKind, category: Option<ChannelId>, allow_type_choice: bool, cx: &mut App) {
    let request = CreateChannelRequest { server, kind, category, allow_type_choice };
    ShellRequests::entity(cx).update(cx, |r, cx| {
        r.create_channel = Some(request);
        cx.notify();
    });
}

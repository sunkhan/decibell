//! Where the user is: the active view, community and channel (`uiStore.activeView`,
//! `chatStore.activeServerId` / `activeChannelId`), and the Settings tab.

use gpui::{Context, EventEmitter, SharedString};

use crate::ids::{ChannelId, ServerId};

/// The main area's view (`uiStore.activeView`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ActiveView {
    #[default]
    Home,
    Server,
    Browse,
    Voice,
    Dm,
}

pub struct Navigation {
    pub active_view: ActiveView,
    pub active_server: Option<ServerId>,
    pub active_channel: Option<ChannelId>,
    /// Settings → the open tab (`uiStore.settingsTab`).
    pub settings_tab: SharedString,
}

impl Default for Navigation {
    fn default() -> Self {
        Navigation {
            active_view: ActiveView::Home,
            active_server: None,
            active_channel: None,
            settings_tab: SharedString::new_static("account"),
        }
    }
}

/// What the message model follows (its per-channel cache is ordered by visits).
pub enum NavEvent {
    /// A channel became the active one **while a community was active**: the renderer's
    /// `setActiveChannel` moves it to the front of the channel cache's LRU here.
    ChannelActivated { server: ServerId, channel: ChannelId },
}

impl EventEmitter<NavEvent> for Navigation {}

impl Navigation {
    pub fn set_active_view(&mut self, view: ActiveView, cx: &mut Context<Self>) {
        if self.active_view != view {
            self.active_view = view;
            cx.notify();
        }
    }

    pub fn set_active_server(&mut self, server: Option<ServerId>, cx: &mut Context<Self>) {
        if self.active_server != server {
            self.active_server = server;
            cx.notify();
        }
    }

    /// Callers set the active community before activating one of its channels (the
    /// server bar does, and every event handler checks `active_server` first).
    pub fn set_active_channel(&mut self, channel: Option<ChannelId>, cx: &mut Context<Self>) {
        if let (Some(server), Some(channel)) = (self.active_server, &channel) {
            // Emitted even when the channel is already active: a revisit still counts.
            cx.emit(NavEvent::ChannelActivated {
                server,
                channel: channel.clone(),
            });
        }
        if self.active_channel != channel {
            self.active_channel = channel;
            cx.notify();
        }
    }

    pub fn set_settings_tab(&mut self, tab: impl Into<SharedString>, cx: &mut Context<Self>) {
        let tab = tab.into();
        if self.settings_tab != tab {
            self.settings_tab = tab;
            cx.notify();
        }
    }

    /// The channel on screen, when the server view shows one.
    pub fn active_channel_key(&self) -> Option<(ServerId, &ChannelId)> {
        Some((self.active_server?, self.active_channel.as_ref()?))
    }
}

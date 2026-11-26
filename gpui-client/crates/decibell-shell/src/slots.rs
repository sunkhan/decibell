//! What feature crates plug into the shell: the panes of the main layout, overlays drawn over
//! it, and entries of the shell's context menus. Registered once at start-up (the app's
//! `boot`, through each feature crate's `init`), before the main window opens.
//!
//! ```ignore
//! // in decibell-chat's init(cx):
//! decibell_shell::register_pane(cx, Slot::Chat, |window, cx| cx.new(|cx| ChatPanel::new(window, cx)).into());
//! decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| ImageViewer::new(window, cx)).into());
//! ```
//!
//! A pane is built when its place first shows and dropped when it goes (as the renderer mounts
//! and unmounts components), then drawn as a cached view: it must observe what it reads
//! (`decibell_common::Watch`). Overlays live as long as the signed-in main view and draw
//! nothing while closed.

use std::collections::HashMap;
use std::rc::Rc;

use decibell_model::{ChannelId, ServerId};
use decibell_ui::{ActiveTheme as _, FontRole, Label, MenuEntry};
use gpui::{AnyView, App, AppContext as _, Context, Global, IntoElement, Render, SharedString, Window, div, prelude::*};

/// A place in the main layout (`layouts/MainLayout.tsx`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// Home: right of the conversation list (`FriendsPage.tsx`: `decibell-home`'s Friends page).
    Home,
    /// The DM view: the conversation (`DmChatPanel`) and the friends rail beside it
    /// (`FriendsList`, when `Layout::prefs().dm_friends_panel`).
    Dm,
    /// The text channel's chat (`ChatPanel`): under the channel header, left of the right panel.
    Chat,
    /// Right of the chat, under the channel header (default: the member list while
    /// `members_panel` is on). Message search replaces it with a view that shows its results
    /// while a search of this community is open and the member list otherwise
    /// ([`crate::MembersList`]).
    RightPanel,
    /// The channel header's right end (`SearchBox`, 244 px centred over the members column).
    HeaderSearch,
    /// The voice view (`VoicePanel`).
    Voice,
    /// Discover (`ServerBrowseView`): the whole workspace, no sidebar.
    Browse,
    /// The rows of the conversation sidebar under "Direct Messages" (`ConversationSidebar`'s
    /// list).
    DmList,
    /// The unread-DM tiles of the server bar, between the home button and the server tiles
    /// (`UnreadDmTiles`; draws nothing while every conversation is read).
    UnreadDms,
    /// The P2P call stage (`CallStage`) the DM pane shows above the conversation while a call
    /// with its peer is outgoing, connecting or active. Built by the DM pane through
    /// [`build_slot`]; the view sizes itself (drawn uncached) and reads the call store for
    /// the peer.
    DmCallStage,
}

impl Slot {
    pub fn name(self) -> &'static str {
        match self {
            Slot::Home => "Home",
            Slot::Dm => "Direct message",
            Slot::Chat => "Chat",
            Slot::RightPanel => "Right panel",
            Slot::HeaderSearch => "Search",
            Slot::Voice => "Voice",
            Slot::Browse => "Discover",
            Slot::DmList => "Conversations",
            Slot::UnreadDms => "Unread DMs",
            Slot::DmCallStage => "Call",
        }
    }
}

/// Builds a pane's view.
pub type PaneBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

/// The shell's own view for a place no feature registered.
pub type PaneFallback = dyn Fn(&mut Window, &mut App) -> AnyView;

/// What a channel-list context menu was opened on.
#[derive(Debug, Clone, PartialEq)]
pub enum ChannelMenuTarget {
    /// A channel row (text or voice).
    Channel { server: ServerId, channel: ChannelId },
    /// A category header.
    Category { server: ServerId, channel: ChannelId },
    /// The list's empty area (the create actions).
    Empty { server: ServerId },
}

/// Adds entries to the channel list's context menu (after the shell's own).
pub type ChannelMenuExtension = Rc<dyn Fn(&ChannelMenuTarget, &mut App) -> Vec<MenuEntry>>;

/// What the user panel's voice controls ask a feature to open (`UserPanel.tsx`: the
/// connection-stats popover, the Go Live dialog, the stream-audio popover, the device menus on a
/// right click of mute / deafen). `anchor` is the clicked control's bounds in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    /// The latency readout: the connection-stats popover.
    Latency,
    /// "Stream" while not streaming: the Go Live dialog.
    GoLive,
    /// The stream-audio button beside Stop.
    StreamAudio,
    /// A right click on mute: the input devices.
    InputDevices,
    /// A right click on deafen: the output devices.
    OutputDevices,
}

/// Handles a [`PanelAction`] (registered by the voice feature).
pub type PanelActionHandler = Rc<dyn Fn(PanelAction, gpui::Bounds<gpui::Pixels>, &mut Window, &mut App)>;

#[derive(Default)]
struct Registry {
    panes: HashMap<Slot, PaneBuilder>,
    overlays: Vec<PaneBuilder>,
    channel_menu: Vec<ChannelMenuExtension>,
    panel_action: Option<PanelActionHandler>,
}

impl Global for Registry {}

fn registry(cx: &mut App) -> &mut Registry {
    cx.default_global::<Registry>()
}

/// Fill `slot` with the view `build` makes (replacing the shell's default or placeholder).
pub fn register_pane(cx: &mut App, slot: Slot, build: impl Fn(&mut Window, &mut App) -> AnyView + 'static) {
    registry(cx).panes.insert(slot, Rc::new(build));
}

/// Draw the view `build` makes over the main layout while signed in (a modal, a popup, a menu:
/// it decides itself when it is open, from the model).
pub fn register_overlay(cx: &mut App, build: impl Fn(&mut Window, &mut App) -> AnyView + 'static) {
    registry(cx).overlays.push(Rc::new(build));
}

/// Add entries to the channel list's context menu.
pub fn register_channel_menu(cx: &mut App, entries: impl Fn(&ChannelMenuTarget, &mut App) -> Vec<MenuEntry> + 'static) {
    registry(cx).channel_menu.push(Rc::new(entries));
}

/// Handle the user panel's voice controls (one handler; the voice feature's).
pub fn register_user_panel_action(
    cx: &mut App,
    handler: impl Fn(PanelAction, gpui::Bounds<gpui::Pixels>, &mut Window, &mut App) + 'static,
) {
    registry(cx).panel_action = Some(Rc::new(handler));
}

/// Run the registered handler for `action` (nothing when no feature registered one).
pub(crate) fn user_panel_action(action: PanelAction, anchor: gpui::Bounds<gpui::Pixels>, window: &mut Window, cx: &mut App) {
    let handler = cx.try_global::<Registry>().and_then(|r| r.panel_action.clone());
    if let Some(handler) = handler {
        handler(action, anchor, window, cx);
    }
}

/// The view registered for `slot`, built now (a sub-slot another pane hosts, e.g.
/// [`Slot::DmCallStage`] in the DM pane): the host keeps it while it shows. `None` when no feature
/// registered one.
pub fn build_slot(slot: Slot, window: &mut Window, cx: &mut App) -> Option<AnyView> {
    pane_builder(cx, slot).map(|build| build(window, cx))
}

/// Whether a feature registered a view for `slot`.
pub fn is_registered(cx: &App, slot: Slot) -> bool {
    pane_builder(cx, slot).is_some()
}

pub(crate) fn pane_builder(cx: &App, slot: Slot) -> Option<PaneBuilder> {
    cx.try_global::<Registry>().and_then(|r| r.panes.get(&slot).cloned())
}

pub(crate) fn overlay_builders(cx: &App) -> Vec<PaneBuilder> {
    cx.try_global::<Registry>().map(|r| r.overlays.clone()).unwrap_or_default()
}

pub(crate) fn channel_menu_extensions(cx: &App) -> Vec<ChannelMenuExtension> {
    cx.try_global::<Registry>().map(|r| r.channel_menu.clone()).unwrap_or_default()
}

/// One place of the layout as a view of the layout's own: built when it first shows, kept while
/// it shows, dropped when it goes.
#[derive(Default)]
pub(crate) struct PaneHost {
    shown: Option<(Slot, AnyView)>,
}

impl PaneHost {
    /// The view for `slot`: the registered one, else `fallback` (the shell's default), else a
    /// placeholder naming the slot.
    pub fn view(
        &mut self,
        slot: Slot,
        window: &mut Window,
        cx: &mut App,
        fallback: Option<&PaneFallback>,
    ) -> AnyView {
        if let Some((shown, view)) = &self.shown
            && *shown == slot
        {
            return view.clone();
        }
        let view = match pane_builder(cx, slot) {
            Some(build) => build(window, cx),
            None => match fallback {
                Some(build) => build(window, cx),
                None => cx.new(|_| Placeholder { slot }).into(),
            },
        };
        self.shown = Some((slot, view.clone()));
        view
    }

    /// The place is not drawn any more: drop its view.
    pub fn clear(&mut self) {
        self.shown = None;
    }
}

/// What a place shows until its feature crate registers a view: the place's name.
pub struct Placeholder {
    slot: Slot,
}

impl Render for Placeholder {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Label::new(SharedString::new_static(self.slot.name()))
                    .font(FontRole::Display)
                    .size(13.)
                    .weight(500.)
                    .color(theme.color_text_muted),
            )
    }
}

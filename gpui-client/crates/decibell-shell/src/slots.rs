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
    /// Home: right of the conversation list (default: the shell's Friends page frame,
    /// `FriendsPage.tsx`).
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

#[derive(Default)]
struct Registry {
    panes: HashMap<Slot, PaneBuilder>,
    overlays: Vec<PaneBuilder>,
    channel_menu: Vec<ChannelMenuExtension>,
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

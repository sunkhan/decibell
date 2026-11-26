//! Communities outside the settings windows (`gpui-client/CONVENTIONS.md`, "Servers"):
//!
//! - [`browse`]: Discover (`ServerBrowseView.tsx`), the shell's `Slot::Browse`: the directory, its
//!   search, joining from it, by a pasted invite code or link, or by host and port.
//! - [`join`]: the confirm-then-join dialog for an invite link (`DeepLinkJoinModal.tsx`), open
//!   while `ServersStore::pending_invite` holds one. The OS's `decibell://` handler calls
//!   [`open_invite_link`].
//! - [`invites`]: the Invites dialog (`InviteModal.tsx`, `modal::INVITE_MANAGE`).
//! - [`invite_friends`]: Invite friends (`InviteFriendsModal.tsx` + `personalInvite.ts`,
//!   `modal::INVITE_FRIENDS`): personal invites DM'd to friends who aren't members.
//! - [`create_channel`]: the create dialog for channels and categories (`CreateChannelModal.tsx`),
//!   answering the shell's `ShellRequests::create_channel`.
//! - [`reorder`]: dragging the channel list (the shell's `ChannelReorder`) and the Sync / Keep /
//!   Cancel prompt (`CategorySyncPrompt.tsx`).
//! - [`welcome`]: the welcome banner of an empty text channel (`WelcomeState.tsx`), the chat
//!   pane's welcome slot.
//!
//! What the shell already draws and this crate leaves to it: the server-name dropdown
//! (`ServerActionsDropdown`), the channel list's context menu, the delete confirmation, the
//! leave-server confirmation and the kicked / banned notice (`MembershipRevokedToast`). The
//! renderer's server tiles have no context menu.

pub mod browse;
pub mod create_channel;
mod follow;
pub mod invite_friends;
pub mod invites;
pub mod join;
pub mod reorder;
mod ui;
pub mod welcome;

#[cfg(feature = "fixtures")]
pub mod fixture;
#[cfg(feature = "fixtures")]
mod screens;
mod selftests;

use std::rc::Rc;

use gpui::{App, AppContext as _, Global};

struct Initialised;

impl Global for Initialised {}

/// What a screen sets up that the views hold as their own state (what the user typed, a request
/// that is out): read by the views as they open. Never set in the app.
#[derive(Default)]
pub(crate) struct ScreenState {
    pub browse_search: Option<String>,
    pub invite_creating: bool,
    pub friends: Option<(invite_friends::CentralMembers, Vec<(decibell_model::Uid, invite_friends::RowState)>)>,
    pub create_fields: Option<(String, bool)>,
}

impl Global for ScreenState {}

pub(crate) fn screen_state(cx: &App) -> Option<&ScreenState> {
    cx.try_global::<ScreenState>()
}

/// The view of a kind on screen (each is drawn once: a pane or an overlay), for the self-tests
/// and the cards of the windowed lists, whose callbacks hold no view.
pub(crate) struct Current<T: 'static>(pub gpui::WeakEntity<T>);

impl<T: 'static> Global for Current<T> {}

pub(crate) fn current<T: 'static>(cx: &App) -> Option<gpui::Entity<T>> {
    cx.try_global::<Current<T>>().and_then(|c| c.0.upgrade())
}

/// Once, after `Model::init`, before the main window opens: Browse fills the shell's
/// `Slot::Browse`; the join, Invites, Invite friends, create-channel and category-sync dialogs
/// become overlays of the main layout; the channel list can be dragged; empty text channels show
/// the welcome banner.
pub fn init(cx: &mut App) {
    // Once: a screen built in the app (`--screen`) finds it done already.
    if cx.has_global::<Initialised>() {
        return;
    }
    cx.set_global(Initialised);
    decibell_shell::register_pane(cx, decibell_shell::Slot::Browse, |window, cx| cx.new(|cx| browse::BrowseView::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(join::JoinOverlay::new).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| invites::InviteManageOverlay::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| invite_friends::InviteFriendsOverlay::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| create_channel::CreateChannelOverlay::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(reorder::SyncPromptOverlay::new).into());
    let reorder = reorder::Reorder::new(cx);
    decibell_shell::register_channel_reorder(cx, Rc::new(reorder));
    decibell_chat::slots(cx).welcome = Some(Rc::new(welcome::build));
}

/// What the OS's `decibell://` handler (the single-instance socket, a link that started the app)
/// calls with the link it was given (`deep_link_received`): the join dialog opens for it, now if
/// signed in, else as soon as the main layout shows. Takes the code-only link
/// (`decibell://invite/<CODE>`) and the older endpoint-carrying shapes (`inviteLink.ts`'s grammar,
/// `decibell_model::invite_link`). False (and a warning in the log) when it doesn't parse.
pub fn open_invite_link(url: &str, cx: &mut App) -> bool {
    decibell_model::actions::servers::open_invite_link(url, cx)
}

/// The screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The self-tests: each starts the app again as a child (`--check`), against the local stack.
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    selftests::selftests()
}

/// The checks `decibell --check <name>` runs inside the app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    selftests::checks()
}

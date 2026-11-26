//! The people side of the GPUI client (`gpui-client/CONVENTIONS.md`, "Home / DMs / people"):
//!
//! - [`dm::DmPane`] ([`Slot::Dm`]): a conversation — header with the peer's status and the
//!   encryption chip, the encryption notices, the call stage slot, the message list on the
//!   peer's slice, the composer slot — and the Friends rail beside it.
//! - [`friends::FriendsPage`] ([`Slot::Home`]): tabs, search, Add Friend, the rows and their
//!   actions; [`friends::FriendsRail`] beside an open conversation.
//! - [`profile::ProfilePopup`] / [`profile::ProfileModal`]: the anchored popout and the profile
//!   screen (Profile and Privacy tabs).
//! - [`status::CustomStatusModal`]: "Set a custom status".
//! - [`menu::UserMenu`]: the user context menu, its moderation dialogs, and the registration API
//!   other crates add their entries through ([`menu::register_user_menu`]).
//!
//! Registration APIs other crates use:
//! - [`menu::register_user_menu`]: sections of the user menu (the voice crate's local mute,
//!   volume and voice moderation), ordered among this crate's own.
//! - [`dm::set_dm_composer`]: the DM composer under the conversation (the composer crate's).
//! - [`Slot::DmCallStage`]: the call stage above the conversation (the voice crate's), drawn
//!   while a call with the peer is outgoing, connecting or active.

pub mod dm;
pub mod e2ee;
mod fmt;
#[cfg(feature = "fixtures")]
pub mod fixture;
pub mod friends;
pub mod hooks;
pub mod menu;
mod place;
pub mod profile;
#[cfg(feature = "fixtures")]
mod screens;
mod selftests;
pub mod status;
pub mod ui;

use decibell_shell::Slot;
use gpui::{AppContext as _, Global};

/// Set once [`init`] ran in this process (screens call it too: captures skip the app's boot).
struct Registered;

impl Global for Registered {}

/// Once at start-up (the app's `registry::init_features`): fills the shell's `Slot::Home` and
/// `Slot::Dm`, and registers the profile popup, the profile screen, the custom status dialog and
/// the user menu as overlays. Safe to call again (a screen registers what it draws).
pub fn init(cx: &mut gpui::App) {
    if cx.has_global::<Registered>() {
        return;
    }
    cx.set_global(Registered);
    menu::register_own_sections(cx);
    // Nothing about a peer's keys carries over to the next account.
    if let Some(model) = cx.try_global::<decibell_model::Model>().cloned() {
        cx.observe(&model.auth, |auth, cx| {
            if !auth.read(cx).is_authenticated
                && let Some(store) = e2ee::PeerInfos::existing(cx)
            {
                store.update(cx, |s, cx| s.clear(cx));
            }
        })
        .detach();
    }
    decibell_shell::register_pane(cx, Slot::Home, |window, cx| cx.new(|cx| friends::FriendsPage::new(window, cx)).into());
    decibell_shell::register_pane(cx, Slot::Dm, |window, cx| cx.new(|cx| dm::DmPane::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| profile::ProfilePopup::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |_, cx| cx.new(profile::ProfileModal::new).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| status::CustomStatusModal::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| menu::UserMenu::new(window, cx)).into());
}

/// The screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The self-tests: each starts the app again in child processes (`selftests.rs`).
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    selftests::selftests()
}

/// The checks `decibell --check <name>` runs inside the app for those self-tests.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    selftests::checks()
}

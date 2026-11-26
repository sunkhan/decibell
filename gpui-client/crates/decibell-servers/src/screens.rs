//! The screens, drawn from [`crate::fixture`] (no network, no clock): `decibell-capture --screen
//! <name>` and `decibell --screen <name>`. Each is the signed-in shell (Daily Gaming `#general`)
//! with the stage's state: Browse, the join dialog, the invite dialogs, the sidebar's menus, the
//! create dialog, a drag, the sync prompt, an empty channel's welcome banner.

use decibell_harness::Screen;
use decibell_ui::AssumeClientDecorations;
use gpui::{AnyView, App, AppContext as _, Window};

use crate::fixture::{self, Create, Join, Role, Stage};

/// Captures render one frame on a clock that doesn't run: entrances show their end.
fn settle_motion_for_capture(cx: &mut App) {
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
}

fn build(stage: Stage, cx: &mut App) -> AnyView {
    settle_motion_for_capture(cx);
    // The welcome banner is the chat pane's: it must be there (the capture tool registers no
    // feature; the app's `init_features` does).
    if matches!(stage, Stage::WelcomeText | Stage::WelcomeEncrypted) {
        decibell_chat::init(cx);
    }
    crate::init(cx);
    fixture::apply(stage, cx);
    cx.new(decibell_shell::Shell::new).into()
}

macro_rules! screen {
    ($name:literal, $about:literal, $stage:expr) => {
        Screen { name: $name, about: $about, build: |_: &mut Window, cx: &mut App| build($stage, cx) }
    };
}

pub fn screens() -> Vec<Screen> {
    vec![
        screen!("browse", "Discover: the directory's cards (one with a picture), the search and invite boxes", Stage::Browse),
        screen!("browse-empty", "Discover with nothing to join: \"No servers found.\"", Stage::BrowseEmpty),
        screen!("browse-searching", "Discover filtered by a search (\"synth\")", Stage::BrowseSearching),
        screen!("join-dialog-resolving", "The deep-link join dialog while central resolves the code", Stage::Join(Join::Resolving)),
        screen!("join-dialog-resolved", "The join dialog with the community's name, description and members", Stage::Join(Join::Resolved)),
        screen!("join-dialog-expired", "The join dialog for an unknown or expired code", Stage::Join(Join::Expired)),
        screen!("join-dialog-member", "The join dialog for a community we are already in", Stage::Join(Join::Member)),
        screen!("invite-manage", "Invites: the create form and five invites (one personal), as the owner", Stage::InviteManage),
        screen!("invite-manage-creating", "Invites while a create is out (\"Creating...\")", Stage::InviteManageCreating),
        screen!("invite-manage-empty", "Invites with none yet", Stage::InviteManageEmpty),
        screen!("invite-manage-truncated", "Invites: only the most recent fit the reply", Stage::InviteManageTruncated),
        screen!("invite-friends", "Invite friends: friends who aren't members, one sent, one already here", Stage::InviteFriends),
        screen!("server-menu-owner", "The server menu, as the owner", Stage::ServerMenu(Role::Owner)),
        screen!("server-menu-moderator", "The server menu, as a moderator with Manage Invites", Stage::ServerMenu(Role::Moderator)),
        screen!("server-menu-member", "The server menu, as a member who can't create invites", Stage::ServerMenu(Role::Member)),
        screen!("leave-confirm", "Leave server: the confirmation", Stage::LeaveConfirm),
        screen!("create-channel-text", "Create text channel (from the list's menu)", Stage::CreateChannel(Create::Text)),
        screen!("create-channel-voice", "Create a channel from a category's +: the Text / Voice choice on Voice", Stage::CreateChannel(Create::Voice)),
        screen!("create-channel-category", "Create category", Stage::CreateChannel(Create::Category)),
        screen!("create-channel-encrypted", "Create text channel, named, end-to-end encrypted on", Stage::CreateChannel(Create::Encrypted)),
        screen!("channel-menu", "The channel list's context menu on #general (as the owner)", Stage::ChannelMenu),
        screen!("sidebar-dragging-channel", "#clips dragged over #general: the drop line above it", Stage::DraggingChannel),
        screen!("sidebar-dragging-category", "Voice Channels dragged above Information", Stage::DraggingCategory),
        screen!("category-sync-prompt", "#announcements dropped into the syncing Text Channels: Sync / Keep / Cancel", Stage::CategorySyncPrompt),
        screen!("welcome-text", "An empty text channel (#clips): the welcome banner with both actions", Stage::WelcomeText),
        screen!("welcome-encrypted", "An empty encrypted channel (#vault): the same banner", Stage::WelcomeEncrypted),
    ]
}

//! Friends: the page Home gives everything right of the conversation list
//! (`features/friends/FriendsPage.tsx`) and the 260 px rail beside an open conversation
//! (`FriendsList.tsx`). Both read the friends store; names, statuses and messages through the
//! users and presence stores, for the people they show.

mod page;
mod rail;

pub use page::{Filter, FriendsPage};
pub use rail::{FriendsRail, RAIL_WIDTH};

use decibell_model::live::friends::FriendStatus;
use decibell_model::{Model, Uid, actions};
use gpui::{App, Global};

/// What the next Friends page starts on (the screens and the self-tests; the renderer always
/// starts on "All" with the form closed).
#[derive(Default)]
pub(crate) struct Initial(pub Option<(Filter, bool)>);

impl Global for Initial {}

/// Start the next Friends page on `filter`, with the Add Friend form open or not.
pub fn set_initial(filter: Filter, add_open: bool, cx: &mut App) {
    cx.default_global::<Initial>().0 = Some((filter, add_open));
}

/// `STATUS_LABEL` of the friends page (a row that isn't an accepted friend says what it is).
pub fn status_label(status: FriendStatus) -> &'static str {
    match status {
        FriendStatus::Online => "Online",
        FriendStatus::Offline => "Offline",
        FriendStatus::PendingIncoming => "Wants to be friends",
        FriendStatus::PendingOutgoing => "Request sent",
        FriendStatus::Blocked => "Blocked",
    }
}

/// The DM view with `uid`'s conversation (created when new): `setActiveDmUser(uid)` +
/// `setActiveView("dm")`.
pub fn open_conversation(uid: Uid, cx: &mut App) {
    let fallback = Model::global(cx).live.friends.read(cx).get(uid).map(|f| f.username.clone()).unwrap_or_default();
    let name = decibell_common::user_name(uid, &fallback, cx);
    actions::dm::open_dm(uid, name, cx);
}

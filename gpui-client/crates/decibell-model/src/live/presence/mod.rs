//! Statuses, status messages, our own presence, the watch set, auto-away and the avatar
//! cache (`stores/presenceStore.ts`, `stores/avatarStore.ts`, `features/presence/`).
//!
//! | Entity | Changes | Keyed event |
//! |---|---|---|
//! | `statuses` ([`Presence`]) | a watched user's status | `PresenceChanged(uids)` |
//! | `messages` ([`StatusMessages`]) | a watched user's custom status | `MessagesChanged(uids)` |
//! | `own` ([`OwnPresence`]) | our status / message / idle, from any device | — |
//! | `interest` ([`PresenceInterest`]) | views mounting / unmounting | never notifies |
//! | `auto_away` ([`AutoAway`]) | our idle flag | — |
//! | `avatars` ([`Avatars`]) | announced versions, finished fetches | `AvatarsChanged(uids)` |

pub mod auto_away;
pub mod avatars;
pub mod interest;
pub mod statuses;

use std::rc::Rc;

use gpui::{App, AppContext as _, Entity};

pub use auto_away::{AutoAway, IdleReading, IdleSource};
pub use avatars::Avatars;
pub use interest::PresenceInterest;
pub use statuses::{DisplayStatus, OwnPresence, OwnStatus, Presence, PresenceStatus, StatusMessage, StatusMessages};

use crate::ids::Uid;

#[derive(Clone)]
pub struct PresenceStores {
    pub statuses: Entity<Presence>,
    pub messages: Entity<StatusMessages>,
    pub own: Entity<OwnPresence>,
    pub interest: Entity<PresenceInterest>,
    pub auto_away: Entity<AutoAway>,
    pub avatars: Entity<Avatars>,
}

impl PresenceStores {
    pub(crate) fn new(cx: &mut App) -> PresenceStores {
        let own = cx.new(|_| OwnPresence::default());
        let interest = cx.new(|_| PresenceInterest::new(Rc::new(crate::actions::presence::subscribe)));
        let avatars = cx.new(|_| Avatars::new(Rc::new(crate::actions::presence::fetch_avatar), interest.downgrade()));
        let auto_away = cx.new(|_| AutoAway::new(Rc::new(crate::actions::presence::report_idle)));
        PresenceStores {
            statuses: cx.new(|_| Presence::default()),
            messages: cx.new(|_| StatusMessages::default()),
            own,
            interest,
            auto_away,
            avatars,
        }
    }

    /// One user's dot (`usePresence`): ourselves as others see us, anyone else from
    /// central (the community's flag until central answered). The view must also retain
    /// the uid in `interest` while it is shown.
    pub fn display(&self, me: Uid, uid: Uid, community_online: bool, cx: &App) -> DisplayStatus {
        if uid > 0 && uid == me {
            return self.own.read(cx).display();
        }
        self.statuses.read(cx).display(uid, community_online)
    }

    /// One user's custom status (`useStatusMessage`): ours shows to us even while
    /// invisible; others' only while they are visible.
    pub fn message<'a>(&self, me: Uid, uid: Uid, cx: &'a App) -> Option<&'a StatusMessage> {
        if uid > 0 && uid == me {
            return self.own.read(cx).message.as_ref();
        }
        self.messages.read(cx).get(uid)
    }
}

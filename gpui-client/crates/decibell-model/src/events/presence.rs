//! Statuses, status messages, our own presence, pictures (`features/presence/
//! usePresenceEvents.ts`, `useAuthEvents.ts`' `avatar_changed`, `avatarStore.seed`).

use std::time::Duration;

use decibell_core::events::{AvatarChangedPayload, OwnPresencePayload, PresenceDeltaPayload, UserListUpdatedPayload};
use gpui::{App, SharedString};

use crate::Model;
use crate::ids::Uid;
use crate::live::presence::StatusMessage;

/// A subscription answer: the newly watched users' presence (those not in `users` are
/// offline) and every one's picture (the presence entry's, else `avatars`', else none).
/// Accepted friends flip online / offline with it.
pub fn user_list_updated(model: &Model, p: &UserListUpdatedPayload, cx: &mut App) {
    let presence = model.live.presence.clone();
    presence.statuses.update(cx, |s, cx| {
        s.apply_covered(&p.covered, &p.users, cx);
    });
    presence.messages.update(cx, |m, cx| m.apply_covered(&p.covered, &p.users, cx));
    let versions: Vec<(Uid, SharedString)> = p
        .covered
        .iter()
        .map(|uid| {
            let version = p
                .users
                .iter()
                .find(|u| u.uid == *uid)
                .map(|u| u.avatar_version.clone())
                .or_else(|| p.avatars.iter().find(|a| a.uid == *uid).map(|a| a.version.clone()))
                .unwrap_or_default();
            (*uid, version.into())
        })
        .collect();
    presence.avatars.update(cx, |a, cx| a.set_versions(versions, cx));
    let online: std::collections::HashSet<Uid> = p.users.iter().map(|u| u.uid).collect();
    let answered: std::collections::HashSet<Uid> = p.covered.iter().copied().collect();
    model
        .live
        .friends
        .update(cx, |f, cx| f.reconcile(|uid| answered.contains(&uid).then(|| online.contains(&uid)), cx));
}

/// One watched user's full current presence.
pub fn presence_delta(model: &Model, p: &PresenceDeltaPayload, cx: &mut App) {
    if p.uid <= 0 {
        return;
    }
    let presence = model.live.presence.clone();
    presence.statuses.update(cx, |s, cx| {
        s.apply_delta(p.uid, p.online, &p.status, cx);
    });
    presence.messages.update(cx, |m, cx| m.apply_delta(p.uid, p.online, &p.status_text, &p.status_emoji, cx));
    if p.online {
        presence.avatars.update(cx, |a, cx| a.set_versions([(p.uid, p.avatar_version.clone().into())], cx));
    }
    let (uid, online) = (p.uid, p.online);
    let shown: std::collections::HashMap<Uid, bool> = {
        let statuses = presence.statuses.read(cx);
        model
            .live
            .friends
            .read(cx)
            .list()
            .iter()
            .map(|f| (f.uid, if f.uid == uid { online } else { statuses.status(f.uid).is_some() }))
            .collect()
    };
    model.live.friends.update(cx, |f, cx| f.reconcile(|u| shown.get(&u).copied(), cx));
}

/// Our own state: just before `login_succeeded`, and whenever a device of ours changes
/// it. The account's "friends only DMs" is adopted rather than pushed (the last device to
/// sign in used to win): `Settings::friends_only_dms` takes central's value.
pub fn own_presence(model: &Model, p: &OwnPresencePayload, cx: &mut App) {
    let message = StatusMessage::of(&p.status_text, &p.status_emoji);
    let clears_in = if p.status_clear_in.is_finite() && p.status_clear_in > 0.0 {
        Duration::from_secs_f64(p.status_clear_in)
    } else {
        Duration::ZERO
    };
    let now = cx.background_executor().now();
    model
        .live
        .presence
        .own
        .update(cx, |o, cx| o.apply_account(p.supported, &p.status, message, clears_in, p.account_idle, now, cx));
    if model.settings.read(cx).get().friends_only_dms != p.dm_friends_only {
        let value = p.dm_friends_only;
        model.settings.update(cx, |s, cx| s.change(|v| v.friends_only_dms = value, cx));
    }
}

/// A user's picture changed (a watcher's push, or our own upload).
pub fn avatar_changed(model: &Model, p: &AvatarChangedPayload, cx: &mut App) {
    model.live.presence.avatars.update(cx, |a, cx| a.set_versions([(p.uid, p.version.clone().into())], cx));
}

/// Sign-in: nothing sends our own picture's version (we don't watch ourselves).
pub fn seed_own_avatar(model: &Model, uid: Uid, cx: &mut App) {
    model.live.presence.avatars.update(cx, |a, cx| a.seed(uid, cx));
}

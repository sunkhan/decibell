//! Users by uid: the one place a uid's current name comes from (`stores/usersStore.ts`).
//!
//! Every store keys people on their central account uid; names are only what gets drawn.
//! Fed by every event that carries a name with its uid (`events/users.rs`) and by
//! renames, so a rename changes one entry here and nothing else.
//!
//! Only real accounts (uid > 0) live here. A community's placeholder for a name it never
//! saw with a uid is negative and unique only within that community: callers draw those
//! from the member row they came with (`Roster::display_name` does).

use std::collections::{HashMap, HashSet};

use gpui::{Context, EventEmitter, SharedString};

use crate::ids::{Uid, is_account};

#[derive(Default)]
pub struct UsersStore {
    /// uid → the latest name the servers sent for it.
    names: HashMap<Uid, SharedString>,
    /// uids whose name came from central (this run).
    from_central: HashSet<Uid>,
}

/// The uids whose name changed in one `remember`. A view that shows a few people
/// subscribes and re-renders only when one of them is listed; a view that shows many
/// just observes the store.
pub struct NamesChanged(pub Vec<Uid>);

impl EventEmitter<NamesChanged> for UsersStore {}

impl UsersStore {
    /// Record `(uid, name)` pairs: one notification for the batch, none when nothing
    /// changed. `from_central`: central always sends the current name, so it wins. A
    /// community's name can be older (it learns a rename when the member next connects
    /// to it), so it only fills in a uid central hasn't named yet.
    pub fn remember<'a>(
        &mut self,
        pairs: impl IntoIterator<Item = (Uid, &'a str)>,
        from_central: bool,
        cx: &mut Context<Self>,
    ) {
        let changed = self.remember_quietly(pairs, from_central);
        if !changed.is_empty() {
            cx.emit(NamesChanged(changed));
            cx.notify();
        }
    }

    /// `remember` without the notification; returns the uids whose name changed.
    pub(crate) fn remember_quietly<'a>(
        &mut self,
        pairs: impl IntoIterator<Item = (Uid, &'a str)>,
        from_central: bool,
    ) -> Vec<Uid> {
        let mut changed = Vec::new();
        for (uid, name) in pairs {
            if !is_account(uid) || name.is_empty() {
                continue;
            }
            if from_central {
                self.from_central.insert(uid);
            } else if self.from_central.contains(&uid) {
                continue;
            }
            if self.names.get(&uid).is_some_and(|known| &**known == name) {
                continue;
            }
            self.names.insert(uid, SharedString::from(name.to_string()));
            changed.push(uid);
        }
        changed
    }

    /// The current name of `uid`, if the servers told us one.
    pub fn get(&self, uid: Uid) -> Option<&SharedString> {
        if is_account(uid) { self.names.get(&uid) } else { None }
    }

    /// The current name of `uid`, or `fallback` (the name a payload carried) while the
    /// servers haven't told us one. People never see uids: with no fallback either, a
    /// view shows "Unknown user".
    pub fn name_of(&self, uid: Uid, fallback: &SharedString) -> SharedString {
        self.get(uid).unwrap_or(fallback).clone()
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Every known name, for the headless dump.
    pub fn iter(&self) -> impl Iterator<Item = (Uid, &SharedString)> {
        self.names.iter().map(|(uid, name)| (*uid, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(store: &UsersStore, uid: Uid) -> String {
        store.name_of(uid, &"fallback".into()).to_string()
    }

    #[test]
    fn central_wins_and_a_community_only_fills_gaps() {
        let mut users = UsersStore::default();
        // A community names 7 and 8 first.
        assert_eq!(users.remember_quietly([(7, "perf2"), (8, "perf3")], false), vec![7, 8]);
        assert_eq!(name(&users, 7), "perf2");
        // Central renames 7: it wins.
        assert_eq!(users.remember_quietly([(7, "perf-two")], true), vec![7]);
        assert_eq!(name(&users, 7), "perf-two");
        // The community still holds the old name: ignored from now on.
        assert!(users.remember_quietly([(7, "perf2")], false).is_empty());
        assert_eq!(name(&users, 7), "perf-two");
        // 8 was never named by central: another community may still update it.
        assert_eq!(users.remember_quietly([(8, "perf3b")], false), vec![8]);
        // Central's later names keep winning.
        assert_eq!(users.remember_quietly([(7, "perf-2")], true), vec![7]);
        assert_eq!(name(&users, 7), "perf-2");
    }

    #[test]
    fn only_accounts_with_names_are_kept_and_repeats_change_nothing() {
        let mut users = UsersStore::default();
        assert!(users.remember_quietly([(0, "system"), (-5, "placeholder"), (9, "")], true).is_empty());
        assert!(users.is_empty());
        assert_eq!(name(&users, -5), "fallback");
        assert_eq!(name(&users, 0), "fallback");
        assert_eq!(users.remember_quietly([(1, "alice")], true), vec![1]);
        assert!(users.remember_quietly([(1, "alice")], true).is_empty());
        assert!(users.remember_quietly([(1, "alice")], false).is_empty());
        assert_eq!(users.len(), 1);
    }

    #[test]
    fn a_central_sighting_with_the_same_name_still_marks_the_uid_as_centrals() {
        let mut users = UsersStore::default();
        users.remember_quietly([(3, "carol")], false);
        // Same name from central: nothing changes on screen, but central now owns it.
        assert!(users.remember_quietly([(3, "carol")], true).is_empty());
        assert!(users.remember_quietly([(3, "old-carol")], false).is_empty());
        assert_eq!(name(&users, 3), "carol");
    }
}

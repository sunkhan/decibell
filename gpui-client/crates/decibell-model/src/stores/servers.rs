//! The communities the account belongs to, central's public directory, their pictures
//! and a pending invite (`chatStore`'s `servers`, `discoverableServers`,
//! `serverPictureVersions`, `serverPictures`, `pendingInvite`).

use std::collections::{HashMap, HashSet};

use decibell_core::events::ServerInfo;
use gpui::{Context, SharedString};

use crate::ids::ServerId;

/// One community as central (or its own authentication answer) describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub id: ServerId,
    pub name: SharedString,
    pub description: SharedString,
    pub host: SharedString,
    pub port: i32,
    pub member_count: i32,
}

impl From<&ServerInfo> for Server {
    fn from(s: &ServerInfo) -> Server {
        Server {
            id: ServerId::from_central(s.id),
            name: s.name.clone().into(),
            description: s.description.clone().into(),
            host: s.host_ip.clone().into(),
            port: s.port,
            member_count: s.member_count,
        }
    }
}

/// An invite waiting for the confirm-then-join prompt (a clicked or deep link).
/// `host` / `port` only for the older endpoint-carrying links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInvite {
    pub code: SharedString,
    pub host: Option<SharedString>,
    pub port: Option<u16>,
}

#[derive(Default)]
pub struct ServersStore {
    /// Communities the user is a **member** of: the server bar's source. Filled from the
    /// sign-in reply's memberships and each community's authentication answer; never
    /// from the directory, so a directory refresh can't add or remove the user's tiles
    /// (and unlisted communities, which the directory never names, still show).
    servers: Vec<Server>,
    /// Central's public directory: the Discover view's source, replaced on each refresh.
    discoverable: Vec<Server>,
    /// sha256-hex of each community's picture; empty = none set.
    picture_versions: HashMap<ServerId, SharedString>,
    /// The picture itself as central sent it (a `data:` URL), fetched lazily by the tile
    /// that sees a version without bytes. Dropped when the version changes.
    pictures: HashMap<ServerId, SharedString>,
    /// Communities whose picture editor is saving: the editor shows its own refusal, so
    /// the event handler doesn't toast for these (`pictureSavesInFlight`).
    pub picture_saves_in_flight: HashSet<ServerId>,
    pub pending_invite: Option<PendingInvite>,
}

impl ServersStore {
    pub fn servers(&self) -> &[Server] {
        &self.servers
    }

    pub fn discoverable(&self) -> &[Server] {
        &self.discoverable
    }

    pub fn get(&self, id: ServerId) -> Option<&Server> {
        self.servers.iter().find(|s| s.id == id)
    }

    /// Add the entries not held yet (first one wins; an entry already held is left as
    /// it is). No notification when nothing was new.
    pub fn merge(&mut self, entries: impl IntoIterator<Item = Server>, cx: &mut Context<Self>) {
        let before = self.servers.len();
        for entry in entries {
            if !self.servers.iter().any(|s| s.id == entry.id) {
                self.servers.push(entry);
            }
        }
        if self.servers.len() != before {
            cx.notify();
        }
    }

    pub fn set_discoverable(&mut self, entries: Vec<Server>, cx: &mut Context<Self>) {
        self.discoverable = entries;
        cx.notify();
    }

    /// A community renamed itself: keep its tile's name in step.
    pub fn rename(&mut self, id: ServerId, name: &str, description: &str, cx: &mut Context<Self>) {
        if let Some(server) = self.servers.iter_mut().find(|s| s.id == id)
            && (&*server.name != name || &*server.description != description)
        {
            server.name = name.to_string().into();
            server.description = description.to_string().into();
            cx.notify();
        }
    }

    pub fn picture_version(&self, id: ServerId) -> &str {
        self.picture_versions.get(&id).map(|v| &**v).unwrap_or("")
    }

    pub fn picture(&self, id: ServerId) -> Option<&SharedString> {
        self.pictures.get(&id)
    }

    /// Set a community's picture version. A changed version drops the cached bytes, so
    /// the next tile that shows it fetches the new picture. Idempotent.
    pub fn set_picture_version(&mut self, id: ServerId, version: &str, cx: &mut Context<Self>) {
        if self.picture_version(id) == version {
            return;
        }
        self.picture_versions.insert(id, version.to_string().into());
        self.pictures.remove(&id);
        cx.notify();
    }

    /// Keep fetched picture bytes, unless the version moved on while the fetch was out.
    pub fn set_picture_data(&mut self, id: ServerId, version: &str, data: SharedString, cx: &mut Context<Self>) {
        if self.picture_version(id) != version {
            return;
        }
        self.pictures.insert(id, data);
        cx.notify();
    }

    pub fn set_pending_invite(&mut self, invite: Option<PendingInvite>, cx: &mut Context<Self>) {
        if self.pending_invite != invite {
            self.pending_invite = invite;
            cx.notify();
        }
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        *self = ServersStore::default();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn server(id: i32, name: &str) -> Server {
        Server {
            id: ServerId::from_central(id),
            name: name.to_string().into(),
            description: "".into(),
            host: "127.0.0.11".into(),
            port: 8082,
            member_count: 0,
        }
    }

    #[gpui::test]
    fn merge_keeps_the_first_entry_and_pictures_follow_their_version(cx: &mut TestAppContext) {
        let store = cx.new(|_| ServersStore::default());
        let notified = std::rc::Rc::new(std::cell::Cell::new(0));
        let _obs = cx.update(|cx| {
            let notified = notified.clone();
            cx.observe(&store, move |_, _| notified.set(notified.get() + 1))
        });
        let id = ServerId::from_central(1);
        store.update(cx, |s, cx| s.merge([server(1, "Daily Gaming"), server(2, "Rustaceans")], cx));
        cx.run_until_parked();
        assert_eq!(notified.get(), 1);
        // The community's own answer for a server we hold changes nothing (and notifies nobody).
        store.update(cx, |s, cx| s.merge([server(1, "renamed elsewhere")], cx));
        cx.run_until_parked();
        assert_eq!(notified.get(), 1);
        store.read_with(cx, |s, _| assert_eq!(&*s.get(id).unwrap().name, "Daily Gaming"));

        store.update(cx, |s, cx| {
            s.set_picture_version(id, "v1", cx);
            // A fetch for an older version landing late is dropped.
            s.set_picture_data(id, "v0", "data:old".into(), cx);
            assert!(s.picture(id).is_none());
            s.set_picture_data(id, "v1", "data:one".into(), cx);
            assert_eq!(s.picture(id).map(|d| &**d), Some("data:one"));
            // Same version again: the bytes stay.
            s.set_picture_version(id, "v1", cx);
            assert!(s.picture(id).is_some());
            // A new version drops them.
            s.set_picture_version(id, "v2", cx);
            assert!(s.picture(id).is_none());
            assert_eq!(s.picture_version(id), "v2");
            assert_eq!(s.picture_version(ServerId::from_central(99)), "");
        });
    }
}

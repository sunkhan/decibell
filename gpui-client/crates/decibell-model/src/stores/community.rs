//! One community's state, as separate entities so a change re-renders only the views
//! that show that part (`chatStore`'s per-server maps):
//!
//! | Entity | Holds | Changes |
//! |---|---|---|
//! | [`CommunityMeta`] | name, description, owner, listing, attachment endpoint | settings edits |
//! | [`Channels`] | the ordered channel list (categories inline), overwrites | channel edits |
//! | [`Roles`] | the role list | role edits |
//! | [`Roster`] | loaded members, paging state, the sidebar's rows | every join / leave / presence flip |
//! | [`Moderation`] | bans, invites | moderation |
//!
//! [`Communities`] is the registry: it notifies only when a community appears, is
//! re-keyed or goes. A community's entities are created by its first event and live
//! until sign-out, like the renderer's map entries.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use decibell_core::events::{
    BanInfoPayload, ChannelInfoPayload, ChannelOverwritePayload, InviteInfoPayload, MemberInfoPayload,
    RoleInfoPayload,
};
use gpui::{App, AppContext as _, Context, Entity, SharedString, Task};

use crate::ids::{ChannelId, ServerId, Uid};
use crate::stores::permissions::has_bits;
use crate::stores::roster_order::RosterKey;

// ── Registry ────────────────────────────────────────────────────────────────

/// The entities of one community. Cheap to clone (handles).
#[derive(Clone)]
pub struct Community {
    pub id: ServerId,
    pub meta: Entity<CommunityMeta>,
    pub channels: Entity<Channels>,
    pub roles: Entity<Roles>,
    pub roster: Entity<Roster>,
    pub moderation: Entity<Moderation>,
}

pub struct Communities {
    map: HashMap<ServerId, Community>,
    /// Given to every roster: how its page and lookup requests leave.
    requester: MemberRequester,
}

impl Communities {
    pub fn new(requester: MemberRequester) -> Communities {
        Communities {
            map: HashMap::new(),
            requester,
        }
    }

    pub fn get(&self, id: ServerId) -> Option<&Community> {
        self.map.get(&id)
    }

    pub fn ids(&self) -> impl Iterator<Item = ServerId> + '_ {
        self.map.keys().copied()
    }

    /// The community's entities, created empty on first use.
    pub fn ensure(&mut self, id: ServerId, cx: &mut Context<Self>) -> Community {
        if let Some(community) = self.map.get(&id) {
            return community.clone();
        }
        let community = Community {
            id,
            meta: cx.new(|_| CommunityMeta::default()),
            channels: cx.new(|_| Channels::default()),
            roles: cx.new(|_| Roles::default()),
            roster: cx.new(|_| {
                let mut roster = Roster::new(id);
                roster.set_requester(self.requester.clone());
                roster
            }),
            moderation: cx.new(|_| Moderation::default()),
        };
        self.map.insert(id, community.clone());
        cx.notify();
        community
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        self.map.clear();
        cx.notify();
    }
}

// ── Meta ────────────────────────────────────────────────────────────────────

/// `serverMeta`, `serverOwner`, `serverPublicListing`, `serverAttachmentConfig`.
#[derive(Default, Debug, Clone, PartialEq)]
pub struct CommunityMeta {
    pub name: SharedString,
    pub description: SharedString,
    /// The owner's uid (0 until the community told us).
    pub owner: Uid,
    pub public_listing: bool,
    /// The attachment HTTPS port the community advertises; 0 = none (HTTP disabled or
    /// an older build).
    pub attachment_port: i32,
    pub max_attachment_bytes: i64,
}

impl CommunityMeta {
    pub fn set_identity(&mut self, name: &str, description: &str, owner: Uid, public_listing: bool, cx: &mut Context<Self>) {
        self.name = name.to_string().into();
        self.description = description.to_string().into();
        self.owner = owner;
        self.public_listing = public_listing;
        cx.notify();
    }

    pub fn set_attachment_config(&mut self, port: i32, max_bytes: i64, cx: &mut Context<Self>) {
        if (self.attachment_port, self.max_attachment_bytes) != (port, max_bytes) {
            self.attachment_port = port;
            self.max_attachment_bytes = max_bytes;
            cx.notify();
        }
    }
}

// ── Channels ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    Text,
    Voice,
    /// A grouping header living in the same ordered list (Discord's flat model): a
    /// channel belongs to the nearest category above it; channels before the first
    /// category are uncategorized.
    Category,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub id: ChannelId,
    pub name: SharedString,
    pub kind: ChannelKind,
    pub voice_bitrate_kbps: i32,
    pub retention_days_text: i32,
    pub retention_days_image: i32,
    pub retention_days_video: i32,
    pub retention_days_document: i32,
    pub retention_days_audio: i32,
    /// The local user's resolved permissions in this channel (role bits with the
    /// channel's overwrites applied; owner / ADMINISTRATOR = everything). Channel lists
    /// arrive per recipient and only contain channels the user can VIEW. 0 is a real
    /// answer: a category header listed only because a channel under it is visible
    /// resolves to nothing.
    pub my_permissions: u64,
    /// Text channels: seconds between messages per member (0 = off).
    pub slowmode_seconds: i32,
    /// Text channels: end-to-end encrypted. Chosen at creation, fixed for life.
    pub encrypted: bool,
    /// Follows its category's overwrites (permissions v3).
    pub permissions_synced: bool,
    /// Category rows: channels may follow its overwrites.
    pub category_sync: bool,
    /// Text channels: what the channel is for; empty = none.
    pub topic: SharedString,
}

impl From<&ChannelInfoPayload> for Channel {
    fn from(c: &ChannelInfoPayload) -> Channel {
        Channel {
            id: c.id.clone().into(),
            name: c.name.clone().into(),
            kind: match c.r#type.as_str() {
                "text" => ChannelKind::Text,
                "voice" => ChannelKind::Voice,
                "category" => ChannelKind::Category,
                _ => ChannelKind::Unknown,
            },
            voice_bitrate_kbps: c.voice_bitrate_kbps,
            retention_days_text: c.retention_days_text,
            retention_days_image: c.retention_days_image,
            retention_days_video: c.retention_days_video,
            retention_days_document: c.retention_days_document,
            retention_days_audio: c.retention_days_audio,
            my_permissions: c.my_permissions,
            slowmode_seconds: c.slowmode_seconds,
            encrypted: c.encrypted,
            permissions_synced: c.permissions_synced,
            category_sync: c.category_sync,
            topic: c.topic.clone().into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OverwriteTarget {
    Role,
    Member,
}

/// One per-channel permission overwrite (permissions v2).
#[derive(Debug, Clone, PartialEq)]
pub struct Overwrite {
    pub target_type: OverwriteTarget,
    /// Role id (decimal string); member rows carry the member's current name here as a
    /// label.
    pub target_id: SharedString,
    /// Member rows: the member's uid (their identity); 0 for roles.
    pub target_uid: Uid,
    pub allow: u64,
    pub deny: u64,
}

impl From<&ChannelOverwritePayload> for Overwrite {
    fn from(o: &ChannelOverwritePayload) -> Overwrite {
        Overwrite {
            target_type: if o.target_type == "member" { OverwriteTarget::Member } else { OverwriteTarget::Role },
            target_id: o.target_id.clone().into(),
            target_uid: o.target_uid,
            allow: o.allow,
            deny: o.deny,
        }
    }
}

#[derive(Default)]
pub struct Channels {
    /// In display order.
    channels: Vec<Channel>,
    /// Only for channels whose overwrites the local user may view (MANAGE_ROLES /
    /// MANAGE_CHANNELS there); refreshed by the server's push after any change.
    overwrites: HashMap<ChannelId, Rc<[Overwrite]>>,
}

impl Channels {
    pub fn list(&self) -> &[Channel] {
        &self.channels
    }

    pub fn get(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| &*c.id == id)
    }

    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    pub fn first_text(&self) -> Option<&Channel> {
        self.channels.iter().find(|c| c.kind == ChannelKind::Text)
    }

    /// The category a channel sits under: the nearest category row above it.
    pub fn category_of(&self, id: &str) -> Option<&Channel> {
        let index = self.channels.iter().position(|c| &*c.id == id)?;
        if self.channels[index].kind == ChannelKind::Category {
            return None;
        }
        self.channels[..index].iter().rev().find(|c| c.kind == ChannelKind::Category)
    }

    /// Replace the list (`community_auth_responded`, `channel_list_updated`).
    pub fn set_all(&mut self, channels: Vec<Channel>, cx: &mut Context<Self>) {
        self.channels = channels;
        cx.notify();
    }

    /// One channel changed (`channel_updated`). An update that carries no permissions
    /// (0: a viewer always holds at least VIEW) keeps the ones we have instead of
    /// locking us out of the channel (review B1). An unknown channel is appended.
    pub fn upsert(&mut self, mut channel: Channel, cx: &mut Context<Self>) {
        match self.channels.iter_mut().find(|c| c.id == channel.id) {
            Some(existing) => {
                if channel.my_permissions == 0 {
                    channel.my_permissions = existing.my_permissions;
                }
                if *existing == channel {
                    return;
                }
                *existing = channel;
            }
            None => self.channels.push(channel),
        }
        cx.notify();
    }

    pub fn overwrites(&self, channel: &str) -> Option<&Rc<[Overwrite]>> {
        self.overwrites.get(channel)
    }

    pub fn set_overwrites(&mut self, channel: ChannelId, overwrites: Vec<Overwrite>, cx: &mut Context<Self>) {
        self.overwrites.insert(channel, overwrites.into());
        cx.notify();
    }

    /// Whether the local user holds `perm` (every bit of it) in `channel`, from the
    /// server-resolved `my_permissions` (`useChannelPermission`). `is_owner`: the local
    /// user owns the community, which always passes. A channel we don't hold is "no".
    pub fn permits(&self, channel: &str, perm: u64, is_owner: bool) -> bool {
        if is_owner {
            return true;
        }
        self.get(channel).is_some_and(|c| has_bits(c.my_permissions, perm))
    }
}

// ── Roles ───────────────────────────────────────────────────────────────────

/// A community role (`RoleInfo` on the wire).
#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    pub id: i64,
    pub name: SharedString,
    /// 0xRRGGBB display colour; 0 = default (no colour).
    pub color: u32,
    /// Dense hierarchy position; higher = more senior; 0 = `everyone`.
    pub position: i32,
    /// Bitfield of [`crate::stores::permissions::perm`] values.
    pub permissions: u64,
    /// The seeded `everyone` role: undeletable, position 0, implicit on every member.
    pub is_default: bool,
    /// Members whose highest role is this one may manage each other: nicknames and voice
    /// moderation, never kick / ban / timeout / roles.
    pub manage_each_other: bool,
    /// Members holding the role (every member for `everyone`).
    pub member_count: i64,
}

impl From<&RoleInfoPayload> for Role {
    fn from(r: &RoleInfoPayload) -> Role {
        Role {
            id: r.id,
            name: r.name.clone().into(),
            color: r.color,
            position: r.position,
            permissions: r.permissions,
            is_default: r.is_default,
            manage_each_other: r.manage_each_other,
            member_count: r.member_count,
        }
    }
}

/// The role list, most senior first, `everyone` last. Empty for a legacy server that
/// predates roles: permission checks fall back to owner-only gating then.
#[derive(Default)]
pub struct Roles {
    roles: Rc<[Role]>,
}

impl Roles {
    pub fn list(&self) -> &[Role] {
        &self.roles
    }

    pub fn get(&self, id: i64) -> Option<&Role> {
        self.roles.iter().find(|r| r.id == id)
    }

    pub fn set(&mut self, roles: Vec<Role>, cx: &mut Context<Self>) {
        self.roles = roles.into();
        cx.notify();
    }
}

// ── Members ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    /// The name the community holds; draw [`crate::Model::display_name`].
    pub username: SharedString,
    /// The account (users by uid); what moderation requests target. Negative for a
    /// community placeholder.
    pub uid: Uid,
    pub joined_at: i64,
    /// The server nickname; empty = none.
    pub nickname: SharedString,
    pub is_owner: bool,
    pub is_online: bool,
    /// Ids of the member's assigned roles (the default `everyone` role is implicit and
    /// never listed).
    pub role_ids: Rc<[i64]>,
    /// Unix seconds until which the member is timed out; 0 = none.
    pub timed_out_until: i64,
    /// Their place in roster order, computed once per version of the member.
    pub key: RosterKey,
}

impl From<&MemberInfoPayload> for Member {
    fn from(m: &MemberInfoPayload) -> Member {
        let username: SharedString = m.username.clone().into();
        let nickname: SharedString = m.nickname.clone().into();
        Member {
            key: RosterKey::new(&nickname, &username, m.uid),
            username,
            uid: m.uid,
            joined_at: m.joined_at,
            nickname,
            is_owner: m.is_owner,
            is_online: m.is_online,
            role_ids: distinct(&m.role_ids),
            timed_out_until: m.timed_out_until,
        }
    }
}

/// A member's role ids without repeats, order kept. The community server sends the
/// requester's own roles twice on a first roster page that also lists them (its
/// `handle_member_list` adds the requester's uid to the role query a second time; found
/// against the local stack, 2026-11-26): harmless to the permission maths, wrong as a
/// list of role chips.
fn distinct(role_ids: &[i64]) -> Rc<[i64]> {
    let mut out: Vec<i64> = Vec::with_capacity(role_ids.len());
    for id in role_ids {
        if !out.contains(id) {
            out.push(*id);
        }
    }
    out.into()
}

impl Member {
    #[cfg(test)]
    pub(crate) fn for_test(uid: Uid, username: &str, nickname: &str, online: bool, role_ids: &[i64]) -> Member {
        let username: SharedString = username.to_string().into();
        let nickname: SharedString = nickname.to_string().into();
        Member {
            key: RosterKey::new(&nickname, &username, uid),
            username,
            uid,
            joined_at: 0,
            nickname,
            is_owner: false,
            is_online: online,
            role_ids: role_ids.into(),
            timed_out_until: 0,
        }
    }
}

/// The whole server's member counts, as every roster packet carries them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RosterCounts {
    pub online_count: i64,
    pub total_members: i64,
}

/// One `MEMBER_LIST_RES` page (see "Roster protocol" in `proto/messages.proto`).
pub struct MemberPage {
    pub members: Vec<Member>,
    /// Rows outside the page: our own on a first page.
    pub extra: Vec<Member>,
    pub revision: u64,
    pub counts: RosterCounts,
    pub online_complete: bool,
    pub has_more: bool,
    pub next_after: SharedString,
    pub first_page: bool,
}

/// Roster paging state. `revision` is the last roster revision applied; a delta that
/// doesn't continue it means we missed something → refetch. The roster holds more
/// members than the sidebar shows: the sidebar draws only the stretch of each part the
/// pages have reached (up to `online_edge` / `offline_edge`), while deltas and lookups
/// also bring in members past it.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterMeta {
    pub revision: u64,
    /// Whole server, loaded or not (from the server, kept by every delta).
    pub total_members: i64,
    pub online_count: i64,
    /// The pages have reached the offline part: every online member is held.
    pub online_complete: bool,
    /// Last online / offline member the pages brought, in roster order (`None`: none
    /// yet). Unused once that part is complete.
    pub online_edge: Option<RosterKey>,
    pub offline_edge: Option<RosterKey>,
    pub has_more: bool,
    pub next_after: SharedString,
    pub loading_more: bool,
    /// Bumped by every first page, which replaces the roster: members looked up under an
    /// older one are asked for again.
    pub generation: u64,
    /// A first page is on its way (a revision gap, a reconnect): further gaps don't ask
    /// again and scroll paging waits for it.
    pub refetching: bool,
}

/// The sidebar's rows: each part's members up to where the pages have reached, in
/// roster order.
#[derive(Debug, Default, PartialEq)]
pub struct VisibleRoster {
    pub online: Vec<Uid>,
    pub offline: Vec<Uid>,
}

// Roster generations, unique across communities and reconnects (a lookup asked under
// one is never mistaken for an answer under the next). UI thread only.
thread_local! {
    static ROSTER_GENERATION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn next_generation() -> u64 {
    ROSTER_GENERATION.with(|g| {
        g.set(g.get() + 1);
        g.get()
    })
}

/// Member lookups (`features/members/memberLookup.ts`): the roster pages in as the
/// sidebar scrolls, so a big community's roster lacks most members at first. Views that
/// show one of them anyway (a message author, a voice participant, a profile) ask for
/// that member by uid ([`Roster::want`]). Requests are batched per community; each uid
/// is asked once per roster generation, and the answer stays current through the deltas
/// every session gets. Only for uids the community itself named: a lookup tells it whom
/// we're looking at, so the friend list never goes through here.
#[derive(Default)]
struct Lookups {
    generation: u64,
    asked: HashSet<Uid>,
    tries: HashMap<Uid, u32>,
    queued: Vec<Uid>,
    /// The 50 ms gather of one render's worth of rows into one request: exists only
    /// while uids are queued.
    flush: Option<Task<()>>,
    /// Retries after a refused lookup: each exists until it fires.
    retries: HashMap<u64, Task<()>>,
    next_retry: u64,
}

/// The server's cap per lookup.
pub const LOOKUP_BATCH: usize = 200;
const LOOKUP_DELAY: Duration = Duration::from_millis(50);
const LOOKUP_RETRY: Duration = Duration::from_millis(1500);
const LOOKUP_MAX_TRIES: u32 = 3;
/// How long a refused page waits before it is asked for again.
pub const PAGE_RETRY: Duration = Duration::from_millis(1000);

/// How roster requests leave: the core call in the app, a recorder in tests.
pub type MemberRequester = Rc<dyn Fn(ServerId, MemberRequest, &mut App)>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberRequest {
    /// `list_members { after: "", limit: ROSTER_FIRST_PAGE }`.
    FirstPage,
    /// `list_members { after, limit: ROSTER_PAGE }`.
    NextPage { after: SharedString },
    /// `list_members { uids }`.
    Lookup(Vec<Uid>),
}

pub struct Roster {
    server: ServerId,
    /// Every member held, loaded by a page, a delta or a lookup. Keys are uids, including
    /// a community's negative placeholders (unique within this roster).
    members: HashMap<Uid, Member>,
    meta: Option<RosterMeta>,
    /// The sidebar's rows, rebuilt on first use after a change (a batch of deltas is one
    /// sort).
    visible: RefCell<Option<Rc<VisibleRoster>>>,
    lookups: Lookups,
    requester: Option<MemberRequester>,
    /// Waiting to ask a refused page again: exists until it fires.
    page_retry: Option<Task<()>>,
}

impl Roster {
    pub fn new(server: ServerId) -> Roster {
        Roster {
            server,
            members: HashMap::new(),
            meta: None,
            visible: RefCell::new(None),
            lookups: Lookups::default(),
            requester: None,
            page_retry: None,
        }
    }

    pub fn set_requester(&mut self, requester: MemberRequester) {
        self.requester = Some(requester);
    }

    pub fn meta(&self) -> Option<&RosterMeta> {
        self.meta.as_ref()
    }

    pub fn members(&self) -> &HashMap<Uid, Member> {
        &self.members
    }

    /// The member record for `uid` in the loaded roster, if any. Besides our own row, a
    /// big community's members may not be loaded yet: see [`Roster::want`].
    pub fn member(&self, uid: Uid) -> Option<&Member> {
        if uid == 0 { None } else { self.members.get(&uid) }
    }

    /// The server nickname of `uid`, when they have one.
    pub fn nickname(&self, uid: Uid) -> Option<&SharedString> {
        self.member(uid).map(|m| &m.nickname).filter(|n| !n.is_empty())
    }

    fn touch(&mut self, cx: &mut Context<Self>) {
        *self.visible.borrow_mut() = None;
        cx.notify();
    }

    /// The sidebar's rows (`visibleRoster`): members past where the pages have reached
    /// (an upsert for someone the pages haven't got to, a lookup's find) are held but
    /// not drawn, so the list never shows a gap; the offline part starts once the online
    /// part is complete.
    pub fn visible(&self) -> Rc<VisibleRoster> {
        if let Some(visible) = &*self.visible.borrow() {
            return visible.clone();
        }
        let mut online: Vec<&Member> = Vec::new();
        let mut offline: Vec<&Member> = Vec::new();
        if let Some(meta) = &self.meta {
            let within = |m: &Member, edge: &Option<RosterKey>| edge.as_ref().is_some_and(|e| m.key <= *e);
            for m in self.members.values() {
                if m.is_online {
                    if meta.online_complete || within(m, &meta.online_edge) {
                        online.push(m);
                    }
                } else if meta.online_complete && (!meta.has_more || within(m, &meta.offline_edge)) {
                    offline.push(m);
                }
            }
        }
        online.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        offline.sort_unstable_by(|a, b| a.key.cmp(&b.key));
        let visible = Rc::new(VisibleRoster {
            online: online.iter().map(|m| m.uid).collect(),
            offline: offline.iter().map(|m| m.uid).collect(),
        });
        *self.visible.borrow_mut() = Some(visible.clone());
        visible
    }

    /// Apply one roster page. The first page replaces the roster (a new generation);
    /// later pages add to it and move the loaded edges. False when a later page doesn't
    /// match the revision we hold (a delta went missing): the caller refetches page 1.
    pub fn apply_page(&mut self, page: MemberPage, cx: &mut Context<Self>) -> bool {
        if !page.first_page {
            match &self.meta {
                // A later page of a roster being replaced: the first page on its way
                // starts over.
                None => return true,
                Some(prev) if prev.refetching => return true,
                Some(prev) if page.revision != prev.revision => return false,
                Some(_) => {}
            }
        }
        let prev = if page.first_page { None } else { self.meta.take() };
        if page.first_page {
            self.members.clear();
        }
        // The page runs in roster order, online members first: its last member of each
        // part is how far that part is loaded now.
        let mut online_edge = prev.as_ref().and_then(|p| p.online_edge.clone());
        let mut offline_edge = prev.as_ref().and_then(|p| p.offline_edge.clone());
        for m in &page.members {
            if m.is_online {
                online_edge = Some(m.key.clone());
            } else {
                offline_edge = Some(m.key.clone());
            }
        }
        for m in page.members.into_iter().chain(page.extra) {
            self.members.insert(m.uid, m);
        }
        self.meta = Some(RosterMeta {
            revision: page.revision,
            total_members: page.counts.total_members,
            online_count: page.counts.online_count,
            online_complete: page.online_complete,
            online_edge,
            offline_edge,
            has_more: page.has_more,
            next_after: page.next_after,
            loading_more: false,
            generation: match prev {
                Some(prev) => prev.generation,
                None => next_generation(),
            },
            refetching: false,
        });
        self.page_retry = None;
        self.touch(cx);
        true
    }

    /// A lookup's finds, merged in when they're as current as the roster. Not as current
    /// (a first page is on its way, or a delta went missing and one will be): that
    /// page's generation asks again.
    pub fn apply_lookup(&mut self, found: Vec<Member>, revision: u64, cx: &mut Context<Self>) {
        let Some(meta) = &self.meta else { return };
        if meta.refetching || revision != meta.revision || found.is_empty() {
            return;
        }
        for m in found {
            self.members.insert(m.uid, m);
        }
        self.touch(cx);
    }

    /// A live delta: a member joined or changed. False when the revision doesn't
    /// continue the last applied one (a gap) so the caller can refetch page 1.
    pub fn upsert(&mut self, member: Member, revision: u64, counts: RosterCounts, cx: &mut Context<Self>) -> bool {
        // No page loaded yet, or a first page on its way: nothing to patch, that page
        // carries it.
        let Some(meta) = &mut self.meta else { return true };
        if meta.refetching {
            return true;
        }
        if revision != meta.revision + 1 {
            return false;
        }
        meta.revision = revision;
        meta.online_count = counts.online_count;
        meta.total_members = counts.total_members;
        // Held whether or not the sidebar has reached its place (`visible` decides
        // that): every view can use the record, and the deltas keep it current.
        self.members.insert(member.uid, member);
        self.touch(cx);
        true
    }

    /// A live delta: a member left. False on a revision gap.
    pub fn remove(&mut self, uid: Uid, revision: u64, counts: RosterCounts, cx: &mut Context<Self>) -> bool {
        let Some(meta) = &mut self.meta else { return true };
        if meta.refetching {
            return true;
        }
        if revision != meta.revision + 1 {
            return false;
        }
        meta.revision = revision;
        meta.online_count = counts.online_count;
        meta.total_members = counts.total_members;
        self.members.remove(&uid);
        self.touch(cx);
        true
    }

    pub fn set_loading_more(&mut self, loading: bool, cx: &mut Context<Self>) {
        if let Some(meta) = &mut self.meta
            && meta.loading_more != loading
        {
            meta.loading_more = loading;
            cx.notify();
        }
    }

    /// A first page is (or no longer is) on its way.
    pub fn set_refetching(&mut self, refetching: bool, cx: &mut Context<Self>) {
        if let Some(meta) = &mut self.meta
            && meta.refetching != refetching
        {
            meta.refetching = refetching;
            cx.notify();
        }
    }

    /// Drop role ids the list no longer names from every member held: a deleted role's
    /// holders get no `MEMBER_UPSERT` (review A3). Members that held none keep their
    /// record untouched. No-op for an empty list.
    pub fn prune_roles(&mut self, known: &[Role], cx: &mut Context<Self>) {
        if known.is_empty() {
            return;
        }
        let is_known = |id: &i64| known.iter().any(|r| r.id == *id);
        let mut changed = false;
        for m in self.members.values_mut() {
            if !m.role_ids.iter().all(is_known) {
                m.role_ids = m.role_ids.iter().copied().filter(is_known).collect();
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    // ── Requests ──

    fn send(&self, request: MemberRequest, cx: &mut App) {
        if let Some(requester) = &self.requester {
            requester(self.server, request, cx);
        }
    }

    /// Ask for page 1 again (`refetchRoster`): a (re)authentication (`force`), or a
    /// revision gap. Deltas wait for it; while one is on its way a gap doesn't ask again.
    pub fn refetch(&mut self, force: bool, cx: &mut Context<Self>) {
        if !force && self.meta.as_ref().is_some_and(|m| m.refetching) {
            return;
        }
        self.set_refetching(true, cx);
        self.send(MemberRequest::FirstPage, cx);
    }

    /// The sidebar came near its loaded end: ask for the next page, once.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(meta) = &mut self.meta else { return };
        if !meta.has_more || meta.loading_more || meta.refetching {
            return;
        }
        meta.loading_more = true;
        let after = meta.next_after.clone();
        cx.notify();
        self.send(MemberRequest::NextPage { after }, cx);
    }

    /// The server refused a page (rate limited): a first page is asked again shortly; a
    /// later one stays "loading" for a moment, then the sidebar asks again or waits for
    /// a scroll.
    pub fn page_refused(&mut self, first_page: bool, cx: &mut Context<Self>) {
        self.page_retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PAGE_RETRY).await;
            let _ = this.update(cx, |this, cx| {
                this.page_retry = None;
                if first_page {
                    this.refetch(true, cx);
                } else {
                    this.set_loading_more(false, cx);
                }
            });
        }));
    }

    /// Drop `uid`'s record as if the pages had not reached them, without touching the revision:
    /// the self-tests' stand-in for a member past the first page (`ROSTER_FIRST_PAGE`) on a stack
    /// whose communities all fit in it. Self-tests only (`fixtures`).
    #[cfg(feature = "fixtures")]
    pub fn forget(&mut self, uid: Uid, cx: &mut Context<Self>) {
        self.members.remove(&uid);
        self.lookups.asked.remove(&uid);
        self.lookups.tries.remove(&uid);
        self.touch(cx);
    }

    /// Look `uid` up when the roster lacks them (`useMemberInterest`): a message author,
    /// a voice participant, a profile. Call it when such a row is built, not from
    /// `render`. No-op when the member is held, when no page has arrived yet, while a
    /// first page is on its way (its generation asks again), or when already asked under
    /// this generation.
    pub fn want(&mut self, uid: Uid, cx: &mut Context<Self>) {
        if uid == 0 || self.members.contains_key(&uid) {
            return;
        }
        let Some(meta) = &self.meta else { return };
        if meta.refetching {
            return;
        }
        self.request_member(uid, meta.generation, cx);
    }

    fn request_member(&mut self, uid: Uid, generation: u64, cx: &mut Context<Self>) {
        let lookups = &mut self.lookups;
        if lookups.generation != generation {
            lookups.generation = generation;
            lookups.asked.clear();
            lookups.tries.clear();
        }
        if !lookups.asked.insert(uid) {
            return;
        }
        lookups.queued.push(uid);
        if lookups.flush.is_none() {
            lookups.flush = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LOOKUP_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    this.lookups.flush = None;
                    let queued = std::mem::take(&mut this.lookups.queued);
                    // Not sent (the connection is gone): the reconnect's first page
                    // starts a new generation, which asks again.
                    for batch in queued.chunks(LOOKUP_BATCH) {
                        this.send(MemberRequest::Lookup(batch.to_vec()), cx);
                    }
                });
            }));
        }
    }

    /// The server refused a lookup (rate limited): ask again shortly, a few times at most.
    pub fn lookup_refused(&mut self, uids: &[Uid], cx: &mut Context<Self>) {
        let lookups = &mut self.lookups;
        let mut again = Vec::new();
        for uid in uids {
            if !lookups.asked.remove(uid) {
                continue;
            }
            let tries = lookups.tries.entry(*uid).or_insert(0);
            *tries += 1;
            if *tries < LOOKUP_MAX_TRIES {
                again.push(*uid);
            }
        }
        if again.is_empty() {
            return;
        }
        let generation = lookups.generation;
        lookups.next_retry += 1;
        let id = lookups.next_retry;
        let retry = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOOKUP_RETRY).await;
            let _ = this.update(cx, |this, cx| {
                this.lookups.retries.remove(&id);
                for uid in again {
                    this.request_member(uid, generation, cx);
                }
            });
        });
        self.lookups.retries.insert(id, retry);
    }
}

// ── Moderation ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Ban {
    /// Names kept as history: drawn as they are, never through the users store.
    pub username: SharedString,
    pub uid: Uid,
    pub banned_by: SharedString,
    pub banned_by_uid: Uid,
    pub reason: SharedString,
    pub banned_at: i64,
    /// 0 = permanent.
    pub expires_at: i64,
}

impl From<&BanInfoPayload> for Ban {
    fn from(b: &BanInfoPayload) -> Ban {
        Ban {
            username: b.username.clone().into(),
            uid: b.uid,
            banned_by: b.banned_by.clone().into(),
            banned_by_uid: b.banned_by_uid,
            reason: b.reason.clone().into(),
            banned_at: b.banned_at,
            expires_at: b.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Invite {
    pub code: SharedString,
    pub created_by: SharedString,
    pub created_by_uid: Uid,
    pub created_at: i64,
    /// 0 = never.
    pub expires_at: i64,
    /// 0 = unlimited.
    pub max_uses: i32,
    pub uses: i32,
    /// Personal invite (Invite friends): only this user can redeem it. Empty = anyone.
    pub for_user: SharedString,
    pub for_uid: Uid,
}

impl From<&InviteInfoPayload> for Invite {
    fn from(i: &InviteInfoPayload) -> Invite {
        Invite {
            code: i.code.clone().into(),
            created_by: i.created_by.clone().into(),
            created_by_uid: i.created_by_uid,
            created_at: i.created_at,
            expires_at: i.expires_at,
            max_uses: i.max_uses,
            uses: i.uses,
            for_user: i.for_user.clone().into(),
            for_uid: i.for_uid,
        }
    }
}

/// `bansByServer`, `invitesByServer` and their "only the newest fit the reply" flags.
#[derive(Default)]
pub struct Moderation {
    /// Newest first.
    pub bans: Vec<Ban>,
    pub bans_truncated: bool,
    /// Newest first.
    pub invites: Vec<Invite>,
    pub invites_truncated: bool,
}

impl Moderation {
    pub fn set_bans(&mut self, bans: Vec<Ban>, truncated: bool, cx: &mut Context<Self>) {
        self.bans = bans;
        self.bans_truncated = truncated;
        cx.notify();
    }

    pub fn set_invites(&mut self, invites: Vec<Invite>, truncated: bool, cx: &mut Context<Self>) {
        self.invites = invites;
        self.invites_truncated = truncated;
        cx.notify();
    }

    /// A created invite goes to the front (replacing one with the same code).
    pub fn upsert_invite(&mut self, invite: Invite, cx: &mut Context<Self>) {
        self.invites.retain(|i| i.code != invite.code);
        self.invites.insert(0, invite);
        cx.notify();
    }

    pub fn remove_invite(&mut self, code: &str, cx: &mut Context<Self>) {
        self.invites.retain(|i| &*i.code != code);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;
    use crate::stores::permissions::perm;

    fn m(uid: Uid, name: &str, online: bool) -> Member {
        Member::for_test(uid, name, "", online, &[])
    }

    fn counts(online: i64, total: i64) -> RosterCounts {
        RosterCounts { online_count: online, total_members: total }
    }

    fn page(members: Vec<Member>, revision: u64, first: bool, online_complete: bool, has_more: bool) -> MemberPage {
        MemberPage {
            members,
            extra: vec![],
            revision,
            counts: counts(2, 6),
            online_complete,
            has_more,
            next_after: "cursor".into(),
            first_page: first,
        }
    }

    fn names(roster: &Roster, uids: &[Uid]) -> Vec<String> {
        uids.iter().map(|u| roster.member(*u).unwrap().username.to_string()).collect()
    }

    fn recorder(roster: &Entity<Roster>, cx: &mut TestAppContext) -> Rc<RefCell<Vec<MemberRequest>>> {
        let sent: Rc<RefCell<Vec<MemberRequest>>> = Rc::default();
        let log = sent.clone();
        roster.update(cx, |r, _| r.set_requester(Rc::new(move |_, request, _| log.borrow_mut().push(request))));
        sent
    }

    #[gpui::test]
    fn pages_extend_the_sidebar_and_members_past_the_edge_are_held_not_drawn(cx: &mut TestAppContext) {
        let roster = cx.new(|_| Roster::new(ServerId::from_central(1)));
        roster.update(cx, |r, cx| {
            // Deltas before any page: nothing to patch.
            assert!(r.upsert(m(9, "zoe", true), 5, counts(1, 1), cx));
            assert!(r.member(9).is_none());

            // Page 1: two online members; the online part is not complete yet. Our own
            // row rides as `extra`.
            let mut first = page(vec![m(1, "alice", true), m(2, "Bob", true)], 10, true, false, true);
            first.extra = vec![m(7, "perf2", true)];
            assert!(r.apply_page(first, cx));
            let meta = r.meta().unwrap().clone();
            assert_eq!((meta.revision, meta.total_members, meta.online_count), (10, 6, 2));
            assert_eq!(meta.online_edge.as_ref().map(|k| k.uid), Some(2));
            assert_eq!(meta.offline_edge, None);
            let v = r.visible();
            // perf2 is held (every view can use the record) but sorts past the edge.
            assert_eq!(names(r, &v.online), ["alice", "Bob"]);
            assert!(v.offline.is_empty(), "the offline part starts once the online part is complete");
            assert_eq!(r.member(7).unwrap().username, "perf2");

            // An upsert for someone the pages haven't reached: held, not drawn. One
            // before the edge: drawn in order.
            assert!(r.upsert(m(8, "zed", true), 11, counts(3, 6), cx));
            assert!(r.upsert(m(3, "Anna", true), 12, counts(4, 6), cx));
            assert_eq!(names(r, &r.visible().online), ["alice", "Anna", "Bob"]);
            assert_eq!(r.meta().unwrap().online_count, 4);

            // Page 2 completes the online part and starts the offline one.
            assert!(r.apply_page(
                page(vec![m(7, "perf2", true), m(8, "zed", true), m(4, "carol", false)], 12, false, true, true),
                cx
            ));
            let v = r.visible();
            assert_eq!(names(r, &v.online), ["alice", "Anna", "Bob", "perf2", "zed"]);
            assert_eq!(names(r, &v.offline), ["carol"]);
            assert_eq!(r.meta().unwrap().generation, meta.generation, "a later page keeps the generation");

            // An offline member past the offline edge waits for its page; with no more
            // pages everyone held is drawn.
            assert!(r.upsert(m(5, "dave", false), 13, counts(4, 6), cx));
            assert_eq!(names(r, &r.visible().offline), ["carol"]);
            assert!(r.apply_page(page(vec![m(5, "dave", false), m(6, "erin", false)], 13, false, true, false), cx));
            assert_eq!(names(r, &r.visible().offline), ["carol", "dave", "erin"]);

            // Going offline moves a member between the parts.
            assert!(r.upsert(m(2, "Bob", false), 14, counts(4, 6), cx));
            let v = r.visible();
            assert_eq!(names(r, &v.online), ["alice", "Anna", "perf2", "zed"]);
            assert_eq!(names(r, &v.offline), ["Bob", "carol", "dave", "erin"]);
            assert!(r.remove(8, 15, counts(3, 5), cx));
            assert_eq!(names(r, &r.visible().online), ["alice", "Anna", "perf2"]);
            assert_eq!(r.meta().unwrap().total_members, 5);
        });
    }

    #[gpui::test]
    fn a_revision_gap_asks_for_page_one_once_and_deltas_wait_for_it(cx: &mut TestAppContext) {
        let roster = cx.new(|_| Roster::new(ServerId::from_central(1)));
        let sent = recorder(&roster, cx);
        roster.update(cx, |r, cx| {
            assert!(r.apply_page(page(vec![m(1, "alice", true)], 10, true, true, false), cx));
            let generation = r.meta().unwrap().generation;
            // Revision 12 doesn't continue 10.
            assert!(!r.upsert(m(2, "bob", true), 12, counts(2, 2), cx));
            assert!(r.member(2).is_none());
            r.refetch(false, cx);
            assert!(r.meta().unwrap().refetching);
            // Another gap while it is on its way: no second request, and deltas are
            // swallowed (the page carries them).
            r.refetch(false, cx);
            assert!(r.upsert(m(3, "carol", true), 99, counts(3, 3), cx));
            assert!(r.remove(1, 100, counts(2, 2), cx));
            assert!(r.member(1).is_some());
            // A later page of the roster being replaced is ignored.
            assert!(r.apply_page(page(vec![m(4, "dave", true)], 10, false, true, false), cx));
            assert!(r.member(4).is_none());
            // A later page with another revision (no refetch pending) reports the gap.
            r.set_refetching(false, cx);
            assert!(!r.apply_page(page(vec![m(4, "dave", true)], 11, false, true, false), cx));
            r.set_refetching(true, cx);
            // The first page replaces everything under a new generation.
            assert!(r.apply_page(page(vec![m(2, "bob", true), m(3, "carol", true)], 20, true, true, false), cx));
            let meta = r.meta().unwrap();
            assert!(!meta.refetching);
            assert!(meta.generation > generation);
            assert!(r.member(1).is_none(), "the first page replaced the roster");
            assert_eq!(names(r, &r.visible().online), ["bob", "carol"]);
        });
        assert_eq!(*sent.borrow(), [MemberRequest::FirstPage]);
    }

    #[gpui::test]
    fn paging_asks_once_and_a_refused_page_is_retried(cx: &mut TestAppContext) {
        let roster = cx.new(|_| Roster::new(ServerId::from_central(1)));
        let sent = recorder(&roster, cx);
        roster.update(cx, |r, cx| {
            r.load_more(cx); // nothing loaded: nothing to extend
            assert!(r.apply_page(page(vec![m(1, "alice", true)], 10, true, false, true), cx));
            r.load_more(cx);
            r.load_more(cx);
            assert!(r.meta().unwrap().loading_more);
            // Refused: "loading" ends after a second, without asking by itself.
            r.page_refused(false, cx);
        });
        assert_eq!(*sent.borrow(), [MemberRequest::NextPage { after: "cursor".into() }]);
        cx.executor().advance_clock(PAGE_RETRY);
        cx.run_until_parked();
        roster.update(cx, |r, cx| {
            assert!(!r.meta().unwrap().loading_more);
            // A refused first page is asked for again.
            r.page_refused(true, cx);
        });
        cx.executor().advance_clock(PAGE_RETRY);
        cx.run_until_parked();
        assert_eq!(sent.borrow().last(), Some(&MemberRequest::FirstPage));
        roster.read_with(cx, |r, _| assert!(r.meta().unwrap().refetching));
    }

    #[gpui::test]
    fn lookups_batch_ask_once_per_generation_and_retry_a_few_times(cx: &mut TestAppContext) {
        let roster = cx.new(|_| Roster::new(ServerId::from_central(1)));
        let sent = recorder(&roster, cx);
        roster.update(cx, |r, cx| {
            r.want(50, cx); // no page yet: not asked
            assert!(r.apply_page(page(vec![m(1, "alice", true)], 10, true, false, true), cx));
            r.want(1, cx); // held
            r.want(0, cx);
            for uid in 100..350 {
                r.want(uid, cx);
            }
            r.want(100, cx); // already asked
        });
        assert!(sent.borrow().is_empty(), "gathered for 50 ms first");
        cx.executor().advance_clock(LOOKUP_DELAY);
        cx.run_until_parked();
        {
            let sent = sent.borrow();
            assert_eq!(sent.len(), 2, "250 uids in batches of 200");
            assert!(matches!(&sent[0], MemberRequest::Lookup(uids) if uids.len() == 200 && uids[0] == 100));
            assert!(matches!(&sent[1], MemberRequest::Lookup(uids) if uids.len() == 50));
        }
        sent.borrow_mut().clear();

        // The answer: merged when it is as current as the roster.
        roster.update(cx, |r, cx| {
            r.apply_lookup(vec![m(100, "hundred", false)], 9, cx);
            assert!(r.member(100).is_none(), "an older revision is dropped");
            r.apply_lookup(vec![m(100, "hundred", false)], 10, cx);
            assert_eq!(r.member(100).unwrap().username, "hundred");
            // Held, but the offline part hasn't started: not drawn.
            assert!(r.visible().offline.is_empty());
        });

        // Refused: asked again after 1.5 s, three tries in all.
        for round in 1..=3 {
            roster.update(cx, |r, cx| r.lookup_refused(&[101, 102, 9999], cx));
            cx.executor().advance_clock(LOOKUP_RETRY + LOOKUP_DELAY);
            cx.run_until_parked();
            if round < 3 {
                assert_eq!(sent.borrow().len(), round, "retry {round}");
                assert_eq!(sent.borrow()[round - 1], MemberRequest::Lookup(vec![101, 102]));
            } else {
                assert_eq!(sent.borrow().len(), 2, "given up after three tries");
            }
        }

        // A new generation (a first page) asks again for whoever is still missing.
        sent.borrow_mut().clear();
        roster.update(cx, |r, cx| {
            assert!(r.apply_page(page(vec![m(1, "alice", true)], 30, true, false, true), cx));
            r.want(101, cx);
        });
        cx.executor().advance_clock(LOOKUP_DELAY);
        cx.run_until_parked();
        assert_eq!(*sent.borrow(), [MemberRequest::Lookup(vec![101])]);
    }

    #[gpui::test]
    fn a_deleted_role_leaves_its_holders(cx: &mut TestAppContext) {
        let roster = cx.new(|_| Roster::new(ServerId::from_central(1)));
        let role = |id| Role { id, name: "r".into(), color: 0, position: id as i32, permissions: 0, is_default: id == 0, manage_each_other: false, member_count: 0 };
        roster.update(cx, |r, cx| {
            let members = vec![Member::for_test(1, "alice", "", true, &[10, 20]), Member::for_test(2, "bob", "", true, &[10])];
            assert!(r.apply_page(page(members, 1, true, true, false), cx));
            let untouched = r.member(2).unwrap().role_ids.clone();
            r.prune_roles(&[role(0), role(10)], cx);
            assert_eq!(&*r.member(1).unwrap().role_ids, &[10]);
            assert!(Rc::ptr_eq(&r.member(2).unwrap().role_ids, &untouched));
            // An empty list (a legacy server) prunes nothing.
            r.prune_roles(&[], cx);
            assert_eq!(&*r.member(1).unwrap().role_ids, &[10]);
        });
    }

    #[gpui::test]
    fn channels_keep_permissions_an_update_omits(cx: &mut TestAppContext) {
        let channels = cx.new(|_| Channels::default());
        let ch = |id: &str, kind, perms| Channel {
            id: id.to_string().into(),
            name: id.to_string().into(),
            kind,
            voice_bitrate_kbps: 64,
            retention_days_text: 0,
            retention_days_image: 0,
            retention_days_video: 0,
            retention_days_document: 0,
            retention_days_audio: 0,
            my_permissions: perms,
            slowmode_seconds: 0,
            encrypted: false,
            permissions_synced: false,
            category_sync: false,
            topic: "".into(),
        };
        let view_send = perm::VIEW_CHANNEL | perm::SEND_MESSAGES;
        channels.update(cx, |c, cx| {
            c.set_all(
                vec![
                    ch("lobby", ChannelKind::Voice, view_send),
                    ch("info", ChannelKind::Category, 0),
                    ch("announcements", ChannelKind::Text, perm::VIEW_CHANNEL),
                    ch("text", ChannelKind::Category, perm::VIEW_CHANNEL),
                    ch("general", ChannelKind::Text, view_send),
                ],
                cx,
            );
            assert_eq!(&*c.first_text().unwrap().id, "announcements");
            assert_eq!(c.category_of("lobby"), None, "before the first category: uncategorized");
            assert_eq!(&*c.category_of("general").unwrap().id, "text");
            assert_eq!(c.category_of("text"), None);

            // Read-only for us; the owner always passes; a channel we don't hold is "no".
            assert!(c.permits("general", perm::SEND_MESSAGES, false));
            assert!(!c.permits("announcements", perm::SEND_MESSAGES, false));
            assert!(c.permits("announcements", perm::SEND_MESSAGES, true));
            assert!(!c.permits("mod-chat", perm::VIEW_CHANNEL, false));
            assert!(!c.permits("info", perm::VIEW_CHANNEL, false), "0 is a real answer");

            // An update without permissions keeps ours; one with permissions replaces them.
            let mut renamed = ch("general", ChannelKind::Text, 0);
            renamed.topic = "talk".into();
            c.upsert(renamed, cx);
            assert_eq!(c.get("general").unwrap().my_permissions, view_send);
            assert_eq!(&*c.get("general").unwrap().topic, "talk");
            c.upsert(ch("general", ChannelKind::Text, perm::VIEW_CHANNEL), cx);
            assert_eq!(c.get("general").unwrap().my_permissions, perm::VIEW_CHANNEL);
            c.upsert(ch("new", ChannelKind::Text, view_send), cx);
            assert_eq!(c.list().len(), 6);
        });
    }
}

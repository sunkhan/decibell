//! Permission bits and the hierarchy rules (`features/servers/permissions.ts`, which
//! mirrors `src/community/authz.hpp`). The server is authoritative: this only decides
//! what the UI offers.

use std::collections::HashMap;

use crate::ids::Uid;
use crate::stores::community::{Member, Role};

/// Permission bits: `chatproj.Permission` in `proto/messages.proto`. Wire contract:
/// values never change, new bits are appended.
pub mod perm {
    pub const ADMINISTRATOR: u64 = 1;
    pub const MANAGE_SERVER: u64 = 2;
    pub const MANAGE_CHANNELS: u64 = 4;
    pub const MANAGE_ROLES: u64 = 8;
    pub const KICK_MEMBERS: u64 = 16;
    pub const BAN_MEMBERS: u64 = 32;
    pub const MANAGE_MESSAGES: u64 = 64;
    pub const MANAGE_INVITES: u64 = 128;
    pub const MANAGE_NICKNAMES: u64 = 256;
    pub const SEND_MESSAGES: u64 = 1024;
    pub const CONNECT_VOICE: u64 = 2048;
    pub const STREAM: u64 = 4096;
    // permissions v2 (per-channel overwrites)
    pub const VIEW_CHANNEL: u64 = 8192;
    pub const READ_HISTORY: u64 = 16384;
    pub const ATTACH_FILES: u64 = 32768;
    // server management + moderation
    pub const VIEW_AUDIT_LOG: u64 = 65536;
    pub const MODERATE_MEMBERS: u64 = 131072;
    // permissions v3
    /// Was VOICE_MODERATE (same bit).
    pub const MUTE_MEMBERS: u64 = 262144;
    pub const DEAFEN_MEMBERS: u64 = 524288;
    pub const MOVE_MEMBERS: u64 = 1048576;
    pub const CREATE_INVITE: u64 = 2097152;
    pub const CHANGE_NICKNAME: u64 = 4194304;
    pub const EMBED_LINKS: u64 = 8388608;
    pub const SPEAK: u64 = 16777216;

    /// "Every permission": what owners and ADMINISTRATOR holders resolve to. The
    /// renderer's `Number.MAX_SAFE_INTEGER` (all 53 bits a JS number keeps exact).
    pub const ALL: u64 = (1 << 53) - 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionGroup {
    General,
    Members,
    Text,
    Voice,
}

pub const PERMISSION_GROUPS: [PermissionGroup; 4] = [
    PermissionGroup::General,
    PermissionGroup::Members,
    PermissionGroup::Text,
    PermissionGroup::Voice,
];

impl PermissionGroup {
    pub fn label(self) -> &'static str {
        match self {
            PermissionGroup::General => "General",
            PermissionGroup::Members => "Members",
            PermissionGroup::Text => "Text",
            PermissionGroup::Voice => "Voice",
        }
    }
}

pub struct EditablePermission {
    pub bit: u64,
    pub label: &'static str,
    pub description: &'static str,
    pub group: PermissionGroup,
}

const fn editable(bit: u64, label: &'static str, description: &'static str, group: PermissionGroup) -> EditablePermission {
    EditablePermission { bit, label, description, group }
}

/// The permission bits surfaced in role editors and the per-channel overwrite editor, in
/// display order (grouped). Every bit here is enforced server-side.
pub const EDITABLE_PERMISSIONS: [EditablePermission; 24] = {
    use PermissionGroup::*;
    [
        editable(perm::ADMINISTRATOR, "Administrator", "Grants every permission. Hierarchy still applies.", General),
        editable(perm::VIEW_CHANNEL, "View Channel", "See the channel and receive its messages and presence.", General),
        editable(perm::MANAGE_SERVER, "Manage Server", "Change the server name, description, picture and storage settings.", General),
        editable(perm::MANAGE_ROLES, "Manage Roles", "Create, edit and assign roles below their own; edit channel permissions.", General),
        editable(perm::MANAGE_CHANNELS, "Manage Channels", "Create, edit, reorder and delete channels; retention, slowmode and wipes.", General),
        editable(perm::VIEW_AUDIT_LOG, "View Audit Log", "See who did what in server settings.", General),
        editable(perm::CREATE_INVITE, "Create Invite", "Invite people, and see or revoke their own invites.", Members),
        editable(perm::MANAGE_INVITES, "Manage Invites", "See and revoke everyone's invites.", Members),
        editable(perm::CHANGE_NICKNAME, "Change Nickname", "Set their own nickname in this server.", Members),
        editable(perm::MANAGE_NICKNAMES, "Manage Nicknames", "Change lower-ranked members' nicknames.", Members),
        editable(perm::KICK_MEMBERS, "Kick Members", "Remove lower-ranked members from the server.", Members),
        editable(perm::BAN_MEMBERS, "Ban Members", "Ban/unban lower-ranked members and see the ban list.", Members),
        editable(perm::MODERATE_MEMBERS, "Time Out Members", "Time out lower-ranked members.", Members),
        editable(perm::READ_HISTORY, "Read Message History", "Load messages sent before joining the channel view.", Text),
        editable(perm::SEND_MESSAGES, "Send Messages", "Post in text channels.", Text),
        editable(perm::ATTACH_FILES, "Attach Files", "Upload files and images with messages.", Text),
        editable(perm::EMBED_LINKS, "Embed Links", "Links and GIFs in their messages show previews.", Text),
        editable(perm::MANAGE_MESSAGES, "Manage Messages", "Delete other members' messages and ignore slowmode.", Text),
        editable(perm::CONNECT_VOICE, "Connect", "Join voice channels.", Voice),
        editable(perm::SPEAK, "Speak", "Talk in voice channels. Without it, others don't hear them.", Voice),
        editable(perm::STREAM, "Stream", "Share the screen in voice channels.", Voice),
        editable(perm::MUTE_MEMBERS, "Mute Members", "Server-mute lower-ranked members in voice.", Voice),
        editable(perm::DEAFEN_MEMBERS, "Deafen Members", "Server-deafen lower-ranked members in voice.", Voice),
        editable(perm::MOVE_MEMBERS, "Move Members", "Move lower-ranked members to any voice channel they can see, or disconnect them.", Voice),
    ]
};

/// The bits that make sense as a per-channel overwrite (everything that is
/// channel-scoped on the server).
pub const CHANNEL_OVERWRITE_BITS: u64 = perm::VIEW_CHANNEL
    | perm::READ_HISTORY
    | perm::SEND_MESSAGES
    | perm::ATTACH_FILES
    | perm::EMBED_LINKS
    | perm::CONNECT_VOICE
    | perm::SPEAK
    | perm::STREAM
    | perm::MUTE_MEMBERS
    | perm::DEAFEN_MEMBERS
    | perm::MOVE_MEMBERS
    | perm::MANAGE_CHANNELS
    | perm::MANAGE_MESSAGES
    | perm::MANAGE_ROLES;

/// [`EDITABLE_PERMISSIONS`] narrowed to [`CHANNEL_OVERWRITE_BITS`], in display order.
pub fn channel_overwrite_permissions() -> impl Iterator<Item = &'static EditablePermission> {
    EDITABLE_PERMISSIONS.iter().filter(|p| CHANNEL_OVERWRITE_BITS & p.bit != 0)
}

/// True when every bit of `bits` is set in `mask`.
pub fn has_bits(mask: u64, bits: u64) -> bool {
    mask & bits == bits
}

/// Set or clear one permission bit.
pub fn toggle_bit(mask: u64, bit: u64, on: bool) -> u64 {
    if on { mask | bit } else { mask & !bit }
}

/// OR of `everyone` + the member's assigned roles. ADMINISTRATOR expands to everything.
/// No role list (a legacy server that never sent one) is no permissions: the callers
/// then gate on ownership alone.
pub fn compute_effective_permissions(roles: &[Role], role_ids: &[i64]) -> u64 {
    if roles.is_empty() {
        return 0;
    }
    let of = |id: i64| roles.iter().find(|r| r.id == id).map_or(0, |r| r.permissions);
    let mut perms = roles.iter().find(|r| r.is_default).map_or(0, |r| r.permissions);
    for id in role_ids {
        perms |= of(*id);
    }
    if perms & perm::ADMINISTRATOR != 0 { perm::ALL } else { perms }
}

/// Hierarchy level = position of the member's highest role (0 with no roles). The owner
/// outranks everything: callers check ownership separately ([`Hierarchy`]).
pub fn member_level(roles: &[Role], role_ids: &[i64]) -> i32 {
    role_ids
        .iter()
        .map(|id| roles.iter().find(|r| r.id == *id).map_or(0, |r| r.position))
        .fold(0, i32::max)
}

/// The role at the member's level: their highest assigned role, or `everyone` with none.
/// Carries the "members can manage each other" flag.
pub fn highest_role<'a>(roles: &'a [Role], role_ids: &[i64]) -> Option<&'a Role> {
    let level = member_level(roles, role_ids);
    if level == 0 {
        return roles.iter().find(|r| r.is_default);
    }
    roles.iter().find(|r| r.position == level && role_ids.contains(&r.id))
}

/// Hierarchy context for moderation UI (`useHierarchy`): the local user's level, their
/// ownership, and whom they may act on. Buttons that act on another member show only
/// when `is_owner || level_of(target) < level`, mirroring the server's gate for kick /
/// ban / timeout / roles ([`Hierarchy::can_moderate`]). Nicknames and voice moderation
/// use [`Hierarchy::can_manage`], which also admits peers (same highest role with
/// "members can manage each other" on: permissions v3).
pub struct Hierarchy<'a> {
    local_uid: Uid,
    owner: Uid,
    roles: &'a [Role],
    members: Option<&'a HashMap<Uid, Member>>,
    pub is_owner: bool,
    pub level: i32,
    peer_flag: bool,
}

impl<'a> Hierarchy<'a> {
    /// `owner`: the community's owner uid (0 = unknown). `members`: its loaded roster.
    pub fn new(local_uid: Uid, owner: Uid, roles: &'a [Role], members: Option<&'a HashMap<Uid, Member>>) -> Hierarchy<'a> {
        let is_owner = local_uid != 0 && owner != 0 && owner == local_uid;
        let my_roles: &[i64] = members
            .and_then(|m| if local_uid != 0 { m.get(&local_uid) } else { None })
            .map_or(&[], |m| &m.role_ids);
        Hierarchy {
            local_uid,
            owner,
            roles,
            members,
            is_owner,
            level: member_level(roles, my_roles),
            peer_flag: highest_role(roles, my_roles).is_some_and(|r| r.manage_each_other),
        }
    }

    /// The level of a member in the loaded roster (0 when not loaded).
    pub fn level_of(&self, uid: Uid) -> i32 {
        let role_ids: &[i64] = self.members.and_then(|m| m.get(&uid)).map_or(&[], |m| &m.role_ids);
        member_level(self.roles, role_ids)
    }

    fn manages(&self, uid: Uid, theirs: i32) -> bool {
        // `uid === owner`: with no owner known (0) this is true only for uid 0.
        if uid == self.owner {
            return false;
        }
        if self.is_owner {
            return true;
        }
        if theirs < self.level {
            return true;
        }
        self.peer_flag && theirs == self.level && uid != self.local_uid
    }

    /// Nicknames and voice moderation: strictly lower-ranked members, and peers when the
    /// shared highest role allows it. Never the owner.
    pub fn can_manage(&self, uid: Uid) -> bool {
        self.manages(uid, self.level_of(uid))
    }

    /// `can_manage` for a member record held outside the roster (a search result for an
    /// offline member the sidebar never loaded).
    pub fn can_manage_record(&self, member: &Member) -> bool {
        self.manages(member.uid, member_level(self.roles, &member.role_ids))
    }

    /// Kick / ban / timeout / roles: the owner, or a strictly higher level. Never peers.
    pub fn can_moderate(&self, uid: Uid) -> bool {
        self.is_owner || self.level_of(uid) < self.level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn role(id: i64, position: i32, permissions: u64) -> Role {
        Role {
            id,
            name: format!("role{id}").into(),
            color: 0,
            position,
            permissions,
            is_default: position == 0,
            manage_each_other: false,
            member_count: 0,
        }
    }

    fn member(uid: Uid, role_ids: &[i64]) -> Member {
        Member::for_test(uid, &format!("user{uid}"), "", true, role_ids)
    }

    /// Daily Gaming's shape: Admin (3) > Moderator (2) > Member (1) > everyone (0).
    fn roles() -> Vec<Role> {
        vec![
            role(30, 3, perm::ADMINISTRATOR),
            role(20, 2, perm::KICK_MEMBERS | perm::MANAGE_NICKNAMES | perm::MUTE_MEMBERS),
            role(10, 1, perm::ATTACH_FILES),
            role(1, 0, perm::VIEW_CHANNEL | perm::SEND_MESSAGES),
        ]
    }

    #[test]
    fn bits() {
        assert!(has_bits(perm::ALL, perm::SPEAK | perm::ADMINISTRATOR));
        assert!(has_bits(0b1110, 0b0110));
        assert!(!has_bits(0b1010, 0b0110));
        assert!(has_bits(5, 0), "no bits asked is always held");
        assert_eq!(toggle_bit(0, perm::STREAM, true), perm::STREAM);
        assert_eq!(toggle_bit(perm::STREAM | perm::SPEAK, perm::STREAM, false), perm::SPEAK);
        assert_eq!(toggle_bit(perm::SPEAK, perm::STREAM, false), perm::SPEAK);
        // Bits past 2^31 behave (the renderer needs BigInt for these).
        let high = 1u64 << 40;
        assert!(has_bits(toggle_bit(perm::SPEAK, high, true), high | perm::SPEAK));
    }

    #[test]
    fn effective_permissions_or_everyone_with_the_assigned_roles() {
        let roles = roles();
        let everyone = perm::VIEW_CHANNEL | perm::SEND_MESSAGES;
        assert_eq!(compute_effective_permissions(&roles, &[]), everyone);
        assert_eq!(compute_effective_permissions(&roles, &[10]), everyone | perm::ATTACH_FILES);
        assert_eq!(
            compute_effective_permissions(&roles, &[10, 20]),
            everyone | perm::ATTACH_FILES | perm::KICK_MEMBERS | perm::MANAGE_NICKNAMES | perm::MUTE_MEMBERS
        );
        // ADMINISTRATOR expands to everything.
        assert_eq!(compute_effective_permissions(&roles, &[30]), perm::ALL);
        // A role id the list no longer names contributes nothing.
        assert_eq!(compute_effective_permissions(&roles, &[999]), everyone);
        // A legacy server without roles: nothing (owner-only gating).
        assert_eq!(compute_effective_permissions(&[], &[10]), 0);
    }

    #[test]
    fn level_is_the_highest_assigned_role() {
        let roles = roles();
        assert_eq!(member_level(&roles, &[]), 0);
        assert_eq!(member_level(&roles, &[10]), 1);
        assert_eq!(member_level(&roles, &[10, 20]), 2);
        assert_eq!(member_level(&roles, &[999]), 0);
        assert_eq!(highest_role(&roles, &[]).unwrap().id, 1, "everyone with no roles");
        assert_eq!(highest_role(&roles, &[10, 20]).unwrap().id, 20);
        assert!(highest_role(&[], &[]).is_none());
    }

    #[test]
    fn hierarchy_is_strict_and_the_owner_is_untouchable() {
        let roles = roles();
        let members: HashMap<Uid, Member> = [
            member(1, &[]),        // the owner, no roles
            member(2, &[30]),      // admin
            member(3, &[20]),      // moderator
            member(4, &[20]),      // another moderator
            member(5, &[10]),      // member
            member(6, &[]),        // only everyone
        ]
        .into_iter()
        .map(|m| (m.uid, m))
        .collect();
        let h = |uid| Hierarchy::new(uid, 1, &roles, Some(&members));

        // The owner manages everyone but themselves, whatever their roles.
        let owner = h(1);
        assert!(owner.is_owner);
        assert_eq!(owner.level, 0);
        assert!(owner.can_manage(2) && owner.can_manage(6) && owner.can_moderate(2));
        assert!(!owner.can_manage(1));

        // A moderator: members and role-less people, not the admin, not the owner, not
        // a fellow moderator (no peer flag), not themselves.
        let moderator = h(3);
        assert!(!moderator.is_owner);
        assert_eq!(moderator.level, 2);
        assert!(moderator.can_manage(5) && moderator.can_manage(6));
        assert!(!moderator.can_manage(2));
        assert!(!moderator.can_manage(1), "never the owner, although the owner's level is 0");
        assert!(!moderator.can_manage(4));
        assert!(!moderator.can_manage(3));
        assert!(moderator.can_moderate(5) && !moderator.can_moderate(4));

        // ADMINISTRATOR never bypasses the hierarchy: the admin can't act on the owner.
        assert!(!h(2).can_manage(1));
        assert!(h(2).can_manage(3));

        // Two members who only hold `everyone` can't manage each other.
        assert!(!h(6).can_manage(5));
        let other = member(7, &[]);
        assert!(!h(6).can_manage_record(&other));
        // Someone not in the loaded roster counts as level 0.
        assert_eq!(moderator.level_of(424242), 0);
        assert!(moderator.can_manage(424242));
        // A record outside the roster is judged by its own roles.
        assert!(!moderator.can_manage_record(&member(8, &[30])));
        assert!(moderator.can_manage_record(&member(9, &[10])));
    }

    #[test]
    fn manage_each_other_admits_peers_for_the_light_actions_only() {
        let mut roles = roles();
        roles[1].manage_each_other = true; // Moderator
        let members: HashMap<Uid, Member> = [member(1, &[]), member(3, &[20]), member(4, &[20]), member(5, &[10, 20]), member(6, &[30])]
            .into_iter()
            .map(|m| (m.uid, m))
            .collect();
        let h = Hierarchy::new(3, 1, &roles, Some(&members));
        assert!(h.can_manage(4), "a peer: same highest role with the flag on");
        assert!(h.can_manage(5), "their highest role is Moderator too");
        assert!(!h.can_manage(3), "never themselves");
        assert!(!h.can_manage(6), "not someone higher");
        assert!(!h.can_manage(1), "not the owner");
        assert!(!h.can_moderate(4), "kick / ban / timeout / roles never work between peers");

        // The flag on `everyone` makes role-less members peers.
        let mut roles = self::roles();
        roles[3].manage_each_other = true;
        let members: HashMap<Uid, Member> =
            [member(6, &[]), member(7, &[]), member(5, &[10])].into_iter().map(|m| (m.uid, m)).collect();
        let h = Hierarchy::new(6, 1, &roles, Some(&members));
        assert!(h.can_manage(7));
        assert!(!h.can_manage(5));
        // A member of a higher role is not a peer of `everyone`'s holders, but manages them.
        assert!(Hierarchy::new(5, 1, &roles, Some(&members)).can_manage(6));
    }

    #[test]
    fn the_overwrite_editor_lists_only_channel_scoped_bits() {
        let labels: Vec<_> = channel_overwrite_permissions().map(|p| p.label).collect();
        assert_eq!(
            labels,
            [
                "View Channel", "Manage Roles", "Manage Channels", "Read Message History", "Send Messages",
                "Attach Files", "Embed Links", "Manage Messages", "Connect", "Speak", "Stream", "Mute Members",
                "Deafen Members", "Move Members"
            ]
        );
        // Every editable bit is distinct.
        let all = EDITABLE_PERMISSIONS.iter().fold(0, |acc, p| {
            assert_eq!(acc & p.bit, 0);
            acc | p.bit
        });
        assert!(has_bits(perm::ALL, all));
    }
}

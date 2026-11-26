//! The screens' fixture: the shell's (`perf1` signed in to the stack's three communities, Daily
//! Gaming `#general` open; `decibell_shell::fixture`) plus what each screen adds — central's
//! directory, an invite being resolved, Daily Gaming's invites, more friends, the ownership or the
//! roles a menu is gated on, a syncing category, empty channels — fed through the model's real
//! router, then what the user did (`Ui`): the view, a typed search, an open dialog or menu, a drag.
//!
//! [`json`] exports the same as the Electron capture pages replay it
//! (`tools/electron-captures/pages/_servers.tsx`): the shell's fixture, the extra events as the
//! addon would deliver them, and the `ui` steps.

use std::rc::Rc;

use decibell_core::events::*;
use decibell_model::stores::invites::{InviteResolve, ResolvedInvite};
use decibell_model::stores::community::ChannelKind;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::servers::PendingInvite;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_shell::fixture::{self as shell, Scene};
use decibell_shell::{ChannelMenuTarget, CreateChannelRequest, ShellRequests, SidebarMenu};
use decibell_ui::ScrollHandle;
use gpui::{App, point, px};
use serde_json::{Value, json as j};

use crate::invite_friends::{CentralMembers, RowState};
use crate::reorder::{self, Before, SyncPrompt};

/// The GPUI window's content starts 10 px in (the client-side margin): a point the Electron page
/// uses at (x, y) is (x + 10, y + 10) here.
const MARGIN: f32 = 10.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Owner,
    Moderator,
    Member,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create {
    Text,
    /// From a category header's "+": the Text / Voice choice, Voice picked.
    Voice,
    Category,
    /// A text channel named and switched to encrypted.
    Encrypted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    Resolving,
    Resolved,
    Expired,
    /// An invite to a community we are in (Daily Gaming): the renderer shows it like any other.
    Member,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Browse,
    BrowseEmpty,
    BrowseSearching,
    Join(Join),
    InviteManage,
    InviteManageCreating,
    InviteManageEmpty,
    InviteManageTruncated,
    InviteFriends,
    ServerMenu(Role),
    LeaveConfirm,
    CreateChannel(Create),
    ChannelMenu,
    DraggingChannel,
    DraggingCategory,
    CategorySyncPrompt,
    WelcomeText,
    WelcomeEncrypted,
}

const DG: i32 = shell::DAILY_GAMING;
const ME: i64 = shell::ME;
/// When the fixture's invites were made (a fixed clock: Sep 2, 2026).
pub const T0: i64 = 1_788_350_400;
pub const INVITE_CODE: &str = "Q7MZX4KP2A";
pub const MEMBER_CODE: &str = "C3N4QV5E9H";
const PIXEL_FORGE_PICTURE: &str = "fixture-pixel-forge-v1";

fn server_info(id: i32, name: &str, description: &str, members: i32, picture: &str) -> ServerInfo {
    ServerInfo {
        id,
        name: name.into(),
        description: description.into(),
        host_ip: format!("127.0.0.{}", 10 + id),
        port: 8082,
        member_count: members,
        picture_version: picture.into(),
    }
}

/// Central's public directory: two communities we are in (not listed by Browse) and six others.
fn directory(all: bool) -> Vec<ServerInfo> {
    let mut list = vec![
        server_info(1, "Daily Gaming", "Games every night", 298, ""),
        server_info(3, "Design Lab", "Pixels and type", 44, "fixture-design-lab-v1"),
    ];
    if all {
        list.extend([
            server_info(
                4,
                "Pixel Forge",
                "Indie game devs sharing progress every week, with playtests on Friday nights and a feedback swap.",
                1284,
                PIXEL_FORGE_PICTURE,
            ),
            server_info(5, "Night Owls", "", 57, ""),
            server_info(6, "Synth Club", "Modular synths, patches and gear talk", 312, ""),
            server_info(7, "Open Kitchen", "Recipes, techniques and the occasional disaster. Share what you cooked this week!", 845, ""),
            server_info(8, "Trail Runners", "Routes, races and recovery.", 96, ""),
            server_info(9, "Chess Corner", "Daily puzzles and weekend blitz", 2, ""),
        ]);
    }
    list
}

const MEMBER_PERMS: u64 = 8192 | 16384 | 1024 | 32768 | 8388608 | 2048 | 16777216 | 4096 | 2097152 | 4194304;
const READ_ONLY: u64 = 8192 | 16384;
const CREATE_INVITE: u64 = 2097152;
const MANAGE_INVITES: u64 = 128;

fn role(id: i64, name: &str, color: u32, position: i32, permissions: u64, members: i64) -> RoleInfoPayload {
    RoleInfoPayload { id, name: name.into(), color, position, permissions, is_default: id == 1, manage_each_other: false, member_count: members }
}

fn roles(everyone: u64, moderator: u64) -> CoreEvent {
    CoreEvent::RoleListReceived(RoleListReceivedPayload {
        server_id: DG.to_string(),
        success: true,
        message: String::new(),
        roles: vec![
            role(2, "Admin", 0xe74c3c, 3, 1, 1),
            role(3, "Moderator", 0x3498db, 2, moderator, 4),
            role(4, "Member", 0x2ecc71, 1, 0, 204),
            role(1, "everyone", 0, 0, everyone, 298),
        ],
    })
}

fn me_with_roles(roles: &[i64]) -> CoreEvent {
    CoreEvent::MemberUpsert(MemberUpsertPayload {
        server_id: DG.to_string(),
        member: MemberInfoPayload {
            username: shell::ME_NAME.into(),
            uid: ME,
            joined_at: 1_760_000_000,
            nickname: String::new(),
            is_owner: false,
            is_online: true,
            role_ids: roles.to_vec(),
            timed_out_until: 0,
        },
        revision: 2,
        online_count: 20,
        total_members: 298,
    })
}

fn own_dg() -> CoreEvent {
    CoreEvent::ServerMetaUpdated(ServerMetaUpdatedPayload {
        server_id: DG.to_string(),
        server_name: "Daily Gaming".into(),
        server_description: "Games every night".into(),
        owner_username: shell::ME_NAME.into(),
        owner_uid: ME,
        public_listing: true,
    })
}

fn invite(code: &str, by: (&str, i64), created: i64, expires: i64, max: i32, uses: i32, to: Option<(&str, i64)>) -> InviteInfoPayload {
    InviteInfoPayload {
        code: code.into(),
        created_by: by.0.into(),
        created_by_uid: by.1,
        created_at: created,
        expires_at: expires,
        max_uses: max,
        uses,
        for_user: to.map(|t| t.0.to_string()).unwrap_or_default(),
        for_uid: to.map_or(0, |t| t.1),
    }
}

fn invites(empty: bool, truncated: bool) -> CoreEvent {
    let list = if empty {
        Vec::new()
    } else {
        vec![
            invite("K9DM3XQ7PA", (shell::ME_NAME, ME), T0, T0 + 7 * 86400, 0, 3, None),
            invite("W2HN8RT4LE", ("alice", 1), T0 - 3600, 0, 10, 7, None),
            invite("B5QX9ZC1MV", ("bob", 2), T0 - 7200, 0, 1, 0, Some(("perf2", 7))),
            invite("Y8FJ2KP6SD", ("carol", 3), T0 - 86400, T0 + 3600, 25, 25, None),
            invite("N3TC7WB4GH", ("dave", 4), T0 - 2 * 86400, T0 + 30 * 86400, 0, 112, None),
        ]
    };
    CoreEvent::InviteListReceived(InviteListReceivedPayload {
        server_id: DG.to_string(),
        success: true,
        message: String::new(),
        invites: list,
        truncated,
    })
}

fn friend(name: &str, uid: i64, status: &str) -> FriendInfoPayload {
    FriendInfoPayload { username: name.into(), uid, status: status.into(), avatar_version: String::new() }
}

fn presence(name: &str, uid: i64, status: &str) -> UserPresencePayload {
    UserPresencePayload {
        username: name.into(),
        uid,
        avatar_version: String::new(),
        status: status.into(),
        status_text: String::new(),
        status_emoji: String::new(),
    }
}

/// Friends who aren't Daily Gaming members (mira, kenji, noor, tomas) beside the shell's.
const MIRA: i64 = 21;
const KENJI: i64 = 22;
const NOOR: i64 = 23;
const TOMAS: i64 = 24;

fn friends() -> Vec<CoreEvent> {
    vec![
        CoreEvent::FriendListReceived(FriendListReceivedPayload {
            friends: vec![
                friend("perf2", 7, "online"),
                friend("alice", 1, "online"),
                friend("perf3", 8, "offline"),
                friend("mira", MIRA, "online"),
                friend("kenji", KENJI, "offline"),
                friend("noor", NOOR, "online"),
                friend("tomas", TOMAS, "online"),
                friend("perf4", 9, "pending_incoming"),
                friend("erin", 5, "pending_outgoing"),
            ],
        }),
        CoreEvent::UserListUpdated(UserListUpdatedPayload {
            users: vec![presence("mira", MIRA, "online"), presence("noor", NOOR, "dnd"), presence("tomas", TOMAS, "away")],
            covered: vec![MIRA, KENJI, NOOR, TOMAS],
            avatars: Vec::new(),
        }),
    ]
}

fn channel(id: &str, name: &str, kind: &str, perms: u64, encrypted: bool, sync: bool) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: name.into(),
        r#type: kind.into(),
        voice_bitrate_kbps: if kind == "voice" { 64 } else { 0 },
        retention_days_text: 0,
        retention_days_image: 0,
        retention_days_video: 0,
        retention_days_document: 0,
        retention_days_audio: 0,
        my_permissions: perms,
        slowmode_seconds: if id == "slow-lane" { 30 } else { 0 },
        encrypted,
        permissions_synced: false,
        category_sync: sync,
        topic: String::new(),
    }
}

/// Daily Gaming's channels with "Text Channels" letting its channels follow its permissions.
fn syncing_list() -> CoreEvent {
    let cat = |id: &str, name: &str, sync: bool| channel(id, name, "category", READ_ONLY, false, sync);
    CoreEvent::ChannelListUpdated(ChannelListUpdatedPayload {
        server_id: DG.to_string(),
        channels: vec![
            cat("cat-information", "Information", false),
            channel("welcome", "welcome", "text", READ_ONLY, false, false),
            channel("announcements", "announcements", "text", READ_ONLY, false, false),
            cat("cat-text-channels", "Text Channels", true),
            channel("general", "general", "text", MEMBER_PERMS, false, false),
            channel("clips", "clips", "text", MEMBER_PERMS, false, false),
            channel("off-topic", "off-topic", "text", MEMBER_PERMS, false, false),
            channel("slow-lane", "slow-lane", "text", MEMBER_PERMS, false, false),
            channel("vault", "vault", "text", MEMBER_PERMS, true, false),
            cat("cat-voice-channels", "Voice Channels", false),
            channel("lounge", "Lounge", "voice", MEMBER_PERMS, false, false),
            channel("squad", "Squad", "voice", MEMBER_PERMS, false, false),
            channel("afk", "AFK", "voice", MEMBER_PERMS, false, false),
        ],
    })
}

fn empty_history(channel: &str) -> CoreEvent {
    CoreEvent::ChannelHistoryReceived(ChannelHistoryReceivedPayload {
        server_id: DG.to_string(),
        channel_id: channel.into(),
        messages: Vec::new(),
        has_more: false,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 0,
    })
}

/// What the stage adds to the shell's events.
pub fn events(stage: Stage) -> Vec<CoreEvent> {
    let mut events = Vec::new();
    let owner = matches!(
        stage,
        Stage::InviteManage
            | Stage::InviteManageCreating
            | Stage::InviteManageEmpty
            | Stage::InviteManageTruncated
            | Stage::ServerMenu(Role::Owner)
            | Stage::CreateChannel(_)
            | Stage::ChannelMenu
            | Stage::DraggingChannel
            | Stage::DraggingCategory
            | Stage::CategorySyncPrompt
            | Stage::WelcomeText
            | Stage::WelcomeEncrypted
    );
    if owner {
        events.push(own_dg());
    }
    match stage {
        Stage::Browse | Stage::BrowseSearching => {
            events.push(CoreEvent::ServerListReceived(ServerListReceivedPayload { servers: directory(true) }));
            events.push(CoreEvent::ServerPictureReceived(ServerPictureReceivedPayload {
                server_id: 4,
                version: PIXEL_FORGE_PICTURE.into(),
                data: shell::picture_data_url(),
            }));
        }
        Stage::BrowseEmpty => events.push(CoreEvent::ServerListReceived(ServerListReceivedPayload { servers: directory(false) })),
        Stage::InviteManage | Stage::InviteManageCreating => events.push(invites(false, false)),
        Stage::InviteManageEmpty => events.push(invites(true, false)),
        Stage::InviteManageTruncated => events.push(invites(false, true)),
        Stage::InviteFriends => events.extend(friends()),
        Stage::ServerMenu(Role::Moderator) => {
            events.push(roles(MEMBER_PERMS, 64 | 16 | 131072 | 1048576 | MANAGE_INVITES));
            events.push(me_with_roles(&[3]));
        }
        Stage::ServerMenu(Role::Member) => {
            events.push(roles(MEMBER_PERMS & !CREATE_INVITE, 64 | 16 | 131072 | 1048576));
            events.push(me_with_roles(&[4]));
        }
        Stage::CategorySyncPrompt => events.push(syncing_list()),
        Stage::WelcomeText => events.push(empty_history("clips")),
        Stage::WelcomeEncrypted => events.push(empty_history("vault")),
        _ => {}
    }
    events
}

/// A point of the Electron page (CSS px of the 1280 × 800 framed window) in this window.
fn at(x: f32, y: f32) -> gpui::Point<gpui::Pixels> {
    point(px(x + MARGIN), px(y + MARGIN))
}

/// Where the screens put the pointer: the context menu over `#general`, the drags' pointer.
pub const CHANNEL_MENU_AT: (f32, f32) = (132., 300.);
pub const DRAG_POINTER: (f32, f32) = (110., 296.);

fn resolved(code: &str, member: bool) -> ResolvedInvite {
    if member {
        ResolvedInvite {
            host: "127.0.0.11".into(),
            port: 8082,
            code: code.into(),
            server_id: 1,
            server_name: "Daily Gaming".into(),
            server_description: "Games every night".into(),
            member_count: 298,
            picture_version: "".into(),
        }
    } else {
        ResolvedInvite {
            host: "127.0.0.14".into(),
            port: 8082,
            code: code.into(),
            server_id: 4,
            server_name: "Pixel Forge".into(),
            server_description: "Indie game devs sharing progress every week, with playtests on Friday nights and a feedback swap.".into(),
            member_count: 1284,
            picture_version: PIXEL_FORGE_PICTURE.into(),
        }
    }
}

/// Fill the model (made without a core) with the shell's fixture and the stage's, then do what
/// the user did.
pub fn apply(stage: Stage, cx: &mut App) -> Model {
    let model = shell::apply(Scene::Server, cx);
    for event in events(stage) {
        decibell_model::events::route(&model, event, cx);
    }
    let server = ServerId::from_central(DG);
    let mut screen = crate::ScreenState::default();
    match stage {
        Stage::Browse | Stage::BrowseEmpty => model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Browse, cx)),
        Stage::BrowseSearching => {
            screen.browse_search = Some("synth".into());
            model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Browse, cx));
        }
        Stage::Join(join) => {
            // No core: lookups stay where the fixture puts them.
            model.invites.update(cx, |i, _| *i = InviteResolve::new(Rc::new(|_, _| {})));
            let code = if join == Join::Member { MEMBER_CODE } else { INVITE_CODE };
            model.invites.update(cx, |i, cx| {
                i.request(code, cx);
                match join {
                    Join::Resolving => {}
                    Join::Resolved => i.finish(code.into(), Ok(resolved(code, false)), cx),
                    Join::Member => i.finish(code.into(), Ok(resolved(code, true)), cx),
                    Join::Expired => i.finish(
                        code.into(),
                        Err(decibell_model::error::UiError {
                            kind: decibell_core::api::ErrorKind::Other,
                            text: "Invite code is unknown or expired".into(),
                            cert: None,
                        }),
                        cx,
                    ),
                }
            });
            model.servers.update(cx, |s, cx| s.set_pending_invite(Some(PendingInvite { code: code.into(), host: None, port: None }), cx));
        }
        Stage::InviteManage | Stage::InviteManageEmpty | Stage::InviteManageTruncated => {
            model.overlays.update(cx, |o, cx| o.open_modal(decibell_model::stores::overlays::modal::INVITE_MANAGE, cx))
        }
        Stage::InviteManageCreating => {
            screen.invite_creating = true;
            model.overlays.update(cx, |o, cx| o.open_modal(decibell_model::stores::overlays::modal::INVITE_MANAGE, cx));
        }
        Stage::InviteFriends => {
            screen.friends = Some((CentralMembers::Known([7, 1, 8].into_iter().collect()), vec![(KENJI, RowState::Sent), (NOOR, RowState::Member)]));
            model.overlays.update(cx, |o, cx| o.open_modal(decibell_model::stores::overlays::modal::INVITE_FRIENDS, cx));
        }
        Stage::ServerMenu(_) => ShellRequests::open_sidebar_menu(SidebarMenu::Server, cx),
        Stage::LeaveConfirm => ShellRequests::leave_server(server, cx),
        Stage::CreateChannel(create) => {
            let (kind, category, choice) = match create {
                Create::Text | Create::Encrypted => (ChannelKind::Text, None, false),
                Create::Voice => (ChannelKind::Voice, Some(ChannelId::from("cat-voice-channels")), true),
                Create::Category => (ChannelKind::Category, None, false),
            };
            if create == Create::Encrypted {
                screen.create_fields = Some(("secret-plans".into(), true));
            }
            ShellRequests::entity(cx).update(cx, |r, cx| {
                r.create_channel = Some(CreateChannelRequest { server, kind, category, allow_type_choice: choice });
                cx.notify();
            });
        }
        Stage::ChannelMenu => ShellRequests::open_sidebar_menu(
            SidebarMenu::Channel(at(CHANNEL_MENU_AT.0, CHANNEL_MENU_AT.1), ChannelMenuTarget::Channel { server, channel: "general".into() }),
            cx,
        ),
        Stage::DraggingChannel | Stage::DraggingCategory => {
            let (channel, before) = if stage == Stage::DraggingChannel {
                ("clips", Before::Id("general".into()))
            } else {
                ("cat-voice-channels", Before::Id("cat-information".into()))
            };
            let drag = reorder::state(cx);
            let list = ScrollHandle::new();
            drag.update(cx, |d, cx| d.show_drag(server, channel.into(), None, at(DRAG_POINTER.0, DRAG_POINTER.1), Some(before), &list, cx));
        }
        Stage::CategorySyncPrompt => {
            let ids = ["cat-information", "welcome", "cat-text-channels", "announcements", "general", "clips", "off-topic", "slow-lane", "vault", "cat-voice-channels", "lounge", "squad", "afk"];
            let drag = reorder::state(cx);
            drag.update(cx, |d, cx| {
                d.show_prompt(
                    SyncPrompt {
                        server,
                        ids: ids.iter().map(|id| ChannelId::from(*id)).collect(),
                        channel: "announcements".into(),
                        category: "cat-text-channels".into(),
                        open: true,
                    },
                    cx,
                )
            });
        }
        Stage::WelcomeText => model.nav.update(cx, |n, cx| n.set_active_channel(Some("clips".into()), cx)),
        Stage::WelcomeEncrypted => model.nav.update(cx, |n, cx| n.set_active_channel(Some("vault".into()), cx)),
    }
    cx.set_global(screen);
    model
}

/// The stage for the Electron page: `{ shell, extra: [{ event, payload }], ui }`.
pub fn json(stage: Stage) -> Value {
    let extra: Vec<Value> = events(stage)
        .iter()
        .map(|e| {
            let (name, payload) = e.to_json();
            j!({ "event": name, "payload": payload })
        })
        .collect();
    let ui = match stage {
        Stage::Browse | Stage::BrowseEmpty => j!({ "view": "browse" }),
        Stage::BrowseSearching => j!({ "view": "browse", "search": "synth" }),
        Stage::Join(join) => {
            let code = if join == Join::Member { MEMBER_CODE } else { INVITE_CODE };
            let entry = match join {
                Join::Resolving => j!({ "status": "loading", "resolved": null, "error": null, "invalid": false }),
                Join::Resolved | Join::Member => {
                    let r = resolved(code, join == Join::Member);
                    j!({ "status": "done", "error": null, "invalid": false, "resolved": {
                        "host": r.host.to_string(), "port": r.port, "code": r.code.to_string(), "serverId": r.server_id,
                        "serverName": r.server_name.to_string(), "serverDescription": r.server_description.to_string(),
                        "memberCount": r.member_count, "pictureVersion": r.picture_version.to_string()
                    } })
                }
                Join::Expired => j!({ "status": "done", "resolved": null, "error": "Invite code is unknown or expired", "invalid": true }),
            };
            j!({ "pendingInvite": { "code": code }, "inviteResolve": { code: entry } })
        }
        Stage::InviteManage | Stage::InviteManageEmpty | Stage::InviteManageTruncated => j!({ "modal": "invite-manage" }),
        Stage::InviteManageCreating => j!({ "modal": "invite-manage", "inviteCreating": true }),
        Stage::InviteFriends => j!({
            "modal": "invite-friends",
            "friendsInCommunity": { "known": true, "memberUids": [7, 1, 8] },
            "friendRows": { "sent": [KENJI], "member": [NOOR] },
        }),
        Stage::ServerMenu(_) => j!({ "menu": "server" }),
        Stage::LeaveConfirm => j!({ "menu": "server", "click": "Leave Server" }),
        Stage::CreateChannel(create) => match create {
            Create::Text => j!({ "contextMenu": { "at": CHANNEL_MENU_AT, "on": null }, "click": "Create Text Channel" }),
            Create::Category => j!({ "contextMenu": { "at": CHANNEL_MENU_AT, "on": null }, "click": "Create Category" }),
            Create::Voice => j!({ "categoryPlus": "cat-voice-channels", "click": "voice" }),
            Create::Encrypted => j!({
                "contextMenu": { "at": CHANNEL_MENU_AT, "on": null },
                "click": "Create Text Channel",
                "createName": "secret-plans",
                "encrypted": true,
            }),
        },
        Stage::ChannelMenu => j!({ "contextMenu": { "at": CHANNEL_MENU_AT, "on": "general" } }),
        Stage::DraggingChannel => j!({ "drag": { "from": "clips", "over": "general", "top": true, "at": DRAG_POINTER } }),
        Stage::DraggingCategory => j!({ "drag": { "from": "cat-voice-channels", "over": "cat-information", "top": true, "at": DRAG_POINTER } }),
        Stage::CategorySyncPrompt => j!({ "drag": { "from": "announcements", "over": "general", "top": true, "at": DRAG_POINTER, "drop": true } }),
        Stage::WelcomeText => j!({ "channel": "clips" }),
        Stage::WelcomeEncrypted => j!({ "channel": "vault" }),
    };
    j!({ "shell": shell::json(Scene::Server), "extra": extra, "ui": ui })
}

/// Every stage, with the file name its JSON goes to.
pub fn stages() -> Vec<(&'static str, Stage)> {
    vec![
        ("browse", Stage::Browse),
        ("browse-empty", Stage::BrowseEmpty),
        ("browse-searching", Stage::BrowseSearching),
        ("join-dialog-resolving", Stage::Join(Join::Resolving)),
        ("join-dialog-resolved", Stage::Join(Join::Resolved)),
        ("join-dialog-expired", Stage::Join(Join::Expired)),
        ("join-dialog-member", Stage::Join(Join::Member)),
        ("invite-manage", Stage::InviteManage),
        ("invite-manage-creating", Stage::InviteManageCreating),
        ("invite-manage-empty", Stage::InviteManageEmpty),
        ("invite-manage-truncated", Stage::InviteManageTruncated),
        ("invite-friends", Stage::InviteFriends),
        ("server-menu-owner", Stage::ServerMenu(Role::Owner)),
        ("server-menu-moderator", Stage::ServerMenu(Role::Moderator)),
        ("server-menu-member", Stage::ServerMenu(Role::Member)),
        ("leave-confirm", Stage::LeaveConfirm),
        ("create-channel-text", Stage::CreateChannel(Create::Text)),
        ("create-channel-voice", Stage::CreateChannel(Create::Voice)),
        ("create-channel-category", Stage::CreateChannel(Create::Category)),
        ("create-channel-encrypted", Stage::CreateChannel(Create::Encrypted)),
        ("channel-menu", Stage::ChannelMenu),
        ("sidebar-dragging-channel", Stage::DraggingChannel),
        ("sidebar-dragging-category", Stage::DraggingCategory),
        ("category-sync-prompt", Stage::CategorySyncPrompt),
        ("welcome-text", Stage::WelcomeText),
        ("welcome-encrypted", Stage::WelcomeEncrypted),
    ]
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn the_stages_fill_the_stores(cx: &mut TestAppContext) {
        let model = cx.update(|cx| apply(Stage::Browse, cx));
        cx.run_until_parked();
        cx.update(|cx| {
            assert_eq!(model.servers.read(cx).discoverable().len(), 8);
            assert_eq!(model.nav.read(cx).active_view, ActiveView::Browse);
        });
    }

    #[test]
    fn every_stage_exports_json() {
        for (name, stage) in stages() {
            let v = json(stage);
            assert!(v["shell"]["events"].is_array(), "{name}");
            assert!(v["ui"].is_object(), "{name}");
        }
    }
}

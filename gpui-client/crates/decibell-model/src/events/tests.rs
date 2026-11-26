//! The router against recorded event sequences (built by hand): what the stores hold
//! after the events a sign-in, a join, a roster's life and a sign-out produce. No core,
//! no network: `Model::init` without `start_core`, so every core call a handler makes
//! answers "not running" at once.

use std::cell::RefCell;
use std::rc::Rc;

use decibell_core::events::*;
use gpui::TestAppContext;

use crate::Model;
use crate::ids::ServerId;
use crate::pump::Incoming;
use crate::stores::community::ChannelKind;
use crate::stores::connections::CentralStatus;
use crate::stores::navigation::ActiveView;
use crate::stores::permissions::perm;
use crate::stores::toasts::Severity;

// ── Builders ────────────────────────────────────────────────────────────────

fn server_info(id: i32, name: &str, picture_version: &str) -> ServerInfo {
    ServerInfo {
        id,
        name: name.into(),
        description: format!("{name} description"),
        host_ip: format!("127.0.0.{}", 10 + id),
        port: 8082,
        member_count: 298,
        picture_version: picture_version.into(),
    }
}

fn channel(id: &str, kind: &str, my_permissions: u64) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: id.into(),
        r#type: kind.into(),
        voice_bitrate_kbps: 64,
        retention_days_text: 0,
        retention_days_image: 0,
        retention_days_video: 0,
        retention_days_document: 0,
        retention_days_audio: 0,
        my_permissions,
        slowmode_seconds: 0,
        encrypted: false,
        permissions_synced: false,
        category_sync: false,
        topic: String::new(),
    }
}

const MEMBER_CHANNEL: u64 = perm::VIEW_CHANNEL | perm::READ_HISTORY | perm::SEND_MESSAGES;

fn daily_gaming_channels() -> Vec<ChannelInfoPayload> {
    vec![
        channel("cat-info", "category", perm::VIEW_CHANNEL),
        channel("announcements", "text", perm::VIEW_CHANNEL | perm::READ_HISTORY),
        channel("cat-text", "category", perm::VIEW_CHANNEL),
        channel("general", "text", MEMBER_CHANNEL),
        channel("lounge", "voice", perm::VIEW_CHANNEL | perm::CONNECT_VOICE | perm::SPEAK),
    ]
}

fn auth_ok(server_id: &str, requested: &str, name: &str, owner_uid: i64, channels: Vec<ChannelInfoPayload>) -> CoreEvent {
    CoreEvent::CommunityAuthResponded(CommunityAuthRespondedPayload {
        server_id: server_id.into(),
        requested_server_id: requested.into(),
        host: "127.0.0.11".into(),
        port: 8082,
        success: true,
        message: String::new(),
        channels,
        error_code: String::new(),
        server_name: name.into(),
        server_description: "desc".into(),
        owner_username: "alice".into(),
        owner_uid,
        attachment_port: 8085,
        max_attachment_bytes: 1 << 30,
        public_listing: true,
    })
}

fn auth_refused(server_id: &str, message: &str) -> CoreEvent {
    CoreEvent::CommunityAuthResponded(CommunityAuthRespondedPayload {
        server_id: server_id.into(),
        requested_server_id: server_id.into(),
        host: "127.0.0.11".into(),
        port: 8082,
        success: false,
        message: message.into(),
        channels: vec![],
        error_code: "banned".into(),
        server_name: String::new(),
        server_description: String::new(),
        owner_username: String::new(),
        owner_uid: 0,
        attachment_port: 0,
        max_attachment_bytes: 0,
        public_listing: false,
    })
}

fn member(uid: i64, username: &str, online: bool, role_ids: &[i64]) -> MemberInfoPayload {
    MemberInfoPayload {
        username: username.into(),
        uid,
        joined_at: 1_700_000_000,
        nickname: String::new(),
        is_owner: uid == 1,
        is_online: online,
        role_ids: role_ids.to_vec(),
        timed_out_until: 0,
    }
}

fn member_page(server: &str, members: Vec<MemberInfoPayload>, extra: Vec<MemberInfoPayload>, revision: u64, first: bool) -> CoreEvent {
    CoreEvent::MemberListReceived(MemberListReceivedPayload {
        server_id: server.into(),
        success: true,
        message: String::new(),
        members,
        revision,
        total_members: 298,
        has_more: false,
        next_after: String::new(),
        first_page: first,
        online_count: 3,
        online_complete: true,
        extra,
        lookup_uids: vec![],
    })
}

fn upsert(server: &str, member: MemberInfoPayload, revision: u64, online_count: i64) -> CoreEvent {
    CoreEvent::MemberUpsert(MemberUpsertPayload {
        server_id: server.into(),
        member,
        revision,
        online_count,
        total_members: 298,
    })
}

fn role(id: i64, name: &str, position: i32, permissions: u64) -> RoleInfoPayload {
    RoleInfoPayload {
        id,
        name: name.into(),
        color: 0,
        position,
        permissions,
        is_default: position == 0,
        manage_each_other: false,
        member_count: 1,
    }
}

fn roles(server: &str) -> CoreEvent {
    CoreEvent::RoleListReceived(RoleListReceivedPayload {
        server_id: server.into(),
        success: true,
        message: String::new(),
        roles: vec![
            role(30, "Admin", 3, perm::ADMINISTRATOR),
            role(20, "Moderator", 2, perm::KICK_MEMBERS | perm::MANAGE_NICKNAMES),
            role(10, "Member", 1, perm::ATTACH_FILES),
            role(1, "everyone", 0, perm::VIEW_CHANNEL | perm::SEND_MESSAGES | perm::CHANGE_NICKNAME),
        ],
    })
}

fn login(username: &str, uid: i64) -> CoreEvent {
    CoreEvent::LoginSucceeded(LoginSucceededPayload { username: username.into(), uid, renames: true, devices: true })
}

fn connection(kind: &str, server_id: Option<&str>) -> ConnectionEventPayload {
    ConnectionEventPayload { server_type: kind.into(), server_id: server_id.map(Into::into) }
}

// ── Harness ─────────────────────────────────────────────────────────────────

fn model(cx: &mut TestAppContext) -> Model {
    cx.update(|cx| Model::init(cx, None))
}

/// Deliver events as the core would: through the sink's queue and the pump.
fn deliver(model: &Model, cx: &mut TestAppContext, events: impl IntoIterator<Item = CoreEvent>) {
    for event in events {
        model.sink().queue().push(Incoming::Core(event));
    }
    cx.run_until_parked();
}

fn toasts(model: &Model, cx: &mut TestAppContext) -> Vec<(Severity, String, Option<String>)> {
    model.toasts.read_with(cx, |t, _| {
        t.toasts().iter().map(|t| (t.severity, t.title.to_string(), t.body.as_ref().map(|b| b.to_string()))).collect()
    })
}

fn signed_in_perf2(cx: &mut TestAppContext) -> Model {
    let model = model(cx);
    deliver(
        &model,
        cx,
        [
            login("perf2", 7),
            CoreEvent::MembershipsReceived(MembershipsReceivedPayload {
                memberships: vec![server_info(1, "Daily Gaming", "pic1")],
            }),
        ],
    );
    model
}

fn gaming() -> ServerId {
    ServerId::from_central(1)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[gpui::test]
fn sign_in_memberships_and_a_community_answer(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    cx.read(|cx| {
        let auth = model.auth.read(cx);
        assert!(auth.is_authenticated && auth.renames && auth.devices);
        assert_eq!((auth.username.as_deref(), auth.uid), (Some("perf2"), 7));
        assert!(auth.is_me(7) && !auth.is_me(1) && !auth.is_me(0));
        // Central named us.
        assert_eq!(&*model.name_of(7, &"?".into(), cx), "perf2");
        // The membership is a tile, marked connecting, with its picture version known.
        let servers = model.servers.read(cx);
        assert_eq!(servers.servers().len(), 1);
        assert_eq!(&*servers.get(gaming()).unwrap().name, "Daily Gaming");
        assert_eq!(servers.picture_version(gaming()), "pic1");
        let connections = model.connections.read(cx);
        assert!(connections.is_pending(gaming()) && !connections.is_connected(gaming()));
        assert!(model.community(gaming(), cx).is_none(), "nothing from the community yet");
    });

    // The user is looking at the community when it answers.
    cx.update(|cx| crate::actions::servers::open_server(gaming(), cx));
    cx.read(|cx| {
        let nav = model.nav.read(cx);
        assert_eq!((nav.active_view, nav.active_server, nav.active_channel.clone()), (ActiveView::Server, Some(gaming()), None));
    });
    deliver(&model, cx, [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels())]);
    cx.read(|cx| {
        let connections = model.connections.read(cx);
        assert!(connections.is_connected(gaming()) && !connections.is_pending(gaming()));
        let community = model.community(gaming(), cx).unwrap();
        let meta = community.meta.read(cx);
        assert_eq!((&*meta.name, meta.owner, meta.public_listing, meta.attachment_port), ("Daily Gaming", 1, true, 8085));
        let channels = community.channels.read(cx);
        assert_eq!(channels.list().len(), 5);
        assert_eq!(channels.get("lounge").unwrap().kind, ChannelKind::Voice);
        assert_eq!(&*channels.category_of("general").unwrap().id, "cat-text");
        // Landed on the first text channel, because this community is on screen.
        assert_eq!(model.nav.read(cx).active_channel.as_deref(), Some("announcements"));
        // The owner was named by the community (central hasn't named uid 1).
        assert_eq!(&*model.name_of(1, &"?".into(), cx), "alice");
        // Still one tile: the community's own answer doesn't duplicate the membership.
        assert_eq!(model.servers.read(cx).servers().len(), 1);
    });
}

#[gpui::test]
fn an_answer_for_another_community_does_not_move_the_active_channel(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    deliver(&model, cx, [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels())]);
    cx.read(|cx| assert_eq!(model.nav.read(cx).active_channel, None, "not on screen: nothing selected"));
    cx.update(|cx| crate::actions::servers::open_server(gaming(), cx));
    cx.read(|cx| assert_eq!(model.nav.read(cx).active_channel.as_deref(), Some("announcements")));
    model.nav.update(cx, |nav, cx| nav.set_active_channel(Some("general".into()), cx));
    // A second community answers, then the first one re-authenticates after a reconnect:
    // the user's channel stays.
    deliver(
        &model,
        cx,
        [
            auth_ok("2", "2", "Rustaceans", 2, vec![channel("general", "text", MEMBER_CHANNEL)]),
            auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels()),
        ],
    );
    cx.read(|cx| {
        let nav = model.nav.read(cx);
        assert_eq!((nav.active_server, nav.active_channel.as_deref()), (Some(gaming()), Some("general")));
        assert_eq!(model.servers.read(cx).servers().len(), 2);
    });
}

#[gpui::test]
fn roster_roles_and_permissions_of_a_member_and_the_owner(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    deliver(
        &model,
        cx,
        [
            auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels()),
            roles("1"),
            member_page(
                "1",
                vec![member(1, "alice", true, &[]), member(2, "bob", true, &[30]), member(3, "carol", false, &[20])],
                // As the community sends our own row on a first page: every role twice.
                vec![member(7, "perf2", true, &[10, 10])],
                40,
                true,
            ),
        ],
    );
    let community = cx.read(|cx| model.community(gaming(), cx).unwrap());
    cx.read(|cx| {
        let roster = community.roster.read(cx);
        let meta = roster.meta().unwrap();
        assert_eq!((meta.revision, meta.total_members, meta.online_count, meta.refetching), (40, 298, 3, false));
        let visible = roster.visible();
        assert_eq!(visible.online, [1, 2, 7]);
        assert_eq!(visible.offline, [3]);
        assert!(roster.member(1).unwrap().is_owner);
        assert_eq!(&*roster.member(7).unwrap().role_ids, &[10], "repeated role ids are dropped");
        assert_eq!(community.roles.read(cx).list().len(), 4);

        // perf2 holds Member: everyone's bits plus ATTACH_FILES, nothing else.
        assert!(!model.is_owner(gaming(), cx));
        assert!(model.has_permission(gaming(), perm::SEND_MESSAGES | perm::ATTACH_FILES, cx));
        assert!(!model.has_permission(gaming(), perm::KICK_MEMBERS, cx));
        assert!(!model.has_permission(gaming(), perm::MANAGE_CHANNELS, cx));
        // Per channel, the community's resolved answer decides.
        assert!(model.channel_permission(gaming(), "general", perm::SEND_MESSAGES, cx));
        assert!(!model.channel_permission(gaming(), "announcements", perm::SEND_MESSAGES, cx));
        assert!(model.channel_permission(gaming(), "announcements", perm::VIEW_CHANNEL, cx));
        assert!(!model.channel_permission(gaming(), "mod-chat", perm::VIEW_CHANNEL, cx));
        // A community we know nothing about.
        assert!(!model.has_permission(ServerId::from_central(99), perm::VIEW_CHANNEL, cx));
    });

    // A live delta: carol comes online; the next revision continues.
    deliver(&model, cx, [upsert("1", member(3, "carol", true, &[20]), 41, 4)]);
    cx.read(|cx| {
        let roster = community.roster.read(cx);
        assert_eq!(roster.visible().online, [1, 2, 3, 7]);
        assert!(roster.visible().offline.is_empty());
        assert_eq!(roster.meta().unwrap().online_count, 4);
    });

    // The same events seen by the owner: every permission, everywhere.
    deliver(&model, cx, [login("alice", 1)]);
    cx.read(|cx| {
        assert!(model.is_owner(gaming(), cx));
        assert!(model.has_permission(gaming(), perm::KICK_MEMBERS | perm::MANAGE_SERVER, cx));
        assert!(model.channel_permission(gaming(), "announcements", perm::SEND_MESSAGES, cx));
    });
    // And by the admin: ADMINISTRATOR is everything server-wide, but a channel still
    // goes by what the community resolved for the session it sent the list to.
    deliver(&model, cx, [login("bob", 2)]);
    cx.read(|cx| {
        assert!(!model.is_owner(gaming(), cx));
        assert!(model.has_permission(gaming(), perm::BAN_MEMBERS, cx));
        assert!(!model.channel_permission(gaming(), "announcements", perm::SEND_MESSAGES, cx));
    });
}

#[gpui::test]
fn a_missed_delta_asks_for_the_first_page_again(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    deliver(
        &model,
        cx,
        [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels()), member_page("1", vec![member(1, "alice", true, &[])], vec![], 10, true)],
    );
    let community = cx.read(|cx| model.community(gaming(), cx).unwrap());
    // Revision 12 after 10: a delta went missing. The handler refetches; with no core
    // the request fails at once, which ends the wait (as when a connection is gone).
    deliver(&model, cx, [upsert("1", member(2, "bob", true, &[]), 12, 2)]);
    cx.read(|cx| {
        let roster = community.roster.read(cx);
        assert!(roster.member(2).is_none(), "the out-of-order delta was not applied");
        assert_eq!(roster.meta().unwrap().revision, 10);
        assert!(!roster.meta().unwrap().refetching);
    });
    // The fresh first page replaces the roster under a new generation.
    let generation = cx.read(|cx| community.roster.read(cx).meta().unwrap().generation);
    deliver(&model, cx, [member_page("1", vec![member(1, "alice", true, &[]), member(2, "bob", true, &[])], vec![], 12, true)]);
    cx.read(|cx| {
        let roster = community.roster.read(cx);
        assert_eq!(roster.visible().online, [1, 2]);
        assert!(roster.meta().unwrap().generation > generation);
    });
    // A member leaves.
    deliver(
        &model,
        cx,
        [CoreEvent::MemberRemove(MemberRemovePayload {
            server_id: "1".into(),
            username: "bob".into(),
            uid: 2,
            revision: 13,
            online_count: 1,
            total_members: 297,
        })],
    );
    cx.read(|cx| {
        let roster = community.roster.read(cx);
        assert_eq!(roster.visible().online, [1]);
        assert_eq!(roster.meta().unwrap().total_members, 297);
    });
}

#[gpui::test]
fn names_central_wins_and_nicknames_show_in_their_community(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    let mut bobby = member(2, "bob", true, &[]);
    bobby.nickname = "Bobby 🛠".into();
    deliver(
        &model,
        cx,
        [
            auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels()),
            member_page("1", vec![bobby, member(100_001, "placeholder_guy", false, &[])], vec![], 1, true),
        ],
    );
    let fallback = gpui::SharedString::from("payload-name");
    cx.read(|cx| {
        assert_eq!(&*model.name_of(2, &fallback, cx), "bob");
        // In the community the nickname shows; elsewhere (a DM) the name.
        assert_eq!(&*model.display_name(Some(gaming()), 2, &fallback, cx), "Bobby 🛠");
        assert_eq!(&*model.display_name(None, 2, &fallback, cx), "bob");
        assert_eq!(&*model.display_name(Some(ServerId::from_central(2)), 2, &fallback, cx), "bob");
        // Someone nobody named: the payload's name.
        assert_eq!(&*model.display_name(Some(gaming()), 555, &fallback, cx), "payload-name");
    });
    // Central renames bob; the community still sends the old name with a later delta.
    deliver(
        &model,
        cx,
        [
            CoreEvent::UserRenamed(UserRenamedPayload { uid: 2, username: "robert".into(), is_self: false }),
            upsert("1", member(2, "bob", true, &[]), 2, 1),
        ],
    );
    cx.read(|cx| {
        assert_eq!(&*model.name_of(2, &fallback, cx), "robert", "central's name wins over the community's older one");
        // The nickname is gone with the delta: the current name shows, not the community's.
        assert_eq!(&*model.display_name(Some(gaming()), 2, &fallback, cx), "robert");
        // The sidebar's order is the community's, by the name it holds.
        let community = model.community(gaming(), cx).unwrap();
        assert_eq!(&*community.roster.read(cx).member(2).unwrap().key.name, "bob");
    });
    // Our own rename reaches the auth store too.
    deliver(&model, cx, [CoreEvent::UserRenamed(UserRenamedPayload { uid: 7, username: "perf-two".into(), is_self: true })]);
    cx.read(|cx| assert_eq!(model.auth.read(cx).username.as_deref(), Some("perf-two")));
}

#[gpui::test]
fn channel_list_changes_follow_the_user(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    cx.update(|cx| crate::actions::servers::open_server(gaming(), cx));
    deliver(&model, cx, [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels())]);
    model.nav.update(cx, |nav, cx| nav.set_active_channel(Some("general".into()), cx));

    // #general is deleted: land on the first text channel left.
    let mut channels = daily_gaming_channels();
    channels.retain(|c| c.id != "general");
    deliver(&model, cx, [CoreEvent::ChannelListUpdated(ChannelListUpdatedPayload { server_id: "1".into(), channels })]);
    cx.read(|cx| {
        assert_eq!(model.nav.read(cx).active_channel.as_deref(), Some("announcements"));
        assert!(!model.community(gaming(), cx).unwrap().channels.read(cx).contains("general"));
    });

    // A settings update that carries no permissions keeps ours.
    let mut renamed = channel("announcements", "text", 0);
    renamed.topic = "Read me".into();
    renamed.slowmode_seconds = 30;
    deliver(
        &model,
        cx,
        [CoreEvent::ChannelUpdated(ChannelUpdatedPayload { server_id: "1".into(), success: true, message: String::new(), channel: Some(renamed) })],
    );
    cx.read(|cx| {
        let community = model.community(gaming(), cx).unwrap();
        let channels = community.channels.read(cx);
        let c = channels.get("announcements").unwrap();
        assert_eq!((&*c.topic, c.slowmode_seconds, c.my_permissions), ("Read me", 30, perm::VIEW_CHANNEL | perm::READ_HISTORY));
    });

    // A channel we created: jump into it; a refused action is a toast.
    deliver(
        &model,
        cx,
        [
            CoreEvent::ChannelActionResponded(ChannelActionRespondedPayload {
                server_id: "1".into(),
                success: true,
                message: String::new(),
                action: "create".into(),
                channel: Some(channel("new-room", "text", MEMBER_CHANNEL)),
            }),
            CoreEvent::ChannelActionResponded(ChannelActionRespondedPayload {
                server_id: "1".into(),
                success: false,
                message: "Missing permission".into(),
                action: "delete".into(),
                channel: None,
            }),
            CoreEvent::RoleActionResponded(RoleActionRespondedPayload {
                server_id: "1".into(),
                success: false,
                message: String::new(),
                action: "update".into(),
                role: None,
            }),
        ],
    );
    cx.read(|cx| assert_eq!(model.nav.read(cx).active_channel.as_deref(), Some("new-room")));
    assert_eq!(
        toasts(&model, cx),
        [(Severity::Error, "Missing permission".into(), None), (Severity::Error, "Role action failed".into(), None)]
    );
}

#[gpui::test]
fn refused_joins_revocations_and_leaving(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    // The auto-rejoin is refused: the stale membership is dropped, with one toast.
    deliver(&model, cx, [auth_refused("1", "You are banned")]);
    cx.read(|cx| {
        assert!(!model.connections.read(cx).is_pending(gaming()));
        assert!(model.overlays.read(cx).auth_error.is_none());
    });
    assert_eq!(
        toasts(&model, cx),
        [(Severity::Error, "Membership revoked".into(), Some("You're no longer a member of Daily Gaming.".into()))]
    );
    // A join the user asked for is refused: the notice for the inline error, and a toast.
    deliver(&model, cx, [auth_refused("3", "")]);
    cx.read(|cx| {
        let notice = model.overlays.read(cx).auth_error.clone().unwrap();
        assert_eq!((notice.server, &*notice.error_code), (ServerId::from_central(3), "banned"));
    });
    assert_eq!(toasts(&model, cx)[1], (Severity::Error, "Couldn't join server".into(), Some("Connection rejected.".into())));

    // Connected and on screen, then kicked: back home with the notice.
    cx.update(|cx| crate::actions::servers::open_server(gaming(), cx));
    deliver(&model, cx, [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels())]);
    deliver(
        &model,
        cx,
        [CoreEvent::MembershipRevoked(MembershipRevokedPayload {
            server_id: "1".into(),
            action: "kick".into(),
            reason: "spam".into(),
            actor: "alice".into(),
            actor_uid: 1,
            expires_at: 0,
        })],
    );
    cx.read(|cx| {
        let nav = model.nav.read(cx);
        assert_eq!((nav.active_view, nav.active_server, nav.active_channel.clone()), (ActiveView::Home, None, None));
        assert!(!model.connections.read(cx).is_connected(gaming()));
        let notice = model.overlays.read(cx).membership_revocation.clone().unwrap();
        assert_eq!((&*notice.action, &*notice.reason, &*notice.actor), ("kick", "spam", "alice"));
    });

    // Leaving by choice: the browse view. A refused moderation action: its text.
    let mod_action = |success, action: &str, message: &str| {
        CoreEvent::ModActionResponded(ModActionRespondedPayload {
            server_id: "1".into(),
            success,
            message: message.into(),
            username: String::new(),
            uid: 0,
            action: action.into(),
        })
    };
    deliver(&model, cx, [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels()), mod_action(true, "leave", "")]);
    cx.read(|cx| {
        assert_eq!(model.nav.read(cx).active_view, ActiveView::Browse);
        assert!(!model.connections.read(cx).is_connected(gaming()));
    });
    deliver(&model, cx, [mod_action(false, "kick", "You can't kick someone above you")]);
    assert_eq!(toasts(&model, cx).last().unwrap().1, "You can't kick someone above you");
}

#[gpui::test]
fn an_invite_join_is_re_keyed_onto_centrals_id(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    let provisional = ServerId::from_wire("127.0.0.13:8082");
    let design = ServerId::from_central(3);
    // `redeem_invite` opens the view under the provisional id and the connection comes up.
    cx.update(|cx| crate::actions::servers::open_server(provisional, cx));
    deliver(&model, cx, [CoreEvent::ConnectionRestored(connection("community", Some("127.0.0.13:8082")))]);
    cx.read(|cx| assert!(model.connections.read(cx).is_connected(provisional)));
    // The community answers with central's id.
    deliver(&model, cx, [auth_ok("3", "127.0.0.13:8082", "Design Lab", 3, vec![channel("critique", "text", MEMBER_CHANNEL)])]);
    cx.read(|cx| {
        let nav = model.nav.read(cx);
        assert_eq!((nav.active_server, nav.active_channel.as_deref()), (Some(design), Some("critique")));
        let connections = model.connections.read(cx);
        assert!(connections.is_connected(design) && !connections.is_connected(provisional));
        // A tile exists although central's list never named it here.
        assert_eq!(&*model.servers.read(cx).get(design).unwrap().name, "Design Lab");
        assert!(model.community(provisional, cx).is_none());
    });
}

#[gpui::test]
fn connection_state_is_apart_from_content(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    deliver(&model, cx, [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels())]);
    let channels_notified = Rc::new(RefCell::new(0));
    let _sub = cx.update(|cx| {
        let community = model.community(gaming(), cx).unwrap();
        let count = channels_notified.clone();
        cx.observe(&community.channels, move |_, _| *count.borrow_mut() += 1)
    });
    deliver(
        &model,
        cx,
        [CoreEvent::ConnectionLost(connection("central", None)), CoreEvent::ConnectionLost(connection("community", Some("1")))],
    );
    cx.read(|cx| {
        let connections = model.connections.read(cx);
        assert_eq!(connections.central, CentralStatus::Reconnecting);
        assert!(!connections.is_connected(gaming()));
        // The channel tree is still there to show, greyed by whoever reads `connections`.
        assert_eq!(model.community(gaming(), cx).unwrap().channels.read(cx).list().len(), 5);
    });
    deliver(
        &model,
        cx,
        [
            CoreEvent::ConnectionRestored(connection("central", None)),
            CoreEvent::ConnectionRestored(connection("community", Some("1"))),
        ],
    );
    cx.read(|cx| {
        let connections = model.connections.read(cx);
        assert_eq!(connections.central, CentralStatus::Connected);
        assert!(connections.is_connected(gaming()));
    });
    assert_eq!(*channels_notified.borrow(), 0, "a connection flap re-renders no channel list");

    // Central refuses to sign this device back in while we are signed in: a warning.
    deliver(&model, cx, [CoreEvent::LoginFailed(LoginFailedPayload { message: "Too many devices".into() })]);
    assert_eq!(toasts(&model, cx), [(Severity::Warning, "Can't reconnect".into(), Some("Too many devices".into()))]);
}

#[gpui::test]
fn lists_pictures_invites_and_bans(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    deliver(
        &model,
        cx,
        [
            CoreEvent::ServerListReceived(ServerListReceivedPayload {
                servers: vec![server_info(1, "Daily Gaming", "pic1"), server_info(3, "Design Lab", "")],
            }),
            CoreEvent::MembershipAdded(server_info(2, "Rustaceans", "pic2")),
            CoreEvent::ServerPictureReceived(ServerPictureReceivedPayload { server_id: 1, version: "pic1".into(), data: "data:image/jpeg;base64,AAAA".into() }),
            CoreEvent::ServerPictureReceived(ServerPictureReceivedPayload { server_id: 2, version: "old".into(), data: "data:stale".into() }),
            CoreEvent::ServerPictureReceived(ServerPictureReceivedPayload { server_id: 3, version: String::new(), data: String::new() }),
        ],
    );
    cx.read(|cx| {
        let servers = model.servers.read(cx);
        // The directory never touches the user's tiles; another device's join adds one.
        assert_eq!(servers.discoverable().len(), 2);
        assert_eq!(servers.servers().iter().map(|s| &*s.name).collect::<Vec<_>>(), ["Daily Gaming", "Rustaceans"]);
        assert!(servers.picture(gaming()).is_some());
        assert!(servers.picture(ServerId::from_central(2)).is_none(), "a fetch for another version is dropped");
        assert!(servers.picture(ServerId::from_central(3)).is_none());
    });
    deliver(&model, cx, [CoreEvent::ServerPictureChanged(ServerPictureChangedPayload { server_id: 1, version: "pic9".into() })]);
    cx.read(|cx| {
        let servers = model.servers.read(cx);
        assert_eq!(servers.picture_version(gaming()), "pic9");
        assert!(servers.picture(gaming()).is_none(), "a new version drops the old bytes");
    });

    let invite = |code: &str| InviteInfoPayload {
        code: code.into(),
        created_by: "alice".into(),
        created_by_uid: 1,
        created_at: 1,
        expires_at: 0,
        max_uses: 0,
        uses: 0,
        for_user: String::new(),
        for_uid: 0,
    };
    let created = |success, invite: Option<InviteInfoPayload>, request_id| {
        CoreEvent::InviteCreateResponded(InviteCreateRespondedPayload {
            server_id: "1".into(),
            success,
            message: String::new(),
            invite,
            request_id,
            already_member: false,
        })
    };
    deliver(
        &model,
        cx,
        [
            CoreEvent::InviteListReceived(InviteListReceivedPayload {
                server_id: "1".into(),
                success: true,
                message: String::new(),
                invites: vec![invite("AAAA000001"), invite("BBBB000002")],
                truncated: true,
            }),
            created(true, Some(invite("CCCC000003")), 0),
            // A refused personal invite (it has a request id) reports to its caller only.
            created(false, None, 17),
            created(false, None, 0),
            CoreEvent::InviteRevokeResponded(InviteRevokeRespondedPayload { server_id: "1".into(), success: true, message: String::new(), code: "AAAA000001".into() }),
            CoreEvent::BanListReceived(BanListReceivedPayload {
                server_id: "1".into(),
                success: true,
                message: String::new(),
                entries: vec![BanInfoPayload {
                    username: "griefer_9000".into(),
                    uid: 100_050,
                    banned_by: "alice".into(),
                    banned_by_uid: 1,
                    reason: "griefing".into(),
                    banned_at: 5,
                    expires_at: 0,
                }],
                revision: 3,
                truncated: false,
            }),
        ],
    );
    cx.read(|cx| {
        let community = model.community(gaming(), cx).unwrap();
        let moderation = community.moderation.read(cx);
        assert_eq!(moderation.invites.iter().map(|i| &*i.code).collect::<Vec<_>>(), ["CCCC000003", "BBBB000002"]);
        assert!(moderation.invites_truncated);
        assert_eq!(&*moderation.bans[0].username, "griefer_9000");
        // Names kept as history never feed the directory.
        assert_eq!(&*model.name_of(100_050, &"-".into(), cx), "-");
    });
    assert_eq!(toasts(&model, cx), [(Severity::Error, "Failed to create invite".into(), None)]);
}

#[gpui::test]
fn answers_reach_the_views_that_listen(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    let heard: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let _sub = cx.update(|cx| {
        let heard = heard.clone();
        cx.subscribe(&model.answers, move |_, event: &CoreEvent, _| heard.borrow_mut().push(event.name()))
    });
    deliver(
        &model,
        cx,
        [
            CoreEvent::ServerUpdateResponded(ServerUpdateRespondedPayload { server_id: "1".into(), success: false, message: "Name too long".into() }),
            CoreEvent::VoicePingUpdated(VoicePingUpdatedPayload { latency_ms: 20 }),
            CoreEvent::ChannelWipeResponded(ChannelWipeRespondedPayload {
                server_id: "1".into(),
                channel_id: "general".into(),
                success: true,
                message: String::new(),
                deleted_message_count: 10,
                deleted_attachment_count: 2,
            }),
        ],
    );
    assert_eq!(*heard.borrow(), ["server_update_responded", "channel_wipe_responded"]);
}

#[gpui::test]
fn signing_out_leaves_nothing_for_the_next_account(cx: &mut TestAppContext) {
    let model = signed_in_perf2(cx);
    cx.update(|cx| crate::actions::servers::open_server(gaming(), cx));
    deliver(
        &model,
        cx,
        [auth_ok("1", "1", "Daily Gaming", 1, daily_gaming_channels()), member_page("1", vec![member(1, "alice", true, &[])], vec![], 1, true)],
    );
    model.overlays.update(cx, |o, cx| {
        o.open_modal(crate::stores::overlays::modal::SETTINGS, cx);
        o.open_profile_popup(1, crate::stores::overlays::Anchor { x: 1.0, y: 2.0 }, Some(gaming()), cx);
    });
    deliver(&model, cx, [CoreEvent::LoggedOut(LoggedOutPayload { reason: Some("password_changed".into()) })]);
    cx.read(|cx| {
        let auth = model.auth.read(cx);
        assert!(!auth.is_authenticated);
        assert_eq!((auth.username.clone(), auth.uid), (None, 0));
        assert_eq!(auth.login_notice.as_deref(), Some("Your password was changed on another device. Sign in with the new one."));
        assert!(model.servers.read(cx).servers().is_empty());
        assert!(model.community(gaming(), cx).is_none());
        assert_eq!(model.connections.read(cx).connected().count(), 0);
        let nav = model.nav.read(cx);
        assert_eq!((nav.active_view, nav.active_server, nav.active_channel.clone()), (ActiveView::Home, None, None));
        let overlays = model.overlays.read(cx);
        assert!(overlays.active_modal.is_none() && overlays.profile_popup.is_none());
    });
    // Our own Log out: no notice. Another device signing this one out: its notice. The
    // next sign-in attempt clears it.
    deliver(&model, cx, [CoreEvent::LoggedOut(LoggedOutPayload { reason: None })]);
    cx.read(|cx| assert_eq!(model.auth.read(cx).login_notice, None));
    deliver(&model, cx, [CoreEvent::LoggedOut(LoggedOutPayload { reason: Some("signed_out".into()) })]);
    cx.read(|cx| {
        assert_eq!(model.auth.read(cx).login_notice.as_deref(), Some("You were signed out on this device. Sign in again to keep using it here."))
    });
    deliver(&model, cx, [login("perf3", 8)]);
    cx.read(|cx| {
        let auth = model.auth.read(cx);
        assert!(auth.is_authenticated && auth.login_notice.is_none());
        assert_eq!(auth.uid, 8);
    });
}

#[gpui::test]
fn sign_in_page_states(cx: &mut TestAppContext) {
    let model = model(cx);
    // No core: the call fails at once, and the page shows it as the renderer does
    // (`String(err)`).
    cx.update(|cx| crate::actions::auth::sign_in("perf2".into(), "wrong".into(), cx));
    cx.run_until_parked();
    cx.read(|cx| {
        let auth = model.auth.read(cx);
        assert!(!auth.is_logging_in);
        assert_eq!(auth.login_error.as_deref(), Some("Error: Not connected to central server"));
    });
    // Central's refusal arrives as an event, with its own words.
    deliver(&model, cx, [CoreEvent::LoginFailed(LoginFailedPayload { message: "Invalid username or password".into() })]);
    cx.read(|cx| assert_eq!(model.auth.read(cx).login_error.as_deref(), Some("Invalid username or password")));
    assert!(toasts(&model, cx).is_empty(), "signed out: the page shows it, no toast");

    cx.update(|cx| crate::actions::auth::register("newbie".into(), "n@example.test".into(), "pw".into(), cx));
    cx.run_until_parked();
    cx.read(|cx| assert!(!model.auth.read(cx).register_result.as_ref().unwrap().success));
    deliver(&model, cx, [CoreEvent::RegisterResponded(RegisterRespondedPayload { success: true, message: "Account created".into() })]);
    cx.read(|cx| {
        let auth = model.auth.read(cx);
        assert!(!auth.is_registering);
        assert_eq!(auth.register_result.as_ref().map(|r| (r.success, &*r.message)), Some((true, "Account created")));
    });
}

#[gpui::test]
fn every_event_name_is_routed(_cx: &mut TestAppContext) {
    // The router's match has no wildcard, so the compiler already holds it to the
    // enum; this pins the count the docs quote.
    assert_eq!(CoreEvent::NAMES.len(), 85);
}

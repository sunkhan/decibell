//! The settings screens' fixture: the shell's (`decibell_shell::fixture`, perf1 in Daily Gaming)
//! plus what the settings windows show, built by hand as the core would emit it and fed through the
//! model's real router: perf1 owns the community (so every tab shows), roles of every shape, bans,
//! channel overwrites and a syncing category. The answers no store keeps (member search, the audit
//! log, storage) come from a fixture requester ([`install`]) that plays the community server: it
//! answers each request through the router, as the real one would.
//!
//! [`json`] exports the same data for the Electron capture pages
//! (`tools/electron-captures/pages/fixtures/settings.json`; `decibell-server-settings-fixture
//! <dir>` writes it).

use std::rc::Rc;

use decibell_core::events::*;
use decibell_model::stores::permissions::perm;
use decibell_model::{Model, ServerId};
use decibell_shell::fixture::{self as shell, Scene};
use gpui::App;

use crate::requests::{self, Request};

/// "Now" in every settings screen (pinned: the role preview's clock, time-out countdowns, the
/// audit log's day headings): 2026-11-25 14:32 UTC.
pub const NOW: i64 = 1_795_617_120;
pub const DAILY_GAMING: i32 = shell::DAILY_GAMING;
pub const ME: i64 = shell::ME;

pub fn server() -> ServerId {
    ServerId::from_central(DAILY_GAMING)
}

/// Permissions of an @everyone member in an ordinary channel (the shell fixture's).
const MEMBER: u64 = 8192 | 16384 | 1024 | 32768 | 8388608 | 2048 | 16777216 | 4096 | 2097152 | 4194304;
const READ_ONLY: u64 = 8192 | 16384;

fn role(id: i64, name: &str, color: u32, position: i32, permissions: u64, members: i64, each_other: bool) -> RoleInfoPayload {
    RoleInfoPayload { id, name: name.into(), color, position, permissions, is_default: id == 1, manage_each_other: each_other, member_count: members }
}

pub fn roles() -> Vec<RoleInfoPayload> {
    use perm::*;
    vec![
        role(2, "Admin", 0xe74c3c, 6, ADMINISTRATOR, 1, false),
        role(
            3,
            "Moderator",
            0x3498db,
            5,
            MANAGE_MESSAGES | KICK_MEMBERS | MODERATE_MEMBERS | MOVE_MEMBERS | MUTE_MEMBERS | DEAFEN_MEMBERS | MANAGE_NICKNAMES | VIEW_AUDIT_LOG,
            4,
            true,
        ),
        role(5, "Event Host", 0x7c5cff, 4, CREATE_INVITE | MANAGE_CHANNELS | MOVE_MEMBERS, 3, false),
        role(6, "Streamer", 0x1abc9c, 3, STREAM | EMBED_LINKS, 12, false),
        role(4, "Member", 0x2ecc71, 2, 0, 204, false),
        role(7, "Muted", 0, 1, 0, 2, false),
        role(1, "everyone", 0, 0, MEMBER, 298, false),
    ]
}

#[allow(clippy::too_many_arguments)]
fn channel(id: &str, name: &str, kind: &str, perms: u64, topic: &str, encrypted: bool, synced: bool, category_sync: bool) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: name.into(),
        r#type: kind.into(),
        voice_bitrate_kbps: match id {
            "lounge" => 96,
            _ => 0,
        },
        retention_days_text: 0,
        retention_days_image: if id == "clips" { 30 } else { 0 },
        retention_days_video: if id == "clips" { 7 } else { 0 },
        retention_days_document: if id == "clips" { 90 } else { 0 },
        retention_days_audio: if id == "clips" { 45 } else { 0 },
        my_permissions: perms,
        slowmode_seconds: if id == "slow-lane" { 30 } else { 0 },
        encrypted,
        permissions_synced: synced,
        category_sync,
        topic: topic.into(),
    }
}

/// Daily Gaming's channels as the owner sees them: the "Text Channels" category syncs, most of its
/// channels follow it.
pub fn channels() -> Vec<ChannelInfoPayload> {
    let all = perm::ALL;
    let _ = (MEMBER, READ_ONLY);
    vec![
        channel("cat-information", "Information", "category", all, "", false, false, false),
        channel("welcome", "welcome", "text", all, "", false, false, false),
        channel("announcements", "announcements", "text", all, "News from the mods", false, false, false),
        channel("cat-text-channels", "Text Channels", "category", all, "", false, false, true),
        channel("general", "general", "text", all, "Anything goes — be nice", false, false, false),
        channel("clips", "clips", "text", all, "", false, true, false),
        channel("off-topic", "off-topic", "text", all, "Everything else", false, true, false),
        channel("slow-lane", "slow-lane", "text", all, "", false, false, false),
        channel("vault", "vault", "text", all, "", true, true, false),
        channel("cat-voice-channels", "Voice Channels", "category", all, "", false, false, false),
        channel("lounge", "Lounge", "voice", all, "", false, false, false),
        channel("squad", "Squad", "voice", all, "", false, false, false),
        channel("afk", "AFK", "voice", all, "", false, false, false),
    ]
}

fn overwrite(channel: &str, kind: &str, id: &str, uid: i64, allow: u64, deny: u64) -> ChannelOverwritePayload {
    ChannelOverwritePayload { channel_id: channel.into(), target_type: kind.into(), target_id: id.into(), target_uid: uid, allow, deny }
}

pub fn overwrites(channel_id: &str) -> Vec<ChannelOverwritePayload> {
    use perm::*;
    match channel_id {
        "general" => vec![
            overwrite("general", "role", "1", 0, READ_HISTORY, EMBED_LINKS),
            overwrite("general", "role", "3", 0, MANAGE_MESSAGES, 0),
            overwrite("general", "role", "7", 0, 0, SEND_MESSAGES | ATTACH_FILES),
            overwrite("general", "member", "perf2", 7, EMBED_LINKS, 0),
        ],
        "cat-text-channels" => vec![overwrite("cat-text-channels", "role", "1", 0, ATTACH_FILES, 0), overwrite("cat-text-channels", "role", "6", 0, EMBED_LINKS, 0)],
        "lounge" => vec![overwrite("lounge", "role", "7", 0, 0, SPEAK | STREAM)],
        _ => Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn member(name: &str, uid: i64, nickname: &str, online: bool, roles: &[i64], owner: bool, joined: i64, timed_out: i64) -> MemberInfoPayload {
    MemberInfoPayload {
        username: name.into(),
        uid,
        joined_at: joined,
        nickname: nickname.into(),
        is_owner: owner,
        is_online: online,
        role_ids: roles.to_vec(),
        timed_out_until: timed_out,
    }
}

/// Every member of the community, as the server's search sees them (by username).
pub fn members() -> Vec<MemberInfoPayload> {
    let day = 86_400;
    let base = 1_760_000_000;
    let mut out = vec![
        member("alice", 1, "", true, &[2], false, base, 0),
        member("bob", 2, "Bobby", true, &[3, 6], false, base + 3 * day, 0),
        member("carol", 3, "", true, &[3], false, base + 5 * day, 0),
        member("dave", 4, "", true, &[5, 4, 6, 7], false, base + 9 * day, 0),
        member("erin", 5, "", false, &[4], false, base + 12 * day, 0),
        member("perf1", ME, "", true, &[4], true, base - 30 * day, 0),
        member("perf2", 7, "", true, &[6, 4], false, base + 20 * day, 0),
        member("perf3", 8, "Third", false, &[4], false, base + 21 * day, 0),
        member("perf4", 9, "", false, &[], false, base + 22 * day, 0),
    ];
    const ONLINE: [&str; 14] =
        ["aurora", "blaze", "cinder", "drift", "ember", "flux", "glimmer", "halo", "iris", "jolt", "kestrel", "lumen", "mosaic", "nova"];
    const OFFLINE: [&str; 12] = ["onyx", "pixel", "quartz", "raven", "sable", "talon", "umbra", "vesper", "willow", "xenon", "yarrow", "zephyr"];
    for (i, name) in ONLINE.iter().enumerate() {
        let timed_out = if *name == "glimmer" { NOW + 2 * 3600 + 59 * 60 } else { 0 };
        out.push(member(name, -1000 - i as i64, "", true, &[4], false, base + (30 + i as i64) * day, timed_out));
    }
    for (i, name) in OFFLINE.iter().enumerate() {
        out.push(member(name, -2000 - i as i64, "", false, &[4], false, base + (50 + i as i64) * day, 0));
    }
    for i in 0..(298 - out.len()) {
        out.push(member(&format!("member{:03}", i + 1), -3000 - i as i64, "", i % 3 == 0, &[4], false, base + (80 + i as i64) * day, 0));
    }
    out
}

pub fn bans() -> BanListReceivedPayload {
    let ban = |name: &str, uid, by: &str, by_uid, reason: &str, at, expires| BanInfoPayload {
        username: String::from(name),
        uid,
        banned_by: String::from(by),
        banned_by_uid: by_uid,
        reason: String::from(reason),
        banned_at: at,
        expires_at: expires,
    };
    BanListReceivedPayload {
        server_id: DAILY_GAMING.to_string(),
        success: true,
        message: String::new(),
        entries: vec![
            ban("toxic_tim", 100_051, "bob", 2, "Harassment in voice", NOW - 5 * 3600, NOW + 7 * 86_400),
            ban("griefer_9000", 100_050, "alice", 1, "Spam links in #general", NOW - 2 * 86_400, 0),
            ban("old_account", 100_052, "carol", 3, "", NOW - 40 * 86_400, 0),
        ],
        revision: 1,
        truncated: true,
    }
}

/// `format_utc` on the server: "2026-10-12 22:40 UTC".
fn utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02} UTC", rem / 3600, rem % 3600 / 60)
}

/// One entry of every action the server writes, newest first, over today, yesterday, this week,
/// earlier this year and last year.
pub fn audit() -> Vec<AuditEntryPayload> {
    let h = 3600;
    let d = 86_400;
    let mut id = 1000;
    let mut e = |at: i64, actor: (&str, i64), action: &str, target: (&str, i64), channel: &str, details: String| {
        id -= 1;
        AuditEntryPayload {
            id,
            timestamp: NOW - at,
            actor: actor.0.into(),
            actor_uid: actor.1,
            action: action.into(),
            target: target.0.into(),
            target_uid: target.1,
            channel_id: channel.into(),
            details,
        }
    };
    let (alice, bob, carol, me) = (("alice", 1), ("bob", 2), ("carol", 3), ("perf1", ME));
    vec![
        e(20 * 60, bob, "kick", ("erin", 5), "", "reason: Repeated spam after a warning".into()),
        e(45 * 60, bob, "ban", ("griefer_9000", 100_050), "", "reason: Spam links in #general; 42 messages purged".into()),
        e(2 * h, alice, "unban", ("old_account", 100_052), "", String::new()),
        e(3 * h, carol, "timeout", ("glimmer", -1006), "", format!("until {}; reason: Cool down", utc(NOW - 3 * h + 6 * h))),
        e(3 * h + 600, carol, "timeout_clear", ("drift", -1003), "", String::new()),
        e(4 * h, bob, "message_delete", ("blaze", -1001), "general", "message 12345".into()),
        e(5 * h, carol, "voice_mod", ("dave", 4), "squad", "voice_move (voice pass)".into()),
        e(5 * h + 300, carol, "voice_mod", ("perf2", 7), "lounge", "server_mute".into()),
        e(5 * h + 600, carol, "voice_mod", ("perf4", 9), "lounge", "voice_disconnect".into()),
        e(d + 2 * h, me, "nickname", ("bob", 2), "", "set to Bobby".into()),
        e(d + 3 * h, carol, "nickname", ("carol", 3), "", "cleared".into()),
        e(d + 4 * h, me, "member_roles", ("perf2", 7), "", "+Streamer -Muted ".into()),
        e(d + 5 * h, me, "role_create", ("Event Host", 0), "", String::new()),
        e(3 * d, me, "role_update", ("Moderator", 0), "", "permissions changed; members can manage each other".into()),
        e(3 * d + h, me, "role_update", ("everyone", 0), "", "permissions changed".into()),
        e(3 * d + 2 * h, alice, "role_delete", ("Old Role", 0), "", String::new()),
        e(4 * d, me, "channel_create", ("vault", 0), "vault", "encrypted".into()),
        e(4 * d + h, me, "channel_rename", ("off-topic", 0), "off-topic", String::new()),
        e(4 * d + 2 * h, me, "channel_update", ("slow-lane", 0), "slow-lane", "slowmode 30s".into()),
        e(4 * d + 3 * h, me, "channel_update", ("general", 0), "general", "topic".into()),
        e(4 * d + 4 * h, me, "channel_update", ("clips", 0), "clips", "retention".into()),
        e(5 * d, alice, "channel_delete", ("memes", 0), "", "1204 messages deleted".into()),
        e(5 * d + h, me, "channel_wipe", ("clips", 0), "clips", "87 messages deleted".into()),
        e(20 * d, me, "overwrite_set", ("role:7", 0), "general", format!("allow 0 deny {}", perm::SEND_MESSAGES | perm::ATTACH_FILES)),
        e(20 * d + h, me, "overwrite_set", ("member:perf2", 7), "general", format!("allow {} deny 0", perm::EMBED_LINKS)),
        e(20 * d + 2 * h, me, "overwrite_set", ("role:3", 0), "announcements", "cleared".into()),
        e(21 * d, me, "overwrite_sync", ("clips", 0), "clips", "synced with category cat-text-channels".into()),
        e(21 * d + h, me, "category_sync", ("Text Channels", 0), "cat-text-channels", "on".into()),
        e(22 * d, bob, "invite_create", ("C3N4QV5E9H", 0), "", "max uses 10".into()),
        e(22 * d + h, me, "invite_create", ("PX7KQ2M9AB", 0), "", "for perf3; max uses 1".into()),
        e(22 * d + 2 * h, alice, "invite_revoke", ("OLDCODE123", 0), "", String::new()),
        e(30 * d, me, "server_update", ("Daily Gaming", 0), "", "name: Daily Gaming; public: on".into()),
        e(30 * d + h, me, "storage_min_free", ("", 0), "", (5u64 << 30).to_string()),
        e(370 * d, alice, "ownership_transfer", ("perf1", ME), "", String::new()),
    ]
}

pub fn storage() -> StorageInfoReceivedPayload {
    let gb = 1u64 << 30;
    let mb = 1u64 << 20;
    StorageInfoReceivedPayload {
        server_id: DAILY_GAMING.to_string(),
        success: true,
        message: String::new(),
        volume_total_bytes: (2000 * gb) as i64,
        volume_available_bytes: (612 * gb + 400 * mb) as i64,
        attachments_bytes: (18 * gb + 410 * mb) as i64,
        thumbnails_bytes: (312 * mb) as i64,
        database_bytes: (96 * mb) as i64,
        attachment_count: 1234,
        min_free_bytes: (5 * gb) as i64,
        by_kind: vec![
            StorageKindUsagePayload { kind: 0, bytes: (9 * gb + 120 * mb) as i64, count: 900 },
            StorageKindUsagePayload { kind: 1, bytes: (7 * gb + 820 * mb) as i64, count: 120 },
            StorageKindUsagePayload { kind: 2, bytes: (gb + 210 * mb) as i64, count: 180 },
            StorageKindUsagePayload { kind: 3, bytes: (310 * mb) as i64, count: 34 },
        ],
        by_channel: vec![
            StorageChannelUsagePayload { channel_id: "clips".into(), bytes: (10 * gb + 200 * mb) as i64, count: 400 },
            StorageChannelUsagePayload { channel_id: "general".into(), bytes: (6 * gb) as i64, count: 600 },
            StorageChannelUsagePayload { channel_id: "off-topic".into(), bytes: (2 * gb + 210 * mb) as i64, count: 234 },
        ],
        largest: vec![
            StorageLargestItemPayload { attachment_id: 9001, filename: "raid-highlights-final-v2.mp4".into(), size_bytes: (gb + 900 * mb) as i64, channel_id: "clips".into(), kind: 1 },
            StorageLargestItemPayload { attachment_id: 9002, filename: "tournament-bracket.pdf".into(), size_bytes: (48 * mb) as i64, channel_id: "general".into(), kind: 2 },
            StorageLargestItemPayload { attachment_id: 9003, filename: String::new(), size_bytes: (12 * mb) as i64, channel_id: "off-topic".into(), kind: 0 },
        ],
    }
}

/// What the settings fixture adds to the shell's, in order.
pub fn events() -> Vec<CoreEvent> {
    let sid = DAILY_GAMING.to_string();
    let all = members();
    let find = |uid: i64| all.iter().find(|m| m.uid == uid).cloned().expect("fixture member");
    let mut events = vec![
        CoreEvent::ServerMetaUpdated(ServerMetaUpdatedPayload {
            server_id: sid.clone(),
            server_name: "Daily Gaming".into(),
            server_description: "Games every night".into(),
            owner_username: shell::ME_NAME.into(),
            owner_uid: ME,
            public_listing: true,
        }),
        CoreEvent::ChannelListUpdated(ChannelListUpdatedPayload { server_id: sid.clone(), channels: channels() }),
        CoreEvent::RoleListReceived(RoleListReceivedPayload { server_id: sid.clone(), success: true, message: String::new(), roles: roles() }),
    ];
    // The roster's records follow: alice no longer owns it, perf1 does; the role changes.
    for (rev, uid) in [(2u64, 1i64), (3, ME), (4, 2), (5, 4), (6, 7), (7, 8)] {
        events.push(CoreEvent::MemberUpsert(MemberUpsertPayload {
            server_id: sid.clone(),
            member: find(uid),
            revision: rev,
            online_count: 20,
            total_members: 298,
        }));
    }
    events.push(CoreEvent::BanListReceived(bans()));
    for channel in ["general", "cat-text-channels", "lounge"] {
        events.push(CoreEvent::ChannelOverwritesReceived(ChannelOverwritesReceivedPayload {
            server_id: sid.clone(),
            success: true,
            message: String::new(),
            channel_id: channel.into(),
            overwrites: overwrites(channel),
        }));
    }
    events
}

/// The community server's answers, as the fixture plays it.
pub fn answer(request: &Request) -> Vec<CoreEvent> {
    let sid = DAILY_GAMING.to_string();
    match request {
        Request::SearchMembers { query, role_id, after, limit, seq, .. } => {
            let q = query.to_lowercase();
            let mut found: Vec<MemberInfoPayload> = members()
                .into_iter()
                .filter(|m| {
                    (match *role_id {
                        -1 => m.role_ids.is_empty(),
                        0 => true,
                        r => m.role_ids.contains(&r),
                    }) && (q.is_empty() || m.username.to_lowercase().contains(&q) || m.nickname.to_lowercase().contains(&q))
                })
                .collect();
            found.sort_by(|a, b| a.username.cmp(&b.username));
            let total = found.len() as i64;
            let start = if after.is_empty() { 0 } else { found.iter().position(|m| m.username.as_str() > after.as_str()).unwrap_or(found.len()) };
            let page: Vec<MemberInfoPayload> = found.iter().skip(start).take(*limit as usize).cloned().collect();
            let has_more = start + page.len() < found.len();
            vec![CoreEvent::MemberSearchReceived(MemberSearchReceivedPayload {
                server_id: sid,
                success: true,
                message: String::new(),
                next_after: page.last().map(|m| m.username.clone()).unwrap_or_default(),
                members: page,
                total_matches: total,
                has_more,
                seq: *seq,
                first_page: after.is_empty(),
            })]
        }
        Request::ListAuditLog { before_id, limit, query, actions, seq, .. } => {
            let q = query.to_lowercase();
            let mut entries: Vec<AuditEntryPayload> = audit()
                .into_iter()
                .filter(|e| {
                    (*before_id == 0 || e.id < *before_id)
                        && (actions.is_empty() || actions.contains(&e.action))
                        && (q.is_empty() || [&e.actor, &e.target, &e.details, &e.channel_id].iter().any(|f| f.to_lowercase().contains(&q)))
                })
                .collect();
            let has_more = entries.len() > *limit as usize;
            entries.truncate(*limit as usize);
            vec![CoreEvent::AuditLogReceived(AuditLogReceivedPayload { server_id: sid, success: true, message: String::new(), entries, has_more, seq: *seq })]
        }
        Request::StorageInfo { .. } => vec![CoreEvent::StorageInfoReceived(storage())],
        Request::ListBans { .. } => vec![CoreEvent::BanListReceived(bans())],
        Request::ListRoles { .. } => {
            vec![CoreEvent::RoleListReceived(RoleListReceivedPayload { server_id: sid, success: true, message: String::new(), roles: roles() })]
        }
        Request::ListOverwrites { channel, .. } => vec![CoreEvent::ChannelOverwritesReceived(ChannelOverwritesReceivedPayload {
            server_id: sid,
            success: true,
            message: String::new(),
            channel_id: channel.to_string(),
            overwrites: overwrites(channel),
        })],
        _ => Vec::new(),
    }
}

/// Put the fixture's community server in front of every request: its answers come through the
/// model's router after the request returned, as the real ones would.
pub fn install(cx: &mut App) {
    requests::install(
        Rc::new(|request, cx| {
            let events = answer(request);
            cx.defer(move |cx| {
                let Some(model) = cx.try_global::<Model>().cloned() else { return };
                for event in events {
                    decibell_model::events::route(&model, event, cx);
                }
            });
            Some(Ok(()))
        }),
        cx,
    );
}

/// The shell's fixture (Daily Gaming #general) plus the settings data, with the fixture server
/// installed and "now" pinned.
pub fn apply(cx: &mut App) -> Model {
    crate::pin_now(Some(NOW));
    let model = shell::apply(Scene::Server, cx);
    for event in events() {
        decibell_model::events::route(&model, event, cx);
    }
    install(cx);
    model
}

/// The fixture as the Electron pages replay it: the shell's events and navigation, the settings
/// events, and the data the pages' stubbed `invoke` answers from.
pub fn json() -> serde_json::Value {
    let mut shell = shell::json(Scene::Server);
    let extra: Vec<serde_json::Value> = events()
        .iter()
        .map(|e| {
            let (name, payload) = e.to_json();
            serde_json::json!({ "event": name, "payload": payload })
        })
        .collect();
    if let Some(list) = shell["events"].as_array_mut() {
        list.extend(extra);
    }
    let to = |e: CoreEvent| e.to_json().1;
    shell["now"] = serde_json::json!(NOW);
    shell["members"] = serde_json::to_value(members()).unwrap_or_default();
    shell["audit"] = serde_json::to_value(audit()).unwrap_or_default();
    shell["storage"] = to(CoreEvent::StorageInfoReceived(storage()));
    shell
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_times_are_written_as_the_server_does() {
        assert_eq!(utc(1_791_844_800), "2026-10-12 22:40 UTC");
        assert_eq!(utc(60), "1970-01-01 00:01 UTC");
    }

    #[test]
    fn the_fixture_has_one_entry_of_every_action() {
        let actions: std::collections::HashSet<String> = audit().into_iter().map(|e| e.action).collect();
        for (action, ..) in crate::server::audit::ACTIONS {
            assert!(actions.contains(action), "{action}");
        }
        assert_eq!(members().len(), 298);
    }

    #[test]
    fn the_fixture_server_pages_member_search() {
        let first = answer(&Request::SearchMembers { server: server(), query: String::new(), role_id: 0, after: String::new(), limit: 100, seq: 3 });
        let CoreEvent::MemberSearchReceived(p) = &first[0] else { panic!() };
        assert_eq!((p.members.len(), p.total_matches, p.has_more, p.seq), (100, 298, true, 3));
        let next = answer(&Request::SearchMembers { server: server(), query: String::new(), role_id: 0, after: p.next_after.clone(), limit: 100, seq: 3 });
        let CoreEvent::MemberSearchReceived(q) = &next[0] else { panic!() };
        assert!(!q.first_page && q.members[0].username > p.next_after);
        let bobby = answer(&Request::SearchMembers { server: server(), query: "BOBBY".into(), role_id: 0, after: String::new(), limit: 100, seq: 4 });
        let CoreEvent::MemberSearchReceived(b) = &bobby[0] else { panic!() };
        assert_eq!(b.members.iter().map(|m| m.username.as_str()).collect::<Vec<_>>(), ["bob"]);
        let none = answer(&Request::SearchMembers { server: server(), query: String::new(), role_id: -1, after: String::new(), limit: 100, seq: 5 });
        let CoreEvent::MemberSearchReceived(n) = &none[0] else { panic!() };
        assert_eq!(n.members.iter().map(|m| m.username.as_str()).collect::<Vec<_>>(), ["perf4"]);
    }
}

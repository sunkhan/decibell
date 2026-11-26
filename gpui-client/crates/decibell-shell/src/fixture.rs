//! The shell's deterministic fixture: what the core would emit for `perf1` signed in to the local
//! stack's three communities, built by hand and fed through the model's real router
//! (`decibell_model::events::route`), so the screens exercise the same code as live data.
//!
//! [`json`] exports the same events as the Electron addon would deliver them (`CoreEvent::to_json`:
//! name + camelCase payload) plus the navigation the screen shows, for the Electron capture pages
//! (`tools/electron-captures/pages/main-*.tsx`), which replay them through the renderer's own event
//! hooks. `decibell-capture --export-shell-fixture <file>` writes it.

use decibell_core::events::*;
use decibell_model::live::voice::rosters::VoiceKey;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{ChannelId, Model, ServerId, actions};
use gpui::App;

pub const ME: i64 = 6;
pub const ME_NAME: &str = "perf1";
pub const DAILY_GAMING: i32 = 1;

/// The fixture's navigation: which view, community, channel, and the voice channel we are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    /// Home (the Friends page beside the conversation list).
    Home,
    /// Daily Gaming `#general`, in voice in `Squad`.
    Server,
}

fn server_info(id: i32, name: &str, description: &str, host: &str, members: i32, picture: &str) -> ServerInfo {
    ServerInfo {
        id,
        name: name.into(),
        description: description.into(),
        host_ip: host.into(),
        port: 8082,
        member_count: members,
        picture_version: picture.into(),
    }
}

/// Permissions of an @everyone member in an ordinary channel: view, read, send, attach, embed,
/// connect, speak, stream, create invites, change nickname.
const MEMBER: u64 = 8192 | 16384 | 1024 | 32768 | 8388608 | 2048 | 16777216 | 4096 | 2097152 | 4194304;
/// Read-only (`announcements`).
const READ_ONLY: u64 = 8192 | 16384;

fn channel(id: &str, name: &str, kind: &str, perms: u64, topic: &str, encrypted: bool) -> ChannelInfoPayload {
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
        category_sync: false,
        topic: topic.into(),
    }
}

fn auth(id: i32, name: &str, description: &str, host: &str, owner: (&str, i64), channels: Vec<ChannelInfoPayload>) -> CoreEvent {
    CoreEvent::CommunityAuthResponded(CommunityAuthRespondedPayload {
        server_id: id.to_string(),
        requested_server_id: id.to_string(),
        host: host.into(),
        port: 8082,
        success: true,
        message: String::new(),
        channels,
        error_code: String::new(),
        server_name: name.into(),
        server_description: description.into(),
        owner_username: owner.0.into(),
        owner_uid: owner.1,
        attachment_port: 8085,
        max_attachment_bytes: 100 << 20,
        public_listing: true,
    })
}

fn member(name: &str, uid: i64, nickname: &str, online: bool, roles: &[i64], owner: bool) -> MemberInfoPayload {
    MemberInfoPayload {
        username: name.into(),
        uid,
        joined_at: 1_760_000_000,
        nickname: nickname.into(),
        is_owner: owner,
        is_online: online,
        role_ids: roles.to_vec(),
        timed_out_until: 0,
    }
}

fn role(id: i64, name: &str, color: u32, position: i32, permissions: u64, members: i64) -> RoleInfoPayload {
    RoleInfoPayload {
        id,
        name: name.into(),
        color,
        position,
        permissions,
        is_default: id == 1,
        manage_each_other: false,
        member_count: members,
    }
}

fn voice_state(name: &str, uid: i64, muted: bool, deafened: bool, server_muted: bool) -> VoiceUserStatePayload {
    VoiceUserStatePayload {
        username: name.into(),
        uid,
        is_muted: muted,
        is_deafened: deafened,
        is_server_muted: server_muted,
        is_server_deafened: false,
        is_suppressed: false,
    }
}

fn voice(channel: &str, people: &[(&str, i64, bool, bool, bool)]) -> CoreEvent {
    CoreEvent::VoicePresenceUpdated(VoicePresenceUpdatedPayload {
        server_id: DAILY_GAMING.to_string(),
        channel_id: channel.into(),
        participants: people.iter().map(|p| p.0.to_string()).collect(),
        participant_uids: people.iter().map(|p| p.1).collect(),
        user_states: people.iter().map(|p| voice_state(p.0, p.1, p.2, p.3, p.4)).collect(),
        user_capabilities: Vec::new(),
    })
}

fn presence(name: &str, uid: i64, status: &str, text: &str) -> UserPresencePayload {
    UserPresencePayload {
        username: name.into(),
        uid,
        avatar_version: String::new(),
        status: status.into(),
        status_text: text.into(),
        status_emoji: String::new(),
    }
}

fn dm_preview(peer: &str, uid: i64, text: &str, sender: (&str, i64), at: i64, unread: i64) -> DmConversationPreviewPayload {
    DmConversationPreviewPayload {
        peer: peer.into(),
        peer_uid: uid,
        last_message_content: text.into(),
        last_message_sender: sender.0.into(),
        last_message_sender_uid: sender.1,
        last_message_id: at,
        last_timestamp: at,
        unread_count: unread,
        encrypted: false,
        decrypt_error: String::new(),
    }
}

fn friend(name: &str, uid: i64, status: &str) -> FriendInfoPayload {
    FriendInfoPayload { username: name.into(), uid, status: status.into(), avatar_version: String::new() }
}

/// Every event of the fixture, in the order the core would deliver them.
pub fn events() -> Vec<CoreEvent> {
    let mut events = vec![
        CoreEvent::LoginSucceeded(LoginSucceededPayload { username: ME_NAME.into(), uid: ME, renames: true, devices: true }),
        CoreEvent::OwnPresence(OwnPresencePayload {
            supported: true,
            status: "online".into(),
            status_text: String::new(),
            status_emoji: String::new(),
            status_clear_in: 0.,
            dm_friends_only: false,
            account_idle: false,
        }),
        CoreEvent::MembershipsReceived(MembershipsReceivedPayload {
            memberships: vec![
                server_info(1, "Daily Gaming", "Games every night", "127.0.0.11", 298, ""),
                server_info(2, "Rustaceans", "Systems programming", "127.0.0.12", 29, ""),
                server_info(3, "Design Lab", "Pixels and type", "127.0.0.13", 42, PICTURE_VERSION),
            ],
        }),
    ];

    let categories = |name: &str| channel(&format!("cat-{}", name.to_lowercase().replace(' ', "-")), name, "category", READ_ONLY, "", false);
    events.push(auth(
        1,
        "Daily Gaming",
        "Games every night",
        "127.0.0.11",
        ("alice", 1),
        vec![
            categories("Information"),
            channel("welcome", "welcome", "text", READ_ONLY, "", false),
            channel("announcements", "announcements", "text", READ_ONLY, "News from the mods", false),
            categories("Text Channels"),
            channel("general", "general", "text", MEMBER, "Anything goes — be nice", false),
            channel("clips", "clips", "text", MEMBER, "", false),
            channel("off-topic", "off-topic", "text", MEMBER, "Everything else", false),
            channel("slow-lane", "slow-lane", "text", MEMBER, "", false),
            channel("vault", "vault", "text", MEMBER, "", true),
            categories("Voice Channels"),
            channel("lounge", "Lounge", "voice", MEMBER, "", false),
            channel("squad", "Squad", "voice", MEMBER, "", false),
            channel("afk", "AFK", "voice", MEMBER, "", false),
        ],
    ));
    events.push(auth(2, "Rustaceans", "Systems programming", "127.0.0.12", ("bob", 2), vec![
        channel("general", "general", "text", MEMBER, "", false),
    ]));
    events.push(auth(3, "Design Lab", "Pixels and type", "127.0.0.13", ("carol", 3), vec![
        channel("general", "general", "text", MEMBER, "", false),
    ]));
    events.push(CoreEvent::ServerPictureReceived(ServerPictureReceivedPayload {
        server_id: 3,
        version: PICTURE_VERSION.into(),
        data: picture_data_url(),
    }));

    events.push(CoreEvent::RoleListReceived(RoleListReceivedPayload {
        server_id: DAILY_GAMING.to_string(),
        success: true,
        message: String::new(),
        roles: vec![
            role(2, "Admin", 0xe74c3c, 3, 1, 1),
            role(3, "Moderator", 0x3498db, 2, 64 | 16 | 131072 | 1048576, 4),
            role(4, "Member", 0x2ecc71, 1, 0, 204),
            role(1, "everyone", 0, 0, MEMBER, 298),
        ],
    }));

    // The roster's first page: the online part complete, the offline part begun.
    let mut members = vec![
        member("alice", 1, "", true, &[], true),
        member("bob", 2, "Bobby", true, &[2], false),
        member("carol", 3, "", true, &[3], false),
        member("dave", 4, "", true, &[4], false),
        member("perf1", ME, "", true, &[4], false),
        member("perf2", 7, "", true, &[4], false),
        member("erin", 5, "", false, &[4], false),
        member("perf3", 8, "", false, &[4], false),
        member("perf4", 9, "", false, &[4], false),
    ];
    const ONLINE: [&str; 14] = [
        "aurora", "blaze", "cinder", "drift", "ember", "flux", "glimmer", "halo", "iris", "jolt", "kestrel", "lumen",
        "mosaic", "nova",
    ];
    const OFFLINE: [&str; 12] =
        ["onyx", "pixel", "quartz", "raven", "sable", "talon", "umbra", "vesper", "willow", "xenon", "yarrow", "zephyr"];
    for (i, name) in ONLINE.iter().enumerate() {
        members.push(member(name, -1000 - i as i64, "", true, &[4], false));
    }
    for (i, name) in OFFLINE.iter().enumerate() {
        members.push(member(name, -2000 - i as i64, "", false, &[4], false));
    }
    let online_count = members.iter().filter(|m| m.is_online).count() as i64;
    events.push(CoreEvent::MemberListReceived(MemberListReceivedPayload {
        server_id: DAILY_GAMING.to_string(),
        success: true,
        message: String::new(),
        members,
        revision: 1,
        total_members: 298,
        has_more: true,
        next_after: "zephyr".into(),
        first_page: true,
        online_count,
        online_complete: true,
        extra: Vec::new(),
        lookup_uids: Vec::new(),
    }));

    // Voice: three people in Lounge (one streaming), us and perf2 in Squad.
    events.push(voice("lounge", &[
        ("alice", 1, false, false, false),
        ("bob", 2, true, false, false),
        ("carol", 3, true, true, false),
        ("dave", 4, false, false, true),
    ]));
    events.push(voice("squad", &[("perf2", 7, false, false, false), (ME_NAME, ME, false, false, false)]));
    events.push(CoreEvent::StreamPresenceUpdated(StreamPresenceUpdatedPayload {
        server_id: DAILY_GAMING.to_string(),
        channel_id: "lounge".into(),
        streams: vec![StreamInfoPayload {
            stream_id: "3_screen".into(),
            owner_username: "carol".into(),
            owner_uid: 3,
            has_audio: true,
            resolution_width: 1920,
            resolution_height: 1080,
            fps: 60,
            current_codec: 1,
            enforced_codec: 0,
            watcher_count: 2,
        }],
    }));

    // Central's statuses for the people on screen.
    events.push(CoreEvent::UserListUpdated(UserListUpdatedPayload {
        users: vec![
            presence("alice", 1, "online", ""),
            presence("bob", 2, "dnd", ""),
            presence("carol", 3, "away", "Designing things"),
            presence("dave", 4, "online", "Back at 9"),
            presence("perf2", 7, "online", ""),
        ],
        covered: vec![1, 2, 3, 4, 5, 7, 8, 9],
        avatars: Vec::new(),
    }));
    events.push(CoreEvent::DmConversationsReceived(DmConversationsReceivedPayload {
        conversations: vec![
            dm_preview("perf2", 7, "see you in Squad tonight?", ("perf2", 7), 1_788_000_600, 2),
            dm_preview("alice", 1, "welcome to Daily Gaming!", ("alice", 1), 1_787_000_000, 0),
        ],
    }));
    events.push(CoreEvent::FriendListReceived(FriendListReceivedPayload {
        friends: vec![
            friend("perf2", 7, "online"),
            friend("alice", 1, "online"),
            friend("perf3", 8, "offline"),
            friend("perf4", 9, "pending_incoming"),
            friend("erin", 5, "pending_outgoing"),
        ],
    }));
    events
}

/// What follows the events in [`Scene::Server`]: we joined Squad (our own join, not an event),
/// the engine reported our state and a ping, perf2 talks.
fn voice_session_events() -> Vec<CoreEvent> {
    vec![
        CoreEvent::VoiceStateChanged(VoiceStateChangedPayload { is_muted: false, is_deafened: false }),
        CoreEvent::VoicePingUpdated(VoicePingUpdatedPayload { latency_ms: 38 }),
        CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid: 7, speaking: true }),
    ]
}

/// Fill the model (made without a core if there is none) with the fixture and show `scene`.
pub fn apply(scene: Scene, cx: &mut App) -> Model {
    let model = match cx.try_global::<Model>() {
        Some(model) => model.clone(),
        None => Model::init(cx, None),
    };
    // The crash-report notice was dismissed long ago (as on any machine that signed in before).
    model.settings.update(cx, |s, cx| {
        let mut settings = s.get().clone();
        settings.crash_reporting_consent_shown = true;
        s.apply_loaded(settings, cx);
    });
    model.auth.update(cx, |a, cx| a.set_booted(Some(ME_NAME.into()), cx));
    for event in events() {
        decibell_model::events::route(&model, event, cx);
    }
    let server = ServerId::from_central(DAILY_GAMING);
    match scene {
        Scene::Home => actions::servers::go_home(cx),
        Scene::Server => {
            actions::servers::open_server(server, cx);
            model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(ChannelId::from("general")), cx));
            let squad: VoiceKey = (server, ChannelId::from("squad"));
            actions::voice::enter_channel(&model, squad, false, false, cx);
            for event in voice_session_events() {
                decibell_model::events::route(&model, event, cx);
            }
        }
    }
    model
}

/// The fixture as the Electron pages replay it: `{ scene, me, events: [{ event, payload }],
/// nav: { view, server, channel, voice: { server, channel } } }`.
#[cfg(feature = "fixtures")]
pub fn json(scene: Scene) -> serde_json::Value {
    let mut list: Vec<serde_json::Value> = events()
        .iter()
        .map(|e| {
            let (name, payload) = e.to_json();
            serde_json::json!({ "event": name, "payload": payload })
        })
        .collect();
    if scene == Scene::Server {
        list.extend(voice_session_events().iter().map(|e| {
            let (name, payload) = e.to_json();
            serde_json::json!({ "event": name, "payload": payload })
        }));
    }
    let nav = match scene {
        Scene::Home => serde_json::json!({ "view": "home", "server": null, "channel": null, "voice": null }),
        Scene::Server => serde_json::json!({
            "view": "server",
            "server": DAILY_GAMING.to_string(),
            "channel": "general",
            "voice": { "server": DAILY_GAMING.to_string(), "channel": "squad" },
        }),
    };
    let _ = ActiveView::Home;
    serde_json::json!({ "scene": format!("{scene:?}").to_lowercase(), "me": { "uid": ME, "username": ME_NAME }, "events": list, "nav": nav })
}

const PICTURE_VERSION: &str = "fixture-design-lab-v1";

/// Design Lab's picture: a 128 × 44 diagonal gradient, as a PNG `data:` URL (stored, not
/// compressed: the encoder is ten lines and the bytes are deterministic).
pub fn picture_data_url() -> String {
    use base64::Engine as _;
    let (w, h) = (128u32, 44u32);
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in 0..h {
        raw.push(0); // filter: none
        for x in 0..w {
            let t = (x as f32 / (w - 1) as f32 + y as f32 / (h - 1) as f32) / 2.;
            let lerp = |a: f32, b: f32| (a + (b - a) * t).round() as u8;
            raw.extend_from_slice(&[lerp(232., 74.), lerp(93., 64.), lerp(117., 190.)]);
        }
    }
    let png = png_rgb(w, h, &raw);
    format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png))
}

fn png_rgb(w: u32, h: u32, raw: &[u8]) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for b in bytes {
            crc ^= *b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
    }
    // zlib with stored deflate blocks of at most 65535 bytes.
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65_535).peekable();
    while let Some(block) = chunks.next() {
        z.push(if chunks.peek().is_none() { 1 } else { 0 });
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in raw {
        a = (a + *byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    /// The fixture goes through the real router and leaves the stores as the screens draw them.
    #[gpui::test]
    fn the_fixture_fills_the_stores_through_the_router(cx: &mut TestAppContext) {
        let model = cx.update(|cx| apply(Scene::Server, cx));
        cx.run_until_parked();
        cx.update(|cx| {
            let server = ServerId::from_central(DAILY_GAMING);
            assert!(model.auth.read(cx).is_authenticated);
            assert_eq!(model.servers.read(cx).servers().len(), 3);
            assert!(model.connections.read(cx).is_connected(server));
            let community = model.community(server, cx).unwrap();
            assert_eq!(community.channels.read(cx).list().len(), 13);
            let roster = community.roster.read(cx);
            assert_eq!(roster.meta().unwrap().total_members, 298);
            let visible = roster.visible();
            assert_eq!((visible.online.len(), visible.offline.len()), (20, 15));
            let squad = (server, ChannelId::from("squad"));
            assert!(model.live.voice.session.read(cx).is_connected_to(server, "squad"));
            assert_eq!(model.live.voice.rosters.read(cx).roster(&squad), &[7, ME]);
            assert!(model.live.voice.speaking.read(cx).is_speaking(7));
            assert_eq!(model.nav.read(cx).active_channel.as_deref(), Some("general"));
            assert_eq!(model.dm.list.read(cx).total_unread(), 2);
            assert_eq!(model.live.friends.read(cx).list().len(), 5);
        });
    }

    #[test]
    fn the_picture_is_a_png_data_url() {
        let url = picture_data_url();
        assert!(url.starts_with("data:image/png;base64,iVBORw0KGgo"));
        assert_eq!(url, picture_data_url(), "deterministic");
    }

    #[test]
    fn the_json_export_names_every_event_as_the_addon_would() {
        let json = json(Scene::Server);
        let events = json["events"].as_array().unwrap();
        assert_eq!(events[0]["event"], "login_succeeded");
        assert_eq!(events[0]["payload"]["username"], ME_NAME);
        assert!(events.iter().any(|e| e["event"] == "voice_user_speaking"));
        assert_eq!(json["nav"]["channel"], "general");
    }
}

//! The people fixture: the shell's (`perf1` signed in to the local stack's three communities,
//! `decibell_shell::fixture`) plus friends in every state, statuses with custom messages, DM
//! conversations with their newest pages (plaintext with bob, sealed with alice), and per scene
//! the E2EE state, the peer answers, the navigation and the overlay that is open. Everything goes
//! through the model's real router or its stores' own methods, so the screens exercise the code
//! live data takes.
//!
//! [`json`] exports the same for the Electron capture pages (`tools/electron-captures/pages/
//! home-*.tsx` through `_home.tsx`): the core events as the addon delivers them, then what the
//! page sets in the renderer's stores (E2EE status and changed peers, the peer answers
//! `e2ee_peer_info` gives, the navigation, the overlay). `decibell-home-fixture <dir>` writes one
//! file per scene.

use decibell_core::events::*;
use decibell_model::live::e2ee::E2eeStatus;
use decibell_model::stores::overlays::{Anchor, modal};
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_shell::fixture::{self as shell, ME, ME_NAME};
use gpui::App;

use crate::e2ee::{PeerInfo, PeerInfos};
use crate::friends::Filter;
use crate::menu::ModKind;
use crate::profile::ProfileTab;

pub const DAILY_GAMING: i32 = shell::DAILY_GAMING;
pub const ALICE: Uid = 1;
pub const BOB: Uid = 2;
pub const CAROL: Uid = 3;
pub const DAVE: Uid = 4;
pub const MALLORY: Uid = 31;
pub const TRENT: Uid = 32;

/// Message ids: above the shell fixture's previews (ids there are their timestamps), so the pages
/// here are the conversations' newest rows.
const BOB_IDS: i64 = 1_787_400_000;
const ALICE_IDS: i64 = 1_787_400_100;

/// 2026-08-20 09:00 UTC: far enough back that every clock and day divider is a full date.
const T0: i64 = 1_787_216_400;

/// What a screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scene {
    FriendsOnline,
    FriendsAll,
    FriendsPending,
    FriendsBlocked,
    FriendsAdd,
    DmPlain,
    DmEncrypted,
    DmLocked,
    DmKeyChanged,
    DmWithRail,
    ProfilePopup,
    ProfileModal,
    ProfilePrivacy,
    CustomStatus,
    UserMenuOwn,
    UserMenuMember,
    UserMenuModerator,
    NicknameModal,
    BanModal,
}

impl Scene {
    pub const ALL: [Scene; 19] = [
        Scene::FriendsOnline,
        Scene::FriendsAll,
        Scene::FriendsPending,
        Scene::FriendsBlocked,
        Scene::FriendsAdd,
        Scene::DmPlain,
        Scene::DmEncrypted,
        Scene::DmLocked,
        Scene::DmKeyChanged,
        Scene::DmWithRail,
        Scene::ProfilePopup,
        Scene::ProfileModal,
        Scene::ProfilePrivacy,
        Scene::CustomStatus,
        Scene::UserMenuOwn,
        Scene::UserMenuMember,
        Scene::UserMenuModerator,
        Scene::NicknameModal,
        Scene::BanModal,
    ];

    /// The screen's name (`home-friends-online`, `dm-plain`, …).
    pub fn name(self) -> &'static str {
        match self {
            Scene::FriendsOnline => "home-friends-online",
            Scene::FriendsAll => "home-friends-all",
            Scene::FriendsPending => "home-friends-pending",
            Scene::FriendsBlocked => "home-friends-blocked",
            Scene::FriendsAdd => "home-friends-add",
            Scene::DmPlain => "dm-plain",
            Scene::DmEncrypted => "dm-encrypted",
            Scene::DmLocked => "dm-locked",
            Scene::DmKeyChanged => "dm-key-changed",
            Scene::DmWithRail => "dm-with-friends-rail",
            Scene::ProfilePopup => "profile-popup",
            Scene::ProfileModal => "profile-modal",
            Scene::ProfilePrivacy => "profile-privacy",
            Scene::CustomStatus => "custom-status",
            Scene::UserMenuOwn => "user-menu-own",
            Scene::UserMenuMember => "user-menu-member",
            Scene::UserMenuModerator => "user-menu-moderator",
            Scene::NicknameModal => "nickname-modal",
            Scene::BanModal => "ban-modal",
        }
    }

    pub fn about(self) -> &'static str {
        match self {
            Scene::FriendsOnline => "Home: the Friends page on Online (fixture)",
            Scene::FriendsAll => "Home: the Friends page on All, every section",
            Scene::FriendsPending => "Home: the Friends page on Pending (incoming and outgoing requests)",
            Scene::FriendsBlocked => "Home: the Friends page on Blocked",
            Scene::FriendsAdd => "Home: the Friends page with the Add Friend form open",
            Scene::DmPlain => "A plaintext conversation with bob (he has no keys), rail off",
            Scene::DmEncrypted => "A sealed conversation with alice, rail off",
            Scene::DmLocked => "The conversation with alice while this device is locked (Unlock)",
            Scene::DmKeyChanged => "The conversation with alice after her keys changed",
            Scene::DmWithRail => "The conversation with bob beside the Friends rail",
            Scene::ProfilePopup => "carol's profile popout from Daily Gaming's member list (roles, live stream)",
            Scene::ProfileModal => "bob's profile screen from Daily Gaming (Profile tab)",
            Scene::ProfilePrivacy => "alice's profile screen on Privacy (safety number, fingerprint)",
            Scene::CustomStatus => "Set a custom status, from our saved message",
            Scene::UserMenuOwn => "The user menu on ourselves in Daily Gaming",
            Scene::UserMenuMember => "The user menu on dave in Daily Gaming, as a plain member",
            Scene::UserMenuModerator => "The user menu on dave in Daily Gaming, as an admin (moderation entries)",
            Scene::NicknameModal => "The nickname dialog for dave in Daily Gaming",
            Scene::BanModal => "The ban dialog for dave in Daily Gaming",
        }
    }

    fn is_dm(self) -> bool {
        matches!(self, Scene::DmPlain | Scene::DmEncrypted | Scene::DmLocked | Scene::DmKeyChanged | Scene::DmWithRail)
    }

    fn uses_server(self) -> bool {
        matches!(
            self,
            Scene::ProfilePopup | Scene::ProfileModal | Scene::UserMenuOwn | Scene::UserMenuMember | Scene::UserMenuModerator | Scene::NicknameModal | Scene::BanModal
        )
    }
}

fn friend(name: &str, uid: Uid, status: &str) -> FriendInfoPayload {
    FriendInfoPayload { username: name.into(), uid, status: status.into(), avatar_version: String::new() }
}

fn presence(name: &str, uid: Uid, status: &str, text: &str, emoji: &str) -> UserPresencePayload {
    UserPresencePayload {
        username: name.into(),
        uid,
        avatar_version: String::new(),
        status: status.into(),
        status_text: text.into(),
        status_emoji: emoji.into(),
    }
}

#[allow(clippy::too_many_arguments)] // the payload's fields, flat
fn dm(id: i64, sender: (&str, Uid), content: &str, at: i64, edited: i64, reply: Option<(i64, &str, Uid, &str)>, encrypted: bool, error: &str) -> DmHistoryMessagePayload {
    let (reply_to, reply_sender, reply_uid, reply_content) = reply.unwrap_or((0, "", 0, ""));
    DmHistoryMessagePayload {
        id,
        sender: sender.0.into(),
        sender_uid: sender.1,
        content: content.into(),
        timestamp: at,
        edited_at: edited,
        reply_to,
        reply_to_sender: reply_sender.into(),
        reply_to_sender_uid: reply_uid,
        reply_to_content: reply_content.into(),
        encrypted,
        decrypt_error: error.into(),
    }
}

/// bob ↔ perf1: a plaintext evening with a reply and an edit.
fn bob_page() -> Vec<DmHistoryMessagePayload> {
    let me = (ME_NAME, ME);
    let bob = ("bob", BOB);
    let at = |minutes: i64| T0 + minutes * 60;
    vec![
        dm(BOB_IDS + 1, bob, "hey, are you around tonight?", at(0), 0, None, false, ""),
        dm(BOB_IDS + 2, me, "yes! after 8", at(2), 0, None, false, ""),
        dm(BOB_IDS + 3, bob, "the build on main is red again", at(3), 0, None, false, ""),
        dm(BOB_IDS + 4, bob, "something in the **voice** tests, `cargo test --lib` fails on my box", at(3), 0, None, false, ""),
        dm(BOB_IDS + 5, me, "I saw it too. The jitter buffer test is flaky on loaded machines", at(6), 0, None, false, ""),
        dm(BOB_IDS + 6, bob, "want me to bump the tolerance?", at(7), 0, None, false, ""),
        dm(BOB_IDS + 7, me, "no, I'd rather fix the clock it reads", at(9), at(10), Some((BOB_IDS + 6, "bob", BOB, "want me to bump the tolerance?")), false, ""),
        dm(BOB_IDS + 8, bob, "fair. I'll push the fix tonight then", at(14), 0, None, false, ""),
        dm(BOB_IDS + 9, bob, "and I'll ping you in Squad 🎮", at(15), 0, None, false, ""),
    ]
}

/// alice ↔ perf1: sealed messages (the core opened them; `error` when this device can't).
fn alice_page(error: &str) -> Vec<DmHistoryMessagePayload> {
    let me = (ME_NAME, ME);
    let alice = ("alice", ALICE);
    let at = |minutes: i64| T0 + 86_400 + minutes * 60;
    let body = |text: &'static str| if error.is_empty() { text } else { "" };
    vec![
        dm(ALICE_IDS + 1, alice, body("welcome to Daily Gaming! I set up encryption for us"), at(0), 0, None, true, error),
        dm(ALICE_IDS + 2, me, body("nice, the chip in the header says Encrypted"), at(4), 0, None, true, error),
        dm(ALICE_IDS + 3, alice, body("compare the safety number in my profile when you can"), at(5), 0, None, true, error),
        dm(ALICE_IDS + 4, me, body("will do, it's under Privacy"), at(30), 0, None, true, error),
    ]
}

fn history(peer: (&str, Uid), messages: Vec<DmHistoryMessagePayload>) -> CoreEvent {
    CoreEvent::DmHistoryReceived(DmHistoryReceivedPayload {
        peer: peer.0.into(),
        peer_uid: peer.1,
        messages,
        has_more: false,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 0,
    })
}

fn status(status: &str) -> CoreEvent {
    CoreEvent::E2eeStatusChanged(E2eeStatusPayload {
        supported: true,
        status: status.into(),
        key_id: 1,
        fingerprint: if status == "ready" { "MZXW 6YTB OI6T EMZX GQ3D 2NBQ HE4D CMRT GQ2T MNZW 6YLP".into() } else { String::new() },
    })
}

/// The conversations' newest pages.
fn histories(scene: Scene) -> Vec<CoreEvent> {
    vec![history(("bob", BOB), bob_page()), history(("alice", ALICE), alice_page(if scene == Scene::DmLocked { "locked" } else { "" }))]
}

/// The core events of `scene`, after the shell's.
pub fn events(scene: Scene) -> Vec<CoreEvent> {
    let mut events = vec![
        CoreEvent::FriendListReceived(FriendListReceivedPayload {
            friends: vec![
                friend("alice", ALICE, "online"),
                friend("bob", BOB, "online"),
                friend("carol", CAROL, "online"),
                friend("perf2", 7, "online"),
                friend("dave", DAVE, "offline"),
                friend("perf3", 8, "offline"),
                friend("perf4", 9, "pending_incoming"),
                friend("trent", TRENT, "pending_incoming"),
                friend("erin", 5, "pending_outgoing"),
                friend("mallory", MALLORY, "blocked"),
            ],
        }),
        CoreEvent::UserListUpdated(UserListUpdatedPayload {
            users: vec![
                presence("alice", ALICE, "online", "", ""),
                presence("bob", BOB, "dnd", "Shipping the release", "🚢"),
                presence("carol", CAROL, "away", "Designing things", "🎨"),
                presence("perf2", 7, "online", "", ""),
            ],
            covered: vec![ALICE, BOB, CAROL, DAVE, 5, 7, 8, 9, TRENT, MALLORY],
            avatars: Vec::new(),
        }),
        CoreEvent::DmConversationsReceived(DmConversationsReceivedPayload {
            conversations: vec![
                DmConversationPreviewPayload {
                    peer: "bob".into(),
                    peer_uid: BOB,
                    last_message_content: "and I'll ping you in Squad 🎮".into(),
                    last_message_sender: "bob".into(),
                    last_message_sender_uid: BOB,
                    last_message_id: BOB_IDS + 9,
                    last_timestamp: T0 + 15 * 60,
                    unread_count: 0,
                    encrypted: false,
                    decrypt_error: String::new(),
                },
                DmConversationPreviewPayload {
                    peer: "alice".into(),
                    peer_uid: ALICE,
                    last_message_content: if scene == Scene::DmLocked { String::new() } else { "will do, it's under Privacy".into() },
                    last_message_sender: ME_NAME.into(),
                    last_message_sender_uid: ME,
                    last_message_id: ALICE_IDS + 4,
                    last_timestamp: T0 + 86_400 + 30 * 60,
                    unread_count: 0,
                    encrypted: true,
                    decrypt_error: if scene == Scene::DmLocked { "locked".into() } else { String::new() },
                },
            ],
        }),
    ];
    let e2ee = match scene {
        Scene::DmLocked => "locked",
        _ => "ready",
    };
    events.push(status(e2ee));
    if scene.is_dm() {
        events.extend(histories(scene));
    }
    if scene == Scene::UserMenuModerator {
        // perf1 holds Admin in Daily Gaming for this scene.
        events.push(CoreEvent::MemberUpsert(MemberUpsertPayload {
            server_id: DAILY_GAMING.to_string(),
            member: MemberInfoPayload {
                username: ME_NAME.into(),
                uid: ME,
                joined_at: 1_760_000_000,
                nickname: String::new(),
                is_owner: false,
                is_online: true,
                role_ids: vec![2],
                timed_out_until: 0,
            },
            revision: 2,
            online_count: 20,
            total_members: 298,
        }));
    }
    if scene == Scene::CustomStatus {
        events.push(CoreEvent::OwnPresence(OwnPresencePayload {
            supported: true,
            status: "online".into(),
            status_text: "Playing late tonight".into(),
            status_emoji: "🎮".into(),
            status_clear_in: 4. * 3600.,
            dm_friends_only: false,
            account_idle: false,
        }));
    }
    events
}

/// The peer answers `e2ee_peer_info` gives in `scene`.
pub fn peer_infos(scene: Scene) -> Vec<(Uid, PeerInfo)> {
    let alice = PeerInfo {
        has_keys: true,
        fingerprint: "ONSW G4TF OQQG C3LP OVXH IZLE EBQW 4ZBA MJ2W 4ZDM".into(),
        safety_number: "4QTB 2MXK 7HJA P3LD 9VWE R6NS KZ5C 8GTY FJ2B MP7X WQ4D 3HRA".into(),
        changed_at: if scene == Scene::DmKeyChanged { T0 + 2 * 86_400 } else { 0 },
        pinned: true,
    };
    let bob = PeerInfo { has_keys: false, fingerprint: "".into(), safety_number: "".into(), changed_at: 0, pinned: false };
    vec![(ALICE, alice), (BOB, bob)]
}

/// What the Friends page starts on in `scene`.
pub fn friends_filter(scene: Scene) -> (Filter, bool) {
    match scene {
        Scene::FriendsOnline => (Filter::Online, false),
        Scene::FriendsPending => (Filter::Pending, false),
        Scene::FriendsBlocked => (Filter::Blocked, false),
        Scene::FriendsAdd => (Filter::All, true),
        _ => (Filter::All, false),
    }
}

/// Where a scene's menu / popup opens (window coordinates; the member list of a 1280 × 800
/// window: the popout beside the row, a right click on it).
pub const POPUP_ANCHOR: Anchor = Anchor { x: 1011., y: 160. };
pub const MENU_ANCHOR: Anchor = Anchor { x: 1100., y: 200. };

/// Fill the model with `scene` and show it.
pub fn apply(scene: Scene, cx: &mut App) -> Model {
    let model = shell::apply(shell::Scene::Home, cx);
    let mut later = Vec::new();
    for event in events(scene) {
        // The pages go after the status's own effects ran: becoming "ready" reloads every
        // conversation (`E2eeEvent::BecameReady` → `reload_after_unlock`), as a sign-in does
        // before its pages come.
        if matches!(event, CoreEvent::DmHistoryReceived(_)) {
            later.push(event);
        } else {
            decibell_model::events::route(&model, event, cx);
        }
    }
    if !later.is_empty() {
        let model = model.clone();
        cx.defer(move |cx| {
            for event in later {
                decibell_model::events::route(&model, event, cx);
            }
        });
    }
    let store = PeerInfos::entity(cx);
    for (uid, info) in peer_infos(scene) {
        let key = (model.live.e2ee.read(cx).status, if scene == Scene::DmKeyChanged && uid == ALICE { 1 } else { 0 });
        store.update(cx, |s, cx| s.seed(uid, key, info, cx));
    }
    if scene == Scene::DmKeyChanged {
        // The marker `e2ee_peer_changed` sets (without its toast, which would time out).
        model.live.e2ee.update(cx, |e, cx| e.mark_peer_changed(ALICE, 1, cx));
    }
    // The passphrase prompt the first "locked" opens is the settings agent's overlay.
    model.live.e2ee.update(cx, |e, cx| e.cancel(cx));
    let (filter, add) = friends_filter(scene);
    crate::friends::set_initial(filter, add, cx);
    // The rail is on by default (`dm_friends_panel`); the DM scenes show it only where named.
    let rail = scene == Scene::DmWithRail;
    if model.layout.read(cx).prefs().dm_friends_panel != rail {
        model.layout.update(cx, |l, cx| l.toggle_dm_friends_panel(cx));
    }
    let server = ServerId::from_central(DAILY_GAMING);
    match scene {
        Scene::DmPlain | Scene::DmWithRail => actions::dm::open_dm(BOB, "bob".into(), cx),
        Scene::DmEncrypted | Scene::DmLocked | Scene::DmKeyChanged => actions::dm::open_dm(ALICE, "alice".into(), cx),
        _ => {}
    }
    if scene.uses_server() {
        actions::servers::open_server(server, cx);
        model.nav.update(cx, |nav, cx| nav.set_active_channel(Some("general".into()), cx));
    }
    match scene {
        Scene::ProfilePopup => model.overlays.update(cx, |o, cx| o.open_profile_popup(CAROL, POPUP_ANCHOR, Some(server), cx)),
        Scene::ProfileModal => crate::profile::open_profile(BOB, Some(server), ProfileTab::Profile, cx),
        Scene::ProfilePrivacy => crate::profile::open_profile(ALICE, None, ProfileTab::Privacy, cx),
        Scene::CustomStatus => model.overlays.update(cx, |o, cx| o.open_modal(modal::CUSTOM_STATUS, cx)),
        Scene::UserMenuOwn => model.overlays.update(cx, |o, cx| o.open_context_menu(ME, MENU_ANCHOR, Some(server), cx)),
        Scene::UserMenuMember | Scene::UserMenuModerator => model.overlays.update(cx, |o, cx| o.open_context_menu(DAVE, MENU_ANCHOR, Some(server), cx)),
        Scene::NicknameModal => crate::menu::request_dialog(crate::menu::DialogRequest::Nickname { server, uid: DAVE }, cx),
        Scene::BanModal => crate::menu::request_dialog(crate::menu::DialogRequest::Moderation { kind: ModKind::Ban, server, uid: DAVE }, cx),
        _ => {}
    }
    let _ = E2eeStatus::Ready;
    model
}

/// A window point as the Electron page sees it: its page is the GPUI window's content area,
/// which starts 10 px in (the client-side frame's margin) and under the 32 px title bar.
fn page_point(a: Anchor) -> serde_json::Value {
    serde_json::json!({ "x": a.x - 10., "y": a.y - 42. })
}

/// The scene as the Electron page replays it.
pub fn json(scene: Scene) -> serde_json::Value {
    use serde_json::json;
    let shell = shell::json(shell::Scene::Home);
    let mut list: Vec<serde_json::Value> = shell["events"].as_array().cloned().unwrap_or_default();
    list.extend(events(scene).iter().map(|e| {
        let (name, payload) = e.to_json();
        json!({ "event": name, "payload": payload })
    }));
    let infos: serde_json::Map<String, serde_json::Value> = peer_infos(scene)
        .into_iter()
        .map(|(uid, i)| {
            (
                uid.to_string(),
                json!({ "hasKeys": i.has_keys, "keyId": 1, "fingerprint": i.fingerprint.as_ref(), "safetyNumber": i.safety_number.as_ref(), "changedAt": i.changed_at, "pinned": i.pinned }),
            )
        })
        .collect();
    let (filter, add) = friends_filter(scene);
    let dm_peer = match scene {
        Scene::DmPlain | Scene::DmWithRail => Some(BOB),
        Scene::DmEncrypted | Scene::DmLocked | Scene::DmKeyChanged => Some(ALICE),
        _ => None,
    };
    let overlay = match scene {
        Scene::ProfilePopup => json!({ "kind": "profile-popup", "uid": CAROL, "server": DAILY_GAMING.to_string(), "anchor": page_point(POPUP_ANCHOR) }),
        Scene::ProfileModal => json!({ "kind": "profile", "uid": BOB, "server": DAILY_GAMING.to_string(), "tab": "profile" }),
        Scene::ProfilePrivacy => json!({ "kind": "profile", "uid": ALICE, "server": null, "tab": "privacy" }),
        Scene::CustomStatus => json!({ "kind": "custom-status" }),
        Scene::UserMenuOwn => json!({ "kind": "user-menu", "uid": ME, "server": DAILY_GAMING.to_string(), "anchor": page_point(MENU_ANCHOR) }),
        Scene::UserMenuMember | Scene::UserMenuModerator => json!({ "kind": "user-menu", "uid": DAVE, "server": DAILY_GAMING.to_string(), "anchor": page_point(MENU_ANCHOR) }),
        Scene::NicknameModal => json!({ "kind": "nickname", "uid": DAVE, "server": DAILY_GAMING.to_string() }),
        Scene::BanModal => json!({ "kind": "ban", "uid": DAVE, "server": DAILY_GAMING.to_string() }),
        _ => serde_json::Value::Null,
    };
    let view = if dm_peer.is_some() {
        "dm"
    } else if scene.uses_server() {
        "server"
    } else {
        "home"
    };
    json!({
        "scene": scene.name(),
        "me": { "uid": ME, "username": ME_NAME },
        "events": list,
        "nav": { "view": view, "server": scene.uses_server().then(|| DAILY_GAMING.to_string()), "channel": scene.uses_server().then_some("general"), "dm": dm_peer },
        "friends": { "filter": format!("{filter:?}").to_lowercase(), "addOpen": add },
        "rail": scene == Scene::DmWithRail,
        "e2ee": { "changedPeers": if scene == Scene::DmKeyChanged { json!({ ALICE.to_string(): T0 + 2 * 86_400 }) } else { json!({}) } },
        "peerInfo": infos,
        "overlay": overlay,
    })
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    #[gpui::test]
    fn every_scene_fills_the_stores(cx: &mut TestAppContext) {
        for scene in Scene::ALL {
            let model = cx.update(|cx| {
                crate::init(cx);
                apply(scene, cx)
            });
            cx.run_until_parked();
            cx.update(|cx| {
                assert!(model.auth.read(cx).is_authenticated, "{scene:?}");
                assert_eq!(model.live.friends.read(cx).list().len(), 10, "{scene:?}");
                if scene.is_dm() {
                    let peer = model.dm.list.read(cx).active_peer.expect("a conversation");
                    assert!(model.dm.list.read(cx).slice(peer).unwrap().read(cx).fetched, "{scene:?}: its newest page is in");
                }
                let expected = if scene == Scene::DmLocked { E2eeStatus::Locked } else { E2eeStatus::Ready };
                assert_eq!(model.live.e2ee.read(cx).status, expected);
            });
        }
    }

    #[test]
    fn the_json_names_its_scene() {
        let j = json(Scene::DmKeyChanged);
        assert_eq!(j["scene"], "dm-key-changed");
        assert_eq!(j["nav"]["dm"], ALICE);
        assert!(j["events"].as_array().unwrap().iter().any(|e| e["event"] == "dm_history_received"));
    }
}

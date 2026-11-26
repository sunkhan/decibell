//! Feeds the users directory (`features/users/userDirectory.ts`) from every event that
//! names someone with their uid, so drawing a name is one lookup and a rename changes
//! one entry. uids are central's, so one map serves central and every community.
//!
//! Central's names are current and win. A community's can be older (it learns a rename
//! when the member next connects to it), so they only fill in a uid central hasn't
//! named. Names kept as history (ban rows, audit entries, invite labels, wipes) don't
//! feed the store at all: those views draw their own payload name as the fallback.

use decibell_core::events::{ChannelMessagePayload, CoreEvent};
use gpui::App;

use crate::Model;
use crate::ids::Uid;

type Pair<'a> = (Uid, &'a str);

fn central<'a>(model: &Model, pairs: impl IntoIterator<Item = Pair<'a>>, cx: &mut App) {
    model.users.update(cx, |users, cx| users.remember(pairs, true, cx));
}

fn community<'a>(model: &Model, pairs: impl IntoIterator<Item = Pair<'a>>, cx: &mut App) {
    model.users.update(cx, |users, cx| users.remember(pairs, false, cx));
}

/// A message's sender and the sender of what it replies to.
fn sent(m: &ChannelMessagePayload) -> [Pair<'_>; 2] {
    [(m.sender_uid, m.sender.as_str()), (m.reply_to_sender_uid, m.reply_to_sender.as_str())]
}

pub fn directory(model: &Model, event: &CoreEvent, cx: &mut App) {
    use CoreEvent as E;
    match event {
        // ── Central ──
        E::LoginSucceeded(p) => central(model, [(p.uid, p.username.as_str())], cx),
        E::UserListUpdated(p) => central(model, p.users.iter().map(|u| (u.uid, u.username.as_str())), cx),
        E::PresenceDelta(p) => central(model, [(p.uid, p.username.as_str())], cx),
        E::FriendListReceived(p) => central(model, p.friends.iter().map(|f| (f.uid, f.username.as_str())), cx),
        E::DmConversationsReceived(p) => central(
            model,
            p.conversations.iter().flat_map(|c| {
                [(c.peer_uid, c.peer.as_str()), (c.last_message_sender_uid, c.last_message_sender.as_str())]
            }),
            cx,
        ),
        E::MessageReceived(p) => {
            let sent = [(p.sender_uid, p.sender.as_str()), (p.reply_to_sender_uid, p.reply_to_sender.as_str())];
            // DMs come from central; channel messages from a community.
            if p.context == "dm" {
                central(model, sent.into_iter().chain([(p.recipient_uid, p.recipient.as_str())]), cx);
            } else {
                community(model, sent, cx);
            }
        }
        E::DmHistoryReceived(p) => central(
            model,
            [(p.peer_uid, p.peer.as_str())].into_iter().chain(p.messages.iter().flat_map(|m| {
                [(m.sender_uid, m.sender.as_str()), (m.reply_to_sender_uid, m.reply_to_sender.as_str())]
            })),
            cx,
        ),
        E::CallSignal(p) => central(model, [(p.from_uid, p.from.as_str())], cx),
        E::UserRenamed(p) => central(model, [(p.uid, p.username.as_str())], cx),

        // ── Communities ──
        E::ChannelHistoryReceived(p) => community(model, p.messages.iter().flat_map(sent), cx),
        E::MessageSearchReceived(p) => community(model, p.messages.iter().flat_map(sent), cx),
        E::MemberListReceived(p) => community(
            model,
            p.members.iter().chain(&p.extra).map(|m| (m.uid, m.username.as_str())),
            cx,
        ),
        E::MemberSearchReceived(p) => community(model, p.members.iter().map(|m| (m.uid, m.username.as_str())), cx),
        E::MemberUpsert(p) => community(model, [(p.member.uid, p.member.username.as_str())], cx),
        E::VoicePresenceUpdated(p) => {
            community(model, p.user_states.iter().map(|s| (s.uid, s.username.as_str())), cx)
        }
        E::StreamPresenceUpdated(p) => {
            community(model, p.streams.iter().map(|s| (s.owner_uid, s.owner_username.as_str())), cx)
        }
        E::CommunityAuthResponded(p) => community(model, [(p.owner_uid, p.owner_username.as_str())], cx),
        E::ServerMetaUpdated(p) => community(model, [(p.owner_uid, p.owner_username.as_str())], cx),

        // ── Events that name nobody the directory should learn ──
        // (No wildcard: a new event must be placed here or above.)
        E::ConnectionLost(_)
        | E::ConnectionRestored(_)
        | E::AvatarChanged(_)
        | E::LoginFailed(_)
        | E::RegisterResponded(_)
        | E::LoggedOut(_)
        | E::DevicesChanged(_)
        | E::SessionTokenUpdated
        | E::ServerListReceived(_)
        | E::MembershipsReceived(_)
        | E::MembershipAdded(_)
        | E::ChannelPruned(_)
        | E::ChannelUpdated(_)
        | E::ChannelWipeResponded(_)
        | E::ChannelWiped(_)
        | E::OwnPresence(_)
        | E::SystemIdleChanged(_)
        | E::FriendActionResponded(_)
        | E::ModActionResponded(_)
        | E::ChannelMessageRejected(_)
        | E::MembershipRevoked(_)
        | E::RoleListReceived(_)
        | E::MemberRemove(_)
        | E::BanListReceived(_)
        | E::ServerUpdateResponded(_)
        | E::AuditLogReceived(_)
        | E::StorageInfoReceived(_)
        | E::VoiceForceNotify(_)
        | E::ChannelOverwritesReceived(_)
        | E::RoleActionResponded(_)
        | E::ChannelListUpdated(_)
        | E::ChannelActionResponded(_)
        | E::InviteListReceived(_)
        | E::InviteCreateResponded(_)
        | E::InviteRevokeResponded(_)
        | E::VoiceStateChanged(_)
        | E::HotkeyAction(_)
        | E::HotkeysStatus(_)
        | E::VoiceUserSpeaking(_)
        | E::VoiceUserStateChanged(_)
        | E::VoiceInputLevel(_)
        | E::VoicePingUpdated(_)
        | E::VoiceConnectionStats(_)
        | E::VoiceError(_)
        | E::StreamCodecChanged(_)
        | E::StreamGpuFallback(_)
        | E::StreamCaptureEnded
        | E::KeyframeRequested
        | E::NativeStreamFailed(_)
        | E::CapsRefreshed
        | E::DmReadSynced(_)
        | E::DmMessageDeleteResponded(_)
        | E::DmMessageDeleted(_)
        | E::ChannelMessageDeleteResponded(_)
        | E::ChannelMessageDeleted(_)
        | E::DmMessageEditResponded(_)
        | E::DmMessageEdited(_)
        | E::E2eeStatusChanged(_)
        | E::E2eePeerChanged(_)
        | E::VoiceE2eeState(_)
        | E::ChannelMessageEditResponded(_)
        | E::ChannelMessageEdited(_)
        | E::ServerPictureUpdateResponded(_)
        | E::ServerPictureReceived(_)
        | E::ServerPictureChanged(_)
        | E::CallConnected(_)
        | E::CallFailed(_)
        | E::CallDropped(_) => {}
    }
}

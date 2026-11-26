//! DMs out of the unified `message_received` and the DM events
//! (`features/dm/useDmEvents.ts`). The "other user" of a conversation is the non-self side
//! of (sender, recipient): our own echo files under the recipient. Conversations key on
//! that side's uid.

use std::sync::Arc;

use decibell_core::api;
use decibell_core::events::{
    ConnectionEventPayload, DmConversationsReceivedPayload, DmHistoryReceivedPayload, DmMessageDeleteRespondedPayload,
    DmMessageDeletedPayload, DmMessageEditRespondedPayload, DmMessageEditedPayload, DmReadSyncedPayload,
    MessageReceivedPayload,
};
use gpui::{App, SharedString};

use crate::Model;
use crate::call::call;
use crate::chat::history::{HistoryReply, Scope, WaitKey};
use crate::chat::message::{DecryptError, Message};
use crate::dm::PreviewInput;
use crate::stores::navigation::ActiveView;

pub fn message_received(model: &Model, p: MessageReceivedPayload, cx: &mut App) {
    let me = model.auth.read(cx).uid;
    let from_self = me != 0 && p.sender_uid == me;
    let other = if from_self { p.recipient_uid } else { p.sender_uid };
    if other <= 0 {
        return;
    }
    let other_name: SharedString = if from_self { p.recipient.clone() } else { p.sender.clone() }.into();
    // Central's error replies (friends-only recipient, persist failure) come back as a
    // self-DM with no id and our nonce: withdraw the bubble and say why, rather than
    // showing the error text as a message of ours.
    if from_self && p.id == 0 && !p.nonce.is_empty() {
        let pending = model
            .dm
            .list
            .read(cx)
            .slice(other)
            .is_some_and(|s| s.read(cx).bubble(&p.nonce).is_some());
        if pending {
            model.dm.list.update(cx, |l, cx| l.remove_by_nonce(other, &p.nonce, cx));
            let body: SharedString = p.content.into();
            model.toasts.update(cx, |t, cx| t.error("Message not sent", Some(body), cx));
            return;
        }
    }
    let message = Arc::new(Message::from_received(p));
    // "Actively viewing" needs both: this conversation open and the DM view on screen.
    let viewing = model.nav.read(cx).active_view == ActiveView::Dm && model.dm.list.read(cx).active_peer == Some(other);
    model.dm.list.update(cx, |l, cx| l.add_message(other, message, from_self, other_name, viewing, cx));
}

/// Central's conversation previews (on every sign-in).
pub fn dm_conversations_received(model: &Model, p: DmConversationsReceivedPayload, cx: &mut App) {
    let previews: Vec<PreviewInput> = p
        .conversations
        .into_iter()
        .filter(|c| c.peer_uid > 0)
        .map(|c| PreviewInput {
            peer: c.peer_uid,
            username: c.peer.into(),
            row: Arc::new(Message::dm_preview(
                c.last_message_id,
                c.last_message_sender.into(),
                c.last_message_sender_uid,
                c.last_message_content.into(),
                c.last_timestamp,
                c.encrypted,
                DecryptError::from_wire(&c.decrypt_error),
            )),
            last_timestamp: c.last_timestamp,
            unread: c.unread_count,
        })
        .collect();
    model.dm.list.update(cx, |l, cx| l.hydrate(previews, cx));
}

/// One page of a conversation, routed by the echoed mode.
pub fn dm_history_received(model: &Model, p: DmHistoryReceivedPayload, cx: &mut App) {
    let peer = p.peer_uid;
    if peer <= 0 {
        return;
    }
    let (around, after, before) = (p.around_id, p.after_id, p.before_id);
    let (has_more, has_more_after) = (p.has_more, p.has_more_after);
    let page: Vec<Arc<Message>> = p.messages.into_iter().map(|m| Arc::new(Message::from_dm_history(m))).collect();
    // See `events::chat::channel_history_received`: an older page whose cursor an older
    // central didn't echo is never read as the newest.
    let latest = before <= 0 && model.chat.history.read(cx).waiting_before_cursor(&WaitKey::Dm(peer)).is_none_or(|b| b <= 0);
    model.dm.list.update(cx, |l, cx| {
        if around > 0 {
            l.set_window(peer, &page, has_more, has_more_after, cx);
        } else if after > 0 {
            l.append_newer(peer, &page, has_more_after, after, cx);
        } else {
            // No cursor at all: the newest page.
            l.append_history(peer, &page, has_more, latest, cx);
        }
    });
    let newest_id = page.iter().map(|m| m.id).max().unwrap_or(0);
    model.chat.history.update(cx, |h, _| h.settle(&WaitKey::Dm(peer), around, after, HistoryReply { newest_id, has_more_after }));
}

/// Another device of ours read a conversation: its unread badge clears here too.
pub fn dm_read_synced(model: &Model, p: &DmReadSyncedPayload, cx: &mut App) {
    if p.peer_uid > 0 {
        model.dm.list.update(cx, |l, cx| l.apply_read_sync(p.peer_uid, p.up_to_id, cx));
    }
}

pub fn dm_message_delete_responded(model: &Model, p: &DmMessageDeleteRespondedPayload, cx: &mut App) {
    if !p.success {
        model.dm.list.update(cx, |l, cx| l.restore_pending_deletion(p.peer_uid, p.message_id, cx));
        let body: SharedString =
            if p.message.is_empty() { "Server rejected the request.".into() } else { p.message.clone().into() };
        model.toasts.update(cx, |t, cx| t.error("Couldn't delete message", Some(body), cx));
        return;
    }
    model.dm.list.update(cx, |l, cx| l.clear_pending_deletion(p.peer_uid, p.message_id, cx));
}

pub fn dm_message_deleted(model: &Model, p: &DmMessageDeletedPayload, cx: &mut App) {
    model.dm.list.update(cx, |l, cx| {
        l.remove_message(p.peer_uid, p.message_id, cx);
        l.clear_pending_deletion(p.peer_uid, p.message_id, cx);
    });
}

/// Edits are not optimistic (as sends): the broadcast applies; the answer shows failures.
pub fn dm_message_edit_responded(model: &Model, p: &DmMessageEditRespondedPayload, cx: &mut App) {
    if !p.success {
        let body: SharedString =
            if p.message.is_empty() { "Server rejected the request.".into() } else { p.message.clone().into() };
        model.toasts.update(cx, |t, cx| t.error("Couldn't edit message", Some(body), cx));
    }
}

/// Central's connection dropped (`useDmEvents`' `connection_lost` listener:
/// `abandonDmRequests()`): replies to DM history requests went down with it, so their
/// waits end now instead of at their 10 s timeout. A community's drop is
/// `events::chat::community_connection_lost`.
pub fn connection_lost(model: &Model, p: &ConnectionEventPayload, cx: &mut App) {
    if p.server_type == "central" {
        model.chat.history.update(cx, |h, _| h.abandon(Scope::Central));
    }
}

/// Our E2EE keys became usable (`useE2eeEvents.ts` `reloadDms`, on `E2eeEvent::BecameReady`):
/// every sealed DM that arrived before holds a placeholder, so all loaded DM pages go
/// (`invalidateAllHistory`), the sidebar previews are asked for again, and the open
/// conversation reloads its newest page so its panel doesn't sit empty; the others reload
/// when opened. As in the renderer this follows the open conversation whether or not the
/// DM view is on screen, and a sign-in that ends "ready" asks for the previews twice (here
/// and in `signed_in`).
pub fn reload_after_unlock(model: &Model, cx: &mut App) {
    model.dm.list.update(cx, |l, cx| l.invalidate_all_history(cx));
    call(cx, api::dm::request_dm_conversations(), |r, _| {
        if let Err(e) = r {
            log::debug!("request_dm_conversations: {e}");
        }
    });
    if let Some(peer) = model.dm.list.read(cx).active_peer {
        crate::actions::dm::open_history(peer, cx);
    }
}

pub fn dm_message_edited(model: &Model, p: DmMessageEditedPayload, cx: &mut App) {
    let edit = crate::chat::message::Edit {
        content: p.content.into(),
        edited_at: p.edited_at,
        encrypted: p.encrypted,
        decrypt_error: DecryptError::from_wire(&p.decrypt_error),
        suppress_embeds: None,
    };
    model.dm.list.update(cx, |l, cx| l.apply_edit(p.peer_uid, p.message_id, &edit, cx));
}

//! Channel messages and their lifecycle (`features/chat/useChatEvents.ts`, the four
//! `channel_message_*` listeners of `useServerEvents.ts`, `useSearchEvents.ts`), plus the
//! message half of the hooks the foundation's handlers call (`todo::m2`).

use std::sync::Arc;

use decibell_core::api;
use decibell_core::events::{
    ChannelHistoryReceivedPayload, ChannelMessageDeleteRespondedPayload, ChannelMessageDeletedPayload,
    ChannelMessageEditRespondedPayload, ChannelMessageEditedPayload, ChannelMessageRejectedPayload, ChannelPrunedPayload,
    ChannelWipedPayload, MessageReceivedPayload, MessageSearchReceivedPayload,
};
use gpui::{App, Entity, SharedString};

use crate::Model;
use crate::call::call;
use crate::chat::drafts::DraftKey;
use crate::chat::history::{HistoryReply, Scope, WaitKey};
use crate::chat::message::{Message, attachment_keys};
use crate::chat::search::SearchAnswer;
use crate::chat::slice::MessageSlice;
use crate::chat::slowmode::interval_for;
use crate::ids::{ChannelId, ServerId};
use crate::stores::community::Channel;

fn now(cx: &App) -> std::time::Instant {
    cx.background_executor().now()
}

/// The slice of a channel, created where the renderer created a map entry.
pub(crate) fn ensure_slice(model: &Model, server: ServerId, channel: &ChannelId, cx: &mut App) -> Entity<MessageSlice> {
    if let Some(slice) = model.chat.channels.read(cx).get(server, channel) {
        return slice.clone();
    }
    let own = model.auth.read(cx).uid;
    let encrypted = channel_encrypted(model, server, channel, cx);
    model.chat.channels.update(cx, |c, cx| c.ensure(server, channel, own, encrypted, cx))
}

/// `channelIsEncrypted`: the one lookup every send / edit / upload site uses.
pub(crate) fn channel_encrypted(model: &Model, server: ServerId, channel: &str, cx: &App) -> bool {
    model
        .community(server, cx)
        .and_then(|c| c.channels.read(cx).get(channel).map(|ch| ch.encrypted))
        .unwrap_or(false)
}

/// The slowmode interval that applies to us in a channel (0 = none or bypassed).
pub(crate) fn slowmode_interval(model: &Model, server: ServerId, channel: &str, cx: &App) -> u32 {
    let Some(community) = model.community(server, cx) else { return 0 };
    let channels = community.channels.read(cx);
    interval_for(channels.get(channel), model.is_owner(server, cx))
}

fn register_keys(model: &Model, server: ServerId, metas: &[decibell_core::events::EncryptedAttachmentMetaPayload], cx: &App) {
    if !metas.is_empty() {
        model.chat.composer.read(cx).service().register_keys(server, attachment_keys(metas));
    }
}

/// A live channel message (DMs are `events::dm`).
pub fn message_received(model: &Model, p: MessageReceivedPayload, cx: &mut App) {
    if p.context == "dm" {
        super::dm::message_received(model, p, cx);
        return;
    }
    // The server id namespaces the cache; the community router always stamps it.
    if p.server_id.is_empty() {
        return;
    }
    let server = ServerId::from_wire(&p.server_id);
    let channel: ChannelId = p.context.clone().into();
    register_keys(model, server, &p.encrypted_attachments, cx);
    let message = Arc::new(Message::from_received(p));
    let displayed = model.chat.channels.read(cx).is_displayed(server, &channel);
    // A channel never loaded and with no fetch in flight: its first visit loads the newest
    // page anyway, so nothing is cached for it (the slice is checked for that itself).
    let slice = match model.chat.channels.read(cx).get(server, &channel) {
        Some(slice) => Some(slice.clone()),
        None if displayed || message.id == 0 => Some(ensure_slice(model, server, &channel, cx)),
        None => None,
    };
    let own = model.auth.read(cx).uid;
    let ours = own != 0 && message.sender_uid == own;
    if let Some(slice) = slice {
        slice.update(cx, |s, cx| s.add_message(message, displayed, cx));
    }
    // One of ours, from this device or another: a slowmode window starts.
    if ours {
        let interval = slowmode_interval(model, server, &channel, cx);
        let now = now(cx);
        model.chat.slowmode.update(cx, |s, cx| s.note_own_message((server, channel), interval, now, cx));
    }
}

/// A page of channel history, routed by the mode the server echoed.
pub fn channel_history_received(model: &Model, p: ChannelHistoryReceivedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let channel: ChannelId = p.channel_id.clone().into();
    let (around, after, before) = (p.around_id, p.after_id, p.before_id);
    let (has_more, has_more_after) = (p.has_more, p.has_more_after);
    let mut page: Vec<Arc<Message>> = Vec::with_capacity(p.messages.len());
    for m in p.messages {
        register_keys(model, server, &m.encrypted_attachments, cx);
        page.push(Arc::new(Message::from_stored(m)));
    }
    let own = model.auth.read(cx).uid;
    let key = WaitKey::Channel(server, channel.clone());
    let latest = before <= 0 && model.chat.history.read(cx).waiting_before_cursor(&key).is_none_or(|b| b <= 0);
    let slice = ensure_slice(model, server, &channel, cx);
    slice.update(cx, |s, cx| {
        if around > 0 {
            // A jump window: replaces the slice.
            s.set_window(&page, has_more, has_more_after, cx);
        } else if after > 0 {
            s.append_newer(&page, has_more_after, after, own, cx);
        } else {
            // No cursor at all: the channel's newest page, which answers for every row above
            // its oldest; never a reply to an older-page request whose cursor an older
            // server didn't echo (`waiting_before_cursor`).
            s.prepend_history(&page, has_more, latest, own, cx);
        }
        s.set_loading(false, cx);
        // A newer page only extends a slice already loaded; one landing on a slice dropped
        // meanwhile must not stand in for its first page.
        if after <= 0 {
            s.mark_fetched(cx);
        }
    });
    let newest_id = page.iter().map(|m| m.id).max().unwrap_or(0);
    model.chat.history.update(cx, |h, _| h.settle(&key, around, after, HistoryReply { newest_id, has_more_after }));
}

/// Retention pruned messages (attachment tombstones are not applied, as in the renderer).
pub fn channel_pruned(model: &Model, p: &ChannelPrunedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let slice = ensure_slice(model, server, &p.channel_id.clone().into(), cx);
    slice.update(cx, |s, cx| s.apply_pruned(&p.deleted_message_ids, cx));
}

pub fn channel_wiped(model: &Model, p: &ChannelWipedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let slice = ensure_slice(model, server, &p.channel_id.clone().into(), cx);
    slice.update(cx, |s, cx| s.apply_wiped(cx));
}

/// The community refused one of our messages: withdraw exactly that bubble (left in place
/// it would anchor at the tail forever), put its text back in the message box, say why; a
/// slowmode refusal also says how long is left.
pub fn channel_message_rejected(model: &Model, p: &ChannelMessageRejectedPayload, cx: &mut App) {
    let now = now(cx);
    model.chat.pacing.update(cx, |pc, _| pc.note_typed_rejection(now));
    let server = ServerId::from_wire(&p.server_id);
    let channel: ChannelId = p.channel_id.clone().into();
    let mut restored = false;
    if !p.nonce.is_empty()
        && let Some(slice) = model.chat.channels.read(cx).get(server, &channel).cloned()
    {
        let bubble = slice.update(cx, |s, cx| s.remove_by_nonce(&p.nonce, cx));
        if let Some(bubble) = bubble.filter(|b| !b.content.is_empty()) {
            restored = model
                .chat
                .drafts
                .update(cx, |d, cx| d.restore(DraftKey::Channel(server, channel.clone()), bubble.content.clone(), cx));
        }
    }
    if p.retry_after_seconds > 0 {
        let secs = p.retry_after_seconds;
        model.chat.slowmode.update(cx, |s, cx| s.note_refusal((server, channel), secs, now, cx));
    }
    let reason = if p.reason.is_empty() { "The server rejected it." } else { p.reason.as_str() };
    let body: SharedString =
        if restored { format!("{reason} Your text is back in the message box.").into() } else { reason.to_string().into() };
    model.toasts.update(cx, |t, cx| t.error("Message not sent", Some(body), cx));
}

fn existing_slice(model: &Model, server: &str, channel: &str, cx: &App) -> Option<Entity<MessageSlice>> {
    model.chat.channels.read(cx).get(ServerId::from_wire(server), channel).cloned()
}

/// The community answered our delete: on refusal the row comes back and the reason shows.
pub fn channel_message_delete_responded(model: &Model, p: &ChannelMessageDeleteRespondedPayload, cx: &mut App) {
    let slice = existing_slice(model, &p.server_id, &p.channel_id, cx);
    if !p.success {
        if let Some(slice) = slice {
            slice.update(cx, |s, cx| s.restore_pending_deletion(p.message_id, cx));
        }
        let body: SharedString =
            if p.message.is_empty() { "Server rejected the request.".into() } else { p.message.clone().into() };
        model.toasts.update(cx, |t, cx| t.error("Couldn't delete message", Some(body), cx));
        return;
    }
    if let Some(slice) = slice {
        slice.update(cx, |s, _| s.clear_pending_deletion(p.message_id));
    }
}

/// A channel message was deleted, ours or anyone's. Idempotent.
pub fn channel_message_deleted(model: &Model, p: &ChannelMessageDeletedPayload, cx: &mut App) {
    if let Some(slice) = existing_slice(model, &p.server_id, &p.channel_id, cx) {
        slice.update(cx, |s, cx| {
            s.remove_message(p.message_id, cx);
            s.clear_pending_deletion(p.message_id);
        });
    }
}

/// Edits are not optimistic: the broadcast applies them; the answer only shows failures.
pub fn channel_message_edit_responded(model: &Model, p: &ChannelMessageEditRespondedPayload, cx: &mut App) {
    if !p.success {
        let body: SharedString =
            if p.message.is_empty() { "Server rejected the request.".into() } else { p.message.clone().into() };
        model.toasts.update(cx, |t, cx| t.error("Couldn't edit message", Some(body), cx));
    }
}

pub fn channel_message_edited(model: &Model, p: ChannelMessageEditedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    register_keys(model, server, &p.encrypted_attachments, cx);
    let Some(slice) = existing_slice(model, &p.server_id, &p.channel_id, cx) else { return };
    let edit = crate::chat::message::Edit {
        content: p.content.into(),
        edited_at: p.edited_at,
        encrypted: p.encrypted,
        decrypt_error: crate::chat::message::DecryptError::from_wire(&p.decrypt_error),
        suppress_embeds: Some(p.suppress_embeds),
    };
    slice.update(cx, |s, cx| s.apply_edit(p.message_id, &edit, cx));
}

/// A page of search results (`useSearchEvents`).
pub fn message_search_received(model: &Model, p: MessageSearchReceivedPayload, cx: &mut App) {
    let server = ServerId::from_wire(&p.server_id);
    let mut messages = Vec::with_capacity(p.messages.len());
    for m in p.messages {
        register_keys(model, server, &m.encrypted_attachments, cx);
        messages.push(Arc::new(Message::from_stored(m)));
    }
    let answer = SearchAnswer {
        server,
        seq: p.seq,
        success: p.success,
        message: p.message.into(),
        messages,
        has_more: p.has_more,
        next_cursor: p.next_cursor,
        next_offset: p.next_offset,
        total: p.total,
        total_capped: p.total_capped,
        text_skipped: p.text_skipped_channel_ids.into_iter().map(Into::into).collect(),
        partial: p.partial,
        retry_after_ms: p.retry_after_ms,
        first_page: p.first_page,
    };
    model.chat.search.update(cx, |s, cx| s.receive(answer, cx));
}

// ── Hooks out of the foundation's handlers ──────────────────────────────────

/// Signed in: the DM previews; after a dropped connection also what happened meanwhile.
pub fn signed_in(model: &Model, again: bool, cx: &mut App) {
    let uid = model.auth.read(cx).uid;
    model.dm.list.update(cx, |l, _| l.set_own_uid(uid));
    call(cx, api::dm::request_dm_conversations(), |r, _| {
        if let Err(e) = r {
            log::debug!("request_dm_conversations: {e}");
        }
    });
    if again {
        model.chat.history.update(cx, |h, _| h.abandon(Scope::Central));
        model.dm.list.update(cx, |l, cx| l.mark_histories_stale(cx));
    }
}

/// Sign-out: nothing of this account's messages, drafts, searches or previews stays.
pub fn logged_out(model: &Model, cx: &mut App) {
    let c = &model.chat;
    c.viewer.update(cx, |v, cx| v.close(cx));
    c.media_menu.update(cx, |m, cx| m.close(cx));
    c.composer.update(cx, |comp, cx| {
        comp.service().clear_all();
        comp.reset_for_logout(cx);
    });
    c.channels.update(cx, |ch, cx| ch.reset_for_logout(cx));
    c.history.update(cx, |h, _| h.reset_for_logout());
    c.pacing.update(cx, |p, _| p.reset_for_logout());
    c.slowmode.update(cx, |s, cx| s.reset_for_logout(cx));
    c.drafts.update(cx, |d, cx| d.reset_for_logout(cx));
    c.link_previews.update(cx, |l, _| l.reset_for_logout());
    c.search.update(cx, |s, cx| s.reset_for_logout(cx));
    c.audio.update(cx, |a, cx| a.set_active(None, cx));
    c.video.update(cx, |v, cx| v.set_active(None, cx));
    model.dm.list.update(cx, |l, cx| l.reset_for_logout(cx));
}

/// A community authenticated: its message bucket is full again.
pub fn reset_send_pacing(model: &Model, server: ServerId, cx: &mut App) {
    crate::chat::pacing::reset(&model.chat.pacing, server, cx);
}

/// A community authenticated, before its new channel list is stored (review B4): requests
/// made on the old connection are never answered; cached slices refetch or go windowed.
pub fn community_authenticated(model: &Model, server: ServerId, channels: &[Channel], cx: &mut App) {
    model.chat.history.update(cx, |h, _| h.abandon(Scope::Community(server)));
    let live: Vec<ChannelId> = channels.iter().map(|c| c.id.clone()).collect();
    model.chat.channels.update(cx, |c, cx| c.mark_server_stale(server, &live, cx));
}

/// Read the community's attachment endpoint and token into the attachment service.
pub fn attachment_target_changed(model: &Model, server: ServerId, cx: &mut App) {
    let service = model.chat.composer.read(cx).service().clone();
    let args = api::servers::GetAttachmentTargetArgs { server_id: server.wire() };
    call(cx, api::servers::get_attachment_target(args), move |r, _| match r {
        Ok(Some(t)) => service.set_target(
            server,
            Some(decibell_attachments::AttachmentTarget {
                host: t.host,
                port: t.port.clamp(0, u16::MAX as i32) as u16,
                jwt: t.jwt,
                max_bytes: t.max_attachment_bytes.max(0) as u64,
                cert_fingerprint: t.cert_fingerprint,
            }),
        ),
        Ok(None) => {}
        Err(e) => log::error!("get_attachment_target: {e}"),
    });
}

/// Central renewed the session token: every connected community's target is read again
/// (`refreshTargetsFromNative`).
pub fn session_token_updated(model: &Model, cx: &mut App) {
    let servers: Vec<ServerId> = model.connections.read(cx).connected().collect();
    for server in servers {
        let serves = model.community(server, cx).is_some_and(|c| c.meta.read(cx).attachment_port > 0);
        if serves {
            attachment_target_changed(model, server, cx);
        }
    }
}

/// A community's connection dropped: their replies went down with it, the token too.
pub fn community_connection_lost(model: &Model, server: ServerId, cx: &mut App) {
    model.chat.history.update(cx, |h, _| h.abandon(Scope::Community(server)));
    model.chat.composer.read(cx).service().set_target(server, None);
}

/// A channel no longer exists: a future channel reusing the slug refetches.
pub fn purge_channel_state(model: &Model, server: ServerId, channel: &ChannelId, cx: &mut App) {
    model.chat.channels.update(cx, |c, cx| c.purge(server, channel, cx));
}

pub fn legacy_rejection_suppressed(model: &Model, cx: &mut App) -> bool {
    let now = now(cx);
    model.chat.pacing.read(cx).legacy_rejection_suppressed(now)
}

#[cfg(test)]
mod tests;

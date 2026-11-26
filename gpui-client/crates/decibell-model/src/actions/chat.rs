//! What the chat views ask for (`ChatPanel.tsx`'s handlers, `historyRequests.ts`,
//! `sendPacing.ts`'s backstops, `uploadAttachment.ts`'s orchestration, `goToMessage.ts`):
//! store updates plus the core calls, as plain functions over `&mut App`.
//!
//! Paging and jumps return a `Task` that resolves when the reply landed (or didn't); the
//! work itself is detached, so a view that drops the task cancels nothing. A view re-checks
//! its edges [`crate::chat::history::RETRY_DELAY`] after a `false`.

use std::sync::Arc;
use std::time::Duration;

use decibell_attachments::upload::UploadSource;
use decibell_core::api;
use futures::FutureExt as _;
use futures::channel::oneshot;
use gpui::{App, Entity, SharedString, Task};

use crate::Model;
use crate::call::call;
use crate::chat::drafts::DraftKey;
use crate::chat::history::{AROUND_LIMIT, ECHO_PAGE_LIMIT, HistoryReply, HistoryRequest, LATEST_ATTEMPTS, LatestLoad, PAGE_LIMIT, RETRY_DELAY, WaitKey};
use crate::chat::message::{Attachment, Message};
use crate::chat::pacing::pace;
use crate::chat::search::SearchArgs;
use crate::chat::slice::{MessageSlice, ScrollPosition};
use crate::events::chat::{channel_encrypted, ensure_slice, slowmode_interval};
use crate::ids::{ChannelId, ServerId};
use crate::stores::navigation::ActiveView;
use crate::stores::permissions::perm;

/// A sent message must come back as its own broadcast within this long, or the server is
/// asked (`ECHO_TIMEOUT_MS`).
pub const ECHO_TIMEOUT: Duration = Duration::from_secs(30);
/// How far an echo check pages past the send (100 rows a page).
pub const ECHO_CHECK_PAGES: usize = 5;
/// An optimistic delete waits this long for the server's answer before the row comes back.
pub const DELETE_WATCHDOG: Duration = Duration::from_secs(5);

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// A per-send nonce for optimistic bubbles (a UUID v4).
pub fn new_nonce() -> SharedString {
    let mut b = [0u8; 16];
    if getrandom::getrandom(&mut b).is_err() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        b.copy_from_slice(&t.to_le_bytes());
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32]).into()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Run `work` detached; the returned task only waits for its answer.
fn detached<T: 'static>(cx: &mut App, work: impl AsyncFnOnce(&mut gpui::AsyncApp) -> T + 'static) -> Task<Option<T>> {
    let (tx, rx) = oneshot::channel();
    cx.spawn(async move |cx| {
        let _ = tx.send(work(cx).await);
    })
    .detach();
    cx.spawn(async move |_| rx.await.ok())
}

/// Run a message call (send, edit) after the previous one of this community finished, so
/// they reach the server in the order they were made: the core runs calls concurrently
/// on its runtime, and a burst of ten admitted at once raced each other to the socket.
fn call_in_order<T: 'static>(
    model: &Model,
    server: ServerId,
    request: impl Future<Output = Result<T, decibell_core::api::CoreError>> + 'static,
    done: impl FnOnce(Result<T, crate::error::UiError>, &mut App) + 'static,
    cx: &mut App,
) {
    let (tx, rx) = oneshot::channel::<()>();
    let previous = model.chat.pacing.update(cx, |p, _| p.chains.insert(server, rx.map(|_| ()).boxed_local().shared()));
    cx.spawn(async move |cx| {
        if let Some(previous) = previous {
            previous.await;
        }
        let result = if crate::core::is_running() {
            request.await.map_err(crate::error::UiError::from)
        } else {
            Err(crate::error::UiError::not_running())
        };
        let _ = tx.send(());
        cx.update(|cx| done(result, cx));
    })
    .detach();
}

fn slice_of(model: &Model, server: ServerId, channel: &str, cx: &App) -> Option<Entity<MessageSlice>> {
    model.chat.channels.read(cx).get(server, channel).cloned()
}

// ── Requests leaving (the stores' senders) ──────────────────────────────────

/// [`crate::chat::history::HistorySender`] of the app.
pub(crate) fn send_history_request(key: WaitKey, req: HistoryRequest, cx: &mut App, done: crate::chat::history::SentCallback) {
    match key {
        WaitKey::Channel(server, channel) => {
            let args = api::channels::RequestChannelHistoryArgs {
                server_id: server.wire(),
                channel_id: channel.to_string(),
                before_id: req.before_id,
                limit: req.limit,
                around_id: (req.around_id > 0).then_some(req.around_id),
                after_id: (req.after_id > 0).then_some(req.after_id),
            };
            call(cx, api::channels::request_channel_history(args), move |r, cx| {
                if let Err(e) = &r {
                    log::error!("history request: {e}");
                }
                done(r.is_ok(), cx)
            });
        }
        WaitKey::Dm(peer) => {
            let name = model(cx).map(|m| m.name_of(peer, &SharedString::default(), cx)).unwrap_or_default();
            let args = api::dm::RequestDmHistoryArgs {
                peer: name.to_string(),
                peer_uid: peer,
                before_id: req.before_id,
                limit: req.limit,
                around_id: (req.around_id > 0).then_some(req.around_id),
                after_id: (req.after_id > 0).then_some(req.after_id),
            };
            call(cx, api::dm::request_dm_history(args), move |r, cx| {
                if let Err(e) = &r {
                    log::error!("history request: {e}");
                }
                done(r.is_ok(), cx)
            });
        }
    }
}

/// [`crate::chat::search::SearchSender`] of the app.
pub(crate) fn send_search_request(a: SearchArgs, cx: &mut App, done: crate::chat::search::SearchSent) {
    let args = api::channels::SearchMessagesArgs {
        server_id: a.server.wire(),
        seq: Some(a.seq),
        query: Some(a.query),
        channel_ids: Some(a.channel_ids),
        author_uids: Some(a.author_uids),
        after: Some(a.after),
        before: Some(a.before),
        has: Some(a.has),
        sort: Some(a.sort.wire().to_string()),
        cursor: Some(a.cursor),
        offset: Some(a.offset),
        limit: Some(a.limit),
        match_within_words: Some(a.match_within_words),
    };
    call(cx, api::channels::search_messages(args), done);
}

// ── History ─────────────────────────────────────────────────────────────────

/// Send a history request and wait for its reply (`requestChannelHistory`).
pub fn request_history(key: WaitKey, req: HistoryRequest, cx: &mut App) -> impl Future<Output = Option<HistoryReply>> + 'static + use<> {
    match model(cx) {
        Some(m) => m.chat.history.update(cx, |h, cx| h.request(key, req, cx)).left_future(),
        None => futures::future::ready(None).right_future(),
    }
}

/// The channel's newest page into whatever its slice holds, `loading` on meanwhile; up to
/// three attempts while the community is connected (`loadLatestChannelPage`). One load
/// per channel at a time: a second caller shares the first. `false` when none got an
/// answer (the panel shows Retry).
pub fn load_latest(server: ServerId, channel: ChannelId, cx: &mut App) -> LatestLoad {
    let model = Model::global(cx).clone();
    let key = WaitKey::Channel(server, channel.clone());
    if let Some(run) = model.chat.history.read(cx).latest.get(&key) {
        return run.clone();
    }
    let slice = ensure_slice(&model, server, &channel, cx);
    slice.update(cx, |s, cx| s.set_loading(true, cx));
    let run = cx
        .spawn({
            let (model, key) = (model.clone(), key.clone());
            async move |cx| {
                let mut ok = false;
                for attempt in 0..LATEST_ATTEMPTS {
                    if attempt > 0 {
                        cx.background_executor().timer(RETRY_DELAY).await;
                    }
                    // Offline: the re-sync after the reconnect loads it.
                    if !cx.update(|cx| model.connections.read(cx).is_connected(server)) {
                        break;
                    }
                    let reply = cx.update(|cx| request_history(key.clone(), HistoryRequest::latest(PAGE_LIMIT), cx));
                    if reply.await.is_some() {
                        ok = true;
                        break;
                    }
                }
                cx.update(|cx| {
                    model.chat.history.update(cx, |h, _| h.latest.remove(&key));
                    if let Some(slice) = slice_of(&model, server, &channel, cx) {
                        slice.update(cx, |s, cx| s.set_loading(false, cx));
                    }
                });
                ok
            }
        })
        .shared();
    model.chat.history.update(cx, |h, _| h.latest.insert(key, run.clone()));
    run
}

/// Landing on a channel (`ChatPanel`'s first-visit effect): its newest page, unless it is
/// in already. `None`: nothing to load.
pub fn open_channel(server: ServerId, channel: ChannelId, cx: &mut App) -> Option<LatestLoad> {
    let model = model(cx)?;
    let fetched = slice_of(&model, server, &channel, cx).is_some_and(|s| s.read(cx).fetched);
    (!fetched).then(|| load_latest(server, channel, cx))
}

/// The list neared its top (`maybeLoadOlderHistory`): the page below the oldest row, once
/// per boundary until it lands. Older pages wait for the newest.
pub fn load_older(server: ServerId, channel: ChannelId, cx: &mut App) -> Option<Task<Option<bool>>> {
    let model = model(cx)?;
    let slice = slice_of(&model, server, &channel, cx)?;
    let before = slice.update(cx, |s, _| {
        if !s.fetched || !s.has_more_before {
            return None;
        }
        let oldest = s.oldest_stored_id();
        if oldest == 0 || oldest == s.last_before {
            return None;
        }
        s.last_before = oldest;
        Some(oldest)
    })?;
    let reply = request_history(WaitKey::Channel(server, channel), HistoryRequest::before(before, PAGE_LIMIT), cx);
    Some(detached(cx, async move |cx| {
        let answered = reply.await.is_some();
        if !answered {
            cx.update(|cx| {
                slice.update(cx, |s, _| {
                    if s.last_before == before {
                        s.last_before = 0;
                    }
                })
            });
        }
        answered
    }))
}

/// The list neared its bottom while windowed (`maybeLoadNewerHistory`): the page above the
/// newest row. Not during a re-sync (its newest page is on the way).
pub fn load_newer(server: ServerId, channel: ChannelId, cx: &mut App) -> Option<Task<Option<bool>>> {
    let model = model(cx)?;
    let slice = slice_of(&model, server, &channel, cx)?;
    let after = slice.update(cx, |s, _| {
        if !s.has_more_after || s.resyncing {
            return None;
        }
        let newest = s.newest_stored_id();
        if newest == 0 || newest == s.last_after {
            return None;
        }
        s.last_after = newest;
        Some(newest)
    })?;
    let reply = request_history(WaitKey::Channel(server, channel), HistoryRequest::after(after, PAGE_LIMIT), cx);
    Some(detached(cx, async move |cx| {
        let answered = reply.await.is_some();
        if !answered {
            cx.update(|cx| {
                slice.update(cx, |s, _| {
                    if s.last_after == after {
                        s.last_after = 0;
                    }
                })
            });
        }
        answered
    }))
}

/// "Jump to present": drop the windowed slice and reload the newest page, like a fresh
/// entry (unconfirmed sends stay). The list lands at the bottom.
pub fn jump_to_present(server: ServerId, channel: ChannelId, cx: &mut App) -> Option<LatestLoad> {
    let model = model(cx)?;
    let slice = ensure_slice(&model, server, &channel, cx);
    slice.update(cx, |s, cx| {
        s.reset_for_jump(cx);
        s.set_scroll(ScrollPosition { at_bottom: true, anchor_id: 0, offset: 0.0 });
    });
    Some(load_latest(server, channel, cx))
}

/// What [`jump_to_message`] started.
pub enum JumpStart {
    /// The message is in the slice: the list lands on it now.
    Loaded,
    /// A window around it is on its way already.
    AlreadyFetching,
    /// A window around it was asked for; `true` when the list can land (it is loaded),
    /// `false` when it isn't there (a toast said why).
    Fetching(Task<Option<bool>>),
}

/// Jump to a replied-to (or searched) message (`jumpToMessage`): in the slice already, or
/// fetch a context window around it (Discord-style) without the history in between.
pub fn jump_to_message(server: ServerId, channel: ChannelId, id: i64, cx: &mut App) -> Option<JumpStart> {
    let model = model(cx)?;
    let slice = ensure_slice(&model, server, &channel, cx);
    let start = slice.update(cx, |s, cx| {
        if s.find(id).is_some() {
            return Some(JumpStart::Loaded);
        }
        if s.pending_jump == Some(id) {
            return Some(JumpStart::AlreadyFetching);
        }
        s.pending_jump = Some(id);
        s.last_before = 0;
        s.last_after = 0;
        s.set_loading(true, cx);
        None
    });
    if let Some(start) = start {
        return Some(start);
    }
    let reply = request_history(WaitKey::Channel(server, channel.clone()), HistoryRequest::around(id, AROUND_LIMIT), cx);
    Some(JumpStart::Fetching(detached(cx, async move |cx| {
        let reply = reply.await;
        cx.update(|cx| {
            // Landed already, or another jump took over.
            if slice.read(cx).pending_jump != Some(id) {
                return false;
            }
            let here = model.nav.read(cx).active_channel_key().is_some_and(|(s, c)| s == server && *c == channel);
            let loaded = slice.read(cx).find(id).is_some();
            if reply.is_some() && here && loaded {
                return true;
            }
            // No answer (review B4), or the message is gone: stop waiting.
            slice.update(cx, |s, cx| {
                s.pending_jump = None;
                s.set_loading(false, cx);
            });
            if here {
                let (title, body) = if reply.is_some() {
                    ("Couldn't find that message", "It may have been deleted.")
                } else {
                    ("Couldn't load that message", "The server didn't answer. Try again.")
                };
                model.toasts.update(cx, |t, cx| t.error(title, Some(body.into()), cx));
            }
            false
        })
    })))
}

/// The list landed on the jump target (`handleJumpLanded`).
pub fn jump_landed(server: ServerId, channel: &str, cx: &mut App) {
    if let Some(slice) = model(cx).and_then(|m| slice_of(&m, server, channel, cx)) {
        slice.update(cx, |s, _| s.pending_jump = None);
    }
}

/// "Go to message" from outside the chat panel (Transfers, search results:
/// `goToMessage`): open the channel, then the panel runs the jump once its first page is
/// in (`ChannelCache::take_jump_request`).
pub fn go_to_message(server: ServerId, channel: ChannelId, message_id: i64, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    crate::actions::servers::open_server(server, cx);
    let known = model.community(server, cx).is_some_and(|c| c.channels.read(cx).contains(&channel));
    if !known {
        return;
    }
    model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(channel.clone()), cx));
    if message_id > 0 {
        let now = cx.background_executor().now();
        model.chat.channels.update(cx, |c, cx| c.request_jump(server, channel, message_id, now, cx));
    }
}

/// The re-sync of the channel on screen after its community authenticated again
/// (`ChatPanel`'s resyncEpoch effect; review B4). `at_bottom`: the list's live position.
/// Scrolled up (or windowed already): the slice keeps its place and pages down. Caught up:
/// the newest page merges in (or replaces the slice across a gap), live arrivals waiting
/// for it meanwhile.
pub fn resync_displayed(server: ServerId, channel: ChannelId, at_bottom: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let slice = ensure_slice(&model, server, &channel, cx);
    let stored = slice.update(cx, |s, cx| {
        s.last_before = 0;
        s.last_after = 0;
        s.pending_jump = None;
        let stored = s.newest_stored_id() > 0;
        if stored && s.fetched && (s.has_more_after || !at_bottom) {
            s.mark_windowed(cx);
            return None;
        }
        if stored {
            s.set_resyncing(true, cx);
        }
        Some(stored)
    });
    let Some(stored) = stored else { return };
    let run = load_latest(server, channel, cx);
    cx.spawn(async move |cx| {
        run.await;
        if stored {
            cx.update(|cx| slice.update(cx, |s, cx| s.set_resyncing(false, cx)));
        }
    })
    .detach();
}

// ── Sending ─────────────────────────────────────────────────────────────────

/// What a send did, for the composer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Nothing to send.
    Nothing,
    /// The slice was windowed: it jumped to the present instead; the draft stays and the
    /// next send goes through at the bottom.
    JumpedToPresent,
    /// Slowmode: the text and attachments stay, the timer shakes.
    HeldBySlowmode,
    /// The bubble is on screen: clear the box and the reply target.
    Sent { nonce: SharedString },
}

/// Send what the composer holds (`handleSend`): the optimistic bubble, the queued uploads
/// started, then (once every upload ended) the message, paced to the community's bucket.
/// Failures withdraw the bubble and put the text back.
pub fn send_message(server: ServerId, channel: ChannelId, text: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
    let Some(model) = model(cx) else { return SendOutcome::Nothing };
    let content: SharedString = text.trim().to_string().into();
    let (own, username) = {
        let a = model.auth.read(cx);
        (a.uid, a.username.clone())
    };
    let Some(username) = username else { return SendOutcome::Nothing };
    let slice = ensure_slice(&model, server, &channel, cx);
    // Sending into a windowed slice would drop the message past a hidden gap: snap to the
    // present first (a re-sync's newest page is already coming, and settles the send).
    if slice.read(cx).list_windowed() {
        jump_to_present(server, channel, cx);
        return SendOutcome::JumpedToPresent;
    }
    // The live attachments not claimed by a send in flight (a second Enter while uploads
    // run must not send them twice).
    let pending: Vec<SharedString> = model
        .chat
        .composer
        .read(cx)
        .for_channel(server, &channel)
        .filter(|p| p.status != crate::chat::composer::PendingStatus::Failed && !p.claimed)
        .map(|p| p.id.clone())
        .collect();
    if content.is_empty() && pending.is_empty() {
        return SendOutcome::Nothing;
    }
    let key = (server, channel.clone());
    let interval = slowmode_interval(&model, server, &channel, cx);
    let now = cx.background_executor().now();
    let held = model.chat.slowmode.read(cx).holds(&key, interval, slice.read(cx).rows(), own, now);
    if held {
        model.chat.slowmode.update(cx, |s, cx| s.nudge(key, cx));
        return SendOutcome::HeldBySlowmode;
    }
    model.chat.composer.update(cx, |c, cx| c.claim(&pending, true, cx));
    let nonce = new_nonce();
    let mut bubble = Message::optimistic(username, own, content.clone(), now_secs(), nonce.clone(), reply_to, slice.read(cx).newest_stored_id());
    bubble.channel = channel.clone();
    bubble.pending_attachments = pending.iter().cloned().collect();
    slice.update(cx, |s, cx| s.push_bubble(Arc::new(bubble), cx));
    model.chat.drafts.update(cx, |d, cx| d.clear(&DraftKey::Channel(server, channel.clone()), cx));
    // Bytes leave only now, on the user's send.
    model.chat.composer.update(cx, |c, cx| {
        for id in &pending {
            c.start_upload(id, cx);
        }
    });
    let encrypted = channel_encrypted(&model, server, &channel, cx);
    // No attachments: the message goes to the pacer now, in the order the sends were made.
    if pending.is_empty() {
        finish_send(&model, server, channel, content, reply_to, nonce.clone(), pending, encrypted, cx);
        return SendOutcome::Sent { nonce };
    }
    let wait = model.chat.composer.update(cx, |c, _| c.wait_terminal(&pending));
    cx.spawn({
        let nonce = nonce.clone();
        async move |cx| {
            wait.await;
            cx.update(|cx| finish_send(&model, server, channel, content, reply_to, nonce, pending, encrypted, cx));
        }
    })
    .detach();
    SendOutcome::Sent { nonce }
}

#[allow(clippy::too_many_arguments)]
fn finish_send(
    model: &Model,
    server: ServerId,
    channel: ChannelId,
    content: SharedString,
    reply_to: i64,
    nonce: SharedString,
    pending: Vec<SharedString>,
    encrypted: bool,
    cx: &mut App,
) {
    let composer = model.chat.composer.read(cx);
    let ready: Vec<&crate::chat::composer::PendingAttachment> =
        pending.iter().filter_map(|id| composer.get(id)).filter(|p| p.status == crate::chat::composer::PendingStatus::Ready).collect();
    let attachment_ids: Vec<i64> = ready.iter().filter_map(|p| p.attachment_id).collect();
    // Encrypted channel: each sealed upload's real metadata + key goes in the envelope.
    let sealed: Vec<api::channels::EncryptedAttachmentArg> = ready
        .iter()
        .filter_map(|p| p.sealed.as_ref())
        .filter(|m| attachment_ids.contains(&m.id))
        .map(|m| api::channels::EncryptedAttachmentArg {
            id: m.id,
            key_b64: m.key_b64.clone(),
            filename: m.filename.clone(),
            mime: m.mime.clone(),
            size_bytes: m.size_bytes,
            width: m.width,
            height: m.height,
            duration_ms: m.duration_ms,
            placeholder: m.placeholder.clone(),
            chunk_bytes: m.chunk_bytes,
            thumbnail_sizes_mask: m.thumbnail_sizes_mask,
        })
        .collect();
    let slice = slice_of(model, server, &channel, cx);
    // Nothing left to send: every upload failed and there's no text.
    if content.is_empty() && attachment_ids.is_empty() {
        if let Some(slice) = &slice {
            slice.update(cx, |s, cx| s.remove_by_nonce(&nonce, cx));
        }
        if !pending.is_empty() {
            model.toasts.update(cx, |t, cx| t.error("Attachment upload failed", None, cx));
        }
        model.chat.composer.update(cx, |c, cx| {
            for id in &pending {
                c.remove(id, cx);
            }
        });
        return;
    }
    let args = api::channels::SendChannelMessageArgs {
        server_id: server.wire(),
        channel_id: channel.to_string(),
        message: content.to_string(),
        attachment_ids: (!attachment_ids.is_empty()).then_some(attachment_ids),
        nonce: Some(nonce.to_string()),
        reply_to: (reply_to > 0).then_some(reply_to),
        encrypted: Some(encrypted),
        encrypted_attachments: (!sealed.is_empty()).then_some(sealed),
    };
    let model = model.clone();
    pace(
        &model.chat.pacing.clone(),
        server,
        Box::new(move |cx| {
            call_in_order(&model.clone(), server, api::channels::send_channel_message(args), move |r, cx| {
                match r {
                    Ok(()) => {
                        watch_echo(server, channel.clone(), nonce.clone(), cx);
                        // Went out with whatever uploaded; say which didn't.
                        let failed = pending
                            .iter()
                            .filter(|id| {
                                model.chat.composer.read(cx).get(id).is_some_and(|p| p.status == crate::chat::composer::PendingStatus::Failed)
                            })
                            .count();
                        if failed > 0 {
                            let title = format!("{failed} attachment{} failed to upload", if failed > 1 { "s" } else { "" });
                            model.toasts.update(cx, |t, cx| t.error(title, None, cx));
                        }
                        model.chat.composer.update(cx, |c, cx| {
                            for id in &pending {
                                c.remove(id, cx);
                            }
                        });
                    }
                    Err(e) => {
                        log::error!("send_channel_message: {e}");
                        // Never delivered: no phantom bubble; the text comes back for a retry
                        // (queued attachments stay).
                        if let Some(slice) = slice_of(&model, server, &channel, cx) {
                            slice.update(cx, |s, cx| s.remove_by_nonce(&nonce, cx));
                        }
                        model.toasts.update(cx, |t, cx| t.error("Failed to send message", None, cx));
                        if !content.is_empty() {
                            model.chat.drafts.update(cx, |d, cx| {
                                let key = DraftKey::Channel(server, channel.clone());
                                d.set(key.clone(), content.clone(), cx);
                                cx.emit(crate::chat::drafts::DraftsEvent::Restored(key));
                            });
                        }
                        model.chat.composer.update(cx, |c, cx| c.claim(&pending, false, cx));
                    }
                }
            }, cx);
        }),
        cx,
    );
}

/// A picked GIF as a message of its own: its URL is the text (`sendGif`). The typed draft
/// and queued attachments stay.
pub fn send_gif(server: ServerId, channel: ChannelId, url: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
    let Some(model) = model(cx) else { return SendOutcome::Nothing };
    let (own, username) = {
        let a = model.auth.read(cx);
        (a.uid, a.username.clone())
    };
    let Some(username) = username else { return SendOutcome::Nothing };
    let slice = ensure_slice(&model, server, &channel, cx);
    if slice.read(cx).list_windowed() {
        jump_to_present(server, channel, cx);
        return SendOutcome::JumpedToPresent;
    }
    let key = (server, channel.clone());
    let interval = slowmode_interval(&model, server, &channel, cx);
    let now = cx.background_executor().now();
    if model.chat.slowmode.read(cx).holds(&key, interval, slice.read(cx).rows(), own, now) {
        model.chat.slowmode.update(cx, |s, cx| s.nudge(key, cx));
        return SendOutcome::HeldBySlowmode;
    }
    let content: SharedString = url.to_string().into();
    let nonce = new_nonce();
    let mut bubble = Message::optimistic(username, own, content.clone(), now_secs(), nonce.clone(), reply_to, slice.read(cx).newest_stored_id());
    bubble.channel = channel.clone();
    slice.update(cx, |s, cx| s.push_bubble(Arc::new(bubble), cx));
    let args = api::channels::SendChannelMessageArgs {
        server_id: server.wire(),
        channel_id: channel.to_string(),
        message: content.to_string(),
        attachment_ids: None,
        nonce: Some(nonce.to_string()),
        reply_to: (reply_to > 0).then_some(reply_to),
        encrypted: Some(channel_encrypted(&model, server, &channel, cx)),
        encrypted_attachments: None,
    };
    let n = nonce.clone();
    pace(
        &model.chat.pacing.clone(),
        server,
        Box::new(move |cx| {
            call_in_order(&model.clone(), server, api::channels::send_channel_message(args), move |r, cx| match r {
                Ok(()) => watch_echo(server, channel, n, cx),
                Err(e) => {
                    log::error!("send_channel_message (gif): {e}");
                    if let Some(slice) = slice_of(&model, server, &channel, cx) {
                        slice.update(cx, |s, cx| s.remove_by_nonce(&n, cx));
                    }
                    model.toasts.update(cx, |t, cx| t.error("Failed to send GIF", None, cx));
                }
            }, cx);
        }),
        cx,
    );
    SendOutcome::Sent { nonce }
}

/// The echo watchdog of a send (`watchEcho`): exists while the bubble is pending (the slice
/// holds it). After 30 s without the echo the server is asked first (the rows posted since,
/// ours carrying the nonce, or our text from a server that forgot it), since withdrawing
/// on a guess made people send stored messages twice. Only a send the server doesn't have
/// is withdrawn, its text back in the message box. While the community is unreachable the
/// check waits for the reconnect, whose re-sync usually settles the bubble first.
pub fn watch_echo(server: ServerId, channel: ChannelId, nonce: SharedString, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(slice) = slice_of(&model, server, &channel, cx) else { return };
    let task = cx.spawn({
        let (model, slice, nonce) = (model.clone(), slice.clone(), nonce.clone());
        async move |cx| {
            cx.background_executor().timer(ECHO_TIMEOUT).await;
            let pending = |cx: &mut gpui::AsyncApp| cx.update(|cx| slice.read(cx).bubble(&nonce).filter(|m| m.pending).cloned());
            loop {
                if pending(cx).is_none() {
                    return;
                }
                if cx.update(|cx| model.connections.read(cx).is_connected(server)) {
                    break;
                }
                let back = cx.update(|cx| model.chat.channels.update(cx, |c, _| c.wait_for_reconnect(server)));
                if back.await.is_err() {
                    return;
                }
            }
            let Some(sent) = pending(cx) else { return };
            let key = WaitKey::Channel(server, channel.clone());
            ask_server(cx, key, sent.sent_after_id, &|cx| pending(cx).is_none()).await;
            let Some(bubble) = pending(cx) else { return };
            cx.update(|cx| {
                slice.update(cx, |s, cx| s.remove_by_nonce(&nonce, cx));
                let restored = model
                    .chat
                    .drafts
                    .update(cx, |d, cx| d.restore(DraftKey::Channel(server, channel.clone()), bubble.content.clone(), cx));
                let body = if restored {
                    "The server didn't confirm it. Your text is back in the message box."
                } else {
                    "The server didn't confirm it."
                };
                model.toasts.update(cx, |t, cx| t.error("Message not sent", Some(body.into()), cx));
            });
        }
    });
    slice.update(cx, |s, _| s.set_echo_watchdog(nonce, task));
}

/// Page through what was stored after `sent_after_id` until the bubble is settled, the
/// server has nothing newer, or no answer comes (`askServer`).
pub(crate) async fn ask_server(
    cx: &mut gpui::AsyncApp,
    key: WaitKey,
    sent_after_id: i64,
    settled: &dyn Fn(&mut gpui::AsyncApp) -> bool,
) {
    let mut after = sent_after_id;
    for _ in 0..ECHO_CHECK_PAGES {
        let req = if after > 0 { HistoryRequest::after(after, ECHO_PAGE_LIMIT) } else { HistoryRequest::latest(ECHO_PAGE_LIMIT) };
        let reply = cx.update(|cx| request_history(key.clone(), req, cx)).await;
        if settled(cx) {
            return;
        }
        let Some(reply) = reply else { return };
        if after == 0 || !reply.has_more_after || reply.newest_id <= after {
            return;
        }
        after = reply.newest_id;
    }
}

/// `encryptedAttachmentArgs`: an edit re-seals the whole body, so the message's sealed
/// attachments ride along again.
fn encrypted_attachment_args(atts: &[Attachment]) -> Option<Vec<api::channels::EncryptedAttachmentArg>> {
    let out: Vec<_> = atts
        .iter()
        .filter_map(|a| {
            let key = a.key_b64.as_ref().filter(|_| a.encrypted)?;
            Some(api::channels::EncryptedAttachmentArg {
                id: a.id,
                key_b64: key.to_string(),
                filename: a.filename.to_string(),
                mime: a.mime.to_string(),
                size_bytes: a.size_bytes,
                width: a.width,
                height: a.height,
                duration_ms: a.duration_ms,
                placeholder: a.placeholder.to_string(),
                chunk_bytes: if a.chunk_bytes > 0 { a.chunk_bytes } else { decibell_attachments::crypto::CHUNK_BYTES as u32 },
                thumbnail_sizes_mask: a.thumbnail_sizes_mask,
            })
        })
        .collect();
    (!out.is_empty()).then_some(out)
}

/// Edit one of our messages (`submitEdit`): not optimistic, the broadcast applies it; paced
/// like sends (edits draw from the same bucket). An unchanged text costs nothing.
pub fn edit_message(server: ServerId, channel: ChannelId, id: i64, content: &str, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(message) = slice_of(&model, server, &channel, cx).and_then(|s| s.read(cx).find(id).cloned()) else { return };
    if *message.content == *content {
        return;
    }
    let args = api::channels::EditChannelMessageArgs {
        server_id: server.wire(),
        channel_id: channel.to_string(),
        message_id: id,
        content: content.to_string(),
        encrypted: Some(channel_encrypted(&model, server, &channel, cx)),
        encrypted_attachments: encrypted_attachment_args(&message.attachments),
    };
    let m = model.clone();
    pace(
        &model.chat.pacing.clone(),
        server,
        Box::new(move |cx| {
            call_in_order(&m, server, api::channels::edit_channel_message(args), |r, _| {
                if let Err(e) = r {
                    log::error!("edit_channel_message: {e}");
                }
            }, cx)
        }),
        cx,
    );
}

/// ArrowUp on an empty composer: the latest message of ours the user may edit.
pub fn latest_own_editable(server: ServerId, channel: &str, cx: &App) -> Option<i64> {
    let model = model(cx)?;
    let slice = slice_of(&model, server, channel, cx)?;
    let s = slice.read(cx);
    s.rows().iter().zip(s.facts()).rev().find(|(_, f)| f.can_edit).map(|(m, _)| m.id)
}

/// Whether the local user may delete others' messages here (`useCanDeleteOthers`: the
/// server-wide Manage Messages, as the renderer checks it).
pub fn can_delete_others(server: ServerId, cx: &App) -> bool {
    model(cx).is_some_and(|m| m.has_permission(server, perm::MANAGE_MESSAGES, cx))
}

/// Delete a message (`handleDeleteChannelMessage`): it goes at once; the server's answer
/// confirms (or the row comes back with the reason); 5 s without one puts it back.
pub fn delete_message(server: ServerId, channel: ChannelId, id: i64, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(slice) = slice_of(&model, server, &channel, cx) else { return };
    let watchdog = cx.spawn({
        let (model, slice) = (model.clone(), slice.downgrade());
        async move |cx| {
            cx.background_executor().timer(DELETE_WATCHDOG).await;
            cx.update(|cx| {
                let Some(slice) = slice.upgrade() else { return };
                if slice.update(cx, |s, cx| s.is_deleting(id) && s.restore_pending_deletion(id, cx)) {
                    model.toasts.update(cx, |t, cx| t.error("Delete timed out", Some("Couldn't reach the server. Please try again.".into()), cx));
                }
            });
        }
    });
    if slice.update(cx, |s, cx| s.snapshot_and_remove(id, Some(watchdog), cx)).is_none() {
        return;
    }
    let args = api::channels::DeleteChannelMessageArgs { server_id: server.wire(), channel_id: channel.to_string(), message_id: id };
    call(cx, api::channels::delete_channel_message(args), move |r, cx| {
        if let Err(e) = r {
            log::error!("delete_channel_message: {e}");
            slice.update(cx, |s, cx| s.restore_pending_deletion(id, cx));
            model.toasts.update(cx, |t, cx| t.error("Failed to delete message", Some("Please try again.".into()), cx));
        }
    });
}

// ── Composer ────────────────────────────────────────────────────────────────

/// Attach a file to a channel's message box (`queueUpload`): probed, queued; bytes leave
/// with the send. Past the per-message cap: a toast.
pub fn attach_file(server: ServerId, channel: ChannelId, source: UploadSource, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.chat.composer.read(cx).for_channel(server, &channel).count() >= crate::chat::composer::MAX_ATTACHMENTS_PER_MESSAGE {
        let body = format!(
            "You can attach up to {} files per message. Send this one first, then add more.",
            crate::chat::composer::MAX_ATTACHMENTS_PER_MESSAGE
        );
        model.toasts.update(cx, |t, cx| t.error("Attachment limit reached", Some(body.into()), cx));
        return;
    }
    let encrypted = channel_encrypted(&model, server, &channel, cx);
    let service = model.chat.composer.read(cx).service().clone();
    let prepared = service.prepare(source, cx);
    cx.spawn(async move |cx| {
        let file = prepared.await;
        cx.update(|cx| match file {
            Ok(file) => {
                let added = model.chat.composer.update(cx, |c, cx| c.add(server, channel, file, encrypted, cx));
                if added.is_err() {
                    model.toasts.update(cx, |t, cx| t.error("Attachment limit reached", None, cx));
                }
            }
            Err(e) => log::error!("file register: {e}"),
        });
    })
    .detach();
}

/// The composer's text changed (the drafts store is the one owner of the draft).
pub fn set_draft(server: ServerId, channel: ChannelId, text: SharedString, cx: &mut App) {
    if let Some(model) = model(cx) {
        model.chat.drafts.update(cx, |d, cx| d.set(DraftKey::Channel(server, channel), text, cx));
    }
}

/// Whether the server view shows `(server, channel)` right now.
pub fn is_on_screen(server: ServerId, channel: &str, cx: &App) -> bool {
    model(cx).is_some_and(|m| {
        let nav = m.nav.read(cx);
        nav.active_view == ActiveView::Server && nav.active_channel_key().is_some_and(|(s, c)| s == server && &**c == channel)
    })
}

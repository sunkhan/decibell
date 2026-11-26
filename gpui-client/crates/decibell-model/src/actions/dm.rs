//! What the DM views ask for (`DmChatPanel.tsx`'s handlers, `historyRequests.ts`'s DM
//! half, `sendPacing.ts`'s `watchDmEcho`). DM sends are not paced (central paces itself)
//! but go out one after another, in order (`sendChainRef`).

use std::sync::Arc;
use std::time::Duration;

use decibell_core::api;
use futures::FutureExt as _;
use gpui::{App, SharedString, Task};

use crate::Model;
use crate::actions::chat::{ECHO_TIMEOUT, SendOutcome, ask_server, new_nonce, request_history, DELETE_WATCHDOG};
use crate::call::call;
use crate::chat::drafts::DraftKey;
use crate::chat::history::{AROUND_LIMIT, HistoryRequest, LATEST_ATTEMPTS, LatestLoad, PAGE_LIMIT, RETRY_DELAY, WaitKey};
use crate::chat::message::Message;
use crate::chat::slice::MessageSlice;
use crate::ids::Uid;
use crate::stores::connections::CentralStatus;
use crate::stores::navigation::ActiveView;

/// Mark-read waits this long after the conversation shows new rows (`250` ms).
pub const MARK_READ_DELAY: Duration = Duration::from_millis(250);

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

fn slice_of(model: &Model, peer: Uid, cx: &App) -> Option<gpui::Entity<MessageSlice>> {
    model.dm.list.read(cx).slice(peer).cloned()
}

fn peer_name(model: &Model, peer: Uid, cx: &App) -> String {
    let fallback = model.dm.list.read(cx).get(peer).map(|c| c.username.clone()).unwrap_or_default();
    model.name_of(peer, &fallback, cx).to_string()
}

fn central_signed_in(model: &Model, cx: &App) -> bool {
    model.connections.read(cx).central == CentralStatus::Connected && model.auth.read(cx).is_authenticated
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// Open a conversation (the friends list, a profile's Message button): the DM view with
/// `peer` active, the conversation created when new.
pub fn open_dm(peer: Uid, name: SharedString, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.dm.list.update(cx, |l, cx| {
        l.ensure(peer, name, cx);
        l.set_active(Some(peer), cx);
    });
    model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Dm, cx));
}

/// A conversation's newest page (`loadLatestDmPage`), shared by concurrent callers; up to
/// three attempts while central is signed in.
pub fn load_latest(peer: Uid, cx: &mut App) -> LatestLoad {
    let model = Model::global(cx).clone();
    let key = WaitKey::Dm(peer);
    if let Some(run) = model.chat.history.read(cx).latest.get(&key) {
        return run.clone();
    }
    let run = cx
        .spawn({
            let (model, key) = (model.clone(), key.clone());
            async move |cx| {
                let mut ok = false;
                for attempt in 0..LATEST_ATTEMPTS {
                    if attempt > 0 {
                        cx.background_executor().timer(RETRY_DELAY).await;
                    }
                    if !cx.update(|cx| central_signed_in(&model, cx)) {
                        break;
                    }
                    let reply = cx.update(|cx| request_history(key.clone(), HistoryRequest::latest(PAGE_LIMIT), cx));
                    if reply.await.is_some() {
                        ok = true;
                        break;
                    }
                }
                cx.update(|cx| model.chat.history.update(cx, |h, _| h.latest.remove(&key)));
                ok
            }
        })
        .shared();
    model.chat.history.update(cx, |h, _| h.latest.insert(key, run.clone()));
    run
}

/// Showing a conversation: its newest page unless the server's history is in already.
pub fn open_history(peer: Uid, cx: &mut App) -> Option<LatestLoad> {
    let model = model(cx)?;
    let loaded = slice_of(&model, peer, cx).is_some_and(|s| s.read(cx).fetched);
    (!loaded).then(|| load_latest(peer, cx))
}

fn detached<T: 'static>(cx: &mut App, work: impl AsyncFnOnce(&mut gpui::AsyncApp) -> T + 'static) -> Task<Option<T>> {
    let (tx, rx) = futures::channel::oneshot::channel();
    cx.spawn(async move |cx| {
        let _ = tx.send(work(cx).await);
    })
    .detach();
    cx.spawn(async move |_| rx.await.ok())
}

/// Scroll-up paging (`maybeLoadOlderHistory`); waits for the newest page first.
pub fn load_older(peer: Uid, cx: &mut App) -> Option<Task<Option<bool>>> {
    let model = model(cx)?;
    let slice = slice_of(&model, peer, cx)?;
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
    let reply = request_history(WaitKey::Dm(peer), HistoryRequest::before(before, PAGE_LIMIT), cx);
    Some(detached(cx, async move |cx| {
        let ok = reply.await.is_some();
        if !ok {
            cx.update(|cx| slice.update(cx, |s, _| if s.last_before == before { s.last_before = 0 }));
        }
        ok
    }))
}

/// Downward paging while windowed (`maybeLoadNewerHistory`).
pub fn load_newer(peer: Uid, cx: &mut App) -> Option<Task<Option<bool>>> {
    let model = model(cx)?;
    let slice = slice_of(&model, peer, cx)?;
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
    let reply = request_history(WaitKey::Dm(peer), HistoryRequest::after(after, PAGE_LIMIT), cx);
    Some(detached(cx, async move |cx| {
        let ok = reply.await.is_some();
        if !ok {
            cx.update(|cx| slice.update(cx, |s, _| if s.last_after == after { s.last_after = 0 }));
        }
        ok
    }))
}

/// "Jump to present" in a conversation (`resetDmForJump` + the newest page).
pub fn jump_to_present(peer: Uid, cx: &mut App) -> Option<LatestLoad> {
    let model = model(cx)?;
    let slice = slice_of(&model, peer, cx)?;
    slice.update(cx, |s, cx| s.dm_reset_for_jump(cx));
    Some(load_latest(peer, cx))
}

/// Jump to a DM (a reply's parent): loaded, or a window around it.
pub fn jump_to_message(peer: Uid, id: i64, cx: &mut App) -> Option<crate::actions::chat::JumpStart> {
    use crate::actions::chat::JumpStart;
    let model = model(cx)?;
    let slice = slice_of(&model, peer, cx)?;
    let start = slice.update(cx, |s, _| {
        if s.find(id).is_some() {
            return Some(JumpStart::Loaded);
        }
        if s.pending_jump == Some(id) {
            return Some(JumpStart::AlreadyFetching);
        }
        s.pending_jump = Some(id);
        s.last_before = 0;
        s.last_after = 0;
        None
    });
    if let Some(start) = start {
        return Some(start);
    }
    let reply = request_history(WaitKey::Dm(peer), HistoryRequest::around(id, AROUND_LIMIT), cx);
    Some(JumpStart::Fetching(detached(cx, async move |cx| {
        let reply = reply.await;
        cx.update(|cx| {
            if slice.read(cx).pending_jump != Some(id) {
                return false;
            }
            let here = model.dm.list.read(cx).active_peer == Some(peer);
            if reply.is_some() && here && slice.read(cx).find(id).is_some() {
                return true;
            }
            slice.update(cx, |s, _| s.pending_jump = None);
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

/// The list landed on the jump target (`handleJumpLanded`: `pendingJumpIdRef = null`).
pub fn jump_landed(peer: Uid, cx: &mut App) {
    if let Some(slice) = model(cx).and_then(|m| slice_of(&m, peer, cx)) {
        slice.update(cx, |s, _| s.pending_jump = None);
    }
}

/// After central signed us in again, the conversation on screen catches up (`DmChatPanel`'s
/// resync effect). `at_bottom`: the list's live position.
pub fn resync_displayed(peer: Uid, at_bottom: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(slice) = slice_of(&model, peer, cx) else { return };
    let stored = slice.update(cx, |s, cx| {
        s.last_before = 0;
        s.last_after = 0;
        s.pending_jump = None;
        let stored = s.rows().iter().any(|m| !m.preview && m.id > 0);
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
    let run = load_latest(peer, cx);
    cx.spawn(async move |cx| {
        run.await;
        if stored {
            cx.update(|cx| slice.update(cx, |s, cx| s.set_resyncing(false, cx)));
        }
    })
    .detach();
}

/// The conversation is on screen with something unread or newer than our marker: read it
/// up to its newest row now, and tell central 250 ms later (`DmChatPanel`'s mark-read
/// effect; call it where the panel observes its slice). Each call sends its own marker, as
/// the renderer's did (central keeps the greatest).
pub fn mark_read(peer: Uid, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let latest = model.dm.list.read(cx).newest_loaded_id(peer, cx);
    if latest == 0 {
        return;
    }
    if !model.dm.list.update(cx, |l, cx| l.mark_read(peer, latest, cx)) {
        return;
    }
    let name = peer_name(&model, peer, cx);
    cx.spawn(async move |cx| {
        cx.background_executor().timer(MARK_READ_DELAY).await;
        cx.update(|cx| {
            let args = api::dm::MarkDmReadArgs { peer: name, peer_uid: peer, up_to_id: latest };
            call(cx, api::dm::mark_dm_read(args), |r, _| {
                if let Err(e) = r {
                    log::error!("mark_dm_read: {e}");
                }
            });
        });
    })
    .detach();
}

/// Send a DM (`handleSend`): the bubble at once, then the send after the previous one
/// finished (in order); a failure withdraws the bubble and puts the text back unless
/// something new was typed. `Err` carries the failure text the composer shows.
pub fn send_dm(peer: Uid, text: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
    send(peer, text.trim(), reply_to, false, cx)
}

/// A picked GIF to a conversation (`sendGif`): the draft stays.
pub fn send_gif(peer: Uid, url: &str, reply_to: i64, cx: &mut App) -> SendOutcome {
    send(peer, url, reply_to, true, cx)
}

fn send(peer: Uid, content: &str, reply_to: i64, gif: bool, cx: &mut App) -> SendOutcome {
    let Some(model) = model(cx) else { return SendOutcome::Nothing };
    if content.is_empty() || peer <= 0 {
        return SendOutcome::Nothing;
    }
    // Windowed: snap to the present first; the next send goes through.
    if slice_of(&model, peer, cx).is_some_and(|s| s.read(cx).list_windowed()) {
        jump_to_present(peer, cx);
        return SendOutcome::JumpedToPresent;
    }
    let content: SharedString = content.to_string().into();
    let nonce = new_nonce();
    let (own, username) = {
        let a = model.auth.read(cx);
        (a.uid, a.username.clone())
    };
    let name = peer_name(&model, peer, cx);
    if let Some(username) = username {
        let sent_after = slice_of(&model, peer, cx).map_or(0, |s| s.read(cx).newest_stored_id());
        let bubble = Arc::new(Message::optimistic(username, own, content.clone(), now_secs(), nonce.clone(), reply_to, sent_after));
        model.dm.list.update(cx, |l, cx| l.add_message(peer, bubble, true, name.clone().into(), true, cx));
    }
    if !gif {
        model.chat.drafts.update(cx, |d, cx| d.clear(&DraftKey::Dm(peer), cx));
    }
    let args = api::messaging::SendPrivateMessageArgs {
        recipient: name,
        recipient_uid: Some(peer),
        message: content.to_string(),
        reply_to: (reply_to > 0).then_some(reply_to),
        nonce: Some(nonce.to_string()),
    };
    // One after another: this send waits for the previous one's call.
    let previous = model.dm.list.update(cx, |l, _| l.send_chain.take());
    let (tx, rx) = futures::channel::oneshot::channel::<()>();
    model.dm.list.update(cx, |l, _| l.send_chain = Some(rx.map(|_| ()).boxed_local().shared()));
    let n = nonce.clone();
    if !gif {
        model.dm.list.update(cx, |l, cx| l.set_send_error(None, cx));
    }
    cx.spawn(async move |cx| {
        if let Some(previous) = previous {
            previous.await;
        }
        let result = if crate::core::is_running() {
            api::messaging::send_private_message(args).await.map_err(crate::error::UiError::from)
        } else {
            Err(crate::error::UiError::not_running())
        };
        let _ = tx.send(());
        cx.update(|cx| match result {
            Ok(()) => watch_echo(peer, n, cx),
            Err(e) => {
                log::error!("send_private_message: {e}");
                let bubble = model.dm.list.update(cx, |l, cx| l.remove_by_nonce(peer, &n, cx));
                if gif {
                    model.toasts.update(cx, |t, cx| t.error("Failed to send GIF", None, cx));
                    return;
                }
                // Shown under the composer (`setSendError(String(err))`).
                model.dm.list.update(cx, |l, cx| l.set_send_error(Some((peer, e.js_string())), cx));
                // The text back, unless something new was typed since.
                if let Some(bubble) = bubble {
                    model.chat.drafts.update(cx, |d, cx| d.restore(DraftKey::Dm(peer), bubble.content.clone(), cx));
                }
            }
        });
    })
    .detach();
    SendOutcome::Sent { nonce }
}

/// The DM echo watchdog (`watchDmEcho`): central echoes our DIRECT_MSG with the nonce, or
/// answers an error with it. Neither within 30 s → the same check against the
/// conversation's history (DM rows carry no nonce: our text settles it), then withdraw.
pub fn watch_echo(peer: Uid, nonce: SharedString, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(slice) = slice_of(&model, peer, cx) else { return };
    let task = cx.spawn({
        let (model, slice, nonce) = (model.clone(), slice.clone(), nonce.clone());
        async move |cx| {
            cx.background_executor().timer(ECHO_TIMEOUT).await;
            let pending = |cx: &mut gpui::AsyncApp| cx.update(|cx| slice.read(cx).bubble(&nonce).filter(|m| m.pending).cloned());
            // Central offline: the sign-in's re-sync usually settles it; ask after it.
            while pending(cx).is_some() && !cx.update(|cx| central_signed_in(&model, cx)) {
                let back = cx.update(|cx| model.dm.list.update(cx, |l, _| l.wait_for_resync()));
                if back.await.is_err() {
                    return;
                }
            }
            let Some(sent) = pending(cx) else { return };
            ask_server(cx, WaitKey::Dm(peer), sent.sent_after_id, &|cx| pending(cx).is_none()).await;
            let Some(bubble) = pending(cx) else { return };
            cx.update(|cx| {
                model.dm.list.update(cx, |l, cx| l.remove_by_nonce(peer, &nonce, cx));
                let restored = model.chat.drafts.update(cx, |d, cx| d.restore(DraftKey::Dm(peer), bubble.content.clone(), cx));
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

/// Edit one of our DMs (`submitEdit`): not optimistic, the broadcast applies it.
pub fn edit_dm(peer: Uid, id: i64, content: &str, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let Some(message) = slice_of(&model, peer, cx).and_then(|s| s.read(cx).find(id).cloned()) else { return };
    if *message.content == *content {
        return;
    }
    let args = api::dm::EditDmMessageArgs { peer: peer_name(&model, peer, cx), peer_uid: peer, message_id: id, content: content.to_string() };
    call(cx, api::dm::edit_dm_message(args), |r, _| {
        if let Err(e) = r {
            log::error!("edit_dm_message: {e}");
        }
    });
}

/// Delete one of our DMs (`handleDeleteDmMessage`): optimistic, with the 5 s watchdog.
pub fn delete_dm(peer: Uid, id: i64, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let watchdog = cx.spawn({
        let model = model.clone();
        async move |cx| {
            cx.background_executor().timer(DELETE_WATCHDOG).await;
            cx.update(|cx| {
                let pending = model.dm.list.read(cx).slice(peer).is_some_and(|s| s.read(cx).is_deleting(id));
                if pending && model.dm.list.update(cx, |l, cx| l.restore_pending_deletion(peer, id, cx)) {
                    model.toasts.update(cx, |t, cx| t.error("Delete timed out", Some("Couldn't reach the server. Please try again.".into()), cx));
                }
            });
        }
    });
    if model.dm.list.update(cx, |l, cx| l.snapshot_and_remove(peer, id, Some(watchdog), cx)).is_none() {
        return;
    }
    let args = api::dm::DeleteDmMessageArgs { peer: peer_name(&model, peer, cx), peer_uid: peer, message_id: id };
    call(cx, api::dm::delete_dm_message(args), move |r, cx| {
        if let Err(e) = r {
            log::error!("delete_dm_message: {e}");
            model.dm.list.update(cx, |l, cx| l.restore_pending_deletion(peer, id, cx));
            model.toasts.update(cx, |t, cx| t.error("Failed to delete message", Some("Please try again.".into()), cx));
        }
    });
}

/// Settings → Privacy's "Only friends can DM me" (`PrivacyTab.tsx` `handleToggleDms`): the
/// account's value at central (`set_dm_privacy`) and the local copy in `config.json` (which
/// only shows before sign-in). Its one other writer is the sign-in adopting the account's
/// value (`events::presence::own_presence`); `Settings::friends_only_dms` is the one copy.
pub fn set_friends_only_dms(friends_only: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.settings.read(cx).get().friends_only_dms != friends_only {
        model.settings.update(cx, |s, cx| s.change(|v| v.friends_only_dms = friends_only, cx));
    }
    call(cx, api::settings::set_dm_privacy(api::settings::SetDmPrivacyArgs { friends_only }), |r, _| {
        if let Err(e) = r {
            log::error!("set_dm_privacy: {e}");
        }
    });
}

/// The composer's text changed.
pub fn set_draft(peer: Uid, text: SharedString, cx: &mut App) {
    if let Some(model) = model(cx) {
        model.chat.drafts.update(cx, |d, cx| d.set(DraftKey::Dm(peer), text, cx));
    }
}

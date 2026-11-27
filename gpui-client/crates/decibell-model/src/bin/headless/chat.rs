//! `decibell-headless`'s message steps (M2): history, sends, edits, deletes, DMs, search,
//! and waits on what other clients do. The community is the open one (`open-server`);
//! channels and users are named as people see them.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use decibell_model::Model;
use decibell_model::actions::chat::{self as chat_actions, JumpStart, SendOutcome};
use decibell_model::actions::dm as dm_actions;
use decibell_model::chat::history::{HistoryRequest, PAGE_LIMIT, WaitKey};
use decibell_model::chat::search::{SearchQuery, SearchStatus};
use decibell_model::chat::slice::MessageSlice;
use decibell_model::ids::{ChannelId, ServerId, Uid};
use decibell_model::stores::community::ChannelKind;
use gpui::{App, AsyncApp, Entity, SharedString};
use serde_json::{Value, json};

pub const USAGE: &str = "\
Message steps (the open community):
  history <channel> [before [<id>] | around <id> | newer]
                                  show the channel and load a page; waits for the reply
  send <channel> <text>           send (paced); `send-burst <channel> <n> <text>` sends n
  edit <channel> <id|last> <text> edit one of our messages (`last`: the newest one)
  delete <channel> <id|last|mine|prefix:<text>>
                                  delete our message(s): an id, the newest, every loaded one,
                                  or every loaded one starting with <text>
  dm-open <user>                  open the conversation and load its newest page (a <user> of
                                  the DM steps: an account of the stack, `uid:<n>`, or a name
                                  this client knows: a conversation's peer, the users store)
  dm-send <user> <text>           send a DM
  dm-delete <user> <id|last|prefix:<text>>  delete our DM(s)
  dm-read <user>                  read it up to its newest message (mark read); an open
                                  conversation (`dm-open`) counts as being read: a DM arriving
                                  for it isn't unread
  search <query>                  one server-side search (one argument: quote it); waits
  wait-history <channel>          its newest page is in
  wait-message <channel> <text>   a row with that text arrived (another client's message)
  wait-edited <channel> <text>    a row now reads that text
  wait-gone <channel> <text>      no row reads that text any more
  wait-sent <channel>             none of our sends is still pending
  wait-deleted <channel>          every delete of ours was confirmed by the server
  wait-dm-deleted <user>          … in a conversation
  wait-held <channel>             the composer would hold a send (slowmode)
  wait-free <channel>             … and no longer would
  wait-dm <user> <text>           a DM with that text arrived
  wait-dm-gone <user> <text>      no DM reads that text any more
  wait-unread <user> <n>          the conversation shows n unread";

#[derive(Debug, Clone)]
pub enum ChatStep {
    History { channel: String, mode: HistoryMode },
    Send { channel: String, text: String, count: u32 },
    Edit { channel: String, target: String, text: String },
    Delete { channel: String, target: String },
    DmOpen(String),
    DmSend { user: String, text: String },
    DmDelete { user: String, target: String },
    DmRead(String),
    Search(String),
    Wait(ChatWait),
}

#[derive(Debug, Clone)]
pub enum HistoryMode {
    Latest,
    Before(Option<i64>),
    Around(i64),
    Newer,
}

#[derive(Debug, Clone)]
pub enum ChatWait {
    History(String),
    Message(String, String),
    Edited(String, String),
    Gone(String, String),
    Sent(String),
    Deleted(String),
    DmDeleted(String),
    Held(String, bool),
    Dm(String, String),
    DmGone(String, String),
    Unread(String, i64),
}

pub fn is_step(cmd: &str) -> bool {
    matches!(
        cmd,
        "history" | "send" | "send-burst" | "edit" | "delete" | "dm-open" | "dm-send" | "dm-delete" | "dm-read" | "search"
    ) || cmd.starts_with("wait-") && cmd != "wait-file"
}

pub fn parse(cmd: &str, it: &mut std::vec::IntoIter<String>) -> Result<ChatStep, String> {
    let next = |what: &str, it: &mut std::vec::IntoIter<String>| it.next().ok_or(format!("{cmd}: missing {what}"));
    Ok(match cmd {
        "history" => {
            let channel = next("channel", it)?;
            // The mode is optional: look at the next word first.
            let mode = match it.as_slice().first().map(String::as_str) {
                Some("before") => {
                    it.next();
                    let id = it.as_slice().first().and_then(|s| s.parse::<i64>().ok());
                    if id.is_some() {
                        it.next();
                    }
                    HistoryMode::Before(id)
                }
                Some("around") => {
                    it.next();
                    let id = it.next().ok_or("history around: missing id")?;
                    HistoryMode::Around(id.parse().map_err(|_| "history around: id")?)
                }
                Some("newer") => {
                    it.next();
                    HistoryMode::Newer
                }
                _ => HistoryMode::Latest,
            };
            ChatStep::History { channel, mode }
        }
        "send" => ChatStep::Send { channel: next("channel", it)?, text: next("text", it)?, count: 1 },
        "send-burst" => {
            let channel = next("channel", it)?;
            let count = next("count", it)?.parse().map_err(|_| "send-burst: count")?;
            ChatStep::Send { channel, text: next("text", it)?, count }
        }
        "edit" => ChatStep::Edit { channel: next("channel", it)?, target: next("id", it)?, text: next("text", it)? },
        "delete" => ChatStep::Delete { channel: next("channel", it)?, target: next("id", it)? },
        "dm-open" => ChatStep::DmOpen(next("user", it)?),
        "dm-send" => ChatStep::DmSend { user: next("user", it)?, text: next("text", it)? },
        "dm-read" => ChatStep::DmRead(next("user", it)?),
        "dm-delete" => ChatStep::DmDelete { user: next("user", it)?, target: next("id", it)? },
        "search" => ChatStep::Search(next("query", it)?),
        "wait-history" => ChatStep::Wait(ChatWait::History(next("channel", it)?)),
        "wait-message" => ChatStep::Wait(ChatWait::Message(next("channel", it)?, next("text", it)?)),
        "wait-edited" => ChatStep::Wait(ChatWait::Edited(next("channel", it)?, next("text", it)?)),
        "wait-gone" => ChatStep::Wait(ChatWait::Gone(next("channel", it)?, next("text", it)?)),
        "wait-sent" => ChatStep::Wait(ChatWait::Sent(next("channel", it)?)),
        "wait-deleted" => ChatStep::Wait(ChatWait::Deleted(next("channel", it)?)),
        "wait-dm-deleted" => ChatStep::Wait(ChatWait::DmDeleted(next("user", it)?)),
        "wait-held" => ChatStep::Wait(ChatWait::Held(next("channel", it)?, true)),
        "wait-free" => ChatStep::Wait(ChatWait::Held(next("channel", it)?, false)),
        "wait-dm" => ChatStep::Wait(ChatWait::Dm(next("user", it)?, next("text", it)?)),
        "wait-dm-gone" => ChatStep::Wait(ChatWait::DmGone(next("user", it)?, next("text", it)?)),
        "wait-unread" => {
            let user = next("user", it)?;
            ChatStep::Wait(ChatWait::Unread(user, next("n", it)?.parse().map_err(|_| "wait-unread: n")?))
        }
        other => return Err(format!("unknown step {other:?}")),
    })
}

fn stack_uid(account: &str) -> Result<Uid, String> {
    let dev_dir = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache/decibell-dev")))
        .ok_or("no DECIBELL_DEV_DIR and no HOME")?;
    let text = std::fs::read_to_string(dev_dir.join("run/accounts.json")).map_err(|e| e.to_string())?;
    let accounts: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    accounts["accounts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| a["name"] == account)
        .and_then(|a| a["uid"].as_i64())
        .ok_or(format!("{account:?} is not an account of the stack"))
}

/// A user named on the command line: `uid:<n>`, an account of the stack's `accounts.json`, or a
/// name this client already knows (a conversation's peer, the users store: a throwaway account
/// another client registered).
pub fn user_uid(model: &Model, user: &str, cx: &App) -> Result<Uid, String> {
    if let Some(n) = user.strip_prefix("uid:") {
        return n.parse().map_err(|_| format!("{user:?}: not a uid"));
    }
    if let Ok(uid) = stack_uid(user) {
        return Ok(uid);
    }
    let list = model.dm.list.read(cx);
    if let Some(c) = list.conversations().find(|c| &*c.username == user) {
        return Ok(c.peer);
    }
    model
        .users
        .read(cx)
        .iter()
        .find(|(_, name)| &***name == user)
        .map(|(uid, _)| uid)
        .ok_or(format!("{user:?} is neither an account of the stack nor a name this client knows (use uid:<n>)"))
}

fn open_channel(model: &Model, name: &str, cx: &App) -> Result<(ServerId, ChannelId), String> {
    let server = model.nav.read(cx).active_server.ok_or("no community is open")?;
    let community = model.community(server, cx).ok_or("that community has not answered")?;
    let id = community
        .channels
        .read(cx)
        .list()
        .iter()
        .find(|c| &*c.name == name && c.kind == ChannelKind::Text)
        .map(|c| c.id.clone())
        .ok_or(format!("no text channel named {name:?}"))?;
    Ok((server, id))
}

fn slice(model: &Model, server: ServerId, channel: &str, cx: &App) -> Option<Entity<MessageSlice>> {
    model.chat.channels.read(cx).get(server, channel).cloned()
}

async fn until(timeout: Duration, cx: &mut AsyncApp, mut holds: impl FnMut(&mut App) -> bool) -> bool {
    let started = Instant::now();
    loop {
        if cx.update(|cx| holds(cx)) {
            return true;
        }
        if started.elapsed() > timeout {
            return false;
        }
        cx.background_executor().timer(Duration::from_millis(20)).await;
    }
}

fn own_target(s: &MessageSlice, target: &str) -> Result<Vec<i64>, String> {
    let own: Vec<i64> = s.rows().iter().zip(s.facts()).filter(|(m, f)| f.own && m.id > 0).map(|(m, _)| m.id).collect();
    if let Some(prefix) = target.strip_prefix("prefix:") {
        return Ok(s
            .rows()
            .iter()
            .zip(s.facts())
            .filter(|(m, f)| f.own && m.id > 0 && m.content.starts_with(prefix))
            .map(|(m, _)| m.id)
            .collect());
    }
    match target {
        "last" => own.last().map(|id| vec![*id]).ok_or("none of our messages is loaded".into()),
        "mine" => Ok(own),
        id => Ok(vec![id.parse().map_err(|_| format!("not a message id: {id}"))?]),
    }
}

pub async fn run(step: &ChatStep, model: &Model, timeout: Duration, cx: &mut AsyncApp) -> Result<(), String> {
    match step {
        ChatStep::History { channel, mode } => {
            let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
            // As the panel showing it: the active channel, its list mounted (it keeps its own
            // slice bounded).
            cx.update(|cx| {
                model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(id.clone()), cx));
                model.chat.channels.update(cx, |c, _| c.set_displayed(Some((server, id.clone()))));
            });
            let answered = match mode {
                HistoryMode::Latest => cx.update(|cx| chat_actions::load_latest(server, id.clone(), cx)).await,
                HistoryMode::Before(Some(before)) => {
                    let reply = cx.update(|cx| {
                        chat_actions::request_history(WaitKey::Channel(server, id.clone()), HistoryRequest::before(*before, PAGE_LIMIT), cx)
                    });
                    reply.await.is_some()
                }
                HistoryMode::Before(None) => match cx.update(|cx| chat_actions::load_older(server, id.clone(), cx)) {
                    Some(task) => task.await.unwrap_or(false),
                    None => return Err("history before: nothing older to load (or not loaded yet)".into()),
                },
                HistoryMode::Newer => match cx.update(|cx| chat_actions::load_newer(server, id.clone(), cx)) {
                    Some(task) => task.await.unwrap_or(false),
                    None => return Err("history newer: the slice is at the present".into()),
                },
                HistoryMode::Around(target) => match cx.update(|cx| chat_actions::jump_to_message(server, id.clone(), *target, cx)) {
                    Some(JumpStart::Fetching(task)) => task.await.unwrap_or(false),
                    Some(_) => true,
                    None => false,
                },
            };
            if !answered {
                let toasts: Vec<String> = cx.update(|cx| model.toasts.read(cx).toasts().iter().map(|t| t.title.to_string()).collect());
                return Err(format!("history {channel} {mode:?}: no page landed (toasts: {toasts:?})"));
            }
            cx.update(|cx| chat_actions::jump_landed(server, &id, cx));
        }
        ChatStep::Send { channel, text, count } => {
            let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
            for n in 0..*count {
                let text = if *count > 1 { format!("{text} {}", n + 1) } else { text.clone() };
                let outcome = cx.update(|cx| chat_actions::send_message(server, id.clone(), &text, 0, cx));
                match outcome {
                    SendOutcome::Sent { nonce } => crate::say(format!("SENT {channel} {nonce} {text}")),
                    other => crate::say(format!("NOT-SENT {channel} {other:?}")),
                }
            }
        }
        ChatStep::Edit { channel, target, text } => {
            let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
            let ids = cx.update(|cx| {
                let s = slice(model, server, &id, cx).ok_or("channel not loaded")?;
                own_target(s.read(cx), target)
            })?;
            for message in ids {
                cx.update(|cx| chat_actions::edit_message(server, id.clone(), message, text, cx));
            }
        }
        ChatStep::Delete { channel, target } => {
            let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
            let ids = cx.update(|cx| {
                let s = slice(model, server, &id, cx).ok_or("channel not loaded")?;
                own_target(s.read(cx), target)
            })?;
            crate::say(format!("DELETING {channel} {}", ids.len()));
            for message in ids {
                cx.update(|cx| chat_actions::delete_message(server, id.clone(), message, cx));
            }
        }
        ChatStep::DmOpen(user) => {
            let peer = cx.update(|cx| user_uid(model, user, cx))?;
            cx.update(|cx| dm_actions::open_dm(peer, user.clone().into(), cx));
            let run = cx.update(|cx| dm_actions::open_history(peer, cx));
            if let Some(run) = run
                && !run.await
            {
                return Err(format!("dm-open {user}: no answer"));
            }
            // A client with no window reads what it opens: the conversation is "being read"
            // (the app's view adds the focus and the list's position to that).
            cx.update(|cx| {
                model.dm.list.update(cx, |l, _| {
                    l.set_displayed(Some(peer));
                    l.set_reading(peer, true);
                })
            });
        }
        ChatStep::DmSend { user, text } => {
            let peer = cx.update(|cx| user_uid(model, user, cx))?;
            let outcome = cx.update(|cx| dm_actions::send_dm(peer, text, 0, cx));
            crate::say(format!("DM-SENT {user} {outcome:?}"));
        }
        ChatStep::DmDelete { user, target } => {
            let peer = cx.update(|cx| user_uid(model, user, cx))?;
            let ids = cx.update(|cx| {
                let s = model.dm.list.read(cx).slice(peer).cloned().ok_or("conversation not loaded")?;
                own_target(s.read(cx), target)
            })?;
            crate::say(format!("DM-DELETING {user} {}", ids.len()));
            for message in ids {
                cx.update(|cx| dm_actions::delete_dm(peer, message, cx));
            }
        }
        ChatStep::DmRead(user) => {
            let peer = cx.update(|cx| user_uid(model, user, cx))?;
            cx.update(|cx| dm_actions::mark_read(peer, cx));
            // Past the 250 ms before the marker leaves.
            cx.background_executor().timer(Duration::from_millis(400)).await;
        }
        ChatStep::Search(query) => {
            let server = cx.update(|cx| model.nav.read(cx).active_server).ok_or("no community is open")?;
            let q = SearchQuery { text: query.clone().into(), filters: vec![] };
            cx.update(|cx| model.chat.search.update(cx, |s, cx| s.run(server, &q, cx)));
            if !until(timeout, cx, |cx| model.chat.search.read(cx).status != SearchStatus::Loading).await {
                return Err("search: no answer".into());
            }
            let status = cx.update(|cx| model.chat.search.read(cx).status);
            if status != SearchStatus::Done {
                let error = cx.update(|cx| model.chat.search.read(cx).error.clone());
                return Err(format!("search failed: {error:?}"));
            }
        }
        ChatStep::Wait(wait) => {
            let ok = match wait {
                ChatWait::History(channel) => {
                    let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
                    until(timeout, cx, |cx| slice(model, server, &id, cx).is_some_and(|s| s.read(cx).fetched && !s.read(cx).loading)).await
                }
                ChatWait::Message(channel, text) | ChatWait::Edited(channel, text) => {
                    let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
                    until(timeout, cx, |cx| {
                        slice(model, server, &id, cx).is_some_and(|s| s.read(cx).rows().iter().any(|m| m.id > 0 && &*m.content == text.as_str()))
                    })
                    .await
                }
                ChatWait::Gone(channel, text) => {
                    let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
                    until(timeout, cx, |cx| {
                        slice(model, server, &id, cx).is_none_or(|s| !s.read(cx).rows().iter().any(|m| &*m.content == text.as_str()))
                    })
                    .await
                }
                ChatWait::Sent(channel) => {
                    let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
                    until(timeout, cx, |cx| slice(model, server, &id, cx).is_none_or(|s| !s.read(cx).rows().iter().any(|m| m.pending))).await
                }
                ChatWait::Deleted(channel) => {
                    let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
                    until(timeout, cx, |cx| slice(model, server, &id, cx).is_none_or(|s| s.read(cx).pending_deletion_count() == 0)).await
                }
                ChatWait::DmDeleted(user) => {
                    let peer = cx.update(|cx| user_uid(model, user, cx))?;
                    until(timeout, cx, |cx| model.dm.list.read(cx).slice(peer).is_none_or(|s| s.read(cx).pending_deletion_count() == 0)).await
                }
                ChatWait::Held(channel, held) => {
                    let (server, id) = cx.update(|cx| open_channel(model, channel, cx))?;
                    until(timeout, cx, |cx| {
                        let key = (server, id.clone());
                        let interval = chat_interval(model, server, &id, cx);
                        let now = cx.background_executor().now();
                        let own = model.auth.read(cx).uid;
                        let rows = slice(model, server, &id, cx).map(|s| s.read(cx).rows().to_vec()).unwrap_or_default();
                        model.chat.slowmode.read(cx).holds(&key, interval, &rows, own, now) == *held
                    })
                    .await
                }
                ChatWait::Dm(user, text) => {
                    until(timeout, cx, |cx| {
                        let Ok(peer) = user_uid(model, user, cx) else { return false };
                        model.dm.list.read(cx).slice(peer).is_some_and(|s| s.read(cx).rows().iter().any(|m| m.id > 0 && &*m.content == text.as_str()))
                    })
                    .await
                }
                ChatWait::DmGone(user, text) => {
                    let peer = cx.update(|cx| user_uid(model, user, cx))?;
                    until(timeout, cx, |cx| {
                        model.dm.list.read(cx).slice(peer).is_none_or(|s| !s.read(cx).rows().iter().any(|m| &*m.content == text.as_str()))
                    })
                    .await
                }
                ChatWait::Unread(user, n) => {
                    until(timeout, cx, |cx| {
                        let Ok(peer) = user_uid(model, user, cx) else { return false };
                        model.dm.list.read(cx).get(peer).is_some_and(|c| c.unread == *n)
                    })
                    .await
                }
            };
            if !ok {
                return Err(format!("timed out after {timeout:?} waiting for {wait:?}"));
            }
        }
    }
    Ok(())
}

fn chat_interval(model: &Model, server: ServerId, channel: &str, cx: &App) -> u32 {
    let Some(community) = model.community(server, cx) else { return 0 };
    let channels = community.channels.read(cx);
    decibell_model::chat::slowmode::interval_for(channels.get(channel), model.is_owner(server, cx))
}

fn rows_json(s: &MessageSlice) -> Value {
    Value::Array(
        s.rows()
            .iter()
            .zip(s.facts())
            .map(|(m, f)| {
                json!({
                    "id": m.id, "senderUid": m.sender_uid, "content": &*m.content, "pending": m.pending,
                    "edited": f.edited, "own": f.own, "grouped": f.grouped, "newDay": f.new_day,
                    "replyTo": m.reply_to(), "attachments": m.attachments.len(), "encrypted": m.encrypted,
                    "decryptError": format!("{:?}", m.decrypt_error),
                    // What the server sent (or the envelope, sealed) about each file.
                    "files": m.attachments.iter().map(|a| json!({
                        "id": a.id, "kind": format!("{:?}", a.kind), "filename": &*a.filename, "mime": &*a.mime,
                        "size": a.size_bytes, "width": a.width, "height": a.height, "durationMs": a.duration_ms,
                        "thumbnailSizesMask": a.thumbnail_sizes_mask, "placeholder": &*a.placeholder,
                        "encrypted": a.encrypted, "key": a.key_b64.is_some(), "chunkBytes": a.chunk_bytes,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect(),
    )
}

/// `dump chat <channel>`, `dump dm <user>`, `dump search`.
pub fn dump(parts: &[String], model: &Model, cx: &App) -> Result<Value, String> {
    let s = |v: &SharedString| v.to_string();
    Ok(match parts[0].as_str() {
        "chat" => {
            let (server, id) = open_channel(model, &parts[1], cx)?;
            let Some(slice) = slice(model, server, &id, cx) else {
                return Ok(json!({ "loaded": false, "cached": model.chat.channels.read(cx).len() }));
            };
            let sl = slice.read(cx);
            let ids: Vec<i64> = sl.rows().iter().map(|m| m.id).filter(|id| *id > 0).collect();
            let unique = {
                let mut d = ids.clone();
                d.dedup();
                d.len() == ids.len()
            };
            json!({
                "loaded": true, "rows": sl.rows().len(), "oldest": sl.oldest_stored_id(), "newest": sl.newest_stored_id(),
                "hasMoreBefore": sl.has_more_before, "hasMoreAfter": sl.has_more_after, "fetched": sl.fetched,
                "loading": sl.loading, "resyncing": sl.resyncing, "revision": sl.revision(),
                "sorted": ids.windows(2).all(|w| w[0] < w[1]),
                "unique": unique,
                "pendingDeletions": sl.pending_deletion_count(),
                "cached": model.chat.channels.read(cx).len(),
                "queued": model.chat.pacing.read(cx).queued(server),
                "historyWaiting": model.chat.history.read(cx).waiting(),
                "messages": rows_json(sl),
            })
        }
        "dm" => {
            let peer = user_uid(model, &parts[1], cx)?;
            let list = model.dm.list.read(cx);
            let Some(c) = list.get(peer) else { return Ok(json!({ "open": false, "conversations": list.len() })) };
            let sl = c.slice.read(cx);
            json!({
                "open": true, "unread": c.unread, "lastReadId": c.last_read_id, "totalUnread": list.total_unread(),
                "lastMessage": c.last_message.as_ref().map(|m| json!({"id": m.id, "content": &*m.content})),
                "conversations": list.len(), "fetched": sl.fetched, "hasMoreBefore": sl.has_more_before,
                "sendError": list.send_error.as_ref().map(|(_, e)| s(e)),
                "messages": rows_json(sl),
            })
        }
        "search" => {
            let st = model.chat.search.read(cx);
            json!({
                "status": st.status, "seq": st.seq, "total": st.total, "totalCapped": st.total_capped,
                "hasMore": st.has_more, "partial": st.partial, "error": st.error.as_ref().map(s),
                "textSkipped": st.text_skipped.iter().map(s).collect::<Vec<_>>(),
                "results": st.results.iter().map(|m| json!({"id": m.id, "channel": &*m.channel, "content": &*m.content})).collect::<Vec<_>>(),
            })
        }
        other => return Err(format!("dump: unknown {other:?}")),
    })
}

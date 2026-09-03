//! Epoch keys for encrypted text channels (spec 2026-09-04).
//!
//! Per `(server, channel)` the client holds every epoch key it is entitled
//! to. Keys never travel in the clear: a member who holds them seals each
//! one to each member's identity (the DM envelope around
//! `channel_envelope::key_blob_content`) and the community server escrows
//! those blobs per recipient. This module:
//!
//!   - `ensure`: fetch our blobs, unseal them, create epoch 1 when the
//!     channel has none yet, and fill gaps the server reports for others;
//!   - `rotate`: mint the next epoch for the current viewers (after a
//!     removal, when the server names us as filler);
//!   - `seal` / `open` for message bodies;
//!   - the ordered worker that opens incoming channel packets (a missing
//!     epoch key means a fetch, whose reply arrives on the router loop —
//!     same reason the DM worker exists).
//!
//! Fills are paced to central's key-lookup bucket: sealing to N members
//! needs N identity bundles.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex};

use super::channel_envelope;
use super::envelope;
use super::identity::{random_bytes, KEY_LEN};
use super::session::{self as e2ee, Status};
use crate::events;
use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::AppState;

/// What pre-E2EE clients see in `content` for a sealed channel message.
pub const PLACEHOLDER: &str = "🔒 This message is end-to-end encrypted. Update Decibell to read it.";
pub const UNREADABLE: &str = "🔒 Encrypted message";
const FETCH_TIMEOUT: Duration = Duration::from_secs(6);
/// Gap between uncached identity fetches while sealing to many members
/// (central's key bucket is 20 burst / 5 per s).
const FILL_PACE: Duration = Duration::from_millis(250);

/// Everything we know about one encrypted channel.
#[derive(Clone, Default)]
pub struct ChannelKeyring {
    pub encrypted: bool,
    pub current_epoch: u32,
    pub keys: HashMap<u32, [u8; KEY_LEN]>,
    pub members: Vec<String>,
    /// Viewers the server said lack blobs (username → epochs).
    pub needs: Vec<(String, Vec<u32>)>,
}

pub type KeyId = (String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptError {
    Locked,
    NoKey,
    Bad,
}

impl DecryptError {
    pub fn as_str(self) -> &'static str {
        match self {
            DecryptError::Locked => "locked",
            DecryptError::NoKey => "no_key",
            DecryptError::Bad => "bad",
        }
    }
}

fn key_id(server_id: &str, channel_id: &str) -> KeyId {
    (server_id.to_string(), channel_id.to_string())
}

async fn send_community(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    ty: packet::Type,
    payload: packet::Payload,
) -> Result<(), String> {
    let (tx, data) = {
        let s = state.lock().await;
        let client = s.communities.get(server_id).ok_or("Not connected to that community")?;
        let tx = client.connection_write_tx().ok_or("Community connection lost")?;
        (tx, build_packet(ty, payload, Some(&client.jwt)))
    };
    match tokio::time::timeout(FETCH_TIMEOUT, tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".into()),
        Err(_) => Err("Send timed out".into()),
    }
}

// ── Server round-trips ──────────────────────────────────────────────

async fn fetch(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeysRes, String> {
    let rx = {
        let mut s = state.lock().await;
        let (tx, rx) = oneshot::channel();
        s.pending_channel_keys.entry(key_id(server_id, channel_id)).or_default().push(tx);
        rx
    };
    send_community(
        state,
        server_id,
        packet::Type::ChannelKeysReq,
        packet::Payload::ChannelKeysReq(ChannelKeysReq { channel_id: channel_id.to_string() }),
    )
    .await?;
    match tokio::time::timeout(FETCH_TIMEOUT, rx).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(_)) => Err("Connection closed before the key reply".into()),
        Err(_) => {
            state.lock().await.pending_channel_keys.remove(&key_id(server_id, channel_id));
            Err("Channel key lookup timed out".into())
        }
    }
}

async fn publish(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    epoch: u32,
    blobs: Vec<ChannelKeyBlob>,
) -> Result<ChannelKeysPublishRes, String> {
    let rx = {
        let mut s = state.lock().await;
        let (tx, rx) = oneshot::channel();
        s.pending_channel_keys_publish.insert(key_id(server_id, channel_id), tx);
        rx
    };
    send_community(
        state,
        server_id,
        packet::Type::ChannelKeysPublishReq,
        packet::Payload::ChannelKeysPublishReq(ChannelKeysPublishReq {
            channel_id: channel_id.to_string(),
            epoch,
            blobs,
        }),
    )
    .await?;
    match tokio::time::timeout(FETCH_TIMEOUT, rx).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(_)) => Err("Connection closed before the publish reply".into()),
        Err(_) => {
            state.lock().await.pending_channel_keys_publish.remove(&key_id(server_id, channel_id));
            Err("Publishing channel keys timed out".into())
        }
    }
}

// ── Blob seal / unseal (identity envelopes) ─────────────────────────

/// Open one escrowed blob addressed to us.
async fn unseal_blob(
    state: &Arc<Mutex<AppState>>,
    channel_id: &str,
    sender: &str,
    blob: &[u8],
) -> Option<(u32, [u8; KEY_LEN])> {
    let me = state.lock().await.username.clone()?;
    let content = e2ee::open_inbound_raw(state, sender, &me, blob).await.ok()?;
    channel_envelope::parse_key_blob_content(&content, channel_id)
}

/// Seal `keys` (epoch → key) to `recipient`; None when they have no identity.
async fn seal_blobs_for(
    state: &Arc<Mutex<AppState>>,
    channel_id: &str,
    recipient: &str,
    keys: &[(u32, [u8; KEY_LEN])],
) -> Result<Option<Vec<ChannelKeyBlob>>, String> {
    let mut out = Vec::with_capacity(keys.len());
    for (epoch, key) in keys {
        let content = channel_envelope::key_blob_content(channel_id, *epoch, key);
        let Some(wire) = e2ee::seal_outbound_raw(state, recipient, &content).await? else {
            return Ok(None);
        };
        out.push(ChannelKeyBlob { epoch: *epoch, sender: String::new(), recipient: recipient.to_string(), blob: wire });
    }
    Ok(Some(out))
}

// ── Public API ──────────────────────────────────────────────────────

/// Load (and if needed bootstrap) the keyring for a channel. Returns the
/// keyring even for a plaintext channel (`encrypted: false`).
pub async fn ensure(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeyring, String> {
    {
        let s = state.lock().await;
        if s.e2ee.status != Status::Ready {
            return Err("Unlock encryption (Settings → Privacy) to use encrypted channels.".into());
        }
        if let Some(r) = s.channel_keys.get(&key_id(server_id, channel_id)) {
            if !r.encrypted || (r.current_epoch > 0 && r.keys.contains_key(&r.current_epoch)) {
                return Ok(r.clone());
            }
        }
    }
    refresh(state, server_id, channel_id).await
}

/// Always talk to the server: our blobs, the current epoch, who lacks keys.
pub async fn refresh(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeyring, String> {
    let res = fetch(state, server_id, channel_id).await?;
    let mut ring = {
        let s = state.lock().await;
        s.channel_keys.get(&key_id(server_id, channel_id)).cloned().unwrap_or_default()
    };
    ring.encrypted = res.encrypted;
    ring.current_epoch = res.current_epoch;
    ring.members = res.members.clone();
    ring.needs = res.needs.iter().map(|n| (n.username.clone(), n.epochs.clone())).collect();
    for b in &res.blobs {
        if ring.keys.contains_key(&b.epoch) {
            continue;
        }
        match unseal_blob(state, channel_id, &b.sender, &b.blob).await {
            Some((epoch, key)) if epoch == b.epoch => {
                ring.keys.insert(epoch, key);
            }
            _ => log::warn!("[chkeys] #{channel_id}: blob for epoch {} from {} did not open", b.epoch, b.sender),
        }
    }
    state.lock().await.channel_keys.insert(key_id(server_id, channel_id), ring.clone());

    if ring.encrypted && ring.current_epoch == 0 {
        // Nobody has minted a key for this channel yet — we do, for every
        // viewer (ourselves included so our other devices can read).
        return create_epoch(state, server_id, channel_id, 1, &ring.members).await;
    }
    if ring.encrypted && !ring.needs.is_empty() && ring.keys.contains_key(&ring.current_epoch) {
        let st = state.clone();
        let (sid, cid) = (server_id.to_string(), channel_id.to_string());
        tokio::spawn(async move {
            if let Err(e) = fill_gaps(&st, &sid, &cid).await {
                log::warn!("[chkeys] #{cid}: fill failed: {e}");
            }
        });
    }
    Ok(ring)
}

/// Mint `epoch` and escrow it to `members`. Loses the race gracefully: a
/// stale-epoch reply re-fetches and returns whatever the winner published.
async fn create_epoch(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    epoch: u32,
    members: &[String],
) -> Result<ChannelKeyring, String> {
    let key: [u8; KEY_LEN] = random_bytes()?;
    let mut blobs = Vec::new();
    let mut skipped = Vec::new();
    for (i, m) in members.iter().enumerate() {
        match seal_blobs_for(state, channel_id, m, &[(epoch, key)]).await? {
            Some(b) => blobs.extend(b),
            None => skipped.push(m.clone()),
        }
        if i + 1 < members.len() {
            tokio::time::sleep(FILL_PACE).await;
        }
    }
    if !skipped.is_empty() {
        log::info!("[chkeys] #{channel_id}: {} member(s) without encryption keys skipped: {:?}", skipped.len(), skipped);
    }
    if blobs.is_empty() {
        return Err("Nobody in this channel has encryption keys yet.".into());
    }
    match publish(state, server_id, channel_id, epoch, blobs).await {
        Ok(r) if r.success => {
            let mut s = state.lock().await;
            let ring = s.channel_keys.entry(key_id(server_id, channel_id)).or_default();
            ring.encrypted = true;
            ring.current_epoch = epoch;
            ring.keys.insert(epoch, key);
            log::info!("[chkeys] #{channel_id}: created epoch {epoch} for {} member(s)", members.len());
            Ok(ring.clone())
        }
        Ok(_) => {
            log::info!("[chkeys] #{channel_id}: lost the race for epoch {epoch}; re-fetching");
            refresh_no_create(state, server_id, channel_id).await
        }
        Err(e) => Err(e),
    }
}

/// `refresh` without the bootstrap branch (used after losing a race).
async fn refresh_no_create(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeyring, String> {
    let res = fetch(state, server_id, channel_id).await?;
    let mut ring = {
        let s = state.lock().await;
        s.channel_keys.get(&key_id(server_id, channel_id)).cloned().unwrap_or_default()
    };
    ring.encrypted = res.encrypted;
    ring.current_epoch = res.current_epoch;
    ring.members = res.members.clone();
    for b in &res.blobs {
        if ring.keys.contains_key(&b.epoch) {
            continue;
        }
        if let Some((epoch, key)) = unseal_blob(state, channel_id, &b.sender, &b.blob).await {
            if epoch == b.epoch {
                ring.keys.insert(epoch, key);
            }
        }
    }
    state.lock().await.channel_keys.insert(key_id(server_id, channel_id), ring.clone());
    Ok(ring)
}

/// Seal every epoch key we hold for each viewer the server says lacks it.
pub async fn fill_gaps(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<(), String> {
    let ring = {
        let s = state.lock().await;
        s.channel_keys.get(&key_id(server_id, channel_id)).cloned().unwrap_or_default()
    };
    let mut by_epoch: HashMap<u32, Vec<ChannelKeyBlob>> = HashMap::new();
    let mut fetched = 0usize;
    for (user, epochs) in &ring.needs {
        let have: Vec<(u32, [u8; KEY_LEN])> =
            epochs.iter().filter_map(|e| ring.keys.get(e).map(|k| (*e, *k))).collect();
        if have.is_empty() {
            continue;
        }
        match seal_blobs_for(state, channel_id, user, &have).await? {
            Some(blobs) => {
                for b in blobs {
                    by_epoch.entry(b.epoch).or_default().push(b);
                }
            }
            None => log::info!("[chkeys] #{channel_id}: {user} has no encryption keys; can't fill"),
        }
        fetched += 1;
        if fetched % 4 == 0 {
            tokio::time::sleep(FILL_PACE).await;
        }
    }
    for (epoch, blobs) in by_epoch {
        for chunk in blobs.chunks(400) {
            let r = publish(state, server_id, channel_id, epoch, chunk.to_vec()).await?;
            if !r.success {
                log::warn!("[chkeys] #{channel_id}: fill for epoch {epoch} refused: {}", r.message);
            }
        }
    }
    let mut s = state.lock().await;
    if let Some(r) = s.channel_keys.get_mut(&key_id(server_id, channel_id)) {
        r.needs.clear();
    }
    Ok(())
}

/// After a removal: the next epoch for the current viewer list.
pub async fn rotate(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<(), String> {
    let ring = refresh_no_create(state, server_id, channel_id).await?;
    if !ring.encrypted {
        return Ok(());
    }
    create_epoch(state, server_id, channel_id, ring.current_epoch + 1, &ring.members).await.map(|_| ())
}

/// Forget everything cached for a community (disconnect / logout).
pub fn forget_server(s: &mut AppState, server_id: &str) {
    s.channel_keys.retain(|(sid, _), _| sid != server_id);
    s.pending_channel_keys.retain(|(sid, _), _| sid != server_id);
    s.pending_channel_keys_publish.retain(|(sid, _), _| sid != server_id);
}

/// Seal a message body under the channel's current epoch.
pub async fn seal_message(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let ring = ensure(state, server_id, channel_id).await?;
    if !ring.encrypted {
        return Err("This channel isn't encrypted.".into());
    }
    let key = ring
        .keys
        .get(&ring.current_epoch)
        .ok_or("You don't have this channel's current key yet — another member has to share it. Try again in a moment.")?;
    let me = state.lock().await.username.clone().ok_or("Not signed in")?;
    channel_envelope::seal(ring.current_epoch, key, channel_id, &me, text)
}

/// Open a message body; fetches once if the epoch is unknown.
pub async fn open_message(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    sender: &str,
    wire: &[u8],
) -> Result<String, DecryptError> {
    let epoch = channel_envelope::parse_epoch(wire).map_err(|_| DecryptError::Bad)?;
    let lookup = |s: &AppState| -> Option<[u8; KEY_LEN]> {
        s.channel_keys.get(&key_id(server_id, channel_id)).and_then(|r| r.keys.get(&epoch).copied())
    };
    let (ready, key) = {
        let s = state.lock().await;
        (s.e2ee.status == Status::Ready, lookup(&s))
    };
    if !ready {
        return Err(DecryptError::Locked);
    }
    let key = match key {
        Some(k) => k,
        None => {
            if let Err(e) = refresh(state, server_id, channel_id).await {
                log::debug!("[chkeys] #{channel_id}: refresh for epoch {epoch} failed: {e}");
            }
            let s = state.lock().await;
            lookup(&s).ok_or(DecryptError::NoKey)?
        }
    };
    channel_envelope::open(&key, channel_id, sender, wire).map_err(|_| DecryptError::Bad)
}

// ── Router hooks ────────────────────────────────────────────────────

pub async fn on_keys_res(state: &Arc<Mutex<AppState>>, server_id: &str, res: ChannelKeysRes) {
    let waiters = state.lock().await.pending_channel_keys.remove(&key_id(server_id, &res.channel_id));
    if let Some(ws) = waiters {
        for w in ws {
            let _ = w.send(res.clone());
        }
    }
}

pub async fn on_publish_res(state: &Arc<Mutex<AppState>>, server_id: &str, res: ChannelKeysPublishRes) {
    if let Some(w) = state.lock().await.pending_channel_keys_publish.remove(&key_id(server_id, &res.channel_id)) {
        let _ = w.send(res);
    }
}

/// The server says a channel's keys changed or someone lacks them. Only the
/// named filler acts; everyone drops any stale `current` so the next send
/// re-fetches.
pub async fn on_keys_changed(state: &Arc<Mutex<AppState>>, server_id: &str, b: ChannelKeysChanged) {
    let me = {
        let mut s = state.lock().await;
        if let Some(r) = s.channel_keys.get_mut(&key_id(server_id, &b.channel_id)) {
            if r.current_epoch != b.current_epoch {
                r.current_epoch = b.current_epoch;
            }
        }
        s.username.clone().unwrap_or_default()
    };
    if b.filler != me {
        return;
    }
    let st = state.clone();
    let sid = server_id.to_string();
    tokio::spawn(async move {
        let r = if b.rotate {
            rotate(&st, &sid, &b.channel_id).await
        } else {
            refresh(&st, &sid, &b.channel_id).await.map(|_| ())
        };
        if let Err(e) = r {
            log::warn!("[chkeys] #{}: filler action failed: {e}", b.channel_id);
        }
    });
}

// ── The channel crypto worker ───────────────────────────────────────

pub enum ChannelJob {
    Msg(String, ChannelMessage),
    History(String, ChannelHistoryResponse),
    Edited(String, ChannelMessageEdited),
}

pub async fn enqueue(state: &Arc<Mutex<AppState>>, job: ChannelJob) {
    let tx = {
        let mut s = state.lock().await;
        match s.channel_crypto_tx.clone() {
            Some(tx) => tx,
            None => {
                let (tx, rx) = mpsc::channel(512);
                s.channel_crypto_tx = Some(tx.clone());
                tokio::spawn(run_worker(rx, state.clone()));
                tx
            }
        }
    };
    if tx.send(job).await.is_err() {
        log::warn!("[chkeys] worker gone; dropping packet");
    }
}

async fn run_worker(mut rx: mpsc::Receiver<ChannelJob>, state: Arc<Mutex<AppState>>) {
    while let Some(job) = rx.recv().await {
        match job {
            ChannelJob::Msg(sid, m) => handle_msg(&state, &sid, m).await,
            ChannelJob::History(sid, r) => handle_history(&state, &sid, r).await,
            ChannelJob::Edited(sid, b) => handle_edited(&state, &sid, b).await,
        }
    }
}

async fn render(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    sender: &str,
    content: String,
    wire: &[u8],
) -> (String, bool, String) {
    if wire.is_empty() {
        return (content, false, String::new());
    }
    match open_message(state, server_id, channel_id, sender, wire).await {
        Ok(t) => (t, true, String::new()),
        Err(e) => (UNREADABLE.to_string(), true, e.as_str().to_string()),
    }
}

async fn handle_msg(state: &Arc<Mutex<AppState>>, server_id: &str, msg: ChannelMessage) {
    let (content, encrypted, err) =
        render(state, server_id, &msg.channel_id, &msg.sender, msg.content, &msg.envelope).await;
    let reply_to_content = if msg.reply_to_envelope.is_empty() {
        msg.reply_to_content
    } else {
        render(state, server_id, &msg.channel_id, &msg.reply_to_sender, String::new(), &msg.reply_to_envelope).await.0
    };
    events::emit_message_received(events::MessageReceivedPayload {
        context: msg.channel_id,
        server_id: server_id.to_string(),
        sender: msg.sender,
        recipient: String::new(),
        content,
        timestamp: msg.timestamp.to_string(),
        id: msg.id,
        attachments: msg.attachments.into_iter().map(crate::net::community::map_attachment).collect(),
        nonce: msg.nonce,
        edited_at: msg.edited_at,
        reply_to: msg.reply_to,
        reply_to_sender: msg.reply_to_sender,
        reply_to_content,
        reply_to_attachment_kinds: msg.reply_to_attachment_kinds,
        encrypted,
        decrypt_error: err,
    });
}

async fn handle_history(state: &Arc<Mutex<AppState>>, server_id: &str, resp: ChannelHistoryResponse) {
    let channel_id = resp.channel_id.clone();
    let mut messages = Vec::with_capacity(resp.messages.len());
    for m in resp.messages {
        let (content, encrypted, err) = render(state, server_id, &channel_id, &m.sender, m.content, &m.envelope).await;
        let reply_to_content = if m.reply_to_envelope.is_empty() {
            m.reply_to_content
        } else {
            render(state, server_id, &channel_id, &m.reply_to_sender, String::new(), &m.reply_to_envelope).await.0
        };
        messages.push(events::ChannelMessagePayload {
            id: m.id,
            sender: m.sender,
            channel_id: m.channel_id,
            content,
            timestamp: m.timestamp,
            attachments: m.attachments.into_iter().map(crate::net::community::map_attachment).collect(),
            nonce: m.nonce,
            edited_at: m.edited_at,
            reply_to: m.reply_to,
            reply_to_sender: m.reply_to_sender,
            reply_to_content,
            reply_to_attachment_kinds: m.reply_to_attachment_kinds,
            encrypted,
            decrypt_error: err,
        });
    }
    events::emit_channel_history_received(events::ChannelHistoryReceivedPayload {
        server_id: server_id.to_string(),
        channel_id,
        messages,
        has_more: resp.has_more,
        has_more_after: resp.has_more_after,
        around_id: resp.around_id,
        after_id: resp.after_id,
    });
}

async fn handle_edited(state: &Arc<Mutex<AppState>>, server_id: &str, b: ChannelMessageEdited) {
    let (content, encrypted, err) = render(state, server_id, &b.channel_id, &b.editor, b.content, &b.envelope).await;
    events::emit_channel_message_edited(events::ChannelMessageEditedPayload {
        server_id: server_id.to_string(),
        channel_id: b.channel_id,
        message_id: b.message_id,
        content,
        edited_at: b.edited_at,
        editor: b.editor,
        encrypted,
        decrypt_error: err,
    });
}

/// Nothing to persist: keyrings are re-fetched per session (blobs live on
/// the server). Kept as a hook for a local cache later.
#[allow(dead_code)]
pub fn known_channels(s: &AppState) -> HashSet<KeyId> {
    s.channel_keys.keys().cloned().collect()
}

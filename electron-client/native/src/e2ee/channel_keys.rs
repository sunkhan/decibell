//! Epoch keys for encrypted text channels (spec 2026-09-04).
//!
//! Per `(server, channel)` the client holds every epoch key it is entitled
//! to. Keys never travel in the clear: a member who holds them seals each
//! one to each member's identity (the DM envelope around
//! `channel_envelope::key_blob_content`) and the community server escrows
//! those blobs per recipient. This module:
//!
//!   - `ensure`: fetch our blobs (paged), unseal them, drop the ones we
//!     can't use (the server then has a holder seal them again), create
//!     epoch 1 when the channel has none yet — or the next one when nobody
//!     holds the current one — and fill gaps the server reports for others;
//!   - `rotate`: mint the next epoch for the current viewers (after a
//!     removal, when the server names us as filler);
//!   - `seal` / `open` for message bodies;
//!   - the ordered workers, one per community, that open incoming channel
//!     packets (a missing epoch key means a fetch, whose reply arrives on
//!     the router loop — same reason the DM worker exists).
//!
//! Fills are paced to central's key-lookup bucket: sealing to N members
//! needs N identity bundles.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot, Mutex};

use prost::Message as _;

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
    /// Everyone who can view the channel: (name, uid).
    pub members: Vec<(String, i64)>,
    /// Viewers the server said lack blobs: (name, uid, epochs). The first
    /// page; `more_needs` says the filler has more to walk.
    pub needs: Vec<(String, i64, Vec<u32>)>,
    pub more_needs: bool,
    /// Nobody holds the current epoch any more: the next send mints one.
    pub orphaned: bool,
}

/// One channel's loads (review A4): one at a time, reused by whoever waited
/// for it, and remembered briefly so a page of messages under an epoch we
/// don't hold costs one round trip rather than one per message.
#[derive(Default)]
pub struct LoadSlot {
    lock: Mutex<()>,
    /// Loads that succeeded so far.
    done: AtomicU64,
    /// When the last load from the open path finished, successful or not.
    /// CHANNEL_KEYS_CHANGED clears it: a new epoch is worth a fresh look.
    tried: std::sync::Mutex<Option<Instant>>,
}

/// Within this long of the last load, a message under an epoch we don't hold
/// opens as `no_key` without another one.
const RECENT_LOAD: Duration = Duration::from_secs(10);

fn needs_of(res: &ChannelKeysRes) -> Vec<(String, i64, Vec<u32>)> {
    res.needs.iter().map(|n| (n.username.clone(), n.uid, n.epochs.clone())).collect()
}

/// The server's viewer list with its parallel uids.
/// The viewers keys are sealed to: accounts only. A community placeholder
/// (a legacy name, negative uid) has no identity of its own, and its name
/// may belong to someone else at central by now; whoever claims it shows
/// up in `needs` under their uid.
fn members_of(res: &ChannelKeysRes) -> Vec<(String, i64)> {
    res.members
        .iter()
        .enumerate()
        .map(|(i, name)| (name.clone(), res.member_uids.get(i).copied().unwrap_or(0)))
        .filter(|(_, uid)| *uid > 0)
        .collect()
}

pub type KeyId = (String, String);
/// A pending CHANNEL_KEYS_REQ: server, channel and the page's cursors
/// (`after_epoch`, `needs_after_uid`), which the reply echoes.
pub type FetchId = (String, String, u32, i64);

/// Page walks stop here (64 × 256 blobs; 64 × ≤ 200 viewers).
const MAX_PAGES: usize = 64;

/// Spaces out the key requests of one long operation (a load's pages, a
/// fill's publishes). The community takes CHANNEL_KEYS_* (and MLS) requests
/// at 20 burst / 5 per s per session and drops the rest without a word, so
/// a request past the burst would only time out.
struct Pacer(usize);

impl Pacer {
    const BURST: usize = 8;
    const GAP: Duration = Duration::from_millis(250);

    async fn tick(&mut self) {
        if self.0 >= Self::BURST {
            tokio::time::sleep(Self::GAP).await;
        }
        self.0 += 1;
    }
}
/// The server takes at most this many epochs per CHANNEL_KEYS_REJECT.
const MAX_REJECT: usize = 512;

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
        (tx, build_packet(ty, payload, None))
    };
    match tokio::time::timeout(FETCH_TIMEOUT, tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".into()),
        Err(_) => Err("Send timed out".into()),
    }
}

// ── Server round-trips ──────────────────────────────────────────────

/// One page of CHANNEL_KEYS_RES: the first (both cursors 0: viewers, needs
/// and our first blobs), our blobs past `after_epoch`, or the needs of
/// viewers past `needs_after_uid`.
async fn fetch(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    after_epoch: u32,
    needs_after_uid: i64,
) -> Result<ChannelKeysRes, String> {
    let id: FetchId = (server_id.to_string(), channel_id.to_string(), after_epoch, needs_after_uid);
    let rx = {
        let mut s = state.lock().await;
        let (tx, rx) = oneshot::channel();
        s.pending_channel_keys.entry(id.clone()).or_default().push(tx);
        rx
    };
    send_community(
        state,
        server_id,
        packet::Type::ChannelKeysReq,
        packet::Payload::ChannelKeysReq(ChannelKeysReq {
            channel_id: channel_id.to_string(),
            after_epoch,
            needs_after_uid,
        }),
    )
    .await?;
    match tokio::time::timeout(FETCH_TIMEOUT, rx).await {
        Ok(Ok(r)) => Ok(r),
        Ok(Err(_)) => Err("Connection closed before the key reply".into()),
        Err(_) => {
            state.lock().await.pending_channel_keys.remove(&id);
            Err("Channel key lookup timed out".into())
        }
    }
}

/// `key_check`: the new key's check when `epoch` is a creation, else empty.
async fn publish(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    epoch: u32,
    blobs: Vec<ChannelKeyBlob>,
    key_check: Vec<u8>,
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
            key_check,
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

/// Why one of our blobs didn't open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlobFault {
    /// Nothing known to be wrong with it (locked, the sender's key couldn't
    /// be fetched, another device of ours is a key generation ahead): try
    /// again on the next fetch.
    Retry,
    /// Sealed to a generation of our key older than our current one (we
    /// reset): useless to us and nobody's fault. Dropped, to be sealed again.
    Stale,
    /// Doesn't authenticate, binds another channel or epoch, or its key
    /// fails the epoch's key check. Dropped, and its sender refused.
    Bad,
}

/// Open one escrowed blob addressed to us. A v1 blob binds the names it
/// was sealed under, which the server returns with it.
async fn unseal_blob(state: &Arc<Mutex<AppState>>, channel_id: &str, b: &ChannelKeyBlob) -> Result<[u8; KEY_LEN], BlobFault> {
    let (my_uid, my_name, my_kid) = {
        let s = state.lock().await;
        let kid = s.e2ee.store.as_ref().and_then(|st| st.current()).map(|k| k.key_id).unwrap_or(0);
        (s.uid, s.username.clone().ok_or(BlobFault::Retry)?, kid)
    };
    let parties = envelope::Parties {
        sender_uid: b.sender_uid,
        recipient_uid: my_uid,
        sender_name: &b.sender,
        recipient_name: if b.recipient.is_empty() { &my_name } else { &b.recipient },
    };
    let content = match e2ee::open_inbound_raw(state, &parties, &b.blob).await {
        Ok(c) => c,
        Err(e2ee::DecryptError::Bad) => return Err(BlobFault::Bad),
        Err(e2ee::DecryptError::NoKey) => {
            // The generation of ours it names: the recipient's slot, or the
            // sender's in one we sealed to ourselves.
            let ours = envelope::parse_header(&b.blob)
                .ok()
                .map(|h| if b.sender_uid == my_uid { h.sender_key_id } else { h.recipient_key_id });
            return Err(match ours {
                Some(kid) if kid < my_kid => BlobFault::Stale,
                _ => BlobFault::Retry,
            });
        }
        Err(_) => return Err(BlobFault::Retry),
    };
    let (epoch, key) = channel_envelope::parse_key_blob_content(&content, channel_id).ok_or(BlobFault::Bad)?;
    if epoch != b.epoch {
        return Err(BlobFault::Bad);
    }
    if !b.key_check.is_empty() && b.key_check[..] != channel_envelope::key_check(channel_id, epoch, &key)[..] {
        return Err(BlobFault::Bad);
    }
    Ok(key)
}

/// Tell the server our blobs for `epochs` can't be used: it drops them and
/// asks a holder to seal again (`bad`: never their senders).
async fn reject(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str, epochs: &[u32], bad: bool) {
    for chunk in epochs.chunks(MAX_REJECT) {
        let sent = send_community(
            state,
            server_id,
            packet::Type::ChannelKeysReject,
            packet::Payload::ChannelKeysReject(ChannelKeysReject {
                channel_id: channel_id.to_string(),
                epochs: chunk.to_vec(),
                bad,
            }),
        )
        .await;
        let what = if bad { "bad" } else { "stale" };
        match sent {
            Ok(()) => log::info!("[chkeys] #{channel_id}: dropped {} {what} blob(s)", chunk.len()),
            Err(e) => log::warn!("[chkeys] #{channel_id}: dropping {what} blobs failed: {e}"),
        }
    }
}

/// Seal `keys` (epoch → key) to a viewer; None when they have no identity.
async fn seal_blobs_for(
    state: &Arc<Mutex<AppState>>,
    channel_id: &str,
    (recipient, recipient_uid): (&str, i64),
    keys: &[(u32, [u8; KEY_LEN])],
) -> Result<Option<Vec<ChannelKeyBlob>>, String> {
    if recipient_uid <= 0 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(keys.len());
    for (epoch, key) in keys {
        let content = channel_envelope::key_blob_content(channel_id, *epoch, key);
        let Some(wire) = e2ee::seal_outbound_raw(state, recipient_uid, recipient, &content).await? else {
            return Ok(None);
        };
        out.push(ChannelKeyBlob {
            epoch: *epoch,
            sender: String::new(),
            recipient: recipient.to_string(),
            blob: wire,
            sender_uid: 0,
            recipient_uid,
            key_check: Vec::new(),
        });
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

/// Every page of our blobs plus the first of viewers and needs: opens the
/// blobs we lack keys for and drops the ones we can't use. Also returns
/// whether nobody holds the current epoch any more.
async fn load(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeyring, String> {
    let first = fetch(state, server_id, channel_id, 0, 0).await?;
    let mut ring = {
        let s = state.lock().await;
        s.channel_keys.get(&key_id(server_id, channel_id)).cloned().unwrap_or_default()
    };
    ring.encrypted = first.encrypted;
    ring.current_epoch = first.current_epoch;
    ring.members = members_of(&first);
    ring.needs = needs_of(&first);
    ring.more_needs = first.more_needs;
    ring.orphaned = first.current_orphaned;
    let (mut stale, mut bad) = (Vec::new(), Vec::new());
    let mut page = first;
    let mut failed = None;
    let mut pace = Pacer(1);
    for n in 1.. {
        for b in &page.blobs {
            if ring.keys.contains_key(&b.epoch) {
                continue;
            }
            match unseal_blob(state, channel_id, b).await {
                Ok(key) => {
                    ring.keys.insert(b.epoch, key);
                }
                Err(fault) => {
                    log::warn!("[chkeys] #{channel_id}: blob for epoch {} from {} did not open ({fault:?})", b.epoch, b.sender);
                    match fault {
                        BlobFault::Stale => stale.push(b.epoch),
                        BlobFault::Bad => bad.push(b.epoch),
                        BlobFault::Retry => {}
                    }
                }
            }
        }
        let after = match page.blobs.last() {
            Some(b) if page.more_blobs && n < MAX_PAGES => b.epoch,
            _ => break,
        };
        pace.tick().await;
        match fetch(state, server_id, channel_id, after, 0).await {
            Ok(next) => page = next,
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    state.lock().await.channel_keys.insert(key_id(server_id, channel_id), ring.clone());
    reject(state, server_id, channel_id, &stale, false).await;
    reject(state, server_id, channel_id, &bad, true).await;
    match failed {
        Some(e) => Err(e),
        None => Ok(ring),
    }
}

fn load_slot(s: &mut AppState, server_id: &str, channel_id: &str) -> Arc<LoadSlot> {
    s.channel_key_loads.entry(key_id(server_id, channel_id)).or_default().clone()
}

/// `load`, one at a time per channel: a caller that waited out someone
/// else's load takes its result instead of fetching again.
async fn load_shared(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeyring, String> {
    let slot = load_slot(&mut *state.lock().await, server_id, channel_id);
    let seen = slot.done.load(Ordering::Acquire);
    let _running = slot.lock.lock().await;
    if slot.done.load(Ordering::Acquire) != seen {
        if let Some(ring) = state.lock().await.channel_keys.get(&key_id(server_id, channel_id)) {
            return Ok(ring.clone());
        }
    }
    let ring = load(state, server_id, channel_id).await?;
    slot.done.fetch_add(1, Ordering::Release);
    Ok(ring)
}

/// When we hold the current key and the server lists viewers without
/// blobs, seal for them in the background.
fn spawn_fill(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str, ring: &ChannelKeyring) {
    if !ring.encrypted || ring.needs.is_empty() || !ring.keys.contains_key(&ring.current_epoch) {
        return;
    }
    let st = state.clone();
    let (sid, cid) = (server_id.to_string(), channel_id.to_string());
    tokio::spawn(async move {
        if let Err(e) = fill_gaps(&st, &sid, &cid).await {
            log::warn!("[chkeys] #{cid}: fill failed: {e}");
        }
    });
}

/// The open path's load (review A4): never mints an epoch, and runs at most
/// once per channel within `RECENT_LOAD`, so a history page under an epoch
/// we don't hold, or a server naming epochs that don't exist, costs one
/// round trip and holds up only that community's worker.
async fn load_for_open(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) {
    let slot = load_slot(&mut *state.lock().await, server_id, channel_id);
    if slot.tried.lock().unwrap().is_some_and(|at| at.elapsed() < RECENT_LOAD) {
        return;
    }
    match load_shared(state, server_id, channel_id).await {
        Ok(ring) => spawn_fill(state, server_id, channel_id, &ring),
        Err(e) => log::debug!("[chkeys] #{channel_id}: load for a missing epoch failed: {e}"),
    }
    *slot.tried.lock().unwrap() = Some(Instant::now());
}

/// Always talk to the server: our blobs, the current epoch, who lacks keys.
pub async fn refresh(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<ChannelKeyring, String> {
    let ring = load_shared(state, server_id, channel_id).await?;
    if ring.encrypted && (ring.current_epoch == 0 || ring.orphaned) {
        // Nobody has minted a key for this channel yet, or nobody holds the
        // current one any more (so nobody can share it) — we mint the next,
        // for every viewer (ourselves included so our other devices can read).
        return create_epoch(state, server_id, channel_id, ring.current_epoch + 1, &ring.members).await;
    }
    spawn_fill(state, server_id, channel_id, &ring);
    Ok(ring)
}

/// Mint `epoch` and escrow it to `members`. Loses the race gracefully: a
/// stale-epoch reply re-fetches and returns whatever the winner published.
async fn create_epoch(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    epoch: u32,
    members: &[(String, i64)],
) -> Result<ChannelKeyring, String> {
    let key: [u8; KEY_LEN] = random_bytes()?;
    let mut blobs = Vec::new();
    let mut skipped = Vec::new();
    for (name, uid) in members {
        // Only a lookup costs central's key bucket; members whose keys this
        // session already has seal at once (a channel's first send used to
        // wait 250 ms per member).
        let lookup = e2ee::needs_lookup(state, *uid).await;
        match seal_blobs_for(state, channel_id, (name, *uid), &[(epoch, key)]).await? {
            Some(b) => blobs.extend(b),
            None => skipped.push(name.clone()),
        }
        if lookup {
            tokio::time::sleep(FILL_PACE).await;
        }
    }
    if !skipped.is_empty() {
        log::info!("[chkeys] #{channel_id}: {} member(s) without encryption keys skipped: {:?}", skipped.len(), skipped);
    }
    if blobs.is_empty() {
        return Err("Nobody in this channel has encryption keys yet.".into());
    }
    let check = channel_envelope::key_check(channel_id, epoch, &key).to_vec();
    match publish(state, server_id, channel_id, epoch, blobs, check).await {
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
    load_shared(state, server_id, channel_id).await
}

/// Seal every epoch key we hold for each viewer the server says lacks it,
/// walking the needs pages.
pub async fn fill_gaps(state: &Arc<Mutex<AppState>>, server_id: &str, channel_id: &str) -> Result<(), String> {
    let ring = {
        let s = state.lock().await;
        s.channel_keys.get(&key_id(server_id, channel_id)).cloned().unwrap_or_default()
    };
    let (mut needs, mut more) = (ring.needs.clone(), ring.more_needs);
    let mut pace = Pacer(0);
    for n in 1.. {
        fill_page(state, server_id, channel_id, &ring.keys, &needs, &mut pace).await?;
        let after = match needs.last() {
            Some((_, uid, _)) if more && n < MAX_PAGES => *uid,
            _ => break,
        };
        pace.tick().await;
        let page = fetch(state, server_id, channel_id, 0, after).await?;
        needs = needs_of(&page);
        more = page.more_needs;
    }
    let mut s = state.lock().await;
    if let Some(r) = s.channel_keys.get_mut(&key_id(server_id, channel_id)) {
        r.needs.clear();
    }
    Ok(())
}

async fn fill_page(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    keys: &HashMap<u32, [u8; KEY_LEN]>,
    needs: &[(String, i64, Vec<u32>)],
    pace: &mut Pacer,
) -> Result<(), String> {
    let mut by_epoch: HashMap<u32, Vec<ChannelKeyBlob>> = HashMap::new();
    for (user, uid, epochs) in needs {
        let have: Vec<(u32, [u8; KEY_LEN])> = epochs.iter().filter_map(|e| keys.get(e).map(|k| (*e, *k))).collect();
        if have.is_empty() {
            continue;
        }
        // They may have dropped a blob sealed to keys they've replaced
        // since: seal to the ones central has now.
        let lookup = e2ee::recheck_current(state, *uid).await;
        match seal_blobs_for(state, channel_id, (user, *uid), &have).await {
            Ok(Some(blobs)) => {
                for b in blobs {
                    by_epoch.entry(b.epoch).or_default().push(b);
                }
            }
            Ok(None) => log::info!("[chkeys] #{channel_id}: {user} has no encryption keys; can't fill"),
            Err(e) => log::warn!("[chkeys] #{channel_id}: sealing for {user} failed: {e}"),
        }
        if lookup {
            tokio::time::sleep(FILL_PACE).await;
        }
    }
    for (epoch, blobs) in by_epoch {
        for chunk in blobs.chunks(400) {
            pace.tick().await;
            let r = publish(state, server_id, channel_id, epoch, chunk.to_vec(), Vec::new()).await?;
            if !r.success {
                log::warn!("[chkeys] #{channel_id}: fill for epoch {epoch} refused: {}", r.message);
            }
        }
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
    s.channel_key_loads.retain(|(sid, _), _| sid != server_id);
    // Its worker finishes what's queued and ends.
    s.channel_crypto_tx.remove(server_id);
    s.pending_channel_keys.retain(|(sid, ..), _| sid != server_id);
    s.pending_channel_keys_publish.retain(|(sid, _), _| sid != server_id);
}

/// Encrypted attachment metadata as the renderer hands it over / gets it
/// back — mirrors proto `EncryptedAttachmentMeta` with a base64 key.
#[derive(Clone, Debug, Default)]
pub struct AttachmentMeta {
    pub id: i64,
    pub key_b64: String,
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub placeholder: String,
    pub chunk_bytes: u32,
    pub thumbnail_sizes_mask: u32,
}

impl AttachmentMeta {
    fn to_payload(&self) -> events::EncryptedAttachmentMetaPayload {
        events::EncryptedAttachmentMetaPayload {
            id: self.id,
            key_b64: self.key_b64.clone(),
            filename: self.filename.clone(),
            mime: self.mime.clone(),
            size_bytes: self.size_bytes,
            width: self.width,
            height: self.height,
            duration_ms: self.duration_ms,
            placeholder: self.placeholder.clone(),
            chunk_bytes: self.chunk_bytes,
            thumbnail_sizes_mask: self.thumbnail_sizes_mask,
        }
    }
}

/// Seal a message body under the channel's current epoch. Plain text
/// goes under content tag 0x01; text with encrypted attachments as a
/// prost `EncryptedMessageBody` under 0x03.
pub async fn seal_message(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    text: &str,
    attachments: &[AttachmentMeta],
) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let ring = ensure(state, server_id, channel_id).await?;
    if !ring.encrypted {
        return Err("This channel isn't encrypted.".into());
    }
    let key = ring
        .keys
        .get(&ring.current_epoch)
        .ok_or("You don't have this channel's current key yet — another member has to share it. Try again in a moment.")?;
    let me = state.lock().await.uid;
    if attachments.is_empty() {
        return channel_envelope::seal(ring.current_epoch, key, channel_id, me, text);
    }
    let mut body = EncryptedMessageBody { text: text.to_string(), attachments: Vec::new() };
    for a in attachments {
        let key_bytes = base64::engine::general_purpose::STANDARD
            .decode(&a.key_b64)
            .map_err(|e| format!("bad attachment key: {e}"))?;
        if key_bytes.len() != 32 {
            return Err("bad attachment key length".into());
        }
        body.attachments.push(EncryptedAttachmentMeta {
            id: a.id,
            key: key_bytes,
            filename: a.filename.clone(),
            mime: a.mime.clone(),
            size_bytes: a.size_bytes,
            width: a.width,
            height: a.height,
            duration_ms: a.duration_ms,
            placeholder: a.placeholder.clone(),
            chunk_bytes: a.chunk_bytes,
            thumbnail_sizes_mask: a.thumbnail_sizes_mask,
        });
    }
    channel_envelope::seal_tagged(
        ring.current_epoch,
        key,
        channel_id,
        me,
        channel_envelope::CONTENT_BODY,
        &body.encode_to_vec(),
    )
}

/// Open a message body: `(text, encrypted attachment metadata)`.
pub async fn open_message(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    sender: &channel_envelope::Sender<'_>,
    wire: &[u8],
) -> Result<(String, Vec<AttachmentMeta>), DecryptError> {
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
            load_for_open(state, server_id, channel_id).await;
            let s = state.lock().await;
            lookup(&s).ok_or(DecryptError::NoKey)?
        }
    };
    use base64::Engine as _;
    let (tag, plain) = channel_envelope::open_tagged(&key, channel_id, sender, wire).map_err(|_| DecryptError::Bad)?;
    match tag {
        channel_envelope::CONTENT_TEXT => Ok((String::from_utf8(plain).map_err(|_| DecryptError::Bad)?, Vec::new())),
        channel_envelope::CONTENT_BODY => {
            let body = EncryptedMessageBody::decode(plain.as_slice()).map_err(|_| DecryptError::Bad)?;
            let metas = body
                .attachments
                .into_iter()
                .map(|a| AttachmentMeta {
                    id: a.id,
                    key_b64: base64::engine::general_purpose::STANDARD.encode(&a.key),
                    filename: a.filename,
                    mime: a.mime,
                    size_bytes: a.size_bytes,
                    width: a.width,
                    height: a.height,
                    duration_ms: a.duration_ms,
                    placeholder: a.placeholder,
                    chunk_bytes: a.chunk_bytes,
                    thumbnail_sizes_mask: a.thumbnail_sizes_mask,
                })
                .collect();
            Ok((body.text, metas))
        }
        _ => Err(DecryptError::Bad),
    }
}

// ── Router hooks ────────────────────────────────────────────────────

pub async fn on_keys_res(state: &Arc<Mutex<AppState>>, server_id: &str, res: ChannelKeysRes) {
    let id: FetchId = (server_id.to_string(), res.channel_id.clone(), res.after_epoch, res.needs_after_uid);
    let waiters = state.lock().await.pending_channel_keys.remove(&id);
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
        // A new epoch (or a fill on its way): the next message under an
        // epoch we don't hold loads again, however recent the last load.
        if let Some(slot) = s.channel_key_loads.get(&key_id(server_id, &b.channel_id)) {
            *slot.tried.lock().unwrap() = None;
        }
        (s.uid, s.username.clone().unwrap_or_default())
    };
    let is_me = if b.filler_uid > 0 { b.filler_uid == me.0 } else { b.filler == me.1 };
    if !is_me {
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

/// Each job carries the community it came from.
pub enum ChannelJob {
    Msg(String, ChannelMessage),
    History(String, ChannelHistoryResponse),
    Edited(String, ChannelMessageEdited),
    /// Message-search results: each row opens with its own channel.
    Search(String, MessageSearchResponse),
}

impl ChannelJob {
    fn server_id(&self) -> &str {
        match self {
            ChannelJob::Msg(sid, _) | ChannelJob::History(sid, _) | ChannelJob::Edited(sid, _) | ChannelJob::Search(sid, _) => sid,
        }
    }
}

/// Queue a packet on its community's worker, starting one on first use.
///
/// One worker per community (review A4): a single global one let a key
/// fetch for one community — or a server naming epochs that don't exist —
/// hold up chat on all of them. The queue is unbounded so the router never
/// waits on it: the router also carries the key replies the worker may be
/// waiting for, and a full queue deadlocked the two until the fetch timed
/// out. It drains at the rate packets arrive: a job waits for at most one
/// load per channel per `RECENT_LOAD`.
pub async fn enqueue(state: &Arc<Mutex<AppState>>, job: ChannelJob) {
    let mut s = state.lock().await;
    let sid = job.server_id().to_string();
    let job = match s.channel_crypto_tx.get(&sid) {
        Some(tx) => match tx.send(job) {
            Ok(()) => return,
            Err(mpsc::error::SendError(job)) => job, // the worker died; start another
        },
        None => job,
    };
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run_worker(rx, state.clone()));
    let _ = tx.send(job);
    s.channel_crypto_tx.insert(sid, tx);
}

async fn run_worker(mut rx: mpsc::UnboundedReceiver<ChannelJob>, state: Arc<Mutex<AppState>>) {
    while let Some(job) = rx.recv().await {
        match job {
            ChannelJob::Msg(sid, m) => handle_msg(&state, &sid, m).await,
            ChannelJob::History(sid, r) => handle_history(&state, &sid, r).await,
            ChannelJob::Edited(sid, b) => handle_edited(&state, &sid, b).await,
            ChannelJob::Search(sid, r) => handle_search(&state, &sid, r).await,
        }
    }
}

/// Who sealed a body: the uid (v2) and the name it was sent under (v1;
/// the server's `sealed_*` field, else the name on a live packet).
fn sender<'a>(uid: i64, sealed: &'a str, name: &'a str) -> channel_envelope::Sender<'a> {
    channel_envelope::Sender { uid, sealed_name: if sealed.is_empty() { name } else { sealed } }
}

async fn render(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    sender: &channel_envelope::Sender<'_>,
    content: String,
    wire: &[u8],
) -> (String, bool, String, Vec<events::EncryptedAttachmentMetaPayload>) {
    if wire.is_empty() {
        return (content, false, String::new(), Vec::new());
    }
    match open_message(state, server_id, channel_id, sender, wire).await {
        Ok((t, metas)) => (t, true, String::new(), metas.iter().map(|m| m.to_payload()).collect()),
        Err(e) => (UNREADABLE.to_string(), true, e.as_str().to_string(), Vec::new()),
    }
}

async fn handle_msg(state: &Arc<Mutex<AppState>>, server_id: &str, msg: ChannelMessage) {
    let by = sender(msg.sender_uid, &msg.sealed_sender, &msg.sender);
    let (content, encrypted, err, metas) =
        render(state, server_id, &msg.channel_id, &by, msg.content, &msg.envelope).await;
    let reply_to_content = if msg.reply_to_envelope.is_empty() {
        msg.reply_to_content
    } else {
        let parent_by = sender(msg.reply_to_sender_uid, &msg.reply_to_sealed_sender, &msg.reply_to_sender);
        render(state, server_id, &msg.channel_id, &parent_by, String::new(), &msg.reply_to_envelope).await.0
    };
    events::emit_message_received(events::MessageReceivedPayload {
        context: msg.channel_id,
        server_id: server_id.to_string(),
        sender: msg.sender,
        sender_uid: msg.sender_uid,
        recipient: String::new(),
        recipient_uid: 0,
        content,
        timestamp: msg.timestamp.to_string(),
        id: msg.id,
        attachments: msg.attachments.into_iter().map(crate::net::community::map_attachment).collect(),
        nonce: msg.nonce,
        edited_at: msg.edited_at,
        reply_to: msg.reply_to,
        reply_to_sender: msg.reply_to_sender,
        reply_to_sender_uid: msg.reply_to_sender_uid,
        reply_to_content,
        reply_to_attachment_kinds: msg.reply_to_attachment_kinds,
        encrypted,
        decrypt_error: err,
        encrypted_attachments: metas,
        suppress_embeds: msg.suppress_embeds,
    });
}

/// A stored message (history page, search result) for the renderer, with
/// its body and reply preview already opened.
fn message_payload(
    m: ChannelMessage,
    content: String,
    encrypted: bool,
    decrypt_error: String,
    metas: Vec<events::EncryptedAttachmentMetaPayload>,
    reply_to_content: String,
) -> events::ChannelMessagePayload {
    events::ChannelMessagePayload {
        id: m.id,
        sender: m.sender,
        sender_uid: m.sender_uid,
        channel_id: m.channel_id,
        content,
        timestamp: m.timestamp,
        attachments: m.attachments.into_iter().map(crate::net::community::map_attachment).collect(),
        nonce: m.nonce,
        edited_at: m.edited_at,
        reply_to: m.reply_to,
        reply_to_sender: m.reply_to_sender,
        reply_to_sender_uid: m.reply_to_sender_uid,
        reply_to_content,
        reply_to_attachment_kinds: m.reply_to_attachment_kinds,
        encrypted,
        decrypt_error,
        encrypted_attachments: metas,
        suppress_embeds: m.suppress_embeds,
    }
}

/// Opens one stored message of `channel_id` (body + reply preview).
async fn open_stored(
    state: &Arc<Mutex<AppState>>,
    server_id: &str,
    channel_id: &str,
    mut m: ChannelMessage,
) -> events::ChannelMessagePayload {
    let content = std::mem::take(&mut m.content);
    let by = sender(m.sender_uid, &m.sealed_sender, &m.sender);
    let (content, encrypted, err, metas) = render(state, server_id, channel_id, &by, content, &m.envelope).await;
    let reply_to_content = if m.reply_to_envelope.is_empty() {
        std::mem::take(&mut m.reply_to_content)
    } else {
        let parent_by = sender(m.reply_to_sender_uid, &m.reply_to_sealed_sender, &m.reply_to_sender);
        render(state, server_id, channel_id, &parent_by, String::new(), &m.reply_to_envelope).await.0
    };
    message_payload(m, content, encrypted, err, metas, reply_to_content)
}

async fn handle_history(state: &Arc<Mutex<AppState>>, server_id: &str, resp: ChannelHistoryResponse) {
    let channel_id = resp.channel_id.clone();
    let mut messages = Vec::with_capacity(resp.messages.len());
    for m in resp.messages {
        messages.push(open_stored(state, server_id, &channel_id, m).await);
    }
    events::emit_channel_history_received(events::ChannelHistoryReceivedPayload {
        server_id: server_id.to_string(),
        channel_id,
        messages,
        has_more: resp.has_more,
        has_more_after: resp.has_more_after,
        around_id: resp.around_id,
        after_id: resp.after_id,
        before_id: resp.before_id,
    });
}

fn search_payload(
    server_id: &str,
    resp: MessageSearchResponse,
    messages: Vec<events::ChannelMessagePayload>,
) -> events::MessageSearchReceivedPayload {
    events::MessageSearchReceivedPayload {
        server_id: server_id.to_string(),
        seq: resp.seq,
        success: resp.success,
        message: resp.message,
        messages,
        has_more: resp.has_more,
        next_cursor: resp.next_cursor,
        next_offset: resp.next_offset,
        total: resp.total,
        total_capped: resp.total_capped,
        text_skipped_channel_ids: resp.text_skipped_channel_ids,
        partial: resp.partial,
        retry_after_ms: resp.retry_after_ms,
        first_page: resp.first_page,
    }
}

async fn handle_search(state: &Arc<Mutex<AppState>>, server_id: &str, mut resp: MessageSearchResponse) {
    let rows = std::mem::take(&mut resp.messages);
    let mut messages = Vec::with_capacity(rows.len());
    for m in rows {
        let channel_id = m.channel_id.clone();
        messages.push(open_stored(state, server_id, &channel_id, m).await);
    }
    events::emit_message_search_received(search_payload(server_id, resp, messages));
}

/// A search page with no sealed row: nothing to open, no queue to wait in.
pub fn emit_search_plain(server_id: &str, mut resp: MessageSearchResponse) {
    let messages = std::mem::take(&mut resp.messages)
        .into_iter()
        .map(|mut m| {
            let content = std::mem::take(&mut m.content);
            let reply = std::mem::take(&mut m.reply_to_content);
            message_payload(m, content, false, String::new(), Vec::new(), reply)
        })
        .collect();
    events::emit_message_search_received(search_payload(server_id, resp, messages));
}

async fn handle_edited(state: &Arc<Mutex<AppState>>, server_id: &str, b: ChannelMessageEdited) {
    // A live edit: the editor's current name is what a v1 body binds.
    let by = sender(b.editor_uid, "", &b.editor);
    let (content, encrypted, err, metas) = render(state, server_id, &b.channel_id, &by, b.content, &b.envelope).await;
    events::emit_channel_message_edited(events::ChannelMessageEditedPayload {
        server_id: server_id.to_string(),
        channel_id: b.channel_id,
        message_id: b.message_id,
        content,
        edited_at: b.edited_at,
        editor: b.editor,
        editor_uid: b.editor_uid,
        encrypted,
        decrypt_error: err,
        encrypted_attachments: metas,
        suppress_embeds: b.suppress_embeds,
    });
}

/// Nothing to persist: keyrings are re-fetched per session (blobs live on
/// the server). Kept as a hook for a local cache later.
#[allow(dead_code)]
pub fn known_channels(s: &AppState) -> HashSet<KeyId> {
    s.channel_keys.keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    //! Review S14: which blobs a member drops, and which of those refuse
    //! their sender. Peer keys come from the pin, so nothing touches the
    //! network.
    use super::*;
    use crate::e2ee::identity::IdentityKeys;
    use crate::e2ee::keystore::{KeyStore, PeerRecord};

    const ME: i64 = 10;
    const ALICE: i64 = 20;
    const CH: &str = "sealed-room";

    fn identity(key_id: u32) -> IdentityKeys {
        let mut k = IdentityKeys::generate(1).unwrap();
        k.key_id = key_id;
        k
    }

    fn priv32(k: &IdentityKeys) -> [u8; KEY_LEN] {
        k.dh_priv.as_slice().try_into().unwrap()
    }

    /// Me at key generation 5, Alice (generation 4) pinned.
    fn session(me: &IdentityKeys, alice: &IdentityKeys) -> Arc<Mutex<AppState>> {
        let mut s = AppState::default();
        s.uid = ME;
        s.username = Some("me".into());
        s.e2ee.status = Status::Ready;
        let mut store = KeyStore::new(me.key_id, vec![me.clone()]);
        store.peers.insert(
            ALICE,
            PeerRecord {
                sign_pub: alice.sign_public().unwrap().to_vec(),
                key_id: alice.key_id,
                dh_pub: alice.dh_public().unwrap().to_vec(),
                ..Default::default()
            },
        );
        s.e2ee.store = Some(store);
        Arc::new(Mutex::new(s))
    }

    /// Alice's blob for `epoch`, sealed to my generation `my_kid`.
    fn blob(alice: &IdentityKeys, me: &IdentityKeys, my_kid: u32, epoch: u32, content: &[u8], key_check: Vec<u8>) -> ChannelKeyBlob {
        let wire = envelope::seal_bytes(&priv32(alice), ALICE, alice.key_id, &me.dh_public().unwrap(), ME, my_kid, content).unwrap();
        ChannelKeyBlob {
            epoch,
            sender: "alice".into(),
            recipient: String::new(),
            blob: wire,
            sender_uid: ALICE,
            recipient_uid: ME,
            key_check,
        }
    }

    #[tokio::test]
    async fn blobs_open_or_say_why_not() {
        let (me, alice) = (identity(5), identity(4));
        let state = session(&me, &alice);
        let key = [7u8; KEY_LEN];
        let check = |epoch, key: &[u8; KEY_LEN]| channel_envelope::key_check(CH, epoch, key).to_vec();
        let content = |epoch, key: &[u8; KEY_LEN], ch: &str| channel_envelope::key_blob_content(ch, epoch, key);

        let good = blob(&alice, &me, 5, 2, &content(2, &key, CH), check(2, &key));
        assert_eq!(unseal_blob(&state, CH, &good).await, Ok(key));
        // An epoch from before key checks existed opens on the envelope alone.
        let legacy = blob(&alice, &me, 5, 2, &content(2, &key, CH), Vec::new());
        assert_eq!(unseal_blob(&state, CH, &legacy).await, Ok(key));

        // Wrong: a key that isn't the epoch's, another epoch or channel
        // inside, or an envelope that doesn't authenticate.
        let wrong_key = blob(&alice, &me, 5, 2, &content(2, &[8u8; KEY_LEN], CH), check(2, &key));
        assert_eq!(unseal_blob(&state, CH, &wrong_key).await, Err(BlobFault::Bad));
        let wrong_epoch = blob(&alice, &me, 5, 2, &content(3, &key, CH), check(2, &key));
        assert_eq!(unseal_blob(&state, CH, &wrong_epoch).await, Err(BlobFault::Bad));
        let wrong_channel = blob(&alice, &me, 5, 2, &content(2, &key, "other"), check(2, &key));
        assert_eq!(unseal_blob(&state, CH, &wrong_channel).await, Err(BlobFault::Bad));
        let mut tampered = good.clone();
        let last = tampered.blob.len() - 1;
        tampered.blob[last] ^= 1;
        assert_eq!(unseal_blob(&state, CH, &tampered).await, Err(BlobFault::Bad));
        let garbage = ChannelKeyBlob { blob: b"GARBAGE".to_vec(), ..good.clone() };
        assert_eq!(unseal_blob(&state, CH, &garbage).await, Err(BlobFault::Bad));

        // Sealed to a generation of mine I replaced: stale, nobody's fault.
        let old = blob(&alice, &me, 3, 2, &content(2, &key, CH), check(2, &key));
        assert_eq!(unseal_blob(&state, CH, &old).await, Err(BlobFault::Stale));
        // To one newer than this device holds (another device reset): retry.
        let newer = blob(&alice, &me, 9, 2, &content(2, &key, CH), check(2, &key));
        assert_eq!(unseal_blob(&state, CH, &newer).await, Err(BlobFault::Retry));
        // Locked: nothing to say about the blob.
        state.lock().await.e2ee.status = Status::Locked;
        assert_eq!(unseal_blob(&state, CH, &good).await, Err(BlobFault::Retry));
    }

    /// Review A4: a message under an epoch we don't hold loads at most once
    /// per `RECENT_LOAD` (here the load fails at once: no community is
    /// connected), until CHANNEL_KEYS_CHANGED says there's something new.
    #[tokio::test]
    async fn the_open_path_remembers_its_last_load() {
        let state = session(&identity(5), &identity(4));
        let tried = |state: &Arc<Mutex<AppState>>| {
            let state = state.clone();
            async move {
                let s = state.lock().await;
                s.channel_key_loads.get(&key_id("srv", CH)).and_then(|slot| *slot.tried.lock().unwrap())
            }
        };
        load_for_open(&state, "srv", CH).await;
        let first = tried(&state).await.expect("the attempt is remembered");
        load_for_open(&state, "srv", CH).await;
        assert_eq!(tried(&state).await, Some(first), "no second load within the window");
        // Another community's channel of the same name is its own.
        load_for_open(&state, "other", CH).await;
        assert_eq!(tried(&state).await, Some(first));
        let changed = ChannelKeysChanged {
            channel_id: CH.into(),
            current_epoch: 3,
            filler: "someone".into(),
            rotate: false,
            filler_uid: ALICE,
        };
        on_keys_changed(&state, "srv", changed).await;
        assert_eq!(tried(&state).await, None, "a new epoch clears it");
        load_for_open(&state, "srv", CH).await;
        assert!(tried(&state).await.is_some_and(|t| t > first));
        // Forgetting the community drops its bookkeeping.
        forget_server(&mut *state.lock().await, "srv");
        assert!(tried(&state).await.is_none());
    }
}

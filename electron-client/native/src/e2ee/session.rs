//! Runtime side of E2EE DMs: the per-login state, key resolution against
//! central (with the TOFU pin), sealing on send, opening on receive, and
//! the ordered DM crypto worker that keeps decryption out of the central
//! packet router.
//!
//! Why a worker: opening an envelope may need a peer's historical public
//! key, which is a round-trip through central — and the reply arrives on
//! the same `route_packets` loop that received the message. Awaiting the
//! fetch inside the router would deadlock; a separate ordered task keeps
//! DM events in arrival order and lets the router stay non-blocking.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, Mutex};

use super::backup;
use super::call_auth;
use super::envelope::{self, OpenError};
use super::identity::{fingerprint, IdentityKeys, PublicBundle, KEY_LEN};
use super::keystore::{self, KeyStore, PeerRecord};
use crate::events;
use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::AppState;

/// What a pre-E2EE client shows for a sealed message.
pub const PLACEHOLDER: &str = "🔒 This message is end-to-end encrypted. Update Decibell to read it.";
/// Sidebar preview / bubble text when an envelope can't be opened.
pub const UNREADABLE: &str = "🔒 Encrypted message";
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The central we're on has no key endpoints (or we're logged out).
    Unavailable,
    /// Central supports it; this account has never published keys.
    NotSetUp,
    /// A backup exists on central but this device has no usable store.
    Locked,
    /// Keys loaded; sends seal, receives open.
    Ready,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unavailable => "unavailable",
            Status::NotSetUp => "not_set_up",
            Status::Locked => "locked",
            Status::Ready => "ready",
        }
    }
}

/// Lives in `AppState.e2ee`. Reset on logout.
pub struct E2ee {
    pub supported: bool,
    pub status: Status,
    pub store: Option<KeyStore>,
    /// The backup central reported during bootstrap (key_id, blob), so
    /// unlock doesn't fetch it again.
    pub pending_backup: Option<(u32, Vec<u8>)>,
    /// Peers whose *current* bundle we fetched this session. Cleared for
    /// a user on E2EE_KEYS_CHANGED and wholesale on reconnect.
    pub fresh: HashSet<String>,
    /// (username, key_id) → verified bundle, this session.
    pub cache: HashMap<(String, u32), PublicBundle>,
    /// username → their current key_id, this session.
    pub current_ids: HashMap<String, u32>,
    /// True from the moment we send our own E2EE_PUBLISH_KEYS_REQ until
    /// the local store reflects the result. Central broadcasts
    /// E2EE_KEYS_CHANGED for *us* right after the reply, and it can land
    /// before setup/reset has installed the new store — without this
    /// flag `on_keys_changed` would read the old key_id and wrongly lock
    /// the device that just rotated.
    pub publish_in_flight: bool,
}

impl Default for E2ee {
    fn default() -> Self {
        E2ee {
            supported: false,
            status: Status::Unavailable,
            store: None,
            pending_backup: None,
            fresh: HashSet::new(),
            cache: HashMap::new(),
            current_ids: HashMap::new(),
            publish_in_flight: false,
        }
    }
}

impl E2ee {
    pub fn reset(&mut self) {
        *self = E2ee::default();
    }

    pub fn status_payload(&self, username: &str) -> events::E2eeStatusPayload {
        let (key_id, fp) = match self.store.as_ref().and_then(|s| s.current()) {
            Some(k) if self.status == Status::Ready => (
                k.key_id,
                k.sign_public().map(|p| fingerprint(username, &p)).unwrap_or_default(),
            ),
            _ => (0, String::new()),
        };
        events::E2eeStatusPayload {
            supported: self.supported,
            status: self.status.as_str().to_string(),
            key_id,
            fingerprint: fp,
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn arr32(v: &[u8]) -> Option<[u8; KEY_LEN]> {
    if v.len() != KEY_LEN {
        return None;
    }
    let mut a = [0u8; KEY_LEN];
    a.copy_from_slice(v);
    Some(a)
}

pub fn emit_status(s: &AppState) {
    let username = s.username.clone().unwrap_or_default();
    events::emit_e2ee_status_changed(s.e2ee.status_payload(&username));
}

/// Send one packet over the central connection without holding the
/// AppState lock across the (bounded-channel) write.
async fn send_central(
    state: &Arc<Mutex<AppState>>,
    ty: packet::Type,
    payload: packet::Payload,
) -> Result<(), String> {
    let (tx, data) = {
        let s = state.lock().await;
        let central = s.central.as_ref().ok_or("Not connected to central server")?;
        let tx = central.connection_write_tx().ok_or("Central connection lost")?;
        (tx, build_packet(ty, payload, s.token.as_deref()))
    };
    match tokio::time::timeout(FETCH_TIMEOUT, tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".into()),
        Err(_) => Err("Send timed out".into()),
    }
}

// ── Key resolution ──────────────────────────────────────────────────

/// A user's verified bundle: `key_id` 0 = their current one. `Ok(None)`
/// = central has none (or served one that doesn't verify).
pub async fn fetch_bundle(
    state: &Arc<Mutex<AppState>>,
    username: &str,
    key_id: u32,
) -> Result<Option<PublicBundle>, String> {
    let rx = {
        let mut s = state.lock().await;
        let e = &s.e2ee;
        let resolved = if key_id == 0 { e.current_ids.get(username).copied() } else { Some(key_id) };
        if let Some(kid) = resolved {
            if let Some(b) = e.cache.get(&(username.to_string(), kid)) {
                return Ok(Some(b.clone()));
            }
        }
        let (tx, rx) = oneshot::channel();
        s.pending_key_fetches
            .entry((username.to_string(), key_id))
            .or_default()
            .push(tx);
        rx
    };
    send_central(
        state,
        packet::Type::E2eeFetchKeysReq,
        packet::Payload::E2eeFetchKeysReq(E2eeFetchKeysReq {
            username: username.to_string(),
            key_id,
        }),
    )
    .await?;
    let res = match tokio::time::timeout(FETCH_TIMEOUT, rx).await {
        Ok(Ok(res)) => res,
        Ok(Err(_)) => return Err("Central connection closed before the key reply".into()),
        Err(_) => {
            state.lock().await.pending_key_fetches.remove(&(username.to_string(), key_id));
            return Err("Key lookup timed out".into());
        }
    };
    if !res.found {
        return Ok(None);
    }
    let Some(b) = res.bundle else { return Ok(None) };
    let bundle = PublicBundle {
        username: b.username,
        key_id: b.key_id,
        dh_pub: b.dh_pub,
        sign_pub: b.sign_pub,
        signature: b.signature,
        created_at: b.created_at,
    };
    if bundle.username != username || bundle.key_id == 0 {
        log::warn!("[e2ee] bundle for {username} names {} / key {}", bundle.username, bundle.key_id);
        return Ok(None);
    }
    if let Err(e) = bundle.verify() {
        log::warn!("[e2ee] bundle for {username} rejected: {e}");
        return Ok(None);
    }
    let mut s = state.lock().await;
    let kid = bundle.key_id;
    s.e2ee.cache.insert((username.to_string(), kid), bundle.clone());
    if key_id == 0 {
        s.e2ee.current_ids.insert(username.to_string(), kid);
    }
    // Historical generations are worth persisting: an old envelope in
    // history shouldn't cost a fetch every session.
    let mut dirty = false;
    let me = s.username.clone().unwrap_or_default();
    if let Some(store) = s.e2ee.store.as_mut() {
        if store.peer_dh_pub(username, kid).is_none() {
            store.remember_peer_key(username, kid, &bundle.dh_pub);
            dirty = true;
        }
    }
    if dirty {
        if let Some(store) = s.e2ee.store.as_ref() {
            if let Err(e) = keystore::save(&me, store) {
                log::warn!("[e2ee] store save failed: {e}");
            }
        }
    }
    Ok(Some(bundle))
}

/// The peer's current identity for sealing, with the TOFU pin applied:
/// first sight pins it, a later different `sign_pub` is surfaced as a
/// key change and re-pinned. `Ok(None)` = they have no keys.
pub async fn resolve_peer_current(
    state: &Arc<Mutex<AppState>>,
    username: &str,
) -> Result<Option<PublicBundle>, String> {
    {
        let s = state.lock().await;
        if s.e2ee.fresh.contains(username) {
            if let Some(kid) = s.e2ee.current_ids.get(username) {
                return Ok(s.e2ee.cache.get(&(username.to_string(), *kid)).cloned());
            }
            return Ok(None);
        }
    }
    let bundle = fetch_bundle(state, username, 0).await?;
    let mut s = state.lock().await;
    s.e2ee.fresh.insert(username.to_string());
    let Some(bundle) = bundle.as_ref() else { return Ok(None) };
    let me = s.username.clone().unwrap_or_default();
    let mut changed = false;
    let mut dirty = false;
    if let Some(store) = s.e2ee.store.as_mut() {
        let now = now_secs();
        match store.peers.get_mut(username) {
            None => {
                store.peers.insert(
                    username.to_string(),
                    PeerRecord {
                        sign_pub: bundle.sign_pub.clone(),
                        key_id: bundle.key_id,
                        dh_pub: bundle.dh_pub.clone(),
                        first_seen: now,
                        changed_at: 0,
                        history: HashMap::new(),
                    },
                );
                dirty = true;
            }
            Some(rec) => {
                if rec.sign_pub.is_empty() {
                    // Only historical keys were cached before (opened
                    // their history without ever sending) — pin now.
                    rec.sign_pub = bundle.sign_pub.clone();
                    rec.first_seen = now;
                    dirty = true;
                } else if rec.sign_pub != bundle.sign_pub {
                    rec.sign_pub = bundle.sign_pub.clone();
                    rec.changed_at = now;
                    changed = true;
                    dirty = true;
                }
                if rec.key_id != bundle.key_id || rec.dh_pub != bundle.dh_pub {
                    rec.key_id = bundle.key_id;
                    rec.dh_pub = bundle.dh_pub.clone();
                    dirty = true;
                }
            }
        }
        store.remember_peer_key(username, bundle.key_id, &bundle.dh_pub);
    }
    if dirty {
        if let Some(store) = s.e2ee.store.as_ref() {
            if let Err(e) = keystore::save(&me, store) {
                log::warn!("[e2ee] store save failed: {e}");
            }
        }
    }
    drop(s);
    if changed {
        events::emit_e2ee_peer_changed(events::E2eePeerChangedPayload {
            username: username.to_string(),
            key_id: bundle.key_id,
            fingerprint: bundle.fingerprint(),
        });
    }
    Ok(Some(bundle.clone()))
}

// ── Outbound ────────────────────────────────────────────────────────

pub enum Outbound {
    Plain,
    Sealed(Vec<u8>),
}

/// Decide how `text` to `peer` leaves this client. Errors are user-facing.
pub async fn seal_outbound(
    state: &Arc<Mutex<AppState>>,
    peer: &str,
    text: &str,
) -> Result<Outbound, String> {
    let (status, pinned) = {
        let s = state.lock().await;
        let pinned = s
            .e2ee
            .store
            .as_ref()
            .and_then(|st| st.peers.get(peer))
            .map(|p| !p.sign_pub.is_empty())
            .unwrap_or(false);
        (s.e2ee.status, pinned)
    };
    match status {
        Status::Unavailable | Status::NotSetUp => return Ok(Outbound::Plain),
        Status::Locked => {
            if pinned {
                return Err(format!(
                    "Unlock encryption (Settings → Privacy) to send encrypted messages to {peer}."
                ));
            }
            return Ok(Outbound::Plain);
        }
        Status::Ready => {}
    }
    let bundle = resolve_peer_current(state, peer).await?;
    let Some(bundle) = bundle else {
        if pinned {
            return Err(format!(
                "{peer}'s encryption keys are missing from the server; refusing to send unencrypted."
            ));
        }
        return Ok(Outbound::Plain);
    };
    let s = state.lock().await;
    let me = s.username.clone().ok_or("Not signed in")?;
    let store = s.e2ee.store.as_ref().ok_or("Encryption keys not loaded")?;
    let mine = store.current().ok_or("Encryption keys not loaded")?;
    let my_priv = arr32(&mine.dh_priv).ok_or("Corrupt local key")?;
    let peer_pub = bundle.dh_pub_array()?;
    let wire = envelope::seal(&my_priv, &me, mine.key_id, &peer_pub, peer, bundle.key_id, text)?;
    Ok(Outbound::Sealed(wire))
}

/// Seal arbitrary tagged content to `recipient`'s current identity (the
/// same policy as a DM: None when they have no keys and aren't pinned,
/// error when pinned-but-missing). Used for channel key blobs.
pub async fn seal_outbound_raw(
    state: &Arc<Mutex<AppState>>,
    recipient: &str,
    content: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    let bundle = resolve_peer_current(state, recipient).await?;
    let Some(bundle) = bundle else { return Ok(None) };
    let s = state.lock().await;
    let me = s.username.clone().ok_or("Not signed in")?;
    let store = s.e2ee.store.as_ref().ok_or("Encryption keys not loaded")?;
    let mine = store.current().ok_or("Encryption keys not loaded")?;
    let my_priv = arr32(&mine.dh_priv).ok_or("Corrupt local key")?;
    let peer_pub = bundle.dh_pub_array()?;
    envelope::seal_bytes(&my_priv, &me, mine.key_id, &peer_pub, recipient, bundle.key_id, content).map(Some)
}

/// `open_inbound` for tagged content (returns the raw inner bytes).
pub async fn open_inbound_raw(
    state: &Arc<Mutex<AppState>>,
    sender: &str,
    recipient: &str,
    wire: &[u8],
) -> Result<Vec<u8>, DecryptError> {
    let (my_priv, my_kid, peer_pub, peer_kid, i_am_sender) = resolve_open_keys(state, sender, recipient, wire).await?;
    envelope::open_bytes(&my_priv, my_kid, &peer_pub, peer_kid, sender, recipient, i_am_sender, wire)
        .map_err(|_| DecryptError::Bad)
}

// ── Inbound ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecryptError {
    /// Keys not loaded on this device (locked / not set up / unavailable).
    Locked,
    /// Sealed to a generation of *our* key we don't have (a reset).
    NoKey,
    /// The peer's key for that generation isn't on central.
    PeerKey,
    /// Malformed or fails authentication.
    Bad,
}

impl DecryptError {
    pub fn as_str(self) -> &'static str {
        match self {
            DecryptError::Locked => "locked",
            DecryptError::NoKey => "no_key",
            DecryptError::PeerKey => "peer_key",
            DecryptError::Bad => "bad",
        }
    }
}

/// Open `wire` for a message from `sender` to `recipient` (one of them is
/// us). Fetches the peer's historical key if it isn't cached.
pub async fn open_inbound(
    state: &Arc<Mutex<AppState>>,
    sender: &str,
    recipient: &str,
    wire: &[u8],
) -> Result<String, DecryptError> {
    let (my_priv, my_kid, peer_pub, peer_kid, i_am_sender) = resolve_open_keys(state, sender, recipient, wire).await?;
    envelope::open(&my_priv, my_kid, &peer_pub, peer_kid, sender, recipient, i_am_sender, wire).map_err(
        |e| match e {
            OpenError::KeyMismatch | OpenError::Malformed | OpenError::Bad => DecryptError::Bad,
            OpenError::UnsupportedContent(_) => DecryptError::Bad,
        },
    )
}

/// The key material `open` needs for `wire`: our private key for the
/// generation the header names, the peer's public key (cached or fetched),
/// and which slot we fill.
async fn resolve_open_keys(
    state: &Arc<Mutex<AppState>>,
    sender: &str,
    recipient: &str,
    wire: &[u8],
) -> Result<([u8; KEY_LEN], u32, [u8; KEY_LEN], u32, bool), DecryptError> {
    let hdr = envelope::parse_header(wire).map_err(|_| DecryptError::Bad)?;
    let (me, i_am_sender, my_priv, my_kid, peer, peer_kid, cached_pub) = {
        let s = state.lock().await;
        if s.e2ee.status != Status::Ready {
            return Err(DecryptError::Locked);
        }
        let me = s.username.clone().unwrap_or_default();
        let store = s.e2ee.store.as_ref().ok_or(DecryptError::Locked)?;
        let i_am_sender = sender == me;
        let (my_kid, peer, peer_kid) = if i_am_sender {
            (hdr.sender_key_id, recipient.to_string(), hdr.recipient_key_id)
        } else {
            (hdr.recipient_key_id, sender.to_string(), hdr.sender_key_id)
        };
        let mine = store.key(my_kid).ok_or(DecryptError::NoKey)?;
        let my_priv = arr32(&mine.dh_priv).ok_or(DecryptError::Bad)?;
        let cached = store
            .peer_dh_pub(&peer, peer_kid)
            .or_else(|| s.e2ee.cache.get(&(peer.clone(), peer_kid)).map(|b| b.dh_pub.clone()));
        (me, i_am_sender, my_priv, my_kid, peer, peer_kid, cached)
    };
    let peer_pub = match cached_pub {
        Some(p) => p,
        None => match fetch_bundle(state, &peer, peer_kid).await {
            Ok(Some(b)) => b.dh_pub,
            Ok(None) => return Err(DecryptError::PeerKey),
            Err(e) => {
                log::warn!("[e2ee] key fetch for {peer}#{peer_kid} failed: {e}");
                return Err(DecryptError::PeerKey);
            }
        },
    };
    let peer_pub = arr32(&peer_pub).ok_or(DecryptError::Bad)?;
    let _ = me;
    Ok((my_priv, my_kid, peer_pub, peer_kid, i_am_sender))
}

/// `(content, encrypted, decrypt_error)` for one wire body: plaintext rows
/// pass through, sealed rows are opened or replaced by the placeholder.
async fn render(
    state: &Arc<Mutex<AppState>>,
    sender: &str,
    recipient: &str,
    content: String,
    envelope: &[u8],
) -> (String, bool, String) {
    if envelope.is_empty() {
        return (content, false, String::new());
    }
    match open_inbound(state, sender, recipient, envelope).await {
        Ok(text) => (text, true, String::new()),
        Err(e) => (UNREADABLE.to_string(), true, e.as_str().to_string()),
    }
}

// ── Bootstrap / setup / unlock ──────────────────────────────────────

async fn fetch_backup(state: &Arc<Mutex<AppState>>) -> Result<Option<(u32, Vec<u8>)>, String> {
    let rx = {
        let mut s = state.lock().await;
        let (tx, rx) = oneshot::channel();
        s.pending_backup_fetch = Some(tx);
        rx
    };
    send_central(
        state,
        packet::Type::E2eeFetchBackupReq,
        packet::Payload::E2eeFetchBackupReq(E2eeFetchBackupReq {}),
    )
    .await?;
    match tokio::time::timeout(FETCH_TIMEOUT, rx).await {
        Ok(Ok(res)) => Ok(if res.found { Some((res.key_id, res.backup)) } else { None }),
        Ok(Err(_)) => Err("Central connection closed before the backup reply".into()),
        Err(_) => {
            state.lock().await.pending_backup_fetch = None;
            Err("Backup lookup timed out".into())
        }
    }
}

async fn publish(
    state: &Arc<Mutex<AppState>>,
    bundle: Option<PublicBundle>,
    backup: Vec<u8>,
) -> Result<u32, String> {
    let rx = {
        let mut s = state.lock().await;
        let (tx, rx) = oneshot::channel();
        s.pending_key_publish = Some(tx);
        s.e2ee.publish_in_flight = true;
        rx
    };
    send_central(
        state,
        packet::Type::E2eePublishKeysReq,
        packet::Payload::E2eePublishKeysReq(E2eePublishKeysReq {
            bundle: bundle.map(|b| E2eeKeyBundle {
                username: b.username,
                key_id: 0,
                dh_pub: b.dh_pub,
                sign_pub: b.sign_pub,
                signature: b.signature,
                created_at: b.created_at,
            }),
            backup,
        }),
    )
    .await?;
    match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(res)) if res.success => Ok(res.key_id),
        Ok(Ok(res)) => Err(if res.message.is_empty() { "Server refused the keys".into() } else { res.message }),
        Ok(Err(_)) => Err("Central connection closed before the publish reply".into()),
        Err(_) => {
            state.lock().await.pending_key_publish = None;
            Err("Publishing keys timed out".into())
        }
    }
}

/// Runs after every successful LoginRes (spawned, never awaited by the
/// router). Decides the status for this account on this device.
pub async fn bootstrap(state: Arc<Mutex<AppState>>) {
    let (username, supported) = {
        let mut s = state.lock().await;
        // A reconnect re-runs this: forget per-session freshness so a peer
        // who rotated while we were away is re-fetched.
        s.e2ee.fresh.clear();
        s.e2ee.current_ids.clear();
        s.e2ee.cache.clear();
        (s.username.clone().unwrap_or_default(), s.e2ee.supported)
    };
    if username.is_empty() {
        return;
    }
    if !supported {
        let mut s = state.lock().await;
        s.e2ee.status = Status::Unavailable;
        s.e2ee.store = None;
        emit_status(&s);
        return;
    }
    match keystore::load(&username) {
        Ok(Some(store)) if store.current().is_some() => {
            let mut s = state.lock().await;
            s.e2ee.store = Some(store);
            s.e2ee.status = Status::Ready;
            emit_status(&s);
            return;
        }
        Ok(Some(_)) => log::warn!("[e2ee] store for {username} has no current key; treating as locked"),
        Ok(None) => {}
        Err(e) => log::warn!("[e2ee] store for {username} unreadable: {e}"),
    }
    let backup = fetch_backup(&state).await;
    let mut s = state.lock().await;
    match backup {
        Ok(Some((kid, blob))) => {
            s.e2ee.pending_backup = Some((kid, blob));
            s.e2ee.status = Status::Locked;
        }
        Ok(None) => {
            s.e2ee.status = Status::NotSetUp;
        }
        Err(e) => {
            // Can't tell — stay conservative: no setup nudge that could
            // overwrite a backup we failed to see. `setup` re-checks.
            log::warn!("[e2ee] backup lookup failed: {e}");
            s.e2ee.status = if keystore::exists(&username) { Status::Locked } else { Status::NotSetUp };
        }
    }
    emit_status(&s);
}

/// Clears `publish_in_flight` when the publishing flow ends, on every
/// path (success, error, cancellation).
struct PublishGuard(Arc<Mutex<AppState>>);
impl Drop for PublishGuard {
    fn drop(&mut self) {
        let state = self.0.clone();
        tokio::spawn(async move {
            state.lock().await.e2ee.publish_in_flight = false;
        });
    }
}

fn backup_payload(store: &KeyStore) -> backup::BackupPayload {
    backup::BackupPayload { current_key_id: store.current_key_id, keys: store.keys.clone() }
}

/// First-time setup: mint an identity, wrap it under `passphrase`,
/// publish bundle + backup in one request, persist the store.
pub async fn setup(state: &Arc<Mutex<AppState>>, passphrase: String) -> Result<(), String> {
    let username = {
        let s = state.lock().await;
        if !s.e2ee.supported {
            return Err("This server doesn't support encrypted DMs.".into());
        }
        if s.e2ee.status == Status::Ready {
            return Err("Encryption is already set up on this device.".into());
        }
        s.username.clone().ok_or("Not signed in")?
    };
    // Never publish over an existing backup: a second device that skipped
    // the unlock prompt would otherwise orphan the first one's keys.
    if let Some((kid, blob)) = fetch_backup(state).await? {
        let mut s = state.lock().await;
        s.e2ee.pending_backup = Some((kid, blob));
        s.e2ee.status = Status::Locked;
        emit_status(&s);
        return Err("You already have encryption keys. Unlock with your passphrase instead.".into());
    }
    let _guard = PublishGuard(state.clone());
    let identity = IdentityKeys::generate(now_secs())?;
    let bundle = identity.public_bundle(&username)?;
    let store = KeyStore::new(0, vec![identity]);
    let payload = backup_payload(&store);
    let pass = passphrase.clone();
    let blob = tokio::task::spawn_blocking(move || backup::wrap(&pass, &payload))
        .await
        .map_err(|e| e.to_string())??;
    let key_id = publish(state, Some(bundle), blob).await?;
    let mut store = store;
    for k in store.keys.iter_mut() {
        if k.key_id == 0 {
            k.key_id = key_id;
        }
    }
    store.current_key_id = key_id;
    keystore::save(&username, &store)?;
    let mut s = state.lock().await;
    s.e2ee.store = Some(store);
    s.e2ee.status = Status::Ready;
    s.e2ee.pending_backup = None;
    emit_status(&s);
    Ok(())
}

/// Restore the identity on this device from the central backup.
pub async fn unlock(state: &Arc<Mutex<AppState>>, passphrase: String) -> Result<(), String> {
    let (username, pending) = {
        let s = state.lock().await;
        if !s.e2ee.supported {
            return Err("This server doesn't support encrypted DMs.".into());
        }
        (s.username.clone().ok_or("Not signed in")?, s.e2ee.pending_backup.clone())
    };
    let (key_id, blob) = match pending {
        Some(p) => p,
        None => fetch_backup(state)
            .await?
            .ok_or("No encryption backup found for this account. Set up encryption instead.")?,
    };
    let payload = tokio::task::spawn_blocking(move || backup::unwrap(&passphrase, &blob))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| match e {
            backup::UnwrapError::WrongPassphrase => "Wrong passphrase.".to_string(),
            backup::UnwrapError::Malformed => "The backup on the server is corrupted.".to_string(),
        })?;
    let mut keys = payload.keys;
    for k in keys.iter_mut() {
        if k.key_id == 0 {
            k.key_id = key_id;
        }
    }
    let current = if payload.current_key_id == 0 { key_id } else { payload.current_key_id };
    if !keys.iter().any(|k| k.key_id == current) {
        return Err("The backup doesn't contain the current key.".into());
    }
    // Keep any pins/history a previous (unopenable) store held? It didn't
    // open, so there's nothing to keep — but a store that *did* open and
    // was superseded by another device's reset keeps its peers.
    let peers = {
        let s = state.lock().await;
        s.e2ee.store.as_ref().map(|st| st.peers.clone()).unwrap_or_default()
    };
    let mut store = KeyStore::new(current, keys);
    store.peers = peers;
    keystore::save(&username, &store)?;
    let mut s = state.lock().await;
    s.e2ee.store = Some(store);
    s.e2ee.status = Status::Ready;
    s.e2ee.pending_backup = None;
    emit_status(&s);
    Ok(())
}

/// Re-wrap the current keys under a new passphrase (backup-only publish).
pub async fn change_passphrase(state: &Arc<Mutex<AppState>>, passphrase: String) -> Result<(), String> {
    let payload = {
        let s = state.lock().await;
        if s.e2ee.status != Status::Ready {
            return Err("Unlock encryption first.".into());
        }
        backup_payload(s.e2ee.store.as_ref().ok_or("Encryption keys not loaded")?)
    };
    let _guard = PublishGuard(state.clone());
    let blob = tokio::task::spawn_blocking(move || backup::wrap(&passphrase, &payload))
        .await
        .map_err(|e| e.to_string())??;
    publish(state, None, blob).await?;
    Ok(())
}

/// Mint a new identity (new key_id) under `passphrase`. Old keys we still
/// hold ride along in the backup so old history stays readable; if the
/// device was locked (forgotten passphrase) they're gone.
pub async fn reset(state: &Arc<Mutex<AppState>>, passphrase: String) -> Result<(), String> {
    let (username, mut keys, peers) = {
        let s = state.lock().await;
        if !s.e2ee.supported {
            return Err("This server doesn't support encrypted DMs.".into());
        }
        let username = s.username.clone().ok_or("Not signed in")?;
        let (keys, peers) = match s.e2ee.store.as_ref() {
            Some(st) if s.e2ee.status == Status::Ready => (st.keys.clone(), st.peers.clone()),
            _ => (Vec::new(), HashMap::new()),
        };
        (username, keys, peers)
    };
    let _guard = PublishGuard(state.clone());
    let identity = IdentityKeys::generate(now_secs())?;
    let bundle = identity.public_bundle(&username)?;
    keys.retain(|k| k.key_id != 0);
    keys.push(identity);
    let payload = backup::BackupPayload { current_key_id: 0, keys: keys.clone() };
    let pass = passphrase.clone();
    let blob = tokio::task::spawn_blocking(move || backup::wrap(&pass, &payload))
        .await
        .map_err(|e| e.to_string())??;
    let key_id = publish(state, Some(bundle), blob).await?;
    for k in keys.iter_mut() {
        if k.key_id == 0 {
            k.key_id = key_id;
        }
    }
    let mut store = KeyStore::new(key_id, keys);
    store.peers = peers;
    keystore::save(&username, &store)?;
    let mut s = state.lock().await;
    s.e2ee.store = Some(store);
    s.e2ee.status = Status::Ready;
    s.e2ee.pending_backup = None;
    emit_status(&s);
    Ok(())
}

/// E2EE_KEYS_CHANGED for `username`. Another device of *ours* publishing
/// a new generation means this device's keys are stale: drop to locked so
/// the passphrase prompt re-syncs from the new backup.
pub async fn on_keys_changed(state: &Arc<Mutex<AppState>>, username: &str, key_id: u32) {
    let mut s = state.lock().await;
    s.e2ee.fresh.remove(username);
    s.e2ee.current_ids.remove(username);
    let me = s.username.clone().unwrap_or_default();
    if username == me && s.e2ee.status == Status::Ready && !s.e2ee.publish_in_flight {
        let mine = s.e2ee.store.as_ref().map(|st| st.current_key_id).unwrap_or(0);
        if mine != key_id {
            log::info!("[e2ee] our keys were rotated elsewhere (have {mine}, now {key_id}) — locking");
            s.e2ee.status = Status::Locked;
            s.e2ee.pending_backup = None;
            emit_status(&s);
        }
    }
}

// ── P2P call key authentication ─────────────────────────────────────

/// Sign our ephemeral call public key with the current identity. None
/// when encryption isn't ready on this device (the call goes unsigned).
pub async fn sign_own_call_key(
    state: &Arc<Mutex<AppState>>,
    call_id: &str,
    to: &str,
    pub_key: &[u8],
) -> Option<(Vec<u8>, u32)> {
    let s = state.lock().await;
    if s.e2ee.status != Status::Ready {
        return None;
    }
    let me = s.username.clone()?;
    let id = s.e2ee.store.as_ref()?.current()?;
    match call_auth::sign(id, call_id, &me, to, pub_key) {
        Ok(sig) => Some((sig.to_vec(), id.key_id)),
        Err(e) => {
            log::warn!("[e2ee] call key signing failed: {e}");
            None
        }
    }
}

pub enum CallKeyAuth {
    /// Signed by the peer's current identity.
    Verified,
    /// The peer has no E2EE identity — nothing to verify against.
    Unverified,
    /// The peer has an identity but the signature is missing, stale or
    /// wrong; or their keys couldn't be looked up. Refuse the call.
    Rejected(String),
}

/// Check the peer's signature over their ephemeral call key against
/// their *current* identity (TOFU-pinned, so a reset surfaces as a key
/// change like it does for DMs). Verification needs only their public
/// key, so it works even when this device hasn't set up encryption.
pub async fn verify_peer_call_key(
    state: &Arc<Mutex<AppState>>,
    peer: &str,
    call_id: &str,
    peer_pub: &[u8],
    sig: &[u8],
    key_id: u32,
) -> CallKeyAuth {
    let (me, supported) = {
        let s = state.lock().await;
        (s.username.clone().unwrap_or_default(), s.e2ee.supported)
    };
    if !supported {
        // Old central: no key endpoints at all, nobody can be verified.
        return CallKeyAuth::Unverified;
    }
    let current = match resolve_peer_current(state, peer).await {
        Ok(c) => c,
        Err(e) => return CallKeyAuth::Rejected(format!("couldn't look up their encryption keys ({e})")),
    };
    let Some(current) = current else {
        if !sig.is_empty() {
            // Signed, yet the server claims they have no identity — the
            // server is lying about one of the two.
            return CallKeyAuth::Rejected("the call is signed but the server reports no keys".into());
        }
        return CallKeyAuth::Unverified;
    };
    if sig.is_empty() || key_id == 0 {
        return CallKeyAuth::Rejected("they have encryption keys but this call isn't signed".into());
    }
    let bundle = if key_id == current.key_id {
        current.clone()
    } else {
        match fetch_bundle(state, peer, key_id).await {
            Ok(Some(b)) => b,
            Ok(None) => return CallKeyAuth::Rejected("the signing key isn't on the server".into()),
            Err(e) => return CallKeyAuth::Rejected(format!("couldn't look up the signing key ({e})")),
        }
    };
    if bundle.sign_pub != current.sign_pub {
        return CallKeyAuth::Rejected("the call was signed with a previous identity".into());
    }
    if call_auth::verify(&bundle.sign_pub, call_id, peer, &me, peer_pub, sig) {
        CallKeyAuth::Verified
    } else {
        CallKeyAuth::Rejected("the call signature is invalid".into())
    }
}

// ── The DM crypto worker ────────────────────────────────────────────

pub enum DmJob {
    Direct(DirectMessage),
    History(DmHistoryRes),
    Conversations(DmConversationsRes),
    Edited(DmMessageEdited),
}

/// Queue a DM packet for decryption + emit. Installs the worker on first
/// use; the queue is bounded so a flood degrades to back-pressure on the
/// router rather than unbounded memory.
pub async fn enqueue(state: &Arc<Mutex<AppState>>, job: DmJob) {
    let tx = {
        let mut s = state.lock().await;
        match s.dm_crypto_tx.clone() {
            Some(tx) => tx,
            None => {
                let (tx, rx) = mpsc::channel(256);
                s.dm_crypto_tx = Some(tx.clone());
                tokio::spawn(run_worker(rx, state.clone()));
                tx
            }
        }
    };
    if tx.send(job).await.is_err() {
        log::warn!("[e2ee] DM crypto worker gone; dropping packet");
    }
}

async fn run_worker(mut rx: mpsc::Receiver<DmJob>, state: Arc<Mutex<AppState>>) {
    while let Some(job) = rx.recv().await {
        match job {
            DmJob::Direct(m) => handle_direct(&state, m).await,
            DmJob::History(r) => handle_history(&state, r).await,
            DmJob::Conversations(r) => handle_conversations(&state, r).await,
            DmJob::Edited(b) => handle_edited(&state, b).await,
        }
    }
}

async fn my_name(state: &Arc<Mutex<AppState>>) -> String {
    state.lock().await.username.clone().unwrap_or_default()
}

async fn handle_direct(state: &Arc<Mutex<AppState>>, msg: DirectMessage) {
    let me = my_name(state).await;
    let (content, encrypted, err) =
        render(state, &msg.sender, &msg.recipient, msg.content, &msg.envelope).await;
    let reply_to_content = if msg.reply_to_envelope.is_empty() {
        msg.reply_to_content
    } else {
        let parent_recipient =
            if msg.reply_to_sender == me { other(&me, &msg.sender, &msg.recipient) } else { me.clone() };
        render(state, &msg.reply_to_sender, &parent_recipient, String::new(), &msg.reply_to_envelope).await.0
    };
    events::emit_message_received(events::MessageReceivedPayload {
        context: "dm".to_string(),
        server_id: String::new(),
        sender: msg.sender,
        recipient: msg.recipient,
        content,
        timestamp: msg.timestamp.to_string(),
        id: msg.id,
        attachments: Vec::new(),
        nonce: msg.nonce,
        edited_at: msg.edited_at,
        reply_to: msg.reply_to,
        reply_to_sender: msg.reply_to_sender,
        reply_to_content,
        reply_to_attachment_kinds: Vec::new(),
        encrypted,
        decrypt_error: err,
    });
}

/// The conversation partner given the two names on a packet.
fn other(me: &str, a: &str, b: &str) -> String {
    if a == me { b.to_string() } else { a.to_string() }
}

async fn handle_history(state: &Arc<Mutex<AppState>>, res: DmHistoryRes) {
    let me = my_name(state).await;
    let peer = res.peer.clone();
    let mut messages = Vec::with_capacity(res.messages.len());
    for m in res.messages {
        let recipient = if m.sender == me { peer.clone() } else { me.clone() };
        let (content, encrypted, err) = render(state, &m.sender, &recipient, m.content, &m.envelope).await;
        let reply_to_content = if m.reply_to_envelope.is_empty() {
            m.reply_to_content
        } else {
            let parent_recipient = if m.reply_to_sender == me { peer.clone() } else { me.clone() };
            render(state, &m.reply_to_sender, &parent_recipient, String::new(), &m.reply_to_envelope).await.0
        };
        messages.push(events::DmHistoryMessagePayload {
            id: m.id,
            sender: m.sender,
            content,
            timestamp: m.timestamp,
            edited_at: m.edited_at,
            reply_to: m.reply_to,
            reply_to_sender: m.reply_to_sender,
            reply_to_content,
            encrypted,
            decrypt_error: err,
        });
    }
    events::emit_dm_history_received(events::DmHistoryReceivedPayload {
        peer: res.peer,
        messages,
        has_more: res.has_more,
        has_more_after: res.has_more_after,
        around_id: res.around_id,
        after_id: res.after_id,
    });
}

async fn handle_conversations(state: &Arc<Mutex<AppState>>, res: DmConversationsRes) {
    let me = my_name(state).await;
    let mut conversations = Vec::with_capacity(res.conversations.len());
    for c in res.conversations {
        let recipient = if c.last_message_sender == me { c.peer.clone() } else { me.clone() };
        let (content, encrypted, err) = render(
            state,
            &c.last_message_sender,
            &recipient,
            c.last_message_content,
            &c.last_message_envelope,
        )
        .await;
        conversations.push(events::DmConversationPreviewPayload {
            peer: c.peer,
            last_message_content: content,
            last_message_sender: c.last_message_sender,
            last_message_id: c.last_message_id,
            last_timestamp: c.last_timestamp,
            unread_count: c.unread_count,
            encrypted,
            decrypt_error: err,
        });
    }
    events::emit_dm_conversations_received(events::DmConversationsReceivedPayload { conversations });
}

async fn handle_edited(state: &Arc<Mutex<AppState>>, b: DmMessageEdited) {
    let me = my_name(state).await;
    let (content, encrypted, err) = if b.envelope.is_empty() {
        (b.content, false, String::new())
    } else if !b.sender.is_empty() {
        let recipient = if b.sender == me { b.peer.clone() } else { me.clone() };
        render(state, &b.sender, &recipient, b.content, &b.envelope).await
    } else {
        // Older central without `sender`: the editor is either us or the
        // peer — try the likelier (peer) first, then ourselves.
        match open_inbound(state, &b.peer, &me, &b.envelope).await {
            Ok(t) => (t, true, String::new()),
            Err(DecryptError::Locked) => (UNREADABLE.to_string(), true, "locked".to_string()),
            Err(_) => match open_inbound(state, &me, &b.peer, &b.envelope).await {
                Ok(t) => (t, true, String::new()),
                Err(e) => (UNREADABLE.to_string(), true, e.as_str().to_string()),
            },
        }
    };
    events::emit_dm_message_edited(events::DmMessageEditedPayload {
        peer: b.peer,
        message_id: b.message_id,
        content,
        edited_at: b.edited_at,
        encrypted,
        decrypt_error: err,
    });
}

// ── Router hooks (called from net/central.rs with the packet in hand) ──

pub async fn on_fetch_keys_res(state: &Arc<Mutex<AppState>>, res: E2eeFetchKeysRes) {
    let waiters = {
        let mut s = state.lock().await;
        s.pending_key_fetches.remove(&(res.username.clone(), res.key_id))
    };
    if let Some(waiters) = waiters {
        for w in waiters {
            let _ = w.send(res.clone());
        }
    }
}

pub async fn on_publish_res(state: &Arc<Mutex<AppState>>, res: E2eePublishKeysRes) {
    if let Some(w) = state.lock().await.pending_key_publish.take() {
        let _ = w.send(res);
    }
}

pub async fn on_fetch_backup_res(state: &Arc<Mutex<AppState>>, res: E2eeFetchBackupRes) {
    if let Some(w) = state.lock().await.pending_backup_fetch.take() {
        let _ = w.send(res);
    }
}

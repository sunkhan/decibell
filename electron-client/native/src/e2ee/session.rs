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
use super::envelope::{self, OpenError, Parties};
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
    /// Logged out, or signed in and the bootstrap hasn't decided yet.
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

/// Lives in `AppState.e2ee`. Reset on logout. Peers are keyed by account
/// uid (uid design, stage 4), so a rename never splits a pin or a cache.
pub struct E2ee {
    /// Central stores bundle bindings and accepts rebinds
    /// (`LoginResponse.e2ee_uid_binding`). Without it we publish name-bound
    /// bundles and never rebind.
    pub uid_binding: bool,
    pub status: Status,
    pub store: Option<KeyStore>,
    /// The backup central reported during bootstrap (key_id, blob), so
    /// unlock doesn't fetch it again.
    pub pending_backup: Option<(u32, Vec<u8>)>,
    /// Peers whose *current* bundle we fetched this session. Cleared for
    /// a user on E2EE_KEYS_CHANGED and wholesale on reconnect.
    pub fresh: HashSet<i64>,
    /// (uid, key_id) → verified bundle, this session.
    pub cache: HashMap<(i64, u32), PublicBundle>,
    /// uid → their current key_id, this session.
    pub current_ids: HashMap<i64, u32>,
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
            uid_binding: false,
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

    pub fn status_payload(&self, uid: i64) -> events::E2eeStatusPayload {
        let (key_id, fp) = match self.store.as_ref().and_then(|s| s.current()) {
            Some(k) if self.status == Status::Ready => (
                k.key_id,
                k.sign_public().map(|p| fingerprint(uid, &p)).unwrap_or_default(),
            ),
            _ => (0, String::new()),
        };
        events::E2eeStatusPayload {
            // Every central serves the key endpoints; the feature is there
            // whenever we're signed in and the bootstrap has run.
            supported: self.status != Status::Unavailable,
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
    // Unit tests can't link the napi event bus (Node-only symbols), so the
    // flows they drive emit nothing there.
    #[cfg(not(test))]
    events::emit_e2ee_status_changed(s.e2ee.status_payload(s.uid));
    #[cfg(test)]
    let _ = s;
}

/// `e2ee_peer_changed`, behind the same test seam as `emit_status`.
fn emit_peer_changed(payload: events::E2eePeerChangedPayload) {
    #[cfg(not(test))]
    events::emit_e2ee_peer_changed(payload);
    #[cfg(test)]
    let _ = payload;
}

/// Persist the store under our uid (logged, never fatal: it's a cache of
/// the backup plus pins).
fn save_store(s: &AppState) {
    if let Some(store) = s.e2ee.store.as_ref() {
        if let Err(e) = keystore::save(s.uid, store) {
            log::warn!("[e2ee] store save failed: {e}");
        }
    }
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

/// `pending_key_fetches` keys: a request by uid waits on "u:<uid>", one by
/// name on "n:<name>"; a reply settles both (it carries the uid and name).
fn wait_key_uid(uid: i64) -> String {
    format!("u:{uid}")
}

fn wait_key_name(name: &str) -> String {
    format!("n:{name}")
}

/// One E2EE_FETCH_KEYS round-trip, by uid when known, else by name.
async fn fetch_keys(
    state: &Arc<Mutex<AppState>>,
    uid: i64,
    name: &str,
    key_id: u32,
) -> Result<E2eeFetchKeysRes, String> {
    let wait = if uid > 0 { wait_key_uid(uid) } else { wait_key_name(name) };
    let rx = {
        let mut s = state.lock().await;
        let (tx, rx) = oneshot::channel();
        s.pending_key_fetches.entry((wait.clone(), key_id)).or_default().push(tx);
        rx
    };
    send_central(
        state,
        packet::Type::E2eeFetchKeysReq,
        packet::Payload::E2eeFetchKeysReq(E2eeFetchKeysReq {
            username: if uid > 0 { String::new() } else { name.to_string() },
            key_id,
            uid: uid.max(0),
        }),
    )
    .await?;
    match tokio::time::timeout(FETCH_TIMEOUT, rx).await {
        Ok(Ok(res)) => Ok(res),
        Ok(Err(_)) => Err("Central connection closed before the key reply".into()),
        Err(_) => {
            state.lock().await.pending_key_fetches.remove(&(wait, key_id));
            Err("Key lookup timed out".into())
        }
    }
}

/// Check a fetch reply and cache what verifies. Returns the account's uid
/// (as asked, or as central resolved a name) and its verified bundle. The
/// session cache holds whatever verifies; the store keeps only generations
/// the pin vouches for (see `fetch_bundle`).
async fn accept_fetched(
    state: &Arc<Mutex<AppState>>,
    asked_uid: i64,
    key_id: u32,
    res: E2eeFetchKeysRes,
) -> (i64, Option<PublicBundle>) {
    let uid = if asked_uid > 0 { asked_uid } else { res.uid.max(0) };
    if !res.found || uid <= 0 {
        return (uid, None);
    }
    let Some(b) = res.bundle else { return (uid, None) };
    let bundle = PublicBundle {
        username: b.username,
        uid: b.uid,
        binding: b.binding,
        signed_name: b.signed_name,
        key_id: b.key_id,
        dh_pub: b.dh_pub,
        sign_pub: b.sign_pub,
        signature: b.signature,
        created_at: b.created_at,
    };
    if bundle.uid != uid || bundle.key_id == 0 || (key_id != 0 && bundle.key_id != key_id) {
        log::warn!("[e2ee] bundle for uid {uid}#{key_id} names uid {} / key {}", bundle.uid, bundle.key_id);
        return (uid, None);
    }
    if let Err(e) = bundle.verify() {
        log::warn!("[e2ee] bundle for uid {uid} rejected: {e}");
        return (uid, None);
    }
    let mut s = state.lock().await;
    let kid = bundle.key_id;
    s.e2ee.cache.insert((uid, kid), bundle.clone());
    if key_id == 0 {
        s.e2ee.current_ids.insert(uid, kid);
    }
    // Historical generations are worth persisting: an old envelope in
    // history shouldn't cost a fetch every session.
    let mut dirty = false;
    if let Some(store) = s.e2ee.store.as_mut() {
        // A pin from the name-keyed store moves to the uid once this
        // account's bundle proves it (by key, never by name).
        if store.adopt_legacy(uid, &bundle.sign_pub, bundle.binding, &bundle.signed_name) {
            dirty = true;
        }
        // Never a pinned peer's identity we didn't pin (review S6): written
        // here it would open their messages from then on, unchecked.
        if store.identity_known(uid, &bundle.sign_pub) && store.peer_dh_pub(uid, kid).is_none() {
            store.remember_peer_key(uid, kid, &bundle.dh_pub);
            dirty = true;
        }
    }
    if dirty {
        save_store(&s);
    }
    (uid, Some(bundle))
}

/// A user's verified bundle: `key_id` 0 = their current one. `Ok(None)`
/// = central has none (or served one that doesn't verify).
///
/// A historical generation (`key_id` ≠ 0) opens messages *from* them, so for
/// a peer we've pinned it must also carry an identity we pinned for them
/// (review S6): a self-signature only proves the bundle is self-consistent,
/// and central could otherwise serve an identity of its own under a key_id
/// nobody pinned and forge their side of the history with no key change
/// shown. The current one is returned as served: `resolve_peer_current`
/// applies the pin to it (surfacing a change), other callers only display
/// it or check our own.
pub async fn fetch_bundle(
    state: &Arc<Mutex<AppState>>,
    uid: i64,
    key_id: u32,
) -> Result<Option<PublicBundle>, String> {
    let Some(bundle) = fetch_verified(state, uid, key_id).await? else { return Ok(None) };
    if key_id == 0 || identity_known(state, uid, &bundle.sign_pub).await {
        return Ok(Some(bundle));
    }
    accept_unpinned_generation(state, uid, bundle).await
}

/// `fetch_bundle` without the pin: from the session cache or central,
/// signature and uid checked.
async fn fetch_verified(
    state: &Arc<Mutex<AppState>>,
    uid: i64,
    key_id: u32,
) -> Result<Option<PublicBundle>, String> {
    if uid <= 0 {
        return Ok(None);
    }
    {
        let s = state.lock().await;
        let e = &s.e2ee;
        let resolved = if key_id == 0 { e.current_ids.get(&uid).copied() } else { Some(key_id) };
        if let Some(kid) = resolved {
            if let Some(b) = e.cache.get(&(uid, kid)) {
                return Ok(Some(b.clone()));
            }
        }
    }
    let res = fetch_keys(state, uid, "", key_id).await?;
    Ok(accept_fetched(state, uid, key_id, res).await.1)
}

/// `KeyStore::identity_known` for `uid`; with no store loaded there are no
/// pins to hold anything against.
async fn identity_known(state: &Arc<Mutex<AppState>>, uid: i64, sign_pub: &[u8]) -> bool {
    let s = state.lock().await;
    s.e2ee.store.as_ref().map(|st| st.identity_known(uid, sign_pub)).unwrap_or(true)
}

/// A historical generation of a pinned peer under an identity we never
/// pinned for them. The honest way to meet one is a key change we haven't
/// seen yet — it is their new current generation — so resolve their current
/// identity (which surfaces the change and re-pins, exactly as a send would)
/// and accept the generation only if it carries that identity. Anything else
/// is central vouching for an identity under an old key_id: refused and
/// never persisted, so the message shows as unreadable (`PeerKey`).
async fn accept_unpinned_generation(
    state: &Arc<Mutex<AppState>>,
    uid: i64,
    bundle: PublicBundle,
) -> Result<Option<PublicBundle>, String> {
    let was_fresh = state.lock().await.e2ee.fresh.contains(&uid);
    let mut current = resolve_peer_current(state, uid, "").await?;
    // This session's idea of their current keys can predate a rotation we
    // weren't told about (E2EE_KEYS_CHANGED only reaches watchers): a
    // generation newer than it is worth one fresh look.
    let stale = current
        .as_ref()
        .is_some_and(|c| c.sign_pub != bundle.sign_pub && c.key_id < bundle.key_id);
    if was_fresh && stale {
        {
            let mut s = state.lock().await;
            s.e2ee.fresh.remove(&uid);
            s.e2ee.current_ids.remove(&uid);
        }
        current = resolve_peer_current(state, uid, "").await?;
    }
    if current.is_some_and(|c| c.sign_pub == bundle.sign_pub) {
        remember_generation(state, uid, &bundle).await;
        return Ok(Some(bundle));
    }
    log::warn!(
        "[e2ee] uid {uid}'s key {} carries an identity never pinned for them; refusing it",
        bundle.key_id
    );
    Ok(None)
}

/// Keep a generation the pin vouches for in the store.
async fn remember_generation(state: &Arc<Mutex<AppState>>, uid: i64, bundle: &PublicBundle) {
    let mut s = state.lock().await;
    let added = match s.e2ee.store.as_mut() {
        Some(store) if store.peer_dh_pub(uid, bundle.key_id).is_none() => {
            store.remember_peer_key(uid, bundle.key_id, &bundle.dh_pub);
            true
        }
        _ => false,
    };
    if added {
        save_store(&s);
    }
}

/// The account behind a name when the caller doesn't know its uid: a key
/// lookup by name (central answers with the uid even when the user has
/// no keys). 0 = no such account.
pub async fn resolve_uid(state: &Arc<Mutex<AppState>>, uid: i64, name: &str) -> Result<i64, String> {
    if uid > 0 {
        return Ok(uid);
    }
    if name.is_empty() {
        return Ok(0);
    }
    let res = fetch_keys(state, 0, name, 0).await?;
    Ok(accept_fetched(state, 0, 0, res).await.0)
}

/// The peer's current identity for sealing, with the TOFU pin applied:
/// first sight pins it, a later different `sign_pub` is surfaced as a
/// key change and re-pinned. `Ok(None)` = they have no usable keys. `name`
/// is only a fallback for an unknown uid.
pub async fn resolve_peer_current(
    state: &Arc<Mutex<AppState>>,
    uid: i64,
    name: &str,
) -> Result<Option<PublicBundle>, String> {
    let uid = resolve_uid(state, uid, name).await?;
    if uid <= 0 {
        return Ok(None);
    }
    {
        let s = state.lock().await;
        if s.e2ee.fresh.contains(&uid) {
            let cached = s.e2ee.current_ids.get(&uid).and_then(|kid| s.e2ee.cache.get(&(uid, *kid)));
            return Ok(cached.filter(|b| !b.stale_name_binding()).cloned());
        }
    }
    // Unpinned fetch: the pin is applied below. (`fetch_bundle` would also
    // recurse: an unpinned generation resolves the current one through here.)
    let bundle = fetch_verified(state, uid, 0).await?;
    let mut s = state.lock().await;
    s.e2ee.fresh.insert(uid);
    let Some(bundle) = bundle.as_ref() else { return Ok(None) };
    if bundle.stale_name_binding() {
        // A v1 signature over a name the account no longer has proves
        // nothing about who holds the key now: never seal to it.
        log::warn!(
            "[e2ee] uid {uid}'s current keys are signed for the name {:?}, not {:?}; treating as no keys",
            bundle.signed_name, bundle.username
        );
        return Ok(None);
    }
    let mut changed = false;
    let mut dirty = false;
    if let Some(store) = s.e2ee.store.as_mut() {
        let now = now_secs();
        match store.peers.get_mut(&uid) {
            None => {
                store.peers.insert(
                    uid,
                    PeerRecord {
                        sign_pub: bundle.sign_pub.clone(),
                        key_id: bundle.key_id,
                        dh_pub: bundle.dh_pub.clone(),
                        first_seen: now,
                        changed_at: 0,
                        history: HashMap::new(),
                        previous: Vec::new(),
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
                    // The old identity stays known for its history.
                    rec.repin(&bundle.sign_pub, now);
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
        store.remember_peer_key(uid, bundle.key_id, &bundle.dh_pub);
    }
    if dirty {
        save_store(&s);
    }
    drop(s);
    if changed {
        emit_peer_changed(events::E2eePeerChangedPayload {
            username: bundle.username.clone(),
            uid,
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

/// Decide how `text` to the peer leaves this client. Errors are
/// user-facing. `peer_uid` 0 = unknown (resolved from `peer` first).
pub async fn seal_outbound(
    state: &Arc<Mutex<AppState>>,
    peer_uid: i64,
    peer: &str,
    text: &str,
) -> Result<Outbound, String> {
    let status = state.lock().await.e2ee.status;
    if matches!(status, Status::Unavailable | Status::NotSetUp) {
        return Ok(Outbound::Plain);
    }
    let uid = resolve_uid(state, peer_uid, peer).await?;
    if uid <= 0 {
        // No such account: central refuses the message itself.
        return Ok(Outbound::Plain);
    }
    let pinned = {
        let s = state.lock().await;
        s.e2ee.store.as_ref().map(|st| st.is_pinned(uid)).unwrap_or(false)
    };
    if status == Status::Locked {
        if pinned {
            return Err(format!(
                "Unlock encryption (Settings → Privacy) to send encrypted messages to {peer}."
            ));
        }
        return Ok(Outbound::Plain);
    }
    let bundle = resolve_peer_current(state, uid, peer).await?;
    let Some(bundle) = bundle else {
        if pinned {
            return Err(format!(
                "{peer}'s encryption keys are missing from the server; refusing to send unencrypted."
            ));
        }
        return Ok(Outbound::Plain);
    };
    let s = state.lock().await;
    let store = s.e2ee.store.as_ref().ok_or("Encryption keys not loaded")?;
    let mine = store.current().ok_or("Encryption keys not loaded")?;
    let my_priv = arr32(&mine.dh_priv).ok_or("Corrupt local key")?;
    let peer_pub = bundle.dh_pub_array()?;
    let wire = envelope::seal(&my_priv, s.uid, mine.key_id, &peer_pub, uid, bundle.key_id, text)?;
    Ok(Outbound::Sealed(wire))
}

/// Seal arbitrary tagged content to the recipient's current identity (TOFU
/// as for a DM: a changed identity is surfaced and re-pinned). Used for
/// channel key blobs. `None` whenever they have no usable keys — pinned or
/// not, unlike `seal_outbound`: skipping a recipient seals nothing, so it
/// can't leak, while an error here would abort the whole escrow and let a
/// server stall channel keys for everyone by withholding one pinned
/// member's bundle.
pub async fn seal_outbound_raw(
    state: &Arc<Mutex<AppState>>,
    recipient_uid: i64,
    recipient: &str,
    content: &[u8],
) -> Result<Option<Vec<u8>>, String> {
    let bundle = resolve_peer_current(state, recipient_uid, recipient).await?;
    let Some(bundle) = bundle else { return Ok(None) };
    let s = state.lock().await;
    let store = s.e2ee.store.as_ref().ok_or("Encryption keys not loaded")?;
    let mine = store.current().ok_or("Encryption keys not loaded")?;
    let my_priv = arr32(&mine.dh_priv).ok_or("Corrupt local key")?;
    let peer_pub = bundle.dh_pub_array()?;
    envelope::seal_bytes(&my_priv, s.uid, mine.key_id, &peer_pub, bundle.uid, bundle.key_id, content).map(Some)
}

/// `open_inbound` for tagged content (returns the raw inner bytes).
pub async fn open_inbound_raw(
    state: &Arc<Mutex<AppState>>,
    parties: &Parties<'_>,
    wire: &[u8],
) -> Result<Vec<u8>, DecryptError> {
    let (my_priv, my_kid, peer_pub, peer_kid, i_am_sender) = resolve_open_keys(state, parties, wire).await?;
    envelope::open_bytes(&my_priv, my_kid, &peer_pub, peer_kid, parties, i_am_sender, wire)
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

/// Open `wire` for a message between `parties` (one of them is us).
/// Fetches the peer's historical key if it isn't cached.
pub async fn open_inbound(
    state: &Arc<Mutex<AppState>>,
    parties: &Parties<'_>,
    wire: &[u8],
) -> Result<String, DecryptError> {
    let (my_priv, my_kid, peer_pub, peer_kid, i_am_sender) = resolve_open_keys(state, parties, wire).await?;
    envelope::open(&my_priv, my_kid, &peer_pub, peer_kid, parties, i_am_sender, wire).map_err(
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
    parties: &Parties<'_>,
    wire: &[u8],
) -> Result<([u8; KEY_LEN], u32, [u8; KEY_LEN], u32, bool), DecryptError> {
    let hdr = envelope::parse_header(wire).map_err(|_| DecryptError::Bad)?;
    let (i_am_sender, my_priv, my_kid, peer_uid, peer_kid, cached_pub) = {
        let s = state.lock().await;
        if s.e2ee.status != Status::Ready {
            return Err(DecryptError::Locked);
        }
        let store = s.e2ee.store.as_ref().ok_or(DecryptError::Locked)?;
        // By uid only: a row with no account (0) or a placeholder isn't ours.
        let i_am_sender = parties.sender_uid != 0 && parties.sender_uid == s.uid;
        let (my_kid, peer_uid, peer_name, peer_kid) = if i_am_sender {
            (hdr.sender_key_id, parties.recipient_uid, parties.recipient_name, hdr.recipient_key_id)
        } else {
            (hdr.recipient_key_id, parties.sender_uid, parties.sender_name, hdr.sender_key_id)
        };
        let mine = store.key(my_kid).ok_or(DecryptError::NoKey)?;
        let my_priv = arr32(&mine.dh_priv).ok_or(DecryptError::Bad)?;
        // A v1 envelope may also open with a not-yet-adopted pin under the
        // name it was sealed with (both predate renames). The session cache
        // holds whatever central served and verified: only an identity pinned
        // for the peer may open their side from it (review S6) — anything
        // else goes through `fetch_bundle`'s check below.
        let cached = store
            .peer_dh_pub(peer_uid, peer_kid)
            .or_else(|| if hdr.version == envelope::VERSION_V1 { store.legacy_dh_pub(peer_name, peer_kid) } else { None })
            .or_else(|| {
                s.e2ee
                    .cache
                    .get(&(peer_uid, peer_kid))
                    .filter(|b| store.identity_known(peer_uid, &b.sign_pub))
                    .map(|b| b.dh_pub.clone())
            });
        (i_am_sender, my_priv, my_kid, peer_uid, peer_kid, cached)
    };
    let peer_pub = match cached_pub {
        Some(p) => p,
        None => match fetch_bundle(state, peer_uid, peer_kid).await {
            Ok(Some(b)) => b.dh_pub,
            Ok(None) => return Err(DecryptError::PeerKey),
            Err(e) => {
                log::warn!("[e2ee] key fetch for uid {peer_uid}#{peer_kid} failed: {e}");
                return Err(DecryptError::PeerKey);
            }
        },
    };
    let peer_pub = arr32(&peer_pub).ok_or(DecryptError::Bad)?;
    Ok((my_priv, my_kid, peer_pub, peer_kid, i_am_sender))
}

/// `(content, encrypted, decrypt_error)` for one wire body: plaintext rows
/// pass through, sealed rows are opened or replaced by the placeholder.
async fn render(
    state: &Arc<Mutex<AppState>>,
    parties: &Parties<'_>,
    content: String,
    envelope: &[u8],
) -> (String, bool, String) {
    if envelope.is_empty() {
        return (content, false, String::new());
    }
    match open_inbound(state, parties, envelope).await {
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

/// E2EE_PUBLISH_KEYS_REQ. `rebind` re-signs the current bundle in place
/// (`bundle` then names its key_id; no backup).
async fn publish(
    state: &Arc<Mutex<AppState>>,
    bundle: Option<PublicBundle>,
    backup: Vec<u8>,
    rebind: bool,
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
                key_id: if rebind { b.key_id } else { 0 },
                dh_pub: b.dh_pub,
                sign_pub: b.sign_pub,
                signature: b.signature,
                created_at: b.created_at,
                uid: 0,
                signed_name: String::new(),
                binding: b.binding,
            }),
            backup,
            rebind,
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
    let (uid, username) = {
        let mut s = state.lock().await;
        // A reconnect re-runs this: forget per-session freshness so a peer
        // who rotated while we were away is re-fetched.
        s.e2ee.fresh.clear();
        s.e2ee.current_ids.clear();
        s.e2ee.cache.clear();
        (s.uid, s.username.clone().unwrap_or_default())
    };
    if username.is_empty() {
        return;
    }
    // Nothing central says turns this off (review S8): `LoginResponse.
    // e2ee_keys` used to gate it, and a central answering false left the
    // store — and with it every pin — unloaded, so DMs to pinned peers went
    // out in plaintext. Every central serves the key endpoints, so the store
    // always loads. Everything binds the account uid; a central that doesn't
    // send ours predates users by uid.
    if uid <= 0 {
        let mut s = state.lock().await;
        s.e2ee.status = Status::Unavailable;
        s.e2ee.store = None;
        emit_status(&s);
        return;
    }
    let loaded = match keystore::load(uid) {
        Ok(None) => migrate_own_legacy_store(&state, uid, &username).await,
        other => other,
    };
    match loaded {
        Ok(Some(store)) if store.current().is_some() => {
            {
                let mut s = state.lock().await;
                s.e2ee.store = Some(store);
                s.e2ee.status = Status::Ready;
                emit_status(&s);
            }
            rebind_if_needed(&state).await;
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
            s.e2ee.status = if keystore::exists(uid, &username) { Status::Locked } else { Status::NotSetUp };
        }
    }
    emit_status(&s);
}

/// Our pre-uid store (filed under our name) becomes the uid file only when
/// central's current bundle for our uid is one of its keys: on a shared OS
/// profile, a reused name's file may be another account's.
async fn migrate_own_legacy_store(
    state: &Arc<Mutex<AppState>>,
    uid: i64,
    username: &str,
) -> Result<Option<KeyStore>, String> {
    let Some(store) = keystore::load_legacy(username)? else { return Ok(None) };
    let ours = match fetch_bundle(state, uid, 0).await {
        Ok(Some(b)) => store
            .keys
            .iter()
            .any(|k| k.sign_public().map(|p| p.as_slice() == b.sign_pub.as_slice()).unwrap_or(false)),
        Ok(None) => false,
        Err(e) => {
            log::warn!("[e2ee] can't check the pre-uid key store against central: {e}");
            return Ok(None);
        }
    };
    if !ours {
        log::warn!("[e2ee] the key store filed under {username:?} isn't this account's; left alone");
        return Ok(None);
    }
    keystore::adopt_legacy_file(uid, username, &store)?;
    Ok(Some(store))
}

/// Re-sign our current bundle over the uid when central still holds the
/// name-bound one (uid design, stage 4). Same keys, same key_id: central
/// swaps the signature in place, so history and other devices are
/// untouched. Runs whenever the keys become ready on this device.
pub async fn rebind_if_needed(state: &Arc<Mutex<AppState>>) {
    let (uid, username, identity) = {
        let s = state.lock().await;
        if !s.e2ee.uid_binding || s.e2ee.status != Status::Ready || s.uid <= 0 {
            return;
        }
        let Some(id) = s.e2ee.store.as_ref().and_then(|st| st.current()).cloned() else { return };
        (s.uid, s.username.clone().unwrap_or_default(), id)
    };
    let published = match fetch_bundle(state, uid, 0).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(e) => {
            log::warn!("[e2ee] rebind: own key lookup failed: {e}");
            return;
        }
    };
    if published.binds_uid() {
        return;
    }
    let same_keys = published.key_id == identity.key_id
        && identity.sign_public().map(|k| k.to_vec()) == Ok(published.sign_pub.clone())
        && identity.dh_public().map(|k| k.to_vec()) == Ok(published.dh_pub.clone());
    if !same_keys {
        // Another device rotated; E2EE_KEYS_CHANGED locks this one.
        log::info!("[e2ee] rebind: central's current keys aren't this device's; skipping");
        return;
    }
    let bundle = match identity.public_bundle(uid, &username) {
        Ok(b) => b,
        Err(e) => {
            log::warn!("[e2ee] rebind: {e}");
            return;
        }
    };
    let _guard = PublishGuard(state.clone());
    match publish(state, Some(bundle), Vec::new(), true).await {
        Ok(kid) => log::info!("[e2ee] keys {kid} re-signed over the account uid"),
        Err(e) => log::warn!("[e2ee] rebind refused: {e}"),
    }
}

/// The bundle to publish for a new identity: uid-bound when central
/// supports it, name-bound otherwise.
fn new_bundle(s: &AppState, identity: &IdentityKeys, username: &str) -> Result<PublicBundle, String> {
    if s.e2ee.uid_binding {
        identity.public_bundle(s.uid, username)
    } else {
        identity.public_bundle_v1(username)
    }
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
        if s.uid <= 0 {
            return Err("Not signed in".into());
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
    let bundle = new_bundle(&*state.lock().await, &identity, &username)?;
    let store = KeyStore::new(0, vec![identity]);
    let payload = backup_payload(&store);
    let pass = passphrase.clone();
    let blob = tokio::task::spawn_blocking(move || backup::wrap(&pass, &payload))
        .await
        .map_err(|e| e.to_string())??;
    let key_id = publish(state, Some(bundle), blob, false).await?;
    let mut store = store;
    for k in store.keys.iter_mut() {
        if k.key_id == 0 {
            k.key_id = key_id;
        }
    }
    store.current_key_id = key_id;
    let mut s = state.lock().await;
    keystore::save(s.uid, &store)?;
    s.e2ee.store = Some(store);
    s.e2ee.status = Status::Ready;
    s.e2ee.pending_backup = None;
    emit_status(&s);
    Ok(())
}

/// Restore the identity on this device from the central backup.
pub async fn unlock(state: &Arc<Mutex<AppState>>, passphrase: String) -> Result<(), String> {
    let pending = {
        let s = state.lock().await;
        if s.uid <= 0 {
            return Err("Not signed in".into());
        }
        s.username.as_ref().ok_or("Not signed in")?;
        s.e2ee.pending_backup.clone()
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
    let (peers, legacy_peers) = {
        let s = state.lock().await;
        s.e2ee
            .store
            .as_ref()
            .map(|st| (st.peers.clone(), st.legacy_peers.clone()))
            .unwrap_or_default()
    };
    let mut store = KeyStore::new(current, keys);
    store.peers = peers;
    store.legacy_peers = legacy_peers;
    {
        let mut s = state.lock().await;
        keystore::save(s.uid, &store)?;
        s.e2ee.store = Some(store);
        s.e2ee.status = Status::Ready;
        s.e2ee.pending_backup = None;
        emit_status(&s);
    }
    let st = state.clone();
    tokio::spawn(async move { rebind_if_needed(&st).await });
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
    publish(state, None, blob, false).await?;
    Ok(())
}

/// Mint a new identity (new key_id) under `passphrase`. Old keys we still
/// hold ride along in the backup so old history stays readable; if the
/// device was locked (forgotten passphrase) they're gone.
pub async fn reset(state: &Arc<Mutex<AppState>>, passphrase: String) -> Result<(), String> {
    let (mut keys, peers, legacy_peers, bundle_for) = {
        let s = state.lock().await;
        if s.uid <= 0 {
            return Err("Not signed in".into());
        }
        let username = s.username.clone().ok_or("Not signed in")?;
        let (keys, peers, legacy) = match s.e2ee.store.as_ref() {
            Some(st) if s.e2ee.status == Status::Ready => {
                (st.keys.clone(), st.peers.clone(), st.legacy_peers.clone())
            }
            _ => (Vec::new(), HashMap::new(), HashMap::new()),
        };
        (keys, peers, legacy, username)
    };
    let _guard = PublishGuard(state.clone());
    let identity = IdentityKeys::generate(now_secs())?;
    let bundle = new_bundle(&*state.lock().await, &identity, &bundle_for)?;
    keys.retain(|k| k.key_id != 0);
    keys.push(identity);
    let payload = backup::BackupPayload { current_key_id: 0, keys: keys.clone() };
    let pass = passphrase.clone();
    let blob = tokio::task::spawn_blocking(move || backup::wrap(&pass, &payload))
        .await
        .map_err(|e| e.to_string())??;
    let key_id = publish(state, Some(bundle), blob, false).await?;
    for k in keys.iter_mut() {
        if k.key_id == 0 {
            k.key_id = key_id;
        }
    }
    let mut store = KeyStore::new(key_id, keys);
    store.peers = peers;
    store.legacy_peers = legacy_peers;
    let mut s = state.lock().await;
    keystore::save(s.uid, &store)?;
    s.e2ee.store = Some(store);
    s.e2ee.status = Status::Ready;
    s.e2ee.pending_backup = None;
    emit_status(&s);
    Ok(())
}

/// E2EE_KEYS_CHANGED for `uid`. Another device of *ours* publishing a new
/// generation means this device's keys are stale: drop to locked so the
/// passphrase prompt re-syncs from the new backup. A rebind keeps the
/// key_id, so it never locks anyone.
pub async fn on_keys_changed(state: &Arc<Mutex<AppState>>, uid: i64, key_id: u32) {
    let mut s = state.lock().await;
    s.e2ee.fresh.remove(&uid);
    s.e2ee.current_ids.remove(&uid);
    // A rebind replaces the signature under the same key_id.
    s.e2ee.cache.remove(&(uid, key_id));
    if uid > 0 && uid == s.uid && s.e2ee.status == Status::Ready && !s.e2ee.publish_in_flight {
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
/// `to_uid` 0 = unknown (resolved from `to`).
pub async fn sign_own_call_key(
    state: &Arc<Mutex<AppState>>,
    call_id: &str,
    to_uid: i64,
    to: &str,
    pub_key: &[u8],
) -> Option<(Vec<u8>, u32)> {
    if state.lock().await.e2ee.status != Status::Ready {
        return None;
    }
    let to_uid = match resolve_uid(state, to_uid, to).await {
        Ok(u) if u > 0 => u,
        Ok(_) => return None,
        Err(e) => {
            log::warn!("[e2ee] call key signing: couldn't resolve {to}: {e}");
            return None;
        }
    };
    let s = state.lock().await;
    let id = s.e2ee.store.as_ref()?.current()?;
    match call_auth::sign(id, call_id, s.uid, to_uid, pub_key) {
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
    /// The peer has no E2EE identity and we never pinned one — nothing to
    /// verify against.
    Unverified,
    /// The peer has an identity (or we pinned one and central now reports
    /// none) but the signature is missing, stale or wrong; or their keys
    /// couldn't be looked up. Refuse the call.
    Rejected(String),
}

/// Check the peer's signature over their ephemeral call key against
/// their *current* identity (TOFU-pinned, so a reset surfaces as a key
/// change like it does for DMs). Verification needs only their public
/// key, so it works even when this device hasn't set up encryption.
#[allow(clippy::too_many_arguments)]
pub async fn verify_peer_call_key(
    state: &Arc<Mutex<AppState>>,
    peer_uid: i64,
    peer: &str,
    call_id: &str,
    peer_pub: &[u8],
    sig: &[u8],
    key_id: u32,
) -> CallKeyAuth {
    let me = state.lock().await.uid;
    if me <= 0 {
        // Not signed in: there is no call to verify.
        return CallKeyAuth::Unverified;
    }
    let peer_uid = match resolve_uid(state, peer_uid, peer).await {
        Ok(u) if u > 0 => u,
        Ok(_) => return CallKeyAuth::Rejected("their account couldn't be found".into()),
        Err(e) => return CallKeyAuth::Rejected(format!("couldn't look up their account ({e})")),
    };
    let current = match resolve_peer_current(state, peer_uid, peer).await {
        Ok(c) => c,
        Err(e) => return CallKeyAuth::Rejected(format!("couldn't look up their encryption keys ({e})")),
    };
    let Some(current) = current else {
        if !sig.is_empty() {
            // Signed, yet the server claims they have no identity — the
            // server is lying about one of the two.
            return CallKeyAuth::Rejected("the call is signed but the server reports no keys".into());
        }
        // A pinned peer never drops to "not verified" (review S7), as a DM
        // to them never drops to plaintext (`seal_outbound`): a central that
        // withholds their bundle and strips the signature could otherwise
        // swap the ephemeral keys and sit in the middle of the call.
        let pinned = {
            let s = state.lock().await;
            s.e2ee.store.as_ref().map(|st| st.is_pinned(peer_uid)).unwrap_or(false)
        };
        if pinned {
            return CallKeyAuth::Rejected("their encryption keys are missing from the server".into());
        }
        return CallKeyAuth::Unverified;
    };
    if sig.is_empty() || key_id == 0 {
        return CallKeyAuth::Rejected("they have encryption keys but this call isn't signed".into());
    }
    let bundle = if key_id == current.key_id {
        current.clone()
    } else {
        match fetch_bundle(state, peer_uid, key_id).await {
            Ok(Some(b)) => b,
            Ok(None) => return CallKeyAuth::Rejected("the signing key isn't on the server".into()),
            Err(e) => return CallKeyAuth::Rejected(format!("couldn't look up the signing key ({e})")),
        }
    };
    if bundle.sign_pub != current.sign_pub {
        return CallKeyAuth::Rejected("the call was signed with a previous identity".into());
    }
    if call_auth::verify(&bundle.sign_pub, call_id, peer_uid, me, peer_pub, sig) {
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

/// Our (uid, name).
async fn me(state: &Arc<Mutex<AppState>>) -> (i64, String) {
    let s = state.lock().await;
    (s.uid, s.username.clone().unwrap_or_default())
}

/// The sealed-under name when the server sent one, else the fallback (a
/// live packet's names are current, which is what it was sealed under).
fn sealed<'a>(sealed: &'a str, fallback: &'a str) -> &'a str {
    if sealed.is_empty() { fallback } else { sealed }
}

async fn handle_direct(state: &Arc<Mutex<AppState>>, msg: DirectMessage) {
    let parties = Parties {
        sender_uid: msg.sender_uid,
        recipient_uid: msg.recipient_uid,
        sender_name: &msg.sender,
        recipient_name: &msg.recipient,
    };
    let (content, encrypted, err) = render(state, &parties, msg.content, &msg.envelope).await;
    let reply_to_content = if msg.reply_to_envelope.is_empty() {
        msg.reply_to_content
    } else {
        // The parent is between the same two accounts; its recipient is
        // whichever of them didn't write it.
        let parent_by_sender = msg.reply_to_sender_uid == msg.sender_uid;
        let (to_uid, to_name) = if parent_by_sender {
            (msg.recipient_uid, msg.recipient.as_str())
        } else {
            (msg.sender_uid, msg.sender.as_str())
        };
        let parent = Parties {
            sender_uid: msg.reply_to_sender_uid,
            recipient_uid: to_uid,
            sender_name: sealed(&msg.reply_to_sealed_sender, &msg.reply_to_sender),
            recipient_name: sealed(&msg.reply_to_sealed_recipient, to_name),
        };
        render(state, &parent, String::new(), &msg.reply_to_envelope).await.0
    };
    events::emit_message_received(events::MessageReceivedPayload {
        context: "dm".to_string(),
        server_id: String::new(),
        sender: msg.sender,
        sender_uid: msg.sender_uid,
        recipient: msg.recipient,
        recipient_uid: msg.recipient_uid,
        content,
        timestamp: msg.timestamp.to_string(),
        id: msg.id,
        attachments: Vec::new(),
        nonce: msg.nonce,
        edited_at: msg.edited_at,
        reply_to: msg.reply_to,
        reply_to_sender: msg.reply_to_sender,
        reply_to_sender_uid: msg.reply_to_sender_uid,
        reply_to_content,
        reply_to_attachment_kinds: Vec::new(),
        encrypted,
        decrypt_error: err,
        encrypted_attachments: Vec::new(),
        suppress_embeds: false,
    });
}

async fn handle_history(state: &Arc<Mutex<AppState>>, res: DmHistoryRes) {
    let (me_uid, me_name) = me(state).await;
    let (peer_uid, peer) = (res.peer_uid, res.peer.clone());
    // The other side of a row written by `sender_uid`.
    let to = |sender_uid: i64| -> (i64, &str) {
        if sender_uid == me_uid { (peer_uid, peer.as_str()) } else { (me_uid, me_name.as_str()) }
    };
    let mut messages = Vec::with_capacity(res.messages.len());
    for m in res.messages {
        let (to_uid, to_name) = to(m.sender_uid);
        let parties = Parties {
            sender_uid: m.sender_uid,
            recipient_uid: to_uid,
            sender_name: sealed(&m.sealed_sender, &m.sender),
            recipient_name: sealed(&m.sealed_recipient, to_name),
        };
        let (content, encrypted, err) = render(state, &parties, m.content, &m.envelope).await;
        let reply_to_content = if m.reply_to_envelope.is_empty() {
            m.reply_to_content
        } else {
            let (to_uid, to_name) = to(m.reply_to_sender_uid);
            let parent = Parties {
                sender_uid: m.reply_to_sender_uid,
                recipient_uid: to_uid,
                sender_name: sealed(&m.reply_to_sealed_sender, &m.reply_to_sender),
                recipient_name: sealed(&m.reply_to_sealed_recipient, to_name),
            };
            render(state, &parent, String::new(), &m.reply_to_envelope).await.0
        };
        messages.push(events::DmHistoryMessagePayload {
            id: m.id,
            sender: m.sender,
            sender_uid: m.sender_uid,
            content,
            timestamp: m.timestamp,
            edited_at: m.edited_at,
            reply_to: m.reply_to,
            reply_to_sender: m.reply_to_sender,
            reply_to_sender_uid: m.reply_to_sender_uid,
            reply_to_content,
            encrypted,
            decrypt_error: err,
        });
    }
    events::emit_dm_history_received(events::DmHistoryReceivedPayload {
        peer: res.peer,
        peer_uid: res.peer_uid,
        messages,
        has_more: res.has_more,
        has_more_after: res.has_more_after,
        around_id: res.around_id,
        after_id: res.after_id,
    });
}

async fn handle_conversations(state: &Arc<Mutex<AppState>>, res: DmConversationsRes) {
    let (me_uid, me_name) = me(state).await;
    let mut conversations = Vec::with_capacity(res.conversations.len());
    for c in res.conversations {
        let (to_uid, to_name) = if c.last_message_sender_uid == me_uid {
            (c.peer_uid, c.peer.as_str())
        } else {
            (me_uid, me_name.as_str())
        };
        let parties = Parties {
            sender_uid: c.last_message_sender_uid,
            recipient_uid: to_uid,
            sender_name: sealed(&c.sealed_sender, &c.last_message_sender),
            recipient_name: sealed(&c.sealed_recipient, to_name),
        };
        let (content, encrypted, err) =
            render(state, &parties, c.last_message_content, &c.last_message_envelope).await;
        conversations.push(events::DmConversationPreviewPayload {
            peer: c.peer,
            peer_uid: c.peer_uid,
            last_message_content: content,
            last_message_sender: c.last_message_sender,
            last_message_sender_uid: c.last_message_sender_uid,
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
    let (me_uid, me_name) = me(state).await;
    let (content, encrypted, err) = if b.envelope.is_empty() {
        (b.content, false, String::new())
    } else {
        // A live edit: the current names are the ones it was sealed under.
        let by_me = b.sender_uid == me_uid;
        let (to_uid, to_name) = if by_me { (b.peer_uid, b.peer.as_str()) } else { (me_uid, me_name.as_str()) };
        let parties = Parties {
            sender_uid: b.sender_uid,
            recipient_uid: to_uid,
            sender_name: &b.sender,
            recipient_name: to_name,
        };
        render(state, &parties, b.content, &b.envelope).await
    };
    events::emit_dm_message_edited(events::DmMessageEditedPayload {
        peer: b.peer,
        peer_uid: b.peer_uid,
        message_id: b.message_id,
        content,
        edited_at: b.edited_at,
        encrypted,
        decrypt_error: err,
    });
}

// ── Router hooks (called from net/central.rs with the packet in hand) ──

pub async fn on_fetch_keys_res(state: &Arc<Mutex<AppState>>, res: E2eeFetchKeysRes) {
    // Settles requests made by uid and by name alike.
    let waiters: Vec<_> = {
        let mut s = state.lock().await;
        let mut w = Vec::new();
        if res.uid > 0 {
            w.extend(s.pending_key_fetches.remove(&(wait_key_uid(res.uid), res.key_id)).unwrap_or_default());
        }
        if !res.username.is_empty() {
            w.extend(s.pending_key_fetches.remove(&(wait_key_name(&res.username), res.key_id)).unwrap_or_default());
        }
        w
    };
    for w in waiters {
        let _ = w.send(res.clone());
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

#[cfg(test)]
mod tests {
    //! Opening through the session (uid design, stage 4): v2 by uid, v1
    //! by the sealed-under names, legacy name-keyed pins, our own echo.
    //! Peer keys are cached, so nothing touches the network.
    use super::*;
    use crate::e2ee::envelope::CONTENT_TEXT;

    const ALICE: i64 = 10;
    const BOB: i64 = 20;

    fn identity(key_id: u32) -> IdentityKeys {
        let mut k = IdentityKeys::generate(1).unwrap();
        k.key_id = key_id;
        k
    }

    fn priv32(k: &IdentityKeys) -> [u8; KEY_LEN] {
        arr32(&k.dh_priv).unwrap()
    }

    fn pin(k: &IdentityKeys) -> PeerRecord {
        PeerRecord {
            sign_pub: k.sign_public().unwrap().to_vec(),
            key_id: k.key_id,
            dh_pub: k.dh_public().unwrap().to_vec(),
            ..Default::default()
        }
    }

    /// A ready session for (uid, name) holding `mine`.
    fn session(uid: i64, name: &str, mine: &IdentityKeys) -> (Arc<Mutex<AppState>>, KeyStore) {
        let mut s = AppState::default();
        s.uid = uid;
        s.username = Some(name.to_string());
        s.e2ee.status = Status::Ready;
        let store = KeyStore::new(mine.key_id, vec![mine.clone()]);
        (Arc::new(Mutex::new(s)), store)
    }

    async fn install(state: &Arc<Mutex<AppState>>, store: KeyStore) {
        state.lock().await.e2ee.store = Some(store);
    }

    /// `state::boot()` for flows that save the key store: the per-process
    /// temp dir keystore.rs's tests install too (a OnceLock: the first call
    /// wins, and both install the same config). Tests that save use their
    /// own uid so the files never collide.
    fn boot_tmp() {
        let dir = std::env::temp_dir().join(format!("decibell-e2ee-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::state::set_boot(crate::state::BootConfig {
            user_data_dir: dir,
            app_version: "test".into(),
            e2ee_local_key: Some([7u8; 32]),
        });
    }

    /// Review S8: no flag from central keeps the store — and the pins in
    /// it — from loading (the `e2ee_keys` gate is gone), and a pinned peer
    /// whose keys central then withholds is refused, never sent plaintext.
    #[tokio::test]
    async fn bootstrap_always_loads_the_pins() {
        boot_tmp();
        const ME: i64 = 9301;
        let (mine, bob) = (identity(3), identity(7));
        let mut store = KeyStore::new(3, vec![mine]);
        store.peers.insert(BOB, pin(&bob));
        keystore::save(ME, &store).unwrap();
        let mut s = AppState::default();
        s.uid = ME;
        s.username = Some("me".into());
        let state = Arc::new(Mutex::new(s));
        bootstrap(state.clone()).await;
        {
            let s = state.lock().await;
            assert_eq!(s.e2ee.status, Status::Ready);
            assert!(s.e2ee.status_payload(ME).supported);
            assert!(s.e2ee.store.as_ref().unwrap().is_pinned(BOB));
        }
        // Central answers "no keys" for the pinned peer (fresh, nothing cached).
        state.lock().await.e2ee.fresh.insert(BOB);
        assert!(seal_outbound(&state, BOB, "bob", "hi").await.is_err());
        keystore::delete(ME);
    }

    /// Review S7: central reporting "no keys" (with an unsigned call) for a
    /// peer we pinned is a refusal, never an unverified-but-allowed call.
    #[tokio::test]
    async fn pinned_peer_without_keys_fails_call_verification() {
        const CAROL: i64 = 30;
        let (a, b) = (identity(3), identity(7));
        let (state, mut store) = session(ALICE, "alice", &a);
        store.peers.insert(BOB, pin(&b));
        install(&state, store).await;
        // Central answered "no keys" for both this session (nothing cached).
        state.lock().await.e2ee.fresh.extend([BOB, CAROL]);
        let pub_key = [1u8; 32];
        let bob = verify_peer_call_key(&state, BOB, "bob", "call-1", &pub_key, &[], 0).await;
        assert!(matches!(bob, CallKeyAuth::Rejected(_)));
        // Someone we never pinned is still an unverified-but-allowed call.
        let carol = verify_peer_call_key(&state, CAROL, "carol", "call-2", &pub_key, &[], 0).await;
        assert!(matches!(carol, CallKeyAuth::Unverified));
        // A signature with no identity behind it stays a refusal either way.
        let signed = verify_peer_call_key(&state, CAROL, "carol", "call-3", &pub_key, &[9u8; 64], 4).await;
        assert!(matches!(signed, CallKeyAuth::Rejected(_)));
    }

    /// What central's E2EE_FETCH_KEYS_RES carries for `b`.
    fn fetched(b: &PublicBundle) -> E2eeFetchKeysRes {
        E2eeFetchKeysRes {
            username: b.username.clone(),
            key_id: b.key_id,
            found: true,
            bundle: Some(E2eeKeyBundle {
                username: b.username.clone(),
                key_id: b.key_id,
                dh_pub: b.dh_pub.clone(),
                sign_pub: b.sign_pub.clone(),
                signature: b.signature.clone(),
                created_at: b.created_at,
                uid: b.uid,
                signed_name: b.signed_name.clone(),
                binding: b.binding,
            }),
            uid: b.uid,
        }
    }

    /// Review S6: central serving an identity of its own as one of a pinned
    /// peer's generations — self-signed, so it verifies — never opens a
    /// message "from" them (DMs or channel key blobs) and is never kept.
    #[tokio::test]
    async fn a_pinned_peers_unpinned_identity_never_opens_their_messages() {
        boot_tmp();
        const ME: i64 = 9311;
        let (me, a) = (identity(7), identity(5));
        let (state, mut store) = session(ME, "me", &me);
        store.peers.insert(ALICE, pin(&a));
        install(&state, store).await;
        {
            // This session already resolved Alice's current keys: the pin.
            let mut s = state.lock().await;
            s.e2ee.fresh.insert(ALICE);
            s.e2ee.current_ids.insert(ALICE, 5);
            s.e2ee.cache.insert((ALICE, 5), a.public_bundle(ALICE, "alice").unwrap());
        }
        let my_pub = me.dh_public().unwrap();
        let from_alice = Parties { sender_uid: ALICE, recipient_uid: ME, sender_name: "alice", recipient_name: "me" };
        // An "old" generation 2, then a "newer" 9 (that one costs a re-fetch
        // of her current keys, which fails here: no central — still refused).
        for kid in [2u32, 9] {
            let fake = identity(kid);
            let (uid, served) = accept_fetched(&state, ALICE, kid, fetched(&fake.public_bundle(ALICE, "alice").unwrap())).await;
            assert_eq!(uid, ALICE);
            assert!(served.is_some(), "self-signed, so it verifies on its own");
            let wire = envelope::seal(&priv32(&fake), ALICE, kid, &my_pub, ME, 7, "send me your passphrase").unwrap();
            assert_eq!(open_inbound(&state, &from_alice, &wire).await, Err(DecryptError::PeerKey));
            let blob = envelope::seal_bytes(&priv32(&fake), ALICE, kid, &my_pub, ME, 7, &[2, 1, 2, 3]).unwrap();
            assert_eq!(open_inbound_raw(&state, &from_alice, &blob).await, Err(DecryptError::PeerKey));
            let s = state.lock().await;
            let st = s.e2ee.store.as_ref().unwrap();
            assert!(st.peer_dh_pub(ALICE, kid).is_none(), "never persisted");
            assert_eq!(st.peers[&ALICE].sign_pub, a.sign_public().unwrap().to_vec(), "the pin is untouched");
        }
        // Her real messages still open.
        let wire = envelope::seal(&priv32(&a), ALICE, 5, &my_pub, ME, 7, "hi").unwrap();
        assert_eq!(open_inbound(&state, &from_alice, &wire).await.unwrap(), "hi");
        keystore::delete(ME);
    }

    /// Review S6, the honest case: a peer's reset met first in a message is
    /// their new current identity — surfaced and re-pinned as a send would,
    /// then it opens — and the retired identity's generations still open.
    #[tokio::test]
    async fn a_key_change_first_seen_in_history_is_surfaced_and_opens() {
        boot_tmp();
        const ME: i64 = 9312;
        let (me, a, b) = (identity(7), identity(3), identity(5));
        let (state, mut store) = session(ME, "me", &me);
        store.peers.insert(ALICE, pin(&a));
        install(&state, store).await;
        {
            // Alice reset to B (not resolved yet this session); central still
            // serves A's generation 3.
            let mut s = state.lock().await;
            s.e2ee.current_ids.insert(ALICE, 5);
            s.e2ee.cache.insert((ALICE, 5), b.public_bundle(ALICE, "alice").unwrap());
            s.e2ee.cache.insert((ALICE, 3), a.public_bundle(ALICE, "alice").unwrap());
        }
        let my_pub = me.dh_public().unwrap();
        let from_alice = Parties { sender_uid: ALICE, recipient_uid: ME, sender_name: "alice", recipient_name: "me" };
        let wire = envelope::seal(&priv32(&b), ALICE, 5, &my_pub, ME, 7, "new keys").unwrap();
        assert_eq!(open_inbound(&state, &from_alice, &wire).await.unwrap(), "new keys");
        {
            let s = state.lock().await;
            let rec = &s.e2ee.store.as_ref().unwrap().peers[&ALICE];
            assert_eq!(rec.sign_pub, b.sign_public().unwrap().to_vec(), "re-pinned");
            assert!(rec.changed_at > 0, "surfaced as a key change");
            assert!(rec.knows_identity(&a.sign_public().unwrap()));
        }
        let old = envelope::seal(&priv32(&a), ALICE, 3, &my_pub, ME, 7, "old keys").unwrap();
        assert_eq!(open_inbound(&state, &from_alice, &old).await.unwrap(), "old keys");
        // Another generation signed by the retired identity passes too.
        let mut a2 = identity(2);
        a2.sign_priv = a.sign_priv.clone();
        state.lock().await.e2ee.cache.insert((ALICE, 2), a2.public_bundle(ALICE, "alice").unwrap());
        assert!(fetch_bundle(&state, ALICE, 2).await.unwrap().is_some());
        let older = envelope::seal(&priv32(&a2), ALICE, 2, &my_pub, ME, 7, "older").unwrap();
        assert_eq!(open_inbound(&state, &from_alice, &older).await.unwrap(), "older");
        keystore::delete(ME);
    }

    #[tokio::test]
    async fn opens_v2_by_uid_whatever_the_names() {
        let (a, b) = (identity(3), identity(7));
        let (state, mut store) = session(BOB, "bob", &b);
        store.peers.insert(ALICE, pin(&a));
        install(&state, store).await;
        let wire = envelope::seal(&priv32(&a), ALICE, 3, &b.dh_public().unwrap(), BOB, 7, "hi bob").unwrap();
        // Both renamed since: a v2 envelope doesn't care.
        let p = Parties { sender_uid: ALICE, recipient_uid: BOB, sender_name: "alice2", recipient_name: "robert" };
        assert_eq!(open_inbound(&state, &p, &wire).await.unwrap(), "hi bob");
        // Server-stamped uids that don't match fail authentication.
        let wrong = Parties { sender_uid: 11, ..p };
        assert!(open_inbound(&state, &wrong, &wire).await.is_err());
    }

    #[tokio::test]
    async fn opens_v1_history_with_sealed_names_and_a_legacy_pin() {
        let (a, b) = (identity(3), identity(7));
        let (state, mut store) = session(BOB, "bob", &b);
        // The pin is still name-keyed (a pre-uid store, not yet adopted).
        store.legacy_peers.insert("alice".into(), pin(&a));
        install(&state, store).await;
        let mut plain = vec![CONTENT_TEXT];
        plain.extend_from_slice(b"old times");
        let wire = envelope::seal_v1(&priv32(&a), "alice", 3, &b.dh_public().unwrap(), "bob", 7, &plain).unwrap();
        let p = Parties { sender_uid: ALICE, recipient_uid: BOB, sender_name: "alice", recipient_name: "bob" };
        assert_eq!(open_inbound(&state, &p, &wire).await.unwrap(), "old times");
        assert_eq!(render(&state, &p, String::new(), &wire).await, ("old times".to_string(), true, String::new()));
    }

    #[tokio::test]
    async fn opens_our_own_echo_and_reports_missing_keys() {
        let (a, b) = (identity(3), identity(7));
        let (state, mut store) = session(ALICE, "alice", &a);
        store.peers.insert(BOB, pin(&b));
        install(&state, store).await;
        let wire = envelope::seal(&priv32(&a), ALICE, 3, &b.dh_public().unwrap(), BOB, 7, "sent by me").unwrap();
        let p = Parties { sender_uid: ALICE, recipient_uid: BOB, sender_name: "alice", recipient_name: "bob" };
        assert_eq!(open_inbound(&state, &p, &wire).await.unwrap(), "sent by me");
        // Sealed to a generation of our key we don't hold (a reset).
        let wire = envelope::seal(&priv32(&b), BOB, 7, &a.dh_public().unwrap(), ALICE, 4, "to my next key").unwrap();
        let p = Parties { sender_uid: BOB, recipient_uid: ALICE, sender_name: "bob", recipient_name: "alice" };
        assert_eq!(open_inbound(&state, &p, &wire).await, Err(DecryptError::NoKey));
        // Locked device.
        state.lock().await.e2ee.status = Status::Locked;
        assert_eq!(open_inbound(&state, &p, &wire).await, Err(DecryptError::Locked));
    }
}

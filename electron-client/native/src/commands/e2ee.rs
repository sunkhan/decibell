//! E2EE DM commands — the renderer's whole surface for encryption.
//! Design: docs/superpowers/specs/2026-09-03-e2ee-dms-design.md.
//!
//!   - `e2ee_get_status()`: what this account looks like on this device
//!     (also pushed as the `e2ee_status_changed` event).
//!   - `e2ee_setup({passphrase})`: first-time key generation + publish.
//!   - `e2ee_unlock({passphrase})`: restore from the central backup.
//!   - `e2ee_change_passphrase({passphrase})`: re-wrap the backup.
//!   - `e2ee_reset({passphrase})`: new identity (forgot passphrase / rotate).
//!   - `e2ee_peer_info({username})`: does the peer have keys, their
//!     fingerprint and the conversation's safety number.
//!
//! Sends and receives need nothing from the renderer: `send_private_message`
//! / `edit_dm_message` seal, the central router's DM worker opens.
//!
//! `js_name` is explicit on every command: napi's camel-casing splits on
//! the digit (`e2ee_get_status` → `e2EeGetStatus`), which the renderer's
//! `invoke()` normaliser (`_x` → `X`) would never produce.

use crate::e2ee::identity::{fingerprint, safety_number};
use crate::e2ee::session::{self, Status};
use crate::state;

#[napi(object)]
pub struct E2eeStatus {
    pub supported: bool,
    /// "unavailable" | "not_set_up" | "locked" | "ready"
    pub status: String,
    pub key_id: u32,
    pub fingerprint: String,
}

#[napi(js_name = "e2eeGetStatus")]
pub async fn e2ee_get_status() -> napi::Result<E2eeStatus> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    let username = s.username.clone().unwrap_or_default();
    let p = s.e2ee.status_payload(&username);
    Ok(E2eeStatus {
        supported: p.supported,
        status: p.status,
        key_id: p.key_id,
        fingerprint: p.fingerprint,
    })
}

#[napi(object)]
pub struct PassphraseArgs {
    pub passphrase: String,
}

#[napi(js_name = "e2eeSetup")]
pub async fn e2ee_setup(args: PassphraseArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    session::setup(&state_arc, args.passphrase)
        .await
        .map_err(napi::Error::from_reason)
}

#[napi(js_name = "e2eeUnlock")]
pub async fn e2ee_unlock(args: PassphraseArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    session::unlock(&state_arc, args.passphrase)
        .await
        .map_err(napi::Error::from_reason)
}

#[napi(js_name = "e2eeChangePassphrase")]
pub async fn e2ee_change_passphrase(args: PassphraseArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    session::change_passphrase(&state_arc, args.passphrase)
        .await
        .map_err(napi::Error::from_reason)
}

#[napi(js_name = "e2eeReset")]
pub async fn e2ee_reset(args: PassphraseArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    session::reset(&state_arc, args.passphrase)
        .await
        .map_err(napi::Error::from_reason)
}

#[napi(object)]
pub struct PeerInfoArgs {
    pub username: String,
}

#[napi(object)]
pub struct E2eePeerInfo {
    /// The peer has published keys (so messages to them are sealed).
    pub has_keys: bool,
    pub key_id: u32,
    pub fingerprint: String,
    /// Both sides' identities hashed together — identical on both
    /// screens; "" unless we're ready and they have keys.
    pub safety_number: String,
    /// Unix seconds of the last identity change we saw; 0 = never.
    pub changed_at: i64,
    /// We've talked to this identity before (TOFU pin present).
    pub pinned: bool,
}

#[napi(js_name = "e2eePeerInfo")]
pub async fn e2ee_peer_info(args: PeerInfoArgs) -> napi::Result<E2eePeerInfo> {
    let state_arc = state::shared();
    let ready = state_arc.lock().await.e2ee.status == Status::Ready;
    let bundle = if ready {
        session::resolve_peer_current(&state_arc, &args.username)
            .await
            .map_err(napi::Error::from_reason)?
    } else {
        // Not ready → the pin (if any) is what we know; central lookups
        // still work but there's no local identity to pair them with.
        session::fetch_bundle(&state_arc, &args.username, 0)
            .await
            .map_err(napi::Error::from_reason)?
    };
    let s = state_arc.lock().await;
    let me = s.username.clone().unwrap_or_default();
    let rec = s.e2ee.store.as_ref().and_then(|st| st.peers.get(&args.username));
    let (pinned, changed_at) = rec
        .map(|r| (!r.sign_pub.is_empty(), r.changed_at))
        .unwrap_or((false, 0));
    let Some(b) = bundle else {
        return Ok(E2eePeerInfo {
            has_keys: false,
            key_id: 0,
            fingerprint: String::new(),
            safety_number: String::new(),
            changed_at,
            pinned,
        });
    };
    let mine = if s.e2ee.status == Status::Ready {
        s.e2ee.store.as_ref().and_then(|st| st.current()).and_then(|k| k.sign_public().ok())
    } else {
        None
    };
    let safety = mine
        .map(|my_sign| safety_number(&me, &my_sign, &args.username, &b.sign_pub))
        .unwrap_or_default();
    Ok(E2eePeerInfo {
        has_keys: true,
        key_id: b.key_id,
        fingerprint: fingerprint(&args.username, &b.sign_pub),
        safety_number: safety,
        changed_at,
        pinned,
    })
}

//! On-disk key store: `<userData>/e2ee/u<uid>.json`.
//!
//! Holds every generation of our own identity (old envelopes name the
//! key_id they were sealed under, so old private keys stay), the pinned
//! identity of every peer we've talked to (by uid) with the identities
//! pinned before it, and each peer's historical public keys by key_id. Encrypted at rest with AES-256-GCM
//! under the at-rest key Electron main hands us (a `safeStorage`-wrapped
//! random key) or, failing that, the same hostname+user derivation
//! `config.rs` uses for the stored login credentials; the AAD binds the
//! account uid. The store is a cache of what the passphrase backup holds
//! (plus pins): if it can't be opened, the account shows as *locked* and
//! the passphrase rebuilds it.
//!
//! Before the uid binding (uid design, stage 4) the file was
//! `<hex(username)>.json`, bound to the name, with pins keyed by name. The
//! first load migrates it (once the keys prove to be this account's): pins
//! move to `legacy_peers` and are adopted by uid when a fetched bundle
//! proves the account by its identity key, never by its name (names change
//! hands). Until then a legacy pin only opens v1 history sealed under that
//! name (both from before renames existed).

use std::collections::HashMap;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::identity::{b64, random_bytes, IdentityKeys};

pub const STORE_VERSION: u32 = 2;
/// Outer file version of a name-bound store (`<hex(username)>.json`).
const FILE_VERSION_NAME: u32 = 1;
/// Outer file version of a uid-bound store (`u<uid>.json`).
const FILE_VERSION_UID: u32 = 2;
const UID_AAD_DOMAIN: &[u8] = b"decibell-keystore-uid";

/// What we know about a peer's identity.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeerRecord {
    /// The pinned identity (Ed25519 public key). A fetched bundle with a
    /// different one is a *key change* — surfaced, then re-pinned.
    #[serde(with = "b64")]
    pub sign_pub: Vec<u8>,
    /// Their current generation as last fetched.
    pub key_id: u32,
    #[serde(with = "b64")]
    pub dh_pub: Vec<u8>,
    pub first_seen: i64,
    /// Unix seconds of the last identity change; 0 = never.
    pub changed_at: i64,
    /// key_id → dh_pub for every generation seen while opening history,
    /// so old envelopes don't cost a fetch each session. Once the peer is
    /// pinned, only generations of a pinned identity (`sign_pub` or
    /// `previous`) are written here.
    #[serde(default)]
    pub history: HashMap<u32, String>,
    /// Identities (Ed25519 public keys, base64) pinned before `sign_pub`,
    /// oldest first: a key change moves the old pin here so history sealed
    /// under its generations still opens — and an identity central serves
    /// under an old key_id that was never pinned doesn't (review S6).
    #[serde(default)]
    pub previous: Vec<String>,
}

/// Enough for any honest history; past it the oldest retired identity goes
/// (its generations already in `history` still open).
const MAX_PREVIOUS_IDENTITIES: usize = 64;

impl PeerRecord {
    /// True when `sign_pub` is the pinned identity or one pinned before it.
    pub fn knows_identity(&self, sign_pub: &[u8]) -> bool {
        use base64::Engine as _;
        if !self.sign_pub.is_empty() && self.sign_pub == sign_pub {
            return true;
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(sign_pub);
        self.previous.iter().any(|p| *p == b64)
    }

    /// A key change: `sign_pub` becomes the pin and the old identity moves
    /// to `previous`.
    pub fn repin(&mut self, sign_pub: &[u8], now: i64) {
        use base64::Engine as _;
        if !self.sign_pub.is_empty() {
            let old = base64::engine::general_purpose::STANDARD.encode(&self.sign_pub);
            if !self.previous.contains(&old) {
                self.previous.push(old);
            }
            if self.previous.len() > MAX_PREVIOUS_IDENTITIES {
                let excess = self.previous.len() - MAX_PREVIOUS_IDENTITIES;
                self.previous.drain(..excess);
            }
        }
        self.sign_pub = sign_pub.to_vec();
        self.changed_at = now;
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KeyStore {
    pub version: u32,
    pub current_key_id: u32,
    pub keys: Vec<IdentityKeys>,
    /// Pins by account uid.
    #[serde(default)]
    pub peers: HashMap<i64, PeerRecord>,
    /// Pins from the name-keyed store, waiting to be adopted by uid. Only
    /// `adopt_legacy` and `legacy_dh_pub` (v1 history) read them.
    #[serde(default)]
    pub legacy_peers: HashMap<String, PeerRecord>,
}

/// The pre-uid store layout (pins keyed by name).
#[derive(Deserialize)]
struct KeyStoreV1 {
    current_key_id: u32,
    keys: Vec<IdentityKeys>,
    #[serde(default)]
    peers: HashMap<String, PeerRecord>,
}

impl KeyStore {
    pub fn new(current_key_id: u32, keys: Vec<IdentityKeys>) -> KeyStore {
        KeyStore {
            version: STORE_VERSION,
            current_key_id,
            keys,
            peers: HashMap::new(),
            legacy_peers: HashMap::new(),
        }
    }

    pub fn current(&self) -> Option<&IdentityKeys> {
        self.keys.iter().find(|k| k.key_id == self.current_key_id)
    }

    pub fn key(&self, key_id: u32) -> Option<&IdentityKeys> {
        self.keys.iter().find(|k| k.key_id == key_id)
    }

    /// The pin for `uid`.
    pub fn pin(&self, uid: i64) -> Option<&PeerRecord> {
        self.peers.get(&uid)
    }

    /// True when we've pinned an identity for this peer.
    pub fn is_pinned(&self, uid: i64) -> bool {
        self.pin(uid).map(|p| !p.sign_pub.is_empty()).unwrap_or(false)
    }

    /// May a verified bundle of `uid` carrying this identity open their
    /// messages without a key change being surfaced first? Yes before we've
    /// pinned them (first sight is trusted: TOFU), else only for the pinned
    /// identity or one pinned before it.
    pub fn identity_known(&self, uid: i64, sign_pub: &[u8]) -> bool {
        match self.pin(uid) {
            Some(rec) if !rec.sign_pub.is_empty() => rec.knows_identity(sign_pub),
            _ => true,
        }
    }

    /// Move a name-keyed pin to `uid` once a verified bundle of that
    /// account proves it: the pinned identity key is this bundle's (any
    /// generation), or the bundle is name-bound (binding 1) to the pin's
    /// name (signed by this account while it held the name; keys changed
    /// since). Never by the current name alone: names change hands. A uid
    /// already pinned keeps its pin. True if moved.
    pub fn adopt_legacy(&mut self, uid: i64, sign_pub: &[u8], binding: u32, signed_name: &str) -> bool {
        if uid <= 0 || self.legacy_peers.is_empty() || self.is_pinned(uid) {
            return false;
        }
        let name = self
            .legacy_peers
            .iter()
            .find(|(_, rec)| !rec.sign_pub.is_empty() && rec.sign_pub == sign_pub)
            .map(|(name, _)| name.clone())
            .or_else(|| {
                (binding == 1 && !signed_name.is_empty() && self.legacy_peers.contains_key(signed_name))
                    .then(|| signed_name.to_string())
            });
        let Some(name) = name else { return false };
        let Some(mut rec) = self.legacy_peers.remove(&name) else { return false };
        // Keep the generations already cached under the uid.
        if let Some(cur) = self.peers.remove(&uid) {
            for (kid, pubkey) in cur.history {
                rec.history.entry(kid).or_insert(pubkey);
            }
        }
        self.peers.insert(uid, rec);
        true
    }

    /// A peer's public agreement key for one generation, if cached.
    pub fn peer_dh_pub(&self, uid: i64, key_id: u32) -> Option<Vec<u8>> {
        record_dh_pub(self.pin(uid)?, key_id)
    }

    /// The key a not-yet-adopted name-keyed pin holds for `name`: only for
    /// opening a v1 envelope sealed under that name (v1 history and the
    /// pin both come from before names could change hands).
    pub fn legacy_dh_pub(&self, name: &str, key_id: u32) -> Option<Vec<u8>> {
        record_dh_pub(self.legacy_peers.get(name)?, key_id)
    }

    pub fn remember_peer_key(&mut self, uid: i64, key_id: u32, dh_pub: &[u8]) {
        use base64::Engine as _;
        let rec = self.peers.entry(uid).or_default();
        rec.history
            .insert(key_id, base64::engine::general_purpose::STANDARD.encode(dh_pub));
    }
}

fn record_dh_pub(p: &PeerRecord, key_id: u32) -> Option<Vec<u8>> {
    use base64::Engine as _;
    if p.key_id == key_id && !p.dh_pub.is_empty() {
        return Some(p.dh_pub.clone());
    }
    let b = p.history.get(&key_id)?;
    base64::engine::general_purpose::STANDARD.decode(b).ok()
}

/// On-disk envelope around the encrypted store.
#[derive(Serialize, Deserialize)]
struct StoreFile {
    v: u32,
    /// "safe_storage" | "derived" — which at-rest key wrote it (for
    /// diagnostics; the AEAD decides whether it opens).
    protector: String,
    nonce: String,
    ct: String,
}

fn dir() -> PathBuf {
    crate::state::boot().user_data_dir.join("e2ee")
}

pub fn path_for(uid: i64) -> PathBuf {
    dir().join(format!("u{uid}.json"))
}

/// The pre-uid file. Usernames are ≤32 chars but not guaranteed
/// filesystem-safe; hex is.
fn legacy_path_for(username: &str) -> PathBuf {
    let hex: String = username.bytes().map(|b| format!("{b:02x}")).collect();
    dir().join(format!("{hex}.json"))
}

fn uid_aad(uid: i64) -> Vec<u8> {
    let mut a = UID_AAD_DOMAIN.to_vec();
    a.push(0);
    a.extend_from_slice(&uid.to_le_bytes());
    a
}

fn at_rest_key() -> ([u8; 32], &'static str) {
    match crate::state::boot().e2ee_local_key {
        Some(k) => (k, "safe_storage"),
        None => (crate::config::derive_key(), "derived"),
    }
}

/// A store exists for this account (either file).
pub fn exists(uid: i64, username: &str) -> bool {
    path_for(uid).is_file() || (!username.is_empty() && legacy_path_for(username).is_file())
}

/// Decrypt one store file. `Ok(None)` = no file.
fn open_file(path: &PathBuf, version: u32, aad: &[u8]) -> Result<Option<Vec<u8>>, String> {
    use base64::Engine as _;
    let raw = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let file: StoreFile = serde_json::from_slice(&raw).map_err(|e| format!("parse store: {e}"))?;
    if file.v != version {
        return Err(format!("unknown store file version {}", file.v));
    }
    let std_b64 = base64::engine::general_purpose::STANDARD;
    let nonce = std_b64.decode(&file.nonce).map_err(|e| e.to_string())?;
    let ct = std_b64.decode(&file.ct).map_err(|e| e.to_string())?;
    if nonce.len() != 12 {
        return Err("bad store nonce".into());
    }
    let (mut key, _) = at_rest_key();
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad })
        .map_err(|_| "key store does not open with this machine's at-rest key".to_string());
    key.zeroize();
    plain.map(Some)
}

/// `Ok(None)` = no store for this account. `Err` = present but unreadable
/// (different at-rest key, corruption) — the caller treats that as locked.
pub fn load(uid: i64) -> Result<Option<KeyStore>, String> {
    let Some(mut plain) = open_file(&path_for(uid), FILE_VERSION_UID, &uid_aad(uid))? else {
        return Ok(None);
    };
    let store = serde_json::from_slice::<KeyStore>(&plain).map_err(|e| format!("decode store: {e}"));
    plain.zeroize();
    store.map(Some)
}

/// A pre-uid store filed under `username`, unsaved: the caller checks its
/// keys are this account's (a reused name on a shared OS profile could be
/// someone else's) before `adopt_legacy_file` moves it to the uid file.
pub fn load_legacy(username: &str) -> Result<Option<KeyStore>, String> {
    if username.is_empty() {
        return Ok(None);
    }
    let Some(mut plain) = open_file(&legacy_path_for(username), FILE_VERSION_NAME, username.as_bytes())? else {
        return Ok(None);
    };
    let old = serde_json::from_slice::<KeyStoreV1>(&plain).map_err(|e| format!("decode store: {e}"));
    plain.zeroize();
    let old = old?;
    let mut store = KeyStore::new(old.current_key_id, old.keys);
    store.legacy_peers = old.peers;
    Ok(Some(store))
}

/// Save a verified pre-uid store as `uid`'s and remove the name-keyed file.
pub fn adopt_legacy_file(uid: i64, username: &str, store: &KeyStore) -> Result<(), String> {
    save(uid, store)?;
    let _ = std::fs::remove_file(legacy_path_for(username));
    log::info!("[e2ee] key store migrated to the uid file ({} pins to adopt)", store.legacy_peers.len());
    Ok(())
}

pub fn save(uid: i64, store: &KeyStore) -> Result<(), String> {
    use base64::Engine as _;
    if uid <= 0 {
        return Err("no account uid for the key store".into());
    }
    let path = path_for(uid);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let nonce: [u8; 12] = random_bytes()?;
    let (mut key, protector) = at_rest_key();
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let mut plain = serde_json::to_vec(store).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &uid_aad(uid) })
        .map_err(|_| "AEAD seal failed".to_string());
    key.zeroize();
    plain.zeroize();
    let ct = ct?;
    let std_b64 = base64::engine::general_purpose::STANDARD;
    let file = StoreFile {
        v: FILE_VERSION_UID,
        protector: protector.to_string(),
        nonce: std_b64.encode(nonce),
        ct: std_b64.encode(ct),
    };
    let json = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
    // Write-then-rename so a crash mid-write never leaves a torn store.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))
}

#[cfg(test)]
pub fn delete(uid: i64) {
    let _ = std::fs::remove_file(path_for(uid));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot_tmp() -> tempdir::TempDirGuard {
        tempdir::install()
    }

    /// Minimal boot config for tests — `state::boot()` is a OnceLock, so
    /// every test in the process shares the first directory installed.
    mod tempdir {
        use std::path::PathBuf;
        pub struct TempDirGuard;
        pub fn install() -> TempDirGuard {
            let dir: PathBuf = std::env::temp_dir().join(format!(
                "decibell-e2ee-test-{}",
                std::process::id()
            ));
            let _ = std::fs::create_dir_all(&dir);
            crate::state::set_boot(crate::state::BootConfig {
                user_data_dir: dir,
                app_version: "test".into(),
                e2ee_local_key: Some([7u8; 32]),
            });
            TempDirGuard
        }
    }

    /// Write a pre-uid store the way the old client did.
    fn save_legacy(username: &str, json: &[u8]) {
        use base64::Engine as _;
        let path = legacy_path_for(username);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let nonce = [3u8; 12];
        let cipher = Aes256Gcm::new_from_slice(&[7u8; 32]).unwrap();
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: json, aad: username.as_bytes() }).unwrap();
        let b = base64::engine::general_purpose::STANDARD;
        let file = StoreFile { v: FILE_VERSION_NAME, protector: "safe_storage".into(), nonce: b.encode(nonce), ct: b.encode(ct) };
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    }

    #[test]
    fn save_load_roundtrip_and_peer_cache() {
        let _g = boot_tmp();
        let uid = 9001;
        let mut k = IdentityKeys::generate(1).unwrap();
        k.key_id = 4;
        let mut store = KeyStore::new(4, vec![k]);
        store.remember_peer_key(20, 2, &[9u8; 32]);
        store.peers.get_mut(&20).unwrap().sign_pub = vec![1u8; 32];
        save(uid, &store).unwrap();
        assert!(exists(uid, ""));
        let back = load(uid).unwrap().unwrap();
        assert_eq!(back.current_key_id, 4);
        assert_eq!(back.current().unwrap().key_id, 4);
        assert_eq!(back.peer_dh_pub(20, 2).unwrap(), vec![9u8; 32]);
        assert!(back.peer_dh_pub(20, 3).is_none());
        assert_eq!(back.peers[&20].sign_pub, vec![1u8; 32]);
        assert!(back.is_pinned(20) && !back.is_pinned(21));
        delete(uid);
        assert!(load(uid).unwrap().is_none());
        assert!(save(0, &store).is_err());
    }

    #[test]
    fn store_is_bound_to_the_uid() {
        let _g = boot_tmp();
        let store = KeyStore::new(1, vec![IdentityKeys::generate(1).unwrap()]);
        save(9101, &store).unwrap();
        // Copy one account's file over another's: the AAD refuses it.
        std::fs::copy(path_for(9101), path_for(9102)).unwrap();
        assert!(load(9102).is_err());
        delete(9101);
        delete(9102);
    }

    #[test]
    fn pre_uid_store_is_migrated_and_its_pins_adopted() {
        let _g = boot_tmp();
        let mut k = IdentityKeys::generate(1).unwrap();
        k.key_id = 2;
        let old = serde_json::json!({
            "version": 1,
            "current_key_id": 2,
            "keys": [k],
            "peers": { "bob": { "sign_pub": "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=", "key_id": 5,
                                "dh_pub": "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=", "first_seen": 1,
                                "changed_at": 0, "history": {} } }
        });
        save_legacy("migrant", &serde_json::to_vec(&old).unwrap());
        assert!(exists(9201, "migrant"));
        assert!(load(9201).unwrap().is_none(), "a uid load never reads a name-keyed file");
        let mut store = load_legacy("migrant").unwrap().unwrap();
        assert_eq!(store.current().unwrap().dh_public().unwrap(), k.dh_public().unwrap());
        assert!(legacy_path_for("migrant").exists(), "nothing moves before the caller checks the keys");
        adopt_legacy_file(9201, "migrant", &store).unwrap();
        assert!(!legacy_path_for("migrant").exists(), "the name-keyed file is gone");
        assert!(path_for(9201).is_file());
        // A legacy pin counts for nothing until adopted, and only v1 history
        // opens with it by name.
        assert!(store.peers.is_empty() && !store.is_pinned(77));
        assert_eq!(store.legacy_dh_pub("bob", 5).unwrap(), vec![2u8; 32]);
        // The current name alone adopts nothing (names change hands)...
        assert!(!store.adopt_legacy(77, &[9u8; 32], 2, "bob"));
        // ...the pinned identity key does, once.
        assert!(store.adopt_legacy(77, &[1u8; 32], 2, ""));
        assert!(!store.adopt_legacy(78, &[1u8; 32], 2, ""));
        assert!(store.is_pinned(77) && !store.is_pinned(78));
        assert_eq!(store.peer_dh_pub(77, 5).unwrap(), vec![2u8; 32]);
        // A second load reads the uid file.
        let again = load(9201).unwrap().unwrap();
        assert_eq!(again.legacy_peers.len(), 1);
        delete(9201);
    }

    /// Review S6: after a key change the old identity still counts (its
    /// generations open history), an identity never pinned doesn't, and
    /// before the first pin anything is first sight.
    #[test]
    fn a_key_change_keeps_the_old_identity_known() {
        let _g = boot_tmp();
        let (a, b, c) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        let mut store = KeyStore::new(1, vec![IdentityKeys::generate(1).unwrap()]);
        assert!(store.identity_known(7, &a), "nothing pinned: first sight");
        store.remember_peer_key(7, 2, &[9u8; 32]);
        assert!(store.identity_known(7, &c), "history alone is no pin");
        store.peers.get_mut(&7).unwrap().sign_pub = a.to_vec();
        assert!(store.identity_known(7, &a) && !store.identity_known(7, &b));
        store.peers.get_mut(&7).unwrap().repin(&b, 100);
        assert_eq!(store.peers[&7].sign_pub, b.to_vec());
        assert_eq!(store.peers[&7].changed_at, 100);
        assert!(store.identity_known(7, &a) && store.identity_known(7, &b));
        assert!(!store.identity_known(7, &c));
        // Back and forth doesn't pile up duplicates.
        store.peers.get_mut(&7).unwrap().repin(&a, 101);
        store.peers.get_mut(&7).unwrap().repin(&b, 102);
        assert_eq!(store.peers[&7].previous.len(), 2);
        // The retired identities survive a save / load.
        save(9401, &store).unwrap();
        let back = load(9401).unwrap().unwrap();
        assert!(back.identity_known(7, &a) && !back.identity_known(7, &c));
        delete(9401);
        // Bounded: the oldest retired identity goes first.
        let rec = store.peers.get_mut(&7).unwrap();
        for i in 0..(MAX_PREVIOUS_IDENTITIES as u8 + 5) {
            rec.repin(&[100u8.wrapping_add(i); 32], 200);
        }
        assert_eq!(rec.previous.len(), MAX_PREVIOUS_IDENTITIES);
        assert!(!rec.knows_identity(&a));
    }

    #[test]
    fn a_name_bound_bundle_adopts_its_names_pin() {
        let mut store = KeyStore::new(1, vec![IdentityKeys::generate(1).unwrap()]);
        let rec = PeerRecord { sign_pub: vec![1u8; 32], key_id: 5, dh_pub: vec![2u8; 32], first_seen: 1, ..Default::default() };
        store.legacy_peers.insert("carol".into(), rec);
        // Keys generations already cached under the uid are kept.
        store.remember_peer_key(42, 6, &[3u8; 32]);
        // A binding-1 bundle signed for "carol" by uid 42 (new keys since).
        assert!(store.adopt_legacy(42, &[7u8; 32], 1, "carol"));
        assert!(store.is_pinned(42) && store.legacy_peers.is_empty());
        assert_eq!(store.peers[&42].sign_pub, vec![1u8; 32], "the old pin stays, so the change is surfaced");
        assert_eq!(store.peer_dh_pub(42, 6).unwrap(), vec![3u8; 32]);
    }
}

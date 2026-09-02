//! On-disk key store: `<userData>/e2ee/<username>.json`.
//!
//! Holds every generation of our own identity (old envelopes name the
//! key_id they were sealed under, so old private keys stay), the pinned
//! identity of every peer we've talked to, and each peer's historical
//! public keys by key_id. Encrypted at rest with AES-256-GCM under the
//! at-rest key Electron main hands us (a `safeStorage`-wrapped random
//! key) or, failing that, the same hostname+user derivation `config.rs`
//! uses for the stored login credentials. The store is a cache of what
//! the passphrase backup holds (plus pins): if it can't be opened, the
//! account shows as *locked* and the passphrase rebuilds it.

use std::collections::HashMap;
use std::path::PathBuf;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::identity::{b64, random_bytes, IdentityKeys};

pub const STORE_VERSION: u32 = 1;
const FILE_VERSION: u32 = 1;

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
    /// so old envelopes don't cost a fetch each session.
    #[serde(default)]
    pub history: HashMap<u32, String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct KeyStore {
    pub version: u32,
    pub current_key_id: u32,
    pub keys: Vec<IdentityKeys>,
    #[serde(default)]
    pub peers: HashMap<String, PeerRecord>,
}

impl KeyStore {
    pub fn new(current_key_id: u32, keys: Vec<IdentityKeys>) -> KeyStore {
        KeyStore { version: STORE_VERSION, current_key_id, keys, peers: HashMap::new() }
    }

    pub fn current(&self) -> Option<&IdentityKeys> {
        self.keys.iter().find(|k| k.key_id == self.current_key_id)
    }

    pub fn key(&self, key_id: u32) -> Option<&IdentityKeys> {
        self.keys.iter().find(|k| k.key_id == key_id)
    }

    /// A peer's public agreement key for one generation, if cached.
    pub fn peer_dh_pub(&self, username: &str, key_id: u32) -> Option<Vec<u8>> {
        use base64::Engine as _;
        let p = self.peers.get(username)?;
        if p.key_id == key_id && !p.dh_pub.is_empty() {
            return Some(p.dh_pub.clone());
        }
        let b = p.history.get(&key_id)?;
        base64::engine::general_purpose::STANDARD.decode(b).ok()
    }

    pub fn remember_peer_key(&mut self, username: &str, key_id: u32, dh_pub: &[u8]) {
        use base64::Engine as _;
        let rec = self.peers.entry(username.to_string()).or_default();
        rec.history
            .insert(key_id, base64::engine::general_purpose::STANDARD.encode(dh_pub));
    }
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

/// Usernames are ≤32 chars but not guaranteed filesystem-safe; hex is.
pub fn path_for(username: &str) -> PathBuf {
    let hex: String = username.bytes().map(|b| format!("{b:02x}")).collect();
    dir().join(format!("{hex}.json"))
}

fn at_rest_key() -> ([u8; 32], &'static str) {
    match crate::state::boot().e2ee_local_key {
        Some(k) => (k, "safe_storage"),
        None => (crate::config::derive_key(), "derived"),
    }
}

pub fn exists(username: &str) -> bool {
    path_for(username).is_file()
}

/// `Ok(None)` = no store for this user. `Err` = present but unreadable
/// (different at-rest key, corruption) — the caller treats that as locked.
pub fn load(username: &str) -> Result<Option<KeyStore>, String> {
    use base64::Engine as _;
    let path = path_for(username);
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let file: StoreFile = serde_json::from_slice(&raw).map_err(|e| format!("parse store: {e}"))?;
    if file.v != FILE_VERSION {
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
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad: username.as_bytes() })
        .map_err(|_| "key store does not open with this machine's at-rest key".to_string());
    key.zeroize();
    let mut plain = plain?;
    let store = serde_json::from_slice::<KeyStore>(&plain).map_err(|e| format!("decode store: {e}"));
    plain.zeroize();
    store.map(Some)
}

pub fn save(username: &str, store: &KeyStore) -> Result<(), String> {
    use base64::Engine as _;
    let path = path_for(username);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    let nonce: [u8; 12] = random_bytes()?;
    let (mut key, protector) = at_rest_key();
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let mut plain = serde_json::to_vec(store).map_err(|e| e.to_string())?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: username.as_bytes() })
        .map_err(|_| "AEAD seal failed".to_string());
    key.zeroize();
    plain.zeroize();
    let ct = ct?;
    let std_b64 = base64::engine::general_purpose::STANDARD;
    let file = StoreFile {
        v: FILE_VERSION,
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

pub fn delete(username: &str) {
    let _ = std::fs::remove_file(path_for(username));
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
                user_data_dir: dir.clone(),
                cache_dir: dir,
                app_version: "test".into(),
                e2ee_local_key: Some([7u8; 32]),
            });
            TempDirGuard
        }
    }

    #[test]
    fn save_load_roundtrip_and_peer_cache() {
        let _g = boot_tmp();
        let user = "alice/Ünïcode";
        let mut k = IdentityKeys::generate(1).unwrap();
        k.key_id = 4;
        let mut store = KeyStore::new(4, vec![k]);
        store.remember_peer_key("bob", 2, &[9u8; 32]);
        store.peers.get_mut("bob").unwrap().sign_pub = vec![1u8; 32];
        save(user, &store).unwrap();
        assert!(exists(user));
        let back = load(user).unwrap().unwrap();
        assert_eq!(back.current_key_id, 4);
        assert_eq!(back.current().unwrap().key_id, 4);
        assert_eq!(back.peer_dh_pub("bob", 2).unwrap(), vec![9u8; 32]);
        assert!(back.peer_dh_pub("bob", 3).is_none());
        assert_eq!(back.peers["bob"].sign_pub, vec![1u8; 32]);
        delete(user);
        assert!(load(user).unwrap().is_none());
    }

    #[test]
    fn store_is_bound_to_username() {
        let _g = boot_tmp();
        let store = KeyStore::new(1, vec![IdentityKeys::generate(1).unwrap()]);
        save("carol", &store).unwrap();
        // Copy carol's file over dave's name: the AAD refuses it.
        std::fs::copy(path_for("carol"), path_for("dave")).unwrap();
        assert!(load("dave").is_err());
        delete("carol");
        delete("dave");
    }
}

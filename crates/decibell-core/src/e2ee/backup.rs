//! Passphrase-wrapped private-key backup, stored on central so a second
//! device can recover the identity with one passphrase.
//!
//!   kek     = Argon2id(passphrase, kdf_salt[16], m = 64 MiB, t = 3, p = 1) → 32 B
//!   header  = 0x01 ‖ m_kib u32 LE ‖ t u32 LE ‖ p u32 LE ‖ kdf_salt ‖ nonce[12]
//!   blob    = header ‖ AES-256-GCM(kek, nonce, JSON(payload), aad = header) ‖ tag
//!
//! The parameters ride in the header so they can be raised later without
//! breaking old blobs; `unwrap` caps them so a hostile blob can't make a
//! client allocate gigabytes. Argon2 takes ~100 ms at these settings —
//! callers run wrap/unwrap on the blocking pool.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

use super::identity::{random_bytes, IdentityKeys};

pub const VERSION: u8 = 1;
const KDF_SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const HEADER_LEN: usize = 1 + 4 * 3 + KDF_SALT_LEN + NONCE_LEN;
const TAG_LEN: usize = 16;
pub const M_KIB: u32 = 64 * 1024;
pub const T_COST: u32 = 3;
pub const P_COST: u32 = 1;
/// Refuse to derive with more than this (a blob is our own upload, but
/// central could hand back anything).
const MAX_M_KIB: u32 = 1024 * 1024;
const MAX_T: u32 = 16;
const MAX_P: u32 = 8;
pub const MIN_PASSPHRASE_CHARS: usize = 10;
/// Server-side cap on the stored blob.
pub const MAX_BLOB_LEN: usize = 8 * 1024;

/// What the backup protects. Keys with `key_id == 0` were published in
/// the same request as this backup; on unwrap they take the key_id the
/// server stored next to the blob.
#[derive(Clone, Serialize, Deserialize)]
pub struct BackupPayload {
    pub current_key_id: u32,
    pub keys: Vec<IdentityKeys>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnwrapError {
    Malformed,
    /// Authentication failed — wrong passphrase (or a corrupted blob;
    /// AEAD can't tell them apart).
    WrongPassphrase,
}

fn derive_kek(passphrase: &str, salt: &[u8], m_kib: u32, t: u32, p: u32) -> Result<[u8; 32], String> {
    let params = Params::new(m_kib, t, p, Some(32)).map_err(|e| format!("argon2 params: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut kek = [0u8; 32];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut kek)
        .map_err(|e| format!("argon2: {e}"))?;
    Ok(kek)
}

pub fn wrap(passphrase: &str, payload: &BackupPayload) -> Result<Vec<u8>, String> {
    if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(format!("passphrase must be at least {MIN_PASSPHRASE_CHARS} characters"));
    }
    let salt: [u8; KDF_SALT_LEN] = random_bytes()?;
    let nonce: [u8; NONCE_LEN] = random_bytes()?;
    let mut header = Vec::with_capacity(HEADER_LEN);
    header.push(VERSION);
    header.extend_from_slice(&M_KIB.to_le_bytes());
    header.extend_from_slice(&T_COST.to_le_bytes());
    header.extend_from_slice(&P_COST.to_le_bytes());
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce);

    let mut kek = derive_kek(passphrase, &salt, M_KIB, T_COST, P_COST)?;
    let mut plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let cipher = Aes256Gcm::new_from_slice(&kek).map_err(|e| e.to_string())?;
    let body = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &header })
        .map_err(|_| "AEAD seal failed".to_string())?;
    kek.zeroize();
    plain.zeroize();

    let mut blob = header;
    blob.extend_from_slice(&body);
    if blob.len() > MAX_BLOB_LEN {
        return Err("backup too large".into());
    }
    Ok(blob)
}

pub fn unwrap(passphrase: &str, blob: &[u8]) -> Result<BackupPayload, UnwrapError> {
    if blob.len() < HEADER_LEN + TAG_LEN || blob[0] != VERSION {
        return Err(UnwrapError::Malformed);
    }
    let u32_at = |at: usize| u32::from_le_bytes([blob[at], blob[at + 1], blob[at + 2], blob[at + 3]]);
    let (m_kib, t, p) = (u32_at(1), u32_at(5), u32_at(9));
    if m_kib == 0 || m_kib > MAX_M_KIB || t == 0 || t > MAX_T || p == 0 || p > MAX_P {
        return Err(UnwrapError::Malformed);
    }
    let header = &blob[..HEADER_LEN];
    let salt = &header[13..13 + KDF_SALT_LEN];
    let nonce = &header[13 + KDF_SALT_LEN..HEADER_LEN];
    let mut kek = derive_kek(passphrase, salt, m_kib, t, p).map_err(|_| UnwrapError::Malformed)?;
    let cipher = Aes256Gcm::new_from_slice(&kek).map_err(|_| UnwrapError::Malformed)?;
    let plain = cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: &blob[HEADER_LEN..], aad: header })
        .map_err(|_| UnwrapError::WrongPassphrase);
    kek.zeroize();
    let mut plain = plain?;
    let payload = serde_json::from_slice::<BackupPayload>(&plain).map_err(|_| UnwrapError::Malformed);
    plain.zeroize();
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BackupPayload {
        let mut k = IdentityKeys::generate(5).unwrap();
        k.key_id = 2;
        BackupPayload { current_key_id: 2, keys: vec![k, IdentityKeys::generate(6).unwrap()] }
    }

    #[test]
    fn round_trip() {
        let p = sample();
        let blob = wrap("correct horse battery", &p).unwrap();
        assert!(blob.len() < MAX_BLOB_LEN);
        let back = unwrap("correct horse battery", &blob).unwrap();
        assert_eq!(back.current_key_id, 2);
        assert_eq!(back.keys.len(), 2);
        assert_eq!(back.keys[0].dh_public().unwrap(), p.keys[0].dh_public().unwrap());
        assert_eq!(back.keys[1].key_id, 0);
    }

    #[test]
    fn wrong_passphrase_and_tamper() {
        let blob = wrap("correct horse battery", &sample()).unwrap();
        assert!(matches!(unwrap("correct horse batter", &blob), Err(UnwrapError::WrongPassphrase)));
        let mut t = blob.clone();
        *t.last_mut().unwrap() ^= 1;
        assert!(matches!(unwrap("correct horse battery", &t), Err(UnwrapError::WrongPassphrase)));
        // header tamper (parameters are authenticated as AAD)
        let mut t = blob.clone();
        t[5] ^= 1;
        assert!(unwrap("correct horse battery", &t).is_err());
        assert!(matches!(unwrap("x", &[]), Err(UnwrapError::Malformed)));
    }

    #[test]
    fn short_passphrase_refused() {
        assert!(wrap("short", &sample()).is_err());
    }

    #[test]
    fn hostile_parameters_refused() {
        let blob = wrap("correct horse battery", &sample()).unwrap();
        let mut t = blob.clone();
        t[1..5].copy_from_slice(&(MAX_M_KIB + 1).to_le_bytes());
        assert!(matches!(unwrap("correct horse battery", &t), Err(UnwrapError::Malformed)));
    }
}

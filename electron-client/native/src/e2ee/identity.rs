//! Long-term E2EE identity: one X25519 agreement key + one Ed25519
//! signing key per user, published to central as a self-signed bundle.
//!
//!   signature = Ed25519(sign_priv, "decibell-e2ee-bundle-v1" ‖ username ‖ dh_pub ‖ sign_pub)
//!
//! The signature binds the two public halves to the username so central
//! can't serve a mix-and-match bundle; a peer's *identity* is its
//! `sign_pub` (that's what fingerprints and safety numbers hash).
//! `key_id` is central-assigned bookkeeping (monotonic per user) that
//! envelopes name so old history can always find the right key.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::{Zeroize, ZeroizeOnDrop};

pub const KEY_LEN: usize = 32;
pub const SIG_LEN: usize = 64;
const BUNDLE_DOMAIN: &[u8] = b"decibell-e2ee-bundle-v1";
const FP_DOMAIN: &[u8] = b"decibell-e2ee-fp-v1";
const SAFETY_DOMAIN: &[u8] = b"decibell-e2ee-safety-v1";

/// Our private identity (one generation). Serialised into the local key
/// store and the passphrase backup; zeroised on drop.
#[derive(Clone, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
pub struct IdentityKeys {
    /// 0 until central assigns one (a freshly generated identity).
    pub key_id: u32,
    #[serde(with = "b64")]
    pub dh_priv: Vec<u8>,
    /// Ed25519 seed.
    #[serde(with = "b64")]
    pub sign_priv: Vec<u8>,
    pub created_at: i64,
}

impl std::fmt::Debug for IdentityKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdentityKeys")
            .field("key_id", &self.key_id)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// A user's public identity as published on central.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicBundle {
    pub username: String,
    pub key_id: u32,
    #[serde(with = "b64")]
    pub dh_pub: Vec<u8>,
    #[serde(with = "b64")]
    pub sign_pub: Vec<u8>,
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
    pub created_at: i64,
}

pub fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut out = [0u8; N];
    SystemRandom::new()
        .fill(&mut out)
        .map_err(|_| "system RNG failed".to_string())?;
    Ok(out)
}

fn arr32(bytes: &[u8], what: &str) -> Result<[u8; KEY_LEN], String> {
    if bytes.len() != KEY_LEN {
        return Err(format!("{what} must be {KEY_LEN} bytes, got {}", bytes.len()));
    }
    let mut a = [0u8; KEY_LEN];
    a.copy_from_slice(bytes);
    Ok(a)
}

fn bundle_message(username: &str, dh_pub: &[u8], sign_pub: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(BUNDLE_DOMAIN.len() + username.len() + KEY_LEN * 2 + 2);
    m.extend_from_slice(BUNDLE_DOMAIN);
    m.push(0);
    m.extend_from_slice(username.as_bytes());
    m.push(0);
    m.extend_from_slice(dh_pub);
    m.extend_from_slice(sign_pub);
    m
}

impl IdentityKeys {
    pub fn generate(created_at: i64) -> Result<IdentityKeys, String> {
        let dh: [u8; KEY_LEN] = random_bytes()?;
        let seed: [u8; KEY_LEN] = random_bytes()?;
        Ok(IdentityKeys {
            key_id: 0,
            dh_priv: dh.to_vec(),
            sign_priv: seed.to_vec(),
            created_at,
        })
    }

    pub fn dh_secret(&self) -> Result<StaticSecret, String> {
        Ok(StaticSecret::from(arr32(&self.dh_priv, "dh_priv")?))
    }

    pub fn dh_public(&self) -> Result<[u8; KEY_LEN], String> {
        Ok(PublicKey::from(&self.dh_secret()?).to_bytes())
    }

    fn signing_key(&self) -> Result<SigningKey, String> {
        Ok(SigningKey::from_bytes(&arr32(&self.sign_priv, "sign_priv")?))
    }

    pub fn sign_public(&self) -> Result<[u8; KEY_LEN], String> {
        Ok(self.signing_key()?.verifying_key().to_bytes())
    }

    /// The self-signed bundle to publish. `key_id` is whatever this
    /// identity currently carries (0 before central assigns one).
    pub fn public_bundle(&self, username: &str) -> Result<PublicBundle, String> {
        let dh_pub = self.dh_public()?;
        let sign_pub = self.sign_public()?;
        let sig: Signature = self
            .signing_key()?
            .sign(&bundle_message(username, &dh_pub, &sign_pub));
        Ok(PublicBundle {
            username: username.to_string(),
            key_id: self.key_id,
            dh_pub: dh_pub.to_vec(),
            sign_pub: sign_pub.to_vec(),
            signature: sig.to_bytes().to_vec(),
            created_at: self.created_at,
        })
    }
}

impl PublicBundle {
    /// Structural + signature check. Anything that fails here is treated
    /// as "no keys" by the caller: never seal to an unverified key.
    pub fn verify(&self) -> Result<(), String> {
        if self.username.is_empty() {
            return Err("bundle has no username".into());
        }
        let sign_pub = arr32(&self.sign_pub, "sign_pub")?;
        arr32(&self.dh_pub, "dh_pub")?;
        if self.signature.len() != SIG_LEN {
            return Err(format!("signature must be {SIG_LEN} bytes"));
        }
        let vk = VerifyingKey::from_bytes(&sign_pub).map_err(|_| "bad sign_pub".to_string())?;
        let mut sig = [0u8; SIG_LEN];
        sig.copy_from_slice(&self.signature);
        vk.verify(
            &bundle_message(&self.username, &self.dh_pub, &self.sign_pub),
            &Signature::from_bytes(&sig),
        )
        .map_err(|_| "bundle signature does not verify".to_string())
    }

    pub fn dh_pub_array(&self) -> Result<[u8; KEY_LEN], String> {
        arr32(&self.dh_pub, "dh_pub")
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.username, &self.sign_pub)
    }
}

/// 30 bytes of SHA-256 → 48 base32 chars → "XXXX XXXX … " (12 groups).
fn format_groups(digest: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut bits: u32 = 0;
    let mut nbits = 0;
    let mut chars = Vec::with_capacity(48);
    for &b in &digest[..30] {
        bits = (bits << 8) | b as u32;
        nbits += 8;
        while nbits >= 5 {
            nbits -= 5;
            chars.push(ALPHABET[((bits >> nbits) & 31) as usize]);
        }
    }
    let mut out = String::with_capacity(48 + 11);
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && i % 4 == 0 {
            out.push(' ');
        }
        out.push(*c as char);
    }
    out
}

/// A user's identity fingerprint — what two people compare out of band.
pub fn fingerprint(username: &str, sign_pub: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(FP_DOMAIN);
    h.update([0u8]);
    h.update(username.as_bytes());
    h.update([0u8]);
    h.update(sign_pub);
    format_groups(&h.finalize())
}

/// One string per conversation, identical on both sides: hash of the two
/// fingerprints in byte order of the usernames.
pub fn safety_number(a_user: &str, a_sign_pub: &[u8], b_user: &str, b_sign_pub: &[u8]) -> String {
    let (lo, hi) = if a_user <= b_user {
        ((a_user, a_sign_pub), (b_user, b_sign_pub))
    } else {
        ((b_user, b_sign_pub), (a_user, a_sign_pub))
    };
    let mut h = Sha256::new();
    h.update(SAFETY_DOMAIN);
    h.update([0u8]);
    h.update(fingerprint(lo.0, lo.1).as_bytes());
    h.update([0u8]);
    h.update(fingerprint(hi.0, hi.1).as_bytes());
    format_groups(&h.finalize())
}

/// serde helper: Vec<u8> ⇄ standard base64.
pub mod b64 {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        base64::engine::general_purpose::STANDARD.encode(v).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_signs_and_verifies() {
        let id = IdentityKeys::generate(1).unwrap();
        let b = id.public_bundle("alice").unwrap();
        b.verify().unwrap();
        assert_eq!(b.dh_pub.len(), 32);
        assert_eq!(b.sign_pub.len(), 32);
        assert_eq!(b.signature.len(), 64);
    }

    #[test]
    fn bundle_rejects_tampering() {
        let id = IdentityKeys::generate(1).unwrap();
        let mut b = id.public_bundle("alice").unwrap();
        b.username = "bob".into();
        assert!(b.verify().is_err());
        let mut b = id.public_bundle("alice").unwrap();
        b.dh_pub[3] ^= 1;
        assert!(b.verify().is_err());
        let other = IdentityKeys::generate(1).unwrap();
        let mut b = id.public_bundle("alice").unwrap();
        b.sign_pub = other.sign_public().unwrap().to_vec();
        assert!(b.verify().is_err());
    }

    #[test]
    fn fingerprint_format_and_symmetry() {
        let a = IdentityKeys::generate(1).unwrap();
        let b = IdentityKeys::generate(1).unwrap();
        let fa = fingerprint("alice", &a.sign_public().unwrap());
        assert_eq!(fa.len(), 48 + 11);
        assert_eq!(fa.split(' ').count(), 12);
        let s1 = safety_number("alice", &a.sign_public().unwrap(), "bob", &b.sign_public().unwrap());
        let s2 = safety_number("bob", &b.sign_public().unwrap(), "alice", &a.sign_public().unwrap());
        assert_eq!(s1, s2);
        assert_ne!(s1, fa);
    }

    #[test]
    fn identity_roundtrips_through_json() {
        let id = IdentityKeys::generate(7).unwrap();
        let json = serde_json::to_string(&id).unwrap();
        let back: IdentityKeys = serde_json::from_str(&json).unwrap();
        assert_eq!(back.dh_public().unwrap(), id.dh_public().unwrap());
        assert_eq!(back.sign_public().unwrap(), id.sign_public().unwrap());
    }
}

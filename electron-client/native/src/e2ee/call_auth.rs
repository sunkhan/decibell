//! Authenticated key exchange for P2P DM calls.
//!
//! A call's media is sealed with keys from an ephemeral X25519 exchange
//! (`media::call_crypto`), but the ephemeral public keys ride through
//! central inside INVITE / ACCEPT — a malicious central could swap them
//! and sit in the middle. The E2EE identity closes that: each side signs
//! its ephemeral key with its Ed25519 identity key,
//!
//!   sig = Ed25519(sign_priv, "decibell-call-auth-v1" ‖ 0 ‖ call_id ‖ 0 ‖ from ‖ 0 ‖ to ‖ 0 ‖ pub_key)
//!
//! and the other side verifies it against the peer's *current* identity
//! (the TOFU-pinned one). Binding the call id and both names means a
//! signature can't be replayed into another call or another pair.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use super::identity::{IdentityKeys, KEY_LEN, SIG_LEN};

const DOMAIN: &[u8] = b"decibell-call-auth-v1";

fn message(call_id: &str, from: &str, to: &str, pub_key: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(DOMAIN.len() + call_id.len() + from.len() + to.len() + pub_key.len() + 4);
    m.extend_from_slice(DOMAIN);
    m.push(0);
    m.extend_from_slice(call_id.as_bytes());
    m.push(0);
    m.extend_from_slice(from.as_bytes());
    m.push(0);
    m.extend_from_slice(to.as_bytes());
    m.push(0);
    m.extend_from_slice(pub_key);
    m
}

/// Sign our ephemeral call key. `from` is our own username (central stamps
/// the same value on the relayed packet — an honest central, that is).
pub fn sign(identity: &IdentityKeys, call_id: &str, from: &str, to: &str, pub_key: &[u8]) -> Result<[u8; SIG_LEN], String> {
    if identity.sign_priv.len() != KEY_LEN {
        return Err("corrupt signing key".into());
    }
    let mut seed = [0u8; KEY_LEN];
    seed.copy_from_slice(&identity.sign_priv);
    let sk = SigningKey::from_bytes(&seed);
    let sig: Signature = sk.sign(&message(call_id, from, to, pub_key));
    Ok(sig.to_bytes())
}

/// Verify the peer's signature over their ephemeral key with their
/// identity signing key.
pub fn verify(sign_pub: &[u8], call_id: &str, from: &str, to: &str, pub_key: &[u8], sig: &[u8]) -> bool {
    if sign_pub.len() != KEY_LEN || sig.len() != SIG_LEN {
        return false;
    }
    let mut pk = [0u8; KEY_LEN];
    pk.copy_from_slice(sign_pub);
    let Ok(vk) = VerifyingKey::from_bytes(&pk) else { return false };
    let mut s = [0u8; SIG_LEN];
    s.copy_from_slice(sig);
    vk.verify(&message(call_id, from, to, pub_key), &Signature::from_bytes(&s)).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_binding() {
        let id = IdentityKeys::generate(1).unwrap();
        let pk = id.sign_public().unwrap();
        let eph = [7u8; 32];
        let sig = sign(&id, "call-1", "alice", "bob", &eph).unwrap();
        assert!(verify(&pk, "call-1", "alice", "bob", &eph, &sig));
        // every bound field matters
        assert!(!verify(&pk, "call-2", "alice", "bob", &eph, &sig));
        assert!(!verify(&pk, "call-1", "bob", "alice", &eph, &sig));
        assert!(!verify(&pk, "call-1", "alice", "carol", &eph, &sig));
        assert!(!verify(&pk, "call-1", "alice", "bob", &[8u8; 32], &sig));
        // another identity
        let other = IdentityKeys::generate(1).unwrap();
        assert!(!verify(&other.sign_public().unwrap(), "call-1", "alice", "bob", &eph, &sig));
        // malformed
        assert!(!verify(&pk, "call-1", "alice", "bob", &eph, &sig[..63]));
        assert!(!verify(&pk[..31], "call-1", "alice", "bob", &eph, &sig));
    }
}

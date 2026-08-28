//! Key agreement + schedule for P2P DM calls.
//!
//! Each side mints an ephemeral X25519 key per call and ships the public
//! half through central (INVITE / ACCEPT). Both derive the same material:
//!
//!   shared = X25519(my_secret, peer_public)
//!   prk    = HKDF-Extract(salt = call_id, ikm = shared)
//!   okm    = HKDF-Expand(prk, info = "decibell-call-v1|<lower>|<higher>", 144 B)
//!
//! `okm` is sliced into four (key, salt) pairs — voice a→b, voice b→a,
//! media a→b, media b→a — where side A is the lexicographically lower
//! username. Binding both usernames and the call id into the schedule
//! means a key from one call/pair can never open another's datagrams.
//! No key-confirmation message is needed: the first datagram that opens
//! is the confirmation.
//!
//! Trust model: central authenticates both identities and relays the
//! public keys. A malicious central could substitute keys (MITM); a
//! safety-number display is the planned mitigation, not in v1.

use ring::{agreement, hkdf, rand};

use super::media_socket::SocketKeys;

pub const PUBLIC_KEY_LEN: usize = 32;
const KEY_LEN: usize = 32;
const SALT_LEN: usize = 4;
const PAIR_LEN: usize = KEY_LEN + SALT_LEN;
const OKM_LEN: usize = PAIR_LEN * 4;

pub struct CallKeys {
    pub voice: SocketKeys,
    pub media: SocketKeys,
}

/// Our half of the agreement. Consumed by `derive` (ring's ephemeral key
/// can only be used once, by design).
pub struct LocalKeyPair {
    secret: agreement::EphemeralPrivateKey,
    pub public: [u8; PUBLIC_KEY_LEN],
}

pub fn generate() -> Result<LocalKeyPair, String> {
    let rng = rand::SystemRandom::new();
    let secret = agreement::EphemeralPrivateKey::generate(&agreement::X25519, &rng)
        .map_err(|_| "X25519 keygen failed".to_string())?;
    let pk = secret
        .compute_public_key()
        .map_err(|_| "X25519 public key failed".to_string())?;
    let mut public = [0u8; PUBLIC_KEY_LEN];
    public.copy_from_slice(pk.as_ref());
    Ok(LocalKeyPair { secret, public })
}

struct OkmLen(usize);
impl hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        self.0
    }
}

pub fn derive(
    local: LocalKeyPair,
    peer_public: &[u8],
    call_id: &str,
    self_username: &str,
    peer_username: &str,
) -> Result<CallKeys, String> {
    if peer_public.len() != PUBLIC_KEY_LEN {
        return Err(format!("peer public key must be {PUBLIC_KEY_LEN} bytes"));
    }
    if self_username == peer_username {
        return Err("cannot call yourself".to_string());
    }
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, peer_public);
    let (lower, higher) = if self_username < peer_username {
        (self_username, peer_username)
    } else {
        (peer_username, self_username)
    };
    let we_are_a = self_username == lower;
    let info = format!("decibell-call-v1|{lower}|{higher}");

    let okm: [u8; OKM_LEN] = agreement::agree_ephemeral(local.secret, &peer, |shared| {
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, call_id.as_bytes()).extract(shared);
        let mut out = [0u8; OKM_LEN];
        prk.expand(&[info.as_bytes()], OkmLen(OKM_LEN))
            .and_then(|o| o.fill(&mut out))
            .map(|_| out)
    })
    .map_err(|_| "X25519 agreement failed".to_string())?
    .map_err(|_| "HKDF expand failed".to_string())?;

    let pair = |i: usize| -> ([u8; KEY_LEN], [u8; SALT_LEN]) {
        let base = i * PAIR_LEN;
        let mut k = [0u8; KEY_LEN];
        let mut s = [0u8; SALT_LEN];
        k.copy_from_slice(&okm[base..base + KEY_LEN]);
        s.copy_from_slice(&okm[base + KEY_LEN..base + PAIR_LEN]);
        (k, s)
    };
    // slots: 0 voice a→b, 1 voice b→a, 2 media a→b, 3 media b→a
    let socket_keys = |a2b: usize, b2a: usize| {
        let (ka, sa) = pair(a2b);
        let (kb, sb) = pair(b2a);
        if we_are_a {
            SocketKeys { tx_key: ka, tx_salt: sa, rx_key: kb, rx_salt: sb }
        } else {
            SocketKeys { tx_key: kb, tx_salt: sb, rx_key: ka, rx_salt: sa }
        }
    };
    Ok(CallKeys {
        voice: socket_keys(0, 1),
        media: socket_keys(2, 3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_derive_mirrored_keys() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        let (pa, pb) = (a.public, b.public);
        let ka = derive(a, &pb, "call-1", "alice", "bob").unwrap();
        let kb = derive(b, &pa, "call-1", "bob", "alice").unwrap();
        assert_eq!(ka.voice.tx_key, kb.voice.rx_key);
        assert_eq!(ka.voice.rx_key, kb.voice.tx_key);
        assert_eq!(ka.voice.tx_salt, kb.voice.rx_salt);
        assert_eq!(ka.media.tx_key, kb.media.rx_key);
        assert_eq!(ka.media.rx_key, kb.media.tx_key);
        assert_ne!(ka.voice.tx_key, ka.voice.rx_key);
        assert_ne!(ka.voice.tx_key, ka.media.tx_key);
    }

    #[test]
    fn call_id_separates_keys() {
        let a1 = generate().unwrap();
        let b1 = generate().unwrap();
        let (pa, pb) = (a1.public, b1.public);
        let k1 = derive(a1, &pb, "call-1", "alice", "bob").unwrap();
        // same peer key material, different call id → different keys
        let a2 = generate().unwrap();
        let _ = pa;
        let k2 = derive(a2, &pb, "call-2", "alice", "bob").unwrap();
        assert_ne!(k1.voice.tx_key, k2.voice.tx_key);
    }

    #[test]
    fn rejects_bad_peer_key() {
        let a = generate().unwrap();
        assert!(derive(a, &[0u8; 31], "c", "alice", "bob").is_err());
    }
}

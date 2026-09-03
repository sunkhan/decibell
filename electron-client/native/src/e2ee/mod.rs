//! End-to-end encrypted DMs. Design:
//! docs/superpowers/specs/2026-09-03-e2ee-dms-design.md.
//!
//!   identity  — X25519 + Ed25519 identity, self-signed bundle, fingerprints
//!   envelope  — the sealed message body (per-message HKDF + AES-256-GCM)
//!   backup    — passphrase (Argon2id) wrapped private keys, stored on central
//!   call_auth — Ed25519 signatures over the ephemeral P2P-call keys
//!   group     — the MLS group per voice channel (OpenMLS) → media keys
//!   keystore  — the encrypted-at-rest local store (own keys + peer pins)
//!   session   — runtime: status, key resolution, seal/open, the DM worker
//!
//! The renderer never touches any of this directly: it sees decrypted DM
//! events with `encrypted` / `decryptError` flags, a status event, and the
//! `e2ee_*` commands in `commands/e2ee.rs`.

pub mod backup;
pub mod call_auth;
pub mod envelope;
pub mod group;
pub mod identity;
pub mod keystore;
pub mod session;

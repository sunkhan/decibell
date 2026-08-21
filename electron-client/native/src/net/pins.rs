//! Certificate pinning store (Theme A).
//!
//! Every TLS connection the client makes (central, each community) used to
//! accept any certificate (`NoVerifier`), so anyone on the path could
//! terminate TLS and read the password + JWT. Now:
//!
//! * **Central** is trust-on-first-use: the sha256 of the certificate seen
//!   on the first connect is stored here and every later connect must
//!   present the same one.
//! * **Communities** are pinned to the fingerprint central reports for them
//!   (`CommunityServerInfo.cert_fingerprint`, learned from the directory,
//!   memberships, or invite resolution). Central is itself pinned, so that
//!   channel is trusted. A community central hasn't fingerprinted yet (or a
//!   raw `host:port` deep link) falls back to TOFU.
//!
//! A mismatch fails the handshake with a `CERT_MISMATCH:` error the renderer
//! recognises; `trust_certificate` lets the user re-pin deliberately.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

struct Store {
    loaded: bool,
    /// host:port → sha256-hex of the pinned leaf certificate.
    pins: HashMap<String, String>,
    /// host:port → fingerprint central reported for that community.
    expected: HashMap<String, String>,
    /// host:port the user chose to re-trust: the next connect pins
    /// whatever is presented and ignores `expected` once.
    retrust: HashSet<String>,
}

static STORE: OnceLock<Mutex<Store>> = OnceLock::new();

fn store() -> &'static Mutex<Store> {
    STORE.get_or_init(|| {
        Mutex::new(Store {
            loaded: false,
            pins: HashMap::new(),
            expected: HashMap::new(),
            retrust: HashSet::new(),
        })
    })
}

fn pins_path() -> PathBuf {
    crate::state::boot().user_data_dir.join("cert_pins.json")
}

fn ensure_loaded(store: &mut Store) {
    if store.loaded {
        return;
    }
    store.loaded = true;
    if let Ok(text) = std::fs::read_to_string(pins_path()) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) {
            store.pins = map;
        }
    }
}

fn persist(store: &Store) {
    if let Ok(text) = serde_json::to_string_pretty(&store.pins) {
        let path = pins_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, text) {
            log::warn!("cert_pins.json: write failed: {}", e);
        }
    }
}

pub fn key(host: &str, port: u16) -> String {
    format!("{}:{}", host.to_ascii_lowercase(), port)
}

/// Record the fingerprint central reported for a community.
pub fn set_expected(host: &str, port: u16, fingerprint: &str) {
    if fingerprint.is_empty() {
        return;
    }
    let mut s = store().lock().unwrap();
    s.expected.insert(key(host, port), fingerprint.to_ascii_lowercase());
}

/// What the verifier should enforce for a connection to host:port.
pub enum Policy {
    /// Must equal this fingerprint (central-reported or previously pinned).
    Exact(String),
    /// Nothing known yet: accept, then pin.
    Tofu,
}

pub fn policy_for(host: &str, port: u16) -> Policy {
    let mut s = store().lock().unwrap();
    ensure_loaded(&mut s);
    let k = key(host, port);
    if s.retrust.contains(&k) {
        return Policy::Tofu;
    }
    if let Some(fp) = s.expected.get(&k) {
        return Policy::Exact(fp.clone());
    }
    if let Some(fp) = s.pins.get(&k) {
        return Policy::Exact(fp.clone());
    }
    Policy::Tofu
}

/// Called by the verifier after a successful handshake decision.
pub fn record_seen(host: &str, port: u16, fingerprint: &str) {
    let mut s = store().lock().unwrap();
    ensure_loaded(&mut s);
    let k = key(host, port);
    s.retrust.remove(&k);
    if s.pins.get(&k).map(|f| f.as_str()) != Some(fingerprint) {
        s.pins.insert(k, fingerprint.to_string());
        persist(&s);
    }
}

/// User-initiated: forget what we knew about host:port and accept whatever
/// it presents on the next connect (pinning that).
pub fn retrust(host: &str, port: u16) {
    let mut s = store().lock().unwrap();
    ensure_loaded(&mut s);
    let k = key(host, port);
    s.pins.remove(&k);
    s.expected.remove(&k);
    s.retrust.insert(k);
    persist(&s);
}

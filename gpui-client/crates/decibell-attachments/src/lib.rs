//! Attachments for the native client: what the Electron client split between its renderer
//! (upload, thumbnails, ThumbHash, encryption) and its main process (download, decryption,
//! Range playback, speed caps, the downloads manager), as one plain Rust crate on tokio.
//! No UI types; the app drives it and draws what it reports.
//!
//! | Electron | Here |
//! |---|---|
//! | `features/chat/attachmentCrypto.ts`, `electron/main/attachmentCrypto.ts` | [`crypto`] |
//! | `features/chat/thumbhash.ts` (+ the `thumbhash` npm package) | [`thumbhash`] |
//! | `uploadAttachment.ts` probe / thumbnails, `fs.ts` `guessMime` | [`thumbs`], [`kind`] |
//! | `uploadAttachment.ts` upload, `uploadPacing.ts`, `attachmentsStore.ts` progress | [`upload`] |
//! | `netFetch.ts`, `attachmentFetch.ts`, `downloadPacer.ts`, `protocol.ts` | [`fetch`] |
//! | `downloads.ts`, `features/transfers/downloads.ts` | [`manager`] |
//! | `protocol.ts` + `mediaServer.ts` (bytes by range for `<img>` / `<video>`), `imageRetention.ts` | [`media`] |
//! | `attachmentRegistry.ts`, `attachmentKeys.ts` | [`Attachments`] (targets + keys) |
//!
//! ## Wiring it into the app
//!
//! ```ignore
//! // Once, inside the app's tokio runtime:
//! let attachments = Attachments::new(Arc::new(CoreTls), tokio::runtime::Handle::current());
//! // On COMMUNITY_AUTH_RES and on every session-token renewal (core's get_attachment_target):
//! attachments.set_target(server_id, AttachmentTarget { host, port, jwt, max_bytes, cert_fingerprint });
//! // As messages of an encrypted channel are opened (core's encrypted_attachments):
//! attachments.register_keys(server_id, metas);
//! ```
//!
//! `CoreTls` is the app's [`PinnedTls`]: today `decibell_core::net::pins::set_expected(host,
//! port, fp)` then `decibell_core::net::tls::create_pinned_connector(host, port).0` (core's
//! verifier, exact pin); see the hand-off notes for the side-effect-free constructor core
//! should grow.

pub mod crypto;
mod error;
pub mod fetch;
mod http;
pub mod kind;
pub mod manager;
pub mod media;
pub mod names;
mod pacer;
pub mod thumbhash;
pub mod thumbs;
pub mod upload;

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub use crate::crypto::FileKey;
pub use crate::error::Error;
pub use crate::http::PinnedTls;
pub use crate::kind::AttachmentKind;
pub use crate::pacer::{Limits, MIN_LIMIT_BPS};
pub use tokio_util::sync::CancellationToken;

use crate::http::{Endpoint, HttpClient};
use crate::pacer::DownloadPacers;

/// A community's attachment endpoint, as core's `get_attachment_target` gives it.
#[derive(Clone, Debug)]
pub struct AttachmentTarget {
    pub host: String,
    /// The attachment HTTPS port (`COMMUNITY_AUTH_RES.attachment_port`, 8085).
    pub port: u16,
    /// The session token, sent as `Authorization: Bearer`. Replace the target when central
    /// renews it (Electron re-read targets on `session_token_updated`, review B2).
    pub jwt: String,
    /// Per-file upload cap the server reported; 0 = unlimited.
    pub max_bytes: u64,
    /// sha256-hex of the certificate the community presented on its chat connection; the
    /// attachment listener shares it, and connections are pinned to exactly this.
    pub cert_fingerprint: String,
}

impl AttachmentTarget {
    pub(crate) fn endpoint(&self) -> Endpoint {
        Endpoint { host: self.host.clone(), port: self.port, fingerprint: self.cert_fingerprint.to_ascii_lowercase() }
    }
}

/// The real metadata + key of an encrypted-channel attachment, as the message envelope carries
/// it (proto `EncryptedAttachmentMeta`; core's `EncryptedAttachmentMetaPayload` and the send
/// path's `AttachmentMeta` have exactly these fields).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EncryptedAttachmentMeta {
    pub id: i64,
    pub key_b64: String,
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub placeholder: String,
    pub chunk_bytes: u32,
    pub thumbnail_sizes_mask: u32,
}

/// A registered key: what the readers need to open a sealed attachment.
#[derive(Clone, Debug)]
pub struct AttachmentKey {
    pub key: FileKey,
    pub chunk_bytes: u64,
    /// Plaintext size.
    pub size_bytes: u64,
    pub mime: String,
    pub filename: String,
}

impl AttachmentKey {
    /// From an envelope entry; `None` for a malformed one (wrong key length, chunk size out of
    /// range), as main's `registerKeys` skipped them.
    pub fn from_meta(m: &EncryptedAttachmentMeta) -> Option<Self> {
        let key = FileKey::from_b64(&m.key_b64).ok()?;
        let chunk = m.chunk_bytes as u64;
        if chunk == 0 || chunk > crypto::MAX_CHUNK_BYTES {
            return None;
        }
        Some(Self {
            key,
            chunk_bytes: chunk,
            size_bytes: m.size_bytes.max(0) as u64,
            mime: if m.mime.is_empty() { "application/octet-stream".into() } else { m.mime.clone() },
            filename: if m.filename.is_empty() { "attachment".into() } else { m.filename.clone() },
        })
    }
}

/// The service: attachment targets per server, keys per (server, attachment), the HTTP client
/// and the speed caps. One per app; cheap to share (`Arc`).
pub struct Attachments {
    pub(crate) http: HttpClient,
    targets: RwLock<HashMap<String, AttachmentTarget>>,
    keys: RwLock<HashMap<(String, i64), AttachmentKey>>,
    limits: Limits,
    pub(crate) pacers: DownloadPacers,
    pub(crate) rt: tokio::runtime::Handle,
}

impl Attachments {
    /// `rt` runs the connection drivers and the downloads manager's transfers, so the sync
    /// methods may be called from any thread (the UI's).
    pub fn new(tls: Arc<dyn PinnedTls>, rt: tokio::runtime::Handle) -> Arc<Self> {
        Arc::new(Self {
            http: HttpClient::new(tls),
            targets: RwLock::new(HashMap::new()),
            keys: RwLock::new(HashMap::new()),
            limits: Limits::default(),
            pacers: DownloadPacers::default(),
            rt,
        })
    }

    /// Register or replace a server's target (community auth, token renewal).
    pub fn set_target(&self, server_id: &str, target: AttachmentTarget) {
        let mut t = self.targets.write().unwrap();
        if let Some(old) = t.get(server_id) {
            if old.host != target.host || old.port != target.port || old.cert_fingerprint != target.cert_fingerprint {
                self.http.forget_host(&old.host, old.port);
            }
        }
        t.insert(server_id.to_string(), target);
    }

    /// Forget a server's target (disconnect, leave).
    pub fn clear_target(&self, server_id: &str) {
        if let Some(old) = self.targets.write().unwrap().remove(server_id) {
            self.http.forget_host(&old.host, old.port);
        }
    }

    /// Forget every target (sign-out).
    pub fn clear_all_targets(&self) {
        let mut t = self.targets.write().unwrap();
        for old in t.values() {
            self.http.forget_host(&old.host, old.port);
        }
        t.clear();
    }

    pub fn target(&self, server_id: &str) -> Option<AttachmentTarget> {
        self.targets.read().unwrap().get(server_id).cloned()
    }

    /// Register the keys of a message's encrypted attachments (per session, never persisted).
    /// Malformed entries are skipped; at most 200 per call, as main's IPC handler took.
    pub fn register_keys<'a>(&self, server_id: &str, metas: impl IntoIterator<Item = &'a EncryptedAttachmentMeta>) {
        let mut keys = self.keys.write().unwrap();
        for m in metas.into_iter().take(200) {
            if let Some(k) = AttachmentKey::from_meta(m) {
                keys.insert((server_id.to_string(), m.id), k);
            }
        }
    }

    /// Register one key directly (e.g. an upload we just sealed).
    pub fn register_key(&self, server_id: &str, attachment_id: i64, key: AttachmentKey) {
        self.keys.write().unwrap().insert((server_id.to_string(), attachment_id), key);
    }

    /// Drop the keys of one server (`Some`) or all of them.
    pub fn clear_keys(&self, server_id: Option<&str>) {
        let mut keys = self.keys.write().unwrap();
        match server_id {
            None => keys.clear(),
            Some(s) => keys.retain(|(sid, _), _| sid != s),
        }
    }

    pub fn key(&self, server_id: &str, attachment_id: i64) -> Option<AttachmentKey> {
        self.keys.read().unwrap().get(&(server_id.to_string(), attachment_id)).cloned()
    }

    /// The speed caps (Settings → Network).
    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub(crate) fn require_target(&self, server_id: &str) -> Result<AttachmentTarget, Error> {
        self.target(server_id).ok_or(Error::NotConnected)
    }
}

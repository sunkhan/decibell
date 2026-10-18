//! Persisted configuration: the saved sign-in (encrypted) and plain-JSON
//! `AppSettings`. Lives at `<userData>/config.json`, written 0600 through
//! write-to-temp-then-rename.
//!
//! The saved sign-in keeps "signed in until you log out" across restarts
//! (owner decision: keep the stored password, lock it down — review S10):
//!   - sealed with AES-256-GCM under a key derived from the at-rest key
//!     Electron main keeps wrapped by the OS keychain (safeStorage, the
//!     same key the E2EE key store uses, `BootConfig.e2ee_local_key`),
//!     with a random nonce per seal. Without a protected keychain
//!     (safeStorage unavailable, or Linux's `basic_text` backend, which
//!     wraps with a hard-coded password) it falls back to the
//!     hostname+user derivation below — obfuscation only — and says so in
//!     the log; the file records which key sealed it;
//!   - never handed to the renderer: `load_config` reports only the
//!     username, and native signs in itself (`commands::auth::auto_login`);
//!   - saved only once central accepts a password sign-in, and forgotten
//!     when central refuses that password outright (net/central.rs);
//!   - the old format (derived key, clock-seeded nonce) is opened once and
//!     re-sealed in the new one, so nobody is signed out by the upgrade.
//!
//! Replaces `tauri::AppHandle::path().app_data_dir()` with
//! `state::boot().user_data_dir`, set once from Electron main's
//! `app.getPath('userData')`.

use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const SALT: &[u8] = b"decibell-config-v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub friends_only_dms: bool,
    #[serde(default)]
    pub stream_stereo: bool,
    /// When switching voice channels while screen-sharing, carry the stream
    /// into the new channel instead of ending it. Default false = end it.
    #[serde(default)]
    pub take_stream_on_channel_switch: bool,
    /// None means "use system default"
    pub input_device: Option<String>,
    /// None means "use system default"
    pub output_device: Option<String>,
    /// Use a separate output device for stream audio
    #[serde(default)]
    pub separate_stream_output: bool,
    /// Output device for stream audio when separate_stream_output is true (None = system default)
    pub stream_output_device: Option<String>,
    /// Voice activation threshold in dB (-60 to 0). Below this, mic input is silenced.
    /// None means use default (-50 dB).
    pub voice_threshold_db: Option<f64>,
    /// Echo cancellation (AEC3) — removes speaker audio bleeding into the mic
    #[serde(default)]
    pub aec_enabled: bool,
    /// Noise suppression level: 0=off, 1=light(6dB), 2=moderate(12dB), 3=aggressive(18dB), 4=very aggressive(21dB)
    #[serde(default)]
    pub noise_suppression_level: u8,
    /// Automatic gain control (AGC2) — normalizes mic volume. Default on;
    /// a config that already carries the field keeps whatever it says.
    #[serde(default = "default_true")]
    pub agc_enabled: bool,
    /// Per-user volume in dB by account uid (decimal). 0 = default,
    /// negative = quieter, positive = louder. (The name-keyed
    /// `user_volumes` / `local_muted_users` it replaced are dropped: a name
    /// can belong to someone else after a rename.)
    #[serde(default)]
    pub user_volumes_by_uid: std::collections::HashMap<String, f64>,
    /// Accounts (uid, decimal) locally muted by this client.
    #[serde(default)]
    pub local_muted_uids: Vec<String>,
    /// Attachment upload cap in bytes per second. 0 = unlimited.
    #[serde(default)]
    pub upload_limit_bps: u64,
    /// Attachment download cap in bytes per second. 0 = unlimited.
    #[serde(default)]
    pub download_limit_bps: u64,
    /// Persisted volume (0.0–1.0) for the chat audio-attachment player.
    /// None = never set; client falls back to 1.0.
    pub media_audio_volume: Option<f64>,
    #[serde(default)]
    pub media_audio_muted: bool,
    /// Persisted volume (0.0–1.0) for the chat video-attachment player.
    pub media_video_volume: Option<f64>,
    #[serde(default)]
    pub media_video_muted: bool,

    /// Codec preference toggles. When false, that codec is removed
    /// from the encode list before advertisement to peers, so the
    /// streamer never auto-picks it. Both default true.
    #[serde(default = "default_true")]
    pub use_av1: bool,
    #[serde(default = "default_true")]
    pub use_h265: bool,

    /// Last-used screen-share / streaming settings. Each field is
    /// Option-wrapped so missing entries fall back to client defaults
    /// rather than overwriting them with zeros on first load.
    pub stream_resolution: Option<String>,
    pub stream_fps: Option<u32>,
    pub stream_quality: Option<String>,
    pub stream_video_bitrate_kbps: Option<u32>,
    pub stream_share_audio: Option<bool>,
    pub stream_audio_bitrate_kbps: Option<u32>,
    /// Share-audio application filter: `selected` | `all_except` | `all`
    /// (see `media::stream_audio_filter::StreamAudioMode`) plus the ticked
    /// app identities (lowercase program names, platform-local). Stored as
    /// plain strings so an unknown mode from a newer build round-trips;
    /// the client falls back to `all` if it doesn't recognise it.
    pub stream_audio_mode: Option<String>,
    pub stream_audio_apps: Option<Vec<String>>,
    /// VideoCodec enum byte (0=UNKNOWN/Auto, 1=H264_HW, 2=H264_SW,
    /// 3=H265, 4=AV1). Restored on load; downgraded to 0 by the client
    /// at runtime if the saved codec isn't in the user's encodeCaps.
    pub stream_enforced_codec: Option<u8>,

    /// Selected UI palette: one of `nocturne`, `matinee`, `graphite`,
    /// `graphite-light`, `console`, `console-light`, `console-split`. Stored as a plain
    /// string rather than an enum so an unknown value from a newer
    /// build round-trips instead of failing the whole deserialise; the
    /// client whitelist-checks it on load and falls back to the
    /// default if it doesn't recognise it.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// Message-body font size in px, or 0 for "use the theme's own".
    /// 0 doubles as the serde default for a config written before this
    /// field existed, which is exactly the behaviour we want there.
    #[serde(default)]
    pub text_size_px: f64,
    /// List-row scale multiplier. 0 = never set; client falls back to 1.
    #[serde(default)]
    pub list_density: f64,

    /// Crash reporting (Sentry) opt-out. Defaults to `true` (opt-out
    /// posture). Existing 0.6.5 configs without this field deserialize
    /// to the default via #[serde(default = "default_true")], so users
    /// upgrading get auto-opted-in.
    #[serde(default = "default_true")]
    pub crash_reporting_enabled: bool,
    /// Anonymous per-install identifier (UUID v4). Generated once on
    /// first boot of a Sentry-enabled build and persisted forever.
    /// Lets Sentry group multiple crashes from the same install without
    /// identifying the user.
    #[serde(default)]
    pub crash_reporting_install_id: Option<String>,
    /// Whether the first-launch disclosure banner has been shown.
    /// Flips true on dismiss; never reverted. Independent of `_enabled`
    /// (have we told the user? vs. is it on?).
    #[serde(default)]
    pub crash_reporting_consent_shown: bool,

    /// Unfurl links in messages into preview cards (title, description,
    /// thumbnail fetched by the client). Default on; a config written
    /// before the field existed deserialises to on.
    #[serde(default = "default_true")]
    pub link_previews_enabled: bool,

    /// GIF search with the provider's content filter switched off
    /// (explicit content included). Default off = the provider's
    /// `low` filter.
    #[serde(default)]
    pub gif_unfiltered: bool,

    /// Auto-away: minutes without input before the user shows as Away
    /// (also on screen lock / sleep). 0 = never. Default 10.
    #[serde(default = "default_away_after_minutes")]
    pub away_after_minutes: u32,

    /// Folder attachments download into (Transfers panel). Empty = the
    /// OS Downloads folder.
    #[serde(default)]
    pub download_dir: String,
    /// Show a save dialog for every download instead of using
    /// `download_dir`. Default off.
    #[serde(default)]
    pub ask_download_location: bool,

    /// Global hotkeys (Settings → Keybinds; see hotkeys/mod.rs). Empty by
    /// default — nothing is bound out of the box.
    #[serde(default)]
    pub hotkeys: Vec<HotkeyBindingSetting>,
    /// "voice_activity" (default) or "push_to_talk".
    #[serde(default)]
    pub input_mode: Option<String>,
    /// How long push-to-talk keeps transmitting after release, in ms.
    /// None = the default (100).
    pub ptt_release_delay_ms: Option<u32>,
}

/// One persisted keybind. `action` stays a string so a config written by
/// a newer build with an action this one doesn't know still loads (the
/// binding is skipped, not the whole settings file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBindingSetting {
    pub id: String,
    pub action: String,
    pub keys: Vec<String>,
}

/// The defaults are the serde ones. A derived `Default` would zero every
/// field instead — `default_true` toggles off, the theme "" — and that is
/// exactly what a fresh install got: `load()` hands out `AppSettings::default()`
/// when there is no config file yet, and the login path writes it to disk.
impl Default for AppSettings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("AppSettings deserialises from an empty object")
    }
}

fn default_true() -> bool {
    true
}

fn default_away_after_minutes() -> u32 {
    10
}

fn default_theme() -> String {
    "nocturne".to_string()
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Never print the password (a stray `{:?}` in a log line).
impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// On-disk config format
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    /// The pre-S10 saved sign-in: base64(nonce ‖ AES-256-GCM ciphertext)
    /// under `derive_key()`. Read once and re-sealed as `saved_login`;
    /// kept as is only while it can't be opened here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    credentials: Option<String>,
    /// The saved sign-in, sealed (see the module doc).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    saved_login: Option<SealedLogin>,
    /// This install's device id (multi-device sign-in, `device_id`). Kept
    /// here rather than in `settings`, which the renderer writes back whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
    #[serde(default)]
    settings: AppSettings,
}

const SEALED_LOGIN_VERSION: u32 = 2;
const SEALED_LOGIN_AAD: &[u8] = b"decibell saved login v2";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SealedLogin {
    v: u32,
    /// Which key sealed it: "keychain" or "derived" (`KeySource`).
    key: String,
    nonce: String,
    ct: String,
}

/// What native's own code gets back. Not serialisable on purpose: the
/// renderer gets `PublicConfig`, which has no password in it.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub credentials: Option<Credentials>,
    pub settings: AppSettings,
}

/// `load_config`'s answer to the renderer.
#[derive(Debug, Clone, Serialize)]
pub struct PublicConfig {
    /// The account a saved sign-in is for; the renderer calls
    /// `auto_login` when this is set.
    pub saved_username: Option<String>,
    pub settings: AppSettings,
}

impl LoadedConfig {
    pub fn public(self) -> PublicConfig {
        PublicConfig {
            saved_username: self.credentials.map(|c| c.username),
            settings: self.settings,
        }
    }
}

fn config_path() -> PathBuf {
    crate::state::boot().user_data_dir.join("config.json")
}

/// Also the fallback at-rest key for the E2EE key store (e2ee/keystore.rs)
/// when Electron's safeStorage isn't available — and for the saved sign-in
/// in that case. Any local process can compute it.
pub(crate) fn derive_key() -> [u8; 32] {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown-host".to_string());
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "unknown-user".to_string());

    let mut hasher = Sha256::new();
    hasher.update(SALT);
    hasher.update(hostname.as_bytes());
    hasher.update(user.as_bytes());
    hasher.finalize().into()
}

/// Set from `init`: the at-rest key Electron handed over is wrapped by a
/// real OS keychain (not Linux `basic_text`). Only then does the saved
/// sign-in use it.
static LOCAL_KEY_PROTECTED: AtomicBool = AtomicBool::new(false);

pub fn set_local_key_protected(protected: bool) {
    LOCAL_KEY_PROTECTED.store(protected, Ordering::SeqCst);
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum KeySource {
    /// Derived from the keychain-wrapped at-rest key.
    Keychain,
    /// `derive_key()`: hostname + user, no protection.
    Derived,
}

impl KeySource {
    fn label(self) -> &'static str {
        match self {
            KeySource::Keychain => "keychain",
            KeySource::Derived => "derived",
        }
    }

    fn parse(label: &str) -> Option<Self> {
        match label {
            "keychain" => Some(KeySource::Keychain),
            "derived" => Some(KeySource::Derived),
            _ => None,
        }
    }
}

/// The keys a saved sign-in may be sealed under on this machine now.
struct CredKeys {
    keychain: Option<[u8; 32]>,
    derived: [u8; 32],
}

impl CredKeys {
    fn current() -> Self {
        let keychain = if LOCAL_KEY_PROTECTED.load(Ordering::SeqCst) {
            crate::state::boot().e2ee_local_key.as_ref().map(keychain_subkey)
        } else {
            None
        };
        if keychain.is_none() {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                log::warn!(
                    "[config] no OS keychain protection (safeStorage unavailable or Linux basic_text): \
                     the saved sign-in falls back to the machine-derived key"
                )
            });
        }
        CredKeys { keychain, derived: derive_key() }
    }

    /// What a new seal uses: the keychain key whenever there is one.
    fn preferred(&self) -> (KeySource, &[u8; 32]) {
        match &self.keychain {
            Some(k) => (KeySource::Keychain, k),
            None => (KeySource::Derived, &self.derived),
        }
    }

    fn get(&self, source: KeySource) -> Option<&[u8; 32]> {
        match source {
            KeySource::Keychain => self.keychain.as_ref(),
            KeySource::Derived => Some(&self.derived),
        }
    }
}

/// The saved sign-in's own key, separate from the key store's use of the
/// same at-rest key.
fn keychain_subkey(local: &[u8; 32]) -> [u8; 32] {
    use ring::hkdf;
    struct Len32;
    impl hkdf::KeyType for Len32 {
        fn len(&self) -> usize {
            32
        }
    }
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, b"decibell-config-v2").extract(local);
    let mut out = [0u8; 32];
    prk.expand(&[b"saved sign-in"], Len32)
        .and_then(|okm| okm.fill(&mut out))
        .expect("HKDF-SHA256 expands to 32 bytes");
    out
}

fn login_aad(source: KeySource) -> Vec<u8> {
    let mut aad = SEALED_LOGIN_AAD.to_vec();
    aad.push(0);
    aad.extend_from_slice(source.label().as_bytes());
    aad
}

fn seal_login(creds: &Credentials, keys: &CredKeys) -> Result<SealedLogin, String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let (source, key) = keys.preferred();
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("Cipher init failed: {}", e))?;
    let mut nonce = [0u8; 12];
    SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| "No randomness for the nonce".to_string())?;
    let json = serde_json::to_vec(creds).map_err(|e| format!("Serialize failed: {}", e))?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &json, aad: &login_aad(source) })
        .map_err(|e| format!("Encrypt failed: {}", e))?;
    let b64 = base64::engine::general_purpose::STANDARD;
    Ok(SealedLogin {
        v: SEALED_LOGIN_VERSION,
        key: source.label().to_string(),
        nonce: b64.encode(nonce),
        ct: b64.encode(ct),
    })
}

fn open_login(sealed: &SealedLogin, keys: &CredKeys) -> Result<Credentials, String> {
    if sealed.v != SEALED_LOGIN_VERSION {
        return Err(format!("unknown saved sign-in version {}", sealed.v));
    }
    let source = KeySource::parse(&sealed.key)
        .ok_or_else(|| format!("unknown saved sign-in key {:?}", sealed.key))?;
    let key = keys
        .get(source)
        .ok_or("sealed under the keychain key, which isn't available now")?;
    let b64 = base64::engine::general_purpose::STANDARD;
    let nonce = b64.decode(&sealed.nonce).map_err(|e| e.to_string())?;
    let ct = b64.decode(&sealed.ct).map_err(|e| e.to_string())?;
    if nonce.len() != 12 {
        return Err("bad saved sign-in nonce".into());
    }
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|e| format!("Cipher init failed: {}", e))?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &ct, aad: &login_aad(source) })
        .map_err(|_| "saved sign-in doesn't open with this machine's key".to_string())?;
    serde_json::from_slice(&plain).map_err(|e| format!("Deserialize failed: {}", e))
}

/// The pre-S10 format: base64(nonce ‖ ciphertext) under `derive_key()`.
fn open_legacy(encrypted: &str, derived: &[u8; 32]) -> Result<Credentials, String> {
    let cipher =
        Aes256Gcm::new_from_slice(derived).map_err(|e| format!("Cipher init failed: {}", e))?;
    let combined = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;
    if combined.len() < 12 {
        return Err("Encrypted data too short".to_string());
    }
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let plaintext = cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| "Decryption failed (wrong machine or corrupted config)".to_string())?;
    serde_json::from_slice(&plaintext).map_err(|e| format!("Deserialize failed: {}", e))
}

/// The saved sign-in in `file`, and whether it should be sealed again
/// (old format, or the derived key while the keychain key is available).
fn resolve_credentials(file: &ConfigFile, keys: &CredKeys) -> (Option<Credentials>, bool) {
    if let Some(sealed) = &file.saved_login {
        return match open_login(sealed, keys) {
            Ok(creds) => {
                let upgrade = sealed.key == KeySource::Derived.label() && keys.keychain.is_some();
                (Some(creds), upgrade)
            }
            Err(e) => {
                log::warn!("[config] saved sign-in unavailable: {}", e);
                (None, false)
            }
        };
    }
    if let Some(legacy) = &file.credentials {
        match open_legacy(legacy, &keys.derived) {
            Ok(creds) => return (Some(creds), true),
            Err(e) => log::warn!("[config] old-format saved sign-in unavailable: {}", e),
        }
    }
    (None, false)
}

/// Serializes every writer, so two concurrent saves can't interleave
/// their read-preserve-write sequences (e.g. login persisting credentials
/// while a settings save races the read of the old file).
static SAVE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn read_file(path: &Path) -> Result<Option<ConfigFile>, String> {
    let data = match std::fs::read_to_string(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Failed to read config: {}", e)),
    };
    serde_json::from_str(&data)
        .map(Some)
        .map_err(|e| format!("Failed to parse config: {}", e))
}

fn write_file(path: &Path, config: &ConfigFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    write_atomic(path, json.as_bytes())
}

pub fn load() -> Result<LoadedConfig, String> {
    load_at(&config_path(), &CredKeys::current())
}

fn load_at(path: &Path, keys: &CredKeys) -> Result<LoadedConfig, String> {
    let Some(config) = read_file(path)? else {
        return Ok(LoadedConfig {
            credentials: None,
            settings: AppSettings::default(),
        });
    };
    let (credentials, reseal) = resolve_credentials(&config, keys);
    if reseal {
        if let Err(e) = reseal_at(path, keys) {
            log::warn!("[config] couldn't re-seal the saved sign-in: {}", e);
        }
    }
    Ok(LoadedConfig {
        credentials,
        settings: config.settings,
    })
}

/// Move a saved sign-in onto the preferred format (once per upgrade).
/// Re-reads under the lock: whatever is on disk now is what gets sealed.
fn reseal_at(path: &Path, keys: &CredKeys) -> Result<(), String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut config) = read_file(path)? else { return Ok(()) };
    let (Some(creds), true) = resolve_credentials(&config, keys) else { return Ok(()) };
    config.saved_login = Some(seal_login(&creds, keys)?);
    config.credentials = None;
    write_file(path, &config)?;
    log::info!("[config] saved sign-in re-sealed under the {} key", keys.preferred().0.label());
    Ok(())
}

pub fn save(credentials: Option<&Credentials>, settings: &AppSettings) -> Result<(), String> {
    save_at(&config_path(), &CredKeys::current(), credentials, settings)
}

fn save_at(
    path: &Path,
    keys: &CredKeys,
    credentials: Option<&Credentials>,
    settings: &AppSettings,
) -> Result<(), String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let existing = read_file(path).ok().flatten().unwrap_or_default();
    let config = match credentials {
        Some(creds) => ConfigFile {
            credentials: None,
            saved_login: Some(seal_login(creds, keys)?),
            device_id: existing.device_id,
            settings: settings.clone(),
        },
        // Only updating settings: keep whatever sign-in is saved, as is.
        None => ConfigFile {
            credentials: existing.credentials,
            saved_login: existing.saved_login,
            device_id: existing.device_id,
            settings: settings.clone(),
        },
    };
    write_file(path, &config)
}

// ── Device id (docs/superpowers/specs/2026-10-18-multi-device-design.md) ──

/// This install's id for central (`LoginRequest.device_id`), made once.
static DEVICE_ID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// 128 random bits, hex.
fn random_device_id() -> String {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0u8; 16];
    if SystemRandom::new().fill(&mut bytes).is_err() {
        // No randomness: a time-based id still tells this install apart.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes = nanos.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The id stored in `path`, or a new one written there.
fn device_id_at(path: &Path) -> Result<String, String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut config = read_file(path)?.unwrap_or_default();
    if let Some(id) = config.device_id.as_ref().filter(|id| !id.is_empty() && id.len() <= 64) {
        return Ok(id.clone());
    }
    let id = random_device_id();
    config.device_id = Some(id.clone());
    write_file(path, &config)?;
    Ok(id)
}

/// Replace the id stored in `path` with a new one.
fn renew_device_id_at(path: &Path) -> Result<String, String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut config = read_file(path)?.unwrap_or_default();
    let id = random_device_id();
    config.device_id = Some(id.clone());
    write_file(path, &config)?;
    Ok(id)
}

/// This install's device id: read once, made on first use. A config that
/// can't be read or written still gets one for this run (signing in then
/// counts as a new device after a restart, which is harmless).
pub fn device_id() -> String {
    let mut cached = DEVICE_ID.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(id) = cached.as_ref() {
        return id.clone();
    }
    let id = device_id_at(&config_path()).unwrap_or_else(|e| {
        log::warn!("[config] device id not stored: {e}");
        random_device_id()
    });
    *cached = Some(id.clone());
    id
}

/// Central closed our connection because another connection signed in with
/// our device id while we were connected (SESSION_CLOSED REPLACED): this
/// install was copied. Take a new id, so the two copies are two devices.
pub fn renew_device_id() -> String {
    let id = renew_device_id_at(&config_path()).unwrap_or_else(|e| {
        log::warn!("[config] new device id not stored: {e}");
        random_device_id()
    });
    *DEVICE_ID.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
    id
}

/// Write-to-temp-then-rename so a crash mid-write can never tear the
/// config file (which would lose saved credentials AND settings). The
/// rename is atomic on every platform we ship as long as the temp file
/// lives in the same directory. Created owner-only (0600) on unix; on
/// Windows the file inherits the per-user profile ACL.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    // A temp file left by a crash may have the old 0644 mode: start over
    // so the new one is created with ours.
    let _ = std::fs::remove_file(&tmp);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts
        .open(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|_| f.sync_all()));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Failed to write config: {}", e));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Failed to replace config: {}", e)
    })?;
    Ok(())
}

pub fn clear_credentials() -> Result<(), String> {
    clear_at(&config_path())
}

fn clear_at(path: &Path) -> Result<(), String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut config = match read_file(path) {
        Ok(Some(c)) => c,
        Ok(None) => return Ok(()),
        // Unreadable: replace it rather than leave a sign-in behind.
        Err(_) => ConfigFile::default(),
    };
    config.credentials = None;
    config.saved_login = None;
    write_file(path, &config)
}

/// Central refused this username + password outright: forget the saved
/// sign-in if it is that pair, so a changed password doesn't fail (and
/// count against the account's failed sign-ins) on every launch. A saved
/// pair that differs (a typo on the sign-in page) stays.
pub fn forget_credentials_if(username: &str, password: &str) -> Result<bool, String> {
    forget_if_at(&config_path(), &CredKeys::current(), username, password)
}

fn forget_if_at(path: &Path, keys: &CredKeys, username: &str, password: &str) -> Result<bool, String> {
    let _guard = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut config) = read_file(path)? else { return Ok(false) };
    match resolve_credentials(&config, keys).0 {
        Some(saved) if saved.username == username && saved.password == password => {
            config.credentials = None;
            config.saved_login = None;
            write_file(path, &config)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh install (no config file) gets `AppSettings::default()` from
    /// `load()`, so it has to be the serde defaults, not zeroed fields.
    #[test]
    fn default_settings_are_the_serde_defaults() {
        let d = AppSettings::default();
        assert!(d.agc_enabled);
        assert!(d.use_av1 && d.use_h265);
        assert!(d.crash_reporting_enabled);
        assert!(d.link_previews_enabled);
        assert_eq!(d.away_after_minutes, 10);
        assert_eq!(d.theme, "nocturne");
        assert!(!d.aec_enabled);
        assert!(d.input_device.is_none());
    }

    /// A config written before AGC defaulted on keeps what it says; one
    /// without the field gets the new default.
    #[test]
    fn agc_default_respects_an_existing_value() {
        let s: AppSettings = serde_json::from_str(r#"{"agc_enabled": false}"#).unwrap();
        assert!(!s.agc_enabled);
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(s.agc_enabled);
    }

    // ── Saved sign-in (review S10) ──────────────────────────────────

    fn keys(keychain: bool) -> CredKeys {
        CredKeys {
            keychain: keychain.then(|| keychain_subkey(&[7u8; 32])),
            derived: [9u8; 32],
        }
    }

    fn creds() -> Credentials {
        Credentials { username: "alice".into(), password: "correct horse".into() }
    }

    /// A config file per test, in a fresh directory.
    fn tmp_config(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("decibell-config-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.json")
    }

    /// What the pre-S10 client wrote: derived key, nonce prepended.
    fn legacy_blob(c: &Credentials, derived: &[u8; 32]) -> String {
        let cipher = Aes256Gcm::new_from_slice(derived).unwrap();
        let nonce = [5u8; 12];
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), serde_json::to_vec(c).unwrap().as_ref()).unwrap();
        let mut combined = nonce.to_vec();
        combined.extend_from_slice(&ct);
        base64::engine::general_purpose::STANDARD.encode(combined)
    }

    #[test]
    fn sealed_login_round_trips_under_either_key() {
        for keychain in [true, false] {
            let k = keys(keychain);
            let sealed = seal_login(&creds(), &k).unwrap();
            assert_eq!(sealed.key, if keychain { "keychain" } else { "derived" });
            assert_eq!(open_login(&sealed, &k).unwrap(), creds());
        }
    }

    #[test]
    fn every_seal_takes_a_fresh_random_nonce() {
        let k = keys(true);
        let a = seal_login(&creds(), &k).unwrap();
        let b = seal_login(&creds(), &k).unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ct, b.ct);
    }

    #[test]
    fn sealed_login_refuses_the_wrong_key_and_tampering() {
        let sealed = seal_login(&creds(), &keys(true)).unwrap();
        // The keychain key is gone (keyring reset, other machine).
        assert!(open_login(&sealed, &keys(false)).is_err());
        // Relabelled as derived: the label is bound in the AAD.
        let relabelled = SealedLogin { key: "derived".into(), ..sealed.clone() };
        assert!(open_login(&relabelled, &keys(true)).is_err());
        let other = CredKeys { keychain: Some(keychain_subkey(&[8u8; 32])), derived: [9u8; 32] };
        assert!(open_login(&sealed, &other).is_err());
    }

    #[test]
    fn debug_never_prints_the_password() {
        assert!(!format!("{:?}", creds()).contains("correct horse"));
    }

    #[test]
    fn old_format_file_migrates_once_and_keeps_settings() {
        let path = tmp_config("migrate");
        let k = keys(true);
        let old = format!(
            r#"{{"credentials": "{}", "settings": {{"theme": "matinee"}}}}"#,
            legacy_blob(&creds(), &k.derived)
        );
        std::fs::write(&path, old).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }

        let loaded = load_at(&path, &k).unwrap();
        assert_eq!(loaded.credentials, Some(creds()));
        assert_eq!(loaded.settings.theme, "matinee");

        let file: ConfigFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(file.credentials.is_none(), "old field dropped");
        assert_eq!(file.saved_login.as_ref().unwrap().key, "keychain");
        assert_eq!(file.settings.theme, "matinee");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // Second load: same sign-in, nothing left to migrate.
        let before = file.saved_login.clone();
        assert_eq!(load_at(&path, &k).unwrap().credentials, Some(creds()));
        let file: ConfigFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.saved_login, before);
    }

    #[test]
    fn derived_seal_upgrades_once_a_keychain_appears() {
        let path = tmp_config("upgrade");
        save_at(&path, &keys(false), Some(&creds()), &AppSettings::default()).unwrap();
        let file: ConfigFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.saved_login.unwrap().key, "derived");
        assert_eq!(load_at(&path, &keys(true)).unwrap().credentials, Some(creds()));
        let file: ConfigFile = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.saved_login.unwrap().key, "keychain");
    }

    #[test]
    fn settings_saves_keep_the_sign_in_and_an_unopenable_one_is_kept_too() {
        let path = tmp_config("preserve");
        save_at(&path, &keys(true), Some(&creds()), &AppSettings::default()).unwrap();
        let mut s = AppSettings::default();
        s.theme = "graphite".into();
        save_at(&path, &keys(true), None, &s).unwrap();
        let loaded = load_at(&path, &keys(true)).unwrap();
        assert_eq!(loaded.credentials, Some(creds()));
        assert_eq!(loaded.settings.theme, "graphite");
        // Keychain unavailable this launch: no sign-in, but nothing lost.
        assert_eq!(load_at(&path, &keys(false)).unwrap().credentials, None);
        save_at(&path, &keys(false), None, &s).unwrap();
        assert_eq!(load_at(&path, &keys(true)).unwrap().credentials, Some(creds()));
    }

    #[test]
    fn a_refused_password_is_forgotten_only_if_it_is_the_saved_one() {
        let path = tmp_config("forget");
        let k = keys(true);
        save_at(&path, &k, Some(&creds()), &AppSettings::default()).unwrap();
        assert!(!forget_if_at(&path, &k, "alice", "typo").unwrap());
        assert_eq!(load_at(&path, &k).unwrap().credentials, Some(creds()));
        assert!(forget_if_at(&path, &k, "alice", "correct horse").unwrap());
        assert_eq!(load_at(&path, &k).unwrap().credentials, None);
        clear_at(&path).unwrap();
        assert!(load_at(&path, &k).unwrap().settings.agc_enabled);
    }

    #[test]
    fn the_device_id_is_made_once_and_survives_every_writer() {
        let path = tmp_config("device-id");
        let k = keys(true);
        let id = device_id_at(&path).unwrap();
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(device_id_at(&path).unwrap(), id, "read back, not made again");
        // Saving a sign-in, saving settings, forgetting and clearing the
        // sign-in all keep it.
        save_at(&path, &k, Some(&creds()), &AppSettings::default()).unwrap();
        save_at(&path, &k, None, &AppSettings::default()).unwrap();
        forget_if_at(&path, &k, "alice", "correct horse").unwrap();
        clear_at(&path).unwrap();
        assert_eq!(device_id_at(&path).unwrap(), id);
        // A copied install renews it.
        let renewed = renew_device_id_at(&path).unwrap();
        assert_ne!(renewed, id);
        assert_eq!(device_id_at(&path).unwrap(), renewed);
        // A settings save from the renderer keeps it too.
        let mut s = AppSettings::default();
        s.theme = "graphite".into();
        save_at(&path, &k, None, &s).unwrap();
        assert_eq!(device_id_at(&path).unwrap(), renewed);
    }

    #[test]
    fn the_renderer_view_has_no_password() {
        let v = serde_json::to_value(
            LoadedConfig { credentials: Some(creds()), settings: AppSettings::default() }.public(),
        )
        .unwrap();
        assert_eq!(v["saved_username"], "alice");
        assert!(!v.to_string().contains("correct horse"));
    }
}

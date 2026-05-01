use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const SALT: &[u8] = b"decibell-config-v1";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AppSettings {
    #[serde(default)]
    pub friends_only_dms: bool,
    #[serde(default)]
    pub stream_stereo: bool,
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
    /// Automatic gain control (AGC2) — normalizes mic volume
    #[serde(default)]
    pub agc_enabled: bool,
    /// Per-user volume in dB (username → dB). 0 = default, negative = quieter, positive = louder.
    #[serde(default)]
    pub user_volumes: std::collections::HashMap<String, f64>,
    /// Users locally muted by this client
    #[serde(default)]
    pub local_muted_users: Vec<String>,
    /// Attachment upload cap in bytes per second. 0 = unlimited.
    #[serde(default)]
    pub upload_limit_bps: u64,
    /// Attachment download cap in bytes per second. 0 = unlimited.
    #[serde(default)]
    pub download_limit_bps: u64,
    /// How many recently-visited channels keep their messages, scroll
    /// position, and history flags cached in RAM. The rest is dropped.
    /// 0 means "use the client default" (currently 10) — that lets a
    /// migration from older configs land on a sensible value rather
    /// than evicting everything.
    #[serde(default)]
    pub channel_cache_size: u32,
    /// Persisted volume (0.0–1.0) for the chat audio-attachment player.
    /// None = never set; client falls back to 1.0.
    pub media_audio_volume: Option<f64>,
    #[serde(default)]
    pub media_audio_muted: bool,
    /// Persisted volume (0.0–1.0) for the chat video-attachment player.
    pub media_video_volume: Option<f64>,
    #[serde(default)]
    pub media_video_muted: bool,

    /// Codec preference toggles (spec §7.1). When false, that codec is
    /// removed from the encode list before advertisement to peers, so
    /// the streamer never auto-picks it. Both default true; the
    /// Settings → Codecs panel grays the toggle out when the local
    /// hardware does not support that codec at all.
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
    /// VideoCodec enum byte (0=UNKNOWN/Auto, 1=H264_HW, 2=H264_SW,
    /// 3=H265, 4=AV1). Restored on load; downgraded to 0 by the client
    /// at runtime if the saved codec isn't in the user's encodeCaps.
    pub stream_enforced_codec: Option<u8>,
}

fn default_true() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// On-disk config format
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    /// AES-256-GCM encrypted + base64-encoded credentials JSON
    credentials: Option<String>,
    #[serde(default)]
    settings: AppSettings,
}

/// Returned to the frontend on load
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedConfig {
    pub credentials: Option<Credentials>,
    pub settings: AppSettings,
}

fn config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    Ok(dir.join("config.json"))
}

fn derive_key() -> [u8; 32] {
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

fn encrypt_credentials(creds: &Credentials) -> Result<String, String> {
    let key = derive_key();
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Cipher init failed: {}", e))?;

    let json = serde_json::to_vec(creds).map_err(|e| format!("Serialize failed: {}", e))?;

    let nonce_bytes: [u8; 12] = rand_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, json.as_ref())
        .map_err(|e| format!("Encrypt failed: {}", e))?;

    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(base64::engine::general_purpose::STANDARD.encode(&combined))
}

fn decrypt_credentials(encrypted: &str) -> Result<Credentials, String> {
    let key = derive_key();
    let cipher =
        Aes256Gcm::new_from_slice(&key).map_err(|e| format!("Cipher init failed: {}", e))?;

    let combined = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|e| format!("Base64 decode failed: {}", e))?;

    if combined.len() < 12 {
        return Err("Encrypted data too short".to_string());
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| "Decryption failed (wrong machine or corrupted config)".to_string())?;

    serde_json::from_slice(&plaintext).map_err(|e| format!("Deserialize failed: {}", e))
}

fn rand_nonce() -> [u8; 12] {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut hasher = Sha256::new();
    hasher.update(seed.to_le_bytes());
    hasher.update(std::process::id().to_le_bytes());
    let hash = hasher.finalize();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&hash[..12]);
    nonce
}

pub fn load(app: &AppHandle) -> Result<LoadedConfig, String> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(LoadedConfig {
            credentials: None,
            settings: AppSettings::default(),
        });
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config: {}", e))?;

    let config: ConfigFile =
        serde_json::from_str(&data).map_err(|e| format!("Failed to parse config: {}", e))?;

    let credentials = config
        .credentials
        .as_deref()
        .and_then(|enc| decrypt_credentials(enc).ok());

    Ok(LoadedConfig {
        credentials,
        settings: config.settings,
    })
}

pub fn save(app: &AppHandle, credentials: Option<&Credentials>, settings: &AppSettings) -> Result<(), String> {
    let path = config_path(app)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config dir: {}", e))?;
    }

    let encrypted_creds = match credentials {
        Some(creds) => Some(encrypt_credentials(creds)?),
        None => {
            // Preserve existing encrypted credentials if we're only updating settings
            if path.exists() {
                let data = std::fs::read_to_string(&path).ok();
                data.and_then(|d| serde_json::from_str::<ConfigFile>(&d).ok())
                    .and_then(|c| c.credentials)
            } else {
                None
            }
        }
    };

    let config = ConfigFile {
        credentials: encrypted_creds,
        settings: settings.clone(),
    };

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(&path, json).map_err(|e| format!("Failed to write config: {}", e))?;

    Ok(())
}

pub fn clear_credentials(app: &AppHandle) -> Result<(), String> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(());
    }

    let data = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config: {}", e))?;

    let mut config: ConfigFile =
        serde_json::from_str(&data).unwrap_or_default();

    config.credentials = None;

    let json = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    std::fs::write(&path, json).map_err(|e| format!("Failed to write config: {}", e))?;

    Ok(())
}

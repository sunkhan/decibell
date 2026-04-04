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
    /// Per-user volume in dB (username → dB). 0 = default, negative = quieter, positive = louder.
    #[serde(default)]
    pub user_volumes: std::collections::HashMap<String, f64>,
    /// Users locally muted by this client
    #[serde(default)]
    pub local_muted_users: Vec<String>,
}

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

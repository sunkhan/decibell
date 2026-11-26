//! The E2EE at-rest key: 32 random bytes that seal the key store on disk
//! (`decibell_core::e2ee::keystore`) and, through a derived subkey, the saved sign-in
//! (`decibell_core::config`).
//!
//! Electron keeps it in `<userData>/e2ee/local.key`, wrapped by `safeStorage`
//! (`electron-client/electron/main/e2eeLocalKey.ts`). Here it lives in the OS keychain
//! itself through GPUI's credentials API (Secret Service on Linux, Credential Manager on
//! Windows, Keychain on macOS). The rules are the Electron ones:
//!
//! - no keychain (no backend, or it answers with an error): no key. The core then seals
//!   with its machine-derived key, as it always did without `safeStorage`;
//! - a stored value that is not 32 bytes of base64: ignored, **not** replaced (whatever
//!   it sealed stays recoverable), no key;
//! - nothing stored yet: generate, store, use it. If storing fails: no key;
//! - `protected` is true exactly when the key came out of (or went into) the keychain.
//!   Electron's extra case, Linux `basic_text` (a key wrapped with a hard-coded password),
//!   has no twin here: GPUI has no such backend.
//!
//! **The saved sign-in depends on this**: with a protected key the core seals it under a
//! subkey of this key, else under the derived key, and re-seals once when a keychain
//! appears. A key that changes (another profile's, a wiped keychain) makes the saved
//! sign-in unreadable, which shows as the sign-in page, and locks the E2EE key store until
//! the passphrase is typed again. So one install keeps one key: the item is named after
//! the user data directory.

use std::path::Path;

use base64::Engine as _;
use gpui::{App, Task};

/// What `lifecycle::init` gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalKey {
    /// base64 of the 32 bytes; `None` = the core's derived key is used.
    pub key_b64: Option<String>,
    /// The key is guarded by a real OS keychain.
    pub protected: bool,
}

impl LocalKey {
    pub fn none() -> LocalKey {
        LocalKey::default()
    }
}

/// Where the at-rest key comes from. The app uses [`KeychainKey`]; tests and the headless
/// tool inject a [`FixedKey`].
pub trait LocalKeyProvider: 'static {
    fn load(&self, user_data_dir: &Path, cx: &mut App) -> Task<LocalKey>;
}

/// A key decided by the caller (`LocalKey::none()` for "no keychain on this machine").
pub struct FixedKey(pub LocalKey);

impl LocalKeyProvider for FixedKey {
    fn load(&self, _: &Path, _: &mut App) -> Task<LocalKey> {
        Task::ready(self.0.clone())
    }
}

/// A key kept in a file of the user data directory (`dev-local.key`): for development builds and
/// the self-tests, **which must never write to the desktop's keychain** (every throwaway profile
/// would leave an item in the developer's keyring; 2026-11-26). It behaves like a keychain that
/// works (`protected`), so the saved sign-in and the key store take the same path as in a shipped
/// build. Never for a shipped build: whatever can read the profile can read the file.
pub struct FileKey;

/// The file's name inside the user data directory.
pub const DEV_KEY_FILE: &str = "dev-local.key";

impl LocalKeyProvider for FileKey {
    fn load(&self, user_data_dir: &Path, _: &mut App) -> Task<LocalKey> {
        Task::ready(file_key(user_data_dir))
    }
}

fn file_key(user_data_dir: &Path) -> LocalKey {
    let path = user_data_dir.join(DEV_KEY_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) if is_valid(text.trim()) => LocalKey { key_b64: Some(text.trim().to_string()), protected: true },
        // As with the keychain: a value of another form is ignored, not replaced.
        Ok(_) => {
            log::warn!("[e2ee] {} has an unexpected form; ignoring it", path.display());
            LocalKey::none()
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let Some(key) = generate() else { return LocalKey::none() };
            let stored = std::fs::create_dir_all(user_data_dir).and_then(|()| std::fs::write(&path, &key));
            match stored {
                Ok(()) => LocalKey { key_b64: Some(key), protected: true },
                Err(e) => {
                    log::warn!("[e2ee] couldn't store the development key in {}: {e}", path.display());
                    LocalKey::none()
                }
            }
        }
        Err(e) => {
            log::warn!("[e2ee] couldn't read {}: {e}", path.display());
            LocalKey::none()
        }
    }
}

/// The OS keychain, through GPUI's credentials API.
pub struct KeychainKey;

/// The account name stored beside the secret (GPUI's API wants one).
const KEYCHAIN_USER: &str = "decibell";

fn keychain_url(user_data_dir: &Path) -> String {
    format!("decibell://e2ee-local-key/{}", user_data_dir.display())
}

fn is_valid(key_b64: &str) -> bool {
    base64::engine::general_purpose::STANDARD
        .decode(key_b64)
        .is_ok_and(|bytes| bytes.len() == 32)
}

fn generate() -> Option<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).ok()?;
    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
}

impl LocalKeyProvider for KeychainKey {
    fn load(&self, user_data_dir: &Path, cx: &mut App) -> Task<LocalKey> {
        let url = keychain_url(user_data_dir);
        let read = cx.read_credentials(&url);
        cx.spawn(async move |cx| {
            match read.await {
                Ok(Some((_, secret))) => match String::from_utf8(secret) {
                    Ok(key) if is_valid(&key) => LocalKey {
                        key_b64: Some(key),
                        protected: true,
                    },
                    _ => {
                        log::warn!("[e2ee] the keychain's local key has an unexpected form; ignoring it");
                        LocalKey::none()
                    }
                },
                Ok(None) => {
                    let Some(key) = generate() else {
                        return LocalKey::none();
                    };
                    let write =
                        cx.update(|cx| cx.write_credentials(&url, KEYCHAIN_USER, key.as_bytes()));
                    match write.await {
                        Ok(()) => LocalKey {
                            key_b64: Some(key),
                            protected: true,
                        },
                        Err(e) => {
                            log::warn!("[e2ee] couldn't store the local key in the keychain: {e:#}");
                            LocalKey::none()
                        }
                    }
                }
                Err(e) => {
                    log::warn!("[e2ee] no keychain: {e:#}");
                    LocalKey::none()
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_key_is_created_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let first = file_key(&profile);
        assert!(first.protected && first.key_b64.as_deref().is_some_and(is_valid));
        assert_eq!(file_key(&profile), first, "a second start reads the same key");
        // A file of another form is left alone and gives no key.
        std::fs::write(profile.join(DEV_KEY_FILE), "not a key").unwrap();
        assert_eq!(file_key(&profile), LocalKey::none());
        assert_eq!(std::fs::read_to_string(profile.join(DEV_KEY_FILE)).unwrap(), "not a key");
    }

    #[test]
    fn generated_keys_are_32_bytes() {
        let key = generate().unwrap();
        assert!(is_valid(&key));
        assert_ne!(key, generate().unwrap());
        assert!(!is_valid("not base64 !"));
        assert!(!is_valid(&base64::engine::general_purpose::STANDARD.encode([0u8; 31])));
    }

    #[test]
    fn one_item_per_user_data_dir() {
        assert_ne!(
            keychain_url(Path::new("/home/a/.config/decibell")),
            keychain_url(Path::new("/tmp/profile-b"))
        );
    }
}

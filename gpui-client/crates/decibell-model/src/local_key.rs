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
        // GPUI's Secret Service client can't run beside the core before
        // `core::prepare` (see `core`'s module doc): it would take the app down.
        if !crate::core::gpui_dbus_usable() {
            log::warn!("[e2ee] core::prepare was not called before the Application was created: no keychain");
            return Task::ready(LocalKey::none());
        }
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

//! The update feed: one JSON file the release workflow writes and (when the owner has a key)
//! signs, published on a fixed release so the client never needs GitHub's API
//! (gpui-client/PACKAGING.md, "Updates"):
//!
//! ```text
//! https://github.com/<repo>/releases/download/<channel>/decibell-native.json       the feed
//! https://github.com/<repo>/releases/download/<channel>/decibell-native.json.sig   base64 Ed25519 signature
//! {
//!   "schema": 1,
//!   "version": "0.9.5",
//!   "release_page": "https://github.com/<repo>/releases/tag/native-v0.9.5",
//!   "files": {
//!     "linux-x86_64-appimage": { "url": "https://github.com/<repo>/releases/download/native-v0.9.5/…AppImage",
//!                                "size": 123, "sha512": "<base64>" },
//!     "windows-x86_64-nsis":   { … }
//!   }
//! }
//! ```
//!
//! The signature covers the file's exact bytes. electron-updater's `latest*.yml` carries the same
//! SHA-512 (base64); the signature is what it lacks.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use url::Url;

use crate::version::Version;

pub const FEED_FILE: &str = "decibell-native.json";
/// The largest feed accepted.
pub const FEED_CAP: usize = 64 * 1024;
/// The largest download accepted.
pub const ARTIFACT_CAP: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Deserialize)]
pub struct Feed {
    pub schema: u32,
    pub version: String,
    #[serde(default)]
    pub release_page: Option<String>,
    pub files: BTreeMap<String, Artifact>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Artifact {
    pub url: String,
    pub size: u64,
    pub sha512: String,
}

/// The feed's entry for this machine's self-updating package.
pub fn artifact_key() -> &'static str {
    if cfg!(windows) { "windows-x86_64-nsis" } else { "linux-x86_64-appimage" }
}

/// The baked public key, if the build has one.
pub fn baked_key() -> Option<[u8; 32]> {
    #[cfg(feature = "test-hooks")]
    if let Ok(value) = std::env::var("DECIBELL_TEST_UPDATE_PUBLIC_KEY") {
        return decode_key(&value);
    }
    decode_key(env!("DECIBELL_UPDATE_PUBLIC_KEY_B64"))
}

pub fn decode_key(b64: &str) -> Option<[u8; 32]> {
    let bytes = STANDARD.decode(b64.trim()).ok()?;
    bytes.try_into().ok()
}

/// Checks the signature when a key is given; without a key there is nothing to check.
pub fn verify(feed: &[u8], signature: Option<&[u8]>, key: Option<&[u8; 32]>) -> Result<(), String> {
    let Some(key) = key else { return Ok(()) };
    let signature = signature.ok_or("the update feed is not signed")?;
    let text = std::str::from_utf8(signature).map_err(|_| "the update feed's signature is not text")?;
    let raw = STANDARD.decode(text.trim()).map_err(|_| "the update feed's signature is not base64")?;
    let raw: [u8; 64] = raw.try_into().map_err(|_| "the update feed's signature has the wrong length")?;
    let key = VerifyingKey::from_bytes(key).map_err(|_| "this build's update key is invalid")?;
    key.verify_strict(feed, &Signature::from_bytes(&raw))
        .map_err(|_| "the update feed's signature doesn't match".to_string())
}

/// A feed's version and this machine's artifact, checked: schema 1, a version, an HTTPS URL on one
/// of `hosts` under `/<repo>/releases/download/`, a size within the cap, a 64-byte SHA-512.
pub fn parse(bytes: &[u8], repo: &str, hosts: &[String]) -> Result<(Feed, Version, Option<Artifact>), String> {
    let feed: Feed =
        serde_json::from_slice(crate::install::strip_bom(bytes)).map_err(|e| format!("the update feed is malformed ({e})"))?;
    if feed.schema != 1 {
        return Err(format!("the update feed has schema {} (this version reads 1)", feed.schema));
    }
    let version = Version::parse(&feed.version).ok_or("the update feed's version is malformed")?;
    let artifact = match feed.files.get(artifact_key()) {
        None => None,
        Some(artifact) => {
            check_url(&artifact.url, repo, hosts)?;
            if artifact.size == 0 || artifact.size > ARTIFACT_CAP {
                return Err("the update's size is out of range".into());
            }
            sha512(artifact)?;
            Some(artifact.clone())
        }
    };
    Ok((feed, version, artifact))
}

/// The artifact's SHA-512 as bytes.
pub fn sha512(artifact: &Artifact) -> Result<[u8; 64], String> {
    STANDARD
        .decode(artifact.sha512.trim())
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| "the update's SHA-512 is malformed".to_string())
}

fn check_url(url: &str, repo: &str, hosts: &[String]) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|_| "the update's URL is malformed")?;
    let allowed = hosts.iter().any(|h| Some(h.as_str()) == parsed.host_str());
    let allowed = allowed && parsed.scheme() == "https";
    // The self-tests' fixture server.
    #[cfg(any(test, feature = "test-hooks"))]
    let allowed = allowed || (parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1"));
    if !allowed || !parsed.path().starts_with(&format!("/{repo}/releases/download/")) {
        return Err(format!("the update is not on {repo}'s releases"));
    }
    Ok(parsed)
}

/// The release page the notify-only mode opens: the feed's, if it is on the repository's releases,
/// else the repository's release list.
pub fn release_page(feed: &Feed, repo: &str) -> String {
    let fallback = format!("https://github.com/{repo}/releases");
    match feed.release_page.as_deref().and_then(|p| Url::parse(p).ok()) {
        Some(page)
            if page.scheme() == "https"
                && page.host_str() == Some("github.com")
                && page.path().starts_with(&format!("/{repo}/releases")) =>
        {
            page.to_string()
        }
        _ => fallback,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    pub(crate) fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    pub(crate) fn sign(key: &SigningKey, bytes: &[u8]) -> Vec<u8> {
        STANDARD.encode(key.sign(bytes).to_bytes()).into_bytes()
    }

    fn hosts() -> Vec<String> {
        vec!["github.com".into()]
    }

    fn feed(url: &str) -> String {
        format!(
            r#"{{"schema":1,"version":"9.9.9","files":{{"{}":{{"url":"{url}","size":10,"sha512":"{}"}}}}}}"#,
            artifact_key(),
            STANDARD.encode([1u8; 64])
        )
    }

    #[test]
    fn verifies_only_with_a_key() {
        let k = key();
        let public = k.verifying_key().to_bytes();
        let body = feed("https://github.com/o/r/releases/download/t/x").into_bytes();
        let sig = sign(&k, &body);
        assert!(verify(&body, None, None).is_ok(), "no key baked: nothing to check");
        assert!(verify(&body, Some(&sig), Some(&public)).is_ok());
        assert_eq!(verify(&body, None, Some(&public)).unwrap_err(), "the update feed is not signed");
        let mut tampered = body.clone();
        tampered[20] ^= 1;
        assert_eq!(verify(&tampered, Some(&sig), Some(&public)).unwrap_err(), "the update feed's signature doesn't match");
        let other = SigningKey::from_bytes(&[8u8; 32]);
        assert!(verify(&body, Some(&sign(&other, &body)), Some(&public)).is_err(), "another key's signature");
        assert!(verify(&body, Some(b"not base64!"), Some(&public)).is_err());
    }

    #[test]
    fn parses_and_checks_the_artifact() {
        let ok = feed("https://github.com/o/r/releases/download/native-v9.9.9/x");
        let (_, version, artifact) = parse(ok.as_bytes(), "o/r", &hosts()).unwrap();
        assert_eq!(version.to_string(), "9.9.9");
        assert_eq!(artifact.unwrap().size, 10);
        for bad in [
            "https://github.com/other/r/releases/download/t/x",
            "https://evil.example/o/r/releases/download/t/x",
            "http://github.com/o/r/releases/download/t/x",
            "https://github.com/o/r/archive/x",
        ] {
            assert!(parse(feed(bad).as_bytes(), "o/r", &hosts()).is_err(), "{bad}");
        }
        assert!(parse(br#"{"schema":2,"version":"1.0.0","files":{}}"#, "o/r", &hosts()).is_err());
        assert!(parse(br#"{"schema":1,"version":"x","files":{}}"#, "o/r", &hosts()).is_err());
        let (_, _, none) = parse(br#"{"schema":1,"version":"1.0.0","files":{}}"#, "o/r", &hosts()).unwrap();
        assert!(none.is_none(), "no artifact for this machine: notify only");
    }

    #[test]
    fn release_pages_stay_on_the_repository() {
        let mut f: Feed = serde_json::from_str(r#"{"schema":1,"version":"1.0.0","files":{}}"#).unwrap();
        assert_eq!(release_page(&f, "o/r"), "https://github.com/o/r/releases");
        f.release_page = Some("https://github.com/o/r/releases/tag/native-v1.0.0".into());
        assert_eq!(release_page(&f, "o/r"), "https://github.com/o/r/releases/tag/native-v1.0.0");
        f.release_page = Some("https://evil.example/o/r/releases".into());
        assert_eq!(release_page(&f, "o/r"), "https://github.com/o/r/releases");
    }
}

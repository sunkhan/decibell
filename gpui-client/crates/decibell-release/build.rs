//! Bakes what a release build fixes at build time:
//!
//! - `DECIBELL_VERSION`: the one version the repository has, `electron-client/package.json`'s
//!   `"version"` (CLAUDE.md: "the only version site"), until the Electron client retires. About,
//!   the device list, the updater and crash reports all show it. A build without that file fails:
//!   there is no second place to take a version from.
//! - `DECIBELL_UPDATE_PUBLIC_KEY` (optional, from the environment): the Ed25519 public key the
//!   update feed must be signed with, as base64 of the 32 raw bytes. Baked in, the updater refuses
//!   any feed whose signature doesn't verify; absent, it trusts HTTPS to GitHub and the SHA-512s
//!   in the feed, as electron-updater does (gpui-client/PACKAGING.md, "Updates").
//! - `DECIBELL_UPDATE_REPO` (default `sunkhan/decibell`) and `DECIBELL_UPDATE_CHANNEL` (default
//!   `native-latest`): where the feed is, `https://github.com/<repo>/releases/download/<channel>/`.

use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let package_json = manifest.join("../../../electron-client/package.json");
    println!("cargo::rerun-if-changed={}", package_json.display());
    println!("cargo::rerun-if-changed=build.rs");
    let text = std::fs::read_to_string(&package_json).unwrap_or_else(|e| {
        panic!("{}: {e} (the one version site: CLAUDE.md, \"Release\")", package_json.display())
    });
    let version = version_of(&text)
        .unwrap_or_else(|| panic!("{}: no \"version\": \"x.y.z\"", package_json.display()));
    println!("cargo::rustc-env=DECIBELL_VERSION={version}");
    // For dependents' build scripts (decibell-app's Windows resources).
    println!("cargo::metadata=VERSION={version}");

    for var in ["DECIBELL_UPDATE_PUBLIC_KEY", "DECIBELL_UPDATE_REPO", "DECIBELL_UPDATE_CHANNEL"] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    let key = std::env::var("DECIBELL_UPDATE_PUBLIC_KEY").unwrap_or_default();
    let key = key.trim();
    if !key.is_empty() {
        // 32 bytes in base64 are 44 characters ending in one '='. A wrong value fails the build
        // rather than shipping an updater that refuses every feed.
        let ok = key.len() == 44
            && key.ends_with('=')
            && key[..43].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/');
        assert!(
            ok,
            "DECIBELL_UPDATE_PUBLIC_KEY must be base64 of a raw 32-byte Ed25519 public key \
             (gpui-client/PACKAGING.md, \"Signing key\")"
        );
    }
    println!("cargo::rustc-env=DECIBELL_UPDATE_PUBLIC_KEY_B64={key}");
    let repo = std::env::var("DECIBELL_UPDATE_REPO").unwrap_or_else(|_| "sunkhan/decibell".into());
    assert!(
        repo.split('/').count() == 2 && repo.bytes().all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b)),
        "DECIBELL_UPDATE_REPO must be <owner>/<repo>"
    );
    println!("cargo::rustc-env=DECIBELL_UPDATE_REPO={repo}");
    let channel = std::env::var("DECIBELL_UPDATE_CHANNEL").unwrap_or_else(|_| "native-latest".into());
    assert!(
        !channel.is_empty() && channel.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "DECIBELL_UPDATE_CHANNEL must be a tag name"
    );
    println!("cargo::rustc-env=DECIBELL_UPDATE_CHANNEL={channel}");
}

/// The top-level `"version"` of package.json (the first `"version"` key: npm writes it before any
/// nested object that could hold another), checked to be `MAJOR.MINOR.PATCH[-pre]`.
fn version_of(json: &str) -> Option<String> {
    let at = json.find("\"version\"")?;
    let rest = json[at + "\"version\"".len()..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let version = &rest[..rest.find('"')?];
    let core = version.split(['-', '+']).next()?;
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    let clean = version.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.+".contains(&b));
    (numeric && clean).then(|| version.to_string())
}

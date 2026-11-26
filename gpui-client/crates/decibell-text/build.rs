//! Packs the Twemoji SVGs the client can draw into one file, `$OUT_DIR/twemoji.pack`, which
//! `src/emoji/pack.rs` embeds:
//!
//! ```text
//! u32 count
//! count × { u32 key_offset, u32 data_offset }      sorted by key; one more entry closes the last
//! keys   (the emoji's characters, UTF-8, as `EmojiAsset::chars` spells them)
//! data   (each SVG deflated on its own, so one emoji costs one small inflate)
//! ```
//!
//! Source: `gpui-client/assets/twemoji/` (gitignored; `node gpui-client/tools/fetch-assets.mjs`
//! fetches it). Without it the pack is empty and emoji are drawn by the system emoji font: the
//! build warns and goes on, so a checkout without the assets still compiles.
//!
//! Left out: files whose name keeps U+FE0F inside a multi-code-point sequence. The renderer
//! (Twemoji.tsx `toCodePoints`) drops FE0F before looking a sequence up, so it never finds
//! them, and neither does `decibell_richtext::emoji::twemoji_asset`.

use std::path::{Path, PathBuf};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/twemoji");
    println!("cargo:rerun-if-changed={}", dir.display());
    println!("cargo:rerun-if-changed=build.rs");

    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(read) => {
            for entry in read {
                let path = entry.unwrap().path();
                if path.extension().is_none_or(|extension| extension != "svg") {
                    continue;
                }
                let stem = path.file_stem().unwrap().to_str().unwrap();
                let Some(key) = key_for(stem) else { continue };
                let svg = std::fs::read(&path).unwrap();
                entries.push((key, miniz_oxide::deflate::compress_to_vec(&svg, 8)));
            }
        }
        Err(_) => println!(
            "cargo:warning=gpui-client/assets/twemoji is missing (run tools/fetch-assets.mjs): \
             emoji will be drawn by the system font"
        ),
    }
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));

    let mut index = Vec::new();
    let mut keys = Vec::new();
    let mut data = Vec::new();
    for (key, deflated) in &entries {
        index.extend_from_slice(&(keys.len() as u32).to_le_bytes());
        index.extend_from_slice(&(data.len() as u32).to_le_bytes());
        keys.extend_from_slice(key.as_bytes());
        data.extend_from_slice(deflated);
    }
    index.extend_from_slice(&(keys.len() as u32).to_le_bytes());
    index.extend_from_slice(&(data.len() as u32).to_le_bytes());

    let mut pack = (entries.len() as u32).to_le_bytes().to_vec();
    pack.extend_from_slice(&index);
    pack.extend_from_slice(&keys);
    pack.extend_from_slice(&data);
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("twemoji.pack");
    std::fs::write(out, pack).unwrap();
}

/// `1f468-200d-1f469` → the characters, or `None` for a file the lookup can never reach.
fn key_for(stem: &str) -> Option<String> {
    let points: Vec<u32> = stem
        .split('-')
        .map(|hex| u32::from_str_radix(hex, 16).ok())
        .collect::<Option<_>>()?;
    if points.len() > 1 && points.contains(&0xFE0F) {
        return None;
    }
    points.into_iter().map(char::from_u32).collect()
}

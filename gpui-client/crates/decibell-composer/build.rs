//! The emoji picker's dataset (`assets/emoji-data/native.json`, `@emoji-mart/data` 1.2.1's
//! `sets/15/native.json`) as static Rust data: nothing is parsed at run time, and the pages are
//! only read while the picker shows them or a search walks them.
//!
//! What is kept is what `EmojiPicker.tsx` reads: the emoji in the object's key order (search results
//! follow it), each with its id, name, keywords, `skins[0].native`; the lower-cased name and keywords
//! the search compares with (JS lower-cases them per query: done once here); the categories' order
//! and members. A checkout without the assets (`node tools/fetch-assets.mjs`) builds an empty set
//! and warns; the picker then shows no emoji.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let source = manifest.join("../../assets/emoji-data/native.json");
    println!("cargo:rerun-if-changed={}", source.display());
    println!("cargo:rerun-if-changed=build.rs");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("emoji_data.rs");
    let text = match std::fs::read_to_string(&source) {
        Ok(text) => text,
        Err(_) => {
            println!("cargo:warning=assets/emoji-data/native.json is missing (node tools/fetch-assets.mjs): the emoji picker will be empty");
            std::fs::write(&out, "pub static EMOJI: &[Emoji] = &[];\npub static CATEGORIES: &[(&str, &[u16])] = &[];\npub static BY_ID: &[u16] = &[];\n").unwrap();
            return;
        }
    };
    let json: serde_json::Value = serde_json::from_str(&text).expect("native.json is JSON");
    let emojis = json["emojis"].as_object().expect("emojis");
    // serde_json's map is sorted; JavaScript's key order is the file's (with array-index keys
    // first, ascending: "100" and "1234" open the file anyway). Recover it from the text.
    let mut ids: Vec<(&String, &serde_json::Value)> = emojis.iter().collect();
    let position = |id: &str| -> usize {
        let needle = format!("\"{id}\":{{\"id\":\"{id}\"");
        text.find(&needle).unwrap_or_else(|| panic!("{id} not found in native.json"))
    };
    let numeric = |id: &str| -> Option<u64> {
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) && (id == "0" || !id.starts_with('0'))).then(|| id.parse().ok()).flatten()
    };
    ids.sort_by_key(|(id, _)| match numeric(id) {
        Some(n) => (0usize, n as usize),
        None => (1, position(id)),
    });
    let index_of: std::collections::HashMap<&str, usize> = ids.iter().enumerate().map(|(i, (id, _))| (id.as_str(), i)).collect();

    let mut code = String::new();
    code.push_str("pub static EMOJI: &[Emoji] = &[\n");
    for (id, emoji) in &ids {
        let name = emoji["name"].as_str().unwrap_or_default();
        let native = emoji["skins"][0]["native"].as_str().unwrap_or_default();
        let keywords: Vec<&str> = emoji["keywords"].as_array().map(|k| k.iter().filter_map(|k| k.as_str()).collect()).unwrap_or_default();
        let lower: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
        writeln!(
            code,
            "    Emoji {{ id: {:?}, name: {:?}, name_lower: {:?}, keywords_lower: &{:?}, native: {:?} }},",
            id,
            name,
            name.to_lowercase(),
            lower,
            native
        )
        .unwrap();
    }
    code.push_str("];\n\npub static CATEGORIES: &[(&str, &[u16])] = &[\n");
    for category in json["categories"].as_array().expect("categories") {
        let id = category["id"].as_str().unwrap_or_default();
        let members: Vec<usize> = category["emojis"]
            .as_array()
            .map(|m| m.iter().filter_map(|e| e.as_str()).filter_map(|e| index_of.get(e).copied()).collect())
            .unwrap_or_default();
        writeln!(code, "    ({id:?}, &{members:?}),").unwrap();
    }
    // Ids sorted, for looking a "recent" id up.
    let mut by_id: Vec<usize> = (0..ids.len()).collect();
    by_id.sort_by(|a, b| ids[*a].0.cmp(ids[*b].0));
    code.push_str("];\n\n");
    writeln!(code, "pub static BY_ID: &[u16] = &{by_id:?};").unwrap();
    std::fs::write(&out, code).unwrap();
}

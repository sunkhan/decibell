//! Shared by the integration tests: reading the vector files written by
//! tools/vectors.mjs and printing trees the way that script does.
#![allow(dead_code)]

use std::io::Read;
use std::path::{Path, PathBuf};

use decibell_highlight::testing::{self, Item};
use decibell_highlight::{Budget, Fallback, Language};

pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub struct Case {
    pub lang: String,
    pub source: String,
    pub flag: String,
    /// lowlight's relevance, as JavaScript prints the number.
    pub relevance: String,
    pub tree: String,
    /// The code as JavaScript holds it: UTF-16 code units.
    pub code: Vec<u16>,
}

pub fn read_gz(path: &Path) -> String {
    let mut text = String::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .read_to_string(&mut text)
        .unwrap();
    text
}

/// Decodes a JSON string literal (as `JSON.stringify` writes it) to UTF-16.
pub fn json_string(s: &str) -> Vec<u16> {
    let s = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).expect("JSON string");
    let mut out = Vec::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u16; 2];
            out.extend_from_slice(c.encode_utf16(&mut buf));
            continue;
        }
        match chars.next().expect("escape") {
            'n' => out.push(0x0a),
            't' => out.push(0x09),
            'r' => out.push(0x0d),
            'b' => out.push(0x08),
            'f' => out.push(0x0c),
            'u' => {
                let hex: String = chars.by_ref().take(4).collect();
                out.push(u16::from_str_radix(&hex, 16).expect("\\u escape"));
            }
            c => out.push(c as u16),
        }
    }
    out
}

pub fn read_cases(path: &Path) -> Vec<Case> {
    read_gz(path)
        .lines()
        .map(|line| {
            let mut f = line.splitn(6, '\t');
            let mut next = || f.next().expect("6 fields").to_string();
            let (lang, source, flag, relevance, tree) = (next(), next(), next(), next(), next());
            Case { lang, source, flag, relevance, tree, code: json_string(&next()) }
        })
        .collect()
}

/// The canonical tree string of tools/vectors.mjs.
pub fn tree_string(items: &[Item]) -> String {
    let mut out = String::new();
    for item in items {
        match item {
            Item::Open(classes) => {
                out.push('O');
                out.push_str(classes);
                out.push(';');
            }
            Item::Close => out.push_str("C;"),
            Item::Text(r) => out.push_str(&format!("T{};", r.len())),
        }
    }
    out
}

/// What CodeBlock.tsx's `highlight(lang, code)` yields, as (flag, tree,
/// relevance).
pub fn ours(lang: &str, code: &[u16]) -> (&'static str, String, f64) {
    let plain = || if code.is_empty() { String::new() } else { format!("T{};", code.len()) };
    let Some(language) = Language::from_name(lang) else {
        return ("ok", plain(), 0.0);
    };
    match testing::tree(language, code, &Budget::UNLIMITED) {
        Ok((items, relevance)) => {
            // the text runs must spell the input, in order
            let mut at = 0;
            for item in &items {
                if let Item::Text(r) = item {
                    assert_eq!(r.start, at, "text out of order");
                    at = r.end;
                }
            }
            assert_eq!(at, code.len(), "text does not cover the input");
            ("ok", tree_string(&items), relevance)
        }
        Err(Fallback::EngineError) => ("throw", plain(), 0.0),
        Err(other) => panic!("unexpected fallback {other:?}"),
    }
}

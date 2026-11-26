//! Differential test against the Electron renderer (vectors from tests/vectors/gen.mjs, which runs
//! the renderer's own richText.ts / MessageText.tsx / LinkEmbeds.tsx / RichInput.tsx / Twemoji.tsx
//! / inviteLink.ts in an Electron 33 window).
//!
//! * edge + url suites: inputs and records committed in full (edge.jsonl), compared case by case.
//! * matrix / emoji / fixture / fuzz / urlfuzz: inputs regenerated here (input hashes prove they are the
//!   renderer's inputs), records compared by SHA-256 per 1000-case chunk (suites.json); when
//!   gen.mjs has left the full records in target-richtext/vectors/, every case is diffed.

mod common;

use common::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn report(name: &str, bad: &[(String, String, String)], total: usize) {
    let show = if std::env::var_os("DECIBELL_RICHTEXT_REPORT_ALL").is_some() { usize::MAX } else { 12 };
    for (input, want, got) in bad.iter().take(show) {
        eprintln!("── {name} mismatch: input {}\n   renderer: {want}\n   rust:     {got}", serde_json::to_string(input).unwrap());
    }
    eprintln!("{name}: {} of {total} cases differ", bad.len());
}

#[test]
fn edge_and_url_suites() {
    let text = std::fs::read_to_string(vectors_dir().join("edge.jsonl")).unwrap();
    let mut bad = Vec::new();
    let mut n = 0;
    for line in text.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        let input = v[0].as_str().unwrap();
        let want = serde_json::to_string(&v[1]).unwrap();
        let url_fns = v.get(2).is_some();
        let got = record_string(input, url_fns);
        n += 1;
        if got != want {
            bad.push((input.to_owned(), want, got));
        }
    }
    report("edge+url", &bad, n);
    println!("edge+url: {n} cases compared, {} mismatches", bad.len());
    assert!(bad.is_empty());
    assert!(n > 1000);
}

fn chunk_hash<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut h = Sha256::new();
    for l in lines {
        h.update(l.as_bytes());
        h.update(b"\n");
    }
    let d = h.finalize();
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn hashed_suite(name: &str, inputs: Vec<String>) {
    let meta: Value = serde_json::from_str(&std::fs::read_to_string(vectors_dir().join("suites.json")).unwrap()).unwrap();
    let m = &meta[name];
    assert_eq!(m["count"].as_u64().unwrap() as usize, inputs.len(), "{name}: case count");
    let chunk = m["chunk"].as_u64().unwrap() as usize;
    let enc: Vec<String> = inputs.iter().map(|s| serde_json::to_string(s).unwrap()).collect();
    for (k, c) in enc.chunks(chunk).enumerate() {
        assert_eq!(chunk_hash(c.iter().map(String::as_str)), m["inputs"][k].as_str().unwrap(), "{name}: inputs of chunk {k} differ from the renderer's (corpus generator drift)");
    }
    let url_fns = m["urlFns"].as_bool().unwrap_or(false);
    let recs: Vec<String> = inputs.iter().map(|s| record_string(s, url_fns)).collect();
    let full = std::fs::read_to_string(full_dir().join(format!("{name}.jsonl"))).ok();
    let mut bad = Vec::new();
    if let Some(full) = &full {
        for (i, line) in full.lines().enumerate() {
            let v: Value = serde_json::from_str(line).unwrap();
            assert_eq!(v[0].as_str().unwrap(), inputs[i], "{name}: full vectors are for other inputs");
            let want = serde_json::to_string(&v[1]).unwrap();
            if want != recs[i] {
                bad.push((inputs[i].clone(), want, recs[i].clone()));
            }
        }
        report(name, &bad, inputs.len());
    }
    let mut bad_chunks = Vec::new();
    for (k, c) in recs.chunks(chunk).enumerate() {
        if chunk_hash(c.iter().map(String::as_str)) != m["records"][k].as_str().unwrap() {
            bad_chunks.push(k);
        }
    }
    println!(
        "{name}: {} cases compared ({}), {} mismatching chunks, {} mismatching cases",
        inputs.len(),
        if full.is_some() { "per case + hashes" } else { "by chunk hash" },
        bad_chunks.len(),
        bad.len()
    );
    assert!(bad.is_empty() && bad_chunks.is_empty(), "{name}: chunks {bad_chunks:?} differ (run tests/vectors/gen.mjs for a per-case diff)");
}

#[test]
fn matrix_suite() {
    hashed_suite("matrix", matrix());
}

#[test]
fn emoji_suite() {
    hashed_suite("emoji", emoji_inputs());
}

#[test]
fn fixture_suite() {
    hashed_suite("fixture", fixture());
}

#[test]
fn fuzz_suite() {
    hashed_suite("fuzz", fuzz());
}

#[test]
fn url_fuzz_suite() {
    hashed_suite("urlfuzz", slot_fuzz("urlfuzz.json"));
}

/// `node tests/vectors/gen.mjs --stress=300000 [--seed=N]` first; then
/// `cargo test --release --test differential -- --ignored stress`.
#[test]
#[ignore]
fn stress() {
    let mut any = false;
    for name in ["stress-fuzz", "stress-urlfuzz"] {
        let Ok(text) = std::fs::read_to_string(full_dir().join(format!("{name}.jsonl"))) else { continue };
        any = true;
        let mut bad = Vec::new();
        let mut n = 0;
        for line in text.lines() {
            let v: Value = serde_json::from_str(line).unwrap();
            let input = v[0].as_str().unwrap();
            let want = serde_json::to_string(&v[1]).unwrap();
            let got = record_string(input, v.get(2).is_some());
            n += 1;
            if got != want {
                bad.push((input.to_owned(), want, got));
            }
        }
        report(name, &bad, n);
        println!("{name}: {n} cases compared, {} mismatches", bad.len());
        assert!(bad.is_empty());
    }
    assert!(any, "no stress vectors: run tests/vectors/gen.mjs --stress=N");
}

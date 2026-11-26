//! Tree-level differential test: for every corpus formula, the port's markup must equal what
//! KaTeX 0.18.4's `renderToString` emitted (MathML half removed), byte for byte; KaTeX
//! ParseErrors must come out as the same error span (same message); KaTeX's non-ParseError
//! throws must be internal errors here. Vectors: `tests/data/markup.jsonl.gz`
//! (`tools/gen_markup.mjs`). Known, explained differences are listed in
//! `tests/data/markup_known.txt` (`id<TAB>reason`); anything else fails.
//!
//! `TEX_MARKUP_VERBOSE=1` prints every mismatch with context.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};

use decibell_tex::{Settings, render_to_markup};

fn data(name: &str) -> std::path::PathBuf {
    // `file!()` is absolute when built from a manifest outside the crate.
    let here = std::path::Path::new(file!());
    let root = if here.is_absolute() {
        here.parent().unwrap().parent().unwrap().to_path_buf()
    } else {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    };
    root.join("tests/data").join(name)
}

fn known() -> HashMap<String, String> {
    let mut m = HashMap::new();
    if let Ok(s) = std::fs::read_to_string(data("markup_known.txt")) {
        for line in s
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            let (id, why) = line.split_once('\t').unwrap_or((line, ""));
            m.insert(id.to_string(), why.to_string());
        }
    }
    m
}

#[test]
fn markup_matches_katex() {
    // Debug builds need far more stack per nesting level than release ones.
    std::thread::Builder::new()
        .stack_size(256 << 20)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn run() {
    let f = std::fs::File::open(data("markup.jsonl.gz")).expect("tests/data/markup.jsonl.gz");
    let r = BufReader::new(flate2::read::GzDecoder::new(f));
    let verbose = std::env::var("TEX_MARKUP_VERBOSE").is_ok();
    let known = known();
    let (mut total, mut same, mut known_fail) = (0, 0, 0);
    let mut unexpected = Vec::new();
    for line in r.lines() {
        let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = v["id"].as_str().unwrap();
        let tex = v["tex"].as_str().unwrap();
        let display = v["display"].as_bool().unwrap();
        let status = v["status"].as_str().unwrap();
        let html = v["html"].as_str().unwrap();
        total += 1;
        let settings = Settings {
            display_mode: display,
            ..Default::default()
        };
        let ours = render_to_markup(tex, &settings);
        let ok = match (status, &ours) {
            ("throw", Err(_)) => true,
            ("throw", Ok(_)) => false,
            (_, Ok(m)) => m == html,
            (_, Err(_)) => false,
        };
        if ok {
            same += 1;
            continue;
        }
        if known.contains_key(id) {
            known_fail += 1;
            continue;
        }
        if verbose {
            let o = match &ours {
                Ok(m) => m.clone(),
                Err(e) => format!("!INTERNAL {}", e),
            };
            let k = if status == "throw" {
                format!("!THROW {}", html)
            } else {
                html.to_string()
            };
            let j = k.bytes().zip(o.bytes()).take_while(|(a, b)| a == b).count();
            let lo = |s: &str| {
                s.char_indices()
                    .map(|(i, _)| i)
                    .rfind(|&i| i <= j.saturating_sub(60))
                    .unwrap_or(0)
            };
            let cut = |s: &str, from: usize| s[from..].chars().take(200).collect::<String>();
            eprintln!(
                "MISMATCH {} {:?} display={}\n  katex: {}\n  ours : {}",
                id,
                tex,
                display,
                cut(&k, lo(&k)),
                cut(&o, lo(&o))
            );
        }
        unexpected.push(id.to_string());
    }
    eprintln!(
        "markup: {}/{} identical, {} known differences, {} unexpected",
        same,
        total,
        known_fail,
        unexpected.len()
    );
    assert!(
        unexpected.is_empty(),
        "unexpected markup differences: {:?}",
        &unexpected[..unexpected.len().min(40)]
    );
}

//! Shared by the integration tests and the benchmark: the corpora (regenerated exactly as
//! tests/vectors/corpus.mjs does) and the canonical record (as tests/vectors/harness/entry.js
//! writes it from the renderer's output).
#![allow(dead_code)]

use decibell_richtext::{composer, links, plain, Document, SpanKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub fn vectors_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors")
}

pub fn full_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target-richtext/vectors")
}

// ── corpora ──

/// corpus.mjs `rng` (the mulberry32 variant of qt-client/tools/gen-fixture.mjs).
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Rng {
        Rng(seed)
    }
    pub fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x6d2b79f5);
        let mut t = self.0;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        ((t ^ (t >> 14)) as f64) / 4294967296.0
    }
    pub fn index(&mut self, n: usize) -> usize {
        (self.next() * n as f64).floor() as usize
    }
    pub fn pick<'a, T>(&mut self, a: &'a [T]) -> &'a T {
        &a[self.index(a.len())]
    }
    pub fn between(&mut self, lo: i64, hi: i64) -> i64 {
        lo + (self.next() * (hi - lo + 1) as f64).floor() as i64
    }
}

pub fn matrix() -> Vec<String> {
    let m: Value = serde_json::from_str(&std::fs::read_to_string(vectors_dir().join("matrix.json")).unwrap()).unwrap();
    let strs = |k: &str| -> Vec<String> { m[k].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect() };
    let markers: Vec<(String, String)> = m["markers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_owned(), p[1].as_str().unwrap().to_owned()))
        .collect();
    let (contents, prefixes, suffixes) = (strs("contents"), strs("prefixes"), strs("suffixes"));
    let mut out = Vec::new();
    for (o, c) in &markers {
        for x in &contents {
            for p in &prefixes {
                for s in &suffixes {
                    out.push(format!("{p}{o}{x}{c}{s}"));
                }
            }
        }
    }
    out
}

pub fn fuzz() -> Vec<String> {
    let f: Value = serde_json::from_str(&std::fs::read_to_string(vectors_dir().join("fuzz.json")).unwrap()).unwrap();
    let mut pool: Vec<String> = Vec::new();
    for fr in f["fragments"].as_array().unwrap() {
        for _ in 0..fr[1].as_u64().unwrap() {
            pool.push(fr[0].as_str().unwrap().to_owned());
        }
    }
    let mut rnd = Rng::new(f["seed"].as_u64().unwrap() as u32);
    let max = f["maxFragments"].as_u64().unwrap() as usize;
    (0..f["count"].as_u64().unwrap())
        .map(|_| {
            let n = 1 + rnd.index(max);
            let mut s = String::new();
            for _ in 0..n {
                s.push_str(&pool[rnd.index(pool.len())]);
            }
            s
        })
        .collect()
}

/// corpus.mjs `slotFuzz`: per slot, `min..=max` picks from its weighted fragments.
pub fn slot_fuzz(file: &str) -> Vec<String> {
    let f: Value = serde_json::from_str(&std::fs::read_to_string(vectors_dir().join(file)).unwrap()).unwrap();
    let pools: Vec<(u64, u64, Vec<String>)> = f["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            let mut p = Vec::new();
            for it in s["items"].as_array().unwrap() {
                for _ in 0..it[1].as_u64().unwrap() {
                    p.push(it[0].as_str().unwrap().to_owned());
                }
            }
            (s["min"].as_u64().unwrap(), s["max"].as_u64().unwrap(), p)
        })
        .collect();
    let mut rnd = Rng::new(f["seed"].as_u64().unwrap() as u32);
    (0..f["count"].as_u64().unwrap())
        .map(|_| {
            let mut s = String::new();
            for (min, max, p) in &pools {
                let n = min + (rnd.next() * (max - min + 1) as f64).floor() as u64;
                for _ in 0..n {
                    s.push_str(&p[rnd.index(p.len())]);
                }
            }
            s
        })
        .collect()
}

pub fn emoji_inputs() -> Vec<String> {
    std::fs::read_to_string(vectors_dir().join("emoji-inputs.txt"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str::<String>(l).unwrap())
        .collect()
}

/// The 10,000 message texts of qt-client/tools/gen-fixture.mjs --seed 7 (same draw order).
pub fn fixture() -> Vec<String> {
    const WORDS: &str = "the a stream tonight build new server patch voice channel lag fps frame bitrate encoder works fine \
        broken again fixed it now ok yes no maybe later tomorrow today game ranked match lobby queue join leave win lost \
        gg nice clip watch this look at that why how when where who what config driver update reboot test release \
        linux windows wayland nvidia audio mic headset echo noise loud quiet sorry thanks great cool wow lol \
        selam naber tamam evet hayır sonra bugün yarın oyun maç ses kanal güncelleme";
    let words: Vec<&str> = WORDS.split(' ').collect();
    let emoji = ["😂", "🔥", "👍", "❤️", "🎮", "✨", "😭", "🙏", "👀", "💀", "🎉", "😅"];
    let langs: [(&str, &str); 4] = [
        ("rust", "fn main() {\n    let rate = 120;\n    println!(\"{rate} fps\");\n}"),
        ("ts", "const fps = await probe();\nif (fps < 60) retry();"),
        ("cpp", "int main() {\n  return decibell::run();\n}"),
        ("sh", "cargo test --lib\nnpx napi build --platform"),
    ];
    let weights: Vec<f64> = (0..16).map(|i| if i < 4 { 12.0 } else if i < 8 { 4.0 } else { 1.0 }).collect();
    let total_w: f64 = weights.iter().sum();
    let mut r = Rng::new(7);
    let sentence = |r: &mut Rng, lo: i64, hi: i64| -> String {
        let n = r.between(lo, hi);
        let w: Vec<&str> = (0..n).map(|_| *r.pick(&words)).collect();
        let s = w.join(" ");
        let mut c = s.chars();
        let f = c.next().unwrap();
        f.to_uppercase().collect::<String>() + c.as_str()
    };
    let content = |r: &mut Rng| -> String {
        let x = r.next();
        if x < 0.40 {
            return sentence(r, 1, 8);
        }
        if x < 0.58 {
            let s = sentence(r, 10, 30);
            return s + *r.pick(&[".", "?", "!", ""]);
        }
        if x < 0.64 {
            let mut p = Vec::new();
            let mut i = 0;
            loop {
                let n = r.between(2, 4);
                if i >= n {
                    break;
                }
                p.push(sentence(r, 15, 40) + ".");
                i += 1;
            }
            return p.join(" ");
        }
        if x < 0.71 {
            let s = sentence(r, 3, 10);
            let mut w: Vec<String> = s.split(' ').map(str::to_owned).collect();
            let i = r.between(0, w.len() as i64 - 1) as usize;
            w[i] = format!("`{}`", r.pick(&["cargo build", "fps=120", "nvenc", "d3d11va", "--release", "config.json", "v0.9.4"]));
            return w.join(" ");
        }
        if x < 0.74 {
            let (lang, code) = *r.pick(&langs);
            return format!("{}:\n```{}\n{}\n```", sentence(r, 2, 8), lang, code);
        }
        if x < 0.80 {
            let s = sentence(r, 4, 14);
            let mut w: Vec<String> = s.split(' ').map(str::to_owned).collect();
            let i = r.between(0, w.len() as i64 - 1) as usize;
            let m = *r.pick(&[("**", "**"), ("*", "*"), ("__", "__"), ("~~", "~~")]);
            w[i] = format!("{}{}{}", m.0, w[i], m.1);
            return w.join(" ");
        }
        if x < 0.88 {
            let s = sentence(r, 2, 10);
            let e1 = *r.pick(&emoji);
            let e2 = if r.next() < 0.3 { *r.pick(&emoji) } else { "" };
            return format!("{s} {e1}{e2}");
        }
        if x < 0.90 {
            let n = r.between(1, 3);
            return (0..n).map(|_| *r.pick(&emoji)).collect::<Vec<_>>().join("");
        }
        if x < 0.95 {
            let pre = *r.pick(&["check this ", "link: ", "", "docs here "]);
            let clips = format!("clips/{}", r.between(100, 999));
            let issue = format!("issue/{}", r.between(1, 500));
            let paths = ["patch-notes".to_owned(), clips, "docs/voice".to_owned(), issue];
            let p = r.pick(&paths).clone();
            return format!("{pre}https://example.com/{p}");
        }
        let a = sentence(r, 1, 6);
        let b = sentence(r, 2, 8).to_lowercase();
        format!("{a}\nand {b}")
    };
    let mut out = Vec::with_capacity(10000);
    for i in 0..10000 {
        if r.next() < 0.08 {
            r.between(1, 10);
        } else if r.next() < 0.5 {
            r.between(5, 90);
        } else {
            r.between(2, 15);
        }
        let mut x = r.next() * total_w;
        for w in &weights {
            x -= w;
            if x < 0.0 {
                break;
            }
        }
        let mut c = content(&mut r);
        if i > 5 && r.next() < 0.06 {
            r.between(1, 5);
        }
        if r.next() < 0.07 {
            let n = if r.next() < 0.7 { 1 } else { r.between(2, 3) };
            for _ in 0..n {
                r.index(18);
            }
            if r.next() < 0.4 {
                c = String::new();
            }
        }
        out.push(c);
    }
    out
}

// ── canonical record ──

fn invite(u: &str) -> Value {
    match links::parse_invite_link(u) {
        None => Value::Null,
        Some(i) => match i.endpoint {
            None => json!([i.code]),
            Some((h, p)) => json!([i.code, h, p]),
        },
    }
}

fn media(u: &str) -> Value {
    links::media_label_for(u).map_or(Value::Null, |m| Value::String(m.as_str().to_owned()))
}

fn push_text(out: &mut Vec<Value>, style: u8, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(Value::Array(last)) = out.last_mut() {
        if last[0] == 0 && last[1] == style as u64 {
            let s = last[2].as_str().unwrap().to_owned() + text;
            last[2] = Value::String(s);
            return;
        }
    }
    out.push(json!([0, style, text]));
}

fn emoji_key(asset: Option<decibell_richtext::EmojiAsset>) -> Value {
    asset.map_or(Value::Null, |a| Value::String(a.name()))
}

pub fn items(doc: &Document, src: &str, emoji_size: u8) -> Vec<Value> {
    let mut out = Vec::new();
    for sp in doc.spans() {
        let st = sp.style.0 & 15;
        let t = doc.text(src, sp);
        match sp.kind {
            SpanKind::Text if sp.style.is_code() => out.push(json!([3, st, t])),
            SpanKind::Text => push_text(&mut out, st, t),
            SpanKind::Emoji { asset } => out.push(json!([1, st, t, emoji_key(asset), emoji_size])),
            SpanKind::Link { invite: inv, .. } => {
                out.push(json!([2, st, t, inv as u8, invite(t), media(t), links::is_gif_url(t) as u8]))
            }
            SpanKind::Math => out.push(json!([5, st, t, 0])),
            SpanKind::DisplayMath => out.push(json!([5, st, t, 1])),
            SpanKind::CodeBlock { lang } => {
                let tag: Value = match lang {
                    Some(l) => l.tag().into(),
                    None => doc.code_tag(src, sp).map_or(Value::Null, |t| t.to_ascii_lowercase().into()),
                };
                let label = lang.map_or("code", |l| l.tag());
                out.push(json!([4, st, tag, label, t]));
            }
        }
    }
    out
}

pub fn record(src: &str, url_fns: bool) -> Value {
    let doc = decibell_richtext::parse(src, &Default::default());
    let size = doc.jumbo_size().unwrap_or(decibell_richtext::INLINE_EMOJI_SIZE);
    let bubble = items(&doc, src, size);
    let pv = plain::preview(src);
    let preview = items(&pv, "", 13);
    let embeds: Vec<Value> = doc.embed_links(src, 3).iter().map(|u| json!([u, links::is_invite_link(u) as u8])).collect();
    let lone = doc.lone_link(src);
    let mut comp = Vec::new();
    let mut at = 0;
    for a in composer::emoji_atoms(src) {
        if a.range.0 > at {
            comp.push(json!([0, &src[at..a.range.0]]));
        }
        comp.push(json!([1, a.native, emoji_key(a.asset)]));
        at = a.range.1;
    }
    if at < src.len() {
        comp.push(json!([0, &src[at..]]));
    }
    let mut rec = vec![
        Value::Array(bubble),
        Value::Array(preview),
        Value::Array(embeds),
        lone.map_or(Value::Null, |l| l.into()),
        lone.map_or(Value::Null, media),
        (doc.has_formatting() as u8).into(),
        plain::rich_text_to_plain(src).into(),
        json!([composer::has_tokenizable(src) as u8, comp]),
        (composer::has_open_code_fence(src) as u8).into(),
    ];
    if url_fns {
        rec.push(json!([invite(src), links::is_invite_link(src) as u8, media(src), links::is_gif_url(src) as u8]));
    }
    Value::Array(rec)
}

pub fn record_string(src: &str, url_fns: bool) -> String {
    serde_json::to_string(&record(src, url_fns)).unwrap()
}

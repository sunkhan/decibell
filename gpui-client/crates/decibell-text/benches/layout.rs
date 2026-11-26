//! Layout cost on the 10k-message fixture: `cargo bench -p decibell-text` (Linux; GPUI's real
//! text system with the bundled fonts, no window).
//!
//! Message sources come from `DECIBELL_FIXTURE_TEXTS` (a JSON array, what
//! `tools/fixture-texts.py` prints); without it the fidelity vectors' fixture paragraphs are
//! used as plain text. The numbers in the README are from the former.

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[path = "../tests/common/mod.rs"]
mod common;
#[path = "../tests/common/counting.rs"]
mod counting;

#[global_allocator]
static ALLOCATOR: counting::Counting = counting::Counting;

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn main() {
    eprintln!("the layout bench runs on Linux only");
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn main() {
    use std::time::{Duration, Instant};

    use decibell_richtext::{ParseOptions, parse};
    use decibell_text::{RichContent, RichStyle};
    use gpui::Hsla;

    let texts: Vec<String> = match std::env::var("DECIBELL_FIXTURE_TEXTS") {
        Ok(path) => serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap(),
        Err(_) => fixture_from_vectors(),
    };
    let engine = common::engine();
    let grey = |l| Hsla { h: 0., s: 0., l, a: 1. };
    let style = RichStyle {
        family: "Decibell Sans".into(),
        mono_family: "Decibell Mono".into(),
        size: 14.5,
        leading: 1.45,
        letter_spacing: -0.16,
        color: grey(0.9),
        link: grey(0.7),
        muted: grey(0.5),
        meta_size: 11.5,
        error: grey(0.6),
        code_background: grey(0.1),
        code_radius: 6.,
    };
    let documents: Vec<_> = texts.iter().map(|text| parse(text, &ParseOptions::default())).collect();
    let percentile = |sorted: &[f64], q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
    let mut slot = |_, _, _| 60.0f32;

    // Warm the faces and the glyph-independent tables with a few messages that are not timed.
    for (index, (document, source)) in documents.iter().zip(&texts).take(50).enumerate() {
        let content = RichContent { document, source, edited: false };
        engine.rich_layout(u64::MAX - index as u64, content, &style, 560., 1., &mut slot);
    }

    // Cold: every message shaped and wrapped for the first time.
    let mut times = Vec::with_capacity(texts.len());
    let mut allocations = Vec::with_capacity(texts.len());
    let (_, bytes_before) = counting::counters();
    let stats_before = engine.stats();
    let mut kept = Vec::with_capacity(texts.len());
    let batch_started = Instant::now();
    let mut first_150 = Duration::ZERO;
    for (index, (document, source)) in documents.iter().zip(&texts).enumerate() {
        let content = RichContent { document, source, edited: index % 25 == 7 };
        let (count_before, _) = counting::counters();
        let started = Instant::now();
        let layout = engine.rich_layout(index as u64, content, &style, 560., 1., &mut slot);
        times.push(started.elapsed().as_secs_f64() * 1e6);
        allocations.push((counting::counters().0 - count_before) as f64);
        kept.push(layout);
        if index == 149 {
            first_150 = batch_started.elapsed();
        }
    }
    let total = batch_started.elapsed();
    let (_, bytes_after) = counting::counters();
    let stats = engine.stats();
    times.sort_by(f64::total_cmp);
    allocations.sort_by(f64::total_cmp);
    let count = texts.len() as f64;
    println!("{} messages at 560 px, scale 1 (cold: shaped and wrapped once each)", texts.len());
    println!(
        "  time per message      median {:.1} µs   p90 {:.1}   p99 {:.1}   max {:.1}   mean {:.1}",
        percentile(&times, 0.5),
        percentile(&times, 0.9),
        percentile(&times, 0.99),
        percentile(&times, 1.0),
        total.as_secs_f64() * 1e6 / count
    );
    println!(
        "  allocations per layout median {:.0}   p99 {:.0}",
        percentile(&allocations, 0.5),
        percentile(&allocations, 0.99)
    );
    println!(
        "  retained per message  {:.0} bytes (paragraph + layout + cache entries; {} shaper calls)",
        (bytes_after - bytes_before) as f64 / count,
        stats.shapes - stats_before.shapes
    );
    println!("  first 150 messages    {:.2} ms (a cold \"open channel\")", first_150.as_secs_f64() * 1e3);
    let per_ms = 1e3 / (total.as_secs_f64() * 1e6 / count);
    println!("  a 3.57 ms frame (280 Hz) fits {:.0} cold messages, a 16 ms budget {:.0}", per_ms * 3.57, per_ms * 16.);

    // Re-wrap: a window's worth of messages at another width, while their paragraphs are
    // still cached (a resize): shaping is reused, only the lines are broken again.
    let window = texts.len() - 300..texts.len();
    let mut times = Vec::new();
    let shapes_before = engine.stats().shapes;
    for index in window.clone() {
        let content = RichContent { document: &documents[index], source: &texts[index], edited: index % 25 == 7 };
        let started = Instant::now();
        kept.push(engine.rich_layout(index as u64, content, &style, 400., 1., &mut slot));
        times.push(started.elapsed().as_secs_f64() * 1e6);
    }
    times.sort_by(f64::total_cmp);
    println!(
        "re-wrap at 400 px      median {:.2} µs   p99 {:.2}   ({} shaper calls for {} messages)",
        percentile(&times, 0.5),
        percentile(&times, 0.99),
        engine.stats().shapes - shapes_before,
        window.len()
    );

    // A cache hit: what a row costs to look its layout up again.
    let started = Instant::now();
    let (count_before, _) = counting::counters();
    for (index, (document, source)) in documents.iter().zip(&texts).enumerate().skip(texts.len() - 300) {
        let content = RichContent { document, source, edited: index % 25 == 7 };
        std::hint::black_box(engine.rich_layout(index as u64, content, &style, 400., 1., &mut slot));
    }
    println!(
        "cache hit              {:.0} ns, {} allocations",
        started.elapsed().as_secs_f64() * 1e9 / 300.,
        counting::counters().0 - count_before
    );

    // Fallback: the first text in a script the bundled fonts lack (fontconfig is loaded, the
    // system's font for it is mapped and added, the text is shaped again), then a second.
    let plain = decibell_text::TextStyle::new(engine.face(&decibell_text::FontSpec::new("Decibell Sans", 400)), 14.5, grey(0.9));
    for (name, text) in [("first fallback (CJK)", "中文文本"), ("same font again", "日本語"), ("another font (Arabic)", "مرحبا"), ("emoji font", "🫨"), ("known characters", "中文 مرحبا")] {
        let started = Instant::now();
        let layout = engine.label_layout(text, &plain, decibell_text::Wrapping::NOWRAP, f32::INFINITY, 1.);
        println!("fallback: {name:<24} {:>8.2} ms (width {:.1} px)", started.elapsed().as_secs_f64() * 1e3, layout.width());
    }

    // Emoji: rasterising one from its packed SVG.
    let emoji: Vec<_> = ["😀", "🔥", "🎉", "👍", "❤️", "😂", "✨", "😭", "🙏", "👀", "💀", "🎮", "👨‍👩‍👧‍👦", "🏳️‍🌈", "🇹🇷", "🧑‍💻"]
        .iter()
        .filter_map(|emoji| decibell_richtext::emoji::twemoji_asset(emoji))
        .collect();
    println!(
        "emoji pack             {} emoji, {:.2} MB in the binary",
        decibell_text::packed_emoji_count(),
        decibell_text::packed_emoji_bytes() as f64 / 1e6
    );
    if !emoji.is_empty() {
        let started = Instant::now();
        for _ in 0..200 {
            for asset in &emoji {
                std::hint::black_box(decibell_text::emoji_svg(*asset));
            }
        }
        println!("  look up + inflate    {:.1} µs", started.elapsed().as_secs_f64() * 1e6 / (200 * emoji.len()) as f64);
        for size in [24u32, 30, 36, 48, 56, 112] {
            let mut times = Vec::new();
            for _ in 0..20 {
                for asset in &emoji {
                    let started = Instant::now();
                    std::hint::black_box(decibell_text::rasterize_emoji(*asset, size));
                    times.push(started.elapsed().as_secs_f64() * 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            println!(
                "  rasterise at {size:>3} px  median {:.0} µs   p90 {:.0}   max {:.0}   ({} KB of pixels)",
                percentile(&times, 0.5),
                percentile(&times, 0.9),
                percentile(&times, 1.0),
                size * size * 4 / 1024
            );
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn fixture_from_vectors() -> Vec<String> {
    use std::io::Read as _;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fidelity.json.gz");
    let mut packed = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_end(&mut packed)
        .unwrap();
    let vectors: serde_json::Value = serde_json::from_slice(&packed).unwrap();
    let texts = vectors["texts"].as_array().unwrap();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for case in vectors["cases"].as_array().unwrap() {
        if !case[4].as_str().unwrap().starts_with('f') || !seen.insert(case[0].as_u64().unwrap()) {
            continue;
        }
        let segs = texts[case[0].as_u64().unwrap() as usize].as_array().unwrap();
        out.push(segs.iter().filter_map(|seg| seg[1].as_str()).collect::<String>());
    }
    out
}

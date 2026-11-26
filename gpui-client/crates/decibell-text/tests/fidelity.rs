//! Line breaks, line geometry and heights against Chromium, from committed vectors.
//!
//! `tests/data/fidelity.json.gz` holds a corpus (the 10k-message fixture's paragraphs and a
//! hand-written set, at several widths, sizes and themes) and what Chromium (Electron 33)
//! did with each case at scale 1 and 2. The engine lays the same cases out through GPUI's
//! real Linux text system with the bundled fonts, and the two are compared line by line.
//! How the vectors are made: `tools/README.md`.
//!
//! Linux only: the widths depend on the platform's shaper (cosmic-text + harfrust here,
//! DirectWrite on Windows), and the vectors were only checked against this one.

#![cfg(any(target_os = "linux", target_os = "freebsd"))]

mod common;

use std::collections::HashMap;
use std::io::Read as _;
use std::rc::Rc;

use decibell_text::Layout;
use serde_json::Value;

/// Cases whose line breaks are known to differ, with the reason. Anything else that
/// differs fails the test.
const KNOWN: &[(&str, &str)] = &[
    // A line that ends within one LayoutUnit (1/64 device px) of the box's edge: Blink's
    // float arithmetic and ours round differently there.
    ("f15.0/n16/560/body", "edge"),
    ("f65.0/n16/560/body", "edge"),
    ("f1005.0/n/320/body", "edge"),
    // Soft hyphens: when the hyphen does not fit, Blink does not go back to the previous
    // opportunity but breaks the word where the line is full; we go back.
    ("h8.0/n/160/words", "soft hyphen"),
    ("h8.0/g/160/words", "soft hyphen"),
    ("h8.0/n/320/words", "soft hyphen"),
    ("h8.0/n/160/normal", "soft hyphen"),
    ("h8.0/g/160/normal", "soft hyphen"),
    ("h8.0/n/320/normal", "soft hyphen"),
];

#[test]
fn line_breaks_and_heights_match_chromium() {
    let mut packed = Vec::new();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/fidelity.json.gz");
    flate2::read::GzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_end(&mut packed)
        .unwrap();
    let vectors: Value = serde_json::from_slice(&packed).unwrap();
    let engine = common::engine();
    let styles: HashMap<String, common::BodyStyle> = vectors["styles"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, value)| (name.clone(), common::BodyStyle::from_json(value)))
        .collect();
    let texts = vectors["texts"].as_array().unwrap();
    let known: HashMap<&str, &str> = KNOWN.iter().copied().collect();

    for (scale_index, scale) in vectors["scales"].as_array().unwrap().iter().enumerate() {
        let scale = scale.as_f64().unwrap() as f32;
        // (cases, identical) for the fixture's message bodies and for the hand-written set.
        let mut fixture = (0usize, 0usize);
        let mut hand = (0usize, 0usize);
        let mut unexplained = Vec::new();
        let mut widths = Vec::new();
        let mut worst_height = 0.0f32;
        let mut worst_top = 0.0f32;
        for case in vectors["cases"].as_array().unwrap() {
            let segs = texts[case[0].as_u64().unwrap() as usize].as_array().unwrap();
            if !common::comparable(segs) {
                continue;
            }
            let (style, wrap) = (case[1].as_str().unwrap(), case[3].as_str().unwrap());
            let width = case[2].as_f64().unwrap() as f32;
            let id = format!("{}/{style}/{width}/{wrap}", case[4].as_str().unwrap());
            let body = &styles[style];
            let theirs = case[5 + scale_index].as_array().unwrap();
            let their_height = theirs[0].as_f64().unwrap() as f32 / 128.;
            let their_lines: Vec<(u32, f32, Option<f32>)> = theirs[1..]
                .iter()
                .map(|line| {
                    (
                        line[0].as_u64().unwrap() as u32,
                        line[1].as_f64().unwrap() as f32 / 128.,
                        line[2].as_f64().map(|top| top as f32 / 128.),
                    )
                })
                .collect();

            let paragraph = Rc::new(common::paragraph(&engine, body, wrap, segs, scale));
            let layout = Layout::new(paragraph, width, &engine);
            let ours = common::lines(&layout, &engine, body, segs, scale);
            let text = layout.paragraph().text();
            let same = common::line_starts(&ours, text, wrap) == common::line_starts(&their_lines, text, wrap);
            let counter = if id.starts_with('f') { &mut fixture } else { &mut hand };
            counter.0 += 1;
            if !same {
                if !known.contains_key(id.as_str()) {
                    unexplained.push(id);
                }
                continue;
            }
            counter.1 += 1;
            worst_height = worst_height.max((layout.height() - their_height).abs());
            for (ours, theirs) in ours.iter().zip(&their_lines) {
                if theirs.1 > 0. {
                    widths.push((ours.1 - theirs.1).abs());
                }
                if let (Some(ours), Some(theirs)) = (ours.2, theirs.2) {
                    worst_top = worst_top.max((ours - theirs).abs());
                }
            }
        }
        widths.sort_by(f32::total_cmp);
        let p99 = widths[widths.len() * 99 / 100];
        let max = *widths.last().unwrap();
        println!(
            "scale {scale}: fixture {}/{} identical, hand-written {}/{}; line width p99 {p99:.4} max {max:.4} px; \
             height max {worst_height:.4} px; line top max {worst_top:.4} px",
            fixture.1, fixture.0, hand.1, hand.0
        );
        assert!(unexplained.is_empty(), "scale {scale}: line breaks differ from Chromium's in {unexplained:?}");
        assert!(fixture.1 as f64 >= fixture.0 as f64 * 0.9995, "scale {scale}: fixture agreement fell");
        // Where the breaks agree, the paragraph is exactly as high and every line starts at
        // the same y; widths agree to Chromium's own grid (the page reports them snapped).
        assert!(worst_height < 0.002, "scale {scale}: a height differs by {worst_height} px");
        assert!(worst_top < 0.002, "scale {scale}: a line top differs by {worst_top} px");
        assert!(p99 <= 1. / 64. + 0.001, "scale {scale}: line widths drifted (p99 {p99} px)");
        assert!(max < 0.2, "scale {scale}: a line width differs by {max} px");
    }
}

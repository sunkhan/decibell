//! Structural properties over every corpus input (edge, matrix, emoji, fixture, fuzz, URL fuzz).

mod common;

use common::*;
use decibell_richtext::plain::{as_displayed, as_displayed_range, rich_text_to_plain, TextPos};
use decibell_richtext::{composer, js, parse, plain, Block, Document, ParseOptions, SpanKind};

fn corpus() -> Vec<String> {
    let mut v = Vec::new();
    let edge = std::fs::read_to_string(vectors_dir().join("edge.jsonl")).unwrap();
    for line in edge.lines() {
        let x: serde_json::Value = serde_json::from_str(line).unwrap();
        v.push(x[0].as_str().unwrap().to_owned());
    }
    v.extend(matrix());
    v.extend(emoji_inputs());
    v.extend(fixture());
    v.extend(fuzz());
    v.extend(slot_fuzz("urlfuzz.json"));
    v
}

fn check_spans(doc: &Document, src: &str) {
    let mut at = 0u32;
    for (i, sp) in doc.spans().iter().enumerate() {
        let (a, b) = (sp.src.start, sp.src.end);
        assert!(a <= b && b as usize <= src.len(), "span {i} out of range in {src:?}");
        assert!(src.is_char_boundary(a as usize) && src.is_char_boundary(b as usize), "span {i} not on char boundaries in {src:?}");
        assert!(a >= at, "span {i} overlaps or is out of order in {src:?}");
        at = b;
        let t = doc.text(src, sp);
        assert!(!t.is_empty(), "empty span {i} in {src:?}");
        if sp.text_is_source() {
            assert_eq!(t, &src[a as usize..b as usize]);
        }
        if let SpanKind::Link { .. } = sp.kind {
            assert!(sp.text_is_source());
        }
    }
}

#[test]
fn span_invariants() {
    let opts = ParseOptions::default();
    let mut n = 0;
    for src in corpus() {
        let doc = parse(&src, &opts);
        check_spans(&doc, &src);

        // Blocks cover the spans in order, paragraphs are never empty.
        let mut count = 0;
        for b in doc.blocks() {
            match b {
                Block::Paragraph(s) => {
                    assert!(!s.is_empty());
                    assert!(s.iter().all(|x| !x.is_block()));
                    count += s.len();
                }
                Block::Code(s) => {
                    assert!(matches!(s.kind, SpanKind::CodeBlock { .. }));
                    count += 1;
                }
                Block::Math(s) => {
                    assert!(matches!(s.kind, SpanKind::DisplayMath));
                    count += 1;
                }
            }
        }
        assert_eq!(count, doc.spans().len());

        // Spans and hidden ranges tile the source.
        let hidden = composer::hidden_ranges(&doc, &src);
        let mut pieces: Vec<(usize, usize)> = hidden.iter().map(|r| (r.start, r.end)).collect();
        pieces.extend(doc.spans().iter().filter(|s| s.src.start < s.src.end).map(|s| (s.src.start as usize, s.src.end as usize)));
        pieces.sort();
        let mut at = 0;
        for (a, b) in pieces {
            assert_eq!(a, at, "gap or overlap at {a} in {src:?}");
            at = b;
        }
        assert_eq!(at, src.len());

        // An unformatted message is drawn from its raw string (escape backslashes stay):
        // displayed == source with its :shortcodes: expanded.
        if doc.is_plain() {
            assert_eq!(as_displayed(&doc, &src), decibell_richtext::emoji::expand_shortcodes(&src));
            assert!(doc.spans().iter().all(|s| s.style.0 == 0));
        }
        // Jumbo only for unformatted, emoji-only messages.
        if doc.jumbo_size().is_some() {
            assert!(doc.is_plain());
            assert!(doc.spans().iter().any(|s| matches!(s.kind, SpanKind::Emoji { .. })));
            assert!(doc
                .spans()
                .iter()
                .all(|s| matches!(s.kind, SpanKind::Emoji { .. }) || doc.text(&src, s).chars().all(js::is_space)));
        }

        // Selection: the whole range equals as_displayed; source offsets are monotonic.
        let whole = as_displayed_range(&doc, &src, TextPos { span: 0, offset: 0 }, TextPos { span: doc.spans().len(), offset: 0 });
        assert_eq!(whole, as_displayed(&doc, &src));
        let mut last = 0;
        for (i, sp) in doc.spans().iter().enumerate() {
            for off in [0, doc.text(&src, sp).len()] {
                let o = doc.source_offset(TextPos { span: i, offset: off });
                assert!(o >= last && o as usize <= src.len());
                last = o;
            }
        }

        // The preview line is a detached document over its own text.
        let pv = plain::preview(&src);
        let own = pv.own_text().unwrap();
        check_spans(&pv, own);
        assert!(!own.contains('\n'));
        assert_eq!(pv.spans().iter().map(|s| pv.text("", s)).collect::<String>(), own);

        // Composer atoms: ordered, disjoint, inside the text.
        let mut end = 0;
        for a in composer::emoji_atoms(&src) {
            assert!(a.range.0 >= end && a.range.1 <= src.len() && a.range.0 < a.range.1);
            end = a.range.1;
        }
        n += 1;
    }
    println!("span invariants hold over {n} inputs");
}

/// Text with no markup characters and no link scheme parses to one plain run; its preview is the
/// text with whitespace runs collapsed and trimmed (and nothing else changes).
#[test]
fn unmarked_text() {
    let mut rnd = Rng::new(31337);
    let alphabet: Vec<char> = "abcxyz ABC 0189 .,;!?'\"()[]{}-+=/#%&@^|\n\t\r éüıİß日本語 \u{a0}\u{3000}\u{2028}\u{feff}\u{85}שלום"
        .chars()
        .collect();
    for _ in 0..20000 {
        let n = rnd.index(40);
        let s: String = (0..n).map(|_| alphabet[rnd.index(alphabet.len())]).collect();
        let doc = parse(&s, &ParseOptions::default());
        assert!(doc.is_plain(), "{s:?}");
        assert_eq!(as_displayed(&doc, &s), s);
        assert!(!doc.has_formatting());
        assert_eq!(doc.lone_link(&s), None);
        let collapsed: String = {
            let mut out = String::new();
            let mut pending = false;
            for c in s.chars() {
                if js::is_space(c) {
                    pending = !out.is_empty();
                } else {
                    if pending {
                        out.push(' ');
                    }
                    pending = false;
                    out.push(c);
                }
            }
            out
        };
        assert_eq!(rich_text_to_plain(&s), collapsed, "{s:?}");
        assert_eq!(plain::preview(&s).own_text().unwrap(), collapsed);
    }
}

#[test]
fn budget_option() {
    let src = "```a```\n$x$ $$y$$";
    let d0 = parse(src, &ParseOptions { expensive_budget: 0 });
    assert!(d0.spans().iter().all(|s| matches!(s.kind, SpanKind::Text)));
    let d2 = parse(src, &ParseOptions { expensive_budget: 2 });
    let kinds: Vec<_> = d2.spans().iter().map(|s| s.kind).collect();
    assert!(matches!(kinds[0], SpanKind::CodeBlock { lang: None }));
    assert!(kinds.iter().any(|k| matches!(k, SpanKind::Math)));
    assert!(!kinds.iter().any(|k| matches!(k, SpanKind::DisplayMath)));
}

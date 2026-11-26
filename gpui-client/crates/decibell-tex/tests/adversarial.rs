//! Adversarial input: every case must finish quickly and never crash. Deep nesting beyond the
//! port's bound comes back as an internal error (the app shows the raw TeX), macro bombs stop
//! at `maxExpand` as KaTeX's ParseError, size blow-ups clamp to `maxSize`.

use std::time::Instant;

use decibell_tex::{Item, LayoutOptions, Rendered, layout};

fn run(tex: &str, display: bool) -> (Rendered, f64) {
    let t = Instant::now();
    let r = layout(tex, display, &LayoutOptions::default());
    (r, t.elapsed().as_secs_f64())
}

#[test]
fn adversarial_inputs_are_bounded() {
    std::thread::Builder::new().stack_size(64 << 20).spawn(body).unwrap().join().unwrap();
}

fn body() {
    let deep_braces = format!("{}x{}", "{".repeat(10_000), "}".repeat(10_000));
    let deep_frac = format!("{}x{}", "\\frac{".repeat(2_000), "}{y}".repeat(2_000));
    let deep_sup = format!("{}x{}", "x^{".repeat(2_000), "}".repeat(2_000));
    let deep_left = format!("{}x{}", "\\left(".repeat(2_000), "\\right)".repeat(2_000));
    let long_sum: String = (0..2_000).map(|i| format!("x_{{{}}}+", i)).collect();
    let cases: Vec<(String, &str)> = vec![
        (deep_braces, "internal"),
        (deep_frac, "internal"),
        (deep_sup, "internal"),
        (deep_left, "internal"),
        (long_sum, "math"),
        ("\\def\\a{\\b\\b\\b\\b}\\def\\b{\\c\\c\\c\\c}\\def\\c{\\d\\d\\d\\d}\\def\\d{\\a}\\a".into(), "error"),
        ("\\def\\x{x\\x}\\x".into(), "error"),
        ("\\newcommand\\x{\\x}\\x".into(), "error"),
        ("\\rule{1e9em}{1e9em}".into(), "math"),
        ("\\kern99999in x".into(), "math"),
        ("\\raisebox{1e6em}{x}".into(), "math"),
        ("\\Huge\\Huge\\Huge\\sqrt{\\sqrt{\\sqrt{\\left(\\rule{1em}{15em}\\right)}}}".into(), "math"),
        ("\\begin{matrix}".to_string() + &"a&".repeat(3_000) + "\\end{matrix}", "math"),
        ("{".repeat(3) + "\\verb|", "error"),
        ("\u{0}\u{1}\u{7f}".into(), "error"),
    ];
    for (tex, want) in &cases {
        for display in [false, true] {
            let (r, secs) = run(tex, display);
            let got = match &r {
                Rendered::Math(_) => "math",
                Rendered::Error { .. } => "error",
                Rendered::Internal { .. } => "internal",
            };
            assert_eq!(got, *want, "{:?}… (display {}): {:?}", &tex.chars().take(40).collect::<String>(), display, r);
            // Debug build; release is ~20× faster.
            assert!(secs < 2.0, "{:?}… took {:.3} s", &tex.chars().take(40).collect::<String>(), secs);
        }
    }
    // maxSize clamps a huge rule to 16em.
    let Rendered::Math(m) = layout("\\rule{1e9em}{1e9em}", false, &LayoutOptions::default()) else { panic!() };
    let rule_h = m.items.iter().filter_map(|i| if let Item::Rect { h, .. } = i { Some(*h) } else { None }).fold(0f32, f32::max);
    let em = 16.0 * 1.21;
    assert!(rule_h <= 16.0 * em + 1.0, "rule height {} px", rule_h);
}

#[test]
fn everyday_api() {
    let opts = LayoutOptions::default();
    let Rendered::Math(m) = layout("x^2+\\frac{a}{b}", false, &opts) else { panic!() };
    let glyphs: Vec<_> = m.items.iter().filter_map(|i| if let Item::Glyph(g) = i { Some(*g) } else { None }).collect();
    assert_eq!(glyphs.iter().map(|g| g.ch).collect::<String>(), "x2+ba");
    assert_eq!(glyphs[0].font, decibell_tex::KatexFont::MathItalic);
    assert!((glyphs[0].size - 19.359375).abs() < 1e-4);
    assert_eq!(m.bases.len(), 2, "x^2+ | a/b: one break after the binary operator");
    assert!(m.items.iter().any(|i| matches!(i, Item::Rect { .. })), "fraction bar");
    match layout("\\frac{a", false, &opts) {
        Rendered::Error { message } => assert!(message.starts_with("ParseError: KaTeX parse error:")),
        other => panic!("{:?}", other),
    }
    // Undefined commands render (as KaTeX does) as the command in the error colour.
    let Rendered::Math(m) = layout("\\foo", false, &opts) else { panic!() };
    assert!(m.items.iter().all(|i| matches!(i, Item::Glyph(g) if g.color == decibell_tex::layout::Color::Error)));
    // KaTeX throws a plain Error for fonts it has no metrics for.
    assert!(matches!(layout("\\texttt{\\textbf{x}}", false, &opts), Rendered::Internal { .. }));
    // trust:false refuses links.
    let Rendered::Math(m) = layout("\\href{https://example.com}{x}", false, &opts) else { panic!() };
    assert!(m.items.iter().all(|i| matches!(i, Item::Glyph(g) if g.color == decibell_tex::layout::Color::Error)));
}

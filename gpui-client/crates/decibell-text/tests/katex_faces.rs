//! KaTeX's font files mark every face Regular in OS/2 (`fsSelection` 0x0040); fontconfig (and so
//! Chromium) takes the slant from the style name. The engine marks the italic faces before it
//! hands them to GPUI's text system, whose font database reads `fsSelection`: otherwise upright
//! `KaTeX_Main` resolved to the italic face and italic `KaTeX_Math` got a synthetic slant on top
//! (every formula drew slanted, digits and operators included).

#![cfg(any(target_os = "linux", target_os = "freebsd"))]

use std::sync::Arc;

use decibell_text::Engine;
use gpui::{Font, FontFeatures, FontStyle, FontWeight, SharedString, TextSystem, px};

fn font(family: &'static str, style: FontStyle) -> Font {
    Font { family: SharedString::new_static(family), features: FontFeatures::default(), fallbacks: None, weight: FontWeight::NORMAL, style }
}

#[test]
fn upright_and_italic_katex_faces_resolve_to_themselves() {
    let platform = gpui_wgpu::CosmicTextSystem::new_without_system_fonts("Decibell Inter");
    let text_system = Arc::new(TextSystem::new(Arc::new(platform)));
    let engine = Engine::new(text_system.clone());
    // The first formula hands the KaTeX faces to the text system.
    let _ = engine.formula("1 = x", false, 16., 1.);
    let advance = |family: &'static str, style: FontStyle| {
        let id = text_system.resolve_font(&font(family, style));
        (id, f32::from(text_system.advance(id, px(1000.), '1').expect("a 1").width))
    };
    // KaTeX_Main-Regular's "1" is 500 units wide, KaTeX_Main-Italic's 511.
    let (upright, upright_1) = advance("KaTeX_Main", FontStyle::Normal);
    let (italic, italic_1) = advance("KaTeX_Main", FontStyle::Italic);
    assert_ne!(upright, italic);
    assert!((upright_1 - 500.).abs() < 0.5, "upright KaTeX_Main is the Regular face ({upright_1})");
    assert!((italic_1 - 511.).abs() < 0.5, "italic KaTeX_Main is the Italic face ({italic_1})");
    // KaTeX_Math has italic faces only: an italic request takes the face as it is (no synthetic
    // twin), the same font an upright request falls back to.
    assert_eq!(advance("KaTeX_Math", FontStyle::Italic).0, advance("KaTeX_Math", FontStyle::Normal).0, "no synthetic slant on KaTeX_Math-Italic");
}

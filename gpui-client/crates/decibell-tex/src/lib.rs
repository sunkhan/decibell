//! TeX math layout for Decibell's native client: a port of KaTeX 0.18.4 (MIT, see
//! `LICENSE-KATEX`) plus the CSS layout KaTeX leaves to the browser.
//!
//! Pipeline, file by file as in KaTeX: [`lexer`] → [`macro_expander`] → [`parser`] (with
//! [`functions`] and [`environments`]) → [`build_html`] / [`build_common`] (the box tree of
//! [`dom_tree`], the same spans/classes/styles KaTeX's `renderToString` emits) → [`layout`]
//! (what Chromium does with that markup and `katex.css`) → a positioned display list.
//!
//! No GPUI, no I/O. Fonts are named by [`KatexFont`]; a painter loads them lazily from
//! `assets/fonts/katex/`.
//!
//! ```
//! use decibell_tex::{layout, Item, LayoutOptions, Rendered};
//! // Settings are Decibell's (trust: false, maxSize 16, maxExpand 256); text at 16 CSS px,
//! // laid out on Chromium's grid for a 2× display.
//! match layout(r"x^2+\frac{a}{b}", false, &LayoutOptions::default()) {
//!     Rendered::Math(m) => {
//!         // CSS px from the formula's left edge, y down from the first baseline.
//!         for item in &m.items {
//!             match item {
//!                 Item::Glyph(g) => { let _ = (g.font, g.ch, g.x, g.y, g.size, g.color); }
//!                 Item::Rect { .. } => {}  // rules, fraction bars, borders, backgrounds
//!                 Item::Path(p) => { let _ = (&p.paths, p.transform, p.clip); } // SVG
//!             }
//!         }
//!         // Inline formulas may wrap between bases; `lines` is Chromium's line geometry.
//!         let _ = (&m.bases, &m.lines, m.width);
//!     }
//!     Rendered::Error { message } => { let _ = message; } // raw TeX in the error colour
//!     Rendered::Internal { message } => { let _ = message; } // raw TeX as code
//! }
//! ```
//!
//! Coordinates are CSS px for [`LayoutOptions::text_px`]; divide by it for em of the
//! surrounding text (`.katex` itself is 1.21 of that, as in `katex.css`).

pub mod build_common;
pub mod build_html;
pub mod build_tree;
pub mod data;
pub mod define_function;
pub mod delimiter;
pub mod dom_tree;
pub mod environments;
pub mod font_metrics;
pub mod fonts;
pub mod functions;
pub mod layout;
pub mod lexer;
pub mod macro_expander;
pub mod macros;
pub mod namespace;
pub mod options;
pub mod parse_error;
pub mod parse_node;
pub mod parser;
pub mod settings;
pub mod spacing_data;
pub mod stretchy;
pub mod style;
pub mod svg_geometry;
pub mod symbols;
pub mod token;
pub mod unicode_accents;
pub mod unicode_scripts;
pub mod unicode_sup_or_sub;
pub mod unicode_symbols;
pub mod units;
pub mod utils;
pub mod wide_character;

pub use fonts::KatexFont;
pub use layout::{Item, LayoutOptions, MathLayout, Rendered, layout};
pub use settings::Settings;

use build_tree::{error_markup, render_to_dom_tree};
use dom_tree::HtmlNode;

/// KaTeX's `renderToString(tex, settings)` with the MathML half left out (it is invisible),
/// i.e. exactly the markup Chromium lays out. `Err` carries the message of a non-ParseError
/// failure, which KaTeX rethrows (Decibell's renderer then shows the raw TeX as code).
pub fn render_to_markup(tex: &str, settings: &Settings) -> Result<String, String> {
    match render_to_dom_tree(tex, settings)? {
        build_tree::Rendered::Tree(span) => {
            let mut out = String::new();
            HtmlNode::span(span).to_markup(&mut out);
            Ok(out)
        }
        build_tree::Rendered::ParseError { message } => {
            Ok(error_markup(tex, &message, &settings.error_color))
        }
    }
}

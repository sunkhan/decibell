//! Decibell's message rich text for the native client, ported from the Electron renderer
//! (`electron-client/src/features/chat/richText.ts`, `MessageText.tsx`, `LinkEmbeds.tsx`,
//! `Twemoji.tsx`, `inviteLink.ts`, `RichInput.tsx`, `MessagePreview.tsx`). Text in, a compact
//! [`Document`] out: no GPUI, no I/O. A layout crate draws it.
//!
//! The renderer is the specification, oddities included; the differential test
//! (`tests/differential.rs`) holds this crate to it case by case against vectors produced by
//! running the renderer's own code in an Electron 33 window (`tests/vectors/gen.mjs`), so URL
//! questions get Blink's answers, not the WHATWG spec's.
//!
//! # Renderer behaviour kept on purpose (don't "fix" here without changing Electron too)
//!
//! * An unformatted message (the parse is one text node, or nothing) is drawn from its **raw**
//!   string: escape backslashes stay visible (`\*hi\*`), and an empty fence (`` ```\n``` ``)
//!   shows its markers (MessageText.tsx `isPlainText(nodes) || nodes.length === 0` → `content`).
//! * `:shortcodes:` expand before emoji matching, so an expansion can fuse with its neighbours
//!   (`👨‍:woman:` → one ZWJ emoji, `🇦:flag-us:` → 🇦🇺 + 🇸); an unknown `:id:` still eats its
//!   colons. The composer instead matches emoji and shortcodes on the raw text (RichInput.tsx).
//! * Twemoji assets are looked up with U+FE0F dropped from every multi-code-point sequence, so
//!   841 of the 3,720 files (ZWJ sequences that keep FE0F) are never used: those emoji fall back
//!   to the system font.
//! * Past 10 code blocks / math spans a message draws the rest as inline code (newlines and all).
//! * A code block or display math inside emphasis keeps the emphasis style (it renders inside
//!   `<strong>` etc.); an emphasis whose content renders nothing still counts as formatting.
//! * The one-line preview drops markers first, so it expands shortcodes and draws emoji inside
//!   code, math and URLs; a lone GIF / image link previews as "GIF" / "Image".
//! * Autolinks: not after an ASCII letter/digit; run to whitespace / `<>"\``; shed `.,:;!?'"`
//!   and unbalanced `)]}` until stable; at most 2048 UTF-16 units; host by Blink (an emoji glued
//!   to `https://x.com` becomes part of the link; spaces mapped from IDNA, ZWJ and `%20` are
//!   accepted; IDNA data is Unicode 15.1).
//! * Whitespace is JavaScript's: U+FEFF counts, U+0085 doesn't (jumbo, trims, `\s` collapsing).

//!
//! ```
//! use decibell_richtext::{parse, ParseOptions, Block};
//! let src = "**hi** :smile: https://example.com/a.gif";
//! let doc = parse(src, &ParseOptions::default());
//! for block in doc.blocks() {
//!     if let Block::Paragraph(spans) = block {
//!         for s in spans {
//!             let _text = doc.text(src, s); // "hi", " ", "😄", " ", "https://…"
//!         }
//!     }
//! }
//! assert_eq!(doc.embed_links(src, 3), vec!["https://example.com/a.gif"]);
//! ```

mod doc;
pub mod emoji;
pub mod js;
mod lang;
pub mod links;
mod parse;
pub mod plain;
pub mod composer;
#[rustfmt::skip]
mod tables;
mod url;
#[rustfmt::skip]
mod url_tables;

pub use doc::{parse, Block, Blocks, Document, ParseOptions, Span, SpanKind, Style};
pub use emoji::EmojiAsset;
pub use lang::Lang;
pub use links::{EmbedKind, Invite, MediaLabel};

/// MessageText.tsx `INLINE_EMOJI_SIZE`: emoji in running text, px.
pub const INLINE_EMOJI_SIZE: u8 = 24;

/// MessageText.tsx `jumboSizeFor`: an unformatted message that is only emoji (whitespace
/// aside) draws them at 56 / 44 / 38 / 32 px for 1 / 2 / 3 / more. `None` = inline size.
pub fn jumbo_size(doc: &Document) -> Option<u8> {
    doc.jumbo
}

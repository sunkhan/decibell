//! Decibell's text engine. See `README.md` for the concepts and examples.

mod blink_breaks;
mod cache;
mod engine;
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod fallback;
mod font;
mod hit;
mod layout;
mod paint;
mod paragraph;
mod style;

mod code;
mod emoji;
pub mod input;
mod label;
mod math;
mod rich;

pub use cache::{Generations, KeyHasher, LayoutCache, layout_key};
pub use engine::{Engine, Stats};
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub use fallback::prewarm_system_fonts;
pub use font::{Face, FontSpec};
pub use hit::{Affinity, Hit, Pos};
pub use layout::{Layout, LineInfo};
pub use paint::PaintOptions;
pub use paragraph::{OBJECT_REPLACEMENT, Paragraph, ParagraphBuilder};
pub use style::{Chip, LineHeight, OverflowWrap, TextStyle, WhiteSpace, WordBreak, Wrapping};
pub use code::CodeStyle;
pub use emoji::{EmojiStore, emoji_svg, packed_emoji_bytes, packed_emoji_count, rasterize_emoji};
pub use label::{Label, MathBlock, TextBlock};
pub use math::{Formula, Math, MathColors};
pub use rich::{
    EDITED_TAG, RichContent, RichHit, RichLayout, RichPaint, RichPos, RichSelection, RichStyle, RichText, Slot,
    SlotKind,
};

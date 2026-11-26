//! Code-block languages as CodeBlock.tsx sees them: the fence's tag is lower-cased by the parser,
//! and it is "known" when `lowlight.registered(tag)` (a grammar name or alias registered there).

use crate::tables::{GRAMMARS, LANG_ALIASES};

/// A known code-block language: one of the names / aliases CodeBlock.tsx registers.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Lang(pub(crate) u8);

impl Lang {
    /// Look up a fence tag (already lower-case, as the parser makes it). `None` = the block
    /// shows "code" and is highlighted as plain text.
    pub fn from_tag(tag: &str) -> Option<Lang> {
        LANG_ALIASES.binary_search_by(|(a, _)| (*a).cmp(tag)).ok().map(|i| Lang(i as u8))
    }

    /// The tag as typed, lower-cased: what the code block's header shows (upper-cased by CSS).
    pub fn tag(self) -> &'static str {
        LANG_ALIASES[self.0 as usize].0
    }

    /// The highlight.js grammar it resolves to (`javascript` for `js`, `ini` for `toml`, …).
    pub fn grammar(self) -> &'static str {
        GRAMMARS[LANG_ALIASES[self.0 as usize].1 as usize]
    }

    /// Every tag CodeBlock.tsx accepts, sorted.
    pub fn all() -> impl Iterator<Item = Lang> {
        (0..LANG_ALIASES.len()).map(|i| Lang(i as u8))
    }
}

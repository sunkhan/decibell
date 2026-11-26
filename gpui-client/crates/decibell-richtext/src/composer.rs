//! What the message composer takes from the grammar.
//!
//! Electron's composer (`RichInput.tsx`, a contentEditable) does **not** style markdown in
//! place: it turns emoji and known `:shortcode:`s into emoji atoms as you type, and
//! `MessagePreview.tsx` shows the draft rendered exactly like a sent message (the same
//! [`crate::parse`] / MessageText output) in a strip above the input, only when the draft has
//! formatting beyond plain text and autolinks. Enter sends, except inside an unclosed ``` fence.

use crate::doc::{with_events, Document};
use crate::emoji::{self, EmojiAsset};
use std::ops::Range;

/// MessagePreview.tsx: show the live preview strip for this draft (non-empty and
/// `hasFormatting(parseRichTextUncached(draft))`). Render it with [`crate::parse`].
pub fn preview_visible(draft: &str) -> bool {
    !draft.is_empty() && has_formatting(draft)
}

/// richText.ts `hasFormatting` straight from a string (no document built).
pub fn has_formatting(text: &str) -> bool {
    with_events(text, |ev| ev.has_formatting())
}

/// RichInput.tsx `hasOpenCodeFence`: an odd number of non-overlapping ``` (escapes ignored).
/// While true, plain Enter inserts a line break instead of sending.
pub fn has_open_code_fence(text: &str) -> bool {
    text.matches("```").count() % 2 == 1
}

/// An emoji atom the composer makes from typed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Atom<'a> {
    /// Bytes of the text node it replaces.
    pub range: (usize, usize),
    /// What it serializes to (and draws): the matched emoji, or a shortcode's emoji.
    pub native: &'a str,
    pub asset: Option<EmojiAsset>,
}

impl Atom<'_> {
    pub fn range(&self) -> Range<usize> {
        self.range.0..self.range.1
    }
}

/// RichInput.tsx `tokenize` over one text node: emoji-regex matches and known `:shortcode:`s on
/// the raw text (no expansion first, unlike the message renderer), sorted by start (emoji first
/// on a tie), overlaps dropped left to right.
pub fn emoji_atoms(text: &str) -> Vec<Atom<'_>> {
    let mut m: Vec<Atom<'_>> = emoji::find_iter(text)
        .map(|r| Atom { range: (r.start, r.end), native: &text[r.clone()], asset: None })
        .collect();
    if text.contains(':') {
        for sc in emoji::shortcode_matches(text) {
            if let Some(native) = emoji::shortcode(sc.id(text)) {
                m.push(Atom { range: (sc.start, sc.end), native, asset: None });
            }
        }
        m.sort_by_key(|a| a.range.0);
    }
    let mut out: Vec<Atom<'_>> = Vec::with_capacity(m.len());
    let mut last_end = 0;
    for mut a in m {
        if a.range.0 >= last_end {
            last_end = a.range.1;
            a.asset = emoji::twemoji_asset(a.native);
            out.push(a);
        }
    }
    out
}

/// RichInput.tsx `hasTokenizable`: does the text hold an emoji or a known `:shortcode:`?
pub fn has_tokenizable(text: &str) -> bool {
    if emoji::contains_emoji(text) {
        return true;
    }
    text.contains(':') && emoji::shortcode_matches(text).any(|m| emoji::shortcode(m.id(text)).is_some())
}

/// Source ranges the rendered message does not show: emphasis / code / math markers, fences and
/// their tags, escape backslashes, `<>` around a no-preview link, swallowed newlines. For a
/// composer that wants to show the raw draft with its markers dimmed (Electron's does not).
pub fn hidden_ranges(doc: &Document, src: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for s in doc.spans() {
        let (a, b) = (s.src.start as usize, s.src.end as usize);
        if a > at {
            out.push(at..a);
        }
        at = at.max(b);
    }
    if at < src.len() {
        out.push(at..src.len());
    }
    out
}

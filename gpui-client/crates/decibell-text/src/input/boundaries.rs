//! Caret stops in plain text: grapheme clusters and words, as Chromium moves over them.
//!
//! Words follow Blink's `visible_units_word.cc`: the candidates are the UAX #29 word
//! boundaries (ICU's word break iterator there, `unicode-segmentation` here), and a move
//! stops at the first one that touches a letter, a digit or `_` on the far side: Ctrl+Left
//! lands before a word, Ctrl+Right after one (Linux, macOS) or before the next one
//! (Windows, which also skips the spaces that follow). The vectors in
//! `tests/data/chromium-input.json` were recorded from Chromium with real key events.
//!
//! Everything here is byte offsets into a `&str`, always on a `char` boundary.

use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// The grapheme boundary after `offset` (the text's length when there is none).
pub fn next_grapheme(text: &str, offset: usize) -> usize {
    if offset >= text.len() {
        return text.len();
    }
    let mut cursor = GraphemeCursor::new(offset, text.len(), true);
    cursor.next_boundary(text, 0).ok().flatten().unwrap_or(text.len())
}

/// The grapheme boundary before `offset` (0 when there is none).
pub fn prev_grapheme(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let mut cursor = GraphemeCursor::new(offset.min(text.len()), text.len(), true);
    cursor.prev_boundary(text, 0).ok().flatten().unwrap_or(0)
}

/// `offset` moved back to a grapheme boundary (itself when it is one).
pub fn floor_grapheme(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let mut cursor = GraphemeCursor::new(offset, text.len(), true);
    if cursor.is_boundary(text, 0).unwrap_or(true) { offset } else { prev_grapheme(text, offset) }
}

/// Blink's `IsAlphanumeric(c) || c == '_'`: what makes a word boundary a stop.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where Ctrl+Right goes from `offset`: the first word boundary after it that follows a
/// word character (the end of a word). With `skip_space` (Windows) the white space after
/// that word is passed too, so the caret lands before the next word.
pub fn next_word(text: &str, offset: usize, skip_space: bool) -> usize {
    let mut found = text.len();
    for (start, word) in text.split_word_bound_indices() {
        let end = start + word.len();
        if end > offset && word.chars().next_back().is_some_and(is_word_char) {
            found = end;
            break;
        }
    }
    if skip_space {
        for c in text[found..].chars() {
            if !c.is_whitespace() || c == '\n' {
                break;
            }
            found += c.len_utf8();
        }
    }
    found
}

/// Where Ctrl+Left goes from `offset`: the last word boundary before it that precedes a
/// word character (the start of a word).
pub fn prev_word(text: &str, offset: usize) -> usize {
    let mut found = 0;
    for (start, word) in text.split_word_bound_indices() {
        if start >= offset {
            break;
        }
        if word.chars().next().is_some_and(is_word_char) {
            found = start;
        }
    }
    found
}

/// The word a double click at `offset` selects: the UAX #29 segment there (a word, a run
/// of white space, one punctuation mark). At the end of the text, the last segment.
pub fn word_at(text: &str, offset: usize) -> (usize, usize) {
    let mut last = (0, 0);
    for (start, word) in text.split_word_bound_indices() {
        let end = start + word.len();
        last = (start, end);
        if offset < end {
            break;
        }
    }
    last
}

/// The hard line (between `\n`s) around `offset`, without its line break.
pub fn line_at(text: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(text.len());
    let start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    let end = text[offset..].find('\n').map_or(text.len(), |at| offset + at);
    (start, end)
}

/// UTF-16 code units in `text`.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// A UTF-16 offset → the byte offset of the same place (clamped to the text; an offset
/// inside a surrogate pair goes to the pair's start).
pub fn utf16_to_byte(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units + c.len_utf16() > utf16 {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// A byte offset → UTF-16 code units before it.
pub fn byte_to_utf16(text: &str, byte: usize) -> usize {
    utf16_len(&text[..byte.min(text.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every caret stop of `text`, walking right from 0.
    fn stops(text: &str) -> Vec<usize> {
        let mut stops = vec![0];
        while *stops.last().unwrap() < text.len() {
            stops.push(next_grapheme(text, *stops.last().unwrap()));
        }
        stops
    }

    #[test]
    fn graphemes_are_one_step() {
        // A ZWJ family, a flag, a keycap, a skin tone: one step each.
        for cluster in ["👨‍👩‍👧‍👦", "🇹🇷", "1️⃣", "👍🏽", "e\u{301}", "ğ", "İ", "ı", "한", "กำ"] {
            let text = format!("a{cluster}b");
            assert_eq!(stops(&text), vec![0, 1, 1 + cluster.len(), 2 + cluster.len()], "{cluster:?}");
            assert_eq!(prev_grapheme(&text, 1 + cluster.len()), 1, "{cluster:?}");
        }
        // Combining marks stay with their base, also stacked ones.
        assert_eq!(stops("a\u{308}\u{301}b"), vec![0, 5, 6]);
        // CJK: one ideograph, one step. CRLF: one step.
        assert_eq!(stops("日本語"), vec![0, 3, 6, 9]);
        assert_eq!(stops("a\r\nb"), vec![0, 1, 3, 4]);
        assert_eq!(prev_grapheme("", 0), 0);
        assert_eq!(next_grapheme("x", 5), 1);
    }

    #[test]
    fn floor_lands_on_a_cluster_start() {
        let text = "a👨‍👩‍👧b";
        for inside in 2..text.len() - 1 {
            assert_eq!(floor_grapheme(text, inside), 1, "{inside}");
        }
        assert_eq!(floor_grapheme(text, text.len() - 1), text.len() - 1);
        assert_eq!(floor_grapheme(text, 99), text.len());
    }

    #[test]
    fn words_stop_where_blink_stops() {
        let text = "hello, wörld_1  foo.bar";
        //          0123456789…  (ö is 2 bytes)
        let right: Vec<usize> = std::iter::successors(Some(0), |&at| (at < text.len()).then(|| next_word(text, at, false))).collect();
        assert_eq!(right, vec![0, 5, 15, 24]);
        let left: Vec<usize> = std::iter::successors(Some(text.len()), |&at| (at > 0).then(|| prev_word(text, at))).collect();
        assert_eq!(left, vec![24, 17, 7, 0]);
        // Windows: Ctrl+Right lands before the next word.
        assert_eq!(next_word(text, 0, true), 5);
        assert_eq!(next_word("one  two", 0, true), 5);
        // Punctuation alone is never a stop; the ends of the text are.
        assert_eq!(next_word("a ... ", 1, false), 6);
        assert_eq!(prev_word("... a", 3), 0);
        // Turkish letters are letters.
        assert_eq!(next_word("ışık ığdır", 0, false), "ışık".len());
        assert_eq!(prev_word("İstanbul şehir", "İstanbul şehir".len()), "İstanbul ".len());
    }

    #[test]
    fn double_click_words_and_lines() {
        assert_eq!(word_at("one two", 1), (0, 3));
        assert_eq!(word_at("one  two", 3), (3, 5));
        assert_eq!(word_at("one two", 7), (4, 7));
        assert_eq!(word_at("a.b", 1), (1, 2));
        assert_eq!(line_at("ab\ncd\n", 1), (0, 2));
        assert_eq!(line_at("ab\ncd\n", 3), (3, 5));
        assert_eq!(line_at("ab\ncd\n", 6), (6, 6));
        assert_eq!(line_at("ab\ncd\n", 2), (0, 2));
    }

    #[test]
    fn utf16_round_trips() {
        let text = "a😀ğ日";
        assert_eq!(utf16_len(text), 1 + 2 + 1 + 1);
        for (byte, _) in text.char_indices().chain([(text.len(), ' ')]) {
            assert_eq!(utf16_to_byte(text, byte_to_utf16(text, byte)), byte);
        }
        // Inside the surrogate pair: the pair's start.
        assert_eq!(utf16_to_byte(text, 2), 1);
        assert_eq!(utf16_to_byte(text, 99), text.len());
    }
}

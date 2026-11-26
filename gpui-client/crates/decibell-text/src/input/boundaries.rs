//! Caret stops in plain text: grapheme clusters and words, as Chromium moves over them.
//!
//! Word stops are Chromium's on Linux, recorded with real key events
//! (`tests/data/chromium-input.json`, checked by `tests/input_vectors.rs`): spaces are
//! skipped, and a word, a run of punctuation and a line break are each one stop. Ctrl+Left
//! lands before one, Ctrl+Right after one (Linux, macOS) or before the next (Windows).
//! Chromium finds words in Chinese, Japanese and Thai text with ICU's dictionaries; here a
//! run of such letters is one word (the dictionaries would add about 2 MB to the binary).
//!
//! Everything here is byte offsets into a `&str`, always on a `char` boundary.

use unicode_segmentation::GraphemeCursor;

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

/// What a character is to word movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Class {
    /// Letters, digits, `_`, and the marks that continue them.
    Word,
    /// Spaces and tabs: skipped.
    Space,
    /// A line break: a stop of its own.
    Newline,
    /// Everything else (punctuation, symbols, emoji): a run of it is one stop.
    Other,
}

fn class(c: char) -> Class {
    if c == '\n' {
        Class::Newline
    } else if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Other
    }
}

/// Whether `c`, between `before` and `after`, stays inside a word: an apostrophe between
/// letters ("don't"), a full stop or comma between digits ("3.14", "1,000"). Chromium's
/// word breaks are ICU's with the full stop and the colon taken out of the characters
/// that join letters, so "example.com" is three stops.
fn joins(before: char, c: char, after: char) -> bool {
    match c {
        '\'' | '’' => before.is_alphabetic() && after.is_alphabetic(),
        '.' | ',' => before.is_numeric() && after.is_numeric(),
        _ => false,
    }
}

/// Combining marks and joiners: they belong to whatever they follow.
fn continues(c: char) -> bool {
    matches!(c, '\u{300}'..='\u{36f}' | '\u{200d}' | '\u{fe00}'..='\u{fe0f}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{20e3}')
}

/// The end of the run of one class that starts at `offset`.
fn run_end(text: &str, offset: usize) -> usize {
    let mut chars = text[offset..].char_indices().peekable();
    let Some((_, first)) = chars.next() else {
        return offset;
    };
    let kind = class(first);
    if kind == Class::Newline {
        return offset + 1;
    }
    let mut before = first;
    while let Some((at, c)) = chars.next() {
        let same = class(c) == kind || continues(c);
        let joined = kind == Class::Word && chars.peek().is_some_and(|(_, after)| joins(before, c, *after));
        if !same && !joined {
            return offset + at;
        }
        before = c;
    }
    text.len()
}

/// The start of the run of one class that ends at `offset`.
fn run_start(text: &str, offset: usize) -> usize {
    // Walk the runs from the line's start: a run is short, and so is a line.
    let line = text[..offset].rfind('\n').map_or(0, |at| at + 1);
    if line == offset {
        return offset.saturating_sub(1);
    }
    let mut start = line;
    loop {
        let end = run_end(text, start);
        if end >= offset {
            return start;
        }
        start = end;
    }
}

/// Where Ctrl+Right goes from `offset` (Chromium on Linux, recorded with real key events:
/// `tests/data/chromium-input.json`): past any spaces, then to the end of the next word,
/// of the next run of punctuation, or past one line break. With `skip_space` (Windows,
/// whose convention is to land before the next word) the spaces after that are passed too.
pub fn next_word(text: &str, offset: usize, skip_space: bool) -> usize {
    let spaces = |from: usize| -> usize {
        from + text[from..].chars().take_while(|c| class(*c) == Class::Space).map(char::len_utf8).sum::<usize>()
    };
    let mut found = run_end(text, spaces(offset.min(text.len())));
    if skip_space {
        found = spaces(found);
    }
    found
}

/// Where Ctrl+Left goes from `offset`: back over any spaces, then to the start of the word
/// or run of punctuation before them, or back over one line break.
pub fn prev_word(text: &str, offset: usize) -> usize {
    let mut at = offset.min(text.len());
    while let Some(c) = text[..at].chars().next_back().filter(|c| class(*c) == Class::Space) {
        at -= c.len_utf8();
    }
    if at == 0 { 0 } else { run_start(text, at) }
}

/// The word a double click at `offset` selects: the word there, the run of spaces there,
/// or the one punctuation mark (grapheme) there. At the end of the text, what is before it.
pub fn word_at(text: &str, offset: usize) -> (usize, usize) {
    if text.is_empty() {
        return (0, 0);
    }
    let offset = if offset >= text.len() { prev_grapheme(text, text.len()) } else { floor_grapheme(text, offset) };
    let c = text[offset..].chars().next().unwrap_or(' ');
    match class(c) {
        Class::Other | Class::Newline => (offset, next_grapheme(text, offset)),
        kind => {
            // The run of this class around the offset, on its line.
            let line = text[..offset].rfind('\n').map_or(0, |at| at + 1);
            let mut start = line;
            loop {
                let end = if class(text[start..].chars().next().unwrap_or(' ')) == Class::Space {
                    start + text[start..].chars().take_while(|c| class(*c) == Class::Space).map(char::len_utf8).sum::<usize>()
                } else {
                    run_end(text, start)
                };
                if end > offset {
                    debug_assert!(kind != Class::Other);
                    return (start, end);
                }
                start = end;
            }
        }
    }
}

/// Where Backspace deletes from, with the caret at `offset`: Chromium takes one code point
/// off a combining sequence (the accent of "é" typed as e + U+0301, the vowel sign of a
/// Thai syllable) but a whole emoji sequence (a family, a flag, a keycap, a skin tone).
/// Delete, by contrast, always takes the whole grapheme ([`next_grapheme`]).
pub fn backspace_start(text: &str, offset: usize) -> usize {
    let start = prev_grapheme(text, offset);
    let cluster = &text[start..offset.min(text.len())];
    let mut chars = cluster.chars();
    let last = chars.next_back();
    if chars.next().is_none() {
        return start;
    }
    let emoji = cluster.chars().any(|c| {
        matches!(c, '\u{200d}' | '\u{20e3}' | '\u{fe0e}' | '\u{fe0f}' | '\u{1f1e6}'..='\u{1f1ff}' | '\u{1f3fb}'..='\u{1f3ff}' | '\u{e0020}'..='\u{e007f}')
    });
    if emoji || cluster == "\r\n" {
        return start;
    }
    offset - last.map_or(0, char::len_utf8)
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
    fn words_stop_where_chromium_stops() {
        let right = |text: &str| -> Vec<usize> {
            std::iter::successors(Some(0), |&at| (at < text.len()).then(|| next_word(text, at, false))).collect()
        };
        let left = |text: &str| -> Vec<usize> {
            std::iter::successors(Some(text.len()), |&at| (at > 0).then(|| prev_word(text, at))).collect()
        };
        // (The full set is in tests/input_vectors.rs; these are ASCII, so bytes = UTF-16.)
        assert_eq!(right("hello, world_1  foo.bar"), [0, 5, 6, 14, 19, 20, 23]);
        assert_eq!(left("hello, world_1  foo.bar"), [23, 20, 19, 16, 7, 5, 0]);
        assert_eq!(right("a ... b"), [0, 1, 5, 7]);
        assert_eq!(left("a ... b"), [7, 6, 2, 0]);
        assert_eq!(right("line one\nline two\n\nfour"), [0, 4, 8, 9, 13, 17, 18, 19, 23]);
        assert_eq!(left("line one\nline two\n\nfour"), [23, 19, 18, 17, 14, 9, 8, 5, 0]);
        assert_eq!(right("3.14 1,000 v2.0 don't"), [0, 4, 10, 15, 21]);
        assert_eq!(right("http://a.b"), [0, 4, 7, 8, 9, 10]);
        // Windows: Ctrl+Right lands before the next word.
        assert_eq!(next_word("one  two", 0, true), 5);
        // Turkish letters are letters.
        assert_eq!(next_word("ışık ığdır", 0, false), "ışık".len());
        assert_eq!(prev_word("İstanbul şehir", "İstanbul şehir".len()), "İstanbul ".len());
    }

    #[test]
    fn double_click_words_and_lines() {
        assert_eq!(word_at("one two", 1), (0, 3));
        assert_eq!(word_at("one  two", 3), (3, 5));
        assert_eq!(word_at("one two", 7), (4, 7));
        assert_eq!(word_at("a, b", 1), (1, 2));
        assert_eq!(word_at("example.com", 2), (0, 7));
        assert_eq!(word_at("don't", 4), (0, 5));
        assert_eq!(word_at("a\nb", 1), (1, 2));
        assert_eq!(word_at("", 0), (0, 0));
        assert_eq!(line_at("ab\ncd\n", 1), (0, 2));
        assert_eq!(line_at("ab\ncd\n", 3), (3, 5));
        assert_eq!(line_at("ab\ncd\n", 6), (6, 6));
        assert_eq!(line_at("ab\ncd\n", 2), (0, 2));
    }

    #[test]
    fn backspace_takes_a_code_point_or_a_whole_emoji() {
        let after = |text: &str| text[..backspace_start(text, text.len())].to_string();
        assert_eq!(after("e\u{301}"), "e");
        assert_eq!(after("a\u{308}\u{301}"), "a\u{308}");
        assert_eq!(after("กำ"), "ก");
        assert_eq!(after("한"), "");
        for emoji in ["👨‍👩‍👧‍👦", "🇹🇷", "1️⃣", "👍🏽", "😀"] {
            assert_eq!(after(&format!("a{emoji}")), "a", "{emoji}");
        }
        assert_eq!(after("ab"), "a");
        assert_eq!(after(""), "");
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

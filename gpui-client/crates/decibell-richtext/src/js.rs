//! The bits of JavaScript string semantics the renderer's code leans on.

/// JS `\s` / `String.prototype.trim` whitespace: WhiteSpace + LineTerminator. Differs from
/// `char::is_whitespace` (Unicode White_Space): JS has U+FEFF, Rust has U+0085.
#[inline]
pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
            | '\r'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
#[inline]
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `String.prototype.length` (UTF-16 code units).
#[inline]
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// The char starting at byte `i` (a char boundary), if any.
#[inline]
pub(crate) fn char_at(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|t| t.chars().next())
}

/// The char ending at byte `i` (a char boundary), if any.
#[inline]
pub(crate) fn char_before(s: &str, i: usize) -> Option<char> {
    s.get(..i).and_then(|t| t.chars().next_back())
}

/// `/[A-Za-z0-9]/` on the UTF-16 unit at `i` (non-ASCII is never a word char, so a byte test
/// on UTF-8 gives the same answer).
#[inline]
pub(crate) fn is_word_byte(b: &[u8], i: isize) -> bool {
    i >= 0 && (i as usize) < b.len() && b[i as usize].is_ascii_alphanumeric()
}

//! Port of KaTeX 0.18.4 `src/unicodeAccents.js`: combining accents the parser turns into
//! accent commands (text-mode command, and the math-mode one where it differs).

use crate::data::unicode_data::UNICODE_ACCENTS;

/// `unicodeAccents[c]` as `(text, math)`. KaTeX uses `math || text` in math mode.
pub fn get(c: char) -> Option<(&'static str, Option<&'static str>)> {
    UNICODE_ACCENTS
        .binary_search_by(|(k, _, _)| k.cmp(&c))
        .ok()
        .map(|i| (UNICODE_ACCENTS[i].1, UNICODE_ACCENTS[i].2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert!(UNICODE_ACCENTS.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(get('\u{0301}'), Some(("\\'", Some("\\acute"))));
        assert_eq!(get('\u{0327}'), Some(("\\c", None)));
        assert_eq!(get('a'), None);
    }
}

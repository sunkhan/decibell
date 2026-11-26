//! Port of KaTeX 0.18.4 `src/unicodeSymbols.js`: precomposed accented letters (Latin and
//! Greek) → the base letter followed by its combining accents, which the parser then builds as
//! accent nodes. The table is the evaluated object (it is computed with `String.normalize`).

use crate::data::unicode_data::UNICODE_SYMBOLS;

/// `unicodeSymbols[c]`.
pub fn get(c: char) -> Option<&'static str> {
    UNICODE_SYMBOLS
        .binary_search_by(|(k, _)| k.cmp(&c))
        .ok()
        .map(|i| UNICODE_SYMBOLS[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert!(UNICODE_SYMBOLS.windows(2).all(|w| w[0].0 < w[1].0));
        assert_eq!(get('\u{e1}'), Some("a\u{301}"));
        assert_eq!(get('a'), None);
    }
}

//! Port of KaTeX 0.18.4 `src/unicodeSupOrSub.ts`: Unicode super/subscript characters (², ₙ, …)
//! the parser turns into `^{…}` / `_{…}`.

use crate::data::unicode_data::{SUB_CHARS, SUBS_AND_SUPS};

/// `uSubsAndSups[text]`: only a token that is exactly one of the characters matches (a token
/// with trailing combining marks does not, as in KaTeX).
pub fn get(text: &str) -> Option<&'static str> {
    let mut it = text.chars();
    let c = it.next()?;
    if it.next().is_some() {
        return None;
    }
    SUBS_AND_SUPS
        .binary_search_by(|(k, _)| k.cmp(&c))
        .ok()
        .map(|i| SUBS_AND_SUPS[i].1)
}

/// `unicodeSubRegEx.test(text)`: the regex is anchored at the start and tests the first
/// character only.
pub fn is_sub(text: &str) -> bool {
    match text.chars().next() {
        Some(c) => SUB_CHARS.binary_search(&c).is_ok(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert!(SUBS_AND_SUPS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(SUB_CHARS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(get("²"), Some("2"));
        assert_eq!(get("₂"), Some("2"));
        assert_eq!(get("²\u{301}"), None);
        assert_eq!(get("x"), None);
        assert!(is_sub("₂"));
        assert!(is_sub("ₙx"));
        assert!(!is_sub("²"));
        assert!(!is_sub(""));
    }
}

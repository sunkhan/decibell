//! Port of KaTeX 0.18.4 `src/unicodeScripts.ts`: the Unicode scripts KaTeX accepts in `\text{}`
//! without font metrics (their glyphs come from a fallback font). KaTeX calls these with UTF-16
//! code units (`charCodeAt(0)`); callers pass the code unit, so astral characters never match.

/// `scriptData`: script (family) name and its inclusive code point blocks, in KaTeX's order.
const SCRIPT_DATA: &[(&str, &[(u32, u32)])] = &[
    (
        "latin",
        &[
            (0x0100, 0x024f), // Latin Extended-A and Latin Extended-B
            (0x0300, 0x036f), // Combining Diacritical marks
        ],
    ),
    ("cyrillic", &[(0x0400, 0x04ff)]),
    ("armenian", &[(0x0530, 0x058F)]),
    ("brahmic", &[(0x0900, 0x109F)]),
    ("georgian", &[(0x10A0, 0x10ff)]),
    (
        "cjk",
        &[
            (0x3000, 0x30FF), // CJK symbols and punctuation, Hiragana, Katakana
            (0x4E00, 0x9FAF), // CJK ideograms
            (0xFF00, 0xFF60), // Fullwidth punctuation
        ],
    ),
    ("hangul", &[(0xAC00, 0xD7AF)]),
];

/// `scriptFromCodepoint`: the script (family) of `codepoint`, if it is in a supported block.
pub fn script_from_codepoint(codepoint: u32) -> Option<&'static str> {
    SCRIPT_DATA
        .iter()
        .find(|(_, blocks)| {
            blocks
                .iter()
                .any(|&(lo, hi)| codepoint >= lo && codepoint <= hi)
        })
        .map(|(name, _)| *name)
}

/// `supportedCodepoint`: `codepoint` falls in one of the supported blocks.
pub fn supported_codepoint(codepoint: u32) -> bool {
    script_from_codepoint(codepoint).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts() {
        assert_eq!(script_from_codepoint(0x0101), Some("latin"));
        assert_eq!(script_from_codepoint(0x0410), Some("cyrillic"));
        assert_eq!(script_from_codepoint(0x4E2D), Some("cjk"));
        assert_eq!(script_from_codepoint(0xAC00), Some("hangul"));
        assert_eq!(script_from_codepoint(0x41), None);
        assert_eq!(script_from_codepoint(0xD835), None);
        assert!(supported_codepoint(0x0915));
        assert!(!supported_codepoint(0x03B1));
    }
}

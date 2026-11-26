//! Port of KaTeX 0.18.4 `src/wide-character.ts`: the font and CSS class for the Unicode
//! Mathematical Alphanumeric Symbols (U+1D400–U+1D7FF), which `makeOrd` draws from KaTeX's own
//! fonts. The caller has already checked that the first UTF-16 unit is 0xD835.

use crate::fonts::FontName;
use crate::parse_node::Mode;

/// `WideChar`: `(font, mathClass, textClass)`; `font` is `None` for KaTeX's `""` (no font).
type WideChar = (Option<FontName>, &'static str, &'static str);

const BOLD_UPRIGHT: WideChar = (Some(FontName::MainBold), "mathbf", "textbf");
const ITALIC: WideChar = (Some(FontName::MathItalic), "mathnormal", "textit");
const BOLD_ITALIC: WideChar = (Some(FontName::MainBoldItalic), "boldsymbol", "boldsymbol");
const SCRIPT: WideChar = (Some(FontName::ScriptRegular), "mathscr", "textscr");
const NO_FONT: WideChar = (None, "", "");
const FRAKTUR: WideChar = (Some(FontName::FrakturRegular), "mathfrak", "textfrak");
const DOUBLE_STRUCK: WideChar = (Some(FontName::AmsRegular), "mathbb", "textbb");
// Note that KaTeX uses a bold font, but font metrics for regular Fraktur.
const BOLD_FRAKTUR: WideChar = (
    Some(FontName::FrakturRegular),
    "mathboldfrak",
    "textboldfrak",
);
const SANS_SERIF: WideChar = (Some(FontName::SansSerifRegular), "mathsf", "textsf");
const BOLD_SANS_SERIF: WideChar = (Some(FontName::SansSerifBold), "mathboldsf", "textboldsf");
const ITALIC_SANS_SERIF: WideChar = (Some(FontName::SansSerifItalic), "mathitsf", "textitsf");
const MONOSPACE: WideChar = (Some(FontName::TypewriterRegular), "mathtt", "texttt");

/// 26 characters per row.
#[rustfmt::skip]
const WIDE_LATIN_LETTER_DATA: [WideChar; 26] = [
    BOLD_UPRIGHT, BOLD_UPRIGHT, // A-Z, a-z
    ITALIC, ITALIC, // A-Z, a-z
    BOLD_ITALIC, BOLD_ITALIC, // A-Z, a-z
    // Map fancy A-Z letters to script, not calligraphic.
    SCRIPT, NO_FONT, // A-Z script, a-z — no font
    NO_FONT, NO_FONT, // A-Z bold script, a-z bold script — no font
    FRAKTUR, FRAKTUR, // A-Z, a-z
    DOUBLE_STRUCK, DOUBLE_STRUCK, // A-Z double-struck, k double-struck
    BOLD_FRAKTUR, BOLD_FRAKTUR, // A-Z, a-z
    SANS_SERIF, SANS_SERIF, // A-Z, a-z
    BOLD_SANS_SERIF, BOLD_SANS_SERIF, // A-Z, a-z
    ITALIC_SANS_SERIF, ITALIC_SANS_SERIF, // A-Z, a-z
    NO_FONT, NO_FONT, // A-Z bold italic sans, a-z bold italic sans - no font
    MONOSPACE, MONOSPACE, // A-Z, a-z
];

/// Ten numerals per row.
#[rustfmt::skip]
const WIDE_NUMERAL_DATA: [WideChar; 5] = [
    BOLD_UPRIGHT,    // 0-9
    NO_FONT,         // 0-9 double-struck. No KaTeX font.
    SANS_SERIF,      // 0-9
    BOLD_SANS_SERIF, // 0-9
    MONOSPACE,       // 0-9
];

/// `wideCharacterFont(wideChar)` and then `[mode + "Class"]`: the font (`None` for no font) and
/// the class for `mode`. `Err` carries KaTeX's ParseError message for characters outside the
/// supported blocks (the caller wraps it in a ParseError).
pub fn wide_character_font(
    wide_char: &str,
    mode: Mode,
) -> Result<(Option<FontName>, &'static str), String> {
    // KaTeX decodes the first surrogate pair; for any string whose first unit is 0xD835 that is
    // the first char's code point.
    let code_point = wide_char.chars().next().map_or(0, |c| c as u32);
    let data = if (0x1D400..0x1D6A4).contains(&code_point) {
        WIDE_LATIN_LETTER_DATA[((code_point - 0x1D400) / 26) as usize]
    } else if (0x1D7CE..=0x1D7FF).contains(&code_point) {
        WIDE_NUMERAL_DATA[((code_point - 0x1D7CE) / 10) as usize]
    } else if code_point == 0x1D6A5 || code_point == 0x1D6A6 {
        // dotless i or j
        WIDE_LATIN_LETTER_DATA[0]
    } else if 0x1D6A6 < code_point && code_point < 0x1D7CE {
        // Greek letters. Not supported, yet.
        NO_FONT
    } else {
        // We don't support any wide characters outside 1D400–1D7FF.
        return Err(format!("Unsupported character: {wide_char}"));
    };
    let class = match mode {
        Mode::Math => data.1,
        Mode::Text => data.2,
    };
    Ok((data.0, class))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fonts() {
        assert_eq!(
            wide_character_font("\u{1D400}", Mode::Math),
            Ok((Some(FontName::MainBold), "mathbf"))
        );
        assert_eq!(
            wide_character_font("\u{1D400}", Mode::Text),
            Ok((Some(FontName::MainBold), "textbf"))
        );
        assert_eq!(wide_character_font("\u{1D4B6}", Mode::Math), Ok((None, ""))); // a-z script
        assert_eq!(
            wide_character_font("\u{1D55C}", Mode::Math),
            Ok((Some(FontName::AmsRegular), "mathbb"))
        );
        assert_eq!(
            wide_character_font("\u{1D7CE}", Mode::Text),
            Ok((Some(FontName::MainBold), "textbf"))
        );
        assert_eq!(
            wide_character_font("\u{1D7F6}", Mode::Math),
            Ok((Some(FontName::TypewriterRegular), "mathtt"))
        );
        assert_eq!(
            wide_character_font("\u{1D6A5}", Mode::Math),
            Ok((Some(FontName::MainBold), "mathbf"))
        );
        assert_eq!(wide_character_font("\u{1D6B0}", Mode::Math), Ok((None, "")));
        assert_eq!(
            wide_character_font("\u{1D6A4}", Mode::Math),
            Err("Unsupported character: \u{1D6A4}".to_string())
        );
    }
}

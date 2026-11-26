//! The KaTeX font faces (the 20 TTFs in `assets/fonts/katex/`) and KaTeX's metric font names
//! (`types/fonts.ts` `FontName`, the 18 faces `fontMetricsData` covers).

/// One of KaTeX's 20 font files. Every glyph in the display list names the face it needs, so a
/// painter can load faces lazily (most formulas need two or three).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[repr(u8)]
pub enum KatexFont {
    AmsRegular,
    CaligraphicBold,
    CaligraphicRegular,
    FrakturBold,
    FrakturRegular,
    MainBold,
    MainBoldItalic,
    MainItalic,
    MainRegular,
    MathBoldItalic,
    MathItalic,
    SansSerifBold,
    SansSerifItalic,
    SansSerifRegular,
    ScriptRegular,
    Size1Regular,
    Size2Regular,
    Size3Regular,
    Size4Regular,
    TypewriterRegular,
}

impl KatexFont {
    pub const ALL: [KatexFont; 20] = [
        KatexFont::AmsRegular,
        KatexFont::CaligraphicBold,
        KatexFont::CaligraphicRegular,
        KatexFont::FrakturBold,
        KatexFont::FrakturRegular,
        KatexFont::MainBold,
        KatexFont::MainBoldItalic,
        KatexFont::MainItalic,
        KatexFont::MainRegular,
        KatexFont::MathBoldItalic,
        KatexFont::MathItalic,
        KatexFont::SansSerifBold,
        KatexFont::SansSerifItalic,
        KatexFont::SansSerifRegular,
        KatexFont::ScriptRegular,
        KatexFont::Size1Regular,
        KatexFont::Size2Regular,
        KatexFont::Size3Regular,
        KatexFont::Size4Regular,
        KatexFont::TypewriterRegular,
    ];

    /// File name in `assets/fonts/katex/` (and KaTeX's `dist/fonts/`).
    pub fn file_name(self) -> &'static str {
        match self {
            KatexFont::AmsRegular => "KaTeX_AMS-Regular.ttf",
            KatexFont::CaligraphicBold => "KaTeX_Caligraphic-Bold.ttf",
            KatexFont::CaligraphicRegular => "KaTeX_Caligraphic-Regular.ttf",
            KatexFont::FrakturBold => "KaTeX_Fraktur-Bold.ttf",
            KatexFont::FrakturRegular => "KaTeX_Fraktur-Regular.ttf",
            KatexFont::MainBold => "KaTeX_Main-Bold.ttf",
            KatexFont::MainBoldItalic => "KaTeX_Main-BoldItalic.ttf",
            KatexFont::MainItalic => "KaTeX_Main-Italic.ttf",
            KatexFont::MainRegular => "KaTeX_Main-Regular.ttf",
            KatexFont::MathBoldItalic => "KaTeX_Math-BoldItalic.ttf",
            KatexFont::MathItalic => "KaTeX_Math-Italic.ttf",
            KatexFont::SansSerifBold => "KaTeX_SansSerif-Bold.ttf",
            KatexFont::SansSerifItalic => "KaTeX_SansSerif-Italic.ttf",
            KatexFont::SansSerifRegular => "KaTeX_SansSerif-Regular.ttf",
            KatexFont::ScriptRegular => "KaTeX_Script-Regular.ttf",
            KatexFont::Size1Regular => "KaTeX_Size1-Regular.ttf",
            KatexFont::Size2Regular => "KaTeX_Size2-Regular.ttf",
            KatexFont::Size3Regular => "KaTeX_Size3-Regular.ttf",
            KatexFont::Size4Regular => "KaTeX_Size4-Regular.ttf",
            KatexFont::TypewriterRegular => "KaTeX_Typewriter-Regular.ttf",
        }
    }

    /// The CSS family name (`@font-face font-family`).
    pub fn family(self) -> &'static str {
        use KatexFont::*;
        match self {
            AmsRegular => "KaTeX_AMS",
            CaligraphicBold | CaligraphicRegular => "KaTeX_Caligraphic",
            FrakturBold | FrakturRegular => "KaTeX_Fraktur",
            MainBold | MainBoldItalic | MainItalic | MainRegular => "KaTeX_Main",
            MathBoldItalic | MathItalic => "KaTeX_Math",
            SansSerifBold | SansSerifItalic | SansSerifRegular => "KaTeX_SansSerif",
            ScriptRegular => "KaTeX_Script",
            Size1Regular => "KaTeX_Size1",
            Size2Regular => "KaTeX_Size2",
            Size3Regular => "KaTeX_Size3",
            Size4Regular => "KaTeX_Size4",
            TypewriterRegular => "KaTeX_Typewriter",
        }
    }

    /// The face's `@font-face` weight is 700.
    pub fn is_bold(self) -> bool {
        use KatexFont::*;
        matches!(
            self,
            CaligraphicBold
                | FrakturBold
                | MainBold
                | MainBoldItalic
                | MathBoldItalic
                | SansSerifBold
        )
    }

    /// The face's `@font-face` style is italic.
    pub fn is_italic(self) -> bool {
        use KatexFont::*;
        matches!(
            self,
            MainBoldItalic | MainItalic | MathBoldItalic | MathItalic | SansSerifItalic
        )
    }
}

/// KaTeX `FontName`: the faces with metrics in `fontMetricsData`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FontName {
    AmsRegular,
    CaligraphicRegular,
    FrakturRegular,
    MainBold,
    MainBoldItalic,
    MainItalic,
    MainRegular,
    MathBoldItalic,
    MathItalic,
    SansSerifBold,
    SansSerifItalic,
    SansSerifRegular,
    ScriptRegular,
    Size1Regular,
    Size2Regular,
    Size3Regular,
    Size4Regular,
    TypewriterRegular,
}

impl FontName {
    /// KaTeX's spelling (`"Main-Regular"`).
    pub fn as_str(self) -> &'static str {
        use FontName::*;
        match self {
            AmsRegular => "AMS-Regular",
            CaligraphicRegular => "Caligraphic-Regular",
            FrakturRegular => "Fraktur-Regular",
            MainBold => "Main-Bold",
            MainBoldItalic => "Main-BoldItalic",
            MainItalic => "Main-Italic",
            MainRegular => "Main-Regular",
            MathBoldItalic => "Math-BoldItalic",
            MathItalic => "Math-Italic",
            SansSerifBold => "SansSerif-Bold",
            SansSerifItalic => "SansSerif-Italic",
            SansSerifRegular => "SansSerif-Regular",
            ScriptRegular => "Script-Regular",
            Size1Regular => "Size1-Regular",
            Size2Regular => "Size2-Regular",
            Size3Regular => "Size3-Regular",
            Size4Regular => "Size4-Regular",
            TypewriterRegular => "Typewriter-Regular",
        }
    }

    /// Parses KaTeX's spelling. `None` for names KaTeX has no metrics for (e.g. the
    /// `"SansSerif-BoldItalic"` that `retrieveTextFontName` can build; KaTeX throws a plain
    /// `Error` for those, which the port reports as an internal error).
    pub fn parse(s: &str) -> Option<FontName> {
        use FontName::*;
        Some(match s {
            "AMS-Regular" => AmsRegular,
            "Caligraphic-Regular" => CaligraphicRegular,
            "Fraktur-Regular" => FrakturRegular,
            "Main-Bold" => MainBold,
            "Main-BoldItalic" => MainBoldItalic,
            "Main-Italic" => MainItalic,
            "Main-Regular" => MainRegular,
            "Math-BoldItalic" => MathBoldItalic,
            "Math-Italic" => MathItalic,
            "SansSerif-Bold" => SansSerifBold,
            "SansSerif-Italic" => SansSerifItalic,
            "SansSerif-Regular" => SansSerifRegular,
            "Script-Regular" => ScriptRegular,
            "Size1-Regular" => Size1Regular,
            "Size2-Regular" => Size2Regular,
            "Size3-Regular" => Size3Regular,
            "Size4-Regular" => Size4Regular,
            "Typewriter-Regular" => TypewriterRegular,
            _ => return None,
        })
    }
}

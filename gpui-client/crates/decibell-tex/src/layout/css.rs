//! `katex.css` (0.18.4, as compiled in `dist/katex.min.css`, which the app imports) plus
//! Decibell's two overrides, applied to KaTeX's markup: the cascade for exactly the selectors
//! KaTeX's stylesheet has, resolved to computed values in device pixels.
//!
//! Rule order and specificity follow the stylesheet: every `.katex .x` rule is (0,2,0) and later
//! ones win; compound selectors (`.vlist > span > span`, `.mfrac .frac-line`,
//! `.katex-sizing.reset-sizeN.sizeM`, …) are applied after them by specificity; inline styles last.

use crate::dom_tree::{CssProp, CssStyle, CssVal};
use crate::layout::color::{Color, parse_color};

/// A KaTeX font family (`font-family` of the rule that set it).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Family {
    Main,
    Math,
    Ams,
    Caligraphic,
    Fraktur,
    SansSerif,
    Script,
    Size1,
    Size2,
    Size3,
    Size4,
    Typewriter,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Display {
    Inline,
    InlineBlock,
    Block,
    InlineTable,
    TableRow,
    TableCell,
    InlineFlexRow,
    InlineFlexColumn,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Static,
    Relative,
    Absolute,
}

/// A length: device px, a percentage of the containing block (plus device px, for `calc()`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Len {
    Auto,
    Px(f32),
    Pct(f32, f32),
}

impl Len {
    pub fn is_auto(self) -> bool {
        matches!(self, Len::Auto)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum VAlign {
    Baseline,
    Bottom,
    Px(f32),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BorderStyle {
    None,
    Solid,
    Dashed,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Border {
    /// Device px as specified (before Chromium snaps it).
    pub width: f32,
    pub style: BorderStyle,
    /// `None` = currentColor.
    pub color: Option<Color>,
}

impl Border {
    const NONE: Border = Border {
        width: 3.0,
        style: BorderStyle::None,
        color: None,
    };
}

/// Inherited properties.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Inherited {
    /// Computed font size, device px (Blink keeps it as a float).
    pub font_px: f32,
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    pub color: Color,
    /// `line-height` as a number (KaTeX only uses numbers: 1.2, and 0 for `.katex-smash`).
    pub line_height: f32,
    pub text_align: TextAlign,
}

/// The computed style of one element.
#[derive(Clone, Debug)]
pub struct Computed {
    pub inh: Inherited,
    pub display: Display,
    pub position: Position,
    pub width: Len,
    pub height: Len,
    pub min_width: Len,
    pub min_height: Len,
    pub max_width: Len,
    /// top, right, bottom, left.
    pub margin: [Len; 4],
    pub padding: [f32; 4],
    pub border: [Border; 4],
    pub offsets: [Len; 4],
    pub valign: VAlign,
    pub overflow_hidden: bool,
    pub border_box: bool,
    pub background: Option<Color>,
    /// `text-shadow` offset (device px); KaTeX's `\pmb` is the only user.
    pub text_shadow: Option<(f32, f32)>,
    /// `height: inherit` (the `<svg>` rule): resolved from the parent's computed height.
    pub height_inherit: bool,
    /// The element is `.eqn-num` (CSS generates "(n)" before it).
    pub eqn_num: bool,
}

impl Computed {
    /// The `.katex` root inside a host of `host_px` CSS px at device scale `dsf`.
    pub fn root(host_px: f32, dsf: f32) -> Computed {
        let inh = Inherited {
            font_px: (host_px * dsf) * 1.21,
            family: Family::Main,
            bold: false,
            italic: false,
            color: Color::Current,
            line_height: 1.2,
            text_align: TextAlign::Left,
        };
        Computed {
            inh,
            display: Display::Inline,
            position: Position::Relative,
            width: Len::Auto,
            height: Len::Auto,
            min_width: Len::Auto,
            min_height: Len::Auto,
            max_width: Len::Auto,
            margin: [Len::Px(0.0); 4],
            padding: [0.0; 4],
            border: [Border::NONE; 4],
            offsets: [Len::Auto; 4],
            valign: VAlign::Baseline,
            overflow_hidden: false,
            border_box: false,
            background: None,
            text_shadow: None,
            height_inherit: false,
            eqn_num: false,
        }
    }

    /// Default (non-inherited reset) for a child of `parent`.
    fn child_of(parent: &Computed) -> Computed {
        let mut c = Computed::root(0.0, 1.0);
        c.inh = parent.inh;
        c.position = Position::Static;
        c
    }

    pub fn em(&self, v: f64) -> f32 {
        // Blink: CSS number (double) × font size (float), stored as a float length.
        (v * self.inh.font_px as f64) as f32
    }
}

/// What the selectors with combinators need to know about the ancestors.
#[derive(Clone, Copy, Default)]
pub struct Anc<'a> {
    pub parent: &'a [&'static str],
    pub grandparent: &'a [&'static str],
    pub in_mfrac: bool,
    pub in_overline: bool,
    pub in_underline: bool,
    pub in_accent: bool,
    pub in_mtable: bool,
    pub in_delim_mult: bool,
    pub parent_is_katex_html: bool,
    /// The element is the `.katex-html` of a display formula's `.katex`.
    pub display_html: bool,
}

fn has(classes: &[&str], c: &str) -> bool {
    classes.contains(&c)
}

/// The `.katex-sizing.reset-sizeN.sizeM` factor, as compiled in katex.min.css.
fn sizing_factor(from: usize, to: usize) -> f64 {
    #[rustfmt::skip]
    const F: [[f64; 11]; 11] = [
        [1.0, 1.2, 1.4, 1.6, 1.8, 2.0, 2.4, 2.88, 3.456, 4.148, 4.976],
        [0.8333333333, 1.0, 1.1666666667, 1.3333333333, 1.5, 1.6666666667, 2.0, 2.4, 2.88, 3.4566666667, 4.1466666667],
        [0.7142857143, 0.8571428571, 1.0, 1.1428571429, 1.2857142857, 1.4285714286, 1.7142857143, 2.0571428571, 2.4685714286, 2.9628571429, 3.5542857143],
        [0.625, 0.75, 0.875, 1.0, 1.125, 1.25, 1.5, 1.8, 2.16, 2.5925, 3.11],
        [0.5555555556, 0.6666666667, 0.7777777778, 0.8888888889, 1.0, 1.1111111111, 1.3333333333, 1.6, 1.92, 2.3044444444, 2.7644444444],
        [0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.2, 1.44, 1.728, 2.074, 2.488],
        [0.4166666667, 0.5, 0.5833333333, 0.6666666667, 0.75, 0.8333333333, 1.0, 1.2, 1.44, 1.7283333333, 2.0733333333],
        [0.3472222222, 0.4166666667, 0.4861111111, 0.5555555556, 0.625, 0.6944444444, 0.8333333333, 1.0, 1.2, 1.4402777778, 1.7277777778],
        [0.2893518519, 0.3472222222, 0.4050925926, 0.462962963, 0.5208333333, 0.5787037037, 0.6944444444, 0.8333333333, 1.0, 1.2002314815, 1.4398148148],
        [0.2410800386, 0.2892960463, 0.337512054, 0.3857280617, 0.4339440694, 0.4821600771, 0.5785920926, 0.6943105111, 0.8331726133, 1.0, 1.1996142719],
        [0.2009646302, 0.2411575563, 0.2813504823, 0.3215434084, 0.3617363344, 0.4019292605, 0.4823151125, 0.578778135, 0.6945337621, 0.8336012862, 1.0],
    ];
    F[from - 1][to - 1]
}

fn class_number(classes: &[&str], prefix: &str) -> Option<usize> {
    classes
        .iter()
        .find_map(|c| c.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok()))
}

/// Parses a CSS length string KaTeX writes (`"-0.02em"`, `"2px"`, `"50%"`, `"0"`,
/// `"calc(100% - 0.3em)"`).
fn parse_len(s: &str, c: &Computed) -> Option<Len> {
    let s = s.trim();
    if s == "auto" {
        return Some(Len::Auto);
    }
    if let Some(inner) = s.strip_prefix("calc(").and_then(|r| r.strip_suffix(')')) {
        // `A% ± Bem` / `A% ± Bpx`
        let (a, rest) = inner.split_once('%')?;
        let a: f32 = a.trim().parse().ok()?;
        let rest = rest.trim();
        let (sign, b) = if let Some(b) = rest.strip_prefix('+') {
            (1.0, b)
        } else {
            let b = rest.strip_prefix('-')?;
            (-1.0, b)
        };
        let Len::Px(px) = parse_len(b, c)? else {
            return None;
        };
        return Some(Len::Pct(a, sign * px));
    }
    if let Some(n) = s.strip_suffix('%') {
        return Some(Len::Pct(n.trim().parse().ok()?, 0.0));
    }
    if let Some(n) = s.strip_suffix("em") {
        return Some(Len::Px(c.em(n.trim().parse().ok()?)));
    }
    if let Some(n) = s.strip_suffix("px") {
        let v: f64 = n.trim().parse().ok()?;
        return Some(Len::Px(
            (v * c.inh.font_px as f64 / c.inh.font_px as f64) as f32 * DSF_HINT.with(|d| d.get()),
        ));
    }
    if s == "0" {
        return Some(Len::Px(0.0));
    }
    None
}

thread_local! {
    /// The device scale used to resolve `px` values in inline styles.
    pub static DSF_HINT: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
}

fn val_len(v: &CssVal, c: &Computed) -> Option<Len> {
    match v {
        CssVal::Em(e) => Some(Len::Px(c.em(*e))),
        CssVal::Str(s) => parse_len(s, c),
        CssVal::Static(s) => parse_len(s, c),
    }
}

fn val_str(v: &CssVal) -> String {
    v.to_string_value()
}

/// Computes the style of an element with `classes` and inline `style`, child of `parent`.
pub fn compute(
    classes: &[&'static str],
    style: &CssStyle,
    parent: &Computed,
    anc: &Anc,
    is_svg: bool,
) -> Computed {
    let dsf = DSF_HINT.with(|d| d.get());
    let mut c = Computed::child_of(parent);
    let em = |c: &Computed, v: f64| Len::Px(c.em(v));

    // ---- font-size first (lengths below are relative to the element's own font size) ----
    if has(classes, "vlist-s") {
        c.inh.font_px = dsf; // 1px
    }
    if (has(classes, "katex-sizing") || has(classes, "fontsize-ensurer"))
        && let (Some(from), Some(to)) = (
            class_number(classes, "reset-size"),
            class_number(classes, "size"),
        )
        && (1..=11).contains(&from)
        && (1..=11).contains(&to)
    {
        c.inh.font_px = (sizing_factor(from, to) * parent.inh.font_px as f64) as f32;
    }

    // ---- single-class rules (0,2,0), stylesheet order ----
    for &cl in classes {
        let _ = cl;
    }
    let h = |n: &str| has(classes, n);
    if h("katex-base") {
        c.position = Position::Relative;
        c.display = Display::InlineBlock;
    }
    if h("katex-strut") {
        c.display = Display::InlineBlock;
    }
    if h("textbf") {
        c.inh.bold = true;
    }
    if h("textit") {
        c.inh.italic = true;
    }
    if h("textrm") {
        c.inh.family = Family::Main;
    }
    if h("textsf") {
        c.inh.family = Family::SansSerif;
    }
    if h("texttt") {
        c.inh.family = Family::Typewriter;
    }
    if h("mathnormal") {
        c.inh.family = Family::Math;
        c.inh.italic = true;
    }
    if h("mathit") {
        c.inh.family = Family::Main;
        c.inh.italic = true;
    }
    if h("mathrm") {
        c.inh.italic = false;
    }
    if h("mathbf") {
        c.inh.family = Family::Main;
        c.inh.bold = true;
    }
    if h("boldsymbol") {
        c.inh.family = Family::Math;
        c.inh.bold = true;
        c.inh.italic = true;
    }
    if h("amsrm") {
        c.inh.family = Family::Ams;
    }
    if h("mathbb") || h("textbb") {
        c.inh.family = Family::Ams;
    }
    if h("mathcal") {
        c.inh.family = Family::Caligraphic;
    }
    if h("mathfrak") || h("textfrak") {
        c.inh.family = Family::Fraktur;
    }
    if h("mathboldfrak") || h("textboldfrak") {
        c.inh.family = Family::Fraktur;
        c.inh.bold = true;
    }
    if h("mathtt") {
        c.inh.family = Family::Typewriter;
    }
    if h("mathscr") || h("textscr") {
        c.inh.family = Family::Script;
    }
    if h("mathsf") || h("textsf") {
        c.inh.family = Family::SansSerif;
    }
    if h("mathboldsf") || h("textboldsf") {
        c.inh.family = Family::SansSerif;
        c.inh.bold = true;
    }
    if h("mathsfit") || h("mathitsf") || h("textitsf") {
        c.inh.family = Family::SansSerif;
        c.inh.italic = true;
    }
    if h("mainrm") {
        c.inh.family = Family::Main;
        c.inh.italic = false;
    }
    if h("vlist-t") {
        c.display = Display::InlineTable;
    }
    if h("vlist-r") {
        c.display = Display::TableRow;
    }
    if h("vlist") {
        c.display = Display::TableCell;
        c.valign = VAlign::Bottom;
        c.position = Position::Relative;
    }
    if h("vlist-t2") {
        c.margin[1] = Len::Px(-2.0 * dsf);
    }
    if h("vlist-s") {
        c.display = Display::TableCell;
        c.valign = VAlign::Bottom;
        c.width = Len::Px(2.0 * dsf);
        c.min_width = Len::Px(2.0 * dsf);
    }
    if h("katex-vbox") {
        c.display = Display::InlineFlexColumn;
    }
    if h("katex-thinbox") {
        c.display = Display::InlineFlexRow;
        c.width = Len::Px(0.0);
        c.max_width = Len::Px(0.0);
    }
    if h("msupsub") {
        c.inh.text_align = TextAlign::Left;
    }
    if h("mspace") {
        c.display = Display::InlineBlock;
    }
    if h("katex-smash") {
        c.display = Display::Inline;
        c.inh.line_height = 0.0;
    }
    if h("llap") || h("rlap") || h("clap") {
        c.width = Len::Px(0.0);
        c.position = Position::Relative;
    }
    if h("katex-rule") {
        c.display = Display::InlineBlock;
        for b in c.border.iter_mut() {
            *b = Border {
                width: 0.0,
                style: BorderStyle::Solid,
                color: None,
            };
        }
        c.position = Position::Relative;
    }
    if h("katex-hline") {
        c.display = Display::InlineBlock;
        c.width = Len::Pct(100.0, 0.0);
        c.border[2].style = BorderStyle::Solid;
    }
    if h("katex-hdashline") {
        c.display = Display::InlineBlock;
        c.width = Len::Pct(100.0, 0.0);
        c.border[2].style = BorderStyle::Dashed;
    }
    if h("katex-rule") || h("katex-hline") || h("katex-hdashline") {
        c.min_height = Len::Px(dsf);
    }
    if h("nulldelimiter") {
        c.display = Display::InlineBlock;
        c.width = em(&c, 0.12);
    }
    if h("delimcenter") {
        c.position = Position::Relative;
    }
    if h("op-symbol") {
        c.position = Position::Relative;
    }
    if h("katex-overlay") {
        c.display = Display::Block;
    }
    if h("svg-align") {
        c.inh.text_align = TextAlign::Left;
    }
    if h("katex-stretchy") {
        c.width = Len::Pct(100.0, 0.0);
        c.display = Display::Block;
        c.position = Position::Relative;
        c.overflow_hidden = true;
    }
    if h("hide-tail") {
        c.width = Len::Pct(100.0, 0.0);
        c.position = Position::Relative;
        c.overflow_hidden = true;
    }
    if h("halfarrow-left") {
        c.position = Position::Absolute;
        c.offsets[3] = Len::Px(0.0);
        c.width = Len::Pct(50.2, 0.0);
        c.overflow_hidden = true;
    }
    if h("halfarrow-right") {
        c.position = Position::Absolute;
        c.offsets[1] = Len::Px(0.0);
        c.width = Len::Pct(50.2, 0.0);
        c.overflow_hidden = true;
    }
    if h("brace-left") {
        c.position = Position::Absolute;
        c.offsets[3] = Len::Px(0.0);
        c.width = Len::Pct(25.1, 0.0);
        c.overflow_hidden = true;
    }
    if h("brace-center") {
        c.position = Position::Absolute;
        c.offsets[3] = Len::Pct(25.0, 0.0);
        c.width = Len::Pct(50.0, 0.0);
        c.overflow_hidden = true;
    }
    if h("brace-right") {
        c.position = Position::Absolute;
        c.offsets[1] = Len::Px(0.0);
        c.width = Len::Pct(25.1, 0.0);
        c.overflow_hidden = true;
    }
    if h("x-arrow-pad") {
        c.padding = [0.0, c.em(0.5), 0.0, c.em(0.5)];
    }
    if h("cd-arrow-pad") {
        c.padding = [0.0, c.em(0.55556), 0.0, c.em(0.27778)];
    }
    if h("x-arrow") || h("mover") || h("munder") {
        c.inh.text_align = TextAlign::Center;
    }
    if h("boxpad") {
        c.padding = [0.0, c.em(0.3), 0.0, c.em(0.3)];
    }
    if h("fbox") || h("fcolorbox") {
        c.border_box = true;
        for b in c.border.iter_mut() {
            *b = Border {
                width: c.inh.font_px * 0.04,
                style: BorderStyle::Solid,
                color: None,
            };
        }
    }
    if h("cancel-pad") {
        c.padding = [0.0, c.em(0.2), 0.0, c.em(0.2)];
    }
    if h("cancel-lap") {
        c.margin[3] = em(&c, -0.2);
        c.margin[1] = em(&c, -0.2);
    }
    if h("katex-sout") {
        c.border[2].style = BorderStyle::Solid;
        c.border[2].width = c.em(0.08);
    }
    if h("angl") {
        c.border_box = true;
        c.border[0] = Border {
            width: c.em(0.049),
            style: BorderStyle::Solid,
            color: None,
        };
        c.border[1] = Border {
            width: c.em(0.049),
            style: BorderStyle::Solid,
            color: None,
        };
        c.margin[1] = em(&c, 0.03889);
    }
    if h("anglpad") {
        c.padding = [0.0, c.em(0.03889), 0.0, c.em(0.03889)];
    }
    if h("eqn-num") {
        c.eqn_num = true;
    }
    if h("mtr-glue") {
        c.width = Len::Pct(50.0, 0.0);
    }
    if h("cd-vert-arrow") {
        c.display = Display::InlineBlock;
        c.position = Position::Relative;
    }
    if h("cd-label-left") {
        c.display = Display::InlineBlock;
        c.position = Position::Absolute;
        c.offsets[1] = Len::Pct(50.0, c.em(0.3));
        c.inh.text_align = TextAlign::Left;
    }
    if h("cd-label-right") {
        c.display = Display::InlineBlock;
        c.position = Position::Absolute;
        c.offsets[3] = Len::Pct(50.0, c.em(0.3));
        c.inh.text_align = TextAlign::Right;
    }
    // `.katex-display` and Decibell's overrides (margin 0, left aligned).
    if h("katex-display") {
        c.display = Display::Block;
        c.inh.text_align = TextAlign::Left;
    }

    // ---- compound selectors, by specificity ----
    let p = anc.parent;
    if is_svg {
        // `.katex svg` (0,1,1)
        c.display = Display::Block;
        c.position = Position::Absolute;
        c.width = Len::Pct(100.0, 0.0);
        c.height_inherit = true;
    }
    if has(p, "vlist") {
        // `.vlist > span` (0,2,1)
        c.display = Display::Block;
        c.height = Len::Px(0.0);
        c.position = Position::Relative;
    }
    if has(anc.grandparent, "vlist") && !is_svg {
        // `.vlist > span > span` (0,2,2)
        c.display = Display::InlineBlock;
    }
    // (0,3,0)
    if anc.in_mfrac && h("frac-line") {
        c.display = Display::InlineBlock;
        c.width = Len::Pct(100.0, 0.0);
        c.border[2].style = BorderStyle::Solid;
        c.min_height = Len::Px(dsf);
    }
    if (anc.in_overline && h("overline-line")) || (anc.in_underline && h("underline-line")) {
        c.display = Display::InlineBlock;
        c.width = Len::Pct(100.0, 0.0);
        c.border[2].style = BorderStyle::Solid;
        c.min_height = Len::Px(dsf);
    }
    if h("katex-inner") && (has(p, "llap") || has(p, "rlap") || has(p, "clap")) {
        c.position = Position::Absolute;
        if has(p, "llap") {
            c.offsets[1] = Len::Px(0.0);
        } else {
            c.offsets[3] = Len::Px(0.0);
        }
    }
    if h("katex-fix") && (has(p, "llap") || has(p, "rlap") || has(p, "clap")) {
        c.display = Display::InlineBlock;
    }
    if h("katex-root") && has(p, "sqrt") {
        c.margin[3] = em(&c, 0.2777777778);
        c.margin[1] = em(&c, -0.5555555556);
    }
    if anc.parent_is_katex_html && h("katex-newline") {
        c.display = Display::Block;
    }
    if h("delimsizing") {
        if h("size1") {
            c.inh.family = Family::Size1;
        } else if h("size2") {
            c.inh.family = Family::Size2;
        } else if h("size3") {
            c.inh.family = Family::Size3;
        } else if h("size4") {
            c.inh.family = Family::Size4;
        }
    }
    if h("op-symbol") {
        if h("small-op") {
            c.inh.family = Family::Size1;
        }
        if h("large-op") {
            c.inh.family = Family::Size2;
        }
    }
    if h("vlist-t") && (has(p, "op-limits") || has(p, "katex-accent")) {
        c.inh.text_align = TextAlign::Center;
    }
    if anc.in_accent && h("accent-body") {
        c.position = Position::Relative;
        if !h("accent-full") {
            c.width = Len::Px(0.0); // (0,4,0)
        }
    }
    if anc.in_mtable && h("vertical-separator") {
        c.display = Display::InlineBlock;
        c.min_width = Len::Px(dsf);
    }
    if anc.in_mtable && h("arraycolsep") {
        c.display = Display::InlineBlock;
    }
    if h("vlist-t") && anc.in_mtable {
        // (0,4,0) `.mtable .col-align-x > .vlist-t`
        if has(p, "col-align-c") {
            c.inh.text_align = TextAlign::Center;
        } else if has(p, "col-align-l") {
            c.inh.text_align = TextAlign::Left;
        } else if has(p, "col-align-r") {
            c.inh.text_align = TextAlign::Right;
        }
    }
    if anc.in_mfrac && has(anc.grandparent, "mfrac") {
        // `.mfrac > span > span` (0,2,2)
        c.inh.text_align = TextAlign::Center;
    }
    if anc.in_delim_mult && (has(p, "delim-size1") || has(p, "delim-size4")) {
        // `.delimsizing.mult .delim-sizeN > span` (0,4,1)
        c.inh.family = if has(p, "delim-size1") {
            Family::Size1
        } else {
            Family::Size4
        };
    }
    if has(anc.grandparent, "clap") && has(p, "katex-inner") {
        // `.clap > .katex-inner > span` (0,3,1)
        c.margin[3] = Len::Pct(-50.0, 0.0);
        c.margin[1] = Len::Pct(50.0, 0.0);
    }
    if h("vlist-t") && has(anc.parent, "mfrac") {
        // nothing (`.mfrac > span > span` targets the row)
    }
    if has(p, "vlist") && h("pstrut") {
        // never: pstrut is a grandchild
    }
    if has(anc.grandparent, "vlist") && h("pstrut") {
        // `.vlist > span > .pstrut` (0,3,1)
        c.overflow_hidden = true;
        c.width = Len::Px(0.0);
    }
    if anc.display_html {
        c.display = Display::Block;
        c.position = Position::Relative;
    }
    if anc.display_html && false {
        unreachable!();
    }
    if h("katex-tag") && anc.parent_is_katex_html && anc.display_html_parent() {
        c.position = Position::Absolute;
        c.offsets[1] = Len::Px(0.0);
    }

    // ---- inline style ----
    for (prop, v) in &style.0 {
        match prop {
            CssProp::BackgroundColor => {
                if let Some(col) = parse_color(&val_str(v)) {
                    c.background = Some(col);
                }
            }
            CssProp::BorderBottomWidth => {
                if let Some(Len::Px(w)) = val_len(v, &c) {
                    c.border[2].width = w;
                }
            }
            CssProp::BorderRightWidth => {
                if let Some(Len::Px(w)) = val_len(v, &c) {
                    c.border[1].width = w;
                }
            }
            CssProp::BorderTopWidth => {
                if let Some(Len::Px(w)) = val_len(v, &c) {
                    c.border[0].width = w;
                }
            }
            CssProp::BorderWidth => {
                let s = val_str(v);
                let parts: Vec<Len> = s
                    .split_whitespace()
                    .filter_map(|t| parse_len(t, &c))
                    .collect();
                let w = expand4(&parts);
                for (i, l) in w.iter().enumerate() {
                    if let Len::Px(px) = l {
                        c.border[i].width = *px;
                    }
                }
            }
            CssProp::BorderStyle => {
                let st = border_style(&val_str(v));
                for b in c.border.iter_mut() {
                    b.style = st;
                }
            }
            CssProp::BorderRightStyle => c.border[1].style = border_style(&val_str(v)),
            CssProp::BorderColor => {
                if let Some(col) = parse_color(&val_str(v)) {
                    for b in c.border.iter_mut() {
                        b.color = Some(col);
                    }
                }
            }
            CssProp::Bottom => {
                if let Some(l) = val_len(v, &c) {
                    c.offsets[2] = l;
                }
            }
            CssProp::Top => {
                if let Some(l) = val_len(v, &c) {
                    c.offsets[0] = l;
                }
            }
            CssProp::Left => {
                if let Some(l) = val_len(v, &c) {
                    c.offsets[3] = l;
                }
            }
            CssProp::Color => {
                if let Some(col) = parse_color(&val_str(v)) {
                    c.inh.color = col;
                }
            }
            CssProp::Height => {
                if let Some(l) = val_len(v, &c) {
                    c.height = l;
                    c.height_inherit = false;
                }
            }
            CssProp::Width => {
                if let Some(l) = val_len(v, &c) {
                    c.width = l;
                }
            }
            CssProp::MinWidth => {
                if let Some(l) = val_len(v, &c) {
                    c.min_width = l;
                }
            }
            CssProp::Margin => {
                let s = val_str(v);
                let parts: Vec<Len> = s
                    .split_whitespace()
                    .filter_map(|t| parse_len(t, &c))
                    .collect();
                c.margin = expand4(&parts);
            }
            CssProp::MarginLeft => {
                if let Some(l) = val_len(v, &c) {
                    c.margin[3] = l;
                }
            }
            CssProp::MarginRight => {
                if let Some(l) = val_len(v, &c) {
                    c.margin[1] = l;
                }
            }
            CssProp::MarginTop => {
                if let Some(l) = val_len(v, &c) {
                    c.margin[0] = l;
                }
            }
            CssProp::PaddingLeft => {
                if let Some(Len::Px(l)) = val_len(v, &c) {
                    c.padding[3] = l;
                }
            }
            CssProp::Position => {
                c.position = match val_str(v).as_str() {
                    "absolute" => Position::Absolute,
                    "relative" => Position::Relative,
                    _ => Position::Static,
                }
            }
            CssProp::TextShadow => {
                let s = val_str(v);
                let parts: Vec<Len> = s
                    .split_whitespace()
                    .filter_map(|t| parse_len(t, &c))
                    .collect();
                if let (Some(Len::Px(x)), Some(Len::Px(y))) = (parts.first(), parts.get(1)) {
                    c.text_shadow = Some((*x, *y));
                }
            }
            CssProp::VerticalAlign => {
                if let Some(Len::Px(l)) = val_len(v, &c) {
                    c.valign = VAlign::Px(l);
                }
            }
        }
    }
    c
}

impl Anc<'_> {
    fn display_html_parent(&self) -> bool {
        // set by the engine on children of a display `.katex-html`
        self.in_display_html_child()
    }

    fn in_display_html_child(&self) -> bool {
        self.parent_is_katex_html && DISPLAY_MODE.with(|d| d.get())
    }
}

thread_local! {
    /// Whether the formula being laid out is in display mode.
    pub static DISPLAY_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn expand4(parts: &[Len]) -> [Len; 4] {
    match parts.len() {
        1 => [parts[0]; 4],
        2 => [parts[0], parts[1], parts[0], parts[1]],
        3 => [parts[0], parts[1], parts[2], parts[1]],
        4 => [parts[0], parts[1], parts[2], parts[3]],
        _ => [Len::Px(0.0); 4],
    }
}

fn border_style(s: &str) -> BorderStyle {
    match s.trim() {
        "solid" => BorderStyle::Solid,
        "dashed" => BorderStyle::Dashed,
        _ => BorderStyle::None,
    }
}

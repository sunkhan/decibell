//! Port of KaTeX `src/types/nodes.ts` (the parse-node union), `src/parseNode.ts` (its
//! assertion helpers) and the small shared types of `src/types/index.ts`.
//!
//! Each KaTeX node type is one [`ParseNode`] variant named after it (`"genfrac"` →
//! [`ParseNode::GenFrac`], `"accent-token"` → [`ParseNode::AccentToken`]). Optional JS fields
//! are `Option`s or `bool`s defaulting to `false` where KaTeX only tests truthiness.

use crate::parse_error::{Result, ierr, perr};
use crate::token::{Loc, Token};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Math,
    Text,
}

/// `StyleStr`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StyleStr {
    Display,
    Text,
    Script,
    ScriptScript,
}

/// `Measurement` (`{number, unit}`); units are always one of KaTeX's valid units.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Measurement {
    pub number: f64,
    pub unit: &'static str,
}

impl Measurement {
    pub const fn new(number: f64, unit: &'static str) -> Measurement {
        Measurement { number, unit }
    }
}

/// `Atom` (atoms.ts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Atom {
    Bin,
    Close,
    Inner,
    Open,
    Punct,
    Rel,
}

impl Atom {
    pub fn as_str(self) -> &'static str {
        match self {
            Atom::Bin => "bin",
            Atom::Close => "close",
            Atom::Inner => "inner",
            Atom::Open => "open",
            Atom::Punct => "punct",
            Atom::Rel => "rel",
        }
    }
}

/// `Side`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// `ColSeparationType` (environments/array.ts).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColSeparationType {
    Align,
    Alignat,
    Gather,
    Small,
    Cd,
}

/// `AlignSpec` (environments/array.ts).
#[derive(Clone, PartialEq, Debug)]
pub enum AlignSpec {
    Separator {
        separator: String,
    },
    Align {
        align: String,
        pregap: Option<f64>,
        postgap: Option<f64>,
    },
}

/// A row tag: `true`/`false` (automatic numbering or not) or an explicit tag.
#[derive(Clone, Debug)]
pub enum RowTag {
    Auto(bool),
    Explicit(Vec<ParseNode>),
}

#[derive(Clone, Debug)]
pub struct ArrayNode {
    pub mode: Mode,
    pub col_separation_type: Option<ColSeparationType>,
    pub hskip_before_and_after: Option<bool>,
    pub add_jot: Option<bool>,
    pub cols: Option<Vec<AlignSpec>>,
    pub arraystretch: f64,
    pub body: Vec<Vec<ParseNode>>,
    pub row_gaps: Vec<Option<Measurement>>,
    pub h_lines_before_row: Vec<Vec<bool>>,
    pub tags: Option<Vec<RowTag>>,
    pub leqno: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct OpNode {
    pub mode: Mode,
    pub limits: bool,
    pub always_handle_sup_sub: bool,
    pub suppress_base_shift: bool,
    pub parent_is_sup_sub: bool,
    pub symbol: bool,
    /// `name` (absent when `body` is set).
    pub name: Option<String>,
    /// `body` (absent when `name` is set).
    pub body: Option<Vec<ParseNode>>,
}

#[derive(Clone, Debug)]
pub struct OrdGroupNode {
    pub mode: Mode,
    pub body: Vec<ParseNode>,
    pub semisimple: bool,
    pub loc: Option<Loc>,
}

#[derive(Clone, Debug)]
pub struct SupSubNode {
    pub mode: Mode,
    pub base: Option<Box<ParseNode>>,
    pub sup: Option<Box<ParseNode>>,
    pub sub: Option<Box<ParseNode>>,
}

/// The symbol node types (`mathord`, `textord`, `spacing`, `accent-token`, `op-token`).
#[derive(Clone, Debug)]
pub struct SymNode {
    pub mode: Mode,
    pub text: String,
    pub loc: Option<Loc>,
}

#[derive(Clone, Debug)]
pub struct AtomNode {
    pub mode: Mode,
    pub family: Atom,
    pub text: String,
    pub loc: Option<Loc>,
}

#[derive(Clone, Debug)]
pub struct AccentNode {
    pub mode: Mode,
    pub label: String,
    pub is_stretchy: bool,
    pub is_shifty: bool,
    pub base: Box<ParseNode>,
}

#[derive(Clone, Debug)]
pub struct GenFracNode {
    pub mode: Mode,
    pub continued: bool,
    pub numer: Box<ParseNode>,
    pub denom: Box<ParseNode>,
    pub has_bar_line: bool,
    pub left_delim: Option<String>,
    pub right_delim: Option<String>,
    pub bar_size: Option<Measurement>,
}

#[derive(Clone, Debug)]
pub struct LeftRightNode {
    pub mode: Mode,
    pub body: Vec<ParseNode>,
    pub left: String,
    pub right: String,
    pub right_color: Option<String>,
}

#[derive(Clone, Debug)]
pub struct MathChoiceNode {
    pub mode: Mode,
    pub display: Vec<ParseNode>,
    pub text: Vec<ParseNode>,
    pub script: Vec<ParseNode>,
    pub scriptscript: Vec<ParseNode>,
}

#[derive(Clone, Debug)]
pub struct EncloseNode {
    pub mode: Mode,
    pub body: Box<ParseNode>,
    pub label: String,
    pub background_color: Option<String>,
    pub border_color: Option<String>,
}

#[derive(Clone, Debug)]
pub struct IncludeGraphicsNode {
    pub mode: Mode,
    pub alt: String,
    pub width: Measurement,
    pub height: Measurement,
    pub totalheight: Measurement,
    pub src: String,
}

/// KaTeX's parse tree node (`AnyParseNode`).
#[derive(Clone, Debug)]
pub enum ParseNode {
    Array(Box<ArrayNode>),
    CdLabel {
        mode: Mode,
        side: Side,
        label: Box<ParseNode>,
    },
    CdLabelParent {
        mode: Mode,
        fragment: Box<ParseNode>,
    },
    Color {
        mode: Mode,
        color: String,
        body: Vec<ParseNode>,
    },
    ColorToken {
        mode: Mode,
        color: String,
    },
    Op(Box<OpNode>),
    OrdGroup(OrdGroupNode),
    Raw {
        mode: Mode,
        string: String,
    },
    Size {
        mode: Mode,
        value: Measurement,
        is_blank: bool,
    },
    Styling {
        mode: Mode,
        style: StyleStr,
        reset_font: bool,
        body: Vec<ParseNode>,
    },
    SupSub(SupSubNode),
    Tag {
        mode: Mode,
        body: Vec<ParseNode>,
        tag: Vec<ParseNode>,
    },
    Text {
        mode: Mode,
        body: Vec<ParseNode>,
        font: Option<&'static str>,
    },
    Url {
        mode: Mode,
        url: String,
    },
    Verb {
        mode: Mode,
        body: String,
        star: bool,
    },
    Atom(AtomNode),
    MathOrd(SymNode),
    TextOrd(SymNode),
    Spacing(SymNode),
    AccentToken(SymNode),
    OpToken(SymNode),
    Accent(Box<AccentNode>),
    AccentUnder(Box<AccentNode>),
    Cr {
        mode: Mode,
        new_line: bool,
        size: Option<Measurement>,
    },
    DelimSizing {
        mode: Mode,
        size: u8,
        mclass: &'static str,
        delim: String,
    },
    Enclose(Box<EncloseNode>),
    Environment {
        mode: Mode,
        name: String,
        name_group: Box<ParseNode>,
    },
    Font {
        mode: Mode,
        font: &'static str,
        body: Box<ParseNode>,
    },
    GenFrac(Box<GenFracNode>),
    HBox {
        mode: Mode,
        body: Vec<ParseNode>,
    },
    HorizBrace {
        mode: Mode,
        label: String,
        is_over: bool,
        base: Box<ParseNode>,
    },
    Href {
        mode: Mode,
        href: String,
        body: Vec<ParseNode>,
    },
    Html {
        mode: Mode,
        attributes: Vec<(String, String)>,
        body: Vec<ParseNode>,
    },
    HtmlMathml {
        mode: Mode,
        html: Vec<ParseNode>,
        mathml: Vec<ParseNode>,
    },
    IncludeGraphics(Box<IncludeGraphicsNode>),
    Infix {
        mode: Mode,
        replace_with: String,
        size: Option<Measurement>,
        token: Option<Box<Token>>,
    },
    Internal {
        mode: Mode,
    },
    Kern {
        mode: Mode,
        dimension: Measurement,
    },
    Lap {
        mode: Mode,
        alignment: String,
        body: Box<ParseNode>,
    },
    LeftRight(Box<LeftRightNode>),
    LeftRightRight {
        mode: Mode,
        delim: String,
        color: Option<String>,
    },
    MathChoice(Box<MathChoiceNode>),
    Middle {
        mode: Mode,
        delim: String,
    },
    MClass {
        mode: Mode,
        mclass: &'static str,
        body: Vec<ParseNode>,
        is_character_box: bool,
    },
    OperatorName {
        mode: Mode,
        body: Vec<ParseNode>,
        always_handle_sup_sub: bool,
        limits: bool,
        parent_is_sup_sub: bool,
    },
    Overline {
        mode: Mode,
        body: Box<ParseNode>,
    },
    Underline {
        mode: Mode,
        body: Box<ParseNode>,
    },
    Phantom {
        mode: Mode,
        body: Vec<ParseNode>,
    },
    VPhantom {
        mode: Mode,
        body: Box<ParseNode>,
    },
    Pmb {
        mode: Mode,
        mclass: &'static str,
        body: Vec<ParseNode>,
    },
    RaiseBox {
        mode: Mode,
        dy: Measurement,
        body: Box<ParseNode>,
    },
    Rule {
        mode: Mode,
        shift: Option<Measurement>,
        width: Measurement,
        height: Measurement,
    },
    Sizing {
        mode: Mode,
        size: u8,
        body: Vec<ParseNode>,
    },
    Smash {
        mode: Mode,
        body: Box<ParseNode>,
        smash_height: bool,
        smash_depth: bool,
    },
    Sqrt {
        mode: Mode,
        body: Box<ParseNode>,
        index: Option<Box<ParseNode>>,
    },
    VCenter {
        mode: Mode,
        body: Box<ParseNode>,
    },
    /// `body` is `None` when KaTeX calls the handler without arguments (`\\cdlongequal`).
    XArrow {
        mode: Mode,
        label: String,
        body: Option<Box<ParseNode>>,
        below: Option<Box<ParseNode>>,
    },
}

impl ParseNode {
    /// KaTeX's `type` string.
    pub fn type_name(&self) -> &'static str {
        use ParseNode::*;
        match self {
            Array(_) => "array",
            CdLabel { .. } => "cdlabel",
            CdLabelParent { .. } => "cdlabelparent",
            Color { .. } => "color",
            ColorToken { .. } => "color-token",
            Op(_) => "op",
            OrdGroup(_) => "ordgroup",
            Raw { .. } => "raw",
            Size { .. } => "size",
            Styling { .. } => "styling",
            SupSub(_) => "supsub",
            Tag { .. } => "tag",
            Text { .. } => "text",
            Url { .. } => "url",
            Verb { .. } => "verb",
            Atom(_) => "atom",
            MathOrd(_) => "mathord",
            TextOrd(_) => "textord",
            Spacing(_) => "spacing",
            AccentToken(_) => "accent-token",
            OpToken(_) => "op-token",
            Accent(_) => "accent",
            AccentUnder(_) => "accentUnder",
            Cr { .. } => "cr",
            DelimSizing { .. } => "delimsizing",
            Enclose(_) => "enclose",
            Environment { .. } => "environment",
            Font { .. } => "font",
            GenFrac(_) => "genfrac",
            HBox { .. } => "hbox",
            HorizBrace { .. } => "horizBrace",
            Href { .. } => "href",
            Html { .. } => "html",
            HtmlMathml { .. } => "htmlmathml",
            IncludeGraphics(_) => "includegraphics",
            Infix { .. } => "infix",
            Internal { .. } => "internal",
            Kern { .. } => "kern",
            Lap { .. } => "lap",
            LeftRight(_) => "leftright",
            LeftRightRight { .. } => "leftright-right",
            MathChoice(_) => "mathchoice",
            Middle { .. } => "middle",
            MClass { .. } => "mclass",
            OperatorName { .. } => "operatorname",
            Overline { .. } => "overline",
            Underline { .. } => "underline",
            Phantom { .. } => "phantom",
            VPhantom { .. } => "vphantom",
            Pmb { .. } => "pmb",
            RaiseBox { .. } => "raisebox",
            Rule { .. } => "rule",
            Sizing { .. } => "sizing",
            Smash { .. } => "smash",
            Sqrt { .. } => "sqrt",
            VCenter { .. } => "vcenter",
            XArrow { .. } => "xArrow",
        }
    }

    pub fn mode(&self) -> Mode {
        use ParseNode::*;
        match self {
            Array(n) => n.mode,
            Op(n) => n.mode,
            OrdGroup(n) => n.mode,
            SupSub(n) => n.mode,
            Atom(n) => n.mode,
            MathOrd(n) | TextOrd(n) | Spacing(n) | AccentToken(n) | OpToken(n) => n.mode,
            Accent(n) | AccentUnder(n) => n.mode,
            Enclose(n) => n.mode,
            GenFrac(n) => n.mode,
            IncludeGraphics(n) => n.mode,
            LeftRight(n) => n.mode,
            MathChoice(n) => n.mode,
            CdLabel { mode, .. }
            | CdLabelParent { mode, .. }
            | Color { mode, .. }
            | ColorToken { mode, .. }
            | Raw { mode, .. }
            | Size { mode, .. }
            | Styling { mode, .. }
            | Tag { mode, .. }
            | Text { mode, .. }
            | Url { mode, .. }
            | Verb { mode, .. }
            | Cr { mode, .. }
            | DelimSizing { mode, .. }
            | Environment { mode, .. }
            | Font { mode, .. }
            | HBox { mode, .. }
            | HorizBrace { mode, .. }
            | Href { mode, .. }
            | Html { mode, .. }
            | HtmlMathml { mode, .. }
            | Infix { mode, .. }
            | Internal { mode }
            | Kern { mode, .. }
            | Lap { mode, .. }
            | LeftRightRight { mode, .. }
            | Middle { mode, .. }
            | MClass { mode, .. }
            | OperatorName { mode, .. }
            | Overline { mode, .. }
            | Underline { mode, .. }
            | Phantom { mode, .. }
            | VPhantom { mode, .. }
            | Pmb { mode, .. }
            | RaiseBox { mode, .. }
            | Rule { mode, .. }
            | Sizing { mode, .. }
            | Smash { mode, .. }
            | Sqrt { mode, .. }
            | VCenter { mode, .. }
            | XArrow { mode, .. } => *mode,
        }
    }

    /// The node's `loc` (only the node types KaTeX attaches one to carry it).
    pub fn loc(&self) -> Option<&Loc> {
        match self {
            ParseNode::OrdGroup(n) => n.loc.as_ref(),
            ParseNode::Atom(n) => n.loc.as_ref(),
            ParseNode::MathOrd(n)
            | ParseNode::TextOrd(n)
            | ParseNode::Spacing(n)
            | ParseNode::AccentToken(n)
            | ParseNode::OpToken(n) => n.loc.as_ref(),
            _ => None,
        }
    }

    /// The `text` of a symbol node (`checkSymbolNodeType(node)?.text`).
    pub fn symbol_text(&self) -> Option<&str> {
        match self {
            ParseNode::Atom(n) => Some(&n.text),
            ParseNode::MathOrd(n)
            | ParseNode::TextOrd(n)
            | ParseNode::Spacing(n)
            | ParseNode::AccentToken(n)
            | ParseNode::OpToken(n) => Some(&n.text),
            _ => None,
        }
    }

    /// `checkSymbolNodeType(node) != null`.
    pub fn is_symbol_node(&self) -> bool {
        self.symbol_text().is_some()
    }

    pub fn ordgroup(mode: Mode, body: Vec<ParseNode>) -> ParseNode {
        ParseNode::OrdGroup(OrdGroupNode {
            mode,
            body,
            semisimple: false,
            loc: None,
        })
    }

    pub fn textord(mode: Mode, text: impl Into<String>) -> ParseNode {
        ParseNode::TextOrd(SymNode {
            mode,
            text: text.into(),
            loc: None,
        })
    }

    pub fn mathord(mode: Mode, text: impl Into<String>) -> ParseNode {
        ParseNode::MathOrd(SymNode {
            mode,
            text: text.into(),
            loc: None,
        })
    }
}

/// `assertSymbolNodeType(node).text`.
pub fn assert_symbol_text(node: &ParseNode) -> Result<&str> {
    match node.symbol_text() {
        Some(t) => Ok(t),
        None => ierr(format!(
            "Expected node of symbol group type, but got node of type {}",
            node.type_name()
        )),
    }
}

/// `assertNodeType` failure.
pub fn type_error<T>(expected: &str, got: &ParseNode) -> Result<T> {
    ierr(format!(
        "Expected node of type {}, but got node of type {}",
        expected,
        got.type_name()
    ))
}

/// `normalizeArgument` (defineFunction.ts): unwraps a one-element ordgroup.
pub fn normalize_argument(arg: ParseNode) -> ParseNode {
    match arg {
        ParseNode::OrdGroup(mut g) if g.body.len() == 1 => g.body.pop().unwrap(),
        other => other,
    }
}

/// `normalizeArgument` on a borrowed node.
pub fn normalize_argument_ref(arg: &ParseNode) -> &ParseNode {
    match arg {
        ParseNode::OrdGroup(g) if g.body.len() == 1 => &g.body[0],
        other => other,
    }
}

/// `ordargument` (defineFunction.ts): an ordgroup's body, or the node alone.
pub fn ordargument(arg: ParseNode) -> Vec<ParseNode> {
    match arg {
        ParseNode::OrdGroup(g) => g.body,
        other => vec![other],
    }
}

/// `ordargument` on a borrowed node.
pub fn ordargument_ref(arg: &ParseNode) -> &[ParseNode] {
    match arg {
        ParseNode::OrdGroup(g) => &g.body,
        other => std::slice::from_ref(other),
    }
}

/// `assertCharacterGroup` (parseNode.ts).
pub fn assert_character_group(
    group: &OrdGroupNode,
    error_message: &str,
    allow_spaces: bool,
) -> Result<String> {
    let mut text = String::new();
    for node in &group.body {
        match node {
            ParseNode::TextOrd(n) => text.push_str(&n.text),
            ParseNode::Spacing(n) if allow_spaces && n.text == " " => text.push(' '),
            _ => return perr(error_message, group.loc.as_ref()),
        }
    }
    Ok(text)
}

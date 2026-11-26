//! Port of KaTeX `src/Token.ts` and `src/SourceLocation.ts`.

use std::rc::Rc;

/// The text a lexer reads: the formula itself or a macro body.
#[derive(Clone, Debug)]
pub enum Src {
    Static(&'static str),
    Shared(Rc<str>),
}

impl Src {
    pub fn as_str(&self) -> &str {
        match self {
            Src::Static(s) => s,
            Src::Shared(s) => s,
        }
    }

    /// Lexer identity (KaTeX compares `loc.lexer` objects). Two lexers over the same static
    /// macro body compare equal here; that only changes error-message excerpts.
    pub fn same(&self, other: &Src) -> bool {
        let a = self.as_str();
        let b = other.as_str();
        std::ptr::eq(a.as_ptr(), b.as_ptr()) && a.len() == b.len()
    }
}

/// `SourceLocation`: byte offsets into the lexer's input.
#[derive(Clone, Debug)]
pub struct Loc {
    pub src: Src,
    pub start: u32,
    pub end: u32,
}

impl Loc {
    /// `SourceLocation.range(first, second)`.
    pub fn range(first: Option<&Loc>, second: Option<&Loc>) -> Option<Loc> {
        match (first, second) {
            (Some(a), Some(b)) if a.src.same(&b.src) => Some(Loc {
                src: a.src.clone(),
                start: a.start,
                end: b.end,
            }),
            _ => None,
        }
    }
}

/// `Token`.
#[derive(Clone, Debug)]
pub struct Token {
    pub text: String,
    pub loc: Option<Loc>,
    pub noexpand: bool,
    pub treat_as_relax: bool,
}

impl Token {
    pub fn new(text: impl Into<String>, loc: Option<Loc>) -> Token {
        Token {
            text: text.into(),
            loc,
            noexpand: false,
            treat_as_relax: false,
        }
    }

    /// `token.range(endToken, text)`.
    pub fn range(&self, end: &Token, text: String) -> Token {
        Token::new(text, Loc::range(self.loc.as_ref(), end.loc.as_ref()))
    }
}

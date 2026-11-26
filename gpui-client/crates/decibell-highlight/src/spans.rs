//! The flat result: the engine's tree (nested spans, UTF-16 offsets) folded
//! into ordered byte ranges with the colour the stylesheet would give each.

use std::ops::Range;

use crate::engine::{Event, Node};
use crate::generated::Language;
use crate::{Fallback, Token};

/// A run of text and how it is drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Byte range in the highlighted string.
    pub range: Range<usize>,
    /// The colour: that of the innermost enclosing scope the stylesheet
    /// styles (CSS `color` inherits, and every rule has the same
    /// specificity, so the nearest styled ancestor wins).
    pub token: Token,
    /// Inside a comment, at any depth (`font-style: italic` inherits and
    /// nothing resets it: a doctag in a comment is keyword-coloured italic).
    pub italic: bool,
    path: Range<u32>,
}

/// A highlighted block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Highlighted {
    /// The language after alias resolution; `None` when no registered
    /// language was named (the block is plain text and the header says
    /// "code").
    pub language: Option<Language>,
    /// Set when the language is known but the block is plain text anyway.
    pub fallback: Option<Fallback>,
    /// Consecutive, non-empty runs covering the whole string in order.
    /// Adjacent runs differ in their path.
    pub spans: Vec<Span>,
    paths: Vec<Node>,
}

impl Highlighted {
    /// The full nesting of a span, outermost first: every scope and
    /// sub-language wrapper around it (the `<span>` ancestors lowlight
    /// produces).
    pub fn path(&self, span: &Span) -> &[Node] {
        &self.paths[span.path.start as usize..span.path.end as usize]
    }

    /// The innermost scope around a span, if any.
    pub fn scope(&self, span: &Span) -> Option<crate::Scope> {
        self.path(span).iter().rev().find_map(|n| match n {
            Node::Scope(s) => Some(*s),
            Node::Language(_) => None,
        })
    }

    pub(crate) fn plain(code: &str, language: Option<Language>, fallback: Option<Fallback>) -> Highlighted {
        let spans = if code.is_empty() {
            Vec::new()
        } else {
            vec![Span { range: 0..code.len(), token: Token::Plain, italic: false, path: 0..0 }]
        };
        Highlighted { language, fallback, spans, paths: Vec::new() }
    }

    pub(crate) fn from_events(code: &str, language: Language, events: &[Event]) -> Highlighted {
        let mut out = Highlighted { language: Some(language), fallback: None, spans: Vec::new(), paths: Vec::new() };
        let mut stack: Vec<Node> = Vec::new();
        // where the current stack was last written to `paths`
        let mut stack_path: Option<Range<u32>> = Some(0..0);
        // UTF-16 offset → byte offset, walking forward with the text
        let mut chars = code.char_indices().peekable();
        let mut unit = 0usize;
        let mut to_byte = |target: usize| -> usize {
            while let Some(&(byte, c)) = chars.peek() {
                // a boundary inside a surrogate pair snaps to the character's start
                if unit + c.len_utf16() > target {
                    return byte;
                }
                unit += c.len_utf16();
                chars.next();
            }
            code.len()
        };
        let mut end_byte = 0usize;
        for e in events {
            match *e {
                Event::Open(node) => {
                    stack.push(node);
                    stack_path = None;
                }
                Event::Close => {
                    stack.pop();
                    stack_path = None;
                }
                Event::Text(a, b) => {
                    // the engine emits text in order and without gaps
                    let start = to_byte(a as usize).max(end_byte);
                    let end = to_byte(b as usize).max(start);
                    if start == end {
                        continue;
                    }
                    end_byte = end;
                    if let Some(last) = out.spans.last_mut() {
                        let same = out.paths[last.path.start as usize..last.path.end as usize] == stack[..];
                        if same && last.range.end == start {
                            last.range.end = end;
                            continue;
                        }
                    }
                    let path = match &stack_path {
                        Some(p) => p.clone(),
                        None => {
                            let from = out.paths.len() as u32;
                            out.paths.extend_from_slice(&stack);
                            let p = from..out.paths.len() as u32;
                            stack_path = Some(p.clone());
                            p
                        }
                    };
                    let token = stack
                        .iter()
                        .rev()
                        .find_map(|n| match n {
                            Node::Scope(s) => s.token(),
                            Node::Language(_) => None,
                        })
                        .unwrap_or(Token::Plain);
                    let italic = stack.iter().any(|n| matches!(n, Node::Scope(s) if s.token() == Some(Token::Comment)));
                    out.spans.push(Span { range: start..end, token, italic, path });
                }
            }
        }
        // text the engine did not emit (never expected) stays plain
        if end_byte < code.len() {
            out.spans.push(Span { range: end_byte..code.len(), token: Token::Plain, italic: false, path: 0..0 });
        }
        out
    }
}

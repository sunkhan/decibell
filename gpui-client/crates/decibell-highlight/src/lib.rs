//! highlight.js-compatible code highlighting without a JavaScript engine.
//!
//! The Electron client colours fenced code blocks with lowlight
//! (highlight.js core) for a fixed set of grammars. This crate colours them
//! the same, token for token: the real highlight.js grammars are extracted
//! to data at build time (`tools/extract.mjs`) and interpreted by a Rust
//! port of highlight.js's engine (`engine.rs`), with an ECMAScript regex
//! engine underneath (`regex.rs`). See the README for the architecture, the
//! test strategy and how to move to a new highlight.js version.
//!
//! ```
//! use decibell_highlight::{highlight, Token};
//!
//! let code = "let x = 1; // one";
//! let out = highlight(Some("rs"), code);
//! assert_eq!(out.language.map(|l| l.name()), Some("rust"));
//! let comment = out.spans.iter().find(|s| s.token == Token::Comment).unwrap();
//! assert_eq!(&code[comment.range.clone()], "// one");
//! assert!(comment.italic);
//! ```
//!
//! No GPUI, no I/O. A language's grammar is parsed and its regexes compiled
//! on first use, then kept for the life of the process.

mod callbacks;
mod engine;
mod generated;
mod grammar;
mod regex;
mod spans;

use std::ops::Range;
use std::time::{Duration, Instant};

pub use engine::Node;
pub use generated::{Language, Scope, HIGHLIGHT_JS_VERSION, LOWLIGHT_VERSION};
pub use spans::{Highlighted, Span};

use engine::{Call, Event, Status, Stop};
use generated::{ALL_LANGUAGES, LANGUAGES, SCOPE_NAMES};

impl Language {
    /// Every registered language, in the Electron client's registration order.
    pub fn all() -> &'static [Language] {
        &ALL_LANGUAGES
    }

    /// The name or alias as highlight.js resolves it (case-insensitive):
    /// what `lowlight.registered(lang)` accepts in CodeBlock.tsx.
    pub fn from_name(name: &str) -> Option<Language> {
        engine::get_language(name)
    }

    /// The registered name (`"javascript"`, `"xml"`, …).
    pub fn name(self) -> &'static str {
        LANGUAGES[self as usize].name
    }

    /// highlight.js's display name (`"JavaScript"`, `"HTML, XML"`, …).
    pub fn display_name(self) -> &'static str {
        LANGUAGES[self as usize].display_name
    }

    /// Bytes of embedded grammar data.
    pub fn embedded_size(self) -> usize {
        LANGUAGES[self as usize].data.len()
    }

    /// Whether the grammar has been materialised (it is on first use).
    pub fn is_loaded(self) -> bool {
        grammar::is_loaded(self)
    }

    /// Parses the grammar and compiles all its regexes now, e.g. to move the
    /// first-use cost off a latency-sensitive path. Normal use needs no
    /// preloading: regexes compile as the engine first needs them.
    pub fn preload(self) {
        let _ = grammar::grammar(self).compile_all();
    }
}

impl Scope {
    /// The highlight.js scope name (`"title.function"`).
    pub fn name(self) -> &'static str {
        SCOPE_NAMES[self as usize]
    }

    /// The scope's first tier (`"title"` for `title.function`): the part
    /// that becomes the `hljs-…` class.
    pub fn head(self) -> &'static str {
        let name = self.name();
        name.split_once('.').map_or(name, |(head, _)| head)
    }

    /// The CSS classes lowlight gives the scope's span
    /// (`"hljs-title function_"`).
    pub fn css_classes(self) -> String {
        let mut out = String::new();
        for (i, piece) in self.name().split('.').enumerate() {
            if i == 0 {
                out.push_str("hljs-");
                out.push_str(piece);
            } else {
                out.push(' ');
                out.push_str(piece);
                out.extend(std::iter::repeat_n('_', i));
            }
        }
        out
    }

    /// The colour the renderer's stylesheet gives this scope's span, if it
    /// styles it (globals.css, `.rich-codeblock .hljs-*`).
    pub fn token(self) -> Option<Token> {
        Token::for_class(self.head())
    }
}

/// The colour groups of the renderer's code-block stylesheet
/// (`electron-client/src/styles/globals.css`, `.rich-codeblock .hljs-*`).
/// Each maps to one palette token, the same in every theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    /// No styled scope: the block's own text colour (`--color-text-primary`).
    Plain,
    /// keyword, built_in, literal, doctag, meta → `--color-accent-bright`
    Keyword,
    /// title, name, section, type, selector-tag → `--color-accent`
    Title,
    /// string, regexp, quote, addition → `--color-success`
    String,
    /// number, symbol, attr, attribute, variable, template-variable,
    /// selector-class, selector-id → `--color-warning`
    Number,
    /// comment → `--color-text-muted` (and italic)
    Comment,
    /// deletion → `--color-error`
    Deletion,
    /// operator, punctuation → `--color-text-secondary`
    Operator,
}

impl Token {
    /// The palette token (CSS custom property) this colour group uses.
    pub fn css_var(self) -> &'static str {
        match self {
            Token::Plain => "--color-text-primary",
            Token::Keyword => "--color-accent-bright",
            Token::Title => "--color-accent",
            Token::String => "--color-success",
            Token::Number => "--color-warning",
            Token::Comment => "--color-text-muted",
            Token::Deletion => "--color-error",
            Token::Operator => "--color-text-secondary",
        }
    }

    /// The group that styles `hljs-<class>`, if any.
    pub fn for_class(class: &str) -> Option<Token> {
        Some(match class {
            "keyword" | "built_in" | "literal" | "doctag" | "meta" => Token::Keyword,
            "title" | "name" | "section" | "type" | "selector-tag" => Token::Title,
            "string" | "regexp" | "quote" | "addition" => Token::String,
            "number" | "symbol" | "attr" | "attribute" | "variable" | "template-variable"
            | "selector-class" | "selector-id" => Token::Number,
            "comment" => Token::Comment,
            "deletion" => Token::Deletion,
            "operator" | "punctuation" => Token::Operator,
            _ => return None,
        })
    }
}

/// Limits on one highlight. Highlighting is a backtracking-regex scan, so a
/// hostile or merely unlucky paste can make it slow; past any limit the block
/// is returned as plain text ([`Highlighted::fallback`] says why).
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// Longest input highlighted, in bytes. The Electron client has no cap of
    /// its own (CodeBlock.tsx highlights whatever a message holds).
    pub max_bytes: usize,
    /// Regex work allowed, in matcher steps (one per position tried and per
    /// backtrack). Code spends 10–90 steps per byte depending on the grammar,
    /// at roughly 100–200 million steps a second. A step limit, unlike a
    /// time limit, gives the same result on every machine.
    pub max_steps: u64,
    /// Wall-clock limit, checked every few thousand steps.
    pub max_time: Option<Duration>,
}

impl Default for Budget {
    /// 256 KiB, 100 million steps (about four times what the largest allowed
    /// input normally needs), half a second.
    fn default() -> Budget {
        Budget {
            max_bytes: 256 * 1024,
            max_steps: 100_000_000,
            max_time: Some(Duration::from_millis(500)),
        }
    }
}

impl Budget {
    /// No limits at all (tests, tools).
    pub const UNLIMITED: Budget = Budget { max_bytes: usize::MAX, max_steps: u64::MAX, max_time: None };
}

/// Why a block came back as plain text although its language is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fallback {
    /// Longer than [`Budget::max_bytes`].
    TooLong,
    /// Ran out of [`Budget::max_steps`] or [`Budget::max_time`].
    OutOfBudget,
    /// highlight.js itself gives up on this input (its infinite-loop guard);
    /// lowlight throws and CodeBlock.tsx falls back to plain text too.
    EngineError,
    /// A defect in this crate (a grammar regex failed to compile, or
    /// sub-languages nested absurdly deep). Never expected.
    Internal,
}

/// Restores the thread's regex budget when a highlight ends, however it ends.
struct BudgetGuard;

impl BudgetGuard {
    fn set(budget: &Budget) -> BudgetGuard {
        let steps = if budget.max_steps == u64::MAX { regress::budget::UNLIMITED } else { budget.max_steps };
        regress::budget::set(steps, budget.max_time.map(|t| Instant::now() + t));
        BudgetGuard
    }
}

impl Drop for BudgetGuard {
    fn drop(&mut self) {
        regress::budget::clear();
    }
}

/// Highlights `code` as `language` (a name or alias as typed after the
/// fence, any case) with the default [`Budget`].
///
/// As in CodeBlock.tsx: a missing or unregistered language is plain text;
/// there is no auto-detection of the block's own language.
pub fn highlight(language: Option<&str>, code: &str) -> Highlighted {
    highlight_with(language, code, &Budget::default())
}

/// [`highlight`] with explicit limits.
pub fn highlight_with(language: Option<&str>, code: &str, budget: &Budget) -> Highlighted {
    let Some(language) = language.and_then(Language::from_name) else {
        return Highlighted::plain(code, None, None);
    };
    highlight_language(language, code, budget)
}

/// Highlights `code` in a known language.
pub fn highlight_language(language: Language, code: &str, budget: &Budget) -> Highlighted {
    if code.len() > budget.max_bytes {
        return Highlighted::plain(code, Some(language), Some(Fallback::TooLong));
    }
    let units: Vec<u16> = code.encode_utf16().collect();
    match events(language, &units, budget) {
        Ok(events) => Highlighted::from_events(code, language, &events),
        Err(fallback) => Highlighted::plain(code, Some(language), Some(fallback)),
    }
}

fn events(language: Language, units: &[u16], budget: &Budget) -> Result<Vec<Event>, Fallback> {
    events_and_relevance(language, units, budget).map(|(events, _)| events)
}

fn events_and_relevance(language: Language, units: &[u16], budget: &Budget) -> Result<(Vec<Event>, f64), Fallback> {
    let _guard = BudgetGuard::set(budget);
    let mut call = Call::default();
    match engine::highlight(&mut call, language, units, 0, true, None) {
        Ok(outcome) => match outcome.status {
            Status::Done => Ok((outcome.events, outcome.relevance)),
            // lowlight: `if (result.errorRaised) throw …`, caught by CodeBlock.tsx
            Status::Error | Status::Illegal => Err(Fallback::EngineError),
        },
        Err(Stop::Budget) => Err(Fallback::OutOfBudget),
        Err(Stop::Regex(_) | Stop::Depth) => Err(Fallback::Internal),
    }
}

/// The output tree in a canonical text form, for comparing with lowlight's
/// HAST (see tests/): `(`*classes*`|` opens a span, `)` closes it, text is
/// the UTF-16 length of each run. Offsets are UTF-16 code units, exactly as
/// JavaScript sees the string, so this is also defined where a span boundary
/// splits a surrogate pair (which [`Highlighted`] cannot represent).
#[doc(hidden)]
pub mod testing {
    use super::*;

    /// One element of the tree.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Item {
        /// A span opens, with its class attribute as lowlight writes it.
        Open(String),
        Close,
        /// A text run (UTF-16 offsets into the input).
        Text(Range<usize>),
    }

    /// The tree for `code` and highlight.js's relevance score, or the
    /// fallback reason.
    pub fn tree(language: Language, code: &[u16], budget: &Budget) -> Result<(Vec<Item>, f64), Fallback> {
        let (events, relevance) = events_and_relevance(language, code, budget)?;
        let mut out: Vec<Item> = Vec::with_capacity(events.len());
        let mut open = 0usize;
        for e in events {
            match e {
                Event::Open(Node::Scope(s)) => {
                    open += 1;
                    out.push(Item::Open(s.css_classes()));
                }
                Event::Open(Node::Language(l)) => {
                    open += 1;
                    out.push(Item::Open(l.name().to_string()));
                }
                Event::Close => {
                    open -= 1;
                    out.push(Item::Close);
                }
                Event::Text(a, b) => match out.last_mut() {
                    // lowlight merges adjacent text
                    Some(Item::Text(r)) if r.end == a as usize => r.end = b as usize,
                    _ => out.push(Item::Text(a as usize..b as usize)),
                },
            }
        }
        out.extend(std::iter::repeat_n(Item::Close, open));
        Ok((out, relevance))
    }

    /// The source of the joined regex highlight.js builds for a mode's rules
    /// from `from` on.
    pub fn joined_source(language: Language, mode: u32, from: usize) -> String {
        grammar::grammar(language).joined_source(mode, from)
    }

    /// Compiles every regex of a grammar; returns how many there are.
    pub fn compile_all(language: Language) -> Result<usize, String> {
        grammar::grammar(language).compile_all()
    }

    /// Every scope a grammar's modes and keywords can emit.
    pub fn scopes_used(language: Language) -> Vec<Scope> {
        grammar::grammar(language).scopes_used()
    }

    /// Regex matcher steps a highlight spends (what [`Budget::max_steps`]
    /// counts).
    pub fn steps(language: Language, code: &str) -> u64 {
        const START: u64 = u64::MAX / 2;
        let units: Vec<u16> = code.encode_utf16().collect();
        regress::budget::set(START, None);
        let mut call = Call::default();
        let _ = engine::highlight(&mut call, language, &units, 0, true, None);
        let used = START - regress::budget::remaining();
        regress::budget::clear();
        used
    }

    /// Regex steps left in this thread's budget (`u64::MAX` = unlimited).
    pub fn budget_remaining() -> u64 {
        regress::budget::remaining()
    }
}

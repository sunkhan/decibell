//! Port of KaTeX `src/ParseError.ts`, plus the port's error type.
//!
//! KaTeX distinguishes `ParseError` (rendered, with `throwOnError: false`, as the raw TeX in the
//! error colour) from any other thrown `Error` (an internal failure: `renderToString` rethrows
//! it, and Decibell's renderer then shows the raw TeX as code). [`Error`] keeps that split.

use crate::token::Loc;
use crate::utils::utf16_len;

/// A KaTeX `ParseError`: the raw message and the location it points at.
#[derive(Clone, Debug)]
pub struct ParseError {
    pub raw_message: String,
    pub loc: Option<Loc>,
}

impl ParseError {
    pub fn new(message: impl Into<String>, loc: Option<&Loc>) -> ParseError {
        ParseError {
            raw_message: message.into(),
            loc: loc.cloned(),
        }
    }

    /// The full message KaTeX builds (`error.message`): `"KaTeX parse error: …"` with the
    /// position and an underlined excerpt of the input. Positions count UTF-16 code units,
    /// as in JavaScript.
    pub fn message(&self) -> String {
        let mut error = format!("KaTeX parse error: {}", self.raw_message);
        if let Some(loc) = &self.loc
            && loc.start <= loc.end
        {
            let input = loc.src.as_str();
            let start = loc.start as usize;
            let end = loc.end as usize;
            let ustart = utf16_len(&input[..start]);
            if start == input.len() {
                error.push_str(" at end of input: ");
            } else {
                error.push_str(&format!(" at position {}: ", ustart + 1));
            }
            let mut underlined = String::new();
            for c in input[start..end].chars() {
                underlined.push(c);
                underlined.push('\u{0332}');
            }
            // 15 UTF-16 units of context either side.
            let left = if ustart > 15 {
                format!("…{}", slice_utf16_back(&input[..start], 15))
            } else {
                input[..start].to_string()
            };
            let rest = &input[end..];
            let right = if utf16_len(rest) > 15 {
                format!("{}…", slice_utf16_front(rest, 15))
            } else {
                rest.to_string()
            };
            error.push_str(&left);
            error.push_str(&underlined);
            error.push_str(&right);
        }
        error
    }

    /// `error.toString()` (what KaTeX puts in the error span's `title`).
    pub fn to_js_string(&self) -> String {
        format!("ParseError: {}", self.message())
    }

    /// The UTF-16 offset KaTeX reports as `position`.
    pub fn position(&self) -> Option<usize> {
        self.loc
            .as_ref()
            .filter(|l| l.start <= l.end)
            .map(|l| utf16_len(&l.src.as_str()[..l.start as usize]))
    }
}

fn slice_utf16_back(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices().rev() {
        units += c.len_utf16();
        if units >= n {
            return &s[i..];
        }
    }
    s
}

fn slice_utf16_front(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units >= n {
            return &s[..i];
        }
        units += c.len_utf16();
    }
    s
}

/// Everything that can stop a render.
#[derive(Clone, Debug)]
pub enum Error {
    /// A KaTeX `ParseError` (KaTeX renders it as the raw TeX in the error colour).
    Parse(ParseError),
    /// Any other KaTeX failure (a thrown plain `Error`, e.g. a font without metrics), which
    /// `renderToString` does not catch. Also used for constructs this port refuses.
    Internal(String),
}

impl From<ParseError> for Error {
    fn from(e: ParseError) -> Error {
        Error::Parse(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Parse(p) => f.write_str(&p.message()),
            Error::Internal(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// `throw new ParseError(msg, tokenOrNode)` as an `Err`.
pub fn perr<T>(message: impl Into<String>, loc: Option<&Loc>) -> Result<T> {
    Err(Error::Parse(ParseError::new(message, loc)))
}

/// `throw new Error(msg)` as an `Err`.
pub fn ierr<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Internal(message.into()))
}

/// Nesting bound for the recursive parts of the port (parser, builders, layout). KaTeX relies on
/// the JavaScript stack; a native stack overflow would abort the client, so nesting deeper than
/// this is an internal error (the app then shows the raw TeX, as for any KaTeX exception).
/// Measured in a release build: 2.6–4.8 KB of stack per level depending on the construct, so
/// the deepest formula needs about 0.5 MB of stack (debug builds need several times more).
pub const MAX_DEPTH: u32 = 100;

thread_local! {
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// RAII guard counting one level of recursion.
pub struct DepthGuard(());

impl DepthGuard {
    pub fn enter() -> Result<DepthGuard> {
        let d = DEPTH.with(|d| {
            let v = d.get() + 1;
            d.set(v);
            v
        });
        if d > MAX_DEPTH {
            DEPTH.with(|c| c.set(c.get() - 1));
            return Err(Error::Internal(
                "decibell-tex: formula nested too deeply".into(),
            ));
        }
        Ok(DepthGuard(()))
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

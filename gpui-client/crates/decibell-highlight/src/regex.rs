//! The regex engine behind the grammars: `regress` (ECMAScript syntax and
//! semantics), run over UTF-16 code units the way JavaScript runs them.
//!
//! highlight.js builds every regex with the `m` flag, plus `i` for
//! case-insensitive languages and `u` for `unicodeRegex` ones. Without `u` a
//! JavaScript regex sees a string as UCS-2 code units (an astral character is
//! two "characters"); with `u` as code points. regress has both views.

use regress::{Flags, Match, Regex};

pub(crate) struct Re {
    re: Regex,
    unicode: bool,
}

/// A regex gave up because the thread's match budget ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OutOfBudget;

impl Re {
    pub fn new(source: &str, case_insensitive: bool, unicode: bool) -> Result<Re, String> {
        let flags = Flags {
            icase: case_insensitive,
            multiline: true,
            unicode,
            ..Flags::default()
        };
        Regex::with_flags(source, flags)
            .map(|re| Re { re, unicode })
            .map_err(|e| format!("{e} in /{source}/"))
    }

    /// The first match starting at or after `start` (JavaScript `exec` on a
    /// global regex with `lastIndex = start`).
    pub fn find_from(&self, text: &[u16], start: usize) -> Result<Option<Match>, OutOfBudget> {
        if start > text.len() {
            return Ok(None);
        }
        let found = if self.unicode {
            self.re.find_from_utf16(text, start).next()
        } else {
            self.re.find_from_ucs2(text, start).next()
        };
        if found.is_none() && regress::budget::exhausted() {
            return Err(OutOfBudget);
        }
        Ok(found)
    }
}

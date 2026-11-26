//! Port of KaTeX `src/Lexer.ts`. KaTeX lexes with one sticky regular expression; this is the
//! same grammar written out by hand, alternative by alternative, in the regex's order:
//!
//! 1. `[ \r\n\t]+` → `" "`
//! 2. `\\(\n|[ \r\t]+\n?)[ \r\t]*` → `"\\ "`
//! 3. a. one BMP code point from `!`..`[`, `]`..U+2027, U+202A..U+D7FF, U+F900..U+FFFF, plus combining marks
//!    b. one astral code point plus combining marks
//!    c. `\verb*` + delimiter + text on one line + delimiter
//!    d. `\verb` + delimiter (not `*` or a letter) + text on one line + delimiter
//!    e. `\\[a-zA-Z@]+` (trailing whitespace consumed, not part of the text)
//!    f. `\\` + any non-surrogate code unit
//!
//! Anything else is `Unexpected character`. Offsets are bytes (error messages convert to the
//! UTF-16 positions KaTeX reports).

use crate::parse_error::{Result, perr};
use crate::token::{Loc, Src, Token};

pub struct Lexer {
    pub src: Src,
    pos: usize,
    catcodes: Vec<(char, u8)>,
}

fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\r' | '\n' | '\t')
}

fn is_combining(c: char) -> bool {
    ('\u{0300}'..='\u{036f}').contains(&c)
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// Alternative 3a's character class.
fn is_single_bmp(c: char) -> bool {
    let u = c as u32;
    (0x21..=0x5B).contains(&u)
        || (0x5D..=0x2027).contains(&u)
        || (0x202A..=0xD7FF).contains(&u)
        || (0xF900..=0xFFFF).contains(&u)
}

impl Lexer {
    pub fn new(src: Src) -> Lexer {
        Lexer {
            src,
            pos: 0,
            catcodes: vec![('%', 14), ('~', 13)],
        }
    }

    pub fn set_catcode(&mut self, c: char, code: u8) {
        if let Some(e) = self.catcodes.iter_mut().find(|e| e.0 == c) {
            e.1 = code;
        } else {
            self.catcodes.push((c, code));
        }
    }

    /// `this.catcodes[text]`.
    pub fn catcode(&self, text: &str) -> Option<u8> {
        let mut it = text.chars();
        let c = it.next()?;
        if it.next().is_some() {
            return None;
        }
        self.catcodes.iter().find(|e| e.0 == c).map(|e| e.1)
    }

    fn loc(&self, start: usize, end: usize) -> Option<Loc> {
        Some(Loc {
            src: self.src.clone(),
            start: start as u32,
            end: end as u32,
        })
    }

    /// Scans `\verb` text after the delimiter at byte `d_at` (delimiter `d`); returns the end
    /// offset after the closing delimiter.
    fn scan_verb(input: &str, d_at: usize, d: char) -> Option<usize> {
        let mut i = d_at + d.len_utf8();
        for c in input[i..].chars() {
            if c == d {
                return Some(i + c.len_utf8());
            }
            if is_line_terminator(c) {
                return None;
            }
            i += c.len_utf8();
        }
        None
    }

    /// Matches one token at `pos`: returns (text, end offset), or None if nothing matches here.
    fn match_at(input: &str, pos: usize) -> Option<(String, usize)> {
        let rest = &input[pos..];
        let mut chars = rest.char_indices();
        let (_, c) = chars.next()?;
        if is_space(c) {
            let end = rest
                .find(|ch: char| !is_space(ch))
                .map_or(input.len(), |i| pos + i);
            return Some((" ".to_string(), end));
        }
        if c == '\\' {
            let n = rest[1..].chars().next()?;
            // 2. control space
            if n == '\n' || matches!(n, ' ' | '\r' | '\t') {
                let mut i = pos + 1;
                if n == '\n' {
                    i += 1;
                } else {
                    while let Some(ch) = input[i..].chars().next() {
                        if matches!(ch, ' ' | '\r' | '\t') {
                            i += 1
                        } else {
                            break;
                        }
                    }
                    if input[i..].starts_with('\n') {
                        i += 1;
                    }
                }
                while let Some(ch) = input[i..].chars().next() {
                    if matches!(ch, ' ' | '\r' | '\t') {
                        i += 1
                    } else {
                        break;
                    }
                }
                return Some(("\\ ".to_string(), i));
            }
            // 3c. \verb*
            if rest.starts_with("\\verb*") {
                let d_at = pos + 6;
                if let Some(d) = input[d_at..].chars().next()
                    && (d as u32) <= 0xFFFF
                    && let Some(end) = Self::scan_verb(input, d_at, d)
                {
                    return Some((input[pos..end].to_string(), end));
                }
            }
            // 3d. \verb
            if rest.starts_with("\\verb") {
                let d_at = pos + 5;
                if let Some(d) = input[d_at..].chars().next()
                    && d != '*'
                    && !d.is_ascii_alphabetic()
                    && (d as u32) <= 0xFFFF
                    && let Some(end) = Self::scan_verb(input, d_at, d)
                {
                    return Some((input[pos..end].to_string(), end));
                }
            }
            // 3e. control word
            if n.is_ascii_alphabetic() || n == '@' {
                let mut i = pos + 1;
                while let Some(ch) = input[i..].chars().next() {
                    if ch.is_ascii_alphabetic() || ch == '@' {
                        i += 1
                    } else {
                        break;
                    }
                }
                let text = input[pos..i].to_string();
                while let Some(ch) = input[i..].chars().next() {
                    if is_space(ch) { i += 1 } else { break }
                }
                return Some((text, i));
            }
            // 3f. control symbol (any BMP code unit)
            if (n as u32) <= 0xFFFF {
                let end = pos + 1 + n.len_utf8();
                return Some((input[pos..end].to_string(), end));
            }
            return None;
        }
        if is_single_bmp(c) || (c as u32) > 0xFFFF {
            let mut i = pos + c.len_utf8();
            for (_, ch) in chars {
                if is_combining(ch) {
                    i += ch.len_utf8()
                } else {
                    break;
                }
            }
            return Some((input[pos..i].to_string(), i));
        }
        None
    }

    /// `lex()`.
    pub fn lex(&mut self) -> Result<Token> {
        loop {
            let input = self.src.as_str();
            let pos = self.pos;
            if pos == input.len() {
                return Ok(Token::new("EOF", self.loc(pos, pos)));
            }
            let Some((text, end)) = Self::match_at(input, pos) else {
                let c = input[pos..].chars().next().unwrap();
                return perr(
                    format!("Unexpected character: '{}'", c),
                    self.loc(pos, pos + c.len_utf8()).as_ref(),
                );
            };
            self.pos = end;
            if self.catcode(&text) == Some(14) {
                // comment: skip to after the next newline (or to EOF)
                let input = self.src.as_str();
                match input[self.pos..].find('\n') {
                    Some(i) => self.pos += i + 1,
                    None => self.pos = input.len(),
                }
                continue;
            }
            return Ok(Token::new(text, self.loc(pos, end)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex_all(s: &'static str) -> Vec<String> {
        let mut l = Lexer::new(Src::Static(s));
        let mut out = vec![];
        loop {
            let t = l.lex().unwrap();
            if t.text == "EOF" {
                break;
            }
            out.push(t.text);
        }
        out
    }

    #[test]
    fn tokens() {
        assert_eq!(
            lex_all("x^2 + \\alpha  b"),
            vec!["x", "^", "2", " ", "+", " ", "\\alpha", "b"]
        );
        assert_eq!(lex_all("\\verb|a b|c"), vec!["\\verb|a b|", "c"]);
        assert_eq!(lex_all("a%comment\nb"), vec!["a", "b"]);
        assert_eq!(lex_all("\\ \n x"), vec!["\\ ", "x"]);
        assert_eq!(lex_all("e\u{301}\\\\"), vec!["e\u{301}", "\\\\"]);
    }
}

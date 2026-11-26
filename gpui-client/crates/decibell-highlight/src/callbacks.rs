//! Hand-written twins of the functions in the highlight.js grammars.
//!
//! A grammar can attach JavaScript callbacks to a mode (`on:begin`,
//! `on:end`, and the internal `__beforeBegin`); they can veto a match
//! (`response.ignoreMatch()`) and keep a value on the mode (`response.data`).
//! tools/extract.mjs identifies each by its exact source text and refuses a
//! function it does not know, so this list is complete for the pinned
//! highlight.js version: the `match` below has no wildcard arm on purpose.

use crate::engine::MMatch;
use crate::generated::Callback;

/// What a callback did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Keep,
    /// `response.ignoreMatch()`
    Ignore,
}

/// JavaScript `\s` (WhiteSpace + LineTerminator).
pub(crate) fn is_js_space(u: u16) -> bool {
    matches!(
        u,
        0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

fn skip_spaces(s: &[u16]) -> usize {
    s.iter().take_while(|&&u| is_js_space(u)).count()
}

fn find(haystack: &[u16], needle: &[u16], from: usize) -> bool {
    if needle.is_empty() {
        return true;
    }
    if from >= haystack.len() || haystack.len() - from < needle.len() {
        return false;
    }
    haystack[from..].windows(needle.len()).any(|w| w == needle)
}

/// Runs a callback. `input` is the text being highlighted (`match.input`),
/// `data` the mode's `response.data._beginMatch` slot (`None` = undefined).
pub(crate) fn run(cb: Callback, m: &MMatch, input: &[u16], data: &mut Option<Vec<u16>>) -> Verdict {
    let group = |n: usize| m.group(n).map(|r| &input[r]);
    match cb {
        // const before = match.input[match.index - 1];
        // if (before === ".") response.ignoreMatch();
        Callback::SkipIfHasPrecedingDot => {
            if m.index() > 0 && input[m.index() - 1] == u16::from(b'.') {
                return Verdict::Ignore;
            }
            Verdict::Keep
        }
        // if (m.index !== 0) resp.ignoreMatch();
        Callback::ShebangAtStart => {
            if m.index() != 0 {
                Verdict::Ignore
            } else {
                Verdict::Keep
            }
        }
        // resp.data._beginMatch = m[1];
        Callback::SaveBeginMatch => {
            *data = group(1).map(<[u16]>::to_vec);
            Verdict::Keep
        }
        // resp.data._beginMatch = m[1] || m[2];
        Callback::SaveBeginMatch1Or2 => {
            *data = match group(1) {
                Some(g) if !g.is_empty() => Some(g.to_vec()),
                _ => group(2).map(<[u16]>::to_vec),
            };
            Verdict::Keep
        }
        // if (resp.data._beginMatch !== m[1]) resp.ignoreMatch();
        Callback::EndSameAsBegin => {
            if data.as_deref() != group(1) {
                Verdict::Ignore
            } else {
                Verdict::Keep
            }
        }
        Callback::JsIsTrulyOpeningTag => js_is_truly_opening_tag(m, input),
    }
}

/// javascript.js `XML_TAG.isTrulyOpeningTag`: is `<Name` the start of JSX,
/// or a generic / comparison?
fn js_is_truly_opening_tag(m: &MMatch, input: &[u16]) -> Verdict {
    let lexeme = &input[m.range()];
    let after = m.index() + lexeme.len();
    let next = input.get(after).copied();
    // `<Array<Array<number>>`, `<T, A extends keyof T, V>`
    if next == Some(u16::from(b'<')) || next == Some(u16::from(b',')) {
        return Verdict::Ignore;
    }
    let mut verdict = Verdict::Keep;
    // `<something>`: look for a matching closing tag
    if next == Some(u16::from(b'>')) {
        // hasClosingTag: "</" + match[0].slice(1), anywhere after the match
        let mut tag: Vec<u16> = vec![u16::from(b'<'), u16::from(b'/')];
        tag.extend_from_slice(lexeme.get(1..).unwrap_or(&[]));
        if !find(input, &tag, after) {
            verdict = Verdict::Ignore;
        }
    }
    let rest = input.get(after..).unwrap_or(&[]);
    // afterMatch.match(/^\s*=/): `<T = any>(key?: string) => …`
    if rest.get(skip_spaces(rest)) == Some(&u16::from(b'=')) {
        return Verdict::Ignore;
    }
    // afterMatch.match(/^\s+extends\s+/): `<From extends string>`
    let lead = skip_spaces(rest);
    if lead > 0 {
        let word: Vec<u16> = "extends".encode_utf16().collect();
        let tail = &rest[lead..];
        if tail.starts_with(&word) && tail.get(word.len()).is_some_and(|&u| is_js_space(u)) {
            return Verdict::Ignore;
        }
    }
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Runs `cb` on the first match of `pattern` in `text` at or after `start`.
    fn on(cb: Callback, pattern: &str, text: &str, start: usize, data: &mut Option<Vec<u16>>) -> Verdict {
        let input = u(text);
        let m = MMatch::for_test(pattern, &input, start);
        run(cb, &m, &input, data)
    }

    #[test]
    fn skip_if_has_preceding_dot() {
        let cb = Callback::SkipIfHasPrecedingDot;
        assert_eq!(on(cb, r"\bclass\b", "a.class", 0, &mut None), Verdict::Ignore);
        assert_eq!(on(cb, r"\bclass\b", "class A", 0, &mut None), Verdict::Keep);
        assert_eq!(on(cb, r"\bclass\b", "a. class", 0, &mut None), Verdict::Keep);
        assert_eq!(on(cb, r"\bclass\b", "..class", 0, &mut None), Verdict::Ignore);
    }

    #[test]
    fn shebang_at_start() {
        let cb = Callback::ShebangAtStart;
        assert_eq!(on(cb, r"^#![ ]*\/", "#!/bin/sh", 0, &mut None), Verdict::Keep);
        // `^` also matches after a newline (the m flag); the callback is what
        // restricts the shebang to the very start
        assert_eq!(on(cb, r"^#![ ]*\/", "x\n#!/bin/sh", 0, &mut None), Verdict::Ignore);
    }

    #[test]
    fn end_same_as_begin() {
        let mut data = None;
        assert_eq!(on(Callback::SaveBeginMatch, r"<<-?(\w+)", "cat <<EOF", 0, &mut data), Verdict::Keep);
        assert_eq!(data, Some(u("EOF")));
        assert_eq!(on(Callback::EndSameAsBegin, r"^(\w+)$", "EOX", 0, &mut data), Verdict::Ignore);
        assert_eq!(on(Callback::EndSameAsBegin, r"^(\w+)$", "EOF", 0, &mut data), Verdict::Keep);
        // a longer word that starts the same is not the delimiter
        assert_eq!(on(Callback::EndSameAsBegin, r"(\w+)", "EOFX", 0, &mut data), Verdict::Ignore);
    }

    #[test]
    fn end_same_as_begin_undefined_groups() {
        // m[1] undefined on both sides: `undefined !== undefined` is false
        let mut data = Some(u("stale"));
        assert_eq!(on(Callback::SaveBeginMatch, r"a|(b)", "a", 0, &mut data), Verdict::Keep);
        assert_eq!(data, None);
        assert_eq!(on(Callback::EndSameAsBegin, r"a|(b)", "a", 0, &mut data), Verdict::Keep);
        assert_eq!(on(Callback::EndSameAsBegin, r"a|(b)", "b", 0, &mut data), Verdict::Ignore);
        // an empty capture is not undefined
        let mut data = None;
        assert_eq!(on(Callback::EndSameAsBegin, r"x(y*)", "x", 0, &mut data), Verdict::Ignore);
    }

    #[test]
    fn php_heredoc_begin() {
        let cb = Callback::SaveBeginMatch1Or2;
        let pattern = r#"<<<[ \t]*(?:(\w+)|"(\w+)")\n"#;
        let mut data = None;
        assert_eq!(on(cb, pattern, "<<<EOT\n", 0, &mut data), Verdict::Keep);
        assert_eq!(data, Some(u("EOT")));
        assert_eq!(on(cb, pattern, "<<< \"SQL\"\n", 0, &mut data), Verdict::Keep);
        assert_eq!(data, Some(u("SQL")));
        // `m[1] || m[2]` with an empty m[1] takes m[2], here undefined
        assert_eq!(on(cb, r"x(a*)(b)?", "x", 0, &mut data), Verdict::Keep);
        assert_eq!(data, None);
    }

    #[test]
    fn js_is_truly_opening_tag() {
        let cb = Callback::JsIsTrulyOpeningTag;
        let tag = r"<[A-Za-z0-9\\._:-]+";
        let check = |text: &str| on(cb, tag, text, 0, &mut None);
        // generics
        assert_eq!(check("<T, U>(a: T) => a"), Verdict::Ignore);
        assert_eq!(check("<Array<Array<number>>>"), Verdict::Ignore);
        assert_eq!(check("<T = any>(key?: string) => 1"), Verdict::Ignore);
        assert_eq!(check("<T=any>"), Verdict::Ignore);
        assert_eq!(check("<From extends string>"), Verdict::Ignore);
        assert_eq!(check("<From\textends\u{a0}string>"), Verdict::Ignore);
        // `<name>` needs its closing tag somewhere after
        assert_eq!(check("<div>text</div>"), Verdict::Keep);
        assert_eq!(check("<div>text"), Verdict::Ignore);
        assert_eq!(check("<div>text</divx>"), Verdict::Keep); // indexOf("</div") is enough
        assert_eq!(check("<T>(x: T) => x"), Verdict::Ignore);
        // attributes, self-closing: no look-ahead applies
        assert_eq!(check("<div className=\"a\">"), Verdict::Keep);
        assert_eq!(check("<br />"), Verdict::Keep);
        assert_eq!(check("<a href={b}>c</a>"), Verdict::Keep);
        // not the word `extends`, or nothing after it
        assert_eq!(check("<From extendsX>"), Verdict::Keep);
        assert_eq!(check("<From extends"), Verdict::Keep);
        // at the very end of the text
        assert_eq!(check("<div"), Verdict::Keep);
        // the match is not at the start of the text
        let input = u("x = <b>c;\n</b>");
        let m = MMatch::for_test(tag, &input, 2);
        assert_eq!(run(cb, &m, &input, &mut None), Verdict::Keep);
        let input = u("</b> x = <b>c;");
        let m = MMatch::for_test(tag, &input, 5);
        assert_eq!(run(cb, &m, &input, &mut None), Verdict::Ignore);
    }

    #[test]
    fn js_space() {
        for c in " \t\n\r\u{b}\u{c}\u{a0}\u{1680}\u{2000}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}".encode_utf16() {
            assert!(is_js_space(c), "{c:#x}");
        }
        for c in "a0_\u{200b}\u{180e}\u{85}".encode_utf16() {
            assert!(!is_js_space(c), "{c:#x}");
        }
    }
}

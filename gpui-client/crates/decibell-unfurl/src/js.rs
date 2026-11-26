//! The few JavaScript semantics the ported TypeScript leans on, so the Rust port gives the same
//! answers: `\s` / `String.prototype.trim` whitespace, `parseInt(s, 10)`, `Number → String`,
//! lengths and slices in UTF-16 code units, and `encodeURIComponent`.

/// JS `\s` and `trim()` whitespace: WhiteSpace + LineTerminator (ECMA-262). Differs from Rust's
/// `char::is_whitespace` (U+FEFF is in, U+0085 is out).
pub(crate) fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// The same set as a regex character-class body, for patterns ported from `\s`.
pub(crate) const SPACE_CLASS: &str = r"\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}";

/// `s.trim()`.
pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `s.trimEnd()`.
pub(crate) fn trim_end(s: &str) -> &str {
    s.trim_end_matches(is_space)
}

/// `s.length`: UTF-16 code units.
pub(crate) fn len16(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// `s.slice(0, n)` in UTF-16 code units. A surrogate pair cut in half leaves JS a lone high
/// surrogate, which Rust strings cannot hold; it becomes U+FFFD, which is also what Chromium draws
/// for it and what a USVString conversion (URLSearchParams) turns it into.
pub(crate) fn slice16(s: &str, n: usize) -> String {
    let mut out = String::with_capacity(s.len().min(n * 3));
    let mut used = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if used + w > n {
            if used < n {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// `parseInt(s, 10)`: leading JS whitespace, an optional sign, then ASCII digits up to the first
/// non-digit. `None` is NaN (no digits). Large values lose precision as in JS (an f64).
pub(crate) fn parse_int(s: &str) -> Option<f64> {
    let t = s.trim_start_matches(is_space);
    let (neg, digits) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let end = digits
        .bytes()
        .position(|b| !b.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    // Parsing the digit run as a float rounds the way JS's StringToNumber does for long runs.
    let v: f64 = digits[..end].parse().ok()?;
    Some(if neg { -v } else { v })
}

/// `String(n)` for a finite number (Number::toString(10)), via Rust's shortest round-trip digits,
/// which are the same digits JS picks.
pub(crate) fn number_to_string(n: f64) -> String {
    if n == 0.0 {
        return "0".into();
    }
    if !n.is_finite() {
        return if n.is_nan() {
            "NaN".into()
        } else if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    let sign = if n < 0.0 { "-" } else { "" };
    // "d.ddde±x" → digits "dddd" and the decimal exponent of the first digit.
    let e = format!("{:e}", n.abs());
    let (mant, exp) = e.split_once('e').expect("LowerExp always has an exponent");
    let exp: i32 = exp.parse().expect("integer exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n_ = exp + 1; // ECMA-262's n: value = 0.digits × 10^n
    let body = if k <= n_ && n_ <= 21 {
        format!("{digits}{}", "0".repeat((n_ - k) as usize))
    } else if 0 < n_ && n_ <= 21 {
        format!("{}.{}", &digits[..n_ as usize], &digits[n_ as usize..])
    } else if -6 < n_ && n_ <= 0 {
        format!("0.{}{digits}", "0".repeat((-n_) as usize))
    } else {
        let e_sign = if n_ > 0 { "+" } else { "-" };
        let e_abs = (n_ - 1).abs();
        if k == 1 {
            format!("{digits}e{e_sign}{e_abs}")
        } else {
            format!("{}.{}e{e_sign}{e_abs}", &digits[..1], &digits[1..])
        }
    };
    format!("{sign}{body}")
}

/// `encodeURIComponent(s)`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )` as UTF-8 %XX.
pub(crate) fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A regex character class matching `word` ASCII-case-insensitively (`ci("meta")` →
/// `[Mm][Ee][Tt][Aa]`): JS's `/i` without `/u` only folds ASCII here, where Rust's `(?i)` would
/// also fold U+212A KELVIN SIGN into `k` and U+017F LONG S into `s`.
pub(crate) fn ci(word: &str) -> String {
    let mut out = String::new();
    for c in word.chars() {
        if c.is_ascii_alphabetic() {
            out.push('[');
            out.push(c.to_ascii_uppercase());
            out.push(c.to_ascii_lowercase());
            out.push(']');
        } else {
            out.push_str(&regex::escape(&c.to_string()));
        }
    }
    out
}

/// Bytes as Node's `latin1` (each byte is one code point U+0000..U+00FF). Node decodes HTTP header
/// values this way, and the TS uses it to scan raw bytes for markers.
pub(crate) fn latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_matches_js() {
        assert_eq!(parse_int("640"), Some(640.0));
        assert_eq!(parse_int("  640px"), Some(640.0));
        assert_eq!(parse_int("\u{FEFF}\u{A0}+12"), Some(12.0));
        assert_eq!(parse_int("-3"), Some(-3.0));
        assert_eq!(parse_int("6.4e2"), Some(6.0));
        assert_eq!(parse_int("0x280"), Some(0.0));
        assert_eq!(parse_int("px"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_int("\u{85}1"), None); // NEL is not JS whitespace
    }

    #[test]
    fn number_to_string_matches_js() {
        assert_eq!(number_to_string(480.0), "480");
        assert_eq!(number_to_string(480.5), "480.5");
        assert_eq!(number_to_string(-0.0), "0");
        assert_eq!(number_to_string(1e21), "1e+21");
        assert_eq!(number_to_string(1.5e21), "1.5e+21");
        assert_eq!(number_to_string(1e20), "100000000000000000000");
        assert_eq!(number_to_string(0.000001), "0.000001");
        assert_eq!(number_to_string(1.5e-7), "1.5e-7");
        assert_eq!(
            number_to_string(12345678901234567890.0),
            "12345678901234567000"
        );
        assert_eq!(number_to_string(-42.25), "-42.25");
    }

    #[test]
    fn utf16_slicing() {
        assert_eq!(len16("a😀b"), 4);
        assert_eq!(slice16("a😀b", 2), "a\u{FFFD}");
        assert_eq!(slice16("a😀b", 3), "a😀");
        assert_eq!(slice16("abc", 10), "abc");
    }

    #[test]
    fn trims_js_whitespace_only() {
        assert_eq!(trim("\u{FEFF} x \u{3000}"), "x");
        assert_eq!(trim("\u{85}x"), "\u{85}x");
    }

    #[test]
    fn encode_uri_component_matches_js() {
        assert_eq!(encode_uri_component("aZ09-_.!~*'()"), "aZ09-_.!~*'()");
        assert_eq!(encode_uri_component("a b/c?é"), "a%20b%2Fc%3F%C3%A9");
    }
}

//! Port of KaTeX `src/utils.ts` plus the JavaScript number semantics the rest of the port
//! relies on: `Number.prototype.toString` (every number KaTeX writes into markup goes through
//! it) and `toFixed(4)` (`makeEm`, in `units.rs`).

use std::fmt::Write as _;

/// JavaScript `ToString(number)` (ECMA-262 Number::toString with radix 10).
pub fn js_num(x: f64) -> String {
    let mut s = String::new();
    write_js_num(&mut s, x);
    s
}

/// Appends JavaScript `ToString(number)` of `x` to `out`.
pub fn write_js_num(out: &mut String, x: f64) {
    if x.is_nan() {
        out.push_str("NaN");
        return;
    }
    if x == 0.0 {
        out.push('0');
        return;
    }
    if x.is_infinite() {
        out.push_str(if x > 0.0 { "Infinity" } else { "-Infinity" });
        return;
    }
    if x < 0.0 {
        out.push('-');
    }
    let a = x.abs();
    // Fast path: small integers.
    if a < 1e15 && a.fract() == 0.0 {
        let _ = write!(out, "{}", a as u64);
        return;
    }
    // Shortest round-trip digits, as JS uses. Rust's `{:e}` prints them as d.ddddde±x.
    let e = format!("{:e}", a);
    let (mant, exp) = e.split_once('e').expect("exp format");
    let exp: i32 = exp.parse().expect("exponent");
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exp + 1; // x = 0.d1d2... × 10^n
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        let e1 = n - 1;
        if e1 >= 0 {
            out.push('+');
        }
        let _ = write!(out, "{}", e1);
    }
}

/// `+x.toFixed(4)`: the value KaTeX's `makeEm` writes, as a number. `toFixed` rounds the
/// exact binary value half away from zero; the parse back gives the nearest double to the
/// four-decimal string, which is exactly `n / 1e4` for the integer `n`.
pub fn to_fixed4(x: f64) -> f64 {
    if !x.is_finite() || x.abs() >= 1e21 {
        return x;
    }
    let neg = x < 0.0;
    let a = x.abs();
    let y = a * 10000.0;
    let fl = y.floor();
    let frac = y - fl;
    let n = if (frac - 0.5).abs() > 1e-6 {
        if frac > 0.5 { fl + 1.0 } else { fl }
    } else {
        // Near a tie: decide on the exact decimal expansion of `a`.
        // Any double that can be within 1e-6 of a tie has at most ~70 fractional
        // binary digits, so 90 decimal places are exact.
        let s = format!("{:.90}", a);
        let (int_part, frac_part) = s.split_once('.').unwrap();
        let head: String = frac_part.chars().take(4).collect();
        let rest = &frac_part[4..];
        let base: f64 = format!("{}{}", int_part, head).parse::<f64>().unwrap();
        let first = rest.as_bytes()[0];
        let up = first >= b'5' /* tie or above: toFixed picks the larger n */;
        if up { base + 1.0 } else { base }
    };
    let r = n / 10000.0;
    if neg { -r } else { r }
}

/// KaTeX `utils.escape`: HTML-escapes `& < > " '`.
pub fn escape_into(out: &mut String, text: &str) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '>' => out.push_str("&gt;"),
            '<' => out.push_str("&lt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
}

/// The first UTF-16 code unit of `s` (JS `s.charCodeAt(0)`), or `None` for "".
pub fn first_code_unit(s: &str) -> Option<u16> {
    let c = s.chars().next()?;
    let mut buf = [0u16; 2];
    Some(c.encode_utf16(&mut buf)[0])
}

/// Length of `s` in UTF-16 code units (JS `s.length`).
pub fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

use crate::parse_node::ParseNode;

/// `getBaseElem(group)`.
pub fn get_base_elem(group: &ParseNode) -> &ParseNode {
    match group {
        ParseNode::OrdGroup(g) if g.body.len() == 1 => get_base_elem(&g.body[0]),
        ParseNode::Color { body, .. } if body.len() == 1 => get_base_elem(&body[0]),
        ParseNode::Font { body, .. } => get_base_elem(body),
        _ => group,
    }
}

/// `isCharacterBox(group)`.
pub fn is_character_box(group: &ParseNode) -> bool {
    matches!(
        get_base_elem(group),
        ParseNode::MathOrd(_) | ParseNode::TextOrd(_) | ParseNode::Atom(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_formatting() {
        assert_eq!(js_num(0.0), "0");
        assert_eq!(js_num(-0.0), "0");
        assert_eq!(js_num(1.5), "1.5");
        assert_eq!(js_num(-0.2222), "-0.2222");
        assert_eq!(js_num(400000.0), "400000");
        assert_eq!(js_num(1e21), "1e+21");
        assert_eq!(js_num(1.5e-7), "1.5e-7");
        assert_eq!(js_num(0.000001), "0.000001");
        assert_eq!(js_num(0.1 + 0.2), "0.30000000000000004");
        assert_eq!(js_num(123456789012345680000.0), "123456789012345680000");
    }

    #[test]
    fn to_fixed_rounding() {
        assert_eq!(to_fixed4(0.22222), 0.2222);
        assert_eq!(to_fixed4(1.03125), 1.0313); // exact tie: larger n
        assert_eq!(to_fixed4(-1.03125), -1.0313);
        assert_eq!(to_fixed4(0.00004), 0.0);
        assert_eq!(to_fixed4(-0.00004), 0.0);
        assert_eq!(to_fixed4(0.69444), 0.6944);
        assert_eq!(to_fixed4(1.00005), 1.0001); // 1.00005 is above the tie in binary
    }
}

//! Port of KaTeX `src/functions/char.ts` (`\@char`, the target of the `\char` macro).

use crate::define_function::{FunctionContext, FunctionSpec};
use crate::parse_error::{Result, perr};
use crate::parse_node::{ParseNode, SymNode, assert_character_group, type_error};

/// JavaScript `parseInt(s)` (no radix): leading whitespace, sign, `0x` → hex, else decimal.
pub fn js_parse_int(s: &str) -> f64 {
    let t = s.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let (neg, t) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let (radix, digits) = if t.len() >= 2 && (t.starts_with("0x") || t.starts_with("0X")) {
        (16, &t[2..])
    } else {
        (10, t)
    };
    let mut v = 0.0f64;
    let mut any = false;
    for c in digits.chars() {
        match c.to_digit(radix) {
            Some(d) => {
                v = v * radix as f64 + d as f64;
                any = true;
            }
            None => break,
        }
    }
    if !any {
        return f64::NAN;
    }
    if neg { -v } else { v }
}

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a = args.into_iter().next().unwrap();
    let ParseNode::OrdGroup(arg) = &a else {
        return type_error("ordgroup", &a);
    };
    let number = assert_character_group(arg, "\\@char has non-numeric argument", false)?;
    let code = js_parse_int(&number);
    let text = if code.is_nan() {
        return perr(format!("\\@char has non-numeric argument {}", number), None);
    } else if code < 0.0 || code >= 1_114_111.0 {
        return perr(format!("\\@char with invalid code point {}", number), None);
    } else {
        // Lone surrogates (0xD800–0xDFFF) cannot exist in a Rust string: U+FFFD stands in.
        char::from_u32(code as u32)
            .unwrap_or('\u{FFFD}')
            .to_string()
    };
    Ok(ParseNode::TextOrd(SymNode {
        mode: ctx.parser.mode,
        text,
        loc: None,
    }))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "textord",
    names: &["\\@char"],
    num_args: 1,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

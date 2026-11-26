//! Port of KaTeX `src/functions/includegraphics.ts`. With `trust: false` the arguments are
//! parsed and validated exactly as KaTeX does (so its errors match), then the command renders
//! as KaTeX's unsupported-command text.

use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{ParseNode, type_error};
use crate::units::valid_unit;

/// `/^[-+]? *(\d+(\.\d*)?|\.\d+)$/.test(s)`.
fn is_plain_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    if i < b.len() && b[i].is_ascii_digit() {
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == b'.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    } else if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    } else {
        return false;
    }
    i == b.len()
}

/// The unit of `/([-+]?) *(\d+(?:\.\d*)?|\.\d+) *([a-z]{2})/.exec(s)`, if it matches.
fn size_unit(s: &str) -> Option<&str> {
    let b = s.as_bytes();
    for start in 0..=b.len() {
        for with_sign in [true, false] {
            let mut i = start;
            if with_sign {
                if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                    i += 1;
                } else {
                    continue;
                }
            }
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if i < b.len() && b[i].is_ascii_digit() {
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i < b.len() && b[i] == b'.' {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            } else if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                continue;
            }
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if i + 2 <= b.len() && b[i].is_ascii_lowercase() && b[i + 1].is_ascii_lowercase() {
                return Some(&s[i..i + 2]);
            }
        }
    }
    None
}

/// `sizeData(str)` (only its errors matter: the node is never built).
fn size_data(s: &str) -> Result<()> {
    if is_plain_number(s) {
        return Ok(());
    }
    let Some(unit) = size_unit(s) else {
        return perr(format!("Invalid size: '{}' in \\includegraphics", s), None);
    };
    if valid_unit(unit).is_none() {
        return perr(
            format!("Invalid unit: '{}' in \\includegraphics.", unit),
            None,
        );
    }
    Ok(())
}

/// JavaScript `String.prototype.trim` (whitespace and line terminators, incl. U+FEFF).
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    opt_args: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    if let Some(Some(o)) = opt_args.first() {
        let ParseNode::Raw {
            string: attribute_str,
            ..
        } = o
        else {
            return type_error("raw", o);
        };
        for attr in attribute_str.split(',') {
            let key_val: Vec<&str> = attr.split('=').collect();
            if key_val.len() == 2 {
                let s = js_trim(key_val[1]);
                match js_trim(key_val[0]) {
                    "alt" => {}
                    "width" | "height" | "totalheight" => size_data(s)?,
                    _ => {
                        return perr(
                            format!("Invalid key: '{}' in \\includegraphics.", key_val[0]),
                            None,
                        );
                    }
                }
            }
        }
    }
    let a0 = &args[0];
    let ParseNode::Url { .. } = a0 else {
        return type_error("url", a0);
    };
    // isTrusted({command: "\\includegraphics", url: src}) is false.
    Ok(ctx.parser.format_unsupported_cmd("\\includegraphics"))
}

/// Unreachable with `trust: false` (KaTeX builds an `<img>` here).
pub fn html_builder(group: &ParseNode, _options: &Options) -> Result<HtmlNode> {
    ierr(format!(
        "decibell-tex: '{}' nodes need trust, which is never granted",
        group.type_name()
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "includegraphics",
    names: &["\\includegraphics"],
    num_args: 1,
    num_optional_args: 1,
    arg_types: &[ArgType::Raw, ArgType::Url],
    allowed_in_text: false,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

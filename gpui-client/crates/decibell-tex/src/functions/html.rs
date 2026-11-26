//! Port of KaTeX `src/functions/html.ts` (`\htmlClass`, `\htmlId`, `\htmlStyle`, `\htmlData`).
//!
//! Decibell renders with `trust: false`, so after parsing its arguments (and validating
//! `\htmlData`'s key/value list, which can fail first) every one of these renders as KaTeX's
//! unsupported-command text. An `html` node can therefore never reach the builder.

use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{ParseNode, type_error};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a0 = &args[0];
    let ParseNode::Raw { string: value, .. } = a0 else {
        return type_error("raw", a0);
    };
    match ctx.func_name {
        "\\htmlClass" | "\\htmlId" | "\\htmlStyle" => {}
        "\\htmlData" => {
            let mut data: Vec<String> = Vec::new();
            let mut current = String::new();
            let mut rest = value.as_str();
            while let Some(c) = rest.chars().next() {
                if rest.starts_with("{,}") {
                    current.push(',');
                    rest = &rest[3..];
                    continue;
                } else if c == ',' {
                    data.push(std::mem::take(&mut current));
                } else {
                    current.push(c);
                }
                rest = &rest[c.len_utf8()..];
            }
            data.push(current);
            for item in &data {
                if !item.contains('=') {
                    return perr(
                        format!("\\htmlData key/value '{}' missing equals sign", item),
                        None,
                    );
                }
            }
        }
        _ => return ierr("Unrecognized html command"),
    }
    // `parser.settings.isTrusted(...)` is false: trust is never granted.
    Ok(ctx.parser.format_unsupported_cmd(ctx.func_name))
}

/// Unreachable with `trust: false` (the handler never returns an `html` node).
pub fn html_builder(group: &ParseNode, _options: &Options) -> Result<HtmlNode> {
    ierr(format!(
        "decibell-tex: '{}' nodes need trust, which is never granted",
        group.type_name()
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "html",
    names: &["\\htmlClass", "\\htmlId", "\\htmlStyle", "\\htmlData"],
    num_args: 2,
    arg_types: &[ArgType::Raw, ArgType::Original],
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

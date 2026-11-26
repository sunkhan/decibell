//! Port of KaTeX `src/functions/math.ts` (`\(`…`\)` and `$`…`$` inside text).

use crate::define_function::{FunctionContext, FunctionSpec};
use crate::parse_error::{Result, perr};
use crate::parse_node::{Mode, ParseNode, StyleStr};

fn open_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let outer = ctx.parser.mode;
    ctx.parser.switch_mode(Mode::Math);
    let close = if ctx.func_name == "\\(" { "\\)" } else { "$" };
    let body = ctx.parser.parse_expression(false, Some(close))?;
    ctx.parser.expect(close, true)?;
    ctx.parser.switch_mode(outer);
    Ok(ParseNode::Styling {
        mode: ctx.parser.mode,
        style: StyleStr::Text,
        reset_font: true,
        body,
    })
}

fn close_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    perr(format!("Mismatched {}", ctx.func_name), None)
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "styling",
        names: &["\\(", "$"],
        num_args: 0,
        allowed_in_text: true,
        allowed_in_math: false,
        handler: Some(open_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "text",
        names: &["\\)", "\\]"],
        num_args: 0,
        allowed_in_text: true,
        allowed_in_math: false,
        handler: Some(close_handler),
        ..FunctionSpec::DEFAULT
    },
];

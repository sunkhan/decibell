//! Port of KaTeX `src/functions/styling.ts`.

use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::functions::sizing::sizing_group;
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::{ParseNode, StyleStr};
use crate::style::Style;

fn handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = ctx.parser.parse_expression(true, ctx.break_on_token_text)?;
    let style = match ctx.func_name {
        "\\displaystyle" => StyleStr::Display,
        "\\textstyle" => StyleStr::Text,
        "\\scriptstyle" => StyleStr::Script,
        "\\scriptscriptstyle" => StyleStr::ScriptScript,
        other => return ierr(format!("Unknown style: {}", other)),
    };
    Ok(ParseNode::Styling {
        mode: ctx.parser.mode,
        style,
        reset_font: false,
        body,
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Styling {
        style,
        reset_font,
        body,
        ..
    } = group
    else {
        unreachable!()
    };
    let mut new_options = options.having_style(Style::from_style_str(*style));
    if *reset_font {
        new_options = new_options.with_font("");
    }
    sizing_group(body, &new_options, options)
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "styling",
    names: &[
        "\\displaystyle",
        "\\textstyle",
        "\\scriptstyle",
        "\\scriptscriptstyle",
    ],
    num_args: 0,
    allowed_in_text: true,
    primitive: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

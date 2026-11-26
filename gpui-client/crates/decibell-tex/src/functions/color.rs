//! Port of KaTeX `src/functions/color.ts`.

use crate::build_common::make_fragment;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::macros::MacroDef;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, ordargument, type_error};
use crate::token::Src;

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Color { color, body, .. } = group else {
        unreachable!()
    };
    let elements = build_expression(
        body,
        &options.with_color(color),
        RealGroup::No,
        (None, None),
    )?;
    Ok(make_fragment(elements))
}

fn textcolor_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let c = it.next().unwrap();
    let ParseNode::ColorToken { color, .. } = c else {
        return type_error("color-token", &c);
    };
    let body = it.next().unwrap();
    Ok(ParseNode::Color {
        mode: ctx.parser.mode,
        color,
        body: ordargument(body),
    })
}

fn color_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let c = args.into_iter().next().unwrap();
    let ParseNode::ColorToken { color, .. } = c else {
        return type_error("color-token", &c);
    };
    ctx.parser.gullet.macros.set(
        "\\current@color",
        Some(MacroDef::Str(Src::Shared(color.as_str().into()))),
        false,
    );
    let body = ctx.parser.parse_expression(true, ctx.break_on_token_text)?;
    Ok(ParseNode::Color {
        mode: ctx.parser.mode,
        color,
        body,
    })
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "color",
        names: &["\\textcolor"],
        num_args: 2,
        allowed_in_text: true,
        arg_types: &[ArgType::Color, ArgType::Original],
        handler: Some(textcolor_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "color",
        names: &["\\color"],
        num_args: 1,
        allowed_in_text: true,
        arg_types: &[ArgType::Color],
        handler: Some(color_handler),
        ..FunctionSpec::DEFAULT
    },
];

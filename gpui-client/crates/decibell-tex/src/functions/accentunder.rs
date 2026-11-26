//! Port of KaTeX `src/functions/accentunder.ts`.

use crate::build_common::{VListChild, VListElem, VListParam, make_span, make_vlist};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{AccentNode, ParseNode};
use crate::stretchy::stretchy_svg;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let base = args.into_iter().next().unwrap();
    Ok(ParseNode::AccentUnder(Box::new(AccentNode {
        mode: ctx.parser.mode,
        label: ctx.func_name.to_string(),
        is_stretchy: false,
        is_shifty: false,
        base: Box::new(base),
    })))
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::AccentUnder(g) = group else {
        unreachable!()
    };
    let inner_group = build_group(Some(&g.base), options, None)?;
    let accent_body = stretchy_svg(group, options)?;
    let kern = if g.label == "\\utilde" { 0.12 } else { 0.0 };
    let top = inner_group.height();
    let mut e = VListElem::new(HtmlNode::span(accent_body));
    e.wrapper_classes = vec!["svg-align"];
    let vlist = make_vlist(VListParam::Top(
        top,
        vec![
            VListChild::Elem(e),
            VListChild::Kern(kern),
            VListChild::elem(inner_group),
        ],
    ))?;
    Ok(make_span(
        vec!["mord", "accentunder"],
        vec![HtmlNode::span(vlist)],
        Some(options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "accentUnder",
    names: &[
        "\\underleftarrow",
        "\\underrightarrow",
        "\\underleftrightarrow",
        "\\undergroup",
        "\\underlinesegment",
        "\\utilde",
    ],
    num_args: 1,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

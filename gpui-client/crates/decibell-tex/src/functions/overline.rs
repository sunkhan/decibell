//! Port of KaTeX `src/functions/overline.ts`.

use crate::build_common::{VListChild, VListParam, make_line_span, make_span, make_vlist};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(ParseNode::Overline {
        mode: ctx.parser.mode,
        body: Box::new(args.into_iter().next().unwrap()),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Overline { body, .. } = group else {
        unreachable!()
    };
    let inner_group = build_group(Some(body), &options.having_cramped_style(), None)?;
    let line = make_line_span("overline-line", options, None);
    let t = options.font_metrics().default_rule_thickness;
    let vlist = make_vlist(VListParam::FirstBaseline(vec![
        VListChild::elem(inner_group),
        VListChild::Kern(3.0 * t),
        VListChild::elem(HtmlNode::span(line)),
        VListChild::Kern(t),
    ]))?;
    Ok(make_span(
        vec!["mord", "katex-overline"],
        vec![HtmlNode::span(vlist)],
        Some(options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "overline",
    names: &["\\overline"],
    num_args: 1,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

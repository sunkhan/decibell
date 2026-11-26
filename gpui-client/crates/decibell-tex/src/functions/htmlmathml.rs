//! Port of KaTeX `src/functions/htmlmathml.ts` (`\html@mathml`; only the HTML half is built).

use crate::build_common::make_fragment;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, ordargument};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let html = ordargument(it.next().unwrap());
    let mathml = ordargument(it.next().unwrap());
    Ok(ParseNode::HtmlMathml {
        mode: ctx.parser.mode,
        html,
        mathml,
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::HtmlMathml { html, .. } = group else {
        unreachable!()
    };
    Ok(make_fragment(build_expression(
        html,
        options,
        RealGroup::No,
        (None, None),
    )?))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "htmlmathml",
    names: &["\\html@mathml"],
    num_args: 2,
    allowed_in_argument: true,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

//! Port of KaTeX `src/functions/vcenter.ts`.

use crate::build_common::{VListChild, VListParam, make_vlist};
use crate::build_html::build_group;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::VCenter {
        mode: ctx.parser.mode,
        body: Box::new(body),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::VCenter { body, .. } = group else {
        unreachable!()
    };
    let body = build_group(Some(body), options, None)?;
    let axis = options.font_metrics().axis_height;
    let dy = 0.5 * ((body.height() - axis) - (body.depth() + axis));
    Ok(HtmlNode::span(make_vlist(VListParam::Shift(
        dy,
        vec![VListChild::elem(body)],
    ))?))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "vcenter",
    names: &["\\vcenter"],
    num_args: 1,
    arg_types: &[ArgType::Original],
    allowed_in_text: false,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

//! Port of KaTeX `src/functions/raisebox.ts`.

use crate::build_common::{VListChild, VListParam, make_vlist};
use crate::build_html::build_group;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, type_error};
use crate::units::calculate_size;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let a0 = it.next().unwrap();
    let ParseNode::Size { value, .. } = a0 else {
        return type_error("size", &a0);
    };
    let body = it.next().unwrap();
    Ok(ParseNode::RaiseBox {
        mode: ctx.parser.mode,
        dy: value,
        body: Box::new(body),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::RaiseBox { dy, body, .. } = group else {
        unreachable!()
    };
    let body = build_group(Some(body), options, None)?;
    let dy = calculate_size(dy, options)?;
    Ok(HtmlNode::span(make_vlist(VListParam::Shift(
        -dy,
        vec![VListChild::elem(body)],
    ))?))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "raisebox",
    names: &["\\raisebox"],
    num_args: 2,
    arg_types: &[ArgType::Size, ArgType::HBox],
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

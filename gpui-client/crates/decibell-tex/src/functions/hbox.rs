//! Port of KaTeX `src/functions/hbox.ts`.

use crate::build_common::make_fragment;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, ordargument};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a = args.into_iter().next().unwrap();
    Ok(ParseNode::HBox {
        mode: ctx.parser.mode,
        body: ordargument(a),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::HBox { body, .. } = group else {
        unreachable!()
    };
    Ok(make_fragment(build_expression(
        body,
        &options.with_font(""),
        RealGroup::No,
        (None, None),
    )?))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "hbox",
    names: &["\\hbox"],
    num_args: 1,
    arg_types: &[ArgType::Text],
    allowed_in_text: true,
    primitive: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

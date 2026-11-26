//! Port of KaTeX `src/functions/kern.ts` (strict-mode warnings are console-only in KaTeX).

use crate::build_common::make_glue;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, type_error};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a = args.into_iter().next().unwrap();
    let ParseNode::Size { value, .. } = a else {
        return type_error("size", &a);
    };
    Ok(ParseNode::Kern {
        mode: ctx.parser.mode,
        dimension: value,
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Kern { dimension, .. } = group else {
        unreachable!()
    };
    make_glue(dimension, options)
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "kern",
    names: &["\\kern", "\\mkern", "\\hskip", "\\mskip"],
    num_args: 1,
    arg_types: &[ArgType::Size],
    primitive: true,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

//! Port of KaTeX `src/functions/pmb.ts` (poor man's bold: a text-shadow).

use crate::build_common::make_span_s;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode};
use crate::functions::mclass::binrel_class;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, ordargument};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a = args.into_iter().next().unwrap();
    let mclass = binrel_class(&a);
    Ok(ParseNode::Pmb {
        mode: ctx.parser.mode,
        mclass,
        body: ordargument(a),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Pmb { mclass, body, .. } = group else {
        unreachable!()
    };
    let elements = build_expression(body, options, RealGroup::Yes, (None, None))?;
    let mut node = make_span_s(vec![*mclass], elements, Some(options), CssStyle::new());
    node.style
        .set(CssProp::TextShadow, CssVal::Static("0.02em 0.01em 0.04px"));
    Ok(HtmlNode::span(node))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "pmb",
    names: &["\\pmb"],
    num_args: 1,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

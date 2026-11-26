//! Port of KaTeX `src/functions/mathchoice.ts`.

use crate::build_common::make_fragment;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{MathChoiceNode, ParseNode, ordargument};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let display = ordargument(it.next().unwrap());
    let text = ordargument(it.next().unwrap());
    let script = ordargument(it.next().unwrap());
    let scriptscript = ordargument(it.next().unwrap());
    Ok(ParseNode::MathChoice(Box::new(MathChoiceNode {
        mode: ctx.parser.mode,
        display,
        text,
        script,
        scriptscript,
    })))
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::MathChoice(g) = group else {
        unreachable!()
    };
    // chooseMathStyle
    let body = match options.style.size {
        0 => &g.display,
        1 => &g.text,
        2 => &g.script,
        3 => &g.scriptscript,
        _ => &g.text,
    };
    Ok(make_fragment(build_expression(
        body,
        options,
        RealGroup::No,
        (None, None),
    )?))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "mathchoice",
    names: &["\\mathchoice"],
    num_args: 4,
    primitive: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

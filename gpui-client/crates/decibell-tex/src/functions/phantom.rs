//! Port of KaTeX `src/functions/phantom.ts` (`\phantom`, `\vphantom`; `\hphantom` is a macro).

use crate::build_common::{make_fragment, make_span};
use crate::build_html::{RealGroup, build_expression, build_group};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, ordargument};

fn phantom_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::Phantom {
        mode: ctx.parser.mode,
        body: ordargument(body),
    })
}

pub fn html_builder_phantom(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Phantom { body, .. } = group else {
        unreachable!()
    };
    let elements = build_expression(body, &options.with_phantom(), RealGroup::No, (None, None))?;
    Ok(make_fragment(elements))
}

fn vphantom_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::VPhantom {
        mode: ctx.parser.mode,
        body: Box::new(body),
    })
}

pub fn html_builder_vphantom(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::VPhantom { body, .. } = group else {
        unreachable!()
    };
    let inner = make_span(
        vec!["katex-inner"],
        vec![build_group(Some(body), &options.with_phantom(), None)?],
        None,
        CssStyle::new(),
    );
    let fix = make_span(vec!["katex-fix"], vec![], None, CssStyle::new());
    Ok(make_span(
        vec!["mord", "rlap"],
        vec![inner, fix],
        Some(options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "phantom",
        names: &["\\phantom"],
        num_args: 1,
        allowed_in_text: true,
        handler: Some(phantom_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "vphantom",
        names: &["\\vphantom"],
        num_args: 1,
        allowed_in_text: true,
        handler: Some(vphantom_handler),
        ..FunctionSpec::DEFAULT
    },
];

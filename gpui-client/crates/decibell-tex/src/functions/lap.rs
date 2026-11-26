//! Port of KaTeX `src/functions/lap.ts` (`\mathllap`, `\mathrlap`, `\mathclap`).

use crate::build_common::{make_span, make_span_s};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;
use crate::units::make_em;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::Lap {
        mode: ctx.parser.mode,
        alignment: ctx.func_name[5..].to_string(),
        body: Box::new(body),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Lap {
        alignment, body, ..
    } = group
    else {
        unreachable!()
    };
    let align: &'static str = match alignment.as_str() {
        "llap" => "llap",
        "rlap" => "rlap",
        _ => "clap",
    };
    let inner = if align == "clap" {
        let i = make_span(
            vec![],
            vec![build_group(Some(body), options, None)?],
            None,
            CssStyle::new(),
        );
        make_span(vec!["katex-inner"], vec![i], Some(options), CssStyle::new())
    } else {
        make_span(
            vec!["katex-inner"],
            vec![build_group(Some(body), options, None)?],
            None,
            CssStyle::new(),
        )
    };
    let fix = make_span(vec!["katex-fix"], vec![], None, CssStyle::new());
    let mut node = make_span_s(
        vec![align],
        vec![inner, fix],
        Some(options),
        CssStyle::new(),
    );
    let mut strut = make_span_s(vec!["katex-strut"], vec![], None, CssStyle::new());
    strut
        .style
        .set(CssProp::Height, make_em(node.height + node.depth));
    if node.depth != 0.0 {
        strut
            .style
            .set(CssProp::VerticalAlign, make_em(-node.depth));
    }
    node.children.insert(0, HtmlNode::span(strut));
    let node = make_span(
        vec!["katex-thinbox"],
        vec![HtmlNode::span(node)],
        Some(options),
        CssStyle::new(),
    );
    Ok(make_span(
        vec!["mord", "katex-vbox"],
        vec![node],
        Some(options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "lap",
    names: &["\\mathllap", "\\mathrlap", "\\mathclap"],
    num_args: 1,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

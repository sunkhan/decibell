//! Port of KaTeX `src/functions/href.ts` (`\href`, `\url`). With `trust: false` both parse
//! their arguments and then render as KaTeX's unsupported-command text.

use crate::build_common::make_anchor;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, type_error};

fn href_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a0 = &args[0];
    let ParseNode::Url { .. } = a0 else {
        return type_error("url", a0);
    };
    // isTrusted({command: "\\href", url}) is false.
    Ok(ctx.parser.format_unsupported_cmd("\\href"))
}

fn url_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a0 = &args[0];
    let ParseNode::Url { .. } = a0 else {
        return type_error("url", a0);
    };
    // isTrusted({command: "\\url", url}) is false.
    Ok(ctx.parser.format_unsupported_cmd("\\url"))
}

/// KaTeX's builder (an anchor); unreachable with `trust: false`.
pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Href { href, body, .. } = group else {
        unreachable!()
    };
    let elements = build_expression(body, options, RealGroup::No, (None, None))?;
    Ok(make_anchor(href, vec![], elements, options))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "href",
        names: &["\\href"],
        num_args: 2,
        arg_types: &[ArgType::Url, ArgType::Original],
        allowed_in_text: true,
        handler: Some(href_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "href",
        names: &["\\url"],
        num_args: 1,
        arg_types: &[ArgType::Url],
        allowed_in_text: true,
        handler: Some(url_handler),
        ..FunctionSpec::DEFAULT
    },
];

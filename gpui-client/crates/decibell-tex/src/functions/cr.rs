//! Port of KaTeX `src/functions/cr.ts`: `\\` (row breaks inside arrays are handled by
//! `parseArray`; this is the top-level line break).

use crate::build_common::make_span_s;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, type_error};
use crate::parser::ArgSpec;
use crate::units::{calculate_size, make_em};

fn handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let parser = &mut *ctx.parser;
    let size = if parser.gullet.future()?.text == "[" {
        // `parser.parseSizeGroup(true)`, through the public argument parser.
        let spec = ArgSpec {
            num_args: 0,
            num_optional_args: 1,
            arg_types: &[ArgType::Size],
            primitive: false,
            is_sqrt: false,
        };
        let (_, mut opt) = parser.parse_arguments("\\\\", &spec)?;
        match opt.pop().flatten() {
            Some(ParseNode::Size { value, .. }) => Some(value),
            Some(other) => return type_error("size", &other),
            None => None,
        }
    } else {
        None
    };
    // strict: "warn" never applies strict behaviour, so \\ always breaks the line.
    Ok(ParseNode::Cr {
        mode: parser.mode,
        new_line: true,
        size,
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Cr { new_line, size, .. } = group else {
        unreachable!()
    };
    let mut span = make_span_s(vec!["mspace"], vec![], Some(options), CssStyle::new());
    if *new_line {
        span.classes.push("katex-newline");
        if let Some(size) = size {
            span.style
                .set(CssProp::MarginTop, make_em(calculate_size(size, options)?));
        }
    }
    Ok(HtmlNode::span(span))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "cr",
    names: &["\\\\"],
    num_args: 0,
    num_optional_args: 0,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

//! Port of KaTeX `src/functions/sizing.ts`.

use crate::build_common::make_fragment;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::options::{Options, reset_size_class};
use crate::parse_error::Result;
use crate::parse_node::ParseNode;

/// `sizingGroup(value, options, baseOptions)`.
pub fn sizing_group(
    value: &[ParseNode],
    options: &Options,
    base_options: &Options,
) -> Result<HtmlNode> {
    let mut inner = build_expression(value, options, RealGroup::No, (None, None))?;
    let multiplier = options.size_multiplier / base_options.size_multiplier;
    for node in inner.iter_mut() {
        let classes = node.classes().to_vec();
        match classes.iter().position(|c| *c == "katex-sizing") {
            None => {
                let extra = options.sizing_classes(base_options);
                if let Some(c) = node.classes_mut() {
                    c.extend(extra);
                }
            }
            Some(pos) => {
                if classes.get(pos + 1).copied() == Some(reset_size_class(options.size))
                    && let Some(c) = node.classes_mut()
                {
                    c[pos + 1] = reset_size_class(base_options.size);
                }
            }
        }
        let (h, d) = (node.height(), node.depth());
        node.set_height(h * multiplier);
        node.set_depth(d * multiplier);
    }
    Ok(make_fragment(inner))
}

const SIZE_FUNCS: [&str; 11] = [
    "\\tiny",
    "\\sixptsize",
    "\\scriptsize",
    "\\footnotesize",
    "\\small",
    "\\normalsize",
    "\\large",
    "\\Large",
    "\\LARGE",
    "\\huge",
    "\\Huge",
];

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Sizing { size, body, .. } = group else {
        unreachable!()
    };
    let new_options = options.having_size(*size);
    sizing_group(body, &new_options, options)
}

fn handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = ctx
        .parser
        .parse_expression(false, ctx.break_on_token_text)?;
    let size = SIZE_FUNCS
        .iter()
        .position(|f| *f == ctx.func_name)
        .map(|i| i as u8 + 1)
        .unwrap_or(0);
    Ok(ParseNode::Sizing {
        mode: ctx.parser.mode,
        size,
        body,
    })
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "sizing",
    names: &SIZE_FUNCS,
    num_args: 0,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

//! Port of KaTeX `src/functions/operatorname.ts` (HTML side). `\operatorname` itself is a
//! macro (in `data/macros_data.rs`).

use crate::build_common::make_span;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::functions::assemble_sup_sub::assemble_sup_sub;
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::{ParseNode, SymNode, ordargument, type_error};

/// `htmlBuilder` (HtmlBuilderSupSub): an `operatorname` node, or a `supsub` whose base is one.
pub fn html_builder(grp: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let (group_body, sup_group, sub_group, has_limits) = match grp {
        ParseNode::SupSub(s) => match s.base.as_deref() {
            Some(ParseNode::OperatorName { body, .. }) => {
                (body, s.sup.as_deref(), s.sub.as_deref(), true)
            }
            Some(other) => return type_error("operatorname", other),
            None => return ierr("Expected node of type operatorname, but got undefined"),
        },
        ParseNode::OperatorName { body, .. } => (body, None, None, false),
        other => return type_error("operatorname", other),
    };
    let base = if !group_body.is_empty() {
        // Symbol children become textords (`"text" in child`).
        let body: Vec<ParseNode> = group_body
            .iter()
            .map(|child| match child.symbol_text() {
                Some(t) => ParseNode::TextOrd(SymNode {
                    mode: child.mode(),
                    text: t.to_string(),
                    loc: None,
                }),
                None => child.clone(),
            })
            .collect();
        let mut expression = build_expression(
            &body,
            &options.with_font("mathrm"),
            RealGroup::Yes,
            (None, None),
        )?;
        for child in expression.iter_mut() {
            if let HtmlNode::Symbol(s) = child {
                s.text = s
                    .text
                    .replacen('\u{2212}', "-", 1)
                    .replacen('\u{2217}', "*", 1);
            }
        }
        make_span(vec!["mop"], expression, Some(options), CssStyle::new())
    } else {
        make_span(vec!["mop"], vec![], Some(options), CssStyle::new())
    };
    if has_limits {
        Ok(HtmlNode::span(assemble_sup_sub(
            base,
            sup_group,
            sub_group,
            options,
            options.style,
            0.0,
            0.0,
        )?))
    } else {
        Ok(base)
    }
}

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::OperatorName {
        mode: ctx.parser.mode,
        body: ordargument(body),
        always_handle_sup_sub: ctx.func_name == "\\operatornamewithlimits",
        limits: false,
        parent_is_sup_sub: false,
    })
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "operatorname",
    names: &["\\operatorname@", "\\operatornamewithlimits"],
    num_args: 1,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

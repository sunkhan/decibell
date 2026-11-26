//! Port of KaTeX `src/functions/horizBrace.ts` (`\overbrace`, `\underbrace`, brackets).

use crate::build_common::{VListChild, VListElem, VListParam, make_span, make_vlist};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::{ParseNode, type_error};
use crate::stretchy::stretchy_svg;
use crate::style::Style;

fn svg_align(elem: HtmlNode) -> VListChild {
    let mut e = VListElem::new(elem);
    e.wrapper_classes = vec!["svg-align"];
    VListChild::Elem(e)
}

/// `htmlBuilder` (an `HtmlBuilderSupSub`: also called by supsub for a brace with a script).
pub fn html_builder(grp: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let style = options.style;
    let mut sup_sub_group = None;
    let group: &ParseNode = match grp {
        ParseNode::SupSub(ss) => {
            sup_sub_group = Some(match (&ss.sup, &ss.sub) {
                (Some(sup), _) => {
                    build_group(Some(sup), &options.having_style(style.sup()), Some(options))?
                }
                (None, sub) => build_group(
                    sub.as_deref(),
                    &options.having_style(style.sub()),
                    Some(options),
                )?,
            });
            match ss.base.as_deref() {
                Some(b @ ParseNode::HorizBrace { .. }) => b,
                Some(other) => return type_error("horizBrace", other),
                None => return ierr("Expected node of type horizBrace, but got undefined"),
            }
        }
        ParseNode::HorizBrace { .. } => grp,
        other => return type_error("horizBrace", other),
    };
    let ParseNode::HorizBrace { is_over, base, .. } = group else {
        unreachable!()
    };
    let body = build_group(
        Some(base),
        &options.having_base_style(Some(Style::DISPLAY)),
        None,
    )?;
    let brace_body = stretchy_svg(group, options)?;
    let mut vlist = if *is_over {
        make_vlist(VListParam::FirstBaseline(vec![
            VListChild::elem(body),
            VListChild::Kern(0.1),
            svg_align(HtmlNode::span(brace_body)),
        ]))?
    } else {
        let bottom = body.depth() + 0.1 + brace_body.height;
        make_vlist(VListParam::Bottom(
            bottom,
            vec![
                svg_align(HtmlNode::span(brace_body)),
                VListChild::Kern(0.1),
                VListChild::elem(body),
            ],
        ))?
    };
    let class = if *is_over { "mover" } else { "munder" };
    if let Some(ssg) = sup_sub_group {
        let v_span = make_span(
            vec!["minner", class],
            vec![HtmlNode::span(vlist)],
            Some(options),
            CssStyle::new(),
        );
        vlist = if *is_over {
            make_vlist(VListParam::FirstBaseline(vec![
                VListChild::elem(v_span),
                VListChild::Kern(0.2),
                VListChild::elem(ssg),
            ]))?
        } else {
            let bottom = v_span.depth() + 0.2 + ssg.height() + ssg.depth();
            make_vlist(VListParam::Bottom(
                bottom,
                vec![
                    VListChild::elem(ssg),
                    VListChild::Kern(0.2),
                    VListChild::elem(v_span),
                ],
            ))?
        };
    }
    Ok(make_span(
        vec!["minner", class],
        vec![HtmlNode::span(vlist)],
        Some(options),
        CssStyle::new(),
    ))
}

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(ParseNode::HorizBrace {
        mode: ctx.parser.mode,
        label: ctx.func_name.to_string(),
        is_over: ctx.func_name.contains("\\over"),
        base: Box::new(args.into_iter().next().unwrap()),
    })
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "horizBrace",
    names: &[
        "\\overbrace",
        "\\underbrace",
        "\\overbracket",
        "\\underbracket",
    ],
    num_args: 1,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

//! Port of KaTeX `src/functions/arrow.ts` (extensible arrows `\xrightarrow` & co.).

use crate::build_common::{VListElem, VListParam, make_span, make_vlist, wrap_fragment};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;
use crate::stretchy::stretchy_svg;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    opt_args: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next();
    let below = opt_args.into_iter().next().flatten();
    Ok(ParseNode::XArrow {
        mode: ctx.parser.mode,
        label: ctx.func_name.to_string(),
        body: body.map(Box::new),
        below: below.map(Box::new),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::XArrow {
        label, body, below, ..
    } = group
    else {
        unreachable!()
    };
    let style = options.style;
    let new_options = options.having_style(style.sup());
    let mut upper_group = wrap_fragment(
        build_group(body.as_deref(), &new_options, Some(options))?,
        options,
    );
    let pad = if label.starts_with("\\x") {
        "x-arrow-pad"
    } else {
        "cd-arrow-pad"
    };
    if let Some(c) = upper_group.classes_mut() {
        c.push(pad);
    }
    let mut lower_group = None;
    if let Some(below) = below {
        let new_options = options.having_style(style.sub());
        let mut lg = wrap_fragment(
            build_group(Some(below), &new_options, Some(options))?,
            options,
        );
        if let Some(c) = lg.classes_mut() {
            c.push(pad);
        }
        lower_group = Some(lg);
    }
    let arrow_body = stretchy_svg(group, options)?;
    let axis = options.font_metrics().axis_height;
    let arrow_shift = -axis + 0.5 * arrow_body.height;
    let mut upper_shift = -axis - 0.5 * arrow_body.height - 0.111;
    if upper_group.depth() > 0.25 || label == "\\xleftequilibrium" {
        upper_shift -= upper_group.depth();
    }
    let arrow_height = arrow_body.height;
    let mut arrow = VListElem::shifted(HtmlNode::span(arrow_body), arrow_shift);
    arrow.wrapper_classes = vec!["svg-align"];
    let vlist = match lower_group {
        Some(lg) => {
            let lower_shift = -axis + lg.height() + 0.5 * arrow_height + 0.111;
            make_vlist(VListParam::IndividualShift(vec![
                VListElem::shifted(upper_group, upper_shift),
                arrow,
                VListElem::shifted(lg, lower_shift),
            ]))?
        }
        None => make_vlist(VListParam::IndividualShift(vec![
            VListElem::shifted(upper_group, upper_shift),
            arrow,
        ]))?,
    };
    Ok(make_span(
        vec!["mrel", "x-arrow"],
        vec![HtmlNode::span(vlist)],
        Some(options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "xArrow",
    names: &[
        "\\xleftarrow",
        "\\xrightarrow",
        "\\xLeftarrow",
        "\\xRightarrow",
        "\\xleftrightarrow",
        "\\xLeftrightarrow",
        "\\xhookleftarrow",
        "\\xhookrightarrow",
        "\\xmapsto",
        "\\xrightharpoondown",
        "\\xrightharpoonup",
        "\\xleftharpoondown",
        "\\xleftharpoonup",
        "\\xrightleftharpoons",
        "\\xleftrightharpoons",
        "\\xlongequal",
        "\\xtwoheadrightarrow",
        "\\xtwoheadleftarrow",
        "\\xtofrom",
        "\\xrightleftarrows",
        "\\xrightequilibrium",
        "\\xleftequilibrium",
        "\\\\cdrightarrow",
        "\\\\cdleftarrow",
        "\\\\cdlongequal",
    ],
    num_args: 1,
    num_optional_args: 1,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

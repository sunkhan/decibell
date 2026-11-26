//! Port of KaTeX `src/functions/sqrt.ts`.

use crate::build_common::{
    VListChild, VListElem, VListParam, make_span, make_vlist, wrap_fragment,
};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::delimiter::make_sqrt_image;
use crate::dom_tree::{CssProp, CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;
use crate::style::Style;
use crate::units::make_em;

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    opt_args: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let index = opt_args.into_iter().next().flatten();
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::Sqrt {
        mode: ctx.parser.mode,
        body: Box::new(body),
        index: index.map(Box::new),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Sqrt { body, index, .. } = group else {
        unreachable!()
    };
    let mut inner = build_group(Some(body), &options.having_cramped_style(), None)?;
    if inner.height() == 0.0 {
        inner.set_height(options.font_metrics().x_height);
    }
    let mut inner = wrap_fragment(inner, options);
    let metrics = options.font_metrics();
    let theta = metrics.default_rule_thickness;
    let mut phi = theta;
    if options.style.id < Style::TEXT.id {
        phi = options.font_metrics().x_height;
    }
    let mut line_clearance = theta + phi / 4.0;
    let min_delimiter_height = inner.height() + inner.depth() + line_clearance + theta;
    let img = make_sqrt_image(min_delimiter_height, options)?;
    let rule_width = img.rule_width;
    let advance_width = img.advance_width;
    let img = img.span;
    let delim_depth = img.height - rule_width;
    if delim_depth > inner.height() + inner.depth() + line_clearance {
        line_clearance = (line_clearance + delim_depth - inner.height() - inner.depth()) / 2.0;
    }
    let img_shift = img.height - inner.height() - line_clearance - rule_width;
    inner
        .style_mut()
        .set(CssProp::PaddingLeft, make_em(advance_width));
    let inner_height = inner.height();
    let mut inner_elem = VListElem::new(inner);
    inner_elem.wrapper_classes = vec!["svg-align"];
    let body = make_vlist(VListParam::FirstBaseline(vec![
        VListChild::Elem(inner_elem),
        VListChild::Kern(-(inner_height + img_shift)),
        VListChild::elem(HtmlNode::span(img)),
        VListChild::Kern(rule_width),
    ]))?;
    match index {
        None => Ok(make_span(
            vec!["mord", "sqrt"],
            vec![HtmlNode::span(body)],
            Some(options),
            CssStyle::new(),
        )),
        Some(index) => {
            let new_options = options.having_style(Style::SCRIPTSCRIPT);
            let rootm = build_group(Some(index), &new_options, Some(options))?;
            let to_shift = 0.6 * (body.height - body.depth);
            let root_vlist =
                make_vlist(VListParam::Shift(-to_shift, vec![VListChild::elem(rootm)]))?;
            let root_wrap = make_span(
                vec!["katex-root"],
                vec![HtmlNode::span(root_vlist)],
                None,
                CssStyle::new(),
            );
            Ok(make_span(
                vec!["mord", "sqrt"],
                vec![root_wrap, HtmlNode::span(body)],
                Some(options),
                CssStyle::new(),
            ))
        }
    }
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "sqrt",
    names: &["\\sqrt"],
    num_args: 1,
    num_optional_args: 1,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

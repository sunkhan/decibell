//! Port of KaTeX `src/functions/supsub.ts`.

use crate::build_common::{VListElem, VListParam, make_span, make_vlist};
use crate::build_html::{build_group, get_type_of_dom_tree};
use crate::define_function::FunctionSpec;
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::functions::{accent, horiz_brace, op, operatorname};
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::ParseNode;
use crate::style::Style;
use crate::units::make_em;
use crate::utils::is_character_box;

pub static SPECS: &[FunctionSpec] = &[];

type Builder = fn(&ParseNode, &Options) -> Result<HtmlNode>;

/// `htmlBuilderDelegate(group, options)`.
fn delegate(group: &ParseNode, options: &Options) -> Option<Builder> {
    let ParseNode::SupSub(g) = group else {
        return None;
    };
    let base = g.base.as_deref()?;
    match base {
        ParseNode::Op(o) => {
            let d =
                o.limits && (options.style.size == Style::DISPLAY.size || o.always_handle_sup_sub);
            if d { Some(op::html_builder) } else { None }
        }
        ParseNode::OperatorName {
            always_handle_sup_sub,
            limits,
            ..
        } => {
            let d =
                *always_handle_sup_sub && (options.style.size == Style::DISPLAY.size || *limits);
            if d {
                Some(operatorname::html_builder)
            } else {
                None
            }
        }
        ParseNode::Accent(a) => {
            if is_character_box(&a.base) {
                Some(accent::html_builder)
            } else {
                None
            }
        }
        ParseNode::HorizBrace { is_over, .. } => {
            let is_sup = g.sub.is_none();
            if is_sup == *is_over {
                Some(horiz_brace::html_builder)
            } else {
                None
            }
        }
        _ => None,
    }
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    if let Some(d) = delegate(group, options) {
        return d(group, options);
    }
    let ParseNode::SupSub(g) = group else {
        unreachable!()
    };
    let value_base = g.base.as_deref();
    let base = build_group(value_base, options, None)?;
    let metrics = options.font_metrics();
    let mut sup_shift = 0.0;
    let mut sub_shift = 0.0;
    let is_char_box = value_base.map(is_character_box).unwrap_or(false);
    let mut supm = None;
    let mut subm = None;
    if let Some(s) = g.sup.as_deref() {
        let new_options = options.having_style(options.style.sup());
        let m = build_group(Some(s), &new_options, Some(options))?;
        if !is_char_box {
            sup_shift = base.height()
                - new_options.font_metrics().sup_drop * new_options.size_multiplier
                    / options.size_multiplier;
        }
        supm = Some(m);
    }
    if let Some(s) = g.sub.as_deref() {
        let new_options = options.having_style(options.style.sub());
        let m = build_group(Some(s), &new_options, Some(options))?;
        if !is_char_box {
            sub_shift = base.depth()
                + new_options.font_metrics().sub_drop * new_options.size_multiplier
                    / options.size_multiplier;
        }
        subm = Some(m);
    }
    let min_sup_shift = if options.style == Style::DISPLAY {
        metrics.sup1
    } else if options.style.cramped {
        metrics.sup3
    } else {
        metrics.sup2
    };
    let multiplier = options.size_multiplier;
    let margin_right = make_em((0.5 / metrics.pt_per_em) / multiplier);
    let mut margin_left = None;
    if subm.is_some() {
        let is_oiint = matches!(value_base, Some(ParseNode::Op(o)) if matches!(o.name.as_deref(), Some("\\oiint") | Some("\\oiiint")));
        match &base {
            HtmlNode::Symbol(s) => margin_left = Some(make_em(-s.italic)),
            HtmlNode::Span(s) if is_oiint => margin_left = Some(make_em(-s.italic)),
            _ if is_oiint => margin_left = Some(make_em(-0.0)),
            _ => {}
        }
    }
    let supsub = match (supm, subm) {
        (Some(supm), Some(subm)) => {
            sup_shift = sup_shift
                .max(min_sup_shift)
                .max(supm.depth() + 0.25 * metrics.x_height);
            sub_shift = sub_shift.max(metrics.sub2);
            let rule_width = metrics.default_rule_thickness;
            let max_width = 4.0 * rule_width;
            if (sup_shift - supm.depth()) - (subm.height() - sub_shift) < max_width {
                sub_shift = max_width - (sup_shift - supm.depth()) + subm.height();
                let psi = 0.8 * metrics.x_height - (sup_shift - supm.depth());
                if psi > 0.0 {
                    sup_shift += psi;
                    sub_shift -= psi;
                }
            }
            let mut e1 = VListElem::shifted(subm, sub_shift);
            e1.margin_right = Some(margin_right.clone());
            e1.margin_left = margin_left;
            let mut e2 = VListElem::shifted(supm, -sup_shift);
            e2.margin_right = Some(margin_right);
            make_vlist(VListParam::IndividualShift(vec![e1, e2]))?
        }
        (None, Some(subm)) => {
            sub_shift = sub_shift
                .max(metrics.sub1)
                .max(subm.height() - 0.8 * metrics.x_height);
            let mut e = VListElem::new(subm);
            e.margin_left = margin_left;
            e.margin_right = Some(margin_right);
            make_vlist(VListParam::Shift(
                sub_shift,
                vec![crate::build_common::VListChild::Elem(e)],
            ))?
        }
        (Some(supm), None) => {
            sup_shift = sup_shift
                .max(min_sup_shift)
                .max(supm.depth() + 0.25 * metrics.x_height);
            let mut e = VListElem::new(supm);
            e.margin_right = Some(margin_right);
            make_vlist(VListParam::Shift(
                -sup_shift,
                vec![crate::build_common::VListChild::Elem(e)],
            ))?
        }
        (None, None) => return ierr("supsub must have either sup or sub."),
    };
    let mclass = get_type_of_dom_tree(Some(&base), Some(true)).unwrap_or("mord");
    let msupsub = make_span(
        vec!["msupsub"],
        vec![HtmlNode::span(supsub)],
        None,
        CssStyle::new(),
    );
    Ok(make_span(
        vec![mclass],
        vec![base, msupsub],
        Some(options),
        CssStyle::new(),
    ))
}

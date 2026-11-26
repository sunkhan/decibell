//! Port of KaTeX `src/functions/utils/assembleSupSub.ts` (limits above/below operators).

use crate::build_common::{VListChild, VListElem, VListParam, make_span, make_span_s, make_vlist};
use crate::build_html::build_group;
use crate::dom_tree::{CssProp, CssStyle, HtmlNode, Span};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;
use crate::style::Style;
use crate::units::make_em;
use crate::utils::is_character_box;

fn elem_ml(elem: HtmlNode, ml: f64) -> VListChild {
    let mut e = VListElem::new(elem);
    e.margin_left = Some(make_em(ml));
    VListChild::Elem(e)
}

/// `assembleSupSub(base, supGroup, subGroup, options, style, slant, baseShift)`.
pub fn assemble_sup_sub(
    base: HtmlNode,
    sup_group: Option<&ParseNode>,
    sub_group: Option<&ParseNode>,
    options: &Options,
    style: Style,
    slant: f64,
    base_shift: f64,
) -> Result<Span> {
    let base = make_span_s(vec![], vec![base], None, CssStyle::new());
    let sub_is_single_character = sub_group.map(is_character_box).unwrap_or(false);
    let fm = options.font_metrics();
    let mut sup = None;
    if let Some(sg) = sup_group {
        let elem = build_group(Some(sg), &options.having_style(style.sup()), Some(options))?;
        let kern = fm.big_op_spacing1.max(fm.big_op_spacing3 - elem.depth());
        sup = Some((elem, kern));
    }
    let mut sub = None;
    if let Some(sg) = sub_group {
        let elem = build_group(Some(sg), &options.having_style(style.sub()), Some(options))?;
        let kern = fm.big_op_spacing2.max(fm.big_op_spacing4 - elem.height());
        sub = Some((elem, kern));
    }
    let has_sub = sub.is_some();
    let (bh, bd) = (base.height, base.depth);
    let base = HtmlNode::span(base);
    let final_group = match (sup, sub) {
        (Some((sup_e, sup_k)), Some((sub_e, sub_k))) => {
            let bottom =
                fm.big_op_spacing5 + sub_e.height() + sub_e.depth() + sub_k + bd + base_shift;
            make_vlist(VListParam::Bottom(
                bottom,
                vec![
                    VListChild::Kern(fm.big_op_spacing5),
                    elem_ml(sub_e, -slant),
                    VListChild::Kern(sub_k),
                    VListChild::elem(base),
                    VListChild::Kern(sup_k),
                    elem_ml(sup_e, slant),
                    VListChild::Kern(fm.big_op_spacing5),
                ],
            ))?
        }
        (None, Some((sub_e, sub_k))) => {
            let top = bh - base_shift;
            make_vlist(VListParam::Top(
                top,
                vec![
                    VListChild::Kern(fm.big_op_spacing5),
                    elem_ml(sub_e, -slant),
                    VListChild::Kern(sub_k),
                    VListChild::elem(base),
                ],
            ))?
        }
        (Some((sup_e, sup_k)), None) => {
            let bottom = bd + base_shift;
            make_vlist(VListParam::Bottom(
                bottom,
                vec![
                    VListChild::elem(base),
                    VListChild::Kern(sup_k),
                    elem_ml(sup_e, slant),
                    VListChild::Kern(fm.big_op_spacing5),
                ],
            ))?
        }
        (None, None) => {
            let HtmlNode::Span(b) = base else {
                unreachable!()
            };
            return Ok(*b);
        }
    };
    let mut parts = vec![HtmlNode::span(final_group)];
    if has_sub && slant != 0.0 && !sub_is_single_character {
        let mut spacer = make_span_s(vec!["mspace"], vec![], Some(options), CssStyle::new());
        spacer.style.set(CssProp::MarginRight, make_em(slant));
        parts.insert(0, HtmlNode::span(spacer));
    }
    let HtmlNode::Span(s) = make_span(
        vec!["mop", "op-limits"],
        parts,
        Some(options),
        CssStyle::new(),
    ) else {
        unreachable!()
    };
    Ok(*s)
}

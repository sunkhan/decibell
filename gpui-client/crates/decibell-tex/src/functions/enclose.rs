//! Port of KaTeX `src/functions/enclose.ts` (`\colorbox`, `\fcolorbox`, `\fbox`, `\cancel`,
//! `\bcancel`, `\xcancel`, `\phase`, `\sout`, `\angl`).

use crate::build_common::{
    VListElem, VListParam, make_span, make_span_s, make_svg_span, make_vlist, wrap_fragment,
};
use crate::build_html::build_group;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode, SvgChild, SvgNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{EncloseNode, Measurement, ParseNode, type_error};
use crate::stretchy::stretchy_enclose;
use crate::svg_geometry::phase_path;
use crate::units::{calculate_size, make_em, make_em_str};
use crate::utils::{is_character_box, js_num};

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Enclose(g) = group else {
        unreachable!()
    };
    let mut inner = wrap_fragment(build_group(Some(&g.body), options, None)?, options);
    let label = g.label.get(1..).unwrap_or("");
    let mut scale = options.size_multiplier;
    let is_single_char = is_character_box(&g.body);
    let is_cancel = label.contains("cancel");
    let mut img;
    let img_shift;
    if label == "sout" {
        img = make_span_s(
            vec!["katex-stretchy", "katex-sout"],
            vec![],
            None,
            CssStyle::new(),
        );
        img.height = options.font_metrics().default_rule_thickness / scale;
        img_shift = -0.5 * options.font_metrics().x_height;
    } else if label == "phase" {
        let line_weight = calculate_size(&Measurement::new(0.6, "pt"), options)?;
        let clearance = calculate_size(&Measurement::new(0.35, "ex"), options)?;
        let new_options = options.having_base_sizing();
        scale /= new_options.size_multiplier;
        let angle_height = inner.height() + inner.depth() + line_weight + clearance;
        inner.style_mut().set(
            CssProp::PaddingLeft,
            make_em(angle_height / 2.0 + line_weight),
        );
        let view_box_height = (1000.0 * angle_height * scale).floor();
        let path = phase_path(view_box_height);
        let svg = SvgNode {
            children: vec![SvgChild::Path {
                name: "phase",
                alternate: Some(path),
            }],
            attributes: vec![
                ("width", "400em".to_string()),
                ("height", make_em_str(view_box_height / 1000.0)),
                ("viewBox", format!("0 0 400000 {}", js_num(view_box_height))),
                ("preserveAspectRatio", "xMinYMin slice".to_string()),
            ],
            style: CssStyle::new(),
        };
        img = make_svg_span(vec!["hide-tail"], vec![svg], Some(options), CssStyle::new());
        img.style.set(CssProp::Height, make_em(angle_height));
        img_shift = inner.depth() + line_weight + clearance;
    } else {
        let pad = if is_cancel {
            if is_single_char {
                None
            } else {
                Some("cancel-pad")
            }
        } else if label == "angl" {
            Some("anglpad")
        } else {
            Some("boxpad")
        };
        if let (Some(p), Some(c)) = (pad, inner.classes_mut()) {
            c.push(p);
        }
        let fm = options.font_metrics();
        let top_pad;
        let bottom_pad;
        let mut rule_thickness = 0.0;
        if label.contains("box") {
            rule_thickness = fm.fboxrule.max(options.min_rule_thickness);
            top_pad = fm.fboxsep
                + if label == "colorbox" {
                    0.0
                } else {
                    rule_thickness
                };
            bottom_pad = top_pad;
        } else if label == "angl" {
            rule_thickness = fm.default_rule_thickness.max(options.min_rule_thickness);
            top_pad = 4.0 * rule_thickness;
            bottom_pad = 0.0f64.max(0.25 - inner.depth());
        } else {
            top_pad = if is_single_char { 0.2 } else { 0.0 };
            bottom_pad = top_pad;
        }
        img = stretchy_enclose(&inner, label, top_pad, bottom_pad, options)?;
        if label.contains("fbox") || label.contains("boxed") || label.contains("fcolorbox") {
            img.style.set(CssProp::BorderStyle, CssVal::Static("solid"));
            img.style.set(CssProp::BorderWidth, make_em(rule_thickness));
        } else if label == "angl" && rule_thickness != 0.049 {
            img.style
                .set(CssProp::BorderTopWidth, make_em(rule_thickness));
            img.style
                .set(CssProp::BorderRightWidth, make_em(rule_thickness));
        }
        img_shift = inner.depth() + bottom_pad;
        if let Some(bg) = &g.background_color {
            img.style
                .set(CssProp::BackgroundColor, CssVal::Str(bg.as_str().into()));
            if let Some(bc) = &g.border_color {
                img.style
                    .set(CssProp::BorderColor, CssVal::Str(bc.as_str().into()));
            }
        }
    }
    let (inner_h, inner_d) = (inner.height(), inner.depth());
    let mut vlist = if g.background_color.is_some() {
        make_vlist(VListParam::IndividualShift(vec![
            VListElem::shifted(HtmlNode::span(img), img_shift),
            VListElem::shifted(inner, 0.0),
        ]))?
    } else {
        let mut e = VListElem::shifted(HtmlNode::span(img), img_shift);
        if is_cancel || label.contains("phase") {
            e.wrapper_classes = vec!["svg-align"];
        }
        make_vlist(VListParam::IndividualShift(vec![
            VListElem::shifted(inner, 0.0),
            e,
        ]))?
    };
    if is_cancel {
        vlist.height = inner_h;
        vlist.depth = inner_d;
    }
    let classes = if is_cancel && !is_single_char {
        vec!["mord", "cancel-lap"]
    } else {
        vec!["mord"]
    };
    Ok(make_span(
        classes,
        vec![HtmlNode::span(vlist)],
        Some(options),
        CssStyle::new(),
    ))
}

fn color_token(node: ParseNode) -> Result<String> {
    match node {
        ParseNode::ColorToken { color, .. } => Ok(color),
        other => type_error("color-token", &other),
    }
}

fn enclose(
    mode: crate::parse_node::Mode,
    label: &str,
    body: ParseNode,
    bg: Option<String>,
    border: Option<String>,
) -> ParseNode {
    ParseNode::Enclose(Box::new(EncloseNode {
        mode,
        body: Box::new(body),
        label: label.to_string(),
        background_color: bg,
        border_color: border,
    }))
}

fn colorbox_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let color = color_token(it.next().unwrap())?;
    let body = it.next().unwrap();
    Ok(enclose(
        ctx.parser.mode,
        ctx.func_name,
        body,
        Some(color),
        None,
    ))
}

fn fcolorbox_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let border = color_token(it.next().unwrap())?;
    let background = color_token(it.next().unwrap())?;
    let body = it.next().unwrap();
    Ok(enclose(
        ctx.parser.mode,
        ctx.func_name,
        body,
        Some(background),
        Some(border),
    ))
}

fn fbox_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(enclose(
        ctx.parser.mode,
        "\\fbox",
        args.into_iter().next().unwrap(),
        None,
        None,
    ))
}

fn named_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    // `\sout` in math mode only warns (strict: "warn").
    Ok(enclose(
        ctx.parser.mode,
        ctx.func_name,
        args.into_iter().next().unwrap(),
        None,
        None,
    ))
}

fn angl_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(enclose(
        ctx.parser.mode,
        "\\angl",
        args.into_iter().next().unwrap(),
        None,
        None,
    ))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "enclose",
        names: &["\\colorbox"],
        num_args: 2,
        allowed_in_text: true,
        arg_types: &[ArgType::Color, ArgType::HBox],
        handler: Some(colorbox_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "enclose",
        names: &["\\fcolorbox"],
        num_args: 3,
        allowed_in_text: true,
        arg_types: &[ArgType::Color, ArgType::Color, ArgType::HBox],
        handler: Some(fcolorbox_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "enclose",
        names: &["\\fbox"],
        num_args: 1,
        arg_types: &[ArgType::HBox],
        allowed_in_text: true,
        handler: Some(fbox_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "enclose",
        names: &["\\cancel", "\\bcancel", "\\xcancel", "\\phase"],
        num_args: 1,
        handler: Some(named_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "enclose",
        names: &["\\sout"],
        num_args: 1,
        allowed_in_text: true,
        handler: Some(named_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "enclose",
        names: &["\\angl"],
        num_args: 1,
        arg_types: &[ArgType::HBox],
        allowed_in_text: false,
        handler: Some(angl_handler),
        ..FunctionSpec::DEFAULT
    },
];

//! Port of KaTeX `src/stretchy.ts` (HTML half; `stretchyMathML` is not needed): the SVG spans
//! of stretchy accents, arrows and braces (`stretchySvg`) and the boxes / strike lines of
//! `\fbox`, `\colorbox`, `\cancel` & co. (`stretchyEnclose`).

use crate::build_common::{make_span_s, make_svg_span};
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode, Span, SvgChild, SvgNode};
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::ParseNode;
use crate::units::{make_em, make_em_str};
use crate::utils::js_num;

/// `katexImagesData[label]`: path names, min width (em), viewBox height, and the
/// preserveAspectRatio alignment of single-path images.
fn katex_images_data(
    label: &str,
) -> Option<(&'static [&'static str], f64, f64, Option<&'static str>)> {
    Some(match label {
        "overrightarrow" => (&["rightarrow"], 0.888, 522.0, Some("xMaxYMin")),
        "overleftarrow" => (&["leftarrow"], 0.888, 522.0, Some("xMinYMin")),
        "underrightarrow" => (&["rightarrow"], 0.888, 522.0, Some("xMaxYMin")),
        "underleftarrow" => (&["leftarrow"], 0.888, 522.0, Some("xMinYMin")),
        "xrightarrow" => (&["rightarrow"], 1.469, 522.0, Some("xMaxYMin")),
        "\\cdrightarrow" => (&["rightarrow"], 3.0, 522.0, Some("xMaxYMin")),
        "xleftarrow" => (&["leftarrow"], 1.469, 522.0, Some("xMinYMin")),
        "\\cdleftarrow" => (&["leftarrow"], 3.0, 522.0, Some("xMinYMin")),
        "Overrightarrow" => (&["doublerightarrow"], 0.888, 560.0, Some("xMaxYMin")),
        "xRightarrow" => (&["doublerightarrow"], 1.526, 560.0, Some("xMaxYMin")),
        "xLeftarrow" => (&["doubleleftarrow"], 1.526, 560.0, Some("xMinYMin")),
        "overleftharpoon" => (&["leftharpoon"], 0.888, 522.0, Some("xMinYMin")),
        "xleftharpoonup" => (&["leftharpoon"], 0.888, 522.0, Some("xMinYMin")),
        "xleftharpoondown" => (&["leftharpoondown"], 0.888, 522.0, Some("xMinYMin")),
        "overrightharpoon" => (&["rightharpoon"], 0.888, 522.0, Some("xMaxYMin")),
        "xrightharpoonup" => (&["rightharpoon"], 0.888, 522.0, Some("xMaxYMin")),
        "xrightharpoondown" => (&["rightharpoondown"], 0.888, 522.0, Some("xMaxYMin")),
        "xlongequal" => (&["longequal"], 0.888, 334.0, Some("xMinYMin")),
        "\\cdlongequal" => (&["longequal"], 3.0, 334.0, Some("xMinYMin")),
        "xtwoheadleftarrow" => (&["twoheadleftarrow"], 0.888, 334.0, Some("xMinYMin")),
        "xtwoheadrightarrow" => (&["twoheadrightarrow"], 0.888, 334.0, Some("xMaxYMin")),

        "overleftrightarrow" => (&["leftarrow", "rightarrow"], 0.888, 522.0, None),
        "overbrace" => (&["leftbrace", "midbrace", "rightbrace"], 1.6, 548.0, None),
        "underbrace" => (
            &["leftbraceunder", "midbraceunder", "rightbraceunder"],
            1.6,
            548.0,
            None,
        ),
        "underleftrightarrow" => (&["leftarrow", "rightarrow"], 0.888, 522.0, None),
        "xleftrightarrow" => (&["leftarrow", "rightarrow"], 1.75, 522.0, None),
        "xLeftrightarrow" => (&["doubleleftarrow", "doublerightarrow"], 1.75, 560.0, None),
        "xrightleftharpoons" => (
            &["leftharpoondownplus", "rightharpoonplus"],
            1.75,
            716.0,
            None,
        ),
        "xleftrightharpoons" => (
            &["leftharpoonplus", "rightharpoondownplus"],
            1.75,
            716.0,
            None,
        ),
        "xhookleftarrow" => (&["leftarrow", "righthook"], 1.08, 522.0, None),
        "xhookrightarrow" => (&["lefthook", "rightarrow"], 1.08, 522.0, None),
        "overlinesegment" => (&["leftlinesegment", "rightlinesegment"], 0.888, 522.0, None),
        "underlinesegment" => (&["leftlinesegment", "rightlinesegment"], 0.888, 522.0, None),
        "overbracket" => (&["leftbracketover", "rightbracketover"], 1.6, 440.0, None),
        "underbracket" => (&["leftbracketunder", "rightbracketunder"], 1.6, 410.0, None),
        "overgroup" => (&["leftgroup", "rightgroup"], 0.888, 342.0, None),
        "undergroup" => (&["leftgroupunder", "rightgroupunder"], 0.888, 342.0, None),
        "xmapsto" => (&["leftmapsto", "rightarrow"], 1.5, 522.0, None),
        "xtofrom" => (&["leftToFrom", "rightToFrom"], 1.75, 528.0, None),

        "xrightleftarrows" => (
            &["baraboveleftarrow", "rightarrowabovebar"],
            1.75,
            901.0,
            None,
        ),
        "xrightequilibrium" => (
            &["baraboveshortleftharpoon", "rightharpoonaboveshortbar"],
            1.75,
            716.0,
            None,
        ),
        "xleftequilibrium" => (
            &["shortbaraboveleftharpoon", "shortrightharpoonabovebar"],
            1.75,
            716.0,
            None,
        ),
        _ => return None,
    })
}

/// The wide-accent path names (`label + imgIndex`, `"tilde" + imgIndex`), interned.
fn wide_path_name(prefix: &str, index: usize) -> &'static str {
    const WIDEHAT: [&str; 5] = ["widehat0", "widehat1", "widehat2", "widehat3", "widehat4"];
    const WIDECHECK: [&str; 5] = [
        "widecheck0",
        "widecheck1",
        "widecheck2",
        "widecheck3",
        "widecheck4",
    ];
    const TILDE: [&str; 5] = ["tilde0", "tilde1", "tilde2", "tilde3", "tilde4"];
    match prefix {
        "widehat" => WIDEHAT[index],
        "widecheck" => WIDECHECK[index],
        _ => TILDE[index],
    }
}

fn svg(paths: Vec<SvgChild>, attributes: Vec<(&'static str, String)>) -> SvgNode {
    SvgNode {
        children: paths,
        attributes,
        style: CssStyle::new(),
    }
}

/// `stretchySvg(group, options)` for accent / accentUnder / xArrow / horizBrace nodes.
pub fn stretchy_svg(group: &ParseNode, options: &Options) -> Result<Span> {
    let (label, base): (&str, Option<&ParseNode>) = match group {
        ParseNode::Accent(a) | ParseNode::AccentUnder(a) => (&a.label, Some(&a.base)),
        ParseNode::HorizBrace { label, base, .. } => (label, Some(base)),
        ParseNode::XArrow { label, .. } => (label, None),
        other => {
            return ierr(format!(
                "stretchySvg called on a {} node",
                other.type_name()
            ));
        }
    };
    // `group.label.slice(1)` (labels are ASCII control sequences)
    let label = label.get(1..).unwrap_or("");
    let (mut span, min_width, height);
    if let (true, Some(base)) = (
        matches!(label, "widehat" | "widecheck" | "widetilde" | "utilde"),
        base,
    ) {
        let num_chars = match base {
            ParseNode::OrdGroup(g) => g.body.len(),
            _ => 1,
        };
        let (view_box_width, view_box_height, h, path_name);
        if num_chars > 5 {
            if label == "widehat" || label == "widecheck" {
                view_box_height = 420.0;
                view_box_width = 2364.0;
                h = 0.42;
                path_name = wide_path_name(label, 4);
            } else {
                view_box_height = 312.0;
                view_box_width = 2340.0;
                h = 0.34;
                path_name = "tilde4";
            }
        } else {
            let img_index = [1usize, 1, 2, 2, 3, 3][num_chars];
            if label == "widehat" || label == "widecheck" {
                view_box_width = [0.0, 1062.0, 2364.0, 2364.0, 2364.0][img_index];
                view_box_height = [0.0, 239.0, 300.0, 360.0, 420.0][img_index];
                h = [0.0, 0.24, 0.3, 0.3, 0.36, 0.42][img_index];
                path_name = wide_path_name(label, img_index);
            } else {
                view_box_width = [0.0, 600.0, 1033.0, 2339.0, 2340.0][img_index];
                view_box_height = [0.0, 260.0, 286.0, 306.0, 312.0][img_index];
                h = [0.0, 0.26, 0.286, 0.3, 0.306, 0.34][img_index];
                path_name = wide_path_name("tilde", img_index);
            }
        }
        let node = svg(
            vec![SvgChild::Path {
                name: path_name,
                alternate: None,
            }],
            vec![
                ("width", "100%".to_string()),
                ("height", make_em_str(h)),
                (
                    "viewBox",
                    format!("0 0 {} {}", js_num(view_box_width), js_num(view_box_height)),
                ),
                ("preserveAspectRatio", "none".to_string()),
            ],
        );
        span = make_svg_span(vec![], vec![node], Some(options), CssStyle::new());
        min_width = 0.0;
        height = h;
    } else {
        let view_box_width = 400000.0;
        let Some((paths, mw, view_box_height, align)) = katex_images_data(label) else {
            return ierr(format!("No SVG data for \"{}\".", label));
        };
        height = view_box_height / 1000.0;
        min_width = mw;
        let (width_classes, aligns): (&[&'static str], [&str; 3]) = match paths.len() {
            1 => match align {
                Some(a) => (&["hide-tail"], [a, "", ""]),
                None => {
                    return ierr(format!(
                        "Expected 4-tuple for single-path SVG data \"{}\".",
                        label
                    ));
                }
            },
            2 => (
                &["halfarrow-left", "halfarrow-right"],
                ["xMinYMin", "xMaxYMin", ""],
            ),
            3 => (
                &["brace-left", "brace-center", "brace-right"],
                ["xMinYMin", "xMidYMin", "xMaxYMin"],
            ),
            n => {
                return ierr(format!(
                    "Correct katexImagesData or update code here to support\n                    {} children.",
                    n
                ));
            }
        };
        let mut spans = Vec::new();
        let mut single = None;
        for (i, p) in paths.iter().enumerate() {
            let node = svg(
                vec![SvgChild::Path {
                    name: p,
                    alternate: None,
                }],
                vec![
                    ("width", "400em".to_string()),
                    ("height", make_em_str(height)),
                    (
                        "viewBox",
                        format!("0 0 {} {}", js_num(view_box_width), js_num(view_box_height)),
                    ),
                    ("preserveAspectRatio", format!("{} slice", aligns[i])),
                ],
            );
            let mut s = make_svg_span(
                vec![width_classes[i]],
                vec![node],
                Some(options),
                CssStyle::new(),
            );
            if paths.len() == 1 {
                single = Some(s);
                break;
            }
            s.style.set(CssProp::Height, make_em(height));
            spans.push(HtmlNode::span(s));
        }
        span = match single {
            Some(s) => s,
            None => make_span_s(
                vec!["katex-stretchy"],
                spans,
                Some(options),
                CssStyle::new(),
            ),
        };
    }
    span.height = height;
    span.style.set(CssProp::Height, make_em(height));
    if min_width > 0.0 {
        span.style.set(CssProp::MinWidth, make_em(min_width));
    }
    Ok(span)
}

/// `stretchyEnclose(inner, label, topPad, bottomPad, options)`; `label` is the command name
/// without its backslash.
pub fn stretchy_enclose(
    inner: &HtmlNode,
    label: &str,
    top_pad: f64,
    bottom_pad: f64,
    options: &Options,
) -> Result<Span> {
    let total_height = inner.height() + inner.depth() + top_pad + bottom_pad;
    let mut img;
    if label.contains("fbox") || label.contains("color") || label.contains("angl") {
        let class: &'static str = match label {
            "fbox" => "fbox",
            "colorbox" => "colorbox",
            "fcolorbox" => "fcolorbox",
            "angl" => "angl",
            other => return ierr(format!("stretchyEnclose: unexpected label {}", other)),
        };
        img = make_span_s(
            vec!["katex-stretchy", class],
            vec![],
            Some(options),
            CssStyle::new(),
        );
        if label == "fbox"
            && options.color.is_some()
            && let Some(color) = options.get_color()
        {
            img.style.set(CssProp::BorderColor, CssVal::Str(color));
        }
    } else {
        let mut lines = Vec::new();
        if label == "bcancel" || label == "xcancel" {
            lines.push(SvgChild::Line {
                attributes: vec![
                    ("x1", "0".to_string()),
                    ("y1", "0".to_string()),
                    ("x2", "100%".to_string()),
                    ("y2", "100%".to_string()),
                    ("stroke-width", "0.046em".to_string()),
                ],
            });
        }
        if label == "cancel" || label == "xcancel" {
            lines.push(SvgChild::Line {
                attributes: vec![
                    ("x1", "0".to_string()),
                    ("y1", "100%".to_string()),
                    ("x2", "100%".to_string()),
                    ("y2", "0".to_string()),
                    ("stroke-width", "0.046em".to_string()),
                ],
            });
        }
        let node = svg(
            lines,
            vec![
                ("width", "100%".to_string()),
                ("height", make_em_str(total_height)),
            ],
        );
        img = make_svg_span(vec![], vec![node], Some(options), CssStyle::new());
    }
    img.height = total_height;
    img.style.set(CssProp::Height, make_em(total_height));
    Ok(img)
}

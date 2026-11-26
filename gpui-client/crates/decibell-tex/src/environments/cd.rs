//! Port of KaTeX `src/environments/cd.ts` (HTML side): `parseCD` (the `{CD}` environment's
//! body, used by `array.rs`) and the internal `\\cdleft` / `\\cdright` / `\\cdparent`
//! functions that place labels beside vertical arrows. The `{CD}` environment spec itself is
//! defined in `array.ts` (here: `array::ENV_SPECS`), so `ENV_SPECS` is empty.

use crate::build_common::wrap_fragment;
use crate::build_html::build_group;
use crate::define_function::{EnvSpec, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, HtmlNode};
use crate::macros::MacroDef;
use crate::options::Options;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{
    AlignSpec, ArrayNode, Atom, AtomNode, ColSeparationType, Mode, OrdGroupNode, ParseNode, Side,
    StyleStr, SymNode, assert_symbol_text,
};
use crate::parser::Parser;
use crate::token::Src;
use crate::units::make_em;

pub static ENV_SPECS: &[EnvSpec] = &[];

/// `newCell()`: an empty display-style cell, as `parseArray` would build it.
fn new_cell() -> Vec<ParseNode> {
    Vec::new()
}

fn wrap_cell(body: Vec<ParseNode>) -> ParseNode {
    ParseNode::Styling {
        mode: Mode::Math,
        style: StyleStr::Display,
        reset_font: true,
        body,
    }
}

/// `isStartOfArrow(node)`.
fn is_start_of_arrow(node: &ParseNode) -> bool {
    matches!(node, ParseNode::TextOrd(n) if n.text == "@")
}

/// `isLabelEnd(node, endChar)`.
fn is_label_end(node: &ParseNode, end_char: &str) -> bool {
    match node {
        ParseNode::MathOrd(n) => n.text == end_char,
        ParseNode::Atom(n) => n.text == end_char,
        _ => false,
    }
}

fn math_ordgroup(body: Vec<ParseNode>) -> ParseNode {
    ParseNode::OrdGroup(OrdGroupNode {
        mode: Mode::Math,
        body,
        semisimple: false,
        loc: None,
    })
}

/// `cdArrow(arrowChar, labels, parser)`.
fn cd_arrow(
    arrow_char: &str,
    labels: [Vec<ParseNode>; 2],
    parser: &mut Parser,
) -> Result<ParseNode> {
    let [l0, l1] = labels;
    match arrow_char {
        ">" | "<" => {
            let name = if arrow_char == ">" {
                "\\\\cdrightarrow"
            } else {
                "\\\\cdleftarrow"
            };
            parser.call_function(
                name,
                vec![math_ordgroup(l0)],
                vec![Some(math_ordgroup(l1))],
                None,
                None,
            )
        }
        "A" | "V" => {
            let func_name = if arrow_char == "A" {
                "\\uparrow"
            } else {
                "\\downarrow"
            };
            let left_label =
                parser.call_function("\\\\cdleft", vec![math_ordgroup(l0)], vec![], None, None)?;
            let bare_arrow = ParseNode::Atom(AtomNode {
                mode: Mode::Math,
                family: Atom::Rel,
                text: func_name.into(),
                loc: None,
            });
            let sized_arrow =
                parser.call_function("\\Big", vec![bare_arrow], vec![], None, None)?;
            let right_label =
                parser.call_function("\\\\cdright", vec![math_ordgroup(l1)], vec![], None, None)?;
            let arrow_group = math_ordgroup(vec![left_label, sized_arrow, right_label]);
            parser.call_function("\\\\cdparent", vec![arrow_group], vec![], None, None)
        }
        "=" => parser.call_function("\\\\cdlongequal", vec![], vec![], None, None),
        "|" => {
            let arrow = ParseNode::TextOrd(SymNode {
                mode: Mode::Math,
                text: "\\Vert".into(),
                loc: None,
            });
            parser.call_function("\\Big", vec![arrow], vec![], None, None)
        }
        _ => Ok(ParseNode::TextOrd(SymNode {
            mode: Mode::Math,
            text: " ".into(),
            loc: None,
        })),
    }
}

/// `parseCD(parser)`.
pub fn parse_cd(parser: &mut Parser) -> Result<ArrayNode> {
    // Get the array's parse nodes with \\ temporarily mapped to \cr.
    let mut parsed_rows: Vec<Vec<ParseNode>> = Vec::new();
    parser.gullet.begin_group();
    parser.gullet.macros.set(
        "\\cr",
        Some(MacroDef::Str(Src::Static("\\\\\\relax"))),
        false,
    );
    parser.gullet.begin_group();
    loop {
        parsed_rows.push(parser.parse_expression(false, Some("\\\\"))?);
        parser.gullet.end_group()?;
        parser.gullet.begin_group();
        let next = parser.fetch()?.text.clone();
        if next == "&" || next == "\\\\" {
            parser.consume();
        } else if next == "\\end" {
            if parsed_rows.last().map(|r| r.is_empty()).unwrap_or(false) {
                parsed_rows.pop(); // final row ended in \\
            }
            break;
        } else {
            let loc = parser.next_token.as_ref().and_then(|t| t.loc.clone());
            return perr("Expected \\\\ or \\cr or \\end", loc.as_ref());
        }
    }

    let mut row: Vec<ParseNode> = Vec::new();
    let mut body: Vec<Vec<ParseNode>> = Vec::new();

    for (i, row_nodes) in parsed_rows.into_iter().enumerate() {
        let mut nodes: Vec<Option<ParseNode>> = row_nodes.into_iter().map(Some).collect();
        let mut cell = new_cell();
        let mut j = 0usize;
        while j < nodes.len() {
            let is_arrow = nodes[j].as_ref().map(is_start_of_arrow).unwrap_or(false);
            if !is_arrow {
                cell.push(nodes[j].take().unwrap());
            } else {
                row.push(wrap_cell(std::mem::take(&mut cell)));
                j += 1;
                let Some(Some(arrow_node)) = nodes.get(j) else {
                    // assertSymbolNodeType(undefined) throws a TypeError in KaTeX.
                    return ierr("Cannot read properties of undefined (reading 'type')");
                };
                let arrow_char = assert_symbol_text(arrow_node)?.to_string();
                let mut labels: [Vec<ParseNode>; 2] = [Vec::new(), Vec::new()];
                if "=|.".contains(arrow_char.as_str()) {
                    // no labels
                } else if "<>AV".contains(arrow_char.as_str()) {
                    for label in labels.iter_mut() {
                        let mut in_label = true;
                        let mut k = j + 1;
                        while k < nodes.len() {
                            let n = nodes[k].as_ref().unwrap();
                            if is_label_end(n, &arrow_char) {
                                in_label = false;
                                j = k;
                                break;
                            }
                            if is_start_of_arrow(n) {
                                return perr(
                                    format!(
                                        "Missing a {} character to complete a CD arrow.",
                                        arrow_char
                                    ),
                                    n.loc(),
                                );
                            }
                            label.push(nodes[k].take().unwrap());
                            k += 1;
                        }
                        if in_label {
                            let loc = nodes[j].as_ref().and_then(|n| n.loc().cloned());
                            return perr(
                                format!(
                                    "Missing a {} character to complete a CD arrow.",
                                    arrow_char
                                ),
                                loc.as_ref(),
                            );
                        }
                    }
                } else {
                    return perr("Expected one of \"<>AV=|.\" after @", arrow_node.loc());
                }
                let arrow = cd_arrow(&arrow_char, labels, parser)?;
                row.push(ParseNode::Styling {
                    mode: Mode::Math,
                    style: StyleStr::Display,
                    reset_font: true,
                    body: vec![arrow],
                });
                cell = new_cell();
            }
            j += 1;
        }
        if i % 2 == 0 {
            // Even rows: cell, arrow, cell, arrow, … cell
            row.push(wrap_cell(cell));
        } else if !row.is_empty() {
            // Odd rows: vert arrow, empty cell, … vert arrow
            row.remove(0);
        }
        body.push(std::mem::take(&mut row));
    }
    // The row KaTeX pushes after the last one.
    body.push(Vec::new());

    // End row group, then the array group defining \\.
    parser.gullet.end_group()?;
    parser.gullet.end_group()?;

    let n = body[0].len();
    let cols = vec![
        AlignSpec::Align {
            align: "c".into(),
            pregap: Some(0.25),
            postgap: Some(0.25)
        };
        n
    ];
    let rows = body.len();
    Ok(ArrayNode {
        mode: Mode::Math,
        col_separation_type: Some(ColSeparationType::Cd),
        hskip_before_and_after: None,
        add_jot: Some(true),
        cols: Some(cols),
        arraystretch: 1.0,
        body,
        row_gaps: vec![None],
        h_lines_before_row: vec![Vec::new(); rows + 1],
        tags: None,
        leqno: None,
    })
}

fn cdlabel_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let side = if ctx.func_name == "\\\\cdleft" {
        Side::Left
    } else {
        Side::Right
    };
    let label = args.into_iter().next().unwrap();
    Ok(ParseNode::CdLabel {
        mode: ctx.parser.mode,
        side,
        label: Box::new(label),
    })
}

fn cdparent_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let fragment = args.into_iter().next().unwrap();
    Ok(ParseNode::CdLabelParent {
        mode: ctx.parser.mode,
        fragment: Box::new(fragment),
    })
}

pub fn html_builder_cdlabel(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::CdLabel { side, label, .. } = group else {
        unreachable!()
    };
    let new_options = options.having_style(options.style.sup());
    let mut label = wrap_fragment(
        build_group(Some(label), &new_options, Some(options))?,
        options,
    );
    if let Some(c) = label.classes_mut() {
        c.push(match side {
            Side::Left => "cd-label-left",
            Side::Right => "cd-label-right",
        });
    }
    let d = label.depth();
    label.style_mut().set(CssProp::Bottom, make_em(0.8 - d));
    // Zero the label's height and depth so the arrow sets the row's vertical alignment.
    label.set_height(0.0);
    label.set_depth(0.0);
    Ok(label)
}

pub fn html_builder_cdlabelparent(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::CdLabelParent { fragment, .. } = group else {
        unreachable!()
    };
    let mut parent = wrap_fragment(build_group(Some(fragment), options, None)?, options);
    if let Some(c) = parent.classes_mut() {
        c.push("cd-vert-arrow");
    }
    Ok(parent)
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "cdlabel",
        names: &["\\\\cdleft", "\\\\cdright"],
        num_args: 1,
        handler: Some(cdlabel_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "cdlabelparent",
        names: &["\\\\cdparent"],
        num_args: 1,
        handler: Some(cdparent_handler),
        ..FunctionSpec::DEFAULT
    },
];

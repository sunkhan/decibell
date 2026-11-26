//! Port of KaTeX `src/environments/array.ts` (HTML side): `parseArray`, every environment it
//! defines (array, darray, the matrix family, smallmatrix, subarray, cases, align, gather,
//! alignat, equation, CD hookup), the shared HTML builder, and `\hline` outside arrays.
//! `\nonumber` / `\notag` are string macros (in `data/macros_data.rs`).

use crate::build_common::{
    VListElem, VListParam, make_fragment, make_line_span, make_span, make_span_s, make_vlist,
};
use crate::build_html::{RealGroup, build_expression, build_group};
use crate::define_function::{ArgType, EnvContext, EnvSpec, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode};
use crate::environments::cd::parse_cd;
use crate::macros::MacroDef;
use crate::options::Options;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{
    AlignSpec, ArrayNode, ColSeparationType, LeftRightNode, Measurement, OrdGroupNode, ParseNode,
    RowTag, StyleStr, assert_character_group, assert_symbol_text, type_error,
};
use crate::parser::{ArgSpec, Parser};
use crate::style::Style;
use crate::token::{Src, Token};
use crate::units::{calculate_size, make_em, make_em_str};
use crate::utils::js_num;

/// `getHLines(parser)`: one entry per `\hline` / `\hdashline` (true = dashed).
fn get_h_lines(parser: &mut Parser) -> Result<Vec<bool>> {
    let mut info = Vec::new();
    parser.consume_spaces()?;
    let mut nxt = parser.fetch()?.text.clone();
    if nxt == "\\relax" {
        // an artifact of the \cr macro
        parser.consume();
        parser.consume_spaces()?;
        nxt = parser.fetch()?.text.clone();
    }
    while nxt == "\\hline" || nxt == "\\hdashline" {
        parser.consume();
        info.push(nxt == "\\hdashline");
        parser.consume_spaces()?;
        nxt = parser.fetch()?.text.clone();
    }
    Ok(info)
}

/// `validateAmsEnvironmentContext(context)`.
fn validate_ams_environment_context(ctx: &EnvContext) -> Result<()> {
    if !ctx.parser.settings.display_mode {
        return perr(
            format!("{{{}}} can be used only in display mode.", ctx.env_name),
            None,
        );
    }
    Ok(())
}

/// `getAutoTag(name)`: `None` is KaTeX's `undefined`.
fn get_auto_tag(name: &str) -> Option<bool> {
    if !name.contains("ed") {
        Some(!name.contains('*'))
    } else {
        None
    }
}

/// `parseArray`'s option object.
#[derive(Default)]
struct ArrayOpts {
    hskip_before_and_after: Option<bool>,
    add_jot: Option<bool>,
    cols: Option<Vec<AlignSpec>>,
    arraystretch: Option<f64>,
    col_separation_type: Option<ColSeparationType>,
    auto_tag: Option<bool>,
    single_row: bool,
    empty_single_row: bool,
    max_num_cols: Option<usize>,
    leqno: Option<bool>,
}

/// JavaScript `parseFloat(s)`.
fn js_parse_float(s: &str) -> f64 {
    let t = s.trim_start();
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if t[i..].starts_with("Infinity") {
        return if b.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut had_digits = i > digits_start;
    if i < b.len() && b[i] == b'.' {
        let j = i + 1;
        let mut k = j;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > j || had_digits {
            had_digits = had_digits || k > j;
            i = k;
        }
    }
    if !had_digits {
        return f64::NAN;
    }
    // exponent
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut k = i + 1;
        if k < b.len() && (b[k] == b'+' || b[k] == b'-') {
            k += 1;
        }
        let es = k;
        while k < b.len() && b[k].is_ascii_digit() {
            k += 1;
        }
        if k > es {
            i = k;
        }
    }
    let lit = &t[..i];
    let lit = lit.strip_prefix('+').unwrap_or(lit);
    let fixed = lit.strip_suffix('.').unwrap_or(lit);
    let fixed = if let Some(r) = fixed.strip_prefix("-.") {
        format!("-0.{}", r)
    } else if let Some(r) = fixed.strip_prefix('.') {
        format!("0.{}", r)
    } else {
        fixed.to_string()
    };
    fixed.parse().unwrap_or(f64::NAN)
}

/// `parseSizeGroup(true)` through the public argument parser (an optional size argument).
fn parse_optional_size(parser: &mut Parser) -> Result<Option<Measurement>> {
    let spec = ArgSpec {
        num_args: 0,
        num_optional_args: 1,
        arg_types: &[ArgType::Size],
        primitive: false,
        is_sqrt: false,
    };
    let (_, mut opt) = parser.parse_arguments("\\\\", &spec)?;
    match opt.pop().flatten() {
        Some(ParseNode::Size { value, .. }) => Ok(Some(value)),
        Some(other) => type_error("size", &other),
        None => Ok(None),
    }
}

fn begin_row(parser: &mut Parser, auto_tag: Option<bool>) {
    if auto_tag == Some(true) {
        parser
            .gullet
            .macros
            .set("\\@eqnsw", Some(MacroDef::Str(Src::Static("1"))), true);
    }
}

fn end_row(
    parser: &mut Parser,
    auto_tag: Option<bool>,
    tags: &mut Option<Vec<RowTag>>,
) -> Result<()> {
    if let Some(tags) = tags {
        if parser.gullet.macros.get("\\df@tag").is_some() {
            let t = parser.subparse(vec![Token::new("\\df@tag", None)])?;
            tags.push(RowTag::Explicit(t));
            parser.gullet.macros.set("\\df@tag", None, true);
        } else {
            let eqnsw_one = matches!(parser.gullet.macros.get("\\@eqnsw"), Some(MacroDef::Str(s)) if s.as_str() == "1");
            tags.push(RowTag::Auto(auto_tag == Some(true) && eqnsw_one));
        }
    }
    Ok(())
}

fn next_token_loc(parser: &Parser) -> Option<crate::token::Loc> {
    parser.next_token.as_ref().and_then(|t| t.loc.clone())
}

/// `parseArray(parser, {...}, style)`.
fn parse_array(parser: &mut Parser, o: ArrayOpts, style: StyleStr) -> Result<ArrayNode> {
    parser.gullet.begin_group();
    if !o.single_row {
        // \cr is equivalent to \\ without the optional size argument
        parser.gullet.macros.set(
            "\\cr",
            Some(MacroDef::Str(Src::Static("\\\\\\relax"))),
            false,
        );
    }
    let mut arraystretch = o.arraystretch.unwrap_or(0.0);
    if arraystretch == 0.0 || arraystretch.is_nan() {
        match parser.gullet.expand_macro_as_text("\\arraystretch")? {
            None => arraystretch = 1.0,
            Some(stretch) => {
                arraystretch = js_parse_float(&stretch);
                if arraystretch == 0.0 || arraystretch.is_nan() || arraystretch < 0.0 {
                    return perr(format!("Invalid \\arraystretch: {}", stretch), None);
                }
            }
        }
    }
    // Start group for first cell
    parser.gullet.begin_group();

    let mut row: Vec<ParseNode> = Vec::new();
    let mut body: Vec<Vec<ParseNode>> = Vec::new();
    let mut row_gaps: Vec<Option<Measurement>> = Vec::new();
    let mut h_lines_before_row: Vec<Vec<bool>> = Vec::new();
    let mut tags: Option<Vec<RowTag>> = if o.auto_tag.is_some() {
        Some(Vec::new())
    } else {
        None
    };

    begin_row(parser, o.auto_tag);
    h_lines_before_row.push(get_h_lines(parser)?);

    loop {
        let cell_body =
            parser.parse_expression(false, Some(if o.single_row { "\\end" } else { "\\\\" }))?;
        parser.gullet.end_group()?;
        parser.gullet.begin_group();
        let cell = ParseNode::Styling {
            mode: parser.mode,
            style,
            reset_font: true,
            body: vec![ParseNode::OrdGroup(OrdGroupNode {
                mode: parser.mode,
                body: cell_body,
                semisimple: false,
                loc: None,
            })],
        };
        row.push(cell);
        let next = parser.fetch()?.text.clone();
        if next == "&" {
            // {equation} or {split}; for {array} KaTeX only warns (strict "warn").
            if o.max_num_cols == Some(row.len())
                && (o.single_row || o.col_separation_type.is_some())
            {
                return perr(
                    "Too many tab characters: &",
                    next_token_loc(parser).as_ref(),
                );
            }
            parser.consume();
        } else if next == "\\end" {
            end_row(parser, o.auto_tag, &mut tags)?;
            // `cell` is the last item in `row`.
            let only_empty = row.len() == 1
                && matches!(&row[0], ParseNode::Styling { body: b, .. }
                    if b.len() == 1 && matches!(&b[0], ParseNode::OrdGroup(g) if g.body.is_empty()));
            body.push(std::mem::take(&mut row));
            if only_empty && (body.len() > 1 || !o.empty_single_row) {
                body.pop();
            }
            if h_lines_before_row.len() < body.len() + 1 {
                h_lines_before_row.push(Vec::new());
            }
            break;
        } else if next == "\\\\" {
            parser.consume();
            let mut size = None;
            if parser.gullet.future()?.text != " " {
                size = parse_optional_size(parser)?;
            }
            row_gaps.push(size);
            end_row(parser, o.auto_tag, &mut tags)?;
            h_lines_before_row.push(get_h_lines(parser)?);
            body.push(std::mem::take(&mut row));
            begin_row(parser, o.auto_tag);
        } else {
            return perr(
                "Expected & or \\\\ or \\cr or \\end",
                next_token_loc(parser).as_ref(),
            );
        }
    }
    // End cell group, then the array group defining \cr.
    parser.gullet.end_group()?;
    parser.gullet.end_group()?;

    Ok(ArrayNode {
        mode: parser.mode,
        col_separation_type: o.col_separation_type,
        hskip_before_and_after: o.hskip_before_and_after,
        add_jot: o.add_jot,
        cols: o.cols,
        arraystretch,
        body,
        row_gaps,
        h_lines_before_row,
        tags,
        leqno: o.leqno,
    })
}

/// `dCellStyle(envName)`.
fn d_cell_style(env_name: &str) -> StyleStr {
    if env_name.starts_with('d') {
        StyleStr::Display
    } else {
        StyleStr::Text
    }
}

fn col_align_class(align: &str) -> Result<&'static str> {
    Ok(match align {
        "l" => "col-align-l",
        "c" => "col-align-c",
        "r" => "col-align-r",
        other => {
            return ierr(format!(
                "decibell-tex: no static class for col-align-{}",
                other
            ));
        }
    })
}

/// One built row (`Outrow`).
struct Outrow {
    cells: Vec<Option<HtmlNode>>,
    height: f64,
    depth: f64,
    pos: f64,
}

/// The array `htmlBuilder`.
pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Array(g) = group else {
        unreachable!()
    };
    let nr = g.body.len();
    let mut nc = 0usize;
    let mut hlines: Vec<(f64, bool)> = Vec::new();
    let fm = options.font_metrics();
    let rule_thickness = fm.array_rule_width.max(options.min_rule_thickness);

    // Horizontal spacing
    let pt = 1.0 / fm.pt_per_em;
    let mut arraycolsep = 5.0 * pt;
    if g.col_separation_type == Some(ColSeparationType::Small) {
        let local_multiplier = options.having_style(Style::SCRIPT).size_multiplier;
        arraycolsep = 0.2778 * (local_multiplier / options.size_multiplier);
    }

    // Vertical spacing
    let baselineskip = if g.col_separation_type == Some(ColSeparationType::Cd) {
        calculate_size(&Measurement::new(3.0, "ex"), options)?
    } else {
        12.0 * pt
    };
    let jot = 3.0 * pt;
    let arrayskip = g.arraystretch * baselineskip;
    let arstrut_height = 0.7 * arrayskip;
    let arstrut_depth = 0.3 * arrayskip;

    let mut total_height = 0.0f64;
    let set_h_line_pos = |hlines_in_gap: Option<&Vec<bool>>,
                          total_height: &mut f64,
                          hlines: &mut Vec<(f64, bool)>| {
        if let Some(gap) = hlines_in_gap {
            for (i, dashed) in gap.iter().enumerate() {
                if i > 0 {
                    *total_height += 0.25;
                }
                hlines.push((*total_height, *dashed));
            }
        }
    };
    set_h_line_pos(g.h_lines_before_row.first(), &mut total_height, &mut hlines);

    let mut body: Vec<Outrow> = Vec::with_capacity(nr);
    for (r, inrow) in g.body.iter().enumerate() {
        let mut height = arstrut_height;
        let mut depth = arstrut_depth;
        if nc < inrow.len() {
            nc = inrow.len();
        }
        let mut cells = Vec::with_capacity(inrow.len());
        for cell in inrow {
            let elt = build_group(Some(cell), options, None)?;
            if depth < elt.depth() {
                depth = elt.depth();
            }
            if height < elt.height() {
                height = elt.height();
            }
            cells.push(Some(elt));
        }
        let mut gap = 0.0;
        if let Some(Some(row_gap)) = g.row_gaps.get(r) {
            gap = calculate_size(row_gap, options)?;
            if gap > 0.0 {
                gap += arstrut_depth;
                if depth < gap {
                    depth = gap;
                }
                gap = 0.0;
            }
        }
        if g.add_jot == Some(true) && r + 1 < nr {
            depth += jot;
        }
        total_height += height;
        let pos = total_height;
        total_height += depth + gap;
        body.push(Outrow {
            cells,
            height,
            depth,
            pos,
        });
        set_h_line_pos(
            g.h_lines_before_row.get(r + 1),
            &mut total_height,
            &mut hlines,
        );
    }

    let offset = total_height / 2.0 + fm.axis_height;
    let empty_cols = Vec::new();
    let col_descriptions: &Vec<AlignSpec> = g.cols.as_ref().unwrap_or(&empty_cols);
    let mut cols: Vec<HtmlNode> = Vec::new();
    let mut tag_spans: Vec<VListElem> = Vec::new();

    if let Some(tags) = &g.tags {
        let any = tags.iter().any(|t| !matches!(t, RowTag::Auto(false)));
        if any {
            for (r, rw) in body.iter().enumerate() {
                let shift = rw.pos - offset;
                let mut tag_span = match tags.get(r) {
                    Some(RowTag::Auto(true)) => {
                        make_span_s(vec!["eqn-num"], vec![], Some(options), CssStyle::new())
                    }
                    Some(RowTag::Auto(false)) => {
                        make_span_s(vec![], vec![], Some(options), CssStyle::new())
                    }
                    Some(RowTag::Explicit(t)) => make_span_s(
                        vec![],
                        build_expression(t, options, RealGroup::Yes, (None, None))?,
                        Some(options),
                        CssStyle::new(),
                    ),
                    None => return ierr("Cannot read properties of undefined (reading 'length')"),
                };
                tag_span.depth = rw.depth;
                tag_span.height = rw.height;
                tag_spans.push(VListElem::shifted(HtmlNode::span(tag_span), shift));
            }
        }
    }

    let arraycolsep_span = |width: f64| {
        let mut s = make_span_s(vec!["arraycolsep"], vec![], None, CssStyle::new());
        s.style.set(CssProp::Width, make_em(width));
        HtmlNode::span(s)
    };

    let mut c = 0usize;
    let mut col_descr_num = 0usize;
    while c < nc || col_descr_num < col_descriptions.len() {
        let mut col_descr = col_descriptions.get(col_descr_num);
        let mut first_separator = true;
        while let Some(AlignSpec::Separator { separator }) = col_descr {
            if !first_separator {
                cols.push(arraycolsep_span(fm.double_rule_sep));
            }
            if separator == "|" || separator == ":" {
                let line_type = if separator == "|" { "solid" } else { "dashed" };
                let mut sep = make_span_s(
                    vec!["vertical-separator"],
                    vec![],
                    Some(options),
                    CssStyle::new(),
                );
                sep.style.set(CssProp::Height, make_em(total_height));
                sep.style
                    .set(CssProp::BorderRightWidth, make_em(rule_thickness));
                sep.style
                    .set(CssProp::BorderRightStyle, CssVal::Static(line_type));
                sep.style.set(
                    CssProp::Margin,
                    CssVal::Str(format!("0 {}", make_em_str(-rule_thickness / 2.0)).into()),
                );
                let shift = total_height - offset;
                if shift != 0.0 && !shift.is_nan() {
                    sep.style.set(CssProp::VerticalAlign, make_em(-shift));
                }
                cols.push(HtmlNode::span(sep));
            } else {
                return perr(format!("Invalid separator type: {}", separator), None);
            }
            col_descr_num += 1;
            col_descr = col_descriptions.get(col_descr_num);
            first_separator = false;
        }

        if c < nc {
            let (align, pregap, postgap) = match col_descr {
                Some(AlignSpec::Align {
                    align,
                    pregap,
                    postgap,
                }) => (Some(align.as_str()), *pregap, *postgap),
                _ => (None, None, None),
            };
            if c > 0 || g.hskip_before_and_after == Some(true) {
                let sepwidth = pregap.unwrap_or(arraycolsep);
                if sepwidth != 0.0 {
                    cols.push(arraycolsep_span(sepwidth));
                }
            }
            let mut col_elems = Vec::new();
            for row in body.iter_mut() {
                let Some(Some(mut elem)) = row.cells.get_mut(c).map(Option::take) else {
                    continue;
                };
                let shift = row.pos - offset;
                elem.set_depth(row.depth);
                elem.set_height(row.height);
                col_elems.push(VListElem::shifted(elem, shift));
            }
            let col_vlist = make_vlist(VListParam::IndividualShift(col_elems))?;
            let class = col_align_class(align.filter(|a| !a.is_empty()).unwrap_or("c"))?;
            cols.push(make_span(
                vec![class],
                vec![HtmlNode::span(col_vlist)],
                None,
                CssStyle::new(),
            ));
            if c + 1 < nc || g.hskip_before_and_after == Some(true) {
                let sepwidth = postgap.unwrap_or(arraycolsep);
                if sepwidth != 0.0 {
                    cols.push(arraycolsep_span(sepwidth));
                }
            }
        }
        c += 1;
        col_descr_num += 1;
    }

    let mut table_body = make_span(vec!["mtable"], cols, None, CssStyle::new());

    // \hline(s)
    if !hlines.is_empty() {
        let line = make_line_span("katex-hline", options, Some(rule_thickness));
        let dashes = make_line_span("katex-hdashline", options, Some(rule_thickness));
        let mut elems = vec![VListElem::shifted(table_body, 0.0)];
        while let Some((pos, dashed)) = hlines.pop() {
            let line_shift = pos - offset;
            let l = if dashed { dashes.clone() } else { line.clone() };
            elems.push(VListElem::shifted(HtmlNode::span(l), line_shift));
        }
        table_body = HtmlNode::span(make_vlist(VListParam::IndividualShift(elems))?);
    }

    if tag_spans.is_empty() {
        Ok(make_span(
            vec!["mord"],
            vec![table_body],
            Some(options),
            CssStyle::new(),
        ))
    } else {
        let eqn_num_col = make_vlist(VListParam::IndividualShift(tag_spans))?;
        let tag_col = make_span(
            vec!["katex-tag"],
            vec![HtmlNode::span(eqn_num_col)],
            Some(options),
            CssStyle::new(),
        );
        Ok(make_fragment(vec![table_body, tag_col]))
    }
}

/// Column specs of `{array}`-like environments: the argument's symbols.
fn colspec_nodes(arg: &ParseNode) -> Result<&[ParseNode]> {
    if arg.is_symbol_node() {
        Ok(std::slice::from_ref(arg))
    } else if let ParseNode::OrdGroup(g) = arg {
        Ok(&g.body)
    } else {
        type_error("ordgroup", arg)
    }
}

fn array_handler(
    ctx: &mut EnvContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut cols = Vec::new();
    for nde in colspec_nodes(&args[0])? {
        let ca = assert_symbol_text(nde)?;
        if "lcr".contains(ca) {
            cols.push(AlignSpec::Align {
                align: ca.to_string(),
                pregap: None,
                postgap: None,
            });
        } else if ca == "|" || ca == ":" {
            cols.push(AlignSpec::Separator {
                separator: ca.to_string(),
            });
        } else {
            return perr(format!("Unknown column alignment: {}", ca), nde.loc());
        }
    }
    let n = cols.len();
    let opts = ArrayOpts {
        cols: Some(cols),
        hskip_before_and_after: Some(true),
        max_num_cols: Some(n),
        ..Default::default()
    };
    let style = d_cell_style(ctx.env_name);
    Ok(ParseNode::Array(Box::new(parse_array(
        ctx.parser, opts, style,
    )?)))
}

fn matrix_handler(
    ctx: &mut EnvContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let base = ctx.env_name.replace('*', "");
    let delimiters: Option<(&str, &str)> = match base.as_str() {
        "pmatrix" => Some(("(", ")")),
        "bmatrix" => Some(("[", "]")),
        "Bmatrix" => Some(("\\{", "\\}")),
        "vmatrix" => Some(("|", "|")),
        "Vmatrix" => Some(("\\Vert", "\\Vert")),
        _ => None,
    };
    let mut col_align = "c".to_string();
    let mut opts = ArrayOpts {
        hskip_before_and_after: Some(false),
        cols: Some(vec![AlignSpec::Align {
            align: col_align.clone(),
            pregap: None,
            postgap: None,
        }]),
        ..Default::default()
    };
    if ctx.env_name.ends_with('*') {
        let parser = &mut *ctx.parser;
        parser.consume_spaces()?;
        if parser.fetch()?.text == "[" {
            parser.consume();
            parser.consume_spaces()?;
            col_align = parser.fetch()?.text.clone();
            if !"lcr".contains(col_align.as_str()) {
                return perr("Expected l or c or r", next_token_loc(parser).as_ref());
            }
            parser.consume();
            parser.consume_spaces()?;
            parser.expect("]", true)?;
            parser.consume();
            opts.cols = Some(vec![AlignSpec::Align {
                align: col_align.clone(),
                pregap: None,
                postgap: None,
            }]);
        }
    }
    let mut res = parse_array(ctx.parser, opts, d_cell_style(ctx.env_name))?;
    let num_cols = res.body.iter().map(|r| r.len()).max().unwrap_or(0);
    res.cols = Some(vec![
        AlignSpec::Align {
            align: col_align,
            pregap: None,
            postgap: None
        };
        num_cols
    ]);
    let res = ParseNode::Array(Box::new(res));
    Ok(match delimiters {
        Some((l, r)) => ParseNode::LeftRight(Box::new(LeftRightNode {
            mode: ctx.mode,
            body: vec![res],
            left: l.to_string(),
            right: r.to_string(),
            right_color: None,
        })),
        None => res,
    })
}

fn smallmatrix_handler(
    ctx: &mut EnvContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let opts = ArrayOpts {
        arraystretch: Some(0.5),
        ..Default::default()
    };
    let mut res = parse_array(ctx.parser, opts, StyleStr::Script)?;
    res.col_separation_type = Some(ColSeparationType::Small);
    Ok(ParseNode::Array(Box::new(res)))
}

fn subarray_handler(
    ctx: &mut EnvContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut cols = Vec::new();
    for nde in colspec_nodes(&args[0])? {
        let ca = assert_symbol_text(nde)?;
        if "lc".contains(ca) {
            cols.push(AlignSpec::Align {
                align: ca.to_string(),
                pregap: None,
                postgap: None,
            });
        } else {
            return perr(format!("Unknown column alignment: {}", ca), nde.loc());
        }
    }
    if cols.len() > 1 {
        return perr("{subarray} can contain only one column", None);
    }
    let opts = ArrayOpts {
        cols: Some(cols),
        hskip_before_and_after: Some(false),
        arraystretch: Some(0.5),
        ..Default::default()
    };
    let res = parse_array(ctx.parser, opts, StyleStr::Script)?;
    if !res.body.is_empty() && res.body[0].len() > 1 {
        return perr("{subarray} can contain only one column", None);
    }
    Ok(ParseNode::Array(Box::new(res)))
}

fn cases_handler(
    ctx: &mut EnvContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let opts = ArrayOpts {
        arraystretch: Some(1.2),
        cols: Some(vec![
            AlignSpec::Align {
                align: "l".into(),
                pregap: Some(0.0),
                postgap: Some(1.0),
            },
            AlignSpec::Align {
                align: "l".into(),
                pregap: Some(0.0),
                postgap: Some(0.0),
            },
        ]),
        ..Default::default()
    };
    let res = parse_array(ctx.parser, opts, d_cell_style(ctx.env_name))?;
    let r = ctx.env_name.contains('r');
    Ok(ParseNode::LeftRight(Box::new(LeftRightNode {
        mode: ctx.mode,
        body: vec![ParseNode::Array(Box::new(res))],
        left: if r { "." } else { "\\{" }.to_string(),
        right: if r { "\\}" } else { "." }.to_string(),
        right_color: None,
    })))
}

/// `alignedHandler` (align, align*, aligned, split, alignat, alignat*, alignedat).
fn aligned_handler(
    ctx: &mut EnvContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    if !ctx.env_name.contains("ed") {
        validate_ams_environment_context(ctx)?;
    }
    let is_split = ctx.env_name == "split";
    let opts = ArrayOpts {
        cols: Some(Vec::new()),
        add_jot: Some(true),
        auto_tag: if is_split {
            None
        } else {
            get_auto_tag(ctx.env_name)
        },
        empty_single_row: true,
        col_separation_type: Some(if ctx.env_name.contains("at") {
            ColSeparationType::Alignat
        } else {
            ColSeparationType::Align
        }),
        max_num_cols: if is_split { Some(2) } else { None },
        leqno: Some(ctx.parser.settings.leqno),
        ..Default::default()
    };
    let mut res = parse_array(ctx.parser, opts, StyleStr::Display)?;

    let mut num_maths = 0usize;
    let mut num_cols = 0usize;
    if let Some(ParseNode::OrdGroup(g)) = args.first() {
        let message = "Number of columns should be a positive integer";
        let num_columns = assert_character_group(g, message, false)?;
        let all_digits = !num_columns.is_empty() && num_columns.bytes().all(|b| b.is_ascii_digit());
        let value: f64 = if all_digits {
            num_columns.parse().unwrap_or(f64::INFINITY)
        } else {
            f64::NAN
        };
        if !all_digits || value < 1.0 {
            return perr(message, g.loc.as_ref());
        }
        // Absurd counts would allocate absurd column lists; KaTeX would too (and hang).
        num_maths = if value > 1e6 {
            1_000_000
        } else {
            value as usize
        };
        num_cols = num_maths * 2;
    }
    let is_aligned = num_cols == 0;
    for row in res.body.iter_mut() {
        let mut i = 1;
        while i < row.len() {
            let ParseNode::Styling { body, .. } = &mut row[i] else {
                return type_error("styling", &row[i]);
            };
            let first = &mut body[0];
            let ParseNode::OrdGroup(og) = first else {
                return type_error("ordgroup", first);
            };
            og.body.insert(0, ParseNode::ordgroup(ctx.mode, vec![]));
            i += 2;
        }
        if !is_aligned {
            let cur_maths = row.len() as f64 / 2.0;
            if (num_maths as f64) < cur_maths {
                return perr(
                    format!(
                        "Too many math in a row: expected {}, but got {}",
                        num_maths,
                        js_num(cur_maths)
                    ),
                    row[0].loc(),
                );
            }
        } else if num_cols < row.len() {
            num_cols = row.len();
        }
    }
    let mut cols = Vec::with_capacity(num_cols);
    for i in 0..num_cols {
        let mut align = "r";
        let mut pregap = 0.0;
        if i % 2 == 1 {
            align = "l";
        } else if i > 0 && is_aligned {
            pregap = 1.0; // one \quad
        }
        cols.push(AlignSpec::Align {
            align: align.into(),
            pregap: Some(pregap),
            postgap: Some(0.0),
        });
    }
    res.cols = Some(cols);
    res.col_separation_type = Some(if is_aligned {
        ColSeparationType::Align
    } else {
        ColSeparationType::Alignat
    });
    Ok(ParseNode::Array(Box::new(res)))
}

fn gathered_handler(
    ctx: &mut EnvContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    if ctx.env_name == "gather" || ctx.env_name == "gather*" {
        validate_ams_environment_context(ctx)?;
    }
    let opts = ArrayOpts {
        cols: Some(vec![AlignSpec::Align {
            align: "c".into(),
            pregap: None,
            postgap: None,
        }]),
        add_jot: Some(true),
        col_separation_type: Some(ColSeparationType::Gather),
        auto_tag: get_auto_tag(ctx.env_name),
        empty_single_row: true,
        leqno: Some(ctx.parser.settings.leqno),
        ..Default::default()
    };
    Ok(ParseNode::Array(Box::new(parse_array(
        ctx.parser,
        opts,
        StyleStr::Display,
    )?)))
}

fn equation_handler(
    ctx: &mut EnvContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    validate_ams_environment_context(ctx)?;
    let opts = ArrayOpts {
        auto_tag: get_auto_tag(ctx.env_name),
        empty_single_row: true,
        single_row: true,
        max_num_cols: Some(1),
        leqno: Some(ctx.parser.settings.leqno),
        ..Default::default()
    };
    Ok(ParseNode::Array(Box::new(parse_array(
        ctx.parser,
        opts,
        StyleStr::Display,
    )?)))
}

fn cd_handler(
    ctx: &mut EnvContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    validate_ams_environment_context(ctx)?;
    Ok(ParseNode::Array(Box::new(parse_cd(ctx.parser)?)))
}

pub static ENV_SPECS: &[EnvSpec] = &[
    EnvSpec {
        node_type: "array",
        names: &["array", "darray"],
        num_args: 1,
        arg_types: &[],
        handler: array_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &[
            "matrix", "pmatrix", "bmatrix", "Bmatrix", "vmatrix", "Vmatrix", "matrix*", "pmatrix*",
            "bmatrix*", "Bmatrix*", "vmatrix*", "Vmatrix*",
        ],
        num_args: 0,
        arg_types: &[],
        handler: matrix_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["smallmatrix"],
        num_args: 0,
        arg_types: &[],
        handler: smallmatrix_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["subarray"],
        num_args: 1,
        arg_types: &[],
        handler: subarray_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["cases", "dcases", "rcases", "drcases"],
        num_args: 0,
        arg_types: &[],
        handler: cases_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["align", "align*", "aligned", "split"],
        num_args: 0,
        arg_types: &[],
        handler: aligned_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["gathered", "gather", "gather*"],
        num_args: 0,
        arg_types: &[],
        handler: gathered_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["alignat", "alignat*", "alignedat"],
        num_args: 1,
        arg_types: &[],
        handler: aligned_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["equation", "equation*"],
        num_args: 0,
        arg_types: &[],
        handler: equation_handler,
    },
    EnvSpec {
        node_type: "array",
        names: &["CD"],
        num_args: 0,
        arg_types: &[],
        handler: cd_handler,
    },
];

fn hline_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    perr(
        format!("{} valid only within array environment", ctx.func_name),
        None,
    )
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "text",
    names: &["\\hline", "\\hdashline"],
    num_args: 0,
    allowed_in_text: true,
    allowed_in_math: true,
    handler: Some(hline_handler),
    ..FunctionSpec::DEFAULT
}];

#[cfg(test)]
mod tests {
    use super::js_parse_float;

    #[test]
    fn parse_float_like_js() {
        assert_eq!(js_parse_float("1.5"), 1.5);
        assert_eq!(js_parse_float("  2em"), 2.0);
        assert_eq!(js_parse_float(".5"), 0.5);
        assert_eq!(js_parse_float("-1"), -1.0);
        assert_eq!(js_parse_float("3."), 3.0);
        assert_eq!(js_parse_float("1e2x"), 100.0);
        assert!(js_parse_float("x").is_nan());
        assert!(js_parse_float(".").is_nan());
        assert_eq!(js_parse_float("Infinity"), f64::INFINITY);
    }
}

//! Port of KaTeX `src/functions/delimsizing.ts`: `\big…`, `\left`, `\right`, `\middle`.

use crate::build_common::make_span;
use crate::build_html::{RealGroup, build_expression, make_null_delimiter};
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::delimiter::{make_left_right_delim, make_sized_delim};
use crate::dom_tree::{CssStyle, HtmlNode, IsMiddle};
use crate::macros::MacroDef;
use crate::options::Options;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{LeftRightNode, ParseNode, normalize_argument};

/// `delimiterSizes[funcName]` → (mclass, size).
fn delimiter_size(name: &str) -> Option<(&'static str, u8)> {
    Some(match name {
        "\\bigl" => ("mopen", 1),
        "\\Bigl" => ("mopen", 2),
        "\\biggl" => ("mopen", 3),
        "\\Biggl" => ("mopen", 4),
        "\\bigr" => ("mclose", 1),
        "\\Bigr" => ("mclose", 2),
        "\\biggr" => ("mclose", 3),
        "\\Biggr" => ("mclose", 4),
        "\\bigm" => ("mrel", 1),
        "\\Bigm" => ("mrel", 2),
        "\\biggm" => ("mrel", 3),
        "\\Biggm" => ("mrel", 4),
        "\\big" => ("mord", 1),
        "\\Big" => ("mord", 2),
        "\\bigg" => ("mord", 3),
        "\\Bigg" => ("mord", 4),
        _ => return None,
    })
}

/// `delimiters`.
fn is_delimiter(d: &str) -> bool {
    matches!(
        d,
        "(" | "\\lparen"
            | ")"
            | "\\rparen"
            | "["
            | "\\lbrack"
            | "]"
            | "\\rbrack"
            | "\\{"
            | "\\lbrace"
            | "\\}"
            | "\\rbrace"
            | "\\lfloor"
            | "\\rfloor"
            | "\u{230a}"
            | "\u{230b}"
            | "\\lceil"
            | "\\rceil"
            | "\u{2308}"
            | "\u{2309}"
            | "<"
            | ">"
            | "\\langle"
            | "\u{27e8}"
            | "\\rangle"
            | "\u{27e9}"
            | "\\lt"
            | "\\gt"
            | "\\lvert"
            | "\\rvert"
            | "\\lVert"
            | "\\rVert"
            | "\\lgroup"
            | "\\rgroup"
            | "\u{27ee}"
            | "\u{27ef}"
            | "\\lmoustache"
            | "\\rmoustache"
            | "\u{23b0}"
            | "\u{23b1}"
            | "/"
            | "\\backslash"
            | "|"
            | "\\vert"
            | "\\|"
            | "\\Vert"
            | "\\uparrow"
            | "\\Uparrow"
            | "\\downarrow"
            | "\\Downarrow"
            | "\\updownarrow"
            | "\\Updownarrow"
            | "."
    )
}

/// `checkDelimiter(delim, context)`: the delimiter's text.
fn check_delimiter(delim: &ParseNode, func_name: &str) -> Result<String> {
    match delim.symbol_text() {
        Some(t) if is_delimiter(t) => Ok(t.to_string()),
        Some(t) => perr(
            format!("Invalid delimiter '{}' after '{}'", t, func_name),
            delim.loc(),
        ),
        None => perr(
            format!("Invalid delimiter type '{}'", delim.type_name()),
            delim.loc(),
        ),
    }
}

fn delimsizing_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let arg = normalize_argument(args.into_iter().next().unwrap());
    let delim = check_delimiter(&arg, ctx.func_name)?;
    let (mclass, size) = delimiter_size(ctx.func_name).unwrap_or(("mord", 1));
    Ok(ParseNode::DelimSizing {
        mode: ctx.parser.mode,
        size,
        mclass,
        delim,
    })
}

pub fn html_builder_delimsizing(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::DelimSizing {
        mode,
        size,
        mclass,
        delim,
    } = group
    else {
        unreachable!()
    };
    if delim == "." {
        return Ok(make_span(vec![mclass], vec![], None, CssStyle::new()));
    }
    Ok(HtmlNode::span(make_sized_delim(
        delim,
        *size,
        options,
        *mode,
        vec![mclass],
    )?))
}

fn right_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let color = match ctx.parser.gullet.macros.get("\\current@color") {
        None => None,
        Some(MacroDef::Str(src)) => Some(src.as_str().to_string()),
        Some(_) => return perr("\\current@color set to non-string in \\right", None),
    };
    let delim = check_delimiter(&args[0], ctx.func_name)?;
    // An empty colour string is falsy in KaTeX (`rightColor ? … : options`); keep it as is.
    Ok(ParseNode::LeftRightRight {
        mode: ctx.parser.mode,
        delim,
        color,
    })
}

fn left_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let delim = check_delimiter(&args[0], ctx.func_name)?;
    let parser = &mut *ctx.parser;
    parser.leftright_depth += 1;
    let body = parser.parse_expression(false, None)?;
    parser.leftright_depth -= 1;
    parser.expect("\\right", false)?;
    // `parser.parseFunction()`: the next token is `\right`, a function, so `parseGroup`
    // (whose "atom" name skips the argument check, as an absent name does) reaches the same
    // `parseFunction` call.
    let right = parser.parse_group("atom", None)?;
    let (right_delim, right_color) = match right {
        Some(ParseNode::LeftRightRight { delim, color, .. }) => (delim, color),
        Some(other) => {
            return ierr(format!(
                "Expected node of type leftright-right, but got node of type {}",
                other.type_name()
            ));
        }
        None => return ierr("Expected node of type leftright-right, but got null"),
    };
    Ok(ParseNode::LeftRight(Box::new(LeftRightNode {
        mode: parser.mode,
        body,
        left: delim,
        right: right_delim,
        right_color,
    })))
}

pub fn html_builder_leftright(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::LeftRight(g) = group else {
        unreachable!()
    };
    let mut inner = build_expression(
        &g.body,
        options,
        RealGroup::Yes,
        (Some("mopen"), Some("mclose")),
    )?;
    let mut inner_height = 0.0f64;
    let mut inner_depth = 0.0f64;
    let mut had_middle = false;
    for node in &inner {
        if is_middle(node) {
            had_middle = true;
        } else {
            inner_height = node.height().max(inner_height);
            inner_depth = node.depth().max(inner_depth);
        }
    }
    inner_height *= options.size_multiplier;
    inner_depth *= options.size_multiplier;
    let left_delim = if g.left == "." {
        make_null_delimiter(options, vec!["mopen"])
    } else {
        HtmlNode::span(make_left_right_delim(
            &g.left,
            inner_height,
            inner_depth,
            options,
            g.mode,
            vec!["mopen"],
        )?)
    };
    inner.insert(0, left_delim);
    if had_middle {
        for node in inner.iter_mut().skip(1) {
            let mid = match node {
                HtmlNode::Span(s) => s.is_middle.take(),
                _ => None,
            };
            if let Some(m) = mid {
                *node = HtmlNode::span(make_left_right_delim(
                    &m.delim,
                    inner_height,
                    inner_depth,
                    &m.options,
                    g.mode,
                    vec![],
                )?);
            }
        }
    }
    let right_delim = if g.right == "." {
        make_null_delimiter(options, vec!["mclose"])
    } else {
        let color_options = match g.right_color.as_deref() {
            Some(c) if !c.is_empty() => options.with_color(c),
            _ => options.clone(),
        };
        HtmlNode::span(make_left_right_delim(
            &g.right,
            inner_height,
            inner_depth,
            &color_options,
            g.mode,
            vec!["mclose"],
        )?)
    };
    inner.push(right_delim);
    Ok(make_span(
        vec!["minner"],
        inner,
        Some(options),
        CssStyle::new(),
    ))
}

/// `isMiddleDelimNode(node)`.
fn is_middle(node: &HtmlNode) -> bool {
    matches!(node, HtmlNode::Span(s) if s.is_middle.is_some())
}

fn middle_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let delim = check_delimiter(&args[0], ctx.func_name)?;
    if ctx.parser.leftright_depth == 0 {
        return perr("\\middle without preceding \\left", args[0].loc());
    }
    Ok(ParseNode::Middle {
        mode: ctx.parser.mode,
        delim,
    })
}

pub fn html_builder_middle(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Middle { mode, delim } = group else {
        unreachable!()
    };
    if delim == "." {
        return Ok(make_null_delimiter(options, vec![]));
    }
    let mut span = make_sized_delim(delim, 1, options, *mode, vec![])?;
    span.is_middle = Some(Box::new(IsMiddle {
        delim: delim.clone(),
        options: options.clone(),
    }));
    Ok(HtmlNode::span(span))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "delimsizing",
        names: &[
            "\\bigl", "\\Bigl", "\\biggl", "\\Biggl", "\\bigr", "\\Bigr", "\\biggr", "\\Biggr",
            "\\bigm", "\\Bigm", "\\biggm", "\\Biggm", "\\big", "\\Big", "\\bigg", "\\Bigg",
        ],
        num_args: 1,
        arg_types: &[ArgType::Primitive],
        handler: Some(delimsizing_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "leftright-right",
        names: &["\\right"],
        num_args: 1,
        primitive: true,
        handler: Some(right_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "leftright",
        names: &["\\left"],
        num_args: 1,
        primitive: true,
        handler: Some(left_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "middle",
        names: &["\\middle"],
        num_args: 1,
        primitive: true,
        handler: Some(middle_handler),
        ..FunctionSpec::DEFAULT
    },
];

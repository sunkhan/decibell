//! Port of KaTeX `src/functions/text.ts`.

use crate::build_common::make_span;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, ordargument};

/// `optionsWithFont(group, options)`.
fn options_with_font(font: Option<&str>, options: &Options) -> Options {
    let Some(font) = font else {
        return options.clone();
    };
    match font {
        // textFontFamilies (`\text` maps to undefined: falls through to the shape lookup).
        "\\textrm" | "\\textnormal" => options.with_text_font_family("textrm"),
        "\\textsf" => options.with_text_font_family("textsf"),
        "\\texttt" => options.with_text_font_family("texttt"),
        "\\textbf" => options.with_text_font_weight("textbf"),
        "\\textmd" => options.with_text_font_weight("textmd"),
        "\\emph" => {
            if options.font_shape == "textit" {
                options.with_text_font_shape("textup")
            } else {
                options.with_text_font_shape("textit")
            }
        }
        "\\textit" => options.with_text_font_shape("textit"),
        "\\textup" => options.with_text_font_shape("textup"),
        // `textFontShapes[font]` is undefined (e.g. "\\text"): the shape becomes "".
        _ => options.with_text_font_shape(""),
    }
}

/// The interned `funcName` stored as the node's `font`.
fn intern_font(name: &str) -> Option<&'static str> {
    NAMES.iter().copied().find(|n| *n == name)
}

const NAMES: [&str; 10] = [
    "\\text",
    "\\textrm",
    "\\textsf",
    "\\texttt",
    "\\textnormal",
    "\\textbf",
    "\\textmd",
    "\\textit",
    "\\textup",
    "\\emph",
];

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::Text {
        mode: ctx.parser.mode,
        body: ordargument(body),
        font: intern_font(ctx.func_name),
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Text { body, font, .. } = group else {
        unreachable!()
    };
    let new_options = options_with_font(*font, options);
    let inner = build_expression(body, &new_options, RealGroup::Yes, (None, None))?;
    Ok(make_span(
        vec!["mord", "text"],
        inner,
        Some(&new_options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "text",
    names: &NAMES,
    num_args: 1,
    arg_types: &[ArgType::Text],
    allowed_in_argument: true,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

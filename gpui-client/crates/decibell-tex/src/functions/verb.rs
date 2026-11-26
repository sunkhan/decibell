//! Port of KaTeX `src/functions/verb.ts` (the `\verb` token is produced by the lexer; the
//! function only fires when the delimiter never closes on the line).

use crate::build_common::{make_span, make_symbol, try_combine_chars};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::fonts::FontName;
use crate::options::Options;
use crate::parse_error::{Result, perr};
use crate::parse_node::ParseNode;

fn handler(
    _ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    perr(
        "\\verb ended by end of line instead of matching delimiter",
        None,
    )
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Verb { mode, body, star } = group else {
        unreachable!()
    };
    // makeVerb: spaces become U+2423 (starred) or U+00A0.
    let text = body.replace(' ', if *star { "\u{2423}" } else { "\u{a0}" });
    let new_options = options.having_style(options.style.text());
    let mut nodes = Vec::new();
    for c in text.chars() {
        let s = if c == '~' {
            "\\textasciitilde".to_string()
        } else {
            c.to_string()
        };
        nodes.push(HtmlNode::Symbol(Box::new(make_symbol(
            &s,
            FontName::TypewriterRegular,
            *mode,
            Some(&new_options),
            vec!["mord", "texttt"],
        ))));
    }
    try_combine_chars(&mut nodes);
    let mut classes = vec!["mord", "text"];
    classes.extend(new_options.sizing_classes(options));
    Ok(make_span(
        classes,
        nodes,
        Some(&new_options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "verb",
    names: &["\\verb"],
    num_args: 0,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

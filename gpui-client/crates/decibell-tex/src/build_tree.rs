//! Port of KaTeX `src/buildTree.ts`, `src/parseTree.ts` and the error path of `katex.ts`
//! (`renderToDomTree` / `renderError`), HTML output only.

use std::rc::Rc;

use crate::build_common::make_span_s;
use crate::build_html::build_html;
use crate::dom_tree::{CssStyle, HtmlNode, Span, SymbolNode};
use crate::options::Options;
use crate::parse_error::{Error, Result, perr};
use crate::parse_node::{Mode, ParseNode};
use crate::parser::Parser;
use crate::settings::Settings;
use crate::style::Style;
use crate::token::{Src, Token};

/// `parseTree(toParse, settings)`.
pub fn parse_tree(input: &str, settings: &Settings) -> Result<Vec<ParseNode>> {
    let mut parser = Parser::new(Src::Shared(Rc::from(input)), settings);
    parser.gullet.macros.current.remove("\\df@tag");
    let mut tree = parser.parse()?;
    parser.gullet.macros.current.remove("\\current@color");
    parser.gullet.macros.current.remove("\\color");
    if parser.gullet.macros.get("\\df@tag").is_some() {
        if !settings.display_mode {
            return perr("\\tag works only in display equations", None);
        }
        let tag = parser.subparse(vec![Token::new("\\df@tag", None)])?;
        tree = vec![ParseNode::Tag {
            mode: Mode::Text,
            body: tree,
            tag,
        }];
    }
    Ok(tree)
}

/// `optionsFromSettings(settings)`.
pub fn options_from_settings(settings: &Settings) -> Options {
    Options::new(
        if settings.display_mode {
            Style::DISPLAY
        } else {
            Style::TEXT
        },
        settings.max_size,
        settings.min_rule_thickness,
    )
}

/// `buildHTMLTree` (`.katex` span, display wrapper) minus the MathML half.
pub fn build_html_tree(tree: &[ParseNode], settings: &Settings) -> Result<Span> {
    let options = options_from_settings(settings);
    let html = build_html(tree, &options)?;
    let katex = make_span_s(
        vec!["katex"],
        vec![HtmlNode::span(html)],
        None,
        CssStyle::new(),
    );
    if settings.display_mode {
        let mut classes = vec!["katex-display"];
        if settings.leqno {
            classes.push("leqno");
        }
        if settings.fleqn {
            classes.push("fleqn");
        }
        Ok(make_span_s(
            classes,
            vec![HtmlNode::span(katex)],
            None,
            CssStyle::new(),
        ))
    } else {
        Ok(katex)
    }
}

/// The result of `renderToDomTree` with `throwOnError: false`.
pub enum Rendered {
    Tree(Span),
    /// A ParseError: KaTeX renders `<span class="katex-error" title=… style="color:…">tex</span>`.
    ParseError {
        message: String,
    },
}

/// `renderToDomTree(expression, options)`: `Err` is a non-ParseError KaTeX would rethrow.
pub fn render_to_dom_tree(tex: &str, settings: &Settings) -> std::result::Result<Rendered, String> {
    let r = parse_tree(tex, settings).and_then(|tree| build_html_tree(&tree, settings));
    match r {
        Ok(span) => Ok(Rendered::Tree(span)),
        Err(Error::Parse(p)) => Ok(Rendered::ParseError {
            message: p.to_js_string(),
        }),
        Err(Error::Internal(m)) => Err(m),
    }
}

/// `renderError` as markup.
pub fn error_markup(tex: &str, message: &str, color: &str) -> String {
    let mut node = make_span_s(
        vec!["katex-error"],
        vec![HtmlNode::Symbol(Box::new(SymbolNode::new(
            tex,
            0.0,
            0.0,
            0.0,
            0.0,
            0.0,
            vec![],
            CssStyle::new(),
        )))],
        None,
        CssStyle::new(),
    );
    node.set_attribute("title", message);
    node.set_attribute("style", format!("color:{}", color));
    let mut out = String::new();
    HtmlNode::span(node).to_markup(&mut out);
    out
}

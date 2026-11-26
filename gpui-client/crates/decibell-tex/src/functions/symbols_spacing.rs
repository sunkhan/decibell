//! Port of KaTeX `src/functions/symbolsSpacing.ts`.

use crate::build_common::{OrdType, make_ord, make_span, mathsym};
use crate::define_function::FunctionSpec;
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::{Result, perr};
use crate::parse_node::{Mode, ParseNode};

pub static SPECS: &[FunctionSpec] = &[];

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Spacing(n) = group else {
        unreachable!()
    };
    // regularSpace
    let regular: Option<&'static str> = match n.text.as_str() {
        " " | "\\ " | "\\space" => Some(""),
        "~" | "\\nobreakspace" => Some("nobreak"),
        _ => None,
    };
    let css_space: Option<&'static str> = match n.text.as_str() {
        "\\nobreak" => Some("nobreak"),
        "\\allowbreak" => Some("allowbreak"),
        _ => None,
    };
    if let Some(class_name) = regular {
        if n.mode == Mode::Text {
            let mut ord = make_ord(&n.text, n.mode, OrdType::TextOrd, options)?;
            if let Some(c) = ord.classes_mut() {
                c.push(class_name);
            }
            Ok(ord)
        } else {
            let sym = mathsym(&n.text, n.mode, options, vec![])?;
            Ok(make_span(
                vec!["mspace", class_name],
                vec![HtmlNode::Symbol(Box::new(sym))],
                Some(options),
                CssStyle::new(),
            ))
        }
    } else if let Some(c) = css_space {
        Ok(make_span(
            vec!["mspace", c],
            vec![],
            Some(options),
            CssStyle::new(),
        ))
    } else {
        perr(format!("Unknown type of space \"{}\"", n.text), None)
    }
}

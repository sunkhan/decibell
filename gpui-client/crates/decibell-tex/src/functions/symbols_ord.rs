//! Port of KaTeX `src/functions/symbolsOrd.ts`.

use crate::build_common::{OrdType, make_ord};
use crate::define_function::FunctionSpec;
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;

pub static SPECS: &[FunctionSpec] = &[];

pub fn html_builder_mathord(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::MathOrd(n) = group else {
        unreachable!()
    };
    make_ord(&n.text, n.mode, OrdType::MathOrd, options)
}

pub fn html_builder_textord(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::TextOrd(n) = group else {
        unreachable!()
    };
    make_ord(&n.text, n.mode, OrdType::TextOrd, options)
}

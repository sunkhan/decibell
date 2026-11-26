//! Port of KaTeX `src/functions/symbolsOp.ts`.

use crate::build_common::mathsym;
use crate::define_function::FunctionSpec;
use crate::dom_tree::HtmlNode;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{Atom, ParseNode};

pub static SPECS: &[FunctionSpec] = &[];

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Atom(n) = group else {
        unreachable!()
    };
    let class = match n.family {
        Atom::Bin => "mbin",
        Atom::Close => "mclose",
        Atom::Inner => "minner",
        Atom::Open => "mopen",
        Atom::Punct => "mpunct",
        Atom::Rel => "mrel",
    };
    Ok(HtmlNode::Symbol(Box::new(mathsym(
        &n.text,
        n.mode,
        options,
        vec![class],
    )?)))
}

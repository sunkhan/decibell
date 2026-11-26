//! Port of KaTeX `src/functions/ordgroup.ts`.

use crate::build_common::{make_fragment, make_span};
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::FunctionSpec;
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::ParseNode;

pub static SPECS: &[FunctionSpec] = &[];

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::OrdGroup(g) = group else {
        unreachable!()
    };
    if g.semisimple {
        return Ok(make_fragment(build_expression(
            &g.body,
            options,
            RealGroup::No,
            (None, None),
        )?));
    }
    Ok(make_span(
        vec!["mord"],
        build_expression(&g.body, options, RealGroup::Yes, (None, None))?,
        Some(options),
        CssStyle::new(),
    ))
}

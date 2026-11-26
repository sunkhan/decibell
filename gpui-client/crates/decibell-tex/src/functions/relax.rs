//! Port of KaTeX `src/functions/relax.ts`.

use crate::define_function::{FunctionContext, FunctionSpec};
use crate::parse_error::Result;
use crate::parse_node::ParseNode;

fn handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(ParseNode::Internal {
        mode: ctx.parser.mode,
    })
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "internal",
    names: &["\\relax"],
    num_args: 0,
    allowed_in_text: true,
    allowed_in_argument: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

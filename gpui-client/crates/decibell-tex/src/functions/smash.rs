//! Port of KaTeX `src/functions/smash.ts`.

use crate::build_common::{VListChild, VListParam, make_span, make_span_s, make_vlist};
use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, assert_symbol_text, type_error};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    opt_args: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut smash_height = false;
    let mut smash_depth = false;
    match opt_args.into_iter().next().flatten() {
        Some(o) => {
            let ParseNode::OrdGroup(tb) = &o else {
                return type_error("ordgroup", &o);
            };
            for node in &tb.body {
                let letter = assert_symbol_text(node)?;
                if letter == "t" {
                    smash_height = true;
                } else if letter == "b" {
                    smash_depth = true;
                } else {
                    smash_height = false;
                    smash_depth = false;
                    break;
                }
            }
        }
        None => {
            smash_height = true;
            smash_depth = true;
        }
    }
    let body = args.into_iter().next().unwrap();
    Ok(ParseNode::Smash {
        mode: ctx.parser.mode,
        body: Box::new(body),
        smash_height,
        smash_depth,
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Smash {
        body,
        smash_height,
        smash_depth,
        ..
    } = group
    else {
        unreachable!()
    };
    let mut node = make_span_s(
        vec![],
        vec![build_group(Some(body), options, None)?],
        None,
        CssStyle::new(),
    );
    if !smash_height && !smash_depth {
        return Ok(HtmlNode::span(node));
    }
    if *smash_height {
        node.height = 0.0;
    }
    if *smash_depth {
        node.depth = 0.0;
    }
    if *smash_height && *smash_depth {
        return Ok(make_span(
            vec!["mord", "katex-smash"],
            vec![HtmlNode::span(node)],
            Some(options),
            CssStyle::new(),
        ));
    }
    for child in node.children.iter_mut() {
        if *smash_height {
            child.set_height(0.0);
        }
        if *smash_depth {
            child.set_depth(0.0);
        }
    }
    let smashed = make_vlist(VListParam::FirstBaseline(vec![VListChild::elem(
        HtmlNode::span(node),
    )]))?;
    Ok(make_span(
        vec!["mord"],
        vec![HtmlNode::span(smashed)],
        Some(options),
        CssStyle::new(),
    ))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "smash",
    names: &["\\smash"],
    num_args: 1,
    num_optional_args: 1,
    allowed_in_text: true,
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];

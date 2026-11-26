//! Port of KaTeX `src/functions/mclass.ts` (HTML side).

use crate::build_common::make_span;
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{Atom, OpNode, ParseNode, SupSubNode, ordargument};
use crate::utils::is_character_box;

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::MClass { mclass, body, .. } = group else {
        unreachable!()
    };
    let elements = build_expression(body, options, RealGroup::Yes, (None, None))?;
    Ok(make_span(
        vec![*mclass],
        elements,
        Some(options),
        CssStyle::new(),
    ))
}

/// `binrelClass(arg)`.
pub fn binrel_class(arg: &ParseNode) -> &'static str {
    let atom = match arg {
        ParseNode::OrdGroup(g) if !g.body.is_empty() => &g.body[0],
        other => other,
    };
    match atom {
        ParseNode::Atom(a) if a.family == Atom::Bin => "mbin",
        ParseNode::Atom(a) if a.family == Atom::Rel => "mrel",
        _ => "mord",
    }
}

fn mclass_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    let mclass = match ctx.func_name {
        "\\mathord" => "mord",
        "\\mathbin" => "mbin",
        "\\mathrel" => "mrel",
        "\\mathopen" => "mopen",
        "\\mathclose" => "mclose",
        "\\mathpunct" => "mpunct",
        _ => "minner",
    };
    let is_character_box = is_character_box(&body);
    Ok(ParseNode::MClass {
        mode: ctx.parser.mode,
        mclass,
        body: ordargument(body),
        is_character_box,
    })
}

fn binrel_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let a0 = it.next().unwrap();
    let a1 = it.next().unwrap();
    let mclass = binrel_class(&a0);
    let is_character_box = is_character_box(&a1);
    Ok(ParseNode::MClass {
        mode: ctx.parser.mode,
        mclass,
        body: ordargument(a1),
        is_character_box,
    })
}

fn stackrel_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let shifted_arg = it.next().unwrap();
    let base_arg = it.next().unwrap();
    let mclass = if ctx.func_name != "\\stackrel" {
        binrel_class(&base_arg)
    } else {
        "mrel"
    };
    let base_op = ParseNode::Op(Box::new(OpNode {
        mode: base_arg.mode(),
        limits: true,
        always_handle_sup_sub: true,
        parent_is_sup_sub: false,
        symbol: false,
        suppress_base_shift: ctx.func_name != "\\stackrel",
        name: None,
        body: Some(ordargument(base_arg)),
    }));
    let shifted_mode = shifted_arg.mode();
    let supsub = if ctx.func_name == "\\underset" {
        ParseNode::SupSub(SupSubNode {
            mode: shifted_mode,
            base: Some(Box::new(base_op)),
            sup: None,
            sub: Some(Box::new(shifted_arg)),
        })
    } else {
        ParseNode::SupSub(SupSubNode {
            mode: shifted_mode,
            base: Some(Box::new(base_op)),
            sup: Some(Box::new(shifted_arg)),
            sub: None,
        })
    };
    let is_character_box = is_character_box(&supsub);
    Ok(ParseNode::MClass {
        mode: ctx.parser.mode,
        mclass,
        body: vec![supsub],
        is_character_box,
    })
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "mclass",
        names: &[
            "\\mathord",
            "\\mathbin",
            "\\mathrel",
            "\\mathopen",
            "\\mathclose",
            "\\mathpunct",
            "\\mathinner",
        ],
        num_args: 1,
        primitive: true,
        handler: Some(mclass_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "mclass",
        names: &["\\@binrel"],
        num_args: 2,
        handler: Some(binrel_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "mclass",
        names: &["\\stackrel", "\\overset", "\\underset"],
        num_args: 2,
        handler: Some(stackrel_handler),
        ..FunctionSpec::DEFAULT
    },
];

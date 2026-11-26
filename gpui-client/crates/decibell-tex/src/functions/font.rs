//! Port of KaTeX `src/functions/font.ts` (HTML side).

use crate::build_html::build_group;
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::HtmlNode;
use crate::functions::mclass::binrel_class;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{OrdGroupNode, ParseNode, normalize_argument};
use crate::utils::is_character_box;

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Font { font, body, .. } = group else {
        unreachable!()
    };
    let new_options = options.with_font(font);
    build_group(Some(body), &new_options, None)
}

fn font_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = normalize_argument(args.into_iter().next().unwrap());
    // fontAliases, then `func.slice(1)`.
    let font = match ctx.func_name {
        "\\Bbb" => "mathbb",
        "\\bold" => "mathbf",
        "\\frak" => "mathfrak",
        "\\mathrm" => "mathrm",
        "\\mathit" => "mathit",
        "\\mathbf" => "mathbf",
        "\\mathnormal" => "mathnormal",
        "\\mathsfit" => "mathsfit",
        "\\mathbb" => "mathbb",
        "\\mathcal" => "mathcal",
        "\\mathfrak" => "mathfrak",
        "\\mathscr" => "mathscr",
        "\\mathsf" => "mathsf",
        _ => "mathtt",
    };
    Ok(ParseNode::Font {
        mode: ctx.parser.mode,
        font,
        body: Box::new(body),
    })
}

fn boldsymbol_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    let mclass = binrel_class(&body);
    let is_character_box = is_character_box(&body);
    let mode = ctx.parser.mode;
    Ok(ParseNode::MClass {
        mode,
        mclass,
        body: vec![ParseNode::Font {
            mode,
            font: "boldsymbol",
            body: Box::new(body),
        }],
        is_character_box,
    })
}

fn old_font_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mode = ctx.parser.mode;
    let body = ctx.parser.parse_expression(true, ctx.break_on_token_text)?;
    let font = match ctx.func_name {
        "\\rm" => "mathrm",
        "\\sf" => "mathsf",
        "\\tt" => "mathtt",
        "\\bf" => "mathbf",
        "\\it" => "mathit",
        _ => "mathcal",
    };
    Ok(ParseNode::Font {
        mode,
        font,
        body: Box::new(ParseNode::OrdGroup(OrdGroupNode {
            mode: ctx.parser.mode,
            body,
            semisimple: false,
            loc: None,
        })),
    })
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "font",
        names: &[
            "\\mathrm",
            "\\mathit",
            "\\mathbf",
            "\\mathnormal",
            "\\mathsfit",
            "\\mathbb",
            "\\mathcal",
            "\\mathfrak",
            "\\mathscr",
            "\\mathsf",
            "\\mathtt",
            "\\Bbb",
            "\\bold",
            "\\frak",
        ],
        num_args: 1,
        allowed_in_argument: true,
        handler: Some(font_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "mclass",
        names: &["\\boldsymbol", "\\bm"],
        num_args: 1,
        handler: Some(boldsymbol_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "font",
        names: &["\\rm", "\\sf", "\\tt", "\\bf", "\\it", "\\cal"],
        num_args: 0,
        allowed_in_text: true,
        handler: Some(old_font_handler),
        ..FunctionSpec::DEFAULT
    },
];

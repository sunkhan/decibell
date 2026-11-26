//! Port of KaTeX `src/functions/def.ts`: `\def`, `\gdef`, `\edef`, `\xdef`, `\let`,
//! `\futurelet`, and the `\global` / `\long` prefixes.

use std::rc::Rc;

use crate::define_function::{FunctionContext, FunctionSpec};
use crate::macros::{MacroDef, MacroExpansion};
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::ParseNode;
use crate::parser::Parser;
use crate::token::Token;

/// `globalMap`.
fn global_map(name: &str) -> Option<&'static str> {
    Some(match name {
        "\\global" => "\\global",
        "\\long" => "\\\\globallong",
        "\\\\globallong" => "\\\\globallong",
        "\\def" => "\\gdef",
        "\\gdef" => "\\gdef",
        "\\edef" => "\\xdef",
        "\\xdef" => "\\xdef",
        "\\let" => "\\\\globallet",
        "\\futurelet" => "\\\\globalfuture",
        _ => return None,
    })
}

/// `/^(?:[\\{}$&#^_]|EOF)$/.test(name)`.
fn is_not_control_sequence(name: &str) -> bool {
    matches!(name, "\\" | "{" | "}" | "$" | "&" | "#" | "^" | "_" | "EOF")
}

/// `checkControlSequence(tok)`.
fn check_control_sequence(tok: &Token) -> Result<String> {
    if is_not_control_sequence(&tok.text) {
        return perr("Expected a control sequence", tok.loc.as_ref());
    }
    Ok(tok.text.clone())
}

/// `getRHS(parser)`.
fn get_rhs(parser: &mut Parser) -> Result<Token> {
    let mut tok = parser.gullet.pop_token()?;
    if tok.text == "=" {
        tok = parser.gullet.pop_token()?;
        if tok.text == " " {
            tok = parser.gullet.pop_token()?;
        }
    }
    Ok(tok)
}

/// `letCommand(parser, name, tok, global)`. Marks `tok` noexpand when it has no macro
/// definition (KaTeX mutates the token object; `\futurelet` pushes it back afterwards).
fn let_command(parser: &mut Parser, name: &str, tok: &mut Token, global: bool) {
    let mac = match parser.gullet.macros.get(&tok.text) {
        Some(m) => m,
        None => {
            tok.noexpand = true;
            let unexpandable = !parser.gullet.is_expandable(&tok.text);
            MacroDef::Expansion(Rc::new(MacroExpansion {
                tokens: vec![tok.clone()],
                num_args: 0,
                delimiters: None,
                unexpandable,
            }))
        }
    };
    parser.gullet.macros.set(name, Some(mac), global);
}

fn internal(ctx: &FunctionContext) -> ParseNode {
    ParseNode::Internal {
        mode: ctx.parser.mode,
    }
}

fn global_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    ctx.parser.consume_spaces()?;
    let token = ctx.parser.fetch()?.clone();
    if let Some(mapped) = global_map(&token.text) {
        let prefix = ctx.func_name == "\\global" || ctx.func_name == "\\\\globallong";
        if let (true, Some(t)) = (prefix, ctx.parser.next_token.as_mut()) {
            t.text = mapped.to_string();
        }
        // `parser.parseFunction()`: the fetched token is a function, so parseGroup("atom")
        // takes exactly that path.
        let node = ctx.parser.parse_group("atom", None)?;
        return match node {
            Some(n @ ParseNode::Internal { .. }) => Ok(n),
            Some(other) => ierr(format!(
                "Expected node of type internal, but got node of type {}",
                other.type_name()
            )),
            None => ierr("Expected node of type internal, but got null"),
        };
    }
    perr("Invalid token after macro prefix", token.loc.as_ref())
}

fn def_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let gullet = &mut ctx.parser.gullet;
    let mut tok = gullet.pop_token()?;
    let name = tok.text.clone();
    if is_not_control_sequence(&name) {
        return perr("Expected a control sequence", tok.loc.as_ref());
    }
    let mut num_args = 0usize;
    let mut insert: Option<Token> = None;
    let mut delimiters: Vec<Vec<String>> = vec![vec![]];
    while gullet.future()?.text != "{" {
        tok = gullet.pop_token()?;
        if tok.text == "#" {
            if gullet.future()?.text == "{" {
                insert = Some(gullet.future()?.clone());
                delimiters[num_args].push("{".to_string());
                break;
            }
            tok = gullet.pop_token()?;
            let b = tok.text.as_bytes();
            if !(b.len() == 1 && (b'1'..=b'9').contains(&b[0])) {
                return perr(format!("Invalid argument number \"{}\"", tok.text), None);
            }
            if (b[0] - b'0') as usize != num_args + 1 {
                return perr(
                    format!("Argument number \"{}\" out of order", tok.text),
                    None,
                );
            }
            num_args += 1;
            delimiters.push(vec![]);
        } else if tok.text == "EOF" {
            return perr("Expected a macro definition", None);
        } else {
            delimiters[num_args].push(tok.text.clone());
        }
    }
    let mut tokens = gullet.consume_arg(None)?.tokens;
    if let Some(ins) = insert {
        tokens.insert(0, ins);
    }
    if ctx.func_name == "\\edef" || ctx.func_name == "\\xdef" {
        tokens = ctx.parser.gullet.expand_tokens(tokens)?;
        tokens.reverse();
    }
    let global = global_map(ctx.func_name) == Some(ctx.func_name);
    ctx.parser.gullet.macros.set(
        &name,
        Some(MacroDef::Expansion(Rc::new(MacroExpansion {
            tokens,
            num_args,
            delimiters: Some(delimiters),
            unexpandable: false,
        }))),
        global,
    );
    Ok(internal(ctx))
}

fn let_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let t = ctx.parser.gullet.pop_token()?;
    let name = check_control_sequence(&t)?;
    ctx.parser.gullet.consume_spaces()?;
    let mut tok = get_rhs(ctx.parser)?;
    let global = ctx.func_name == "\\\\globallet";
    let_command(ctx.parser, &name, &mut tok, global);
    Ok(internal(ctx))
}

fn futurelet_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let t = ctx.parser.gullet.pop_token()?;
    let name = check_control_sequence(&t)?;
    let middle = ctx.parser.gullet.pop_token()?;
    let mut tok = ctx.parser.gullet.pop_token()?;
    let global = ctx.func_name == "\\\\globalfuture";
    let_command(ctx.parser, &name, &mut tok, global);
    ctx.parser.gullet.push_token(tok);
    ctx.parser.gullet.push_token(middle);
    Ok(internal(ctx))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "internal",
        names: &["\\global", "\\long", "\\\\globallong"],
        num_args: 0,
        allowed_in_text: true,
        handler: Some(global_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "internal",
        names: &["\\def", "\\gdef", "\\edef", "\\xdef"],
        num_args: 0,
        allowed_in_text: true,
        primitive: true,
        handler: Some(def_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "internal",
        names: &["\\let", "\\\\globallet"],
        num_args: 0,
        allowed_in_text: true,
        primitive: true,
        handler: Some(let_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "internal",
        names: &["\\futurelet", "\\\\globalfuture"],
        num_args: 0,
        allowed_in_text: true,
        primitive: true,
        handler: Some(futurelet_handler),
        ..FunctionSpec::DEFAULT
    },
];

//! Port of KaTeX `src/macros.ts` and `src/defineMacro.ts`.
//!
//! String-valued built-ins live in `data/macros_data.rs` (generated, evaluated values). The
//! function-valued ones are [`BuiltinMacro::Fn`] entries, implemented here by name in
//! [`call_builtin`].

use std::rc::Rc;

use crate::data::macros_data;
use crate::macro_expander::MacroExpander;
use crate::parse_error::{Result, perr};
use crate::parse_node::Mode;
use crate::symbols::{self, Group};
use crate::token::{Src, Token};

/// A built-in macro table entry.
#[derive(Clone, Copy, Debug)]
pub enum BuiltinMacro {
    Str(&'static str),
    /// Function-valued: implemented by name in [`call_builtin`].
    Fn,
}

/// `MacroExpansion`: tokens in reverse order.
#[derive(Clone, Debug)]
pub struct MacroExpansion {
    pub tokens: Vec<Token>,
    pub num_args: usize,
    pub delimiters: Option<Vec<Vec<String>>>,
    pub unexpandable: bool,
}

/// `MacroDefinition`.
#[derive(Clone, Debug)]
pub enum MacroDef {
    /// A string body, lexed on every expansion.
    Str(Src),
    Expansion(Rc<MacroExpansion>),
    /// A built-in function macro, by name.
    Fn(&'static str),
    /// The `|` / `\|` closures `braketHelper` installs.
    BraketMid(Rc<BraketMid>),
}

#[derive(Debug)]
pub struct BraketMid {
    pub one: bool,
    pub double: bool,
    pub middle: Vec<Token>,
    pub middle_double: Vec<Token>,
    pub old_middle: Option<MacroDef>,
    pub old_middle_double: Option<MacroDef>,
}

impl MacroDef {
    pub fn from_builtin(entry: (&'static str, &'static BuiltinMacro)) -> MacroDef {
        match entry.1 {
            BuiltinMacro::Str(s) => MacroDef::Str(Src::Static(s)),
            BuiltinMacro::Fn => MacroDef::Fn(entry.0),
        }
    }
}

/// What a function macro returns: a string to lex, or tokens.
pub enum FnResult {
    Str(String),
    StaticStr(&'static str),
    Expansion(MacroExpansion),
}

/// The built-in macro `name`, with its interned name.
pub fn builtin(name: &str) -> Option<(&'static str, &'static BuiltinMacro)> {
    let t = macros_data::BUILTIN;
    t.binary_search_by(|(k, _)| k.as_bytes().cmp(name.as_bytes()))
        .ok()
        .map(|i| (t[i].0, &t[i].1))
}

fn expansion(tokens: Vec<Token>) -> FnResult {
    FnResult::Expansion(MacroExpansion {
        tokens,
        num_args: 0,
        delimiters: None,
        unexpandable: false,
    })
}

fn digit_to_number(s: &str) -> Option<u32> {
    let mut it = s.chars();
    let c = it.next()?;
    if it.next().is_some() {
        return None;
    }
    c.to_digit(16)
}

fn dots_by_token(next: &str) -> Option<&'static str> {
    Some(match next {
        "," => "\\dotsc",
        "\\not"
        | "+"
        | "="
        | "<"
        | ">"
        | "-"
        | "*"
        | ":"
        | "\\DOTSB"
        | "\\coprod"
        | "\\bigvee"
        | "\\bigwedge"
        | "\\biguplus"
        | "\\bigcap"
        | "\\bigcup"
        | "\\prod"
        | "\\sum"
        | "\\bigotimes"
        | "\\bigoplus"
        | "\\bigodot"
        | "\\bigsqcup"
        | "\\And"
        | "\\longrightarrow"
        | "\\Longrightarrow"
        | "\\longleftarrow"
        | "\\Longleftarrow"
        | "\\longleftrightarrow"
        | "\\Longleftrightarrow"
        | "\\mapsto"
        | "\\longmapsto"
        | "\\hookrightarrow"
        | "\\doteq"
        | "\\mathbin"
        | "\\mathrel"
        | "\\relbar"
        | "\\Relbar"
        | "\\xrightarrow"
        | "\\xleftarrow" => "\\dotsb",
        "\\DOTSI" | "\\int" | "\\oint" | "\\iint" | "\\iiint" | "\\iiiint" | "\\idotsint" => {
            "\\dotsi"
        }
        "\\DOTSX" => "\\dotsx",
        _ => return None,
    })
}

fn space_after_dots(next: &str) -> bool {
    matches!(
        next,
        ")" | "]"
            | "\\rbrack"
            | "\\}"
            | "\\rbrace"
            | "\\rangle"
            | "\\rceil"
            | "\\rfloor"
            | "\\rgroup"
            | "\\rmoustache"
            | "\\right"
            | "\\bigr"
            | "\\biggr"
            | "\\Bigr"
            | "\\Biggr"
            | "$"
            | ";"
            | "."
            | ","
    )
}

/// Runs the function macro `name`.
pub fn call_builtin(name: &str, ctx: &mut MacroExpander) -> Result<FnResult> {
    match name {
        "\\noexpand" => {
            let mut t = ctx.pop_token()?;
            if ctx.is_expandable(&t.text) {
                t.noexpand = true;
                t.treat_as_relax = true;
            }
            Ok(expansion(vec![t]))
        }
        "\\expandafter" => {
            let t = ctx.pop_token()?;
            ctx.expand_once(true)?;
            Ok(expansion(vec![t]))
        }
        "\\@firstoftwo" => {
            let mut args = ctx.consume_args(2, None)?;
            args.truncate(1);
            Ok(expansion(args.pop().unwrap()))
        }
        "\\@secondoftwo" => {
            let mut args = ctx.consume_args(2, None)?;
            Ok(expansion(args.pop().unwrap()))
        }
        "\\@ifnextchar" => {
            let mut args = ctx.consume_args(3, None)?;
            ctx.consume_spaces()?;
            let next = ctx.future()?.text.clone();
            let else_ = args.pop().unwrap();
            let then = args.pop().unwrap();
            if args[0].len() == 1 && args[0][0].text == next {
                Ok(expansion(then))
            } else {
                Ok(expansion(else_))
            }
        }
        "\\TextOrMath" => {
            let mut args = ctx.consume_args(2, None)?;
            if ctx.mode == Mode::Text {
                args.truncate(1);
                Ok(expansion(args.pop().unwrap()))
            } else {
                Ok(expansion(args.pop().unwrap()))
            }
        }
        "\\char" => {
            let mut token = ctx.pop_token()?;
            let mut base = 0u32;
            let mut number: u32 = 0;
            if token.text == "'" {
                base = 8;
                token = ctx.pop_token()?;
            } else if token.text == "\"" {
                base = 16;
                token = ctx.pop_token()?;
            } else if token.text == "`" {
                token = ctx.pop_token()?;
                let mut units = token.text.encode_utf16();
                if token.text.starts_with('\\') {
                    units.next();
                    number = units.next().map_or(u32::MAX, u32::from);
                } else if token.text == "EOF" {
                    return perr("\\char` missing argument", None);
                } else {
                    number = units.next().map_or(u32::MAX, u32::from);
                }
            } else {
                base = 10;
            }
            if base != 0 {
                match digit_to_number(&token.text) {
                    Some(d) if d < base => number = d,
                    _ => return perr(format!("Invalid base-{} digit {}", base, token.text), None),
                }
                loop {
                    let next = ctx.future()?.text.clone();
                    match digit_to_number(&next) {
                        Some(d) if d < base => {
                            number = number.wrapping_mul(base).wrapping_add(d);
                            ctx.pop_token()?;
                        }
                        _ => break,
                    }
                }
            }
            // JS prints NaN for "\\char`" followed by a one-unit-short control symbol.
            let num = if number == u32::MAX {
                "NaN".to_string()
            } else {
                number.to_string()
            };
            Ok(FnResult::Str(format!("\\@char{{{}}}", num)))
        }
        "\\newcommand" => newcommand(ctx, false, true, false),
        "\\renewcommand" => newcommand(ctx, true, false, false),
        "\\providecommand" => newcommand(ctx, true, true, true),
        "\\message" | "\\errmessage" => {
            ctx.consume_args(1, None)?;
            Ok(FnResult::StaticStr(""))
        }
        "\\show" => {
            ctx.pop_token()?;
            Ok(FnResult::StaticStr(""))
        }
        "\\dots" => {
            let next = ctx.expand_after_future()?.text.clone();
            let mut thedots = "\\dotso";
            if let Some(d) = dots_by_token(&next) {
                thedots = d;
            } else if next.starts_with("\\not") {
                thedots = "\\dotsb";
            } else if let Some(info) = symbols::get(Mode::Math, &next)
                && matches!(info.group, Group::Bin | Group::Rel)
            {
                thedots = "\\dotsb";
            }
            Ok(FnResult::StaticStr(thedots))
        }
        "\\dotso" => {
            let next = &ctx.future()?.text;
            Ok(FnResult::StaticStr(if space_after_dots(next) {
                "\\ldots\\,"
            } else {
                "\\ldots"
            }))
        }
        "\\dotsc" => {
            let next = &ctx.future()?.text;
            Ok(FnResult::StaticStr(
                if space_after_dots(next) && next != "," {
                    "\\ldots\\,"
                } else {
                    "\\ldots"
                },
            ))
        }
        "\\cdots" => {
            let next = &ctx.future()?.text;
            Ok(FnResult::StaticStr(if space_after_dots(next) {
                "\\@cdots\\,"
            } else {
                "\\@cdots"
            }))
        }
        "\\tag@literal" => {
            if ctx.macros.get("\\df@tag").is_some() {
                return perr("Multiple \\tag", None);
            }
            Ok(FnResult::StaticStr("\\gdef\\df@tag{\\text{#1}}"))
        }
        "\\bra@ket" => braket_helper(ctx, false),
        "\\bra@set" => braket_helper(ctx, true),
        _ => Err(crate::parse_error::Error::Internal(format!(
            "function macro {} is not ported",
            name
        ))),
    }
}

fn newcommand(
    ctx: &mut MacroExpander,
    exists_ok: bool,
    nonexists_ok: bool,
    skip_if_exists: bool,
) -> Result<FnResult> {
    let mut arg = ctx.consume_arg(None)?.tokens;
    if arg.len() != 1 {
        return perr("\\newcommand's first argument must be a macro name", None);
    }
    let name = arg[0].text.clone();
    let exists = ctx.is_defined(&name);
    if exists && !exists_ok {
        return perr(
            format!(
                "\\newcommand{{{}}} attempting to redefine {}; use \\renewcommand",
                name, name
            ),
            None,
        );
    }
    if !exists && !nonexists_ok {
        return perr(
            format!(
                "\\renewcommand{{{}}} when command {} does not yet exist; use \\newcommand",
                name, name
            ),
            None,
        );
    }
    let mut num_args = 0usize;
    arg = ctx.consume_arg(None)?.tokens;
    if arg.len() == 1 && arg[0].text == "[" {
        let mut arg_text = String::new();
        let mut token = ctx.expand_next_token()?;
        while token.text != "]" && token.text != "EOF" {
            arg_text.push_str(&token.text);
            token = ctx.expand_next_token()?;
        }
        // /^\s*[0-9]+\s*$/
        let trimmed = arg_text.trim_matches(|c: char| c.is_whitespace());
        if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
            return perr(format!("Invalid number of arguments: {}", arg_text), None);
        }
        num_args = trimmed.parse::<usize>().unwrap_or(usize::MAX);
        arg = ctx.consume_arg(None)?.tokens;
    }
    if !(exists && skip_if_exists) {
        ctx.macros.set(
            &name,
            Some(MacroDef::Expansion(Rc::new(MacroExpansion {
                tokens: arg,
                num_args,
                delimiters: None,
                unexpandable: false,
            }))),
            false,
        );
    }
    Ok(FnResult::StaticStr(""))
}

fn braket_helper(ctx: &mut MacroExpander, one: bool) -> Result<FnResult> {
    let left = ctx.consume_arg(None)?.tokens;
    let middle = ctx.consume_arg(None)?.tokens;
    let middle_double = ctx.consume_arg(None)?.tokens;
    let right = ctx.consume_arg(None)?.tokens;
    let old_middle = ctx.macros.get("|");
    let old_middle_double = ctx.macros.get("\\|");
    ctx.macros.begin_group();
    let has_double = !middle_double.is_empty();
    let mk = |double: bool| {
        MacroDef::BraketMid(Rc::new(BraketMid {
            one,
            double,
            middle: middle.clone(),
            middle_double: middle_double.clone(),
            old_middle: old_middle.clone(),
            old_middle_double: old_middle_double.clone(),
        }))
    };
    ctx.macros.set("|", Some(mk(false)), false);
    if has_double {
        ctx.macros.set("\\|", Some(mk(true)), false);
    }
    let arg = ctx.consume_arg(None)?.tokens;
    let mut input = right;
    input.extend(arg);
    input.extend(left);
    let mut expanded = ctx.expand_tokens(input)?;
    ctx.macros.end_group()?;
    expanded.reverse();
    Ok(expansion(expanded))
}

/// Runs a `braketHelper` `midMacro` closure.
pub fn call_braket_mid(mid: &BraketMid, ctx: &mut MacroExpander) -> Result<FnResult> {
    if mid.one {
        ctx.macros.set("|", mid.old_middle.clone(), false);
        if !mid.middle_double.is_empty() {
            ctx.macros.set("\\|", mid.old_middle_double.clone(), false);
        }
    }
    let mut doubled = mid.double;
    if !mid.double && !mid.middle_double.is_empty() {
        let next = &ctx.future()?.text;
        if next == "|" {
            ctx.pop_token()?;
            doubled = true;
        }
    }
    Ok(expansion(if doubled {
        mid.middle_double.clone()
    } else {
        mid.middle.clone()
    }))
}

//! Port of KaTeX `src/MacroExpander.ts` (the "gullet").

use std::rc::Rc;

use crate::functions;
use crate::lexer::Lexer;
use crate::macros::{self, FnResult, MacroDef, MacroExpansion};
use crate::namespace::Namespace;
use crate::parse_error::{Error, ParseError, Result, perr};
use crate::parse_node::Mode;
use crate::settings::Settings;
use crate::symbols;
use crate::token::{Loc, Src, Token};
use crate::utils::utf16_len;

/// `implicitCommands`.
pub fn is_implicit_command(name: &str) -> bool {
    matches!(name, "^" | "_" | "\\limits" | "\\nolimits")
}

/// `MacroArg`.
pub struct MacroArg {
    pub tokens: Vec<Token>,
    pub start: Token,
    pub end: Token,
}

pub struct MacroExpander<'s> {
    pub settings: &'s Settings,
    pub expansion_count: u32,
    pub lexer: Lexer,
    pub macros: Namespace,
    /// Tokens in REVERSE order.
    pub stack: Vec<Token>,
    pub mode: Mode,
}

impl<'s> MacroExpander<'s> {
    pub fn new(input: Src, settings: &'s Settings, mode: Mode) -> MacroExpander<'s> {
        MacroExpander {
            settings,
            expansion_count: 0,
            lexer: Lexer::new(input),
            macros: Namespace::default(),
            stack: Vec::new(),
            mode,
        }
    }

    pub fn feed(&mut self, input: Src) {
        self.lexer = Lexer::new(input);
    }

    pub fn switch_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    pub fn begin_group(&mut self) {
        self.macros.begin_group();
    }

    pub fn end_group(&mut self) -> Result<()> {
        self.macros.end_group()
    }

    pub fn end_groups(&mut self) {
        self.macros.end_groups();
    }

    /// `future()`: the top token, lexing one if the stack is empty.
    pub fn future(&mut self) -> Result<&Token> {
        if self.stack.is_empty() {
            let t = self.lexer.lex()?;
            self.stack.push(t);
        }
        Ok(self.stack.last().unwrap())
    }

    pub fn pop_token(&mut self) -> Result<Token> {
        self.future()?;
        Ok(self.stack.pop().unwrap())
    }

    pub fn push_token(&mut self, token: Token) {
        self.stack.push(token);
    }

    pub fn push_tokens(&mut self, tokens: Vec<Token>) {
        self.stack.extend(tokens);
    }

    /// `scanArgument(isOptional)`.
    pub fn scan_argument(&mut self, is_optional: bool) -> Result<Option<Token>> {
        let (start, tokens, end);
        if is_optional {
            self.consume_spaces()?;
            if self.future()?.text != "[" {
                return Ok(None);
            }
            start = self.pop_token()?;
            let arg = self.consume_arg(Some(&["]".to_string()]))?;
            tokens = arg.tokens;
            end = arg.end;
        } else {
            let arg = self.consume_arg(None)?;
            tokens = arg.tokens;
            start = arg.start;
            end = arg.end;
        }
        self.push_token(Token::new("EOF", end.loc.clone()));
        self.push_tokens(tokens);
        Ok(Some(Token::new(
            "",
            Loc::range(start.loc.as_ref(), end.loc.as_ref()),
        )))
    }

    pub fn consume_spaces(&mut self) -> Result<()> {
        loop {
            if self.future()?.text == " " {
                self.stack.pop();
            } else {
                return Ok(());
            }
        }
    }

    /// `consumeArg(delims)`.
    pub fn consume_arg(&mut self, delims: Option<&[String]>) -> Result<MacroArg> {
        let mut tokens: Vec<Token> = Vec::new();
        let delims = delims.filter(|d| !d.is_empty());
        let is_delimited = delims.is_some();
        if !is_delimited {
            self.consume_spaces()?;
        }
        let start = self.future()?.clone();
        let mut depth: i64 = 0;
        let mut matched = 0usize;
        let mut tok;
        loop {
            tok = self.pop_token()?;
            tokens.push(tok.clone());
            if tok.text == "{" {
                depth += 1;
            } else if tok.text == "}" {
                depth -= 1;
                if depth == -1 {
                    return perr("Extra }", tok.loc.as_ref());
                }
            } else if tok.text == "EOF" {
                let expected = match delims {
                    Some(d) => d[matched].clone(),
                    None => "}".to_string(),
                };
                return perr(
                    format!(
                        "Unexpected end of input in a macro argument, expected '{}'",
                        expected
                    ),
                    tok.loc.as_ref(),
                );
            }
            if let Some(d) = delims {
                if (depth == 0 || (depth == 1 && d[matched] == "{")) && tok.text == d[matched] {
                    matched += 1;
                    if matched == d.len() {
                        let n = tokens.len();
                        tokens.truncate(n - matched);
                        break;
                    }
                } else {
                    matched = 0;
                }
            }
            if !(depth != 0 || is_delimited) {
                break;
            }
        }
        if start.text == "{" && tokens.last().map(|t| t.text == "}").unwrap_or(false) {
            tokens.pop();
            tokens.remove(0);
        }
        tokens.reverse();
        Ok(MacroArg {
            tokens,
            start,
            end: tok,
        })
    }

    /// `consumeArgs(numArgs, delimiters)`.
    pub fn consume_args(
        &mut self,
        num_args: usize,
        delimiters: Option<&[Vec<String>]>,
    ) -> Result<Vec<Vec<Token>>> {
        if let Some(delimiters) = delimiters {
            if delimiters.len() != num_args + 1 {
                return perr(
                    "The length of delimiters doesn't match the number of args!",
                    None,
                );
            }
            for d in &delimiters[0] {
                let tok = self.pop_token()?;
                if *d != tok.text {
                    return perr(
                        "Use of the macro doesn't match its definition",
                        tok.loc.as_ref(),
                    );
                }
            }
        }
        let mut args = Vec::with_capacity(num_args);
        for i in 0..num_args {
            let d = delimiters.map(|d| d[i + 1].as_slice());
            args.push(self.consume_arg(d)?.tokens);
        }
        Ok(args)
    }

    pub fn count_expansion(&mut self, amount: usize) -> Result<()> {
        self.expansion_count = self.expansion_count.saturating_add(amount as u32);
        if self.expansion_count > self.settings.max_expand {
            return perr(
                "Too many expansions: infinite loop or need to increase maxExpand setting",
                None,
            );
        }
        Ok(())
    }

    /// `expandOnce(expandableOnly)`: `None` for KaTeX's `false` (not expanded), else the
    /// number of tokens pushed.
    pub fn expand_once(&mut self, expandable_only: bool) -> Result<Option<usize>> {
        let top = self.pop_token()?;
        let name = top.text.clone();
        let expansion = if !top.noexpand {
            self.get_expansion(&name)?
        } else {
            None
        };
        let Some(expansion) = expansion.filter(|e| !(expandable_only && e.unexpandable)) else {
            if expandable_only && name.starts_with('\\') && !self.is_defined(&name) {
                // KaTeX: expansion == null && name[0] === "\\" && !isDefined(name)
                if self.get_expansion_is_none(&name)? {
                    return perr(format!("Undefined control sequence: {}", name), None);
                }
            }
            self.push_token(top);
            return Ok(None);
        };
        self.count_expansion(1)?;
        let mut tokens = expansion.tokens;
        let args = self.consume_args(expansion.num_args, expansion.delimiters.as_deref())?;
        if expansion.num_args > 0 {
            let mut i = tokens.len() as isize - 1;
            while i >= 0 {
                let iu = i as usize;
                if tokens[iu].text == "#" {
                    if iu == 0 {
                        return perr(
                            "Incomplete placeholder at end of macro body",
                            tokens[iu].loc.as_ref(),
                        );
                    }
                    i -= 1;
                    let iu = i as usize;
                    let t = &tokens[iu];
                    if t.text == "#" {
                        tokens.remove(iu + 1);
                    } else if t.text.len() == 1 && (b'1'..=b'9').contains(&t.text.as_bytes()[0]) {
                        let n = (t.text.as_bytes()[0] - b'0') as usize;
                        let Some(arg) = args.get(n - 1) else {
                            // JS splices in `...undefined` and throws a TypeError.
                            return Err(Error::Internal("args[n - 1] is not iterable".into()));
                        };
                        tokens.splice(iu..iu + 2, arg.iter().cloned());
                    } else {
                        return perr("Not a valid argument number", t.loc.as_ref());
                    }
                }
                i -= 1;
            }
        }
        let n = tokens.len();
        self.push_tokens(tokens);
        Ok(Some(n))
    }

    /// In `expandOnce`, KaTeX's undefined-control-sequence check tests `expansion == null`,
    /// i.e. the macro is not defined at all (an unexpandable one doesn't count).
    fn get_expansion_is_none(&mut self, name: &str) -> Result<bool> {
        Ok(self.macros.get(name).is_none())
    }

    pub fn expand_after_future(&mut self) -> Result<&Token> {
        self.expand_once(false)?;
        self.future()
    }

    pub fn expand_next_token(&mut self) -> Result<Token> {
        loop {
            if self.expand_once(false)?.is_none() {
                let mut token = self.stack.pop().unwrap();
                if token.treat_as_relax {
                    token.text = "\\relax".to_string();
                }
                return Ok(token);
            }
        }
    }

    pub fn expand_macro(&mut self, name: &str) -> Result<Option<Vec<Token>>> {
        if self.macros.has(name) {
            Ok(Some(self.expand_tokens(vec![Token::new(name, None)])?))
        } else {
            Ok(None)
        }
    }

    /// `expandTokens(tokens)`: input reversed, output forward.
    pub fn expand_tokens(&mut self, tokens: Vec<Token>) -> Result<Vec<Token>> {
        let mut output = Vec::new();
        let old_len = self.stack.len();
        self.push_tokens(tokens);
        while self.stack.len() > old_len {
            if self.expand_once(true)?.is_none() {
                let mut token = self.stack.pop().unwrap();
                if token.treat_as_relax {
                    token.noexpand = false;
                    token.treat_as_relax = false;
                }
                output.push(token);
            }
        }
        self.count_expansion(output.len())?;
        Ok(output)
    }

    pub fn expand_macro_as_text(&mut self, name: &str) -> Result<Option<String>> {
        Ok(self
            .expand_macro(name)?
            .map(|tokens| tokens.iter().map(|t| t.text.as_str()).collect()))
    }

    /// `_getExpansion(name)`.
    pub fn get_expansion(&mut self, name: &str) -> Result<Option<MacroExpansion>> {
        let Some(definition) = self.macros.get(name) else {
            return Ok(None);
        };
        if utf16_len(name) == 1
            && let Some(code) = self.lexer.catcode(name)
            && code != 13
        {
            return Ok(None);
        }
        let result = match definition {
            MacroDef::Str(src) => return Ok(Some(self.lex_body(src)?)),
            MacroDef::Expansion(e) => return Ok(Some((*e).clone())),
            MacroDef::Fn(fname) => macros::call_builtin(fname, self)?,
            MacroDef::BraketMid(mid) => macros::call_braket_mid(&mid, self)?,
        };
        match result {
            FnResult::Str(s) => Ok(Some(self.lex_body(Src::Shared(Rc::from(s)))?)),
            FnResult::StaticStr(s) => Ok(Some(self.lex_body(Src::Static(s))?)),
            FnResult::Expansion(e) => Ok(Some(e)),
        }
    }

    fn lex_body(&mut self, src: Src) -> Result<MacroExpansion> {
        let expansion = src.as_str();
        let mut num_args = 0;
        if expansion.contains('#') {
            let stripped = expansion.replace("##", "");
            while stripped.contains(&format!("#{}", num_args + 1)) {
                num_args += 1;
            }
        }
        let mut body_lexer = Lexer::new(src);
        let mut tokens = Vec::new();
        loop {
            let tok = body_lexer.lex()?;
            if tok.text == "EOF" {
                break;
            }
            tokens.push(tok);
        }
        tokens.reverse();
        Ok(MacroExpansion {
            tokens,
            num_args,
            delimiters: None,
            unexpandable: false,
        })
    }

    /// `isDefined(name)`.
    pub fn is_defined(&self, name: &str) -> bool {
        self.macros.has(name)
            || functions::get(name).is_some()
            || symbols::has(Mode::Math, name)
            || symbols::has(Mode::Text, name)
            || is_implicit_command(name)
    }

    /// `isExpandable(name)`.
    pub fn is_expandable(&self, name: &str) -> bool {
        if let Some(m) = self.macros.get(name) {
            return match m {
                MacroDef::Expansion(e) => !e.unexpandable,
                _ => true,
            };
        }
        functions::get(name).map(|f| !f.primitive).unwrap_or(false)
    }
}

/// Builds a ParseError with no location (KaTeX's `new ParseError(msg)`).
pub fn bare_error(msg: impl Into<String>) -> Error {
    Error::Parse(ParseError::new(msg, None))
}

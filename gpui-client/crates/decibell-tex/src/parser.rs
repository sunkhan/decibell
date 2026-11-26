//! Port of KaTeX `src/Parser.ts`.

use crate::define_function::{ArgType, BreakToken, FunctionContext};
use crate::functions;
use crate::macro_expander::{MacroExpander, is_implicit_command};
use crate::macros::MacroDef;
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::{
    AccentNode, AtomNode, Measurement, Mode, OrdGroupNode, ParseNode, SupSubNode, SymNode,
};
use crate::settings::Settings;
use crate::symbols::{self, Group};
use crate::token::{Loc, Src, Token};
use crate::units::valid_unit;
use crate::utils::first_code_unit;
use crate::{unicode_accents, unicode_sup_or_sub, unicode_symbols};

/// What `parseArguments` needs from a function or environment spec.
pub struct ArgSpec {
    pub num_args: usize,
    pub num_optional_args: usize,
    pub arg_types: &'static [ArgType],
    pub primitive: bool,
    pub is_sqrt: bool,
}

pub struct Parser<'s> {
    pub mode: Mode,
    pub gullet: MacroExpander<'s>,
    pub settings: &'s Settings,
    pub leftright_depth: i32,
    pub next_token: Option<Token>,
}

/// `Parser.endOfExpression`.
fn is_end_of_expression(text: &str) -> bool {
    matches!(text, "}" | "\\endgroup" | "\\end" | "\\right" | "&")
}

impl<'s> Parser<'s> {
    pub fn new(input: Src, settings: &'s Settings) -> Parser<'s> {
        Parser {
            mode: Mode::Math,
            gullet: MacroExpander::new(input, settings, Mode::Math),
            settings,
            leftright_depth: 0,
            next_token: None,
        }
    }

    /// `expect(text, consume)`.
    pub fn expect(&mut self, text: &str, consume: bool) -> Result<()> {
        let t = self.fetch()?;
        if t.text != text {
            let msg = format!("Expected '{}', got '{}'", text, t.text);
            let loc = t.loc.clone();
            return perr(msg, loc.as_ref());
        }
        if consume {
            self.consume();
        }
        Ok(())
    }

    pub fn consume(&mut self) {
        self.next_token = None;
    }

    /// `fetch()`: the next fully expanded token (not consumed).
    pub fn fetch(&mut self) -> Result<&Token> {
        if self.next_token.is_none() {
            self.next_token = Some(self.gullet.expand_next_token()?);
        }
        Ok(self.next_token.as_ref().unwrap())
    }

    fn fetch_text(&mut self) -> Result<String> {
        Ok(self.fetch()?.text.clone())
    }

    pub fn switch_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.gullet.switch_mode(mode);
    }

    /// `parse()`.
    pub fn parse(&mut self) -> Result<Vec<ParseNode>> {
        self.gullet.begin_group();
        if self.settings.color_is_text_color {
            self.gullet.macros.set(
                "\\color",
                Some(MacroDef::Str(Src::Static("\\textcolor"))),
                false,
            );
        }
        let r = (|| {
            let parse = self.parse_expression(false, None)?;
            self.expect("EOF", true)?;
            self.gullet.end_group()?;
            Ok(parse)
        })();
        self.gullet.end_groups();
        r
    }

    /// `subparse(tokens)`.
    pub fn subparse(&mut self, tokens: Vec<Token>) -> Result<Vec<ParseNode>> {
        let old = self.next_token.take();
        self.consume();
        self.gullet.push_token(Token::new("}", None));
        self.gullet.push_tokens(tokens);
        let parse = self.parse_expression(false, None)?;
        self.expect("}", true)?;
        self.next_token = old;
        Ok(parse)
    }

    /// `parseExpression(breakOnInfix, breakOnTokenText)`.
    pub fn parse_expression(
        &mut self,
        break_on_infix: bool,
        break_on_token_text: Option<BreakToken>,
    ) -> Result<Vec<ParseNode>> {
        let _depth = crate::parse_error::DepthGuard::enter()?;
        let mut body = Vec::new();
        loop {
            if self.mode == Mode::Math {
                self.consume_spaces()?;
            }
            let lex = self.fetch_text()?;
            if is_end_of_expression(&lex) {
                break;
            }
            if let Some(b) = break_on_token_text
                && lex == b
            {
                break;
            }
            if break_on_infix
                && let Some(f) = functions::get(&lex)
                && f.infix
            {
                break;
            }
            let atom = self.parse_atom(break_on_token_text)?;
            match atom {
                None => break,
                Some(ParseNode::Internal { .. }) => continue,
                Some(a) => body.push(a),
            }
        }
        if self.mode == Mode::Text {
            form_ligatures(&mut body);
        }
        self.handle_infix_nodes(body)
    }

    /// `handleInfixNodes(body)`.
    fn handle_infix_nodes(&mut self, mut body: Vec<ParseNode>) -> Result<Vec<ParseNode>> {
        let mut over_index: Option<usize> = None;
        let mut func_name = String::new();
        for (i, node) in body.iter().enumerate() {
            if let ParseNode::Infix {
                replace_with,
                token,
                ..
            } = node
            {
                if over_index.is_some() {
                    return perr(
                        "only one infix operator per group",
                        token.as_ref().and_then(|t| t.loc.as_ref()),
                    );
                }
                over_index = Some(i);
                func_name = replace_with.clone();
            }
        }
        let Some(over_index) = over_index.filter(|_| !func_name.is_empty()) else {
            return Ok(body);
        };
        let denom_body: Vec<ParseNode> = body.split_off(over_index + 1);
        let infix = body.pop().unwrap();
        let numer_body = body;
        let mode = self.mode;
        let numer = match numer_body.len() {
            1 if matches!(numer_body[0], ParseNode::OrdGroup(_)) => {
                numer_body.into_iter().next().unwrap()
            }
            _ => ParseNode::ordgroup(mode, numer_body),
        };
        let denom = match denom_body.len() {
            1 if matches!(denom_body[0], ParseNode::OrdGroup(_)) => {
                denom_body.into_iter().next().unwrap()
            }
            _ => ParseNode::ordgroup(mode, denom_body),
        };
        let node = if func_name == "\\\\abovefrac" {
            self.call_function(&func_name, vec![numer, infix, denom], vec![], None, None)?
        } else {
            self.call_function(&func_name, vec![numer, denom], vec![], None, None)?
        };
        Ok(vec![node])
    }

    /// `handleSupSubscript(name)`.
    fn handle_sup_subscript(&mut self, name: &str) -> Result<ParseNode> {
        let symbol_token = self.fetch()?.clone();
        self.consume();
        self.consume_spaces()?;
        let mut group;
        loop {
            group = self.parse_group(name, None)?;
            if !matches!(group, Some(ParseNode::Internal { .. })) {
                break;
            }
        }
        match group {
            Some(g) => Ok(g),
            None => perr(
                format!("Expected group after '{}'", symbol_token.text),
                symbol_token.loc.as_ref(),
            ),
        }
    }

    /// `formatUnsupportedCmd(text)`.
    pub fn format_unsupported_cmd(&self, text: &str) -> ParseNode {
        let body: Vec<ParseNode> = text
            .chars()
            .map(|c| ParseNode::textord(Mode::Text, c.to_string()))
            .collect();
        let text_node = ParseNode::Text {
            mode: self.mode,
            body,
            font: None,
        };
        ParseNode::Color {
            mode: self.mode,
            color: self.settings.error_color.clone(),
            body: vec![text_node],
        }
    }

    /// `parseAtom(breakOnTokenText)`.
    fn parse_atom(&mut self, break_on_token_text: Option<BreakToken>) -> Result<Option<ParseNode>> {
        let mut base = self.parse_group("atom", break_on_token_text)?;
        if matches!(base, Some(ParseNode::Internal { .. })) {
            return Ok(base);
        }
        if self.mode == Mode::Text {
            return Ok(base);
        }
        let mut superscript: Option<ParseNode> = None;
        let mut subscript: Option<ParseNode> = None;
        loop {
            self.consume_spaces()?;
            let lex = self.fetch()?.clone();
            if lex.text == "\\limits" || lex.text == "\\nolimits" {
                match &mut base {
                    Some(ParseNode::Op(op)) => {
                        op.limits = lex.text == "\\limits";
                        op.always_handle_sup_sub = true;
                    }
                    Some(ParseNode::OperatorName {
                        always_handle_sup_sub,
                        limits,
                        ..
                    }) => {
                        if *always_handle_sup_sub {
                            *limits = lex.text == "\\limits";
                        }
                    }
                    _ => {
                        return perr(
                            "Limit controls must follow a math operator",
                            lex.loc.as_ref(),
                        );
                    }
                }
                self.consume();
            } else if lex.text == "^" {
                if superscript.is_some() {
                    return perr("Double superscript", lex.loc.as_ref());
                }
                superscript = Some(self.handle_sup_subscript("superscript")?);
            } else if lex.text == "_" {
                if subscript.is_some() {
                    return perr("Double subscript", lex.loc.as_ref());
                }
                subscript = Some(self.handle_sup_subscript("subscript")?);
            } else if lex.text == "'" {
                if superscript.is_some() {
                    return perr("Double superscript", lex.loc.as_ref());
                }
                let prime = ParseNode::textord(self.mode, "\\prime");
                let mut primes = vec![prime.clone()];
                self.consume();
                while self.fetch()?.text == "'" {
                    primes.push(prime.clone());
                    self.consume();
                }
                if self.fetch()?.text == "^" {
                    primes.push(self.handle_sup_subscript("superscript")?);
                }
                superscript = Some(ParseNode::ordgroup(self.mode, primes));
            } else if let Some(first) = unicode_sup_or_sub::get(&lex.text) {
                let is_sub = unicode_sup_or_sub::is_sub(&lex.text);
                let mut subsup_tokens = vec![Token::new(first, None)];
                self.consume();
                loop {
                    let token = self.fetch_text()?;
                    let Some(t) = unicode_sup_or_sub::get(&token) else {
                        break;
                    };
                    if unicode_sup_or_sub::is_sub(&token) != is_sub {
                        break;
                    }
                    subsup_tokens.insert(0, Token::new(t, None));
                    self.consume();
                }
                let body = self.subparse(subsup_tokens)?;
                let g = ParseNode::ordgroup(Mode::Math, body);
                if is_sub {
                    subscript = Some(g);
                } else {
                    superscript = Some(g);
                }
            } else {
                break;
            }
        }
        if superscript.is_some() || subscript.is_some() {
            Ok(Some(ParseNode::SupSub(SupSubNode {
                mode: self.mode,
                base: base.map(Box::new),
                sup: superscript.map(Box::new),
                sub: subscript.map(Box::new),
            })))
        } else {
            Ok(base)
        }
    }

    /// `parseFunction(breakOnTokenText, name)`.
    fn parse_function(
        &mut self,
        break_on_token_text: Option<BreakToken>,
        name: Option<&str>,
    ) -> Result<Option<ParseNode>> {
        let token = self.fetch()?.clone();
        let Some((func, func_data)) = functions::lookup(&token.text) else {
            return Ok(None);
        };
        self.consume();
        if let Some(name) = name
            && name != "atom"
            && !func_data.allowed_in_argument
        {
            return perr(
                format!("Got function '{}' with no arguments as {}", func, name),
                token.loc.as_ref(),
            );
        }
        if self.mode == Mode::Text && !func_data.allowed_in_text {
            return perr(
                format!("Can't use function '{}' in text mode", func),
                token.loc.as_ref(),
            );
        } else if self.mode == Mode::Math && !func_data.allowed_in_math {
            return perr(
                format!("Can't use function '{}' in math mode", func),
                token.loc.as_ref(),
            );
        }
        let spec = ArgSpec {
            num_args: func_data.num_args,
            num_optional_args: func_data.num_optional_args,
            arg_types: func_data.arg_types,
            primitive: func_data.primitive,
            is_sqrt: func_data.node_type == "sqrt",
        };
        let (args, opt_args) = self.parse_arguments(func, &spec)?;
        Ok(Some(self.call_function(
            func,
            args,
            opt_args,
            Some(token),
            break_on_token_text,
        )?))
    }

    /// `callFunction(name, args, optArgs, token, breakOnTokenText)`.
    pub fn call_function(
        &mut self,
        name: &str,
        args: Vec<ParseNode>,
        opt_args: Vec<Option<ParseNode>>,
        token: Option<Token>,
        break_on_token_text: Option<BreakToken>,
    ) -> Result<ParseNode> {
        match functions::lookup(name) {
            Some((func_name, spec)) if spec.handler.is_some() => {
                let mut ctx = FunctionContext {
                    func_name,
                    parser: self,
                    token,
                    break_on_token_text,
                };
                (spec.handler.unwrap())(&mut ctx, args, opt_args)
            }
            _ => perr(format!("No function handler for {}", name), None),
        }
    }

    /// `parseArguments(func, funcData)`.
    pub fn parse_arguments(
        &mut self,
        func: &str,
        spec: &ArgSpec,
    ) -> Result<(Vec<ParseNode>, Vec<Option<ParseNode>>)> {
        let total = spec.num_args + spec.num_optional_args;
        let mut args = Vec::new();
        let mut opt_args: Vec<Option<ParseNode>> = Vec::new();
        for i in 0..total {
            let mut arg_type = spec.arg_types.get(i).copied();
            let is_optional = i < spec.num_optional_args;
            if (spec.primitive && arg_type.is_none())
                || (spec.is_sqrt && i == 1 && opt_args.first().map(|o| o.is_none()).unwrap_or(true))
            {
                arg_type = Some(ArgType::Primitive);
            }
            let arg = self.parse_group_of_type(
                &format!("argument to '{}'", func),
                arg_type,
                is_optional,
            )?;
            if is_optional {
                opt_args.push(arg);
            } else if let Some(arg) = arg {
                args.push(arg);
            } else {
                return perr("Null argument, please report this as a bug", None);
            }
        }
        Ok((args, opt_args))
    }

    /// `parseGroupOfType(name, type, optional)`.
    fn parse_group_of_type(
        &mut self,
        name: &str,
        arg_type: Option<ArgType>,
        optional: bool,
    ) -> Result<Option<ParseNode>> {
        match arg_type {
            Some(ArgType::Color) => self.parse_color_group(optional),
            Some(ArgType::Size) => self.parse_size_group(optional),
            Some(ArgType::Url) => self.parse_url_group(optional),
            Some(ArgType::Math) => Ok(self
                .parse_argument_group(optional, Some(Mode::Math))?
                .map(ParseNode::OrdGroup)),
            Some(ArgType::Text) => Ok(self
                .parse_argument_group(optional, Some(Mode::Text))?
                .map(ParseNode::OrdGroup)),
            Some(ArgType::HBox) => {
                let group = self.parse_argument_group(optional, Some(Mode::Text))?;
                Ok(group.map(|g| ParseNode::Styling {
                    mode: g.mode,
                    style: crate::parse_node::StyleStr::Text,
                    reset_font: true,
                    body: vec![ParseNode::OrdGroup(g)],
                }))
            }
            Some(ArgType::Raw) => {
                let token = self.parse_string_group(optional)?;
                Ok(token.map(|t| ParseNode::Raw {
                    mode: Mode::Text,
                    string: t.text,
                }))
            }
            Some(ArgType::Primitive) => {
                if optional {
                    return perr("A primitive argument cannot be optional", None);
                }
                let group = self.parse_group(name, None)?;
                match group {
                    Some(g) => Ok(Some(g)),
                    None => {
                        let t = self.fetch()?.clone();
                        perr(format!("Expected group as {}", name), t.loc.as_ref())
                    }
                }
            }
            Some(ArgType::Original) | None => Ok(self
                .parse_argument_group(optional, None)?
                .map(ParseNode::OrdGroup)),
        }
    }

    pub fn consume_spaces(&mut self) -> Result<()> {
        while self.fetch()?.text == " " {
            self.consume();
        }
        Ok(())
    }

    /// `parseStringGroup(optional)`.
    fn parse_string_group(&mut self, optional: bool) -> Result<Option<Token>> {
        let Some(mut arg_token) = self.gullet.scan_argument(optional)? else {
            return Ok(None);
        };
        let mut s = String::new();
        loop {
            let next = self.fetch()?;
            if next.text == "EOF" {
                break;
            }
            s.push_str(&next.text);
            self.consume();
        }
        self.consume();
        arg_token.text = s;
        Ok(Some(arg_token))
    }

    /// `parseRegexGroup(regex, modeName)` for the one regex KaTeX passes (sizes).
    fn parse_size_regex_group(&mut self, mode_name: &str) -> Result<Token> {
        let first = self.fetch()?.clone();
        let mut last = first.clone();
        let mut s = String::new();
        loop {
            let next = self.fetch()?;
            if next.text == "EOF" {
                break;
            }
            let candidate = format!("{}{}", s, next.text);
            if !size_prefix_matches(&candidate) {
                break;
            }
            last = next.clone();
            s = candidate;
            self.consume();
        }
        if s.is_empty() {
            return perr(
                format!("Invalid {}: '{}'", mode_name, first.text),
                first.loc.as_ref(),
            );
        }
        Ok(first.range(&last, s))
    }

    /// `parseColorGroup(optional)`.
    fn parse_color_group(&mut self, optional: bool) -> Result<Option<ParseNode>> {
        let Some(res) = self.parse_string_group(optional)? else {
            return Ok(None);
        };
        let t = res.text.as_str();
        let hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
        let valid = if let Some(h) = t.strip_prefix('#') {
            matches!(h.len(), 3 | 4 | 6 | 8) && hex(h)
        } else {
            (t.len() == 6 && hex(t))
                || (!t.is_empty() && t.bytes().all(|b| b.is_ascii_alphabetic()))
        };
        if !valid {
            return perr(format!("Invalid color: '{}'", t), res.loc.as_ref());
        }
        let mut color = t.to_string();
        if t.len() == 6 && hex(t) {
            color = format!("#{}", t);
        }
        Ok(Some(ParseNode::ColorToken {
            mode: self.mode,
            color,
        }))
    }

    /// `parseSizeGroup(optional)`.
    fn parse_size_group(&mut self, optional: bool) -> Result<Option<ParseNode>> {
        let mut is_blank = false;
        self.gullet.consume_spaces()?;
        let res = if !optional && self.gullet.future()?.text != "{" {
            Some(self.parse_size_regex_group("size")?)
        } else {
            self.parse_string_group(optional)?
        };
        let Some(mut res) = res else {
            return Ok(None);
        };
        if !optional && res.text.is_empty() {
            res.text = "0pt".to_string();
            is_blank = true;
        }
        let Some((number, unit)) = match_size(&res.text) else {
            return perr(format!("Invalid size: '{}'", res.text), res.loc.as_ref());
        };
        let Some(unit) = valid_unit(&unit) else {
            return perr(format!("Invalid unit: '{}'", unit), res.loc.as_ref());
        };
        Ok(Some(ParseNode::Size {
            mode: self.mode,
            value: Measurement { number, unit },
            is_blank,
        }))
    }

    /// `parseUrlGroup(optional)`.
    fn parse_url_group(&mut self, optional: bool) -> Result<Option<ParseNode>> {
        self.gullet.lexer.set_catcode('%', 13);
        self.gullet.lexer.set_catcode('~', 12);
        let res = self.parse_string_group(optional);
        self.gullet.lexer.set_catcode('%', 14);
        self.gullet.lexer.set_catcode('~', 13);
        let Some(res) = res? else {
            return Ok(None);
        };
        let mut url = String::new();
        let mut it = res.text.chars().peekable();
        while let Some(c) = it.next() {
            if c == '\\'
                && let Some(&n) = it.peek()
                && "#$%&~_^{}".contains(n)
            {
                url.push(n);
                it.next();
                continue;
            }
            url.push(c);
        }
        Ok(Some(ParseNode::Url {
            mode: self.mode,
            url,
        }))
    }

    /// `parseArgumentGroup(optional, mode)`.
    pub fn parse_argument_group(
        &mut self,
        optional: bool,
        mode: Option<Mode>,
    ) -> Result<Option<OrdGroupNode>> {
        let Some(arg_token) = self.gullet.scan_argument(optional)? else {
            return Ok(None);
        };
        let outer_mode = self.mode;
        if let Some(m) = mode {
            self.switch_mode(m);
        }
        self.gullet.begin_group();
        let expression = self.parse_expression(false, Some("EOF"))?;
        self.expect("EOF", true)?;
        self.gullet.end_group()?;
        let result = OrdGroupNode {
            mode: self.mode,
            body: expression,
            semisimple: false,
            loc: arg_token.loc,
        };
        if mode.is_some() {
            self.switch_mode(outer_mode);
        }
        Ok(Some(result))
    }

    /// `parseGroup(name, breakOnTokenText)`.
    pub fn parse_group(
        &mut self,
        name: &str,
        break_on_token_text: Option<BreakToken>,
    ) -> Result<Option<ParseNode>> {
        let first = self.fetch()?.clone();
        let text = first.text.as_str();
        if text == "{" || text == "\\begingroup" {
            self.consume();
            let group_end = if text == "{" { "}" } else { "\\endgroup" };
            self.gullet.begin_group();
            let expression = self.parse_expression(false, Some(group_end))?;
            let last = self.fetch()?.clone();
            self.expect(group_end, true)?;
            self.gullet.end_group()?;
            Ok(Some(ParseNode::OrdGroup(OrdGroupNode {
                mode: self.mode,
                body: expression,
                semisimple: text == "\\begingroup",
                loc: Loc::range(first.loc.as_ref(), last.loc.as_ref()),
            })))
        } else {
            let mut result = self.parse_function(break_on_token_text, Some(name))?;
            if result.is_none() {
                result = self.parse_symbol()?;
            }
            if result.is_none() && text.starts_with('\\') && !is_implicit_command(text) {
                result = Some(self.format_unsupported_cmd(text));
                self.consume();
            }
            Ok(result)
        }
    }

    /// `parseSymbol()`.
    fn parse_symbol(&mut self) -> Result<Option<ParseNode>> {
        let nucleus = self.fetch()?.clone();
        let mut text = nucleus.text.clone();
        if let Some(rest) = text.strip_prefix("\\verb")
            && rest
                .chars()
                .next()
                .map(|c| !c.is_ascii_alphabetic())
                .unwrap_or(false)
        {
            self.consume();
            let mut arg = rest;
            let star = arg.starts_with('*');
            if star {
                arg = &arg[1..];
            }
            let first = arg.chars().next();
            let last = arg.chars().last();
            if arg.chars().count() < 2 || first != last {
                return perr(
                    "\\verb assertion failed --\n                    please report what input caused this bug",
                    None,
                );
            }
            let f = first.unwrap();
            let inner = &arg[f.len_utf8()..arg.len() - f.len_utf8()];
            return Ok(Some(ParseNode::Verb {
                mode: Mode::Text,
                body: inner.to_string(),
                star,
            }));
        }
        // Accented Unicode characters → base + combining accents.
        if let Some(c0) = text.chars().next()
            && let Some(decomposed) = unicode_symbols::get(c0)
            && !symbols::has(self.mode, &c0.to_string())
        {
            text = format!("{}{}", decomposed, &text[c0.len_utf8()..]);
        }
        // Strip trailing combining marks.
        let mut accents: Vec<char> = Vec::new();
        let base_end = text
            .char_indices()
            .rev()
            .take_while(|(_, c)| ('\u{0300}'..='\u{036f}').contains(c))
            .last()
            .map(|(i, _)| i);
        if let Some(i) = base_end {
            accents = text[i..].chars().collect();
            text.truncate(i);
            if text == "i" {
                text = "\u{0131}".to_string();
            } else if text == "j" {
                text = "\u{0237}".to_string();
            }
        }
        let loc = nucleus.loc.clone();
        let mut symbol = if let Some(info) = symbols::get(self.mode, &text) {
            let group = info.group;
            if group.is_atom() {
                let family = match group {
                    Group::Bin => crate::parse_node::Atom::Bin,
                    Group::Close => crate::parse_node::Atom::Close,
                    Group::Inner => crate::parse_node::Atom::Inner,
                    Group::Open => crate::parse_node::Atom::Open,
                    Group::Punct => crate::parse_node::Atom::Punct,
                    _ => crate::parse_node::Atom::Rel,
                };
                ParseNode::Atom(AtomNode {
                    mode: self.mode,
                    family,
                    text,
                    loc,
                })
            } else {
                let n = SymNode {
                    mode: self.mode,
                    text,
                    loc,
                };
                match group {
                    Group::AccentToken => ParseNode::AccentToken(n),
                    Group::MathOrd => ParseNode::MathOrd(n),
                    Group::OpToken => ParseNode::OpToken(n),
                    Group::Spacing => ParseNode::Spacing(n),
                    _ => ParseNode::TextOrd(n),
                }
            }
        } else if first_code_unit(&text).map(|u| u >= 0x80).unwrap_or(false) {
            ParseNode::TextOrd(SymNode {
                mode: Mode::Text,
                text,
                loc,
            })
        } else {
            return Ok(None);
        };
        self.consume();
        for accent in accents {
            let Some((text_cmd, math_cmd)) = unicode_accents::get(accent) else {
                return perr(
                    format!("Unknown accent ' {}'", accent),
                    nucleus.loc.as_ref(),
                );
            };
            let command = match self.mode {
                Mode::Math => math_cmd.unwrap_or(text_cmd),
                Mode::Text => text_cmd,
            };
            symbol = ParseNode::Accent(Box::new(AccentNode {
                mode: self.mode,
                label: command.to_string(),
                is_stretchy: false,
                is_shifty: true,
                base: Box::new(symbol),
            }));
        }
        Ok(Some(symbol))
    }

    /// Fails with KaTeX's internal "Expected node of type" error.
    pub fn internal<T>(msg: &str) -> Result<T> {
        ierr(msg)
    }
}

/// `formLigatures(group)` (text mode).
fn form_ligatures(group: &mut Vec<ParseNode>) {
    let mut n = group.len() as isize - 1;
    let mut i: isize = 0;
    while i < n {
        let iu = i as usize;
        let v = match &group[iu] {
            ParseNode::TextOrd(a) => a.text.clone(),
            _ => {
                i += 1;
                continue;
            }
        };
        let next_text = match group.get(iu + 1) {
            Some(ParseNode::TextOrd(b)) => b.text.clone(),
            _ => {
                i += 1;
                continue;
            }
        };
        if v == "-" && next_text == "-" {
            let after_is_dash = i + 1 < n
                && matches!(group.get(iu + 2), Some(ParseNode::TextOrd(c)) if c.text == "-");
            if after_is_dash {
                let loc = Loc::range(group[iu].loc(), group[iu + 2].loc());
                group.splice(
                    iu..iu + 3,
                    [ParseNode::TextOrd(SymNode {
                        mode: Mode::Text,
                        text: "---".into(),
                        loc,
                    })],
                );
                n -= 2;
            } else {
                let loc = Loc::range(group[iu].loc(), group[iu + 1].loc());
                group.splice(
                    iu..iu + 2,
                    [ParseNode::TextOrd(SymNode {
                        mode: Mode::Text,
                        text: "--".into(),
                        loc,
                    })],
                );
                n -= 1;
            }
        }
        // KaTeX re-reads `next.text` (the node that followed before any splice).
        if (v == "'" || v == "`") && next_text == v {
            let loc = Loc::range(group[iu].loc(), group.get(iu + 1).and_then(|g| g.loc()));
            group.splice(
                iu..(iu + 2).min(group.len()),
                [ParseNode::TextOrd(SymNode {
                    mode: Mode::Text,
                    text: format!("{}{}", v, v),
                    loc,
                })],
            );
            n -= 1;
        }
        i += 1;
    }
}

/// `/^[-+]? *(?:$|\d+|\d+\.\d*|\.\d*) *[a-z]{0,2} *$/.test(s)`.
fn size_prefix_matches(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    // number: empty | \d+ | \d+\.\d* | \.\d*  — greedy is exact here (what follows is
    // spaces or letters).
    // The number alternatives: `$` (only at the very end), `\d+(\.\d*)?`, `\.\d*`.
    if i == b.len() {
        return true;
    }
    if !(b[i].is_ascii_digit() || b[i] == b'.') {
        return false;
    }
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    let ls = i;
    while i < b.len() && b[i].is_ascii_lowercase() && i - ls < 2 {
        i += 1;
    }
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    i == b.len()
}

/// `/([-+]?) *(\d+(?:\.\d*)?|\.\d+) *([a-z]{2})/.exec(s)` → (number, unit).
fn match_size(s: &str) -> Option<(f64, String)> {
    let b = s.as_bytes();
    for start in 0..=b.len() {
        for with_sign in [true, false] {
            let mut i = start;
            let mut sign = "";
            if with_sign {
                if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
                    sign = if b[i] == b'-' { "-" } else { "+" };
                    i += 1;
                } else {
                    continue;
                }
            }
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            let ns = i;
            if i < b.len() && b[i].is_ascii_digit() {
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i < b.len() && b[i] == b'.' {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            } else if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                continue;
            }
            let num = &s[ns..i];
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if i + 2 <= b.len() && b[i].is_ascii_lowercase() && b[i + 1].is_ascii_lowercase() {
                let text = format!("{}{}", sign, num);
                return Some((js_parse_number(&text), s[i..i + 2].to_string()));
            }
        }
    }
    None
}

/// JS unary `+` on a string of the form `[-+]?(\d+(\.\d*)?|\.\d+)`.
fn js_parse_number(s: &str) -> f64 {
    let t = s.strip_prefix('+').unwrap_or(s);
    let t = t.strip_suffix('.').unwrap_or(t);
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let t = if t.starts_with('.') {
        format!("0{}", t)
    } else {
        t.to_string()
    };
    let v: f64 = t.parse().unwrap_or(f64::NAN);
    if neg { -v } else { v }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_regexes() {
        assert!(size_prefix_matches("1"));
        assert!(size_prefix_matches("-1.5 em"));
        assert!(size_prefix_matches(""));
        assert!(!size_prefix_matches("1emx"));
        assert!(!size_prefix_matches("e"));
        assert!(size_prefix_matches("-"));
        assert!(size_prefix_matches("."));
        assert_eq!(match_size("1.5em"), Some((1.5, "em".into())));
        assert_eq!(match_size("-.5 pt"), Some((-0.5, "pt".into())));
        assert_eq!(match_size("x 2pt"), Some((2.0, "pt".into())));
        assert_eq!(match_size("2p"), None);
    }
}

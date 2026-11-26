//! The embedded grammar data (written by tools/extract.mjs) and its lazily
//! compiled regexes.
//!
//! A language's data is parsed on its first use and kept; a regex is compiled
//! the first time the engine needs it (most modes of a grammar are never
//! entered by a given snippet), then kept too.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::generated::{Callback, Language, Scope, LANGUAGES, SCOPES, SCOPE_NAMES};
use crate::regex::Re;

pub(crate) const SKIP: u32 = 1;
pub(crate) const EXCLUDE_BEGIN: u32 = 2;
pub(crate) const RETURN_BEGIN: u32 = 4;
pub(crate) const EXCLUDE_END: u32 = 8;
pub(crate) const RETURN_END: u32 = 16;
pub(crate) const ENDS_WITH_PARENT: u32 = 32;
pub(crate) const ENDS_PARENT: u32 = 64;

/// What a match of the mode's joined regex means (highlight.js `type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RuleKind {
    /// A child mode begins (index into `Grammar::modes`).
    Begin(u32),
    End,
    Illegal,
}

pub(crate) struct Rule {
    pub src: &'static str,
    pub groups: u32,
    pub kind: RuleKind,
}

/// highlight.js `beginScope` / `endScope` after compilation.
pub(crate) enum ScopeSpec {
    None,
    /// `_wrap`: the whole match in one scope. `raw` is the name as written,
    /// `aliased` after `classNameAliases`.
    Wrap { raw: Scope, aliased: Scope },
    /// `_multi`: (match group, scope after aliases); `None` = no scope, the
    /// group's text goes through keyword processing.
    Multi(Vec<(u32, Option<Scope>)>),
}

pub(crate) enum SubLanguage {
    None,
    /// A name looked up among the registered languages as is (no aliases).
    One(&'static str),
    /// Auto-detection among these (empty = every registered language).
    Auto(Vec<&'static str>),
}

/// How a keyword is emitted.
#[derive(Clone, Copy)]
pub(crate) enum KeywordKind {
    Scope(Scope),
    /// A kind starting with `_`: counts for relevance, not highlighted.
    RelevanceOnly,
}

pub(crate) struct KeywordTable {
    pattern: &'static str,
    re: OnceLock<Result<Re, String>>,
    pub words: HashMap<&'static str, (KeywordKind, f64)>,
}

/// One alternation `(r1)|(r2)|…` over the rules from some index on
/// (highlight.js `MultiRegex`).
pub(crate) struct MultiRegex {
    pub re: Re,
    /// (number of the group wrapping a rule, rule index), ascending.
    pub rule_at: Vec<(u32, u32)>,
}

pub(crate) struct Mode {
    pub flags: u32,
    /// Scope after `classNameAliases` (what `startNewMode` opens).
    pub scope: Option<Scope>,
    /// Scope as written (what a continuation reopens).
    pub scope_raw: Option<Scope>,
    pub relevance: f64,
    end_src: Option<&'static str>,
    end_re: OnceLock<Result<Re, String>>,
    pub rules: Vec<Rule>,
    /// Number of `Begin` rules (highlight.js `matcher.count`).
    pub begin_count: usize,
    matchers: Vec<OnceLock<Result<MultiRegex, String>>>,
    pub keywords: Option<u32>,
    pub begin_scope: ScopeSpec,
    pub end_scope: ScopeSpec,
    pub starts: Option<u32>,
    pub sub: SubLanguage,
    pub before_begin: Option<Callback>,
    pub on_begin: Option<Callback>,
    pub on_end: Option<Callback>,
}

impl Mode {
    pub fn has(&self, flag: u32) -> bool {
        self.flags & flag != 0
    }
}

pub(crate) struct Grammar {
    pub language: Language,
    pub case_insensitive: bool,
    pub unicode: bool,
    pub modes: Vec<Mode>,
    pub keyword_tables: Vec<KeywordTable>,
}

struct Reader {
    data: &'static [u8],
    at: usize,
}

impl Reader {
    fn u8(&mut self) -> u8 {
        let b = self.data[self.at];
        self.at += 1;
        b
    }
    fn varint(&mut self) -> u32 {
        let mut n = 0u32;
        let mut shift = 0;
        loop {
            let b = self.u8();
            n |= u32::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return n;
            }
            shift += 7;
        }
    }
    fn str(&mut self) -> &'static str {
        let len = self.varint() as usize;
        let s = &self.data[self.at..self.at + len];
        self.at += len;
        std::str::from_utf8(s).expect("grammar data: utf-8")
    }
}

pub(crate) fn scope_by_name(name: &str) -> Option<Scope> {
    let i = SCOPE_NAMES.binary_search(&name).ok()?;
    Some(SCOPES[i])
}

fn scope(name: &str) -> Scope {
    scope_by_name(name).unwrap_or_else(|| panic!("grammar data: unknown scope {name}"))
}

impl Grammar {
    fn load(language: Language) -> Grammar {
        let entry = &LANGUAGES[language as usize];
        let mut r = Reader { data: entry.data, at: 0 };
        assert_eq!(&entry.data[..4], b"DHLG", "grammar data: magic");
        r.at = 4;
        assert_eq!(r.u8(), 1, "grammar data: version");
        assert_eq!(r.str(), entry.name);
        let flags = r.u8();
        let mut aliases: Vec<(&'static str, &'static str)> = Vec::new();
        for _ in 0..r.varint() {
            let k = r.str();
            let v = r.str();
            aliases.push((k, v));
        }
        let strings: Vec<&'static str> = (0..r.varint()).map(|_| r.str()).collect();
        // `classNameAliases[x] || x`
        let aliased = |name: &'static str| -> &'static str {
            aliases
                .iter()
                .find(|(k, v)| *k == name && !v.is_empty())
                .map_or(name, |(_, v)| *v)
        };

        let mut keyword_tables = Vec::new();
        for _ in 0..r.varint() {
            let pattern = strings[r.varint() as usize];
            let n = r.varint() as usize;
            let mut words = HashMap::with_capacity(n);
            for _ in 0..n {
                let word = r.str();
                let kind = strings[r.varint() as usize];
                let relevance = f64::from(r.varint()) / 1000.0;
                let kind = if kind.starts_with('_') {
                    KeywordKind::RelevanceOnly
                } else {
                    KeywordKind::Scope(scope(aliased(kind)))
                };
                words.insert(word, (kind, relevance));
            }
            keyword_tables.push(KeywordTable { pattern, re: OnceLock::new(), words });
        }

        struct Raw {
            mode: Mode,
            begin: Option<(&'static str, u32)>,
            term: Option<(&'static str, u32)>,
            illegal: Option<(&'static str, u32)>,
            contains: Vec<u32>,
        }
        let mut raws: Vec<Raw> = Vec::new();
        for _ in 0..r.varint() {
            let flags = r.varint();
            let scope_idx = r.varint();
            let relevance = f64::from(r.varint()) / 1000.0;
            let re = |r: &mut Reader| -> Option<(&'static str, u32)> {
                let s = r.varint();
                (s != 0).then(|| (strings[s as usize - 1], r.varint()))
            };
            let begin = re(&mut r);
            let end = r.varint();
            let term = re(&mut r);
            let illegal = re(&mut r);
            let keywords = r.varint();
            let spec = |r: &mut Reader| -> ScopeSpec {
                match r.varint() {
                    0 => ScopeSpec::None,
                    1 => {
                        let name = strings[r.varint() as usize];
                        ScopeSpec::Wrap { raw: scope(name), aliased: scope(aliased(name)) }
                    }
                    2 => ScopeSpec::Multi(
                        (0..r.varint())
                            .map(|_| {
                                let group = r.varint();
                                let name = r.varint();
                                // `aliases[name] || name`, and an empty name is "no scope"
                                let sc = (name != 0)
                                    .then(|| aliased(strings[name as usize - 1]))
                                    .filter(|s| !s.is_empty())
                                    .map(scope);
                                (group, sc)
                            })
                            .collect(),
                    ),
                    k => panic!("grammar data: scope kind {k}"),
                }
            };
            let begin_scope = spec(&mut r);
            let end_scope = spec(&mut r);
            let contains: Vec<u32> = (0..r.varint()).map(|_| r.varint()).collect();
            let starts = r.varint();
            let sub = match r.varint() {
                0 => SubLanguage::None,
                1 => SubLanguage::One(strings[r.varint() as usize]),
                2 => SubLanguage::Auto((0..r.varint()).map(|_| strings[r.varint() as usize]).collect()),
                k => panic!("grammar data: sublanguage kind {k}"),
            };
            let cb = |r: &mut Reader| -> Option<Callback> {
                match r.u8() {
                    0 => None,
                    id => Some(Callback::from_id(id).expect("grammar data: callback id")),
                }
            };
            let before_begin = cb(&mut r);
            let on_begin = cb(&mut r);
            let on_end = cb(&mut r);
            let scope_raw = (scope_idx != 0).then(|| strings[scope_idx as usize - 1]);
            raws.push(Raw {
                mode: Mode {
                    flags,
                    scope: scope_raw.map(|s| scope(aliased(s))),
                    scope_raw: scope_raw.map(scope),
                    relevance,
                    end_src: (end != 0).then(|| strings[end as usize - 1]),
                    end_re: OnceLock::new(),
                    rules: Vec::new(),
                    begin_count: contains.len(),
                    matchers: Vec::new(),
                    keywords: (keywords != 0).then(|| keywords - 1),
                    begin_scope,
                    end_scope,
                    starts: (starts != 0).then(|| starts - 1),
                    sub,
                    before_begin,
                    on_begin,
                    on_end,
                },
                begin,
                term,
                illegal,
                contains,
            });
        }
        assert_eq!(r.at, r.data.len(), "grammar data: trailing bytes");

        // buildModeRegex: the children's begins, then the end, then illegal
        let begins: Vec<Option<(&'static str, u32)>> = raws.iter().map(|r| r.begin).collect();
        let modes = raws
            .into_iter()
            .map(|raw| {
                let mut mode = raw.mode;
                for &c in &raw.contains {
                    let (src, groups) = begins[c as usize].expect("grammar data: child without begin");
                    mode.rules.push(Rule { src, groups, kind: RuleKind::Begin(c) });
                }
                if let Some((src, groups)) = raw.term {
                    mode.rules.push(Rule { src, groups, kind: RuleKind::End });
                }
                if let Some((src, groups)) = raw.illegal {
                    mode.rules.push(Rule { src, groups, kind: RuleKind::Illegal });
                }
                mode.matchers = (0..mode.rules.len()).map(|_| OnceLock::new()).collect();
                mode
            })
            .collect();

        Grammar {
            language,
            case_insensitive: flags & 1 != 0,
            unicode: flags & 2 != 0,
            modes,
            keyword_tables,
        }
    }

    /// The joined regex over `rules[from..]`, or `None` when there are no
    /// rules left (highlight.js then makes a matcher that never matches).
    pub fn matcher(&self, mode: u32, from: usize) -> Result<Option<&MultiRegex>, String> {
        let m = &self.modes[mode as usize];
        let Some(cell) = m.matchers.get(from) else {
            return Ok(None);
        };
        cell.get_or_init(|| {
            let rules = &m.rules[from..];
            let src = join_rules(rules.iter().map(|r| r.src));
            let re = Re::new(&src, self.case_insensitive, self.unicode)?;
            let mut rule_at = Vec::with_capacity(rules.len());
            let mut match_at = 1u32;
            for (i, rule) in rules.iter().enumerate() {
                rule_at.push((match_at, (from + i) as u32));
                match_at += rule.groups + 1;
            }
            Ok(MultiRegex { re, rule_at })
        })
        .as_ref()
        .map(Some)
        .map_err(Clone::clone)
    }

    /// The source of the joined regex, for the tests that compare it with
    /// highlight.js's own.
    pub fn joined_source(&self, mode: u32, from: usize) -> String {
        join_rules(self.modes[mode as usize].rules[from..].iter().map(|r| r.src))
    }

    pub fn end_re(&self, mode: u32) -> Result<Option<&Re>, String> {
        let m = &self.modes[mode as usize];
        let Some(src) = m.end_src else {
            return Ok(None);
        };
        m.end_re
            .get_or_init(|| Re::new(src, self.case_insensitive, self.unicode))
            .as_ref()
            .map(Some)
            .map_err(Clone::clone)
    }

    pub fn keyword_re(&self, table: u32) -> Result<&Re, String> {
        let t = &self.keyword_tables[table as usize];
        t.re
            .get_or_init(|| Re::new(t.pattern, self.case_insensitive, self.unicode))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Every scope the grammar can emit (tests).
    pub fn scopes_used(&self) -> Vec<Scope> {
        let mut out = Vec::new();
        let mut add = |s: Scope| {
            if !out.contains(&s) {
                out.push(s);
            }
        };
        for m in &self.modes {
            m.scope.into_iter().chain(m.scope_raw).for_each(&mut add);
            for spec in [&m.begin_scope, &m.end_scope] {
                match spec {
                    ScopeSpec::None => {}
                    ScopeSpec::Wrap { raw, aliased } => {
                        add(*raw);
                        add(*aliased);
                    }
                    ScopeSpec::Multi(list) => list.iter().filter_map(|(_, s)| *s).for_each(&mut add),
                }
            }
        }
        for t in &self.keyword_tables {
            for (kind, _) in t.words.values() {
                if let KeywordKind::Scope(s) = kind {
                    add(*s);
                }
            }
        }
        out
    }

    /// Compiles every regex of the grammar (tests and measurements; normal
    /// use compiles on demand). Returns how many were compiled.
    pub fn compile_all(&self) -> Result<usize, String> {
        let mut n = 0;
        for (i, m) in self.modes.iter().enumerate() {
            if self.end_re(i as u32)?.is_some() {
                n += 1;
            }
            if self.matcher(i as u32, 0)?.is_some() {
                n += 1;
            }
            let _ = m;
        }
        for i in 0..self.keyword_tables.len() {
            self.keyword_re(i as u32)?;
            n += 1;
        }
        Ok(n)
    }
}

static GRAMMARS: [OnceLock<Grammar>; LANGUAGES.len()] = [const { OnceLock::new() }; LANGUAGES.len()];

/// The grammar of a language, parsed on first use.
pub(crate) fn grammar(language: Language) -> &'static Grammar {
    GRAMMARS[language as usize].get_or_init(|| Grammar::load(language))
}

/// Whether a language's grammar has been loaded yet.
pub(crate) fn is_loaded(language: Language) -> bool {
    GRAMMARS[language as usize].get().is_some()
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

/// highlight.js `_rewriteBackreferences(regexps, { joinWith: "|" })`: wraps
/// every regex in a group, joins them, and shifts the numbered backreferences
/// by the groups that now precede them. Ported with its scanner
/// (`BACKREF_RE = /\[(?:[^\\\]]|\\.)*\]|\(\??|\\([1-9][0-9]*)|\\./`),
/// including that it does not count `(?<name>…)` as a group.
pub(crate) fn join_rules<'a>(regexps: impl Iterator<Item = &'a str>) -> String {
    let mut num_captures = 0u32;
    let mut out = String::new();
    for (n, re) in regexps.enumerate() {
        if n > 0 {
            out.push('|');
        }
        out.push('(');
        num_captures += 1;
        let offset = num_captures;
        let chars: Vec<char> = re.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '[' => {
                    // \[(?:[^\\\]]|\\.)*\]
                    let mut j = i + 1;
                    let end = loop {
                        match chars.get(j) {
                            Some(']') => break Some(j + 1),
                            Some('\\') => match chars.get(j + 1) {
                                Some(&c) if !is_line_terminator(c) => j += 2,
                                _ => break None,
                            },
                            Some(_) => j += 1,
                            None => break None,
                        }
                    };
                    match end {
                        Some(end) => {
                            out.extend(&chars[i..end]);
                            i = end;
                        }
                        None => {
                            out.push('[');
                            i += 1;
                        }
                    }
                }
                '(' => {
                    if chars.get(i + 1) == Some(&'?') {
                        out.push_str("(?");
                        i += 2;
                    } else {
                        out.push('(');
                        num_captures += 1;
                        i += 1;
                    }
                }
                '\\' => match chars.get(i + 1) {
                    Some(&d) if ('1'..='9').contains(&d) => {
                        let mut j = i + 1;
                        let mut digits = String::new();
                        while let Some(&d) = chars.get(j).filter(|c| c.is_ascii_digit()) {
                            digits.push(d);
                            j += 1;
                        }
                        // JS Number(); a reference this long is not a real one
                        let n: u64 = digits.parse().unwrap_or(u64::MAX / 2);
                        out.push('\\');
                        out.push_str(&(n + u64::from(offset)).to_string());
                        i = j;
                    }
                    Some(&c) if !is_line_terminator(c) => {
                        out.push('\\');
                        out.push(c);
                        i += 2;
                    }
                    _ => {
                        out.push('\\');
                        i += 1;
                    }
                },
                c => {
                    out.push(c);
                    i += 1;
                }
            }
        }
        out.push(')');
    }
    out
}

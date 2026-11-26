//! A port of highlight.js's `_highlight` (lib/core.js, 11.11.2), driven by
//! the extracted grammar data. Function and variable names follow the
//! JavaScript so the two can be read side by side; comments quote the
//! original where the behaviour is not obvious.
//!
//! Differences from the original, all deliberate:
//!
//! * Text is tracked as ranges of the input, not copied strings. highlight.js
//!   builds its output from the match buffer, and in three corner cases its
//!   buffer stops being a slice of the input (it appends a literal `"\n"`
//!   for an empty `illegal` match, the string `"undefined"` for an ignored
//!   empty match, and the text of match groups that did not take part). This
//!   port always emits the input's own text at those positions.
//! * State that highlight.js keeps on the compiled grammar for the life of
//!   the page (`response.data`, and the `position` fields its resumable
//!   matcher overwrites) lives in [`Call`]: one top-level highlight plus its
//!   sub-languages. A highlight therefore never depends on what was
//!   highlighted before it; it behaves like a freshly created highlight.js.
//! * Every regex runs under the thread's match budget (see `regex.rs`).

use std::collections::HashMap;
use std::ops::Range;

use crate::callbacks::{self, Verdict};
use crate::generated::{Language, Scope, ALL_LANGUAGES, LANGUAGES};
use crate::grammar::{
    grammar, Grammar, KeywordKind, RuleKind, ScopeSpec, SubLanguage, ENDS_PARENT, ENDS_WITH_PARENT,
    EXCLUDE_BEGIN, EXCLUDE_END, RETURN_BEGIN, RETURN_END, SKIP,
};
use crate::regex::OutOfBudget;

/// highlight.js `MAX_KEYWORD_HITS`.
const MAX_KEYWORD_HITS: u32 = 7;
/// Sub-languages nest (markdown → xml → javascript → xml …), each level on a
/// strictly shorter text; this only guards the stack.
const MAX_DEPTH: usize = 48;

/// An element of the output tree: a scope's `<span>`, or the wrapper around a
/// sub-language's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Node {
    /// `<span class="hljs-…">`
    Scope(Scope),
    /// The `<span class="xml">` lowlight puts around an embedded language.
    Language(Language),
}

/// The output as highlight.js's emitter receives it. Offsets are UTF-16 code
/// units of the top-level text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Open(Node),
    Close,
    Text(u32, u32),
}

/// Why a highlight stopped for good (the whole result is discarded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The match budget ran out.
    Budget,
    /// A grammar regex did not compile (a bug in the data or the engine).
    Regex(String),
    /// Sub-languages nested deeper than `MAX_DEPTH`.
    Depth,
}

impl From<OutOfBudget> for Stop {
    fn from(_: OutOfBudget) -> Stop {
        Stop::Budget
    }
}

/// How a `_highlight` call ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Done,
    /// An `illegal` match with `ignoreIllegals` off: `{ illegal: true, relevance: 0 }`.
    Illegal,
    /// highlight.js threw (its infinite-loop guard, or a TypeError on a
    /// degenerate mode stack) and, in safe mode, returned what it had with
    /// `relevance: 0` and `errorRaised`.
    Error,
}

pub(crate) struct Outcome {
    pub events: Vec<Event>,
    pub relevance: f64,
    /// The mode stack at the end (`_top`), root first: the continuation.
    pub top: Vec<u32>,
    pub status: Status,
}

#[derive(Default)]
struct MatcherState {
    built: Vec<bool>,
    position: Vec<u32>,
}

/// Per-call copies of the state highlight.js keeps on compiled modes.
#[derive(Default)]
pub(crate) struct Call {
    /// `mode.data._beginMatch`
    begin_match: HashMap<(Language, u32), Option<Vec<u16>>>,
    /// `ResumableMultiRegex`: which sub-matchers exist, and the rules'
    /// `position` fields as their construction left them.
    matchers: HashMap<(Language, u32), MatcherState>,
    depth: usize,
}

/// One match of a mode's joined regex (`EnhancedMatch`): the rule that
/// matched, and its groups renumbered so that 0 is the rule's own match
/// (`match.splice(0, i)`).
pub(crate) struct MMatch {
    m: regress::Match,
    /// Index into `m.captures` of the group wrapping the rule.
    off: usize,
    pub kind: RuleKind,
    rule: u32,
}

impl MMatch {
    /// `match.index`
    pub fn index(&self) -> usize {
        self.m.start()
    }
    /// The range of `match[0]`.
    pub fn range(&self) -> Range<usize> {
        self.m.range()
    }
    /// The range of `match[n]`, `None` when the group did not take part.
    pub fn group(&self, n: usize) -> Option<Range<usize>> {
        if n == 0 {
            return Some(self.m.range());
        }
        self.m.captures.get(self.off + n).cloned().flatten()
    }
    /// `match.length - 1`
    fn max_group(&self) -> usize {
        self.m.captures.len() - self.off - 1
    }
}

#[cfg(test)]
impl MMatch {
    /// The first match of `pattern` in `text` from `start`, as the engine
    /// would hand it to a callback (the pattern wrapped in a rule group).
    pub(crate) fn for_test(pattern: &str, text: &[u16], start: usize) -> MMatch {
        let re = crate::regex::Re::new(&format!("({pattern})"), false, false).unwrap();
        let m = re.find_from(text, start).unwrap().expect("pattern matches");
        MMatch { m, off: 0, kind: RuleKind::Begin(0), rule: 0 }
    }
}

/// `getLanguage(name)`: a registered name or alias, case-insensitively.
pub(crate) fn get_language(name: &str) -> Option<Language> {
    let lower;
    let name = if name.bytes().any(|b| b.is_ascii_uppercase()) || !name.is_ascii() {
        lower = name.to_lowercase();
        lower.as_str()
    } else {
        name
    };
    if let Some(i) = LANGUAGES.iter().position(|l| l.name == name) {
        return Some(ALL_LANGUAGES[i]);
    }
    crate::generated::ALIASES
        .binary_search_by(|(alias, _)| (*alias).cmp(name))
        .ok()
        .map(|i| crate::generated::ALIASES[i].1)
}

/// `languages[name]`: a registered name exactly as written (how an explicit
/// `subLanguage` is looked up).
fn registered_exact(name: &str) -> Option<Language> {
    LANGUAGES.iter().position(|l| l.name == name).map(|i| ALL_LANGUAGES[i])
}

struct Run<'a> {
    call: &'a mut Call,
    g: &'static Grammar,
    /// `codeToHighlight`
    code: &'a [u16],
    /// Where `code` starts in the top-level text.
    base: u32,
    ignore_illegals: bool,
    /// `top` and its `parent` chain, root (the language) first.
    stack: Vec<u32>,
    events: Vec<Event>,
    /// `modeBuffer`, as a range of `code`.
    buf: (usize, usize),
    relevance: f64,
    iterations: u64,
    resume_scan_at_same_position: bool,
    /// `top.matcher.regexIndex`. highlight.js keeps it on the mode's matcher;
    /// it is always reset (`considerAll`) before a mode's first exec and only
    /// read right after an exec on the same mode, so one variable is enough.
    regex_index: usize,
    /// `lastMatch`: (it was a begin match, its index).
    last_match: Option<(bool, usize)>,
    keyword_hits: HashMap<String, u32>,
    continuations: HashMap<&'static str, Vec<u32>>,
}

enum Thrown {
    Illegal,
    Error,
    Stop(Stop),
}

impl From<Stop> for Thrown {
    fn from(s: Stop) -> Thrown {
        Thrown::Stop(s)
    }
}
impl From<OutOfBudget> for Thrown {
    fn from(_: OutOfBudget) -> Thrown {
        Thrown::Stop(Stop::Budget)
    }
}

/// `_highlight(languageName, codeToHighlight, ignoreIllegals, continuation)`.
/// `base` is the offset of `code` in the top-level text.
pub(crate) fn highlight(
    call: &mut Call,
    language: Language,
    code: &[u16],
    base: u32,
    ignore_illegals: bool,
    continuation: Option<Vec<u32>>,
) -> Result<Outcome, Stop> {
    if call.depth >= MAX_DEPTH {
        return Err(Stop::Depth);
    }
    call.depth += 1;
    let mut run = Run {
        call,
        g: grammar(language),
        code,
        base,
        ignore_illegals,
        stack: continuation.unwrap_or_else(|| vec![0]),
        events: Vec::new(),
        buf: (0, 0),
        relevance: 0.0,
        iterations: 0,
        resume_scan_at_same_position: false,
        regex_index: 0,
        last_match: None,
        keyword_hits: HashMap::new(),
        continuations: HashMap::new(),
    };
    let status = match run.run() {
        Ok(()) => Status::Done,
        Err(Thrown::Illegal) => Status::Illegal,
        Err(Thrown::Error) => Status::Error,
        Err(Thrown::Stop(stop)) => {
            run.call.depth -= 1;
            return Err(stop);
        }
    };
    run.call.depth -= 1;
    Ok(Outcome {
        // `relevance: 0` on both error paths
        relevance: if status == Status::Done { run.relevance } else { 0.0 },
        events: run.events,
        top: run.stack,
        status,
    })
}

impl Run<'_> {
    fn top(&self) -> u32 {
        *self.stack.last().expect("mode stack")
    }

    fn text(&mut self, start: usize, end: usize) {
        if start < end {
            self.events.push(Event::Text(self.base + start as u32, self.base + end as u32));
        }
    }

    /// `modeBuffer += …` for text that directly follows the buffer.
    fn buf_append(&mut self, start: usize, end: usize) {
        let end = end.min(self.code.len());
        let start = start.min(end);
        if self.buf.0 == self.buf.1 {
            self.buf = (start, end);
        } else {
            self.buf.1 = end;
        }
    }

    fn run(&mut self) -> Result<(), Thrown> {
        // processContinuations(): reopen the scopes of the modes we resume in
        for i in 1..self.stack.len() {
            if let Some(scope) = self.g.modes[self.stack[i] as usize].scope_raw {
                self.events.push(Event::Open(Node::Scope(scope)));
            }
        }
        let mut index = 0usize;
        loop {
            self.iterations += 1;
            if self.resume_scan_at_same_position {
                // only regexes not matched previously will now be considered
                self.resume_scan_at_same_position = false;
            } else {
                self.regex_index = 0;
            }
            let Some(m) = self.exec(index)? else { break };
            self.buf_append(index, m.index());
            let processed = self.process_lexeme(&m)?;
            index = m.index() + processed;
        }
        // processLexeme(codeToHighlight.substring(index))
        self.buf_append(index.min(self.code.len()), self.code.len());
        self.process_buffer()?;
        Ok(())
    }

    // ── ResumableMultiRegex ──────────────────────────────────────────────

    /// `getMatcher(index)` builds the sub-matcher on first use, and building
    /// it reassigns `position` on the rule objects it shares with the other
    /// sub-matchers (`opts.position = this.position++`): rules from `from` on
    /// are renumbered from 0. Reproduced, because `exec` reads `position`.
    fn note_matcher(&mut self, mode: u32, from: usize) {
        if from == 0 {
            // always the first one built, and it numbers the rules 0..n
            return;
        }
        let n = self.g.modes[mode as usize].rules.len();
        let state = self.call.matchers.entry((self.g.language, mode)).or_insert_with(|| MatcherState {
            built: Vec::new(),
            position: (0..n as u32).collect(),
        });
        if state.built.len() <= from {
            state.built.resize(from + 1, false);
        }
        if !state.built[from] {
            state.built[from] = true;
            for (i, position) in state.position.iter_mut().enumerate().skip(from) {
                *position = (i - from) as u32;
            }
        }
    }

    fn position(&self, mode: u32, rule: u32) -> usize {
        match self.call.matchers.get(&(self.g.language, mode)) {
            Some(state) => state.position[rule as usize] as usize,
            None => rule as usize,
        }
    }

    /// `MultiRegex.exec`
    fn multi_exec(&mut self, mode: u32, from: usize, last_index: usize) -> Result<Option<MMatch>, Stop> {
        self.note_matcher(mode, from);
        let Some(matcher) = self.g.matcher(mode, from).map_err(Stop::Regex)? else {
            return Ok(None);
        };
        let Some(m) = matcher.re.find_from(self.code, last_index)? else {
            return Ok(None);
        };
        // const i = match.findIndex((el, i) => i > 0 && el !== undefined);
        let off = m
            .captures
            .iter()
            .position(Option::is_some)
            .ok_or_else(|| Stop::Regex("a joined regex matched without a rule group".into()))?;
        let at = matcher
            .rule_at
            .binary_search_by_key(&(off as u32 + 1), |&(group, _)| group)
            .map_err(|_| Stop::Regex("a joined regex matched inside a rule group".into()))?;
        let rule = matcher.rule_at[at].1;
        let kind = self.g.modes[mode as usize].rules[rule as usize].kind;
        Ok(Some(MMatch { m, off, kind, rule }))
    }

    /// `ResumableMultiRegex.exec`
    fn exec(&mut self, last_index: usize) -> Result<Option<MMatch>, Stop> {
        let top = self.top();
        let mut result = self.multi_exec(top, self.regex_index, last_index)?;
        // "resume scanning at the existing position but also skip the current
        // rule ONLY": run the remaining rules here, and if they do not match
        // right here, all rules from the next position
        if self.regex_index != 0 && !matches!(&result, Some(r) if r.index() == last_index) {
            result = self.multi_exec(top, 0, last_index + 1)?;
        }
        if let Some(r) = &result {
            self.regex_index += self.position(top, r.rule) + 1;
            if self.regex_index == self.g.modes[top as usize].begin_count {
                // wrap-around to considering all matches again
                self.regex_index = 0;
            }
        }
        Ok(result)
    }

    // ── keywords, sub-languages ──────────────────────────────────────────

    /// `emitKeyword(keyword, scope)`
    fn emit_keyword(&mut self, start: usize, end: usize, scope: Scope) {
        if start >= end {
            return;
        }
        self.events.push(Event::Open(Node::Scope(scope)));
        self.text(start, end);
        self.events.push(Event::Close);
    }

    /// `processKeywords()` over `code[start..end]`.
    fn process_keywords(&mut self, start: usize, end: usize) -> Result<(), Stop> {
        let g = self.g;
        let Some(table_idx) = g.modes[self.top() as usize].keywords else {
            self.text(start, end);
            return Ok(());
        };
        if start >= end {
            return Ok(());
        }
        let table = &g.keyword_tables[table_idx as usize];
        let re = g.keyword_re(table_idx).map_err(Stop::Regex)?;
        let buffer = &self.code[start..end];
        let mut last_index = 0usize;
        // start of the text not yet emitted (`buf`)
        let mut pending = 0usize;
        let mut word = String::new();
        while let Some(m) = re.find_from(buffer, last_index)? {
            let (ms, me) = (m.start(), m.end());
            // const word = language.case_insensitive ? match[0].toLowerCase() : match[0];
            word.clear();
            let units = &buffer[ms..me];
            if units.iter().all(|&u| u < 0x80) {
                word.extend(units.iter().map(|&u| {
                    let c = u as u8 as char;
                    if g.case_insensitive { c.to_ascii_lowercase() } else { c }
                }));
            } else {
                let s = String::from_utf16_lossy(units);
                word.push_str(&if g.case_insensitive { s.to_lowercase() } else { s });
            }
            if let Some(&(kind, keyword_relevance)) = table.words.get(word.as_str()) {
                self.text(start + pending, start + ms);
                let hits = match self.keyword_hits.get_mut(word.as_str()) {
                    Some(n) => {
                        *n += 1;
                        *n
                    }
                    None => {
                        self.keyword_hits.insert(word.clone(), 1);
                        1
                    }
                };
                if hits <= MAX_KEYWORD_HITS {
                    self.relevance += keyword_relevance;
                }
                match kind {
                    // "_ implied for relevance only, do not highlight"
                    KeywordKind::RelevanceOnly => pending = ms,
                    KeywordKind::Scope(scope) => {
                        self.emit_keyword(start + ms, start + me, scope);
                        pending = me;
                    }
                }
            }
            if me == ms {
                // A keyword pattern that matches the empty string would hang
                // highlight.js (exec does not advance lastIndex); none does.
                break;
            }
            last_index = me;
        }
        self.text(start + pending, end);
        Ok(())
    }

    /// `processSubLanguage()` over `code[start..end]`.
    fn process_sub_language(&mut self, start: usize, end: usize) -> Result<(), Stop> {
        if start >= end {
            return Ok(());
        }
        let g = self.g;
        let top = &g.modes[self.top() as usize];
        let code = &self.code[start..end];
        let base = self.base + start as u32;
        let (events, language, relevance) = match &top.sub {
            SubLanguage::None => unreachable!("process_sub_language without a sub-language"),
            SubLanguage::One(name) => {
                // if (!languages[top.subLanguage]) { emitter.addText(modeBuffer); return; }
                let Some(language) = registered_exact(name) else {
                    self.text(start, end);
                    return Ok(());
                };
                let continuation = self.continuations.get(name).cloned();
                let result = highlight(self.call, language, code, base, true, continuation)?;
                self.continuations.insert(name, result.top);
                (result.events, Some(language), result.relevance)
            }
            SubLanguage::Auto(names) => highlight_auto(self.call, code, base, names)?,
        };
        // "Counting embedded language score towards the host language may be
        // disabled with zeroing the containing mode relevance."
        if top.relevance > 0.0 {
            self.relevance += relevance;
        }
        // emitter.__addSublanguage(result._emitter, result.language): lowlight
        // wraps the children in a span named after the language, or splices
        // them in when there is none (auto-detection settled on plain text).
        if let Some(language) = language {
            self.events.push(Event::Open(Node::Language(language)));
        }
        let mut depth = 0i32;
        for &e in &events {
            match e {
                Event::Open(_) => depth += 1,
                Event::Close => depth -= 1,
                Event::Text(..) => {}
            }
        }
        self.events.extend(events);
        // nodes the sub-language left open end with its text
        for _ in 0..depth.max(0) {
            self.events.push(Event::Close);
        }
        if language.is_some() {
            self.events.push(Event::Close);
        }
        Ok(())
    }

    /// `processBuffer()`
    fn process_buffer(&mut self) -> Result<(), Stop> {
        let (start, end) = self.buf;
        self.buf = (0, 0);
        if matches!(self.g.modes[self.top() as usize].sub, SubLanguage::None) {
            self.process_keywords(start, end)
        } else {
            self.process_sub_language(start, end)
        }
    }

    /// `emitMultiClass(scope, match)`
    fn emit_multi_class(&mut self, spec: &[(u32, Option<Scope>)], m: &MMatch) -> Result<(), Stop> {
        let max = m.max_group();
        for &(group, scope) in spec {
            if group as usize > max {
                break;
            }
            // highlight.js would emit the string "undefined" for a group that
            // did not take part; emit nothing
            let Some(range) = m.group(group as usize) else { continue };
            match scope {
                Some(scope) => self.emit_keyword(range.start, range.end, scope),
                None => self.process_keywords(range.start, range.end)?,
            }
        }
        Ok(())
    }

    // ── modes ────────────────────────────────────────────────────────────

    /// `startNewMode(mode, match)`
    fn start_new_mode(&mut self, mode: u32, m: &MMatch) -> Result<(), Stop> {
        let g = self.g;
        let new_mode = &g.modes[mode as usize];
        if let Some(scope) = new_mode.scope {
            self.events.push(Event::Open(Node::Scope(scope)));
        }
        match &new_mode.begin_scope {
            ScopeSpec::None => {}
            // beginScope just wraps the begin match itself in a scope
            ScopeSpec::Wrap { aliased, .. } => {
                let (start, end) = self.buf;
                self.emit_keyword(start, end, *aliased);
                self.buf = (0, 0);
            }
            // at this point modeBuffer should just be the match
            ScopeSpec::Multi(spec) => {
                self.emit_multi_class(spec, m)?;
                self.buf = (0, 0);
            }
        }
        self.stack.push(mode);
        Ok(())
    }

    /// `endOfMode(mode, match, matchPlusRemainder)` for the frame at `frame`.
    /// Returns the stack index of the mode that ends.
    fn end_of_mode(&mut self, frame: usize, m: &MMatch) -> Result<Option<usize>, Thrown> {
        let g = self.g;
        let mode_id = self.stack[frame];
        let mode = &g.modes[mode_id as usize];
        // let matched = startsWith(mode.endRe, matchPlusRemainder);
        let mut matched = match g.end_re(mode_id).map_err(Stop::Regex)? {
            Some(re) => {
                let remainder = &self.code[m.index().min(self.code.len())..];
                matches!(re.find_from(remainder, 0)?, Some(found) if found.start() == 0)
            }
            None => false,
        };
        if matched {
            if let Some(cb) = mode.on_end {
                let data = self.call.begin_match.entry((g.language, mode_id)).or_default();
                if callbacks::run(cb, m, self.code, data) == Verdict::Ignore {
                    matched = false;
                }
            }
            if matched {
                // while (mode.endsParent && mode.parent) mode = mode.parent;
                let mut frame = frame;
                while g.modes[self.stack[frame] as usize].has(ENDS_PARENT) && frame > 0 {
                    frame -= 1;
                }
                return Ok(Some(frame));
            }
        }
        // "even if on:end fires an `ignore` it's still possible that we might
        // trigger the end node because of a parent mode"
        if mode.has(ENDS_WITH_PARENT) {
            if frame == 0 {
                // endOfMode(undefined, …): a TypeError in highlight.js
                return Err(Thrown::Error);
            }
            return self.end_of_mode(frame - 1, m);
        }
        Ok(None)
    }

    /// `doIgnore(lexeme)`
    fn do_ignore(&mut self, m: &MMatch) -> usize {
        if self.regex_index == 0 {
            // no more regexes to potentially match here, so we move the
            // cursor forward one space
            self.buf_append(m.index(), m.index() + 1);
            1
        } else {
            // no need to move the cursor, we still have additional regexes to
            // try and match at this very spot
            self.resume_scan_at_same_position = true;
            0
        }
    }

    /// `doBeginMatch(match)`
    fn do_begin_match(&mut self, m: &MMatch, mode: u32) -> Result<usize, Stop> {
        let g = self.g;
        let new_mode = &g.modes[mode as usize];
        let lexeme = m.range();
        // first internal before callbacks, then the public ones
        for cb in [new_mode.before_begin, new_mode.on_begin].into_iter().flatten() {
            let data = self.call.begin_match.entry((g.language, mode)).or_default();
            if callbacks::run(cb, m, self.code, data) == Verdict::Ignore {
                return Ok(self.do_ignore(m));
            }
        }
        if new_mode.has(SKIP) {
            self.buf_append(lexeme.start, lexeme.end);
        } else {
            if new_mode.has(EXCLUDE_BEGIN) {
                self.buf_append(lexeme.start, lexeme.end);
            }
            self.process_buffer()?;
            if !new_mode.has(RETURN_BEGIN) && !new_mode.has(EXCLUDE_BEGIN) {
                self.buf = (lexeme.start, lexeme.end);
            }
        }
        self.start_new_mode(mode, m)?;
        Ok(if new_mode.has(RETURN_BEGIN) { 0 } else { lexeme.len() })
    }

    /// `doEndMatch(match)`; `None` is `NO_MATCH`.
    fn do_end_match(&mut self, m: &MMatch) -> Result<Option<usize>, Thrown> {
        let g = self.g;
        let lexeme = m.range();
        let Some(end_frame) = self.end_of_mode(self.stack.len() - 1, m)? else {
            return Ok(None);
        };
        let end_mode = self.stack[end_frame];
        let origin = &g.modes[self.top() as usize];
        match &origin.end_scope {
            ScopeSpec::Wrap { raw, .. } => {
                self.process_buffer()?;
                // emitKeyword(lexeme, top.endScope._wrap): no classNameAliases here
                self.emit_keyword(lexeme.start, lexeme.end, *raw);
            }
            ScopeSpec::Multi(spec) => {
                self.process_buffer()?;
                self.emit_multi_class(spec, m)?;
            }
            ScopeSpec::None => {
                if origin.has(SKIP) {
                    self.buf_append(lexeme.start, lexeme.end);
                } else {
                    if !(origin.has(RETURN_END) || origin.has(EXCLUDE_END)) {
                        self.buf_append(lexeme.start, lexeme.end);
                    }
                    self.process_buffer()?;
                    if origin.has(EXCLUDE_END) {
                        self.buf = (lexeme.start, lexeme.end);
                    }
                }
            }
        }
        // do { … top = top.parent; } while (top !== endMode.parent);
        loop {
            let top = &g.modes[self.top() as usize];
            if top.scope.is_some() {
                self.events.push(Event::Close);
            }
            if !top.has(SKIP) && matches!(top.sub, SubLanguage::None) {
                self.relevance += top.relevance;
            }
            self.stack.pop();
            if self.stack.len() == end_frame {
                break;
            }
        }
        if self.stack.is_empty() {
            // the language itself "ended" (endsParent up to the root):
            // highlight.js goes on with `top === undefined` and throws
            self.stack.push(0);
            return Err(Thrown::Error);
        }
        if let Some(starts) = g.modes[end_mode as usize].starts {
            self.start_new_mode(starts, m)?;
        }
        Ok(Some(if origin.has(RETURN_END) { 0 } else { lexeme.len() }))
    }

    /// `processLexeme(textBeforeMatch, match)`; the text before the match is
    /// already in the buffer. Returns how far to advance.
    fn process_lexeme(&mut self, m: &MMatch) -> Result<usize, Thrown> {
        let lexeme = m.range();
        let is_begin = matches!(m.kind, RuleKind::Begin(_));

        // "we've found a 0 width match and we're stuck, so we need to advance"
        if let Some((true, last_index)) = self.last_match {
            if m.kind == RuleKind::End && last_index == m.index() && lexeme.is_empty() {
                // spit the "skipped" character that our regex choked on back
                // into the output sequence
                self.buf_append(m.index(), m.index() + 1);
                return Ok(1);
            }
        }
        self.last_match = Some((is_begin, m.index()));

        match m.kind {
            RuleKind::Begin(mode) => return Ok(self.do_begin_match(m, mode)?),
            RuleKind::Illegal if !self.ignore_illegals => return Err(Thrown::Illegal),
            RuleKind::End => {
                if let Some(processed) = self.do_end_match(m)? {
                    return Ok(processed);
                }
            }
            RuleKind::Illegal => {}
        }

        // "edge case for when illegal matches $ (end of line/text) which is
        // technically a 0 width match but not a begin/end match"
        if m.kind == RuleKind::Illegal && lexeme.is_empty() {
            // highlight.js appends a literal "\n" here unless at the end of
            // the text; the character at this position is kept instead
            self.buf_append(m.index(), m.index() + 1);
            return Ok(1);
        }

        // "infinite loops are BAD, this is a last ditch catch all"
        if self.iterations > 100_000 && self.iterations > m.index() as u64 * 3 {
            return Err(Thrown::Error);
        }

        // an end match that could not be completed (doEndMatch returned
        // NO_MATCH), or an ignored illegal
        self.buf_append(lexeme.start, lexeme.end);
        Ok(lexeme.len())
    }
}

/// `highlightAuto(code, languageSubset)` as `processSubLanguage` uses it:
/// every candidate is highlighted with `ignoreIllegals` off and the most
/// relevant wins; plain text (relevance 0) is always a candidate and wins
/// ties, then the earlier language. Returns the winner's events, its
/// language (`None` for plain text) and relevance.
pub(crate) fn highlight_auto(
    call: &mut Call,
    code: &[u16],
    base: u32,
    names: &[&'static str],
) -> Result<(Vec<Event>, Option<Language>, f64), Stop> {
    // languageSubset.filter(getLanguage).filter(autoDetection)
    let candidates: Vec<Language> = if names.is_empty() {
        ALL_LANGUAGES.to_vec()
    } else {
        names.iter().filter_map(|n| get_language(n)).collect()
    };
    let mut best: Option<(Vec<Event>, Language, f64)> = None;
    for language in candidates {
        if !LANGUAGES[language as usize].autodetect {
            continue;
        }
        let result = highlight(call, language, code, base, false, None)?;
        let beats = match &best {
            Some((_, _, relevance)) => result.relevance > *relevance,
            None => result.relevance > 0.0,
        };
        if beats {
            best = Some((result.events, language, result.relevance));
        }
    }
    Ok(match best {
        Some((events, language, relevance)) => (events, Some(language), relevance),
        // justTextHighlightResult(code)
        None => (
            if code.is_empty() { Vec::new() } else { vec![Event::Text(base, base + code.len() as u32)] },
            None,
            0.0,
        ),
    })
}

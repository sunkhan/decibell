//! Message search (spec 2026-10-14): the query and its chips (`features/search/
//! searchQuery.ts`, pure), and the one search at a time with its results
//! (`searchStore.ts`, `useSearchEvents.ts`). The results panel belongs to one community;
//! the header box's text and chips live in `drafts` per community, so switching channels
//! keeps them; `query` is what the results are for. **Nothing about a search is stored on
//! the device** (owner, 2026-10-14): no recent searches, nothing written anywhere.
//!
//! Timers: the 8 s "no answer" wait exists while a page is awaited; a retry after a "too
//! fast" answer exists until it fires. A new search supersedes the old one (its `seq`):
//! answers to anything else are dropped.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Datelike, Local, NaiveDate, TimeZone};
use gpui::{App, Context, SharedString, Task};

use crate::chat::message::Message;
use crate::error::UiError;
use crate::ids::{ChannelId, ServerId, Uid};

/// No answer this long: the server predates search (it never replies).
pub const NO_ANSWER: Duration = Duration::from_millis(8000);
/// Retries after a "too fast" / "busy" answer, before giving up.
pub const MAX_RETRIES: u32 = 3;
pub const PAGE_SIZE: i32 = 25;

// ── The query (searchQuery.ts) ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HasKind {
    Link,
    File,
    Image,
    Video,
    Audio,
    Document,
}

impl HasKind {
    pub const ALL: [HasKind; 6] = [HasKind::Link, HasKind::File, HasKind::Image, HasKind::Video, HasKind::Audio, HasKind::Document];

    /// `MessageSearchRequest.Has` bits.
    pub fn bit(self) -> u32 {
        match self {
            HasKind::File => 1,
            HasKind::Image => 2,
            HasKind::Video => 4,
            HasKind::Audio => 8,
            HasKind::Document => 16,
            HasKind::Link => 32,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HasKind::Link => "Link",
            HasKind::File => "File",
            HasKind::Image => "Image",
            HasKind::Video => "Video",
            HasKind::Audio => "Audio",
            HasKind::Document => "Document",
        }
    }
}

/// A local calendar day ("YYYY-MM-DD" in the renderer).
pub type Day = NaiveDate;

/// One chip. "inside" isn't a filter but an option shown the same way: match the words
/// anywhere inside words (`match_within_words`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SearchFilter {
    /// A member by account; `username` is the name it was picked under.
    From { uid: Uid, username: SharedString },
    In { channel: ChannelId },
    Has(HasKind),
    Before(Day),
    During(Day),
    After(Day),
    Inside,
}

impl SearchFilter {
    /// `sameFilter`: one chip per member / channel / kind; one date per operator; one
    /// "inside".
    pub fn same(&self, other: &SearchFilter) -> bool {
        use SearchFilter as F;
        match (self, other) {
            (F::From { uid: a, .. }, F::From { uid: b, .. }) => a == b,
            (F::In { channel: a }, F::In { channel: b }) => a == b,
            (F::Has(a), F::Has(b)) => a == b,
            (F::Before(_), F::Before(_)) | (F::During(_), F::During(_)) | (F::After(_), F::After(_)) | (F::Inside, F::Inside) => true,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Hash)]
pub struct SearchQuery {
    pub text: SharedString,
    pub filters: Vec<SearchFilter>,
}

impl SearchQuery {
    /// Adds a chip, replacing an equal one (and the same date operator).
    pub fn with_filter(&self, f: SearchFilter) -> SearchQuery {
        let mut filters: Vec<SearchFilter> = self.filters.iter().filter(|x| !x.same(&f)).cloned().collect();
        filters.push(f);
        SearchQuery { text: self.text.clone(), filters }
    }

    pub fn matches_inside(&self) -> bool {
        self.filters.contains(&SearchFilter::Inside)
    }

    /// Something to search for: words, or at least one filter (the inside option alone
    /// isn't one).
    pub fn is_searchable(&self) -> bool {
        if self.filters.iter().any(|f| *f != SearchFilter::Inside) {
            return true;
        }
        if self.matches_inside() { !search_fragments(&self.text).is_empty() } else { !search_words(&self.text).is_empty() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSort {
    #[default]
    Newest,
    Oldest,
    Relevance,
}

impl SearchSort {
    pub fn wire(self) -> &'static str {
        match self {
            SearchSort::Newest => "newest",
            SearchSort::Oldest => "oldest",
            SearchSort::Relevance => "relevance",
        }
    }
}

/// Local midnight of `day` plus `add_days`, as unix seconds.
fn day_start(day: Day, add_days: i64) -> i64 {
    let d = day + chrono::Days::new(add_days.max(0) as u64);
    match Local.with_ymd_and_hms(d.year(), d.month(), d.day(), 0, 0, 0).earliest() {
        Some(t) => t.timestamp(),
        None => 0,
    }
}

/// A typed date ("2026-10-03", "2026-10-3", "today", "yesterday"), or `None` (`parseDay`).
pub fn parse_day(input: &str, today: Day) -> Option<Day> {
    let t = input.trim().to_lowercase();
    match t.as_str() {
        "today" => return Some(today),
        "yesterday" => return today.pred_opt(),
        _ => {}
    }
    let mut parts = t.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || !(1..=2).contains(&m.len()) || !(1..=2).contains(&d.len()) {
        return None;
    }
    if ![y, m, d].iter().all(|p| p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    NaiveDate::from_ymd_opt(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
}

pub fn today() -> Day {
    Local::now().date_naive()
}

/// Unix-second bounds of the date chips (`dateBounds`): `after` inclusive, `before`
/// exclusive, 0 = unbounded. before: a day = earlier than that day; after: a day = later
/// than that day; during: that day.
pub fn date_bounds(filters: &[SearchFilter]) -> (i64, i64) {
    let (mut after, mut before) = (0i64, 0i64);
    let lower = |v: i64, after: &mut i64| {
        if v > *after {
            *after = v;
        }
    };
    let upper = |v: i64, before: &mut i64| {
        if v > 0 && (*before == 0 || v < *before) {
            *before = v;
        }
    };
    for f in filters {
        match f {
            SearchFilter::Before(d) => upper(day_start(*d, 0), &mut before),
            SearchFilter::After(d) => lower(day_start(*d, 1), &mut after),
            SearchFilter::During(d) => {
                lower(day_start(*d, 0), &mut after);
                upper(day_start(*d, 1), &mut before);
            }
            _ => {}
        }
    }
    (after, before)
}

/// Lower-case and strip accents the way the server folds text, with the Turkish dotless ı
/// as i (`fold`). The renderer decomposes (NFD) first; here combining marks are dropped and
/// the precomposed Latin letters (Latin-1 and Latin Extended-A) map to their base letter,
/// which is what the queries and highlights need; other scripts pass through lower-cased.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_combining(c) {
            continue;
        }
        let base = latin_base(c).unwrap_or(c);
        for l in base.to_lowercase() {
            out.push(if l == 'ı' { 'i' } else { l });
        }
    }
    out
}

fn is_combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

fn latin_base(c: char) -> Option<char> {
    const MAP: &[(&str, char)] = &[
        ("ÀÁÂÃÄÅĀĂĄ", 'A'), ("àáâãäåāăą", 'a'), ("ÇĆĈĊČ", 'C'), ("çćĉċč", 'c'), ("ĎĐ", 'D'), ("ďđ", 'd'),
        ("ÈÉÊËĒĔĖĘĚ", 'E'), ("èéêëēĕėęě", 'e'), ("ĜĞĠĢ", 'G'), ("ĝğġģ", 'g'), ("ĤĦ", 'H'), ("ĥħ", 'h'),
        ("ÌÍÎÏĨĪĬĮİ", 'I'), ("ìíîïĩīĭį", 'i'), ("Ĵ", 'J'), ("ĵ", 'j'), ("Ķ", 'K'), ("ķ", 'k'),
        ("ĹĻĽĿŁ", 'L'), ("ĺļľŀł", 'l'), ("ÑŃŅŇ", 'N'), ("ñńņň", 'n'), ("ÒÓÔÕÖØŌŎŐ", 'O'), ("òóôõöøōŏő", 'o'),
        ("ŔŖŘ", 'R'), ("ŕŗř", 'r'), ("ŚŜŞŠ", 'S'), ("śŝşš", 's'), ("ŢŤŦ", 'T'), ("ţťŧ", 't'),
        ("ÙÚÛÜŨŪŬŮŰŲ", 'U'), ("ùúûüũūŭůűų", 'u'), ("Ŵ", 'W'), ("ŵ", 'w'), ("ÝŶŸ", 'Y'), ("ýÿŷ", 'y'),
        ("ŹŻŽ", 'Z'), ("źżž", 'z'),
    ];
    if (c as u32) < 0xC0 || (c as u32) > 0x17F {
        return None;
    }
    MAP.iter().find(|(set, _)| set.contains(c)).map(|(_, b)| *b)
}

/// The tokens of a query's text: `(excluded, phrase, raw)` for each word or "phrase"
/// (`/(-?)(?:"([^"]*)"?|(\S+))/g`).
fn tokens(text: &str) -> Vec<(bool, bool, &str)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with(char::is_whitespace) {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        let excluded = bytes[i] == b'-';
        let mut j = if excluded { i + 1 } else { i };
        if j < bytes.len() && bytes[j] == b'"' {
            let start = j + 1;
            let end = text[start..].find('"').map_or(text.len(), |e| start + e);
            out.push((excluded, true, &text[start..end]));
            i = (end + 1).min(text.len());
            continue;
        }
        if excluded && (j >= bytes.len() || text[j..].starts_with(char::is_whitespace)) {
            // A lone "-" is a word of its own.
            j = i;
        }
        let end = text[j..].find(char::is_whitespace).map_or(text.len(), |e| j + e);
        if j == i && excluded {
            out.push((false, false, &text[i..end]));
        } else {
            out.push((excluded, false, &text[j..end]));
        }
        i = end;
    }
    out
}

/// A word the server matches: folded; `prefix` when it also matches words starting with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchWord {
    pub folded: String,
    pub prefix: bool,
}

/// The words a query's text matches (`searchWords`, the server's `build_fts_query`):
/// excluded words left out; a bare word of 3+ letters also matches as a prefix (its last
/// piece: "e-mail" → "e" then "mail…"); punctuation splits words.
pub fn search_words(text: &str) -> Vec<SearchWord> {
    let mut out = Vec::new();
    for (excluded, phrase, raw) in tokens(text) {
        if excluded {
            continue;
        }
        let folded = fold(raw);
        let parts: Vec<&str> = folded.split(|c: char| !c.is_alphanumeric()).filter(|p| !p.is_empty()).collect();
        let letters: usize = parts.iter().map(|p| p.chars().count()).sum();
        for (i, w) in parts.iter().enumerate() {
            out.push(SearchWord { folded: w.to_string(), prefix: !phrase && letters >= 3 && i == parts.len() - 1 });
        }
    }
    out
}

/// The words and "phrases" as inside-word fragments (folded, punctuation kept, excluded
/// ones left out: `searchFragments`).
pub fn search_fragments(text: &str) -> Vec<String> {
    tokens(text).into_iter().filter(|(ex, _, _)| !ex).map(|(_, _, raw)| fold(raw)).filter(|f| !f.is_empty()).collect()
}

/// One `search_messages` request (`toSearchArgs`).
#[derive(Debug, Clone, PartialEq)]
pub struct SearchArgs {
    pub server: ServerId,
    pub seq: u32,
    pub query: String,
    pub channel_ids: Vec<String>,
    pub author_uids: Vec<Uid>,
    pub after: i64,
    pub before: i64,
    pub has: u32,
    pub sort: SearchSort,
    pub cursor: i64,
    pub offset: i32,
    pub limit: i32,
    pub match_within_words: bool,
}

pub fn to_search_args(server: ServerId, q: &SearchQuery, sort: SearchSort, seq: u32, page: Page) -> SearchArgs {
    let (after, before) = date_bounds(&q.filters);
    let mut has = 0;
    let mut author_uids = Vec::new();
    let mut channel_ids = Vec::new();
    for f in &q.filters {
        match f {
            SearchFilter::Has(k) => has |= k.bit(),
            SearchFilter::From { uid, .. } => author_uids.push(*uid),
            SearchFilter::In { channel } => channel_ids.push(channel.to_string()),
            _ => {}
        }
    }
    SearchArgs {
        server,
        seq,
        query: q.text.trim().to_string(),
        channel_ids,
        author_uids,
        after,
        before,
        has,
        sort,
        cursor: page.cursor,
        offset: page.offset,
        limit: PAGE_SIZE,
        match_within_words: q.matches_inside(),
    }
}

// ── The store (searchStore.ts) ──────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchStatus {
    #[default]
    Idle,
    Loading,
    Done,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Page {
    pub cursor: i64,
    pub offset: i32,
}

/// `MESSAGE_SEARCH_RES`, mapped.
#[derive(Debug, Clone)]
pub struct SearchAnswer {
    pub server: ServerId,
    pub seq: u32,
    pub success: bool,
    pub message: SharedString,
    pub messages: Vec<Arc<Message>>,
    pub has_more: bool,
    pub next_cursor: i64,
    pub next_offset: i32,
    pub total: i64,
    pub total_capped: bool,
    pub text_skipped: Vec<ChannelId>,
    pub partial: bool,
    pub retry_after_ms: i32,
    pub first_page: bool,
}

/// How a request leaves: the core call in the app, a recorder in tests. `done` hears a
/// failure to send.
pub type SearchSender = Rc<dyn Fn(SearchArgs, &mut App, SearchSent)>;

/// Told when a search request couldn't be sent.
pub type SearchSent = Box<dyn FnOnce(Result<(), UiError>, &mut App)>;

pub struct SearchStore {
    /// The community the results are for; `None` = nothing searched yet.
    pub server: Option<ServerId>,
    /// The results panel shows. Closing keeps the results: running the same search again
    /// just shows them.
    pub open: bool,
    pub query: SearchQuery,
    pub sort: SearchSort,
    /// The answer waited for / shown.
    pub seq: u32,
    pub status: SearchStatus,
    pub results: Vec<Arc<Message>>,
    /// First page's count; -1 = not counted.
    pub total: i64,
    pub total_capped: bool,
    pub has_more: bool,
    pub loading_more: bool,
    pub next_cursor: i64,
    pub next_offset: i32,
    /// Encrypted channels whose text wasn't searched.
    pub text_skipped: Vec<ChannelId>,
    pub partial: bool,
    pub error: Option<SharedString>,
    /// The result last jumped to (ringed in the list).
    pub jumped_id: i64,
    /// The header box's contents per community.
    drafts: HashMap<ServerId, SearchQuery>,
    /// Bumped to focus the header box (Ctrl+F).
    pub focus_nonce: u64,
    next_seq: u32,
    retries: u32,
    last_page: Page,
    /// The 8 s wait for an answer: exists while one is awaited.
    no_answer: Option<Task<()>>,
    /// A retry after a "too fast" answer: exists until it fires.
    retry: Option<Task<()>>,
    sender: SearchSender,
}

impl SearchStore {
    pub fn new(sender: SearchSender) -> SearchStore {
        SearchStore {
            server: None,
            open: false,
            query: SearchQuery::default(),
            sort: SearchSort::Newest,
            seq: 0,
            status: SearchStatus::Idle,
            results: Vec::new(),
            total: -1,
            total_capped: false,
            has_more: false,
            loading_more: false,
            next_cursor: 0,
            next_offset: 0,
            text_skipped: Vec::new(),
            partial: false,
            error: None,
            jumped_id: 0,
            drafts: HashMap::new(),
            focus_nonce: 0,
            next_seq: 1,
            retries: 0,
            last_page: Page::default(),
            no_answer: None,
            retry: None,
            sender,
        }
    }

    /// Replace how requests leave (fixtures and screens: a store with no core answers from the
    /// routed `MESSAGE_SEARCH_RES` only; a request that couldn't leave would turn it into an error).
    pub fn set_sender(&mut self, sender: SearchSender) {
        self.sender = sender;
    }

    pub fn draft(&self, server: ServerId) -> SearchQuery {
        self.drafts.get(&server).cloned().unwrap_or_default()
    }

    pub fn set_draft(&mut self, server: ServerId, q: SearchQuery, cx: &mut Context<Self>) {
        if self.drafts.get(&server) != Some(&q) {
            self.drafts.insert(server, q);
            cx.notify();
        }
    }

    pub fn request_focus(&mut self, cx: &mut Context<Self>) {
        self.focus_nonce += 1;
        cx.notify();
    }

    /// Run `q` on `server` and open the panel.
    pub fn run(&mut self, server: ServerId, q: &SearchQuery, cx: &mut Context<Self>) {
        if !q.is_searchable() {
            return;
        }
        let query = SearchQuery { text: q.text.trim().to_string().into(), filters: q.filters.clone() };
        // Reopening the same finished search costs the server nothing.
        if !self.open && self.server == Some(server) && self.status == SearchStatus::Done && self.query == query {
            self.open = true;
            cx.notify();
            return;
        }
        self.start(server, query, self.sort, cx);
    }

    pub fn set_sort(&mut self, sort: SearchSort, cx: &mut Context<Self>) {
        let Some(server) = self.server else { return };
        if sort == self.sort {
            return;
        }
        self.start(server, self.query.clone(), sort, cx);
    }

    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        if self.server.is_none() || !self.has_more || self.loading_more || self.status != SearchStatus::Done {
            return;
        }
        self.retries = 0;
        self.loading_more = true;
        cx.notify();
        self.send(Page { cursor: self.next_cursor, offset: self.next_offset }, cx);
    }

    pub fn retry_search(&mut self, cx: &mut Context<Self>) {
        if let Some(server) = self.server {
            self.start(server, self.query.clone(), self.sort, cx);
        }
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.open {
            self.open = false;
            cx.notify();
        }
    }

    pub fn set_jumped(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.jumped_id != id {
            self.jumped_id = id;
            cx.notify();
        }
    }

    fn start(&mut self, server: ServerId, query: SearchQuery, sort: SearchSort, cx: &mut Context<Self>) {
        self.no_answer = None;
        self.retry = None;
        self.retries = 0;
        // Relevance needs words to rank by, and the index (not the inside-words scan).
        let effective = if sort == SearchSort::Relevance && (search_words(&query.text).is_empty() || query.matches_inside()) {
            SearchSort::Newest
        } else {
            sort
        };
        self.server = Some(server);
        self.open = true;
        self.query = query;
        self.sort = effective;
        self.seq = self.next_seq;
        self.next_seq += 1;
        self.status = SearchStatus::Loading;
        self.results.clear();
        self.total = -1;
        self.total_capped = false;
        self.has_more = false;
        self.loading_more = false;
        self.next_cursor = 0;
        self.next_offset = 0;
        self.text_skipped.clear();
        self.partial = false;
        self.error = None;
        self.jumped_id = 0;
        cx.notify();
        self.send(Page::default(), cx);
    }

    fn send(&mut self, page: Page, cx: &mut Context<Self>) {
        let Some(server) = self.server else { return };
        self.last_page = page;
        let seq = self.seq;
        self.no_answer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NO_ANSWER).await;
            let _ = this.update(cx, |s, cx| {
                s.no_answer = None;
                if s.seq != seq || (s.status != SearchStatus::Loading && !s.loading_more) {
                    return;
                }
                s.status = SearchStatus::Error;
                s.loading_more = false;
                s.error = Some("The server didn't answer. It may need an update before it can search.".into());
                cx.notify();
            });
        }));
        let args = to_search_args(server, &self.query, self.sort, seq, page);
        let sender = self.sender.clone();
        let this = cx.weak_entity();
        cx.defer(move |cx| {
            sender(
                args,
                cx,
                Box::new(move |result, cx| {
                    let Err(e) = result else { return };
                    let _ = this.update(cx, |s, cx| {
                        if s.seq != seq {
                            return;
                        }
                        s.no_answer = None;
                        s.retry = None;
                        s.status = SearchStatus::Error;
                        s.loading_more = false;
                        let text = if e.text.is_empty() { SharedString::new_static("Couldn't reach the server.") } else { e.js_string() };
                        s.error = Some(text);
                        cx.notify();
                    });
                }),
            )
        });
    }

    /// `MESSAGE_SEARCH_RES` (`receive`). Answers to a superseded search are dropped.
    pub fn receive(&mut self, p: SearchAnswer, cx: &mut Context<Self>) {
        if Some(p.server) != self.server || p.seq != self.seq {
            return;
        }
        self.no_answer = None;
        if !p.success {
            // Rate limited / busy: the server says when to come back.
            if p.retry_after_ms > 0 && self.retries < MAX_RETRIES {
                self.retries += 1;
                let (seq, page) = (self.seq, self.last_page);
                let wait = Duration::from_millis(p.retry_after_ms.min(5000) as u64);
                self.retry = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |s, cx| {
                        s.retry = None;
                        if s.seq == seq {
                            s.send(page, cx);
                        }
                    });
                }));
                return;
            }
            self.status = SearchStatus::Error;
            self.loading_more = false;
            self.error = Some(if p.message.is_empty() { "Search failed.".into() } else { p.message });
            cx.notify();
            return;
        }
        self.retries = 0;
        let seen: HashSet<i64> = if p.first_page { HashSet::new() } else { self.results.iter().map(|m| m.id).collect() };
        let fresh = p.messages.into_iter().filter(|m| !seen.contains(&m.id));
        if p.first_page {
            self.results = fresh.collect();
            self.total = p.total;
            self.total_capped = p.total_capped;
            self.text_skipped = p.text_skipped;
        } else {
            self.results.extend(fresh);
        }
        self.status = SearchStatus::Done;
        self.loading_more = false;
        self.has_more = p.has_more;
        self.next_cursor = p.next_cursor;
        self.next_offset = p.next_offset;
        self.partial = p.partial;
        self.error = None;
        cx.notify();
    }

    /// Timers held (tests).
    pub fn waiting(&self) -> (bool, bool) {
        (self.no_answer.is_some(), self.retry.is_some())
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        let sender = self.sender.clone();
        let next_seq = self.next_seq;
        *self = SearchStore::new(sender);
        self.next_seq = next_seq;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::*;
    use crate::chat::message::testing::stored;

    fn day(s: &str) -> Day {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn query_rules() {
        let words = search_words("Héllo -bad \"exact phrase\" e-mail ok");
        assert_eq!(
            words,
            vec![
                SearchWord { folded: "hello".into(), prefix: true },
                SearchWord { folded: "exact".into(), prefix: false },
                SearchWord { folded: "phrase".into(), prefix: false },
                SearchWord { folded: "e".into(), prefix: false },
                SearchWord { folded: "mail".into(), prefix: true },
                SearchWord { folded: "ok".into(), prefix: false },
            ]
        );
        assert_eq!(fold("IŞIK Ünlü"), "isik unlu");
        assert_eq!(search_fragments("ab-c -x \"q r\""), vec!["ab-c".to_string(), "q r".to_string()]);
        let q = SearchQuery { text: "  ".into(), filters: vec![] };
        assert!(!q.is_searchable());
        assert!(!q.with_filter(SearchFilter::Inside).is_searchable());
        assert!(q.with_filter(SearchFilter::Has(HasKind::Image)).is_searchable());
        let q = SearchQuery { text: "-only".into(), filters: vec![] };
        assert!(!q.is_searchable(), "excluded words alone search nothing");
        // One date per operator; a second "before" replaces the first.
        let q = SearchQuery::default()
            .with_filter(SearchFilter::Before(day("2026-10-01")))
            .with_filter(SearchFilter::Before(day("2026-09-01")))
            .with_filter(SearchFilter::From { uid: 3, username: "carol".into() })
            .with_filter(SearchFilter::From { uid: 3, username: "carol2".into() })
            .with_filter(SearchFilter::Has(HasKind::Link))
            .with_filter(SearchFilter::Has(HasKind::Image));
        assert_eq!(q.filters.len(), 4);
        let args = to_search_args(ServerId::from_central(1), &q, SearchSort::Oldest, 4, Page::default());
        assert_eq!((args.has, args.author_uids.clone(), args.limit, args.seq), (34, vec![3], PAGE_SIZE, 4));
        assert_eq!(args.before, day_start(day("2026-09-01"), 0));
        assert_eq!(args.after, 0);
        // During: that day, both bounds.
        let (after, before) = date_bounds(&[SearchFilter::During(day("2026-10-03"))]);
        assert_eq!(before - after, 86_400);
        let t = day("2026-11-26");
        assert_eq!(parse_day("today", t), Some(t));
        assert_eq!(parse_day("Yesterday", t), Some(day("2026-11-25")));
        assert_eq!(parse_day("2026-10-3", t), Some(day("2026-10-03")));
        assert_eq!(parse_day("2026-02-30", t), None);
        assert_eq!(parse_day("26-10-03", t), None);
    }

    type Sent = Rc<RefCell<Vec<SearchArgs>>>;

    fn store(cx: &mut TestAppContext) -> (Entity<SearchStore>, Sent) {
        let sent: Sent = Rc::default();
        let s = sent.clone();
        let store = cx.new(|_| SearchStore::new(Rc::new(move |args, _, _| s.borrow_mut().push(args))));
        (store, sent)
    }

    fn answer(server: ServerId, seq: u32, ids: &[i64], first: bool, has_more: bool) -> SearchAnswer {
        SearchAnswer {
            server,
            seq,
            success: true,
            message: "".into(),
            messages: ids.iter().map(|id| stored(*id, 1, "x", 0)).collect(),
            has_more,
            next_cursor: ids.last().copied().unwrap_or(0),
            next_offset: 0,
            total: 40,
            total_capped: false,
            text_skipped: vec!["vault".into()],
            partial: false,
            retry_after_ms: 0,
            first_page: first,
        }
    }

    #[gpui::test]
    fn a_new_search_supersedes_and_pages_dedupe(cx: &mut TestAppContext) {
        let (store, sent) = store(cx);
        let server = ServerId::from_central(1);
        let q = SearchQuery { text: "gaming".into(), filters: vec![] };
        store.update(cx, |s, cx| s.run(server, &q, cx));
        cx.run_until_parked();
        let first_seq = sent.borrow()[0].seq;
        // Superseded before its answer.
        let q2 = SearchQuery { text: "raid".into(), filters: vec![] };
        store.update(cx, |s, cx| s.run(server, &q2, cx));
        cx.run_until_parked();
        let seq = sent.borrow()[1].seq;
        assert!(seq > first_seq);
        store.update(cx, |s, cx| {
            s.receive(answer(server, first_seq, &[1, 2], true, false), cx);
            assert_eq!(s.status, SearchStatus::Loading, "the old answer is dropped");
            s.receive(answer(server, seq, &[10, 9, 8], true, true), cx);
            assert_eq!((s.status, s.results.len(), s.total), (SearchStatus::Done, 3, 40));
            assert_eq!(s.text_skipped, vec![ChannelId::from("vault")]);
            assert_eq!(s.waiting(), (false, false), "no timer at rest");
            s.load_more(cx);
            s.load_more(cx); // once
        });
        cx.run_until_parked();
        assert_eq!(sent.borrow().len(), 3);
        assert_eq!(sent.borrow()[2].cursor, 8);
        store.update(cx, |s, cx| {
            s.receive(answer(server, seq, &[8, 7, 6], false, false), cx);
            assert_eq!(s.results.iter().map(|m| m.id).collect::<Vec<_>>(), vec![10, 9, 8, 7, 6]);
            // Closing keeps the results; the same search reopens them without asking.
            s.close(cx);
            s.run(server, &q2, cx);
            assert!(s.open);
        });
        cx.run_until_parked();
        assert_eq!(sent.borrow().len(), 3);
        // Relevance falls back to newest without words to rank.
        store.update(cx, |s, cx| {
            s.set_sort(SearchSort::Relevance, cx);
            assert_eq!(s.sort, SearchSort::Relevance);
            let only_filters = SearchQuery::default().with_filter(SearchFilter::Has(HasKind::Image));
            s.run(server, &only_filters, cx);
            assert_eq!(s.sort, SearchSort::Newest);
        });
    }

    #[gpui::test]
    fn busy_answers_retry_and_silence_errors(cx: &mut TestAppContext) {
        let (store, sent) = store(cx);
        let server = ServerId::from_central(1);
        let q = SearchQuery { text: "x y z".into(), filters: vec![] };
        store.update(cx, |s, cx| s.run(server, &q, cx));
        cx.run_until_parked();
        let seq = sent.borrow()[0].seq;
        for round in 1..=3 {
            store.update(cx, |s, cx| {
                let mut busy = answer(server, seq, &[], true, false);
                busy.success = false;
                busy.retry_after_ms = 700;
                s.receive(busy, cx);
                assert_eq!(s.waiting(), (false, true));
            });
            cx.executor().advance_clock(Duration::from_millis(700));
            cx.run_until_parked();
            assert_eq!(sent.borrow().len(), 1 + round);
        }
        store.update(cx, |s, cx| {
            let mut busy = answer(server, seq, &[], true, false);
            busy.success = false;
            busy.retry_after_ms = 700;
            s.receive(busy, cx);
            assert_eq!(s.status, SearchStatus::Error, "three retries, then the error");
        });
        // A server that never answers: an error after 8 s.
        store.update(cx, |s, cx| s.retry_search(cx));
        cx.executor().advance_clock(NO_ANSWER);
        cx.run_until_parked();
        store.read_with(cx, |s, _| {
            assert_eq!(s.status, SearchStatus::Error);
            assert!(s.error.as_deref().unwrap().contains("didn't answer"));
            assert_eq!(s.waiting(), (false, false));
        });
    }
}

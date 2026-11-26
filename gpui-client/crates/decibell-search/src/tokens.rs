//! Typed filters in the search box ("from:dana", "has:image", "before:2026-10-01") and how they
//! turn into chips: `features/search/searchTokens.ts`, function for function. The query itself
//! (chips, words, dates, the request) is the model's (`decibell_model::chat::search`, the port of
//! `searchQuery.ts`).
//!
//! Offsets are bytes of the box's text (the renderer's are UTF-16 units; both only ever slice
//! the same text with them).

use decibell_model::chat::search::{Day, HasKind, SearchFilter, SearchQuery, parse_day};
use decibell_model::stores::community::{Channel, ChannelKind};
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Uid};
use gpui::SharedString;

/// A filter operator the box understands (`OPS`; "inside" is an option, never typed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    From,
    In,
    Has,
    Before,
    During,
    After,
}

impl Op {
    /// `OPS`, in the menu's order.
    pub const ALL: [Op; 6] = [Op::From, Op::In, Op::Has, Op::Before, Op::During, Op::After];

    pub fn name(self) -> &'static str {
        match self {
            Op::From => "from",
            Op::In => "in",
            Op::Has => "has",
            Op::Before => "before",
            Op::During => "during",
            Op::After => "after",
        }
    }

    /// `before:`, `during:`, `after:` (`DATE_OPS`).
    pub fn is_date(self) -> bool {
        matches!(self, Op::Before | Op::During | Op::After)
    }

    /// The operator a word names, case-insensitively.
    pub fn parse(word: &str) -> Option<Op> {
        Op::ALL.into_iter().find(|op| op.name().eq_ignore_ascii_case(word))
    }

    /// The chip a date operator makes for `day`.
    pub fn date_filter(self, day: Day) -> Option<SearchFilter> {
        match self {
            Op::Before => Some(SearchFilter::Before(day)),
            Op::During => Some(SearchFilter::During(day)),
            Op::After => Some(SearchFilter::After(day)),
            _ => None,
        }
    }
}

/// The filter being typed at the end of the text (`TypedToken`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypedToken {
    pub op: Op,
    pub value: String,
    /// Where it starts in the text (bytes).
    pub start: usize,
}

/// JavaScript's `\s` (the renderer's regexes): Unicode white space plus U+FEFF.
pub fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

/// `/(?:^|\s)(from|in|has|before|during|after):(\S*)$/i`: the operator and value after the last
/// white space (or the whole text), if it is one.
pub fn trailing_token(text: &str) -> Option<TypedToken> {
    let start = text.char_indices().rev().find(|(_, c)| is_js_space(*c)).map_or(0, |(at, c)| at + c.len_utf8());
    let word = &text[start..];
    let (op, value) = word.split_once(':')?;
    let op = Op::parse(op)?;
    Some(TypedToken { op, value: value.to_string(), start })
}

/// A member a `from:` can name: the account, its current name (`nameOf(uid, username)`) and the
/// community nickname ("" = none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberRef {
    pub uid: Uid,
    pub name: SharedString,
    pub nickname: SharedString,
}

/// A text channel an `in:` can name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRef {
    pub id: ChannelId,
    pub name: SharedString,
    pub encrypted: bool,
}

/// What filters can name (`TokenContext`): members (the loaded roster, then what the server's
/// member search found) and the readable text channels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenContext {
    pub members: Vec<MemberRef>,
    pub channels: Vec<ChannelRef>,
}

/// `HAS_ALIASES`.
pub fn has_kind_for(value: &str) -> Option<HasKind> {
    Some(match value.to_lowercase().as_str() {
        "link" | "links" | "url" => HasKind::Link,
        "file" | "files" | "attachment" | "attachments" => HasKind::File,
        "image" | "images" | "picture" | "pictures" => HasKind::Image,
        "video" | "videos" => HasKind::Video,
        "audio" | "sound" | "sounds" => HasKind::Audio,
        "document" | "documents" | "doc" | "docs" => HasKind::Document,
        _ => return None,
    })
}

/// `HAS_KINDS`' wire names (`"link"`, …): what `has:` values start with.
pub fn has_name(kind: HasKind) -> &'static str {
    match kind {
        HasKind::Link => "link",
        HasKind::File => "file",
        HasKind::Image => "image",
        HasKind::Video => "video",
        HasKind::Audio => "audio",
        HasKind::Document => "document",
    }
}

/// JavaScript's `String.prototype.trim` (white space and line terminators).
pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// A complete typed filter as a chip, or `None` if it names nothing known (`resolveToken`).
pub fn resolve_token(op: Op, value: &str, ctx: &TokenContext, today: Day) -> Option<SearchFilter> {
    let v = js_trim(value);
    if v.is_empty() {
        return None;
    }
    match op {
        Op::From => {
            let lower = v.strip_prefix('@').unwrap_or(v).to_lowercase();
            let m = ctx
                .members
                .iter()
                .find(|m| m.name.to_lowercase() == lower)
                .or_else(|| ctx.members.iter().find(|m| !m.nickname.is_empty() && m.nickname.to_lowercase() == lower))?;
            Some(SearchFilter::From { uid: m.uid, username: m.name.clone() })
        }
        Op::In => {
            let lower = v.strip_prefix('#').unwrap_or(v).to_lowercase();
            let c = ctx.channels.iter().find(|c| c.name.to_lowercase() == lower)?;
            Some(SearchFilter::In { channel: c.id.clone() })
        }
        Op::Has => has_kind_for(v).map(SearchFilter::Has),
        op => op.date_filter(parse_day(v, today)?),
    }
}

/// Turns every complete, known filter in the text into a chip (when a search runs); unknown ones
/// stay as text (`chipTokens`). The words left are joined by single spaces.
pub fn chip_tokens(q: &SearchQuery, ctx: &TokenContext, today: Day) -> SearchQuery {
    let mut query = SearchQuery { text: SharedString::default(), filters: q.filters.clone() };
    let mut kept: Vec<&str> = Vec::new();
    for word in q.text.split(is_js_space).filter(|w| !w.is_empty()) {
        // `/^(from|in|has|before|during|after):(.+)$/i`
        let chip = word
            .split_once(':')
            .filter(|(_, value)| !value.is_empty())
            .and_then(|(op, value)| Some((Op::parse(op)?, value)))
            .and_then(|(op, value)| resolve_token(op, value, ctx, today));
        match chip {
            Some(f) => query = query.with_filter(f),
            None => kept.push(word),
        }
    }
    query.text = kept.join(" ").into();
    query
}

/// Text channels a member can search: VIEW + READ_HISTORY there (`isSearchableChannel`; the
/// renderer's `hasBits` asks for both bits, the administrator bit alone doesn't do).
pub fn is_searchable_channel(channel: &Channel, is_owner: bool) -> bool {
    if channel.kind != ChannelKind::Text {
        return false;
    }
    if is_owner {
        return true;
    }
    let bits = perm::VIEW_CHANNEL | perm::READ_HISTORY;
    channel.my_permissions & bits == bits
}

/// The category a channel sits under, "" when none (`categoryOf`).
pub fn category_of(channels: &[Channel], id: &str) -> SharedString {
    let mut category = SharedString::default();
    for c in channels {
        if c.kind == ChannelKind::Category {
            category = c.name.clone();
        }
        if &*c.id == id {
            return category;
        }
    }
    SharedString::default()
}

/// What `onText` does with the box's new text: a finished filter followed by a space becomes a
/// chip right away (`/(^|\s)(from|in|has|before|during|after):(\S+)\s$/i`), else the text is
/// taken as it is (and the menu opens).
pub fn on_text(text: &str, draft: &SearchQuery, ctx: &TokenContext, today: Day) -> (SearchQuery, bool) {
    if let Some(last) = text.chars().next_back().filter(|c| is_js_space(*c)) {
        let body = &text[..text.len() - last.len_utf8()];
        // The word before the final space: after the previous white space, non-empty.
        let start = body.char_indices().rev().find(|(_, c)| is_js_space(*c)).map_or(0, |(at, c)| at + c.len_utf8());
        let word = &body[start..];
        if let Some((op, value)) = word.split_once(':')
            && !value.is_empty()
            && let Some(op) = Op::parse(op)
            && let Some(f) = resolve_token(op, value, ctx, today)
        {
            let rest = body[..start].trim_end_matches(is_js_space);
            let text: SharedString = if rest.is_empty() { SharedString::default() } else { format!("{rest} ").into() };
            return (SearchQuery { text, filters: draft.filters.clone() }.with_filter(f), false);
        }
    }
    (SearchQuery { text: text.to_string().into(), filters: draft.filters.clone() }, true)
}

/// `addChip`: the typed token (if any) gives way to the chip; the text keeps a trailing space.
pub fn add_chip(draft: &SearchQuery, token: Option<&TypedToken>, f: SearchFilter) -> SearchQuery {
    let text: &str = match token {
        Some(token) => draft.text[..token.start.min(draft.text.len())].trim_end_matches(is_js_space),
        None => &draft.text,
    };
    let text: SharedString = if text.is_empty() { SharedString::default() } else { format!("{text} ").into() };
    SearchQuery { text, filters: draft.filters.clone() }.with_filter(f)
}

/// `startOp`: the operator appended to the text, after one space.
pub fn start_op(draft: &SearchQuery, op: Op) -> SearchQuery {
    let base = draft.text.trim_end_matches(is_js_space);
    let text = if base.is_empty() { format!("{}:", op.name()) } else { format!("{base} {}:", op.name()) };
    SearchQuery { text: text.into(), filters: draft.filters.clone() }
}

/// The inside-words option toggled (`pick` of the "inside" row).
pub fn toggle_inside(draft: &SearchQuery) -> SearchQuery {
    if draft.matches_inside() {
        SearchQuery { text: draft.text.clone(), filters: draft.filters.iter().filter(|f| **f != SearchFilter::Inside).cloned().collect() }
    } else {
        draft.with_filter(SearchFilter::Inside)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> Day {
        Day::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    pub(crate) fn ctx() -> TokenContext {
        TokenContext {
            members: vec![
                MemberRef { uid: 1, name: "alice".into(), nickname: "".into() },
                MemberRef { uid: 2, name: "bob".into(), nickname: "Bobby".into() },
                MemberRef { uid: 3, name: "Dana".into(), nickname: "".into() },
            ],
            channels: vec![
                ChannelRef { id: "general".into(), name: "general".into(), encrypted: false },
                ChannelRef { id: "off-topic".into(), name: "off-topic".into(), encrypted: false },
                ChannelRef { id: "vault".into(), name: "vault".into(), encrypted: true },
            ],
        }
    }

    #[test]
    fn the_trailing_token() {
        assert_eq!(trailing_token("from:"), Some(TypedToken { op: Op::From, value: "".into(), start: 0 }));
        assert_eq!(trailing_token("raid FROM:da"), Some(TypedToken { op: Op::From, value: "da".into(), start: 5 }));
        assert_eq!(trailing_token("raid from:da "), None, "a space after it ends it");
        assert_eq!(trailing_token("xfrom:da"), None);
        assert_eq!(trailing_token("in:#gen"), Some(TypedToken { op: Op::In, value: "#gen".into(), start: 0 }));
        assert_eq!(trailing_token("before:2026-10-01"), Some(TypedToken { op: Op::Before, value: "2026-10-01".into(), start: 0 }));
        assert_eq!(trailing_token("from:in:x"), Some(TypedToken { op: Op::From, value: "in:x".into(), start: 0 }));
        assert_eq!(trailing_token("inside:"), None);
        assert_eq!(trailing_token("ölçü has:im").map(|t| t.start), Some("ölçü ".len()));
    }

    #[test]
    fn resolving_and_chips() {
        let ctx = ctx();
        let today = day("2026-11-26");
        assert_eq!(resolve_token(Op::From, "@ALICE", &ctx, today), Some(SearchFilter::From { uid: 1, username: "alice".into() }));
        assert_eq!(resolve_token(Op::From, "bobby", &ctx, today), Some(SearchFilter::From { uid: 2, username: "bob".into() }));
        assert_eq!(resolve_token(Op::From, "bo", &ctx, today), None);
        assert_eq!(resolve_token(Op::In, "#Off-Topic", &ctx, today), Some(SearchFilter::In { channel: "off-topic".into() }));
        assert_eq!(resolve_token(Op::Has, "Pictures", &ctx, today), Some(SearchFilter::Has(HasKind::Image)));
        assert_eq!(resolve_token(Op::During, "yesterday", &ctx, today), Some(SearchFilter::During(day("2026-11-25"))));
        assert_eq!(resolve_token(Op::After, " ", &ctx, today), None);
        let q = chip_tokens(&SearchQuery { text: "raid  from:dana has:nothing  in:general  gg".into(), filters: vec![] }, &ctx, today);
        assert_eq!(q.text.as_ref(), "raid has:nothing gg");
        assert_eq!(q.filters, vec![SearchFilter::From { uid: 3, username: "Dana".into() }, SearchFilter::In { channel: "general".into() }]);
    }

    #[test]
    fn typing_a_finished_filter_makes_a_chip() {
        let ctx = ctx();
        let today = day("2026-11-26");
        let empty = SearchQuery::default();
        let (q, menu) = on_text("raid has:image ", &empty, &ctx, today);
        assert_eq!((q.text.as_ref(), q.filters.clone(), menu), ("raid ", vec![SearchFilter::Has(HasKind::Image)], false));
        let (q, menu) = on_text("has:imag ", &empty, &ctx, today);
        assert_eq!((q.text.as_ref(), q.filters.len(), menu), ("has:imag ", 0, true));
        let (q, _) = on_text("in:general ", &empty, &ctx, today);
        assert_eq!((q.text.as_ref(), q.filters.clone()), ("", vec![SearchFilter::In { channel: "general".into() }]));
        let draft = SearchQuery { text: "raid from:da".into(), filters: vec![] };
        let token = trailing_token(&draft.text).unwrap();
        let q = add_chip(&draft, Some(&token), SearchFilter::From { uid: 3, username: "Dana".into() });
        assert_eq!(q.text.as_ref(), "raid ");
        assert_eq!(start_op(&SearchQuery { text: "raid  ".into(), filters: vec![] }, Op::Has).text.as_ref(), "raid has:");
        assert_eq!(start_op(&SearchQuery::default(), Op::Before).text.as_ref(), "before:");
        let on = toggle_inside(&SearchQuery::default());
        assert!(on.matches_inside());
        assert!(!toggle_inside(&on).matches_inside());
    }
}

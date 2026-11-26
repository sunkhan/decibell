//! The search grammar against the renderer's own: `src/renderer_grammar.json` is what the real
//! `searchTokens.ts`, `searchQuery.ts`, `highlight.tsx` and `DatePicker.tsx` give for a table of
//! inputs (`tools/electron-captures/pages/search-grammar.tsx` says how to regenerate it). This
//! crate's ports (`tokens`, `highlight`, `calendar`) and the model's (`decibell_model::chat::search`:
//! `fold`, `search_words`, `search_fragments`, `parse_day`, `with_filter`, `to_search_args`) must
//! give the same, except the model's `fold` on the cases listed in `FOLD_DIFFERS`.

use chrono::{NaiveDate, Weekday};
use decibell_model::chat::search::{
    Day, Page, SearchFilter, SearchQuery, SearchSort, fold, parse_day, search_fragments, search_words, to_search_args,
};
use decibell_model::ServerId;
use decibell_search::calendar;
use decibell_search::highlight::Highlighter;
use decibell_search::tokens::{ChannelRef, MemberRef, Op, TokenContext, chip_tokens, has_kind_for, has_name, resolve_token, trailing_token};
use serde_json::{Value, json};

/// Texts whose fold the model gives differently from the renderer: the renderer decomposes (NFD)
/// and drops combining marks, so letters with a stroke or bar (Đ, Ł, Ø, Ħ, Ŧ…) stay themselves,
/// and Greek / other scripts lose their accents too. The model maps a table of precomposed
/// Latin letters to their base without a Unicode normalizer (docs/model.md §15.5 (7)): it folds
/// Đ → d, Ł → l, Ø → o (the renderer keeps đ, ł, ø) and keeps Greek tonos (ά, where the renderer
/// gives α).
const FOLD_DIFFERS: [&str; 2] = ["Đorđe Łódź", "ά έ"];

fn grammar() -> Value {
    serde_json::from_str(include_str!("../src/renderer_grammar.json")).expect("renderer_grammar.json")
}

fn day(s: &str) -> Day {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect(s)
}

/// The page's `CTX` (`search-grammar.tsx`).
fn ctx() -> TokenContext {
    let m = |uid, name: &str, nickname: &str| MemberRef { uid, name: name.into(), nickname: nickname.into() };
    let c = |id: &str, encrypted| ChannelRef { id: id.into(), name: id.into(), encrypted };
    TokenContext {
        members: vec![m(1, "alice", ""), m(2, "bob", "Bobby"), m(3, "Dana", ""), m(4, "İlkay", ""), m(5, "ışık", "Işık")],
        channels: vec![c("general", false), c("off-topic", false), c("vault", true), c("Oyun-Gecesi", false)],
    }
}

/// A chip in the renderer's shape.
fn filter_json(f: &SearchFilter) -> Value {
    match f {
        SearchFilter::From { uid, username } => json!({ "kind": "from", "uid": uid, "username": username.as_ref() }),
        SearchFilter::In { channel } => json!({ "kind": "in", "channelId": channel.as_ref() }),
        SearchFilter::Has(kind) => json!({ "kind": "has", "has": has_name(*kind) }),
        SearchFilter::Before(d) => json!({ "kind": "before", "date": d.format("%Y-%m-%d").to_string() }),
        SearchFilter::During(d) => json!({ "kind": "during", "date": d.format("%Y-%m-%d").to_string() }),
        SearchFilter::After(d) => json!({ "kind": "after", "date": d.format("%Y-%m-%d").to_string() }),
        SearchFilter::Inside => json!({ "kind": "inside" }),
    }
}

fn filter_from_json(v: &Value) -> SearchFilter {
    let date = || day(v["date"].as_str().unwrap());
    match v["kind"].as_str().unwrap() {
        "from" => SearchFilter::From { uid: v["uid"].as_i64().unwrap(), username: v["username"].as_str().unwrap().to_string().into() },
        "in" => SearchFilter::In { channel: v["channelId"].as_str().unwrap().to_string().into() },
        "has" => SearchFilter::Has(has_kind_for(v["has"].as_str().unwrap()).unwrap()),
        "before" => SearchFilter::Before(date()),
        "during" => SearchFilter::During(date()),
        "after" => SearchFilter::After(date()),
        _ => SearchFilter::Inside,
    }
}

fn op(name: &str) -> Op {
    Op::parse(name).expect(name)
}

#[test]
fn tokens_words_and_folds_match_the_renderer() {
    let g = grammar();
    let today = day("2026-11-26");
    let mut differs = Vec::new();
    for case in g["texts"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        // The trailing token: operator, value, and the text before it.
        let token = trailing_token(text).map(|t| json!({ "op": t.op.name(), "value": t.value, "before": &text[..t.start] }));
        assert_eq!(token.unwrap_or(Value::Null), case["token"], "trailingToken({text:?})");
        let words: Vec<Value> = search_words(text).into_iter().map(|w| json!({ "folded": w.folded, "prefix": w.prefix })).collect();
        let fragments = search_fragments(text);
        let folded = fold(text);
        if FOLD_DIFFERS.contains(&text) {
            assert_ne!(folded, case["fold"].as_str().unwrap(), "{text:?} now folds as the renderer does: take it off FOLD_DIFFERS");
            differs.push(text);
            continue;
        }
        assert_eq!(folded, case["fold"].as_str().unwrap(), "fold({text:?})");
        assert_eq!(Value::Array(words), case["words"], "searchWords({text:?})");
        assert_eq!(json!(fragments), case["fragments"], "searchFragments({text:?})");
        let q = SearchQuery { text: text.to_string().into(), filters: vec![] };
        assert_eq!(json!(q.is_searchable()), case["searchable"], "isSearchable({text:?})");
        assert_eq!(json!(q.with_filter(SearchFilter::Inside).is_searchable()), case["searchableInside"], "isSearchable inside ({text:?})");
        assert_eq!(json!(has_kind_for(text).map(has_name)), case["has"], "hasKindFor({text:?})");
    }
    assert_eq!(differs.len(), FOLD_DIFFERS.len());
    let _ = today;
}

#[test]
fn chips_resolving_and_requests_match_the_renderer() {
    let g = grammar();
    // The page's `now` is the real date; the cases' dates are absolute, and "today" / "yesterday"
    // are resolved against it there: compare those through `parse_day` below instead.
    let today = decibell_model::chat::search::today();
    let ctx = ctx();
    for case in g["queries"].as_array().unwrap() {
        let text = case["text"].as_str().unwrap();
        let q = chip_tokens(&SearchQuery { text: text.to_string().into(), filters: vec![] }, &ctx, today);
        let filters: Vec<Value> = q.filters.iter().map(filter_json).collect();
        let expected = &case["result"];
        // `before:today` resolves to the page's today: only its kind is compared.
        let strip = |v: &Value| -> Vec<Value> {
            v.as_array().unwrap().iter().map(|f| if text.contains("today") && f["kind"] == "before" { json!({ "kind": "before" }) } else { f.clone() }).collect()
        };
        assert_eq!(q.text.as_ref(), expected["text"].as_str().unwrap(), "chipTokens({text:?}).text");
        assert_eq!(strip(&Value::Array(filters)), strip(&expected["filters"]), "chipTokens({text:?}).filters");
        let args = to_search_args(ServerId::from_central(1), &q, SearchSort::Newest, 7, Page::default());
        assert_eq!(args.query, case["args"]["query"].as_str().unwrap());
        assert_eq!(json!(args.channel_ids), case["args"]["channelIds"]);
        assert_eq!(json!(args.author_uids), case["args"]["authorUids"]);
        assert_eq!(json!(args.has), case["args"]["has"]);
        assert_eq!(json!(args.match_within_words), case["args"]["matchWithinWords"]);
    }
    for case in g["resolve"].as_array().unwrap() {
        let (name, value) = (case["op"].as_str().unwrap(), case["value"].as_str().unwrap());
        if value == "today" || value == "yesterday" {
            continue;
        }
        let got = resolve_token(op(name), value, &ctx, today).map(|f| filter_json(&f)).unwrap_or(Value::Null);
        assert_eq!(got, case["filter"], "resolveToken({name}, {value:?})");
    }
    for case in g["days"].as_array().unwrap() {
        let (input, now) = (case["input"].as_str().unwrap(), case["now"].as_str().unwrap());
        let got = parse_day(input, day(now)).map(|d| d.format("%Y-%m-%d").to_string());
        assert_eq!(json!(got), case["day"], "parseDay({input:?}, {now})");
    }
    // withFilter, one chip after another.
    let mut q = SearchQuery::default();
    let steps = g["withFilter"].as_array().unwrap();
    let added = [
        json!({ "kind": "from", "uid": 3, "username": "carol" }),
        json!({ "kind": "from", "uid": 3, "username": "carol2" }),
        json!({ "kind": "before", "date": "2026-10-01" }),
        json!({ "kind": "before", "date": "2026-09-01" }),
        json!({ "kind": "has", "has": "link" }),
        json!({ "kind": "has", "has": "image" }),
        json!({ "kind": "inside" }),
        json!({ "kind": "inside" }),
        json!({ "kind": "in", "channelId": "general" }),
    ];
    for (step, f) in steps.iter().zip(added.iter()) {
        q = q.with_filter(filter_from_json(f));
        assert_eq!(Value::Array(q.filters.iter().map(filter_json).collect()), *step, "withFilter after {f}");
    }
}

#[test]
fn highlights_match_the_renderer() {
    for case in grammar()["highlight"].as_array().unwrap() {
        let (query, text, inside) = (case["query"].as_str().unwrap(), case["text"].as_str().unwrap(), case["inside"].as_bool().unwrap());
        let h = if inside { Highlighter::fragments(search_fragments(query)) } else { Highlighter::words(&search_words(query)) };
        let marks: Option<Vec<String>> = h.map(|h| h.marks(text).into_iter().map(|r| text[r].to_string()).collect());
        assert_eq!(json!(marks), case["marks"], "highlight {query:?} in {text:?} (inside: {inside})");
    }
}

#[test]
fn the_date_picker_grid_matches_the_renderer() {
    let before = decibell_model::regional::Regional::pin(Some(decibell_model::regional::Regional::en_us()));
    let regional = decibell_model::regional::Regional::get();
    for case in grammar()["calendars"].as_array().unwrap() {
        let active = day(case["active"].as_str().unwrap());
        // en-US (run.sh): Sunday first.
        let days: Vec<String> = calendar::cells(active, Weekday::Sun).iter().map(|c| c.day.format("%Y-%m-%d").to_string()).collect();
        assert_eq!(json!(days), case["days"], "the grid of {active}");
        assert_eq!(json!(calendar::weekday_heads(Weekday::Sun)), case["heads"]);
        assert_eq!(regional.month_year(chrono::Datelike::year(&active), chrono::Datelike::month(&active) as u8), case["title"].as_str().unwrap());
        assert_eq!(regional.first_weekday(), Weekday::Sun);
    }
    decibell_model::regional::Regional::pin(before);
}

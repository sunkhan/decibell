//! The search screens' deterministic state: the chat fixture (the shell's events, signed in as
//! perf1 in Daily Gaming, in voice in Squad, `#general`'s newest page of the 10,000-message
//! channel) plus what each screen adds: a channel update, a channel's empty history, the box's
//! draft and focus, and a search with its answer routed through the model's real router (a store
//! whose requests go nowhere: there is no core).
//!
//! [`json`] exports the same as the Electron capture pages replay it
//! (`tools/electron-captures/pages/search-*.tsx`, `_search.tsx`): the events, the navigation, the
//! draft in the renderer's shape, the search and its answer, and the pinned "today".

use std::rc::Rc;

use chrono::NaiveDate;
use decibell_core::events::*;
use decibell_model::chat::search::{Day, HasKind, SearchFilter, SearchQuery};
use decibell_model::{ChannelId, Model, ServerId};
use decibell_shell::fixture as shell;
use gpui::{App, Global};

use crate::format::PinnedToday;

/// The day the screens take as today (and the Electron pages' `Date`): after every fixture
/// message, so no result reads "Today" or "Yesterday" in either client whatever the real date.
pub fn today() -> Day {
    NaiveDate::from_ymd_opt(2026, 11, 26).expect("a date")
}

/// What a screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// `#general`: name, topic, the members toggle and the empty box at the right end.
    HeaderText,
    /// `#vault`: the E2EE badge (its history: none yet).
    HeaderEncrypted,
    /// `#general` with a topic longer than the header.
    HeaderLongTopic,
    /// The box focused, empty: the filters and the inside-words option.
    BoxEmpty,
    /// "gg" typed: "Search for gg" first.
    BoxSuggest,
    /// `from:perf`: members.
    BoxSuggestFrom,
    /// `in:`: the text channels, this one first.
    BoxSuggestIn,
    /// `has:`: the six kinds.
    BoxSuggestHas,
    /// Four chips and "gg", not focused.
    BoxChips,
    /// `before:`: the two days and the month grid.
    DatePicker,
    /// "wayland": results across two channels, "10,000+", the encrypted channel skipped.
    Results,
    /// Nothing matched.
    ResultsEmpty,
    /// The server refused the search ("You're searching too fast.").
    ResultsRefused,
}

impl Stage {
    pub fn page(self) -> &'static str {
        match self {
            Stage::HeaderText => "header-text",
            Stage::HeaderEncrypted => "header-encrypted",
            Stage::HeaderLongTopic => "header-long-topic",
            Stage::BoxEmpty => "search-box-empty",
            Stage::BoxSuggest => "search-box-suggest",
            Stage::BoxSuggestFrom => "search-box-suggest-from",
            Stage::BoxSuggestIn => "search-box-suggest-in",
            Stage::BoxSuggestHas => "search-box-suggest-has",
            Stage::BoxChips => "search-box-chips",
            Stage::DatePicker => "date-picker",
            Stage::Results => "search-results",
            Stage::ResultsEmpty => "search-results-empty",
            Stage::ResultsRefused => "search-results-refused",
        }
    }

    pub const ALL: [Stage; 13] = [
        Stage::HeaderText,
        Stage::HeaderEncrypted,
        Stage::HeaderLongTopic,
        Stage::BoxEmpty,
        Stage::BoxSuggest,
        Stage::BoxSuggestFrom,
        Stage::BoxSuggestIn,
        Stage::BoxSuggestHas,
        Stage::BoxChips,
        Stage::DatePicker,
        Stage::Results,
        Stage::ResultsEmpty,
        Stage::ResultsRefused,
    ];
}

/// The screens' box takes the focus once it is built (a capture can't click).
pub struct FocusOnMount;

impl Global for FocusOnMount {}

const LONG_TOPIC: &str = "Anything goes — be nice. Game nights are Fridays at 9 pm in Squad; post clips in #clips, keep spoilers \
behind a warning, and ping a moderator if something needs a look. New here? Say hi and tell us what you play.";

/// Permissions of an @everyone member in an ordinary channel (the shell fixture's `MEMBER`).
const MEMBER: u64 = 8192 | 16384 | 1024 | 32768 | 8388608 | 2048 | 16777216 | 4096 | 2097152 | 4194304;

fn channel_info(id: &str, topic: &str, encrypted: bool) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: id.into(),
        r#type: "text".into(),
        voice_bitrate_kbps: 0,
        retention_days_text: 0,
        retention_days_image: 0,
        retention_days_video: 0,
        retention_days_document: 0,
        retention_days_audio: 0,
        my_permissions: MEMBER,
        slowmode_seconds: 0,
        encrypted,
        permissions_synced: false,
        category_sync: false,
        topic: topic.into(),
    }
}

fn history(channel: &str, messages: Vec<ChannelMessagePayload>, has_more: bool) -> CoreEvent {
    CoreEvent::ChannelHistoryReceived(ChannelHistoryReceivedPayload {
        server_id: shell::DAILY_GAMING.to_string(),
        channel_id: channel.into(),
        messages,
        has_more,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 0,
    })
}

/// The channel shown and the events the stage adds to the chat fixture's.
fn extra(stage: Stage) -> (&'static str, Vec<CoreEvent>) {
    match stage {
        Stage::HeaderEncrypted => ("vault", vec![history("vault", Vec::new(), false)]),
        Stage::HeaderLongTopic => (
            "general",
            vec![CoreEvent::ChannelUpdated(ChannelUpdatedPayload {
                server_id: shell::DAILY_GAMING.to_string(),
                success: true,
                message: String::new(),
                channel: Some(channel_info("general", LONG_TOPIC, false)),
            })],
        ),
        _ => ("general", Vec::new()),
    }
}

fn q(text: &str, filters: Vec<SearchFilter>) -> SearchQuery {
    SearchQuery { text: text.to_string().into(), filters }
}

/// The box's draft and whether it has the focus.
fn draft(stage: Stage) -> (SearchQuery, bool) {
    match stage {
        Stage::BoxEmpty => (q("", vec![]), true),
        Stage::BoxSuggest => (q("gg", vec![]), true),
        Stage::BoxSuggestFrom => (q("from:perf", vec![]), true),
        Stage::BoxSuggestIn => (q("in:", vec![]), true),
        Stage::BoxSuggestHas => (q("has:", vec![]), true),
        Stage::BoxChips => (
            q(
                "gg",
                vec![
                    SearchFilter::From { uid: 2, username: "bob".into() },
                    SearchFilter::In { channel: "general".into() },
                    SearchFilter::Has(HasKind::Image),
                    SearchFilter::Before(NaiveDate::from_ymd_opt(2026, 11, 1).expect("a date")),
                ],
            ),
            false,
        ),
        Stage::DatePicker => (q("before:", vec![]), true),
        Stage::Results => (q("wayland", vec![]), false),
        Stage::ResultsEmpty => (q("zzyzx", vec![]), false),
        Stage::ResultsRefused => (q("ranked", vec![]), false),
        _ => (q("", vec![]), false),
    }
}

fn payload(id: i64, sender: (&str, i64), channel: &str, content: &str, timestamp: i64) -> ChannelMessagePayload {
    ChannelMessagePayload {
        id,
        sender: sender.0.into(),
        sender_uid: sender.1,
        channel_id: channel.into(),
        content: content.into(),
        timestamp,
        attachments: Vec::new(),
        nonce: String::new(),
        edited_at: 0,
        reply_to: 0,
        reply_to_sender: String::new(),
        reply_to_sender_uid: 0,
        reply_to_content: String::new(),
        reply_to_attachment_kinds: Vec::new(),
        encrypted: false,
        decrypt_error: String::new(),
        encrypted_attachments: Vec::new(),
        suppress_embeds: false,
    }
}

/// "wayland"'s first page: three newer results in `#off-topic` (a reply, an edit, a file), then
/// the newest 22 of `#general`'s fixture messages that hold the word.
pub fn wayland_results() -> Vec<ChannelMessagePayload> {
    let general = decibell_chat::fixture::general_payloads(&decibell_chat::fixture::generate(decibell_chat::fixture::COUNT));
    let newest = general.last().map_or(1_794_000_000, |m| m.timestamp);
    let mut out = Vec::new();
    let mut reply = payload(10_003, ("carol", 3), "off-topic", "Same here, Wayland with the NVIDIA driver finally works for me 🎉", newest + 900);
    reply.reply_to = 10_001;
    reply.reply_to_sender = "dave".into();
    reply.reply_to_sender_uid = 4;
    reply.reply_to_content = "anyone else on **wayland** now?".into();
    out.push(reply);
    let mut edited = payload(10_002, ("bob", 2), "off-topic", "Screen share on Wayland goes through the portal: pick the monitor, then *Share*.", newest + 600);
    edited.edited_at = newest + 660;
    out.push(edited);
    let mut file = payload(10_001, ("dave", 4), "off-topic", "anyone else on **wayland** now? config attached", newest + 300);
    file.attachments.push(AttachmentPayload {
        id: 100_011,
        message_id: 10_001,
        kind: "document".into(),
        filename: "wayland-session.conf".into(),
        mime: "text/plain".into(),
        size_bytes: 1_843,
        url: String::new(),
        position: 0,
        created_at: newest + 300,
        purged_at: 0,
        width: 0,
        height: 0,
        thumbnail_size_bytes: 0,
        thumbnail_sizes_mask: 0,
        duration_ms: 0,
        placeholder: String::new(),
        encrypted: false,
    });
    out.push(file);
    out.extend(general.into_iter().rev().filter(|m| m.content.to_lowercase().contains("wayland")).take(22));
    out
}

/// The search a stage runs and the server's answer to it (its seq set when it is routed).
fn search(stage: Stage) -> Option<(SearchQuery, MessageSearchReceivedPayload)> {
    let answer = |messages: Vec<ChannelMessagePayload>, total: i64, capped: bool, skipped: Vec<String>, has_more: bool| MessageSearchReceivedPayload {
        server_id: shell::DAILY_GAMING.to_string(),
        seq: 0,
        success: true,
        message: String::new(),
        next_cursor: messages.last().map_or(0, |m| m.id),
        messages,
        has_more,
        next_offset: 0,
        total,
        total_capped: capped,
        text_skipped_channel_ids: skipped,
        partial: false,
        retry_after_ms: 0,
        first_page: true,
    };
    match stage {
        Stage::Results => Some((q("wayland", vec![]), answer(wayland_results(), 10_000, true, vec!["vault".into()], true))),
        Stage::ResultsEmpty => Some((q("zzyzx", vec![]), answer(Vec::new(), 0, false, vec!["vault".into()], false))),
        Stage::ResultsRefused => {
            let mut refused = answer(Vec::new(), -1, false, Vec::new(), false);
            refused.success = false;
            refused.message = "You're searching too fast.".into();
            Some((q("ranked", vec![]), refused))
        }
        _ => None,
    }
}

/// Fill the model with the stage's state (the chat fixture first) and register the panes; the
/// caller builds `decibell_shell::Shell`.
pub fn apply(stage: Stage, cx: &mut App) -> Model {
    cx.set_global(PinnedToday(today()));
    let _source = decibell_chat::fixture::open(decibell_chat::fixture::Which::General, cx);
    let model = Model::global(cx).clone();
    let (channel, events) = extra(stage);
    for event in events {
        decibell_model::events::route(&model, event, cx);
    }
    model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(ChannelId::from(channel)), cx));
    let server = ServerId::from_central(shell::DAILY_GAMING);
    let store = model.chat.search.clone();
    // No core: requests go nowhere, the answer is routed below.
    store.update(cx, |s, _| s.set_sender(Rc::new(|_, _, _| {})));
    if let Some((query, mut answer)) = search(stage) {
        store.update(cx, |s, cx| s.run(server, &query, cx));
        answer.seq = store.read(cx).seq;
        decibell_model::events::route(&model, CoreEvent::MessageSearchReceived(answer), cx);
    }
    let (draft, focus) = draft(stage);
    store.update(cx, |s, cx| s.set_draft(server, draft, cx));
    if focus {
        cx.set_global(FocusOnMount);
    } else if cx.has_global::<FocusOnMount>() {
        cx.remove_global::<FocusOnMount>();
    }
    decibell_chat::init(cx);
    crate::init(cx);
    model
}

/// The renderer's `SearchFilter` (`searchQuery.ts`).
fn filter_json(f: &SearchFilter) -> serde_json::Value {
    use serde_json::json;
    match f {
        SearchFilter::From { uid, username } => json!({ "kind": "from", "uid": uid, "username": username.as_ref() }),
        SearchFilter::In { channel } => json!({ "kind": "in", "channelId": channel.as_ref() }),
        SearchFilter::Has(kind) => json!({ "kind": "has", "has": crate::tokens::has_name(*kind) }),
        SearchFilter::Before(day) => json!({ "kind": "before", "date": day.format("%Y-%m-%d").to_string() }),
        SearchFilter::During(day) => json!({ "kind": "during", "date": day.format("%Y-%m-%d").to_string() }),
        SearchFilter::After(day) => json!({ "kind": "after", "date": day.format("%Y-%m-%d").to_string() }),
        SearchFilter::Inside => json!({ "kind": "inside" }),
    }
}

fn query_json(q: &SearchQuery) -> serde_json::Value {
    serde_json::json!({ "text": q.text.as_ref(), "filters": q.filters.iter().map(filter_json).collect::<Vec<_>>() })
}

/// The stage as the Electron pages replay it: the chat fixture's events (in voice, as the GPUI
/// fixture is) and the stage's, the navigation, and `search`: `{ today, draft, focus, query,
/// answer }`.
pub fn json(stage: Stage) -> serde_json::Value {
    let mut value = decibell_chat::fixture::json(decibell_chat::fixture::Which::General, 1);
    let (channel, events) = extra(stage);
    if let Some(list) = value.get_mut("events").and_then(|events| events.as_array_mut()) {
        for event in events {
            let (name, payload) = event.to_json();
            list.push(serde_json::json!({ "event": name, "payload": payload }));
        }
    }
    value["nav"]["channel"] = serde_json::json!(channel);
    value["nav"]["voice"] = serde_json::json!({ "server": shell::DAILY_GAMING.to_string(), "channel": "squad" });
    let (draft, focus) = draft(stage);
    let search = search(stage);
    value["search"] = serde_json::json!({
        "today": today().format("%Y-%m-%d").to_string(),
        "draft": query_json(&draft),
        "focus": focus,
        "query": search.as_ref().map(|(query, _)| query_json(query)),
        "answer": search.map(|(_, answer)| CoreEvent::MessageSearchReceived(answer).to_json().1),
    });
    value
}

//! Audit Log (`tabs/AuditLogTab.tsx`, VIEW_AUDIT_LOG): newest first, paged by id, 50 a page.
//! Search and the action filter run on the server (`AuditLogRequest.query` / `.actions`), so they
//! cover the whole log, not just the loaded pages. Every entry becomes a sentence with chips
//! ([`describe`], parsing the detail strings the server writes; anything unrecognised falls
//! through as a plain chip). Entries are grouped by day under a rail; the list is windowed
//! against the pane with heights measured once per width.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use decibell_common::{UserAvatar, Watch, display_name, user_name};
use decibell_core::events::{AuditEntryPayload, CoreEvent};
use decibell_model::live::transfers::format_size;
use decibell_model::regional::{self, Date, MonthName, Regional};
use decibell_model::stores::community::{Channel, ChannelKind, Role};
use decibell_model::stores::permissions::{EDITABLE_PERMISSIONS, has_bits};
use decibell_model::stores::settings::TimeFormat;
use decibell_model::{Model, ServerId};
use decibell_ui::{
    ActiveTheme as _, EmptyState, FontRole, Icon, IconName, Label, SectionLabel, Select, SelectIcon, SelectOption,
};
use gpui::{AnyElement, App, Context, Hsla, IntoElement, Render, SharedString, Subscription, Task, Window, div, prelude::*, px};

use super::{SettingsTab, TabContext};
use crate::requests::{self, Request};
use crate::rows::{Measured, PaneRows, StickyTop};
use crate::sentence::{self, Base, Run, Sentence, Shaped};
use crate::ui::{SearchBox, error_banner, outline_button};

const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);
/// A community that predates audit search answers with seq 0, which we drop; say so rather than
/// spin forever.
const NO_ANSWER_AFTER: Duration = Duration::from_millis(8000);
const NO_ANSWER: &str = "The server didn't answer. It may be running an older version without audit search.";
const PAGE: i32 = 50;
/// The toolbar: a 40 px search field and `pb-4`.
const TOOLBAR_H: f32 = 56.;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Danger,
    Warning,
    Success,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    All,
    Moderation,
    Members,
    Roles,
    Channels,
    Invites,
    Server,
}

/// Every action the community server writes (`src/community/main.cpp` add_audit), with its
/// filter group, icon and tone. Tone is the verb's weight, not its subject: destructive =
/// danger, moderation that ends on its own = warning, things that add or restore = success.
pub const ACTIONS: [(&str, Category, IconName, Tone); 25] = [
    ("kick", Category::Moderation, IconName::SettingsUserX, Tone::Danger),
    ("ban", Category::Moderation, IconName::SettingsBan, Tone::Danger),
    ("unban", Category::Moderation, IconName::SettingsUserCheck, Tone::Success),
    ("timeout", Category::Moderation, IconName::SettingsClock, Tone::Warning),
    ("timeout_clear", Category::Moderation, IconName::SettingsClock, Tone::Success),
    ("message_delete", Category::Moderation, IconName::SettingsMessageX, Tone::Danger),
    ("voice_mod", Category::Moderation, IconName::SettingsMicOff, Tone::Warning),
    ("nickname", Category::Members, IconName::SettingsPencil, Tone::Neutral),
    ("member_roles", Category::Members, IconName::SettingsTag, Tone::Neutral),
    ("role_create", Category::Roles, IconName::SettingsShieldPlus, Tone::Success),
    ("role_update", Category::Roles, IconName::SettingsShield, Tone::Neutral),
    ("role_delete", Category::Roles, IconName::SettingsShieldX, Tone::Danger),
    ("channel_create", Category::Channels, IconName::SettingsHash, Tone::Success),
    ("channel_rename", Category::Channels, IconName::SettingsHash, Tone::Neutral),
    ("channel_update", Category::Channels, IconName::SettingsSliders, Tone::Neutral),
    ("channel_delete", Category::Channels, IconName::SettingsTrash, Tone::Danger),
    ("channel_wipe", Category::Channels, IconName::SettingsEraser, Tone::Danger),
    ("overwrite_set", Category::Channels, IconName::SettingsKey, Tone::Neutral),
    ("overwrite_sync", Category::Channels, IconName::SettingsRefresh, Tone::Neutral),
    ("category_sync", Category::Channels, IconName::SettingsRefresh, Tone::Neutral),
    ("invite_create", Category::Invites, IconName::SettingsLink, Tone::Success),
    ("invite_revoke", Category::Invites, IconName::SettingsLinkOff, Tone::Danger),
    ("server_update", Category::Server, IconName::SettingsGear, Tone::Neutral),
    ("storage_min_free", Category::Server, IconName::SettingsDatabase, Tone::Neutral),
    ("ownership_transfer", Category::Server, IconName::SettingsCrown, Tone::Warning),
];

const CATEGORY_OPTIONS: [(Category, &str, IconName); 7] = [
    (Category::All, "All actions", IconName::SettingsFile),
    (Category::Moderation, "Moderation", IconName::SettingsBan),
    (Category::Members, "Members", IconName::SettingsUsers),
    (Category::Roles, "Roles", IconName::SettingsShield),
    (Category::Channels, "Channels", IconName::SettingsHash),
    (Category::Invites, "Invites", IconName::SettingsLink),
    (Category::Server, "Server", IconName::SettingsGear),
];

/// The actions a category filters to (none for "all").
pub fn actions_of(category: Category) -> Vec<String> {
    if category == Category::All {
        return Vec::new();
    }
    ACTIONS.iter().filter(|a| a.1 == category).map(|a| a.0.to_string()).collect()
}

/// `"2026-10-12 22:40 UTC"` (format_utc on the server) → unix seconds.
pub fn parse_utc(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    for start in 0..b.len() {
        let rest = &s[start..];
        if rest.len() < 20 || !rest.is_char_boundary(0) {
            continue;
        }
        let digits = |r: std::ops::Range<usize>| -> Option<i64> {
            let t = rest.get(r)?;
            t.bytes().all(|c| c.is_ascii_digit()).then(|| t.parse().ok()).flatten()
        };
        let sep = |i: usize, c: u8| rest.as_bytes().get(i) == Some(&c);
        if let (Some(y), Some(mo), Some(d), Some(h), Some(mi)) = (digits(0..4), digits(5..7), digits(8..10), digits(11..13), digits(14..16))
            && sep(4, b'-')
            && sep(7, b'-')
            && sep(10, b' ')
            && sep(13, b':')
            && rest.get(16..20) == Some(" UTC")
        {
            return Some(days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60);
        }
    }
    None
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm), `Date.UTC`'s day count.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn day_index(d: Date) -> i64 {
    days_from_civil(d.year as i64, d.month as i64, d.day as i64)
}

/// JavaScript's `Math.round`.
fn js_round(v: f64) -> i64 {
    (v + 0.5).floor() as i64
}

/// "3 days", "1 day", "2 h", "10 min", "45 s".
pub fn duration(sec: i64) -> String {
    if sec >= 86400 * 2 - 60 {
        format!("{} days", js_round(sec as f64 / 86400.))
    } else if sec >= 86400 - 60 {
        "1 day".into()
    } else if sec >= 3600 - 30 {
        format!("{} h", js_round(sec as f64 / 3600.))
    } else if sec >= 60 {
        format!("{} min", js_round(sec as f64 / 60.))
    } else {
        format!("{sec} s")
    }
}

/// `{ month: "short", day, hour, minute }`: "Nov 7, 2:32 PM".
fn short_date_time(epoch: i64, pref: TimeFormat) -> String {
    let regional = Regional::get();
    match regional::local(epoch) {
        Some(t) => format!("{}, {}", regional.month_day(t.date, MonthName::Short), regional.time(t.hour, t.minute, pref)),
        None => String::new(),
    }
}

/// The server's "; "-joined detail string, split into its parts.
fn detail_parts(details: &str) -> Vec<String> {
    details.split("; ").map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// Bits of a permission mask → their labels, in editor order.
fn perm_labels(mask: u64) -> Vec<&'static str> {
    if mask == 0 {
        return Vec::new();
    }
    EDITABLE_PERMISSIONS.iter().filter(|p| has_bits(mask, p.bit)).map(|p| p.label).collect()
}

/// A chip under an entry.
#[derive(Debug, Clone, PartialEq)]
pub enum Chip {
    Plain { icon: Option<IconName>, tone: Option<bool>, text: String, title: Option<String> },
    /// A role added or removed (`member_roles`).
    Role { added: bool, color: u32, name: String },
}

fn chip(icon: IconName, text: impl Into<String>) -> Chip {
    Chip::Plain { icon: Some(icon), tone: None, text: text.into(), title: None }
}

/// One entry, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Described {
    pub icon: IconName,
    pub tone: Tone,
    /// The sentence after the actor's name.
    pub text: Vec<Run>,
    pub chips: Vec<Chip>,
    /// A moderator's reason, shown as a quote.
    pub reason: Option<String>,
    /// Role dots in the sentence: (box tag, colour; 0 = muted).
    pub dots: Vec<(u32, u32)>,
}

/// What `describe` looks things up in.
pub struct Lookup<'a> {
    pub server: ServerId,
    pub channels: &'a [Channel],
    pub roles: &'a [Role],
    pub pref: TimeFormat,
    pub cx: &'a App,
}

impl Lookup<'_> {
    fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| &*c.id == id)
    }
    fn role_by_id(&self, id: i64) -> Option<&Role> {
        if id == -1 { self.roles.iter().find(|r| r.is_default) } else { self.roles.iter().find(|r| r.id == id) }
    }
    fn role_by_name(&self, name: &str) -> Option<&Role> {
        self.roles.iter().find(|r| !r.is_default && &*r.name == name)
    }
    /// `MemberRef`: the display name (nickname) of the current name.
    fn member(&self, username: &str, uid: i64) -> Run {
        let current = user_name(uid, &SharedString::from(username.to_string()), self.cx);
        Run::Strong(display_name(Some(self.server), uid, &current, self.cx).to_string())
    }
    /// `ChannelRef`: `#name` for text channels (and unknown ones).
    fn channel_ref(&self, id: &str, fallback: &str) -> Run {
        let channel = self.channel(id);
        let name = channel.map_or_else(|| fallback.to_string(), |c| c.name.to_string());
        let prefix = if channel.is_none_or(|c| c.kind == ChannelKind::Text) { "#" } else { "" };
        Run::Strong(format!("{prefix}{name}"))
    }
}

struct Builder {
    runs: Vec<Run>,
    dots: Vec<(u32, u32)>,
}

impl Builder {
    fn t(&mut self, text: &str) -> &mut Self {
        self.runs.push(Run::Text(text.into()));
        self
    }
    fn run(&mut self, run: Run) -> &mut Self {
        self.runs.push(run);
        self
    }
    fn strong(&mut self, text: impl Into<String>) -> &mut Self {
        self.runs.push(Run::Strong(text.into()));
        self
    }
    /// `RoleRef`: dot + name, or the fallback when the role is gone.
    fn role(&mut self, role: Option<&Role>, fallback: &str) -> &mut Self {
        match role {
            None => self.strong(fallback),
            Some(r) => {
                let tag = 100 + self.dots.len() as u32;
                self.dots.push((tag, if r.is_default { 0 } else { r.color }));
                self.runs.push(Run::Box { tag, width: 14., height: 8. });
                self.strong(if r.is_default { "@everyone".to_string() } else { r.name.to_string() })
            }
        }
    }
}

/// One entry → icon, tone, a readable sentence and detail chips (`describe`).
pub fn describe(e: &AuditEntryPayload, look: &Lookup) -> Described {
    let meta = ACTIONS.iter().find(|a| a.0 == e.action);
    let (mut icon, mut tone) = meta.map_or((IconName::SettingsGear, Tone::Neutral), |a| (a.2, a.3));
    let mut b = Builder { runs: Vec::new(), dots: Vec::new() };
    let mut chips: Vec<Chip> = Vec::new();
    let mut reason = None;
    let parts = detail_parts(&e.details);
    let mut rest: Vec<String> = Vec::new();
    let self_target = if e.actor_uid != 0 { e.actor_uid == e.target_uid } else { e.actor == e.target };
    let regional = Regional::get();
    let ch_in = |b: &mut Builder| {
        if !e.channel_id.is_empty() {
            b.t(" in ").run(look.channel_ref(&e.channel_id, &e.channel_id));
        }
    };
    let ch = || look.channel_ref(&e.channel_id, if e.target.is_empty() { &e.channel_id } else { &e.target });

    // Shared detail grammar: "reason: …", "until <utc>", "N messages purged".
    for p in &parts {
        if let Some(r) = p.strip_prefix("reason: ") {
            reason = Some(r.to_string());
        } else if let Some(until) = p.strip_prefix("until ").and_then(parse_utc) {
            let text = if e.action == "timeout" {
                format!("{} · until {}", duration(until - e.timestamp), short_date_time(until, look.pref))
            } else {
                format!("Until {}", short_date_time(until, look.pref))
            };
            chips.push(Chip::Plain {
                icon: Some(IconName::SettingsClock),
                tone: None,
                text,
                title: Some(format!("Until {}", regional.full_date_time(until, look.pref))),
            });
        } else if let Some(n) = p.strip_suffix(" messages purged").and_then(|n| n.parse::<u64>().ok()) {
            chips.push(chip(IconName::SettingsTrash, format!("{} messages deleted", regional.group(n))));
        } else {
            rest.push(p.clone());
        }
    }
    let first_number = |p: &str| p.split(' ').next().and_then(|n| n.parse::<u64>().ok()).map_or("NaN".to_string(), |n| regional.group(n));

    match e.action.as_str() {
        "kick" => {
            b.t("kicked ").run(look.member(&e.target, e.target_uid));
        }
        "ban" => {
            b.t("banned ").run(look.member(&e.target, e.target_uid));
            if !parts.iter().any(|p| p.starts_with("until ")) {
                chips.insert(0, chip(IconName::SettingsBan, "Permanent"));
            }
        }
        "unban" => {
            b.t("unbanned ").run(look.member(&e.target, e.target_uid));
        }
        "timeout" => {
            b.t("timed out ").run(look.member(&e.target, e.target_uid));
        }
        "timeout_clear" => {
            b.t("ended the timeout on ").run(look.member(&e.target, e.target_uid));
        }
        "message_delete" => {
            b.t("deleted a message by ").run(look.member(&e.target, e.target_uid));
            ch_in(&mut b);
            // "message <id>": the id means nothing to a reader.
            rest.clear();
        }
        "voice_mod" => {
            let verb = if rest.is_empty() { String::new() } else { rest.remove(0) };
            let where_ = look.channel_ref(&e.channel_id, &e.channel_id);
            if verb.starts_with("voice_move") {
                icon = IconName::SettingsMove;
                tone = Tone::Neutral;
                b.t("moved ").run(look.member(&e.target, e.target_uid)).t(" to ").run(where_);
                if verb.contains("voice pass") {
                    chips.push(Chip::Plain {
                        icon: Some(IconName::SettingsKey),
                        tone: None,
                        text: "Voice pass granted".into(),
                        title: Some("They couldn't join that channel themselves, so the move granted a one-time voice pass.".into()),
                    });
                }
            } else if verb == "voice_disconnect" {
                icon = IconName::SettingsLogout;
                b.t("disconnected ").run(look.member(&e.target, e.target_uid)).t(" from ").run(where_);
            } else {
                let undo = verb.contains("un");
                let deafen = verb.contains("deafen");
                icon = if deafen { IconName::SettingsHeadphones } else if undo { IconName::SettingsMic } else { IconName::SettingsMicOff };
                tone = if undo { Tone::Success } else { Tone::Warning };
                let word = match (undo, deafen) {
                    (true, true) => "undeafened",
                    (true, false) => "unmuted",
                    (false, true) => "server-deafened",
                    (false, false) => "server-muted",
                };
                b.t(&format!("{word} ")).run(look.member(&e.target, e.target_uid)).t(" in ").run(where_);
            }
        }
        "nickname" => {
            let d = if rest.is_empty() { String::new() } else { rest.remove(0) };
            // The username, not the display name: the nickname is what changed.
            let whose = |b: &mut Builder| {
                if self_target {
                    b.t("their own");
                } else {
                    b.strong(e.target.clone()).t("\u{2019}s");
                }
            };
            if d == "cleared" {
                b.t("cleared ");
                whose(&mut b);
                b.t(" nickname");
            } else {
                b.t("changed ");
                whose(&mut b);
                b.t(" nickname to ").strong(d.strip_prefix("set to ").unwrap_or(&d).to_string());
            }
        }
        "member_roles" => {
            if self_target {
                b.t("changed their own roles");
            } else {
                b.t("changed the roles of ").run(look.member(&e.target, e.target_uid));
            }
            let d = if rest.is_empty() { String::new() } else { rest.remove(0) };
            for (added, name) in role_changes(&d) {
                let color = look.role_by_name(&name).map_or(0, |r| r.color);
                chips.push(Chip::Role { added, color, name });
            }
        }
        "role_create" => {
            b.t("created the role ").role(look.role_by_name(&e.target), &e.target);
        }
        "role_update" => {
            let everyone = e.target == "everyone";
            let role = if everyone { look.role_by_id(-1) } else { look.role_by_name(&e.target) };
            b.t("updated the role ").role(role, if everyone { "@everyone" } else { &e.target });
            for p in std::mem::take(&mut rest) {
                if p == "permissions changed" {
                    chips.push(chip(IconName::SettingsShield, "Permissions changed"));
                } else if p.contains("manage each other") {
                    chips.push(chip(
                        IconName::SettingsUsers,
                        if p.contains("can't") { "Can\u{2019}t manage each other" } else { "Can manage each other" },
                    ));
                } else {
                    rest.push(p);
                }
            }
        }
        "role_delete" => {
            b.t("deleted the role ").strong(e.target.clone());
        }
        "channel_create" => {
            b.t("created ").run(ch());
            if rest.first().map(String::as_str) == Some("encrypted") {
                rest.remove(0);
                chips.push(chip(IconName::SettingsLock, "End-to-end encrypted"));
            }
        }
        "channel_rename" => {
            b.t("renamed a channel to ").run(ch());
        }
        "channel_update" => {
            let d = if rest.is_empty() { String::new() } else { rest.remove(0) };
            let slow = d.strip_prefix("slowmode ").and_then(|s| s.strip_suffix('s')).filter(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()));
            if d.starts_with("encryption ") {
                // Entries from when encryption could be switched on an existing channel.
                icon = IconName::SettingsLock;
                b.t(&format!("turned {} encryption in ", if d.ends_with("on") { "on" } else { "off" })).run(ch());
            } else if d == "topic" {
                icon = IconName::SettingsPencil;
                b.t("changed the topic of ").run(ch());
            } else if let Some(seconds) = slow {
                icon = IconName::SettingsClock;
                if seconds == "0" {
                    b.t("turned off slowmode in ").run(ch());
                } else {
                    b.t("set slowmode in ").run(ch()).t(" to ").strong(duration(seconds.parse().unwrap_or(0)));
                }
            } else {
                b.t("changed retention or bitrate in ").run(ch());
            }
        }
        "channel_delete" => {
            b.t("deleted ").strong(format!("#{}", e.target));
            for p in std::mem::take(&mut rest) {
                chips.push(chip(IconName::SettingsTrash, format!("{} messages deleted", first_number(&p))));
            }
        }
        "channel_wipe" => {
            b.t("wiped ").run(ch());
            for p in std::mem::take(&mut rest) {
                chips.push(chip(IconName::SettingsTrash, format!("{} messages deleted", first_number(&p))));
            }
        }
        "overwrite_set" => {
            let (kind, id) = match e.target.find(':') {
                Some(i) => (&e.target[..i], &e.target[i + 1..]),
                None => ("", e.target.as_str()),
            };
            b.t("changed permissions for ");
            if kind == "role" {
                b.role(id.parse::<i64>().ok().and_then(|id| look.role_by_id(id)), "a deleted role");
            } else {
                b.run(look.member(id, e.target_uid));
            }
            ch_in(&mut b);
            let d = if rest.is_empty() { String::new() } else { rest.remove(0) };
            if d == "cleared" {
                chips.push(chip(IconName::SettingsRefresh, "Reset to the server permissions"));
            } else if let Some((allow, deny)) = parse_allow_deny(&d) {
                for l in perm_labels(allow) {
                    chips.push(Chip::Plain { icon: Some(IconName::SettingsCheck), tone: Some(true), text: l.into(), title: None });
                }
                for l in perm_labels(deny) {
                    chips.push(Chip::Plain { icon: Some(IconName::SettingsX), tone: Some(false), text: l.into(), title: None });
                }
            }
        }
        "overwrite_sync" => {
            let raw = if rest.is_empty() { String::new() } else { rest.remove(0) };
            let cat = raw.strip_prefix("synced with category ").unwrap_or(&raw).to_string();
            b.t("synced ").run(ch()).t(" with ").run(look.channel_ref(&cat, &cat));
        }
        "category_sync" => {
            let on = !rest.is_empty() && rest.remove(0) == "on";
            b.t(&format!("turned {} permission sync for ", if on { "on" } else { "off" })).run(look.channel_ref(&e.channel_id, &e.target));
        }
        "invite_create" => {
            b.t("created the invite ").run(Run::Code(e.target.clone()));
            for p in std::mem::take(&mut rest) {
                if let Some(who) = p.strip_prefix("for ") {
                    chips.push(chip(IconName::SettingsUserPlus, format!("For {who}")));
                } else {
                    let text = if p == "unlimited" {
                        "Unlimited uses".to_string()
                    } else {
                        format!("Up to {} uses", p.replacen("max uses ", "", 1))
                    };
                    chips.push(chip(IconName::SettingsUsers, text));
                }
            }
        }
        "invite_revoke" => {
            b.t("revoked the invite ").run(Run::Code(e.target.clone()));
        }
        "server_update" => {
            b.t("updated the server profile");
            for p in std::mem::take(&mut rest) {
                if let Some(name) = p.strip_prefix("name: ") {
                    chips.push(chip(IconName::SettingsPencil, format!("Name: {name}")));
                } else if let Some(public) = p.strip_prefix("public: ") {
                    chips.push(chip(IconName::SettingsUsers, format!("Public listing {public}")));
                } else {
                    chips.push(Chip::Plain { icon: None, tone: None, text: p, title: None });
                }
            }
        }
        "storage_min_free" => {
            let bytes: f64 = if rest.is_empty() { 0. } else { rest.remove(0).parse().unwrap_or(f64::NAN) };
            if bytes != 0. && !bytes.is_nan() {
                b.t("set the upload headroom to ").strong(format_size(bytes.max(0.) as u64));
            } else if bytes.is_nan() {
                b.t("set the upload headroom to ").strong("NaN GB");
            } else {
                b.t("turned off the upload headroom");
            }
        }
        "ownership_transfer" => {
            b.t("transferred ownership to ").run(look.member(&e.target, e.target_uid));
        }
        other => {
            b.t(&other.replace('_', " "));
            if !e.target.is_empty() {
                b.t(" ").strong(e.target.clone());
            }
            ch_in(&mut b);
        }
    }
    for p in rest {
        chips.push(Chip::Plain { icon: None, tone: None, text: p, title: None });
    }
    Described { icon, tone, text: b.runs, chips, reason, dots: b.dots }
}

/// `allow <n> deny <n>`.
fn parse_allow_deny(d: &str) -> Option<(u64, u64)> {
    let rest = d.strip_prefix("allow ")?;
    let (allow, deny) = rest.split_once(" deny ")?;
    Some((allow.parse().ok()?, deny.parse().ok()?))
}

/// `/([+-])(.+?) (?=[+-]|$)/g` over "+Moderator -Member ": each role added or removed.
fn role_changes(d: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let chars: Vec<char> = d.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '+' && chars[i] != '-' {
            i += 1;
            continue;
        }
        let sign = chars[i];
        // The shortest name followed by a space and then a sign or the end.
        let mut j = i + 2;
        let mut found = None;
        while j <= chars.len() {
            if chars.get(j - 1) == Some(&' ') && (j == chars.len() || chars[j] == '+' || chars[j] == '-') && j - 1 > i + 1 {
                found = Some(j);
                break;
            }
            j += 1;
        }
        match found {
            Some(end) => {
                out.push((sign == '+', chars[i + 1..end - 1].iter().collect()));
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// A day's heading: "Today" / "Yesterday" / a weekday within the week (with the date beside),
/// else the date (`dayHeading`).
pub fn day_heading(day: Date, today: Date) -> (String, Option<String>) {
    let regional = Regional::get();
    let diff = day_index(today) - day_index(day);
    if diff == 0 {
        return ("Today".into(), Some(regional.weekday_month_day(day)));
    }
    if diff == 1 {
        return ("Yesterday".into(), Some(regional.weekday_month_day(day)));
    }
    if diff < 7 {
        return (regional::weekday_name(day.weekday()).into(), Some(regional.month_day(day, MonthName::Long)));
    }
    if day.year == today.year {
        (regional.month_day(day, MonthName::Long), None)
    } else {
        (regional.month_day_year(day, MonthName::Long), None)
    }
}

/// A flattened row of the list: a day's heading or an entry.
#[derive(Clone)]
enum Item {
    Day { day: Date, first: bool },
    Entry { index: usize, first: bool, last: bool },
    Footer,
}

/// Each entry's sentence and what it says, by entry id.
type ShapedEntries = HashMap<i64, (Rc<Shaped>, Rc<Described>)>;

pub struct AuditLogTab {
    ctx: TabContext,
    query: SearchBox,
    applied: String,
    category: Category,
    entries: Vec<AuditEntryPayload>,
    has_more: bool,
    loading: bool,
    loaded: bool,
    error: Option<SharedString>,
    seq: u32,
    answered: u32,
    replace: bool,
    /// Each entry's sentence, shaped once (dropped when names, roles, channels, the theme or
    /// the scale change).
    shaped: RefCell<ShapedEntries>,
    shaped_for: RefCell<Option<(decibell_theme::ThemeId, u32)>>,
    measured: Rc<RefCell<Measured>>,
    /// The screens draw the action filter open.
    filter_open: bool,
    _debounce: Option<Task<()>>,
    _no_answer: Option<Task<()>>,
    _watch: Watch,
    _answers: Subscription,
}

impl SettingsTab for AuditLogTab {
    fn dirty(&self, _: &App) -> bool {
        false
    }
}

impl AuditLogTab {
    pub fn new(ctx: TabContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let server = ctx.server;
        let mut watch = Watch::new();
        let reshape = |this: &mut Self| {
            this.shaped.borrow_mut().clear();
            this.measured.borrow_mut().invalidate_from(0);
            true
        };
        watch
            .store_if(&model.settings, cx, move |this, _, _| reshape(this))
            .event(&model.users, cx, move |this, _: &decibell_model::stores::users::NamesChanged| reshape(this));
        if let Some(community) = model.community(server, cx) {
            watch.store_if(&community.channels, cx, move |this, _, _| reshape(this));
            watch.store_if(&community.roles, cx, move |this, _, _| reshape(this));
        }
        let query = SearchBox::new("Search by member, role, channel or reason", window, cx, |this: &mut Self, text, cx| this.typed(text, cx));
        let answers = cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| {
            if let CoreEvent::AuditLogReceived(p) = event
                && ServerId::from_wire(&p.server_id) == server
                && p.seq == this.seq
            {
                this.answered = p.seq;
                this._no_answer = None;
                this.loading = false;
                this.loaded = true;
                if !p.success {
                    this.error = Some(if p.message.is_empty() { "Couldn't load the audit log.".into() } else { p.message.clone().into() });
                } else {
                    this.error = None;
                    if this.replace {
                        this.entries = p.entries.clone();
                        this.measured.borrow_mut().invalidate_from(0);
                    } else {
                        this.entries.extend(p.entries.iter().cloned());
                        // Only the footer row and the rows after it move.
                        let rows = this.items().len();
                        this.measured.borrow_mut().invalidate_from(rows.saturating_sub(p.entries.len() + 2));
                    }
                    this.has_more = p.has_more;
                }
                cx.notify();
            }
        });
        let mut this = AuditLogTab {
            ctx,
            query,
            applied: String::new(),
            category: Category::All,
            entries: Vec::new(),
            has_more: false,
            loading: false,
            loaded: false,
            error: None,
            seq: 0,
            answered: 0,
            replace: true,
            shaped: RefCell::new(HashMap::new()),
            shaped_for: RefCell::new(None),
            measured: Measured::new(px(52.)),
            filter_open: false,
            _debounce: None,
            _no_answer: None,
            _watch: watch,
            _answers: answers,
        };
        this.fetch(0, cx);
        this
    }

    /// Typing settles before it searches (the community rate-limits list fetches); a category
    /// change applies at once.
    fn typed(&mut self, text: &str, cx: &mut Context<Self>) {
        let q = text.trim().to_string();
        if q == self.applied {
            self._debounce = None;
            return;
        }
        self._debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |this, cx| {
                this._debounce = None;
                this.applied = q;
                this.fetch(0, cx);
            });
        }));
    }

    pub fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.category != category {
            self.category = category;
            self.fetch(0, cx);
        }
    }

    #[cfg(feature = "fixtures")]
    /// Focus the search box (the self-tests type into it).
    pub(crate) fn focus_search(&self, window: &mut Window, cx: &mut App) {
        self.query.focus(window, cx);
    }

    #[cfg(feature = "fixtures")]
    /// What the last answer held, and whether one came.
    pub(crate) fn entries(&self) -> (&[AuditEntryPayload], bool) {
        (&self.entries, self.loaded && !self.loading)
    }

    /// The request a fetch sends (tests read it back).
    pub fn request(&self, before_id: i64) -> Request {
        Request::ListAuditLog {
            server: self.ctx.server,
            before_id,
            limit: PAGE,
            query: self.applied.clone(),
            actions: actions_of(self.category),
            seq: self.seq,
        }
    }

    /// A new search keeps the old entries on screen until its answer lands.
    fn fetch(&mut self, before_id: i64, cx: &mut Context<Self>) {
        self.seq = self.seq.wrapping_add(1);
        let seq = self.seq;
        self.replace = before_id == 0;
        self.loading = true;
        cx.notify();
        let weak = cx.weak_entity();
        requests::send(self.request(before_id), cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, cx| {
                    if seq == this.seq {
                        this.loading = false;
                        this.error = Some(e.js_string());
                        cx.notify();
                    }
                });
            }
        });
        self._no_answer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(NO_ANSWER_AFTER).await;
            let _ = this.update(cx, |this, cx| {
                this._no_answer = None;
                if seq == this.seq && this.answered != seq {
                    this.loading = false;
                    this.loaded = true;
                    this.error = Some(NO_ANSWER.into());
                    cx.notify();
                }
            });
        }));
    }

    fn filtering(&self) -> bool {
        !self.applied.is_empty() || self.category != Category::All
    }

    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.query.clear(cx);
        self.applied.clear();
        self._debounce = None;
        self.category = Category::All;
        self.fetch(0, cx);
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::with_capacity(self.entries.len() + 8);
        let mut last_day: Option<Date> = None;
        for (index, e) in self.entries.iter().enumerate() {
            let day = regional::local(e.timestamp).map_or(Date::new(1970, 1, 1), |t| t.date);
            let first = last_day != Some(day);
            if first {
                if let Some(Item::Entry { last, .. }) = items.last_mut() {
                    *last = true;
                }
                items.push(Item::Day { day, first: last_day.is_none() });
                last_day = Some(day);
            }
            items.push(Item::Entry { index, first, last: false });
        }
        if let Some(Item::Entry { last, .. }) = items.last_mut() {
            *last = true;
        }
        items.push(Item::Footer);
        items
    }

    fn described(&self, index: usize, window: &Window, cx: &mut App) -> (Rc<Shaped>, Rc<Described>) {
        let (theme_id, secondary, primary) = {
            let theme = cx.theme();
            (theme.id, theme.color_text_secondary, theme.color_text_primary)
        };
        let key = (theme_id, window.scale_factor().to_bits());
        if *self.shaped_for.borrow() != Some(key) {
            self.shaped.borrow_mut().clear();
            *self.shaped_for.borrow_mut() = Some(key);
        }
        let e = &self.entries[index];
        if let Some(found) = self.shaped.borrow().get(&e.id) {
            return found.clone();
        }
        let (described, actor) = {
            let cx: &App = cx;
            let model = Model::global(cx);
            let community = model.community(self.ctx.server, cx);
            let channels = community.as_ref().map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default();
            let roles = community.as_ref().map(|c| c.roles.read(cx).list().to_vec()).unwrap_or_default();
            let look = Lookup { server: self.ctx.server, channels: &channels, roles: &roles, pref: model.settings.read(cx).time_format(), cx };
            (Rc::new(describe(e, &look)), look.member(&e.actor, e.actor_uid))
        };
        // The actor's avatar (18 px, `mr-1.5`), the actor, then the sentence. The renderer's
        // `inline-flex` avatar sits on the baseline of its letter, so it doesn't make the 20 px
        // line taller: the box is only the part above the baseline (13 px) and the avatar is
        // drawn from 5 px higher, then `translate-y-[3px]` lower.
        let mut runs = vec![Run::Box { tag: 1, width: 24., height: 13. }, actor, Run::Text(" ".into())];
        runs.extend(described.text.iter().cloned());
        let base = Base { font: FontRole::Sans, size: 13., line_height: 20., color: secondary, strong: primary };
        let shaped = Shaped::new(sentence::build(&runs, base, window.scale_factor(), cx));
        self.shaped.borrow_mut().insert(e.id, (shaped.clone(), described.clone()));
        (shaped, described)
    }
}

fn render_chip(id: (SharedString, usize), c: &Chip, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let base = div()
        .id(id)
        .flex()
        .h_6()
        .max_w_full()
        .items_center()
        .gap_1p5()
        .rounded(theme.radius_sm)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light)
        .px_2();
    match c {
        Chip::Plain { icon, tone, text, title } => {
            let icon_color = match tone {
                Some(true) => theme.color_success,
                Some(false) => theme.color_error,
                None => theme.color_text_muted,
            };
            base.when_some(*icon, |this, icon| this.child(Icon::new(icon).size(px(12.)).stroke_width(2.5).color(icon_color)))
                .child(div().min_w_0().child(Label::new(text.clone()).size(12.).color(theme.color_text_secondary).truncate()))
                .when_some(title.clone(), |this, title| this.tooltip(decibell_ui::title_tooltip(title)))
                .into_any_element()
        }
        Chip::Role { added, color, name } => {
            let fg = if *added { theme.color_text_secondary } else { theme.color_text_muted };
            let strike = theme.color_text_muted.opacity(0.6);
            base.child(
                Icon::new(if *added { IconName::SettingsPlus } else { IconName::SettingsX })
                    .size(px(11.))
                    .stroke_width(3.)
                    .color(if *added { theme.color_success } else { theme.color_error }),
            )
            .child(crate::ui::dot(crate::ui::role_color(*color, cx), 8.))
            .child(
                div()
                    .relative()
                    .child(Label::new(name.clone()).size(12.).color(fg).nowrap())
                    // `line-through decoration-text-muted/60`.
                    .when(!*added, |this| this.child(div().absolute().left_0().right_0().top(px(9.5)).h(px(1.)).bg(strike))),
            )
            .into_any_element()
        }
    }
}

fn tone_tile(tone: Tone, icon: IconName, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let (bg, fg, border): (Hsla, Hsla, bool) = match tone {
        Tone::Danger => (theme.color_error.opacity(0.12), theme.color_error, false),
        Tone::Warning => (theme.color_warning.opacity(0.12), theme.color_warning, false),
        Tone::Success => (theme.color_success.opacity(0.12), theme.color_success, false),
        Tone::Neutral => (theme.color_bg_light, theme.color_text_secondary, true),
    };
    div().relative().flex().flex_none().size_8().rounded(theme.radius_md).bg(theme.color_bg_dark).child(
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .rounded(theme.radius_md)
            .bg(bg)
            .when(border, |this| this.border_1().border_color(theme.color_border_divider))
            .child(Icon::new(icon).size(px(15.)).color(fg)),
    )
}

impl Render for AuditLogTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let category = self.category;
        let select = Select::options(
            "audit-filter",
            Some(category),
            CATEGORY_OPTIONS.iter().map(|(c, label, icon)| SelectOption::new(*c, *label).icon(SelectIcon::Icon(*icon))).collect(),
        ).over_modal()
        .on_change(cx.listener(|this, c: &Category, _, cx| this.set_category(*c, cx)));
        let select = if self.filter_open { select.initially_open() } else { select };
        let toolbar = div()
            .mx(px(-8.))
            .flex()
            .items_center()
            .gap_2()
            .bg(theme.color_bg_dark)
            .px_2()
            .pb_4()
            .child(div().min_w_0().flex_1().child(self.query.field("audit-search")))
            .child(div().w(px(192.)).flex_none().child(select));

        let mut body = div().flex().flex_col().pt(px(TOOLBAR_H));
        if let Some(error) = &self.error {
            body = body.child(div().mb_4().child(error_banner("audit-error", error.clone(), None, cx)));
        }
        let filtering = self.filtering();
        if !self.loaded {
            body = body.child(div().py_6().child(Label::new("Loading the audit log…").size(13.).color(theme.color_text_muted)));
        } else if self.entries.is_empty() && !filtering && self.error.is_none() {
            body = body.child(EmptyState::new(
                IconName::SettingsFile,
                "Nothing logged yet",
                "Moderation and settings changes show up here, newest first. Entries are kept for 180 days.",
            ));
        } else if self.entries.is_empty() && filtering {
            body = body.child(
                EmptyState::new(IconName::SettingsSearch, "No matching entries", "Nothing in the last 180 days matches. Try another search or action type.")
                    .action(
                        Button::new("audit-clear", "Clear filters")
                            .variant(decibell_ui::ButtonVariant::Soft)
                            .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| this.clear_filters(cx))),
                    ),
            );
        } else {
            body = body.child(self.list(window, cx));
        }
        div()
            .relative()
            .flex()
            .flex_col()
            .child(body)
            .child(StickyTop::new(&self.ctx.scroll, toolbar))
    }
}

use decibell_ui::Button;

#[cfg(feature = "fixtures")]
impl AuditLogTab {
    /// The `server-settings-audit-log-filters` screen: the action filter drawn open.
    pub fn fixture_open_filter(&mut self, cx: &mut Context<Self>) {
        self.filter_open = true;
        cx.notify();
    }
}

impl AuditLogTab {
    fn list(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let items = Rc::new(self.items());
        let today = regional::local(crate::now_secs())
            .map_or(Date::new(1970, 1, 1), |t| t.date);
        let pref = Model::global(cx).settings.read(cx).time_format();
        // Shape every visible sentence now (render may read the view; the rows can't).
        let mut shaped: HashMap<usize, (Rc<Shaped>, Rc<Described>)> = HashMap::new();
        for item in items.iter() {
            if let Item::Entry { index, .. } = item {
                shaped.insert(*index, self.described(*index, window, cx));
            }
        }
        let shaped = Rc::new(shaped);
        let entries = Rc::new(self.entries.clone());
        let (has_more, loading, filtering, loaded) = (self.has_more, self.loading, self.filtering(), self.loaded);
        let weak = cx.weak_entity();
        let count = items.len();
        PaneRows::measured(&self.ctx.scroll, count, &self.measured, move |range, _, cx| {
            let theme = cx.theme();
            range
                .map(|i| match &items[i] {
                    Item::Day { day, first } => {
                        let (label, sub) = day_heading(*day, today);
                        div()
                            .when(!*first, |this| this.pt_6())
                            .child(
                                div()
                                    .mb_1()
                                    .flex()
                                    .items_baseline()
                                    .gap_2()
                                    .child(SectionLabel::new(label))
                                    .when_some(sub, |this, sub| {
                                        this.child(Label::new(sub).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted))
                                    }),
                            )
                            .into_any_element()
                    }
                    Item::Entry { index, first, last } => {
                        let e = &entries[*index];
                        let (shaped, d) = shaped[index].clone();
                        let actor = user_name(e.actor_uid, &SharedString::from(e.actor.clone()), cx);
                        let mut sentence = Sentence::new(&shaped).element(1, UserAvatar::new(e.actor_uid, actor, 18.), -2.);
                        for (tag, color) in &d.dots {
                            sentence = sentence.dot(*tag, crate::ui::role_color(*color, cx));
                        }
                        let chips_id: SharedString = format!("audit-chips-{}", e.id).into();
                        let clock = Regional::get().clock(e.timestamp, pref);
                        let full = Regional::get().full_date_time_with_seconds(e.timestamp, pref);
                        let rail = theme.color_border_divider;
                        let (first, last) = (*first, *last);
                        div()
                            .id(("audit-entry", e.id as u64))
                            .relative()
                            .flex()
                            .gap(px(14.))
                            .py_2p5()
                            // The day's rail, threaded behind the action tiles.
                            .child(
                                div()
                                    .absolute()
                                    .left(px(15.))
                                    .w(px(2.))
                                    .rounded_full()
                                    .bg(rail)
                                    .top(px(if first { 24. } else { 0. }))
                                    .bottom(px(if last { 24. } else { 0. })),
                            )
                            .child(tone_tile(d.tone, d.icon, cx))
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .pt_1p5()
                                    .child(sentence)
                                    .when_some(d.reason.clone(), |this, reason| {
                                        this.child(
                                            div()
                                                .mt_1p5()
                                                .border_l_2()
                                                .border_color(theme.color_bg_surface)
                                                .pl_2p5()
                                                .child(Label::new(reason).size(13.).line_height(20.).color(theme.color_text_secondary)),
                                        )
                                    })
                                    .when(!d.chips.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .mt_2()
                                                .flex()
                                                .flex_wrap()
                                                .gap_1p5()
                                                .children(d.chips.iter().enumerate().map(|(n, c)| render_chip((chips_id.clone(), n), c, cx))),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id(("audit-time", e.id as u64))
                                    .flex_none()
                                    .pt_1p5()
                                    .tooltip(decibell_ui::title_tooltip(full))
                                    .child(Label::new(clock).font(FontRole::Meta).size(theme.scaled.text_meta).line_height(20.).color(theme.color_text_muted)),
                            )
                            .into_any_element()
                    }
                    Item::Footer => {
                        let weak = weak.clone();
                        let before = entries.last().map_or(0, |e| e.id);
                        // `gap-6` from the last day, then `pt-1`.
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .pb_2()
                            .pt(px(24. + 4.))
                            .map(|this| {
                                if has_more {
                                    this.child(
                                        outline_button("audit-more", if loading { "Loading…" } else { "Load older entries" }, loading, false, cx).when(
                                            !loading,
                                            |b| {
                                                b.on_click(move |_, _, cx| {
                                                    let _ = weak.update(cx, |this, cx| this.fetch(before, cx));
                                                })
                                            },
                                        ),
                                    )
                                } else if loaded && !filtering {
                                    this.child(Label::new("That\u{2019}s everything. Entries are kept for 180 days.").size(12.).color(theme.color_text_muted))
                                } else {
                                    this
                                }
                            })
                            .into_any_element()
                    }
                })
                .collect()
        })
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_times_parse_as_utc() {
        assert_eq!(parse_utc("2026-10-12 22:40 UTC"), Some(1_791_844_800));
        assert_eq!(parse_utc("until 1970-01-01 00:01 UTC"), Some(60));
        assert_eq!(parse_utc("2026-10-12 22:40"), None);
    }

    #[test]
    fn durations_round_as_the_renderer() {
        assert_eq!(duration(45), "45 s");
        assert_eq!(duration(600), "10 min");
        assert_eq!(duration(3570), "1 h");
        assert_eq!(duration(86400), "1 day");
        assert_eq!(duration(7 * 86400), "7 days");
        assert_eq!(duration(86400 * 2 - 61), "1 day");
    }

    #[test]
    fn role_changes_split_on_signs() {
        assert_eq!(role_changes("+Moderator -Member "), vec![(true, "Moderator".into()), (false, "Member".into())]);
        assert_eq!(role_changes("+Team A "), vec![(true, "Team A".into())]);
        assert_eq!(role_changes(""), vec![]);
    }

    #[test]
    fn categories_filter_to_their_actions() {
        assert!(actions_of(Category::All).is_empty());
        assert_eq!(actions_of(Category::Invites), vec!["invite_create".to_string(), "invite_revoke".to_string()]);
        assert_eq!(actions_of(Category::Moderation).len(), 7);
    }

    #[test]
    fn allow_deny_details_parse() {
        assert_eq!(parse_allow_deny("allow 1024 deny 8192"), Some((1024, 8192)));
        assert_eq!(parse_allow_deny("cleared"), None);
    }
}

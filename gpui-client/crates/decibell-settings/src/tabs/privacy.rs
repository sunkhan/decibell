//! Settings → Privacy (`PrivacyTab.tsx`, `features/e2ee/E2eeSection.tsx`): the encryption card,
//! friends-only DMs, "Show me as Away" with its threshold, link previews, unfiltered GIF search and
//! anonymous crash reports. Every control applies at once (the renderer saves on each change).
//!
//! Friends-only DMs is the account's (CLAUDE.md "User statuses"): the toggle flips the one copy
//! (`Settings::friends_only_dms`) and asks central (`actions::dm::set_friends_only_dms`); central's
//! answer comes back as `own_presence`, which the model adopts when it differs, so the switch
//! settles on the account's value. Crash reports take effect at the next start (the renderer's
//! words); reporting itself is a later task: the choice is kept where the renderer keeps it
//! (`crash_reporting_enabled` in `config.json`).

use decibell_common::Watch;
use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
use decibell_model::{Model, actions};
use decibell_ui::{
    ActiveTheme as _, Button, FontRole, Icon, IconName, Label, ScrollHandle, Select, SelectOption, SelectSize,
};
use gpui::{AnyElement, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use super::TabId;
use crate::rows::{Btn, ToggleRow, away_after_label, card, hover, quiet_outline_look};
use crate::window::CloseLink;

/// Auto-away thresholds, minutes (0 = never).
pub const AWAY_AFTER_OPTIONS: [u32; 7] = [0, 1, 5, 10, 15, 30, 60];

/// What the tab shows of the settings, compared on each change.
#[derive(Clone, PartialEq)]
struct Shown {
    friends_only: bool,
    away_after: u32,
    link_previews: bool,
    gif_unfiltered: bool,
    crash_reports: bool,
}

fn shown(s: &decibell_core::config::AppSettings) -> Shown {
    Shown {
        friends_only: s.friends_only_dms,
        away_after: s.away_after_minutes,
        link_previews: s.link_previews_enabled,
        gif_unfiltered: s.gif_unfiltered,
        crash_reports: s.crash_reporting_enabled,
    }
}

pub struct PrivacyTab {
    scroll: ScrollHandle,
    close: CloseLink,
    shown: Shown,
    _watch: Watch,
}

impl PrivacyTab {
    pub fn new(close: CloseLink, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.settings, cx, |this: &mut PrivacyTab, s, _| {
                let next = shown(s.get());
                std::mem::replace(&mut this.shown, next.clone()) != next
            })
            .store(&model.live.e2ee, cx);
        Self { scroll: ScrollHandle::new(), close, shown: shown(model.settings.read(cx).get()), _watch: watch }
    }
}

fn change(cx: &mut App, f: impl FnOnce(&mut decibell_core::config::AppSettings)) {
    let settings = Model::global(cx).settings.clone();
    settings.update(cx, |s, cx| s.change(f, cx));
}

fn open_passphrase(mode: PassphraseMode, cx: &mut App) {
    let e2ee = Model::global(cx).live.e2ee.clone();
    e2ee.update(cx, |e, cx| e.open(mode, cx));
}

impl Render for PrivacyTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.shown.clone();
        let e2ee = e2ee_section(window, cx);
        let away = away_row(s.away_after, window, cx);
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(e2ee)
            .child(
                ToggleRow::new(
                    "privacy-friends-only",
                    "Only accept DMs from friends",
                    "When enabled, only users in your friends list can send you direct messages",
                    s.friends_only,
                )
                .probe("privacy-friends-only")
                .on_toggle(move |_, _, cx| actions::dm::set_friends_only_dms(!s.friends_only, cx)),
            )
            .child(away)
            .child(
                ToggleRow::new(
                    "privacy-link-previews",
                    "Show link previews",
                    "Fetches a title, description and thumbnail for links in messages. Each site you preview sees your IP address, the same as opening the link would.",
                    s.link_previews,
                )
                .on_toggle(move |_, _, cx| change(cx, |v| v.link_previews_enabled = !s.link_previews)),
            )
            .child(
                ToggleRow::new(
                    "privacy-gif-unfiltered",
                    "Unfiltered GIF search",
                    "Search GIFs with the provider's content filter switched off, explicit content included. Off hides explicit adult content and nothing else.",
                    s.gif_unfiltered,
                )
                .on_toggle(move |_, _, cx| change(cx, |v| v.gif_unfiltered = !s.gif_unfiltered)),
            )
            .child(
                ToggleRow::new(
                    "privacy-crash-reports",
                    "Send anonymous crash reports",
                    "Helps fix bugs that happen in the field. No usernames, no message contents, no server names. Restart required for changes to apply.",
                    s.crash_reports,
                )
                // No live SDK teardown: takes effect on the next launch.
                .on_toggle(move |_, _, cx| change(cx, |v| v.crash_reporting_enabled = !s.crash_reports)),
            );
        super::pane(TabId::Privacy, &self.scroll, &self.close).child(body)
    }
}

/// "Show me as Away": the toggle row's look with a small select (`w-36`) instead of the switch.
fn away_row(minutes: u32, window: &mut Window, cx: &mut App) -> AnyElement {
    let id: gpui::ElementId = "privacy-away".into();
    let (h, listener) = hover(window, cx, &id);
    let theme = cx.theme();
    let options: Vec<SelectOption<u32>> = AWAY_AFTER_OPTIONS
        .iter()
        .map(|m| {
            let label = if *m == 0 { "Never".to_string() } else { format!("After {}", away_after_label(*m)) };
            SelectOption::new(*m, label)
        })
        .collect();
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(decibell_ui::motion::mix(theme.color_bg_light, theme.color_bg_lighter, h))
        .px_4()
        .py_3p5()
        .on_hover(listener)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .pr_4()
                .child(Label::new("Show me as Away").size(14.).weight(500.).color(theme.color_text_primary))
                .child(
                    div().mt_1().child(
                        Label::new(
                            "After this long without keyboard or mouse input or talking in voice, and right away when your screen locks or the computer sleeps. Only while your status is Online.",
                        )
                        .size(12.)
                        .line_height(12. * 1.55)
                        .color(theme.color_text_muted),
                    ),
                ),
        )
        .child(
            div().w(px(144.)).flex_none().child(
                Select::options("privacy-away-select", Some(minutes), options)
                    .size(SelectSize::Sm)
                    .on_change(|m: &u32, _, cx| {
                        let m = *m;
                        change(cx, |v| v.away_after_minutes = m);
                    }),
            ),
        )
        .into_any_element()
}

/// `E2eeSection`: the state-driven card. "unavailable" only means the core hasn't decided yet
/// (signed out, or the keys are still loading after sign-in).
fn e2ee_section(_: &mut Window, cx: &mut App) -> AnyElement {
    let model = Model::global(cx).clone();
    let e = model.live.e2ee.read(cx);
    let (supported, status, fingerprint) = (e.supported, e.status, e.fingerprint.clone());
    let theme = cx.theme();
    let primary = |id: &'static str, label: &'static str, mode: PassphraseMode| {
        div()
            .relative()
            .children(decibell_shell::probes::probe(id))
            .child(Button::new(id, label).on_click(move |_, _, cx| open_passphrase(mode, cx)))
            .into_any_element()
    };
    let secondary = |id: &'static str, label: &'static str, mode: PassphraseMode, cx: &App| {
        Btn::new(id, label, quiet_outline_look(cx), cx.theme().radius_sm)
            .on_click(move |_, _, cx| open_passphrase(mode, cx))
            .into_any_element()
    };
    let (description, badge, actions): (&str, Option<(&str, bool)>, Vec<AnyElement>) =
        if !supported || status == E2eeStatus::Unavailable {
            (
                "Encrypts your direct messages on your device so only you and the person you're talking to can read them. Checking this device's encryption keys…",
                None,
                Vec::new(),
            )
        } else if status == E2eeStatus::NotSetUp {
            (
                "Encrypt your direct messages so only you and the person you're talking to can read them. The server only ever stores encrypted data. Protected by a passphrase you choose; you'll need it on other devices.",
                Some(("Off", false)),
                vec![primary("e2ee-setup", "Set up encryption", PassphraseMode::Setup)],
            )
        } else if status == E2eeStatus::Locked {
            (
                "Your encryption keys are backed up, but this device hasn't unlocked them. Enter your passphrase to read and send encrypted messages here.",
                Some(("Locked", false)),
                vec![
                    secondary("e2ee-reset", "Reset keys", PassphraseMode::Reset, cx),
                    primary("e2ee-unlock", "Unlock", PassphraseMode::Unlock),
                ],
            )
        } else {
            (
                "Messages with anyone who has also set up encryption are sealed on your device; the server only sees encrypted data. Your fingerprint below is what others compare against your profile.",
                Some(("On", true)),
                vec![
                    secondary("e2ee-reset", "Reset keys", PassphraseMode::Reset, cx),
                    secondary("e2ee-change", "Change passphrase", PassphraseMode::Change, cx),
                ],
            )
        };
    let badge = badge.map(|(text, on)| {
        let (bg, fg) =
            if on { (theme.color_success.opacity(0.15), theme.color_success) } else { (theme.color_text_muted.opacity(0.15), theme.color_text_muted) };
        // `rounded-sm px-1.5 py-px font-channel text-[10px] font-semibold uppercase tracking-[0.06em]`.
        div()
            .rounded(theme.radius_sm)
            .px_1p5()
            .py(px(1.))
            .bg(bg)
            .child(
                Label::new(SharedString::from(text.to_uppercase()))
                    .font(FontRole::Channel)
                    .size(10.)
                    .weight(600.)
                    .tracking(0.06)
                    .color(fg),
            )
    });
    let fingerprint = (status == E2eeStatus::Ready && !fingerprint.is_empty()).then(|| {
        div()
            .mt_2p5()
            .child(
                Label::new("YOUR FINGERPRINT").size(10.).weight(600.).tracking(0.07).color(theme.color_text_muted),
            )
            .child(
                div().mt_1().child(
                    Label::new(fingerprint).font(FontRole::Mono).size(11.).tracking(0.06).color(theme.color_text_primary),
                ),
            )
    });
    card(cx)
        .px_4()
        .py_3p5()
        .child(
            div().flex().items_start().justify_between().gap_4().child(
                div()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(Icon::new(IconName::ChatLock).size(px(14.)).color(theme.color_text_secondary))
                            .child(Label::new("End-to-end encrypted DMs").size(14.).weight(500.).color(theme.color_text_primary))
                            .children(badge),
                    )
                    .child(
                        div()
                            .mt_1()
                            .child(Label::new(description).size(12.).line_height(12. * 1.55).color(theme.color_text_muted)),
                    )
                    .children(fingerprint),
            ),
        )
        .when(!actions.is_empty(), |this| this.child(div().mt_3().flex().justify_end().gap_2().children(actions)))
        .into_any_element()
}

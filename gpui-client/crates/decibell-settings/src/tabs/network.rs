//! Settings → Network (`NetworkTab.tsx`, `features/transfers/DownloadsSettings.tsx`): where
//! downloads land (the OS Downloads folder or a chosen one, "Ask where to save each file"), and
//! the attachment speed caps: presets, a custom MB/s value, Unlimited.
//!
//! A preset applies at once, as in the renderer. The custom box applies when it loses focus
//! (`onBlur`: a press anywhere else, Tab, the window going to the background) and on Enter; there is
//! no draft and no save bar, as in the renderer: leaving the tab (another tab, ✕, the backdrop)
//! applies what the box holds first, as the blur those presses cause does there, and so does Esc
//! (the renderer closes on Esc without a blur and drops the typed value: `docs/electron-quirks.md`
//! #27). The caps reach the attachment service through the model
//! (`CoreAttachmentService::set_limits` on every settings change), mid-transfer too; they are saved
//! in `config.json` (`upload_limit_bps`, `download_limit_bps`).

use std::path::PathBuf;

use decibell_common::Watch;
use decibell_model::{Model, actions};
use decibell_text::input::{InputEvent, InputState, numeric};
use decibell_ui::{ActiveTheme as _, Button, FontRole, Label, ScrollHandle, TextField, TextFieldVariant};
use gpui::{AnyElement, App, AppContext as _, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*};

use super::TabId;
use crate::rows::{Btn, Look, ToggleRow, card};
use crate::window::CloseLink;

/// Presets tuned around typical broadband allowances: "Unlimited" first, then whole MB/s.
pub const PRESETS_MBPS: [(&str, f64); 8] = [
    ("Unlimited", 0.),
    ("1 MB/s", 1.),
    ("2 MB/s", 2.),
    ("5 MB/s", 5.),
    ("10 MB/s", 10.),
    ("25 MB/s", 25.),
    ("50 MB/s", 50.),
    ("100 MB/s", 100.),
];

/// The smallest non-zero cap (below it the community's 30 s inactivity deadline could cut a slow
/// download off; the pacers clamp to the same floor).
pub const MIN_MBPS: f64 = 0.1;

pub fn bps_to_mbps(bps: u64) -> f64 {
    bps as f64 / (1024. * 1024.)
}

pub fn mbps_to_bps(mbps: f64) -> u64 {
    (mbps * 1024. * 1024.).round().max(0.) as u64
}

/// `String(Math.round(mbps * 100) / 100)`.
pub fn format_mbps(mbps: f64) -> String {
    let r = (mbps * 100.).round() / 100.;
    if r.fract() == 0. { format!("{}", r as i64) } else { format!("{r}") }
}

pub fn preset_for(bps: u64) -> Option<f64> {
    PRESETS_MBPS.iter().find(|(_, m)| mbps_to_bps(*m) == bps).map(|(_, m)| *m)
}

/// What committing the custom box does (`onBlur`): `None` leaves everything as it is (not a
/// number, or negative); else the cap in bytes per second and what the box shows after.
pub fn commit_custom(text: &str) -> Option<(u64, String)> {
    let n: f64 = parse_float(text)?;
    if n < 0. {
        return None;
    }
    let next = mbps_to_bps(if n == 0. { 0. } else { n.max(MIN_MBPS) });
    let shown = if preset_for(next).is_none() { format_mbps(bps_to_mbps(next)) } else { String::new() };
    Some((next, shown))
}

/// The step of the custom box (`step={0.5}`); its base is the minimum (`min={MIN_MBPS}`).
pub const STEP_MBPS: f64 = 0.5;

/// ArrowUp / ArrowDown in the box, as Chromium steps an `<input type="number" min="0.1" step="0.5">`
/// (`InputType::StepUpFromLayoutObject`): what isn't a number counts as 0; below the minimum,
/// ArrowUp jumps to it and ArrowDown does nothing; otherwise the value moves to the next multiple
/// of the step above (below) it, counted from the minimum. `None`: the box keeps its text.
pub fn step_custom(text: &str, up: bool) -> Option<String> {
    let current = parse_float(text).filter(|v| v.is_finite()).unwrap_or(0.);
    if up && current < MIN_MBPS {
        return Some(format_number(MIN_MBPS));
    }
    if !up && current <= MIN_MBPS {
        return None;
    }
    // Steps from the base, rounded so 7.6 - 0.1 = 7.5 counts as whole.
    let steps = ((current - MIN_MBPS) / STEP_MBPS * 1e9).round() / 1e9;
    let next = if steps.fract() == 0. {
        steps + if up { 1. } else { -1. }
    } else if up {
        steps.floor() + 1.
    } else {
        steps.ceil() - 1.
    };
    let value = MIN_MBPS + next * STEP_MBPS;
    (value >= MIN_MBPS - 1e-9).then(|| format_number(value))
}

/// A number as JavaScript prints it (`String(7.6)` → "7.6"), past f64 noise.
fn format_number(v: f64) -> String {
    let r = (v * 1e9).round() / 1e9;
    if r.fract() == 0. { format!("{}", r as i64) } else { format!("{r}") }
}

/// JS `parseFloat`: the longest leading number ("7.5abc" → 7.5).
fn parse_float(text: &str) -> Option<f64> {
    let t = text.trim_start();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_digit = false;
    for (i, c) in t.char_indices() {
        match c {
            '0'..='9' => {
                seen_digit = true;
                end = i + 1;
            }
            '.' if !seen_dot => {
                seen_dot = true;
                end = i + 1;
            }
            '+' | '-' if i == 0 => end = 1,
            _ => break,
        }
    }
    if !seen_digit {
        return None;
    }
    t[..end].trim_end_matches('.').parse().ok().or_else(|| t[..end].parse().ok())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    Upload,
    Download,
}

/// One cap's custom box (`type="number"`: what Chromium lets into one), and the text it showed
/// when last committed or synced.
struct Custom {
    input: Entity<InputState>,
    committed: String,
}

impl Custom {
    fn new(committed: String, window: &mut Window, cx: &mut Context<NetworkTab>) -> Custom {
        let input = cx.new(|cx| InputState::single_line(window, cx).accept(numeric).placeholder("e.g. 7.5").with_value(&committed));
        Custom { input, committed }
    }

    fn dirty(&self, cx: &App) -> bool {
        self.input.read(cx).value() != self.committed
    }

    fn show(&self, text: &str, cx: &mut App) {
        self.input.update(cx, |input, cx| input.set_value(text, cx));
    }
}

#[derive(Clone, PartialEq)]
struct Shown {
    upload: u64,
    download: u64,
    dir: String,
    ask: bool,
}

fn shown(s: &decibell_core::config::AppSettings) -> Shown {
    Shown { upload: s.upload_limit_bps, download: s.download_limit_bps, dir: s.download_dir.clone(), ask: s.ask_download_location }
}

/// The OS Downloads folder, where downloads land when no folder is chosen (the one the
/// attachments' download manager uses).
pub fn default_download_dir() -> PathBuf {
    actions::transfers::os_downloads_dir()
}

/// "/home/me/…/Decibell downloads": keep both ends of a long path (`shortenMiddle`, 56).
pub fn shorten_middle(p: &str, max: usize) -> String {
    let chars: Vec<char> = p.chars().collect();
    if chars.len() <= max {
        return p.to_string();
    }
    let head = ((max - 1) as f64 * 0.4).ceil() as usize;
    let tail = max - 1 - head;
    format!("{}…{}", chars[..head].iter().collect::<String>(), chars[chars.len() - tail..].iter().collect::<String>())
}

pub struct NetworkTab {
    scroll: ScrollHandle,
    close: CloseLink,
    shown: Shown,
    upload: Custom,
    download: Custom,
    default_dir: String,
    _subscriptions: Vec<Subscription>,
    _watch: Watch,
}

impl NetworkTab {
    pub fn new(close: CloseLink, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let s = shown(model.settings.read(cx).get());
        let custom_text = |bps: u64| if preset_for(bps).is_none() { format_mbps(bps_to_mbps(bps)) } else { String::new() };
        let upload = Custom::new(custom_text(s.upload), window, cx);
        let download = Custom::new(custom_text(s.download), window, cx);
        let watch = Watch::new();
        let mut subscriptions = vec![cx.observe(&model.settings, |this, settings, cx| {
            let next = shown(settings.read(cx).get());
            if next != this.shown {
                // A custom value shows in its box (`useEffect` on isCustom / mbps); a typed
                // draft is left alone.
                for (custom, bps) in [(&mut this.upload, next.upload), (&mut this.download, next.download)] {
                    if !custom.dirty(cx) && preset_for(bps).is_none() {
                        custom.committed = format_mbps(bps_to_mbps(bps));
                        custom.show(&custom.committed, cx);
                    }
                }
                this.shown = next;
                cx.notify();
            }
        })];
        for (direction, input) in [(Direction::Upload, &upload.input), (Direction::Download, &download.input)] {
            subscriptions.push(cx.subscribe(input, move |this, _, event: &InputEvent, cx| match event {
                // Enter and `onBlur` apply it; typing redraws the box only.
                InputEvent::Submit | InputEvent::Blur => this.commit(direction, cx),
                _ => {}
            }));
        }
        Self {
            scroll: ScrollHandle::new(),
            close,
            shown: s,
            upload,
            download,
            default_dir: crate::hooks::SettingsHooks::get(cx)
                .staged
                .downloads_default
                .clone()
                .unwrap_or_else(|| default_download_dir().display().to_string()),
            _subscriptions: subscriptions,
            _watch: watch,
        }
    }

    /// A box holds text it hasn't applied yet (it has the focus: a blur applies it).
    pub fn typed(&self, cx: &App) -> bool {
        self.upload.dirty(cx) || self.download.dirty(cx)
    }

    /// Leaving the tab: what the boxes hold applies first (the blur the renderer's leaving press
    /// causes).
    pub fn commit_all(&mut self, cx: &mut Context<Self>) {
        self.commit(Direction::Upload, cx);
        self.commit(Direction::Download, cx);
    }

    fn custom(&mut self, direction: Direction) -> &mut Custom {
        match direction {
            Direction::Upload => &mut self.upload,
            Direction::Download => &mut self.download,
        }
    }

    /// Apply a cap (a preset, or a committed custom value).
    fn apply(&mut self, direction: Direction, bps: u64, cx: &mut Context<Self>) {
        let (upload, download) = match direction {
            Direction::Upload => (bps, self.shown.download),
            Direction::Download => (self.shown.upload, bps),
        };
        let settings = Model::global(cx).settings.clone();
        settings.update(cx, |s, cx| {
            s.change(
                |v| {
                    v.upload_limit_bps = upload;
                    v.download_limit_bps = download;
                },
                cx,
            )
        });
    }

    fn pick_preset(&mut self, direction: Direction, mbps: f64, cx: &mut Context<Self>) {
        let c = self.custom(direction);
        c.committed.clear();
        c.show("", cx);
        self.apply(direction, mbps_to_bps(mbps), cx);
        cx.notify();
    }

    /// The custom box's `onBlur` (also Enter, and leaving the tab).
    pub fn commit(&mut self, direction: Direction, cx: &mut Context<Self>) {
        let c = self.custom(direction);
        if !c.dirty(cx) {
            return;
        }
        let text = c.input.read(cx).value().to_string();
        match commit_custom(&text) {
            Some((bps, shown)) => {
                c.committed = shown;
                c.show(&c.committed.clone(), cx);
                self.apply(direction, bps, cx);
            }
            // Not a number: nothing applies (the renderer kept the text in the box).
            None => c.committed = text,
        }
        cx.notify();
    }

    /// ArrowUp / ArrowDown in a box: the value steps as Chromium's number input does; like typing,
    /// it applies when the box loses the focus.
    fn step(&mut self, direction: Direction, up: bool, cx: &mut Context<Self>) {
        let c = self.custom(direction);
        let text = c.input.read(cx).value().to_string();
        if let Some(next) = step_custom(&text, up) {
            c.show(&next, cx);
        }
    }

    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let default = self.default_dir.clone();
        let picked = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let picked = path.display().to_string();
            cx.update(|cx| actions::transfers::set_download_dir(if picked == default { String::new() } else { picked }, cx));
        })
        .detach();
    }

    fn rate_row(&self, direction: Direction, cx: &mut Context<Self>) -> AnyElement {
        let (label, description, bps, custom) = match direction {
            Direction::Upload => (
                "Upload limit",
                "Per file. If you upload multiple files at once, each gets this cap.",
                self.shown.upload,
                &self.upload,
            ),
            Direction::Download => (
                "Download limit",
                "Per file. If you download multiple files at once, each gets this cap.",
                self.shown.download,
                &self.download,
            ),
        };
        let preset = preset_for(bps);
        let theme = cx.theme();
        let presets = div().flex().flex_wrap().gap_2().children(PRESETS_MBPS.iter().enumerate().map(|(i, (name, mbps))| {
            let selected = preset == Some(*mbps);
            let look = if selected {
                Look {
                    bg: theme.color_accent,
                    bg_hover: theme.color_accent,
                    text: theme.color_on_accent,
                    text_hover: theme.color_on_accent,
                    border: None,
                }
            } else {
                Look {
                    bg: theme.color_bg_mid,
                    bg_hover: theme.color_surface_hover,
                    text: theme.color_text_secondary,
                    text_hover: theme.color_text_primary,
                    border: None,
                }
            };
            let mbps = *mbps;
            Btn::new((if direction == Direction::Upload { "rate-up" } else { "rate-down" }, i), *name, look, theme.radius_sm)
                .text(12., 500.)
                .pad(12., 6.)
                .probe(format!("{}:{}", if direction == Direction::Upload { "rate-up" } else { "rate-down" }, name))
                .on_click(cx.listener(move |this, _, _, cx| this.pick_preset(direction, mbps, cx)))
        }));
        let box_id = if direction == Direction::Upload { "rate-up-custom" } else { "rate-down-custom" };
        card(cx)
            .p_4()
            .child(div().mb_0p5().child(Label::new(label).size(13.).weight(500.).color(theme.color_text_primary)))
            .child(div().mb_3().child(Label::new(description).size(12.).color(theme.color_text_muted)))
            .child(presets)
            .child(
                div()
                    .mt_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Label::new("Custom:").size(12.).color(theme.color_text_muted))
                    .child(
                        div()
                            .relative()
                            .children(decibell_shell::probes::probe(box_id))
                            // `type="number"`: the arrows step the value (before the field's own
                            // caret moves see them).
                            .capture_key_down(cx.listener(move |this, event: &gpui::KeyDownEvent, _, cx| {
                                let m = event.keystroke.modifiers;
                                let plain = !(m.control || m.alt || m.shift || m.platform || m.function);
                                let up = match event.keystroke.key.as_str() {
                                    "up" if plain => true,
                                    "down" if plain => false,
                                    _ => return,
                                };
                                this.step(direction, up, cx);
                                cx.stop_propagation();
                            }))
                            .child(TextField::new(box_id).variant(TextFieldVariant::Compact).editor(&custom.input)),
                    )
                    .child(Label::new("MB/s").size(12.).color(theme.color_text_muted)),
            )
            .into_any_element()
    }
}

impl Render for NetworkTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let s = self.shown.clone();
        let theme = cx.theme();
        let shown_dir = if s.dir.is_empty() { self.default_dir.clone() } else { s.dir.clone() };
        let ask = s.ask;
        let reset_look = Look {
            bg: theme.color_bg_mid,
            bg_hover: theme.color_surface_hover,
            text: theme.color_text_secondary,
            text_hover: theme.color_text_primary,
            border: None,
        };
        let downloads = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .child(div().mb_2().child(Label::new("Downloads").font(FontRole::Display).size(14.).weight(600.).color(theme.color_text_primary)))
                    .child(
                        Label::new(
                            "Where the attachments you download are saved. Progress and history are in the Transfers panel in the title bar (Ctrl+J).",
                        )
                        .size(12.)
                        .color(theme.color_text_muted),
                    ),
            )
            .child(
                card(cx)
                    .p_4()
                    .child(div().mb_0p5().child(Label::new("Location").size(13.).weight(500.).color(theme.color_text_primary)))
                    .child(
                        div().mb_3().child(
                            Label::new(if s.dir.is_empty() { "Your system's Downloads folder." } else { "A folder you chose." })
                                .size(12.)
                                .color(theme.color_text_muted),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .id("network-download-dir")
                                    .min_w_0()
                                    .flex_1()
                                    .rounded(theme.radius_sm)
                                    .border_1()
                                    .border_color(theme.color_border)
                                    .bg(theme.color_bg_mid)
                                    .px_2p5()
                                    .py_1p5()
                                    .tooltip(decibell_ui::title_tooltip(SharedString::from(shown_dir.clone())))
                                    .child(Label::new(shorten_middle(&shown_dir, 56)).size(12.).color(theme.color_text_secondary).truncate()),
                            )
                            .child(Button::new("network-change-dir", "Change…").on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))))
                            .when(!s.dir.is_empty(), |this| {
                                this.child(
                                    Btn::new("network-reset-dir", "Reset", reset_look, theme.radius_sm)
                                        .text(13., 600.)
                                        .on_click(|_, _, cx| actions::transfers::set_download_dir(String::new(), cx)),
                                )
                            }),
                    ),
            )
            .child(
                ToggleRow::new(
                    "network-ask",
                    "Ask where to save each file",
                    "Show a save dialog for every download instead of saving straight to the folder above.",
                    ask,
                )
                .on_toggle(move |_, _, cx| actions::transfers::set_ask_download_location(!ask, cx)),
            );
        let theme = cx.theme();
        let speed_intro = div()
            .mt_4()
            .child(
                div().mb_2().child(
                    Label::new("Attachment transfer speed").font(FontRole::Display).size(14.).weight(600.).color(theme.color_text_primary),
                ),
            )
            .child(
                div().mb_4().child(
                    Label::new(
                        "Limit how much of your connection attachments use — the files, images, videos and audio you send or open. Takes effect immediately, even mid-transfer. Voice, streams, link previews and GIFs are never limited.",
                    )
                    .size(12.)
                    .color(theme.color_text_muted),
                ),
            );
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(downloads)
            .child(speed_intro)
            .child(self.rate_row(Direction::Upload, cx))
            .child(self.rate_row(Direction::Download, cx));
        super::pane(TabId::Network, &self.scroll, &self.close).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_as_the_renderer_computes_them() {
        assert_eq!(mbps_to_bps(1.), 1_048_576);
        assert_eq!(preset_for(mbps_to_bps(25.)), Some(25.));
        assert_eq!(preset_for(1234), None);
        assert_eq!(format_mbps(7.5), "7.5");
        assert_eq!(format_mbps(7.0), "7");
        assert_eq!(commit_custom("7.5"), Some((mbps_to_bps(7.5), "7.5".into())));
        assert_eq!(commit_custom("0.01"), Some((mbps_to_bps(0.1), "0.1".into())), "clamped to the floor");
        assert_eq!(commit_custom("5"), Some((mbps_to_bps(5.), String::new())), "a preset clears the box");
        assert_eq!(commit_custom("0"), Some((0, String::new())));
        assert_eq!(commit_custom("abc"), None);
        assert_eq!(commit_custom("-3"), None);
        assert_eq!(commit_custom("7.5MB"), Some((mbps_to_bps(7.5), "7.5".into())));
    }

    /// Chromium's ArrowUp / ArrowDown on `<input type="number" min="0.1" step="0.5">`.
    #[test]
    fn arrows_step_as_chromium_does() {
        let up = |t: &str| step_custom(t, true);
        let down = |t: &str| step_custom(t, false);
        assert_eq!(up(""), Some("0.1".into()), "empty: the minimum");
        assert_eq!(up("0"), Some("0.1".into()));
        assert_eq!(up("abc"), Some("0.1".into()));
        assert_eq!(down(""), None, "nothing below the minimum");
        assert_eq!(down("0.1"), None);
        assert_eq!(up("0.1"), Some("0.6".into()));
        assert_eq!(up("0.6"), Some("1.1".into()));
        assert_eq!(down("1.1"), Some("0.6".into()));
        assert_eq!(up("7.5"), Some("7.6".into()), "off the grid: the next value on it");
        assert_eq!(down("7.5"), Some("7.1".into()));
        assert_eq!(up("7.6"), Some("8.1".into()));
        assert_eq!(down("0.3"), Some("0.1".into()));
        assert_eq!(up("100.1"), Some("100.6".into()));
    }

    #[test]
    fn long_paths_keep_both_ends() {
        let p = "/home/someone/a/very/long/path/that/goes/on/and/on/Decibell downloads";
        let s = shorten_middle(p, 56);
        assert_eq!(s.chars().count(), 56);
        assert!(s.starts_with("/home/someone/a/very") && s.ends_with("Decibell downloads"));
        assert_eq!(shorten_middle("/short", 56), "/short");
    }
}

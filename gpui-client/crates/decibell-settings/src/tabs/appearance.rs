//! Settings → Appearance (`AppearanceTab.tsx`): the seven theme cards (each drawn in its own
//! palette from the generated `.theme-preview` scope), text size and list density (the generated
//! ranges and steps; applied live through the model's `Settings`, which the app turns into
//! `Theme::set_scale`, and mirrored into `ui.json` so the next start opens with them), and the
//! time format.
//!
//! Arrow keys move the focus between the cards and Enter / Space picks one (the renderer's
//! manual-activation radio group: picking on every arrow would repaint the whole window per key).

use decibell_common::{UserAvatar, Watch};
use decibell_model::Model;
use decibell_model::chat::time::format_clock;
use decibell_model::stores::settings::TimeFormat;
use decibell_theme::{
    DEFAULT_ROW_SCALE, DEFAULT_TEXT_SIZE_PX, ROW_SCALE_MAX, ROW_SCALE_MIN, ROW_SCALE_STEP, TEXT_SIZE_MAX_PX,
    TEXT_SIZE_MIN_PX, TEXT_SIZE_STEP_PX, ThemeId, ThemeMode,
};
use decibell_ui::identity::person_slot;
use decibell_ui::{
    ActiveTheme as _, FontRole, Icon, IconName, Label, LetterAvatar, Presence, ScrollHandle, Segment,
    SegmentedControl, Slider, StatusDot, hsla,
};
use gpui::{
    AnyElement, App, Context, FocusHandle, IntoElement, Render, SharedString, Window, div, prelude::*, px, relative,
};

use super::TabId;
use crate::rows::{Btn, Look, hover, section_label};
use crate::window::CloseLink;

/// An afternoon time, so the 12- and 24-hour samples differ (`SAMPLE_TIME`, 2026-01-01 14:32 local).
fn sample_time() -> i64 {
    use chrono::{Local, TimeZone as _};
    Local.with_ymd_and_hms(2026, 1, 1, 14, 32, 0).single().map_or(0, |t| t.timestamp())
}

#[derive(Clone, Copy, PartialEq)]
struct Shown {
    theme: ThemeId,
    text_size: f64,
    row_scale: f64,
    time_format: TimeFormat,
}

fn shown(s: &decibell_model::stores::settings::Settings) -> Shown {
    Shown { theme: s.theme(), text_size: s.text_size_px(), row_scale: s.row_scale(), time_format: s.time_format() }
}

pub struct AppearanceTab {
    scroll: ScrollHandle,
    close: CloseLink,
    shown: Shown,
    /// Our uid: the sample message is ours (renames and our picture redraw it).
    me: decibell_model::Uid,
    cards: Vec<FocusHandle>,
    _watch: Watch,
}

impl AppearanceTab {
    pub fn new(close: CloseLink, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.settings, cx, |this: &mut AppearanceTab, s, _| {
                let next = shown(s);
                std::mem::replace(&mut this.shown, next) != next
            })
            .store_if(&model.auth, cx, |this: &mut AppearanceTab, auth, _| std::mem::replace(&mut this.me, auth.uid) != auth.uid)
            .people(cx, |this: &AppearanceTab, uid| uid == this.me);
        let cards = ThemeId::ALL.iter().map(|_| cx.focus_handle()).collect();
        let me = model.auth.read(cx).uid;
        Self { scroll: ScrollHandle::new(), close, shown: shown(model.settings.read(cx)), me, cards, _watch: watch }
    }

    fn move_focus(&mut self, by: isize, absolute: Option<usize>, window: &mut Window, cx: &mut App) {
        let count = self.cards.len() as isize;
        let current = self.cards.iter().position(|f| f.is_focused(window)).unwrap_or(self.shown.theme.index()) as isize;
        let next = match absolute {
            Some(i) => i as isize,
            None => (current + by).rem_euclid(count),
        };
        self.cards[next as usize].focus(window, cx);
    }
}

fn commit(theme: ThemeId, cx: &mut App) {
    let settings = Model::global(cx).settings.clone();
    if settings.read(cx).theme() != theme {
        settings.update(cx, |s, cx| s.set_theme(theme, cx));
    }
}

/// The selected card's tick: `<circle fill=accent>` + the check stroked in `var(--p-content)`.
fn card_tick(accent: gpui::Hsla, content: gpui::Hsla) -> impl IntoElement {
    // The circle is r = 10 in the 24-unit box of a 16 px glyph: 13.33 px across, centred.
    let d = 16. * 20. / 24.;
    div()
        .absolute()
        .top(px(6.))
        .right(px(6.))
        .size(px(16.))
        .flex()
        .items_center()
        .justify_center()
        .child(div().absolute().size(px(d)).rounded_full().bg(accent))
        .child(Icon::new(IconName::SettingsUiCardCheck).size(px(16.)).color(content))
}

fn theme_card(this: &AppearanceTab, id: ThemeId, window: &mut Window, cx: &mut Context<AppearanceTab>) -> AnyElement {
    let element_id: gpui::ElementId = ("theme-card", id.index()).into();
    let (h, listener) = hover(window, cx, &element_id);
    let selected = id == this.shown.theme;
    let focus = this.cards[id.index()].clone();
    let theme = cx.theme();
    let p = &id.scopes().theme_preview;
    let (chrome, sidebar, content, divider, accent, secondary, muted) = (
        hsla(p.color_bg_darkest),
        hsla(p.color_bg_dark),
        hsla(p.color_bg_mid),
        hsla(p.color_border_divider),
        hsla(p.color_accent),
        hsla(p.color_text_secondary),
        hsla(p.color_text_muted),
    );
    let ring = theme.shadow_ring.clone();
    let focus_ring = ring.clone();
    let mode = match id.mode() {
        ThemeMode::Dark => "DARK",
        ThemeMode::Light => "LIGHT",
    };
    let preview = div()
        .relative()
        .flex()
        .h(px(76.))
        .w_full()
        .overflow_hidden()
        .rounded(theme.radius_md)
        .child(div().w(px(10.)).flex_none().bg(chrome))
        .child(div().w(px(26.)).flex_none().border_r_1().border_color(divider).bg(sidebar))
        .child(
            div()
                .flex()
                .flex_1()
                .flex_col()
                .justify_center()
                .gap_1p5()
                .bg(content)
                .px_2p5()
                .child(div().h_1().w(relative(0.6)).rounded_full().bg(secondary))
                .child(div().h_1().w(relative(0.4)).rounded_full().bg(muted))
                .child(div().mt_0p5().h_3().w(px(42.)).rounded(theme.radius_sm).bg(accent)),
        )
        .when(selected, |this| this.child(card_tick(accent, content)));
    div()
        .id(element_id)
        .track_focus(&focus)
        .flex()
        .h(px(132.))
        .flex_col()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(if selected { theme.color_accent } else { theme.color_border_divider })
        .bg(decibell_ui::motion::mix(theme.color_bg_light, theme.color_bg_lighter, h))
        .p_2p5()
        .cursor_pointer()
        .when(selected, |this| this.shadow(ring))
        .focus_visible(move |style| style.shadow(focus_ring))
        .on_hover(listener)
        .relative()
        .children(decibell_shell::probes::probe(format!("theme-card:{}", id.as_str())))
        .on_click(move |_, _, cx| commit(id, cx))
        .on_key_down(move |event, _, cx| {
            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                cx.stop_propagation();
                commit(id, cx);
            }
        })
        .child(preview)
        .child(
            div()
                .mt_auto()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div().min_w_0().child(
                        Label::new(id.display_name()).size(13.).weight(500.).color(theme.color_text_primary).truncate(),
                    ),
                )
                .child(
                    div().flex_none().rounded(theme.radius_sm).bg(theme.color_surface_hover).px_1p5().py_0p5().child(
                        Label::new(mode).font(FontRole::Meta).size(9.5).tracking(0.1).color(theme.color_text_muted),
                    ),
                ),
        )
        .into_any_element()
}

/// `ScaleSlider`'s frame: the label row (readout, Reset), the slider between its two end labels,
/// and the preview card under it.
#[allow(clippy::too_many_arguments)]
fn scale_slider(
    id: &'static str,
    label: &'static str,
    readout: String,
    ends: (AnyElement, AnyElement),
    slider: Slider,
    is_default: bool,
    reset: impl Fn(&mut App) + 'static,
    preview: AnyElement,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let reset_look = Look {
        bg: gpui::Hsla::transparent_black(),
        bg_hover: theme.color_surface_hover,
        text: theme.color_text_muted,
        text_hover: theme.color_text_primary,
        border: None,
    };
    div()
        .mt_6()
        .child(
            div()
                .mb_2p5()
                .flex()
                .items_baseline()
                .justify_between()
                .child(
                    Label::new(SharedString::from(label.to_uppercase()))
                        .size(11.)
                        .weight(600.)
                        .tracking(0.07)
                        .color(theme.color_text_muted),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(Label::new(readout).font(FontRole::Meta).size(10.5).color(theme.color_text_muted))
                        .child(
                            Btn::new(gpui::ElementId::Name(format!("{id}-reset").into()), "Reset", reset_look, theme.radius_sm)
                                .text(10.5, 500.)
                                .pad(6., 2.)
                                .disabled(is_default)
                                .hide_when_disabled()
                                .on_click(move |_, _, cx| reset(cx)),
                        ),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().w_12().flex_none().child(ends.0))
                .child(div().flex_1().child(slider))
                .child(div().w_12().flex_none().flex().justify_end().child(ends.1)),
        )
        .child(
            div()
                .mt_3()
                .overflow_hidden()
                .rounded(theme.radius_md)
                .border_1()
                .border_color(theme.color_border_divider)
                .bg(theme.color_bg_light)
                .px_3()
                .py_2p5()
                .child(preview),
        )
        .into_any_element()
}

/// `${Math.round(px * 10) / 10}px`.
fn px_readout(value: f32) -> String {
    let rounded = (value * 10.).round() / 10.;
    if rounded.fract() == 0. { format!("{}px", rounded as i64) } else { format!("{rounded}px") }
}

impl Render for AppearanceTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let (me, username) = {
            let auth = model.auth.read(cx);
            (auth.uid, auth.username.clone())
        };
        let s = self.shown;
        let cards: Vec<AnyElement> = ThemeId::ALL.into_iter().map(|id| theme_card(self, id, window, cx)).collect();
        let theme = cx.theme();
        let grid = div()
            .grid()
            .grid_cols(2)
            .gap_3()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                let (by, absolute) = match event.keystroke.key.as_str() {
                    "right" | "down" => (1, None),
                    "left" | "up" => (-1, None),
                    "home" => (0, Some(0)),
                    "end" => (0, Some(ThemeId::ALL.len() - 1)),
                    _ => return,
                };
                cx.stop_propagation();
                this.move_focus(by, absolute, window, cx);
            }))
            .children(cards);

        // Text size: a real message row (the sample scales as the chat will).
        let name_color = decibell_ui::identity::name_color(theme, person_slot(me, username.as_deref().unwrap_or("?")));
        let time_label = format!("Today at {}", format_clock(sample_time(), s.time_format));
        let scaled = theme.scaled;
        let avatar: AnyElement = match &username {
            Some(name) => UserAvatar::new(me, name.clone(), 38.).into_any_element(),
            None => LetterAvatar::new(0, "?", 38.).into_any_element(),
        };
        let text_preview = div()
            .flex()
            .gap_3()
            .child(avatar)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .child(
                                Label::new(username.clone().unwrap_or_else(|| "You".into()))
                                    .font(FontRole::Channel)
                                    .size(scaled.text_sender)
                                    .weight(theme.palette.font_weight_emphasis)
                                    .color(name_color),
                            )
                            .child(Label::new(time_label).font(FontRole::Meta).size(scaled.text_meta).color(theme.color_text_muted)),
                    )
                    .child(
                        div().mt_0p5().child(
                            Label::new("The quick brown fox jumps over the lazy dog.")
                                .size(scaled.text_body)
                                .line_height(scaled.text_body * theme.palette.leading_body)
                                .color(theme.color_text_primary),
                        ),
                    ),
            )
            .into_any_element();
        let text_slider = Slider::new("appearance-text-size", s.text_size as f32, TEXT_SIZE_MIN_PX, TEXT_SIZE_MAX_PX)
            .step(TEXT_SIZE_STEP_PX)
            .on_change(|value: &f32, _, cx| {
                let settings = Model::global(cx).settings.clone();
                let value = *value as f64;
                settings.update(cx, |s, cx| {
                    if s.text_size_px() != value {
                        s.set_text_size_px(value, cx)
                    }
                });
            });
        let text_size = scale_slider(
            "appearance-text",
            "Text size",
            px_readout(scaled.text_body),
            (
                Label::new("A").size(10.).color(theme.color_text_muted).into_any_element(),
                Label::new("A").size(15.).color(theme.color_text_muted).into_any_element(),
            ),
            text_slider,
            (s.text_size - DEFAULT_TEXT_SIZE_PX as f64).abs() < 0.001,
            |cx| {
                let settings = Model::global(cx).settings.clone();
                settings.update(cx, |s, cx| s.set_text_size_px(DEFAULT_TEXT_SIZE_PX as f64, cx));
            },
            text_preview,
            cx,
        );

        // List density: a members-list block (`.list-row` at 7 / 8 / 10 px base metrics).
        let theme = cx.theme();
        let k = s.row_scale as f32;
        let rows = [("sunkhan", true), ("nova", false)].map(|(name, online)| {
            div()
                .flex()
                .items_center()
                .rounded(theme.radius_sm)
                .py(px(7. * k))
                .px(px(8. * k))
                .gap(px(10. * k))
                .child(
                    div()
                        .relative()
                        .flex_none()
                        .child(LetterAvatar::new(0, name, 34. * k))
                        .child(
                            div().absolute().bottom(px(-1.)).right(px(-1.)).child(
                                StatusDot::new(if online { Presence::Online } else { Presence::Offline }, theme.color_bg_light)
                                    .size(13. * k),
                            ),
                        ),
                )
                .child(
                    div().min_w_0().child(
                        Label::new(name)
                            .font(FontRole::Channel)
                            .size(theme.scaled.text_member)
                            .weight(if online { 500. } else { 400. })
                            .color(if online { theme.color_text_secondary } else { theme.color_text_muted })
                            .truncate(),
                    ),
                )
                .into_any_element()
        });
        let density_preview = div().mx(px(-4.)).children(rows).into_any_element();
        let density_slider = Slider::new("appearance-density", k, ROW_SCALE_MIN, ROW_SCALE_MAX)
            .step(ROW_SCALE_STEP)
            .on_change(|value: &f32, _, cx| {
                let settings = Model::global(cx).settings.clone();
                let value = *value as f64;
                settings.update(cx, |s, cx| {
                    if (s.row_scale() - value).abs() > 1e-6 {
                        s.set_row_scale(value, cx)
                    }
                });
            });
        let density = scale_slider(
            "appearance-density",
            "List density",
            format!("{}%", (s.row_scale * 100.).round() as i64),
            (
                Label::new("Compact").size(10.5).color(theme.color_text_muted).into_any_element(),
                Label::new("Roomy").size(10.5).color(theme.color_text_muted).into_any_element(),
            ),
            density_slider,
            (s.row_scale - DEFAULT_ROW_SCALE as f64).abs() < 0.001,
            |cx| {
                let settings = Model::global(cx).settings.clone();
                settings.update(cx, |s, cx| s.set_row_scale(DEFAULT_ROW_SCALE as f64, cx));
            },
            density_preview,
            cx,
        );

        let theme = cx.theme();
        let sample = sample_time();
        let note = if s.time_format == TimeFormat::Auto {
            format!("Follows your system language: {}.", format_clock(sample, TimeFormat::Auto))
        } else {
            format!("Times show as {}.", format_clock(sample, s.time_format))
        };
        let time = div()
            .mt_6()
            .child(section_label("Time format", false, cx))
            .child(
                SegmentedControl::new(
                    "appearance-time-format",
                    vec![
                        Segment::new(TimeFormat::Auto, "Automatic"),
                        Segment::new(TimeFormat::H12, "12-hour"),
                        Segment::new(TimeFormat::H24, "24-hour"),
                    ],
                    s.time_format,
                )
                .on_change(|format: &TimeFormat, _, cx| {
                    let settings = Model::global(cx).settings.clone();
                    let format = *format;
                    if settings.read(cx).time_format() != format {
                        settings.update(cx, |s, cx| s.set_time_format(format, cx));
                    }
                }),
            )
            .child(div().mt_2().child(Label::new(note).size(12.).line_height(12. * 1.55).color(theme.color_text_muted)));

        let body = div()
            .child(section_label("Theme", false, cx))
            .child(grid)
            .child(text_size)
            .child(density)
            .child(time);
        super::pane(TabId::Appearance, &self.scroll, &self.close).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readouts() {
        assert_eq!(px_readout(14.5), "14.5px");
        assert_eq!(px_readout(15.0), "15px");
        assert_eq!(px_readout(13.96), "14px");
    }
}

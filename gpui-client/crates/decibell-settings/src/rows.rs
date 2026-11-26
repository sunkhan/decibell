//! The pieces the tabs repeat: the uppercase section label, the card, the toggle row, and the
//! buttons and text links whose classes the kit's variants don't cover (each documents the TSX
//! classes it draws; the kit's `Button` is used wherever a variant matches exactly).

use std::rc::Rc;

use decibell_ui::motion::{Transition, mix};
use decibell_ui::{ActiveTheme as _, Hover, Label, Switch};
use gpui::{
    App, ClickEvent, ElementId, Hsla, IntoElement, Pixels, RenderOnce, SharedString, Window, div, prelude::*, px,
};

type Click = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type Toggle = Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

/// `mb-2.5 pl-0.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted` (the
/// tabs' own `SectionLabel`; Appearance's has no `pl-0.5`: `inset = false`).
pub fn section_label(text: &'static str, inset: bool, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .mb_2p5()
        .when(inset, |this| this.pl_0p5())
        .child(
            Label::new(SharedString::from(text.to_uppercase()))
                .size(11.)
                .weight(600.)
                .tracking(0.07)
                .color(theme.color_text_muted),
        )
}

/// `rounded-md border border-border-divider bg-bg-light`.
pub fn card(cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(theme.color_border_divider)
        .bg(theme.color_bg_light)
}

/// The settings toggle row (`ToggleRow` / `ToggleSwitch` in Privacy, Audio, Codecs,
/// DownloadsSettings): `flex items-center justify-between rounded-md border border-border-divider
/// bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter`, the title `text-[14px]
/// font-medium text-text-primary` (Codecs: `text-text-muted` while disabled), the description
/// `mt-1 text-[12px] leading-[1.55] text-text-muted`, the switch at the right.
#[derive(IntoElement)]
pub struct ToggleRow {
    id: ElementId,
    title: SharedString,
    description: SharedString,
    on: bool,
    disabled: bool,
    on_toggle: Option<Toggle>,
    probe: Option<SharedString>,
}

impl ToggleRow {
    pub fn new(id: impl Into<ElementId>, title: impl Into<SharedString>, description: impl Into<SharedString>, on: bool) -> Self {
        Self { id: id.into(), title: title.into(), description: description.into(), on, disabled: false, on_toggle: None, probe: None }
    }

    /// Where the switch is, for the self-tests (`decibell_shell::probes`).
    pub fn probe(mut self, key: impl Into<SharedString>) -> Self {
        self.probe = Some(key.into());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_toggle(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ToggleRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = Hover::new(window, cx, self.id.clone(), Some(Transition::TAILWIND));
        let (h, listener) = (hover.progress, hover.listener());
        let theme = cx.theme();
        let bg = mix(theme.color_bg_light, theme.color_bg_lighter, h);
        let title = if self.disabled { theme.color_text_muted } else { theme.color_text_primary };
        let switch = {
            let switch = Switch::new((self.id.clone(), "switch"), self.on).disabled(self.disabled);
            match self.on_toggle {
                Some(handler) => switch.on_toggle(move |on, window, cx| handler(on, window, cx)),
                None => switch,
            }
        };
        let switch = div()
            .relative()
            .flex_none()
            .children(self.probe.and_then(decibell_shell::probes::probe))
            .child(switch);
        div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_between()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border_divider)
            .bg(bg)
            .px_4()
            .py_3p5()
            .on_hover(listener)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr_4()
                    .child(Label::new(self.title).size(14.).weight(500.).color(title))
                    .child(
                        div().mt_1().child(
                            Label::new(self.description).size(12.).line_height(12. * 1.55).color(theme.color_text_muted),
                        ),
                    ),
            )
            .child(switch)
    }
}

/// The colours of a [`Btn`] at rest and hovered.
#[derive(Clone, Copy)]
pub struct Look {
    pub bg: Hsla,
    pub bg_hover: Hsla,
    pub text: Hsla,
    pub text_hover: Hsla,
    pub border: Option<(Hsla, Hsla)>,
}

/// A text button with the TSX's exact padding, size, weight and radius where the kit's variants
/// differ (see each call). Focus ring, `disabled:opacity-50`, transitions as `transition-colors`.
#[derive(IntoElement)]
pub struct Btn {
    id: ElementId,
    label: SharedString,
    look: Look,
    size: f32,
    weight: f32,
    pad: (f32, f32),
    radius: Pixels,
    disabled: bool,
    /// `disabled:opacity-0` (Appearance's Reset) instead of 0.5.
    hide_disabled: bool,
    on_click: Option<Click>,
    probe: Option<SharedString>,
}

impl Btn {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, look: Look, radius: Pixels) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            look,
            size: 13.,
            weight: 500.,
            pad: (16., 8.),
            radius,
            disabled: false,
            hide_disabled: false,
            on_click: None,
            probe: None,
        }
    }

    /// Where the button is, for the self-tests (`decibell_shell::probes`).
    pub fn probe(mut self, key: impl Into<SharedString>) -> Self {
        self.probe = Some(key.into());
        self
    }

    /// Font size and weight.
    pub fn text(mut self, size: f32, weight: f32) -> Self {
        self.size = size;
        self.weight = weight;
        self
    }

    /// Horizontal and vertical padding, px.
    pub fn pad(mut self, x: f32, y: f32) -> Self {
        self.pad = (x, y);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn hide_when_disabled(mut self) -> Self {
        self.hide_disabled = true;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Btn {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = Hover::new(window, cx, self.id.clone(), Some(Transition::TAILWIND));
        let (h, listener) = (hover.progress, hover.listener());
        let focus = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle().tab_stop(!self.disabled))
            .read(cx)
            .clone();
        let h = if self.disabled { 0. } else { h };
        let theme = cx.theme();
        let ring = theme.shadow_ring.clone();
        let look = self.look;
        let disabled = self.disabled;
        div()
            .id(self.id)
            .track_focus(&focus)
            .relative()
            .children(self.probe.and_then(decibell_shell::probes::probe))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .px(px(self.pad.0))
            .py(px(self.pad.1))
            .rounded(self.radius)
            .bg(mix(look.bg, look.bg_hover, h))
            .when_some(look.border, |this, (rest, hovered)| this.border_1().border_color(mix(rest, hovered, h)))
            .focus_visible(move |style| style.shadow(ring))
            .on_hover(listener)
            .map(|this| {
                if disabled {
                    this.opacity(if self.hide_disabled { 0. } else { 0.5 }).cursor_default()
                } else {
                    this.cursor_pointer()
                        .when_some(self.on_click, |this, handler| this.on_click(move |e, window, cx| handler(e, window, cx)))
                }
            })
            .child(
                Label::new(self.label)
                    .size(self.size)
                    .line_height(self.size * 1.5)
                    .weight(self.weight)
                    .color(mix(look.text, look.text_hover, h))
                    .nowrap(),
            )
    }
}

/// An inline text button: `text-[size] <weight> <color> hover:<hover colour>`, underlined on hover
/// when `underline` (`underline-offset-2 hover:underline`).
#[derive(IntoElement)]
pub struct TextLink {
    id: ElementId,
    label: SharedString,
    size: f32,
    weight: f32,
    color: Hsla,
    hover_color: Hsla,
    underline: bool,
    disabled: bool,
    on_click: Option<Click>,
    probe: Option<SharedString>,
}


impl TextLink {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, color: Hsla, hover_color: Hsla) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            size: 12.,
            weight: 400.,
            color,
            hover_color,
            underline: true,
            disabled: false,
            on_click: None,
            probe: None,
        }
    }

    /// Where the link is, for the self-tests (`decibell_shell::probes`).
    pub fn probe(mut self, key: impl Into<SharedString>) -> Self {
        self.probe = Some(key.into());
        self
    }

    pub fn text(mut self, size: f32, weight: f32) -> Self {
        self.size = size;
        self.weight = weight;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for TextLink {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state((self.id.clone(), "hover"), cx, |_, _| false);
        let focus = window
            .use_keyed_state((self.id.clone(), "focus"), cx, |_, cx| cx.focus_handle().tab_stop(!self.disabled))
            .read(cx)
            .clone();
        let hovered = *state.read(cx) && !self.disabled;
        let color = if hovered { self.hover_color } else { self.color };
        let ring = cx.theme().shadow_ring.clone();
        let radius = cx.theme().radius_sm;
        let disabled = self.disabled;
        div()
            .id(self.id)
            .track_focus(&focus)
            .relative()
            .children(self.probe.and_then(decibell_shell::probes::probe))
            .flex_none()
            .rounded(radius)
            .focus_visible(move |style| style.shadow(ring))
            .on_hover(move |on, _, cx| {
                state.update(cx, |state, cx| {
                    if *state != *on {
                        *state = *on;
                        cx.notify();
                    }
                })
            })
            .map(|this| {
                if disabled {
                    this.opacity(0.5)
                } else {
                    this.cursor_pointer()
                        .when_some(self.on_click, |this, handler| this.on_click(move |e, window, cx| handler(e, window, cx)))
                }
            })
            .when(hovered && self.underline, |this| this.underline().text_decoration_color(color))
            .child(Label::new(self.label).size(self.size).line_height(self.size * 1.5).weight(self.weight).color(color))
    }
}


/// `"1 minute"`, `"5 minutes"`, `"1 hour"` (`StatusMenu.tsx` `awayAfterLabel`).
pub fn away_after_label(minutes: u32) -> String {
    if minutes == 0 {
        return "Never".into();
    }
    if minutes.is_multiple_of(60) {
        return if minutes == 60 { "1 hour".into() } else { format!("{} hours", minutes / 60) };
    }
    if minutes == 1 { "1 minute".into() } else { format!("{minutes} minutes") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_as_the_renderer_words_them() {
        assert_eq!(away_after_label(0), "Never");
        assert_eq!(away_after_label(1), "1 minute");
        assert_eq!(away_after_label(15), "15 minutes");
        assert_eq!(away_after_label(60), "1 hour");
        assert_eq!(away_after_label(120), "2 hours");
    }
}

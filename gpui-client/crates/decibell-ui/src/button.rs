//! Buttons: the renderer's class strings as variants.
//!
//! | Variant | Renderer classes |
//! |---|---|
//! | [`ButtonVariant::Primary`] | `rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50` (CLAUDE.md's idiom; no transition) |
//! | [`ButtonVariant::Secondary`] | `rounded-md border border-border bg-transparent font-medium text-text-primary transition-colors hover:bg-surface-hover` (ConfirmModal Cancel) |
//! | [`ButtonVariant::Ghost`] | `rounded-sm font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary` (SaveBar Reset) |
//! | [`ButtonVariant::Danger`] | `rounded-md bg-error font-semibold text-on-error transition-colors hover:bg-error/90` (ConfirmModal) |
//! | [`ButtonVariant::DangerSoft`] | `rounded-md border border-error/20 bg-error/10 font-medium text-error transition-colors hover:bg-error/20` |
//! | [`ButtonVariant::DangerOutline`] | `rounded-sm border border-error/40 font-semibold text-error hover:border-error/70 hover:bg-error/10` |
//! | [`ButtonVariant::Soft`] | `rounded-sm bg-accent-soft font-medium text-accent-bright transition-colors hover:bg-accent-mid` |
//!
//! Sizes: [`ButtonSize::Md`] `px-4 py-2 text-[13px]`, [`ButtonSize::Sm`] `px-3 py-1.5 text-[12px]`.
//! Every button takes the theme's focus ring on keyboard focus (`focus-visible:shadow-ring`),
//! is activated by Enter / Space while focused, and goes to 50 % with a not-allowed cursor when
//! disabled. [`IconButton`] is the square glyph button (close, dismiss); [`LinkButton`] the text
//! link (a toast's action, "Reset" in a menu).

use gpui::{
    AnyElement, App, ClickEvent, ElementId, Hsla, IntoElement, Pixels, RenderOnce, SharedString,
    Window, div, prelude::*, px,
};

use crate::interaction::{Hover, focus_handle, ring};
use crate::motion::{Transition, mix};
use crate::{ActiveTheme as _, Icon, IconName, Label, Spinner};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
    DangerSoft,
    DangerOutline,
    Soft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonSize {
    /// `px-4 py-2 text-[13px]`: 35.5 px tall.
    Md,
    /// `px-3 py-1.5 text-[12px]`: 30 px tall.
    Sm,
}

/// The colours of one variant at rest and under the pointer.
struct Look {
    bg: Hsla,
    bg_hover: Hsla,
    text: Hsla,
    text_hover: Hsla,
    border: Option<(Hsla, Hsla)>,
    weight: f32,
    radius: Pixels,
    transition: Option<Transition>,
}

impl ButtonVariant {
    fn look(self, cx: &App) -> Look {
        let t = cx.theme();
        let clear = Hsla::transparent_black();
        match self {
            ButtonVariant::Primary => Look {
                bg: t.color_accent,
                bg_hover: t.color_accent_hover,
                text: t.color_on_accent,
                text_hover: t.color_on_accent,
                border: None,
                weight: 600.,
                radius: t.radius_sm,
                transition: None,
            },
            ButtonVariant::Secondary => Look {
                bg: clear,
                bg_hover: t.color_surface_hover,
                text: t.color_text_primary,
                text_hover: t.color_text_primary,
                border: Some((t.color_border, t.color_border)),
                weight: 500.,
                radius: t.radius_md,
                transition: Some(Transition::TAILWIND),
            },
            ButtonVariant::Ghost => Look {
                bg: clear,
                bg_hover: t.color_surface_hover,
                text: t.color_text_secondary,
                text_hover: t.color_text_primary,
                border: None,
                weight: 600.,
                radius: t.radius_sm,
                transition: None,
            },
            ButtonVariant::Danger => Look {
                bg: t.color_error,
                bg_hover: t.color_error.opacity(0.9),
                text: t.color_on_error,
                text_hover: t.color_on_error,
                border: None,
                weight: 600.,
                radius: t.radius_md,
                transition: Some(Transition::TAILWIND),
            },
            ButtonVariant::DangerSoft => Look {
                bg: t.color_error.opacity(0.1),
                bg_hover: t.color_error.opacity(0.2),
                text: t.color_error,
                text_hover: t.color_error,
                border: Some((t.color_error.opacity(0.2), t.color_error.opacity(0.2))),
                weight: 500.,
                radius: t.radius_md,
                transition: Some(Transition::TAILWIND),
            },
            ButtonVariant::DangerOutline => Look {
                bg: clear,
                bg_hover: t.color_error.opacity(0.1),
                text: t.color_error,
                text_hover: t.color_error,
                border: Some((t.color_error.opacity(0.4), t.color_error.opacity(0.7))),
                weight: 600.,
                radius: t.radius_sm,
                transition: None,
            },
            ButtonVariant::Soft => Look {
                bg: t.color_accent_soft,
                bg_hover: t.color_accent_mid,
                text: t.color_accent_bright,
                text_hover: t.color_accent_bright,
                border: None,
                weight: 500.,
                radius: t.radius_sm,
                transition: Some(Transition::TAILWIND),
            },
        }
    }
}

/// A text button.
///
/// ```ignore
/// Button::new("save", "Save changes").on_click(|_, window, cx| save(window, cx))
/// Button::new("cancel", "Cancel").variant(ButtonVariant::Secondary).on_click(…)
/// Button::new("leave", "Leave Server").variant(ButtonVariant::Danger).full_width()
/// ```
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    icon: Option<IconName>,
    disabled: bool,
    loading: bool,
    full_width: bool,
    weight: Option<f32>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            variant: ButtonVariant::Primary,
            size: ButtonSize::Md,
            icon: None,
            disabled: false,
            loading: false,
            full_width: false,
            weight: None,
            on_click: None,
        }
    }

    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub fn size(mut self, size: ButtonSize) -> Self {
        self.size = size;
        self
    }

    /// A leading glyph (`gap-1.5`), 14 px.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// A spinner before the label, and the button disabled (the label is the caller's: "Saving…").
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// `w-full`, the label centred.
    pub fn full_width(mut self) -> Self {
        self.full_width = true;
        self
    }

    /// Overrides the variant's font weight (a few dialogs set `font-medium` on a primary).
    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = Some(weight);
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let look = self.variant.look(cx);
        let disabled = self.disabled || self.loading;
        let focus = focus_handle(window, cx, (self.id.clone(), "focus"), !disabled);
        let hover = Hover::new(window, cx, self.id.clone(), look.transition);
        let progress = if disabled { 0. } else { hover.progress };
        let (text_size, line_height) = match self.size {
            ButtonSize::Md => (13., 19.5),
            ButtonSize::Sm => (12., 18.),
        };
        let text = mix(look.text, look.text_hover, progress);
        let ring = ring(cx);

        div()
            .id(self.id)
            .track_focus(&focus)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1p5()
            .map(|this| match self.size {
                ButtonSize::Md => this.px_4().py_2(),
                ButtonSize::Sm => this.px_3().py_1p5(),
            })
            .when(self.full_width, |this| this.w_full())
            .rounded(look.radius)
            .bg(mix(look.bg, look.bg_hover, progress))
            .when_some(look.border, |this, (rest, hovered)| {
                this.border_1().border_color(mix(rest, hovered, progress))
            })
            .focus_visible(move |style| style.shadow(ring))
            .on_hover(hover.listener())
            .map(|this| {
                if disabled {
                    this.opacity(0.5).cursor_not_allowed()
                } else {
                    this.cursor_pointer()
                        .when_some(self.on_click, |this, handler| this.on_click(handler))
                }
            })
            .when(self.loading, |this| this.child(Spinner::new().size(px(14.)).color(text)))
            .when_some(self.icon.filter(|_| !self.loading), |this, icon| {
                this.child(Icon::new(icon).size(px(14.)).color(text))
            })
            .child(
                Label::new(self.label)
                    .size(text_size)
                    .line_height(line_height)
                    .weight(self.weight.unwrap_or(look.weight))
                    .color(text),
            )
    }
}

/// A square glyph button: `flex h-7 w-7 items-center justify-center rounded-sm text-text-muted
/// transition-colors hover:bg-surface-hover hover:text-text-secondary` (the settings close button,
/// 14 px glyph) or `h-6 w-6` with a 12 px glyph (a toast's dismiss).
///
/// ```ignore
/// IconButton::new("close", IconName::Close).on_click(|_, window, cx| close(window, cx))
/// ```
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: IconName,
    box_size: Pixels,
    icon_size: Pixels,
    color: Option<(Hsla, Hsla)>,
    hover_bg: Option<Hsla>,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl IconButton {
    /// 28 px with a 14 px glyph.
    pub fn new(id: impl Into<ElementId>, icon: IconName) -> Self {
        Self {
            id: id.into(),
            icon,
            box_size: px(28.),
            icon_size: px(14.),
            color: None,
            hover_bg: None,
            disabled: false,
            on_click: None,
        }
    }

    /// `h-6 w-6` with a 12 px glyph.
    pub fn small(mut self) -> Self {
        self.box_size = px(24.);
        self.icon_size = px(12.);
        self
    }

    /// The glyph's colour at rest and under the pointer (default `text-muted` → `text-secondary`).
    pub fn color(mut self, rest: Hsla, hovered: Hsla) -> Self {
        self.color = Some((rest, hovered));
        self
    }

    /// The background under the pointer (default `surface-hover`).
    pub fn hover_bg(mut self, color: Hsla) -> Self {
        self.hover_bg = Some(color);
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (rest, hovered) = self
            .color
            .unwrap_or((theme.color_text_muted, theme.color_text_secondary));
        let hover_bg = self.hover_bg.unwrap_or(theme.color_surface_hover);
        let radius = theme.radius_sm;
        let focus = focus_handle(window, cx, (self.id.clone(), "focus"), !self.disabled);
        let hover = Hover::new(window, cx, self.id.clone(), Some(Transition::TAILWIND));
        let progress = if self.disabled { 0. } else { hover.progress };
        let ring = ring(cx);

        div()
            .id(self.id)
            .track_focus(&focus)
            .flex()
            .flex_none()
            .size(self.box_size)
            .items_center()
            .justify_center()
            .rounded(radius)
            .bg(mix(Hsla::transparent_black(), hover_bg, progress))
            .focus_visible(move |style| style.shadow(ring))
            .on_hover(hover.listener())
            .map(|this| {
                if self.disabled {
                    this.opacity(0.5).cursor_not_allowed()
                } else {
                    this.cursor_pointer()
                        .when_some(self.on_click, |this, handler| this.on_click(handler))
                }
            })
            .child(
                Icon::new(self.icon)
                    .size(self.icon_size)
                    .color(mix(rest, hovered, progress)),
            )
    }
}

/// A text link: `text-[12px] font-semibold text-accent-bright underline-offset-2 hover:underline`
/// (a toast's action), or with [`LinkButton::recolor`] the menu idiom `text-[11px] font-medium
/// text-accent-bright transition-colors hover:text-accent`.
///
/// ```ignore
/// LinkButton::new("undo", "Undo").on_click(…)
/// LinkButton::new("reset", "Reset").size(11.).weight(500.).recolor().on_click(…)
/// ```
#[derive(IntoElement)]
pub struct LinkButton {
    id: ElementId,
    label: SharedString,
    size: f32,
    weight: f32,
    recolor: bool,
    on_click: Option<ClickHandler>,
}

impl LinkButton {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            size: 12.,
            weight: 600.,
            recolor: false,
            on_click: None,
        }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Hover turns it `text-accent` instead of underlining it.
    pub fn recolor(mut self) -> Self {
        self.recolor = true;
        self
    }

    pub fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for LinkButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (rest, hovered_color, radius) = {
            let theme = cx.theme();
            (theme.color_accent_bright, theme.color_accent, theme.radius_sm)
        };
        let focus = focus_handle(window, cx, (self.id.clone(), "focus"), true);
        let hover = Hover::new(
            window,
            cx,
            self.id.clone(),
            self.recolor.then_some(Transition::TAILWIND),
        );
        let ring = ring(cx);
        let color = if self.recolor {
            mix(rest, hovered_color, hover.progress)
        } else {
            rest
        };
        let underline = !self.recolor && hover.progress >= 1.;

        div()
            .id(self.id)
            .track_focus(&focus)
            .flex_none()
            .cursor_pointer()
            .rounded(radius)
            .focus_visible(move |style| style.shadow(ring))
            .on_hover(hover.listener())
            .when_some(self.on_click, |this, handler| this.on_click(handler))
            // `hover:underline` (GPUI draws it at the font's underline position; the renderer's
            // `underline-offset-2` puts it 2 px under the baseline).
            .when(underline, |this| this.underline().text_decoration_color(color))
            .child(
                Label::new(self.label)
                    .size(self.size)
                    .line_height(self.size * 1.5)
                    .weight(self.weight)
                    .color(color),
            )
    }
}

/// Lays out buttons the way dialogs do: `mt-5 flex flex-wrap justify-end gap-2`.
pub fn button_row(buttons: impl IntoIterator<Item = AnyElement>) -> impl IntoElement {
    div().flex().flex_wrap().justify_end().gap_2().children(buttons)
}

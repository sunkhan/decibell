//! Small pieces the people views share: the renderer's local button looks (each TSX file draws
//! its own), a person's gradient, the status pill with Invisible, and the lock glyphs.

use decibell_model::Uid;
use decibell_model::live::presence::DisplayStatus;
use decibell_ui::identity::{avatar_color, person_slot};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label};
use gpui::{Focusable as _, 
    App, Background, ElementId, Hsla, IntoElement, Pixels, SharedString, Stateful, div, linear_color_stop,
    linear_gradient, prelude::*, px,
};

/// One button look, as a TSX `className` spells it.
#[derive(Clone, Copy)]
pub struct Look {
    pub bg: Option<Hsla>,
    pub bg_hover: Option<Hsla>,
    pub fg: Hsla,
    pub fg_hover: Hsla,
    pub border: Option<Hsla>,
    pub border_hover: Option<Hsla>,
    pub weight: f32,
    pub size: f32,
    pub radius: Pixels,
    pub px: f32,
    pub py: f32,
}

impl Look {
    /// `rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover`
    /// (the settings idiom: `PrimaryButton`, Retry, Save).
    pub fn primary(cx: &App) -> Look {
        let t = cx.theme();
        Look {
            bg: Some(t.color_accent),
            bg_hover: Some(t.color_accent_hover),
            fg: t.color_on_accent,
            fg_hover: t.color_on_accent,
            border: None,
            border_hover: None,
            weight: 600.,
            size: 13.,
            radius: t.radius_sm,
            px: 16.,
            py: 8.,
        }
    }

    /// `rounded-sm border border-border px-4 py-2 text-[13px] font-medium text-text-secondary
    /// hover:bg-surface-hover hover:text-text-primary` (`SecondaryButton`).
    pub fn secondary(cx: &App) -> Look {
        let t = cx.theme();
        Look {
            bg: None,
            bg_hover: Some(t.color_surface_hover),
            fg: t.color_text_secondary,
            fg_hover: t.color_text_primary,
            border: Some(t.color_border),
            border_hover: Some(t.color_border),
            weight: 500.,
            size: 13.,
            radius: t.radius_sm,
            px: 16.,
            py: 8.,
        }
    }

    /// `rounded-sm px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover
    /// hover:text-text-primary` (the moderation dialog's Cancel).
    pub fn ghost(cx: &App) -> Look {
        let t = cx.theme();
        Look { bg: None, border: None, border_hover: None, weight: 600., ..Look::secondary(cx) }
            .with(|l| l.fg_hover = t.color_text_primary)
    }

    /// `rounded-sm bg-error px-4 py-2 text-[13px] font-semibold text-on-error hover:bg-error/85`.
    pub fn danger(cx: &App) -> Look {
        let t = cx.theme();
        Look {
            bg: Some(t.color_error),
            bg_hover: Some(t.color_error.opacity(0.85)),
            fg: t.color_on_error,
            fg_hover: t.color_on_error,
            ..Look::primary(cx)
        }
    }

    pub fn with(mut self, f: impl FnOnce(&mut Look)) -> Look {
        f(&mut self);
        self
    }
}

/// A text button in `look`; `disabled` greys it (`disabled:opacity-50`) and drops the hover.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, look: Look, disabled: bool) -> Stateful<gpui::Div> {
    let Look { bg, bg_hover, fg, fg_hover, border, border_hover, weight, size, radius, px: x, py: y } = look;
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(radius)
        .px(px(x))
        .py(px(y))
        .when_some(bg, |this, bg| this.bg(bg))
        .when_some(border, |this, border| this.border_1().border_color(border))
        .text_color(fg)
        .map(|this| {
            if disabled {
                this.opacity(0.5)
            } else {
                this.cursor_pointer().hover(move |s| {
                    let s = s.text_color(fg_hover);
                    let s = match bg_hover {
                        Some(bg) => s.bg(bg),
                        None => s,
                    };
                    match border_hover {
                        Some(b) => s.border_color(b),
                        None => s,
                    }
                })
            }
        })
        .child(Label::new(label).size(size).weight(weight).nowrap())
}

/// A person's avatar fill as a background (`userGradient`: `linear-gradient(135deg, av-N,
/// color-mix(in srgb, av-N 72%, #000))`), for banners.
pub fn person_gradient(uid: Uid, name: &str, cx: &App) -> Background {
    let theme = cx.theme();
    let base = avatar_color(theme, person_slot(uid, name));
    let mut dark: gpui::Rgba = base.into();
    dark.r *= 0.72;
    dark.g *= 0.72;
    dark.b *= 0.72;
    linear_gradient(135., linear_color_stop(base, 0.), linear_color_stop(dark, 1.))
}

/// The colour a status dot / pill takes (`StatusDot.tsx`' `COLOR`).
pub fn status_color(status: DisplayStatus, cx: &App) -> Hsla {
    let t = cx.theme();
    match status {
        DisplayStatus::Online => t.color_success,
        DisplayStatus::Away => t.color_warning,
        DisplayStatus::Dnd => t.color_error,
        DisplayStatus::Offline | DisplayStatus::Invisible => t.color_text_muted,
    }
}

/// `StatusPill`: `inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5 text-[11px]
/// font-medium`, tinted by the status; Invisible reads "Invisible" (the kit's pill knows only the
/// four a dot can show).
pub fn status_pill(status: DisplayStatus, font: FontRole, cx: &App) -> impl IntoElement + use<> {
    let color = status_color(status, cx);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .rounded(cx.theme().radius_sm)
        .bg(color.opacity(0.15))
        .px_2()
        .py_0p5()
        .child(div().size_1p5().rounded_full().bg(color))
        .child(Label::new(status.label()).font(font).size(11.).weight(500.).color(color))
}

/// `LockGlyph` (`MessageBubble.tsx`): the closed padlock at `size` px.
pub fn lock_glyph(size: f32, color: Hsla) -> Icon {
    Icon::new(IconName::ChatLock).size(px(size)).color(color)
}

/// The section label of dialogs and menus: `text-[11px] font-semibold uppercase
/// tracking-[0.07em] text-text-muted`.
pub fn caps_label(text: &str, size: f32, cx: &App) -> Label {
    Label::new(SharedString::from(text.to_uppercase())).size(size).weight(600.).tracking(0.07).color(cx.theme().color_text_muted)
}

/// How a text box looks: the renderer's `<input className=…>`s differ per place.
#[derive(Clone, Copy)]
pub struct BoxLook {
    /// Fixed height (`h-9`), else the padding sets it (`py-[9px]`).
    pub height: Option<f32>,
    pub py: f32,
    pub pl: f32,
    pub pr: f32,
    pub bg: Hsla,
    pub radius: Pixels,
    pub font: FontRole,
    pub size: f32,
    /// A 15 px icon at `left-3` (`pl-[34px]` then).
    pub icon: Option<IconName>,
    /// `focus:shadow-ring` besides `focus:border-accent`.
    pub ring: bool,
}

impl BoxLook {
    /// `rounded-md border border-border px-3 font-channel text-member` on `bg`.
    pub fn new(bg: Hsla, cx: &App) -> BoxLook {
        let t = cx.theme();
        BoxLook {
            height: None,
            py: 0.,
            pl: 12.,
            pr: 12.,
            bg,
            radius: t.radius_md,
            font: FontRole::Channel,
            size: t.scaled.text_member,
            icon: None,
            ring: true,
        }
    }
}

/// A one-line field drawn as the renderer's `<input>` at that place: the editor in a bordered
/// box that turns accent (and rings) while focused.
pub fn input_box(
    id: impl Into<ElementId>,
    editor: &gpui::Entity<decibell_text::input::InputState>,
    look: BoxLook,
    window: &mut gpui::Window,
    cx: &mut App,
) -> gpui::AnyElement {
    // `<input>`: line-height `normal` (the face's own, about 1.2).
    let style = decibell_ui::TextField::input_style(look.font, look.size, 1.2109, cx);
    let focus = editor.read(cx).focus_handle(cx);
    let focused = focus.is_focused(window);
    let t = cx.theme();
    let border = if focused { t.color_accent } else { t.color_border };
    div()
        .id(id)
        .track_focus(&focus)
        .relative()
        .flex()
        .items_center()
        .w_full()
        .min_w_0()
        .when_some(look.height, |this, h| this.h(px(h)))
        .py(px(look.py))
        .pl(px(look.pl))
        .pr(px(look.pr))
        .rounded(look.radius)
        .border_1()
        .border_color(border)
        .bg(look.bg)
        .cursor_text()
        .when(focused && look.ring, |this| this.shadow(t.shadow_ring.clone()))
        .when_some(look.icon, |this, icon| {
            this.child(
                div()
                    .absolute()
                    .left_3()
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(Icon::new(icon).size(px(15.)).color(t.color_text_muted)),
            )
        })
        .child(div().flex_1().min_w_0().child(decibell_text::input::TextInput::new(editor, style)))
        .into_any_element()
}

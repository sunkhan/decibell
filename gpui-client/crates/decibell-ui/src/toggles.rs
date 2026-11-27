//! On / off and one-of-n controls: [`Switch`], [`Checkbox`], [`SegmentedControl`].

use std::rc::Rc;

use gpui::{
    App, BoxShadow, ElementId, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px,
};

use crate::interaction::{Hover, focus_handle, ring};
use crate::motion::{self, Transition, mix};
use crate::{ActiveTheme as _, Icon, IconName, Label};

type ToggleHandler = Rc<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

/// The on/off switch (`components/Switch.tsx`): a 40 × 22 track with a 16 px knob laid out by the
/// track (flex + 2 px padding), sliding 18 px; `transition-all` (150 ms): track, border, glow
/// and knob move together. Space / Enter toggle it while focused; disabled is 50 % and inert.
///
/// ```ignore
/// Switch::new("public", self.public).on_toggle(cx.listener(|this, on: &bool, _, cx| {
///     this.public = *on;
///     cx.notify();
/// }))
/// ```
#[derive(IntoElement)]
pub struct Switch {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Switch {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            disabled: false,
            on_toggle: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Called with the new state.
    pub fn on_toggle(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = focus_handle(window, cx, (self.id.clone(), "focus"), !self.disabled);
        let t = motion::transition_flag(
            window,
            cx,
            (self.id.clone(), "on"),
            self.checked,
            Transition::TAILWIND,
        );
        let theme = cx.theme();
        let ring = ring(cx);
        // `shadow-[0_0_8px_color-mix(in_srgb,var(--color-accent)_22%,transparent)]` while on.
        let mut shadows = vec![
            BoxShadow::new(px(0.), px(0.), theme.color_accent.opacity(0.22 * t))
                .blur_radius(px(4.)),
        ];
        let focused_ring = {
            let mut with_ring = shadows.clone();
            with_ring.extend(ring);
            with_ring
        };
        if t == 0. {
            shadows.clear();
        }
        let checked = self.checked;

        div()
            .id(self.id)
            .track_focus(&focus)
            .flex()
            .flex_none()
            .items_center()
            .w(px(40.))
            .h(px(22.))
            .p(px(2.))
            .rounded_full()
            .border_1()
            .border_color(mix(theme.color_border, theme.color_accent, t))
            .bg(mix(theme.color_bg_lighter, theme.color_accent, t))
            .shadow(shadows)
            .focus_visible(move |style| style.shadow(focused_ring))
            .map(|this| {
                if self.disabled {
                    this.opacity(0.5).cursor_not_allowed()
                } else {
                    this.cursor_pointer()
                        .when_some(self.on_toggle, |this, handler| {
                            this.on_click(move |_, window, cx| handler(&!checked, window, cx))
                        })
                }
            })
            .child(
                div()
                    .size_4()
                    .flex_none()
                    .ml(px(18. * t))
                    .rounded_full()
                    .bg(mix(theme.color_text_muted, theme.color_on_accent, t)),
            )
    }
}

/// A checkbox (`features/voice/StreamAudioAppPicker.tsx`): a 16 px box, `rounded-sm border`;
/// checked `border-accent bg-accent` with an 11 px tick in `text-on-accent`, unchecked
/// `border-text-muted/50 bg-bg-lighter`; the focus ring on the box. With a label it is the
/// picker's row: `flex items-center gap-2.5 rounded-sm px-2 py-1.5 hover:bg-surface-hover`, the
/// label 13 px `text-text-primary`, the whole row clickable.
///
/// ```ignore
/// Checkbox::new(("app", id), checked).label(name).on_toggle(…)
/// ```
#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
    label: Option<SharedString>,
    disabled: bool,
    on_toggle: Option<ToggleHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            label: None,
            disabled: false,
            on_toggle: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
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

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = focus_handle(window, cx, (self.id.clone(), "focus"), !self.disabled);
        // `peer-focus-visible:shadow-ring`: the row holds focus, the box shows the ring.
        let focus_visible = focus.is_focused(window) && window.last_input_was_keyboard();
        let theme = cx.theme();
        let checked = self.checked;
        let check_box = div()
            .flex()
            .flex_none()
            .size_4()
            .items_center()
            .justify_center()
            .rounded(theme.radius_sm)
            .border_1()
            .map(|this| {
                if checked {
                    this.border_color(theme.color_accent)
                        .bg(theme.color_accent)
                        .child(
                            Icon::new(IconName::Checkmark)
                                .size(px(11.))
                                .stroke_width(3.5)
                                .color(theme.color_on_accent),
                        )
                } else {
                    this.border_color(theme.color_text_muted.opacity(0.5))
                        .bg(theme.color_bg_lighter)
                }
            })
            .when(focus_visible, |this| this.shadow(theme.shadow_ring.clone()));
        let row_hover = theme.color_surface_hover;
        let radius = theme.radius_sm;

        div()
            .id(self.id)
            .track_focus(&focus)
            .flex()
            .items_center()
            .when(self.label.is_some(), |this| {
                this.gap_2p5()
                    .px_2()
                    .py_1p5()
                    .rounded(radius)
                    .when(!self.disabled, |this| this.hover(move |style| style.bg(row_hover)))
            })
            .map(|this| {
                if self.disabled {
                    this.opacity(0.5).cursor_not_allowed()
                } else {
                    this.cursor_pointer()
                        .when_some(self.on_toggle, |this, handler| {
                            this.on_click(move |_, window, cx| handler(&!checked, window, cx))
                        })
                }
            })
            .child(check_box)
            .when_some(self.label, |this, label| {
                this.child(
                    div().min_w_0().flex_1().child(
                        Label::new(label)
                            .size(13.)
                            .color(cx.theme().color_text_primary)
                            .truncate(),
                    ),
                )
            })
    }
}

/// One choice of a [`SegmentedControl`].
pub struct Segment<T> {
    pub value: T,
    pub label: SharedString,
    pub disabled: bool,
    /// The option's `title` (shown on hover, disabled or not: why it can't be chosen).
    pub title: Option<SharedString>,
}

impl<T> Segment<T> {
    pub fn new(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            disabled: false,
            title: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }
}

type ChangeHandler<T> = Rc<dyn Fn(&T, &mut Window, &mut App) + 'static>;

/// A row of buttons of which exactly one is selected (`components/SegmentedControl.tsx`):
/// `flex rounded-md bg-bg-darkest p-[3px]`, each option `flex-1 rounded-sm px-3 py-[7px]
/// text-[11px] font-semibold transition-all`; the selected one `bg-accent-mid
/// text-accent-bright` with a 6 px accent glow, the others `text-text-muted
/// hover:text-text-secondary`. Each option is a tab stop, chosen with Enter / Space.
///
/// ```ignore
/// SegmentedControl::new("density", vec![Segment::new(Density::Compact, "Compact"), …], self.density)
///     .on_change(cx.listener(|this, value: &Density, _, cx| { this.density = *value; cx.notify() }))
/// ```
#[derive(IntoElement)]
pub struct SegmentedControl<T: Clone + PartialEq + 'static> {
    id: ElementId,
    segments: Vec<Segment<T>>,
    selected: T,
    disabled: bool,
    on_change: Option<ChangeHandler<T>>,
}

impl<T: Clone + PartialEq + 'static> SegmentedControl<T> {
    pub fn new(id: impl Into<ElementId>, segments: Vec<Segment<T>>, selected: T) -> Self {
        Self {
            id: id.into(),
            segments,
            selected,
            disabled: false,
            on_change: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(&T, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl<T: Clone + PartialEq + 'static> RenderOnce for SegmentedControl<T> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut options = Vec::with_capacity(self.segments.len());
        for (index, segment) in self.segments.into_iter().enumerate() {
            let id = ElementId::NamedChild(
                std::sync::Arc::new(self.id.clone()),
                SharedString::from(index.to_string()),
            );
            let disabled = self.disabled || segment.disabled;
            let is_selected = segment.value == self.selected;
            let focus = focus_handle(window, cx, (id.clone(), "focus"), !disabled);
            let t = motion::transition_flag(
                window,
                cx,
                (id.clone(), "selected"),
                is_selected,
                Transition::TAILWIND,
            );
            let hover = Hover::new(window, cx, id.clone(), Some(Transition::TAILWIND));
            let theme = cx.theme();
            let rest = mix(
                theme.color_text_muted,
                theme.color_text_secondary,
                if disabled { 0. } else { hover.progress },
            );
            let text = mix(rest, theme.color_accent_bright, t);
            let mut shadows = vec![
                BoxShadow::new(px(0.), px(0.), theme.color_accent.opacity(0.1 * t))
                    .blur_radius(px(3.)),
            ];
            if t == 0. {
                shadows.clear();
            }
            let ring = ring(cx);
            let mut ring_shadows = shadows.clone();
            ring_shadows.extend(ring);
            let on_change = self.on_change.clone();
            let value = segment.value;
            let title = segment.title;
            options.push(
                div()
                    .id(id)
                    .when_some(title, |this, title| this.tooltip(crate::title_tooltip(title)))
                    .track_focus(&focus)
                    .flex()
                    .flex_1()
                    .justify_center()
                    .px_3()
                    .py(px(7.))
                    .rounded(theme.radius_sm)
                    .bg(theme.color_accent_mid.opacity(t))
                    .shadow(shadows)
                    .focus_visible(move |style| style.shadow(ring_shadows))
                    .on_hover(hover.listener())
                    .map(|this| {
                        if disabled {
                            this.cursor_not_allowed()
                                .when(!self.disabled && !is_selected, |this| this.opacity(0.4))
                        } else {
                            this.cursor_pointer().when_some(on_change, |this, handler| {
                                this.on_click(move |_, window, cx| handler(&value, window, cx))
                            })
                        }
                    })
                    .child(
                        Label::new(segment.label)
                            .size(11.)
                            .line_height(16.5)
                            .weight(600.)
                            .color(text),
                    ),
            );
        }
        let theme = cx.theme();
        div()
            .id(self.id)
            .flex()
            .rounded(theme.radius_md)
            .bg(theme.color_bg_darkest)
            .p(px(3.))
            .when(self.disabled, |this| this.opacity(0.5))
            .children(options)
    }
}

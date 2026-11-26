//! Form fields: the label / help / error frame around a control ([`Field`]) and the text field's
//! shell ([`TextField`]).

use gpui::{
    AnyElement, App, ElementId, FocusHandle, IntoElement, ParentElement, RenderOnce, SharedString,
    Window, div, prelude::*, px,
};

use crate::interaction::Hover;
use crate::motion::{Transition, mix};
use crate::structure::SectionLabel;
use crate::{ActiveTheme as _, Icon, IconName, Label};

/// A setting with its label (channel settings' `Field`): the uppercase [`SectionLabel`] over the
/// control (`mb-2.5 flex h-3 items-center justify-between`, an optional element at its right),
/// then a hint (`mt-2 text-[12px] leading-[1.55] text-text-muted`) or an error in `text-error`.
/// [`Field::plain`] is the dialogs' label instead (`mb-1.5 text-[12px] font-medium
/// text-text-secondary`, InviteModal).
///
/// ```ignore
/// Field::new("Slowmode").hint("Members wait this long between messages.").child(select)
/// ```
#[derive(IntoElement)]
pub struct Field {
    label: SharedString,
    plain: bool,
    aside: Option<AnyElement>,
    hint: Option<SharedString>,
    error: Option<SharedString>,
    children: Vec<AnyElement>,
}

impl Field {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            plain: false,
            aside: None,
            hint: None,
            error: None,
            children: Vec::new(),
        }
    }

    /// The dialog form label.
    pub fn plain(label: impl Into<SharedString>) -> Self {
        Self {
            plain: true,
            ..Self::new(label)
        }
    }

    pub fn aside(mut self, aside: impl IntoElement) -> Self {
        self.aside = Some(aside.into_any_element());
        self
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Shown in place of the hint.
    pub fn error(mut self, error: Option<SharedString>) -> Self {
        self.error = error;
        self
    }
}

impl ParentElement for Field {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

impl RenderOnce for Field {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let header = if self.plain {
            div()
                .mb_1p5()
                .child(Label::new(self.label).size(12.).weight(500.).color(theme.color_text_secondary))
        } else {
            div()
                .mb_2p5()
                .flex()
                .h_3()
                .items_center()
                .justify_between()
                .gap_3()
                .child(SectionLabel::new(self.label))
                .children(self.aside)
        };
        let note = match (self.error, self.hint) {
            (Some(error), _) => Some((error, theme.color_error)),
            (None, Some(hint)) => Some((hint, theme.color_text_muted)),
            (None, None) => None,
        };
        div()
            .flex()
            .flex_col()
            .child(header)
            .children(self.children)
            .when_some(note, |this, (text, color)| {
                this.child(div().mt_2().child(Label::new(text).size(12.).line_height(12. * 1.55).color(color)))
            })
    }
}

/// **Placeholder until `decibell-text` ships its input.** The text field's final shell — the
/// renderer's `FIELD` (channel settings) / SearchField: `h-10 w-full rounded-md border
/// border-border bg-bg-lighter px-3 text-[13px] text-text-primary transition-all
/// placeholder:text-text-muted hover:border-accent/40 focus:border-accent focus:shadow-ring
/// disabled:opacity-60`, an error state (`border-error`), an optional leading icon (SearchField's
/// 15 px magnifier, `pl-9`). It has no editor of its own: it shows `value` (or the placeholder)
/// as static text, or hosts the editor element given to [`TextField::input`] and shows the focus
/// look while `focus` is focused. The API is meant to stay as it is when the editor arrives.
///
/// ```ignore
/// TextField::new("server-name").focus(&self.name_focus).value(self.name.clone())
///     .placeholder("My server").error(self.name_error.is_some())
/// ```
#[derive(IntoElement)]
pub struct TextField {
    id: ElementId,
    value: SharedString,
    placeholder: SharedString,
    focus: Option<FocusHandle>,
    error: bool,
    disabled: bool,
    leading_icon: Option<IconName>,
    input: Option<AnyElement>,
}

impl TextField {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            value: SharedString::default(),
            placeholder: SharedString::default(),
            focus: None,
            error: false,
            disabled: false,
            leading_icon: None,
            input: None,
        }
    }

    pub fn value(mut self, value: impl Into<SharedString>) -> Self {
        self.value = value.into();
        self
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// The editor's focus handle: the shell shows the focus look while it is focused.
    pub fn focus(mut self, focus: &FocusHandle) -> Self {
        self.focus = Some(focus.clone());
        self
    }

    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn leading_icon(mut self, icon: IconName) -> Self {
        self.leading_icon = Some(icon);
        self
    }

    /// The editor element (from `decibell-text`), drawn in place of the static value.
    pub fn input(mut self, input: impl IntoElement) -> Self {
        self.input = Some(input.into_any_element());
        self
    }
}

impl RenderOnce for TextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = Hover::new(window, cx, self.id.clone(), Some(Transition::TAILWIND));
        let focused = self
            .focus
            .as_ref()
            .is_some_and(|focus| focus.is_focused(window));
        let theme = cx.theme();
        let border = if self.error {
            theme.color_error
        } else if focused {
            theme.color_accent
        } else {
            mix(
                theme.color_border,
                theme.color_accent.opacity(0.4),
                if self.disabled { 0. } else { hover.progress },
            )
        };
        let content = match self.input {
            Some(input) => input,
            None if self.value.is_empty() => Label::new(self.placeholder)
                .size(13.)
                .color(theme.color_text_muted)
                .truncate()
                .into_any_element(),
            None => Label::new(self.value)
                .size(13.)
                .color(theme.color_text_primary)
                .truncate()
                .into_any_element(),
        };
        div()
            .id(self.id)
            .when_some(self.focus.as_ref(), |this, focus| this.track_focus(focus))
            .relative()
            .flex()
            .h_10()
            .w_full()
            .min_w_0()
            .items_center()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(border)
            .bg(theme.color_bg_lighter)
            .map(|this| {
                if self.leading_icon.is_some() {
                    this.pl_9().pr_8()
                } else {
                    this.px_3()
                }
            })
            .when(focused && !self.error, |this| this.shadow(theme.shadow_ring.clone()))
            .when(self.disabled, |this| this.opacity(0.6).cursor_not_allowed())
            .when(!self.disabled, |this| this.cursor_text())
            .on_hover(hover.listener())
            .when_some(self.leading_icon, |this, icon| {
                this.child(
                    div()
                        .absolute()
                        .left_3()
                        .top_0()
                        .bottom_0()
                        .flex()
                        .items_center()
                        .child(Icon::new(icon).size(px(15.)).color(theme.color_text_muted)),
                )
            })
            .child(div().flex_1().min_w_0().child(content))
    }
}

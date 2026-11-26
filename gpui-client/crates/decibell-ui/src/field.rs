//! Form fields: the label / help / error frame around a control ([`Field`]) and the text field's
//! shell ([`TextField`]).

use decibell_text::input::{InputState, InputStyle, TextArea, TextInput};
use decibell_text::{Engine, FontSpec, LineHeight, TextStyle};
use gpui::{
    AnyElement, App, ElementId, Entity, FocusHandle, Focusable as _, IntoElement, ParentElement, RenderOnce, SharedString,
    Window, div, prelude::*, px,
};

use crate::interaction::Hover;
use crate::motion::{Transition, mix};
use crate::structure::SectionLabel;
use crate::{ActiveTheme as _, FontRole, Icon, IconName, Label, ScrollAlign, ScrollArea, ScrollHandle};

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

/// The text field: the renderer's `FIELD` (channel settings) / SearchField box: `h-10 w-full
/// rounded-md border border-border bg-bg-lighter px-3 text-[13px] text-text-primary
/// transition-all placeholder:text-text-muted hover:border-accent/40 focus:border-accent
/// focus:shadow-ring disabled:opacity-60`, an error state (`border-error`), an optional leading
/// icon (SearchField's 15 px magnifier, `pl-9`).
///
/// The text lives in an [`InputState`] the view owns (`decibell_text::input`): the view makes
/// it once, reads `value()` and subscribes to its `InputEvent`s (`Change`, `Submit` on Enter,
/// `Escape`), and passes it to [`TextField::editor`] every render. A multi-line state grows the
/// box with its text up to [`TextField::max_height`], then scrolls.
///
/// ```ignore
/// // in the view's constructor
/// let name = cx.new(|cx| InputState::single_line(window, cx).placeholder("My server").max_bytes(64));
/// cx.subscribe(&name, |this, _, event: &InputEvent, cx| if *event == InputEvent::Submit { this.save(cx) }).detach();
/// // in render
/// TextField::new("server-name").editor(&self.name).error(self.name_error.is_some())
/// ```
///
/// Without an editor it shows `value` (or the placeholder) as static text.
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
    editor: Option<Entity<InputState>>,
    max_height: f32,
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
            editor: None,
            max_height: 160.,
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

    /// An element of your own drawn in place of the static value.
    pub fn input(mut self, input: impl IntoElement) -> Self {
        self.input = Some(input.into_any_element());
        self
    }

    /// The field's text and editing state: this makes it a working field. Its placeholder,
    /// limits and Enter policy are the state's; `disabled` here disables it too.
    pub fn editor(mut self, editor: &Entity<InputState>) -> Self {
        self.editor = Some(editor.clone());
        self
    }

    /// How high a multi-line editor's text may grow before it scrolls, px (default 160, the
    /// composer's).
    pub fn max_height(mut self, max_height: f32) -> Self {
        self.max_height = max_height;
        self
    }

    /// The style a field's text is drawn in: the theme's `font` at `size` px with a line
    /// height factor, `text-text-primary`, placeholder `text-text-muted`, Chromium's default
    /// selection colours (the renderer has no `::selection` rule) and a caret in the text
    /// colour (`caret-color: auto`). For editors drawn outside a `TextField` (the composer).
    pub fn input_style(font: FontRole, size: f32, leading: f32, cx: &mut App) -> InputStyle {
        let engine = Engine::global(cx);
        let theme = cx.theme();
        let face = engine.face(&FontSpec {
            family: theme.font(font),
            weight: 400,
            italic: false,
        });
        let mut text = TextStyle::new(face, size, theme.color_text_primary);
        text.line_height = LineHeight::Factor(leading);
        // The body's letter-spacing is inherited as px of the 16 px root size (see `Label`).
        text.letter_spacing = theme.palette.t_ls_body * 16.;
        InputStyle {
            placeholder_color: theme.color_text_muted,
            ..InputStyle::new(text)
        }
    }
}

impl RenderOnce for TextField {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let hover = Hover::new(window, cx, self.id.clone(), Some(Transition::TAILWIND));
        let mut focus = self.focus;
        let mut multiline = false;
        let editor = self.editor.map(|editor| {
            // `text-[13px]`, line-height `normal` in an `<input>` (Inter's 1.21).
            let style = Self::input_style(FontRole::Sans, 13., 1.2109, cx);
            let disabled = self.disabled;
            editor.update(cx, |state, cx| state.set_disabled(disabled, cx));
            let state = editor.read(cx);
            focus = Some(state.focus_handle(cx));
            multiline = state.rules().multiline;
            let hooked = state.has_reveal_hook();
            if !multiline {
                return TextInput::new(&editor, style).into_any_element();
            }
            // A text area grows to `max_height`, then scrolls with the caret kept in view.
            let scroll = window
                .use_keyed_state((self.id.clone(), "scroll"), cx, |_, _| ScrollHandle::new())
                .read(cx)
                .clone();
            if !hooked {
                let scroll = scroll.clone();
                editor.update(cx, |state, _| {
                    state.on_reveal_caret(move |caret, _| {
                        let (top, bottom) = (caret.top(), caret.bottom());
                        let (from, to) = (scroll.offset(), scroll.offset() + scroll.viewport_height());
                        if top < from || bottom > to {
                            scroll.scroll_into_view(top, caret.size.height, ScrollAlign::Nearest);
                        }
                    })
                });
            }
            ScrollArea::new((self.id.clone(), "area"))
                .track_scroll(&scroll)
                .without_keys()
                .w_full()
                .max_h(px(self.max_height))
                .child(TextArea::new(&editor, style))
                .into_any_element()
        });
        let focused = focus.as_ref().is_some_and(|focus| focus.is_focused(window));
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
        let content = match editor.or(self.input) {
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
            .when_some(focus.as_ref(), |this, focus| this.track_focus(focus))
            .relative()
            .flex()
            // A text area: at least the field's 40 px, `py-2.5` around its lines.
            .map(|this| if multiline { this.min_h_10().py_2p5() } else { this.h_10() })
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

//! The text boxes of Settings' forms, and the one seam for the text editor.
//!
//! The renderer draws two looks the kit's `TextField` (the channel-settings `FIELD`: `h-10
//! rounded-md bg-bg-lighter`) doesn't have: the dialogs' inputs (rename, password, passphrase:
//! `rounded-sm border border-border bg-bg-dark px-3 py-2 text-[13px]`) and Network's custom speed
//! (`w-28 rounded-sm border border-border bg-bg-mid px-2.5 py-1.5 text-[12px]`). So the shells are
//! drawn here.
//!
//! **Typing**: `decibell-text`'s editor isn't on this branch yet. Each box holds its text in an
//! [`Input`] (the view's state) and shows it (dots for a password); the self-tests set it through
//! the views' API. When the editor lands, [`Input`] becomes the owner of an `InputState` (made
//! with the window, `password` for masked boxes) and [`InputBox`] draws `TextInput::new(&state,
//! style)` in place of the static text: nothing outside this file changes.

use decibell_ui::{ActiveTheme as _, Label};
use gpui::{App, Context, ElementId, FocusHandle, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px};

/// One field's text and focus.
pub struct Input {
    text: String,
    pub focus: FocusHandle,
    /// `maxLength` (characters), if any.
    max_chars: Option<usize>,
    /// `onChange={(e) => setName(e.target.value.toLowerCase())}`.
    lowercase: bool,
}

impl Input {
    pub fn new<V>(cx: &mut Context<V>) -> Self {
        Input { text: String::new(), focus: cx.focus_handle(), max_chars: None, lowercase: false }
    }

    pub fn max_chars(mut self, n: usize) -> Self {
        self.max_chars = Some(n);
        self
    }

    pub fn lowercase(mut self) -> Self {
        self.lowercase = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// What typing would do (the renderer's `onChange`): lower-cased and cut to the length limit
    /// where the field has them.
    pub fn set(&mut self, text: &str) {
        let mut text = if self.lowercase { text.to_lowercase() } else { text.to_string() };
        if let Some(n) = self.max_chars
            && text.chars().count() > n
        {
            text = text.chars().take(n).collect();
        }
        self.text = text;
    }

    pub fn clear(&mut self) {
        self.text.clear();
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InputLook {
    /// The dialogs' inputs.
    Dialog,
    /// Network's custom speed box.
    Compact,
}

#[derive(IntoElement)]
pub struct InputBox {
    id: ElementId,
    text: SharedString,
    placeholder: SharedString,
    masked: bool,
    look: InputLook,
    focus: FocusHandle,
    disabled: bool,
}

impl InputBox {
    pub fn new(id: impl Into<ElementId>, input: &Input, look: InputLook) -> Self {
        Self {
            id: id.into(),
            text: SharedString::from(input.text.clone()),
            placeholder: SharedString::default(),
            masked: false,
            look,
            focus: input.focus.clone(),
            disabled: false,
        }
    }

    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// `type="password"`.
    pub fn masked(mut self) -> Self {
        self.masked = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl RenderOnce for InputBox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let theme = cx.theme();
        let (size, bg, pad_x, pad_y) = match self.look {
            InputLook::Dialog => (13., theme.color_bg_dark, 12., 8.),
            InputLook::Compact => (12., theme.color_bg_mid, 10., 6.),
        };
        // `line-height: normal` in an <input>: Inter's ascent + descent, 1.21 of the size.
        let line = (size * 1.2109_f32).round();
        let content = if self.text.is_empty() {
            Label::new(self.placeholder).size(size).line_height(line).color(theme.color_text_muted).truncate()
        } else {
            let shown = if self.masked {
                SharedString::from("•".repeat(self.text.chars().count()))
            } else {
                self.text
            };
            Label::new(shown).size(size).line_height(line).color(theme.color_text_primary).truncate()
        };
        div()
            .id(self.id)
            .track_focus(&self.focus)
            .flex()
            .items_center()
            .min_w_0()
            .map(|this| match self.look {
                InputLook::Dialog => this.w_full(),
                InputLook::Compact => this.w(px(112.)),
            })
            .rounded(theme.radius_sm)
            .border_1()
            .border_color(if focused { theme.color_accent } else { theme.color_border })
            .bg(bg)
            .px(px(pad_x))
            .py(px(pad_y))
            .when(!self.disabled, |this| this.cursor_text())
            .child(div().flex_1().min_w_0().h(px(line)).child(content))
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    struct Host(Input);

    #[gpui::test]
    fn typing_follows_the_fields_rules(cx: &mut TestAppContext) {
        let host = cx.new(|cx| Host(Input::new(cx).max_chars(32).lowercase()));
        host.update(cx, |h, _| {
            h.0.set("NewName");
            assert_eq!(h.0.text(), "newname");
            h.0.set(&"x".repeat(40));
            assert_eq!(h.0.text().len(), 32);
        });
    }
}

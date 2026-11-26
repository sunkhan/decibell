//! The three form dialogs Settings opens over itself, which share their chrome
//! (`RenameUsernameModal.tsx`, `ChangePasswordModal.tsx`, `PassphraseModal.tsx`: "Same chrome as
//! ConfirmModal": the dimmed backdrop, the card `max-w-[440px] rounded-xl border bg-bg-secondary
//! p-6 shadow-modal`, a 16 px title, a muted paragraph, the inputs, the inline error, Cancel and
//! the submit button). Esc and a press on the backdrop cancel unless the dialog is busy; Enter in
//! an input submits (the form's `onSubmit`), Enter on a focused button presses that button.
//!
//! The inputs are `decibell_text`'s editor in the kit's dialog box ([`field`]), passwords masked
//! (`InputState::password`). Rename and change-password are drawn inside the Settings window by
//! the renderer, whose `transform` makes it their containing block: they dim that window only
//! ([`DialogParts::in_settings`]); the passphrase dialog dims the whole window.

pub mod passphrase;
pub mod password;
pub mod rename;

use std::rc::Rc;

use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, Button, ButtonVariant, Label, Modal, TextField, TextFieldVariant, dialog_card};
use gpui::{AnyElement, App, Context, ElementId, Entity, IntoElement, SharedString, Subscription, Window, div, prelude::*};

pub(crate) type Handler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

/// One dialog's content.
pub struct DialogParts {
    pub id: &'static str,
    pub open: bool,
    pub title: SharedString,
    pub body: SharedString,
    pub inputs: Vec<AnyElement>,
    pub error: Option<SharedString>,
    /// Under the error (the unlock dialog's "Forgot your passphrase?").
    pub extra: Option<AnyElement>,
    pub busy: bool,
    /// What the submit button says (its busy wording included: "Changing…", "Working…").
    pub submit_label: SharedString,
    pub danger: bool,
    /// Drawn inside the Settings window in the renderer: dims and centres in it.
    pub in_settings: bool,
    pub on_cancel: Handler,
    pub on_submit: Handler,
}

pub fn dialog(parts: DialogParts, window: &Window, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let DialogParts { id, open, title, body, inputs, error, extra, busy, submit_label, danger, in_settings, on_cancel, on_submit } =
        parts;
    let cancel = {
        let on_cancel = on_cancel.clone();
        Button::new((ElementId::from(id), "cancel"), "Cancel")
            .variant(ButtonVariant::Secondary)
            .disabled(busy)
            .on_click(move |_, window, cx| {
                if !busy {
                    on_cancel(window, cx)
                }
            })
    };
    let submit = {
        let on_submit = on_submit.clone();
        Button::new((ElementId::from(id), "submit"), submit_label)
            .variant(if danger { ButtonVariant::Danger } else { ButtonVariant::Primary })
            .disabled(busy)
            .on_click(move |_, window, cx| on_submit(window, cx))
    };
    let card = dialog_card(440., cx)
        .child(Label::new(title).size(16.).weight(600.).color(theme.color_text_primary))
        .child(div().mt_2().child(Label::new(body).size(12.).line_height(12. * 1.55).color(theme.color_text_muted)))
        .child(
            div()
                .mt_4()
                .flex()
                .flex_col()
                .gap_2p5()
                .children(inputs)
                .children(error.map(|e| Label::new(e).size(12.).color(theme.color_error)))
                .children(extra)
                .child(div().mt_3().flex().justify_end().gap_2().child(cancel).child(submit)),
        );
    let mut modal = Modal::new(id, open).on_close(move |_, window, cx| {
        if !busy {
            on_cancel(window, cx)
        }
    });
    if in_settings {
        let (region, radius) = decibell_ui::settings::dialog_region(window, cx);
        modal = modal.within(region, radius);
    }
    modal.child(card).into_any_element()
}

/// JS's `String(err).replace(/^Error:\s*/, "")` over a core refusal: the plain message.
pub fn plain(error: &decibell_model::UiError) -> SharedString {
    error.plain()
}

/// One of the dialogs' inputs (`inputClass`: `rounded-sm border bg-bg-dark px-3 py-2
/// text-[13px] … focus:border-accent`), disabled while the dialog is busy.
pub fn field(id: &'static str, input: &Entity<InputState>, busy: bool) -> AnyElement {
    TextField::new(id).variant(TextFieldVariant::Dialog).editor(input).disabled(busy).into_any_element()
}

/// Enter in `input` submits the form (`onSubmit`): only from the inputs, so a focused button
/// takes Enter for itself.
pub fn submit_on_enter<V: 'static>(
    input: &Entity<InputState>,
    submit: impl Fn(&mut V, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> Subscription {
    cx.subscribe(input, move |view, _, event: &InputEvent, cx| {
        if *event == InputEvent::Submit {
            submit(view, cx);
        }
    })
}

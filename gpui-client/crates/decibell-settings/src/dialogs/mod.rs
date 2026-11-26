//! The three form dialogs Settings opens over itself, which share their chrome
//! (`RenameUsernameModal.tsx`, `ChangePasswordModal.tsx`, `PassphraseModal.tsx`: "Same chrome as
//! ConfirmModal": the dimmed backdrop, the card `max-w-[440px] rounded-xl border bg-bg-secondary
//! p-6 shadow-modal`, a 16 px title, a muted paragraph, the inputs, the inline error, Cancel and
//! the submit button). Esc and a press on the backdrop cancel unless the dialog is busy; Enter
//! submits (the form's `onSubmit`).

pub mod passphrase;
pub mod password;
pub mod rename;

use std::rc::Rc;

use decibell_ui::{ActiveTheme as _, Button, ButtonVariant, Label, Modal, dialog_card};
use gpui::{AnyElement, App, ElementId, IntoElement, SharedString, Window, div, prelude::*};

type Handler = Rc<dyn Fn(&mut Window, &mut App) + 'static>;

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
    pub on_cancel: Handler,
    pub on_submit: Handler,
}

pub fn dialog(parts: DialogParts, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let DialogParts { id, open, title, body, inputs, error, extra, busy, submit_label, danger, on_cancel, on_submit } = parts;
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
        .on_key_down({
            let on_submit = on_submit.clone();
            move |event, window, cx| {
                if event.keystroke.key == "enter" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    on_submit(window, cx);
                }
            }
        })
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
    Modal::new(id, open)
        .on_close(move |_, window, cx| {
            if !busy {
                on_cancel(window, cx)
            }
        })
        .child(card)
        .into_any_element()
}

/// JS's `String(err).replace(/^Error:\s*/, "")` over a core refusal: the plain message.
pub fn plain(error: &decibell_model::UiError) -> SharedString {
    error.plain()
}

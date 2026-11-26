//! Change username (`RenameUsernameModal.tsx`; uid design, stage 5): central confirms the password,
//! applies the registration rules and reserves the old name for this account for 14 days; the
//! core saves the new name for the automatic sign-in and hands the fresh token to every
//! community, and `user_renamed` re-labels everything that shows us. Drawn by the Account tab,
//! over the Settings window.

use decibell_model::{Model, actions};
use decibell_ui::ActiveTheme as _;
use gpui::{AnyElement, App, Context, SharedString, WeakEntity, Window};

use super::{DialogParts, dialog};
use crate::fields::{Input, InputBox, InputLook};

/// `NAME_RULE = /^[a-z0-9._-]{3,32}$/`.
pub fn name_ok(name: &str) -> bool {
    (3..=32).contains(&name.chars().count())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// `submit`'s checks, before anything is sent.
pub fn validate(name: &str, current: Option<&str>, password: &str) -> Result<(), &'static str> {
    if !name_ok(name) {
        return Err("Use 3–32 lowercase letters, digits, '.', '_' or '-'.");
    }
    if Some(name) == current {
        return Err("That's already your username.");
    }
    if password.is_empty() {
        return Err("Enter your password to confirm.");
    }
    Ok(())
}

pub struct RenameForm {
    pub open: bool,
    pub name: Input,
    pub password: Input,
    pub error: Option<SharedString>,
    pub busy: bool,
}

impl RenameForm {
    pub fn new<V>(cx: &mut Context<V>) -> Self {
        RenameForm {
            open: false,
            name: Input::new(cx).max_chars(32).lowercase(),
            password: Input::new(cx),
            error: None,
            busy: false,
        }
    }

    /// Opening clears the form (the renderer resets it on every open) and focuses the name.
    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.open = true;
        self.name.clear();
        self.password.clear();
        self.error = None;
        self.busy = false;
        self.name.focus.focus(window, cx);
    }

    pub fn cancel(&mut self) {
        if self.open && !self.busy {
            self.open = false;
        }
    }
}

/// The submit: checked here, then central. `form` finds the form in its view again.
pub fn submit<V: 'static>(
    view: &mut V,
    form: fn(&mut V) -> &mut RenameForm,
    cx: &mut Context<V>,
) {
    let current = Model::global(cx).auth.read(cx).username.clone();
    let f = form(view);
    if !f.open || f.busy {
        return;
    }
    let next = f.name.text().trim().to_string();
    if let Err(error) = validate(&next, current.as_deref(), f.password.text()) {
        f.error = Some(SharedString::new_static(error));
        cx.notify();
        return;
    }
    f.busy = true;
    f.error = None;
    let password = f.password.text().to_string();
    cx.notify();
    let this: WeakEntity<V> = cx.weak_entity();
    actions::auth::rename_account(next, password, cx, move |result, cx| {
        let Some(this) = this.upgrade() else { return };
        this.update(cx, |view, cx| {
            let f = form(view);
            match result {
                Ok(name) => {
                    let toasts = Model::global(cx).toasts.clone();
                    toasts.update(cx, |t, cx| t.success("Username changed", Some(format!("You're now {name}.").into()), cx));
                    f.busy = false;
                    f.open = false;
                }
                Err(e) => {
                    f.error = Some(e.plain());
                    f.busy = false;
                }
            }
            cx.notify();
        });
    });
}

/// The dialog.
pub fn render<V: 'static>(
    form: &RenameForm,
    current: Option<SharedString>,
    get: fn(&mut V) -> &mut RenameForm,
    cx: &mut Context<V>,
) -> AnyElement {
    let _ = cx.theme();
    let placeholder: SharedString = match &current {
        Some(name) => format!("New username (now {name})").into(),
        None => "New username".into(),
    };
    let inputs = vec![
        InputBox::new("rename-name", &form.name, InputLook::Dialog).placeholder(placeholder).disabled(form.busy).into_any(),
        InputBox::new("rename-password", &form.password, InputLook::Dialog)
            .placeholder("Current password")
            .masked()
            .disabled(form.busy)
            .into_any(),
    ];
    let weak = cx.weak_entity();
    let cancel = {
        let weak = weak.clone();
        move |_: &mut Window, cx: &mut App| {
            if let Some(v) = weak.upgrade() {
                v.update(cx, |v, cx| {
                    get(v).cancel();
                    cx.notify();
                });
            }
        }
    };
    let submit_handler = move |_: &mut Window, cx: &mut App| {
        if let Some(v) = weak.upgrade() {
            v.update(cx, |v, cx| submit(v, get, cx));
        }
    };
    dialog(
        DialogParts {
            id: "rename-dialog",
            open: form.open,
            title: "Change username".into(),
            body: "Friends, messages, servers and encryption stay with your account. Your old name stays reserved for you for 14 days, then anyone can take it. You can change your username twice a day.".into(),
            inputs,
            error: form.error.clone(),
            extra: None,
            busy: form.busy,
            submit_label: if form.busy { "Changing…".into() } else { "Change username".into() },
            danger: false,
            on_cancel: std::rc::Rc::new(cancel),
            on_submit: std::rc::Rc::new(submit_handler),
        },
        cx,
    )
}

trait IntoAny {
    fn into_any(self) -> AnyElement;
}

impl IntoAny for InputBox {
    fn into_any(self) -> AnyElement {
        gpui::IntoElement::into_any_element(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_renderers_checks() {
        assert_eq!(validate("ab", None, "x"), Err("Use 3–32 lowercase letters, digits, '.', '_' or '-'."));
        assert_eq!(validate("Bad Name", None, "x"), Err("Use 3–32 lowercase letters, digits, '.', '_' or '-'."));
        assert_eq!(validate("perf1", Some("perf1"), "x"), Err("That's already your username."));
        assert_eq!(validate("perf9", Some("perf1"), ""), Err("Enter your password to confirm."));
        assert_eq!(validate("new.name_1-x", Some("perf1"), "pw"), Ok(()));
    }
}

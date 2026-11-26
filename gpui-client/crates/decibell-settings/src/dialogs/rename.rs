//! Change username (`RenameUsernameModal.tsx`; uid design, stage 5): central confirms the password,
//! applies the registration rules and reserves the old name for this account for 14 days; the
//! core saves the new name for the automatic sign-in and hands the fresh token to every
//! community, and `user_renamed` re-labels everything that shows us. Drawn by the Account tab,
//! over the Settings window.

use decibell_model::{Model, actions};
use decibell_text::input::{InputEvent, InputState};
use gpui::{AnyElement, App, AppContext as _, Context, Entity, SharedString, Subscription, WeakEntity, Window};

use super::{DialogParts, dialog, field, submit_on_enter};

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
    name: Entity<InputState>,
    password: Entity<InputState>,
    pub error: Option<SharedString>,
    pub busy: bool,
    _subscriptions: [Subscription; 3],
}

/// The name field's placeholder.
fn placeholder(current: Option<&str>) -> SharedString {
    match current {
        Some(name) => format!("New username (now {name})").into(),
        None => "New username".into(),
    }
}

impl RenameForm {
    /// The form of view `V`, which finds it again with `get` (Enter in either field submits).
    pub fn new<V: 'static>(get: fn(&mut V) -> &mut RenameForm, window: &mut Window, cx: &mut Context<V>) -> Self {
        let current = Model::global(cx).auth.read(cx).username.clone();
        // `maxLength={32}`.
        let name = cx.new(|cx| InputState::single_line(window, cx).max_length(32).placeholder(placeholder(current.as_deref())));
        let password = cx.new(|cx| InputState::password(window, cx).placeholder("Current password"));
        let subscriptions = [
            // `onChange={(e) => setName(e.target.value.toLowerCase())}`.
            cx.subscribe(&name, |_, name, event: &InputEvent, cx| {
                if *event == InputEvent::Change {
                    let value = name.read(cx).value();
                    let lower = value.to_lowercase();
                    if lower != value {
                        name.update(cx, |name, cx| name.set_value(&lower, cx));
                    }
                }
            }),
            submit_on_enter(&name, move |view: &mut V, cx| submit(view, get, cx), cx),
            submit_on_enter(&password, move |view: &mut V, cx| submit(view, get, cx), cx),
        ];
        RenameForm { open: false, name, password, error: None, busy: false, _subscriptions: subscriptions }
    }

    /// Opening clears the form (the renderer resets it on every open) and focuses the name.
    pub fn open(&mut self, window: &mut Window, cx: &mut App) {
        self.open = true;
        let current = Model::global(cx).auth.read(cx).username.clone();
        self.name.update(cx, |name, cx| {
            name.clear(cx);
            name.set_placeholder(placeholder(current.as_deref()), cx);
        });
        self.password.update(cx, |password, cx| password.clear(cx));
        self.error = None;
        self.busy = false;
        self.name.update(cx, |name, cx| name.focus(window, cx));
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
    let next = f.name.read(cx).value().trim().to_string();
    let password = f.password.read(cx).value().to_string();
    if let Err(error) = validate(&next, current.as_deref(), &password) {
        f.error = Some(SharedString::new_static(error));
        cx.notify();
        return;
    }
    f.busy = true;
    f.error = None;
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
pub fn render<V: 'static>(form: &RenameForm, get: fn(&mut V) -> &mut RenameForm, window: &mut Window, cx: &mut Context<V>) -> AnyElement {
    let weak = cx.weak_entity();
    let submit: super::Handler = {
        let weak = weak.clone();
        std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
            if let Some(v) = weak.upgrade() {
                v.update(cx, |v, cx| submit(v, get, cx));
            }
        })
    };
    let inputs = vec![field("rename-name", &form.name, form.busy), field("rename-password", &form.password, form.busy)];
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
            in_settings: true,
            on_cancel: std::rc::Rc::new(cancel),
            on_submit: submit,
        },
        window,
        cx,
    )
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

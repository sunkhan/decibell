//! The sign-in and sign-up pages (`features/auth/LoginPage.tsx`).
//!
//! The text fields are the kit's `TextField` shell: they show their value and take focus, and
//! the text agent's editor plugs into `TextField::input` without changing this page
//! ([`AuthView::set_field`] is where an editor reports what was typed). Until then a sign-in
//! happens without typing: the saved sign-in at start-up, or `--login` in development builds.

use decibell_model::stores::auth::AuthStore;
use decibell_model::{Model, actions};
use decibell_ui::{ActiveTheme as _, BellMark, FontRole, Label, Spinner, TextField};
use gpui::{
    App, ClickEvent, Context, FocusHandle, IntoElement, KeyDownEvent, Render, SharedString, Window, div, prelude::*,
    px,
};

use decibell_common::Watch;

/// Which form the page shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Login,
    Register,
}

/// A field of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthField {
    Username,
    Email,
    Password,
}

pub struct AuthView {
    mode: AuthMode,
    username: SharedString,
    email: SharedString,
    password: SharedString,
    username_focus: FocusHandle,
    email_focus: FocusHandle,
    password_focus: FocusHandle,
    _watch: Watch,
}

/// The version line in the page's corner. The renderer prints this literal ("Decibell 0.6.8"),
/// not the app's version: mirrored as is.
const VERSION_LINE: &str = "Decibell 0.6.8";

impl AuthView {
    pub fn new(mode: AuthMode, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.auth, cx);
        Self {
            mode,
            username: SharedString::default(),
            email: SharedString::default(),
            password: SharedString::default(),
            username_focus: cx.focus_handle(),
            email_focus: cx.focus_handle(),
            password_focus: cx.focus_handle(),
            _watch: watch,
        }
    }

    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: AuthMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            cx.notify();
        }
    }

    /// What an editor reports as typed. Usernames register lowercase-only (central enforces
    /// it): the sign-up form folds as the user types; sign-in keeps it verbatim (accounts from
    /// before the rule may be mixed-case).
    pub fn set_field(&mut self, field: AuthField, value: &str, cx: &mut Context<Self>) {
        let value: SharedString = match (field, self.mode) {
            (AuthField::Username, AuthMode::Register) => value.to_lowercase().into(),
            _ => value.to_string().into(),
        };
        let slot = match field {
            AuthField::Username => &mut self.username,
            AuthField::Email => &mut self.email,
            AuthField::Password => &mut self.password,
        };
        if *slot != value {
            *slot = value;
            cx.notify();
        }
    }

    /// The form's submit (`handleSubmit`).
    pub fn submit(&mut self, cx: &mut Context<Self>) {
        let busy = {
            let auth = Model::global(cx).auth.read(cx);
            match self.mode {
                AuthMode::Login => auth.is_logging_in,
                AuthMode::Register => auth.is_registering,
            }
        };
        // `required` fields: the browser refuses an empty form before submitting.
        if busy || self.username.is_empty() || self.password.is_empty() {
            return;
        }
        match self.mode {
            AuthMode::Login => actions::auth::sign_in(self.username.to_string(), self.password.to_string(), cx),
            AuthMode::Register => {
                if self.email.is_empty() {
                    return;
                }
                actions::auth::register(self.username.to_string(), self.email.to_string(), self.password.to_string(), cx)
            }
        }
    }

    fn label(text: &'static str, cx: &App) -> impl IntoElement {
        // `mb-1.5 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted`
        div().mb_1p5().child(
            Label::new(SharedString::new_static(text))
                .size(11.)
                .weight(600.)
                .tracking(0.07)
                .color(cx.theme().color_text_muted),
        )
    }

    fn field(&self, field: AuthField, cx: &App) -> impl IntoElement + use<> {
        let (id, value, focus) = match field {
            AuthField::Username => ("auth-username", self.username.clone(), &self.username_focus),
            AuthField::Email => ("auth-email", self.email.clone(), &self.email_focus),
            // A password field shows dots, never the text.
            AuthField::Password => (
                "auth-password",
                SharedString::from("•".repeat(self.password.chars().count())),
                &self.password_focus,
            ),
        };
        let _ = cx;
        TextField::new(id).focus(focus).value(value)
    }
}

impl Render for AuthView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let auth: &AuthStore = model.auth.read(cx);
        let theme = cx.theme();
        let login = self.mode == AuthMode::Login;
        let loading = if login { auth.is_logging_in } else { auth.is_registering };
        let error = auth.login_error.clone();
        let notice = auth.login_notice.clone().filter(|_| error.is_none() && login);
        let register_result = auth.register_result.clone();

        let message = |text: SharedString, color| {
            div().mt_3().flex().justify_center().child(
                Label::new(text).size(14.).line_height(20.).color(color),
            )
        };

        let submit = div()
            .id("auth-submit")
            .flex()
            .w_full()
            .h(px(40.))
            .items_center()
            .justify_center()
            .rounded(theme.radius_md)
            .bg(theme.color_accent)
            .when(!loading, |this| {
                this.cursor_pointer()
                    .hover(|s| s.bg(theme.color_accent_hover))
                    .active(|s| s.bg(theme.color_accent_pressed))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.submit(cx)))
            })
            .when(loading, |this| this.opacity(0.5))
            .child(if loading {
                Spinner::ring().size(px(20.)).color(theme.color_on_accent).into_any_element()
            } else {
                Label::new(if login { "Log In" } else { "Register" })
                    .size(14.)
                    .line_height(20.)
                    .weight(600.)
                    .color(theme.color_on_accent)
                    .into_any_element()
            });

        let switch = div()
            .mt_4()
            .flex()
            .justify_center()
            .gap(px(4.))
            .child(
                Label::new(if login { "Need an account?" } else { "Already have an account?" })
                    .size(14.)
                    .line_height(20.)
                    .color(theme.color_text_secondary),
            )
            .child(
                div()
                    .id("auth-switch")
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_mode(if login { AuthMode::Register } else { AuthMode::Login }, cx)
                    }))
                    .child(
                        Label::new(if login { "Register" } else { "Log In" })
                            .size(14.)
                            .line_height(20.)
                            .weight(600.)
                            .color(theme.color_accent),
                    ),
            );

        let form = div()
            .id("auth-form")
            .w_full()
            .max_w(px(400.))
            .rounded(theme.radius_xl)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_secondary)
            .p_8()
            .shadow(theme.shadow_modal.clone())
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter" {
                    this.submit(cx);
                }
            }))
            .child(
                div()
                    .mb_5()
                    .flex()
                    .h_12()
                    .items_end()
                    .justify_center()
                    .child(BellMark::new("auth-mark").bars(13).height(40.).rise(true)),
            )
            .child(
                div().mb_1().flex().justify_center().child(
                    Label::new(if login { "Decibell" } else { "Create an account" })
                        .font(FontRole::Display)
                        .size(22.)
                        .weight(600.)
                        .tracking(-0.02)
                        .color(theme.color_text_bright),
                ),
            )
            .child(
                div().mb_6().flex().justify_center().child(
                    Label::new(if login { "Decentralized game chat" } else { "Join the conversation" })
                        .size(14.)
                        .line_height(20.)
                        .color(theme.color_text_secondary),
                ),
            )
            .child(Self::label("USERNAME", cx))
            .child(div().mb_3().child(self.field(AuthField::Username, cx)))
            .when(!login, |this| {
                this.child(
                    div().mt(px(-6.)).mb_3().child(
                        Label::new("Lowercase letters, digits, and . _ - only.")
                            .size(11.)
                            .color(theme.color_text_muted),
                    ),
                )
                .child(Self::label("EMAIL", cx))
                .child(div().mb_3().child(self.field(AuthField::Email, cx)))
            })
            .child(Self::label("PASSWORD", cx))
            .child(div().mb_5().child(self.field(AuthField::Password, cx)))
            .child(submit)
            .when_some(error, |this, error| this.child(message(error, theme.color_error)))
            .when_some(notice, |this, notice| this.child(message(notice, theme.color_warning)))
            .when_some(register_result, |this, result| {
                let color = if result.success { theme.color_success } else { theme.color_error };
                this.child(message(result.message, color))
            })
            .child(switch);

        div()
            .relative()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.color_bg_primary)
            .child(form)
            .child(
                div()
                    .absolute()
                    .bottom_3()
                    .right_4()
                    .child(Label::new(VERSION_LINE).size(11.).color(theme.color_text_muted)),
            )
    }
}

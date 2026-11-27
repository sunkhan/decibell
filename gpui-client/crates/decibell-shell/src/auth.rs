//! The sign-in and sign-up pages (`features/auth/LoginPage.tsx`): one form in two modes.
//!
//! - The fields are `decibell_text` editors (`InputState`) in the page's own input box
//!   (`rounded-md border bg-bg-primary px-3 py-2.5 text-sm … transition-colors
//!   focus:border-accent`); the password is masked (bullets, never copied). Nothing is focused
//!   when the page opens (the renderer's inputs have no `autoFocus`).
//! - The sign-up form lower-cases the username as it is typed (`onChange`: central registers
//!   lower-case names only); sign-in keeps it verbatim (accounts from before the rule may be
//!   mixed-case). Switching modes keeps what was typed and the result lines (they are the store's).
//! - Enter in any field submits (the form's implicit submission), Enter or Space on a focused
//!   button presses it; Tab runs username → (email) → password → the submit button → the mode
//!   link. While a request is out the submit button is disabled (spinner, 50 %), and so is the
//!   implicit submission; the fields stay editable.
//! - Before anything is sent the browser's form validation runs (`required`, `type="email"`):
//!   the first field that fails takes the focus and Chromium's bubble under it says why
//!   ([`crate::validation`]); it goes when that field loses the focus, is fixed, or after its
//!   time (at least 5 s).
//! - Under the button: the sign-in error (`text-error`), else in the sign-in mode the notice of
//!   why the page shows (`text-warning`: signed out elsewhere, password changed), then the
//!   registration's result (green or red), whichever mode is showing.

use decibell_harness::probes;
use decibell_model::stores::auth::AuthStore;
use decibell_model::{Model, actions};
use decibell_text::input::{InputEvent, InputState, TextInput};
use decibell_text::{Engine, FontSpec, LineHeight, TextStyle, Wrapping};
use decibell_ui::motion::{Transition, mix};
use decibell_ui::{ActiveTheme as _, BellMark, FontRole, Hover, Label, Spinner, TextField};
use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, Context, ElementId, Entity, FocusHandle, Focusable as _, Hsla, IntoElement, PathBuilder,
    Render, SharedString, Subscription, Task, Window, WindowAppearance, canvas, div, point, prelude::*, px, rgb, rgba,
};

use decibell_common::Watch;

use crate::validation::{self, Kind};

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

impl AuthField {
    fn key(self) -> &'static str {
        match self {
            AuthField::Username => "auth-username",
            AuthField::Email => "auth-email",
            AuthField::Password => "auth-password",
        }
    }

    fn kind(self) -> Kind {
        match self {
            AuthField::Email => Kind::Email,
            AuthField::Username | AuthField::Password => Kind::Text,
        }
    }
}

/// Chromium's validation bubble under a refused field.
struct Bubble {
    field: AuthField,
    message: SharedString,
    /// Which showing this is: each new bubble fades in again.
    seq: u32,
}

pub struct AuthView {
    mode: AuthMode,
    username: Entity<InputState>,
    email: Entity<InputState>,
    password: Entity<InputState>,
    submit_focus: FocusHandle,
    switch_focus: FocusHandle,
    bubble: Option<Bubble>,
    bubble_seq: u32,
    /// Hides the bubble when its time is up (exists only while one shows).
    bubble_timer: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _watch: Watch,
}

/// The version line in the page's corner. The renderer prints this literal ("Decibell 0.6.8"),
/// not the app's version: mirrored as is.
const VERSION_LINE: &str = "Decibell 0.6.8";

/// The form's content width: `max-w-[400px]` less `p-8` and the border (the window is never
/// narrower than 800 px, so the form is always 400 px wide).
const CONTENT_WIDTH: f32 = 400. - 64. - 2.;

/// The input box's height: `py-2.5`, `text-sm`'s 20 px line, the border.
const FIELD_HEIGHT: f32 = 42.;

impl AuthView {
    pub fn new(mode: AuthMode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.auth, cx);
        let username = cx.new(|cx| InputState::single_line(window, cx));
        let email = cx.new(|cx| InputState::single_line(window, cx));
        let password = cx.new(|cx| InputState::password(window, cx));
        let subscriptions = [(AuthField::Username, &username), (AuthField::Email, &email), (AuthField::Password, &password)]
            .into_iter()
            .map(|(field, input)| {
                cx.subscribe_in(input, window, move |this, input, event: &InputEvent, window, cx| {
                    this.on_input(field, input, event, window, cx)
                })
            })
            .collect();
        Self {
            mode,
            username,
            email,
            password,
            submit_focus: cx.focus_handle().tab_stop(true),
            switch_focus: cx.focus_handle().tab_stop(true),
            bubble: None,
            bubble_seq: 0,
            bubble_timer: None,
            _subscriptions: subscriptions,
            _watch: watch,
        }
    }

    pub fn mode(&self) -> AuthMode {
        self.mode
    }

    /// The mode link (`setMode`): what was typed stays; a bubble goes (the press that switches
    /// takes the focus from its field).
    pub fn set_mode(&mut self, mode: AuthMode, cx: &mut Context<Self>) {
        if self.mode != mode {
            self.mode = mode;
            self.hide_bubble(cx);
            cx.notify();
        }
    }

    fn input(&self, field: AuthField) -> &Entity<InputState> {
        match field {
            AuthField::Username => &self.username,
            AuthField::Email => &self.email,
            AuthField::Password => &self.password,
        }
    }

    /// What a field holds, as typed.
    pub fn value(&self, field: AuthField, cx: &App) -> String {
        self.input(field).read(cx).value().to_string()
    }

    /// Put `value` in a field as if it were typed there (fixtures, tests).
    pub fn set_field(&mut self, field: AuthField, value: &str, cx: &mut Context<Self>) {
        self.input(field).update(cx, |input, cx| input.set_value(value, cx));
    }

    /// The validation bubble showing now: its field and what it says.
    pub fn bubble(&self) -> Option<(AuthField, SharedString)> {
        self.bubble.as_ref().map(|b| (b.field, b.message.clone()))
    }

    /// The fields in the form's order.
    fn fields(&self) -> &'static [AuthField] {
        match self.mode {
            AuthMode::Login => &[AuthField::Username, AuthField::Password],
            AuthMode::Register => &[AuthField::Username, AuthField::Email, AuthField::Password],
        }
    }

    fn on_input(&mut self, field: AuthField, input: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::Change => {
                // `onChange={(e) => setUsername(mode === "register" ? value.toLowerCase() : value)}`.
                if field == AuthField::Username && self.mode == AuthMode::Register {
                    let value = input.read(cx).value();
                    let lower = value.to_lowercase();
                    if lower != value {
                        input.update(cx, |input, cx| input.set_value(&lower, cx));
                        return;
                    }
                }
                // A shown bubble follows its field's validity (`UpdateVisibleValidationMessage`).
                if self.bubble.as_ref().is_some_and(|b| b.field == field) {
                    match validation::message(field.kind(), input.read(cx).value()) {
                        None => self.hide_bubble(cx),
                        Some(message) => self.show_bubble(field, message, false, cx),
                    }
                }
            }
            InputEvent::Submit => self.submit(window, cx),
            // Blink hides the bubble when its field loses the focus.
            InputEvent::Blur if self.bubble.as_ref().is_some_and(|b| b.field == field) => self.hide_bubble(cx),
            _ => {}
        }
    }

    /// The form's submit (`handleSubmit`), after the browser's checks: nothing while the request
    /// is out (the submit button is disabled, so implicit submission does nothing either).
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let busy = {
            let auth = Model::global(cx).auth.read(cx);
            match self.mode {
                AuthMode::Login => auth.is_logging_in,
                AuthMode::Register => auth.is_registering,
            }
        };
        if busy {
            return;
        }
        for &field in self.fields() {
            if let Some(message) = validation::message(field.kind(), self.input(field).read(cx).value()) {
                // The first invalid field takes the focus (its caret where it was) and the bubble.
                let focus = self.input(field).read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                self.show_bubble(field, message, true, cx);
                return;
            }
        }
        self.hide_bubble(cx);
        let username = self.value(AuthField::Username, cx);
        let password = self.value(AuthField::Password, cx);
        match self.mode {
            AuthMode::Login => actions::auth::sign_in(username, password, cx),
            AuthMode::Register => {
                let email = validation::email_value(self.input(AuthField::Email).read(cx).value());
                actions::auth::register(username, email, password, cx)
            }
        }
    }

    fn show_bubble(&mut self, field: AuthField, message: String, fresh: bool, cx: &mut Context<Self>) {
        if !fresh && self.bubble.as_ref().is_some_and(|b| b.field == field && *b.message == *message) {
            return;
        }
        let seq = match &self.bubble {
            Some(b) if !fresh && b.field == field => b.seq,
            _ => {
                self.bubble_seq = self.bubble_seq.wrapping_add(1);
                self.bubble_seq
            }
        };
        let shown_for = validation::shown_for(&message);
        self.bubble = Some(Bubble { field, message: message.into(), seq });
        self.bubble_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(shown_for).await;
            let _ = this.update(cx, |this, cx| this.hide_bubble(cx));
        }));
        cx.notify();
    }

    fn hide_bubble(&mut self, cx: &mut Context<Self>) {
        self.bubble_timer = None;
        if self.bubble.take().is_some() {
            cx.notify();
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

    /// One input: `w-full rounded-md border border-border bg-bg-primary px-3 py-2.5 text-sm
    /// text-text-primary outline-none transition-colors focus:border-accent`, with its bubble.
    fn field(&self, field: AuthField, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let input = self.input(field).clone();
        let focus = input.read(cx).focus_handle(cx);
        let focused = focus.is_focused(window);
        let accent = decibell_ui::motion::transition_flag(window, cx, (ElementId::from(field.key()), "border"), focused, Transition::TAILWIND);
        let mut style = TextField::input_style(FontRole::Sans, 14., 20. / 14., cx);
        style.text.line_height = LineHeight::Px(20.);
        if probes::enabled() {
            // What the field holds (a password: how many characters).
            let value = input.read(cx).value();
            let shown = if field == AuthField::Password { value.chars().count().to_string() } else { value.to_string() };
            probes::text(field.key(), &SharedString::from(shown));
        }
        let bubble = self.bubble.as_ref().filter(|b| b.field == field).map(|b| bubble(b, window, cx));
        let theme = cx.theme();
        let border = mix(theme.color_border, theme.color_accent, accent);
        div()
            .relative()
            .w_full()
            .child(
                div()
                    .id(field.key())
                    .track_focus(&focus)
                    .relative()
                    .flex()
                    .w_full()
                    .h(px(FIELD_HEIGHT))
                    .items_center()
                    .px_3()
                    .rounded(theme.radius_md)
                    .border_1()
                    .border_color(border)
                    .bg(theme.color_bg_primary)
                    .cursor_text()
                    .child(div().flex_1().min_w_0().child(TextInput::new(&input, style)))
                    .children(probes::probe(field.key())),
            )
            .children(bubble)
            .into_any_element()
    }
}

/// A line of the result / error / notice paragraphs: `mt-3 text-center text-sm`, every line
/// centred.
fn centred(text: &SharedString, color: Hsla, window: &Window, cx: &mut App) -> AnyElement {
    let engine = Engine::global(cx);
    let theme = cx.theme();
    let face = engine.face(&FontSpec { family: theme.font(FontRole::Sans), weight: 400, italic: false });
    let style = TextStyle::new(face, 14., color).line_height_px(20.).letter_spacing(theme.palette.t_ls_body * 16.);
    let layout = engine.label_layout(text, &style, Wrapping::NORMAL, CONTENT_WIDTH, window.scale_factor());
    div()
        .mt_3()
        .flex()
        .flex_col()
        .items_center()
        .children((0..layout.line_count()).filter_map(|i| layout.line(i)).map(|line| {
            let piece = text[line.text.0 as usize..line.text.1 as usize].trim_end().to_string();
            Label::new(piece).size(14.).line_height(20.).color(color).nowrap()
        }))
        .into_any_element()
}

/// Chromium's validation bubble (`validation_bubble.css`, `ValidationMessageOverlayDelegate`):
/// under the field, centred on it, an 8 px arrow pointing 8 px in from the field's left edge, at
/// most half the window wide; a white (dark when the system prefers dark) box with a grey border,
/// Chromium's orange warning icon and the message at 14 px; fading in over 467 ms.
fn bubble(b: &Bubble, window: &mut Window, cx: &mut App) -> AnyElement {
    const ARROW: f32 = 8.;
    const ARROW_MARGIN: f32 = 10.;
    const ICON: f32 = 24.;
    // Border, padding, the icon and its `margin-inline-end`.
    const CHROME: f32 = 2. + 16. + ICON + 8.;
    let dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    // Chromium's own colours for the bubble (`--bubble-background`, `--bubble-border-color`, the
    // text's `CanvasText`), not the theme's: the bubble is the browser's, not the page's.
    let (background, border, text): (Hsla, Hsla, Hsla) =
        if dark { (rgb(0x3b3b3b).into(), rgb(0x858585).into(), rgb(0xffffff).into()) } else { (rgb(0xffffff).into(), rgb(0x808080).into(), rgb(0x000000).into()) };
    // The fields fill the form's content width.
    let field_width = CONTENT_WIDTH;
    let view = f32::from(decibell_ui::visible_window_bounds(window, cx).size.width);
    let max_text = (view / 2. - CHROME).max(40.);
    let engine = Engine::global(cx);
    let face = engine.face(&FontSpec { family: cx.theme().font(FontRole::Sans), weight: 400, italic: false });
    let style = TextStyle::new(face, 14., text).line_height_px(18.);
    let layout = engine.label_layout(&b.message, &style, Wrapping::NORMAL, max_text, window.scale_factor());
    let width = layout.width().min(max_text) + CHROME;
    // Centred on the field; the arrow 8 px in from the field's left, kept 18 px inside the bubble.
    let left = field_width / 2. - width / 2.;
    let arrow_x = (8. - left).clamp(ARROW + ARROW_MARGIN, (width - ARROW - ARROW_MARGIN).max(ARROW + ARROW_MARGIN));
    let opacity = decibell_ui::motion::transition_from(
        window,
        cx,
        ("auth-bubble", b.seq as usize),
        0.,
        1.,
        Transition::new(467, decibell_ui::motion::TAILWIND),
    );
    let arrow = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let tip = bounds.origin.x + px(arrow_x);
            let triangle = |top: f32, color: Hsla, window: &mut Window| {
                let mut path = PathBuilder::fill();
                path.move_to(point(tip - px(ARROW), bounds.origin.y + px(top + ARROW)));
                path.line_to(point(tip, bounds.origin.y + px(top)));
                path.line_to(point(tip + px(ARROW), bounds.origin.y + px(top + ARROW)));
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            };
            triangle(0., border, window);
            triangle(1., background, window);
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(width))
    .h(px(ARROW + 1.));
    // The icon: an orange square with a white "!" (sampled from Chromium's bubble).
    let icon = div()
        .relative()
        .flex_none()
        .size(px(ICON))
        .mr_2()
        .rounded(px(2.))
        .bg(rgb(0xff8c00))
        .child(div().absolute().left(px(10.)).top(px(5.)).w(px(4.)).h(px(8.)).rounded(px(1.)).bg(rgb(0xffffff)))
        .child(div().absolute().left(px(10.)).top(px(14.)).size(px(4.)).rounded(px(1.)).bg(rgb(0xffffff)));
    let body = div()
        .mt(px(ARROW))
        .flex()
        .items_start()
        .p_2()
        .rounded(px(4.))
        .border_1()
        .border_color(border)
        .bg(background)
        .shadow(vec![BoxShadow {
            color: rgba(0x00000033).into(),
            offset: point(px(4.), px(4.)),
            blur_radius: px(2.),
            spread_radius: px(0.),
            inset: false,
        }])
        .child(icon)
        .child(div().mt(px(3.)).mb_1().w(px(width - CHROME)).child(Label::new(b.message.clone()).size(14.).line_height(18.).color(text)));
    decibell_ui::layer(
        5,
        div()
            .absolute()
            .top(px(FIELD_HEIGHT))
            .left(px(left))
            .w(px(width))
            .opacity(opacity)
            .child(arrow)
            .child(body),
    )
    .into_any_element()
}

impl Render for AuthView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let auth: &AuthStore = model.auth.read(cx);
        let login = self.mode == AuthMode::Login;
        let loading = if login { auth.is_logging_in } else { auth.is_registering };
        let error = auth.login_error.clone();
        let notice = auth.login_notice.clone().filter(|_| error.is_none() && login);
        let register_result = auth.register_result.clone();
        if probes::enabled() {
            probes::flag("auth-register", !login);
            probes::flag("auth-busy", loading);
            let bubble = self.bubble.as_ref();
            probes::text("auth-bubble", &bubble.map(|b| b.message.clone()).unwrap_or_default());
            probes::text("auth-bubble-field", &SharedString::new_static(bubble.map_or("", |b| b.field.key())));
            let focused = [AuthField::Username, AuthField::Email, AuthField::Password]
                .into_iter()
                .find(|f| self.input(*f).read(cx).focus_handle(cx).is_focused(window))
                .map(AuthField::key)
                .or_else(|| self.submit_focus.is_focused(window).then_some("auth-submit"))
                .or_else(|| self.switch_focus.is_focused(window).then_some("auth-switch"))
                .unwrap_or("");
            probes::text("auth-focus", &SharedString::new_static(focused));
        }

        // A disabled button leaves the tab order.
        if self.submit_focus.tab_stop != !loading {
            self.submit_focus = self.submit_focus.clone().tab_stop(!loading);
        }

        let username = self.field(AuthField::Username, window, cx);
        let email = (!login).then(|| self.field(AuthField::Email, window, cx));
        let password = self.field(AuthField::Password, window, cx);
        let messages: Vec<AnyElement> = {
            let (red, amber, green) = {
                let theme = cx.theme();
                (theme.color_error, theme.color_warning, theme.color_success)
            };
            let mut lines = Vec::new();
            if let Some(error) = &error {
                lines.push(centred(error, red, window, cx));
            }
            if let Some(notice) = &notice {
                lines.push(centred(notice, amber, window, cx));
            }
            if let Some(result) = &register_result {
                lines.push(centred(&result.message, if result.success { green } else { red }, window, cx));
            }
            lines
        };
        if probes::enabled() {
            let shown = |line: Option<&SharedString>| line.cloned().unwrap_or_default();
            probes::text("auth-error", &shown(error.as_ref()));
            probes::text("auth-notice", &shown(notice.as_ref()));
            probes::text("auth-result", &shown(register_result.as_ref().map(|r| &r.message)));
        }

        // `flex w-full items-center justify-center rounded-md bg-accent py-2.5 text-sm
        // font-semibold text-on-accent transition-colors hover:bg-accent-hover
        // active:bg-accent-pressed disabled:opacity-50`; its focus ring is the theme's.
        let hover = Hover::new(window, cx, "auth-submit-hover", Some(Transition::TAILWIND));
        let theme = cx.theme();
        let ring = theme.shadow_ring.clone();
        let submit = div()
            .id("auth-submit")
            .track_focus(&self.submit_focus)
            .relative()
            .flex()
            .w_full()
            .py_2p5()
            .items_center()
            .justify_center()
            .rounded(theme.radius_md)
            .bg(mix(theme.color_accent, theme.color_accent_hover, hover.progress))
            .on_hover(hover.listener())
            .focus_visible(move |s| s.shadow(ring))
            .when(!loading, |this| {
                let pressed = theme.color_accent_pressed;
                this.cursor_pointer()
                    .active(move |s| s.bg(pressed))
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)))
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
            })
            .children(probes::probe("auth-submit"));

        // `mt-4 text-center text-sm text-text-secondary`: the question, a space, then the
        // `font-semibold text-accent hover:text-accent-bright` button.
        let link_hover = Hover::new(window, cx, "auth-switch-hover", None);
        let theme = cx.theme();
        let ring = theme.shadow_ring.clone();
        let link_color = if link_hover.progress >= 1. { theme.color_accent_bright } else { theme.color_accent };
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
                    .track_focus(&self.switch_focus)
                    // A flex box: the probe's canvas (absolute, no insets) then sits on it.
                    .relative()
                    .flex()
                    .rounded(theme.radius_sm)
                    .cursor_pointer()
                    .on_hover(link_hover.listener())
                    .focus_visible(move |s| s.shadow(ring))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_mode(if login { AuthMode::Register } else { AuthMode::Login }, cx)
                    }))
                    .child(Label::new(if login { "Register" } else { "Log In" }).size(14.).line_height(20.).weight(600.).color(link_color))
                    .children(probes::probe("auth-switch")),
            );

        let theme = cx.theme();
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
            .child(div().mb_3().child(username))
            .when_some(email, |this, email| {
                let theme = cx.theme();
                this.child(
                    div().mt(px(-6.)).mb_3().child(
                        Label::new("Lowercase letters, digits, and . _ - only.")
                            .size(11.)
                            .color(theme.color_text_muted),
                    ),
                )
                .child(Self::label("EMAIL", cx))
                .child(div().mb_3().child(email))
            })
            .child(Self::label("PASSWORD", cx))
            .child(div().mb_5().child(password))
            .child(submit)
            .children(messages)
            .child(switch);

        let theme = cx.theme();
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

//! Self-tests of the sign-in page and the account's devices (`docs/FEATURES.md` §1), against the
//! local test stack, each in a child app as the shell's others (`selftests.rs`).
//!
//! - `auth-typed`: both forms typed into with key presses, as a person would: the browser's
//!   checks (an empty field, an email Chromium refuses), a wrong password for perf1 (central's
//!   refusal under the form), the right one (the main view), then the sign-up form: perf1 again
//!   (central's refusal, the red line), a throwaway account registered (the success line) and
//!   signed in with what the form kept. One registration per run (central
//!   takes ten sign-ups per address, then one per two minutes).
//! - `auth-idle`: the pages at rest draw nothing: the sign-in page, the sign-up page, and with a
//!   field focused while the window is in the background (the caret blinks only in an active
//!   window); a try another window or the pointer disturbed is taken again.
//! - `auth-cert-retrust`: central's pin in the profile's `cert_pins.json` is wrong: the sign-in
//!   opens the "Certificate changed" prompt with the fingerprint central presents; Cancel leaves
//!   the page as it was (no error, not signed in, the pin untouched); Trust re-pins and runs the
//!   sign-in again, which signs in. (A community's pin can't be made wrong on the shared stack:
//!   the fingerprint central reports for it wins over the profile's pin, `net/pins.rs`; the
//!   invite card's retry is the model's unit test `a_refused_certificate_offers_the_prompt_…`.)

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, ensure};
use decibell_harness::{SelfTest, TestCx};
use decibell_model::actions;
use gpui::{KeyCode, Keystroke, PlatformInput};

use crate::probes;
use crate::selftests::{Child, Profile, click_probe, model, run_child, stack, wait_for, wait_probe};

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "auth-typed",
            about: "the sign-in and sign-up forms typed into: the browser's checks, perf1's wrong password then the right one, a throwaway registered and signed in",
            run: |cx| Box::pin(auth_typed(cx)),
        },
        SelfTest {
            name: "auth-idle",
            about: "the sign-in and sign-up pages at rest, unfocused and with a field focused: 0 frames drawn",
            run: |cx| Box::pin(auth_idle(cx)),
        },
        SelfTest {
            name: "auth-cert-retrust",
            about: "a wrong pin for central in the profile: signing in opens \"Certificate changed\"; Cancel stays signed out; Trust re-pins and the sign-in runs again",
            run: |cx| Box::pin(auth_cert_retrust(cx)),
        },
    ]
}

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "auth-typed", about: "(in the app) the forms, typed", run: |cx| Box::pin(check_auth_typed(cx)) },
        SelfTest { name: "auth-idle", about: "(in the app) the pages at rest", run: |cx| Box::pin(check_auth_idle(cx)) },
        SelfTest { name: "auth-cert-retrust", about: "(in the app) the re-trust prompt", run: |cx| Box::pin(check_cert_retrust(cx)) },
    ]
}

fn random_hex(n: usize) -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u32(std::process::id());
    let a = h.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())[..n].to_string()
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is not set"))
}

/// The core honours `DECIBELL_CENTRAL` in this build and it is loopback: only then may a check
/// type a password or register.
fn local_only() -> Result<()> {
    ensure!(
        decibell_model::core::DEVELOPMENT_BUILD,
        "this build ignores DECIBELL_CENTRAL (a release build without dev-local): refusing to talk to any server"
    );
    let central = env("DECIBELL_CENTRAL")?;
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: refusing");
    Ok(())
}

/// What the sign-in page last drew under a probe text key (`auth.rs`).
fn shown(key: &str) -> String {
    probes::text_of(key).map(|t| t.to_string()).unwrap_or_default()
}

/// Draw what is pending now: a window in the background draws only when GPUI dispatches a key
/// event (which draws first), so a click after a change would reach the last frame's handlers and
/// a probe the last frame's place. A modifiers event that names no key changes nothing else.
fn draw_pending(cx: &TestCx) -> Result<()> {
    cx.update(|window, cx| {
        window.dispatch_event(PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent::default()), cx);
    })
}

/// Click a probed element of the frame as it is now.
async fn click(cx: &TestCx, key: &str) -> Result<()> {
    draw_pending(cx)?;
    click_probe(cx, key).await
}

/// A key pressed and released (a button's activation needs both), a frame drawn between them.
fn press(cx: &TestCx, key: &str) -> Result<()> {
    let keystroke = Keystroke { modifiers: Default::default(), key: key.into(), key_char: None };
    cx.key_event(keystroke.clone(), KeyCode::Unknown, true)?;
    draw_pending(cx)?;
    cx.key_event(keystroke, KeyCode::Unknown, false)
}

async fn wait_shown(cx: &TestCx, key: &'static str, want: &str, limit: Duration) -> Result<()> {
    let owned = want.to_string();
    let what = format!("{key} to show {want:?}");
    wait_for(cx, limit, &what, move |_| shown(key) == owned)
        .await
        .with_context(|| format!("{key} shows {:?}", shown(key)))?;
    Ok(())
}

// ── auth-typed ───────────────────────────────────────────────────────────────────────────────

async fn auth_typed(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let name = format!("t{}", random_hex(10));
    cx.log(format_args!("throwaway account {name}"));
    let env = vec![
        ("DECIBELL_TEST_ACCOUNT", name.clone()),
        ("DECIBELL_TEST_EMAIL", format!("{name}@example.test")),
        ("DECIBELL_TEST_PASSWORD", format!("pw-{}", random_hex(12))),
    ];
    let profile = Profile::new("auth-typed")?;
    match run_child(&cx, "auth-typed", false, &stack, &profile.0, &env).await? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            cx.log("SKIP: this binary has no dev-local");
            Ok(())
        }
    }
}

async fn check_auth_typed(cx: TestCx) -> Result<()> {
    local_only()?;
    let perf1_password = env("DECIBELL_DEV_PASSWORD")?;
    let name = env("DECIBELL_TEST_ACCOUNT")?;
    let email = env("DECIBELL_TEST_EMAIL")?;
    let password = env("DECIBELL_TEST_PASSWORD")?;
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    wait_for(&cx, Duration::from_secs(5), "the sign-in page", |_| probes::flag_set("page-sign-in")).await?;
    wait_probe(&cx, "auth-username").await?;
    cx.settle().await?;
    // The renderer's inputs have no autoFocus: nothing is focused as the page opens.
    ensure!(shown("auth-focus").is_empty(), "{} is focused as the page opens", shown("auth-focus"));
    // The mode link, there and back: what was typed stays (nothing yet).
    click(&cx, "auth-switch").await?;
    wait_for(&cx, Duration::from_secs(5), "the sign-up form by the link", |_| probes::flag_set("auth-register")).await?;
    click(&cx, "auth-switch").await?;
    wait_for(&cx, Duration::from_secs(5), "the sign-in form by the link", |_| !probes::flag_set("auth-register")).await?;
    cx.log("the mode link: Register, then Log In");

    // An empty form: the browser refuses it before anything is sent, the username takes the focus
    // and Chromium's bubble.
    click(&cx, "auth-submit").await?;
    wait_shown(&cx, "auth-bubble", crate::validation::VALUE_MISSING, Duration::from_secs(5)).await?;
    ensure!(shown("auth-bubble-field") == "auth-username", "the bubble is on {}", shown("auth-bubble-field"));
    ensure!(shown("auth-focus") == "auth-username", "{} has the focus, not the username", shown("auth-focus"));
    ensure!(!cx.update(|_, cx| model(cx).auth.read(cx).is_logging_in)?, "an empty form was sent");
    cx.log("an empty form: refused, the username focused, \"Please fill out this field.\" under it");

    // Typing fixes it: the bubble goes. Tab to the password, a wrong one, Enter.
    cx.type_text("perf1")?;
    wait_shown(&cx, "auth-bubble", "", Duration::from_secs(5)).await?;
    cx.keystroke("tab")?;
    wait_shown(&cx, "auth-focus", "auth-password", Duration::from_secs(5)).await?;
    cx.type_text("not-perf1s-password")?;
    cx.keystroke("enter")?;
    ensure!(cx.update(|_, cx| model(cx).auth.read(cx).is_logging_in)?, "Enter in the password didn't sign in");
    wait_for(&cx, Duration::from_secs(20), "central's refusal", |cx| model(cx).auth.read(cx).login_error.is_some()).await?;
    let refusal = cx.update(|_, cx| model(cx).auth.read(cx).login_error.clone())?.unwrap_or_default();
    ensure!(&*refusal == "Invalid username or password.", "the refusal reads {refusal:?}");
    wait_shown(&cx, "auth-error", &refusal, Duration::from_secs(5)).await?;
    cx.log(format_args!("a wrong password for perf1: {refusal:?} under the form"));

    // The right one: select what the field holds, type, Enter.
    ensure!(shown("auth-focus") == "auth-password", "the password lost the focus");
    cx.keystroke("ctrl-a")?;
    cx.type_text(&perf1_password)?;
    cx.keystroke("enter")?;
    let t = wait_for(&cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    wait_for(&cx, Duration::from_secs(10), "the main view", |_| !probes::flag_set("page-sign-in") && probes::bounds("home").is_some())
        .await?;
    let who = cx.update(|_, cx| model(cx).auth.read(cx).username.clone())?.unwrap_or_default();
    ensure!(&*who == "perf1", "signed in as {who}");
    cx.log(format_args!("the right password: signed in as perf1 after {} ms, the main view drawn", t.as_millis()));
    log_out(&cx).await?;

    // The sign-up form, by its link: the username lower-cases as typed; Tab through the fields.
    wait_probe(&cx, "auth-switch").await?;
    cx.settle().await?;
    click(&cx, "auth-switch").await?;
    let switched =
        wait_for(&cx, Duration::from_secs(5), "the sign-up form", |_| probes::flag_set("auth-register") && probes::bounds("auth-email").is_some())
            .await;
    if switched.is_err() {
        // Diagnosis: the same click again, a frame later.
        cx.log(format_args!("the link's click did nothing (link at {:?}); again after a frame", probes::bounds("auth-switch")));
        cx.settle().await?;
        draw_pending(&cx)?;
        cx.sleep(Duration::from_millis(300)).await;
        click(&cx, "auth-switch").await?;
        let again = wait_for(&cx, Duration::from_secs(5), "the sign-up form (second click)", |_| probes::flag_set("auth-register")).await;
        cx.log(format_args!("second click: {}", if again.is_ok() { "switched" } else { "nothing either" }));
    }
    if let Err(e) = switched {
        // Where the click went.
        let state = cx.update(|window, _| {
            format!(
                "the link at {:?}, the pointer at {:?}, the window {:?} active {}, page-sign-in {}, register {}",
                probes::bounds("auth-switch"),
                window.mouse_position(),
                window.viewport_size(),
                window.is_window_active(),
                probes::flag_set("page-sign-in"),
                probes::flag_set("auth-register"),
            )
        })?;
        anyhow::bail!("{e:#} ({state})");
    }
    click(&cx, "auth-username").await?;
    wait_shown(&cx, "auth-focus", "auth-username", Duration::from_secs(5)).await?;
    // A taken name first, typed in capitals: the form lower-cases it as typed.
    cx.type_text("PERF1")?;
    wait_shown(&cx, "auth-username", "perf1", Duration::from_secs(5)).await?;
    cx.log("typed PERF1 in the sign-up form: it holds perf1");
    cx.keystroke("tab")?;
    wait_shown(&cx, "auth-focus", "auth-email", Duration::from_secs(5)).await?;
    cx.type_text("not-an-email")?;
    cx.keystroke("tab")?;
    wait_shown(&cx, "auth-focus", "auth-password", Duration::from_secs(5)).await?;
    cx.type_text(&password)?;
    cx.keystroke("tab")?;
    wait_shown(&cx, "auth-focus", "auth-submit", Duration::from_secs(5)).await?;
    // Space presses the focused Register button; Chromium refuses the email.
    press(&cx, "space")?;
    let refused = "Please include an '@' in the email address. 'not-an-email' is missing an '@'.";
    wait_shown(&cx, "auth-bubble", refused, Duration::from_secs(5)).await?;
    ensure!(shown("auth-focus") == "auth-email", "{} has the focus, not the email", shown("auth-focus"));
    ensure!(!cx.update(|_, cx| model(cx).auth.read(cx).is_registering)?, "a refused form was sent");
    cx.log(format_args!("Tab order username → email → password → Register; Space on Register: refused, {refused:?}"));

    // Fix the email, Enter in it: central refuses the taken name (the red line).
    cx.keystroke("ctrl-a")?;
    cx.type_text(&email)?;
    wait_shown(&cx, "auth-bubble", "", Duration::from_secs(5)).await?;
    cx.keystroke("enter")?;
    ensure!(cx.update(|_, cx| model(cx).auth.read(cx).is_registering)?, "Enter in the email didn't register");
    wait_for(&cx, Duration::from_secs(30), "central's answer", |cx| model(cx).auth.read(cx).register_result.is_some()).await?;
    let taken = cx.update(|_, cx| model(cx).auth.read(cx).register_result.clone())?.context("no answer")?;
    ensure!(!taken.success && &*taken.message == "Username or email already exists.", "registering perf1: {taken:?}");
    wait_shown(&cx, "auth-result", &taken.message, Duration::from_secs(5)).await?;
    cx.log(format_args!("perf1 again: {:?} under the form", taken.message));

    // The throwaway name instead (in capitals again), Enter in the username: one registration.
    click(&cx, "auth-username").await?;
    wait_shown(&cx, "auth-focus", "auth-username", Duration::from_secs(5)).await?;
    cx.keystroke("ctrl-a")?;
    cx.type_text(&name.to_uppercase())?;
    wait_shown(&cx, "auth-username", &name, Duration::from_secs(5)).await?;
    let started = Instant::now();
    cx.keystroke("enter")?;
    ensure!(cx.update(|_, cx| model(cx).auth.read(cx).is_registering)?, "Enter in the username didn't register");
    wait_for(&cx, Duration::from_secs(30), "central's answer", |cx| {
        let auth = model(cx).auth.read(cx);
        !auth.is_registering && auth.register_result.is_some()
    })
    .await?;
    let result = cx.update(|_, cx| model(cx).auth.read(cx).register_result.clone())?.context("no answer")?;
    ensure!(result.success, "registering {name} was refused: {}", result.message);
    ensure!(&*result.message == "Registration successful.", "the success line reads {:?}", result.message);
    wait_shown(&cx, "auth-result", &result.message, Duration::from_secs(5)).await?;
    cx.log(format_args!("registered {name} in {} ms: {:?} under the form", started.elapsed().as_millis(), result.message));

    // Back to Log In: what was typed stays, the result line too (the store's); Log In signs in.
    click(&cx, "auth-switch").await?;
    wait_for(&cx, Duration::from_secs(5), "the sign-in form", |_| !probes::flag_set("auth-register")).await?;
    ensure!(shown("auth-username") == name, "the username became {:?}", shown("auth-username"));
    ensure!(shown("auth-password") == password.chars().count().to_string(), "the password wasn't kept");
    ensure!(shown("auth-result") == *result.message, "the result line went with the mode");
    click(&cx, "auth-submit").await?;
    wait_for(&cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    let who = cx.update(|_, cx| model(cx).auth.read(cx).username.clone())?.unwrap_or_default();
    ensure!(*who == *name, "signed in as {who}");
    cx.log(format_args!("Log In with what the form kept: signed in as {name}"));
    log_out(&cx).await
}

/// Log out (forgets this device at central) and wait for the sign-in page.
async fn log_out(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::auth::log_out(cx))?;
    wait_for(cx, Duration::from_secs(15), "the sign-in page", |cx| {
        !model(cx).auth.read(cx).is_authenticated && probes::flag_set("page-sign-in")
    })
    .await?;
    Ok(())
}

// ── auth-cert-retrust ────────────────────────────────────────────────────────────────────────

/// The stack's central certificate fingerprint (`accounts.json`'s `central_cert_sha256`).
fn central_fingerprint() -> Result<String> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let text = std::fs::read_to_string(dev.join("run/accounts.json")).context("reading the stack's accounts.json")?;
    let at = text.find("\"central_cert_sha256\"").context("accounts.json has no central_cert_sha256")?;
    let rest = &text[at + "\"central_cert_sha256\"".len()..];
    let start = rest.find('"').context("central_cert_sha256: no value")? + 1;
    let end = rest[start..].find('"').context("central_cert_sha256: unterminated")?;
    Ok(rest[start..start + end].to_string())
}

/// A wrong fingerprint for central.
const WRONG_PIN: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

async fn auth_cert_retrust(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile::new("auth-cert-retrust")?;
    // Central is pinned on first use: this profile "remembers" another certificate for it.
    std::fs::write(profile.0.join("cert_pins.json"), format!("{{\n  \"{}\": \"{WRONG_PIN}\"\n}}", stack.central))?;
    let env = vec![("DECIBELL_TEST_CENTRAL_CERT", central_fingerprint()?)];
    match run_child(&cx, "auth-cert-retrust", false, &stack, &profile.0, &env).await? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            cx.log("SKIP: this binary has no dev-local");
            Ok(())
        }
    }
}

/// What the profile's `cert_pins.json` pins central to.
fn central_pin() -> Result<Option<String>> {
    let path = std::path::PathBuf::from(env("DECIBELL_USER_DATA_DIR")?).join("cert_pins.json");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let key = format!("\"{}\"", env("DECIBELL_CENTRAL")?);
    let Some(at) = text.find(&key) else { return Ok(None) };
    let rest = &text[at + key.len()..];
    let start = rest.find('"').context("no value")? + 1;
    let end = rest[start..].find('"').context("unterminated")?;
    Ok(Some(rest[start..start + end].to_string()))
}

fn prompt(cx: &gpui::App) -> Option<(String, u16, String)> {
    model(cx).overlays.read(cx).cert_mismatch.as_ref().map(|n| (n.cert.host.to_string(), n.cert.port, n.cert.fingerprint.to_string()))
}

async fn check_cert_retrust(cx: TestCx) -> Result<()> {
    local_only()?;
    let password = env("DECIBELL_DEV_PASSWORD")?;
    let real = env("DECIBELL_TEST_CENTRAL_CERT")?;
    let central = env("DECIBELL_CENTRAL")?;
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    wait_probe(&cx, "auth-username").await?;
    cx.settle().await?;
    ensure!(central_pin()?.as_deref() == Some(WRONG_PIN), "the profile's pin for central isn't the planted one");

    click(&cx, "auth-username").await?;
    cx.type_text("perf1")?;
    cx.keystroke("tab")?;
    cx.type_text(&password)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(20), "the certificate prompt", |cx| prompt(cx).is_some()).await?;
    let presented = cx.update(|_, cx| prompt(cx))?.context("the prompt went")?;
    cx.log(format_args!(
        "signing in: \"Certificate changed\" for {}:{}, presented {}…",
        presented.0,
        presented.1,
        &presented.2[..16.min(presented.2.len())]
    ));
    ensure!(format!("{}:{}", presented.0, presented.1) == central, "the prompt names {}:{}", presented.0, presented.1);
    ensure!(presented.2 == real, "the prompt shows {} where central presents {real}", presented.2);
    let (busy, error) = cx.update(|_, cx| {
        let auth = model(cx).auth.read(cx);
        (auth.is_logging_in, auth.login_error.clone())
    })?;
    ensure!(!busy && error.is_none(), "with the prompt up the page is busy {busy} / shows {error:?}");

    // Cancel: nothing re-pinned, still signed out, nothing under the form.
    wait_probe(&cx, "cert-cancel").await?;
    cx.settle().await?;
    click(&cx, "cert-cancel").await?;
    wait_for(&cx, Duration::from_secs(5), "the prompt to close", |cx| prompt(cx).is_none()).await?;
    cx.sleep(Duration::from_millis(500)).await;
    ensure!(!cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?, "Cancel signed in");
    ensure!(shown("auth-error").is_empty(), "Cancel left {:?} under the form", shown("auth-error"));
    ensure!(central_pin()?.as_deref() == Some(WRONG_PIN), "Cancel changed the pin");
    cx.log("Cancel: the prompt closed, still signed out, the pin untouched");

    // Log In again (the button this time): the prompt again; Trust re-pins and signs in.
    click(&cx, "auth-submit").await?;
    wait_for(&cx, Duration::from_secs(20), "the prompt again", |cx| prompt(cx).is_some()).await?;
    wait_probe(&cx, "cert-trust").await?;
    cx.settle().await?;
    click(&cx, "cert-trust").await?;
    let t = wait_for(&cx, Duration::from_secs(30), "the retried sign-in", |cx| {
        model(cx).auth.read(cx).is_authenticated && prompt(cx).is_none()
    })
    .await?;
    let pin = central_pin()?;
    ensure!(pin.as_deref() == Some(real.as_str()), "after Trust central is pinned to {pin:?}");
    cx.log(format_args!("Trust new certificate: the prompt closed, the sign-in ran again ({} ms) and central is pinned to what it presents", t.as_millis()));
    log_out(&cx).await
}

// ── auth-idle ────────────────────────────────────────────────────────────────────────────────

async fn auth_idle(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile::new("auth-idle")?;
    match run_child(&cx, "auth-idle", false, &stack, &profile.0, &[]).await? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            cx.log("SKIP: this binary has no dev-local");
            Ok(())
        }
    }
}

/// Nothing drawn for 3 s once drawing stopped (with a focused field in the active window: only the
/// caret's blink, Blink's 500 ms on / 500 ms off, at most 7 frames); a try in which the window's
/// focus changed hands or the pointer moved is taken again (five tries, else inconclusive).
async fn rests(cx: &TestCx, what: &str, caret: bool) -> Result<()> {
    let mut last = String::new();
    for attempt in 1..=5 {
        let before = cx.settle().await?;
        let active = cx.update(|window, _| window.is_window_active())?;
        let disturbed = cx.watched_sleep(Duration::from_secs(3)).await?;
        let frames = cx.frames()?.since(before);
        last = format!(
            "try {attempt}: {} drawn, {} presented in 3 s, window {}{}",
            frames.drawn,
            frames.presented,
            if active { "active" } else { "in the background" },
            disturbed.as_ref().map_or(String::new(), |d| format!(" (disturbed: {d})"))
        );
        cx.log(format_args!("{what}: {last}"));
        if disturbed.is_some() {
            continue;
        }
        let allowed = if caret && active { 7 } else { 0 };
        ensure!(frames.drawn <= allowed, "{what} drew at rest: {last}");
        return Ok(());
    }
    Err(decibell_harness::inconclusive(format_args!("{what}: no undisturbed try in five ({last})")))
}

async fn check_auth_idle(cx: TestCx) -> Result<()> {
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    wait_probe(&cx, "auth-switch").await?;
    // The entrance (the bell mark's rise) ends first.
    cx.sleep(Duration::from_secs(1)).await;
    rests(&cx, "the sign-in page", false).await?;
    click(&cx, "auth-switch").await?;
    wait_for(&cx, Duration::from_secs(5), "the sign-up form", |_| probes::flag_set("auth-register")).await?;
    rests(&cx, "the sign-up page", false).await?;
    click(&cx, "auth-username").await?;
    cx.type_text("someone")?;
    rests(&cx, "the sign-up page with a field focused", true).await
}

//! The shell's screens, drawn from [`crate::fixture`] (no network, no clock): `decibell-capture
//! --screen <name>` and `decibell --screen <name>`.

use decibell_harness::Screen;
use decibell_ui::AssumeClientDecorations;
use gpui::{App, AppContext as _};

use crate::auth::{AuthMode, AuthView};
use crate::fixture::{self, Scene};
use crate::shell::Shell;

/// Captures render one frame on a clock that doesn't run: entrances show their end and the glow
/// rests (the kit's screens do the same).
fn settle_motion_for_capture(cx: &mut App) {
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
}

fn signed_out(cx: &mut App) {
    let model = match cx.try_global::<decibell_model::Model>() {
        Some(model) => model.clone(),
        None => decibell_model::Model::init(cx, None),
    };
    model.auth.update(cx, |a, cx| a.set_booted(None, cx));
}

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "auth",
            about: "The sign-in page (signed out, nothing typed)",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                signed_out(cx);
                cx.new(Shell::new).into()
            },
        },
        Screen {
            name: "auth-register",
            about: "The sign-up page",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                signed_out(cx);
                cx.new(|cx| AuthView::new(AuthMode::Register, cx)).into()
            },
        },
        Screen {
            name: "main-server",
            about: "Signed in as perf1: Daily Gaming #general, in voice in Squad, the member list (fixture)",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                fixture::apply(Scene::Server, cx);
                cx.new(Shell::new).into()
            },
        },
        Screen {
            name: "main-home",
            about: "Signed in as perf1: Home, the Friends page beside the conversation list (fixture)",
            build: |_, cx| {
                settle_motion_for_capture(cx);
                fixture::apply(Scene::Home, cx);
                cx.new(Shell::new).into()
            },
        },
    ]
}

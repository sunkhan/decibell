//! Whether the window counts as **focused** for behaviour that depends on the user looking at it
//! (a DM is marked read only while its window is focused: CONVENTIONS, "Home"): the OS gives it
//! the keyboard (`Window::is_window_active`) and it is presented (`Window::is_visible`: a
//! minimised window is never focused, whatever the platform says about activation).
//!
//! Self-tests share the desktop with other windows, so real focus isn't theirs to hold:
//! [`hold_window_focus`] pins the "active" half for this process (visibility stays real, so a
//! minimised window still counts as unfocused). Views follow it with [`observe_window_focus`],
//! which also observes the window's activation and visibility; without a hold (the app) the
//! global never exists and its observer never fires.

use gpui::{App, Context, Global, Subscription, Window};

/// The test hold of the window's "active" state (`None`: the platform's).
#[derive(Clone, Copy, Debug, Default)]
pub struct FocusOverride(pub Option<bool>);

impl Global for FocusOverride {}

/// The window is focused: active (or held so by a self-test) and visible.
pub fn window_focused(window: &Window, cx: &App) -> bool {
    window_active(window, cx) && window.is_visible()
}

/// The window is active (`Window::is_window_active`), or held so by a self-test: what a layer that
/// closes when its window loses the focus asks (the kit's popovers and menus).
pub fn window_active(window: &Window, cx: &App) -> bool {
    cx.try_global::<FocusOverride>().and_then(|o| o.0).unwrap_or_else(|| window.is_window_active())
}

/// Self-tests: hold the window's "active" state at `active` (`None` gives it back to the
/// platform). Observers registered with [`observe_window_focus`] run.
pub fn hold_window_focus(active: Option<bool>, cx: &mut App) {
    cx.set_global(FocusOverride(active));
}

/// Calls `changed` whenever [`window_focused`] may have changed: the window's activation, its
/// visibility, a test's hold. Keep the subscriptions as long as the view.
pub fn observe_window_focus<T: 'static>(
    window: &mut Window,
    cx: &mut Context<T>,
    changed: impl Fn(&mut T, &mut Window, &mut Context<T>) + Clone + 'static,
) -> Vec<Subscription> {
    let (a, b, c) = (changed.clone(), changed.clone(), changed);
    vec![
        cx.observe_window_activation(window, move |this, window, cx| a(this, window, cx)),
        cx.observe_window_visibility(window, move |this, _, window, cx| b(this, window, cx)),
        cx.observe_global_in::<FocusOverride>(window, move |this, window, cx| c(this, window, cx)),
    ]
}

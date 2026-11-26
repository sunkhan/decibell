//! The desktop around the window: what Electron's main process did that needs the OS rather than a
//! feature (`gpui-client/CONVENTIONS.md`, "Platform"; the services table in `HANDOFF.md`).
//!
//! - [`launch`]: what a start asks for (`decibell://` links, `--hotkey`), validated as main did.
//! - [`instance`]: one instance per profile; a second start hands its [`launch::Launch`] over.
//! - [`attention`]: the taskbar flash on an incoming call.
//! - [`progress`]: the transfers' progress on the taskbar / launcher.
//! - [`quit_guard`]: closing with uploads in flight asks first; downloads pause at quit.
//! - [`window_state`]: the window's size, place and maximised state, per profile.
//! - [`session`]: sleep and screen lock → Away.
//!
//! The app calls [`instance::start`] before GPUI starts (a second start never opens a window), then
//! [`install`] once its main window is open. Nothing here polls: the instance listener is a thread
//! blocked in `accept`, the rest are subscriptions to the model and the window.

pub mod attention;
pub mod instance;
pub mod launch;
pub mod progress;
pub mod quit_guard;
pub mod selftests;
pub mod session;
pub mod window_state;
#[cfg(windows)]
mod win32;

use std::path::PathBuf;

use decibell_model::Model;
use futures::StreamExt as _;
use gpui::{AnyView, AnyWindowHandle, App, Entity, Global, Subscription};

pub use instance::{Instance, Start};
pub use launch::Launch;
pub use quit_guard::QuitGuard;

/// Second starts handed to this instance so far (the self-tests' seam).
pub static FORWARDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// What lives as long as the app (the subscriptions end with it).
struct Installed {
    _subscriptions: Vec<Subscription>,
    _window_state: Option<Entity<window_state::Tracker>>,
    quit_guard: Entity<QuitGuard>,
}

impl Global for Installed {}

/// Every service on the main window: the first start's own `launch`, the listener for later ones
/// (`instance`, when this process holds the profile's lock), attention, progress, the quit guard,
/// the window state (with a `profile`) and sleep / lock. Returns the view to draw over the
/// window's content (the quit guard's question).
pub fn install(window: AnyWindowHandle, launch: Launch, instance: Option<Instance>, profile: Option<PathBuf>, cx: &mut App) -> AnyView {
    let mut subscriptions = Vec::new();
    subscriptions.extend(attention::install(window, cx));
    subscriptions.extend(progress::install(window, cx));
    subscriptions.extend(session::install(cx));
    let quit_guard = quit_guard::install(window, cx);
    let window_state = profile.and_then(|profile| {
        window
            .update(cx, |_, window, cx| window_state::track(window, profile, cx))
            .ok()
    });
    if let Some(instance) = instance {
        serve(instance, window, cx);
    }
    apply(launch, window, false, cx);
    let view: AnyView = quit_guard.clone().into();
    cx.set_global(Installed { _subscriptions: subscriptions, _window_state: window_state, quit_guard });
    view
}

/// The quit guard, once [`install`] ran.
pub fn quit_guard(cx: &App) -> Option<Entity<QuitGuard>> {
    cx.try_global::<Installed>().map(|i| i.quit_guard.clone())
}

/// Second starts' requests, on the UI thread, one wake each.
fn serve(instance: Instance, window: AnyWindowHandle, cx: &mut App) {
    let endpoint = instance.endpoint_path().to_path_buf();
    let mut requests = match instance.serve() {
        Ok(requests) => requests,
        Err(e) => {
            log::error!("[instance] {e:#}");
            return;
        }
    };
    marker(format_args!("instance listening"));
    cx.on_app_quit(move |_| {
        instance::remove_endpoint(&endpoint);
        async {}
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Some(launch) = requests.next().await {
            cx.update(|cx| apply(launch, window, true, cx));
        }
    })
    .detach();
}

/// What a start asked for. `forwarded`: a second start's (`second-instance`): the window comes
/// forward (restored if minimised), then its links open and its hotkey fires.
pub fn apply(launch: Launch, window: AnyWindowHandle, forwarded: bool, cx: &mut App) {
    if forwarded {
        FORWARDED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        marker(format_args!(
            "instance forwarded links={} hotkey={} token={}",
            launch.links.len(),
            launch.hotkey.as_deref().unwrap_or("-"),
            launch.activation_token.is_some()
        ));
    }
    // `decibell --hotkey=…` from a compositor binding: act, don't focus.
    if let Some(hotkey) = &launch.hotkey {
        let fired = decibell_model::actions::hotkeys::inject_cli(hotkey);
        marker(format_args!("hotkey {hotkey} {}", if fired { "injected" } else { "refused" }));
        return;
    }
    if forwarded {
        let token = launch.activation_token.clone();
        let _ = window.update(cx, |_, window, _| match &token {
            Some(token) => window.activate_window_with_token(token),
            None => window.activate_window(),
        });
    }
    for link in &launch.links {
        // Before the sign-in it waits in the servers store as the pending invite; the join
        // dialog (decibell-servers) shows it once the main layout is up.
        let opened = cx.try_global::<Model>().is_some() && decibell_model::actions::servers::open_invite_link(link, cx);
        marker(format_args!("deep-link {}", if opened { "pending" } else { "unparsed" }));
    }
}

/// One stdout line for a self-test's parent (`decibell: platform …`), in development builds only.
pub(crate) fn marker(message: std::fmt::Arguments) {
    if cfg!(feature = "dev-local") {
        println!("decibell: platform {message}");
    } else {
        log::debug!("[platform] {message}");
    }
}

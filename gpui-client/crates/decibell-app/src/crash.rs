//! Crash reports (`decibell_release::crash`) in the app: started right after the profile is
//! claimed (Electron starts Sentry before `whenReady`), and told the names a report must never
//! carry as the model learns them (the signed-in account, the communities joined).

use std::path::Path;

use decibell_model::Model;
use gpui::App;

/// Electron's three gates, then the reporter and the native handlers (nothing at all when a gate
/// is shut; the panic hook itself is installed in every build, first thing in `main`).
pub fn start(profile: &Path) {
    if let Some(config) = decibell_release::crash::gate(profile) {
        decibell_release::crash::init(config);
    }
    // A self-test's child panics on request, after reporting started (development builds).
    #[cfg(feature = "dev")]
    decibell_release::crash::panic_for_test();
}

/// Keeps the scrubber's name list current: the account's name and every community's.
pub fn watch_names(model: &Model, cx: &mut App) {
    if !decibell_release::crash::active() {
        return;
    }
    let (auth, servers) = (model.auth.clone(), model.servers.clone());
    let update = {
        let (auth, servers) = (auth.clone(), servers.clone());
        move |cx: &mut App| {
            let mut names: Vec<String> = auth.read(cx).username.iter().map(|n| n.to_string()).collect();
            names.extend(servers.read(cx).servers().iter().map(|s| s.name.to_string()));
            decibell_release::crash::set_names(names);
        }
    };
    let on_auth = update.clone();
    cx.observe(&auth, move |_, cx| on_auth(cx)).detach();
    let on_servers = update.clone();
    cx.observe(&servers, move |_, cx| on_servers(cx)).detach();
    update(cx);
    // The renderer's `connected_servers` tag.
    let connections = model.connections.clone();
    cx.observe(&connections, |connections, cx| {
        decibell_release::crash::set_connected_servers(connections.read(cx).connected().count());
    })
    .detach();
}

/// The GPU the window draws with, for crash reports (`contexts.gpu`): once its first frame drew.
pub fn note_gpu() {
    if decibell_release::crash::active()
        && let Some((name, vendor, driver)) = decibell_video::VideoDevice::adapter()
    {
        decibell_release::crash::set_gpu(&name, vendor, &driver);
    }
}

/// Ends the process when a panic unwinds the UI thread to the bottom of its stack: a panic a
/// catcher let go on (`decibell_core::rt::run` resumes its task's panic in the caller, and
/// `resume_unwind` calls no hook). Held by `boot::run` for its whole run.
pub struct AbortOnUnwind;

impl Drop for AbortOnUnwind {
    fn drop(&mut self) {
        if std::thread::panicking() {
            decibell_release::crash::abort_after_unwind();
        }
    }
}

/// The release tests' panics that need the running app (`DECIBELL_TEST_PANIC`, development
/// builds), once the core runs: `runtime:<message>` panics on the core runtime's blocking pool,
/// where the encoder probe's test encodes run (caught: the app carries on and says so on stdout);
/// `runtime-task:<message>` in an async task of the core's runtime (fatal, as before unwinding:
/// a task tokio would swallow, or whose panic `rt::run` would resume on the UI thread).
#[cfg(feature = "dev")]
pub fn panic_in_app_for_test(cx: &mut App) {
    let Ok(what) = std::env::var("DECIBELL_TEST_PANIC") else { return };
    let (blocking, message) = match what.split_once(':') {
        Some(("runtime", message)) => (true, message.to_string()),
        Some(("runtime-task", message)) => (false, message.to_string()),
        _ => return,
    };
    cx.spawn(async move |cx| {
        // The core's runtime exists once the keychain was read (a test's wait, not the app's).
        while !decibell_model::core::is_running() {
            cx.background_executor().timer(std::time::Duration::from_millis(20)).await;
        }
        let caught = if blocking {
            decibell_core::rt::spawn_blocking(move || -> () { panic!("{message}") }).await.is_err_and(|e| e.is_panic())
        } else {
            decibell_core::rt::spawn(async move { panic!("{message}") }).await.is_err_and(|e| e.is_panic())
        };
        println!("decibell: crash-test runtime panic caught by the runtime: {caught}");
        // The UI thread still takes work after the panic.
        cx.update(|_| println!("decibell: crash-test alive after the runtime panic"));
    })
    .detach();
}

//! The Transfers panel (`features/transfers/*`, main's `downloads.ts`; spec
//! `docs/superpowers/specs/2026-10-04-transfers-panel-design.md`): the title-bar button with its
//! progress ring and dot, the panel with its Downloads and Uploads tabs, and the wiring of
//! `decibell-attachments`' downloads manager into `model.live.transfers`.
//!
//! - **Wiring** ([`init`]): once the core runs, the manager is made on its runtime around the
//!   process's `Attachments` (state in the profile's `downloads.json`, files in Settings' folder or
//!   the OS Downloads folder), attached to the Transfers store (`ManagerControl`), and configured
//!   from the settings; the speed caps follow Settings → Network. Paused encrypted downloads keep
//!   their key only in memory (`ManagerOptions::key_wrap` is none: Electron wrapped it with
//!   `safeStorage`; a keychain-backed `KeyWrap` is a follow-up), so they resume within the session.
//! - **The button** sits in the title bar's actions slot ([`title_bar_button`], drawn by the app's
//!   root once [`enable_title_bar`] was called: the app does it when the core starts, the
//!   transfers screens do it themselves). Ctrl+J (Cmd+J) toggles the panel.
//! - **What the app reads** for the taskbar / dock progress and the quit guard:
//!   `model.live.transfers.read(cx).progress()` (a fraction, or none to clear; `paused_only`) and
//!   `uploads_in_flight()` (closing loses those: ask first). GPUI has no taskbar-progress API at the
//!   pin; Electron throttled its call to 4 a second.

mod format;
mod runs;
#[cfg(feature = "fixtures")]
mod screens;
#[cfg(feature = "fixtures")]
mod selftests;
mod view;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use decibell_attachments::manager::{DownloadManager, ManagerOptions, RevealTarget};
use decibell_model::live::transfers::{DownloadEvents, ManagerControl};
use decibell_model::{Model, actions};
use gpui::{AnyView, App, AppContext as _, Entity, Global, KeyBinding, actions as gpui_actions};

pub use format::{download_status, upload_status};
pub use view::TransfersButton;

gpui_actions!(transfers, [ToggleTransfers]);

/// The app's downloads manager, once made: for what `DownloadControl` doesn't carry (the file to
/// open, what "Show in folder" reveals, the folder "Open folder" opens).
struct Manager(Arc<DownloadManager>);

impl Global for Manager {}

/// The button's view, made on first use; [`enable_title_bar`] turns it on.
#[derive(Default)]
struct Button {
    enabled: bool,
    view: Option<Entity<TransfersButton>>,
}

impl Global for Button {}

/// Screens pin "now" (the "N min ago" labels must not follow the clock).
pub(crate) struct FixedNow(pub i64);

impl Global for FixedNow {}

/// Once at start-up (`crate::install`): Ctrl+J, and the downloads manager as soon as the core runs.
pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(if cfg!(target_os = "macos") { "cmd-j" } else { "ctrl-j" }, ToggleTransfers, None)]);
    cx.on_action(|_: &ToggleTransfers, cx| {
        if let Some(model) = cx.try_global::<Model>().cloned()
            && cx.try_global::<Button>().is_some_and(|b| b.enabled)
        {
            model.live.transfers.update(cx, |t, cx| t.toggle_panel(cx));
        }
    });
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    // The core starts after the features' init: wire when it reports booted (and at once if it
    // already runs). Screens have no core: nothing happens there.
    let mut wired = false;
    let mut try_wire = move |cx: &mut App| {
        if wired || !decibell_model::core::is_running() {
            return;
        }
        wired = true;
        wire_downloads(cx);
    };
    try_wire(cx);
    cx.observe(&model.auth, move |_, cx| try_wire(cx)).detach();
    // Settings → Network: the speed caps reach the attachment service (downloads and uploads).
    let mut caps = (u64::MAX, u64::MAX);
    cx.observe(&model.settings, move |settings, cx| {
        let s = settings.read(cx).get();
        let now = (s.download_limit_bps, s.upload_limit_bps);
        if now == caps || !decibell_model::core::is_running() {
            return;
        }
        caps = now;
        decibell_model::chat::composer::CoreAttachmentService::with_attachments(move |svc| {
            svc.limits().set_download(now.0);
            svc.limits().set_upload(now.1);
        });
        actions::transfers::configure_downloads(cx);
    })
    .detach();
}

/// Makes the manager on the core's runtime, attaches it to the Transfers store, configures it.
fn wire_downloads(cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    let Some(state_dir) = decibell_model::core::user_data_dir() else { return };
    let default_dir = crate::downloads::os_downloads_dir();
    let slot: Arc<OnceLock<DownloadEvents>> = Arc::default();
    let (tx, rx) = futures::channel::oneshot::channel::<(Arc<DownloadManager>, tokio::runtime::Handle)>();
    let events = slot.clone();
    decibell_model::chat::composer::CoreAttachmentService::with_attachments(move |svc| {
        let options = ManagerOptions { state_dir, default_dir, key_wrap: None };
        let manager = Arc::new(DownloadManager::new(svc, options, move |event| {
            // Nothing is emitted before the store attached: the manager starts with no runs.
            if let Some(events) = events.get() {
                events.push(event);
            }
        }));
        let _ = tx.send((manager, tokio::runtime::Handle::current()));
    });
    cx.spawn(async move |cx| {
        let Ok((manager, runtime)) = rx.await else { return };
        cx.update(|cx| {
            let control = Rc::new(ManagerControl { manager: manager.clone(), runtime });
            let events = model.live.transfers.update(cx, |t, cx| t.attach_downloads(control, cx));
            let _ = slot.set(events);
            cx.set_global(Manager(manager));
            actions::transfers::configure_downloads(cx);
            let s = model.settings.read(cx).get();
            let caps = (s.download_limit_bps, s.upload_limit_bps);
            decibell_model::chat::composer::CoreAttachmentService::with_attachments(move |svc| {
                svc.limits().set_download(caps.0);
                svc.limits().set_upload(caps.1);
            });
        });
    })
    .detach();
}

/// Shows the button in the title bar (the app, when the core starts; the transfers screens).
pub fn enable_title_bar(cx: &mut App) {
    cx.default_global::<Button>().enabled = true;
}

/// The title bar's actions: the Transfers button, when enabled.
pub fn title_bar_button(cx: &mut App) -> Option<AnyView> {
    let state = cx.try_global::<Button>()?;
    if !state.enabled {
        return None;
    }
    if let Some(view) = &state.view {
        return Some(view.clone().into());
    }
    let view = cx.new(TransfersButton::new);
    cx.default_global::<Button>().view = Some(view.clone());
    Some(view.into())
}

fn manager(cx: &App) -> Option<Arc<DownloadManager>> {
    cx.try_global::<Manager>().map(|m| m.0.clone())
}

fn toast_error(title: &'static str, body: String, cx: &mut App) {
    if let Some(model) = cx.try_global::<Model>().cloned() {
        model.toasts.update(cx, |t, cx| {
            t.error(title, Some(body.into()), cx);
        });
    }
}

/// "Open" (`downloads:open`): only a finished download whose file still exists.
pub(crate) fn open_download(id: &str, cx: &mut App) {
    let Some(manager) = manager(cx) else { return };
    match manager.open_path(id) {
        Ok(path) => cx.open_with_system(&path),
        Err(e) => toast_error("Couldn't open the file", e.to_string(), cx),
    }
}

/// "Show in folder": the file, else its `.part`, else the folder it was going to.
pub(crate) fn show_in_folder(id: &str, cx: &mut App) {
    let Some(manager) = manager(cx) else { return };
    match manager.reveal_path(id) {
        Some(RevealTarget::Item(path)) => cx.reveal_path(&path),
        Some(RevealTarget::Folder(dir)) => cx.open_with_system(&dir),
        None => {}
    }
}

/// "Open folder": the download folder, made if it isn't there.
pub(crate) fn open_folder(cx: &mut App) {
    let dir: PathBuf = manager(cx).map(|m| m.download_dir()).unwrap_or_else(|| crate::downloads::download_dir(cx));
    let _ = std::fs::create_dir_all(&dir);
    cx.open_with_system(&dir);
}

/// The transfers screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// `content-download` (with the `fixtures` feature).
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::selftests();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The checks the self-test runs inside the signed-in app (`decibell --check content-download`).
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::checks();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

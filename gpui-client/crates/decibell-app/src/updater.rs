//! The updater (`decibell_release::update`) in the app: `update.ts` and its IPC glue.
//!
//! - **State**: every state the updater reaches lands in the model's `UpdateStore` (Settings →
//!   About, the user panel's chip), with the mode and this build's version.
//! - **When**: once, [`decibell_release::update::FIRST_CHECK_DELAY`] after the main window's
//!   first frame (`kickoffInitialCheck`), and on "Check now" / "Try again"; one GPUI one-shot
//!   timer, re-armed after a check only if `RECHECK_EVERY` says so (Electron: never); never while a
//!   check runs; never in a disabled (development) build.
//! - **Restart** (About's button, the chip): the window closes, the core shuts down, and this
//!   version starts again with `--apply-update`, which applies the staged update with a trial
//!   start (`boot::apply_staged_update`, `decibell_release::apply`). A staged update is also
//!   applied at the next start without Restart (electron-updater installs at quit).
//! - **Open release page**: the feed's release in the browser.
//!
//! The check runs on the updater's own thread; its states come back over a channel that wakes the
//! UI thread once per state and closes when the check ends. Nothing runs at rest.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use decibell_model::Model;
use decibell_model::stores::update::{UpdateMode, UpdateStatus, UpdateStore};
use decibell_release::apply::{Failed, Staged};
use decibell_release::update::{self, Mode, Settings, Status, Updater};
use decibell_settings::{SettingsHooks, UpdateHooks};
use futures::StreamExt as _;
use gpui::{AnyWindowHandle, App, Entity, Global, SharedString, Window};

/// Restart was asked for: after the event loop, start again to apply.
static RESTART: AtomicBool = AtomicBool::new(false);

pub fn update_dir(profile: &Path) -> PathBuf {
    profile.join("update")
}

struct Installed {
    updater: Updater,
    store: Entity<UpdateStore>,
}

impl Global for Installed {}

fn model_mode(mode: Mode) -> UpdateMode {
    match mode {
        Mode::SelfUpdate => UpdateMode::SelfUpdate,
        Mode::NotifyOnly => UpdateMode::NotifyOnly,
        Mode::Disabled => UpdateMode::Disabled,
    }
}

fn model_status(status: Status) -> UpdateStatus {
    match status {
        Status::Idle => UpdateStatus::Idle,
        Status::Checking => UpdateStatus::Checking,
        Status::NotAvailable { checked_at } => UpdateStatus::NotAvailable { checked_at },
        Status::Available { version } => UpdateStatus::Available { version: version.into() },
        Status::Downloading { pct, version } => UpdateStatus::Downloading { pct, version: version.into() },
        Status::Downloaded { version } => UpdateStatus::Downloaded { version: version.into() },
        Status::Error { message } => UpdateStatus::Error { message: message.into() },
    }
}

/// Wires the updater to the model and Settings, and schedules the first check after `window`'s
/// first frame. `trial`: this start supervises nothing but was started on trial by the previous
/// version, which waits for the first frame to be on screen.
pub fn install(model: &Model, profile: &Path, trial: Option<String>, window: AnyWindowHandle, cx: &mut App) {
    let updater = Updater::new(Settings::for_profile(profile));
    let mode = updater.mode();
    log::info!("[update] mode={mode:?}, version={}", crate::VERSION);
    let dir = update_dir(profile);
    // A version that didn't start: said once in About (as an error, the state Electron has).
    let initial = match Failed::read(&dir) {
        Some(failed) if !failed.shown => {
            Failed { shown: true, ..failed.clone() }.write(&dir);
            Status::Error {
                message: format!("version {} didn't start ({}), so {} was kept", failed.version, failed.reason, crate::VERSION),
            }
        }
        _ => Status::Idle,
    };
    let store = model.update.clone();
    let initial = model_status(initial);
    marker(&initial);
    store.update(cx, |u, cx| u.set(initial, model_mode(mode), crate::VERSION.into(), cx));
    cx.set_global(Installed { updater, store });
    SettingsHooks::set_update_hooks(Rc::new(Hooks), cx);
    cx.on_action(|_: &decibell_shell::RestartToUpdate, cx| quit_and_install(cx));

    let _ = window.update(cx, |_, window, _| {
        after_first_frame(window, 0, move |cx| {
            if let Some(pipe) = &trial {
                decibell_release::apply::report_started(pipe);
            }
            if mode != Mode::Disabled {
                arm(first_check_delay(), cx);
            }
        })
    });
}

/// Runs `then` once the window's first frame was presented (a next-frame callback after a
/// present, as `crate::video` and the perf marker do).
fn after_first_frame(window: &Window, attempt: u32, then: impl FnOnce(&mut App) + 'static) {
    window.on_next_frame(move |window, cx| {
        if window.frames_presented() == 0 && attempt < 8 {
            after_first_frame(window, attempt + 1, then);
            return;
        }
        then(cx);
    });
}

fn first_check_delay() -> Duration {
    // The release tests (packaging/tests) don't wait 5 s per run.
    #[cfg(feature = "dev")]
    if let Some(ms) = std::env::var("DECIBELL_TEST_UPDATE_DELAY_MS").ok().and_then(|v| v.parse().ok()) {
        return Duration::from_millis(ms);
    }
    update::FIRST_CHECK_DELAY
}

/// Development builds under the release tests: each state on stdout, for the test to follow.
fn marker(status: &UpdateStatus) {
    #[cfg(feature = "dev")]
    if std::env::var_os("DECIBELL_TEST_UPDATE_ORIGIN").is_some() {
        println!("decibell: update {status:?}");
    }
    #[cfg(not(feature = "dev"))]
    let _ = status;
}

/// The one-shot timer: a check `after` from now.
fn arm(after: Duration, cx: &mut App) {
    let timer = cx.background_executor().timer(after);
    cx.spawn(async move |cx| {
        timer.await;
        cx.update(check);
    })
    .detach();
}

/// "Check now": a check unless one runs; its states go to the store as they come.
fn check(cx: &mut App) {
    let Some(installed) = cx.try_global::<Installed>() else { return };
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<Status>();
    let started = installed.updater.check(Box::new(move |status| {
        let _ = tx.unbounded_send(status);
    }));
    if !started {
        return;
    }
    let store = installed.store.clone();
    cx.spawn(async move |cx| {
        while let Some(status) = rx.next().await {
            let status = model_status(status);
            marker(&status);
            cx.update(|cx| {
                store.update(cx, |u, cx| {
                    let (mode, version) = (u.mode, u.current_version.clone());
                    u.set(status, mode, version, cx);
                })
            });
        }
        // The check is over (the updater dropped its end).
        if let Some(every) = update::RECHECK_EVERY {
            cx.update(|cx| arm(every, cx));
        }
    })
    .detach();
}

/// `quitAndInstall()`: only a self-updating install with a download ready.
fn quit_and_install(cx: &mut App) {
    let Some(installed) = cx.try_global::<Installed>() else { return };
    let ready = matches!(installed.store.read(cx).status, UpdateStatus::Downloaded { .. });
    if installed.updater.mode() != Mode::SelfUpdate || !ready {
        return;
    }
    log::info!("[update] restarting to apply the update");
    RESTART.store(true, Ordering::Release);
    cx.quit();
}

struct Hooks;

impl UpdateHooks for Hooks {
    fn check(&self, cx: &mut App) {
        check(cx);
    }

    fn quit_and_install(&self, cx: &mut App) {
        quit_and_install(cx);
    }

    fn open_release_page(&self, cx: &mut App) {
        if let Some(installed) = cx.try_global::<Installed>() {
            let page: SharedString = installed.updater.release_page().into();
            cx.open_url(&page);
        }
    }
}

/// A staged, verified update to apply at this start (self-updating installs only).
pub fn staged_to_apply(profile: &Path) -> Option<Staged> {
    if update::mode() != Mode::SelfUpdate {
        return None;
    }
    let dir = update_dir(profile);
    let staged = Staged::read(&dir)?;
    if Failed::is(&dir, &staged.version) {
        Staged::clear(&dir);
        return None;
    }
    if !staged.verify(&dir) {
        log::warn!("[update] the staged {} no longer matches its checksum; dropped", staged.version);
        Staged::clear(&dir);
        return None;
    }
    Some(staged)
}

/// This start's own arguments, for the version that replaces it (its links, `--theme`, …).
pub fn trial_args() -> Vec<OsString> {
    std::env::args_os()
        .skip(1)
        .filter(|a| a != "--apply-update" && !a.to_string_lossy().starts_with("--update-trial="))
        .collect()
}

/// `--apply-update`: the instance that asked for the restart is closing; wait (up to 10 s) until it
/// lets go of the profile's lock.
pub fn wait_for_profile(profile: &Path) {
    let Ok(paths) = decibell_platform::instance::Paths::for_profile(profile) else { return };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let free = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&paths.lock)
            .map(|file| file.try_lock().is_ok())
            .unwrap_or(true);
        if free || Instant::now() > deadline {
            return;
        }
        // A short wait at a restart, not a timer at rest.
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// After the event loop: Restart was pressed, so this version starts again to apply the update.
pub fn relaunch_if_requested() {
    if !RESTART.load(Ordering::Acquire) {
        return;
    }
    // An AppImage restarts from its file (the running executable lives in its mount).
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()).or_else(|| std::env::current_exe().ok());
    let Some(exe) = exe else { return };
    match std::process::Command::new(&exe).arg(decibell_release::apply::APPLY_ARG).args(trial_args()).spawn() {
        Ok(_) => log::info!("[update] started {} to apply the update", exe.display()),
        Err(e) => log::error!("[update] couldn't start {} again: {e}", exe.display()),
    }
}

/// `--create-shortcut <path>`: the installer's Start menu shortcut with the app's AppUserModelID.
pub fn create_shortcut(link: &str) -> ExitCode {
    #[cfg(windows)]
    {
        let Ok(exe) = std::env::current_exe() else { return ExitCode::from(1) };
        match decibell_release::win::create_shortcut(Path::new(link), &exe, decibell_release::APP_USER_MODEL_ID, "Decibell") {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("decibell: --create-shortcut {link}: {e}");
                ExitCode::from(1)
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("decibell: --create-shortcut ({link}) is the Windows installer's");
        ExitCode::from(2)
    }
}

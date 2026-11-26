//! The core handle: the tokio runtime the core runs on, starting it once, stopping it.
//!
//! One core per process (`decibell_core` keeps its state in process globals and accepts
//! one event sink), so what is held here is process-wide too.
//!
//! Threads this creates, all named: `decibell-rt` (the runtime's 2 workers, and its
//! blocking pool, which tokio names the same). Everything else that runs is the core's
//! own (voice, video, hotkeys) or GPUI's.
//!
//! # D-Bus beside GPUI (Linux)
//!
//! The core is built with zbus on `async-io` (`default-features = false, features =
//! ["zbus-async-io"]` in this crate's manifest), the runtime GPUI's own D-Bus users
//! (`ashpd`, `oo7`) use. With the core's default (`zbus-tokio`, the Electron addon's) cargo
//! would unify both features into one zbus that runs on tokio, and every D-Bus call GPUI
//! makes (portal settings, the file chooser, opening URLs, the keychain, power and sleep
//! events) would panic outside a tokio context. Nothing here has to enter the runtime on
//! GPUI's threads: `cargo tree -p decibell-model -e features -i zbus` must show no `tokio`
//! feature.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use decibell_core::api::lifecycle::{self, InitOptions};
use gpui::{App, Task};

use crate::local_key::LocalKeyProvider;
use crate::pump::ModelSink;

/// What [`start`] needs from the app.
pub struct CoreOptions {
    /// `None`: [`default_user_data_dir`].
    pub user_data_dir: Option<PathBuf>,
    /// The app's version (the device list shows it).
    pub app_version: String,
    /// Where the E2EE at-rest key comes from (`KeychainKey` in the app).
    pub local_key: Box<dyn LocalKeyProvider>,
}

struct Running {
    runtime: tokio::runtime::Runtime,
    sink: Arc<ModelSink>,
    user_data_dir: PathBuf,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
static STARTED: AtomicBool = AtomicBool::new(false);

fn build_runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("decibell-rt")
        .enable_all()
        .build()
        .context("building the core's runtime")
}

/// A development or test build: `local-central` (the app's `dev-local`, the headless tool) or a
/// debug build. Such a build never uses the shipped client's profile ([`default_user_data_dir`],
/// [`check_profile`]).
pub const DEVELOPMENT_BUILD: bool = cfg!(any(feature = "local-central", debug_assertions));

/// Where this client keeps `config.json`, the E2EE key store, `cert_pins.json` and its
/// own `ui.json`: `$DECIBELL_USER_DATA_DIR`, else [`shipped_profile_dir`] in a shipped build and
/// [`development_profile_dir`] in a development one. Never the Electron client's
/// (`decibell-electron-client`): the two keep separate device ids, pins and saved sign-ins.
pub fn default_user_data_dir() -> Result<PathBuf> {
    resolve_user_data_dir(&|name| std::env::var_os(name), DEVELOPMENT_BUILD)
}

/// The shipped client's profile: `decibell` under the platform's config directory
/// (`$XDG_CONFIG_HOME` or `~/.config` on Linux, `%APPDATA%` on Windows, `~/Library/Application
/// Support` on macOS).
pub fn shipped_profile_dir() -> Result<PathBuf> {
    shipped_profile(&|name| std::env::var_os(name))
}

/// A development build's default profile, kept out of the shipped one (owner, 2026-11-26: a test
/// run had created `~/.config/decibell`): `decibell-dev/gpui-default` under the platform's cache
/// directory (`$XDG_CACHE_HOME` or `~/.cache` on Linux, `%LOCALAPPDATA%` on Windows,
/// `~/Library/Caches` on macOS), beside the local stack's own `decibell-dev` files.
pub fn development_profile_dir() -> Result<PathBuf> {
    development_profile(&|name| std::env::var_os(name))
}

type EnvVar<'a> = &'a dyn Fn(&str) -> Option<std::ffi::OsString>;

fn resolve_user_data_dir(var: EnvVar, development: bool) -> Result<PathBuf> {
    if let Some(dir) = var("DECIBELL_USER_DATA_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    if development { development_profile(var) } else { shipped_profile(var) }
}

fn env_path(var: EnvVar, name: &str) -> Option<PathBuf> {
    var(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn shipped_profile(var: EnvVar) -> Result<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        env_path(var, "APPDATA")
    } else if cfg!(target_os = "macos") {
        env_path(var, "HOME").map(|home| home.join("Library/Application Support"))
    } else {
        env_path(var, "XDG_CONFIG_HOME").or_else(|| env_path(var, "HOME").map(|home| home.join(".config")))
    };
    Ok(base
        .context("no home directory to keep the configuration in")?
        .join("decibell"))
}

fn development_profile(var: EnvVar) -> Result<PathBuf> {
    let base = if cfg!(target_os = "windows") {
        env_path(var, "LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        env_path(var, "HOME").map(|home| home.join("Library/Caches"))
    } else {
        env_path(var, "XDG_CACHE_HOME").or_else(|| env_path(var, "HOME").map(|home| home.join(".cache")))
    };
    Ok(base
        .context("no home directory to keep the development profile in")?
        .join("decibell-dev")
        .join("gpui-default"))
}

/// Refuses a development build the shipped client's profile, even when `DECIBELL_USER_DATA_DIR`
/// names it: whatever a test or a development run writes there (a device id, a saved sign-in to
/// the local stack, pins) would be the installed client's next start. Shipped builds take any.
pub fn check_profile(dir: &std::path::Path) -> Result<()> {
    check_profile_with(dir, &|name| std::env::var_os(name), DEVELOPMENT_BUILD)
}

/// [`check_profile`] whatever the build: for tools that are never shipped (`decibell-headless`).
pub fn refuse_shipped_profile(dir: &std::path::Path) -> Result<()> {
    check_profile_with(dir, &|name| std::env::var_os(name), true)
}

fn check_profile_with(dir: &std::path::Path, var: EnvVar, development: bool) -> Result<()> {
    if !development {
        return Ok(());
    }
    let shipped = shipped_profile(var)?;
    let same = |a: &std::path::Path, b: &std::path::Path| {
        a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
    };
    if same(dir, &shipped) {
        bail!(
            "a development build never uses the shipped client's profile ({}): leave DECIBELL_USER_DATA_DIR unset (the default is {}) or point it elsewhere",
            shipped.display(),
            development_profile(var).map(|d| d.display().to_string()).unwrap_or_default()
        );
    }
    Ok(())
}

/// Start the core: the user data directory, the at-rest key, the runtime, then
/// `lifecycle::init` with `sink`. Once per process; resolves when calls can be made.
/// The UI thread never blocks: the keychain read is the only wait, and it is a task.
pub fn start(options: CoreOptions, sink: Arc<ModelSink>, cx: &mut App) -> Task<Result<()>> {
    if STARTED.swap(true, Ordering::SeqCst) {
        return Task::ready(Err(anyhow::anyhow!("the core was already started in this process")));
    }
    let user_data_dir = match options
        .user_data_dir
        .map(Ok)
        .unwrap_or_else(default_user_data_dir)
        .and_then(|dir| {
            check_profile(&dir)?;
            std::fs::create_dir_all(&dir)
                .with_context(|| format!("creating {}", dir.display()))?;
            Ok(dir)
        }) {
        Ok(dir) => dir,
        Err(e) => {
            STARTED.store(false, Ordering::SeqCst);
            return Task::ready(Err(e));
        }
    };
    let runtime = match build_runtime() {
        Ok(runtime) => runtime,
        Err(e) => {
            STARTED.store(false, Ordering::SeqCst);
            return Task::ready(Err(e));
        }
    };
    let key = options.local_key.load(&user_data_dir, cx);
    let app_version = options.app_version;
    cx.spawn(async move |_| {
        let key = key.await;
        let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
        if running.is_some() {
            bail!("the core is already running");
        }
        lifecycle::init(
            InitOptions {
                user_data_dir: user_data_dir.to_string_lossy().into_owned(),
                app_version,
                e2ee_local_key: key.key_b64,
                local_key_protected: Some(key.protected),
            },
            sink.clone(),
            runtime.handle().clone(),
        );
        *running = Some(Running {
            runtime,
            sink,
            user_data_dir,
        });
        Ok(())
    })
}

/// Whether `decibell_core::api` can be called (the core panics on a call before `init`).
pub fn is_running() -> bool {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// The directory the running core uses.
pub fn user_data_dir() -> Option<PathBuf> {
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|r| r.user_data_dir.clone())
}

/// Run a future on the core's runtime without waiting for it (a settings save on quit:
/// it must not depend on the UI thread polling it). Dropped when the core isn't running.
pub fn spawn_detached(future: impl std::future::Future<Output = ()> + Send + 'static) {
    if let Some(running) = &*RUNNING.lock().unwrap_or_else(|e| e.into_inner()) {
        running.runtime.spawn(future);
    }
}

/// Route encoded stream frames to `sink` (the video crate's decoder queues); `None`
/// drops them again. See [`crate::pump::StreamFrameSink`].
pub fn set_stream_frame_sink(sink: Option<Box<dyn crate::pump::StreamFrameSink>>) {
    if let Some(running) = &*RUNNING.lock().unwrap_or_else(|e| e.into_inner()) {
        running.sink.set_stream_frame_sink(sink);
    }
}

/// Stop the core and its runtime, waiting at most `limit` in all. Call it on the main
/// thread **after** `Application::run` returned (it blocks; the event loop is gone by
/// then): `lifecycle::shutdown` drops the engines and connections and says goodbye to a
/// call's peer, then the runtime's threads get what is left of `limit` to finish.
/// Idempotent; returns whether the core's own shutdown finished in time.
pub fn shutdown(limit: Duration) -> bool {
    let Some(running) = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return true;
    };
    running.sink.queue().close();
    let started = std::time::Instant::now();
    let clean = running
        .runtime
        .block_on(async { tokio::time::timeout(limit / 2, lifecycle::shutdown()).await })
        .is_ok_and(|r| r.is_ok());
    running
        .runtime
        .shutdown_timeout(limit.saturating_sub(started.elapsed()));
    clean
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> + 'a {
        move |name| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.into())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn development_builds_keep_out_of_the_shipped_profile() {
        let home = env(&[("HOME", "/home/a")]);
        let shipped = resolve_user_data_dir(&home, false).unwrap();
        assert_eq!(shipped, PathBuf::from("/home/a/.config/decibell"));
        let dev = resolve_user_data_dir(&home, true).unwrap();
        assert_eq!(dev, PathBuf::from("/home/a/.cache/decibell-dev/gpui-default"));
        let xdg = env(&[("HOME", "/home/a"), ("XDG_CONFIG_HOME", "/c"), ("XDG_CACHE_HOME", "/k")]);
        assert_eq!(resolve_user_data_dir(&xdg, false).unwrap(), PathBuf::from("/c/decibell"));
        assert_eq!(resolve_user_data_dir(&xdg, true).unwrap(), PathBuf::from("/k/decibell-dev/gpui-default"));
        // An explicit profile wins in both, and a development build refuses the shipped one.
        let explicit = env(&[("HOME", "/home/a"), ("DECIBELL_USER_DATA_DIR", "/tmp/p")]);
        assert_eq!(resolve_user_data_dir(&explicit, true).unwrap(), PathBuf::from("/tmp/p"));
        assert!(check_profile_with(&shipped, &home, true).is_err());
        assert!(check_profile_with(&shipped, &home, false).is_ok());
        assert!(check_profile_with(&dev, &home, true).is_ok());
        assert!(check_profile_with(std::path::Path::new("/tmp/p"), &home, true).is_ok());
    }

    // Tests run debug builds: development builds.
    const _: () = assert!(DEVELOPMENT_BUILD);

    #[test]
    fn shutdown_without_a_core_is_a_no_op() {
        // No test in this binary starts the core (it is process-global).
        assert!(!is_running());
        assert!(shutdown(Duration::from_millis(10)));
        assert_eq!(user_data_dir(), None);
    }
}

//! What a released build of the Decibell client does on its own, apart from the UI:
//!
//! - [`VERSION`]: the one version (`electron-client/package.json`, read at build time).
//! - [`install`]: the install's directory, its `resources/`, the package kind stamped into it,
//!   and whether this is a packaged build (Electron's `app.isPackaged`).
//! - [`update`] / [`feed`] / [`apply`]: the updater (`electron/main/update.ts`).
//! - [`crash`]: crash reports (`electron/main/sentry.ts`, `lib/sentry.ts`).
//! - [`http`]: the blocking HTTPS client both use, limited to the hosts each names.
//!
//! GPUI-free: `decibell-app` drives it (its `updater` module feeds the model's `UpdateStore`
//! and Settings → About). Nothing here runs at rest: the updater's and the crash reporter's
//! threads exist only while they work, and no timer lives here (the app's one-shot timer starts a
//! check).

pub mod apply;
pub mod crash;
pub mod feed;
pub mod http;
pub mod install;
pub mod update;
pub mod version;
#[cfg(windows)]
pub mod win;

#[cfg(all(feature = "ship", feature = "test-hooks"))]
compile_error!("decibell-release: `ship` and `test-hooks` exclude each other (a shipped build has no test hooks)");

/// The client's version: `electron-client/package.json`'s, the repository's one version site.
pub const VERSION: &str = env!("DECIBELL_VERSION");

/// The Windows AppUserModelID: the process sets it at start and the installer's Start menu
/// shortcut carries it, so the taskbar groups and pins them together. Not Electron's
/// (`com.decibell.app`) while both clients ship, so the two never share a taskbar button
/// (gpui-client/PACKAGING.md, "Decisions").
pub const APP_USER_MODEL_ID: &str = "com.decibell.native";

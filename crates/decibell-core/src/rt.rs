//! The tokio runtime the core runs on (docs/superpowers/specs/2026-11-07-qt-
//! native-client-design.md §6 item 4). The core never builds a runtime: the
//! host hands one over at start-up ([`init`], called by
//! [`crate::api::lifecycle::init`]) and the core spawns through it, so calls
//! made from threads outside any runtime (a native client's UI thread, the
//! core's own media / hotkey threads) work.
//!
//! The rule, for every spawn and for [`run`]: a caller already inside a tokio
//! runtime context keeps using that runtime (exactly what a bare
//! `tokio::spawn` did: in the Electron addon every command already runs on
//! napi's runtime, which is the one it hands to `init`, so nothing moves);
//! a caller outside any runtime goes to the runtime given to [`init`]. Unit
//! tests under `#[tokio::test]` therefore keep their own (paused-clock)
//! runtime.
//!
//! `api`'s async functions are awaited by clients on executors that aren't
//! tokio's; tokio timers and sockets panic outside a runtime context. So each
//! of them runs its body through [`run`]: inline when the caller is inside a
//! runtime, else as a task on the stored runtime whose result it awaits (from
//! any executor: a `JoinHandle` needs no runtime to be polled).

use std::future::Future;
use std::sync::RwLock;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::api::CoreError;

static HANDLE: RwLock<Option<Handle>> = RwLock::new(None);

/// The runtime for calls from outside one. A later call replaces it (an
/// addon reloaded into a fresh runtime).
pub fn init(handle: Handle) {
    *HANDLE.write().unwrap_or_else(|e| e.into_inner()) = Some(handle);
}

fn stored() -> Handle {
    HANDLE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .expect("decibell-core: called outside a tokio runtime before rt::init (api::lifecycle::init)")
}

/// The runtime a call from this thread uses: the current one inside a runtime
/// context, else the one given to [`init`]. Panics outside both, like
/// `Handle::current`.
pub fn handle() -> Handle {
    Handle::try_current().unwrap_or_else(|_| stored())
}

/// `tokio::spawn` that also works outside a runtime context (see the module
/// doc for which runtime it lands on).
#[track_caller]
pub fn spawn<F>(fut: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match Handle::try_current() {
        Ok(_) => tokio::spawn(fut),
        Err(_) => stored().spawn(fut),
    }
}

/// `tokio::task::spawn_blocking` that also works outside a runtime context.
#[track_caller]
pub fn spawn_blocking<F, R>(f: F) -> JoinHandle<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match Handle::try_current() {
        Ok(_) => tokio::task::spawn_blocking(f),
        Err(_) => stored().spawn_blocking(f),
    }
}

/// Run an `api` body on the runtime: inline when the caller is inside a
/// runtime context, else spawned on the stored runtime and awaited. A panic
/// in the body resumes in the caller (as it would have inline); a body
/// cancelled because the runtime shut down answers an error.
pub async fn run<T, F>(body: F) -> Result<T, CoreError>
where
    F: Future<Output = Result<T, CoreError>> + Send + 'static,
    T: Send + 'static,
{
    if Handle::try_current().is_ok() {
        return body.await;
    }
    match stored().spawn(body).await {
        Ok(out) => out,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(_) => Err(CoreError::other("The runtime is shutting down")),
    }
}

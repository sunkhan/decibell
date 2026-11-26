//! zbus's runtime is a build choice (Cargo.toml): `zbus-tokio`, the default
//! and the Electron addon's, or `zbus-async-io`, for a host whose own tree
//! uses zbus on async-io (the GPUI client: its portal, keychain and logind
//! calls run on GPUI's threads, where tokio-mode zbus panics with "there is no
//! reactor running"). These run the core's D-Bus patterns in whichever mode the
//! test binary was built with, against the session bus with a harmless call
//! (`org.freedesktop.DBus.ListNames`; no portal, nothing shown on the
//! desktop), and skip without a session bus:
//!   - the blocking API from a plain thread (system_idle.rs; capture_pipewire.rs
//!     on a `spawn_blocking` thread),
//!   - the async API inside a current-thread runtime on its own thread, under a
//!     tokio timeout (hotkeys/portal.rs),
//!   - a blocking connection's async half driven from a runtime task
//!     (capture_pipewire.rs' Session.Closed watch),
//!   - async-io only: the async API awaited on a thread with no tokio runtime
//!     at all (what a GPUI executor does).
//!
//! `zbus_threads_and_idle_wakeups` prints the threads a connection brings and
//! their context switches while idle (`cargo test --lib zbus_threads --
//! --nocapture --test-threads=1`).

use std::time::Duration;

const DEST: &str = "org.freedesktop.DBus";
const PATH: &str = "/org/freedesktop/DBus";

fn has_bus_daemon(names: &[String]) -> bool {
    names.iter().any(|n| n == DEST)
}

fn blocking_session() -> Option<zbus::blocking::Connection> {
    match zbus::blocking::Connection::session() {
        Ok(c) => Some(c),
        Err(e) => {
            println!("no session bus ({e}): skipped");
            None
        }
    }
}

fn list_names_blocking(conn: &zbus::blocking::Connection) -> Vec<String> {
    conn.call_method(Some(DEST), PATH, Some(DEST), "ListNames", &())
        .expect("ListNames")
        .body()
        .deserialize()
        .expect("ListNames reply")
}

async fn list_names(conn: &zbus::Connection) -> Vec<String> {
    conn.call_method(Some(DEST), PATH, Some(DEST), "ListNames", &())
        .await
        .expect("ListNames")
        .body()
        .deserialize()
        .expect("ListNames reply")
}

#[test]
fn blocking_api_from_a_plain_thread() {
    std::thread::spawn(|| {
        assert!(tokio::runtime::Handle::try_current().is_err(), "a plain thread");
        let Some(conn) = blocking_session() else { return };
        assert!(has_bus_daemon(&list_names_blocking(&conn)));
    })
    .join()
    .unwrap();
}

#[test]
fn async_api_in_a_current_thread_runtime_with_a_tokio_timeout() {
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let conn = match zbus::Connection::session().await {
                Ok(c) => c,
                Err(e) => return println!("no session bus ({e}): skipped"),
            };
            let names = tokio::time::timeout(Duration::from_secs(10), list_names(&conn))
                .await
                .expect("ListNames in time");
            assert!(has_bus_daemon(&names));
        });
    })
    .join()
    .unwrap();
}

#[test]
fn blocking_connection_driven_from_a_runtime_task() {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    rt.block_on(async {
        let Some(conn) = tokio::task::spawn_blocking(blocking_session).await.unwrap() else { return };
        let inner = conn.inner().clone();
        let names = tokio::spawn(async move { list_names(&inner).await }).await.unwrap();
        assert!(has_bus_daemon(&names));
        drop(conn);
    });
}

/// GPUI's executors aren't tokio: with async-io, zbus futures complete on any
/// thread (the connection's executor thread and async-io's reactor drive them).
#[cfg(not(feature = "zbus-tokio"))]
#[test]
fn async_api_on_a_thread_without_tokio() {
    std::thread::spawn(|| {
        assert!(tokio::runtime::Handle::try_current().is_err(), "no tokio here");
        park_block_on(async {
            let conn = match zbus::Connection::session().await {
                Ok(c) => c,
                Err(e) => return println!("no session bus ({e}): skipped"),
            };
            assert!(has_bus_daemon(&list_names(&conn).await));
        });
    })
    .join()
    .unwrap();
}

/// The smallest executor: poll, park until woken.
#[cfg(not(feature = "zbus-tokio"))]
fn park_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park();
    }
}

/// Report only: the threads a blocking session connection brings in this
/// build's mode, and how often they wake while the connection is idle.
#[test]
fn zbus_threads_and_idle_wakeups() {
    fn threads() -> Vec<(u32, String)> {
        let mut out = Vec::new();
        for e in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            let tid: u32 = e.file_name().to_string_lossy().parse().unwrap();
            if let Ok(comm) = std::fs::read_to_string(e.path().join("comm")) {
                out.push((tid, comm.trim().to_string()));
            }
        }
        out
    }
    /// Context switches so far; None once the thread has exited.
    fn switches(tid: u32) -> Option<u64> {
        let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).ok()?;
        Some(
            status
                .lines()
                .filter(|l| l.starts_with("voluntary_ctxt_switches") || l.starts_with("nonvoluntary_ctxt_switches"))
                .filter_map(|l| l.split_whitespace().last()?.parse::<u64>().ok())
                .sum(),
        )
    }
    let mode = if cfg!(feature = "zbus-tokio") { "tokio" } else { "async-io" };
    let before: Vec<u32> = threads().into_iter().map(|(t, _)| t).collect();
    let handle = std::thread::spawn(|| {
        let conn = blocking_session()?;
        assert!(has_bus_daemon(&list_names_blocking(&conn)));
        Some(conn)
    });
    let Some(conn) = handle.join().unwrap() else { return };
    std::thread::sleep(Duration::from_millis(200));
    let new: Vec<(u32, String)> = threads().into_iter().filter(|(t, _)| !before.contains(t)).collect();
    let start: Vec<Option<u64>> = new.iter().map(|(t, _)| switches(*t)).collect();
    std::thread::sleep(Duration::from_secs(3));
    // Per name: (threads, context switches over 3 s idle, threads that exited meanwhile).
    let mut by_name: std::collections::BTreeMap<&str, (usize, u64, usize)> = Default::default();
    for ((t, name), s) in new.iter().zip(&start) {
        let e = by_name.entry(name.as_str()).or_default();
        e.0 += 1;
        match (s, switches(*t)) {
            (Some(s), Some(now)) => e.1 += now - s,
            _ => e.2 += 1,
        }
    }
    println!(
        "[zbus {mode}] threads the connection brought (name: (count, context switches in 3 s idle, exited)): {by_name:?}"
    );
    // What one call costs those threads (an auto-away poll is two or three).
    let before_call: Vec<Option<u64>> = new.iter().map(|(t, _)| switches(*t)).collect();
    assert!(has_bus_daemon(&list_names_blocking(&conn)));
    std::thread::sleep(Duration::from_secs(1));
    let per_call: u64 = new
        .iter()
        .zip(&before_call)
        .filter_map(|((t, _), s)| Some(switches(*t)? - (*s)?))
        .sum();
    println!("[zbus {mode}] context switches on those threads for one ListNames (1 s window): {per_call}");
    drop(conn);
    std::thread::sleep(Duration::from_millis(1500));
    let alive: Vec<String> = threads()
        .into_iter()
        .filter(|(t, _)| new.iter().any(|(n, _)| n == t))
        .map(|(_, n)| n)
        .collect();
    println!("[zbus {mode}] still running 1.5 s after the connection was dropped: {alive:?}");
}

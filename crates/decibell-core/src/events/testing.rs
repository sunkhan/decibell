//! Test-only: a recording [`EventSink`] so unit tests can see what the core
//! emits.
//!
//! The sink is process-wide (`install_sink` is a `OnceLock`) and cargo runs
//! a binary's tests on parallel threads, so the recorder keeps one log for
//! the whole process, each entry stamped with the thread that emitted it:
//! - [`take_own_events`] returns (and removes) what the calling thread
//!   emitted. libtest runs each test on its own thread and thread ids are
//!   never reused, so this is exactly the test's own synchronous emits (a
//!   `#[tokio::test]` on its default current-thread runtime included).
//! - [`take_events`] returns (and removes) the matching events of any
//!   thread, for emits from threads a test spawned (listeners, workers):
//!   match on something the test owns (a uid, a server id) when other tests
//!   could emit the same kind of event at the same time.
//!
//! The log keeps the newest [`LOG_CAP`] events; nothing reads it unless a
//! test asks.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;

use super::{EventSink, StreamFrame, StreamThumbnail};

/// Events kept before the oldest are dropped.
pub const LOG_CAP: usize = 10_000;

struct Recorded {
    thread: ThreadId,
    name: &'static str,
    payload: serde_json::Value,
}

#[derive(Default)]
struct RecordingSink {
    log: Mutex<VecDeque<Recorded>>,
}

impl EventSink for RecordingSink {
    fn event(&self, name: &'static str, payload: serde_json::Value) {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        if log.len() == LOG_CAP {
            log.pop_front();
        }
        log.push_back(Recorded { thread: std::thread::current().id(), name, payload });
    }
    fn stream_frame(&self, _frame: StreamFrame) {}
    fn stream_thumbnail(&self, _thumb: StreamThumbnail) {}
}

fn recorder() -> &'static Arc<RecordingSink> {
    static RECORDER: OnceLock<Arc<RecordingSink>> = OnceLock::new();
    RECORDER.get_or_init(|| Arc::new(RecordingSink::default()))
}

/// Make the recorder the process's event sink. Idempotent; call it at the
/// start of every test that reads events. Panics if another sink was
/// installed first (nothing in the core's own tests installs one).
pub fn install_recording_sink() {
    let rec = recorder();
    let _ = super::install_sink(rec.clone());
    let installed = super::sink().expect("a sink is installed");
    assert!(
        std::ptr::eq(Arc::as_ptr(installed) as *const (), Arc::as_ptr(rec) as *const ()),
        "another event sink was installed before the recording sink"
    );
}

fn take_where(mut keep: impl FnMut(&Recorded) -> bool) -> Vec<(&'static str, serde_json::Value)> {
    let mut log = recorder().log.lock().unwrap_or_else(|e| e.into_inner());
    let mut taken = Vec::new();
    let mut rest = VecDeque::with_capacity(log.len());
    for r in log.drain(..) {
        if keep(&r) {
            taken.push((r.name, r.payload));
        } else {
            rest.push_back(r);
        }
    }
    *log = rest;
    taken
}

/// Remove and return, in emit order, every event the calling thread emitted.
pub fn take_own_events() -> Vec<(&'static str, serde_json::Value)> {
    let me = std::thread::current().id();
    take_where(|r| r.thread == me)
}

/// Remove and return, in emit order, every event (from any thread) that
/// `pred` matches.
pub fn take_events(
    mut pred: impl FnMut(&'static str, &serde_json::Value) -> bool,
) -> Vec<(&'static str, serde_json::Value)> {
    take_where(|r| pred(r.name, &r.payload))
}

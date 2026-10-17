//! What the UDP receive loops do with a receive error (review A2).
//!
//! No receive error ends a loop. Each used to `break` on anything outside a
//! short allowlist, which left voice or video dead while TCP and the UI stayed
//! "connected". Windows reports a datagram bigger than the buffer as
//! WSAEMSGSIZE (10040; the datagram is discarded), so one oversized datagram
//! from the relay or from any host that can reach a P2P socket killed the
//! loop. On Linux an ICMP unreachable on a `connect()`ed socket (relay
//! restart, Wi-Fi roam) reads back once as ECONNREFUSED, and a roam can
//! report ENETUNREACH / EHOSTUNREACH / ENETDOWN for a while. None of these
//! means the socket is gone, and a loop can't tell one that is from one
//! having a bad second. So a loop receives until its stop flag, logs errors
//! at most every `LOG_EVERY`, and backs off once errors come back to back,
//! so a socket that fails instantly can't spin a core.

use std::io;
use std::time::{Duration, Instant};

/// Receive buffer for every media datagram. The largest we send is 1245 B
/// (1270 sealed), so a datagram that fills this buffer is junk, and datagrams
/// up to this size no longer trip WSAEMSGSIZE on Windows.
pub const RECV_BUF_SIZE: usize = 2048;

/// Errors in a row that cost nothing: a junk datagram or an ICMP report is
/// one error between good reads.
const FREE_ERRORS: u32 = 8;
/// Longest pause between receives while every receive fails. Bounds how late
/// a loop sees its stop flag.
const MAX_BACKOFF: Duration = Duration::from_millis(64);
const LOG_EVERY: Duration = Duration::from_secs(5);

/// The read timeout elapsing (or a signal): the loop's normal idle poll, not
/// a fault. ERROR_IO_PENDING (997) was on the old Windows allowlist next to
/// the timeouts, so it stays out of the back-off.
pub fn is_idle(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    ) || (cfg!(windows) && e.raw_os_error() == Some(997))
}

/// A read of `n` bytes filled the whole buffer: on Linux the datagram was
/// cut to fit (Windows reports WSAEMSGSIZE instead). Nothing we send is that
/// big, so the loops drop it rather than parse a truncated datagram.
pub fn is_oversized(n: usize) -> bool {
    n >= RECV_BUF_SIZE
}

/// Receive-error bookkeeping for one loop.
pub struct RecvErrors {
    label: &'static str,
    /// Errors since the last datagram or idle timeout.
    streak: u32,
    last_log: Option<Instant>,
    /// Errors since `last_log` that weren't logged.
    unlogged: u64,
}

impl RecvErrors {
    pub fn new(label: &'static str) -> Self {
        Self { label, streak: 0, last_log: None, unlogged: 0 }
    }

    /// A datagram arrived: the socket works, the streak ends.
    pub fn ok(&mut self) {
        self.streak = 0;
    }

    /// Account for one receive error and sleep its back-off.
    pub fn handle(&mut self, e: &io::Error) {
        let pause = self.on_error(e, Instant::now());
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
    }

    /// Account for one receive error at `now`; returns the pause before the
    /// next receive (zero for an idle timeout or a short streak).
    fn on_error(&mut self, e: &io::Error, now: Instant) -> Duration {
        if is_idle(e) {
            self.streak = 0;
            return Duration::ZERO;
        }
        self.streak = self.streak.saturating_add(1);
        if let Some(unlogged) = self.log_due(now) {
            if unlogged > 0 {
                log::warn!(
                    "[{}] receive error: {} ({} more in the last {} s; still receiving)",
                    self.label,
                    e,
                    unlogged,
                    LOG_EVERY.as_secs()
                );
            } else {
                log::warn!("[{}] receive error: {} (still receiving)", self.label, e);
            }
        }
        backoff(self.streak)
    }

    /// Whether this error gets a log line: `Some(errors not logged since the
    /// previous line)` at most once per `LOG_EVERY`.
    fn log_due(&mut self, now: Instant) -> Option<u64> {
        match self.last_log {
            Some(t) if now.duration_since(t) < LOG_EVERY => {
                self.unlogged += 1;
                None
            }
            _ => {
                self.last_log = Some(now);
                Some(std::mem::take(&mut self.unlogged))
            }
        }
    }
}

/// Pause after the `streak`-th error in a row: nothing for the first
/// `FREE_ERRORS`, then 1, 2, 4 … ms up to `MAX_BACKOFF`.
fn backoff(streak: u32) -> Duration {
    if streak <= FREE_ERRORS {
        return Duration::ZERO;
    }
    let shift = (streak - FREE_ERRORS - 1).min(16);
    Duration::from_millis(1u64 << shift).min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(kind: io::ErrorKind) -> io::Error {
        io::Error::from(kind)
    }

    #[test]
    fn timeouts_are_idle_everything_else_is_an_error() {
        assert!(is_idle(&err(io::ErrorKind::WouldBlock)));
        assert!(is_idle(&err(io::ErrorKind::TimedOut)));
        assert!(is_idle(&err(io::ErrorKind::Interrupted)));
        // ICMP unreachable on a connected socket (Linux), a reset (Windows),
        // WSAEMSGSIZE and a roam: transient errors, never idle.
        assert!(!is_idle(&err(io::ErrorKind::ConnectionRefused)));
        assert!(!is_idle(&err(io::ErrorKind::ConnectionReset)));
        assert!(!is_idle(&io::Error::from_raw_os_error(10040)));
        assert!(!is_idle(&err(io::ErrorKind::NetworkUnreachable)));
    }

    #[test]
    fn errors_back_off_only_when_back_to_back() {
        let mut r = RecvErrors::new("test");
        let t = Instant::now();
        let refused = err(io::ErrorKind::ConnectionRefused);
        for _ in 0..FREE_ERRORS {
            assert_eq!(r.on_error(&refused, t), Duration::ZERO);
        }
        let pauses: Vec<u64> = (0..10).map(|_| r.on_error(&refused, t).as_millis() as u64).collect();
        assert_eq!(pauses, vec![1, 2, 4, 8, 16, 32, 64, 64, 64, 64]);
        // A datagram ends the streak, and so does an idle timeout.
        r.ok();
        assert_eq!(r.on_error(&refused, t), Duration::ZERO);
        for _ in 0..20 {
            r.on_error(&refused, t);
        }
        assert_eq!(r.on_error(&err(io::ErrorKind::WouldBlock), t), Duration::ZERO);
        assert_eq!(r.on_error(&refused, t), Duration::ZERO);
        // The streak counter can't overflow into a zero pause.
        r.streak = u32::MAX - 1;
        assert_eq!(r.on_error(&refused, t), MAX_BACKOFF);
        assert_eq!(r.on_error(&refused, t), MAX_BACKOFF);
    }

    #[test]
    fn a_hot_error_loop_sleeps() {
        // An error source that fails instantly, driven through `handle` like
        // a receive loop would: 40 errors in a row must cost real time, not
        // a spinning core.
        let mut r = RecvErrors::new("test");
        let refused = err(io::ErrorKind::ConnectionRefused);
        let start = Instant::now();
        for _ in 0..FREE_ERRORS + 8 {
            r.handle(&refused);
        }
        // 1 + 2 + 4 + … + 64 + 64 ms
        assert!(start.elapsed() >= Duration::from_millis(190), "{:?}", start.elapsed());
    }

    #[test]
    fn logging_is_rate_limited() {
        let mut r = RecvErrors::new("test");
        let t0 = Instant::now();
        assert_eq!(r.log_due(t0), Some(0));
        assert_eq!(r.log_due(t0 + Duration::from_secs(1)), None);
        assert_eq!(r.log_due(t0 + Duration::from_secs(4)), None);
        // The next line reports what was held back, then the count restarts.
        assert_eq!(r.log_due(t0 + LOG_EVERY), Some(2));
        assert_eq!(r.log_due(t0 + LOG_EVERY + Duration::from_secs(1)), None);
        assert_eq!(r.log_due(t0 + LOG_EVERY * 3), Some(1));
        // Idle timeouts are never logged or counted.
        let before = r.unlogged;
        r.on_error(&err(io::ErrorKind::TimedOut), t0 + LOG_EVERY * 3);
        assert_eq!(r.unlogged, before);
    }

    #[test]
    fn oversized_reads_fill_the_buffer() {
        assert!(!is_oversized(1270));
        assert!(!is_oversized(RECV_BUF_SIZE - 1));
        assert!(is_oversized(RECV_BUF_SIZE));
    }
}

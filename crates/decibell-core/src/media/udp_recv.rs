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
//!
//! A loop sleeps in [`wait`] until its socket has a datagram, its [`Waker`]
//! fires (stop, or new work for it), or its next timed job is due: no read
//! timeout polls a silent socket. The wait is `poll` on Linux and macOS and
//! `WSAPoll` on Windows, over the media socket and a loopback UDP socket the
//! waker sends one byte to (a socket, not a pipe: `WSAPoll` takes sockets
//! only, so all three platforms share it).

use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use super::media_socket::MediaSocket;

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

/// A read past readiness that blocks anyway (a datagram the kernel drops
/// between `poll` and `recv`) returns after this, so the loop sees its stop
/// flag. Never reached in normal operation: the loops only read a socket
/// `poll` called readable.
pub const READ_GUARD: Duration = Duration::from_millis(500);

/// Wakes a receive loop blocked in [`wait`]: a loopback UDP socket that sends
/// itself one byte. Shared (`Arc`) by the loop and whoever stops it or hands
/// it work; waking a loop that isn't waiting makes its next wait return at
/// once (the byte stays queued until then), so a wake is never lost.
pub struct Waker {
    sock: UdpSocket,
    addr: SocketAddr,
}

impl Waker {
    pub fn new() -> io::Result<Waker> {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        sock.set_nonblocking(true)?;
        let addr = sock.local_addr()?;
        Ok(Waker { sock, addr })
    }

    pub fn wake(&self) {
        // A full queue already holds a wake.
        let _ = self.sock.send_to(&[1], self.addr);
    }

    /// Consume every queued wake.
    fn drain(&self) {
        let mut b = [0u8; 8];
        while self.sock.recv_from(&mut b).is_ok() {}
    }
}

/// Why [`wait`] returned. A loop checks its stop flag and its timed jobs
/// after any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ready {
    /// The socket has a datagram (or an error to report: read it).
    Socket,
    /// The waker fired.
    Woken,
    /// `timeout` passed, or a signal interrupted the wait.
    Timeout,
}

/// Block until `socket` is readable, `waker` fires or `timeout` passes
/// (`None`: no timeout). A timeout is rounded up to whole milliseconds, so a
/// loop waiting for a deadline wakes at or after it, never spinning on a
/// sub-millisecond remainder.
pub fn wait(socket: &MediaSocket, waker: &Waker, timeout: Option<Duration>) -> io::Result<Ready> {
    let ms = timeout_ms(timeout);
    let (sock, wake) = poll2(socket_handle(socket.raw_socket()), socket_handle(&waker.sock), ms)?;
    if wake {
        waker.drain();
    }
    Ok(if sock {
        Ready::Socket
    } else if wake {
        Ready::Woken
    } else {
        Ready::Timeout
    })
}

/// Whether `socket` has a datagram (or error) to read right now.
pub fn readable_now(socket: &UdpSocket) -> bool {
    poll1(socket_handle(socket)).unwrap_or(false)
}

fn timeout_ms(timeout: Option<Duration>) -> i32 {
    match timeout {
        None => -1,
        Some(d) => {
            let ms = d.as_nanos().div_ceil(1_000_000);
            ms.min(i32::MAX as u128) as i32
        }
    }
}

#[cfg(unix)]
type Handle = std::os::fd::RawFd;
#[cfg(unix)]
fn socket_handle(s: &UdpSocket) -> Handle {
    use std::os::fd::AsRawFd;
    s.as_raw_fd()
}

#[cfg(windows)]
type Handle = std::os::windows::io::RawSocket;
#[cfg(windows)]
fn socket_handle(s: &UdpSocket) -> Handle {
    use std::os::windows::io::AsRawSocket;
    s.as_raw_socket()
}

/// poll(2) over two descriptors: (first ready, second ready). Error and
/// hang-up count as ready (the read reports them). EINTR is a timeout.
#[cfg(unix)]
fn poll2(a: Handle, b: Handle, timeout_ms: i32) -> io::Result<(bool, bool)> {
    let ready = libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
    let mut fds = [
        libc::pollfd { fd: a, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: b, events: libc::POLLIN, revents: 0 },
    ];
    // SAFETY: two valid pollfd entries for the duration of the call.
    let r = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted { Ok((false, false)) } else { Err(e) };
    }
    Ok((fds[0].revents & ready != 0, fds[1].revents & ready != 0))
}

#[cfg(unix)]
fn poll1(a: Handle) -> io::Result<bool> {
    let ready = libc::POLLIN | libc::POLLERR | libc::POLLHUP | libc::POLLNVAL;
    let mut fd = libc::pollfd { fd: a, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd for the duration of the call.
    let r = unsafe { libc::poll(&mut fd, 1, 0) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd.revents & ready != 0)
}

/// WSAPoll over two sockets. Only POLLRDNORM may be asked for (WSAPoll
/// refuses POLLIN's POLLRDBAND on UDP sockets on some versions); error,
/// hang-up and invalid come back regardless and count as ready.
#[cfg(windows)]
fn poll2(a: Handle, b: Handle, timeout_ms: i32) -> io::Result<(bool, bool)> {
    use windows::Win32::Networking::WinSock::{
        WSAPoll, POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, SOCKET, WSAPOLLFD, WSAPOLL_EVENT_FLAGS,
    };
    let ready = POLLRDNORM.0 | POLLERR.0 | POLLHUP.0 | POLLNVAL.0;
    let mut fds = [
        WSAPOLLFD { fd: SOCKET(a as usize), events: POLLRDNORM, revents: WSAPOLL_EVENT_FLAGS(0) },
        WSAPOLLFD { fd: SOCKET(b as usize), events: POLLRDNORM, revents: WSAPOLL_EVENT_FLAGS(0) },
    ];
    // SAFETY: two valid WSAPOLLFD entries for the duration of the call.
    let r = unsafe { WSAPoll(fds.as_mut_ptr(), 2, timeout_ms) };
    if r < 0 {
        // WSAGetLastError is GetLastError on the calling thread.
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0].revents.0 & ready != 0, fds[1].revents.0 & ready != 0))
}

#[cfg(windows)]
fn poll1(a: Handle) -> io::Result<bool> {
    use windows::Win32::Networking::WinSock::{
        WSAPoll, POLLERR, POLLHUP, POLLNVAL, POLLRDNORM, SOCKET, WSAPOLLFD, WSAPOLL_EVENT_FLAGS,
    };
    let ready = POLLRDNORM.0 | POLLERR.0 | POLLHUP.0 | POLLNVAL.0;
    let mut fd = WSAPOLLFD { fd: SOCKET(a as usize), events: POLLRDNORM, revents: WSAPOLL_EVENT_FLAGS(0) };
    // SAFETY: one valid WSAPOLLFD for the duration of the call.
    let r = unsafe { WSAPoll(&mut fd, 1, 0) };
    if r < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd.revents.0 & ready != 0)
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

    fn plain_pair() -> (UdpSocket, MediaSocket) {
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(peer.local_addr().unwrap()).unwrap();
        peer.connect(sock.local_addr().unwrap()).unwrap();
        (peer, MediaSocket::plain(sock, "7"))
    }

    /// The wait sleeps until there is something: a datagram, a wake, or its
    /// deadline (rounded up, never early), and a wake sent while nobody
    /// waits is kept for the next wait.
    #[test]
    fn wait_returns_for_a_datagram_a_wake_or_its_deadline() {
        let (peer, sock) = plain_pair();
        let waker = Waker::new().unwrap();

        let t0 = Instant::now();
        assert_eq!(wait(&sock, &waker, Some(Duration::from_millis(30))).unwrap(), Ready::Timeout);
        assert!(t0.elapsed() >= Duration::from_millis(30), "{:?}", t0.elapsed());
        // A sub-millisecond remainder still waits it out.
        let t0 = Instant::now();
        assert_eq!(wait(&sock, &waker, Some(Duration::from_micros(300))).unwrap(), Ready::Timeout);
        assert!(t0.elapsed() >= Duration::from_micros(300));

        peer.send(b"\x00x").unwrap();
        assert_eq!(wait(&sock, &waker, None).unwrap(), Ready::Socket);
        let mut buf = [0u8; RECV_BUF_SIZE];
        assert_eq!(sock.recv_ready(&mut buf).unwrap(), 0..2);

        // Woken before the wait (several times): one wait returns, the next
        // sleeps again.
        waker.wake();
        waker.wake();
        assert_eq!(wait(&sock, &waker, None).unwrap(), Ready::Woken);
        assert_eq!(wait(&sock, &waker, Some(Duration::from_millis(20))).unwrap(), Ready::Timeout);

        // Woken from another thread while waiting with no deadline.
        let waker = std::sync::Arc::new(waker);
        let w = waker.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            w.wake();
        });
        let t0 = Instant::now();
        assert_eq!(wait(&sock, &waker, None).unwrap(), Ready::Woken);
        assert!(t0.elapsed() >= Duration::from_millis(25));
        t.join().unwrap();
    }

    /// An ICMP error on a connected socket (relay gone) makes it readable:
    /// the read reports the error instead of the wait sleeping past it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_socket_error_counts_as_readable() {
        let (peer, sock) = plain_pair();
        drop(peer);
        let _ = sock.send(b"keepalive");
        let waker = Waker::new().unwrap();
        assert_eq!(wait(&sock, &waker, Some(Duration::from_secs(2))).unwrap(), Ready::Socket);
        let mut buf = [0u8; RECV_BUF_SIZE];
        let e = sock.recv_ready(&mut buf).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
    }

    #[test]
    fn oversized_reads_fill_the_buffer() {
        assert!(!is_oversized(1270));
        assert!(!is_oversized(RECV_BUF_SIZE - 1));
        assert!(is_oversized(RECV_BUF_SIZE));
    }
}

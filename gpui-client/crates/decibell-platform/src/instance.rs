//! One running instance per profile directory (Electron's `requestSingleInstanceLock` and
//! `second-instance`): a second start hands its command line to the first and exits.
//!
//! - **Who is first**: an exclusive lock on a file of the profile (`File::try_lock`), held by the
//!   first instance for its whole life and dropped by the system when it exits, crashed or not.
//!   So two profiles (a test's, a development one, the shipped one) run side by side.
//! - **The hand-over**: Linux / Unix: a Unix socket in the profile (`instance.sock`, mode `0600`), or
//!   in `$XDG_RUNTIME_DIR` named after the profile when that path is too long for a socket; the
//!   first instance accepts only its own user (`SO_PEERCRED`).
//!   Windows: a loopback TCP port written with a random secret into the profile directory (which
//!   only the user can read); the second start sends the secret first and lets the first take the
//!   foreground (`AllowSetForegroundWindow`).
//! - **The listener** is one named thread blocked in `accept`: it wakes only when a second start
//!   connects, queues the request for the UI thread (one wake) and answers at once.
//!
//! Message: a 4-byte little-endian length, then the [`Launch`] as JSON. Answer: one byte.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use crate::launch::Launch;

/// A request larger than this is refused (a command line is a few hundred bytes).
const MAX_MESSAGE: usize = 64 * 1024;
/// How long a second start keeps trying to reach the first (which may have taken the lock a
/// moment before it listens) before giving up.
const CONNECT_PATIENCE: Duration = Duration::from_secs(3);
/// How long either side waits for the other once connected.
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const ACK: u8 = 1;

/// What [`start`] found.
pub enum Start {
    /// This is the profile's only instance: keep it (the lock lives as long as it does) and
    /// [`Instance::serve`] it once the window is up.
    First(Instance),
    /// Another instance of this profile runs and took the request.
    Forwarded { took: Duration },
}

/// The lock and the endpoint of the first instance.
pub struct Instance {
    lock: File,
    endpoint: Endpoint,
}

/// Where the endpoint of the instance for `profile` lives, and the lock file's path.
#[derive(Debug, Clone)]
pub struct Paths {
    pub lock: PathBuf,
    /// Unix: the socket. Windows: the file with the port and the secret.
    pub endpoint: PathBuf,
}

impl Paths {
    pub fn for_profile(profile: &Path) -> Result<Paths> {
        std::fs::create_dir_all(profile).with_context(|| format!("creating {}", profile.display()))?;
        let canonical = profile.canonicalize().unwrap_or_else(|_| profile.to_path_buf());
        let lock = canonical.join("instance.lock");
        #[cfg(unix)]
        let endpoint = {
            // In the profile (it goes with it), unless the path is too long for `sun_path` (108
            // bytes): then in the user's runtime directory, named after the profile.
            let in_profile = canonical.join("instance.sock");
            let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|d| d.is_dir());
            match runtime {
                Some(dir) if in_profile.as_os_str().len() > 100 => dir.join(format!("decibell-{:016x}.sock", fnv1a(&canonical))),
                _ => in_profile,
            }
        };
        #[cfg(not(unix))]
        let endpoint = canonical.join("instance.port");
        Ok(Paths { lock, endpoint })
    }
}

/// FNV-1a over the path's bytes: a name that is the same in every process (std's hasher is
/// randomised per process).
#[cfg_attr(not(unix), allow(dead_code))]
fn fnv1a(path: &Path) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Become the profile's instance, or hand `launch` to the one that is.
pub fn start(profile: &Path, launch: &Launch) -> Result<Start> {
    let paths = Paths::for_profile(profile)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.lock)
        .with_context(|| format!("opening {}", paths.lock.display()))?;
    match lock.try_lock() {
        Ok(()) => {
            let endpoint = Endpoint::bind(&paths)?;
            Ok(Start::First(Instance { lock, endpoint }))
        }
        Err(std::fs::TryLockError::WouldBlock) => {
            let started = Instant::now();
            forward(&paths, launch)?;
            Ok(Start::Forwarded { took: started.elapsed() })
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e).with_context(|| format!("locking {}", paths.lock.display())),
    }
}

impl Instance {
    /// Starts the listener: what second starts send arrives on the returned channel (drain it on
    /// the UI thread; nothing is sent while nobody starts the app again).
    pub fn serve(self) -> Result<UnboundedReceiver<Launch>> {
        let (tx, rx) = unbounded();
        let Instance { lock, endpoint } = self;
        std::thread::Builder::new()
            .name("decibell-instance".into())
            .spawn(move || {
                // The lock lives as long as the listener: the process's life.
                let _lock = lock;
                endpoint.accept_loop(tx);
            })
            .context("starting the instance listener")?;
        Ok(rx)
    }

    /// Where its endpoint is (removed again by [`remove_endpoint`] at quit).
    pub fn endpoint_path(&self) -> &Path {
        &self.endpoint.path
    }
}

/// Removes the endpoint's file at quit (the lock file stays: it is only ever locked).
pub fn remove_endpoint(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn encode(launch: &Launch) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(launch)?;
    let mut message = (body.len() as u32).to_le_bytes().to_vec();
    message.extend(body);
    Ok(message)
}

fn decode(stream: &mut impl Read) -> Result<Launch> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        bail!("a {len}-byte request");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

/// One accepted connection: read the request, queue it, answer.
fn handle(stream: &mut (impl Read + Write), tx: &UnboundedSender<Launch>) -> Result<()> {
    let launch = decode(stream)?;
    tx.unbounded_send(launch).ok();
    stream.write_all(&[ACK])?;
    Ok(())
}

fn forward(paths: &Paths, launch: &Launch) -> Result<()> {
    let message = encode(launch)?;
    let started = Instant::now();
    loop {
        match Endpoint::connect(paths, &message) {
            Ok(()) => return Ok(()),
            Err(e) if started.elapsed() < CONNECT_PATIENCE => {
                log::debug!("[instance] the running instance isn't listening yet: {e:#}");
                // The first instance holds the lock but has not bound its endpoint yet: a start
                // racing another, never a steady state.
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e).context("the running instance didn't answer"),
        }
    }
}

#[cfg(unix)]
struct Endpoint {
    path: PathBuf,
    listener: std::os::unix::net::UnixListener,
}

#[cfg(unix)]
impl Endpoint {
    fn bind(paths: &Paths) -> Result<Endpoint> {
        use std::os::unix::fs::PermissionsExt as _;
        // We hold the lock: a socket file here is a dead instance's.
        let _ = std::fs::remove_file(&paths.endpoint);
        let listener = std::os::unix::net::UnixListener::bind(&paths.endpoint)
            .with_context(|| format!("listening on {}", paths.endpoint.display()))?;
        std::fs::set_permissions(&paths.endpoint, std::fs::Permissions::from_mode(0o600))?;
        Ok(Endpoint { path: paths.endpoint.clone(), listener })
    }

    fn accept_loop(self, tx: UnboundedSender<Launch>) {
        for stream in self.listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    log::warn!("[instance] accept failed: {e}");
                    continue;
                }
            };
            if !same_user(&stream) {
                log::warn!("[instance] refused a connection from another user");
                continue;
            }
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            if let Err(e) = handle(&mut stream, &tx) {
                log::warn!("[instance] a second start's request: {e:#}");
            }
        }
    }

    fn connect(paths: &Paths, message: &[u8]) -> Result<()> {
        let mut stream = std::os::unix::net::UnixStream::connect(&paths.endpoint)
            .with_context(|| format!("connecting to {}", paths.endpoint.display()))?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        stream.write_all(message)?;
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack)?;
        if ack[0] != ACK {
            bail!("unexpected answer {}", ack[0]);
        }
        Ok(())
    }
}

/// The peer is this process's user (`SO_PEERCRED`; the socket's mode already keeps others out).
#[cfg(target_os = "linux")]
fn same_user(stream: &std::os::unix::net::UnixStream) -> bool {
    use std::os::fd::AsRawFd as _;
    let mut cred = libc::ucred { pid: 0, uid: u32::MAX, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` are valid for writes of the sizes given; the descriptor is open.
    let ok = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } == 0;
    // SAFETY: getuid has no preconditions.
    ok && cred.uid == unsafe { libc::getuid() }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn same_user(_: &std::os::unix::net::UnixStream) -> bool {
    true
}

#[cfg(windows)]
struct Endpoint {
    path: PathBuf,
    listener: std::net::TcpListener,
    secret: [u8; 16],
}

#[cfg(windows)]
impl Endpoint {
    fn bind(paths: &Paths) -> Result<Endpoint> {
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).context("listening on the loopback")?;
        let port = listener.local_addr()?.port();
        let mut secret = [0u8; 16];
        getrandom::getrandom(&mut secret).map_err(|e| anyhow::anyhow!("no random bytes: {e}"))?;
        let hex: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(&paths.endpoint, format!("{port} {} {hex}\n", std::process::id()))
            .with_context(|| format!("writing {}", paths.endpoint.display()))?;
        Ok(Endpoint { path: paths.endpoint.clone(), listener, secret })
    }

    fn accept_loop(self, tx: UnboundedSender<Launch>) {
        for stream in self.listener.incoming() {
            let mut stream = match stream {
                Ok(stream) => stream,
                Err(e) => {
                    log::warn!("[instance] accept failed: {e}");
                    continue;
                }
            };
            let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
            let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
            let mut secret = [0u8; 16];
            if stream.read_exact(&mut secret).is_err() || secret != self.secret {
                log::warn!("[instance] refused a connection without the secret");
                continue;
            }
            if let Err(e) = handle(&mut stream, &tx) {
                log::warn!("[instance] a second start's request: {e:#}");
            }
        }
    }

    fn connect(paths: &Paths, message: &[u8]) -> Result<()> {
        let text = std::fs::read_to_string(&paths.endpoint).with_context(|| format!("reading {}", paths.endpoint.display()))?;
        let mut fields = text.split_whitespace();
        let port: u16 = fields.next().context("no port")?.parse()?;
        let pid: u32 = fields.next().context("no pid")?.parse()?;
        let hex = fields.next().context("no secret")?;
        let mut secret = [0u8; 16];
        for (i, byte) in secret.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2).context("a short secret")?, 16)?;
        }
        // The running instance may take the foreground (Windows lets only the process the user
        // started, this one, hand that right over).
        crate::win32::allow_foreground(pid);
        let mut stream = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        stream.write_all(&secret)?;
        stream.write_all(message)?;
        let mut ack = [0u8; 1];
        stream.read_exact(&mut ack)?;
        if ack[0] != ACK {
            bail!("unexpected answer {}", ack[0]);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt as _;

    #[test]
    fn a_second_start_hands_over_and_profiles_are_apart() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let launch = Launch { links: vec!["decibell://invite/ABCD234567".into()], hotkey: None, activation_token: Some("t1".into()) };
        let Start::First(first) = start(&a, &Launch::default()).unwrap() else { panic!("a: not first") };
        // Another profile is its own instance.
        let Start::First(_other) = start(&b, &Launch::default()).unwrap() else { panic!("b: not first") };
        let mut rx = first.serve().unwrap();
        let Start::Forwarded { took } = start(&a, &launch).unwrap() else { panic!("a again: not forwarded") };
        assert!(took < Duration::from_secs(1), "{took:?}");
        let got = futures::executor::block_on(rx.next()).unwrap();
        assert_eq!(got, launch);
    }

    #[test]
    fn requests_round_trip() {
        let launch = Launch { links: vec![], hotkey: Some("push_to_talk:up".into()), activation_token: None };
        let bytes = encode(&launch).unwrap();
        assert_eq!(decode(&mut bytes.as_slice()).unwrap(), launch);
        let mut huge = ((MAX_MESSAGE + 1) as u32).to_le_bytes().to_vec();
        huge.extend([0u8; 8]);
        assert!(decode(&mut huge.as_slice()).is_err());
    }

    #[test]
    fn names_are_stable() {
        assert_eq!(fnv1a(Path::new("/home/a/.config/decibell")), fnv1a(Path::new("/home/a/.config/decibell")));
        assert_ne!(fnv1a(Path::new("/home/a/.config/decibell")), fnv1a(Path::new("/tmp/p")));
    }
}

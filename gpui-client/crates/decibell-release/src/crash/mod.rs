//! Crash reports: `sentry.ts` and the renderer's `lib/sentry.ts`, without a Sentry SDK.
//!
//! **When** (Electron's three gates): a packaged ship build (not a development build, not a ship
//! binary outside a package), a DSN baked in (`resources/sentry.json`, written by the release
//! workflow from the `SENTRY_DSN` secret), and the Privacy toggle on (`crash_reporting_enabled` in
//! `config.json`, default on; read at start, so a change takes effect at the next start, as in
//! Electron). Otherwise nothing is installed: no hook, no handler, never a socket.
//!
//! **What** is reported:
//! - a **panic nothing catches** (the UI thread, GPUI's workers, our threads, the core's unguarded
//!   threads: [`decibell_panic::here`] says who catches): the panic hook ([`install_panic_hook`],
//!   installed in every build, reporting or not) writes the report to
//!   `<profile>/crash-reports/<event id>.envelope`, sends it within [`SEND_BUDGET`] (Electron's
//!   flush timeout, `@sentry/node`'s default `shutdownTimeout`) and **aborts the process**, as the
//!   aborting profile did: no thread dies silently. One that couldn't be sent goes at the next start;
//! - a **recovered panic** (one the core's guarded workers, its runtime's blocking pool or a section of ours run
//!   through `decibell_panic::catch` catch: the app carries on, as Electron's addon does): a report
//!   at level `error`, `handled: true`, tag `recovered`, written and sent in the background without
//!   holding the panicking thread up; at most [`RECOVERED_PER_RUN`] per run, one per panic location.
//!   A recovered panic that something resumes on the UI thread (`resume_unwind` calls no hook) ends
//!   the process there through the app's guard ([`abort_after_unwind`]), as a backstop;
//! - a **native crash** (SIGSEGV / SIGBUS / SIGILL / SIGFPE / SIGABRT on Linux, an unhandled SEH
//!   exception on Windows: a fault in a driver, as once in NVIDIA's at exit): an async-signal-safe
//!   handler records the signal, the faulting address, the instruction pointer and the mapping it
//!   is in, then lets the crash continue as before (Rust's stack-overflow message, the core dump);
//!   the report is built from that record and sent at the next start. No minidump: there is no
//!   out-of-process handler (Electron's crashpad is a process of its own, awake for the app's
//!   life).
//!
//! **How**: one Sentry envelope per crash (`POST /api/<project>/envelope/`), `platform: native`,
//! `level: fatal`, `release: decibell@<version>`, `environment: production`, `user.id` = the random
//! install id, Electron's tags (`platform`, `arch`, `os_release`, `is_packaged`) plus `install_kind`,
//! the instruction addresses and this executable's `debug_meta`. Scrubbed as [`scrub`] says. No
//! session tracking, no performance tracing, no breadcrumbs, no IP, no host name. Nothing runs at
//! rest: no thread, no timer; the sender thread exists only while a report goes out.

mod image;
mod native;
pub mod scrub;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use url::Url;

pub use image::Image;

use crate::http::{Client, Rule};
use crate::install;

/// How long a crashing process waits for its report to be sent (`@sentry/node`'s
/// `shutdownTimeout`, which bounds Electron's flush at exit).
pub const SEND_BUDGET: Duration = Duration::from_secs(2);
/// Reports kept for a later start, newest first; older ones are dropped.
const KEEP: usize = 10;
const KEEP_FOR: Duration = Duration::from_secs(30 * 24 * 3600);

/// A Sentry DSN: `https://<public key>@<host>[:port][/<path>]/<project id>`.
#[derive(Debug, Clone, PartialEq)]
pub struct Dsn {
    pub public_key: String,
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
    pub project_id: String,
}

impl Dsn {
    pub fn parse(text: &str) -> Option<Dsn> {
        let url = Url::parse(text.trim()).ok()?;
        let public_key = url.username().to_string();
        let host = url.host_str()?.to_string();
        let mut segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let project_id = segments.pop()?.to_string();
        let plausible = !public_key.is_empty()
            && public_key.bytes().all(|b| b.is_ascii_alphanumeric())
            && !project_id.is_empty()
            && project_id.bytes().all(|b| b.is_ascii_digit());
        let scheme_ok = url.scheme() == "https" || (cfg!(any(test, feature = "test-hooks")) && url.scheme() == "http");
        (plausible && scheme_ok).then(|| Dsn {
            public_key,
            scheme: url.scheme().to_string(),
            host,
            port: url.port(),
            path: segments.iter().map(|s| format!("/{s}")).collect(),
            project_id,
        })
    }

    pub fn envelope_url(&self) -> Url {
        let port = self.port.map(|p| format!(":{p}")).unwrap_or_default();
        Url::parse(&format!("{}://{}{port}{}/api/{}/envelope/", self.scheme, self.host, self.path, self.project_id))
            .expect("built from a parsed URL")
    }

    pub fn auth_header(&self) -> String {
        format!(
            "Sentry sentry_version=7, sentry_client=decibell-native/{}, sentry_key={}",
            crate::VERSION,
            self.public_key
        )
    }

    /// The only host a crash report goes to.
    fn rule(&self) -> Rule {
        #[allow(unused_mut)]
        let mut rule = Rule::hosts(&[&self.host]);
        #[cfg(any(test, feature = "test-hooks"))]
        {
            rule.loopback_http = self.scheme == "http";
        }
        rule
    }

    /// The DSN with its key hidden, for the log.
    fn redacted(&self) -> String {
        format!("{}://…@{}/{}", self.scheme, self.host, self.project_id)
    }
}

/// What reporting needs, once the gates are passed.
#[derive(Debug, Clone)]
pub struct Config {
    /// `<profile>/crash-reports`.
    pub dir: PathBuf,
    pub dsn: Dsn,
    pub install_id: String,
}

/// Electron's three gates, in its order, with its log lines.
pub fn gate(profile: &Path) -> Option<Config> {
    if !install::packaged() {
        log::info!("[sentry] disabled (dev mode)");
        return None;
    }
    let (enabled, install_id) = boot_settings(profile);
    if !enabled {
        log::info!("[sentry] disabled (user opt-out)");
        return None;
    }
    let Some(dsn) = baked_dsn() else {
        log::info!("[sentry] disabled (no DSN baked)");
        return None;
    };
    let install_id = install_id.unwrap_or_else(|| persist_install_id(profile));
    log::info!("[sentry] initialized (release=decibell@{}, {})", crate::VERSION, dsn.redacted());
    Some(Config { dir: profile.join("crash-reports"), dsn, install_id })
}

/// `loadDsn()`: `resources/sentry.json`'s `dsn` (a self-test may give one instead).
fn baked_dsn() -> Option<Dsn> {
    #[cfg(feature = "test-hooks")]
    if let Ok(dsn) = std::env::var("DECIBELL_TEST_CRASH_DSN") {
        return Dsn::parse(&dsn);
    }
    let bytes = install::resource("sentry.json")?;
    let json: Value = serde_json::from_slice(&bytes).ok()?;
    let dsn = json.get("dsn")?.as_str()?;
    match Dsn::parse(dsn) {
        Some(dsn) => Some(dsn),
        None => {
            log::warn!("[sentry] resources/sentry.json holds no usable DSN");
            None
        }
    }
}

/// `loadSentryBootConfig()`: the toggle (default on) and the install id, read from `config.json`
/// directly (the core starts later).
fn boot_settings(profile: &Path) -> (bool, Option<String>) {
    let json: Value = std::fs::read(profile.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(install::strip_bom(&b)).ok())
        .unwrap_or(Value::Null);
    let settings = &json["settings"];
    let enabled = settings["crash_reporting_enabled"].as_bool() != Some(false);
    let id = settings["crash_reporting_install_id"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    (enabled, id)
}

/// A new install id (`crypto.randomUUID()`), written into `config.json`'s settings keeping every
/// other field, as `loadSentryBootConfig` does. Called before the core starts, by the instance
/// that holds the profile.
fn persist_install_id(profile: &Path) -> String {
    let id = random_uuid();
    let path = profile.join("config.json");
    let mut json: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(install::strip_bom(&b)).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    if !json["settings"].is_object() {
        json["settings"] = json!({});
    }
    json["settings"]["crash_reporting_install_id"] = json!(id);
    let written = std::fs::create_dir_all(profile).and_then(|_| {
        let bytes = serde_json::to_vec_pretty(&json).unwrap_or_default();
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, &path)
    });
    if let Err(e) = written {
        log::warn!("[sentry] could not persist install_id: {e}");
    }
    id
}

/// 16 random bytes as a v4 UUID (`8-4-4-4-12`).
pub fn random_uuid() -> String {
    let mut b = random16();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[..8], &h[8..12], &h[12..16], &h[16..20], &h[20..])
}

fn random16() -> [u8; 16] {
    use ring::rand::SecureRandom as _;
    let mut b = [0u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut b).is_err() {
        // No OS randomness: a unique-enough id from the clock and the process.
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        b[..8].copy_from_slice(&(t as u64).to_le_bytes());
        b[8..12].copy_from_slice(&std::process::id().to_le_bytes());
    }
    b
}

struct Reporter {
    config: Config,
    image: Image,
    tags: Value,
}

static REPORTER: OnceLock<Arc<Reporter>> = OnceLock::new();
/// Account and community names the app registered (scrubbed from messages).
static NAMES: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// A crash is being reported: a second panic, or the abort that follows a panic, adds nothing.
pub(crate) static REPORTING: AtomicBool = AtomicBool::new(false);
/// Recovered panics reported in this run, by location (a panic in a loop is reported once).
static RECOVERED: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The last recovered panic: what [`abort_after_unwind`] reports when it comes back to the UI
/// thread (`resume_unwind` calls no hook).
static LAST_RECOVERED: Mutex<Option<Recovered>> = Mutex::new(None);

/// Community servers connected now (the renderer's `connected_servers` tag).
static CONNECTED_SERVERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
/// The GPU the window draws with (`contexts.gpu`), once the app knows it.
static GPU: Mutex<Option<Value>> = Mutex::new(None);
/// When this process started (`contexts.app.app_start_time`).
static STARTED_AT: OnceLock<String> = OnceLock::new();

/// How many community servers are connected (the renderer's `Sentry.setTag("connected_servers")`:
/// it helps reproduce "happens when N+ servers connected").
pub fn set_connected_servers(count: usize) {
    CONNECTED_SERVERS.store(count, Ordering::Relaxed);
}

/// The GPU the window draws with, for `contexts.gpu` (Sentry's GPU context: name, vendor id,
/// driver).
pub fn set_gpu(name: &str, vendor_id: u32, driver: &str) {
    if let Ok(mut gpu) = GPU.lock() {
        *gpu = Some(json!({ "name": name, "vendor_id": format!("0x{vendor_id:04x}"), "driver": driver }));
    }
}

/// At most this many recovered panics are reported per run.
pub const RECOVERED_PER_RUN: usize = 5;

#[derive(Clone)]
struct Recovered {
    message: String,
    location: Option<(String, u32)>,
    thread: String,
    frames: Vec<usize>,
}

/// The one panic hook, installed first thing at start in every build (whether reports are sent or
/// not): the usual message on stderr, then a recovered report and back to the catcher, or a fatal
/// report and the end of the process ([`decibell_panic::here`] decides). Idempotent.
pub fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = STARTED_AT.set(rfc3339_now());
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // The usual message on stderr first, as before.
            previous(info);
            match decibell_panic::here() {
                Some(catcher) => {
                    let recovered = Recovered::from(info);
                    if let Ok(mut last) = LAST_RECOVERED.lock() {
                        *last = Some(recovered.clone());
                    }
                    if !REPORTING.load(Ordering::SeqCst)
                        && let Some(reporter) = REPORTER.get()
                    {
                        reporter.recovered(recovered, catcher);
                    }
                    log::warn!("[crash] a panic on {} is caught there ({}); the app carries on", thread_name(), catcher.as_str());
                }
                None => {
                    if !REPORTING.swap(true, Ordering::SeqCst)
                        && let Some(reporter) = REPORTER.get()
                    {
                        reporter.fatal(Recovered::from(info), None);
                    }
                    std::process::abort();
                }
            }
        }));
    });
}

/// A panic came back to the UI thread after a catcher let it go on (`decibell_core::rt::run`
/// resumes a task's panic in its caller, and `resume_unwind` calls no hook): the fatal report of
/// the panic it carries (the last recovered one), then the end of the process. The app's boot
/// calls it from a guard on the UI thread's stack.
pub fn abort_after_unwind() -> ! {
    if !REPORTING.swap(true, Ordering::SeqCst)
        && let Some(reporter) = REPORTER.get()
    {
        let last = LAST_RECOVERED.lock().ok().and_then(|l| l.clone());
        let (event, original) = match last {
            Some(last) => {
                let original = last.thread.clone();
                (last, Some(original))
            }
            None => (
                Recovered { message: "a panic reached the main thread".into(), location: None, thread: thread_name(), frames: Vec::new() },
                None,
            ),
        };
        reporter.fatal(Recovered { thread: thread_name(), ..event }, original.as_deref());
    }
    std::process::abort();
}

fn thread_name() -> String {
    std::thread::current().name().unwrap_or("<unnamed>").to_string()
}

impl Recovered {
    fn from(info: &std::panic::PanicHookInfo<'_>) -> Recovered {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "Box<dyn Any>".into());
        let mut frames = Vec::new();
        backtrace::trace(|frame| {
            frames.push(frame.ip() as usize);
            frames.len() < 100
        });
        Recovered {
            message,
            location: info.location().map(|l| (l.file().to_string(), l.line())),
            thread: thread_name(),
            frames,
        }
    }
}

/// Starts reporting: the native handlers, and the sending of what earlier runs left (the panic
/// hook is [`install_panic_hook`]'s, which reports through what this sets up). Once per process.
pub fn init(config: Config) {
    let image = image::main_image();
    let tags = json!({
        "platform": install::platform(),
        "arch": install::arch(),
        "os_release": install::os_release(),
        "is_packaged": "true",
        "install_kind": install::current().kind.map_or("none", |k| k.as_str()),
    });
    let reporter = Arc::new(Reporter { config, image, tags });
    if REPORTER.set(reporter.clone()).is_err() {
        return;
    }
    let _ = std::fs::create_dir_all(&reporter.config.dir);
    native::install(&reporter.config.dir, &reporter.image);
    // In case the app didn't (it does, first thing): reports need the hook.
    install_panic_hook();
    send_pending(reporter);
}

/// Names that must never appear in a report (the signed-in account, the communities joined),
/// replaced as the app learns them.
pub fn set_names(names: Vec<String>) {
    if let Ok(mut current) = NAMES.lock() {
        *current = names;
    }
}

/// Whether reporting runs in this process.
pub fn active() -> bool {
    REPORTER.get().is_some()
}

/// The release tests' child (`gpui-client/packaging/tests/release_e2e.py`), after `init`:
/// `DECIBELL_TEST_PANIC=<message>` panics on a thread named `decibell-test-panic` that nothing
/// guards (the process ends); `main:<message>` on the main thread; `guarded:<message>` on that
/// thread inside [`decibell_panic::catch`] (recovered: the app starts as usual); `segv` writes
/// through a null pointer; `wait-pending` waits for the reports earlier runs left to be sent, then
/// exits. (`runtime:` and `runtime-task:` are the app's: they need the core.)
/// `DECIBELL_TEST_CRASH_NAMES=a,b` registers names first, as a signed-in app does.
#[cfg(feature = "test-hooks")]
pub fn panic_for_test() {
    let Ok(what) = std::env::var("DECIBELL_TEST_PANIC") else { return };
    // The names the app would have registered after a sign-in (`set_names`).
    if let Ok(names) = std::env::var("DECIBELL_TEST_CRASH_NAMES") {
        set_names(names.split(',').map(str::to_string).collect());
    }
    let on_test_thread = |guarded: bool, message: String| {
        let _ = std::thread::Builder::new()
            .name("decibell-test-panic".into())
            .spawn(move || {
                if guarded {
                    let caught = decibell_panic::catch(|| -> () { panic!("{message}") });
                    println!("decibell: crash-test guarded panic caught: {}", caught.is_err());
                } else {
                    panic!("{message}")
                }
            })
            .map(|t| t.join());
    };
    match what.as_str() {
        "segv" => {
            // SAFETY: none: the test wants a real segmentation fault.
            unsafe { std::ptr::write_volatile(std::ptr::null_mut::<u64>().wrapping_add(1), 1) };
        }
        "wait-pending" => {
            if let Some(sender) = PENDING.lock().ok().and_then(|mut p| p.take()) {
                let _ = sender.join();
            }
            std::process::exit(0);
        }
        other if other.starts_with("runtime:") || other.starts_with("runtime-task:") => {}
        other => match other.split_once(':') {
            Some(("main", message)) => panic!("{message}"),
            Some(("guarded", message)) => on_test_thread(true, message.to_string()),
            _ => on_test_thread(false, other.to_string()),
        },
    }
}

/// The sender of earlier runs' reports while it runs (the release tests wait for it).
static PENDING: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

impl Reporter {
    fn scrubber(&self) -> scrub::Scrubber {
        scrub::Scrubber::from_env(NAMES.lock().map(|n| n.clone()).unwrap_or_default())
    }

    /// A panic that ends the process: written, then sent within [`SEND_BUDGET`].
    /// `resumed_from`: the thread whose caught panic came back to this one.
    fn fatal(&self, panic: Recovered, resumed_from: Option<&str>) {
        let scrubber = self.scrubber();
        let location = panic.location.as_ref().map(|(file, line)| scrubber.location(file, *line));
        let mut event = self.event(
            Exception { kind: "panic", value: scrubber.text(&panic.message), mechanism: json!({"type": "panic", "handled": false}) },
            &panic.frames,
            location,
            &panic.thread,
            now_secs(),
        );
        if let Some(from) = resumed_from {
            event["tags"]["resumed_from"] = json!(from);
        }
        event["tags"]["connected_servers"] = json!(CONNECTED_SERVERS.load(Ordering::Relaxed).to_string());
        let Some(path) = self.write(&event) else { return };
        send_now(self.config.clone(), path, SEND_BUDGET);
    }

    /// A panic a catcher recovers from: level `error`, `handled: true`, tagged with who caught it;
    /// written, then sent on a thread of its own (the panicking thread goes on to its catch at
    /// once). Once per location, at most [`RECOVERED_PER_RUN`] per run.
    fn recovered(&self, panic: Recovered, catcher: decibell_panic::Catcher) {
        let scrubber = self.scrubber();
        let location = panic.location.as_ref().map(|(file, line)| scrubber.location(file, *line));
        let key = location.clone().unwrap_or_else(|| panic.thread.clone());
        {
            let Ok(mut seen) = RECOVERED.lock() else { return };
            if seen.len() >= RECOVERED_PER_RUN || seen.contains(&key) {
                return;
            }
            seen.push(key);
        }
        let mut event = self.event(
            Exception { kind: "panic", value: scrubber.text(&panic.message), mechanism: json!({"type": "panic", "handled": true}) },
            &panic.frames,
            location,
            &panic.thread,
            now_secs(),
        );
        event["level"] = json!("error");
        event["tags"]["recovered"] = json!(catcher.as_str());
        event["tags"]["connected_servers"] = json!(CONNECTED_SERVERS.load(Ordering::Relaxed).to_string());
        let Some(path) = self.write(&event) else { return };
        send_later(self.config.clone(), path);
    }

    fn event(&self, exception: Exception, frames: &[usize], location: Option<String>, thread: &str, timestamp: f64) -> Value {
        let mut tags = self.tags.clone();
        if let Some(location) = location {
            tags["panic_location"] = json!(location);
        }
        tags["thread"] = json!(thread);
        // Sentry wants the oldest call first.
        let frames: Vec<Value> = frames.iter().rev().map(|ip| json!({"instruction_addr": format!("0x{ip:x}")})).collect();
        json!({
            "event_id": random_uuid().replace('-', ""),
            "timestamp": timestamp,
            "platform": "native",
            "level": "fatal",
            "release": format!("decibell@{}", crate::VERSION),
            "environment": "production",
            "user": { "id": self.config.install_id },
            "tags": tags,
            "contexts": contexts(),
            "exception": { "values": [{
                "type": exception.kind,
                "value": exception.value,
                "mechanism": exception.mechanism,
                "stacktrace": { "frames": frames },
            }]},
            "debug_meta": { "images": [self.image.to_json()] },
            "sdk": { "name": "decibell.native", "version": crate::VERSION },
        })
    }

    /// The envelope on disk, before anything is sent.
    fn write(&self, event: &Value) -> Option<PathBuf> {
        let id = event["event_id"].as_str()?.to_string();
        let path = self.config.dir.join(format!("{id}.envelope"));
        let _ = std::fs::create_dir_all(&self.config.dir);
        std::fs::write(&path, envelope(event)).ok()?;
        Some(path)
    }
}

struct Exception {
    kind: &'static str,
    value: String,
    mechanism: Value,
}

/// Sentry's contexts as the Electron SDK fills them, from what this process knows: the OS, the
/// app (name, version, start), the device (architecture, CPUs, memory) and, once the app said, the
/// GPU. Nothing that names the machine or the person.
fn contexts() -> Value {
    let mut contexts = json!({
        "os": { "name": os_name(), "version": install::os_release() },
        "app": { "app_name": "Decibell", "app_version": crate::VERSION },
        "device": {
            "family": "Desktop",
            "arch": install::arch(),
            "processor_count": std::thread::available_parallelism().map_or(0, |n| n.get()),
        },
    });
    if let Some(started) = STARTED_AT.get() {
        contexts["app"]["app_start_time"] = json!(started);
    }
    if let Some(bytes) = memory_size() {
        contexts["device"]["memory_size"] = json!(bytes);
    }
    if let Some(gpu) = GPU.lock().ok().and_then(|g| g.clone()) {
        contexts["gpu"] = gpu;
    }
    contexts
}

/// The machine's memory in bytes.
fn memory_size() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = text.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim().parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
        let mut status = MEMORYSTATUSEX { dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
        // SAFETY: `status` is a valid MEMORYSTATUSEX with its length set, as the call requires.
        unsafe { GlobalMemoryStatusEx(&mut status) }.ok()?;
        Some(status.ullTotalPhys)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

fn os_name() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "windows" => "Windows",
        "macos" => "macOS",
        other => other,
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// An envelope of one event: its header, the item's header, the event.
pub fn envelope(event: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(event).unwrap_or_default();
    let header = json!({ "event_id": event["event_id"], "sent_at": rfc3339_now() });
    let item = json!({ "type": "event", "length": body.len() });
    let mut out = serde_json::to_vec(&header).unwrap_or_default();
    out.push(b'\n');
    out.extend(serde_json::to_vec(&item).unwrap_or_default());
    out.push(b'\n');
    out.extend(body);
    out.push(b'\n');
    out
}

fn rfc3339_now() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Sends one envelope; `true` when it is done with (sent, or refused for good).
fn send_file(config: &Config, path: &Path) -> bool {
    let Ok(body) = std::fs::read(path) else { return true };
    let client = Client::new(config.dsn.rule(), format!("decibell-native/{}", crate::VERSION))
        .timeouts(Duration::from_millis(1500), Duration::from_millis(1500));
    let auth = config.dsn.auth_header();
    let headers = [("X-Sentry-Auth", auth.as_str()), ("Content-Type", "application/x-sentry-envelope")];
    match client.post(&config.dsn.envelope_url(), &headers, &body) {
        Ok(response) if (200..300).contains(&response.status) => {
            let _ = std::fs::remove_file(path);
            true
        }
        // Refused for good (a bad DSN, a rejected event): don't send it again. 429 and 5xx: later.
        Ok(response) if (400..500).contains(&response.status) && response.status != 429 => {
            log::warn!("[sentry] a report was refused ({})", response.status);
            let _ = std::fs::remove_file(path);
            true
        }
        Ok(response) => {
            log::warn!("[sentry] the report waits for the next start ({})", response.status);
            false
        }
        Err(e) => {
            log::warn!("[sentry] the report waits for the next start ({e})");
            false
        }
    }
}

/// From a crashing thread: send on a helper thread, wait at most `budget`.
fn send_now(config: Config, path: PathBuf, budget: Duration) {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("decibell-crash-send".into()).spawn(move || {
        let _ = tx.send(send_file(&config, &path));
    });
    if spawned.is_ok() {
        let _ = rx.recv_timeout(budget);
    }
}

/// From a thread that carries on (a recovered panic): send on a helper thread, don't wait. What
/// isn't sent when the process ends goes at the next start.
fn send_later(config: Config, path: PathBuf) {
    let _ = std::thread::Builder::new().name("decibell-crash-send".into()).spawn(move || {
        send_file(&config, &path);
    });
}

/// What earlier runs left (reports not sent, native crash records): turned into envelopes and
/// sent on a thread that ends when they are done. Nothing to send, no thread.
fn send_pending(reporter: Arc<Reporter>) {
    let dir = reporter.config.dir.clone();
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "envelope" || x == "native"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.is_empty() {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    let now = std::time::SystemTime::now();
    let mut keep = Vec::new();
    for (i, (when, path)) in files.into_iter().enumerate() {
        let old = now.duration_since(when).is_ok_and(|age| age > KEEP_FOR);
        if i >= KEEP || old {
            let _ = std::fs::remove_file(path);
        } else {
            keep.push(path);
        }
    }
    let sender = std::thread::Builder::new().name("decibell-crash-send".into()).spawn(move || {
        for path in keep {
            let path = if path.extension().is_some_and(|x| x == "native") {
                match reporter.native_to_envelope(&path) {
                    Some(envelope) => envelope,
                    None => continue,
                }
            } else {
                path
            };
            send_file(&reporter.config, &path);
        }
    });
    if let (Ok(sender), Ok(mut pending)) = (sender, PENDING.lock()) {
        *pending = Some(sender);
    }
}

impl Reporter {
    /// A native crash record (`native.rs`) as an envelope beside it; the record goes.
    fn native_to_envelope(&self, record: &Path) -> Option<PathBuf> {
        let text = std::fs::read_to_string(record).ok();
        let _ = std::fs::remove_file(record);
        let record = native::Record::parse(&text?)?;
        let scrubber = self.scrubber();
        let mut image = record.image.clone();
        image.code_file = self.image.code_file.clone();
        let module = record.module.as_deref().map(|m| scrubber.text(m.rsplit('/').next().unwrap_or(m)));
        let ours = image.contains(record.pc);
        let place = match (&module, ours) {
            (_, true) => "in decibell".to_string(),
            (Some(module), false) => format!("in {module}"),
            (None, false) => "outside any mapped file".to_string(),
        };
        let mut reporter = Reporter { config: self.config.clone(), image, tags: self.tags.clone() };
        // The record's own release: the binary may have been updated since.
        reporter.tags["native_crash"] = json!("true");
        let mut event = reporter.event(
            Exception {
                kind: record.name(),
                value: format!("{} at 0x{:x} {place}", record.name(), record.fault),
                mechanism: json!({"type": "signalhandler", "handled": false, "meta": {"signal": {"number": record.signal, "name": record.name()}}}),
            },
            &[record.pc],
            None,
            "<native>",
            record.time as f64,
        );
        if !record.release.is_empty() {
            event["release"] = json!(record.release);
        }
        self.write(&event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dsns() {
        let dsn = Dsn::parse("https://abc123@o42.ingest.de.sentry.io/4507").unwrap();
        assert_eq!(dsn.envelope_url().as_str(), "https://o42.ingest.de.sentry.io/api/4507/envelope/");
        assert!(dsn.auth_header().contains("sentry_key=abc123"));
        let prefixed = Dsn::parse("https://k@sentry.example.org/prefix/9").unwrap();
        assert_eq!(prefixed.envelope_url().as_str(), "https://sentry.example.org/prefix/api/9/envelope/");
        for bad in ["", "https://o1.ingest.sentry.io/1", "https://k@o1.ingest.sentry.io/", "ftp://k@h/1", "https://k@h/abc"] {
            assert!(Dsn::parse(bad).is_none(), "{bad}");
        }
        assert!(!dsn.redacted().contains("abc123"));
    }

    #[test]
    fn install_ids_are_uuids_and_persist_beside_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"credentials_v2":"sealed","settings":{"theme":"matinee","crash_reporting_enabled":true}}"#,
        )
        .unwrap();
        assert_eq!(boot_settings(dir.path()), (true, None));
        let id = persist_install_id(dir.path());
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        let json: Value = serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
        assert_eq!(json["credentials_v2"], "sealed", "other fields kept");
        assert_eq!(json["settings"]["theme"], "matinee");
        assert_eq!(boot_settings(dir.path()), (true, Some(id)));
        std::fs::write(dir.path().join("config.json"), r#"{"settings":{"crash_reporting_enabled":false}}"#).unwrap();
        assert!(!boot_settings(dir.path()).0, "the toggle off");
        let empty = tempfile::tempdir().unwrap();
        assert!(boot_settings(empty.path()).0, "default on, as Electron");
    }

    /// The hook in a child process (it aborts on a fatal panic, so never in the test binary
    /// itself): panics on the core's guarded threads, its runtime's and inside
    /// `decibell_panic::catch` are reported as recovered and the process carries on; a panic on
    /// any other thread is reported as fatal and ends the process (SIGABRT, as the aborting
    /// profile did).
    #[cfg(unix)]
    #[test]
    fn caught_panics_carry_on_and_the_rest_end_the_process() {
        const TEST: &str = "crash::tests::caught_panics_carry_on_and_the_rest_end_the_process";
        if let Some(mode) = std::env::var_os("DECIBELL_TEST_HOOK_CHILD") {
            let dir = PathBuf::from(std::env::var_os("DECIBELL_TEST_HOOK_DIR").unwrap());
            install_panic_hook();
            // A DSN nobody listens on: the reports stay on disk for the parent to read.
            let dsn = Dsn::parse("http://0123456789abcdef@127.0.0.1:9/42").unwrap();
            init(Config { dir, dsn, install_id: "test".into() });
            let on = |name: &str, f: fn()| std::thread::Builder::new().name(name.into()).spawn(f).unwrap().join();
            if mode == "caught" {
                // As the core's receive loop guards its body.
                let core = on("decibell-video-recv", || {
                    assert!(std::panic::catch_unwind(|| panic!("core worker")).is_err());
                });
                let runtime = on("decibell-rt-blocking", || {
                    assert!(std::panic::catch_unwind(|| panic!("runtime task")).is_err());
                });
                let ours = on("decibell-media", || {
                    assert!(decibell_panic::catch(|| panic!("guarded section")).is_err());
                });
                assert!(core.is_ok() && runtime.is_ok() && ours.is_ok());
                std::thread::sleep(Duration::from_millis(300));
                println!("ALIVE");
                std::process::exit(0);
            }
            // Ours, unguarded: the process ends.
            let _ = on("decibell-media", || panic!("unguarded"));
            println!("STILL RUNNING");
            std::process::exit(0);
        }
        use std::os::unix::process::ExitStatusExt as _;
        let run = |mode: &str| {
            let dir = tempfile::tempdir().unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
                .env("DECIBELL_TEST_HOOK_CHILD", mode)
                .env("DECIBELL_TEST_HOOK_DIR", dir.path())
                .output()
                .unwrap();
            let events: Vec<Value> = std::fs::read_dir(dir.path())
                .map(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "envelope")).collect::<Vec<_>>())
                .unwrap_or_default()
                .iter()
                .map(|e| serde_json::from_slice::<Value>(std::fs::read(e.path()).unwrap().split(|b| *b == b'\n').nth(2).unwrap()).unwrap())
                .collect();
            (out, events, dir)
        };
        let (out, events, _dir) = run("caught");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && stdout.contains("ALIVE"), "the process carried on: {} {stdout}", out.status);
        let mut recovered: Vec<(String, String)> = events
            .iter()
            .map(|e| (e["tags"]["thread"].as_str().unwrap().to_string(), e["tags"]["recovered"].as_str().unwrap_or("").to_string()))
            .collect();
        recovered.sort();
        assert_eq!(
            recovered,
            [("decibell-media".into(), "guarded".into()), ("decibell-rt-blocking".into(), "runtime".into()), ("decibell-video-recv".into(), "core-thread".into())],
            "one recovered report per caught panic"
        );
        assert!(events.iter().all(|e| e["level"] == "error" && e["exception"]["values"][0]["mechanism"]["handled"] == true));

        let (out, events, _dir) = run("fatal");
        assert_eq!(out.status.signal(), Some(libc::SIGABRT), "a panic nothing catches ends the process: {}", out.status);
        assert!(!String::from_utf8_lossy(&out.stdout).contains("STILL RUNNING"));
        assert_eq!(events.len(), 1, "one fatal report");
        assert_eq!(events[0]["level"], "fatal");
        assert_eq!(events[0]["tags"]["thread"], "decibell-media");
        assert_eq!(events[0]["exception"]["values"][0]["mechanism"]["handled"], false);
    }

    #[test]
    fn dates_and_envelopes() {
        let date = rfc3339_now();
        assert_eq!(date.len(), 20);
        assert!(date.starts_with("20") && date.ends_with('Z'));
        let event = json!({"event_id": "0123456789abcdef0123456789abcdef", "level": "fatal"});
        let text = String::from_utf8(envelope(&event)).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let item: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(item["type"], "event");
        assert_eq!(item["length"].as_u64().unwrap() as usize, lines[2].len());
        assert_eq!(serde_json::from_str::<Value>(lines[2]).unwrap(), event);
    }
}

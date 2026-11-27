//! Crash reports: `sentry.ts` and the renderer's `lib/sentry.ts`, without a Sentry SDK.
//!
//! **When** (Electron's three gates): a packaged ship build (not a development build, not a ship
//! binary outside a package), a DSN baked in (`resources/sentry.json`, written by the release
//! workflow from the `SENTRY_DSN` secret), and the Privacy toggle on (`crash_reporting_enabled` in
//! `config.json`, default on; read at start, so a change takes effect at the next start, as in
//! Electron). Otherwise nothing is installed: no hook, no handler, never a socket.
//!
//! **What** is reported:
//! - a **panic** (the ship profile aborts on panic): a hook writes the report to
//!   `<profile>/crash-reports/<event id>.envelope`, then sends it within [`SEND_BUDGET`] (Electron's
//!   flush timeout, `@sentry/node`'s default `shutdownTimeout`) before the process aborts; one that
//!   couldn't be sent goes at the next start;
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

/// Starts reporting: the panic hook, the native handlers, and the sending of what earlier runs
/// left. Once per process.
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
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // The usual message on stderr first, as before.
        previous(info);
        if REPORTING.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(reporter) = REPORTER.get() {
            reporter.panic(info);
        }
    }));
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
/// `DECIBELL_TEST_PANIC=<message>` panics on a thread named `decibell-test-panic` (the ship profile
/// aborts); `segv` writes through a null pointer; `wait-pending` waits for the reports earlier runs
/// left to be sent, then exits. `DECIBELL_TEST_CRASH_NAMES=a,b` registers names first, as a signed-in
/// app does.
#[cfg(feature = "test-hooks")]
pub fn panic_for_test() {
    let Ok(what) = std::env::var("DECIBELL_TEST_PANIC") else { return };
    // The names the app would have registered after a sign-in (`set_names`).
    if let Ok(names) = std::env::var("DECIBELL_TEST_CRASH_NAMES") {
        set_names(names.split(',').map(str::to_string).collect());
    }
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
        message => {
            let message = message.to_string();
            let _ = std::thread::Builder::new()
                .name("decibell-test-panic".into())
                .spawn(move || panic!("{message}"))
                .map(|t| t.join());
        }
    }
}

/// The sender of earlier runs' reports while it runs (the release tests wait for it).
static PENDING: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

impl Reporter {
    fn scrubber(&self) -> scrub::Scrubber {
        scrub::Scrubber::from_env(NAMES.lock().map(|n| n.clone()).unwrap_or_default())
    }

    fn panic(&self, info: &std::panic::PanicHookInfo<'_>) {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "Box<dyn Any>".into());
        let scrubber = self.scrubber();
        let location = info.location().map(|l| scrubber.location(l.file(), l.line()));
        let mut frames = Vec::new();
        backtrace::trace(|frame| {
            frames.push(frame.ip() as usize);
            frames.len() < 100
        });
        let thread = std::thread::current().name().unwrap_or("<unnamed>").to_string();
        let event = self.event(
            Exception { kind: "panic", value: scrubber.text(&message), mechanism: json!({"type": "panic", "handled": false}) },
            &frames,
            location,
            &thread,
            now_secs(),
        );
        let Some(path) = self.write(&event) else { return };
        send_now(self.config.clone(), path, SEND_BUDGET);
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
            "contexts": { "os": { "name": os_name(), "version": install::os_release() } },
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

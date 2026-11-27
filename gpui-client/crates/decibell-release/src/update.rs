//! The updater: `update.ts`'s state machine without electron-updater. One check at a time on a
//! thread that exists only while it runs; each state goes to the caller as it is reached, in
//! `update.ts`' order: checking → not-available, or available → (self-update) downloading → downloaded,
//! or error.
//!
//! Modes (`detectMode` in `update.ts`):
//! - **disabled**: not a packaged ship build (Electron: `!app.isPackaged`).
//! - **self-update**: the Windows installer's install, or a Linux AppImage (`APPIMAGE` set, as the
//!   AppImage runtime does) whose directory is writable.
//! - **notify-only**: every other package (the tarball, the `.deb`, the AUR package: the package
//!   manager owns the files): "<v> is available." and "Open release page".
//!
//! Trust (gpui-client/PACKAGING.md, "Updates"): the feed comes over HTTPS from github.com; with a
//! public key baked in, its Ed25519 signature must verify; the download must match the feed's size
//! and SHA-512 before it is staged, and again before it is applied. Nothing is ever run that
//! didn't pass both.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::apply::{Failed, Staged, file_sha512};
use crate::feed::{self, Artifact};
use crate::http::{Client, Rule};
use crate::install::{self, Kind};
use crate::version::Version;

/// The hosts the updater may contact: GitHub and the two hosts its release downloads redirect to.
pub const HOSTS: [&str; 3] = ["github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"];
/// `kickoffInitialCheck`: the first check, this long after the window's first frame.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(5);
/// Electron checks once per start (and on "Check now"); `None` keeps that. A `Some` re-arms the
/// one-shot timer after each check (gpui-client/PACKAGING.md lists it as the owner's call).
pub const RECHECK_EVERY: Option<Duration> = None;
/// Resumed attempts within one check before a cut download is reported.
const DOWNLOAD_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    SelfUpdate,
    NotifyOnly,
    Disabled,
}

/// `UpdateStatus` of `update.ts`.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    NotAvailable { checked_at: i64 },
    Available { version: String },
    Downloading { pct: f32, version: String },
    Downloaded { version: String },
    Error { message: String },
}

/// `detectMode()`.
pub fn mode() -> Mode {
    if !install::packaged() {
        return Mode::Disabled;
    }
    match install::current().kind {
        Some(Kind::Nsis) if cfg!(windows) => Mode::SelfUpdate,
        Some(Kind::AppImage) if cfg!(target_os = "linux") && appimage_writable() => Mode::SelfUpdate,
        _ => Mode::NotifyOnly,
    }
}

fn appimage_writable() -> bool {
    let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let Some(dir) = path.parent() else { return false };
        let Ok(dir) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else { return false };
        // SAFETY: a NUL-terminated path; access has no other preconditions.
        path.is_absolute() && path.is_file() && unsafe { libc::access(dir.as_ptr(), libc::W_OK) } == 0
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Where and how to check.
#[derive(Debug, Clone)]
pub struct Settings {
    pub feed: Url,
    pub signature: Url,
    pub repo: String,
    pub key: Option<[u8; 32]>,
    pub rule: Rule,
    pub hosts: Vec<String>,
    /// `<profile>/update`: the download, its record, the failed version.
    pub dir: PathBuf,
    pub current: Version,
    pub mode: Mode,
}

impl Settings {
    /// The release build's: GitHub, the baked repository, channel and key.
    pub fn for_profile(profile: &Path) -> Settings {
        let repo = env!("DECIBELL_UPDATE_REPO").to_string();
        let channel = env!("DECIBELL_UPDATE_CHANNEL");
        #[allow(unused_mut)]
        let mut origin = "https://github.com".to_string();
        #[allow(unused_mut)]
        let mut rule = Rule::hosts(&HOSTS);
        #[cfg(feature = "test-hooks")]
        if let Ok(test_origin) = std::env::var("DECIBELL_TEST_UPDATE_ORIGIN") {
            origin = test_origin.trim_end_matches('/').to_string();
            rule.loopback_http = true;
        }
        let base = format!("{origin}/{repo}/releases/download/{channel}/{}", feed::FEED_FILE);
        Settings {
            feed: Url::parse(&base).expect("a URL from constants"),
            signature: Url::parse(&format!("{base}.sig")).expect("a URL from constants"),
            repo,
            key: feed::baked_key(),
            hosts: HOSTS.iter().map(|h| h.to_string()).collect(),
            rule,
            dir: profile.join("update"),
            current: Version::parse(crate::VERSION).expect("build.rs checked the version"),
            mode: mode(),
        }
    }
}

type Report = Box<dyn Fn(Status) + Send + Sync>;

/// One updater per process.
#[derive(Clone)]
pub struct Updater {
    settings: Arc<Settings>,
    busy: Arc<AtomicBool>,
    release_page: Arc<Mutex<String>>,
}

impl Updater {
    pub fn new(settings: Settings) -> Updater {
        let page = format!("https://github.com/{}/releases", settings.repo);
        Updater { settings: Arc::new(settings), busy: Arc::new(AtomicBool::new(false)), release_page: Arc::new(Mutex::new(page)) }
    }

    pub fn mode(&self) -> Mode {
        self.settings.mode
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// The page "Open release page" opens: the last feed's release, else the release list.
    pub fn release_page(&self) -> String {
        self.release_page.lock().map(|p| p.clone()).unwrap_or_default()
    }

    /// Whether a check is running.
    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// Starts a check on its own thread unless one runs or updates are disabled (`false`). Every
    /// state reached goes to `report`, from that thread; the thread ends with the check.
    pub fn check(&self, report: Report) -> bool {
        if self.settings.mode == Mode::Disabled || self.busy.swap(true, Ordering::AcqRel) {
            return false;
        }
        let (settings, busy, page) = (self.settings.clone(), self.busy.clone(), self.release_page.clone());
        let spawned = std::thread::Builder::new().name("decibell-update".into()).spawn(move || {
            report(Status::Checking);
            let last = match run(&settings, &page, &report) {
                Ok(status) => status,
                Err(message) => Status::Error { message },
            };
            busy.store(false, Ordering::Release);
            report(last);
        });
        if spawned.is_err() {
            self.busy.store(false, Ordering::Release);
            return false;
        }
        true
    }

    /// The check, on the caller's thread (tests).
    pub fn check_blocking(&self, report: &dyn Fn(Status)) -> Status {
        report(Status::Checking);
        match run(&self.settings, &self.release_page, report) {
            Ok(status) => status,
            Err(message) => Status::Error { message },
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// Returns the last state (reported by the caller); intermediate ones go to `report`.
fn run(settings: &Settings, page: &Mutex<String>, report: &dyn Fn(Status)) -> Result<Status, String> {
    let client = Client::new(settings.rule.clone(), format!("Decibell/{} ({})", crate::VERSION, install::platform()));
    // The channel's feed is written only by native releases; until the first one there is none,
    // which is "no update", whatever else the repository has released (Electron's `ev*` tags).
    let feed_bytes = match fetch(&client, &settings.feed, feed::FEED_CAP) {
        Err(e) if e == NOT_FOUND => {
            log::info!("[update] no native release has been published yet ({})", settings.feed);
            return Ok(Status::NotAvailable { checked_at: now_ms() });
        }
        other => other?,
    };
    let signature = match settings.key {
        Some(_) => Some(fetch(&client, &settings.signature, 1024).map_err(|e| format!("the update feed's signature: {e}"))?),
        None => None,
    };
    feed::verify(&feed_bytes, signature.as_deref(), settings.key.as_ref())?;
    let (feed, version, artifact) = feed::parse(&feed_bytes, &settings.repo, &settings.hosts)?;
    if let Ok(mut p) = page.lock() {
        *p = feed::release_page(&feed, &settings.repo);
    }
    let text = version.to_string();
    if version <= settings.current || Failed::is(&settings.dir, &text) {
        return Ok(Status::NotAvailable { checked_at: now_ms() });
    }
    report(Status::Available { version: text.clone() });
    if settings.mode != Mode::SelfUpdate {
        return Ok(Status::Available { version: text });
    }
    let artifact = artifact.ok_or("this update has no package for this system")?;
    download(&client, settings, &text, &artifact, report)?;
    Ok(Status::Downloaded { version: text })
}

const NOT_FOUND: &str = "the update feed answered 404";

fn fetch(client: &Client, url: &Url, cap: usize) -> Result<Vec<u8>, String> {
    let (_, response) = client.get(url, &[]).map_err(|e| e.to_string())?;
    if response.status != 200 {
        return Err(format!("the update feed answered {}", response.status));
    }
    response.read_capped(cap).map_err(|e| e.to_string())
}

/// What the partial download is of (so a resumed download never mixes two releases).
#[derive(Serialize, Deserialize, PartialEq)]
struct Partial {
    url: String,
    size: u64,
    sha512: String,
}

/// The file name a download lands under: the URL's last segment, plain characters only.
fn file_name(artifact: &Artifact) -> Result<String, String> {
    let url = Url::parse(&artifact.url).map_err(|_| "the update's URL is malformed")?;
    let name = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or_default().to_string();
    let plain = !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if plain { Ok(name) } else { Err("the update's file name is malformed".into()) }
}

/// Downloads (resuming a partial download of the same file), checks size and SHA-512, stages it.
fn download(client: &Client, settings: &Settings, version: &str, artifact: &Artifact, report: &dyn Fn(Status)) -> Result<(), String> {
    let dir = &settings.dir;
    std::fs::create_dir_all(dir).map_err(|e| format!("couldn't create {}: {e}", dir.display()))?;
    let name = file_name(artifact)?;
    let expected = feed::sha512(artifact)?;

    // Already here from an earlier check (electron-updater's cached download)?
    if let Some(staged) = Staged::read(dir)
        && staged.version == version
        && staged.sha512 == artifact.sha512
        && staged.verify(dir)
    {
        return Ok(());
    }
    Staged::clear(dir);

    let partial = dir.join(format!("{name}.partial"));
    let record = dir.join(format!("{name}.partial.json"));
    let this = Partial { url: artifact.url.clone(), size: artifact.size, sha512: artifact.sha512.clone() };
    let same = std::fs::read(&record).ok().and_then(|b| serde_json::from_slice::<Partial>(&b).ok()).is_some_and(|p| p == this);
    if !same {
        let _ = std::fs::remove_file(&partial);
        crate::apply::write_atomic(&record, &serde_json::to_vec(&this).unwrap_or_default()).map_err(|e| e.to_string())?;
    }

    let progress = Progress::new(version, artifact.size, report);
    let mut last_error = String::new();
    for attempt in 0..DOWNLOAD_ATTEMPTS {
        match fetch_into(client, artifact, &partial, &progress) {
            Ok(()) => {
                last_error.clear();
                break;
            }
            Err(e) => {
                log::warn!("[update] download attempt {} failed: {e}", attempt + 1);
                last_error = e;
            }
        }
    }
    if !last_error.is_empty() {
        return Err(format!("the download was interrupted ({last_error}); it resumes at the next check"));
    }
    let got = file_sha512(&partial).map_err(|e| e.to_string())?;
    if got != expected {
        let _ = std::fs::remove_file(&partial);
        let _ = std::fs::remove_file(&record);
        return Err("the download doesn't match its checksum".into());
    }
    let landed = dir.join(&name);
    std::fs::rename(&partial, &landed).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&record);
    Staged { version: version.to_string(), file: name, sha512: artifact.sha512.clone(), timeouts: 0 }
        .write(dir)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// One request for what `partial` lacks, appended to it.
fn fetch_into(client: &Client, artifact: &Artifact, partial: &Path, progress: &Progress) -> Result<(), String> {
    let have = std::fs::metadata(partial).map(|m| m.len()).unwrap_or(0);
    if have == artifact.size {
        return Ok(());
    }
    if have > artifact.size {
        let _ = std::fs::remove_file(partial);
        return Err("the partial download is longer than the file".into());
    }
    let url = Url::parse(&artifact.url).map_err(|_| "the update's URL is malformed")?;
    let range = format!("bytes={have}-");
    let headers: Vec<(&str, &str)> = if have > 0 { vec![("Range", range.as_str())] } else { Vec::new() };
    let (_, mut response) = client.get(&url, &headers).map_err(|e| e.to_string())?;
    let mut file = match response.status {
        206 if have > 0 => {
            // `bytes <have>-<last>/<size>`: the rest of this very file.
            let fits = response
                .header("content-range")
                .and_then(|v| v.strip_prefix("bytes "))
                .and_then(|v| v.split_once('-'))
                .is_some_and(|(start, rest)| start.trim() == have.to_string() && rest.ends_with(&format!("/{}", artifact.size)));
            if !fits {
                let _ = std::fs::remove_file(partial);
                return Err("the server resumed at the wrong place".into());
            }
            OpenOptions::new().append(true).open(partial).map_err(|e| e.to_string())?
        }
        200 => File::create(partial).map_err(|e| e.to_string())?,
        416 => {
            let _ = std::fs::remove_file(partial);
            return Err("the server refused to resume".into());
        }
        status => return Err(format!("the download answered {status}")),
    };
    let mut written = if response.status == 206 { have } else { 0 };
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = response.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > artifact.size {
            return Err("the download is longer than the feed says".into());
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        progress.at(written);
    }
    file.sync_all().map_err(|e| e.to_string())?;
    if written != artifact.size {
        return Err(format!("the connection ended at {written} of {} bytes", artifact.size));
    }
    Ok(())
}

/// `download-progress`, at most once per whole percent or every 250 ms.
struct Progress<'a> {
    version: String,
    size: u64,
    report: &'a dyn Fn(Status),
    last: Mutex<(i64, Instant)>,
}

impl<'a> Progress<'a> {
    fn new(version: &str, size: u64, report: &'a dyn Fn(Status)) -> Self {
        Progress { version: version.to_string(), size, report, last: Mutex::new((-1, Instant::now())) }
    }

    fn at(&self, bytes: u64) {
        let pct = bytes as f64 * 100.0 / self.size.max(1) as f64;
        let Ok(mut last) = self.last.lock() else { return };
        if pct.floor() as i64 > last.0 || last.1.elapsed() >= Duration::from_millis(250) {
            *last = (pct.floor() as i64, Instant::now());
            (self.report)(Status::Downloading { pct: pct as f32, version: self.version.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::tests::{key, sign};
    use crate::http::tests::{loopback, serve};
    use base64::Engine as _;
    use sha2::Digest as _;
    use std::sync::atomic::AtomicUsize;

    const REPO: &str = "o/r";

    struct Release {
        payload: Vec<u8>,
        feed: Vec<u8>,
        sig: Vec<u8>,
    }

    fn release(origin: &str, version: &str, payload: Vec<u8>, sha_of: &[u8]) -> Release {
        let sha = base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(sha_of));
        let feed = format!(
            r#"{{"schema":1,"version":"{version}","release_page":"https://github.com/{REPO}/releases/tag/native-v{version}","files":{{"{}":{{"url":"{origin}/{REPO}/releases/download/native-v{version}/Decibell-{version}.bin","size":{},"sha512":"{sha}"}}}}}}"#,
            feed::artifact_key(),
            payload.len()
        )
        .into_bytes();
        let sig = sign(&key(), &feed);
        Release { payload, feed, sig }
    }

    /// A fixture GitHub: the feed and signature on the channel, the file on its release; the file
    /// answers ranges, and is cut after `cut_at` bytes on the first `cuts` requests.
    fn github(release: Arc<Mutex<Release>>, cut_at: usize, cuts: usize) -> (crate::http::tests::Fixture, Arc<AtomicUsize>) {
        let downloads = Arc::new(AtomicUsize::new(0));
        let counter = downloads.clone();
        let fixture = serve(move |head, _| {
            let release = release.lock().unwrap();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            let ok = |body: &[u8]| [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes(), body.to_vec()].concat();
            if path.ends_with("/native-latest/decibell-native.json") {
                return ok(&release.feed);
            }
            if path.ends_with("/native-latest/decibell-native.json.sig") {
                return ok(&release.sig);
            }
            if path.contains("/releases/download/native-v") {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let from = head
                    .lines()
                    .find_map(|l| l.strip_prefix("Range: bytes="))
                    .and_then(|r| r.trim_end_matches('-').trim().parse::<usize>().ok())
                    .unwrap_or(0);
                let total = release.payload.len();
                let body = &release.payload[from..];
                let status = if from > 0 {
                    format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {from}-{}/{total}\r\n", total - 1)
                } else {
                    "HTTP/1.1 200 OK\r\n".to_string()
                };
                let mut reply = format!("{status}Content-Length: {}\r\n\r\n", body.len()).into_bytes();
                if n < cuts {
                    // The connection drops partway: a short body.
                    reply.extend_from_slice(&body[..cut_at.min(body.len())]);
                } else {
                    reply.extend_from_slice(body);
                }
                return reply;
            }
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec()
        });
        (fixture, downloads)
    }

    fn settings(origin: &str, dir: &Path, mode: Mode, key_baked: bool) -> Settings {
        let base = format!("{origin}/{REPO}/releases/download/native-latest/{}", feed::FEED_FILE);
        Settings {
            feed: Url::parse(&base).unwrap(),
            signature: Url::parse(&format!("{base}.sig")).unwrap(),
            repo: REPO.into(),
            key: key_baked.then(|| key().verifying_key().to_bytes()),
            rule: loopback(),
            hosts: vec!["github.com".into()],
            dir: dir.to_path_buf(),
            current: Version::parse("0.0.1").unwrap(),
            mode,
        }
    }

    fn statuses(updater: &Updater) -> (Vec<Status>, Status) {
        let seen = Mutex::new(Vec::new());
        let last = updater.check_blocking(&|s| seen.lock().unwrap().push(s));
        (seen.into_inner().unwrap(), last)
    }

    /// The same payload, its URLs on the fixture's origin (known once it listens).
    fn republish(rel: &Arc<Mutex<Release>>, origin: &str) {
        let payload = rel.lock().unwrap().payload.clone();
        *rel.lock().unwrap() = release(origin, "99.1.0", payload.clone(), &payload);
    }

    fn pending_release(origin_holder: &str) -> Arc<Mutex<Release>> {
        let payload: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        Arc::new(Mutex::new(release(origin_holder, "99.1.0", payload.clone(), &payload)))
    }

    /// A fixture repository with Electron's releases interleaved with native ones: whichever
    /// release GitHub flags as latest, the native updater reads only its own channel, never
    /// `releases/latest`, never an `ev*` tag or Electron's `latest*.yml`.
    #[test]
    fn electron_releases_and_the_latest_flag_are_ignored() {
        for latest in ["ev0.9.9", "native-v99.1.0"] {
            let dir = tempfile::tempdir().unwrap();
            let rel = pending_release("");
            let latest_tag = latest.to_string();
            let inner = github(rel.clone(), 0, 0).0;
            // A front that answers what Electron's updater asks for and passes the rest on.
            let base = inner.base.clone();
            let fixture = serve(move |head, body| {
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                if path.ends_with("/releases/latest") {
                    return format!("HTTP/1.1 302 Found\r\nLocation: /{REPO}/releases/tag/{latest_tag}\r\nContent-Length: 0\r\n\r\n").into_bytes();
                }
                if path.contains("/releases/download/ev") || path.ends_with(".yml") {
                    let yml = b"version: 0.9.9\npath: Decibell-0.9.9.AppImage\n";
                    return [format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", yml.len()).into_bytes(), yml.to_vec()].concat();
                }
                use std::io::{Read as _, Write as _};
                let mut upstream = std::net::TcpStream::connect(base.trim_start_matches("http://")).unwrap();
                upstream.write_all(head.as_bytes()).unwrap();
                upstream.write_all(b"\r\n").unwrap();
                upstream.write_all(body).unwrap();
                let mut reply = Vec::new();
                upstream.read_to_end(&mut reply).unwrap();
                reply
            });
            republish(&rel, &fixture.base);
            let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
            let (_, last) = statuses(&updater);
            assert_eq!(last, Status::Downloaded { version: "99.1.0".into() }, "latest = {latest}");
            for request in fixture.requests.lock().unwrap().iter() {
                let path = request.split_whitespace().nth(1).unwrap_or("");
                assert!(!path.ends_with("/releases/latest") && !path.contains("/ev") && !path.ends_with(".yml"), "latest = {latest}: asked for {path}");
            }
        }
    }

    #[test]
    fn no_native_release_yet_is_no_update() {
        let dir = tempfile::tempdir().unwrap();
        // Only Electron's releases exist: the channel's feed is missing.
        let fixture = serve(|_, _| b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec());
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (seen, last) = statuses(&updater);
        assert_eq!(seen, vec![Status::Checking]);
        assert!(matches!(last, Status::NotAvailable { .. }), "{last:?}");
    }

    #[test]
    fn no_update_when_the_feed_is_not_newer() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Arc::new(Mutex::new(release("", "0.0.1", vec![], &[])));
        let (fixture, downloads) = github(rel.clone(), 0, 0);
        *rel.lock().unwrap() = release(&fixture.base, "0.0.1", b"x".to_vec(), b"x");
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (seen, last) = statuses(&updater);
        assert_eq!(seen, vec![Status::Checking]);
        assert!(matches!(last, Status::NotAvailable { .. }), "{last:?}");
        assert_eq!(downloads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn available_downloaded_verified_staged() {
        let dir = tempfile::tempdir().unwrap();
        let rel = pending_release("");
        let (fixture, _) = github(rel.clone(), 0, 0);
        republish(&rel, &fixture.base);
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (seen, last) = statuses(&updater);
        assert_eq!(seen[0], Status::Checking);
        assert_eq!(seen[1], Status::Available { version: "99.1.0".into() });
        assert!(seen[2..].iter().all(|s| matches!(s, Status::Downloading { .. })), "{seen:?}");
        assert!(matches!(seen.last(), Some(Status::Downloading { pct, .. }) if *pct >= 99.9));
        assert_eq!(last, Status::Downloaded { version: "99.1.0".into() });
        let staged = Staged::read(dir.path()).expect("staged");
        assert!(staged.verify(dir.path()));
        assert_eq!(std::fs::read(staged.path(dir.path())).unwrap(), rel.lock().unwrap().payload);
        assert_eq!(updater.release_page(), "https://github.com/o/r/releases/tag/native-v99.1.0");
        // A second check finds it staged and downloads nothing.
        let requests = fixture.requests.lock().unwrap().len();
        let (_, last) = statuses(&updater);
        assert_eq!(last, Status::Downloaded { version: "99.1.0".into() });
        assert!(fixture.requests.lock().unwrap()[requests..].iter().all(|r| !r.contains("native-v99.1.0/")));
    }

    #[test]
    fn a_tampered_download_is_refused_and_nothing_is_staged() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Arc::new(Mutex::new(release("", "99.1.0", vec![], &[])));
        let (fixture, _) = github(rel.clone(), 0, 0);
        // The feed promises one file's hash; the server sends another of the same size.
        *rel.lock().unwrap() = release(&fixture.base, "99.1.0", b"evil bytes".to_vec(), b"good bytes");
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (_, last) = statuses(&updater);
        assert_eq!(last, Status::Error { message: "the download doesn't match its checksum".into() });
        assert!(Staged::read(dir.path()).is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "nothing left behind");
    }

    #[test]
    fn a_bad_signature_is_refused_before_anything_is_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Arc::new(Mutex::new(release("", "99.1.0", vec![], &[])));
        let (fixture, downloads) = github(rel.clone(), 0, 0);
        {
            let mut r = rel.lock().unwrap();
            *r = release(&fixture.base, "99.1.0", b"x".to_vec(), b"x");
            r.sig = sign(&ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]), &r.feed);
        }
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (seen, last) = statuses(&updater);
        assert_eq!(seen, vec![Status::Checking], "no 'available' for an unsigned feed");
        assert_eq!(last, Status::Error { message: "the update feed's signature doesn't match".into() });
        assert_eq!(downloads.load(Ordering::SeqCst), 0);
        // Without a baked key the same feed is accepted (HTTPS + SHA-512 only, as electron-updater).
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, false));
        let (_, last) = statuses(&updater);
        assert_eq!(last, Status::Downloaded { version: "99.1.0".into() });
    }

    #[test]
    fn a_cut_download_resumes_where_it_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let rel = pending_release("");
        // Two cuts in a row: the third attempt of the same check completes it, with ranges.
        let (fixture, downloads) = github(rel.clone(), 100_000, 2);
        republish(&rel, &fixture.base);
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (_, last) = statuses(&updater);
        assert_eq!(last, Status::Downloaded { version: "99.1.0".into() });
        assert_eq!(downloads.load(Ordering::SeqCst), 3);
        let requests = fixture.requests.lock().unwrap();
        let ranges: Vec<_> = requests.iter().filter_map(|r| r.lines().find(|l| l.starts_with("Range:"))).collect();
        assert_eq!(ranges, vec!["Range: bytes=100000-", "Range: bytes=200000-"]);
        assert!(Staged::read(dir.path()).unwrap().verify(dir.path()));
    }

    #[test]
    fn a_download_cut_on_every_attempt_resumes_at_the_next_check() {
        let dir = tempfile::tempdir().unwrap();
        let rel = pending_release("");
        let (fixture, _) = github(rel.clone(), 50_000, 3);
        republish(&rel, &fixture.base);
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (_, last) = statuses(&updater);
        assert!(matches!(&last, Status::Error { message } if message.starts_with("the download was interrupted")), "{last:?}");
        let partial = std::fs::metadata(dir.path().join("Decibell-99.1.0.bin.partial")).unwrap().len();
        assert_eq!(partial, 150_000, "three cut attempts kept 3 × 50,000 bytes");
        let (_, last) = statuses(&updater);
        assert_eq!(last, Status::Downloaded { version: "99.1.0".into() });
        let last_range = fixture.requests.lock().unwrap().iter().rev().find_map(|r| r.lines().find(|l| l.starts_with("Range:")).map(str::to_string));
        assert_eq!(last_range.as_deref(), Some("Range: bytes=150000-"));
    }

    #[test]
    fn notify_only_reports_available_and_downloads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Arc::new(Mutex::new(release("", "99.1.0", vec![], &[])));
        let (fixture, downloads) = github(rel.clone(), 0, 0);
        *rel.lock().unwrap() = release(&fixture.base, "99.1.0", b"x".to_vec(), b"x");
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::NotifyOnly, true));
        let (seen, last) = statuses(&updater);
        assert_eq!(seen, vec![Status::Checking, Status::Available { version: "99.1.0".into() }]);
        assert_eq!(last, Status::Available { version: "99.1.0".into() });
        assert_eq!(downloads.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_version_that_failed_to_start_is_not_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Arc::new(Mutex::new(release("", "99.1.0", vec![], &[])));
        let (fixture, _) = github(rel.clone(), 0, 0);
        *rel.lock().unwrap() = release(&fixture.base, "99.1.0", b"x".to_vec(), b"x");
        Failed { version: "99.1.0".into(), reason: "r".into(), shown: true }.write(dir.path());
        let updater = Updater::new(settings(&fixture.base, dir.path(), Mode::SelfUpdate, true));
        let (_, last) = statuses(&updater);
        assert!(matches!(last, Status::NotAvailable { .. }), "{last:?}");
    }

    #[test]
    fn one_check_at_a_time_and_none_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        // A feed server that never answers keeps the first check busy.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let updater = Updater::new(settings(&origin, dir.path(), Mode::SelfUpdate, false));
        assert!(updater.check(Box::new(|_| {})));
        assert!(updater.busy());
        assert!(!updater.check(Box::new(|_| {})), "a second check while one runs is ignored");
        let disabled = Updater::new(settings(&origin, dir.path(), Mode::Disabled, false));
        assert!(!disabled.check(Box::new(|_| panic!("no state in a disabled build"))));
        drop(listener);
    }
}

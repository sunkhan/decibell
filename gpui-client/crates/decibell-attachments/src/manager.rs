//! The downloads manager behind the Transfers panel's Downloads tab (`electron/main/downloads.ts`,
//! design `docs/superpowers/specs/2026-10-04-transfers-panel-design.md`), as a state machine with
//! no UI: the app shows what [`DownloadEvent`]s say and calls the commands.
//!
//! A download streams straight to `<final>.part` and is renamed when complete. The destination
//! is either the download folder (a free name, reserved by creating its `.part`) or a path the
//! user picked in a save dialog ([`Destination::Chosen`]); those are the only places the
//! manager writes, which is what main's write-approval rule guaranteed across the IPC boundary.
//! Pause stops the transfer and keeps the `.part`; resume continues from its size with a Range
//! request (attachment ids are immutable). Encrypted files decrypt on the fly through
//! [`crate::fetch`]; the key is snapshotted at start (the registry is cleared on disconnect)
//! and, when the app supplies a [`KeyWrap`] (the OS keychain), kept wrapped in the record until
//! the download finishes so a paused one resumes after a restart. Records persist in
//! `downloads.json` (the last 100, written atomically, debounced, on state changes only);
//! active downloads become paused on quit and are not resumed by themselves.
//!
//! Events: `Changed` about 4× a second per active download and at once on every state change,
//! `Removed` when rows go. Speed is an EWMA over ~0.5 s samples.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

use crate::names::{sanitize_filename, unique_path, with_part};
use crate::{AttachmentKey, AttachmentKind, Attachments, Error, FileKey};

const MAX_RECORDS: usize = 100;
const EMIT_INTERVAL: Duration = Duration::from_millis(250);
const SPEED_SAMPLE: Duration = Duration::from_millis(500);
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
const WRITE_BUFFER: usize = 256 * 1024;
const STORE_FILE: &str = "downloads.json";

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DownloadState {
    Active,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl DownloadState {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// Where the file came from, for the row's labels and "Go to message".
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct DownloadContext {
    pub server_name: String,
    pub channel_id: String,
    pub channel_name: String,
    pub message_id: i64,
    /// The sender's name when the download started (a fallback label).
    pub sender: String,
    /// The sender's account: the UI draws their current name.
    pub sender_uid: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct EncryptedInfo {
    chunk_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wrapped_key: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct DownloadRecord {
    id: String,
    server_id: String,
    attachment_id: i64,
    /// Display name: the final file's name.
    filename: String,
    /// Final path; the bytes live at `path.part` until done.
    path: PathBuf,
    /// Picked in a save dialog: the user agreed to replace what is there.
    #[serde(default)]
    chosen: bool,
    mime: String,
    kind: AttachmentKind,
    total_bytes: u64,
    received_bytes: u64,
    state: DownloadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    started_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_at: Option<i64>,
    #[serde(default)]
    context: DownloadContext,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encrypted: Option<EncryptedInfo>,
}

/// One row of the panel: the record without its key, plus live numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadView {
    pub id: String,
    pub server_id: String,
    pub attachment_id: i64,
    pub filename: String,
    pub mime: String,
    pub kind: AttachmentKind,
    pub total_bytes: u64,
    pub received_bytes: u64,
    pub state: DownloadState,
    /// "Failed — …" wording.
    pub error: Option<String>,
    /// ms since the epoch.
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub context: DownloadContext,
    pub speed_bps: f64,
    /// Seconds left, -1 when unknown.
    pub eta_s: i64,
    /// Finished, but the file is gone from disk (checked by [`DownloadManager::list`]).
    pub missing: bool,
    /// An encrypted-channel file (saved as plaintext).
    pub encrypted: bool,
}

// Moved once into the callback, about 4 times a second per active download: not worth a heap
// allocation per event to shrink.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum DownloadEvent {
    Changed(DownloadView),
    Removed(Vec<String>),
}

/// Where a new download goes.
#[derive(Clone, Debug)]
pub enum Destination {
    /// The download folder (configured, else the OS Downloads folder), under a free name.
    Folder,
    /// A path the user just picked in a save dialog ("Save as…", or "Ask where to save each
    /// file"). Replaces what is there when the download finishes.
    Chosen(PathBuf),
}

#[derive(Clone, Debug)]
pub struct StartArgs {
    pub server_id: String,
    pub attachment_id: i64,
    /// The sender's file name (sanitised here).
    pub filename: String,
    pub mime: String,
    pub kind: AttachmentKind,
    /// Plaintext size from the attachment row / envelope (a sealed file's key overrides it).
    pub size_bytes: u64,
    pub context: DownloadContext,
    pub destination: Destination,
}

/// Wraps a per-file key for a paused encrypted download (Electron: `safeStorage`). Return
/// `None` when no keychain is available: the key then lives only in memory.
pub trait KeyWrap: Send + Sync {
    fn wrap(&self, key: &[u8; 32]) -> Option<String>;
    fn unwrap(&self, wrapped: &str) -> Option<[u8; 32]>;
}

pub struct ManagerOptions {
    /// Where `downloads.json` lives (the app's user-data folder).
    pub state_dir: PathBuf,
    /// The OS Downloads folder.
    pub default_dir: PathBuf,
    pub key_wrap: Option<Arc<dyn KeyWrap>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Stop {
    None = 0,
    Pause = 1,
    Cancel = 2,
    Remove = 3,
    Quit = 4,
}

struct RunCtl {
    cancel: CancellationToken,
    stop: Arc<AtomicU8>,
    received: Arc<AtomicU64>,
    /// f64 bits.
    speed: Arc<AtomicU64>,
}

impl RunCtl {
    fn stop(&self, why: Stop) {
        self.stop.store(why as u8, Ordering::SeqCst);
        self.cancel.cancel();
    }
}

struct State {
    records: Vec<DownloadRecord>,
    runs: HashMap<String, RunCtl>,
    keys: HashMap<String, AttachmentKey>,
    dir: Option<PathBuf>,
    ask_each_time: bool,
    save_scheduled: bool,
}

struct Inner {
    svc: Arc<Attachments>,
    opts: ManagerOptions,
    emit: Box<dyn Fn(DownloadEvent) + Send + Sync>,
    state: Mutex<State>,
}

/// See the module docs. Cheap to clone.
#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<Inner>,
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn new_id() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("OS random source");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl DownloadRecord {
    fn view(&self, run: Option<&RunCtl>, check_disk: bool) -> DownloadView {
        let received = run.map_or(self.received_bytes, |r| r.received.load(Ordering::Relaxed));
        let speed = run.map_or(0.0, |r| f64::from_bits(r.speed.load(Ordering::Relaxed)));
        let left = self.total_bytes.saturating_sub(received);
        DownloadView {
            id: self.id.clone(),
            server_id: self.server_id.clone(),
            attachment_id: self.attachment_id,
            filename: self.filename.clone(),
            mime: self.mime.clone(),
            kind: self.kind,
            total_bytes: self.total_bytes,
            received_bytes: received,
            state: self.state,
            error: self.error.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            context: self.context.clone(),
            speed_bps: speed,
            eta_s: if run.is_some() && speed > 0.0 { (left as f64 / speed).ceil() as i64 } else { -1 },
            missing: check_disk && self.state == DownloadState::Done && !self.path.exists(),
            encrypted: self.encrypted.is_some(),
        }
    }
}

impl DownloadManager {
    /// Load `downloads.json` (unfinished downloads come back paused) and start empty of runs.
    /// `on_event` is called from runtime tasks and from the command methods, never with the
    /// manager's lock held.
    pub fn new(svc: Arc<Attachments>, opts: ManagerOptions, on_event: impl Fn(DownloadEvent) + Send + Sync + 'static) -> Self {
        let records = load(&opts.state_dir.join(STORE_FILE));
        Self {
            inner: Arc::new(Inner {
                svc,
                opts,
                emit: Box::new(on_event),
                state: Mutex::new(State {
                    records,
                    runs: HashMap::new(),
                    keys: HashMap::new(),
                    dir: None,
                    ask_each_time: false,
                    save_scheduled: false,
                }),
            }),
        }
    }

    /// Settings → Network → Downloads: the folder (`None` = the OS Downloads folder; a
    /// relative path is ignored) and "Ask where to save each file".
    pub fn configure(&self, dir: Option<PathBuf>, ask_each_time: bool) {
        let mut s = self.inner.state.lock().unwrap();
        s.dir = dir.filter(|d| d.is_absolute());
        s.ask_each_time = ask_each_time;
    }

    /// Whether the UI should run a save dialog for a plain download.
    pub fn ask_each_time(&self) -> bool {
        self.inner.state.lock().unwrap().ask_each_time
    }

    /// The folder downloads land in (for "Open folder" and the save dialog's start).
    pub fn download_dir(&self) -> PathBuf {
        self.inner.state.lock().unwrap().dir.clone().unwrap_or_else(|| self.inner.opts.default_dir.clone())
    }

    /// Start a download. Fails (with the panel's wording) when the server isn't connected, the
    /// destination can't be reserved, or a chosen path is already being downloaded to.
    pub async fn start(&self, args: StartArgs) -> Result<String, Error> {
        if args.attachment_id <= 0 || args.server_id.is_empty() {
            return Err(Error::Invalid("bad download request"));
        }
        if self.inner.svc.target(&args.server_id).is_none() {
            return Err(not_connected(&args.context.server_name));
        }
        let name = sanitize_filename(&args.filename);
        let (path, chosen) = match &args.destination {
            Destination::Chosen(p) => {
                if !p.is_absolute() {
                    return Err(Error::Invalid("a chosen path must be absolute"));
                }
                {
                    let s = self.inner.state.lock().unwrap();
                    if s.records.iter().any(|r| r.path == *p && matches!(r.state, DownloadState::Active | DownloadState::Paused)) {
                        return Err(Error::Message("That file is already being downloaded".into()));
                    }
                }
                tokio::fs::write(with_part(p), b"").await.map_err(|e| Error::Message(Error::Io(e).download_message()))?;
                (p.clone(), true)
            }
            Destination::Folder => {
                let dir = self.download_dir();
                tokio::fs::create_dir_all(&dir).await.map_err(|e| Error::Message(Error::Io(e).download_message()))?;
                (reserve_in(&dir, &name).await.map_err(|e| Error::Message(Error::Io(e).download_message()))?, false)
            }
        };
        let key = self.inner.svc.key(&args.server_id, args.attachment_id);
        let rec = DownloadRecord {
            id: new_id(),
            server_id: args.server_id.clone(),
            attachment_id: args.attachment_id,
            filename: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name),
            path,
            chosen,
            mime: if args.mime.is_empty() { "application/octet-stream".into() } else { args.mime.clone() },
            kind: args.kind,
            total_bytes: key.as_ref().map_or(args.size_bytes, |k| k.size_bytes),
            received_bytes: 0,
            state: DownloadState::Active,
            error: None,
            started_at: now_ms(),
            finished_at: None,
            context: args.context.clone(),
            encrypted: key.as_ref().map(|k| EncryptedInfo {
                chunk_bytes: k.chunk_bytes,
                wrapped_key: self.inner.opts.key_wrap.as_ref().and_then(|w| w.wrap(k.key.as_bytes())),
            }),
        };
        let id = rec.id.clone();
        {
            let mut s = self.inner.state.lock().unwrap();
            if let Some(k) = key {
                s.keys.insert(id.clone(), k);
            }
            s.records.push(rec);
            trim(&mut s);
        }
        self.spawn_run(&id);
        Ok(id)
    }

    pub fn pause(&self, id: &str) {
        if let Some(r) = self.inner.state.lock().unwrap().runs.get(id) {
            r.stop(Stop::Pause);
        }
    }

    /// Resume a paused one, retry a failed one, or download a cancelled / deleted one again.
    pub fn resume(&self, id: &str) {
        {
            let mut s = self.inner.state.lock().unwrap();
            if s.runs.contains_key(id) {
                return;
            }
            let svc = self.inner.svc.clone();
            let wrap = self.inner.opts.key_wrap.clone();
            let mut fresh_key = None;
            let Some(rec) = s.records.iter_mut().find(|r| r.id == id) else { return };
            if rec.state == DownloadState::Done && rec.path.exists() {
                return;
            }
            if matches!(rec.state, DownloadState::Done | DownloadState::Cancelled) {
                // Download again: the same name while it is free, else a fresh one.
                if rec.path.exists() || with_part(&rec.path).exists() {
                    let dir = rec.path.parent().map(Path::to_path_buf).unwrap_or_default();
                    let name = rec.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    rec.path = unique_path(&dir, &name);
                    rec.filename = rec.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                }
                rec.received_bytes = 0;
                if rec.state == DownloadState::Done {
                    if let Some(enc) = rec.encrypted.as_mut() {
                        // The finished record dropped its wrapped key; the registry may have it.
                        if let Some(k) = svc.key(&rec.server_id, rec.attachment_id) {
                            enc.wrapped_key = wrap.as_ref().and_then(|w| w.wrap(k.key.as_bytes()));
                            fresh_key = Some(k);
                        }
                    }
                }
            }
            if let Some(k) = fresh_key {
                s.keys.insert(id.to_string(), k);
            }
        }
        self.spawn_run(id);
    }

    pub fn cancel(&self, id: &str) {
        let event = {
            let mut s = self.inner.state.lock().unwrap();
            if let Some(r) = s.runs.get(id) {
                r.stop(Stop::Cancel);
                return;
            }
            let Some(rec) = s.records.iter_mut().find(|r| r.id == id) else { return };
            if !matches!(rec.state, DownloadState::Paused | DownloadState::Failed) {
                return;
            }
            let _ = std::fs::remove_file(with_part(&rec.path));
            rec.state = DownloadState::Cancelled;
            rec.received_bytes = 0;
            rec.error = None;
            if let Some(e) = rec.encrypted.as_mut() {
                e.wrapped_key = None;
            }
            rec.view(None, false)
        };
        (self.inner.emit)(DownloadEvent::Changed(event));
        self.schedule_save();
    }

    /// Remove rows (a running one is stopped and its `.part` deleted; a finished file stays on
    /// disk).
    pub fn remove(&self, ids: &[String]) {
        let mut gone = Vec::new();
        {
            let mut s = self.inner.state.lock().unwrap();
            for id in ids {
                if let Some(r) = s.runs.get(id) {
                    r.stop(Stop::Remove);
                    continue;
                }
                if let Some(i) = s.records.iter().position(|r| &r.id == id) {
                    let rec = s.records.remove(i);
                    if rec.state != DownloadState::Done {
                        let _ = std::fs::remove_file(with_part(&rec.path));
                    }
                    s.keys.remove(id);
                    gone.push(id.clone());
                }
            }
        }
        if !gone.is_empty() {
            (self.inner.emit)(DownloadEvent::Removed(gone));
            self.schedule_save();
        }
    }

    /// "Clear": every finished, failed and cancelled row (files stay).
    pub fn clear_finished(&self) {
        let ids: Vec<String> =
            self.inner.state.lock().unwrap().records.iter().filter(|r| r.state.is_finished()).map(|r| r.id.clone()).collect();
        self.remove(&ids);
    }

    /// Every row, newest first, with finished files checked on disk.
    pub fn list(&self) -> Vec<DownloadView> {
        let s = self.inner.state.lock().unwrap();
        s.records.iter().rev().map(|r| r.view(s.runs.get(&r.id), true)).collect()
    }

    /// The file to open: only a finished download whose file still exists. Risky types should
    /// be confirmed first ([`crate::names::is_risky_file`]).
    pub fn open_path(&self, id: &str) -> Result<PathBuf, Error> {
        let s = self.inner.state.lock().unwrap();
        match s.records.iter().find(|r| r.id == id) {
            Some(r) if r.state == DownloadState::Done && r.path.exists() => Ok(r.path.clone()),
            _ => Err(Error::Message("File not found".into())),
        }
    }

    /// What "Show in folder" reveals: the file, else its `.part`, else (as a folder to open) the
    /// folder it was going to.
    pub fn reveal_path(&self, id: &str) -> Option<RevealTarget> {
        let s = self.inner.state.lock().unwrap();
        let r = s.records.iter().find(|r| r.id == id)?;
        let part = with_part(&r.path);
        Some(if r.path.exists() {
            RevealTarget::Item(r.path.clone())
        } else if part.exists() {
            RevealTarget::Item(part)
        } else {
            RevealTarget::Folder(r.path.parent().map(Path::to_path_buf).unwrap_or_default())
        })
    }

    /// Downloads running now (the quit path pauses them; nothing to confirm).
    pub fn active_count(&self) -> usize {
        self.inner.state.lock().unwrap().runs.len()
    }

    /// Quit: everything running becomes paused, saved synchronously.
    pub fn pause_all_for_quit(&self) {
        let mut s = self.inner.state.lock().unwrap();
        let running: Vec<String> = s.runs.keys().cloned().collect();
        for id in running {
            if let Some(r) = s.runs.get(&id) {
                r.stop(Stop::Quit);
                let received = r.received.load(Ordering::Relaxed);
                if let Some(rec) = s.records.iter_mut().find(|x| x.id == id) {
                    rec.state = DownloadState::Paused;
                    rec.received_bytes = received;
                }
            }
        }
        save_now(&self.inner.opts.state_dir, &s.records);
    }

    fn schedule_save(&self) {
        {
            let mut s = self.inner.state.lock().unwrap();
            if s.save_scheduled {
                return;
            }
            s.save_scheduled = true;
        }
        let inner = self.inner.clone();
        self.inner.svc.rt.spawn(async move {
            tokio::time::sleep(SAVE_DEBOUNCE).await;
            let mut s = inner.state.lock().unwrap();
            s.save_scheduled = false;
            save_now(&inner.opts.state_dir, &s.records);
        });
    }

    fn spawn_run(&self, id: &str) {
        let ctl = RunCtl {
            cancel: CancellationToken::new(),
            stop: Arc::new(AtomicU8::new(Stop::None as u8)),
            received: Arc::new(AtomicU64::new(0)),
            speed: Arc::new(AtomicU64::new(0f64.to_bits())),
        };
        let view = {
            let mut s = self.inner.state.lock().unwrap();
            if s.runs.contains_key(id) {
                return;
            }
            let Some(rec) = s.records.iter_mut().find(|r| r.id == id) else { return };
            rec.state = DownloadState::Active;
            rec.error = None;
            rec.finished_at = None;
            ctl.received.store(rec.received_bytes, Ordering::Relaxed);
            let view = rec.view(Some(&ctl), false);
            s.runs.insert(id.to_string(), ctl);
            view
        };
        (self.inner.emit)(DownloadEvent::Changed(view));
        self.schedule_save();
        let me = self.clone();
        let id = id.to_string();
        self.inner.svc.rt.spawn(async move { me.run(id).await });
    }

    async fn run(&self, id: String) {
        let (cancel, stop, received, speed) = {
            let s = self.inner.state.lock().unwrap();
            let r = &s.runs[&id];
            (r.cancel.clone(), r.stop.clone(), r.received.clone(), r.speed.clone())
        };
        let result = self.transfer(&id, &cancel, &received, &speed).await;
        let why = match stop.load(Ordering::SeqCst) {
            1 => Stop::Pause,
            2 => Stop::Cancel,
            3 => Stop::Remove,
            4 => Stop::Quit,
            _ => Stop::None,
        };
        let mut events = Vec::new();
        {
            let mut s = self.inner.state.lock().unwrap();
            s.runs.remove(&id);
            let mut drop_key = false;
            if let Some(rec) = s.records.iter_mut().find(|r| r.id == id) {
                rec.received_bytes = received.load(Ordering::Relaxed);
                match (&result, why) {
                    (_, Stop::Cancel | Stop::Remove) => {
                        let _ = std::fs::remove_file(with_part(&rec.path));
                        rec.state = DownloadState::Cancelled;
                        rec.received_bytes = 0;
                        if let Some(e) = rec.encrypted.as_mut() {
                            e.wrapped_key = None;
                        }
                        drop_key = true;
                    }
                    (_, Stop::Pause | Stop::Quit) => rec.state = DownloadState::Paused,
                    (Ok(()), Stop::None) => {
                        rec.state = DownloadState::Done;
                        rec.finished_at = Some(now_ms());
                        if let Some(e) = rec.encrypted.as_mut() {
                            e.wrapped_key = None;
                        }
                        drop_key = true;
                    }
                    (Err(e), Stop::None) => {
                        rec.state = DownloadState::Failed;
                        rec.error = Some(e.download_message());
                        if !matches!(e, Error::Message(_) | Error::Http(_)) {
                            log::warn!("download {id} failed: {e}");
                        }
                    }
                }
                if why == Stop::Remove {
                    s.records.retain(|r| r.id != id);
                    drop_key = true;
                    events.push(DownloadEvent::Removed(vec![id.clone()]));
                } else if why != Stop::Quit {
                    let rec = s.records.iter().find(|r| r.id == id).expect("record");
                    events.push(DownloadEvent::Changed(rec.view(None, false)));
                }
            }
            if drop_key {
                s.keys.remove(&id);
            }
        }
        for e in events {
            (self.inner.emit)(e);
        }
        if why != Stop::Quit {
            self.schedule_save();
        }
    }

    async fn transfer(&self, id: &str, cancel: &CancellationToken, received: &AtomicU64, speed: &AtomicU64) -> Result<(), Error> {
        let (rec, key) = {
            let s = self.inner.state.lock().unwrap();
            let rec = s.records.iter().find(|r| r.id == id).cloned().ok_or(Error::Cancelled)?;
            let key = s.keys.get(id).cloned();
            (rec, key)
        };
        let svc = &self.inner.svc;
        if svc.target(&rec.server_id).is_none() {
            return Err(not_connected(&rec.context.server_name));
        }
        let key = match (&rec.encrypted, key) {
            (None, _) => None,
            (Some(_), Some(k)) => Some(k),
            (Some(enc), None) => {
                let k = svc.key(&rec.server_id, rec.attachment_id).or_else(|| {
                    let raw = enc.wrapped_key.as_deref().and_then(|w| self.inner.opts.key_wrap.as_ref()?.unwrap(w))?;
                    Some(AttachmentKey {
                        key: FileKey::from_bytes(raw),
                        chunk_bytes: enc.chunk_bytes,
                        size_bytes: rec.total_bytes,
                        mime: rec.mime.clone(),
                        filename: rec.filename.clone(),
                    })
                });
                let k = k.ok_or(Error::NoKey)?;
                self.inner.state.lock().unwrap().keys.insert(id.to_string(), k.clone());
                Some(k)
            }
        };
        let part = with_part(&rec.path);
        let mut offset = match tokio::fs::metadata(&part).await {
            Ok(m) => m.len(),
            Err(_) => 0,
        };
        if rec.total_bytes > 0 && offset > rec.total_bytes {
            offset = 0;
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(offset == 0)
            .open(&part)
            .await
            .map_err(Error::Io)?;
        let mut total = key.as_ref().map_or(rec.total_bytes, |k| k.size_bytes);
        received.store(offset, Ordering::Relaxed);

        let mut meter = Meter::new(offset);
        let complete_already = total > 0 && offset == total;
        if !complete_already {
            let opened = svc.open_with_key(&rec.server_id, rec.attachment_id, key.as_ref(), offset, None, Some(cancel.clone())).await;
            let mut stream = match opened {
                Ok(s) => s,
                // Nothing left to fetch: the part file is the whole file.
                Err(Error::Http(416)) if offset > 0 && offset == total => return self.finish(id, file, &part, total).await,
                Err(e) => return Err(e),
            };
            if key.is_none() {
                if let Some(t) = stream.total() {
                    total = t;
                    self.set_total(id, t);
                }
            }
            let start = stream.start();
            if start != offset {
                return Err(Error::Invalid("range start mismatch"));
            }
            file.seek(std::io::SeekFrom::Start(offset)).await?;
            let mut w = tokio::io::BufWriter::with_capacity(WRITE_BUFFER, file);
            let mut pos = offset;
            let res: Result<(), Error> = async {
                while let Some(b) = stream.next().await? {
                    w.write_all(b).await?;
                    pos += b.len() as u64;
                    received.store(pos, Ordering::Relaxed);
                    if let Some(s) = meter.sample(pos) {
                        speed.store(s.to_bits(), Ordering::Relaxed);
                    }
                    if meter.emit_due() {
                        self.emit_progress(id);
                    }
                }
                Ok(())
            }
            .await;
            // Keep what arrived (a pause resumes from the part file's size).
            let flushed = w.flush().await;
            file = w.into_inner();
            res?;
            flushed?;
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
        }
        self.finish(id, file, &part, total).await
    }

    async fn finish(&self, id: &str, file: tokio::fs::File, part: &Path, total: u64) -> Result<(), Error> {
        file.sync_all().await?;
        drop(file);
        let size = tokio::fs::metadata(part).await?.len();
        if total > 0 && size != total {
            return Err(Error::Message("The download came out the wrong size".into()));
        }
        let (dest, chosen) = {
            let s = self.inner.state.lock().unwrap();
            let r = s.records.iter().find(|r| r.id == id).ok_or(Error::Cancelled)?;
            (r.path.clone(), r.chosen)
        };
        // Something may have taken the reserved name meanwhile.
        let dest = if !chosen && dest.exists() {
            let dir = dest.parent().map(Path::to_path_buf).unwrap_or_default();
            unique_path(&dir, &dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
        } else {
            dest
        };
        tokio::fs::rename(part, &dest).await?;
        let mut s = self.inner.state.lock().unwrap();
        if let Some(r) = s.records.iter_mut().find(|r| r.id == id) {
            r.filename = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            r.path = dest;
            r.total_bytes = size;
        }
        if let Some(r) = s.runs.get(id) {
            r.received.store(size, Ordering::Relaxed);
        }
        Ok(())
    }

    fn set_total(&self, id: &str, total: u64) {
        let mut s = self.inner.state.lock().unwrap();
        if let Some(r) = s.records.iter_mut().find(|r| r.id == id) {
            r.total_bytes = total;
        }
    }

    fn emit_progress(&self, id: &str) {
        let view = {
            let s = self.inner.state.lock().unwrap();
            s.records.iter().find(|r| r.id == id).map(|r| r.view(s.runs.get(id), false))
        };
        if let Some(v) = view {
            (self.inner.emit)(DownloadEvent::Changed(v));
        }
    }
}

/// "Show in folder".
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevealTarget {
    /// Select this file in its folder.
    Item(PathBuf),
    /// Nothing is on disk any more: open this folder.
    Folder(PathBuf),
}

fn not_connected(server_name: &str) -> Error {
    Error::Message(format!("Not connected to {}", if server_name.is_empty() { "the server" } else { server_name }))
}

/// Pick a free name in `dir` and reserve it by creating its `.part` (`wx`: a simultaneous
/// start of the same name loses cleanly and picks again).
async fn reserve_in(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    loop {
        let candidate = unique_path(dir, name);
        match tokio::fs::OpenOptions::new().write(true).create_new(true).open(with_part(&candidate)).await {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

/// Over the cap: drop the oldest finished records, never live ones.
fn trim(s: &mut State) {
    while s.records.len() > MAX_RECORDS {
        let Some(i) = s.records.iter().position(|r| r.state.is_finished() && !s.runs.contains_key(&r.id)) else { break };
        let r = s.records.remove(i);
        if r.state != DownloadState::Done {
            let _ = std::fs::remove_file(with_part(&r.path));
        }
        s.keys.remove(&r.id);
    }
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    downloads: Vec<serde_json::Value>,
}

fn load(file: &Path) -> Vec<DownloadRecord> {
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    let Ok(store) = serde_json::from_str::<StoreFile>(&text) else { return Vec::new() };
    let mut out = Vec::new();
    for v in store.downloads {
        let Ok(mut r) = serde_json::from_value::<DownloadRecord>(v) else { continue };
        if r.id.is_empty() || !r.path.is_absolute() || r.server_id.is_empty() {
            continue;
        }
        // Running when the app last stopped (a crash skips the quit path).
        if r.state == DownloadState::Active {
            r.state = DownloadState::Paused;
        }
        // An unfinished one resumes from its part file: show that count.
        if matches!(r.state, DownloadState::Paused | DownloadState::Failed) {
            r.received_bytes = std::fs::metadata(with_part(&r.path)).map(|m| m.len()).unwrap_or(0);
        }
        out.push(r);
    }
    out
}

fn save_now(dir: &Path, records: &[DownloadRecord]) {
    let file = dir.join(STORE_FILE);
    let tmp = dir.join(format!("{STORE_FILE}.tmp"));
    let body = serde_json::json!({ "version": 1, "downloads": records });
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        std::io::Write::write_all(&mut f, serde_json::to_string(&body).unwrap_or_default().as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, &file)
    };
    if let Err(e) = write() {
        log::warn!("downloads.json: save failed: {e}");
    }
}

/// Speed sampling and the 4 Hz event cadence of one run.
struct Meter {
    sample_at: Instant,
    sample_bytes: u64,
    speed: f64,
    last_emit: Instant,
}

impl Meter {
    fn new(at: u64) -> Self {
        let now = Instant::now();
        Self { sample_at: now, sample_bytes: at, speed: 0.0, last_emit: now }
    }

    fn sample(&mut self, pos: u64) -> Option<f64> {
        let now = Instant::now();
        let dt = now.duration_since(self.sample_at);
        if dt < SPEED_SAMPLE {
            return None;
        }
        let inst = pos.saturating_sub(self.sample_bytes) as f64 / dt.as_secs_f64();
        self.speed = if self.speed == 0.0 { inst } else { self.speed * 0.7 + inst * 0.3 };
        self.sample_at = now;
        self.sample_bytes = pos;
        Some(self.speed)
    }

    fn emit_due(&mut self) -> bool {
        let now = Instant::now();
        if now.duration_since(self.last_emit) >= EMIT_INTERVAL {
            self.last_emit = now;
            true
        } else {
            false
        }
    }
}

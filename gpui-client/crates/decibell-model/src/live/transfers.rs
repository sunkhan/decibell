//! The Transfers panel (`stores/transfersStore.ts`, `features/transfers/{downloads,
//! uploads,format}.ts`, `TransfersButton.tsx`'s derived values; spec
//! `2026-10-04-transfers-panel-design.md`).
//!
//! **Downloads** mirror the `decibell-attachments` crate's `DownloadManager`, which is
//! the source of truth (it persists `downloads.json`): seeded with its `list()`, patched
//! by its `DownloadEvent`s. The manager calls back on its own threads; [`DownloadEvents`]
//! queues those and wakes the UI thread once per burst (the event pump's scheme), and a
//! `Changed` for a download already queued replaces it, so a stalled UI thread holds one
//! entry per download. The commands go through [`DownloadControl`], which the app
//! implements over its manager (`ManagerControl` below does it with a runtime handle).
//!
//! **Uploads** are this session's history, fed from M2's attachments store: every
//! pending that has started sending gets a row that outlives its pending (with its last
//! state). M2 calls [`Transfers::sync_uploads`] with a snapshot ([`PendingUpload`]) when
//! its pendings change, and implements [`UploadControl`] for pause / cancel.
//!
//! **Derived facts** for the app: [`Transfers::progress`] (the taskbar / dock progress:
//! fraction over everything running, paused when nothing runs) and
//! [`Transfers::uploads_in_flight`] (closing the window or quitting loses those: the app
//! asks first). Electron sent the progress at most 4 times a second; the app applies it
//! from an observer of this store, throttled the same way if its platform call is costly.
//!
//! **Rhythm**: about 4 times a second per active transfer. The title-bar button and the
//! open panel observe it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub use decibell_attachments::AttachmentKind;
pub use decibell_attachments::manager::{
    Destination, DownloadContext, DownloadEvent, DownloadManager, DownloadState, DownloadView, StartArgs,
};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::future::LocalBoxFuture;
use gpui::{App, Context, SharedString, Task};

use crate::ids::{ChannelId, ServerId};

/// How often an upload's speed is sampled.
pub const SPEED_SAMPLE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TransfersTab {
    #[default]
    Downloads,
    Uploads,
}

/// The title-bar button's dot: something ended while the panel was shut.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Attention {
    #[default]
    None,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadState {
    Uploading,
    Paused,
    Done,
    Failed,
    Cancelled,
}

/// What M2's attachments store says about one pending (`PendingAttachment`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingStatus {
    /// Not sending yet: no row.
    Queued,
    Pending,
    Uploading,
    /// Uploaded (the message may not be sent yet).
    Ready,
    Failed,
}

/// One pending upload as M2's attachments store holds it (the narrow interface this
/// store needs; see the module doc).
#[derive(Clone)]
pub struct PendingUpload {
    pub pending_id: SharedString,
    pub server: ServerId,
    pub channel: ChannelId,
    pub filename: SharedString,
    pub kind: AttachmentKind,
    pub status: PendingStatus,
    pub paused: bool,
    /// Failed because the user cancelled.
    pub cancelled: bool,
    pub error: Option<SharedString>,
    /// The composer's preview while the pending exists.
    pub preview: Option<Arc<gpui::Image>>,
    /// The server's id once uploaded (for the thumbnail afterwards).
    pub attachment_id: Option<i64>,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
}

/// Pause / cancel, implemented by M2's attachments store.
pub trait UploadControl: 'static {
    fn set_paused(&self, pending_id: &str, paused: bool, cx: &mut App);
    fn cancel(&self, pending_id: &str, cx: &mut App);
}

#[derive(Clone)]
pub struct UploadEntry {
    pub pending_id: SharedString,
    pub server: ServerId,
    pub server_name: SharedString,
    pub channel: ChannelId,
    pub channel_name: SharedString,
    pub filename: SharedString,
    pub kind: AttachmentKind,
    pub preview: Option<Arc<gpui::Image>>,
    pub attachment_id: Option<i64>,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub state: UploadState,
    pub error: Option<SharedString>,
    pub speed_bps: f64,
    pub started_at: Instant,
    pub finished_at: Option<Instant>,
}

impl UploadEntry {
    fn same_view(&self, other: &UploadEntry) -> bool {
        self.state == other.state
            && self.transferred_bytes == other.transferred_bytes
            && self.attachment_id == other.attachment_id
            && self.speed_bps == other.speed_bps
            && self.preview.as_ref().map(|p| p.id) == other.preview.as_ref().map(|p| p.id)
    }
}

/// The downloads manager's commands (`window.decibell.downloads.*`).
pub trait DownloadControl: 'static {
    fn list(&self) -> Vec<DownloadView>;
    fn start(&self, args: StartArgs) -> LocalBoxFuture<'static, Result<String, SharedString>>;
    fn pause(&self, id: &str);
    fn resume(&self, id: &str);
    fn cancel(&self, id: &str);
    fn remove(&self, ids: &[String]);
    fn clear_finished(&self);
    /// Settings → Network → Downloads (`None` = the OS Downloads folder).
    fn configure(&self, dir: Option<std::path::PathBuf>, ask_each_time: bool);
    /// Show a save dialog for a plain download.
    fn ask_each_time(&self) -> bool;
    /// Quitting: every running download is saved as paused, synchronously, so the next launch
    /// resumes it (`pauseAllForQuit`). Called by the app's quit path.
    fn pause_all_for_quit(&self) {}
}

/// [`DownloadControl`] over a `DownloadManager` whose async work runs on `runtime` (the
/// core's, or the app's own).
pub struct ManagerControl {
    pub manager: Arc<DownloadManager>,
    pub runtime: tokio::runtime::Handle,
}

impl DownloadControl for ManagerControl {
    fn list(&self) -> Vec<DownloadView> {
        self.manager.list()
    }
    fn start(&self, args: StartArgs) -> LocalBoxFuture<'static, Result<String, SharedString>> {
        let manager = self.manager.clone();
        let task = self.runtime.spawn(async move { manager.start(args).await.map_err(|e| e.to_string()) });
        Box::pin(async move {
            match task.await {
                Ok(r) => r.map_err(SharedString::from),
                Err(e) => Err(e.to_string().into()),
            }
        })
    }
    fn pause(&self, id: &str) {
        self.manager.pause(id);
    }
    fn resume(&self, id: &str) {
        self.manager.resume(id);
    }
    fn cancel(&self, id: &str) {
        self.manager.cancel(id);
    }
    fn remove(&self, ids: &[String]) {
        self.manager.remove(ids);
    }
    fn clear_finished(&self) {
        self.manager.clear_finished();
    }
    fn configure(&self, dir: Option<std::path::PathBuf>, ask_each_time: bool) {
        self.manager.configure(dir, ask_each_time);
    }
    fn ask_each_time(&self) -> bool {
        self.manager.ask_each_time()
    }
    fn pause_all_for_quit(&self) {
        self.manager.pause_all_for_quit();
    }
}

/// The manager's event callback → the UI thread, one wake per burst.
#[derive(Clone)]
pub struct DownloadEvents {
    queue: Arc<Mutex<Vec<DownloadEvent>>>,
    wake: UnboundedSender<()>,
}

impl DownloadEvents {
    /// Give this to `DownloadManager::new` as `on_event` (`move |e| events.push(e)`).
    pub fn push(&self, event: DownloadEvent) {
        let was_empty = {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            let was_empty = q.is_empty();
            // A newer state of a download already waiting replaces it.
            if let DownloadEvent::Changed(view) = &event
                && let Some(slot) = q.iter_mut().find(|e| matches!(e, DownloadEvent::Changed(v) if v.id == view.id))
            {
                *slot = event;
                return;
            }
            q.push(event);
            was_empty
        };
        if was_empty {
            let _ = self.wake.unbounded_send(());
        }
    }
}

/// Progress over everything running, for the taskbar / dock.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub active: usize,
    pub paused: usize,
    /// Running or paused uploads: closing loses them.
    pub uploads: usize,
    /// Done / total over transfers with a known size; none = clear the progress bar.
    pub fraction: Option<f64>,
    /// Nothing runs, something is paused: show it paused.
    pub paused_only: bool,
}

#[derive(Default)]
pub struct Transfers {
    /// Newest first.
    downloads: Vec<DownloadView>,
    /// Newest first; this session only.
    uploads: Vec<UploadEntry>,
    /// When each tab last saw something start, and when the panel was last opened:
    /// opening shows the tab with news, else the last one.
    activity_downloads: Option<Instant>,
    activity_uploads: Option<Instant>,
    opened_at: Option<Instant>,
    pub panel_open: bool,
    pub tab: TransfersTab,
    pub attention: Attention,
    /// Bumped when a download starts: the button replays its arrow drop.
    pub start_pulse: u64,
    samples: HashMap<SharedString, (Instant, u64, f64)>,
    pub(crate) control: Option<Rc<dyn DownloadControl>>,
    pub(crate) uploads_control: Option<Rc<dyn UploadControl>>,
    _download_pump: Option<Task<()>>,
}

fn ended(state: DownloadState) -> bool {
    matches!(state, DownloadState::Done | DownloadState::Failed)
}

impl Transfers {
    pub fn downloads(&self) -> &[DownloadView] {
        &self.downloads
    }

    pub fn uploads(&self) -> &[UploadEntry] {
        &self.uploads
    }

    /// Connect the app's download manager: seed the list and follow its events. Returns
    /// the callback to hand to `DownloadManager::new`… or, when the manager already
    /// exists, call `events.push` from its existing callback.
    pub fn attach_downloads(&mut self, control: Rc<dyn DownloadControl>, cx: &mut Context<Self>) -> DownloadEvents {
        let (wake, mut woken) = unbounded();
        let events = DownloadEvents { queue: Arc::default(), wake };
        let queue = events.queue.clone();
        self._download_pump = Some(cx.spawn(async move |this, cx| {
            let mut batch = Vec::new();
            while woken.next().await.is_some() {
                std::mem::swap(&mut *queue.lock().unwrap_or_else(|e| e.into_inner()), &mut batch);
                let ok = this
                    .update(cx, |this, cx| {
                        for event in batch.drain(..) {
                            this.apply(event, cx);
                        }
                    })
                    .is_ok();
                batch.clear();
                if !ok {
                    break;
                }
            }
        }));
        self.set_downloads(control.list(), cx);
        self.control = Some(control);
        events
    }

    pub fn set_upload_control(&mut self, control: Rc<dyn UploadControl>) {
        self.uploads_control = Some(control);
    }

    pub fn control(&self) -> Option<Rc<dyn DownloadControl>> {
        self.control.clone()
    }

    pub fn upload_control(&self) -> Option<Rc<dyn UploadControl>> {
        self.uploads_control.clone()
    }

    pub fn set_downloads(&mut self, mut list: Vec<DownloadView>, cx: &mut Context<Self>) {
        list.sort_by_key(|d| std::cmp::Reverse(d.started_at));
        self.downloads = list;
        cx.notify();
    }

    pub fn apply(&mut self, event: DownloadEvent, cx: &mut Context<Self>) {
        match event {
            DownloadEvent::Changed(view) => self.upsert_download(view, cx),
            DownloadEvent::Removed(ids) => self.remove_downloads(&ids, cx),
        }
    }

    pub fn upsert_download(&mut self, d: DownloadView, cx: &mut Context<Self>) {
        let at = self.downloads.iter().position(|x| x.id == d.id);
        let prev_state = at.map(|i| self.downloads[i].state);
        // A download that just finished or failed while nobody was looking lights the
        // dot; failure outranks success.
        if !self.panel_open && ended(d.state) && prev_state.is_none_or(|p| !ended(p) || p != d.state) {
            if d.state == DownloadState::Failed {
                self.attention = Attention::Failed;
            } else if self.attention == Attention::None {
                self.attention = Attention::Done;
            }
        }
        match at {
            Some(i) => {
                if self.downloads[i] == d {
                    return;
                }
                self.downloads[i] = d;
            }
            None => {
                self.activity_downloads = Some(cx.background_executor().now());
                self.downloads.insert(0, d);
            }
        }
        cx.notify();
    }

    pub fn remove_downloads(&mut self, ids: &[String], cx: &mut Context<Self>) {
        let drop: HashSet<&String> = ids.iter().collect();
        let before = self.downloads.len();
        self.downloads.retain(|d| !drop.contains(&d.id));
        if self.downloads.len() != before {
            cx.notify();
        }
    }

    /// M2's pendings changed: mirror every one that has started sending; a row whose
    /// pending is gone keeps its last state (one that vanished mid-flight didn't finish).
    pub fn sync_uploads(
        &mut self,
        pendings: &[PendingUpload],
        names: impl Fn(ServerId, &ChannelId) -> (SharedString, SharedString),
        cx: &mut Context<Self>,
    ) {
        let now = cx.background_executor().now();
        let mut changed = false;
        let mut attention = Attention::None;
        let mut added: Vec<UploadEntry> = Vec::new();
        let mut fresh: HashMap<SharedString, UploadEntry> = HashMap::new();
        let present: HashSet<&SharedString> = pendings.iter().map(|p| &p.pending_id).collect();
        for p in pendings {
            let state = match p.status {
                PendingStatus::Queued | PendingStatus::Pending => continue,
                PendingStatus::Failed if p.cancelled => UploadState::Cancelled,
                PendingStatus::Failed => UploadState::Failed,
                PendingStatus::Ready => UploadState::Done,
                PendingStatus::Uploading if p.paused => UploadState::Paused,
                PendingStatus::Uploading => UploadState::Uploading,
            };
            let old = self.uploads.iter().find(|u| u.pending_id == p.pending_id).cloned();
            let old = old.as_ref();
            let speed = self.speed(&p.pending_id, p.transferred_bytes, state == UploadState::Uploading, now);
            let finished = matches!(state, UploadState::Done | UploadState::Failed | UploadState::Cancelled);
            if state == UploadState::Failed && old.is_none_or(|o| o.state != UploadState::Failed) {
                attention = Attention::Failed;
            }
            let (server_name, channel_name) = match old {
                Some(o) => (o.server_name.clone(), o.channel_name.clone()),
                None => names(p.server, &p.channel),
            };
            let entry = UploadEntry {
                pending_id: p.pending_id.clone(),
                server: p.server,
                server_name,
                channel: p.channel.clone(),
                channel_name,
                filename: p.filename.clone(),
                kind: p.kind,
                preview: p.preview.clone(),
                attachment_id: p.attachment_id,
                total_bytes: p.total_bytes,
                transferred_bytes: p.transferred_bytes,
                state,
                error: (state == UploadState::Failed)
                    .then(|| p.error.clone().unwrap_or_else(|| SharedString::new_static("Upload failed"))),
                speed_bps: speed,
                started_at: old.map_or(now, |o| o.started_at),
                finished_at: if finished { Some(old.and_then(|o| o.finished_at).unwrap_or(now)) } else { None },
            };
            match old {
                Some(o) if o.same_view(&entry) => {}
                Some(_) => {
                    fresh.insert(entry.pending_id.clone(), entry);
                    changed = true;
                }
                None => {
                    added.push(entry);
                    changed = true;
                }
            }
        }
        let mut gone = Vec::new();
        for u in &mut self.uploads {
            if let Some(f) = fresh.remove(&u.pending_id) {
                *u = f;
                continue;
            }
            if present.contains(&u.pending_id) {
                continue;
            }
            if u.preview.is_none() && !matches!(u.state, UploadState::Uploading | UploadState::Paused) {
                continue;
            }
            changed = true;
            gone.push(u.pending_id.clone());
            u.preview = None;
            u.speed_bps = 0.0;
            if matches!(u.state, UploadState::Uploading | UploadState::Paused) {
                u.state = UploadState::Cancelled;
            }
            u.finished_at.get_or_insert(now);
        }
        for id in gone {
            self.samples.remove(&id);
        }
        if !changed {
            return;
        }
        if !added.is_empty() {
            self.activity_uploads = Some(now);
        }
        added.reverse();
        added.append(&mut self.uploads);
        self.uploads = added;
        if attention != Attention::None && !self.panel_open {
            self.attention = attention;
        }
        cx.notify();
    }

    /// An EWMA over ~0.4 s samples (`speedOf`).
    fn speed(&mut self, id: &SharedString, bytes: u64, live: bool, now: Instant) -> f64 {
        if !live {
            self.samples.remove(id);
            return 0.0;
        }
        let Some((at, last, speed)) = self.samples.get_mut(id) else {
            self.samples.insert(id.clone(), (now, bytes, 0.0));
            return 0.0;
        };
        let dt = now.saturating_duration_since(*at);
        if dt >= SPEED_SAMPLE && bytes != *last {
            let inst = bytes.saturating_sub(*last) as f64 / dt.as_secs_f64();
            *speed = if *speed == 0.0 { inst } else { *speed * 0.7 + inst * 0.3 };
            *at = now;
            *last = bytes;
        }
        *speed
    }

    pub fn remove_upload(&mut self, pending_id: &str, cx: &mut Context<Self>) {
        let before = self.uploads.len();
        self.uploads.retain(|u| u.pending_id.as_ref() != pending_id);
        if self.uploads.len() != before {
            cx.notify();
        }
    }

    pub fn clear_finished_uploads(&mut self, cx: &mut Context<Self>) {
        let before = self.uploads.len();
        self.uploads.retain(|u| matches!(u.state, UploadState::Uploading | UploadState::Paused));
        if self.uploads.len() != before {
            cx.notify();
        }
    }

    /// Open the panel: on `tab`, else on the tab with news since it was last opened.
    pub fn open_panel(&mut self, tab: Option<TransfersTab>, cx: &mut Context<Self>) {
        let next = tab.unwrap_or_else(|| {
            let newest = self.activity_downloads.max(self.activity_uploads);
            if newest.is_some() && newest > self.opened_at {
                if self.activity_uploads > self.activity_downloads { TransfersTab::Uploads } else { TransfersTab::Downloads }
            } else {
                self.tab
            }
        });
        self.panel_open = true;
        self.tab = next;
        self.attention = Attention::None;
        self.opened_at = Some(cx.background_executor().now());
        cx.notify();
    }

    pub fn close_panel(&mut self, cx: &mut Context<Self>) {
        if self.panel_open {
            self.panel_open = false;
            cx.notify();
        }
    }

    pub fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        if self.panel_open { self.close_panel(cx) } else { self.open_panel(None, cx) }
    }

    pub fn set_tab(&mut self, tab: TransfersTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    pub fn note_started(&mut self, cx: &mut Context<Self>) {
        self.start_pulse += 1;
        cx.notify();
    }

    /// The taskbar / dock progress.
    pub fn progress(&self) -> TransferProgress {
        #[derive(Default)]
        struct Sum {
            active: usize,
            paused: usize,
            done: u64,
            total: u64,
        }
        impl Sum {
            fn add(&mut self, running: bool, is_paused: bool, got: u64, size: u64) {
                if !running && !is_paused {
                    return;
                }
                if running { self.active += 1 } else { self.paused += 1 }
                if size > 0 {
                    self.done += got;
                    self.total += size;
                }
            }
        }
        let mut sum = Sum::default();
        for d in &self.downloads {
            sum.add(d.state == DownloadState::Active, d.state == DownloadState::Paused, d.received_bytes, d.total_bytes);
        }
        let downloads_running = sum.active + sum.paused;
        for u in &self.uploads {
            sum.add(u.state == UploadState::Uploading, u.state == UploadState::Paused, u.transferred_bytes, u.total_bytes);
        }
        let Sum { active, paused, done, total } = sum;
        let running = active + paused;
        TransferProgress {
            active,
            paused,
            uploads: running - downloads_running,
            fraction: (running > 0).then(|| if total > 0 { done as f64 / total as f64 } else { 0.0 }),
            paused_only: active == 0 && paused > 0,
        }
    }

    /// Uploads a close would lose (the quit guard asks first when > 0). Downloads need no
    /// guard: they pause on quit and resume next launch.
    pub fn uploads_in_flight(&self) -> usize {
        self.progress().uploads
    }
}

/// "18.4 of 44.0 MB": both numbers in the total's unit.
pub fn format_progress(done: u64, total: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 30, "GB"), (1 << 20, "MB"), (1 << 10, "KB")];
    if total == 0 {
        return format_size(done);
    }
    match UNITS.iter().find(|(size, _)| total >= *size) {
        Some((size, label)) => format!("{:.1} of {:.1} {label}", done as f64 / *size as f64, total as f64 / *size as f64),
        None => format!("{done} of {total} B"),
    }
}

/// `formatFileSize` (chat attachments).
pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_speed(bps: f64) -> String {
    format!("{}/s", format_size(bps.round().max(0.0) as u64))
}

pub fn format_eta(seconds: i64) -> String {
    if seconds < 0 {
        return String::new();
    }
    if seconds < 60 {
        return format!("{} s left", seconds.max(1));
    }
    let min = (seconds as f64 / 60.0).round() as i64;
    if min < 60 {
        return format!("{min} min left");
    }
    let (h, m) = (min / 60, min % 60);
    if m > 0 { format!("{h} h {m} min left") } else { format!("{h} h left") }
}

/// Files that can run code when opened: opening one from the panel asks first.
pub fn is_risky_file(filename: &str) -> bool {
    const RISKY: &[&str] = &[
        "exe", "msi", "msix", "appx", "bat", "cmd", "com", "scr", "pif", "cpl", "ps1", "psm1", "vbs", "vbe", "js", "jse",
        "wsf", "wsh", "hta", "lnk", "reg", "jar", "sh", "bash", "zsh", "command", "app", "dmg", "pkg", "run", "bin",
        "appimage", "deb", "rpm", "desktop", "py", "pl", "rb",
    ];
    filename
        .rsplit_once('.')
        .is_some_and(|(_, ext)| !ext.is_empty() && RISKY.contains(&ext.to_lowercase().as_str()))
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn download(id: &str, state: DownloadState, got: u64, total: u64) -> DownloadView {
        DownloadView {
            id: id.into(),
            server_id: "1".into(),
            attachment_id: 1,
            filename: "f.zip".into(),
            mime: "application/zip".into(),
            kind: AttachmentKind::Document,
            total_bytes: total,
            received_bytes: got,
            state,
            error: None,
            started_at: 0,
            finished_at: None,
            context: DownloadContext::default(),
            speed_bps: 0.0,
            eta_s: -1,
            missing: false,
            encrypted: false,
        }
    }

    fn pending(id: &str, status: PendingStatus, got: u64) -> PendingUpload {
        PendingUpload {
            pending_id: id.to_string().into(),
            server: ServerId::from_central(1),
            channel: "general".into(),
            filename: "a.png".into(),
            kind: AttachmentKind::Image,
            status,
            paused: false,
            cancelled: false,
            error: None,
            preview: None,
            attachment_id: None,
            total_bytes: 1000,
            transferred_bytes: got,
        }
    }

    fn names(_: ServerId, _: &ChannelId) -> (SharedString, SharedString) {
        ("Daily Gaming".into(), "general".into())
    }

    #[gpui::test]
    fn attention_progress_and_the_quit_guard(cx: &mut TestAppContext) {
        let t = cx.new(|_| Transfers::default());
        t.update(cx, |t, cx| {
            t.upsert_download(download("a", DownloadState::Active, 50, 100), cx);
            t.upsert_download(download("b", DownloadState::Paused, 0, 100), cx);
            t.sync_uploads(&[pending("u1", PendingStatus::Uploading, 250)], names, cx);
        });
        t.read_with(cx, |t, _| {
            let p = t.progress();
            assert_eq!((p.active, p.paused, p.uploads), (2, 1, 1));
            assert_eq!(p.fraction, Some(300.0 / 1200.0));
            assert!(!p.paused_only);
            assert_eq!(t.uploads_in_flight(), 1);
            assert_eq!(t.uploads()[0].server_name.as_ref(), "Daily Gaming");
        });
        // Finished while the panel is shut: the dot; failure outranks.
        t.update(cx, |t, cx| t.upsert_download(download("a", DownloadState::Done, 100, 100), cx));
        t.read_with(cx, |t, _| assert_eq!(t.attention, Attention::Done));
        t.update(cx, |t, cx| t.sync_uploads(&[pending("u1", PendingStatus::Failed, 250)], names, cx));
        t.read_with(cx, |t, _| {
            assert_eq!(t.attention, Attention::Failed);
            assert_eq!(t.uploads()[0].error.as_deref(), Some("Upload failed"));
            assert!(t.progress().paused_only);
        });
        // Opening shows the tab with news and clears the dot.
        t.update(cx, |t, cx| t.open_panel(None, cx));
        t.read_with(cx, |t, _| {
            assert_eq!(t.attention, Attention::None);
            assert!(t.panel_open);
        });
    }

    #[gpui::test]
    fn an_upload_whose_pending_vanished_mid_flight_is_cancelled(cx: &mut TestAppContext) {
        let t = cx.new(|_| Transfers::default());
        t.update(cx, |t, cx| {
            t.sync_uploads(&[pending("q", PendingStatus::Queued, 0), pending("u", PendingStatus::Uploading, 10)], names, cx);
        });
        t.read_with(cx, |t, _| assert_eq!(t.uploads().len(), 1, "queued pendings get no row"));
        t.update(cx, |t, cx| t.sync_uploads(&[], names, cx));
        t.read_with(cx, |t, _| {
            assert_eq!(t.uploads()[0].state, UploadState::Cancelled);
            assert!(t.uploads()[0].finished_at.is_some());
            assert_eq!(t.uploads_in_flight(), 0);
        });
        // Speed: an EWMA over 0.4 s samples.
        t.update(cx, |t, cx| t.sync_uploads(&[pending("s", PendingStatus::Uploading, 0)], names, cx));
        cx.executor().advance_clock(Duration::from_millis(500));
        t.update(cx, |t, cx| t.sync_uploads(&[pending("s", PendingStatus::Uploading, 500)], names, cx));
        t.read_with(cx, |t, _| assert_eq!(t.uploads()[0].speed_bps, 1000.0));
    }

    #[gpui::test]
    fn manager_events_are_coalesced_per_download(cx: &mut TestAppContext) {
        struct Fake;
        impl DownloadControl for Fake {
            fn list(&self) -> Vec<DownloadView> {
                vec![download("old", DownloadState::Done, 1, 1)]
            }
            fn start(&self, _: StartArgs) -> LocalBoxFuture<'static, Result<String, SharedString>> {
                Box::pin(async { Ok("n".into()) })
            }
            fn pause(&self, _: &str) {}
            fn resume(&self, _: &str) {}
            fn cancel(&self, _: &str) {}
            fn remove(&self, _: &[String]) {}
            fn clear_finished(&self) {}
            fn configure(&self, _: Option<std::path::PathBuf>, _: bool) {}
            fn ask_each_time(&self) -> bool {
                false
            }
        }
        let t = cx.new(|_| Transfers::default());
        let events = t.update(cx, |t, cx| t.attach_downloads(Rc::new(Fake), cx));
        for got in 0..100 {
            events.push(DownloadEvent::Changed(download("n", DownloadState::Active, got, 100)));
        }
        events.push(DownloadEvent::Removed(vec!["old".into()]));
        assert_eq!(events.queue.lock().unwrap().len(), 2, "one entry per download");
        cx.run_until_parked();
        t.read_with(cx, |t, _| {
            assert_eq!(t.downloads().len(), 1);
            assert_eq!(t.downloads()[0].received_bytes, 99);
        });
    }

    #[test]
    fn formatting() {
        assert_eq!(format_progress(19_293_798, 46_137_344), "18.4 of 44.0 MB");
        assert_eq!(format_progress(10, 500), "10 of 500 B");
        assert_eq!(format_eta(0), "1 s left");
        assert_eq!(format_eta(3600 + 120), "1 h 2 min left");
        assert_eq!(format_eta(-1), "");
        assert!(is_risky_file("setup.EXE"));
        assert!(!is_risky_file("photo.png"));
        assert!(!is_risky_file("noext"));
    }
}

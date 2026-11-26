//! The composer's attachments (`stores/attachmentsStore.ts`, the orchestration half of
//! `features/chat/uploadAttachment.ts`): files attached to a channel's message box, their
//! upload state and progress. The transfer itself is `decibell-attachments`, reached
//! through [`AttachmentService`] (the app's [`CoreAttachmentService`]; a fake in tests).
//!
//! Lifecycle of one attachment: **queued** (probed, nothing sent: bytes leave only when the
//! message is sent) → **uploading** (progress, ~10 Hz from the service) → **ready** (the
//! server's attachment id; in an encrypted channel also the real metadata + key that go
//! into the message envelope) or **failed** (error, or cancelled). Removing one cancels its
//! upload. At most [`MAX_ATTACHMENTS_PER_MESSAGE`] per channel.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use decibell_attachments::upload::{PreparedUpload, UploadControl, UploadProgress, UploadSource, UploadedAttachment};
use decibell_attachments::{AttachmentKind, AttachmentTarget, Attachments, EncryptedAttachmentMeta, PinnedTls};
use futures::{FutureExt as _, StreamExt as _};
use futures::channel::{mpsc, oneshot};
use gpui::{App, Context, EventEmitter, SharedString, Task};

use crate::ids::{ChannelId, ServerId};

pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = decibell_attachments::upload::MAX_ATTACHMENTS_PER_MESSAGE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PendingStatus {
    Queued,
    Uploading,
    Ready,
    Failed,
}

impl PendingStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, PendingStatus::Ready | PendingStatus::Failed)
    }
}

#[derive(Clone)]
pub struct PendingAttachment {
    /// Client id (also on the optimistic bubble's `pending_attachments`).
    pub id: SharedString,
    pub server: ServerId,
    pub channel: ChannelId,
    pub status: PendingStatus,
    pub filename: SharedString,
    pub mime: SharedString,
    pub kind: AttachmentKind,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub speed_bps: f64,
    /// The server's id, once ready.
    pub attachment_id: Option<i64>,
    pub error: Option<SharedString>,
    /// Failed because the user cancelled it (not an error).
    pub cancelled: bool,
    /// Held from the Transfers panel: the upload waits before its next request.
    pub paused: bool,
    /// Claimed by a send in flight (a second Enter while uploads run must not send it
    /// again: `claimedPendingsRef`).
    pub claimed: bool,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub placeholder: SharedString,
    /// Encrypted channel: sealed client-side; the real metadata rides the envelope.
    pub encrypted: bool,
    /// Ready, encrypted: what the send puts in `encrypted_attachments`.
    pub sealed: Option<EncryptedAttachmentMeta>,
    pub(crate) file: Arc<PreparedUpload>,
    pub(crate) control: UploadControl,
}

pub enum ComposerEvent {
    /// A channel's attachments changed (added, progress, status, removed).
    Changed(ServerId, ChannelId),
}

impl EventEmitter<ComposerEvent> for ComposerAttachments {}

/// What the composer needs from the attachment service.
pub trait AttachmentService: 'static {
    /// Probe a file the user attached (size, kind, picture facts, thumbnails); nothing is
    /// sent.
    fn prepare(&self, source: UploadSource, cx: &mut App) -> Task<Result<PreparedUpload, String>>;
    /// Send a prepared file; progress arrives on the returned receiver (bounded rate).
    fn upload(
        &self,
        server: ServerId,
        channel: ChannelId,
        file: Arc<PreparedUpload>,
        encrypted: bool,
        control: UploadControl,
        cx: &mut App,
    ) -> (Task<Result<UploadedAttachment, UploadError>>, mpsc::UnboundedReceiver<UploadProgress>);
    /// A community's endpoint and token (community auth, token renewal); `None` forgets it.
    fn set_target(&self, server: ServerId, target: Option<AttachmentTarget>);
    /// Sign-out: every target and every key.
    fn clear_all(&self);
    /// The keys of a message's encrypted attachments (so their readers decrypt).
    fn register_keys(&self, server: ServerId, metas: Vec<EncryptedAttachmentMeta>);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadError {
    pub message: SharedString,
    pub cancelled: bool,
}

pub struct ComposerAttachments {
    /// In the order they were attached.
    pendings: Vec<PendingAttachment>,
    /// Running uploads: each exists until its upload ends.
    uploads: HashMap<SharedString, Task<()>>,
    /// Waits for an upload to end (`waitForUploads`).
    finished: HashMap<SharedString, Vec<oneshot::Sender<()>>>,
    service: Rc<dyn AttachmentService>,
    next_id: u64,
}

impl ComposerAttachments {
    pub fn new(service: Rc<dyn AttachmentService>) -> ComposerAttachments {
        ComposerAttachments { pendings: Vec::new(), uploads: HashMap::new(), finished: HashMap::new(), service, next_id: 0 }
    }

    pub fn service(&self) -> &Rc<dyn AttachmentService> {
        &self.service
    }

    pub fn get(&self, id: &str) -> Option<&PendingAttachment> {
        self.pendings.iter().find(|p| &*p.id == id)
    }

    /// A channel's attachments (the composer's row, `selectForChannel`).
    pub fn for_channel(&self, server: ServerId, channel: &str) -> impl Iterator<Item = &PendingAttachment> {
        self.pendings.iter().filter(move |p| p.server == server && &*p.channel == channel)
    }

    pub fn len(&self) -> usize {
        self.pendings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pendings.is_empty()
    }

    fn changed(&self, server: ServerId, channel: &ChannelId, cx: &mut Context<Self>) {
        cx.emit(ComposerEvent::Changed(server, channel.clone()));
        cx.notify();
    }

    /// Add a probed file as queued (`queueUpload`'s second half). Refused past the
    /// per-message cap.
    pub fn add(
        &mut self,
        server: ServerId,
        channel: ChannelId,
        file: PreparedUpload,
        encrypted: bool,
        cx: &mut Context<Self>,
    ) -> Result<SharedString, &'static str> {
        if self.for_channel(server, &channel).count() >= MAX_ATTACHMENTS_PER_MESSAGE {
            return Err("limit");
        }
        self.next_id += 1;
        let id: SharedString = format!("att-{}", self.next_id).into();
        self.pendings.push(PendingAttachment {
            id: id.clone(),
            server,
            channel: channel.clone(),
            status: PendingStatus::Queued,
            filename: file.name.clone().into(),
            mime: if file.mime.is_empty() { "application/octet-stream".into() } else { file.mime.clone().into() },
            kind: file.kind,
            total_bytes: file.size,
            transferred_bytes: 0,
            speed_bps: 0.0,
            attachment_id: None,
            error: None,
            cancelled: false,
            paused: false,
            claimed: false,
            width: file.width,
            height: file.height,
            duration_ms: file.duration_ms,
            placeholder: file.placeholder.clone().into(),
            encrypted,
            sealed: None,
            file: Arc::new(file),
            control: UploadControl::new(),
        });
        self.changed(server, &channel, cx);
        Ok(id)
    }

    /// Send a queued file's bytes (`startQueuedUpload`). No-op for one that isn't queued.
    pub fn start_upload(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(p) = self.pendings.iter_mut().find(|p| &*p.id == id) else { return };
        if p.status != PendingStatus::Queued {
            return;
        }
        p.status = PendingStatus::Uploading;
        let (server, channel, file, encrypted, control) = (p.server, p.channel.clone(), p.file.clone(), p.encrypted, p.control.clone());
        let id: SharedString = p.id.clone();
        let (result, mut progress) = self.service.upload(server, channel.clone(), file, encrypted, control, cx);
        let task = cx.spawn({
            let id = id.clone();
            async move |this, cx| {
                let mut result = std::pin::pin!(result);
                loop {
                    futures::select_biased! {
                        p = progress.next() => {
                            let Some(p) = p else { continue };
                            let _ = this.update(cx, |this, cx| this.progress(&id, p, cx));
                        }
                        r = (&mut result).fuse() => {
                            // Drain what arrived before the end.
                            while let Ok(p) = progress.try_recv() {
                                let _ = this.update(cx, |this, cx| this.progress(&id, p, cx));
                            }
                            let _ = this.update(cx, |this, cx| this.finish(&id, r, cx));
                            return;
                        }
                    }
                }
            }
        });
        self.uploads.insert(id, task);
        self.changed(server, &channel, cx);
    }

    fn progress(&mut self, id: &str, p: UploadProgress, cx: &mut Context<Self>) {
        let Some(pending) = self.pendings.iter_mut().find(|x| &*x.id == id) else { return };
        if pending.status.is_terminal() {
            return;
        }
        pending.transferred_bytes = p.sent;
        pending.speed_bps = p.speed_bps;
        pending.paused = p.paused;
        let (server, channel) = (pending.server, pending.channel.clone());
        self.changed(server, &channel, cx);
    }

    fn finish(&mut self, id: &SharedString, result: Result<UploadedAttachment, UploadError>, cx: &mut Context<Self>) {
        self.uploads.remove(id);
        let Some(p) = self.pendings.iter_mut().find(|x| x.id == *id) else { return };
        match result {
            Ok(done) => {
                p.status = PendingStatus::Ready;
                p.attachment_id = Some(done.id);
                p.kind = done.kind;
                p.mime = done.mime.into();
                p.filename = done.filename.into();
                p.transferred_bytes = p.total_bytes;
                p.sealed = done.encrypted;
            }
            Err(e) => {
                p.status = PendingStatus::Failed;
                p.error = Some(if e.cancelled { "Cancelled".into() } else { e.message });
                p.cancelled = e.cancelled;
                p.paused = false;
            }
        }
        let (server, channel) = (p.server, p.channel.clone());
        for waiter in self.finished.remove(id).unwrap_or_default() {
            let _ = waiter.send(());
        }
        self.changed(server, &channel, cx);
    }

    /// Resolves once each of `ids` is ready, failed or gone (`waitForUploads`, event-driven).
    pub fn wait_terminal(&mut self, ids: &[SharedString]) -> impl std::future::Future<Output = ()> + 'static + use<> {
        let mut waits = Vec::new();
        for id in ids {
            if self.get(id).is_some_and(|p| !p.status.is_terminal()) {
                let (tx, rx) = oneshot::channel();
                self.finished.entry(id.clone()).or_default().push(tx);
                waits.push(rx);
            }
        }
        async move {
            for w in waits {
                let _ = w.await;
            }
        }
    }

    /// Claim attachments for a send (or release them when it failed).
    pub fn claim(&mut self, ids: &[SharedString], claimed: bool, cx: &mut Context<Self>) {
        let mut touched = Vec::new();
        for p in self.pendings.iter_mut().filter(|p| ids.contains(&p.id)) {
            if p.claimed != claimed {
                p.claimed = claimed;
                touched.push((p.server, p.channel.clone()));
            }
        }
        touched.dedup();
        for (server, channel) in touched {
            self.changed(server, &channel, cx);
        }
    }

    /// Pause / resume from the Transfers panel.
    pub fn set_paused(&mut self, id: &str, paused: bool, cx: &mut Context<Self>) {
        let Some(p) = self.pendings.iter_mut().find(|x| &*x.id == id) else { return };
        if p.paused == paused {
            return;
        }
        if paused { p.control.pause() } else { p.control.resume() }
        p.paused = paused;
        let (server, channel) = (p.server, p.channel.clone());
        self.changed(server, &channel, cx);
    }

    /// Cancel an upload in flight (it ends as failed, cancelled).
    pub fn cancel(&mut self, id: &str) {
        if let Some(p) = self.get(id) {
            p.control.cancel();
        }
    }

    /// Drop an attachment (the chip's ×, or its message went out). A running upload is
    /// cancelled.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(i) = self.pendings.iter().position(|p| &*p.id == id) else { return };
        let p = self.pendings.remove(i);
        p.control.cancel();
        self.uploads.remove(id);
        for waiter in self.finished.remove(id).unwrap_or_default() {
            let _ = waiter.send(());
        }
        self.changed(p.server, &p.channel, cx);
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        for p in &self.pendings {
            p.control.cancel();
        }
        self.pendings.clear();
        self.uploads.clear();
        self.finished.clear();
        cx.notify();
    }
}

// ── The app's service: decibell-attachments on the core's runtime ──────────

/// Trust for the attachment host: exactly the certificate the community's chat connection
/// presented (core's pinning verifier). When the core grows a side-effect-free
/// `tls::exact_connector(fingerprint)`, the body becomes that one line.
pub struct CoreTls;

impl PinnedTls for CoreTls {
    fn connector(&self, host: &str, port: u16, fingerprint: &str) -> Result<tokio_rustls::TlsConnector, String> {
        decibell_core::net::pins::set_expected(host, port, fingerprint);
        Ok(decibell_core::net::tls::create_pinned_connector(host, port).0)
    }
}

enum Op {
    SetTarget(String, Option<AttachmentTarget>),
    ClearAll,
    Keys(String, Vec<EncryptedAttachmentMeta>),
    Prepare(UploadSource, oneshot::Sender<Result<PreparedUpload, String>>),
    #[allow(clippy::type_complexity)]
    Upload {
        server: String,
        channel: String,
        file: Arc<PreparedUpload>,
        encrypted: bool,
        control: UploadControl,
        progress: mpsc::UnboundedSender<UploadProgress>,
        done: oneshot::Sender<Result<UploadedAttachment, UploadError>>,
    },
}

/// [`AttachmentService`] over `decibell_attachments::Attachments`: one service per process,
/// created on the core's runtime at first use; every operation goes through one ordered
/// queue there (a target set on reconnect never overtakes the clear before it). Uploads run
/// as their own tasks. Nothing runs while idle (the queue's task is parked).
#[derive(Default)]
pub struct CoreAttachmentService {
    ops: Mutex<Option<mpsc::UnboundedSender<Op>>>,
}

static SERVICE: OnceLock<Arc<Attachments>> = OnceLock::new();

impl CoreAttachmentService {
    /// The process's `Attachments` once the queue started (the media readers of the views
    /// use it too).
    pub fn attachments() -> Option<Arc<Attachments>> {
        SERVICE.get().cloned()
    }

    fn send(&self, op: Op) {
        let mut ops = self.ops.lock().unwrap_or_else(|e| e.into_inner());
        if ops.as_ref().is_none_or(|tx| tx.is_closed()) {
            if !crate::core::is_running() {
                return;
            }
            let (tx, mut rx) = mpsc::unbounded::<Op>();
            crate::core::spawn_detached(async move {
                let svc = SERVICE
                    .get_or_init(|| Attachments::new(Arc::new(CoreTls), tokio::runtime::Handle::current()))
                    .clone();
                while let Some(op) = rx.next().await {
                    match op {
                        Op::SetTarget(server, Some(t)) => svc.set_target(&server, t),
                        Op::SetTarget(server, None) => svc.clear_target(&server),
                        Op::ClearAll => {
                            svc.clear_all_targets();
                            svc.clear_keys(None);
                        }
                        Op::Keys(server, metas) => svc.register_keys(&server, &metas),
                        Op::Prepare(source, done) => {
                            tokio::spawn(async move {
                                let r = decibell_attachments::upload::prepare(source, None).await;
                                let _ = done.send(r.map_err(|e| e.to_string()));
                            });
                        }
                        Op::Upload { server, channel, file, encrypted, control, progress, done } => {
                            let svc = svc.clone();
                            tokio::spawn(async move {
                                let r = svc
                                    .upload(&server, &channel, &file, encrypted, &control, move |p| {
                                        let _ = progress.unbounded_send(p);
                                    })
                                    .await;
                                let _ = done.send(r.map_err(|e| UploadError {
                                    cancelled: matches!(e, decibell_attachments::Error::Cancelled),
                                    message: e.to_string().into(),
                                }));
                            });
                        }
                    }
                }
            });
            *ops = Some(tx);
        }
        if let Some(tx) = ops.as_ref() {
            let _ = tx.unbounded_send(op);
        }
    }
}

impl AttachmentService for CoreAttachmentService {
    fn prepare(&self, source: UploadSource, cx: &mut App) -> Task<Result<PreparedUpload, String>> {
        let (tx, rx) = oneshot::channel();
        self.send(Op::Prepare(source, tx));
        cx.spawn(async move |_| rx.await.unwrap_or_else(|_| Err("Not connected".into())))
    }

    fn upload(
        &self,
        server: ServerId,
        channel: ChannelId,
        file: Arc<PreparedUpload>,
        encrypted: bool,
        control: UploadControl,
        cx: &mut App,
    ) -> (Task<Result<UploadedAttachment, UploadError>>, mpsc::UnboundedReceiver<UploadProgress>) {
        let (progress, progress_rx) = mpsc::unbounded();
        let (done, rx) = oneshot::channel();
        self.send(Op::Upload { server: server.wire(), channel: channel.to_string(), file, encrypted, control, progress, done });
        let task = cx.spawn(async move |_| {
            rx.await.unwrap_or_else(|_| Err(UploadError { message: "Not connected".into(), cancelled: false }))
        });
        (task, progress_rx)
    }

    fn set_target(&self, server: ServerId, target: Option<AttachmentTarget>) {
        self.send(Op::SetTarget(server.wire(), target));
    }

    fn clear_all(&self) {
        self.send(Op::ClearAll);
    }

    fn register_keys(&self, server: ServerId, metas: Vec<EncryptedAttachmentMeta>) {
        if !metas.is_empty() {
            self.send(Op::Keys(server.wire(), metas));
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    pub type FakeUpload = (SharedString, oneshot::Sender<Result<UploadedAttachment, UploadError>>, mpsc::UnboundedSender<UploadProgress>);

    /// Uploads that finish when the test says.
    #[derive(Default)]
    pub struct FakeService {
        pub uploads: RefCell<Vec<FakeUpload>>,
        pub targets: RefCell<Vec<(ServerId, bool)>>,
        pub keys: RefCell<usize>,
    }

    pub struct Fake(pub Rc<FakeService>);

    impl AttachmentService for Fake {
        fn prepare(&self, source: UploadSource, _: &mut App) -> Task<Result<PreparedUpload, String>> {
            Task::ready(Ok(prepared(match source {
                UploadSource::Memory { name, .. } => name,
                UploadSource::Path(p) => p.display().to_string(),
            })))
        }
        fn upload(
            &self,
            _: ServerId,
            _: ChannelId,
            file: Arc<PreparedUpload>,
            _: bool,
            _: UploadControl,
            cx: &mut App,
        ) -> (Task<Result<UploadedAttachment, UploadError>>, mpsc::UnboundedReceiver<UploadProgress>) {
            let (tx, rx) = oneshot::channel();
            let (ptx, prx) = mpsc::unbounded();
            self.0.uploads.borrow_mut().push((file.name.clone().into(), tx, ptx));
            (cx.spawn(async move |_| rx.await.unwrap_or(Err(UploadError { message: "gone".into(), cancelled: true }))), prx)
        }
        fn set_target(&self, server: ServerId, target: Option<AttachmentTarget>) {
            self.0.targets.borrow_mut().push((server, target.is_some()));
        }
        fn clear_all(&self) {
            self.0.targets.borrow_mut().clear();
        }
        fn register_keys(&self, _: ServerId, metas: Vec<EncryptedAttachmentMeta>) {
            *self.0.keys.borrow_mut() += metas.len();
        }
    }

    pub fn prepared(name: String) -> PreparedUpload {
        PreparedUpload {
            source: UploadSource::Memory { name: name.clone(), mime: "image/png".into(), data: Default::default() },
            name,
            mime: "image/png".into(),
            kind: AttachmentKind::Image,
            size: 1000,
            width: 10,
            height: 10,
            duration_ms: 0,
            placeholder: String::new(),
            thumbnails: vec![],
        }
    }

    pub fn uploaded(id: i64, name: &str) -> UploadedAttachment {
        UploadedAttachment {
            id,
            kind: AttachmentKind::Image,
            filename: name.into(),
            mime: "image/png".into(),
            size_bytes: 1000,
            thumbnail_sizes_mask: 7,
            encrypted: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::testing::*;
    use super::*;

    #[gpui::test]
    fn queued_uploading_ready_or_failed_and_the_cap(cx: &mut TestAppContext) {
        let fake = Rc::new(FakeService::default());
        let composer = cx.new(|_| ComposerAttachments::new(Rc::new(Fake(fake.clone()))));
        let server = ServerId::from_central(1);
        let channel: ChannelId = "general".into();
        let ids: Vec<SharedString> = composer.update(cx, |c, cx| {
            let ids: Vec<_> = (0..MAX_ATTACHMENTS_PER_MESSAGE)
                .map(|i| c.add(server, channel.clone(), prepared(format!("f{i}")), false, cx).unwrap())
                .collect();
            assert_eq!(c.add(server, channel.clone(), prepared("over".into()), false, cx), Err("limit"));
            // Another channel has its own cap.
            assert!(c.add(server, "clips".into(), prepared("other".into()), false, cx).is_ok());
            c.start_upload(&ids[0], cx);
            c.start_upload(&ids[1], cx);
            c.start_upload(&ids[1], cx); // already uploading: once
            ids
        });
        assert_eq!(fake.uploads.borrow().len(), 2);
        let wait = composer.update(cx, |c, _| c.wait_terminal(&ids[..2]));
        let done = Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        cx.spawn(async move |_| {
            wait.await;
            d.set(true);
        })
        .detach();
        let (_, tx0, p0) = fake.uploads.borrow_mut().remove(0);
        p0.unbounded_send(UploadProgress { sent: 500, total: 1000, speed_bps: 10.0, paused: false }).unwrap();
        cx.run_until_parked();
        composer.read_with(cx, |c, _| assert_eq!(c.get(&ids[0]).unwrap().transferred_bytes, 500));
        tx0.send(Ok(uploaded(77, "f0"))).unwrap();
        cx.run_until_parked();
        assert!(!done.get(), "one still uploading");
        let (_, tx1, _) = fake.uploads.borrow_mut().remove(0);
        tx1.send(Err(UploadError { message: "HTTP 500".into(), cancelled: false })).unwrap();
        cx.run_until_parked();
        assert!(done.get());
        composer.update(cx, |c, cx| {
            let a = c.get(&ids[0]).unwrap();
            assert_eq!((a.status, a.attachment_id, a.transferred_bytes), (PendingStatus::Ready, Some(77), 1000));
            let b = c.get(&ids[1]).unwrap();
            assert_eq!((b.status, b.error.as_deref()), (PendingStatus::Failed, Some("HTTP 500")));
            c.remove(&ids[0], cx);
            assert_eq!(c.for_channel(server, "general").count(), MAX_ATTACHMENTS_PER_MESSAGE - 1);
        });
    }
}

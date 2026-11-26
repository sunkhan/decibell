//! Uploading (`uploadAttachment.ts`): probe and prepare a file when it is attached
//! ([`prepare`], the renderer's `queueUpload`; nothing leaves the machine yet), then send it when
//! the message is sent ([`Attachments::upload`], `startQueuedUpload`):
//!
//! `POST /attachments/init` → `PATCH /attachments/<id>` with `Upload-Offset` (tus semantics) in
//! 8 MiB requests (half a second of the cap while one is set) → `POST …/complete` →
//! `POST …/thumbnail?size=N` for 320 / 640 / 1280, best effort.
//!
//! Each PATCH body is streamed from the file in 64 KiB slices (sealed on the fly in an
//! encrypted channel, paced to the upload cap per slice), so an upload holds a couple of
//! slices in memory whatever the file size. Pause holds before the next PATCH (as the renderer
//! did); cancel aborts at once. A failed PATCH is retried 5 times with backoff
//! (250 ms · 2ⁿ, at most 8 s), and before each retry the server's offset is read back
//! (`HEAD`) so the upload resumes where the server actually is: the renderer retried the same
//! offset, which a half-written PATCH turned into a run of 409s. Sealed chunks re-seal to the
//! same bytes (deterministic nonces), so a sealed upload resumes mid-chunk too.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use http::header::{HeaderName, HeaderValue, CONTENT_TYPE};
use http::Method;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{mpsc, watch, Notify};
use tokio_util::sync::CancellationToken;

use crate::crypto::{ciphertext_offset, ciphertext_size, FileKey, CHUNK_BYTES, TAG_BYTES};
use crate::http::{head_timeout_for, ReqBody, Response, Target, STALL_TIMEOUT};
use crate::kind::{guess_mime, AttachmentKind};
use crate::pacer::{upload_chunk_bytes, Direction, Pacer};
use crate::thumbs::{self, thumb_bit, MediaProbe, ProbeSource};
use crate::{AttachmentKey, Attachments, EncryptedAttachmentMeta, Error};

/// Per-message attachment cap the composer enforces (`MAX_ATTACHMENTS_PER_MESSAGE`).
pub const MAX_ATTACHMENTS_PER_MESSAGE: usize = 10;
const MAX_RETRY: u32 = 5;
/// Progress callbacks at most this often (the store's 10 Hz throttle), plus every state change
/// and the final byte.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Upload speed sampling (`uploads.ts`).
const SPEED_SAMPLE: Duration = Duration::from_millis(400);

/// Where an upload's bytes come from.
#[derive(Clone, Debug)]
pub enum UploadSource {
    /// A picked or dropped file, streamed from disk.
    Path(PathBuf),
    /// Bytes already in memory (a pasted picture). `mime` "" = guessed from `name`.
    Memory { name: String, mime: String, data: Bytes },
}

/// A file probed and ready to send (the composer's queued chip).
#[derive(Clone, Debug)]
pub struct PreparedUpload {
    pub source: UploadSource,
    pub name: String,
    pub mime: String,
    pub kind: AttachmentKind,
    pub size: u64,
    /// Shown size of an image / video (orientation applied), 0 unknown.
    pub width: u32,
    pub height: u32,
    /// Audio / video duration, 0 unknown.
    pub duration_ms: u32,
    /// base64 ThumbHash, "" when unavailable (never a reason to fail).
    pub placeholder: String,
    /// `(size_px, jpeg)` thumbnails made from the image or the video's poster frame.
    pub thumbnails: Vec<(u32, Vec<u8>)>,
}

/// Probe a file the user attached: size, name, MIME (by extension, `guessMime`), kind, and for
/// pictures their shown size, ThumbHash and thumbnails (decoded once, off the caller's thread).
/// Videos and audio go through `probe` when the app has one. A picture that fails to decode
/// still uploads, without metadata (the renderer's behaviour).
pub async fn prepare(source: UploadSource, probe: Option<Arc<dyn MediaProbe>>) -> Result<PreparedUpload, Error> {
    let (name, mime, size) = match &source {
        UploadSource::Path(p) => {
            let md = tokio::fs::metadata(p).await?;
            if !md.is_file() {
                return Err(Error::Invalid("not a file"));
            }
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let mime = guess_mime(&name).to_string();
            (name, mime, md.len())
        }
        UploadSource::Memory { name, mime, data } => {
            let mime = if mime.is_empty() { guess_mime(name).to_string() } else { mime.clone() };
            (name.clone(), mime, data.len() as u64)
        }
    };
    let kind = AttachmentKind::from_mime(&mime);
    let mut prepared = PreparedUpload {
        source,
        name,
        mime,
        kind,
        size,
        width: 0,
        height: 0,
        duration_ms: 0,
        placeholder: String::new(),
        thumbnails: Vec::new(),
    };
    let src = prepared.source.clone();
    match kind {
        AttachmentKind::Image => {
            let facts = tokio::task::spawn_blocking(move || {
                let img = match &src {
                    UploadSource::Path(p) => thumbs::decode_image_file(p),
                    UploadSource::Memory { data, .. } => thumbs::decode_image_bytes(data),
                };
                img.ok().map(thumbs::image_facts)
            })
            .await
            .map_err(|_| Error::Invalid("image probe panicked"))?;
            if let Some(f) = facts {
                prepared.width = f.width;
                prepared.height = f.height;
                prepared.placeholder = f.placeholder;
                prepared.thumbnails = f.thumbnails;
            }
        }
        AttachmentKind::Video | AttachmentKind::Audio => {
            if let Some(probe) = probe {
                let probed = tokio::task::spawn_blocking(move || {
                    let source = match &src {
                        UploadSource::Path(p) => ProbeSource::Path(p),
                        UploadSource::Memory { data, .. } => ProbeSource::Bytes(data),
                    };
                    let mut m = probe.probe(source, kind)?;
                    let facts = if kind == AttachmentKind::Video {
                        m.poster.take().map(|p| thumbs::image_facts(image::DynamicImage::ImageRgba8(p)))
                    } else {
                        None
                    };
                    Some((m, facts))
                })
                .await
                .map_err(|_| Error::Invalid("media probe panicked"))?;
                if let Some((m, facts)) = probed {
                    prepared.width = m.width;
                    prepared.height = m.height;
                    prepared.duration_ms = m.duration_ms;
                    if let Some(f) = facts {
                        prepared.placeholder = f.placeholder;
                        prepared.thumbnails = f.thumbnails;
                    }
                }
            }
        }
        AttachmentKind::Document => {}
    }
    Ok(prepared)
}

/// Pause / resume / cancel for one upload, shared with the Transfers panel.
#[derive(Clone, Debug, Default)]
pub struct UploadControl {
    inner: Arc<Control>,
}

#[derive(Debug, Default)]
struct Control {
    paused: AtomicBool,
    wake: Notify,
    cancel: CancellationToken,
}

impl UploadControl {
    pub fn new() -> Self {
        Self::default()
    }
    /// Hold before the next PATCH (the message waits with it).
    pub fn pause(&self) {
        self.inner.paused.store(true, Ordering::SeqCst);
        self.inner.wake.notify_waiters();
    }
    pub fn resume(&self) {
        self.inner.paused.store(false, Ordering::SeqCst);
        self.inner.wake.notify_waiters();
    }
    pub fn cancel(&self) {
        self.inner.cancel.cancel();
    }
    pub fn is_paused(&self) -> bool {
        self.inner.paused.load(Ordering::SeqCst)
    }
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancel.is_cancelled()
    }
    fn token(&self) -> &CancellationToken {
        &self.inner.cancel
    }
}

/// What the progress callback hears, at most every 100 ms and on each change of `paused`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UploadProgress {
    /// Plaintext bytes sent so far (may step back when a failed PATCH is retried).
    pub sent: u64,
    pub total: u64,
    /// EWMA over ~0.4 s samples; 0 while paused.
    pub speed_bps: f64,
    pub paused: bool,
}

/// A finished upload: bind `id` to the message (`attachment_ids`), and in an encrypted channel
/// hand `encrypted` to the send path (`encrypted_attachments`), which seals it into the
/// envelope. The key is also registered with the service, so our own echo opens at once.
#[derive(Clone, Debug)]
pub struct UploadedAttachment {
    pub id: i64,
    pub kind: AttachmentKind,
    pub filename: String,
    pub mime: String,
    pub size_bytes: u64,
    pub thumbnail_sizes_mask: u32,
    pub encrypted: Option<EncryptedAttachmentMeta>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitResponse {
    id: i64,
    #[serde(default)]
    upload_offset: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompleteResponse {
    id: i64,
    kind: i64,
    filename: String,
    mime: String,
    #[allow(dead_code)]
    size_bytes: u64,
}

struct Progress<F: FnMut(UploadProgress)> {
    cb: F,
    total: u64,
    last_emit: Option<Instant>,
    last_paused: bool,
    sample_at: Instant,
    sample_bytes: u64,
    speed: f64,
}

impl<F: FnMut(UploadProgress)> Progress<F> {
    fn report(&mut self, sent: u64, paused: bool, force: bool) {
        let now = Instant::now();
        if paused {
            self.speed = 0.0;
            self.sample_at = now;
            self.sample_bytes = sent;
        } else {
            let dt = now.duration_since(self.sample_at);
            if dt >= SPEED_SAMPLE && sent != self.sample_bytes {
                let inst = sent.saturating_sub(self.sample_bytes) as f64 / dt.as_secs_f64();
                self.speed = if self.speed == 0.0 { inst } else { self.speed * 0.7 + inst * 0.3 };
                self.sample_at = now;
                self.sample_bytes = sent;
            }
        }
        let due = self.last_emit.is_none_or(|t| now.duration_since(t) >= PROGRESS_INTERVAL);
        if force || due || paused != self.last_paused || sent >= self.total {
            self.last_emit = Some(now);
            self.last_paused = paused;
            (self.cb)(UploadProgress { sent, total: self.total, speed_bps: self.speed, paused });
        }
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis((250u64 << attempt.min(6)).min(8000))
}

async fn sleep_or_cancel(d: Duration, cancel: &CancellationToken) -> Result<(), Error> {
    tokio::select! {
        _ = tokio::time::sleep(d) => Ok(()),
        _ = cancel.cancelled() => Err(Error::Cancelled),
    }
}

impl Attachments {
    /// Send a prepared file to `channel_id` (sealed client-side when `encrypted`, i.e. the
    /// channel's flag). `progress` is called from this future, at a bounded rate.
    pub async fn upload(
        self: &Arc<Self>,
        server_id: &str,
        channel_id: &str,
        file: &PreparedUpload,
        encrypted: bool,
        control: &UploadControl,
        progress: impl FnMut(UploadProgress) + Send,
    ) -> Result<UploadedAttachment, Error> {
        let mut prog = Progress {
            cb: progress,
            total: file.size,
            last_emit: None,
            last_paused: false,
            sample_at: Instant::now(),
            sample_bytes: 0,
            speed: 0.0,
        };
        let key = encrypted.then(FileKey::generate);
        let mut id = 0i64;
        let result = self.upload_inner(server_id, channel_id, file, key.as_ref(), control, &mut prog, &mut id).await;
        if result.is_err() && id > 0 && !matches!(result, Err(Error::UploadExpired)) {
            // Abandon the server-side row now instead of leaving it to the idle sweep (it
            // counts against the server's free-space headroom until then).
            let svc = self.clone();
            let server_id = server_id.to_string();
            self.rt.spawn(async move {
                let _ = svc.simple(&server_id, Method::DELETE, &format!("/attachments/{id}"), &[], None).await;
            });
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn upload_inner<F: FnMut(UploadProgress) + Send>(
        self: &Arc<Self>,
        server_id: &str,
        channel_id: &str,
        file: &PreparedUpload,
        key: Option<&FileKey>,
        control: &UploadControl,
        prog: &mut Progress<F>,
        id_out: &mut i64,
    ) -> Result<UploadedAttachment, Error> {
        let cancel = control.token().clone();
        let target = self.require_target(server_id)?;
        let size = file.size;
        if size == 0 {
            // The server refuses empty files at init (size <= 0 → 400).
            return Err(Error::Invalid("empty files can't be uploaded"));
        }
        let wire_total = if key.is_some() { ciphertext_size(size, CHUNK_BYTES) } else { size };
        if target.max_bytes > 0 && wire_total > target.max_bytes {
            return Err(Error::TooLarge);
        }

        // ---- init
        let body = if key.is_some() {
            // Stand-ins: the server stores ciphertext, needs the kind (retention is per kind)
            // and the ciphertext size; the real metadata rides the envelope.
            serde_json::json!({
                "channelId": channel_id,
                "filename": "encrypted",
                "mime": "application/octet-stream",
                "size": wire_total,
                "kind": file.kind.index(),
                "encrypted": true,
            })
        } else {
            serde_json::json!({
                "channelId": channel_id,
                "filename": file.name,
                "mime": file.mime,
                "size": size,
                "width": file.width,
                "height": file.height,
                "durationMs": file.duration_ms,
                "placeholder": file.placeholder,
            })
        };
        let body = Bytes::from(serde_json::to_vec(&body).expect("json"));
        let json = [(CONTENT_TYPE, HeaderValue::from_static("application/json"))];
        let mut attempt = 0;
        let init: InitResponse = loop {
            let r = crate::fetch::with_cancel(self.simple(server_id, Method::POST, "/attachments/init", &json, Some(body.clone())), Some(&cancel)).await;
            match r {
                Ok(resp) if resp.status == 201 || resp.status == 200 => {
                    let bytes = resp.collect(64 * 1024).await?;
                    break serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("bad init response"))?;
                }
                // The per-account init bucket (20, then one per 2 s): wait and try again.
                Ok(resp) if resp.status == 429 && attempt < MAX_RETRY => {
                    attempt += 1;
                    sleep_or_cancel(Duration::from_secs(2) * attempt, &cancel).await?;
                }
                Ok(resp) => return Err(Error::from_status(resp.status)),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e @ (Error::NotConnected | Error::CertMismatch(_) | Error::Invalid(_))) => return Err(e),
                Err(e) => {
                    attempt += 1;
                    if attempt > MAX_RETRY {
                        return Err(e);
                    }
                    sleep_or_cancel(backoff(attempt), &cancel).await?;
                }
            }
        };
        *id_out = init.id;
        let id = init.id;
        // `wire_off` walks what the server holds (ciphertext offsets when sealed). A fresh
        // init always answers 0.
        let mut wire_off = if key.is_some() { 0 } else { init.upload_offset.min(size) };
        let upload_pacer = Arc::new(tokio::sync::Mutex::new(Pacer::new()));
        prog.report(plain_of(wire_off, key.is_some(), size), false, true);

        // ---- PATCH loop
        let mut attempt = 0u32;
        while wire_off < wire_total {
            // Paused from the Transfers panel: hold before the next PATCH.
            while control.is_paused() {
                let woken = control.inner.wake.notified();
                tokio::pin!(woken);
                woken.as_mut().enable();
                if !control.is_paused() {
                    break;
                }
                prog.report(plain_of(wire_off, key.is_some(), size), true, true);
                tokio::select! {
                    _ = woken => {}
                    _ = cancel.cancelled() => return Err(Error::Cancelled),
                }
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let (plain_start, skip) = match key {
                Some(_) => {
                    let sc = CHUNK_BYTES + TAG_BYTES;
                    ((wire_off / sc) * CHUNK_BYTES, (wire_off % sc) as usize)
                }
                None => (wire_off, 0),
            };
            let span = upload_chunk_bytes(self.limits().upload_rate());
            let plain_end = (plain_start + span).min(size);
            let wire_end = match key {
                Some(_) if plain_end == size => wire_total,
                Some(_) => ciphertext_offset(plain_end / CHUNK_BYTES, CHUNK_BYTES),
                None => plain_end,
            };
            let len = wire_end - wire_off;
            let (sent_tx, mut sent_rx) = watch::channel(plain_start);
            let make_body = || {
                spawn_body(
                    self.clone(),
                    file.source.clone(),
                    key.cloned(),
                    plain_start,
                    skip,
                    plain_end,
                    len,
                    upload_pacer.clone(),
                    cancel.clone(),
                    sent_tx.clone(),
                )
            };
            let headers = [
                (HeaderName::from_static("upload-offset"), HeaderValue::from(wire_off)),
                (CONTENT_TYPE, HeaderValue::from_static("application/octet-stream")),
            ];
            let path = format!("/attachments/{id}");
            let target = self.require_target(server_id)?;
            let ep = target.endpoint();
            let send = self.http.send(Target { endpoint: &ep, jwt: &target.jwt }, Method::PATCH, &path, &headers, make_body, head_timeout_for(len));
            tokio::pin!(send);
            let outcome = loop {
                tokio::select! {
                    r = &mut send => break r,
                    _ = cancel.cancelled() => return Err(Error::Cancelled),
                    changed = sent_rx.changed() => {
                        if changed.is_ok() {
                            let sent = *sent_rx.borrow_and_update();
                            prog.report(sent, false, false);
                        }
                    }
                }
            };
            let failure = match outcome {
                Ok(resp) if resp.status == 204 => {
                    let server_off = resp.header("upload-offset").and_then(|v| v.parse::<u64>().ok());
                    wire_off = server_off.unwrap_or(wire_end).min(wire_total);
                    attempt = 0;
                    prog.report(plain_of(wire_off, key.is_some(), size), false, false);
                    continue;
                }
                // Swept (an hour without progress) or deleted: retrying can't help.
                Ok(resp) if resp.status == 404 => return Err(Error::UploadExpired),
                Ok(resp) if matches!(resp.status, 400 | 401 | 403 | 411 | 413) => return Err(Error::from_status(resp.status)),
                Ok(resp) => Error::from_status(resp.status),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e @ (Error::NotConnected | Error::CertMismatch(_))) => return Err(e),
                Err(e) => e,
            };
            attempt += 1;
            if attempt > MAX_RETRY {
                return Err(failure);
            }
            log::debug!("upload {id}: PATCH at {wire_off} failed ({failure}); retry {attempt}");
            sleep_or_cancel(backoff(attempt), &cancel).await?;
            // Realign on what the server really holds (tus: HEAD after a failed PATCH). A
            // refusal the server sends before reading the body (404, 403, 409, 413) closes the
            // connection and can reach us as a write error instead; the HEAD settles a sweep.
            match self.upload_offset(server_id, id, &cancel).await {
                Ok(off) => wire_off = off.min(wire_total),
                Err(Error::Http(404)) => return Err(Error::UploadExpired),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(_) => {}
            }
        }

        // ---- complete
        let resp = crate::fetch::with_cancel(self.simple(server_id, Method::POST, &format!("/attachments/{id}/complete"), &[], None), Some(&cancel)).await?;
        if resp.status != 200 {
            return Err(Error::from_status(resp.status));
        }
        let done: CompleteResponse =
            serde_json::from_slice(&resp.collect(64 * 1024).await?).map_err(|_| Error::Invalid("bad complete response"))?;
        prog.report(size, false, true);
        let final_kind = AttachmentKind::from_index(done.kind).unwrap_or(file.kind);

        // ---- thumbnails (best effort; the file is complete, so a cancel no longer applies)
        let mut mask = 0u32;
        if matches!(final_kind, AttachmentKind::Image | AttachmentKind::Video) {
            for (size_px, jpeg) in &file.thumbnails {
                let body = match key {
                    Some(k) => Bytes::from(k.seal_thumbnail(*size_px, jpeg)),
                    None => Bytes::copy_from_slice(jpeg),
                };
                upload_pacer.lock().await.take(body.len(), self.limits(), Direction::Up, None).await;
                let headers = [(CONTENT_TYPE, HeaderValue::from_static("image/jpeg"))];
                let path = format!("/attachments/{}/thumbnail?size={size_px}", done.id);
                if let Ok(r) = self.simple(server_id, Method::POST, &path, &headers, Some(body)).await {
                    if r.status == 204 {
                        mask |= thumb_bit(*size_px);
                    }
                    r.drain().await;
                }
            }
        }

        let encrypted = key.map(|k| EncryptedAttachmentMeta {
            id: done.id,
            key_b64: k.to_b64(),
            filename: file.name.clone(),
            mime: file.mime.clone(),
            size_bytes: size as i64,
            width: file.width,
            height: file.height,
            duration_ms: file.duration_ms,
            placeholder: file.placeholder.clone(),
            chunk_bytes: CHUNK_BYTES as u32,
            thumbnail_sizes_mask: mask,
        });
        if let Some(k) = key {
            self.register_key(
                server_id,
                done.id,
                AttachmentKey {
                    key: k.clone(),
                    chunk_bytes: CHUNK_BYTES,
                    size_bytes: size,
                    mime: file.mime.clone(),
                    filename: file.name.clone(),
                },
            );
        }
        Ok(UploadedAttachment {
            id: done.id,
            kind: final_kind,
            filename: if key.is_some() { file.name.clone() } else { done.filename },
            mime: if key.is_some() { file.mime.clone() } else { done.mime },
            size_bytes: size,
            thumbnail_sizes_mask: mask,
            encrypted,
        })
    }

    /// The server's offset of an unfinished upload (`HEAD`, `Upload-Offset`).
    pub async fn upload_offset(self: &Arc<Self>, server_id: &str, id: i64, cancel: &CancellationToken) -> Result<u64, Error> {
        let resp = crate::fetch::with_cancel(self.simple(server_id, Method::HEAD, &format!("/attachments/{id}"), &[], None), Some(cancel)).await?;
        if resp.status != 200 {
            return Err(Error::from_status(resp.status));
        }
        resp.header("upload-offset").and_then(|v| v.trim().parse().ok()).ok_or(Error::Invalid("no Upload-Offset"))
    }

    /// One small request with an optional buffered body.
    pub(crate) async fn simple(
        self: &Arc<Self>,
        server_id: &str,
        method: Method,
        path: &str,
        headers: &[(HeaderName, HeaderValue)],
        body: Option<Bytes>,
    ) -> Result<Response, Error> {
        let target = self.require_target(server_id)?;
        let ep = target.endpoint();
        let len = body.as_ref().map_or(0, |b| b.len() as u64);
        let timeout = if len > 0 { head_timeout_for(len) } else { STALL_TIMEOUT };
        self.http
            .send(
                Target { endpoint: &ep, jwt: &target.jwt },
                method,
                path,
                headers,
                || match &body {
                    Some(b) => ReqBody::Full(Some(b.clone())),
                    None => ReqBody::Empty,
                },
                timeout,
            )
            .await
    }
}

/// Plaintext bytes covered by a wire offset.
fn plain_of(wire_off: u64, sealed: bool, size: u64) -> u64 {
    if !sealed {
        return wire_off;
    }
    let sc = CHUNK_BYTES + TAG_BYTES;
    let full = (wire_off / sc) * CHUNK_BYTES;
    (full + (wire_off % sc).min(CHUNK_BYTES)).min(size)
}

/// The PATCH body for plaintext `[plain_start, plain_end)`: a producer task reads 64 KiB slices
/// (one sealed chunk each when sealed), seals them, waits on the upload cap and hands them to
/// the request (at most two queued). `skip` drops that many bytes of the first sealed chunk
/// (resuming mid-chunk; always 0 for a plain upload, whose offsets are plaintext offsets).
#[allow(clippy::too_many_arguments)]
fn spawn_body(
    svc: Arc<Attachments>,
    source: UploadSource,
    key: Option<FileKey>,
    plain_start: u64,
    skip: usize,
    plain_end: u64,
    wire_len: u64,
    pacer: Arc<tokio::sync::Mutex<Pacer>>,
    cancel: CancellationToken,
    sent: watch::Sender<u64>,
) -> ReqBody {
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(2);
    let rt = svc.rt.clone();
    rt.spawn(async move {
        let slice = CHUNK_BYTES as usize;
        let mut file = match &source {
            UploadSource::Path(p) => match tokio::fs::File::open(p).await {
                Ok(mut f) => {
                    if let Err(e) = f.seek(std::io::SeekFrom::Start(plain_start)).await {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                    Some(f)
                }
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            },
            UploadSource::Memory { .. } => None,
        };
        let mut buf = BytesMut::with_capacity(2 * (slice + TAG_BYTES as usize));
        let mut pos = plain_start;
        let mut first = true;
        while pos < plain_end {
            if cancel.is_cancelled() || tx.is_closed() {
                return;
            }
            let n = (plain_end - pos).min(slice as u64) as usize;
            let frame: Bytes = match (&source, &key) {
                // Plain from memory: a zero-copy view.
                (UploadSource::Memory { data, .. }, None) => data.slice(pos as usize..pos as usize + n),
                _ => {
                    // Reclaims the previous slice's allocation once the request dropped it.
                    buf.reserve(n + TAG_BYTES as usize);
                    match (&source, file.as_mut()) {
                        (UploadSource::Memory { data, .. }, _) => buf.extend_from_slice(&data[pos as usize..pos as usize + n]),
                        (_, Some(f)) => {
                            let mut limited = (&mut buf).limit(n);
                            let mut got = 0;
                            while got < n {
                                match f.read_buf(&mut limited).await {
                                    Ok(0) => {
                                        let _ = tx.send(Err(std::io::Error::other("the file got shorter while uploading"))).await;
                                        return;
                                    }
                                    Ok(k) => got += k,
                                    Err(e) => {
                                        let _ = tx.send(Err(e)).await;
                                        return;
                                    }
                                }
                            }
                        }
                        (_, None) => unreachable!(),
                    }
                    if let Some(k) = &key {
                        k.seal_chunk_in_place(pos / CHUNK_BYTES, &mut buf);
                    }
                    let mut b = buf.split();
                    if first && skip > 0 {
                        b.advance(skip.min(b.len()));
                    }
                    b.freeze()
                }
            };
            first = false;
            if !pacer.lock().await.take(frame.len(), svc.limits(), Direction::Up, Some(&cancel)).await {
                return;
            }
            pos += n as u64;
            if tx.send(Ok(frame)).await.is_err() {
                return;
            }
            let _ = sent.send(pos);
        }
    });
    ReqBody::Stream { rx, left: wire_len }
}

/// `upload` is spawned by the app: it must be `Send` (compile-time check, never called).
#[allow(dead_code)]
fn assert_send(svc: Arc<Attachments>, file: PreparedUpload, ctl: UploadControl) {
    fn is_send<T: Send>(_: &T) {}
    is_send(&async move { svc.upload("s", "c", &file, true, &ctl, |_| {}).await });
    is_send(&prepare(UploadSource::Path(PathBuf::new()), None));
}

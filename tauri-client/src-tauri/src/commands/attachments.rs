use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::events;
use crate::net::attachments as net_attach;
use crate::net::attachments::UploadObserver;
use crate::state::SharedState;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRequest {
    pub pending_id: String,
    pub server_id: String,
    pub channel_id: String,
    pub file_path: String,
    pub filename: String,
    pub mime: String,
    // Intrinsic image dimensions, read client-side before upload. 0 when
    // the file isn't an image. Forwarded into the /init metadata so
    // downstream viewers can reserve the right placeholder.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    // Audio + video duration in ms, read client-side. Lets receivers
    // show a duration label (e.g. "3:45") before downloading the file.
    #[serde(default)]
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadResult {
    pub pending_id: String,
    pub attachment_id: i64,
    pub filename: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: i64,
}

fn kind_label(kind_code: i32) -> &'static str {
    match kind_code {
        0 => "image",
        1 => "video",
        3 => "audio",
        _ => "document",
    }
}

struct UploadObs {
    app: AppHandle,
    pending_id: String,
    server_id: String,
    channel_id: String,
    attachment_id: i64,
    filename: String,
    total_bytes: u64,
    cancel: Arc<AtomicBool>,
}

impl net_attach::UploadObserver for UploadObs {
    fn on_progress(&self, transferred: u64, _chunk_size: usize) {
        events::emit_attachment_upload_progress(
            &self.app,
            events::AttachmentUploadProgressPayload {
                pending_id: self.pending_id.clone(),
                server_id: self.server_id.clone(),
                channel_id: self.channel_id.clone(),
                attachment_id: self.attachment_id,
                filename: self.filename.clone(),
                transferred_bytes: transferred,
                total_bytes: self.total_bytes,
            },
        );
    }
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[tauri::command]
pub async fn upload_attachment(
    req: UploadRequest,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<UploadResult, String> {
    // Extract network coordinates up front so we don't hold the state lock
    // across the (potentially hours-long) upload.
    let (host, port, jwt, max_bytes) = {
        let s = state.lock().await;
        let client = s
            .communities
            .get(&req.server_id)
            .ok_or_else(|| format!("Not connected to community {}", req.server_id))?;
        if client.attachment_port == 0 {
            return Err("Server did not advertise an attachment port".to_string());
        }
        (
            client.host.clone(),
            client.attachment_port,
            client.jwt.clone(),
            client.max_attachment_bytes,
        )
    };

    // File metadata before init so we can pre-validate size and get the
    // exact byte count the server expects to match at complete time.
    let path = PathBuf::from(&req.file_path);
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| format!("stat {}: {}", req.file_path, e))?;
    let total_bytes = meta.len();
    if total_bytes == 0 {
        return Err("Cannot upload an empty file".to_string());
    }
    if max_bytes > 0 && (total_bytes as i64) > max_bytes {
        return Err(format!(
            "File is {} bytes, server cap is {}",
            total_bytes, max_bytes
        ));
    }

    // Register cancellation flag before init so a cancel that lands during
    // init is respected.
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut s = state.lock().await;
        s.active_uploads.insert(req.pending_id.clone(), cancel.clone());
    }

    // Throttle shared with any other transfer; rate is dynamic so mid-upload
    // settings changes take effect on the next sub-chunk.
    let upload_rate = {
        let s = state.lock().await;
        s.upload_limit_bps.clone()
    };
    let throttle = net_attach::RateLimiter::new(upload_rate);

    let init = match net_attach::post_init(
        &host, port, &jwt,
        &req.channel_id, &req.filename, &req.mime, total_bytes as i64,
        req.width, req.height, req.duration_ms,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            {
                let mut s = state.lock().await;
                s.active_uploads.remove(&req.pending_id);
            }
            events::emit_attachment_upload_failed(
                &app,
                events::AttachmentUploadFailedPayload {
                    pending_id: req.pending_id.clone(),
                    server_id: req.server_id.clone(),
                    channel_id: req.channel_id.clone(),
                    attachment_id: 0,
                    filename: req.filename.clone(),
                    message: e.clone(),
                    cancelled: false,
                },
            );
            return Err(e);
        }
    };
    let attachment_id = init.id;

    let observer = UploadObs {
        app: app.clone(),
        pending_id: req.pending_id.clone(),
        server_id: req.server_id.clone(),
        channel_id: req.channel_id.clone(),
        attachment_id,
        filename: req.filename.clone(),
        total_bytes,
        cancel: cancel.clone(),
    };

    // Emit an immediate 0-byte progress tick so the UI can render a spinner
    // even before the first chunk write completes.
    observer.on_progress(0, 0);

    let result = stream_file_in_chunks(
        &host, port, &jwt, attachment_id, &path, total_bytes,
        &throttle, &observer,
    )
    .await;

    // Transfer done (or aborted). Pull cancel flag off AppState; a late
    // cancel from the UI after this point is a no-op.
    let was_cancelled = cancel.load(Ordering::Relaxed);
    {
        let mut s = state.lock().await;
        s.active_uploads.remove(&req.pending_id);
    }

    if let Err(e) = result {
        // If cancellation was signalled, tell the server to drop the pending
        // row + .partial so storage doesn't slowly accumulate half-uploads
        // from a user repeatedly starting and cancelling.
        if was_cancelled {
            let _ = net_attach::delete_pending(&host, port, &jwt, attachment_id).await;
        }
        events::emit_attachment_upload_failed(
            &app,
            events::AttachmentUploadFailedPayload {
                pending_id: req.pending_id.clone(),
                server_id: req.server_id.clone(),
                channel_id: req.channel_id.clone(),
                attachment_id,
                filename: req.filename.clone(),
                message: e.clone(),
                cancelled: was_cancelled,
            },
        );
        return Err(e);
    }

    // All bytes on disk — finalize server-side.
    let complete = match net_attach::post_complete(&host, port, &jwt, attachment_id).await {
        Ok(r) => r,
        Err(e) => {
            events::emit_attachment_upload_failed(
                &app,
                events::AttachmentUploadFailedPayload {
                    pending_id: req.pending_id.clone(),
                    server_id: req.server_id.clone(),
                    channel_id: req.channel_id.clone(),
                    attachment_id,
                    filename: req.filename.clone(),
                    message: e.clone(),
                    cancelled: false,
                },
            );
            return Err(e);
        }
    };

    let kind = kind_label(complete.kind).to_string();
    events::emit_attachment_upload_complete(
        &app,
        events::AttachmentUploadCompletePayload {
            pending_id: req.pending_id.clone(),
            server_id: req.server_id.clone(),
            channel_id: req.channel_id.clone(),
            attachment_id: complete.id,
            filename: complete.filename.clone(),
            mime: complete.mime.clone(),
            kind: kind.clone(),
            size_bytes: complete.size_bytes,
        },
    );

    Ok(UploadResult {
        pending_id: req.pending_id,
        attachment_id: complete.id,
        filename: complete.filename,
        mime: complete.mime,
        kind,
        size_bytes: complete.size_bytes,
    })
}

/// Drives the PATCH loop over a local file. Re-seeks from the server's
/// reported offset on transient failures so a dropped connection doesn't
/// force restart from zero. Caller is responsible for init/complete.
async fn stream_file_in_chunks(
    host: &str,
    port: u16,
    jwt: &str,
    attachment_id: i64,
    path: &std::path::Path,
    total_bytes: u64,
    throttle: &net_attach::RateLimiter,
    observer: &dyn net_attach::UploadObserver,
) -> Result<(), String> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {}: {}", path.display(), e))?;

    let mut offset: u64 = 0;
    let mut chunk_buf = vec![0u8; net_attach::UPLOAD_CHUNK_BYTES];

    while offset < total_bytes {
        if observer.is_cancelled() {
            return Err("cancelled".to_string());
        }
        let remaining = total_bytes - offset;
        let chunk_size = remaining.min(net_attach::UPLOAD_CHUNK_BYTES as u64) as usize;
        let slice = &mut chunk_buf[..chunk_size];

        // Re-seek every iteration — simpler than maintaining a running cursor
        // across resume rewinds, and disk seek cost is negligible versus the
        // network cost of the chunk itself.
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| format!("seek: {}", e))?;
        file.read_exact(slice)
            .await
            .map_err(|e| format!("read: {}", e))?;

        let mut attempt: u32 = 0;
        let new_offset = loop {
            if observer.is_cancelled() {
                return Err("cancelled".to_string());
            }
            match net_attach::patch_chunk(
                host, port, jwt, attachment_id, offset, slice, throttle, observer, offset,
            )
            .await
            {
                Ok(off) => break off,
                Err(e) => {
                    if e == "cancelled" { return Err(e); }
                    attempt += 1;
                    if attempt > net_attach::max_retry() {
                        return Err(format!("upload failed after retries: {}", e));
                    }
                    log::warn!(
                        "PATCH attempt {}/{} failed for attachment {}: {}",
                        attempt, net_attach::max_retry(), attachment_id, e
                    );
                    tokio::time::sleep(net_attach::retry_backoff(attempt)).await;
                    // Realign: ask the server where it thinks we are.
                    match net_attach::head_offset(host, port, jwt, attachment_id).await {
                        Ok(server_off) => {
                            // Server has more than we think — advance without
                            // re-sending the chunk. The outer loop re-reads
                            // from disk at the new offset on its next pass.
                            if server_off > offset {
                                break server_off;
                            }
                            // Same offset — fall through to retry the chunk.
                        }
                        Err(he) => {
                            log::warn!("HEAD during resume failed: {}", he);
                            // Keep trying the same offset.
                        }
                    }
                }
            }
        };
        offset = new_offset;
    }
    Ok(())
}

/// Uploads a JPEG thumbnail for an already-`ready` attachment. Called by
/// the JS upload pipeline right after the main upload completes (and
/// only when a thumbnail was successfully extracted client-side). Errors
/// are non-fatal — the message can still send without a thumbnail; the
/// placeholder just falls back to its plain look.
#[tauri::command]
pub async fn upload_attachment_thumbnail(
    server_id: String,
    attachment_id: i64,
    // None → legacy single-size endpoint (`.thumb.jpg`). Some(N) →
    // pre-generated size variant (320/640/1280); server validates.
    size: Option<u32>,
    bytes: Vec<u8>,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    if bytes.is_empty() {
        return Err("Empty thumbnail bytes".to_string());
    }
    let (host, port, jwt) = {
        let s = state.lock().await;
        let client = s
            .communities
            .get(&server_id)
            .ok_or_else(|| format!("Not connected to community {}", server_id))?;
        if client.attachment_port == 0 {
            return Err("Server did not advertise an attachment port".to_string());
        }
        (client.host.clone(), client.attachment_port, client.jwt.clone())
    };
    net_attach::post_thumbnail(&host, port, &jwt, attachment_id, size, &bytes).await
}

/// Fetch the raw JPEG bytes of a server-side thumbnail. Like
/// `fetch_attachment_bytes` but with `?variant=thumb` so the server
/// returns the sibling `.thumb.jpg` file instead of the main attachment.
/// Used by `VideoPlayer` to lazy-load a poster preview when the
/// placeholder scrolls into view.
#[tauri::command]
pub async fn fetch_attachment_thumbnail(
    server_id: String,
    attachment_id: i64,
    // None → server picks the largest available size. Some(N) requests
    // 320, 640, or 1280; server falls back to the nearest if the
    // exact size isn't available, or to the legacy `.thumb.jpg`.
    size: Option<u32>,
    state: State<'_, SharedState>,
) -> Result<tauri::ipc::Response, String> {
    let (host, port, jwt, rate) = {
        let s = state.lock().await;
        let client = s
            .communities
            .get(&server_id)
            .ok_or_else(|| format!("Not connected to community {}", server_id))?;
        if client.attachment_port == 0 {
            return Err("Server did not advertise an attachment port".to_string());
        }
        (
            client.host.clone(),
            client.attachment_port,
            client.jwt.clone(),
            s.download_limit_bps.clone(),
        )
    };
    let throttle = net_attach::RateLimiter::new(rate);
    let null_obs = NullDownloadObs;
    let mut buf: Vec<u8> = Vec::new();
    net_attach::stream_get_variant(
        &host, port, &jwt, attachment_id, "thumb", size, &mut buf, &throttle, &null_obs,
    )
    .await?;
    Ok(tauri::ipc::Response::new(buf))
}

#[tauri::command]
pub async fn cancel_attachment_upload(
    pending_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let flag = {
        let s = state.lock().await;
        s.active_uploads.get(&pending_id).cloned()
    };
    if let Some(flag) = flag {
        flag.store(true, Ordering::Relaxed);
        Ok(())
    } else {
        Err(format!("No upload in progress for {}", pending_id))
    }
}

// ---- download / fetch ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadRequest {
    pub server_id: String,
    pub attachment_id: i64,
    pub destination_path: String,
}

struct DownloadObs {
    app: AppHandle,
    attachment_id: i64,
    cancel: Arc<AtomicBool>,
}

impl net_attach::DownloadObserver for DownloadObs {
    fn on_progress(&self, transferred: u64, total: u64) {
        events::emit_attachment_download_progress(
            &self.app,
            events::AttachmentDownloadProgressPayload {
                attachment_id: self.attachment_id,
                transferred_bytes: transferred,
                total_bytes: total,
            },
        );
    }
    fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TempAttachment {
    /// Absolute path of the saved temp file. Use for `cleanup_temp_attachment`.
    pub path: String,
    /// `http://127.0.0.1:PORT/<filename>` URL served by the local media
    /// server. WebKit's GStreamer pipeline can play from this URL with
    /// proper seek + decode behaviour.
    pub url: String,
}

/// Downloads an attachment to a generated temp file and returns both
/// the path (for cleanup) and a localhost HTTP URL (for `<video>`/`<audio>`
/// src). The local media server serves files matching `decibell-attach-*`
/// from the OS temp dir, so this URL is only routable inside the user's
/// machine.
#[tauri::command]
pub async fn save_attachment_to_temp(
    server_id: String,
    attachment_id: i64,
    filename: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<TempAttachment, String> {
    let extension = std::path::Path::new(&filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_name = format!("decibell-attach-{}-{}.{}", now, attachment_id, extension);
    let temp_path = crate::local_media_server::cache_dir().join(&temp_name);
    let path_str = temp_path
        .to_str()
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())?
        .to_string();

    let port = {
        let s = state.lock().await;
        s.local_media_port
    };
    if port == 0 {
        return Err("Local media server isn't running".to_string());
    }

    download_attachment(
        DownloadRequest {
            server_id,
            attachment_id,
            destination_path: path_str.clone(),
        },
        app,
        state,
    )
    .await?;

    let url = format!(
        "http://127.0.0.1:{}/{}",
        port,
        urlencoding::encode(&temp_name)
    );
    Ok(TempAttachment { path: path_str, url })
}

/// Makes a user-picked file accessible to the local media server (and
/// therefore to a hidden `<video>` element in the renderer) so the JS
/// upload flow can read its dimensions + capture a thumbnail frame.
/// Hard-links the source path into the cache dir under our
/// `decibell-attach-` namespace; falls back to a copy if the source
/// lives on a different filesystem. Returns the localhost URL plus the
/// staged path so the caller can clean up after extraction.
#[tauri::command]
pub async fn stage_file_for_media(
    path: String,
    state: State<'_, SharedState>,
) -> Result<TempAttachment, String> {
    let src = std::path::PathBuf::from(&path);
    if !src.exists() {
        return Err(format!("Source file not found: {}", path));
    }
    let extension = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("bin");
    // nanos + an atomic counter keep the suffix unique across rapid calls
    // without pulling in a randomness dep. These files are short-lived
    // (cleanup runs as soon as JS-side extraction finishes), so collision
    // resistance only needs to cover concurrent uploads from the same UI.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let staged_name = format!("decibell-attach-stage-{}-{}.{}", nanos, seq, extension);
    let staged_path = crate::local_media_server::cache_dir().join(&staged_name);

    // Try hard link first — zero copy, instant. Fails across filesystems
    // or on platforms where the source isn't supported, in which case we
    // fall back to a regular copy.
    if std::fs::hard_link(&src, &staged_path).is_err() {
        tokio::fs::copy(&src, &staged_path)
            .await
            .map_err(|e| format!("Stage copy {} → {}: {}", src.display(), staged_path.display(), e))?;
    }

    let port = {
        let s = state.lock().await;
        s.local_media_port
    };
    if port == 0 {
        return Err("Local media server isn't running".to_string());
    }

    let path_str = staged_path
        .to_str()
        .ok_or_else(|| "Staged path contains non-UTF-8 characters".to_string())?
        .to_string();
    let url = format!(
        "http://127.0.0.1:{}/{}",
        port,
        urlencoding::encode(&staged_name)
    );
    Ok(TempAttachment { path: path_str, url })
}

/// Deletes a temp file previously created by `save_attachment_to_temp`.
/// Validates the path is in the OS temp dir and uses our prefix so a
/// caller can't trick us into deleting arbitrary files.
#[tauri::command]
pub async fn cleanup_temp_attachment(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    let cache_dir = crate::local_media_server::cache_dir();
    if p.parent() != Some(cache_dir.as_path()) {
        return Ok(());
    }
    let in_namespace = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("decibell-attach-"));
    if !in_namespace {
        return Ok(());
    }
    let _ = tokio::fs::remove_file(&p).await;
    Ok(())
}

#[tauri::command]
pub async fn download_attachment(
    req: DownloadRequest,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (host, port, jwt, rate) = {
        let s = state.lock().await;
        let client = s
            .communities
            .get(&req.server_id)
            .ok_or_else(|| format!("Not connected to community {}", req.server_id))?;
        if client.attachment_port == 0 {
            return Err("Server did not advertise an attachment port".to_string());
        }
        (
            client.host.clone(),
            client.attachment_port,
            client.jwt.clone(),
            s.download_limit_bps.clone(),
        )
    };

    let throttle = net_attach::RateLimiter::new(rate);
    let observer = DownloadObs {
        app,
        attachment_id: req.attachment_id,
        cancel: Arc::new(AtomicBool::new(false)),
    };

    let file = tokio::fs::File::create(&req.destination_path)
        .await
        .map_err(|e| format!("create {}: {}", req.destination_path, e))?;

    net_attach::stream_get(&host, port, &jwt, req.attachment_id, 0, file, &throttle, &observer)
        .await
        .map(|_| ())
}

/// Fetch the raw bytes of an attachment for inline preview. Returned as a
/// tauri::ipc::Response so the IPC layer transports them as binary rather
/// than a JSON-encoded base64 string — with many images on screen the
/// base64-in-DOM path became the dominant scroll cost.
///
/// On the JS side: `const bytes = await invoke<ArrayBuffer>(...)` → wrap in
/// a Blob → URL.createObjectURL. The DOM then holds a tiny `blob:` URL
/// instead of a multi-megabyte data URL that's diffed on every render.
///
/// No progress events — used for inline render only, not user-visible
/// transfers.
#[tauri::command]
pub async fn fetch_attachment_bytes(
    server_id: String,
    attachment_id: i64,
    state: State<'_, SharedState>,
) -> Result<tauri::ipc::Response, String> {
    let (host, port, jwt, rate) = {
        let s = state.lock().await;
        let client = s
            .communities
            .get(&server_id)
            .ok_or_else(|| format!("Not connected to community {}", server_id))?;
        if client.attachment_port == 0 {
            return Err("Server did not advertise an attachment port".to_string());
        }
        (
            client.host.clone(),
            client.attachment_port,
            client.jwt.clone(),
            s.download_limit_bps.clone(),
        )
    };

    let throttle = net_attach::RateLimiter::new(rate);
    let null_obs = NullDownloadObs;

    let mut buf: Vec<u8> = Vec::new();
    net_attach::stream_get(&host, port, &jwt, attachment_id, 0, &mut buf, &throttle, &null_obs)
        .await?;

    Ok(tauri::ipc::Response::new(buf))
}

struct NullDownloadObs;
impl net_attach::DownloadObserver for NullDownloadObs {
    fn on_progress(&self, _t: u64, _total: u64) {}
    fn is_cancelled(&self) -> bool { false }
}

// ---- settings: throttle knobs ----

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileMeta {
    pub filename: String,
    pub size_bytes: u64,
    pub mime: String,
    // 0 when dimensions couldn't be read (non-image kind or unsupported format).
    // We pass these through to /attachments/init so downstream viewers
    // reserve the right placeholder size on first render — no layout shift
    // when the data URL loads in.
    pub width: u32,
    pub height: u32,
}

/// Cheap metadata lookup for a file the user picked. UI uses this to
/// populate the pending-attachment card (filename + size + kind) before the
/// upload begins. MIME is inferred from the extension — good enough to route
/// to the right retention bucket; the server doesn't use it for anything
/// security-sensitive.
#[tauri::command]
pub async fn stat_attachment_file(path: String) -> Result<FileMeta, String> {
    let pb = PathBuf::from(&path);
    let meta = tokio::fs::metadata(&pb)
        .await
        .map_err(|e| format!("stat {}: {}", path, e))?;
    let filename = pb
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let mime = mime_from_extension(&filename);

    // For images, read intrinsic dimensions. image::image_dimensions reads
    // only the header bytes (cheap — a few KB even for huge images) so
    // this scales fine to 15 GB files as long as the format has dimensions
    // encoded near the start. Non-images and unreadable files report 0/0.
    let (width, height) = if mime.starts_with("image/") {
        match image::image_dimensions(&pb) {
            Ok((w, h)) => (w, h),
            Err(_) => (0, 0),
        }
    } else {
        (0, 0)
    };

    Ok(FileMeta {
        filename,
        size_bytes: meta.len(),
        mime,
        width,
        height,
    })
}

fn mime_from_extension(filename: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "7z" => "application/x-7z-compressed",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "txt" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[tauri::command]
pub async fn set_transfer_limits(
    upload_bps: u64,
    download_bps: u64,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (up, down) = {
        let s = state.lock().await;
        (s.upload_limit_bps.clone(), s.download_limit_bps.clone())
    };
    up.store(upload_bps, std::sync::atomic::Ordering::Relaxed);
    down.store(download_bps, std::sync::atomic::Ordering::Relaxed);

    // Persist so reboots pick the same rates back up.
    let settings = {
        // We reuse the existing save path. Pull the current config,
        // overwrite the two fields, save. Config is small so this is cheap.
        let loaded = crate::config::load(&app).ok();
        let mut settings = loaded.as_ref().map(|c| c.settings.clone()).unwrap_or_default();
        settings.upload_limit_bps = upload_bps;
        settings.download_limit_bps = download_bps;
        settings
    };
    crate::config::save(&app, None, &settings)?;
    Ok(())
}

/// Persist clipboard-pasted bytes to a temp file and return its absolute
/// path. The frontend then runs that path through the same upload pipeline
/// the file picker uses, avoiding any duplication of the upload logic.
///
/// Filename is sanitized — only alphanumerics, `.`, `-`, `_`, and spaces
/// survive — and a millisecond timestamp prefix prevents collisions
/// across rapid pastes. Cleanup is delegated to the OS temp-dir lifecycle;
/// trying to delete after upload would race with retries.
#[tauri::command]
pub async fn save_paste_to_temp(
    bytes: Vec<u8>,
    filename: String,
) -> Result<String, String> {
    let mut safe_name: String = filename
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    safe_name = safe_name.trim().to_string();
    if safe_name.is_empty() || safe_name.starts_with('.') {
        safe_name = "paste".to_string();
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let temp_name = format!("decibell-paste-{}-{}", now, safe_name);
    let temp_path = std::env::temp_dir().join(&temp_name);

    // Async I/O via tokio::fs::write. The await yields the executor
    // thread back to the runtime during the actual disk write, so
    // other commands aren't queued behind a large paste's flush.
    tokio::fs::write(&temp_path, &bytes)
        .await
        .map_err(|e| format!("Write temp file: {}", e))?;

    temp_path
        .to_str()
        .map(String::from)
        .ok_or_else(|| "Temp path contains non-UTF-8 characters".to_string())
}

/// PNG bytes for an image currently sitting on the OS clipboard, or
/// `None` if the clipboard doesn't carry an image.
///
/// Why this exists: WebKitGTK on Linux does not expose clipboard images
/// via the JS `paste` event (the event fires with empty `clipboardData`
/// for screenshot-style copies). We work around it by reading the OS
/// clipboard from Rust through `arboard`, which speaks the underlying
/// X11/Wayland protocols directly. Same fallback also catches "Copy
/// Image" from browsers — those put HTML markup, not bytes, into the
/// JS clipboard, but the OS clipboard often has the actual image too.
#[derive(Debug, Clone, Serialize)]
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub mime: String,
}

fn try_arboard() -> Option<ClipboardImage> {
    use std::io::Cursor;
    let mut clipboard = arboard::Clipboard::new().ok()?;
    let img = clipboard.get_image().ok()?;
    let width = img.width as u32;
    let height = img.height as u32;
    let buf = image::RgbaImage::from_raw(width, height, img.bytes.into_owned())?;
    let mut png_bytes = Vec::new();
    buf.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .ok()?;
    Some(ClipboardImage {
        bytes: png_bytes,
        mime: "image/png".into(),
    })
}

/// Wayland-compositor-agnostic fallback. arboard's Wayland backend
/// uses `wlr-data-control` which not every compositor implements (and
/// even those that do can flake out depending on protocol version).
/// `wl-paste` (from the `wl-clipboard` package) speaks whichever
/// protocol the compositor exposes, so it's the most universal way to
/// read an image off the system clipboard on Wayland.
#[cfg(target_os = "linux")]
fn try_wl_paste() -> Option<ClipboardImage> {
    use std::process::Command;

    let types_out = Command::new("wl-paste").arg("--list-types").output().ok()?;
    if !types_out.status.success() {
        return None;
    }
    let types_str = String::from_utf8_lossy(&types_out.stdout);

    // Pick the first image MIME the clipboard advertises that we can
    // hand straight to the upload pipeline. Order doesn't really matter
    // for correctness, but PNG first matches what most screenshot tools
    // produce.
    let mime = [
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/bmp",
        "image/gif",
        "image/tiff",
    ]
    .into_iter()
    .find(|m| types_str.lines().any(|line| line.trim() == *m))?;

    let read_out = Command::new("wl-paste")
        .arg("--no-newline")
        .arg("--type")
        .arg(mime)
        .output()
        .ok()?;
    if !read_out.status.success() || read_out.stdout.is_empty() {
        return None;
    }

    Some(ClipboardImage {
        bytes: read_out.stdout,
        mime: mime.to_string(),
    })
}

#[tauri::command]
pub fn read_clipboard_image() -> Result<Option<ClipboardImage>, String> {
    if let Some(img) = try_arboard() {
        return Ok(Some(img));
    }
    #[cfg(target_os = "linux")]
    if let Some(img) = try_wl_paste() {
        return Ok(Some(img));
    }
    Ok(None)
}

/// Returns absolute filesystem paths for any `file://` URIs sitting on
/// the clipboard. WebKitGTK lists `text/uri-list` in `clipboardData.types`
/// but `getData()` for non-text MIME types is gated behind a security
/// policy that returns an empty string, so on Linux we read the URI list
/// directly via `wl-paste`. Returns an empty list when the clipboard
/// doesn't carry file URIs (or on platforms where this isn't needed —
/// Windows surfaces file copies through `clipboardData.files` directly).
#[tauri::command]
pub fn read_clipboard_file_paths() -> Result<Vec<String>, String> {
    #[cfg(target_os = "linux")]
    {
        use std::process::Command;
        let out = match Command::new("wl-paste")
            .arg("--no-newline")
            .arg("--type")
            .arg("text/uri-list")
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => return Ok(Vec::new()),
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let paths: Vec<String> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| l.starts_with("file://"))
            .filter_map(|uri| {
                // file:// + absolute path. Strip the scheme and percent-decode.
                let stripped = uri.strip_prefix("file://")?;
                Some(percent_decode(stripped))
            })
            .collect();
        return Ok(paths);
    }
    #[cfg(not(target_os = "linux"))]
    Ok(Vec::new())
}

#[cfg(target_os = "linux")]
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Open the XDG Desktop Portal's file chooser. Returns absolute paths
/// for selected files (file:// URIs converted to local paths). Empty
/// vec when the user cancels. Linux-only — other platforms still use
/// `tauri-plugin-dialog` from the frontend.
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn pick_attachments_xdg(
    title: String,
    multiple: bool,
) -> Result<Vec<String>, String> {
    use ashpd::desktop::file_chooser::OpenFileRequest;

    let response = OpenFileRequest::default()
        .title(title.as_str())
        .multiple(multiple)
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {}", e))?;

    let response = match response.response() {
        Ok(r) => r,
        // ResponseError::Cancelled is the normal user-dismissed-the-dialog
        // case; surface as an empty selection rather than a Tauri error.
        Err(_) => return Ok(Vec::new()),
    };

    let mut paths = Vec::new();
    for uri in response.uris() {
        let s = uri.to_string();
        if let Some(stripped) = s.strip_prefix("file://") {
            paths.push(percent_decode(stripped));
        }
    }
    Ok(paths)
}

/// Copy a decoded image to the system clipboard. Accepts the raw bytes
/// of an image in any common encoding (PNG/JPEG/WEBP/etc.) — we decode
/// once, then push to the clipboard via `arboard` (which speaks
/// X11 / Win32 / NSPasteboard / Wayland data-control). On Linux, if
/// arboard fails (e.g., compositor doesn't expose data-control), we
/// re-encode to PNG and shell out to `wl-copy`.
#[tauri::command]
pub async fn copy_image_to_clipboard(bytes: Vec<u8>) -> Result<(), String> {
    let decoded = image::load_from_memory(&bytes)
        .map_err(|e| format!("Decode image: {}", e))?;
    let rgba = decoded.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());

    // On Wayland, prefer `wl-copy` — arboard's data-control path will
    // happily report success on compositors (e.g. Niri) where the data
    // never actually reaches the clipboard, leaving a misleading "Image
    // copied" toast with nothing to paste.
    #[cfg(target_os = "linux")]
    {
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            if try_wl_copy_image(width, height, rgba.as_raw()).is_ok() {
                return Ok(());
            }
            // Wayland-but-wl-copy-failed: fall through to arboard as a
            // last resort.
        }
    }

    if try_arboard_set_image(width as usize, height as usize, rgba.as_raw()).is_ok() {
        return Ok(());
    }

    Err("Failed to copy image to clipboard".into())
}

fn try_arboard_set_image(width: usize, height: usize, rgba: &[u8]) -> Result<(), String> {
    use std::borrow::Cow;
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    let img = arboard::ImageData {
        width,
        height,
        bytes: Cow::Borrowed(rgba),
    };
    clipboard.set_image(img).map_err(|e| e.to_string())
}

#[cfg(target_os = "linux")]
fn try_wl_copy_image(width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    use std::io::{Cursor, Write};
    use std::process::{Command, Stdio};

    // Re-encode RGBA → PNG so any pasting app accepts it.
    let buf = image::RgbaImage::from_raw(width, height, rgba.to_vec())
        .ok_or_else(|| "RGBA buffer doesn't match dimensions".to_string())?;
    let mut png = Vec::new();
    buf.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|e| format!("PNG encode: {}", e))?;

    let mut child = Command::new("wl-copy")
        .arg("--type")
        .arg("image/png")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Spawn wl-copy: {}", e))?;

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(&png).map_err(|e| e.to_string())?;
    }
    drop(child.stdin.take());
    let status = child.wait().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("wl-copy exited with {}", status));
    }
    Ok(())
}

/// XDG Desktop Portal save-file dialog. Returns the chosen absolute
/// path, or `None` when the user cancels. Linux-only.
#[cfg(target_os = "linux")]
#[tauri::command]
pub async fn pick_save_path_xdg(
    title: String,
    default_name: String,
) -> Result<Option<String>, String> {
    use ashpd::desktop::file_chooser::SaveFileRequest;

    let response = SaveFileRequest::default()
        .title(title.as_str())
        .current_name(default_name.as_str())
        .send()
        .await
        .map_err(|e| format!("Portal request failed: {}", e))?;

    let response = match response.response() {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };

    for uri in response.uris() {
        let s = uri.to_string();
        if let Some(stripped) = s.strip_prefix("file://") {
            return Ok(Some(percent_decode(stripped)));
        }
    }
    Ok(None)
}

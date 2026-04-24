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
        req.width, req.height,
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

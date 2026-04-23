// Attachment transport: tiny async HTTP/1.1 client over tokio-rustls that
// speaks the narrow subset of methods the community server implements
// (POST /init, PATCH /<id>, HEAD /<id>, POST /<id>/complete, GET /<id>, DELETE /<id>).
//
// We build directly on the existing TLS connector from net::tls — no reqwest
// or hyper dep — because the feature surface is tiny, the existing chat path
// is already using tokio-rustls, and doing it this way keeps the TLS verifier
// behavior consistent with what the chat TCP connection uses.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use super::tls::create_tls_connector;

/// 8 MB chunks — big enough to keep throughput near line-rate over TLS,
/// small enough that a transient failure only costs a few MB of rework.
/// The body inside each chunk is streamed in 64 KB sub-writes so the rate
/// limiter can throttle smoothly instead of in 8-second bursts.
pub const UPLOAD_CHUNK_BYTES: usize = 8 * 1024 * 1024;
/// Sub-chunk write granularity inside a PATCH body. Also the stride of the
/// token-bucket take() calls, so a 1 MB/s limit produces ~15 waits/sec
/// rather than one 8-second pause per chunk.
pub const SUB_CHUNK_BYTES: usize = 64 * 1024;
/// Max backoff between resume retries for a single chunk.
const MAX_RETRY: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitResponse {
    pub id: i64,
    pub upload_offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteResponse {
    pub id: i64,
    pub kind: i32,
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    #[serde(default)]
    pub upload_status: String,
}

#[derive(Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers.iter()
            .find(|(k, _)| k.to_ascii_lowercase() == lower)
            .map(|(_, v)| v.as_str())
    }
}

/// Token-bucket rate limiter. Rate in bytes per second; 0 means unlimited.
/// The capacity is one second of rate (a burst equal to one second of
/// throughput) to keep short bursts responsive without blowing past the cap.
///
/// Dynamic: the rate can be updated from elsewhere and the next take() call
/// picks up the new value. Lets settings-slider changes take effect mid-transfer.
pub struct RateLimiter {
    rate_bps: Arc<AtomicU64>,
    state: std::sync::Mutex<BucketState>,
}

struct BucketState {
    balance: f64,
    last_refill: Instant,
}

impl RateLimiter {
    pub fn new(rate_bps: Arc<AtomicU64>) -> Self {
        Self {
            rate_bps,
            state: std::sync::Mutex::new(BucketState {
                balance: 0.0,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Await until `bytes` worth of tokens are available. Returns immediately
    /// when rate is 0 (unlimited) or when the bucket has enough slack.
    pub async fn take(&self, bytes: u64) {
        loop {
            let rate = self.rate_bps.load(Ordering::Relaxed);
            if rate == 0 {
                return;
            }
            let wait_ms = {
                let mut s = self.state.lock().expect("RateLimiter mutex poisoned");
                let now = Instant::now();
                let elapsed = now.duration_since(s.last_refill).as_secs_f64();
                s.balance = (s.balance + elapsed * rate as f64).min(rate as f64);
                s.last_refill = now;
                if s.balance >= bytes as f64 {
                    s.balance -= bytes as f64;
                    0
                } else {
                    let deficit = bytes as f64 - s.balance;
                    ((deficit / rate as f64) * 1000.0).ceil() as u64
                }
            };
            if wait_ms == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(wait_ms.max(1))).await;
        }
    }
}

async fn connect_tls(host: &str, port: u16) -> Result<TlsStream<TcpStream>, String> {
    let addr = format!("{}:{}", host, port);
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| format!("TCP connect to {} failed: {}", addr, e))?;
    let connector = create_tls_connector();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|e| format!("Invalid server name '{}': {}", host, e))?;
    connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("TLS handshake with {} failed: {}", addr, e))
}

// Read status line + headers; body handling is per-caller (small JSON vs
// streaming). Returns the BufReader so the caller can keep reading body bytes.
async fn read_response_head(
    reader: &mut BufReader<TlsStream<TcpStream>>,
) -> Result<(u16, String, Vec<(String, String)>, usize), String> {
    // Status line
    let mut line = String::new();
    reader.read_line(&mut line).await.map_err(|e| format!("Read status: {}", e))?;
    let line = line.trim_end();
    // "HTTP/1.1 204 No Content"
    let mut parts = line.splitn(3, ' ');
    let _version = parts.next().ok_or("malformed status line")?;
    let status_str = parts.next().ok_or("malformed status line")?;
    let reason = parts.next().unwrap_or("").to_string();
    let status: u16 = status_str.parse().map_err(|_| "bad status code".to_string())?;

    let mut headers = Vec::new();
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.map_err(|e| format!("Read header: {}", e))?;
        let trimmed = line.trim_end();
        if trimmed.is_empty() { break; }
        if let Some(colon) = trimmed.find(':') {
            let name = trimmed[..colon].trim().to_string();
            let value = trimmed[colon + 1..].trim().to_string();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.parse().unwrap_or(0);
            }
            headers.push((name, value));
        }
    }
    Ok((status, reason, headers, content_length))
}

async fn read_full_response(
    mut stream: TlsStream<TcpStream>,
) -> Result<HttpResponse, String> {
    let mut reader = BufReader::new(stream);
    let (status, reason, headers, content_length) = read_response_head(&mut reader).await?;
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await.map_err(|e| format!("Read body: {}", e))?;
    }
    stream = reader.into_inner();
    // Gracefully close without erroring on remote-already-closed.
    let _ = stream.shutdown().await;
    Ok(HttpResponse { status, reason, headers, body })
}

/// Callback invoked during upload with (bytes_transferred_total, chunk_size).
/// Used for progress events + cancellation polling.
pub trait UploadObserver: Send + Sync {
    fn on_progress(&self, transferred: u64, _chunk_size: usize);
    fn is_cancelled(&self) -> bool;
}

/// POST /attachments/init
pub async fn post_init(
    host: &str,
    port: u16,
    jwt: &str,
    channel_id: &str,
    filename: &str,
    mime: &str,
    size: i64,
) -> Result<InitResponse, String> {
    let body = serde_json::json!({
        "channelId": channel_id,
        "filename": filename,
        "mime": mime,
        "size": size,
    })
    .to_string();
    let mut stream = connect_tls(host, port).await?;
    let head = format!(
        "POST /attachments/init HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Bearer {jwt}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n",
        host = host, port = port, jwt = jwt, len = body.len()
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| format!("Write init head: {}", e))?;
    stream.write_all(body.as_bytes()).await.map_err(|e| format!("Write init body: {}", e))?;
    stream.flush().await.map_err(|e| format!("Flush init: {}", e))?;

    let resp = read_full_response(stream).await?;
    if resp.status != 201 {
        return Err(format!("init failed: HTTP {} {}", resp.status, resp.reason));
    }
    serde_json::from_slice::<InitResponse>(&resp.body)
        .map_err(|e| format!("init response parse: {}", e))
}

/// HEAD /attachments/:id — returns current Upload-Offset for resume.
pub async fn head_offset(host: &str, port: u16, jwt: &str, id: i64) -> Result<u64, String> {
    let mut stream = connect_tls(host, port).await?;
    let head = format!(
        "HEAD /attachments/{id} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Bearer {jwt}\r\n\
         Connection: close\r\n\r\n",
        id = id, host = host, port = port, jwt = jwt
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| format!("Write head: {}", e))?;
    stream.flush().await.map_err(|e| format!("Flush head: {}", e))?;
    let resp = read_full_response(stream).await?;
    if resp.status != 200 {
        return Err(format!("HEAD failed: HTTP {} {}", resp.status, resp.reason));
    }
    let off = resp.header("Upload-Offset")
        .ok_or_else(|| "HEAD missing Upload-Offset".to_string())?;
    off.parse::<u64>().map_err(|_| "bad Upload-Offset".to_string())
}

/// PATCH /attachments/:id — streams `chunk` starting at `offset`, throttled.
/// Returns the new offset the server reports. Progress is reported on the
/// observer at each sub-chunk write.
pub async fn patch_chunk(
    host: &str,
    port: u16,
    jwt: &str,
    id: i64,
    offset: u64,
    chunk: &[u8],
    throttle: &RateLimiter,
    observer: &dyn UploadObserver,
    base_transferred: u64,
) -> Result<u64, String> {
    let mut stream = connect_tls(host, port).await?;
    let head = format!(
        "PATCH /attachments/{id} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Bearer {jwt}\r\n\
         Upload-Offset: {off}\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n",
        id = id, host = host, port = port, jwt = jwt,
        off = offset, len = chunk.len()
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| format!("Write patch head: {}", e))?;

    // Stream the body in sub-chunks. The rate limiter waits *before* each
    // write so the socket never gets ahead of the cap.
    let mut sent = 0usize;
    while sent < chunk.len() {
        if observer.is_cancelled() {
            return Err("cancelled".to_string());
        }
        let n = (chunk.len() - sent).min(SUB_CHUNK_BYTES);
        throttle.take(n as u64).await;
        stream.write_all(&chunk[sent..sent + n])
            .await.map_err(|e| format!("Write patch body: {}", e))?;
        sent += n;
        observer.on_progress(base_transferred + sent as u64, n);
    }
    stream.flush().await.map_err(|e| format!("Flush patch: {}", e))?;

    let resp = read_full_response(stream).await?;
    if resp.status != 204 {
        return Err(format!("PATCH failed: HTTP {} {}", resp.status, resp.reason));
    }
    let new_off = resp.header("Upload-Offset")
        .ok_or_else(|| "PATCH missing Upload-Offset".to_string())?;
    new_off.parse::<u64>().map_err(|_| "bad Upload-Offset".to_string())
}

/// POST /attachments/:id/complete — finalizes a fully-uploaded attachment.
pub async fn post_complete(
    host: &str,
    port: u16,
    jwt: &str,
    id: i64,
) -> Result<CompleteResponse, String> {
    let body = "{}"; // no optional sha256 for now
    let mut stream = connect_tls(host, port).await?;
    let head = format!(
        "POST /attachments/{id}/complete HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Bearer {jwt}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\r\n",
        id = id, host = host, port = port, jwt = jwt, len = body.len()
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| format!("Write complete head: {}", e))?;
    stream.write_all(body.as_bytes()).await.map_err(|e| format!("Write complete body: {}", e))?;
    stream.flush().await.map_err(|e| format!("Flush complete: {}", e))?;

    let resp = read_full_response(stream).await?;
    if resp.status != 200 {
        return Err(format!("complete failed: HTTP {} {}", resp.status, resp.reason));
    }
    serde_json::from_slice::<CompleteResponse>(&resp.body)
        .map_err(|e| format!("complete response parse: {}", e))
}

/// DELETE /attachments/:id — abort a pending upload on the server side.
pub async fn delete_pending(
    host: &str,
    port: u16,
    jwt: &str,
    id: i64,
) -> Result<(), String> {
    let mut stream = connect_tls(host, port).await?;
    let head = format!(
        "DELETE /attachments/{id} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Bearer {jwt}\r\n\
         Connection: close\r\n\r\n",
        id = id, host = host, port = port, jwt = jwt
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| format!("Write delete: {}", e))?;
    stream.flush().await.map_err(|e| format!("Flush delete: {}", e))?;
    let resp = read_full_response(stream).await?;
    if resp.status != 204 && resp.status != 404 {
        return Err(format!("DELETE failed: HTTP {} {}", resp.status, resp.reason));
    }
    Ok(())
}

/// Shared cancellation signal for an in-flight upload.
pub struct UploadCancel {
    flag: Arc<AtomicBool>,
}

impl UploadCancel {
    pub fn new() -> Self { Self { flag: Arc::new(AtomicBool::new(false)) } }
    pub fn handle(&self) -> Arc<AtomicBool> { self.flag.clone() }
    pub fn cancel(&self) { self.flag.store(true, Ordering::Relaxed); }
}

impl Default for UploadCancel { fn default() -> Self { Self::new() } }

pub struct CancelFlag(pub Arc<AtomicBool>);
impl CancelFlag {
    pub fn is_set(&self) -> bool { self.0.load(Ordering::Relaxed) }
}

// ---- download ----

/// Callback during download: (bytes_so_far, total_bytes). Total is known from
/// Content-Length when present.
pub trait DownloadObserver: Send + Sync {
    fn on_progress(&self, transferred: u64, total: u64);
    fn is_cancelled(&self) -> bool;
}

/// GET /attachments/:id, writing the response body to `writer`. Honors the
/// caller's throttle. Uses a full-body fetch (no Range) for simplicity —
/// callers that need resume-able downloads can issue repeated requests with
/// a Range header built into `start`.
pub async fn stream_get<W: tokio::io::AsyncWrite + Unpin>(
    host: &str,
    port: u16,
    jwt: &str,
    id: i64,
    start: u64,
    mut writer: W,
    throttle: &RateLimiter,
    observer: &dyn DownloadObserver,
) -> Result<u64, String> {
    let mut stream = connect_tls(host, port).await?;
    let range_hdr = if start > 0 {
        format!("Range: bytes={}-\r\n", start)
    } else {
        String::new()
    };
    let head = format!(
        "GET /attachments/{id} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Authorization: Bearer {jwt}\r\n\
         {range}\
         Connection: close\r\n\r\n",
        id = id, host = host, port = port, jwt = jwt, range = range_hdr
    );
    stream.write_all(head.as_bytes()).await.map_err(|e| format!("Write get: {}", e))?;
    stream.flush().await.map_err(|e| format!("Flush get: {}", e))?;

    let mut reader = BufReader::new(stream);
    let (status, reason, _headers, content_length) = read_response_head(&mut reader).await?;
    if status != 200 && status != 206 {
        return Err(format!("GET failed: HTTP {} {}", status, reason));
    }
    let total = start + content_length as u64;
    let mut transferred = start;
    let mut buf = vec![0u8; SUB_CHUNK_BYTES];
    let mut remaining = content_length;
    while remaining > 0 {
        if observer.is_cancelled() {
            return Err("cancelled".to_string());
        }
        let want = remaining.min(buf.len());
        let got = reader.read(&mut buf[..want]).await
            .map_err(|e| format!("Read body: {}", e))?;
        if got == 0 { break; }
        throttle.take(got as u64).await;
        writer.write_all(&buf[..got]).await.map_err(|e| format!("Write disk: {}", e))?;
        remaining -= got;
        transferred += got as u64;
        observer.on_progress(transferred, total);
    }
    writer.flush().await.map_err(|e| format!("Flush disk: {}", e))?;
    Ok(transferred)
}

/// Exponential backoff between retry attempts. Caller supplies the attempt
/// count (starting at 1).
pub fn retry_backoff(attempt: u32) -> Duration {
    let secs = match attempt {
        1 => 1,
        2 => 2,
        3 => 4,
        _ => 8,
    };
    Duration::from_secs(secs)
}

pub fn max_retry() -> u32 { MAX_RETRY }

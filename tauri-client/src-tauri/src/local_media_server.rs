//! Tiny localhost HTTP server for `<video>`/`<audio>` playback.
//!
//! WebKitGTK delegates `<video>` loading to GStreamer, which can only
//! consume schemes it has source elements for: `file://`, `http://`,
//! `https://`, etc. Custom URI schemes registered with WebKit (asset://
//! and friends) don't propagate — they work for `<img>` and fetch but
//! not for media. `file://` from a `tauri://` origin gets blocked by
//! WebKit's same-origin policy.
//!
//! The pragmatic fix is the same one Electron apps use: bind a tiny
//! HTTP server on `127.0.0.1` and point media URLs at it. GStreamer's
//! `souphttpsrc` handles HTTP fine, including Range requests so seeking
//! works without a full re-download.
//!
//! Security model:
//!   - bound to `127.0.0.1` only (loopback; not reachable from network)
//!   - serves files from the OS temp dir whose filename starts with
//!     `decibell-attach-` (the same namespace `save_attachment_to_temp`
//!     creates)
//!   - GET only; no path traversal (filename is taken verbatim and
//!     joined to temp dir, so any `..` is preserved as-is and would
//!     fail the prefix check)

use std::path::PathBuf;
use std::sync::OnceLock;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const ALLOWED_PREFIX: &str = "decibell-attach-";
const READ_CHUNK: usize = 64 * 1024;

static CACHE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Sets the directory used for downloaded media temp files. Called once
/// at app startup with `app.path().app_cache_dir()`. Creates the
/// directory if missing. Safe to call multiple times — subsequent calls
/// are no-ops once the cell is filled.
pub fn init_cache_dir(dir: PathBuf) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(&dir)?;
    let _ = CACHE_DIR.set(dir);
    Ok(())
}

/// Where downloaded media is staged. On Linux this resolves to
/// `~/.cache/com.decibell.app/`, on macOS `~/Library/Caches/...`,
/// on Windows `%LOCALAPPDATA%\com.decibell.app\Cache`. Falls back to
/// the OS temp dir if `init_cache_dir` was never called (defensive —
/// shouldn't happen in practice).
pub fn cache_dir() -> PathBuf {
    CACHE_DIR.get().cloned().unwrap_or_else(std::env::temp_dir)
}

/// Binds a TCP listener on `127.0.0.1:0`, spawns the accept loop, and
/// returns the chosen port. Errors here mean media playback won't work,
/// but the rest of the app still runs.
pub async fn start() -> Result<u16, std::io::Error> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    tokio::spawn(async move {
        loop {
            let (socket, _) = match listener.accept().await {
                Ok(c) => c,
                Err(_) => continue,
            };
            tokio::spawn(handle_request(socket));
        }
    });

    Ok(port)
}

async fn handle_request(mut socket: TcpStream) {
    let (read_half, mut write_half) = socket.split();
    let mut reader = BufReader::new(read_half);

    // Request line: "GET /<filename> HTTP/1.1"
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.is_err() {
        return;
    }
    let mut parts = request_line.trim().split(' ');
    let method = parts.next().unwrap_or("");
    let raw_path = parts.next().unwrap_or("");

    if method != "GET" {
        let _ = write_simple(&mut write_half, 405, "Method Not Allowed").await;
        return;
    }

    // Headers — we only care about Range.
    let mut range_start: Option<u64> = None;
    let mut range_end: Option<u64> = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).await.is_err() {
            return;
        }
        let header = header.trim();
        if header.is_empty() {
            break;
        }
        let lower = header.to_ascii_lowercase();
        if let Some(rest) = lower.strip_prefix("range:") {
            let rest = rest.trim();
            if let Some(spec) = rest.strip_prefix("bytes=") {
                let mut sides = spec.split('-');
                range_start = sides.next().and_then(|s| s.trim().parse().ok());
                let end_str = sides.next().unwrap_or("").trim();
                if !end_str.is_empty() {
                    range_end = end_str.parse().ok();
                }
            }
        }
    }

    // Decode the URL-encoded filename and validate.
    let filename_raw = raw_path.trim_start_matches('/');
    let filename = match urlencoding::decode(filename_raw) {
        Ok(s) => s.into_owned(),
        Err(_) => {
            let _ = write_simple(&mut write_half, 400, "Bad Request").await;
            return;
        }
    };
    if !filename.starts_with(ALLOWED_PREFIX) || filename.contains('/') {
        let _ = write_simple(&mut write_half, 403, "Forbidden").await;
        return;
    }

    let path = cache_dir().join(&filename);
    let mut file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => {
            let _ = write_simple(&mut write_half, 404, "Not Found").await;
            return;
        }
    };
    let total = match file.metadata().await {
        Ok(m) => m.len(),
        Err(_) => {
            let _ = write_simple(&mut write_half, 500, "Internal Server Error").await;
            return;
        }
    };

    let (start, end, is_partial) = match (range_start, range_end) {
        (Some(s), Some(e)) if s <= e && e < total => (s, e, true),
        (Some(s), None) if s < total => (s, total - 1, true),
        (None, _) => (0, total.saturating_sub(1), false),
        _ => {
            let _ = write_simple(&mut write_half, 416, "Range Not Satisfiable").await;
            return;
        }
    };
    let length = end + 1 - start;
    let mime = guess_mime(&filename);

    let mut head = String::new();
    head.push_str(if is_partial {
        "HTTP/1.1 206 Partial Content\r\n"
    } else {
        "HTTP/1.1 200 OK\r\n"
    });
    head.push_str(&format!("Content-Type: {}\r\n", mime));
    head.push_str(&format!("Content-Length: {}\r\n", length));
    head.push_str("Accept-Ranges: bytes\r\n");
    head.push_str("Cache-Control: no-store\r\n");
    // CORS — lets the WebView draw the video element to a canvas
    // (we use this to capture poster frames when switching videos).
    // Loopback only, so wildcard is safe.
    head.push_str("Access-Control-Allow-Origin: *\r\n");
    if is_partial {
        head.push_str(&format!("Content-Range: bytes {}-{}/{}\r\n", start, end, total));
    }
    head.push_str("\r\n");
    if write_half.write_all(head.as_bytes()).await.is_err() {
        return;
    }

    if file
        .seek(std::io::SeekFrom::Start(start))
        .await
        .is_err()
    {
        return;
    }

    let mut remaining = length;
    let mut buf = vec![0u8; READ_CHUNK];
    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = match file.read(&mut buf[..to_read]).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => break,
        };
        if write_half.write_all(&buf[..n]).await.is_err() {
            break;
        }
        remaining -= n as u64;
    }
}

async fn write_simple(
    w: &mut tokio::net::tcp::WriteHalf<'_>,
    code: u16,
    reason: &str,
) -> std::io::Result<()> {
    let body = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        code, reason
    );
    w.write_all(body.as_bytes()).await
}

fn guess_mime(filename: &str) -> &'static str {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mov" => "video/quicktime",
        "avi" => "video/x-msvideo",
        "ogv" | "ogg" => "video/ogg",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "flac" => "audio/flac",
        "aac" => "audio/aac",
        "opus" => "audio/opus",
        _ => "application/octet-stream",
    }
}

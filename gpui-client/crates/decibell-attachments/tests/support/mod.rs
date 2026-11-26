//! Test support: a stand-in for core's pinning verifier (the app uses core's; tests can't link
//! core) and a mock of the community attachment API (`src/community/attachment_http.cpp`'s
//! routes, status codes and keep-alive rules), with fault knobs.
#![allow(dead_code)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use decibell_attachments::{AttachmentTarget, Attachments, PinnedTls};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use tokio_rustls::rustls::{self, ClientConfig, DigitallySignedStruct, SignatureScheme};
use tokio_rustls::{TlsAcceptor, TlsConnector};

pub const TOKEN: &str = "test-session-token";

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

fn pem_der(pem: &str) -> Vec<u8> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(body).unwrap()
}

pub fn test_cert() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = pem_der(include_str!("test-server.crt"));
    let key = pem_der(include_str!("test-server.key"));
    (CertificateDer::from(cert), PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)))
}

pub fn test_cert_fingerprint() -> String {
    sha256_hex(test_cert().0.as_ref())
}

/// Exact-pin verifier with core's contract (`CERT_MISMATCH:host:port:fp`). TEST ONLY: the app
/// passes core's `net::tls` verifier through its own `PinnedTls`.
#[derive(Debug)]
struct ExactPin {
    host: String,
    port: u16,
    fingerprint: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for ExactPin {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = sha256_hex(end_entity.as_ref());
        if fp != self.fingerprint {
            return Err(rustls::Error::General(format!("CERT_MISMATCH:{}:{}:{}", self.host, self.port, fp)));
        }
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub struct TestTls;

impl PinnedTls for TestTls {
    fn connector(&self, host: &str, port: u16, fingerprint: &str) -> Result<TlsConnector, String> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let v = ExactPin { host: host.into(), port, fingerprint: fingerprint.into(), provider: provider.clone() };
        let cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(v))
            .with_no_client_auth();
        Ok(TlsConnector::from(Arc::new(cfg)))
    }
}

pub fn service() -> Arc<Attachments> {
    Attachments::new(Arc::new(TestTls), tokio::runtime::Handle::current())
}

// ---------------------------------------------------------------------------------------------
// Mock attachment server

#[derive(Clone, Debug)]
pub struct Stored {
    pub expected: u64,
    pub data: Vec<u8>,
    pub ready: bool,
    pub kind: i64,
    pub filename: String,
    pub mime: String,
    pub encrypted: bool,
    pub width: i64,
    pub height: i64,
    pub placeholder: String,
    pub thumbs: HashMap<u32, Vec<u8>>,
}

#[derive(Default)]
pub struct Knobs {
    /// Refuse the next N inits with this status.
    pub init_refuse: Mutex<Option<(u16, usize)>>,
    /// Per-file cap (413 at init / PATCH). 0 = none.
    pub max_bytes: AtomicU64,
    /// Pretend this much free space (507 when size would leave less than 0). u64::MAX = plenty.
    pub free_bytes: AtomicU64,
    /// Drop the connection after taking this many body bytes of the next PATCH (kept, as the
    /// real server keeps what it fwrite()s before close_now()).
    pub cut_next_patch_after: Mutex<Option<usize>>,
    /// Answer the next N PATCHes with this status after reading (and dropping) the body, as the
    /// server's disk-full 500 comes after the body.
    pub patch_status: Mutex<Option<(u16, usize)>>,
    /// The idle sweep takes the upload just as the next PATCH arrives: the row is gone (404
    /// before the body is read, the connection closed; HEAD then 404s too).
    pub sweep_next_patch: std::sync::atomic::AtomicBool,
    /// Serve GETs whole (200) even when a Range was asked.
    pub ignore_range: std::sync::atomic::AtomicBool,
    /// Drop the connection of the next GET after this many body bytes.
    pub cut_next_get_after: Mutex<Option<usize>>,
}

pub struct Mock {
    pub addr: SocketAddr,
    pub files: Mutex<HashMap<i64, Stored>>,
    pub next_id: AtomicU64,
    pub knobs: Knobs,
    pub connections: AtomicUsize,
    pub requests: Mutex<Vec<String>>,
}

impl Mock {
    pub async fn start() -> Arc<Mock> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (cert, key) = test_cert();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS12])
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(cfg));
        let mock = Arc::new(Mock {
            addr,
            files: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            knobs: Knobs { free_bytes: AtomicU64::new(u64::MAX), ..Default::default() },
            connections: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        });
        let m = mock.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                m.connections.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                let m = m.clone();
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(tcp).await {
                        let _ = m.serve(tls).await;
                    }
                });
            }
        });
        mock
    }

    pub fn target(&self) -> AttachmentTarget {
        AttachmentTarget {
            host: "127.0.0.1".into(),
            port: self.addr.port(),
            jwt: TOKEN.into(),
            max_bytes: self.knobs.max_bytes.load(Ordering::SeqCst),
            cert_fingerprint: test_cert_fingerprint(),
        }
    }

    /// A finished attachment to download.
    pub fn seed(&self, data: Vec<u8>, encrypted: bool) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
        self.files.lock().unwrap().insert(
            id,
            Stored {
                expected: data.len() as u64,
                data,
                ready: true,
                kind: 2,
                filename: "f".into(),
                mime: "application/octet-stream".into(),
                encrypted,
                width: 0,
                height: 0,
                placeholder: String::new(),
                thumbs: HashMap::new(),
            },
        );
        id
    }

    pub fn file(&self, id: i64) -> Option<Stored> {
        self.files.lock().unwrap().get(&id).cloned()
    }

    pub fn count(&self, prefix: &str) -> usize {
        self.requests.lock().unwrap().iter().filter(|r| r.starts_with(prefix)).count()
    }

    async fn serve<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(&self, stream: S) -> std::io::Result<()> {
        let mut io = BufReader::new(stream);
        loop {
            let mut line = String::new();
            if io.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            let parts: Vec<&str> = line.trim_end().splitn(3, ' ').collect();
            if parts.len() < 3 {
                return Ok(());
            }
            let (method, target) = (parts[0].to_string(), parts[1].to_string());
            let mut headers = HashMap::new();
            loop {
                let mut h = String::new();
                io.read_line(&mut h).await?;
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            self.requests.lock().unwrap().push(format!("{method} {target}"));
            let (path, query) = target.split_once('?').map_or((target.as_str(), ""), |(a, b)| (a, b));
            let q: HashMap<&str, &str> = query.split('&').filter_map(|kv| kv.split_once('=')).collect();
            let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            if headers.get("authorization").map(String::as_str) != Some(&format!("Bearer {TOKEN}")) {
                return error(&mut io, 401).await;
            }
            let id: i64 = path.strip_prefix("/attachments/").and_then(|r| r.split('/').next()).and_then(|s| s.parse().ok()).unwrap_or(0);
            let tail = path.strip_prefix("/attachments/").and_then(|r| r.split_once('/')).map(|(_, t)| t).unwrap_or("");
            match (method.as_str(), path) {
                ("POST", "/attachments/init") => {
                    let mut body = vec![0u8; len];
                    io.read_exact(&mut body).await?;
                    let j: serde_json::Value = match serde_json::from_slice(&body) {
                        Ok(j) => j,
                        Err(_) => return error(&mut io, 400).await,
                    };
                    if let Some(status) = take_one(&self.knobs.init_refuse) {
                        return error(&mut io, status).await;
                    }
                    let size = j["size"].as_i64().unwrap_or(0);
                    let encrypted = j["encrypted"].as_bool().unwrap_or(false);
                    let max = self.knobs.max_bytes.load(Ordering::SeqCst);
                    if j["channelId"].as_str().unwrap_or("").is_empty() || size <= 0 {
                        return error(&mut io, 400).await;
                    }
                    if max > 0 && size as u64 > max {
                        return error(&mut io, 413).await;
                    }
                    if self.knobs.free_bytes.load(Ordering::SeqCst) < size as u64 {
                        return error(&mut io, 507).await;
                    }
                    let mime = j["mime"].as_str().unwrap_or("application/octet-stream").to_string();
                    let kind = if encrypted {
                        j["kind"].as_i64().unwrap_or(-1)
                    } else if mime.starts_with("image/") {
                        0
                    } else if mime.starts_with("video/") {
                        1
                    } else if mime.starts_with("audio/") {
                        3
                    } else {
                        2
                    };
                    let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
                    self.files.lock().unwrap().insert(
                        id,
                        Stored {
                            expected: size as u64,
                            data: Vec::new(),
                            ready: false,
                            kind,
                            filename: j["filename"].as_str().unwrap_or("").into(),
                            mime,
                            encrypted,
                            width: j["width"].as_i64().unwrap_or(0),
                            height: j["height"].as_i64().unwrap_or(0),
                            placeholder: j["placeholder"].as_str().unwrap_or("").into(),
                            thumbs: HashMap::new(),
                        },
                    );
                    respond(&mut io, 201, &[("Content-Type", "application/json".into())], format!("{{\"id\":{id},\"uploadOffset\":0}}").as_bytes()).await?;
                }
                ("PATCH", _) if tail.is_empty() => {
                    let state = self.files.lock().unwrap().get(&id).map(|f| (f.ready, f.data.len() as u64, f.expected));
                    let Some((ready, cur, expected)) = state else { return error(&mut io, 404).await };
                    if ready {
                        return error(&mut io, 409).await;
                    }
                    if self.knobs.sweep_next_patch.swap(false, Ordering::SeqCst) {
                        self.files.lock().unwrap().remove(&id);
                        return error(&mut io, 404).await;
                    }
                    if let Some(status) = take_one(&self.knobs.patch_status) {
                        let mut sink = vec![0u8; len];
                        io.read_exact(&mut sink).await?;
                        return error(&mut io, status).await;
                    }
                    let off: u64 = match headers.get("upload-offset").and_then(|v| v.parse().ok()) {
                        Some(o) => o,
                        None => return error(&mut io, 400).await,
                    };
                    if off != cur {
                        return error(&mut io, 409).await;
                    }
                    if len as u64 > expected - off {
                        return error(&mut io, 413).await;
                    }
                    let cut = self.knobs.cut_next_patch_after.lock().unwrap().take();
                    let mut left = len;
                    let mut buf = vec![0u8; 64 * 1024];
                    while left > 0 {
                        let want = left.min(buf.len());
                        let want = cut.map_or(want, |c| want.min(c.saturating_sub(len - left)));
                        if want == 0 {
                            return Ok(()); // the cut: drop the connection, keep what arrived
                        }
                        let n = io.read(&mut buf[..want]).await?;
                        if n == 0 {
                            return Ok(());
                        }
                        self.files.lock().unwrap().get_mut(&id).unwrap().data.extend_from_slice(&buf[..n]);
                        left -= n;
                    }
                    let now = self.files.lock().unwrap()[&id].data.len();
                    respond(&mut io, 204, &[("Upload-Offset", now.to_string())], b"").await?;
                }
                ("HEAD", _) if tail.is_empty() => {
                    let Some(f) = self.file(id) else { return error(&mut io, 404).await };
                    let off = if f.ready { f.expected } else { f.data.len() as u64 };
                    let status = if f.ready { "ready" } else { "uploading" };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nUpload-Offset: {off}\r\nUpload-Length: {}\r\nUpload-Status: {status}\r\nConnection: keep-alive\r\nContent-Length: 0\r\n\r\n",
                        f.expected
                    );
                    io.get_mut().write_all(head.as_bytes()).await?;
                }
                ("POST", _) if tail == "complete" => {
                    let outcome: Result<String, u16> = {
                        let mut files = self.files.lock().unwrap();
                        match files.get_mut(&id) {
                            None => Err(404),
                            Some(f) if f.ready || f.data.len() as u64 != f.expected => Err(409),
                            Some(f) => {
                                f.ready = true;
                                Ok(serde_json::json!({"id": id, "kind": f.kind, "filename": f.filename, "mime": f.mime,
                                    "sizeBytes": f.data.len(), "uploadStatus": "ready"}).to_string())
                            }
                        }
                    };
                    match outcome {
                        Ok(body) => respond(&mut io, 200, &[("Content-Type", "application/json".into())], body.as_bytes()).await?,
                        Err(status) => return error(&mut io, status).await,
                    }
                }
                ("POST", _) if tail == "thumbnail" => {
                    let size: u32 = q.get("size").and_then(|s| s.parse().ok()).unwrap_or(0);
                    if len == 0 || len > 512 * 1024 {
                        return error(&mut io, 413).await;
                    }
                    let mut body = vec![0u8; len];
                    io.read_exact(&mut body).await?;
                    if ![320, 640, 1280].contains(&size) {
                        return error(&mut io, 400).await;
                    }
                    let outcome: Result<(), u16> = {
                        let mut files = self.files.lock().unwrap();
                        match files.get_mut(&id) {
                            Some(f) if f.ready => {
                                f.thumbs.insert(size, body);
                                Ok(())
                            }
                            Some(_) => Err(409),
                            None => Err(404),
                        }
                    };
                    match outcome {
                        Ok(()) => respond(&mut io, 204, &[], b"").await?,
                        Err(status) => return error(&mut io, status).await,
                    }
                }
                ("DELETE", _) if tail.is_empty() => {
                    let outcome: Result<(), u16> = {
                        let mut files = self.files.lock().unwrap();
                        match files.get(&id) {
                            Some(f) if !f.ready => {
                                files.remove(&id);
                                Ok(())
                            }
                            Some(_) => Err(409),
                            None => Err(404),
                        }
                    };
                    match outcome {
                        Ok(()) => respond(&mut io, 204, &[], b"").await?,
                        Err(status) => return error(&mut io, status).await,
                    }
                }
                ("GET", _) if tail.is_empty() => {
                    let Some(f) = self.file(id) else { return error(&mut io, 404).await };
                    if !f.ready {
                        return error(&mut io, 404).await;
                    }
                    if q.get("variant") == Some(&"thumb") {
                        let req: u32 = q.get("size").and_then(|s| s.parse().ok()).unwrap_or(0);
                        let mask: u32 = f.thumbs.keys().map(|s| match s {
                            320 => 1,
                            640 => 2,
                            _ => 4,
                        }).sum();
                        let Some(chosen) = decibell_attachments::thumbs::served_thumbnail_size(mask, req) else {
                            return error(&mut io, 404).await;
                        };
                        let body = f.thumbs[&chosen].clone();
                        respond(&mut io, 200, &[("Content-Type", "image/jpeg".into())], &body).await?;
                        continue;
                    }
                    let total = f.data.len() as u64;
                    let mut start = 0u64;
                    let mut end = total.saturating_sub(1);
                    let mut partial = false;
                    if let Some(r) = headers.get("range").and_then(|r| r.strip_prefix("bytes=")) {
                        if !self.knobs.ignore_range.load(Ordering::SeqCst) {
                            let (a, b) = r.split_once('-').unwrap_or((r, ""));
                            if !a.is_empty() {
                                start = a.parse().unwrap_or(u64::MAX);
                            }
                            if !b.is_empty() {
                                end = b.parse().unwrap_or(u64::MAX);
                            }
                            if start >= total || end < start || end >= total {
                                return error(&mut io, 416).await;
                            }
                            partial = true;
                        }
                    }
                    let mut head = if partial {
                        format!("HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\n")
                    } else {
                        "HTTP/1.1 200 OK\r\n".to_string()
                    };
                    let body_len = if total == 0 { 0 } else { end - start + 1 };
                    head += &format!("Content-Type: {}\r\nContent-Length: {body_len}\r\nAccept-Ranges: bytes\r\nConnection: keep-alive\r\n\r\n", f.mime);
                    io.get_mut().write_all(head.as_bytes()).await?;
                    let cut = self.knobs.cut_next_get_after.lock().unwrap().take();
                    let mut pos = start as usize;
                    let stop = (start + body_len) as usize;
                    while pos < stop {
                        let n = (stop - pos).min(256 * 1024);
                        let n = cut.map_or(n, |c| n.min((start as usize + c).saturating_sub(pos)));
                        if n == 0 {
                            let _ = io.get_mut().flush().await;
                            return Ok(());
                        }
                        io.get_mut().write_all(&f.data[pos..pos + n]).await?;
                        pos += n;
                    }
                }
                _ => return error(&mut io, 404).await,
            }
            io.get_mut().flush().await?;
        }
    }
}

/// One use of a "next N requests get this status" knob.
fn take_one(knob: &Mutex<Option<(u16, usize)>>) -> Option<u16> {
    let mut g = knob.lock().unwrap();
    let (status, n) = (*g)?;
    *g = if n > 1 { Some((status, n - 1)) } else { None };
    Some(status)
}

async fn respond<W: AsyncWriteExt + tokio::io::AsyncRead + Unpin>(io: &mut BufReader<W>, status: u16, headers: &[(&str, String)], body: &[u8]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (k, v) in headers {
        head += &format!("{k}: {v}\r\n");
    }
    head += &format!("Connection: keep-alive\r\nContent-Length: {}\r\n\r\n", body.len());
    io.get_mut().write_all(head.as_bytes()).await?;
    io.get_mut().write_all(body).await?;
    io.get_mut().flush().await
}

/// Errors close the connection (the server can't resync after an unread body).
async fn error<W: AsyncWriteExt + tokio::io::AsyncRead + Unpin>(io: &mut BufReader<W>, status: u16) -> std::io::Result<()> {
    let body = format!("{{\"error\":\"{}\"}}", reason(status));
    let head = format!("HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", reason(status), body.len());
    io.get_mut().write_all(head.as_bytes()).await?;
    io.get_mut().write_all(body.as_bytes()).await?;
    io.get_mut().flush().await?;
    let _ = io.get_mut().shutdown().await;
    Ok(())
}

fn reason(s: u16) -> &'static str {
    match s {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        507 => "Insufficient Storage",
        _ => "Status",
    }
}

/// The xorshift32 stream of tests/vectors/vectors.ts.
pub fn xorshift(seed: u32) -> impl FnMut() -> u32 {
    let mut x = if seed == 0 { 1 } else { seed };
    move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x
    }
}

pub fn plaintext(size: usize) -> Vec<u8> {
    let mut next = xorshift(0x9e37_79b9 ^ size as u32);
    (0..size).map(|_| next() as u8).collect()
}

pub fn key_for(size: u64) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = ((i as u64 * 31 + size * 7 + 11) & 0xff) as u8;
    }
    k
}

pub fn repo_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..").canonicalize().unwrap()
}

/// The JS runtime for tests/vectors/gen_vectors.mjs: the Electron client's own (Electron 33,
/// `ELECTRON_RUN_AS_NODE=1`) when installed, else `node`; `None` without the repo's esbuild.
pub fn js_runtime() -> Option<std::process::Command> {
    let root = repo_root();
    if !root.join("electron-client/node_modules/esbuild").exists() {
        return None;
    }
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vectors/gen_vectors.mjs");
    let electron = root.join("electron-client/node_modules/electron/dist/electron");
    let mut cmd = if electron.exists() {
        let mut c = std::process::Command::new(electron);
        c.env("ELECTRON_RUN_AS_NODE", "1");
        c
    } else {
        let ok = std::process::Command::new("node").arg("--version").output().ok()?.status.success();
        if !ok {
            return None;
        }
        std::process::Command::new("node")
    };
    cmd.arg(script);
    Some(cmd)
}

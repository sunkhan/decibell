//! The loopback fixture server: plain HTTP and TLS on one port (a TLS ClientHello starts with
//! 0x16), serving `tests/fixtures/routes.json` exactly as the Node server in
//! `tests/vectors/gen-vectors.mjs` does, plus a few behaviours only the Rust tests need (stalls,
//! slow or endless bodies, holds). One request per connection, `Connection: close`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

pub(crate) fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// A test CA and a leaf for `127.0.0.1` and `a.test`, signed by it.
pub(crate) struct TestPki {
    pub ca: CertificateDer<'static>,
    pub leaf: CertificateDer<'static>,
    pub key: Vec<u8>,
}

pub(crate) fn pki() -> TestPki {
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = CertifiedIssuer::self_signed(ca_params, ca_key).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(vec!["127.0.0.1".to_owned(), "a.test".to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &ca)
        .unwrap();
    TestPki {
        ca: ca.der().clone(),
        leaf: leaf.der().clone(),
        key: leaf_key.serialize_der(),
    }
}

/// Every request a server saw: its path and its header lines as sent.
pub(crate) type Requests = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

/// What a path answers.
#[derive(Clone)]
pub(crate) enum Reply {
    /// Status, headers (`{origin}` / `{hostport}` / `{port}` substituted), body.
    Fixed {
        status: u16,
        headers: Vec<(String, Vec<u8>)>,
        body: Vec<u8>,
    },
    /// Read the request, then never answer.
    Never,
    /// Headers, then `body` in `chunk`-byte writes `delay` apart (endless when `repeat`).
    Trickle {
        headers: Vec<(String, Vec<u8>)>,
        body: Vec<u8>,
        chunk: usize,
        delay: Duration,
        repeat: bool,
    },
    /// Answer `inner` after `delay`.
    Delayed(Duration, Box<Reply>),
}

pub(crate) struct Server {
    pub origin: String,
    pub hostport: String,
    pub port: u16,
    /// Every request: (path, lowercased header lines).
    pub requests: Requests,
    /// Body bytes written per path.
    pub sent: Arc<Mutex<HashMap<String, usize>>>,
    pub max_active: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub(crate) fn hits(&self, path: &str) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p == path)
            .count()
    }

    pub(crate) fn sent(&self, path: &str) -> usize {
        self.sent.lock().unwrap().get(path).copied().unwrap_or(0)
    }

    pub(crate) fn subst(&self, s: &str) -> String {
        s.replace("{origin}", &self.origin)
            .replace("{hostport}", &self.hostport)
            .replace("{port}", &self.port.to_string())
    }
}

/// The body a route.json body spec describes.
pub(crate) fn body_of(spec: &Value) -> Vec<u8> {
    if let Some(f) = spec.get("file").and_then(Value::as_str) {
        return std::fs::read(fixtures().join(f)).unwrap();
    }
    if let Some(t) = spec.get("text").and_then(Value::as_str) {
        return t.as_bytes().to_vec();
    }
    let mut out = Vec::new();
    for p in spec["parts"].as_array().unwrap() {
        let count = p.get("count").and_then(Value::as_u64).unwrap_or(1) as usize;
        if let Some(h) = p.get("hex").and_then(Value::as_str) {
            out.extend(hex(h));
        } else if let Some(h) = p.get("repeatHex").and_then(Value::as_str) {
            out.extend(hex(h).repeat(count));
        } else if let Some(t) = p.get("repeat").and_then(Value::as_str) {
            out.extend(t.repeat(count).into_bytes());
        } else {
            out.extend(p["text"].as_str().unwrap().as_bytes());
        }
    }
    out
}

pub(crate) fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

pub(crate) fn manifest() -> Value {
    serde_json::from_slice(&std::fs::read(fixtures().join("routes.json")).unwrap()).unwrap()
}

/// The routes.json routes as replies (header values are JS strings of latin1 code points).
pub(crate) fn manifest_routes() -> HashMap<String, Reply> {
    let m = manifest();
    let mut out = HashMap::new();
    for (path, r) in m["routes"].as_object().unwrap() {
        let headers = r["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                let v: Vec<u8> = h[1]
                    .as_str()
                    .unwrap()
                    .chars()
                    .map(|c| u8::try_from(u32::from(c)).unwrap())
                    .collect();
                (h[0].as_str().unwrap().to_owned(), v)
            })
            .collect();
        out.insert(
            path.clone(),
            Reply::Fixed {
                status: r["status"].as_u64().unwrap() as u16,
                headers,
                body: body_of(&r["body"]),
            },
        );
    }
    out
}

pub(crate) async fn start(routes: HashMap<String, Reply>, pki: &TestPki) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    let origin = format!("http://{addr}");
    let hostport = addr.to_string();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![pki.leaf.clone()],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pki.key.clone())),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sent = Arc::new(Mutex::new(HashMap::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let ctx = Arc::new(Ctx {
        routes,
        origin: origin.clone(),
        hostport: hostport.clone(),
        port: addr.port(),
        requests: requests.clone(),
        sent: sent.clone(),
        active,
        max_active: max_active.clone(),
    });
    let task = tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else {
                return;
            };
            let ctx = ctx.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let mut first = [0u8; 1];
                if sock.peek(&mut first).await.unwrap_or(0) == 0 {
                    return;
                }
                if first[0] == 0x16 {
                    if let Ok(tls) = acceptor.accept(sock).await {
                        ctx.handle(tls).await;
                    }
                } else {
                    ctx.handle(sock).await;
                }
            });
        }
    });
    Server {
        origin,
        hostport,
        port: addr.port(),
        requests,
        sent,
        max_active,
        task,
    }
}

struct Ctx {
    routes: HashMap<String, Reply>,
    origin: String,
    hostport: String,
    port: u16,
    requests: Requests,
    sent: Arc<Mutex<HashMap<String, usize>>>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

impl Ctx {
    fn subst(&self, v: &[u8]) -> Vec<u8> {
        let s: String = v.iter().map(|&b| b as char).collect();
        s.replace("{origin}", &self.origin)
            .replace("{hostport}", &self.hostport)
            .replace("{port}", &self.port.to_string())
            .chars()
            .map(|c| c as u8)
            .collect()
    }

    async fn handle<S: AsyncRead + AsyncWrite + Unpin>(&self, mut s: S) {
        let mut head = Vec::new();
        let mut buf = [0u8; 4096];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
            if head.len() > 65536 {
                return;
            }
        }
        let text = String::from_utf8_lossy(&head).into_owned();
        let mut lines = text.split("\r\n");
        let target = lines
            .next()
            .unwrap_or("")
            .split(' ')
            .nth(1)
            .unwrap_or("/")
            .to_owned();
        let headers: Vec<(String, String)> = lines
            .take_while(|l| !l.is_empty())
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.to_owned(), v.trim().to_owned()))
            })
            .collect();
        let path = target.split('?').next().unwrap_or("").to_owned();
        self.requests.lock().unwrap().push((path.clone(), headers));
        let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(now, Ordering::SeqCst);
        let reply = self.routes.get(&path).cloned().unwrap_or(Reply::Fixed {
            status: 404,
            headers: vec![("content-type".into(), b"text/plain".to_vec())],
            body: b"not found".to_vec(),
        });
        self.reply(&mut s, &path, reply).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    async fn reply<S: AsyncWrite + Unpin>(&self, s: &mut S, path: &str, reply: Reply) {
        let mut reply = reply;
        while let Reply::Delayed(d, inner) = reply {
            tokio::time::sleep(d).await;
            reply = *inner;
        }
        let write_head = |status: u16, headers: &[(String, Vec<u8>)], len: Option<usize>| {
            let mut h = format!("HTTP/1.1 {status} X\r\n").into_bytes();
            for (k, v) in headers {
                h.extend_from_slice(k.as_bytes());
                h.extend_from_slice(b": ");
                h.extend_from_slice(&self.subst(v));
                h.extend_from_slice(b"\r\n");
            }
            if let Some(n) = len {
                h.extend_from_slice(format!("content-length: {n}\r\n").as_bytes());
            }
            h.extend_from_slice(b"connection: close\r\n\r\n");
            h
        };
        match reply {
            Reply::Never => tokio::time::sleep(Duration::from_secs(3600)).await,
            Reply::Fixed {
                status,
                headers,
                body,
            } => {
                let has_body = status != 204 && status != 304;
                let head = write_head(status, &headers, has_body.then_some(body.len()));
                if s.write_all(&head).await.is_err() {
                    return;
                }
                if has_body {
                    self.write_counted(s, path, &body).await;
                }
                let _ = s.shutdown().await;
            }
            Reply::Trickle {
                headers,
                body,
                chunk,
                delay,
                repeat,
            } => {
                let len = (!repeat).then_some(body.len());
                if s.write_all(&write_head(200, &headers, len)).await.is_err() {
                    return;
                }
                loop {
                    for piece in body.chunks(chunk) {
                        if !self.write_counted(s, path, piece).await {
                            return;
                        }
                        if !delay.is_zero() {
                            tokio::time::sleep(delay).await;
                        }
                    }
                    if !repeat {
                        break;
                    }
                }
                let _ = s.shutdown().await;
            }
            Reply::Delayed(..) => unreachable!(),
        }
    }

    async fn write_counted<S: AsyncWrite + Unpin>(
        &self,
        s: &mut S,
        path: &str,
        data: &[u8],
    ) -> bool {
        for piece in data.chunks(16 * 1024) {
            if s.write_all(piece).await.is_err() || s.flush().await.is_err() {
                return false;
            }
            *self
                .sent
                .lock()
                .unwrap()
                .entry(path.to_owned())
                .or_default() += piece.len();
        }
        true
    }
}

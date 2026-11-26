//! One small HTTPS/1.1 client for the community attachment API (port 8085): hyper's
//! connection-level API over tokio-rustls, keep-alive connections pooled per
//! (host, port, certificate fingerprint), at most [`MAX_CONNECTIONS`] per pool (Chromium's
//! per-host socket pool, which Electron relied on). No background tasks besides each
//! connection's own driver, no timers while idle: an idle connection is checked for age when it
//! is next taken (the server closes keep-alive connections after 30 s of silence).
//!
//! Trust is the app's: [`PinnedTls`] hands in a connector that accepts exactly the community's
//! certificate (core's pinning verifier, `decibell_core::net::tls`), so this crate never
//! decides what a valid certificate is.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use http::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_LENGTH, HOST};
use http::{HeaderMap, Method};
use http_body::{Body, Frame, SizeHint};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::TlsConnector;

use crate::Error;

/// Connections per (host, port, fingerprint).
pub const MAX_CONNECTIONS: usize = 6;
/// TCP + TLS handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Waiting for a response head after a small request, and between body reads. The server
/// gives up on a silent peer after 30 s; we wait a little longer than it would.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(45);
/// An idle keep-alive connection older than this is not reused (the server closes at 30 s).
const IDLE_REUSE: Duration = Duration::from_secs(25);

/// How the attachment host is trusted. Implemented by the app on top of core's pinning
/// verifier; see the crate docs.
pub trait PinnedTls: Send + Sync + 'static {
    /// A connector whose verifier accepts exactly the certificate whose sha256 (lowercase hex)
    /// is `fingerprint`, and fails the handshake with core's `CERT_MISMATCH:host:port:fp`
    /// error otherwise. Called once per pool.
    fn connector(&self, host: &str, port: u16, fingerprint: &str) -> Result<TlsConnector, String>;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
}

pub(crate) struct HttpClient {
    tls: Arc<dyn PinnedTls>,
    pools: Mutex<HashMap<Endpoint, Arc<Pool>>>,
}

struct Pool {
    endpoint: Endpoint,
    connector: TlsConnector,
    idle: Mutex<Vec<(SendRequest<ReqBody>, Instant)>>,
    slots: Arc<Semaphore>,
}

impl Pool {
    fn put_back(&self, sender: SendRequest<ReqBody>) {
        if sender.is_closed() {
            return;
        }
        let mut idle = self.idle.lock().unwrap();
        if idle.len() < MAX_CONNECTIONS {
            idle.push((sender, Instant::now()));
        }
    }

    fn take_idle(&self) -> Option<SendRequest<ReqBody>> {
        let mut idle = self.idle.lock().unwrap();
        while let Some((s, since)) = idle.pop() {
            if !s.is_closed() && since.elapsed() < IDLE_REUSE {
                return Some(s);
            }
        }
        None
    }
}

/// A request body: nothing, one buffer, or a stream of frames from a producer task (uploads),
/// always with a known length so the server gets a Content-Length.
pub(crate) enum ReqBody {
    Empty,
    Full(Option<Bytes>),
    Stream { rx: mpsc::Receiver<Result<Bytes, io::Error>>, left: u64 },
}

impl Body for ReqBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.get_mut() {
            ReqBody::Empty => Poll::Ready(None),
            ReqBody::Full(b) => Poll::Ready(b.take().map(|b| Ok(Frame::data(b)))),
            ReqBody::Stream { rx, left } => {
                if *left == 0 {
                    return Poll::Ready(None);
                }
                match rx.poll_recv(cx) {
                    Poll::Ready(Some(Ok(b))) => {
                        if b.len() as u64 > *left {
                            return Poll::Ready(Some(Err(io::Error::other("upload body longer than declared"))));
                        }
                        *left -= b.len() as u64;
                        Poll::Ready(Some(Ok(Frame::data(b))))
                    }
                    Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
                    Poll::Ready(None) => Poll::Ready(Some(Err(io::Error::other("upload body ended early")))),
                    Poll::Pending => Poll::Pending,
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        match self {
            ReqBody::Empty => true,
            ReqBody::Full(b) => b.is_none(),
            ReqBody::Stream { left, .. } => *left == 0,
        }
    }

    fn size_hint(&self) -> SizeHint {
        match self {
            ReqBody::Empty => SizeHint::with_exact(0),
            ReqBody::Full(b) => SizeHint::with_exact(b.as_ref().map_or(0, |b| b.len() as u64)),
            ReqBody::Stream { left, .. } => SizeHint::with_exact(*left),
        }
    }
}

impl ReqBody {
    fn len(&self) -> u64 {
        self.size_hint().exact().unwrap_or(0)
    }
}

/// Who to talk to and as whom.
pub(crate) struct Target<'a> {
    pub endpoint: &'a Endpoint,
    pub jwt: &'a str,
}

pub(crate) struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    body: Incoming,
    lease: Option<Lease>,
}

struct Lease {
    pool: Arc<Pool>,
    sender: SendRequest<ReqBody>,
    _permit: OwnedSemaphorePermit,
}

impl Response {
    /// The next body frame (`None` at the end). The connection goes back to the pool once the
    /// body has been read to the end; dropping the response earlier closes it.
    pub async fn frame(&mut self) -> Result<Option<Bytes>, Error> {
        loop {
            if self.body.is_end_stream() {
                self.release();
                return Ok(None);
            }
            let next = tokio::time::timeout(STALL_TIMEOUT, self.body.frame()).await.map_err(|_| Error::Timeout)?;
            match next {
                None => {
                    self.release();
                    return Ok(None);
                }
                Some(Err(e)) => return Err(Error::network(e)),
                Some(Ok(f)) => {
                    if let Ok(data) = f.into_data() {
                        if !data.is_empty() {
                            return Ok(Some(data));
                        }
                    }
                }
            }
        }
    }

    /// The whole body, for small answers (JSON, errors, thumbnails). Fails past `limit` bytes.
    pub async fn collect(mut self, limit: usize) -> Result<Bytes, Error> {
        let mut out = BytesMut::new();
        while let Some(b) = self.frame().await? {
            if out.len() + b.len() > limit {
                return Err(Error::Invalid("response larger than expected"));
            }
            if out.is_empty() && b.len() <= limit {
                out.reserve(b.len());
            }
            out.extend_from_slice(&b);
        }
        Ok(out.freeze())
    }

    /// Read and drop the rest of a small body so the connection can be reused.
    pub async fn drain(mut self) {
        let mut seen = 0usize;
        while let Ok(Some(b)) = self.frame().await {
            seen += b.len();
            if seen > 64 * 1024 {
                return;
            }
        }
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn release(&mut self) {
        if let Some(l) = self.lease.take() {
            l.pool.put_back(l.sender);
        }
    }
}

impl HttpClient {
    pub fn new(tls: Arc<dyn PinnedTls>) -> Self {
        Self { tls, pools: Mutex::new(HashMap::new()) }
    }

    fn pool(&self, ep: &Endpoint) -> Result<Arc<Pool>, Error> {
        let mut pools = self.pools.lock().unwrap();
        if let Some(p) = pools.get(ep) {
            return Ok(p.clone());
        }
        if ep.fingerprint.is_empty() {
            // Electron refuses a registered host without a matching fingerprint ("mismatch").
            return Err(Error::CertMismatch(format!("{}:{}: no pinned fingerprint", ep.host, ep.port)));
        }
        let connector = self.tls.connector(&ep.host, ep.port, &ep.fingerprint).map_err(Error::Network)?;
        let pool = Arc::new(Pool {
            endpoint: ep.clone(),
            connector,
            idle: Mutex::new(Vec::new()),
            slots: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        });
        pools.insert(ep.clone(), pool.clone());
        Ok(pool)
    }

    /// Drop every pooled connection to `host` (a community went away or its token changed).
    pub fn forget_host(&self, host: &str, port: u16) {
        self.pools.lock().unwrap().retain(|k, _| !(k.host == host && k.port == port));
    }

    async fn dial(pool: &Pool) -> Result<SendRequest<ReqBody>, Error> {
        let ep = &pool.endpoint;
        let connect = async {
            let tcp = TcpStream::connect((ep.host.as_str(), ep.port)).await.map_err(Error::network)?;
            let _ = tcp.set_nodelay(true);
            let name = ServerName::try_from(ep.host.clone()).map_err(|_| Error::Invalid("bad attachment host name"))?;
            let tls = pool.connector.connect(name, tcp).await.map_err(Error::network)?;
            let (sender, conn) = hyper::client::conn::http1::Builder::new()
                .handshake::<_, ReqBody>(TokioIo::new(tls))
                .await
                .map_err(Error::network)?;
            tokio::spawn(async move {
                // Ends when the server closes it or the last handle is dropped.
                let _ = conn.await;
            });
            Ok::<_, Error>(sender)
        };
        tokio::time::timeout(CONNECT_TIMEOUT, connect).await.map_err(|_| Error::Timeout)?
    }

    /// Send one request. `body` builds the body (called again when a reused keep-alive
    /// connection turns out to be closed and the request goes out once more on a fresh one).
    /// `head_timeout` bounds the wait for the response head after the body went out.
    pub async fn send(
        &self,
        target: Target<'_>,
        method: Method,
        path_and_query: &str,
        headers: &[(HeaderName, HeaderValue)],
        mut body: impl FnMut() -> ReqBody,
        head_timeout: Duration,
    ) -> Result<Response, Error> {
        let pool = self.pool(target.endpoint)?;
        let permit = pool.slots.clone().acquire_owned().await.map_err(|_| Error::Cancelled)?;
        let host_header = if target.endpoint.port == 443 {
            target.endpoint.host.clone()
        } else {
            format!("{}:{}", target.endpoint.host, target.endpoint.port)
        };
        let auth = HeaderValue::from_str(&format!("Bearer {}", target.jwt)).map_err(|_| Error::Invalid("bad token"))?;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let idle = match pool.take_idle() {
                // Its last response has just been read: the driver may need a moment to be idle.
                Some(mut s) => match tokio::time::timeout(Duration::from_secs(1), s.ready()).await {
                    Ok(Ok(())) => Some(s),
                    _ => None,
                },
                None => None,
            };
            let (mut sender, reused) = match idle {
                Some(s) => (s, true),
                None => (Self::dial(&pool).await?, false),
            };
            let b = body();
            let len = b.len();
            let mut req = http::Request::builder()
                .method(method.clone())
                .uri(path_and_query)
                .header(HOST, host_header.as_str())
                .header(AUTHORIZATION, auth.clone());
            for (k, v) in headers {
                req = req.header(k, v);
            }
            if !matches!(method, Method::GET | Method::HEAD) || len > 0 {
                req = req.header(CONTENT_LENGTH, len);
            }
            let req = req.body(b).map_err(|_| Error::Invalid("bad request"))?;
            match tokio::time::timeout(head_timeout, sender.send_request(req)).await {
                Err(_) => return Err(Error::Timeout),
                Ok(Ok(resp)) => {
                    let (parts, body) = resp.into_parts();
                    let mut r = Response {
                        status: parts.status.as_u16(),
                        headers: parts.headers,
                        body,
                        lease: Some(Lease { pool: pool.clone(), sender, _permit: permit }),
                    };
                    if r.body.is_end_stream() {
                        r.release();
                    }
                    return Ok(r);
                }
                Ok(Err(e)) => {
                    // A keep-alive connection the server closed just as we used it: once more
                    // on a fresh connection, when the request surely wasn't taken (never sent),
                    // or it is a read that is safe to repeat.
                    let idempotent = matches!(method, Method::GET | Method::HEAD);
                    if reused && attempt == 1 && (e.is_canceled() || (idempotent && e.is_incomplete_message())) {
                        continue;
                    }
                    return Err(Error::network(e));
                }
            }
        }
    }
}

/// Head timeout for a request carrying `len` body bytes: the stall timeout plus the time the
/// body takes at worst (the 100 KiB/s floor, doubled).
pub(crate) fn head_timeout_for(len: u64) -> Duration {
    STALL_TIMEOUT + Duration::from_secs_f64(len as f64 / crate::pacer::MIN_LIMIT_BPS as f64 * 2.0)
}

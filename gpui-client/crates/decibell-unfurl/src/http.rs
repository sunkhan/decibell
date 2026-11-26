//! A small public-web HTTP/1.1 client on hyper's connection API: one connection per request (no
//! pool, so nothing idles or ticks between requests), TLS by rustls with Mozilla's roots (the same
//! store Node bundles), every destination through the guard in [`crate::public_net`], and bodies
//! read incrementally with Content-Encoding undone and a cap enforced on the decoded bytes. What
//! `get()` / `decoded()` / `readUpTo()` in `linkPreview.ts` did on Node's `http` module, and what
//! `fetch()` did for `gifs.ts`.
//!
//! Callers own the policy around one exchange (redirects, the per-hop deadline, which statuses get
//! a body): [`HttpClient::get`] sends one request and returns the response head;
//! [`Response::read_body`] reads what the caller asks for.

use std::future::{poll_fn, Future};
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Empty;
use hyper::body::{Body, Incoming};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use url::{Position, Url};

use crate::public_net::{
    check_url_with, resolve_public, AddressRule, Destination, Resolve, SystemResolver,
};
use crate::Error;

/// Node's `autoSelectFamilyAttemptTimeout`: how long one address gets before the next is tried.
const ATTEMPT_TIMEOUT: Duration = Duration::from_millis(250);
/// Node's zlib `chunkSize`: decoded output reaches the reader in pieces of at most this.
const DECODED_CHUNK: usize = 16 * 1024;
/// Compressed input is fed to a decoder in slices this big, so a decompression bomb costs at most
/// one slice's worth of work past the cap.
const FEED_SLICE: usize = 4 * 1024;

pub(crate) struct HttpClient {
    rule: AddressRule,
    resolver: Arc<dyn Resolve>,
    /// Built on first use: no TLS state exists until a preview or a GIF search needs it.
    tls: OnceLock<Arc<rustls::ClientConfig>>,
    #[cfg(test)]
    extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
}

impl HttpClient {
    /// The production client: public addresses only, the system resolver, webpki roots.
    pub(crate) fn new() -> Self {
        Self {
            rule: AddressRule::PublicOnly,
            resolver: Arc::new(SystemResolver),
            tls: OnceLock::new(),
            #[cfg(test)]
            extra_roots: Vec::new(),
        }
    }

    /// Test builds only: allow 127.0.0.0/8, script DNS, trust a test CA.
    #[cfg(test)]
    pub(crate) fn for_tests(
        resolver: Arc<dyn Resolve>,
        extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    ) -> Self {
        Self {
            rule: AddressRule::PublicOrLoopback,
            resolver,
            tls: OnceLock::new(),
            extra_roots,
        }
    }

    fn tls_config(&self) -> Arc<rustls::ClientConfig> {
        self.tls
            .get_or_init(|| {
                let mut roots = rustls::RootCertStore {
                    roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
                };
                #[cfg(test)]
                for c in &self.extra_roots {
                    roots.add(c.clone()).expect("test root");
                }
                #[cfg(not(test))]
                let _ = &mut roots;
                let provider = Arc::new(rustls::crypto::ring::default_provider());
                // Node offers no ALPN on https.request either: HTTP/1.1 it is.
                let config = rustls::ClientConfig::builder_with_provider(provider)
                    .with_safe_default_protocol_versions()
                    .expect("ring supports TLS 1.2 and 1.3")
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                Arc::new(config)
            })
            .clone()
    }

    /// Connect to `url`'s host through the guard: refused by URL, or resolved once and refused
    /// unless every answer is allowed, then connected to one of exactly those addresses.
    async fn connect(&self, url: &Url) -> Result<Stream, Error> {
        let dest = check_url_with(url, self.rule).map_err(Error::Refused)?;
        let port = url.port_or_known_default().ok_or(Error::InvalidUrl)?;
        let ips = match &dest {
            Destination::Ip(ip) => vec![*ip],
            Destination::Name(name) => resolve_public(name, &self.resolver, self.rule).await?,
        };
        let tcp = connect_any(&interleave_families(&ips), port).await?;
        let _ = tcp.set_nodelay(true);
        if url.scheme() != "https" {
            return Ok(Stream::Plain(tcp));
        }
        let server_name = match dest {
            // An IP literal sends no SNI and is verified against the certificate's IP SANs.
            Destination::Ip(ip) => ServerName::IpAddress(ip.into()),
            Destination::Name(name) => ServerName::try_from(name.trim_end_matches('.').to_owned())
                .map_err(|e| Error::Network(Box::new(e)))?,
        };
        let tls = tokio_rustls::TlsConnector::from(self.tls_config())
            .connect(server_name, tcp)
            .await
            .map_err(|e| Error::Network(Box::new(e)))?;
        Ok(Stream::Tls(Box::new(tls)))
    }

    /// One GET, no redirect following; resolves once the response head is in. `headers` go out in
    /// order after `Host`; `title_case` spells header names as Node's `http` module does
    /// (`User-Agent`), off for the lowercase names `fetch()` sends.
    pub(crate) async fn get(
        &self,
        url: &Url,
        headers: &[(&'static str, &str)],
        title_case: bool,
    ) -> Result<Response, Error> {
        let stream = self.connect(url).await?;
        let (sender, conn) = http1::Builder::new()
            .title_case_headers(title_case)
            .handshake::<_, Empty<Bytes>>(TokioIo::new(stream))
            .await
            .map_err(|e| Error::Network(Box::new(e)))?;
        let mut sender = sender;
        let mut req = hyper::Request::get(request_target(url)?);
        req = req.header(http::header::HOST, host_header(url));
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let req = req
            .body(Empty::new())
            .map_err(|e| Error::Network(Box::new(e)))?;

        let mut conn: Pin<Box<dyn Future<Output = hyper::Result<()>> + Send>> = Box::pin(conn);
        let mut conn_done = false;
        let mut send = std::pin::pin!(sender.send_request(req));
        let res = poll_fn(|cx| {
            drive(&mut conn, &mut conn_done, cx);
            send.as_mut().poll(cx)
        })
        .await
        .map_err(|e| Error::Network(Box::new(e)))?;
        let (parts, body) = res.into_parts();
        Ok(Response {
            status: parts.status.as_u16(),
            headers: parts.headers,
            body,
            conn,
            conn_done,
        })
    }
}

/// Poll the connection task alongside whatever the caller is waiting on; once it finishes it is
/// never polled again (its errors surface through the request or body future).
fn drive(
    conn: &mut Pin<Box<dyn Future<Output = hyper::Result<()>> + Send>>,
    done: &mut bool,
    cx: &mut Context<'_>,
) {
    if !*done && conn.as_mut().poll(cx).is_ready() {
        *done = true;
    }
}

/// `Host` as Node sends it: the URL's host (IPv6 bracketed) plus `:port` unless it's the scheme's
/// default.
fn host_header(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default();
    match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.to_owned(),
    }
}

/// The request target as Node sends it (`pathname + search`, the WHATWG serialisation). The
/// `http` crate refuses a few characters the URL standard leaves raw in a query (`|`, `^`, `{`,
/// …); only then are the refused bytes percent-encoded.
fn request_target(url: &Url) -> Result<http::Uri, Error> {
    let target = &url[Position::BeforePath..Position::AfterQuery];
    if let Ok(uri) = http::Uri::try_from(target) {
        return Ok(uri);
    }
    let mut escaped = String::with_capacity(target.len() + 16);
    for b in target.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/?%".contains(&b) {
            escaped.push(b as char);
        } else {
            escaped.push_str(&format!("%{b:02X}"));
        }
    }
    http::Uri::try_from(escaped).map_err(|_| Error::InvalidUrl)
}

/// Node's `autoSelectFamily` order: each address once, families alternating, starting with the
/// family of the first answer.
fn interleave_families(ips: &[IpAddr]) -> Vec<IpAddr> {
    let mut first = Vec::new();
    let mut second = Vec::new();
    let first_v6 = ips.first().is_some_and(IpAddr::is_ipv6);
    for &ip in ips {
        let list = if ip.is_ipv6() == first_v6 {
            &mut first
        } else {
            &mut second
        };
        if !list.contains(&ip) {
            list.push(ip);
        }
    }
    let mut out = Vec::with_capacity(first.len() + second.len());
    for i in 0..first.len().max(second.len()) {
        out.extend(first.get(i));
        out.extend(second.get(i));
    }
    out
}

/// Try each vetted address in turn: 250 ms each before moving on (the last one gets the rest of
/// the caller's deadline), as Node's `autoSelectFamily` does.
async fn connect_any(ips: &[IpAddr], port: u16) -> Result<TcpStream, Error> {
    let mut last_err = io::Error::new(io::ErrorKind::NotFound, "no address to connect to");
    for (i, &ip) in ips.iter().enumerate() {
        let attempt = TcpStream::connect(SocketAddr::new(ip, port));
        let result = if i + 1 == ips.len() {
            attempt.await
        } else {
            match tokio::time::timeout(ATTEMPT_TIMEOUT, attempt).await {
                Ok(r) => r,
                Err(_) => continue,
            }
        };
        match result {
            Ok(s) => return Ok(s),
            Err(e) => last_err = e,
        }
    }
    Err(Error::Network(Box::new(last_err)))
}

pub(crate) enum Stream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// A response head with its body still on the wire. Dropping it closes the connection.
pub(crate) struct Response {
    pub status: u16,
    pub headers: http::HeaderMap,
    body: Incoming,
    conn: Pin<Box<dyn Future<Output = hyper::Result<()>> + Send>>,
    conn_done: bool,
}

impl Response {
    /// The first value of `name`, read as latin1 (Node's header strings), or `None`.
    pub(crate) fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .map(|v| crate::js::latin1(v.as_bytes()))
    }

    /// `String(res.headers["content-type"] ?? "").toLowerCase()`.
    pub(crate) fn content_type(&self) -> String {
        self.header("content-type")
            .unwrap_or_default()
            .to_lowercase()
    }

    /// Read the body with its Content-Encoding undone (gzip / x-gzip, deflate, br; anything else
    /// passes through as is), up to `limit` decoded bytes, or through the first chunk that
    /// completes `stop_at` (an ASCII marker, matched case-insensitively across chunk edges), or
    /// until `enough(bytes so far)` says so. `readUpTo` in linkPreview.ts, plus `enough`.
    pub(crate) async fn read_body(
        mut self,
        limit: usize,
        stop_at: Option<&[u8]>,
        mut enough: impl FnMut(&[u8]) -> bool,
    ) -> Result<Vec<u8>, Error> {
        // Node joins repeated Content-Encoding headers with ", " (and then matches none).
        let coding = self
            .headers
            .get_all(http::header::CONTENT_ENCODING)
            .iter()
            .map(|v| crate::js::latin1(v.as_bytes()))
            .collect::<Vec<_>>()
            .join(", ");
        let mut decoder = Decoder::for_coding(crate::js::trim(&coding).to_lowercase().as_str());
        let mut out = Collector::new(limit, stop_at);
        loop {
            let frame = poll_fn(|cx| {
                drive(&mut self.conn, &mut self.conn_done, cx);
                Pin::new(&mut self.body).poll_frame(cx)
            })
            .await;
            match frame {
                None => {
                    decoder.finish(&mut out)?;
                    break;
                }
                Some(Err(e)) => return Err(Error::Terminated(Box::new(e))),
                Some(Ok(frame)) => {
                    let Ok(data) = frame.into_data() else {
                        continue;
                    };
                    decoder.feed(&data, &mut out)?;
                    if !out.stopped && enough(&out.bytes) {
                        out.stopped = true;
                    }
                    if out.stopped {
                        break;
                    }
                }
            }
        }
        Ok(out.bytes)
    }
}

/// The `readUpTo` loop: whole chunks appended until `limit` (the result is cut to `limit`) or the
/// chunk that completes the marker; the previous chunk's tail is carried so a marker straddling
/// two chunks still matches.
struct Collector<'a> {
    bytes: Vec<u8>,
    total: usize,
    limit: usize,
    marker: Option<&'a [u8]>,
    carry: Vec<u8>,
    stopped: bool,
}

impl<'a> Collector<'a> {
    fn new(limit: usize, marker: Option<&'a [u8]>) -> Self {
        Self {
            bytes: Vec::new(),
            total: 0,
            limit,
            marker,
            carry: Vec::new(),
            stopped: false,
        }
    }

    fn chunk(&mut self, value: &[u8]) {
        if self.stopped || value.is_empty() {
            return;
        }
        let keep = value.len().min(self.limit.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&value[..keep]);
        self.total += value.len();
        if self.total >= self.limit {
            self.stopped = true;
            return;
        }
        if let Some(marker) = self.marker {
            // latin1 + toLowerCase: only ASCII letters can fold into an ASCII marker.
            let mut text = std::mem::take(&mut self.carry);
            text.extend(value.iter().map(u8::to_ascii_lowercase));
            if text.windows(marker.len()).any(|w| w == marker) {
                self.stopped = true;
                return;
            }
            let tail = marker.len().saturating_sub(1);
            self.carry = text[text.len().saturating_sub(tail)..].to_vec();
        }
    }

    /// Decoded bytes the reader can still use (a decoder never needs to produce more).
    fn room(&self) -> usize {
        self.limit.saturating_sub(self.total)
    }
}

/// Where the brotli decoder writes: bounded by the reader's remaining room, then it refuses
/// (which stops a decompression bomb in its tracks; the reader has stopped by then anyway).
struct Staging {
    buf: Vec<u8>,
    room: usize,
}

impl Write for Staging {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.room == 0 {
            return Err(io::Error::other("decoded size cap reached"));
        }
        let n = data.len().min(self.room);
        self.buf.extend_from_slice(&data[..n]);
        self.room -= n;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn corrupt(what: &'static str) -> Error {
    Error::Terminated(what.into())
}

/// gzip and zlib the way Node's zlib streams them (what `createGunzip` / `createInflate` gave the
/// TS): output in ≤ 16 KiB steps, each handed to the reader as it comes; a step that fails (bad
/// data, a gzip CRC or length mismatch, an adler32 mismatch) loses its output, as Node never pushes
/// it; several gzip members decode as one stream; after a member, a zero byte means padding and the
/// rest is ignored; the body ending mid-stream is "unexpected end of file".
struct Inflate {
    z: flate2::Decompress,
    gzip: bool,
    crc: flate2::Crc,
    state: InflateState,
    header: Vec<u8>,
    trailer: Vec<u8>,
    /// The step that ended a gzip member, held until its trailer checks out.
    held: Option<Vec<u8>>,
    buf: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InflateState {
    Header,
    Body,
    Trailer,
    Done,
    Padding,
}

/// Length of a complete gzip header at the start of `h`, `None` while incomplete.
fn gzip_header_len(h: &[u8]) -> Result<Option<usize>, Error> {
    if h.len() < 10 {
        // Reject a bad magic as soon as it shows, as zlib does.
        if h.first().is_some_and(|&b| b != 0x1f) || h.get(1).is_some_and(|&b| b != 0x8b) {
            return Err(corrupt("incorrect header check"));
        }
        return Ok(None);
    }
    if h[0] != 0x1f || h[1] != 0x8b {
        return Err(corrupt("incorrect header check"));
    }
    if h[2] != 8 {
        return Err(corrupt("unknown compression method"));
    }
    let flags = h[3];
    if flags & 0xe0 != 0 {
        return Err(corrupt("unknown header flags set"));
    }
    let mut i = 10;
    if flags & 0x04 != 0 {
        let Some(x) = h.get(i..i + 2) else {
            return Ok(None);
        };
        i += 2 + usize::from(u16::from_le_bytes([x[0], x[1]]));
    }
    for flag in [0x08, 0x10] {
        if flags & flag != 0 {
            let Some(rest) = h.get(i..) else {
                return Ok(None);
            };
            let Some(nul) = rest.iter().position(|&b| b == 0) else {
                return Ok(None);
            };
            i += nul + 1;
        }
    }
    if flags & 0x02 != 0 {
        i += 2;
    }
    Ok((h.len() >= i).then_some(i))
}

impl Inflate {
    fn new(gzip: bool) -> Self {
        Self {
            z: flate2::Decompress::new(!gzip),
            gzip,
            crc: flate2::Crc::new(),
            state: if gzip {
                InflateState::Header
            } else {
                InflateState::Body
            },
            header: Vec::new(),
            trailer: Vec::new(),
            held: None,
            buf: vec![0; DECODED_CHUNK],
        }
    }

    fn feed(&mut self, mut input: &[u8], out: &mut Collector<'_>) -> Result<(), Error> {
        loop {
            if out.stopped {
                return Ok(());
            }
            match self.state {
                InflateState::Header => {
                    if input.is_empty() {
                        return Ok(());
                    }
                    let before = self.header.len();
                    self.header.extend_from_slice(input);
                    match gzip_header_len(&self.header)? {
                        None => return Ok(()),
                        Some(n) => {
                            input = &input[n - before..];
                            self.header.clear();
                            self.state = InflateState::Body;
                        }
                    }
                }
                InflateState::Body => {
                    let (in0, out0) = (self.z.total_in(), self.z.total_out());
                    let status = self
                        .z
                        .decompress(input, &mut self.buf, flate2::FlushDecompress::None)
                        .map_err(|_| corrupt("invalid compressed data"))?;
                    let consumed = (self.z.total_in() - in0) as usize;
                    let produced = (self.z.total_out() - out0) as usize;
                    input = &input[consumed..];
                    let step = &self.buf[..produced];
                    if self.gzip {
                        self.crc.update(step);
                    }
                    if status == flate2::Status::StreamEnd {
                        if self.gzip {
                            self.held = Some(step.to_vec());
                            self.trailer.clear();
                            self.state = InflateState::Trailer;
                        } else {
                            out.chunk(step);
                            self.state = InflateState::Done;
                        }
                        continue;
                    }
                    out.chunk(step);
                    if consumed == 0 && produced < self.buf.len() {
                        return Ok(()); // needs more input
                    }
                }
                InflateState::Trailer => {
                    let take = (8 - self.trailer.len()).min(input.len());
                    self.trailer.extend_from_slice(&input[..take]);
                    input = &input[take..];
                    if self.trailer.len() < 8 {
                        // zlib returned this step's output without an error: Node pushed it.
                        if let Some(step) = self.held.take() {
                            out.chunk(&step);
                        }
                        return Ok(());
                    }
                    let t = &self.trailer;
                    let crc = u32::from_le_bytes([t[0], t[1], t[2], t[3]]);
                    let len = u32::from_le_bytes([t[4], t[5], t[6], t[7]]);
                    if crc != self.crc.sum() {
                        self.held = None;
                        return Err(corrupt("incorrect data check"));
                    }
                    if len != self.crc.amount() {
                        self.held = None;
                        return Err(corrupt("incorrect length check"));
                    }
                    if let Some(step) = self.held.take() {
                        out.chunk(&step);
                    }
                    self.state = InflateState::Done;
                }
                InflateState::Done => {
                    if input.is_empty() || !self.gzip {
                        return Ok(()); // what follows a zlib stream is ignored
                    }
                    if input[0] == 0 {
                        self.state = InflateState::Padding;
                        return Ok(());
                    }
                    // Another member.
                    self.z.reset(false);
                    self.crc = flate2::Crc::new();
                    self.state = InflateState::Header;
                }
                InflateState::Padding => return Ok(()),
            }
        }
    }

    fn finish(&mut self) -> Result<(), Error> {
        match self.state {
            InflateState::Done | InflateState::Padding => Ok(()),
            _ => Err(corrupt("unexpected end of file")),
        }
    }
}

enum Decoder {
    Identity,
    Inflate(Box<Inflate>),
    Brotli(Box<brotli_decompressor::DecompressorWriter<Staging>>),
}

impl Decoder {
    fn for_coding(coding: &str) -> Self {
        match coding {
            "gzip" | "x-gzip" => Decoder::Inflate(Box::new(Inflate::new(true))),
            "deflate" => Decoder::Inflate(Box::new(Inflate::new(false))),
            "br" => Decoder::Brotli(Box::new(brotli_decompressor::DecompressorWriter::new(
                Staging {
                    buf: Vec::new(),
                    room: 0,
                },
                DECODED_CHUNK,
            ))),
            _ => Decoder::Identity,
        }
    }

    /// One wire chunk. A decode error after the reader stopped is never seen (the TS loop had
    /// left by then); before, it fails the read.
    fn feed(&mut self, data: &[u8], out: &mut Collector<'_>) -> Result<(), Error> {
        match self {
            Decoder::Identity => {
                out.chunk(data);
                Ok(())
            }
            Decoder::Inflate(z) => z.feed(data, out),
            Decoder::Brotli(d) => {
                for slice in data.chunks(FEED_SLICE) {
                    d.get_mut().room = out.room();
                    let r = d.write_all(slice);
                    let staged = std::mem::take(&mut d.get_mut().buf);
                    let capped = d.get_ref().room == 0;
                    if r.is_err() && !capped {
                        // The failing step's output is lost, as with Node's zlib.
                        return Err(corrupt("invalid brotli data"));
                    }
                    for piece in staged.chunks(DECODED_CHUNK) {
                        out.chunk(piece);
                    }
                    if out.stopped {
                        return Ok(());
                    }
                }
                Ok(())
            }
        }
    }

    /// End of body: the decoder must have seen a complete stream, as Node's zlib reports
    /// "unexpected end of file" otherwise.
    fn finish(&mut self, out: &mut Collector<'_>) -> Result<(), Error> {
        match self {
            Decoder::Identity => Ok(()),
            Decoder::Inflate(z) => z.finish(),
            Decoder::Brotli(d) => {
                d.get_mut().room = out.room();
                let r = d.close();
                let staged = std::mem::take(&mut d.get_mut().buf);
                if r.is_err() && d.get_ref().room != 0 {
                    return Err(corrupt("unexpected end of file"));
                }
                for piece in staged.chunks(DECODED_CHUNK) {
                    out.chunk(piece);
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn happy_eyeballs_order() {
        let got = interleave_families(&[
            ip("1.1.1.1"),
            ip("1.0.0.1"),
            ip("2606::1"),
            ip("1.1.1.1"),
            ip("2606::2"),
        ]);
        assert_eq!(
            got,
            vec![ip("1.1.1.1"), ip("2606::1"), ip("1.0.0.1"), ip("2606::2")]
        );
        let got = interleave_families(&[ip("2606::1"), ip("1.1.1.1")]);
        assert_eq!(got, vec![ip("2606::1"), ip("1.1.1.1")]);
    }

    #[test]
    fn host_and_target() {
        // The WHATWG serialisation goes out as is (`|`, `^`, `{`, `}` stay raw in a query, as Node
        // sends them).
        let u = Url::parse("https://Example.com:443/a b?q=x|y^z{}#frag").unwrap();
        assert_eq!(host_header(&u), "example.com");
        assert_eq!(request_target(&u).unwrap().to_string(), "/a%20b?q=x|y^z{}");
        let u = Url::parse("http://[::1]:8080/").unwrap();
        assert_eq!(host_header(&u), "[::1]:8080");
        assert_eq!(request_target(&u).unwrap().to_string(), "/");
    }

    #[test]
    fn collector_marker_and_limit() {
        let mut c = Collector::new(100, Some(b"</head>"));
        c.chunk(b"<html><head><title>x</title></HE");
        assert!(!c.stopped);
        c.chunk(b"AD><body>");
        assert!(c.stopped);
        assert_eq!(c.bytes, b"<html><head><title>x</title></HEAD><body>");

        let mut c = Collector::new(10, None);
        c.chunk(b"0123456");
        c.chunk(b"789abc");
        assert!(c.stopped);
        assert_eq!(c.bytes, b"0123456789");
    }

    fn decode_all(coding: &str, data: &[u8], limit: usize) -> Result<Vec<u8>, Error> {
        let mut d = Decoder::for_coding(coding);
        let mut c = Collector::new(limit, None);
        for chunk in data.chunks(7) {
            d.feed(chunk, &mut c)?;
            if c.stopped {
                return Ok(c.bytes);
            }
        }
        d.finish(&mut c)?;
        Ok(c.bytes)
    }

    #[test]
    fn decoders() {
        use flate2::write::{GzEncoder, ZlibEncoder};
        let text = b"hello hello hello hello world".repeat(50);
        let mut gz = GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&text).unwrap();
        let gz = gz.finish().unwrap();
        assert_eq!(decode_all("gzip", &gz, 1 << 20).unwrap(), text);
        assert_eq!(decode_all("x-gzip", &gz, 1 << 20).unwrap(), text);
        assert_eq!(decode_all("gzip", &gz, 100).unwrap(), &text[..100]);
        // Two members back to back decode as one stream (Node does the same).
        let both = [gz.clone(), gz.clone()].concat();
        assert_eq!(
            decode_all("gzip", &both, 1 << 20).unwrap(),
            [text.clone(), text.clone()].concat()
        );
        // Truncated: no trailer, so it fails like Node's "unexpected end of file".
        assert!(decode_all("gzip", &gz[..gz.len() - 4], 1 << 20).is_err());
        // Corrupt CRC: the step that read the trailer loses its output and fails the read.
        let mut bad = gz.clone();
        let n = bad.len();
        bad[n - 6] ^= 0xff;
        assert!(decode_all("gzip", &bad, 1 << 20).is_err());
        // Zero padding after a member is ignored; other trailing bytes start a new member.
        let padded = [gz.clone(), vec![0; 10], b"garbage".to_vec()].concat();
        assert_eq!(decode_all("gzip", &padded, 1 << 20).unwrap(), text);
        assert!(decode_all("gzip", &[gz.clone(), b"garbage".to_vec()].concat(), 1 << 20).is_err());
        // An empty body is not a gzip stream.
        assert!(decode_all("gzip", b"", 1 << 20).is_err());
        // A header with every optional field.
        let mut hdr = vec![
            0x1f,
            0x8b,
            8,
            0x04 | 0x08 | 0x10 | 0x02,
            0,
            0,
            0,
            0,
            0,
            3,
            2,
            0,
            b'x',
            b'y',
        ];
        hdr.extend_from_slice(b"name\0comment\0\0\0");
        assert_eq!(gzip_header_len(&hdr).unwrap(), Some(hdr.len()));
        assert_eq!(gzip_header_len(&hdr[..hdr.len() - 1]).unwrap(), None);

        let mut z = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&text).unwrap();
        let z = z.finish().unwrap();
        assert_eq!(decode_all("deflate", &z, 1 << 20).unwrap(), text);
        assert!(decode_all("deflate", &z[..z.len() - 2], 1 << 20).is_err());
        let mut bad = z.clone();
        let n = bad.len();
        bad[n - 1] ^= 0xff; // adler32
        assert!(decode_all("deflate", &bad, 1 << 20).is_err());
        assert_eq!(decode_all("identity", &text, 1 << 20).unwrap(), text);
        assert_eq!(decode_all("compress", &text, 1 << 20).unwrap(), text);
        assert!(decode_all("gzip", b"not gzip at all", 1 << 20).is_err());
    }

    #[test]
    fn decompression_bomb_stays_bounded() {
        use flate2::write::GzEncoder;
        // 64 MiB of zeros compresses to ~64 KiB; reading with a 1 MiB cap must stop at 1 MiB.
        let mut gz = GzEncoder::new(Vec::new(), flate2::Compression::best());
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..64 {
            gz.write_all(&zeros).unwrap();
        }
        let gz = gz.finish().unwrap();
        assert!(gz.len() < 200 * 1024);
        let got = decode_all("gzip", &gz, 1 << 20).unwrap();
        assert_eq!(got.len(), 1 << 20);
    }
}

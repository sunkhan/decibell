//! Reading attachments: plaintext bytes of any attachment by range, plain or sealed
//! (`attachmentFetch.ts`, `protocol.ts`, `mediaServer.ts`, `netFetch.ts`), held to the download
//! cap (`downloadPacer.ts`). A sealed range maps to whole sealed chunks (one upstream Range
//! request), each opened in place in one reused buffer and trimmed to the range, so nothing is
//! buffered beyond a chunk and nothing is allocated per chunk: unlike Electron (which buffered a
//! ranged answer whole and so capped it at 4 MiB), a stream may run to the end of the file.

use std::sync::Arc;

use bytes::Bytes;
use http::Method;
use tokio_util::sync::CancellationToken;

use crate::crypto::{plan_range, sealed_chunk_len, TAG_BYTES};
use crate::http::{Response, Target, STALL_TIMEOUT};
use crate::pacer::{Direction, Pacer, SLICE_BYTES};
use crate::thumbs::THUMB_SIZES;
use crate::{AttachmentKey, Attachments, Error};

/// Largest thumbnail body accepted (the server stores at most 512 KiB, sealed adds a tag).
const MAX_THUMB_BYTES: usize = 512 * 1024 + TAG_BYTES as usize;

/// Plaintext of one attachment from `start` to `end` (inclusive) or its end. Read with
/// [`AttachmentStream::next`]; dropping it mid-way closes the connection.
pub struct AttachmentStream {
    svc: Arc<Attachments>,
    resp: Option<Response>,
    pacer: Arc<tokio::sync::Mutex<Pacer>>,
    cancel: Option<CancellationToken>,
    /// Plaintext size of the whole attachment, when the server or the key told us.
    total: Option<u64>,
    start: u64,
    /// Bytes still to deliver; `None` = until the body ends.
    remaining: Option<u64>,
    mode: Mode,
}

enum Mode {
    Plain {
        frame: Bytes,
        pos: usize,
        /// Bytes to discard first (a server that ignored the Range header).
        skip: u64,
    },
    Sealed(Box<Sealed>),
    Empty,
}

struct Sealed {
    key: crate::FileKey,
    chunk: u64,
    size: u64,
    index: u64,
    last: u64,
    first: u64,
    trim_start: u64,
    trim_end: u64,
    /// Ciphertext bytes to discard first (a server that ignored the Range header).
    skip: u64,
    frame: Bytes,
    frame_pos: usize,
    /// One sealed chunk, opened in place.
    buf: Vec<u8>,
    filled: usize,
    /// Plaintext window of `buf` still to deliver.
    out: (usize, usize),
}

impl AttachmentStream {
    /// Plaintext size of the whole attachment, when known.
    pub fn total(&self) -> Option<u64> {
        self.total
    }

    /// The first plaintext byte this stream delivers.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The next piece of plaintext, borrowed from the stream (valid until the next call);
    /// `None` at the end of the range. Paced to the download cap, in 64 KiB slices while one is
    /// set.
    pub async fn next(&mut self) -> Result<Option<&[u8]>, Error> {
        if self.remaining == Some(0) {
            self.finish().await;
            return Ok(None);
        }
        // Find the window to hand out next (indices into the current frame or chunk buffer).
        let (lo, hi) = loop {
            match &mut self.mode {
                Mode::Empty => return Ok(None),
                Mode::Plain { frame, pos, skip } => {
                    if *pos < frame.len() {
                        break (*pos, frame.len());
                    }
                    let Some(resp) = self.resp.as_mut() else { return Ok(None) };
                    match with_cancel(resp.frame(), self.cancel.as_ref()).await? {
                        None => {
                            if self.remaining.is_some_and(|r| r > 0) || *skip > 0 {
                                return Err(Error::ShortRead);
                            }
                            self.resp = None;
                            return Ok(None);
                        }
                        Some(mut b) => {
                            if *skip > 0 {
                                let n = (*skip).min(b.len() as u64) as usize;
                                *skip -= n as u64;
                                b = b.slice(n..);
                            }
                            *frame = b;
                            *pos = 0;
                        }
                    }
                }
                Mode::Sealed(s) => {
                    if s.out.0 < s.out.1 {
                        break s.out;
                    }
                    if s.index > s.last {
                        self.finish().await;
                        return Ok(None);
                    }
                    let want = sealed_chunk_len(s.index, s.size, s.chunk) as usize;
                    while s.filled < want {
                        if s.frame_pos >= s.frame.len() {
                            let Some(resp) = self.resp.as_mut() else { return Err(Error::ShortRead) };
                            match with_cancel(resp.frame(), self.cancel.as_ref()).await? {
                                None => return Err(Error::ShortRead),
                                Some(mut b) => {
                                    if s.skip > 0 {
                                        let n = s.skip.min(b.len() as u64) as usize;
                                        s.skip -= n as u64;
                                        b = b.slice(n..);
                                    }
                                    s.frame = b;
                                    s.frame_pos = 0;
                                    continue;
                                }
                            }
                        }
                        let n = (want - s.filled).min(s.frame.len() - s.frame_pos);
                        s.buf[s.filled..s.filled + n].copy_from_slice(&s.frame[s.frame_pos..s.frame_pos + n]);
                        s.filled += n;
                        s.frame_pos += n;
                    }
                    let plain = s.key.open_chunk_in_place(s.index, &mut s.buf[..want])?;
                    let lo = if s.index == s.first { s.trim_start as usize } else { 0 };
                    let hi = if s.index == s.last {
                        (s.trim_end - (s.last - s.first) * s.chunk) as usize
                    } else {
                        plain
                    };
                    s.out = (lo.min(plain), hi.min(plain));
                    s.filled = 0;
                    s.index += 1;
                }
            }
        };
        let mut n = hi - lo;
        if let Some(r) = self.remaining {
            n = n.min(r as usize);
        }
        if self.svc.limits().download_rate() > 0 {
            n = n.min(SLICE_BYTES);
        }
        let ok = self.pacer.lock().await.take(n, self.svc.limits(), Direction::Down, self.cancel.as_ref()).await;
        if !ok {
            return Err(Error::Cancelled);
        }
        if let Some(r) = self.remaining.as_mut() {
            *r -= n as u64;
        }
        Ok(Some(match &mut self.mode {
            Mode::Plain { frame, pos, .. } => {
                *pos += n;
                &frame[lo..lo + n]
            }
            Mode::Sealed(s) => {
                s.out.0 += n;
                &s.buf[lo..lo + n]
            }
            Mode::Empty => unreachable!(),
        }))
    }

    /// The range is delivered: read the body's end so the keep-alive connection goes back to
    /// the pool (a body that still has bytes, from a server that ignored the range, is dropped
    /// with its connection instead).
    async fn finish(&mut self) {
        if let Some(mut resp) = self.resp.take() {
            let _ = resp.frame().await;
        }
    }

    /// Everything left, appended to `out`.
    pub async fn read_to_end(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        while let Some(b) = self.next().await? {
            out.extend_from_slice(b);
        }
        Ok(())
    }
}

fn content_range_total(v: Option<&str>) -> Option<u64> {
    let v = v?;
    v.rsplit('/').next()?.trim().parse().ok()
}

impl Attachments {
    /// Plaintext of attachment `id` from byte `start` to `end` (inclusive) or its end, opened
    /// with the registered key when there is one.
    pub async fn open(
        self: &Arc<Self>,
        server_id: &str,
        id: i64,
        start: u64,
        end: Option<u64>,
        cancel: Option<CancellationToken>,
    ) -> Result<AttachmentStream, Error> {
        let key = self.key(server_id, id);
        self.open_with_key(server_id, id, key.as_ref(), start, end, cancel).await
    }

    /// [`Self::open`] with the key supplied by the caller (the downloads manager keeps its own
    /// copy: the registry is cleared on disconnect and empty after a restart). `None` = plain.
    pub async fn open_with_key(
        self: &Arc<Self>,
        server_id: &str,
        id: i64,
        key: Option<&AttachmentKey>,
        start: u64,
        end: Option<u64>,
        cancel: Option<CancellationToken>,
    ) -> Result<AttachmentStream, Error> {
        if id <= 0 {
            return Err(Error::Invalid("bad attachment id"));
        }
        let target = self.require_target(server_id)?;
        let ep = target.endpoint();
        let path = format!("/attachments/{id}");
        let pacer = self.pacers.get(server_id, id);
        let mut stream = AttachmentStream {
            svc: self.clone(),
            resp: None,
            pacer,
            cancel: cancel.clone(),
            total: None,
            start,
            remaining: None,
            mode: Mode::Empty,
        };

        let Some(key) = key else {
            let range = match (start, end) {
                (0, None) => None,
                (s, None) => Some(format!("bytes={s}-")),
                (s, Some(e)) => Some(format!("bytes={s}-{e}")),
            };
            let headers: Vec<_> = range
                .iter()
                .map(|r| (http::header::RANGE, http::HeaderValue::from_str(r).expect("ascii range")))
                .collect();
            let send = self.http.send(Target { endpoint: &ep, jwt: &target.jwt }, Method::GET, &path, &headers, || crate::http::ReqBody::Empty, STALL_TIMEOUT);
            let resp = with_cancel(send, cancel.as_ref()).await?;
            let mut skip = 0;
            match resp.status {
                200 => {
                    let len: Option<u64> = resp.header("content-length").and_then(|v| v.parse().ok());
                    stream.total = len;
                    skip = start;
                    stream.remaining = match (end, len) {
                        (Some(e), _) => Some(e + 1 - start),
                        (None, Some(l)) => Some(l.saturating_sub(start)),
                        (None, None) => None,
                    };
                }
                206 => {
                    stream.total = content_range_total(resp.header("content-range"));
                    let len: Option<u64> = resp.header("content-length").and_then(|v| v.parse().ok());
                    stream.remaining = len;
                }
                s => return Err(Error::from_status(s)),
            }
            stream.mode = Mode::Plain { frame: Bytes::new(), pos: 0, skip };
            stream.resp = Some(resp);
            return Ok(stream);
        };

        let size = key.size_bytes;
        stream.total = Some(size);
        if size == 0 {
            return if start == 0 { Ok(stream) } else { Err(Error::Http(416)) };
        }
        if start >= size || end.is_some_and(|e| e < start) {
            return Err(Error::Http(416));
        }
        let end = end.map_or(size - 1, |e| e.min(size - 1));
        let plan = plan_range(size, key.chunk_bytes, Some((start, end)));
        let (ct_start, ct_end) = plan.upstream.expect("non-empty");
        let range = [(http::header::RANGE, http::HeaderValue::from_str(&format!("bytes={ct_start}-{ct_end}")).expect("ascii range"))];
        let send = self.http.send(
            Target { endpoint: &ep, jwt: &target.jwt },
            Method::GET,
            &path,
            &range,
            || crate::http::ReqBody::Empty,
            STALL_TIMEOUT,
        );
        let resp = with_cancel(send, cancel.as_ref()).await?;
        let skip = match resp.status {
            206 => 0,
            200 => ct_start,
            s => return Err(Error::from_status(s)),
        };
        stream.remaining = Some(end + 1 - start);
        stream.mode = Mode::Sealed(Box::new(Sealed {
            key: key.key.clone(),
            chunk: key.chunk_bytes,
            size,
            index: plan.first_chunk,
            last: plan.last_chunk,
            first: plan.first_chunk,
            trim_start: plan.trim_start,
            trim_end: plan.trim_end,
            skip,
            frame: Bytes::new(),
            frame_pos: 0,
            buf: vec![0u8; (key.chunk_bytes + TAG_BYTES) as usize],
            filled: 0,
            out: (0, 0),
        }));
        stream.resp = Some(resp);
        Ok(stream)
    }

    /// A whole attachment in memory (Copy image, small files), decrypted when sealed.
    pub async fn fetch_all(self: &Arc<Self>, server_id: &str, id: i64, cancel: Option<CancellationToken>) -> Result<Vec<u8>, Error> {
        let mut s = self.open(server_id, id, 0, None, cancel).await?;
        let mut out = Vec::with_capacity(s.total().unwrap_or(0).min(256 << 20) as usize);
        s.read_to_end(&mut out).await?;
        Ok(out)
    }

    /// A thumbnail (`?variant=thumb&size=N`), opened when the attachment is sealed. The server
    /// answers with the nearest size it has, and a sealed thumbnail's nonce names its size, so
    /// the other sizes are tried when `size_px` doesn't open it (Electron failed those).
    pub async fn fetch_thumbnail(
        self: &Arc<Self>,
        server_id: &str,
        id: i64,
        size_px: u32,
        cancel: Option<CancellationToken>,
    ) -> Result<Bytes, Error> {
        let target = self.require_target(server_id)?;
        let ep = target.endpoint();
        let path = format!("/attachments/{id}?variant=thumb&size={size_px}");
        let send = self.http.send(Target { endpoint: &ep, jwt: &target.jwt }, Method::GET, &path, &[], || crate::http::ReqBody::Empty, STALL_TIMEOUT);
        let mut resp = with_cancel(send, cancel.as_ref()).await?;
        if resp.status != 200 {
            return Err(Error::from_status(resp.status));
        }
        let pacer = self.pacers.get(server_id, id);
        let mut body = Vec::new();
        while let Some(b) = resp.frame().await? {
            if body.len() + b.len() > MAX_THUMB_BYTES {
                return Err(Error::Invalid("thumbnail larger than the server allows"));
            }
            if !pacer.lock().await.take(b.len(), self.limits(), Direction::Down, cancel.as_ref()).await {
                return Err(Error::Cancelled);
            }
            body.extend_from_slice(&b);
        }
        let Some(key) = self.key(server_id, id) else { return Ok(Bytes::from(body)) };
        let order = std::iter::once(size_px).chain(THUMB_SIZES.iter().copied().filter(|&s| s != size_px));
        for size in order {
            if let Ok(jpeg) = key.key.open_thumbnail(size, &body) {
                return Ok(Bytes::from(jpeg));
            }
        }
        Err(Error::Decrypt)
    }
}

pub(crate) async fn with_cancel<T>(fut: impl std::future::Future<Output = Result<T, Error>>, cancel: Option<&CancellationToken>) -> Result<T, Error> {
    match cancel {
        None => fut.await,
        Some(c) => tokio::select! {
            r = fut => r,
            _ = c.cancelled() => Err(Error::Cancelled),
        },
    }
}

/// The futures the app spawns must be `Send` (checked at compile time, never called).
#[allow(dead_code)]
fn assert_send(svc: Arc<Attachments>) {
    fn is_send<T: Send>(_: &T) {}
    is_send(&svc.open("s", 1, 0, None, None));
    is_send(&svc.fetch_thumbnail("s", 1, 320, None));
    is_send(&svc.fetch_all("s", 1, None));
}

//! What `protocol.ts` and `mediaServer.ts` gave `<img>`, `<video>` and `<audio>`: plaintext of
//! any attachment, plain or sealed, by byte range, held to the download cap.
//!
//! * [`MediaReader`]: a seekable reader for a decoder (the future FFmpeg player through an
//!   AVIO context, the image decoder). Sequential reads stream from one open range request; a
//!   small pump task keeps up to [`READ_AHEAD_BYTES`] decoded ahead in recycled buffers, so the
//!   network keeps flowing while the decoder works; a seek outside what is buffered drops the
//!   request and opens a new one where the reader lands (a short forward seek reads through).
//!   [`MediaReader::blocking`] adapts it to `std::io::Read + Seek` for a decoder thread.
//! * [`ImageCache`]: the client's in-memory, byte-bounded keep of recently shown pictures
//!   (`imageRetention.ts`): most recently shown first within a budget, the newest always kept,
//!   concurrent fetches of one picture shared, nothing on disk and nothing across restarts
//!   (owner decision).

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::{mpsc, OnceCell};
use tokio_util::sync::CancellationToken;

use crate::{Attachments, Error};

/// Plaintext the pump keeps ahead of the reader.
pub const READ_AHEAD_BYTES: usize = 256 * 1024;
const PIECE: usize = 64 * 1024;
const PIECES: usize = READ_AHEAD_BYTES / PIECE;
/// A forward seek this short reads through instead of re-requesting.
const SKIP_THROUGH: u64 = 256 * 1024;

/// What every [`MediaReader`] of this process has pulled so far (self-tests and measurements:
/// that a player reads by range, and how much of a sealed file a seek opens). Four relaxed
/// atomic adds per 64 KiB piece; nothing reads them in the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReaderTotals {
    /// Range requests opened (the first read, and each seek outside what was buffered).
    pub ranges: u64,
    /// Plaintext bytes the pumps received from the server, read-ahead included.
    pub fetched: u64,
    /// Sealed chunks those bytes came out of (a chunk opened by two requests counts twice).
    pub chunks: u64,
    /// Plaintext bytes handed to the decoders.
    pub read: u64,
}

impl ReaderTotals {
    pub fn since(self, before: ReaderTotals) -> ReaderTotals {
        ReaderTotals {
            ranges: self.ranges - before.ranges,
            fetched: self.fetched - before.fetched,
            chunks: self.chunks - before.chunks,
            read: self.read - before.read,
        }
    }
}

static RANGES: AtomicU64 = AtomicU64::new(0);
static FETCHED: AtomicU64 = AtomicU64::new(0);
static CHUNKS: AtomicU64 = AtomicU64::new(0);
static READ: AtomicU64 = AtomicU64::new(0);

/// The totals now.
pub fn reader_totals() -> ReaderTotals {
    ReaderTotals {
        ranges: RANGES.load(Ordering::Relaxed),
        fetched: FETCHED.load(Ordering::Relaxed),
        chunks: CHUNKS.load(Ordering::Relaxed),
        read: READ.load(Ordering::Relaxed),
    }
}

struct Pump {
    rx: mpsc::Receiver<Result<Vec<u8>, Error>>,
    free: mpsc::Sender<Vec<u8>>,
    stop: CancellationToken,
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Seekable plaintext of one attachment.
pub struct MediaReader {
    svc: Arc<Attachments>,
    server_id: String,
    id: i64,
    size: Option<u64>,
    pos: u64,
    pump: Option<Pump>,
    /// The piece being read and how far into it.
    cur: Vec<u8>,
    cur_pos: usize,
    /// Bytes of the stream still to pass over before `pos` (a short forward seek):
    /// `pos` = the stream's next byte + `pending_skip`.
    pending_skip: u64,
}

impl MediaReader {
    /// A reader at byte 0. The size is learned from the key (sealed) or the first response.
    pub fn new(svc: Arc<Attachments>, server_id: &str, id: i64) -> Self {
        let size = svc.key(server_id, id).map(|k| k.size_bytes);
        Self { svc, server_id: server_id.to_string(), id, size, pos: 0, pump: None, cur: Vec::new(), cur_pos: 0, pending_skip: 0 }
    }

    /// Plaintext size, once known (sealed: at once; plain: after the first read or
    /// [`Self::probe_size`]).
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    /// Learn the size (one request) without reading.
    pub async fn probe_size(&mut self) -> Result<u64, Error> {
        if let Some(s) = self.size {
            return Ok(s);
        }
        self.ensure_pump().await?;
        self.size.ok_or(Error::Invalid("the server did not say how big the file is"))
    }

    /// Move to `pos`. Cheap: nothing is requested until the next read. A short forward move
    /// reads through what is (nearly) here; anything else re-requests from `pos`.
    pub fn seek(&mut self, pos: u64) {
        if pos == self.pos {
            return;
        }
        let stream_pos = self.pos - self.pending_skip;
        if pos >= stream_pos && pos - stream_pos <= SKIP_THROUGH && (self.pump.is_some() || self.cur_pos < self.cur.len()) {
            self.pending_skip = pos - stream_pos;
            self.pos = pos;
            return;
        }
        self.restart_at(pos);
    }

    fn restart_at(&mut self, pos: u64) {
        self.pump = None;
        self.cur.clear();
        self.cur_pos = 0;
        self.pos = pos;
        self.pending_skip = 0;
    }

    async fn ensure_pump(&mut self) -> Result<(), Error> {
        if self.pump.is_some() {
            return Ok(());
        }
        let stop = CancellationToken::new();
        let mut stream = self.svc.open(&self.server_id, self.id, self.pos, None, Some(stop.clone())).await?;
        if let Some(total) = stream.total() {
            self.size = Some(total);
        }
        let (tx, rx) = mpsc::channel::<Result<Vec<u8>, Error>>(PIECES);
        let (free_tx, mut free_rx) = mpsc::channel::<Vec<u8>>(PIECES + 2);
        for _ in 0..PIECES {
            let _ = free_tx.try_send(Vec::with_capacity(PIECE));
        }
        let stop2 = stop.clone();
        RANGES.fetch_add(1, Ordering::Relaxed);
        // For the totals: where the next piece starts and the sealed chunk last counted.
        let chunk_bytes = self.svc.key(&self.server_id, self.id).map(|k| k.chunk_bytes).filter(|c| *c > 0);
        let (mut at, mut counted) = (self.pos, None::<u64>);
        self.svc.rt.spawn(async move {
            let mut piece = match free_rx.recv().await {
                Some(p) => p,
                None => return,
            };
            loop {
                let next = tokio::select! {
                    r = stream.next() => r,
                    _ = stop2.cancelled() => return,
                };
                match next {
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        return;
                    }
                    Ok(None) => {
                        if !piece.is_empty() {
                            let _ = tx.send(Ok(piece)).await;
                        }
                        return;
                    }
                    Ok(Some(mut data)) => {
                        // Hand over what arrived at once (a decoder wants a header's first bytes
                        // without waiting for a full piece); at most PIECE bytes per piece.
                        while !data.is_empty() {
                            let n = data.len().min(PIECE - piece.len());
                            piece.extend_from_slice(&data[..n]);
                            data = &data[n..];
                            FETCHED.fetch_add(n as u64, Ordering::Relaxed);
                            if let Some(chunk) = chunk_bytes {
                                let (first, last) = (at / chunk, (at + n as u64 - 1) / chunk);
                                let first = counted.map_or(first, |c| first.max(c + 1));
                                if last >= first {
                                    CHUNKS.fetch_add(last + 1 - first, Ordering::Relaxed);
                                    counted = Some(last);
                                }
                            }
                            at += n as u64;
                            if tx.send(Ok(piece)).await.is_err() {
                                return;
                            }
                            piece = tokio::select! {
                                p = free_rx.recv() => match p { Some(p) => p, None => return },
                                _ = stop2.cancelled() => return,
                            };
                        }
                    }
                }
            }
        });
        self.pump = Some(Pump { rx, free: free_tx, stop });
        Ok(())
    }

    /// Read into `buf` from the current position; 0 at the end of the file.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if buf.is_empty() || self.size.is_some_and(|s| self.pos >= s) {
            return Ok(0);
        }
        loop {
            if self.cur_pos < self.cur.len() {
                if self.pending_skip > 0 {
                    let n = (self.pending_skip as usize).min(self.cur.len() - self.cur_pos);
                    self.cur_pos += n;
                    self.pending_skip -= n as u64;
                    continue;
                }
                let n = buf.len().min(self.cur.len() - self.cur_pos);
                buf[..n].copy_from_slice(&self.cur[self.cur_pos..self.cur_pos + n]);
                self.cur_pos += n;
                self.pos += n as u64;
                READ.fetch_add(n as u64, Ordering::Relaxed);
                return Ok(n);
            }
            if self.pump.is_none() {
                // A new request starts exactly at `pos`.
                self.pending_skip = 0;
            }
            self.ensure_pump().await?;
            let pump = self.pump.as_mut().expect("pump");
            // Hand the spent piece back for reuse.
            if self.cur.capacity() > 0 {
                let mut spent = std::mem::take(&mut self.cur);
                spent.clear();
                let _ = pump.free.try_send(spent);
            }
            self.cur_pos = 0;
            match pump.rx.recv().await {
                Some(Ok(piece)) => self.cur = piece,
                Some(Err(e)) => {
                    self.pump = None;
                    return Err(e);
                }
                None => {
                    // End of the body: the end of the file.
                    self.pump = None;
                    self.pending_skip = 0;
                    if self.size.is_none() {
                        self.size = Some(self.pos);
                    }
                    return Ok(0);
                }
            }
        }
    }

    /// A blocking `Read + Seek` view for a decoder thread (never call it from inside the
    /// runtime: it blocks on `rt`).
    pub fn blocking(self) -> BlockingMediaReader {
        let rt = self.svc.rt.clone();
        BlockingMediaReader { inner: self, rt }
    }
}

impl Attachments {
    /// Attachment `id` exactly as the server holds it, no key applied: a sealed file's
    /// ciphertext (checks that what is stored is not the plaintext). Blocks on the service's
    /// runtime: never call it from inside the runtime.
    pub fn stored_blocking(self: &Arc<Self>, server_id: &str, id: i64) -> Result<Vec<u8>, Error> {
        self.rt.block_on(async {
            let mut stream = self.open_with_key(server_id, id, None, 0, None, None).await?;
            let mut out = Vec::new();
            stream.read_to_end(&mut out).await?;
            Ok(out)
        })
    }
}

/// [`MediaReader`] as `std::io::Read + Seek`.
pub struct BlockingMediaReader {
    inner: MediaReader,
    rt: tokio::runtime::Handle,
}

impl BlockingMediaReader {
    pub fn into_inner(self) -> MediaReader {
        self.inner
    }
}

impl io::Read for BlockingMediaReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let r = &mut self.inner;
        self.rt.block_on(r.read(buf)).map_err(io::Error::other)
    }
}

impl io::Seek for BlockingMediaReader {
    fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
        let target = match to {
            io::SeekFrom::Start(p) => p as i128,
            io::SeekFrom::Current(d) => self.inner.pos as i128 + d as i128,
            io::SeekFrom::End(d) => {
                let size = match self.inner.size {
                    Some(s) => s,
                    None => {
                        let r = &mut self.inner;
                        self.rt.block_on(r.probe_size()).map_err(io::Error::other)?
                    }
                };
                size as i128 + d as i128
            }
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"));
        }
        self.inner.seek(target as u64);
        Ok(target as u64)
    }
}

/// Which bytes of an attachment a picture is.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ImageVariant {
    Original,
    /// `?variant=thumb&size=N`
    Thumbnail(u32),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImageKey {
    pub server_id: String,
    pub attachment_id: i64,
    pub variant: ImageVariant,
}

/// Encoded pictures kept in memory, most recently shown first within a byte budget
/// (`imageRetention.ts`: 128 MiB by default; the newest entry always stays).
pub struct ImageCache {
    budget: usize,
    inner: Mutex<CacheInner>,
}

#[derive(Default)]
struct CacheInner {
    entries: HashMap<ImageKey, (Bytes, u64)>,
    order: BTreeMap<u64, ImageKey>,
    tick: u64,
    total: usize,
    loading: HashMap<ImageKey, Arc<OnceCell<Result<Bytes, String>>>>,
}

impl CacheInner {
    fn touch(&mut self, key: &ImageKey) -> Option<Bytes> {
        let (bytes, t) = self.entries.get_mut(key)?;
        self.order.remove(t);
        self.tick += 1;
        *t = self.tick;
        self.order.insert(self.tick, key.clone());
        Some(bytes.clone())
    }

    fn insert(&mut self, key: ImageKey, bytes: Bytes, budget: usize) {
        if let Some((old, t)) = self.entries.remove(&key) {
            self.order.remove(&t);
            self.total -= old.len();
        }
        self.tick += 1;
        self.total += bytes.len();
        self.order.insert(self.tick, key.clone());
        self.entries.insert(key, (bytes, self.tick));
        while self.total > budget && self.entries.len() > 1 {
            let Some((_, oldest)) = self.order.pop_first() else { break };
            if let Some((b, _)) = self.entries.remove(&oldest) {
                self.total -= b.len();
            }
        }
    }
}

impl ImageCache {
    pub const DEFAULT_BUDGET: usize = 128 * 1024 * 1024;

    pub fn new(budget_bytes: usize) -> Self {
        Self { budget: budget_bytes, inner: Mutex::new(CacheInner::default()) }
    }

    /// A kept picture, marked as just shown.
    pub fn get(&self, key: &ImageKey) -> Option<Bytes> {
        self.inner.lock().unwrap().touch(key)
    }

    pub fn insert(&self, key: ImageKey, bytes: Bytes) {
        self.inner.lock().unwrap().insert(key, bytes, self.budget);
    }

    /// Bytes kept now.
    pub fn total_bytes(&self) -> usize {
        self.inner.lock().unwrap().total
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Forget everything (sign-out) or one server's pictures (left, wiped).
    pub fn clear(&self, server_id: Option<&str>) {
        let mut g = self.inner.lock().unwrap();
        match server_id {
            None => *g = CacheInner::default(),
            Some(s) => {
                let gone: Vec<ImageKey> = g.entries.keys().filter(|k| k.server_id == s).cloned().collect();
                for k in gone {
                    if let Some((b, t)) = g.entries.remove(&k) {
                        g.order.remove(&t);
                        g.total -= b.len();
                    }
                }
            }
        }
    }

    /// The picture from memory, or fetched once however many callers ask at the same time
    /// (decrypted when sealed, held to the download cap).
    pub async fn get_or_fetch(&self, svc: &Arc<Attachments>, key: ImageKey) -> Result<Bytes, Error> {
        let cell = {
            let mut g = self.inner.lock().unwrap();
            if let Some(b) = g.touch(&key) {
                return Ok(b);
            }
            g.loading.entry(key.clone()).or_insert_with(|| Arc::new(OnceCell::new())).clone()
        };
        let result = cell
            .get_or_init(|| async {
                let r = match key.variant {
                    ImageVariant::Original => svc.fetch_all(&key.server_id, key.attachment_id, None).await.map(Bytes::from),
                    ImageVariant::Thumbnail(size) => svc.fetch_thumbnail(&key.server_id, key.attachment_id, size, None).await,
                };
                r.map_err(|e| e.to_string())
            })
            .await
            .clone();
        let mut g = self.inner.lock().unwrap();
        if g.loading.get(&key).is_some_and(|c| Arc::ptr_eq(c, &cell)) {
            g.loading.remove(&key);
            if let Ok(b) = &result {
                g.insert(key, b.clone(), self.budget);
            }
        }
        result.map_err(Error::Message)
    }
}

impl Default for ImageCache {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BUDGET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(i: i64) -> ImageKey {
        ImageKey { server_id: "s".into(), attachment_id: i, variant: ImageVariant::Original }
    }

    #[test]
    fn budget_keeps_most_recent() {
        let c = ImageCache::new(100);
        c.insert(k(1), Bytes::from(vec![0u8; 40]));
        c.insert(k(2), Bytes::from(vec![0u8; 40]));
        assert!(c.get(&k(1)).is_some()); // 1 is now the most recent
        c.insert(k(3), Bytes::from(vec![0u8; 40])); // over budget: 2 goes
        assert!(c.get(&k(2)).is_none());
        assert!(c.get(&k(1)).is_some() && c.get(&k(3)).is_some());
        assert_eq!(c.total_bytes(), 80);
        c.insert(k(4), Bytes::from(vec![0u8; 500])); // bigger than the budget: kept alone
        assert_eq!(c.len(), 1);
        assert!(c.get(&k(4)).is_some());
        c.clear(Some("s"));
        assert!(c.is_empty() && c.total_bytes() == 0);
    }
}

//! Recycled buffers for whole video frames (design 2026-11-07 §6 item 6).
//!
//! The video receive path hands every reassembled, decrypted frame to the
//! [`EventSink`](crate::events::EventSink) as a [`FrameBuf`]: an owned byte
//! buffer that, when it came from a pool, returns its allocation there on
//! drop. A native client turns the process-wide pool on once at startup
//! ([`enable`]); its decoder holds the `FrameBuf` until the frame is decoded,
//! so in steady state a watched stream reuses a handful of allocations and
//! nothing between the socket and the decoder allocates.
//!
//! The pool is **off by default** (the Electron addon never enables it): every
//! `take` is then a plain `vec![0; len]` and a `FrameBuf` is a plain `Vec`,
//! which [`FrameBuf::into_vec`] gives back as the very same allocation.
//!
//! The free list is a `Mutex<Vec<Vec<u8>>>` reserved to its bound when the
//! pool is enabled, so neither a take nor a return allocates; the critical
//! section is a best-fit scan of at most `max_buffers` entries. A returned
//! buffer whose capacity is above the retained bound, or that finds the list
//! full, is freed (after the lock is released).

use std::fmt;
use std::mem::ManuallyDrop;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

/// A pool of frame-sized byte buffers. The process uses one ([`global`]);
/// tests build their own.
pub struct FramePool {
    enabled: AtomicBool,
    max_buffers: AtomicUsize,
    max_retained: AtomicUsize,
    free: Mutex<Vec<Vec<u8>>>,
    takes: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
    returned: AtomicU64,
    dropped: AtomicU64,
}

/// What a pool did so far. A disabled pool counts nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Buffers handed out.
    pub takes: u64,
    /// Takes served from the free list.
    pub hits: u64,
    /// Takes that allocated (nothing on the list was big enough).
    pub misses: u64,
    /// Buffers put back on the free list.
    pub returned: u64,
    /// Buffers freed instead: the list was full or the buffer above the
    /// retained-capacity bound.
    pub dropped: u64,
    /// Buffers on the free list now.
    pub idle: usize,
}

impl FramePool {
    /// A pool that is off: every take allocates, nothing is kept.
    pub const fn disabled() -> Self {
        FramePool {
            enabled: AtomicBool::new(false),
            max_buffers: AtomicUsize::new(0),
            max_retained: AtomicUsize::new(0),
            free: Mutex::new(Vec::new()),
            takes: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            returned: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// An enabled pool keeping at most `max_buffers` idle buffers of at most
    /// `max_retained_capacity_bytes` each.
    pub fn new(max_buffers: usize, max_retained_capacity_bytes: usize) -> Self {
        let pool = Self::disabled();
        pool.enable(max_buffers, max_retained_capacity_bytes);
        pool
    }

    /// Turn the pool on. Only the first call counts (returns false after).
    pub fn enable(&self, max_buffers: usize, max_retained_capacity_bytes: usize) -> bool {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        if self.enabled.load(Ordering::Acquire) {
            return false;
        }
        // Reserved once, so a return never grows the list.
        free.reserve_exact(max_buffers);
        self.max_buffers.store(max_buffers, Ordering::Relaxed);
        self.max_retained.store(max_retained_capacity_bytes, Ordering::Relaxed);
        self.enabled.store(true, Ordering::Release);
        true
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    /// A buffer of `len` bytes (contents unspecified). Off: `vec![0; len]`.
    pub fn take(&'static self, len: usize) -> FrameBuf {
        if !self.is_enabled() {
            return FrameBuf::from(vec![0u8; len]);
        }
        self.takes.fetch_add(1, Ordering::Relaxed);
        let found = {
            let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
            // Best fit: the smallest buffer that holds `len`, so a delta
            // frame doesn't take the one a keyframe needs.
            let mut best: Option<(usize, usize)> = None;
            for (i, v) in free.iter().enumerate() {
                let cap = v.capacity();
                if cap >= len && best.is_none_or(|(_, c)| cap < c) {
                    best = Some((i, cap));
                }
            }
            best.map(|(i, _)| free.swap_remove(i))
        };
        let vec = match found {
            Some(mut v) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                // Pooled vectors keep the length they were filled to (their
                // bytes stay initialised); this only zero-fills a shortfall,
                // within capacity, never reallocating.
                if v.len() < len {
                    v.resize(len, 0);
                }
                v
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                vec![0u8; len]
            }
        };
        FrameBuf { vec, start: 0, end: len, pool: Some(self) }
    }

    /// A buffer holding a copy of `src`. Off: `src.to_vec()`.
    pub fn take_copy(&'static self, src: &[u8]) -> FrameBuf {
        if !self.is_enabled() {
            return FrameBuf::from(src.to_vec());
        }
        let mut buf = self.take(src.len());
        buf.copy_from_slice(src);
        buf
    }

    pub fn stats(&self) -> PoolStats {
        let idle = self.free.lock().map(|f| f.len()).unwrap_or(0);
        PoolStats {
            takes: self.takes.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            returned: self.returned.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            idle,
        }
    }

    /// A pooled `FrameBuf` was dropped: keep its allocation or free it.
    fn give_back(&self, vec: Vec<u8>) {
        if vec.capacity() > self.max_retained.load(Ordering::Relaxed) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let overflow = {
            let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
            if free.len() < self.max_buffers.load(Ordering::Relaxed) {
                free.push(vec);
                None
            } else {
                Some(vec)
            }
        };
        if overflow.is_some() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        } else {
            self.returned.fetch_add(1, Ordering::Relaxed);
        }
        // `overflow` is freed here, outside the lock.
    }
}

static GLOBAL: FramePool = FramePool::disabled();

/// Turn the process-wide pool on, once, before the first voice session (a
/// native client does this at startup; the Electron addon never does). Later
/// calls change nothing and return false.
pub fn enable(max_buffers: usize, max_retained_capacity_bytes: usize) -> bool {
    GLOBAL.enable(max_buffers, max_retained_capacity_bytes)
}

/// The process-wide pool (off unless [`enable`]d).
pub fn global() -> &'static FramePool {
    &GLOBAL
}

/// [`FramePool::take`] on the process-wide pool.
pub fn take(len: usize) -> FrameBuf {
    GLOBAL.take(len)
}

/// [`FramePool::take_copy`] on the process-wide pool.
pub fn take_copy(src: &[u8]) -> FrameBuf {
    GLOBAL.take_copy(src)
}

/// The process-wide pool's counters.
pub fn stats() -> PoolStats {
    GLOBAL.stats()
}

/// An owned frame buffer: the bytes are `self[..]`. From a pool, it returns
/// its allocation there on drop; otherwise it is a plain `Vec` (and
/// [`into_vec`](Self::into_vec) hands back that same allocation).
///
/// A pooled buffer may start past the front of its allocation (an in-place
/// decrypted frame drops its seal header with [`advance`](Self::advance),
/// moving nothing). An unpooled one never does: `advance` moves its bytes
/// down as `Vec::drain` would, so `into_vec` never copies it.
pub struct FrameBuf {
    /// `vec[..]` is initialised; the frame is `vec[start..end]`.
    vec: Vec<u8>,
    start: usize,
    end: usize,
    pool: Option<&'static FramePool>,
}

impl FrameBuf {
    /// An empty, unpooled buffer.
    pub const fn new() -> Self {
        FrameBuf { vec: Vec::new(), start: 0, end: 0, pool: None }
    }

    pub fn len(&self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Whether the allocation goes back to a pool on drop.
    pub fn is_pooled(&self) -> bool {
        self.pool.is_some()
    }

    /// The bytes as a `Vec`, detached from any pool. Unpooled (or pooled and
    /// starting at the front): the same allocation, no copy. A pooled buffer
    /// past its front moves its bytes down once, within the allocation.
    pub fn into_vec(self) -> Vec<u8> {
        let mut this = ManuallyDrop::new(self);
        let mut vec = std::mem::take(&mut this.vec);
        vec.truncate(this.end);
        if this.start > 0 {
            vec.drain(..this.start);
        }
        vec
    }

    /// Keep the first `len` bytes.
    pub fn truncate(&mut self, len: usize) {
        if len < self.len() {
            self.end = self.start + len;
        }
    }

    /// Drop the first `n` bytes.
    pub fn advance(&mut self, n: usize) {
        let n = n.min(self.len());
        if self.pool.is_some() {
            self.start += n;
        } else {
            // Unpooled buffers stay plain Vecs starting at the front.
            self.vec.truncate(self.end);
            self.vec.drain(..n);
            self.end -= n;
        }
    }
}

impl Default for FrameBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<u8>> for FrameBuf {
    fn from(vec: Vec<u8>) -> Self {
        let end = vec.len();
        FrameBuf { vec, start: 0, end, pool: None }
    }
}

impl Drop for FrameBuf {
    fn drop(&mut self) {
        if let Some(pool) = self.pool {
            pool.give_back(std::mem::take(&mut self.vec));
        }
    }
}

impl Deref for FrameBuf {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.vec[self.start..self.end]
    }
}

impl DerefMut for FrameBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.vec[self.start..self.end]
    }
}

impl AsRef<[u8]> for FrameBuf {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl AsMut<[u8]> for FrameBuf {
    fn as_mut(&mut self) -> &mut [u8] {
        self
    }
}

/// An unpooled copy.
impl Clone for FrameBuf {
    fn clone(&self) -> Self {
        FrameBuf::from(self.to_vec())
    }
}

impl fmt::Debug for FrameBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameBuf")
            .field("len", &self.len())
            .field("pooled", &self.is_pooled())
            .finish()
    }
}

impl PartialEq for FrameBuf {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl Eq for FrameBuf {}

impl PartialEq<[u8]> for FrameBuf {
    fn eq(&self, other: &[u8]) -> bool {
        **self == *other
    }
}

impl PartialEq<&[u8]> for FrameBuf {
    fn eq(&self, other: &&[u8]) -> bool {
        **self == **other
    }
}

impl<const N: usize> PartialEq<[u8; N]> for FrameBuf {
    fn eq(&self, other: &[u8; N]) -> bool {
        **self == other[..]
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for FrameBuf {
    fn eq(&self, other: &&[u8; N]) -> bool {
        **self == other[..]
    }
}

impl PartialEq<Vec<u8>> for FrameBuf {
    fn eq(&self, other: &Vec<u8>) -> bool {
        **self == other[..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(max_buffers: usize, max_retained: usize) -> &'static FramePool {
        Box::leak(Box::new(FramePool::new(max_buffers, max_retained)))
    }

    #[test]
    fn a_dropped_buffer_returns_and_is_reused() {
        let p = pool(4, 1 << 20);
        let a = p.take(1000);
        assert!(a.is_pooled());
        assert_eq!(a.len(), 1000);
        let ptr = a.as_ptr();
        drop(a);
        assert_eq!(p.stats(), PoolStats { takes: 1, misses: 1, returned: 1, idle: 1, ..Default::default() });
        // A smaller frame reuses the same allocation.
        let b = p.take(600);
        assert_eq!(b.as_ptr(), ptr);
        assert_eq!(b.len(), 600);
        assert_eq!(p.stats().hits, 1);
        drop(b);
        // ...and a bigger one than anything idle allocates.
        let c = p.take(4000);
        assert_ne!(c.as_ptr(), ptr);
        assert_eq!(p.stats().misses, 2);
    }

    #[test]
    fn best_fit_keeps_the_big_buffer_for_the_big_frame() {
        let p = pool(4, 1 << 20);
        let (big, small) = (p.take(100_000), p.take(10_000));
        let (big_ptr, small_ptr) = (big.as_ptr(), small.as_ptr());
        drop(big);
        drop(small);
        let delta = p.take(8_000);
        assert_eq!(delta.as_ptr(), small_ptr, "a delta takes the smallest buffer that fits");
        let key = p.take(90_000);
        assert_eq!(key.as_ptr(), big_ptr);
    }

    #[test]
    fn into_vec_detaches_without_copying() {
        let p = pool(4, 1 << 20);
        let mut a = p.take(64);
        a.copy_from_slice(&[7u8; 64]);
        let ptr = a.as_ptr();
        let v = a.into_vec();
        assert_eq!(v.as_ptr(), ptr);
        assert_eq!(v, vec![7u8; 64]);
        drop(v);
        let s = p.stats();
        assert_eq!((s.returned, s.dropped, s.idle), (0, 0, 0), "a detached buffer never comes back");

        // Unpooled: the very Vec it was made from.
        let v = vec![1u8, 2, 3, 4];
        let ptr = v.as_ptr();
        let b = FrameBuf::from(v);
        assert!(!b.is_pooled());
        let v = b.into_vec();
        assert_eq!(v.as_ptr(), ptr);
    }

    #[test]
    fn advance_and_truncate_view_the_same_bytes_pooled_or_not() {
        let p = pool(4, 1 << 20);
        let data: Vec<u8> = (0..100u8).collect();
        let mut pooled = p.take_copy(&data);
        let mut plain = FrameBuf::from(data.clone());
        let pooled_ptr = pooled.as_ptr();
        let plain_ptr = plain.as_ptr();
        for b in [&mut pooled, &mut plain] {
            b.truncate(90);
            b.advance(8);
            assert_eq!(b, &data[8..90]);
        }
        // Pooled: a view into the same allocation. Unpooled: bytes moved to
        // the front, so into_vec is still free.
        assert_eq!(pooled.as_ptr(), unsafe { pooled_ptr.add(8) });
        assert_eq!(plain.as_ptr(), plain_ptr);
        let v = plain.into_vec();
        assert_eq!(v.as_ptr(), plain_ptr);
        assert_eq!(v, data[8..90]);
        assert_eq!(pooled.into_vec(), data[8..90]);
        // Past the end clamps.
        let mut c = FrameBuf::from(vec![1u8, 2, 3]);
        c.advance(10);
        assert!(c.is_empty());
        c.truncate(5);
        assert!(c.is_empty());
    }

    #[test]
    fn bounds_free_what_the_pool_would_not_keep() {
        let p = pool(2, 10_000);
        let bufs: Vec<FrameBuf> = (0..3).map(|_| p.take(1000)).collect();
        drop(bufs);
        let s = p.stats();
        assert_eq!((s.returned, s.dropped, s.idle), (2, 1, 2), "the list holds max_buffers");
        // Above the retained-capacity bound: freed on return.
        drop(p.take(20_000));
        let s = p.stats();
        assert_eq!((s.dropped, s.idle), (2, 2));
    }

    #[test]
    fn a_disabled_pool_hands_out_plain_vectors_and_counts_nothing() {
        static OFF: FramePool = FramePool::disabled();
        let a = OFF.take(32);
        assert!(!a.is_pooled());
        assert_eq!(&a[..], &[0u8; 32][..]);
        let b = OFF.take_copy(b"abc");
        assert!(!b.is_pooled());
        assert_eq!(b, b"abc");
        drop((a, b));
        assert_eq!(OFF.stats(), PoolStats::default());
        // Enabling is once only.
        let p = pool(1, 100);
        assert!(!p.enable(8, 1 << 20));
    }

    #[test]
    fn concurrent_takes_and_drops_balance() {
        let p = pool(8, 1 << 20);
        let threads: Vec<_> = (0..6)
            .map(|t| {
                std::thread::spawn(move || {
                    let mut held = Vec::new();
                    for i in 0..2000usize {
                        let mut b = p.take(100 + (i * 37 + t) % 5000);
                        b[0] = t as u8;
                        // Hand some across threads' lifetimes, drop others at once.
                        if i % 3 == 0 {
                            held.push(b);
                        }
                        if held.len() > 4 {
                            held.remove(0);
                        }
                    }
                    held
                })
            })
            .collect();
        // The survivors are dropped here, on another thread than took them.
        let survivors: Vec<FrameBuf> = threads.into_iter().flat_map(|h| h.join().unwrap()).collect();
        drop(survivors);
        let s = p.stats();
        assert_eq!(s.takes, 6 * 2000);
        assert_eq!(s.takes, s.hits + s.misses);
        assert_eq!(s.returned + s.dropped, s.takes, "every pooled buffer came back or was freed");
        assert!(s.idle <= 8);
        assert_eq!(s.idle as u64, s.returned - s.hits, "idle = returned - reused");
    }
}

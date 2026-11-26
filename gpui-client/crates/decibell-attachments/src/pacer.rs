//! Speed caps (Settings → Network), the token bucket of `uploadPacing.ts` and
//! `downloadPacer.ts`: a non-zero cap is floored at 100 KiB/s (the community server cuts a
//! connection after 30 s without progress), an idle bucket banks at most 0.1 s of credit, a
//! charge may overdraw and the next one waits out the debt, sleeps are at most 250 ms so a
//! changed cap applies promptly, and the cap is re-read on every charge. Bytes are charged in
//! 64 KiB slices (main's gotcha: bodies arrive in big chunks; charging them whole gives
//! bursts and gaps). Uploads have one bucket per file; downloads one per (server, attachment),
//! shared by every transfer of that file, pruned when idle (no timer: on the next lookup).
//! Nothing here sleeps unless a transfer is waiting on its cap.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

pub const MIN_LIMIT_BPS: u64 = 100 * 1024;
const BURST_SECONDS: f64 = 0.1;
const MAX_SLEEP: Duration = Duration::from_millis(250);
const IDLE_PRUNE: Duration = Duration::from_secs(30);
/// Pacing granularity.
pub const SLICE_BYTES: usize = 64 * 1024;

/// The two caps, in bytes per second (0 = unlimited). Shared by every transfer; changing one
/// applies to running transfers within a slice.
#[derive(Debug, Default)]
pub struct Limits {
    upload: AtomicU64,
    download: AtomicU64,
}

impl Limits {
    pub fn set_upload(&self, bps: u64) {
        self.upload.store(bps, Ordering::Relaxed);
    }
    pub fn set_download(&self, bps: u64) {
        self.download.store(bps, Ordering::Relaxed);
    }
    /// The upload cap in force (floor applied), 0 = unlimited.
    pub fn upload_rate(&self) -> u64 {
        effective(self.upload.load(Ordering::Relaxed))
    }
    /// The download cap in force (floor applied), 0 = unlimited.
    pub fn download_rate(&self) -> u64 {
        effective(self.download.load(Ordering::Relaxed))
    }
}

fn effective(limit: u64) -> u64 {
    if limit > 0 {
        limit.max(MIN_LIMIT_BPS)
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Up,
    Down,
}

/// One token bucket.
#[derive(Debug)]
pub(crate) struct Pacer {
    tokens: f64,
    stamp: Instant,
}

impl Pacer {
    pub fn new() -> Self {
        Self { tokens: 0.0, stamp: Instant::now() }
    }

    /// Charge `bytes`, sleeping while over the cap. `false` when `cancel` fired (nothing more
    /// is charged then).
    pub async fn take(&mut self, bytes: usize, limits: &Limits, dir: Direction, cancel: Option<&CancellationToken>) -> bool {
        let mut left = bytes;
        while left > 0 {
            if cancel.is_some_and(|c| c.is_cancelled()) {
                return false;
            }
            let rate = match dir {
                Direction::Up => limits.upload_rate(),
                Direction::Down => limits.download_rate(),
            } as f64;
            let now = Instant::now();
            if rate == 0.0 {
                self.tokens = 0.0;
                self.stamp = now;
                return true;
            }
            let elapsed = now.duration_since(self.stamp).as_secs_f64();
            self.tokens = (rate * BURST_SECONDS).min(self.tokens + elapsed * rate);
            self.stamp = now;
            if self.tokens >= 0.0 {
                let n = left.min(SLICE_BYTES);
                self.tokens -= n as f64;
                left -= n;
                continue;
            }
            let wait = Duration::from_secs_f64(-self.tokens / rate).min(MAX_SLEEP);
            match cancel {
                Some(c) => {
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = c.cancelled() => return false,
                    }
                }
                None => tokio::time::sleep(wait).await,
            }
        }
        true
    }

    fn idle_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.stamp)
    }
}

/// A bucket shared by the transfers of one file.
pub(crate) type SharedPacer = Arc<tokio::sync::Mutex<Pacer>>;

/// Download buckets per (server, attachment).
#[derive(Debug, Default)]
pub(crate) struct DownloadPacers {
    map: Mutex<HashMap<(String, i64), SharedPacer>>,
}

impl DownloadPacers {
    pub fn get(&self, server_id: &str, attachment_id: i64) -> SharedPacer {
        let now = Instant::now();
        let mut map = self.map.lock().unwrap();
        // A bucket untouched for 30 s is equivalent to a fresh one; drop those nobody holds.
        map.retain(|_, p| {
            Arc::strong_count(p) > 1 || p.try_lock().map(|g| g.idle_for(now) < IDLE_PRUNE).unwrap_or(true)
        });
        map.entry((server_id.to_string(), attachment_id))
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(Pacer::new())))
            .clone()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
}

/// Bytes per upload PATCH (`uploadChunkBytes`): 8 MiB uncapped, otherwise about half a second
/// of the cap in whole sealed chunks (so pause bites within a chunk and progress keeps moving).
pub fn upload_chunk_bytes(rate: u64) -> u64 {
    const MAX: u64 = 8 * 1024 * 1024;
    let c = crate::crypto::CHUNK_BYTES;
    if rate == 0 {
        return MAX;
    }
    let half = (rate / 2 / c) * c;
    half.max(c).min(MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_sizes() {
        assert_eq!(upload_chunk_bytes(0), 8 * 1024 * 1024);
        assert_eq!(upload_chunk_bytes(MIN_LIMIT_BPS), 64 * 1024);
        assert_eq!(upload_chunk_bytes(1024 * 1024), 512 * 1024);
        assert_eq!(upload_chunk_bytes(1 << 30), 8 * 1024 * 1024);
        assert_eq!(effective(1), MIN_LIMIT_BPS);
    }

    #[tokio::test]
    async fn pacer_holds_the_rate() {
        let limits = Limits::default();
        limits.set_download(400 * 1024);
        let mut p = Pacer::new();
        let t = Instant::now();
        // 400 KiB at 400 KiB/s with no banked credit: about a second.
        assert!(p.take(400 * 1024, &limits, Direction::Down, None).await);
        let s = t.elapsed().as_secs_f64();
        assert!((0.8..1.3).contains(&s), "took {s}");
        // Lifting the cap releases immediately.
        limits.set_download(0);
        let t = Instant::now();
        assert!(p.take(10 << 20, &limits, Direction::Down, None).await);
        assert!(t.elapsed() < Duration::from_millis(20));
    }

    #[tokio::test]
    async fn pacer_cancels() {
        let limits = Limits::default();
        limits.set_upload(MIN_LIMIT_BPS);
        let mut p = Pacer::new();
        let c = CancellationToken::new();
        let c2 = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c2.cancel();
        });
        let t = Instant::now();
        assert!(!p.take(10 << 20, &limits, Direction::Up, Some(&c)).await);
        assert!(t.elapsed() < Duration::from_millis(400));
    }

    #[test]
    fn idle_buckets_are_pruned() {
        let ps = DownloadPacers::default();
        let a = ps.get("s", 1);
        a.try_lock().unwrap().stamp = Instant::now() - Duration::from_secs(60);
        drop(a);
        let _b = ps.get("s", 2);
        assert_eq!(ps.len(), 1);
    }
}

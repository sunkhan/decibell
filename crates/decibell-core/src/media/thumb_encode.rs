//! Stream thumbnail encoding shared by the native capture paths (Windows
//! `encoder_thread` + `thumbnail`, Linux `encoder_thread_linux`).
//!
//! The encoder threads only hand over raw pixels (`ThumbnailJob`); the
//! box downscale to THUMBNAIL_MAX_EDGE and the JPEG encode run on a
//! separate worker thread, so a 960 px thumbnail never costs the stream a
//! frame. Keep the numbers in step with the renderer path
//! (`features/voice/thumbnailConfig.ts`) and the community server's
//! `MAX_STREAM_THUMB_BYTES` (src/community/main.cpp).

#![cfg(any(target_os = "linux", target_os = "windows"))]

use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder};

/// Longest edge of a thumbnail, px. Stream tiles in the voice view reach
/// ~960 px, so this is what they display 1:1 on a non-HiDPI screen.
pub const THUMBNAIL_MAX_EDGE: u32 = 960;
/// The community server drops bigger thumbnails silently.
pub const MAX_THUMBNAIL_BYTES: usize = 256 * 1024;
/// JPEG qualities tried in order until one fits MAX_THUMBNAIL_BYTES.
const QUALITY_LADDER: [u8; 3] = [80, 65, 50];
/// Steady-state cadence.
pub const THUMBNAIL_INTERVAL: Duration = Duration::from_secs(15);
/// The first thumbnail is taken on the first frame, which is often black
/// or half-drawn while capture warms up; a second follows this soon after
/// instead of a full interval later.
pub const THUMBNAIL_FOLLOWUP: Duration = Duration::from_secs(2);

/// When the next thumbnail is due: immediately, then after
/// THUMBNAIL_FOLLOWUP, then every THUMBNAIL_INTERVAL.
pub struct ThumbnailSchedule {
    next: Option<Instant>,
    taken: u32,
}

impl ThumbnailSchedule {
    pub fn new() -> Self {
        Self { next: None, taken: 0 }
    }

    pub fn due(&self, now: Instant) -> bool {
        self.next.is_none_or(|t| now >= t)
    }

    /// Record that a thumbnail was started at `now`.
    pub fn taken(&mut self, now: Instant) {
        self.taken += 1;
        let wait = if self.taken == 1 { THUMBNAIL_FOLLOWUP } else { THUMBNAIL_INTERVAL };
        self.next = Some(now + wait);
    }
}

impl Default for ThumbnailSchedule {
    fn default() -> Self {
        Self::new()
    }
}

/// Raw source pixels for one thumbnail, already copied out of the
/// capture / staging memory. 4 bytes per pixel, `stride` bytes per row.
pub struct ThumbnailJob {
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    /// `Bgra` or `Rgba`; the downscale is channel-order agnostic.
    pub color: ColorType,
}

/// Longest edge clamped to THUMBNAIL_MAX_EDGE, aspect ratio preserved.
pub fn thumbnail_target_size(src_w: u32, src_h: u32) -> (u32, u32) {
    if src_w >= src_h {
        let target_w = src_w.min(THUMBNAIL_MAX_EDGE);
        let target_h = ((target_w as u64) * (src_h as u64) / (src_w.max(1) as u64)) as u32;
        (target_w.max(1), target_h.max(1))
    } else {
        let target_h = src_h.min(THUMBNAIL_MAX_EDGE);
        let target_w = ((target_h as u64) * (src_w as u64) / (src_h.max(1) as u64)) as u32;
        (target_w.max(1), target_h.max(1))
    }
}

/// Area-average (box) downscale of 4-byte pixels with `src_pitch` stride
/// into a tightly packed `dst_w × dst_h` buffer. Reads only `src_w * 4`
/// bytes per row and never indexes out of bounds.
pub fn area_downscale_4bpp(
    src: &[u8],
    src_w: usize,
    src_h: usize,
    src_pitch: usize,
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return;
    }
    for dy in 0..dst_h {
        let y0 = dy * src_h / dst_h;
        let y1 = ((dy + 1) * src_h / dst_h).max(y0 + 1).min(src_h);
        for dx in 0..dst_w {
            let x0 = dx * src_w / dst_w;
            let x1 = ((dx + 1) * src_w / dst_w).max(x0 + 1).min(src_w);
            let mut acc = [0u32; 4];
            let mut n = 0u32;
            for y in y0..y1 {
                let row = y * src_pitch;
                let Some(px_row) = src.get(row + x0 * 4..row + x1 * 4) else {
                    continue;
                };
                for px in px_row.chunks_exact(4) {
                    acc[0] += px[0] as u32;
                    acc[1] += px[1] as u32;
                    acc[2] += px[2] as u32;
                    acc[3] += px[3] as u32;
                    n += 1;
                }
            }
            let d = (dy * dst_w + dx) * 4;
            if let (Some(out), true) = (dst.get_mut(d..d + 4), n > 0) {
                for (o, a) in out.iter_mut().zip(acc) {
                    *o = ((a + n / 2) / n) as u8;
                }
            }
        }
    }
}

/// JPEG-encode tightly packed pixels, stepping quality down until the
/// result fits MAX_THUMBNAIL_BYTES.
pub fn encode_jpeg_capped(pixels: &[u8], w: u16, h: u16, color: ColorType) -> Result<Vec<u8>, String> {
    let mut last_len = 0;
    for quality in QUALITY_LADDER {
        let mut out = Vec::with_capacity(64 * 1024);
        Encoder::new(&mut out, quality)
            .encode(pixels, w, h, color)
            .map_err(|e| format!("JPEG encode: {e:?}"))?;
        if out.len() <= MAX_THUMBNAIL_BYTES {
            return Ok(out);
        }
        last_len = out.len();
    }
    Err(format!(
        "thumbnail still {last_len} B at the lowest quality (cap {MAX_THUMBNAIL_BYTES} B)"
    ))
}

/// Downscale + encode one job.
pub fn render_thumbnail(job: &ThumbnailJob) -> Result<Vec<u8>, String> {
    if job.width == 0 || job.height == 0 {
        return Err("empty thumbnail source".to_string());
    }
    let (tw, th) = thumbnail_target_size(job.width as u32, job.height as u32);
    let (tw, th) = (tw as usize, th as usize);
    if tw == job.width && th == job.height && job.stride == job.width * 4 {
        return encode_jpeg_capped(&job.pixels, tw as u16, th as u16, job.color);
    }
    let mut packed = vec![0u8; tw * th * 4];
    area_downscale_4bpp(&job.pixels, job.width, job.height, job.stride, &mut packed, tw, th);
    encode_jpeg_capped(&packed, tw as u16, th as u16, job.color)
}

/// Owns the background thread that renders thumbnails and pushes the JPEGs
/// to the network sender. Dropping it closes the job channel; the thread
/// finishes the job in hand (if any) and exits on its own — never joined,
/// so encoder teardown doesn't wait on a JPEG encode.
pub struct ThumbnailWorker {
    tx: Option<SyncSender<ThumbnailJob>>,
}

impl ThumbnailWorker {
    pub fn spawn(out: tokio::sync::mpsc::Sender<Vec<u8>>) -> Self {
        // Depth 1: at a 15 s cadence a second job only queues if an encode
        // is somehow still running, and then the older one is the one we
        // want gone.
        let (tx, rx) = mpsc::sync_channel::<ThumbnailJob>(1);
        let spawned = std::thread::Builder::new()
            .name("decibell-thumbnail".to_string())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    match render_thumbnail(&job) {
                        // try_send: the network sender may still be busy
                        // with the previous one; dropping a thumbnail beats
                        // queueing them.
                        Ok(jpeg) => {
                            let _ = out.try_send(jpeg);
                        }
                        Err(e) => log::warn!("[thumbnail] {e}"),
                    }
                }
            });
        match spawned {
            Ok(_) => Self { tx: Some(tx) },
            Err(e) => {
                log::warn!("[thumbnail] worker thread failed to start: {e}");
                Self { tx: None }
            }
        }
    }

    /// Hand a job to the worker. Returns false (and drops the job) when the
    /// worker is busy or gone.
    pub fn submit(&self, job: ThumbnailJob) -> bool {
        match &self.tx {
            Some(tx) => match tx.try_send(job) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
            },
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_size_landscape() {
        assert_eq!(thumbnail_target_size(1920, 1080), (960, 540));
        assert_eq!(thumbnail_target_size(3840, 2160), (960, 540));
        assert_eq!(thumbnail_target_size(2560, 1080), (960, 405));
    }

    #[test]
    fn target_size_portrait_square_small() {
        assert_eq!(thumbnail_target_size(1080, 1920), (540, 960));
        assert_eq!(thumbnail_target_size(1000, 1000), (960, 960));
        assert_eq!(thumbnail_target_size(800, 600), (800, 600));
    }

    #[test]
    fn area_downscale_averages_and_respects_pitch() {
        // 4×2 source, pitch 20 (4 bytes padding, absent on the last row).
        let mut src = vec![0u8; 20 + 16];
        for x in 0..4 {
            let v = (x * 40) as u8;
            for c in 0..4 {
                src[x * 4 + c] = v;
                src[20 + x * 4 + c] = v + 10;
            }
        }
        let mut dst = vec![0u8; 2 * 4];
        area_downscale_4bpp(&src, 4, 2, 20, &mut dst, 2, 1);
        // Left: mean of 0, 40, 10, 50 = 25; right: mean of 80, 120, 90, 130 = 105.
        assert_eq!(&dst[0..4], &[25, 25, 25, 25]);
        assert_eq!(&dst[4..8], &[105, 105, 105, 105]);
    }

    #[test]
    fn area_downscale_never_reads_past_a_short_buffer() {
        // Claims 4×4 but only holds 2 full rows: must not panic.
        let src = vec![200u8; 4 * 4 * 2];
        let mut dst = vec![0u8; 2 * 2 * 4];
        area_downscale_4bpp(&src, 4, 4, 16, &mut dst, 2, 2);
        assert_eq!(dst[0], 200);
    }

    #[test]
    fn render_scales_1080p_to_960_and_fits_the_cap() {
        // Worst case for JPEG: per-pixel noise.
        let (w, h) = (1920usize, 1080usize);
        let mut seed = 0x1234_5678u32;
        let pixels: Vec<u8> = (0..w * h * 4)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let job = ThumbnailJob { pixels, width: w, height: h, stride: w * 4, color: ColorType::Bgra };
        let jpeg = render_thumbnail(&job).expect("noise still fits at the lowest quality");
        assert!(jpeg.len() <= MAX_THUMBNAIL_BYTES, "{} B", jpeg.len());
        // SOF0 carries height then width.
        let sof = jpeg.windows(2).position(|p| p == [0xFF, 0xC0]).expect("baseline SOF");
        let height = u16::from_be_bytes([jpeg[sof + 5], jpeg[sof + 6]]);
        let width = u16::from_be_bytes([jpeg[sof + 7], jpeg[sof + 8]]);
        assert_eq!((width, height), (960, 540));
    }

    #[test]
    fn schedule_is_now_then_followup_then_interval() {
        let t0 = Instant::now();
        let mut s = ThumbnailSchedule::new();
        assert!(s.due(t0));
        s.taken(t0);
        assert!(!s.due(t0 + Duration::from_millis(1999)));
        assert!(s.due(t0 + THUMBNAIL_FOLLOWUP));
        let t1 = t0 + THUMBNAIL_FOLLOWUP;
        s.taken(t1);
        assert!(!s.due(t1 + Duration::from_secs(14)));
        assert!(s.due(t1 + THUMBNAIL_INTERVAL));
        let t2 = t1 + THUMBNAIL_INTERVAL;
        s.taken(t2);
        assert!(!s.due(t2 + Duration::from_secs(14)));
        assert!(s.due(t2 + THUMBNAIL_INTERVAL));
    }

    #[test]
    fn worker_delivers_a_jpeg() {
        let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let worker = ThumbnailWorker::spawn(out_tx);
        let job = ThumbnailJob {
            pixels: vec![128u8; 64 * 36 * 4],
            width: 64,
            height: 36,
            stride: 64 * 4,
            color: ColorType::Rgba,
        };
        assert!(worker.submit(job));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(jpeg) = out_rx.try_recv() {
                assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
                break;
            }
            assert!(Instant::now() < deadline, "worker never produced a thumbnail");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

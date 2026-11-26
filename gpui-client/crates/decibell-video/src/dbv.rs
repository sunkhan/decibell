//! `.dbv` test streams: whole encoded frames as the receive path hands them on.
//!
//! The format of the Qt spike's `qt-client/src/video/streamfile.h` (files are interchangeable),
//! little-endian:
//!
//! - **DBV1** (`tools/streamgen`, synthetic): `"DBV1"`, u8 codec (1 H.264, 2 HEVC, 3 AV1),
//!   3 zero bytes, u32 width, u32 height, u32 fps; per frame u32 size, u32 flags (bit 0:
//!   keyframe), the bytes. Played at `fps`. Parameter sets are in the bitstream.
//! - **DBV2** (a real stream recorded by the core's `stream-dump` feature): `"DBV2"`, u8 wire
//!   codec (1 / 2 H.264, 3 H.265, 4 AV1), 15 zero bytes; per frame u32 size, u32 flags (bit 0
//!   keyframe, bit 1 a description follows), u64 arrival µs, [u32 size, description], the
//!   bytes. Played on its recorded arrival times.
//!
//! [`Feeder`] replays a file into a [`VideoHub`] at its timing, standing in for the core's
//! receive thread: the test screen and the self-tests are fed by it.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use parking_lot::Mutex;

use crate::hub::{EncodedFrame, VideoHub};
use crate::packet::{PADDING, Packet, PacketBytes};

pub struct FrameHeader {
    pub size: u32,
    pub keyframe: bool,
    /// The file started over at this frame.
    pub wrapped: bool,
    /// DBV2: arrival time since the first frame. DBV1: `None` (play at `fps`).
    pub at_us: Option<u64>,
    /// DBV2: the decoder configuration record sent with this frame.
    pub description: Option<Vec<u8>>,
}

pub struct StreamFile {
    file: BufReader<File>,
    pub version: u8,
    /// The wire's `VideoCodec` byte.
    pub codec: u8,
    /// DBV2: all 0 (the bitstream says; arrival times pace it).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

const HEADER_BYTES: u64 = 20;

impl StreamFile {
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = BufReader::with_capacity(
            1 << 20,
            File::open(path).with_context(|| format!("opening {}", path.display()))?,
        );
        let mut head = [0u8; HEADER_BYTES as usize];
        file.read_exact(&mut head)?;
        ensure!(
            &head[..3] == b"DBV" && (head[3] == b'1' || head[3] == b'2'),
            "{}: not a .dbv file",
            path.display()
        );
        let version = head[3] - b'0';
        let codec = match (version, head[4]) {
            (1, 1) => 1,
            (1, 2) => 3,
            (1, 3) => 4,
            (2, codec @ 1..=4) => codec,
            _ => bail!("{}: unknown codec id {}", path.display(), head[4]),
        };
        let word = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
        Ok(Self {
            file,
            version,
            codec,
            width: word(8),
            height: word(12),
            fps: word(16),
        })
    }

    fn read_header(&mut self) -> Option<FrameHeader> {
        let mut head = [0u8; 8];
        self.file.read_exact(&mut head).ok()?;
        let size = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
        let flags = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let mut at_us = None;
        let mut description = None;
        if self.version == 2 {
            let mut at = [0u8; 8];
            self.file.read_exact(&mut at).ok()?;
            at_us = Some(u64::from_le_bytes(at));
            if flags & 2 != 0 {
                let mut len = [0u8; 4];
                self.file.read_exact(&mut len).ok()?;
                let mut bytes = vec![0; u32::from_le_bytes(len) as usize];
                self.file.read_exact(&mut bytes).ok()?;
                description = Some(bytes);
            }
        }
        Some(FrameHeader {
            size,
            keyframe: flags & 1 != 0,
            wrapped: false,
            at_us,
            description,
        })
    }

    /// The next frame's header; then [`StreamFile::read`] its bytes. Wraps to the first frame at
    /// the end.
    pub fn next_frame(&mut self) -> Result<FrameHeader> {
        if let Some(header) = self.read_header() {
            return Ok(header);
        }
        self.file.seek(SeekFrom::Start(HEADER_BYTES))?;
        let mut header = self.read_header().context("empty stream file")?;
        header.wrapped = true;
        Ok(header)
    }

    pub fn read(&mut self, into: &mut [u8]) -> Result<()> {
        Ok(self.file.read_exact(into)?)
    }

    /// Back to the first frame (a keyframe): the next header says `wrapped`.
    pub fn rewind(&mut self) -> Result<()> {
        // Past the end: `next_frame` wraps, as at the end of the file.
        self.file.seek(SeekFrom::End(0))?;
        Ok(())
    }
}

/// A frame buffer that goes back to the feeder when FFmpeg is done with it: the feeder's
/// stand-in for the core's pooled `FrameBuf`, and the pattern for carrying one in a [`Packet`].
struct FeedBuf {
    bytes: Vec<u8>,
    home: Arc<Mutex<Vec<Vec<u8>>>>,
}

// Safety: the Vec's heap block does not move with the struct, and the feeder zeroes PADDING
// bytes of spare capacity after the contents.
unsafe impl PacketBytes for FeedBuf {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn zero_padding(&self) -> usize {
        PADDING
    }
}

impl Drop for FeedBuf {
    fn drop(&mut self) {
        let mut home = self.home.lock();
        if home.len() < 8 {
            home.push(std::mem::take(&mut self.bytes));
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FeedOptions {
    /// Stop after this many frames (`None`: loop the file until stopped).
    pub frames: Option<u64>,
    /// Push frames as fast as the hub takes them instead of at the stream's timing (decode
    /// benchmarks; the 3-frame queue then sheds unless the caller paces by other means).
    pub unpaced: bool,
}

/// A thread (`video-feed-<n>`) pushing a file's frames into a hub.
pub struct Feeder {
    stop: Arc<AtomicBool>,
    pushed: Arc<AtomicU64>,
    done: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    rewind: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// From the file's header (0 for a recording).
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub codec: u8,
}

static FEEDERS: AtomicU64 = AtomicU64::new(0);

impl Feeder {
    pub fn start(hub: VideoHub, key: &str, path: &Path, options: FeedOptions) -> Result<Feeder> {
        let mut file = StreamFile::open(path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let pushed = Arc::new(AtomicU64::new(0));
        let done = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let rewind = Arc::new(AtomicBool::new(false));
        let (width, height, fps, codec) = (file.width, file.height, file.fps, file.codec);
        let key = key.to_owned();
        let thread = std::thread::Builder::new()
            .name(format!(
                "video-feed-{}",
                FEEDERS.fetch_add(1, Ordering::Relaxed)
            ))
            .spawn({
                let (stop, pushed, done) = (stop.clone(), pushed.clone(), done.clone());
                let (paused, rewind) = (paused.clone(), rewind.clone());
                move || {
                    let buffers: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
                    let mut start = Instant::now();
                    let mut index = 0u64;
                    let mut first_at_us = None;
                    let mut loop_base_us = 0u64;
                    let mut last_due_us = 0u64;
                    while !stop.load(Ordering::Relaxed)
                        && options.frames.is_none_or(|frames| index < frames)
                    {
                        if paused.load(Ordering::Acquire) {
                            // A streamer that stopped sending: parked until told otherwise,
                            // and the stream's clock does not run meanwhile.
                            let since = Instant::now();
                            while paused.load(Ordering::Acquire) && !stop.load(Ordering::Relaxed) {
                                std::thread::park();
                            }
                            start += since.elapsed();
                            continue;
                        }
                        if rewind.swap(false, Ordering::AcqRel) && file.rewind().is_err() {
                            break;
                        }
                        let Ok(header) = file.next_frame() else { break };
                        let due_us = match header.at_us {
                            // A recording: its own arrival times, looped end to start.
                            Some(at) => {
                                let first = *first_at_us.get_or_insert(at);
                                if header.wrapped {
                                    loop_base_us = last_due_us + 16_667;
                                }
                                loop_base_us + at.saturating_sub(first)
                            }
                            None => index * 1_000_000 / file.fps.max(1) as u64,
                        };
                        last_due_us = due_us;
                        index += 1;
                        let recycled = buffers.lock().pop();
                        let mut bytes = recycled.unwrap_or_default();
                        let len = header.size as usize;
                        bytes.clear();
                        bytes.resize(len + PADDING, 0);
                        if file.read(&mut bytes[..len]).is_err() {
                            break;
                        }
                        // The padding stays allocated and zero, past the length.
                        bytes.truncate(len);
                        if !options.unpaced {
                            let due = start + Duration::from_micros(due_us);
                            let now = Instant::now();
                            if due > now {
                                std::thread::sleep(due - now);
                            }
                        }
                        hub.push(
                            &key,
                            EncodedFrame {
                                codec: file.codec,
                                keyframe: header.keyframe,
                                timestamp: due_us as i64,
                                data: Packet::new(FeedBuf {
                                    bytes,
                                    home: buffers.clone(),
                                }),
                                description: header.description,
                                discontinuity: false,
                            },
                        );
                        pushed.fetch_add(1, Ordering::Relaxed);
                    }
                    done.store(true, Ordering::Release);
                }
            })?;
        Ok(Feeder {
            stop,
            pushed,
            done,
            paused,
            rewind,
            thread: Some(thread),
            width,
            height,
            fps,
            codec,
        })
    }

    /// Frames pushed so far.
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }

    /// The feeder pushed its last frame (it had a frame limit, or was stopped).
    pub fn finished(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Stops sending (`true`: a streamer whose frames no longer arrive) or goes on where it
    /// stopped, with the stream's clock where it was.
    pub fn pause(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }

    /// What a streamer does with a keyframe request, as a callback for
    /// [`crate::HubCallbacks::request_keyframe`]: the next frame sent is a keyframe (the
    /// file's first frame; the frames after it follow).
    pub fn keyframe_answer(&self) -> impl Fn() + Send + Sync + 'static {
        let rewind = self.rewind.clone();
        move || rewind.store(true, Ordering::Release)
    }

    /// Stops and waits for the thread (at most one frame interval).
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

impl Drop for Feeder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Rewrites the first `frames` frames of a DBV1 HEVC file (Annex B, parameter sets in the
/// bitstream) the way the streamer sends HEVC on the wire: every NAL unit prefixed with its
/// 4-byte length, the parameter sets out of band in an `hvcC` record that rides on each
/// keyframe as the frame's description. The result is a DBV2 recording. For tests of the
/// description path; the `hvcC` carries what a decoder needs (the parameter sets), not the
/// profile summary a container would want.
pub fn write_hvcc_recording(source: &Path, target: &Path, frames: usize) -> Result<()> {
    use std::io::Write as _;

    let mut file = StreamFile::open(source)?;
    ensure!(
        file.version == 1 && file.codec == 3,
        "{}: not a DBV1 HEVC file",
        source.display()
    );
    let mut out = std::io::BufWriter::new(File::create(target)?);
    let mut head = [0u8; HEADER_BYTES as usize];
    head[..4].copy_from_slice(b"DBV2");
    head[4] = 3;
    out.write_all(&head)?;
    let frame_us = 1_000_000 / file.fps.max(1) as u64;
    let mut bytes = Vec::new();
    for index in 0..frames {
        let header = file.next_frame()?;
        bytes.resize(header.size as usize, 0);
        file.read(&mut bytes)?;
        let mut parameter_sets: [Option<&[u8]>; 3] = [None; 3];
        let mut payload = Vec::with_capacity(bytes.len() + 64);
        for nal in annex_b_nals(&bytes) {
            match (nal[0] >> 1) & 0x3f {
                // VPS, SPS, PPS.
                kind @ 32..=34 => parameter_sets[kind as usize - 32] = Some(nal),
                _ => {
                    payload.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                    payload.extend_from_slice(nal);
                }
            }
        }
        let description = if header.keyframe {
            ensure!(
                parameter_sets.iter().all(Option::is_some),
                "keyframe {index} has no parameter sets"
            );
            let mut hvcc = vec![0u8; 23];
            hvcc[0] = 1; // configurationVersion
            hvcc[21] = 0xfc | 3; // lengthSizeMinusOne = 3
            hvcc[22] = 3; // numOfArrays
            for (kind, nal) in parameter_sets.iter().flatten().enumerate() {
                hvcc.push(0x80 | (32 + kind as u8));
                hvcc.extend_from_slice(&1u16.to_be_bytes());
                hvcc.extend_from_slice(&(nal.len() as u16).to_be_bytes());
                hvcc.extend_from_slice(nal);
            }
            Some(hvcc)
        } else {
            None
        };
        out.write_all(&(payload.len() as u32).to_le_bytes())?;
        let flags = header.keyframe as u32 | (description.is_some() as u32) << 1;
        out.write_all(&flags.to_le_bytes())?;
        out.write_all(&(index as u64 * frame_us).to_le_bytes())?;
        if let Some(description) = description {
            out.write_all(&(description.len() as u32).to_le_bytes())?;
            out.write_all(&description)?;
        }
        out.write_all(&payload)?;
    }
    out.flush()?;
    Ok(())
}

/// The NAL units of an Annex B access unit (without their start codes).
fn annex_b_nals(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= bytes.len() {
        if bytes[index] == 0 && bytes[index + 1] == 0 && bytes[index + 2] == 1 {
            starts.push(index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let mut end = starts.get(n + 1).map_or(bytes.len(), |next| next - 3);
            // The zero byte of a four-byte start code (and trailing zero padding) belongs to
            // no NAL unit.
            while end > start && bytes[end - 1] == 0 {
                end -= 1;
            }
            &bytes[start..end]
        })
        .filter(|nal| !nal.is_empty())
        .collect()
}

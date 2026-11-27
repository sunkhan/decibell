//! What the renderer's `probeMetadata` (`uploadAttachment.ts`) learned about a video or audio
//! file before it was sent, read here with the FFmpeg the client already links (the demuxers
//! and software decoders of `tools/build-ffmpeg.sh`): no `<video>` element, the same answers.
//! Every rule below was checked against what the Electron client stored for the same files
//! (`tests/assets/electron.md`).
//!
//! - **Size** (`video.videoWidth` × `videoHeight`): the stored picture's size stretched by its
//!   sample aspect ratio as Chromium's `VideoAspectRatio::GetNaturalSize` stretches it (wide
//!   pixels widen the picture, narrow ones heighten it), then turned by the track's display
//!   matrix (a quarter turn swaps the sides: a phone's portrait clip).
//! - **Duration** (`finiteDurationMs`): Chromium's `FFmpegDemuxer` takes the longest of the
//!   streams' durations and the container's. Two of its ways show in what it stores:
//!   - an MP3 with a Xing / Info / VBRI frame count lasts `frames × samples per frame` (the
//!     FFmpeg Chromium carries; ours subtracts the encoder's delay and padding: 2.040 s there
//!     where ours says 2.000);
//!   - a `<video>` (preloading, then seeking to the poster) reads about two seconds of packets
//!     past the poster's moment; a file that ends inside them has its demuxer reach the end,
//!     which stretches the duration to the last packet's end (a 1 s MP4 whose last AAC frame
//!     ends at 1.0027 s: 1003 ms; a 2.5 s one: 2500). An `<audio>` (`preload="metadata"`)
//!     never does (a 1 s M4A: 1000).
//!
//!   When the container gives no duration the renderer's `resolveDuration` seeks past the end
//!   and reads the length the demuxer then found: here the packets are read to the end.
//! - **Poster** (video only): the picture shown at `min(0.5 s, a tenth of the length)` — the
//!   renderer's seek (`(video.duration || 1) * 0.1`) — drawn upright at the shown size; its
//!   thumbnails and ThumbHash are made from it (`thumbs::poster_facts`). After the seek Chromium
//!   shows the last frame that starts at or before the moment (a frame that ends exactly on it
//!   is skipped: 25 fps, moment 0.2 s → the frame at 0.2 s, not the one at 0.16 s). The picture
//!   is converted with the frame's own matrix and range, an unspecified matrix taken as BT.601
//!   as Chromium's `PaintCanvasVideoRenderer` does, and is never kept larger than the largest
//!   thumbnail.
//!
//! Bounded for a hostile file, as the renderer's waits were: each of the three steps (the
//! header, the poster, the length) gives up after 5 s (the renderer's `withTimeout`s and
//! `resolveDuration`'s timer) with what the renderer had then: nothing if the header never
//! read, no poster, a length of 0. The decoder refuses pictures over 8K UHD, decodes on one
//! thread (no frame-thread copies), the demuxer takes at most 64 streams, and the poster is
//! scaled straight from the decoded frame to at most 1280 px on its long edge. Reads go through
//! a 64 KiB custom I/O over the file (or the pasted bytes): FFmpeg opens nothing itself.
//!
//! Call from a blocking thread (`upload::prepare` does, through `spawn_blocking`).

use std::cell::Cell;
use std::ffi::{c_int, c_void, CStr};
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

use ffmpeg_sys_next as ff;
use image::RgbaImage;

use crate::kind::AttachmentKind;
use crate::thumbs::{thumbnail_dims, MediaProbe, ProbeSource, ProbedMedia, THUMB_SIZES};

/// How long each of the renderer's waits lasted (metadata, the poster's seek, the duration
/// scan): `withTimeout(…, 5000)`, `resolveDuration`'s 5 s timer.
pub const STEP: Duration = Duration::from_secs(5);
/// The largest picture the poster's decoder takes (8K UHD).
const MAX_PIXELS: i64 = 7680 * 4320;
/// A file with more streams than this is refused (FFmpeg's default is 1000).
const MAX_STREAMS: c_int = 64;
const IO_BUFFER: usize = 64 * 1024;
const MICROS: ff::AVRational = ff::AVRational { num: 1, den: 1_000_000 };
// swscale's flags (stable values; an enum since FFmpeg 8, macros before): area averaging for the
// downscale, exact rounding, chroma interpolated to every output pixel.
const SWS_AREA: c_int = 0x20;
const SWS_FULL_CHR_H_INT: c_int = 0x2000;
const SWS_ACCURATE_RND: c_int = 0x40000;

/// The app's [`MediaProbe`]: FFmpeg, as linked for playback.
#[derive(Clone, Copy, Debug, Default)]
pub struct FfmpegProbe;

impl MediaProbe for FfmpegProbe {
    fn probe(&self, source: ProbeSource<'_>, kind: AttachmentKind) -> Option<ProbedMedia> {
        probe(source, kind, STEP)
    }
}

/// [`FfmpegProbe`] with another time limit per step (tests). `None`: the header could not be
/// read (the renderer's failed probe: no size, no length, no poster).
pub fn probe(source: ProbeSource<'_>, kind: AttachmentKind, step: Duration) -> Option<ProbedMedia> {
    if !matches!(kind, AttachmentKind::Video | AttachmentKind::Audio) {
        return None;
    }
    match source {
        ProbeSource::Path(path) => run(std::fs::File::open(path).ok()?, kind, step),
        ProbeSource::Bytes(bytes) => run(std::io::Cursor::new(bytes), kind, step),
    }
}

/// How far past the poster's moment a `<video>` reads before it has enough (Chromium's
/// `FFmpegDemuxerStream::HasAvailableCapacity`: two seconds' worth per stream). Between the
/// Electron client's 2 s clip (read to its end) and its 2.5 s one (not).
const READ_AHEAD: i64 = 2_000_000;

fn run<R: Read + Seek>(reader: R, kind: AttachmentKind, step: Duration) -> Option<ProbedMedia> {
    let mut input = Input::open(reader, step)?;
    let mut duration = input.duration();
    if let Some(frames) = input.mp3_frames_duration() {
        duration = Some(duration.map_or(frames, |d| d.max(frames)));
    }
    let mut out = ProbedMedia::default();
    if kind == AttachmentKind::Video {
        let target = poster_target(duration);
        if let Some(video) = input.video() {
            (out.width, out.height) = video.shown;
            input.restart_clock(step);
            out.poster = input.poster(&video, target);
        }
        if let Some(d) = duration {
            input.restart_clock(step);
            if let Some(end) = input.end_within(target + READ_AHEAD) {
                duration = Some(d.max(end));
            }
        }
    }
    let duration = match duration {
        Some(micros) => Some(micros),
        None => {
            input.restart_clock(step);
            input.scan_to_end()
        }
    };
    out.duration_ms = finite_ms(duration);
    Some(out)
}

/// The poster's moment: `Math.min(0.5, (video.duration || 1) * 0.1)` seconds, in µs (an
/// unknown length is the renderer's `Infinity`, a 0 its `|| 1`).
pub fn poster_target(duration: Option<i64>) -> i64 {
    match duration {
        None => 500_000,
        Some(d) if d <= 0 => 100_000,
        Some(d) => (d / 10).min(500_000),
    }
}

/// `finiteDurationMs`: `Math.round(seconds * 1000)`, 0 for an unknown or empty length.
pub fn finite_ms(duration: Option<i64>) -> u32 {
    match duration {
        Some(micros) if micros > 0 => ((micros + 500) / 1000).min(u32::MAX as i64) as u32,
        _ => 0,
    }
}

/// Chromium's natural size (`VideoAspectRatio::GetNaturalSize` for a pixel aspect ratio): wide
/// pixels widen the picture, narrow ones make it taller, each side rounded.
pub fn natural_size(width: u32, height: u32, sar: (i32, i32)) -> (u32, u32) {
    let (num, den) = sar;
    if num <= 0 || den <= 0 || width == 0 || height == 0 {
        return (width, height);
    }
    let ratio = num as f64 / den as f64;
    let (mut w, mut h) = (width as f64, height as f64);
    if ratio >= 1.0 {
        w *= ratio;
    } else {
        h /= ratio;
    }
    (w.round().clamp(1.0, u32::MAX as f64) as u32, h.round().clamp(1.0, u32::MAX as f64) as u32)
}

/// A display matrix's clockwise turn in quarter turns (0..=3), as `fftools`' `get_rotation`
/// reads it (FFmpeg stores counter-clockwise degrees).
pub fn quarter_turns(ccw_degrees: f64) -> u8 {
    if !ccw_degrees.is_finite() {
        return 0;
    }
    let clockwise = -ccw_degrees.round();
    ((clockwise / 90.0).round() as i64).rem_euclid(4) as u8
}

struct Video {
    index: c_int,
    /// The size as shown (sample aspect ratio and turn applied).
    shown: (u32, u32),
    /// Clockwise quarter turns.
    turns: u8,
}

/// The reader behind the custom I/O, and the step's deadline.
struct Io<R> {
    reader: R,
    deadline: Cell<Instant>,
    size: Option<i64>,
}

struct Input<R: Read + Seek> {
    format: *mut ff::AVFormatContext,
    avio: *mut ff::AVIOContext,
    /// Boxed: the I/O callbacks hold its address.
    io: Box<Io<R>>,
}

unsafe extern "C" fn read_packet<R: Read + Seek>(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    // Safety: `opaque` is the `Io<R>` boxed in `Input<R>`, which outlives the I/O context; FFmpeg
    // hands a writable buffer of `size` bytes.
    let io = unsafe { &mut *(opaque as *mut Io<R>) };
    if Instant::now() > io.deadline.get() {
        return ff::AVERROR_EXIT;
    }
    if size <= 0 {
        return 0;
    }
    let out = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
    loop {
        match io.reader.read(out) {
            Ok(0) => return ff::AVERROR_EOF,
            Ok(n) => return n as c_int,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return ff::AVERROR(ff::EIO),
        }
    }
}

unsafe extern "C" fn seek<R: Read + Seek>(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    // Safety: as in `read_packet`.
    let io = unsafe { &mut *(opaque as *mut Io<R>) };
    if whence & ff::AVSEEK_SIZE != 0 {
        return io.size.unwrap_or(-1);
    }
    let to = match whence & !ff::AVSEEK_FORCE {
        ff::SEEK_SET if offset >= 0 => SeekFrom::Start(offset as u64),
        ff::SEEK_CUR => SeekFrom::Current(offset),
        ff::SEEK_END => SeekFrom::End(offset),
        _ => return -1,
    };
    io.reader.seek(to).map_or(-1, |at| at as i64)
}

unsafe extern "C" fn interrupted<R>(opaque: *mut c_void) -> c_int {
    // Safety: as in `read_packet`; only the deadline is read.
    let io = unsafe { &*(opaque as *const Io<R>) };
    (Instant::now() > io.deadline.get()) as c_int
}

fn rescale(value: i64, from: ff::AVRational, to: ff::AVRational) -> i64 {
    // Safety: pure arithmetic.
    unsafe { ff::av_rescale_q(value, from, to) }
}

impl<R: Read + Seek> Input<R> {
    /// Reads the header and the streams' facts (`loadedmetadata`), within one step.
    fn open(mut reader: R, step: Duration) -> Option<Input<R>> {
        let size = reader.seek(SeekFrom::End(0)).ok().map(|n| n as i64);
        reader.seek(SeekFrom::Start(0)).ok()?;
        let io = Box::new(Io { reader, deadline: Cell::new(Instant::now() + step), size });
        // Safety: FFmpeg object setup. Every pointer is checked; what was made is freed by
        // `Drop` (an `Input` exists from the first allocation on), and `avformat_open_input` frees
        // the context it was given when it fails.
        unsafe {
            let buffer = ff::av_malloc(IO_BUFFER) as *mut u8;
            if buffer.is_null() {
                return None;
            }
            let opaque = &*io as *const Io<R> as *mut c_void;
            let avio = ff::avio_alloc_context(buffer, IO_BUFFER as c_int, 0, opaque, Some(read_packet::<R>), None, Some(seek::<R>));
            if avio.is_null() {
                ff::av_free(buffer as *mut c_void);
                return None;
            }
            let mut input = Input { format: std::ptr::null_mut(), avio, io };
            let mut format = ff::avformat_alloc_context();
            if format.is_null() {
                return None;
            }
            (*format).pb = avio;
            (*format).flags |= ff::AVFMT_FLAG_CUSTOM_IO as c_int;
            (*format).max_streams = MAX_STREAMS;
            (*format).interrupt_callback = ff::AVIOInterruptCB { callback: Some(interrupted::<R>), opaque };
            if ff::avformat_open_input(&mut format, std::ptr::null(), std::ptr::null(), std::ptr::null_mut()) < 0 {
                return None;
            }
            input.format = format;
            if ff::avformat_find_stream_info(format, std::ptr::null_mut()) < 0 {
                return None;
            }
            Some(input)
        }
    }

    fn restart_clock(&self, step: Duration) {
        self.io.deadline.set(Instant::now() + step);
    }

    fn expired(&self) -> bool {
        Instant::now() > self.io.deadline.get()
    }

    fn streams(&self) -> &[*mut ff::AVStream] {
        // Safety: the open context's stream table.
        unsafe {
            let format = &*self.format;
            if format.streams.is_null() {
                return &[];
            }
            std::slice::from_raw_parts(format.streams, format.nb_streams as usize)
        }
    }

    /// The container's start, which Chromium makes the media's time 0 (an MP3 starts at 0:
    /// `FFmpegDemuxer` ignores the start FFmpeg moves forward for its discarded samples).
    fn start(&self) -> i64 {
        // Safety: the open context.
        unsafe {
            let format = &*self.format;
            let mp3 = !format.iformat.is_null() && CStr::from_ptr((*format.iformat).name).to_bytes() == b"mp3";
            if mp3 || format.start_time == ff::AV_NOPTS_VALUE {
                0
            } else {
                format.start_time
            }
        }
    }

    /// `media.duration` at `loadedmetadata`, µs: the longest of the audio and video streams'
    /// durations and the container's; `None` (Chromium's infinite duration) when the container
    /// gives none.
    fn duration(&self) -> Option<i64> {
        // Safety: the open context and its streams.
        unsafe {
            let format = &*self.format;
            if format.duration == ff::AV_NOPTS_VALUE {
                return None;
            }
            let mut longest = format.duration;
            for &stream in self.streams() {
                let stream = &*stream;
                let kind = (*stream.codecpar).codec_type;
                let av = kind == ff::AVMediaType::AVMEDIA_TYPE_AUDIO || kind == ff::AVMediaType::AVMEDIA_TYPE_VIDEO;
                if av && stream.duration != ff::AV_NOPTS_VALUE && stream.duration > 0 {
                    longest = longest.max(rescale(stream.duration, stream.time_base, MICROS));
                }
            }
            Some(longest)
        }
    }

    /// The first video stream Chromium would show (not a cover picture), if this client has
    /// a decoder for it.
    fn video(&self) -> Option<Video> {
        // Safety: the open context and its streams.
        unsafe {
            for (index, &stream) in self.streams().iter().enumerate() {
                let parameters = &*(*stream).codecpar;
                if parameters.codec_type != ff::AVMediaType::AVMEDIA_TYPE_VIDEO
                    || (*stream).disposition & ff::AV_DISPOSITION_ATTACHED_PIC != 0
                {
                    continue;
                }
                if decoder(parameters.codec_id).is_null() || parameters.width <= 0 || parameters.height <= 0 {
                    // Chromium leaves out a stream it can't decode: no picture, no size.
                    return None;
                }
                let sar = ff::av_guess_sample_aspect_ratio(self.format, stream, std::ptr::null_mut());
                let natural = natural_size(parameters.width as u32, parameters.height as u32, (sar.num, sar.den));
                let side = ff::av_packet_side_data_get(
                    parameters.coded_side_data,
                    parameters.nb_coded_side_data,
                    ff::AVPacketSideDataType::AV_PKT_DATA_DISPLAYMATRIX,
                );
                let turns = if side.is_null() || (*side).data.is_null() || (*side).size < 36 {
                    0
                } else {
                    quarter_turns(ff::av_display_rotation_get((*side).data as *const i32))
                };
                let shown = if turns % 2 == 1 { (natural.1, natural.0) } else { natural };
                return Some(Video { index: index as c_int, shown, turns });
            }
            None
        }
    }

    /// The picture at `target` µs, upright at the shown size (at most the largest thumbnail).
    /// `None` when nothing decoded in time (the renderer's poster was optional too).
    fn poster(&mut self, video: &Video, target: i64) -> Option<RgbaImage> {
        let start = self.start();
        let stream = *self.streams().get(video.index as usize)?;
        let mut decoder = Decoder::open(stream)?;
        for (index, &other) in self.streams().iter().enumerate() {
            // Safety: the context's own streams.
            unsafe { (*other).discard = if index as c_int == video.index { ff::AVDiscard::AVDISCARD_DEFAULT } else { ff::AVDiscard::AVDISCARD_ALL } };
        }
        // Safety: the stream's time base.
        let time_base = unsafe { (*stream).time_base };
        let packet = Packet::new()?;
        let mut chosen = Frame::new()?;
        let mut have = false;
        let mut done = false;
        let mut take = |frame: &mut Frame| -> bool {
            // Frames leave the decoder in presentation order: keep the last one that starts at
            // or before the target; the first after it ends the search (and is the poster when
            // none came before).
            let stamp = frame.timestamp();
            let at = if stamp == ff::AV_NOPTS_VALUE { 0 } else { rescale(stamp, time_base, MICROS) - start };
            if at <= target || !have {
                chosen.take(frame);
                have = true;
            }
            at > target
        };
        while !done && !self.expired() {
            // Safety: the open context; the packet is unreferenced before each reuse.
            let read = unsafe {
                ff::av_packet_unref(packet.0);
                ff::av_read_frame(self.format, packet.0)
            };
            if read < 0 {
                // The end (or a read error): flush what the decoder holds.
                decoder.send(std::ptr::null_mut());
                while !done && decoder.receive() {
                    done = take(&mut decoder.frame);
                }
                break;
            }
            // Safety: a packet just read.
            if unsafe { (*packet.0).stream_index } != video.index {
                continue;
            }
            decoder.send(packet.0);
            while !done && decoder.receive() {
                done = take(&mut decoder.frame);
            }
        }
        if !have || self.expired() {
            return None;
        }
        chosen.to_upright(video)
    }

    /// Where the audio and video packets end (µs from the start) when the file ends before
    /// `limit`: a `<video>`'s read-ahead reaches the end there. `None` when a packet starts past
    /// `limit` first, or nothing could be read in time.
    fn end_within(&mut self, limit: i64) -> Option<i64> {
        let start = self.start();
        // Safety: the open context; every stream is read again from the start.
        unsafe {
            for &stream in self.streams() {
                (*stream).discard = ff::AVDiscard::AVDISCARD_DEFAULT;
            }
            if ff::avformat_seek_file(self.format, -1, i64::MIN, (*self.format).start_time.max(0), i64::MAX, 0) < 0 {
                return None;
            }
        }
        self.read_ends(Some(limit), start)
    }

    /// `resolveDuration`'s seek past the end: read every packet and take where the last one
    /// ends, µs from the start. `None` past the step's time (the renderer kept `Infinity`: 0).
    fn scan_to_end(&mut self) -> Option<i64> {
        let start = self.start();
        self.read_ends(None, start)
    }

    /// Reads packets to the end (or until one starts past `limit`, then `None`) and returns where
    /// the last audio or video packet ends, µs from `start`.
    fn read_ends(&mut self, limit: Option<i64>, start: i64) -> Option<i64> {
        let packet = Packet::new()?;
        let mut end: Option<i64> = None;
        let bases: Vec<(ff::AVRational, bool)> = self
            .streams()
            .iter()
            .map(|&stream| {
                // Safety: the context's own streams.
                unsafe {
                    let kind = (*(*stream).codecpar).codec_type;
                    ((*stream).time_base, kind == ff::AVMediaType::AVMEDIA_TYPE_AUDIO || kind == ff::AVMediaType::AVMEDIA_TYPE_VIDEO)
                }
            })
            .collect();
        loop {
            if self.expired() {
                return None;
            }
            // Safety: the open context; the packet is unreferenced before each reuse.
            let read = unsafe {
                ff::av_packet_unref(packet.0);
                ff::av_read_frame(self.format, packet.0)
            };
            if read == ff::AVERROR_EOF {
                break;
            }
            if read < 0 {
                if self.expired() {
                    return None;
                }
                break;
            }
            // Safety: a packet just read.
            let p = unsafe { &*packet.0 };
            let Some(&(base, counted)) = bases.get(p.stream_index as usize) else { continue };
            let stamp = if p.pts != ff::AV_NOPTS_VALUE { p.pts } else { p.dts };
            if !counted || stamp == ff::AV_NOPTS_VALUE {
                continue;
            }
            if limit.is_some_and(|limit| rescale(stamp, base, MICROS) - start > limit) {
                return None;
            }
            let ends = rescale(stamp + p.duration.max(0), base, MICROS);
            end = Some(end.map_or(ends, |e| e.max(ends)));
        }
        end.map(|e| e - start)
    }

    /// An MP3's length as the FFmpeg in Chromium gives it: the frame count of its Xing / Info /
    /// VBRI tag times the samples per frame (`mp3_parse_vbr_tags`; FFmpeg 8 takes the encoder's
    /// delay and padding off). Read from the file directly; `None` for any other container or
    /// an MP3 without a frame count (FFmpeg's estimate from the bitrate stands then).
    fn mp3_frames_duration(&mut self) -> Option<i64> {
        // Safety: the open context's demuxer name.
        let mp3 = unsafe {
            let format = &*self.format;
            !format.iformat.is_null() && CStr::from_ptr((*format.iformat).name).to_bytes() == b"mp3"
        };
        if !mp3 {
            return None;
        }
        // FFmpeg reads through its own buffer and remembers its position: put the reader back
        // where it was.
        let reader = &mut self.io.reader;
        let at = reader.stream_position().ok()?;
        let found = mp3_tag(reader);
        reader.seek(SeekFrom::Start(at)).ok()?;
        let (frames, samples, rate) = found?;
        Some(((frames as i128 * samples as i128 * 1_000_000 + rate as i128 / 2) / rate as i128) as i64)
    }
}

impl<R: Read + Seek> Drop for Input<R> {
    fn drop(&mut self) {
        // Safety: ours. The format context goes first (it reads through the I/O context), then
        // the I/O context and its buffer (FFmpeg may have replaced the buffer: free the current
        // one); `io` is dropped after this body.
        unsafe {
            if !self.format.is_null() {
                ff::avformat_close_input(&mut self.format);
            }
            if !self.avio.is_null() {
                ff::av_freep(&mut (*self.avio).buffer as *mut *mut u8 as *mut c_void);
                ff::avio_context_free(&mut self.avio);
            }
        }
    }
}

/// The first MPEG audio frame after an ID3v2 tag: its Xing / Info / VBRI frame count, samples
/// per frame and sample rate (Layer III only, as FFmpeg reads it).
fn mp3_tag<R: Read + Seek>(reader: &mut R) -> Option<(u32, u32, u32)> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 10];
    reader.read_exact(&mut head).ok()?;
    let mut base = 0u64;
    if &head[..3] == b"ID3" {
        let size = head[6..10].iter().fold(0u64, |n, b| (n << 7) | (*b as u64 & 0x7f));
        base = 10 + size + if head[5] & 0x10 != 0 { 10 } else { 0 };
    }
    // A few bytes of padding may sit between the tag and the first frame.
    reader.seek(SeekFrom::Start(base)).ok()?;
    let mut buf = vec![0u8; 4096 + 64];
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let buf = &buf[..filled];
    let be32 = |at: usize| buf.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    for at in 0..buf.len().saturating_sub(4).min(4096) {
        let header = be32(at)?;
        if header & 0xFFE0_0000 != 0xFFE0_0000 {
            continue;
        }
        let version = (header >> 19) & 3; // 0: MPEG 2.5, 2: MPEG 2, 3: MPEG 1
        let layer = (header >> 17) & 3; // 1: Layer III
        let rate_index = (header >> 10) & 3;
        if version == 1 || layer != 1 || rate_index == 3 || (header >> 12) & 0xF == 0xF {
            continue;
        }
        let rate = [44_100, 48_000, 32_000][rate_index as usize] >> match version {
            3 => 0,
            2 => 1,
            _ => 2,
        };
        let lsf = version != 3;
        let samples = if lsf { 576 } else { 1152 };
        let mono = (header >> 6) & 3 == 3;
        let side = match (lsf, mono) {
            (false, false) => 32,
            (false, true) => 17,
            (true, false) => 17,
            (true, true) => 9,
        };
        let xing = at + 4 + side;
        if matches!(buf.get(xing..xing + 4), Some(b"Xing") | Some(b"Info")) {
            let flags = be32(xing + 4)?;
            if flags & 1 != 0 {
                let frames = be32(xing + 8)?;
                return (frames > 0).then_some((frames, samples, rate));
            }
        }
        let vbri = at + 4 + 32;
        if buf.get(vbri..vbri + 4) == Some(b"VBRI") && buf.get(vbri + 4..vbri + 6) == Some(&[0, 1]) {
            let frames = be32(vbri + 14)?;
            return (frames > 0).then_some((frames, samples, rate));
        }
        return None;
    }
    None
}

/// The software decoder for a codec (AV1: dav1d; FFmpeg's own AV1 decoder only drives a
/// hardware one).
fn decoder(id: ff::AVCodecID) -> *const ff::AVCodec {
    // Safety: lookups in FFmpeg's static tables.
    unsafe {
        if id == ff::AVCodecID::AV_CODEC_ID_AV1 {
            let dav1d = ff::avcodec_find_decoder_by_name(c"libdav1d".as_ptr());
            if !dav1d.is_null() {
                return dav1d;
            }
        }
        ff::avcodec_find_decoder(id)
    }
}

struct Packet(*mut ff::AVPacket);

impl Packet {
    fn new() -> Option<Packet> {
        // Safety: an allocation, checked.
        let p = unsafe { ff::av_packet_alloc() };
        (!p.is_null()).then_some(Packet(p))
    }
}

impl Drop for Packet {
    fn drop(&mut self) {
        // Safety: ours.
        unsafe { ff::av_packet_free(&mut self.0) }
    }
}

struct Frame(*mut ff::AVFrame);

impl Frame {
    fn new() -> Option<Frame> {
        // Safety: an allocation, checked.
        let f = unsafe { ff::av_frame_alloc() };
        (!f.is_null()).then_some(Frame(f))
    }

    fn timestamp(&self) -> i64 {
        // Safety: a frame the decoder filled.
        unsafe {
            let f = &*self.0;
            if f.best_effort_timestamp != ff::AV_NOPTS_VALUE {
                f.best_effort_timestamp
            } else {
                f.pts
            }
        }
    }

    /// Moves `other`'s picture into this frame (`other` is left empty).
    fn take(&mut self, other: &mut Frame) {
        // Safety: both are valid frames; the old picture is released first.
        unsafe {
            ff::av_frame_unref(self.0);
            ff::av_frame_move_ref(self.0, other.0);
        }
    }

    /// The picture converted to RGBA at the poster's size (before the turn), then turned.
    fn to_upright(&self, video: &Video) -> Option<RgbaImage> {
        let (shown_w, shown_h) = video.shown;
        let (w, h) = thumbnail_dims(shown_w, shown_h, *THUMB_SIZES.last()?)?;
        // The picture as stored, at the size it is drawn before the turn.
        let (pre_w, pre_h) = if video.turns % 2 == 1 { (h, w) } else { (w, h) };
        // Safety: a decoded frame; the scaler is checked and freed; the destination holds
        // `stride × pre_h` bytes (stride rounded up for FFmpeg's vector writes).
        let rgba = unsafe {
            let f = &*self.0;
            if f.width <= 0 || f.height <= 0 || f.format < 0 || f.format >= ff::AVPixelFormat::AV_PIX_FMT_NB as c_int {
                return None;
            }
            let format: ff::AVPixelFormat = std::mem::transmute::<c_int, ff::AVPixelFormat>(f.format);
            let flags = SWS_AREA | SWS_ACCURATE_RND | SWS_FULL_CHR_H_INT;
            let sws = ff::sws_getContext(
                f.width,
                f.height,
                format,
                pre_w as c_int,
                pre_h as c_int,
                ff::AVPixelFormat::AV_PIX_FMT_RGBA,
                flags,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            );
            if sws.is_null() {
                return None;
            }
            let matrix = match f.colorspace {
                ff::AVColorSpace::AVCOL_SPC_BT709 => ff::SWS_CS_ITU709,
                ff::AVColorSpace::AVCOL_SPC_BT2020_NCL | ff::AVColorSpace::AVCOL_SPC_BT2020_CL => ff::SWS_CS_BT2020,
                ff::AVColorSpace::AVCOL_SPC_FCC => ff::SWS_CS_FCC,
                ff::AVColorSpace::AVCOL_SPC_SMPTE240M => ff::SWS_CS_SMPTE240M,
                // Unspecified, BT.470BG, SMPTE 170M: BT.601, Chromium's default.
                _ => ff::SWS_CS_ITU601,
            };
            let full = f.color_range == ff::AVColorRange::AVCOL_RANGE_JPEG
                || matches!(
                    format,
                    ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P | ff::AVPixelFormat::AV_PIX_FMT_YUVJ422P | ff::AVPixelFormat::AV_PIX_FMT_YUVJ444P
                );
            ff::sws_setColorspaceDetails(
                sws,
                ff::sws_getCoefficients(matrix),
                full as c_int,
                ff::sws_getCoefficients(ff::SWS_CS_DEFAULT),
                1,
                0,
                1 << 16,
                1 << 16,
            );
            let stride = (pre_w as usize * 4).next_multiple_of(64);
            let mut buffer = vec![0u8; stride * pre_h as usize + 64];
            let dst = [buffer.as_mut_ptr(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()];
            let dst_stride = [stride as c_int, 0, 0, 0];
            let rows = ff::sws_scale(
                sws,
                f.data.as_ptr() as *const *const u8,
                f.linesize.as_ptr(),
                0,
                f.height,
                dst.as_ptr(),
                dst_stride.as_ptr(),
            );
            ff::sws_freeContext(sws);
            if rows <= 0 {
                return None;
            }
            let mut packed = Vec::with_capacity(pre_w as usize * pre_h as usize * 4);
            for row in buffer.chunks(stride).take(pre_h as usize) {
                packed.extend_from_slice(&row[..pre_w as usize * 4]);
            }
            RgbaImage::from_raw(pre_w, pre_h, packed)?
        };
        Some(match video.turns {
            1 => image::imageops::rotate90(&rgba),
            2 => image::imageops::rotate180(&rgba),
            3 => image::imageops::rotate270(&rgba),
            _ => rgba,
        })
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        // Safety: ours.
        unsafe { ff::av_frame_free(&mut self.0) }
    }
}

struct Decoder {
    context: *mut ff::AVCodecContext,
    frame: Frame,
}

impl Decoder {
    fn open(stream: *mut ff::AVStream) -> Option<Decoder> {
        // Safety: the stream's parameters; the context is checked and freed by `Drop` from the
        // moment it exists.
        unsafe {
            let parameters = (*stream).codecpar;
            let codec = decoder((*parameters).codec_id);
            if codec.is_null() {
                return None;
            }
            let context = ff::avcodec_alloc_context3(codec);
            if context.is_null() {
                return None;
            }
            let this = Decoder { context, frame: Frame::new()? };
            if ff::avcodec_parameters_to_context(context, parameters) < 0 {
                return None;
            }
            (*context).pkt_timebase = (*stream).time_base;
            (*context).max_pixels = MAX_PIXELS;
            (*context).thread_count = 1;
            if ff::avcodec_open2(context, codec, std::ptr::null_mut()) < 0 {
                return None;
            }
            Some(this)
        }
    }

    /// One packet in (null: the end). A refused packet is skipped, as a player skips a
    /// damaged one.
    fn send(&mut self, packet: *mut ff::AVPacket) {
        // Safety: an open decoder; a packet just read, or null to flush.
        unsafe {
            ff::avcodec_send_packet(self.context, packet);
        }
    }

    /// The next decoded picture into `self.frame`; false when it needs more input.
    fn receive(&mut self) -> bool {
        // Safety: an open decoder and our frame.
        unsafe { ff::avcodec_receive_frame(self.context, self.frame.0) >= 0 }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // Safety: ours.
        unsafe { ff::avcodec_free_context(&mut self.context) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/assets").join(name)
    }

    fn media_asset(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../decibell-media/tests/assets").join(name)
    }

    /// The test pictures' frame number (`tests/assets/make.py`: grey 16 + step·n) read from the
    /// field away from the white block, in the poster's RGBA (full range).
    fn frame_number(poster: &RgbaImage, step: f32) -> u32 {
        let (w, h) = poster.dimensions();
        let p = poster.get_pixel(w / 2, h / 2).0;
        // Limited-range luma Y shows as (Y − 16) · 255 / 219.
        let y = p[1] as f32 * 219. / 255. + 16.;
        ((y - 16.) / step).round() as u32
    }

    /// Where the white block went: (left half, top half) of the upright poster.
    fn block(poster: &RgbaImage) -> (bool, bool) {
        let (w, h) = poster.dimensions();
        let corners = [(2, 2), (w - 3, 2), (2, h - 3), (w - 3, h - 3)];
        let white = corners.iter().find(|&&(x, y)| poster.get_pixel(x, y).0[1] > 230).copied().expect("the white block in a corner");
        (white.0 < w / 2, white.1 < h / 2)
    }

    #[test]
    fn a_turned_h264_clip() {
        let m = probe(ProbeSource::Path(&asset("rot90.mp4")), AttachmentKind::Video, STEP).expect("probed");
        // Stored 160 × 90, shown turned a quarter. 1 s long, but its last AAC frame ends at
        // 1.0027 s and a `<video>` reads that far.
        assert_eq!((m.width, m.height, m.duration_ms), (90, 160, 1003));
        let poster = m.poster.expect("a poster");
        assert_eq!(poster.dimensions(), (90, 160));
        // 1 s long: the poster's moment is 0.1 s; frames start every 40 ms (0, 40, 80, 120…):
        // frame 2 (80 ms) is the last to start before it.
        assert_eq!(frame_number(&poster, 8.), 2);
        // `-display_rotation 90` turns the picture counter-clockwise: the stored top-left block
        // is shown at the bottom left (what `ffmpeg -i rot90.mp4 x.png` draws too).
        assert_eq!(block(&poster), (true, false));
    }

    #[test]
    fn wide_pixels_and_av1() {
        let m = probe(ProbeSource::Path(&asset("sar.webm")), AttachmentKind::Video, STEP).expect("probed");
        // 120 × 90 stored with a 2:1 sample aspect ratio.
        assert_eq!((m.width, m.height, m.duration_ms), (240, 90, 2000));
        let poster = m.poster.expect("a poster (dav1d)");
        assert_eq!(poster.dimensions(), (240, 90));
        // 2 s: the moment is 0.2 s; frames every 40 ms: frame 4 (160 ms) ends exactly on it,
        // so the frame shown is 5 (200 ms), as the Electron client's poster of this file is.
        assert_eq!(frame_number(&poster, 4.), 5);
        assert_eq!(block(&poster), (true, true));
    }

    #[test]
    fn a_moment_between_frames() {
        let m = probe(ProbeSource::Path(&asset("noaudio.mp4")), AttachmentKind::Video, STEP).expect("probed");
        assert_eq!((m.width, m.height, m.duration_ms), (160, 90, 3000));
        // 3 s: the moment is 0.3 s; at 9 fps frame 2 starts at 222 ms, frame 3 at 333.
        assert_eq!(frame_number(&m.poster.expect("a poster"), 8.), 2);
    }

    #[test]
    fn from_memory_as_from_disk() {
        let bytes = std::fs::read(asset("rot90.mp4")).unwrap();
        let m = probe(ProbeSource::Bytes(&bytes), AttachmentKind::Video, STEP).expect("probed");
        assert_eq!((m.width, m.height, m.duration_ms), (90, 160, 1003));
        assert!(m.poster.is_some());
    }

    #[test]
    fn audio_has_a_length_and_nothing_else() {
        // The Electron client's lengths for the media crate's files: an Info-tagged MP3 counts
        // whole frames (85 × 24 ms), one without a tag keeps FFmpeg's estimate from its bitrate.
        for (name, ms) in [("cbr.mp3", 2040), ("vbr_noxing.mp3", 2029), ("aac.m4a", 2000), ("opus.ogg", 2000), ("flac.flac", 2000)] {
            let path = media_asset(name);
            if !path.exists() {
                continue;
            }
            let m = probe(ProbeSource::Path(&path), AttachmentKind::Audio, STEP).unwrap_or_else(|| panic!("{name} probed"));
            assert_eq!((m.width, m.height, m.poster.is_some()), (0, 0, false), "{name}");
            if name.ends_with(".mp3") {
                assert_eq!(m.duration_ms, ms, "{name}");
            } else {
                assert!(m.duration_ms.abs_diff(ms) <= 30, "{name}: {} ms", m.duration_ms);
            }
        }
        // The clip's sound alone, as an audio upload reads it: no read-ahead (1000, not 1003).
        let m = probe(ProbeSource::Path(&asset("rot90.mp4")), AttachmentKind::Audio, STEP).expect("probed");
        assert_eq!((m.width, m.height, m.duration_ms, m.poster.is_some()), (0, 0, 1000, false));
    }

    #[test]
    fn not_media_and_damaged_files() {
        let text = b"hello, this is not a video at all".repeat(100);
        assert!(probe(ProbeSource::Bytes(&text), AttachmentKind::Video, STEP).is_none());
        assert!(probe(ProbeSource::Bytes(&[]), AttachmentKind::Audio, STEP).is_none());
        // The first half of a clip: its header is whole (the moov box leads), its pictures
        // are not all there.
        let bytes = std::fs::read(asset("noaudio.mp4")).unwrap();
        let half = &bytes[..bytes.len() / 2];
        if let Some(m) = probe(ProbeSource::Bytes(half), AttachmentKind::Video, STEP) {
            assert_eq!((m.width, m.height), (160, 90));
        }
        // Every truncation of it either reads or doesn't; none hangs or crashes.
        for cut in (0..bytes.len()).step_by(97) {
            let _ = probe(ProbeSource::Bytes(&bytes[..cut]), AttachmentKind::Video, Duration::from_millis(500));
        }
    }

    /// `tests/assets/electron.md`: what the Electron client stored for the same files. Sizes,
    /// lengths and the poster's frame are equal; the ThumbHash too, or within a few levels (it is
    /// made from pixels the two decoders and scalers round apart). One recorded difference:
    /// `ends25.mp4`'s length (electron-quirks #40).
    #[test]
    fn the_electron_clients_values() {
        struct Row {
            file: std::path::PathBuf,
            size: (u32, u32),
            duration_ms: u32,
            frame: Option<(u32, f32)>,
            placeholder: &'static str,
        }
        let video = |name: &str, size, duration_ms, frame, step, placeholder| Row {
            file: asset(name),
            size,
            duration_ms,
            frame: Some((frame, step)),
            placeholder,
        };
        let audio = |file: std::path::PathBuf, duration_ms| Row { file, size: (0, 0), duration_ms, frame: None, placeholder: "" };
        let rows = [
            video("rot90.mp4", (90, 160), 1003, 2, 8., "BwgGBADvDCDkzUOUeWgAAAAAAA=="),
            video("sar.webm", (240, 90), 2000, 5, 4., "CAgGA4DvnGjvvdnNAAAAAAA="),
            video("noaudio.mp4", (160, 90), 3000, 2, 8., "BwgGBIDvnGj/vYnevLwAAAAAAA=="),
            video("ends2.mp4", (64, 36), 2005, 5, 3., "BggGBIDvrHj/zYrevLwAAAAAAA=="),
            // Electron: 2517 (its demuxer happened to read this 5 KB clip to its end).
            video("ends25.mp4", (64, 36), 2500, 6, 3., "BwgGBIDvrHj/zYrevLwAAAAAAA=="),
            video("vp9.webm", (120, 90), 2000, 5, 4., "CAgGBYDvnGj/vYnevMm7mQAAAAAA"),
            audio(asset("tone1.m4a"), 1000),
            audio(media_asset("cbr.mp3"), 2040),
            audio(media_asset("vbr_noxing.mp3"), 2029),
        ];
        for row in rows {
            let name = row.file.file_name().unwrap().to_string_lossy().into_owned();
            let kind = if row.frame.is_some() { AttachmentKind::Video } else { AttachmentKind::Audio };
            let m = probe(ProbeSource::Path(&row.file), kind, STEP).unwrap_or_else(|| panic!("{name}: probed"));
            assert_eq!(((m.width, m.height), m.duration_ms), (row.size, row.duration_ms), "{name}");
            let Some((frame, step)) = row.frame else {
                assert!(m.poster.is_none(), "{name}");
                continue;
            };
            let poster = m.poster.unwrap_or_else(|| panic!("{name}: a poster"));
            assert_eq!(frame_number(&poster, step), frame, "{name}: the poster's frame");
            let facts = crate::thumbs::poster_facts(poster, m.width, m.height);
            assert_eq!(facts.thumbnails.len(), 3, "{name}: thumbnails (mask 7)");
            let ours = crate::thumbhash::decode_b64(&facts.placeholder).expect("our ThumbHash");
            let theirs = crate::thumbhash::decode_b64(row.placeholder).expect("Electron's ThumbHash");
            assert_eq!((ours.width, ours.height), (theirs.width, theirs.height), "{name}: the ThumbHash's shape");
            let diff = ours.rgba.iter().zip(&theirs.rgba).map(|(a, b)| a.abs_diff(*b) as u32).sum::<u32>() as f32 / ours.rgba.len() as f32;
            assert!(diff <= 3., "{name}: the ThumbHash is {diff} levels from Electron's on average");
        }
    }

    #[test]
    fn pictures_and_documents_are_not_probed() {
        assert!(probe(ProbeSource::Path(&asset("rot90.mp4")), AttachmentKind::Image, STEP).is_none());
        assert!(probe(ProbeSource::Path(&asset("rot90.mp4")), AttachmentKind::Document, STEP).is_none());
    }

    #[test]
    fn the_renderers_arithmetic() {
        assert_eq!(poster_target(None), 500_000);
        assert_eq!(poster_target(Some(0)), 100_000);
        assert_eq!(poster_target(Some(1_000_000)), 100_000);
        assert_eq!(poster_target(Some(60_000_000)), 500_000);
        assert_eq!(finite_ms(None), 0);
        assert_eq!(finite_ms(Some(0)), 0);
        assert_eq!(finite_ms(Some(2_028_846)), 2029);
        assert_eq!(finite_ms(Some(1_499)), 1);
        assert_eq!(natural_size(120, 90, (2, 1)), (240, 90));
        assert_eq!(natural_size(120, 90, (1, 2)), (120, 180));
        assert_eq!(natural_size(1920, 1080, (0, 1)), (1920, 1080));
        assert_eq!(natural_size(720, 576, (16, 15)), (768, 576));
        // FFmpeg's degrees are counter-clockwise; a phone's portrait clip says -90.
        assert_eq!(quarter_turns(-90.), 1);
        assert_eq!(quarter_turns(90.), 3);
        assert_eq!(quarter_turns(180.), 2);
        assert_eq!(quarter_turns(-180.), 2);
        assert_eq!(quarter_turns(0.), 0);
        assert_eq!(quarter_turns(f64::NAN), 0);
    }
}

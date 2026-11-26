//! What a stream's decoder thread drives: a [`PictureDecoder`] made by the platform's
//! [`Backend`], and the FFmpeg plumbing both the hardware and the software decoders share.
//!
//! The software decoder lives here because it is the same on every platform: FFmpeg's own
//! H.264 / HEVC decoders or dav1d, handing each picture's planes to an [`Uploader`] (the
//! platform's way of getting them into textures).

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use ffmpeg_sys_next as ff;
use parking_lot::Mutex;

use crate::codec::{Codec, CodecSupport, DecodeBackend, DecodePath};
use crate::select::Order;
use crate::packet::{PADDING, Packet};
use crate::stats::Counters;

/// Why a decode or an open failed, as far as recovery cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The GPU device the decoder lives on is gone (or was replaced). Every decoder on it is
    /// dead; decode again once the renderer has a new device.
    DeviceLost,
    /// This decoder failed; another one (software) may work.
    Decoder,
}

#[derive(Debug)]
pub(crate) struct DecodeError {
    pub failure: Failure,
    pub message: String,
}

impl DecodeError {
    pub fn decoder(message: impl Into<String>) -> Self {
        Self {
            failure: Failure::Decoder,
            message: message.into(),
        }
    }

    pub fn device_lost(message: impl Into<String>) -> Self {
        Self {
            failure: Failure::DeviceLost,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

pub(crate) type DecodeResult<T> = Result<T, DecodeError>;

/// One row of a published picture's luma plane, read back from where the renderer samples it.
/// For tests: the test streams carry their frame number as a barcode in the top rows.
#[derive(Clone, Debug)]
pub struct LumaRow {
    /// The serial of the frame (1-based count of packets handed to this stream's decoders).
    pub serial: u64,
    pub width: u32,
    pub height: u32,
    pub row: u32,
    pub bytes: Vec<u8>,
}

/// A pending request for a [`LumaRow`] of the next published picture.
#[derive(Default)]
pub(crate) struct CaptureCell {
    wanted: AtomicBool,
    request: Mutex<Option<(f32, mpsc::Sender<LumaRow>)>>,
}

impl CaptureCell {
    pub fn request(&self, y_fraction: f32) -> mpsc::Receiver<LumaRow> {
        let (sender, receiver) = mpsc::channel();
        *self.request.lock() = Some((y_fraction, sender));
        self.wanted.store(true, Ordering::Release);
        receiver
    }

    /// The row to read from a picture `height` rows tall, when a capture is pending.
    pub fn take(&self, height: u32) -> Option<(u32, mpsc::Sender<LumaRow>)> {
        if !self.wanted.swap(false, Ordering::AcqRel) {
            return None;
        }
        let (y_fraction, sender) = self.request.lock().take()?;
        let row = ((height as f32 * y_fraction) as u32).min(height.saturating_sub(1));
        Some((row, sender))
    }
}

/// What a decoder needs to know about the frame it is given.
pub(crate) struct FrameCx<'a> {
    pub counters: &'a Counters,
    /// The frame's serial: the tag its picture is published with.
    pub serial: u64,
    pub capture: &'a CaptureCell,
}

pub(crate) trait PictureDecoder: Send {
    fn backend(&self) -> DecodeBackend;
    fn path(&self) -> DecodePath {
        self.backend().path()
    }
    /// Decodes one whole encoded frame and hands the pictures it yields to the stream's
    /// surface. Returns how many were handed over (0 or 1: streams have no reordering).
    fn decode(
        &mut self,
        packet: Packet,
        keyframe: bool,
        pts: i64,
        cx: &FrameCx<'_>,
    ) -> DecodeResult<u32>;
}

/// What the hub asks of the platform.
pub(crate) trait Backend: Send + Sync + 'static {
    /// The decoders this machine could use, best first (`select.rs` walks it). Software only
    /// while the renderer has no device.
    fn order(&self) -> Order;
    /// Opens `backend`'s decoder for a stream. An error that is not a device loss means this
    /// decoder does not work for the codec here.
    fn open(
        &self,
        backend: DecodeBackend,
        codec: Codec,
        description: Option<&[u8]>,
        target: &crate::hub::StreamTarget,
    ) -> DecodeResult<Box<dyn PictureDecoder>>;
    /// Whether a decoder can be opened now (false between a device loss and the renderer's
    /// new device).
    fn ready(&self) -> bool;
    /// Makes the renderer abandon its device as after a real loss (tests).
    fn simulate_device_loss(&self);
    /// What `backend` would do with `codec` as far as can be told without opening a decoder
    /// (the driver's lists, the FFmpeg build): `path` is `None` when it certainly cannot.
    fn support(&self, backend: DecodeBackend, codec: Codec) -> CodecSupport;
    /// Names the GPU and its driver version: what a probe's result is valid for. `None`: the
    /// renderer has no device yet.
    fn device_key(&self) -> Option<String>;
    /// Plane textures alive in the process (tests: that a closed stream frees them).
    fn live_textures(&self) -> i64;
    /// Lets the renderer's device notice which of its submissions the GPU has finished, so the
    /// pictures they sampled are released. Does not wait.
    fn collect(&self);
}

pub(crate) fn av_error(code: c_int) -> String {
    let mut buffer = [0 as c_char; 128];
    // Safety: the buffer is writable for its length; av_strerror terminates it.
    unsafe {
        ff::av_strerror(code, buffer.as_mut_ptr(), buffer.len());
        CStr::from_ptr(buffer.as_ptr())
            .to_string_lossy()
            .into_owned()
    }
}

/// Whether FFmpeg was built with a software decoder for `codec`.
pub(crate) fn has_software_decoder(codec: Codec) -> bool {
    !find_decoder(codec, false).is_null()
}

/// What the software decoders do with `codec`.
pub(crate) fn software_support(codec: Codec) -> CodecSupport {
    let has = has_software_decoder(codec);
    CodecSupport {
        codec,
        path: has.then_some(DecodePath::Software),
        backend: has.then_some(DecodeBackend::Software),
        probed: false,
        max_size: None,
    }
}

fn find_decoder(codec: Codec, hardware: bool) -> *const ff::AVCodec {
    // Safety: lookups in FFmpeg's static codec table.
    unsafe {
        match codec {
            Codec::H264 => ff::avcodec_find_decoder(ff::AVCodecID::AV_CODEC_ID_H264),
            Codec::Hevc => ff::avcodec_find_decoder(ff::AVCodecID::AV_CODEC_ID_HEVC),
            // FFmpeg's own AV1 decoder only decodes through a hwaccel; dav1d is the software one.
            Codec::Av1 if hardware => ff::avcodec_find_decoder_by_name(c"av1".as_ptr()),
            Codec::Av1 => ff::avcodec_find_decoder_by_name(c"libdav1d".as_ptr()),
        }
    }
}

/// FFmpeg's log level follows `DECIBELL_VIDEO_FFMPEG_LOG` (a number, FFmpeg's scale: 16 error,
/// 24 warning, 32 info, 40 verbose); errors only by default. Set once, at the first open.
fn init_logging() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let level = std::env::var("DECIBELL_VIDEO_FFMPEG_LOG")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(16);
        // Safety: a global integer in libavutil.
        unsafe { ff::av_log_set_level(level) };
    });
}

unsafe extern "C" fn free_packet(opaque: *mut c_void, _data: *mut u8) {
    // Safety: `opaque` is the carrier `send` handed to av_buffer_create, freed once.
    unsafe { Packet::free_raw(opaque) };
}

/// An open `AVCodecContext` with the one packet and one frame it is driven through.
pub(crate) struct FfDecoder {
    context: *mut ff::AVCodecContext,
    packet: *mut ff::AVPacket,
    pub frame: *mut ff::AVFrame,
}

// Safety: used by one decoder thread at a time; FFmpeg contexts are not thread-affine.
unsafe impl Send for FfDecoder {}

pub(crate) type GetFormat =
    unsafe extern "C" fn(*mut ff::AVCodecContext, *const ff::AVPixelFormat) -> ff::AVPixelFormat;

/// The body of a hardware decoder's `get_format`: `wanted` or nothing (failing the stream's
/// first packet, so that it moves to the next decoder instead of FFmpeg decoding on the CPU
/// behind our back).
///
/// # Safety
/// `formats` is the list FFmpeg passes to `get_format`, terminated by `AV_PIX_FMT_NONE`.
pub(crate) unsafe fn pick_format(
    formats: *const ff::AVPixelFormat,
    wanted: ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // Safety: the caller's contract; read as integers (a value this FFmpeg knows and the
    // bindings do not must not become an invalid Rust enum).
    unsafe {
        let mut format = formats.cast::<c_int>();
        while *format != -1 {
            if *format == wanted as c_int {
                return wanted;
            }
            format = format.add(1);
        }
    }
    ff::AVPixelFormat::AV_PIX_FMT_NONE
}

impl FfDecoder {
    /// `hardware`: the device context to decode on and the `get_format` that picks its pixel
    /// format; `None` decodes in software on `threads` slice / tile threads. `description` is
    /// the stream's out-of-band configuration record (hvcC / av1C / avcC), when it has one.
    pub fn open(
        codec: Codec,
        description: Option<&[u8]>,
        hardware: Option<(*mut ff::AVBufferRef, GetFormat)>,
        threads: usize,
    ) -> DecodeResult<Self> {
        init_logging();
        let decoder = find_decoder(codec, hardware.is_some());
        if decoder.is_null() {
            return Err(DecodeError::decoder(format!(
                "FFmpeg has no {} decoder for {}",
                if hardware.is_some() {
                    "hardware"
                } else {
                    "software"
                },
                codec.label()
            )));
        }
        // Safety: plain FFmpeg object setup; every pointer is checked before use and freed on
        // the error paths by `Drop`.
        unsafe {
            let context = ff::avcodec_alloc_context3(decoder);
            if context.is_null() {
                return Err(DecodeError::decoder("avcodec_alloc_context3 failed"));
            }
            let mut this = Self {
                context,
                packet: std::ptr::null_mut(),
                frame: std::ptr::null_mut(),
            };
            // Streams have no reordering: every packet in gives its picture out.
            (*context).flags |= ff::AV_CODEC_FLAG_LOW_DELAY as c_int;
            (*context).pkt_timebase = ff::AVRational {
                num: 1,
                den: 1_000_000,
            };
            if let Some(description) = description.filter(|bytes| !bytes.is_empty()) {
                let extradata = ff::av_mallocz(description.len() + PADDING).cast::<u8>();
                if extradata.is_null() {
                    return Err(DecodeError::decoder("out of memory"));
                }
                std::ptr::copy_nonoverlapping(description.as_ptr(), extradata, description.len());
                (*context).extradata = extradata;
                (*context).extradata_size = description.len() as c_int;
            }
            match hardware {
                Some((device, get_format)) => {
                    (*context).hw_device_ctx = ff::av_buffer_ref(device);
                    (*context).get_format = Some(get_format);
                    (*context).thread_count = 1;
                }
                None => {
                    (*context).thread_count = threads.max(1) as c_int;
                    // Frame threads would add a frame of delay per thread.
                    (*context).thread_type = ff::FF_THREAD_SLICE as c_int;
                }
            }
            let opened = ff::avcodec_open2(context, decoder, std::ptr::null_mut());
            if opened < 0 {
                return Err(DecodeError::decoder(format!(
                    "opening the {} decoder: {}",
                    codec.label(),
                    av_error(opened)
                )));
            }
            this.packet = ff::av_packet_alloc();
            this.frame = ff::av_frame_alloc();
            if this.packet.is_null() || this.frame.is_null() {
                return Err(DecodeError::decoder("out of memory"));
            }
            Ok(this)
        }
    }

    /// Hands one encoded frame to the decoder. The packet's buffer is not copied: FFmpeg holds
    /// it by reference and its owner is dropped when FFmpeg is done with it.
    pub fn send(&mut self, packet: Packet, keyframe: bool, pts: i64) -> DecodeResult<()> {
        let (data, len, opaque) = packet.into_raw();
        // Safety: `self.packet` is a blank packet; the buffer is alive until `free_packet`.
        unsafe {
            let buffer = ff::av_buffer_create(
                data,
                len,
                Some(free_packet),
                opaque,
                ff::AV_BUFFER_FLAG_READONLY as c_int,
            );
            if buffer.is_null() {
                Packet::free_raw(opaque);
                return Err(DecodeError::decoder("out of memory"));
            }
            let packet = self.packet;
            (*packet).buf = buffer;
            (*packet).data = data;
            (*packet).size = len as c_int;
            (*packet).pts = pts;
            (*packet).dts = pts;
            (*packet).flags = if keyframe {
                ff::AV_PKT_FLAG_KEY as c_int
            } else {
                0
            };
            let sent = ff::avcodec_send_packet(self.context, packet);
            // Drops our reference; the decoder keeps its own for as long as it reads the bytes.
            ff::av_packet_unref(packet);
            if sent < 0 {
                return Err(DecodeError::decoder(format!(
                    "avcodec_send_packet: {}",
                    av_error(sent)
                )));
            }
        }
        Ok(())
    }

    /// The next decoded picture into `self.frame` (unref it when done). False: none ready.
    pub fn receive(&mut self) -> DecodeResult<bool> {
        // Safety: context and frame are valid for the decoder's life.
        let received = unsafe { ff::avcodec_receive_frame(self.context, self.frame) };
        if received == ff::AVERROR(ff::EAGAIN) || received == ff::AVERROR_EOF {
            return Ok(false);
        }
        if received < 0 {
            return Err(DecodeError::decoder(format!(
                "avcodec_receive_frame: {}",
                av_error(received)
            )));
        }
        Ok(true)
    }

    pub fn unref_frame(&mut self) {
        // Safety: the frame is valid; unref of an empty frame is a no-op.
        unsafe { ff::av_frame_unref(self.frame) };
    }

    /// What the frame in `self.frame` is.
    pub fn frame_info(&self) -> FrameInfo {
        // Safety: the frame is valid. Enum fields are read as integers: a value this FFmpeg
        // knows and the bindings do not must not become an invalid Rust enum.
        unsafe {
            let frame = self.frame;
            let int = |field: *const c_int| field.read();
            let format = int(std::ptr::addr_of!((*frame).format));
            let (sw_format, coded) = if (*frame).hw_frames_ctx.is_null() {
                (format, ((*frame).width, (*frame).height))
            } else {
                let frames = (*(*frame).hw_frames_ctx)
                    .data
                    .cast::<ff::AVHWFramesContext>();
                (
                    int(std::ptr::addr_of!((*frames).sw_format).cast()),
                    ((*frames).width, (*frames).height),
                )
            };
            let colorspace = int(std::ptr::addr_of!((*frame).colorspace).cast());
            let range = int(std::ptr::addr_of!((*frame).color_range).cast());
            let chroma_location = int(std::ptr::addr_of!((*frame).chroma_location).cast());
            let height = (*frame).height.max(0) as u32;
            FrameInfo {
                width: (*frame).width.max(0) as u32,
                height,
                coded_width: coded.0.max(0) as u32,
                coded_height: coded.1.max(0) as u32,
                format,
                sw_format,
                color: ColorInfo {
                    matrix: match colorspace {
                        5 | 6 => Matrix::Bt601,   // AVCOL_SPC_BT470BG, SMPTE170M
                        9 | 10 => Matrix::Bt2020, // AVCOL_SPC_BT2020_NCL, _CL
                        1 => Matrix::Bt709,
                        // Unspecified: what the streamer sends (BT.709 for HD).
                        _ if height < 720 => Matrix::Bt601,
                        _ => Matrix::Bt709,
                    },
                    full_range: range == 2, // AVCOL_RANGE_JPEG
                    // AVCHROMA_LOC_CENTER = 2; the encoders here produce left-sited chroma
                    // (or say nothing, which means the same).
                    chroma_centered: chroma_location == 2,
                },
                data: [(*frame).data[0], (*frame).data[1], (*frame).data[2]],
                linesize: [
                    (*frame).linesize[0],
                    (*frame).linesize[1],
                    (*frame).linesize[2],
                ],
            }
        }
    }
}

impl Drop for FfDecoder {
    fn drop(&mut self) {
        // Safety: each pointer is null or owned by this decoder.
        unsafe {
            ff::avcodec_free_context(&mut self.context);
            ff::av_packet_free(&mut self.packet);
            ff::av_frame_free(&mut self.frame);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Matrix {
    Bt601,
    Bt709,
    Bt2020,
}

/// A picture's colour description, as the frame carries it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColorInfo {
    pub matrix: Matrix,
    pub full_range: bool,
    pub chroma_centered: bool,
}

pub(crate) struct FrameInfo {
    /// The picture's size (the image behind it may be larger: coded size).
    pub width: u32,
    pub height: u32,
    /// The size of the image behind a hardware frame (its frames pool's): the picture is its
    /// top-left part.
    pub coded_width: u32,
    pub coded_height: u32,
    /// `AVPixelFormat` of the frame.
    pub format: c_int,
    /// For a hardware frame: what its image holds.
    pub sw_format: c_int,
    pub color: ColorInfo,
    pub data: [*mut u8; 3],
    pub linesize: [c_int; 3],
}

/// A software-decoded 8-bit 4:2:0 picture: where its planes are in memory.
pub(crate) struct SwPicture<'a> {
    pub width: u32,
    pub height: u32,
    pub color: ColorInfo,
    pub luma: Plane<'a>,
    pub chroma: Chroma<'a>,
}

#[derive(Clone, Copy)]
pub(crate) struct Plane<'a> {
    /// `stride * (rows - 1) + row_bytes` bytes.
    pub bytes: &'a [u8],
    pub stride: usize,
}

impl Plane<'_> {
    pub fn row(&self, y: usize, len: usize) -> &[u8] {
        &self.bytes[y * self.stride..y * self.stride + len]
    }
}

pub(crate) enum Chroma<'a> {
    /// U and V as separate half-size planes (FFmpeg's H.264 / HEVC decoders, dav1d).
    Planar(Plane<'a>, Plane<'a>),
    /// Interleaved U, V (NV12).
    Interleaved(Plane<'a>),
}

/// Gets software pictures to where the renderer samples them.
pub(crate) trait Uploader: Send {
    /// Uploads the picture and makes it the stream's newest. False: no slot was free.
    fn upload(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool>;
}

/// How many slice / tile threads a software decoder gets: enough for 1080p60 of any codec and
/// 1440p120 H.264 (docs/video-design.md §11), few enough that a fallback stream does not take
/// the machine.
pub(crate) fn software_threads() -> usize {
    std::thread::available_parallelism()
        .map(|cores| cores.get() / 2)
        .unwrap_or(2)
        .clamp(1, 4)
}

pub(crate) struct SoftwareDecoder {
    decoder: FfDecoder,
    uploader: Box<dyn Uploader>,
}

impl SoftwareDecoder {
    pub fn open(
        codec: Codec,
        description: Option<&[u8]>,
        uploader: Box<dyn Uploader>,
    ) -> DecodeResult<Self> {
        Ok(Self {
            decoder: FfDecoder::open(codec, description, None, software_threads())?,
            uploader,
        })
    }

    fn hand_over(&mut self, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        let info = self.decoder.frame_info();
        let planar = info.format == ff::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int
            || info.format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as c_int;
        let interleaved = info.format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int;
        if !planar && !interleaved {
            // 10-bit streams (P010 / YUV420P10) need 16-bit planes. The streamer sends 8-bit.
            return Err(DecodeError::decoder(format!(
                "unsupported picture format {} (only 8-bit 4:2:0)",
                info.format
            )));
        }
        let (width, height) = (info.width as usize, info.height as usize);
        if width == 0 || height == 0 || info.linesize.iter().take(2).any(|&stride| stride <= 0) {
            return Err(DecodeError::decoder(
                "the decoder returned an empty picture",
            ));
        }
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        // Safety: FFmpeg guarantees `linesize * rows` readable bytes per plane of a decoded
        // frame; the slices end at the last row's last pixel.
        let plane = |index: usize, rows: usize, row_bytes: usize| unsafe {
            let stride = info.linesize[index] as usize;
            Plane {
                bytes: std::slice::from_raw_parts(
                    info.data[index],
                    stride * (rows - 1) + row_bytes,
                ),
                stride,
            }
        };
        let mut color = info.color;
        color.full_range |= info.format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as c_int;
        let picture = SwPicture {
            width: info.width,
            height: info.height,
            color,
            luma: plane(0, height, width),
            chroma: if interleaved {
                Chroma::Interleaved(plane(1, chroma_height, chroma_width * 2))
            } else {
                Chroma::Planar(
                    plane(1, chroma_height, chroma_width),
                    plane(2, chroma_height, chroma_width),
                )
            },
        };
        cx.counters.set_size(info.width, info.height);
        self.uploader.upload(&picture, cx)
    }
}

impl PictureDecoder for SoftwareDecoder {
    fn backend(&self) -> DecodeBackend {
        DecodeBackend::Software
    }

    fn decode(
        &mut self,
        packet: Packet,
        keyframe: bool,
        pts: i64,
        cx: &FrameCx<'_>,
    ) -> DecodeResult<u32> {
        self.decoder.send(packet, keyframe, pts)?;
        let mut published = 0;
        while self.decoder.receive()? {
            cx.counters.frame_decoded(cx.serial);
            let handed = self.hand_over(cx);
            self.decoder.unref_frame();
            if handed? {
                published += 1;
            }
        }
        Ok(published)
    }
}

/// The frame number a test stream's picture carries: 16 cells across the top rows, most
/// significant bit first, white = 1 (`tools/streamgen`). `row` is a luma row inside the band.
pub fn barcode(row: &[u8]) -> u32 {
    let width = row.len();
    (0..16).fold(0, |number, cell| {
        let x = (cell * 2 + 1) * width / 32;
        number << 1 | (row[x] > 128) as u32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_barcode() {
        let width = 1920;
        let number = 0b1010_0000_1111_0101u32;
        let row: Vec<u8> = (0..width)
            .map(|x| {
                if (number >> (15 - x * 16 / width)) & 1 == 1 {
                    235
                } else {
                    16
                }
            })
            .collect();
        assert_eq!(barcode(&row), number);
    }

    #[test]
    fn every_codec_has_a_software_decoder() {
        for codec in Codec::ALL {
            assert!(has_software_decoder(codec), "{codec:?}");
        }
    }
}

//! One file, opened: its video and audio tracks and their packets in file order.

use std::ffi::c_int;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use decibell_video::{Codec, PADDING, Packet, PacketBytes};
use ffmpeg_sys_next as ff;
use parking_lot::Mutex;

use crate::ffi::{av_error, codec_name, from_micros, to_micros};
use crate::source::{Avio, Source};

/// Why a file cannot be played, as the card says it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The bytes could not be read (the network, the server, a sealed file that does not open).
    Read(String),
    /// Not a container this client plays, or damaged.
    Format(String),
    /// A container we read with a codec we do not decode ("mpeg4", "ac3", 10-bit …).
    Codec(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpenError::Read(reason) => write!(f, "{reason}"),
            OpenError::Format(reason) => write!(f, "This file can't be played ({reason})"),
            OpenError::Codec(reason) => write!(f, "Unsupported format ({reason})"),
        }
    }
}

/// The video track of a file.
#[derive(Clone, Debug)]
pub struct VideoTrack {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    /// Frames per second as the container says (0.0: unknown).
    pub frame_rate: f64,
    /// The codec's configuration record (avcC, hvcC, av1C), if the container has one.
    pub description: Option<Arc<[u8]>>,
    pub(crate) index: c_int,
    pub(crate) time_base: ff::AVRational,
}

/// The audio track of a file.
#[derive(Clone, Debug)]
pub struct AudioTrack {
    /// FFmpeg's name of the codec ("aac", "opus").
    pub codec: String,
    pub(crate) index: c_int,
    pub(crate) time_base: ff::AVRational,
}

/// A packet read from the file.
pub(crate) enum Read {
    Video {
        packet: Packet,
        keyframe: bool,
        /// Presentation time in microseconds from the start of the file.
        pts: i64,
    },
    /// The audio packet is in [`Demuxer::packet`] until the next read.
    Audio { pts: Option<i64> },
    End,
}

pub(crate) struct Demuxer {
    format: *mut ff::AVFormatContext,
    avio: Avio,
    packet: *mut ff::AVPacket,
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
    /// Microseconds; 0 when the container does not say (the renderer's `Infinity` guard).
    pub duration: i64,
    /// The container's start time: every time given out is relative to it.
    start: i64,
    pub container: String,
}

// Safety: owned and used by the player's one control thread.
unsafe impl Send for Demuxer {}

/// `AVPacket` shells, recycled: a video packet's buffer goes to the decoder inside one.
static SHELLS: Mutex<Vec<Shell>> = Mutex::new(Vec::new());

struct Shell(*mut ff::AVPacket);
// Safety: an AVPacket owns its buffer by reference count; nothing is thread-affine.
unsafe impl Send for Shell {}

/// A demuxed packet's bytes on their way to the decoder: FFmpeg's own buffer, not a copy.
struct Owned(Shell);

// Safety: `data` / `size` of a referenced packet do not move, and FFmpeg's demuxers allocate
// AV_INPUT_BUFFER_PADDING_SIZE zeroed bytes after every packet.
unsafe impl PacketBytes for Owned {
    fn bytes(&self) -> &[u8] {
        // Safety: the shell holds a reference to the buffer.
        unsafe {
            let packet = self.0.0;
            if (*packet).data.is_null() || (*packet).size <= 0 {
                return &[];
            }
            std::slice::from_raw_parts((*packet).data, (*packet).size as usize)
        }
    }

    fn zero_padding(&self) -> usize {
        PADDING
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // Safety: ours; unref releases the buffer, the shell is kept for the next packet.
        unsafe { ff::av_packet_unref(self.0.0) };
        let mut shells = SHELLS.lock();
        if shells.len() < 16 {
            shells.push(Shell(self.0.0));
        } else {
            drop(shells);
            unsafe { ff::av_packet_free(&mut self.0.0) };
        }
    }
}

fn video_codec(id: ff::AVCodecID) -> Option<Codec> {
    Some(match id {
        ff::AVCodecID::AV_CODEC_ID_H264 => Codec::H264,
        ff::AVCodecID::AV_CODEC_ID_HEVC => Codec::Hevc,
        ff::AVCodecID::AV_CODEC_ID_AV1 => Codec::Av1,
        ff::AVCodecID::AV_CODEC_ID_VP8 => Codec::Vp8,
        ff::AVCodecID::AV_CODEC_ID_VP9 => Codec::Vp9,
        _ => return None,
    })
}

impl Demuxer {
    /// Opens the file and finds its tracks. Reads the container's header (and, for formats
    /// without one, up to two seconds of packets).
    pub fn open(source: Box<dyn Source>, abort: Arc<AtomicBool>) -> Result<Demuxer, OpenError> {
        let mut avio =
            Avio::new(source, abort).ok_or_else(|| OpenError::Read("out of memory".into()))?;
        // Safety: plain FFmpeg object setup; every pointer is checked, and freed on the error
        // paths (avformat_open_input frees the context it was given when it fails).
        unsafe {
            let mut format = ff::avformat_alloc_context();
            if format.is_null() {
                return Err(OpenError::Read("out of memory".into()));
            }
            (*format).pb = avio.context;
            (*format).flags |= ff::AVFMT_FLAG_CUSTOM_IO as c_int;
            (*format).max_analyze_duration = 2_000_000;
            let opened = ff::avformat_open_input(
                &mut format,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            );
            if opened < 0 {
                return Err(match avio.take_error() {
                    Some(reason) => OpenError::Read(reason),
                    None => OpenError::Format(av_error(opened)),
                });
            }
            let mut this = Demuxer {
                format,
                avio,
                packet: ff::av_packet_alloc(),
                video: None,
                audio: None,
                duration: 0,
                start: 0,
                container: String::new(),
            };
            if this.packet.is_null() {
                return Err(OpenError::Read("out of memory".into()));
            }
            let found = ff::avformat_find_stream_info(format, std::ptr::null_mut());
            if found < 0 {
                return Err(match this.avio.take_error() {
                    Some(reason) => OpenError::Read(reason),
                    None => OpenError::Format(av_error(found)),
                });
            }
            if !(*format).iformat.is_null() {
                this.container = std::ffi::CStr::from_ptr((*(*format).iformat).name)
                    .to_string_lossy()
                    .into_owned();
            }

            let streams = std::slice::from_raw_parts((*format).streams, (*format).nb_streams as usize);
            let mut unsupported: Option<String> = None;
            for (index, &stream) in streams.iter().enumerate() {
                let parameters = (*stream).codecpar;
                (*stream).discard = ff::AVDiscard::AVDISCARD_ALL;
                let kind = (*parameters).codec_type;
                let id = (*parameters).codec_id;
                if kind == ff::AVMediaType::AVMEDIA_TYPE_VIDEO && this.video.is_none() {
                    // A cover picture (MP3, M4A) is not a video track.
                    if (*stream).disposition & ff::AV_DISPOSITION_ATTACHED_PIC as c_int != 0 {
                        continue;
                    }
                    let Some(codec) = video_codec(id) else {
                        unsupported.get_or_insert_with(|| codec_name(id));
                        continue;
                    };
                    let format = (*parameters).format;
                    let eight_bit = format == ff::AVPixelFormat::AV_PIX_FMT_YUV420P as c_int
                        || format == ff::AVPixelFormat::AV_PIX_FMT_YUVJ420P as c_int
                        || format == ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int
                        || format == ff::AVPixelFormat::AV_PIX_FMT_NONE as c_int;
                    if !eight_bit {
                        // The client's slots are 8-bit 4:2:0 (docs/video-design.md §5.2).
                        unsupported.get_or_insert_with(|| {
                            format!("{}, not 8-bit 4:2:0", codec_name(id))
                        });
                        continue;
                    }
                    let rate = if (*stream).avg_frame_rate.den != 0 && (*stream).avg_frame_rate.num != 0 {
                        (*stream).avg_frame_rate
                    } else {
                        (*stream).r_frame_rate
                    };
                    let extradata = if (*parameters).extradata.is_null() || (*parameters).extradata_size <= 0 {
                        None
                    } else {
                        Some(Arc::from(std::slice::from_raw_parts(
                            (*parameters).extradata,
                            (*parameters).extradata_size as usize,
                        )))
                    };
                    (*stream).discard = ff::AVDiscard::AVDISCARD_DEFAULT;
                    this.video = Some(VideoTrack {
                        codec,
                        width: (*parameters).width.max(0) as u32,
                        height: (*parameters).height.max(0) as u32,
                        frame_rate: if rate.den != 0 { rate.num as f64 / rate.den as f64 } else { 0.0 },
                        description: extradata,
                        index: index as c_int,
                        time_base: (*stream).time_base,
                    });
                } else if kind == ff::AVMediaType::AVMEDIA_TYPE_AUDIO && this.audio.is_none() {
                    if ff::avcodec_find_decoder(id).is_null() {
                        unsupported.get_or_insert_with(|| codec_name(id));
                        continue;
                    }
                    (*stream).discard = ff::AVDiscard::AVDISCARD_DEFAULT;
                    this.audio = Some(AudioTrack {
                        codec: codec_name(id),
                        index: index as c_int,
                        time_base: (*stream).time_base,
                    });
                }
            }
            if this.video.is_none() && this.audio.is_none() {
                return Err(match unsupported {
                    Some(codec) => OpenError::Codec(codec),
                    None => OpenError::Format("no video or audio in it".into()),
                });
            }
            if this.video.is_none()
                && let Some(codec) = unsupported
                && streams.iter().any(|&stream| {
                    (*(*stream).codecpar).codec_type == ff::AVMediaType::AVMEDIA_TYPE_VIDEO
                        && (*stream).disposition & ff::AV_DISPOSITION_ATTACHED_PIC as c_int == 0
                })
            {
                // A video whose picture we cannot decode is not played as sound alone.
                return Err(OpenError::Codec(codec));
            }
            if (*format).start_time != ff::AV_NOPTS_VALUE {
                this.start = (*format).start_time;
            }
            if (*format).duration != ff::AV_NOPTS_VALUE && (*format).duration > 0 {
                this.duration = (*format).duration;
            }
            Ok(this)
        }
    }

    /// The audio track's parameters, time base and the container's start time, for its decoder.
    pub(crate) fn audio_parameters(&self) -> Option<(*const ff::AVCodecParameters, ff::AVRational, i64)> {
        let audio = self.audio.as_ref()?;
        // Safety: the index is one of this context's streams.
        unsafe {
            let stream = *(*self.format).streams.add(audio.index as usize);
            Some(((*stream).codecpar.cast_const(), audio.time_base, self.start))
        }
    }

    /// Stops reading a track nobody plays (an audio attachment's video track).
    pub(crate) fn drop_video(&mut self) {
        if let Some(video) = self.video.take() {
            // Safety: the index is one of this context's streams.
            unsafe {
                let stream = *(*self.format).streams.add(video.index as usize);
                (*stream).discard = ff::AVDiscard::AVDISCARD_ALL;
            }
        }
    }

    /// The audio packet of the last [`Read::Audio`].
    pub(crate) fn packet(&self) -> *mut ff::AVPacket {
        self.packet
    }

    /// The next packet of the two tracks, in file order.
    pub fn read(&mut self) -> Result<Read, OpenError> {
        // Safety: the context and the packet are valid; a packet is unreferenced before reuse.
        unsafe {
            loop {
                ff::av_packet_unref(self.packet);
                let read = ff::av_read_frame(self.format, self.packet);
                if read == ff::AVERROR_EOF {
                    return Ok(Read::End);
                }
                if read < 0 {
                    return Err(match self.avio.take_error() {
                        Some(reason) => OpenError::Read(reason),
                        // A read that fails without the source failing is the end of what the
                        // file has (a truncated upload): play what was there.
                        None if read == ff::AVERROR(ff::EIO) => return Ok(Read::End),
                        None => OpenError::Format(av_error(read)),
                    });
                }
                let packet = self.packet;
                let index = (*packet).stream_index;
                let stamp = if (*packet).pts != ff::AV_NOPTS_VALUE {
                    Some((*packet).pts)
                } else if (*packet).dts != ff::AV_NOPTS_VALUE {
                    Some((*packet).dts)
                } else {
                    None
                };
                if let Some(video) = &self.video
                    && video.index == index
                {
                    let pts = stamp.map_or(0, |stamp| to_micros(stamp, video.time_base) - self.start);
                    let keyframe = (*packet).flags & ff::AV_PKT_FLAG_KEY as c_int != 0;
                    if (*packet).size <= 0 {
                        continue;
                    }
                    let shell = SHELLS.lock().pop().map_or_else(|| ff::av_packet_alloc(), |shell| shell.0);
                    if shell.is_null() {
                        return Err(OpenError::Read("out of memory".into()));
                    }
                    ff::av_packet_move_ref(shell, packet);
                    return Ok(Read::Video {
                        packet: Packet::new(Owned(Shell(shell))),
                        keyframe,
                        pts,
                    });
                }
                if let Some(audio) = &self.audio
                    && audio.index == index
                {
                    return Ok(Read::Audio {
                        pts: stamp.map(|stamp| to_micros(stamp, audio.time_base) - self.start),
                    });
                }
            }
        }
    }

    /// Moves to the last keyframe at or before `micros` (of the video track when there is one:
    /// the audio follows from there and is trimmed to the target by its decoder).
    pub fn seek(&mut self, micros: i64) -> Result<(), OpenError> {
        let target = micros.max(0) + self.start;
        // Safety: the context is valid.
        unsafe {
            let (index, stamp) = match (&self.video, &self.audio) {
                (Some(video), _) => (video.index, from_micros(target, video.time_base)),
                (None, Some(audio)) => (audio.index, from_micros(target, audio.time_base)),
                (None, None) => (-1, target),
            };
            let mut sought = ff::av_seek_frame(self.format, index, stamp, ff::AVSEEK_FLAG_BACKWARD as c_int);
            if sought < 0 {
                // Formats without an index (ADTS, a headerless MP3) seek by bytes or not at
                // all: the start always works.
                sought = ff::av_seek_frame(self.format, -1, target, ff::AVSEEK_FLAG_BACKWARD as c_int);
            }
            if sought < 0 && micros <= 0 {
                sought = ff::avformat_seek_file(self.format, -1, i64::MIN, self.start, i64::MAX, 0);
            }
            if sought < 0 {
                return Err(match self.avio.take_error() {
                    Some(reason) => OpenError::Read(reason),
                    None => OpenError::Format(format!("seek: {}", av_error(sought))),
                });
            }
        }
        Ok(())
    }
}

impl Drop for Demuxer {
    fn drop(&mut self) {
        // Safety: ours. The custom I/O context outlives the format context (`avio` is dropped
        // after this body).
        unsafe {
            ff::av_packet_free(&mut self.packet);
            ff::avformat_close_input(&mut self.format);
        }
    }
}

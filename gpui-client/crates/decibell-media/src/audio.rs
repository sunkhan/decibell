//! A file's audio track decoded and resampled to what the device plays: interleaved stereo
//! `f32` at the device's rate.

use std::ffi::c_int;

use ffmpeg_sys_next as ff;

use crate::demux::OpenError;
use crate::ffi::av_error;
use crate::render::CHANNELS;

pub(crate) struct AudioDecoder {
    context: *mut ff::AVCodecContext,
    frame: *mut ff::AVFrame,
    resampler: *mut ff::SwrContext,
    /// What the resampler was set up for: a frame that differs gets a new one.
    input: (c_int, c_int, c_int),
    out_rate: u32,
    /// Resampled frames, reused: grows to the largest decoded frame once.
    scratch: Vec<f32>,
    /// Media time (microseconds) of the next output frame, once known.
    next_pts: Option<i64>,
    /// Output frames still to drop before the seek target is reached.
    skip: u64,
    /// Drop everything before this media time (an accurate seek).
    trim_to: Option<i64>,
    time_base: ff::AVRational,
    /// The container's start time (microseconds): media times are relative to it.
    start: i64,
}

// Safety: owned and used by the player's one control thread.
unsafe impl Send for AudioDecoder {}

/// What one decoded frame gave.
pub(crate) struct Decoded<'a> {
    /// Interleaved stereo at the output rate.
    pub samples: &'a [f32],
    /// Media time of the first sample.
    pub pts: i64,
}

impl AudioDecoder {
    pub fn open(
        parameters: *const ff::AVCodecParameters,
        time_base: ff::AVRational,
        start: i64,
        out_rate: u32,
    ) -> Result<AudioDecoder, OpenError> {
        // Safety: plain FFmpeg object setup; freed by `Drop` on the error paths.
        unsafe {
            let codec = ff::avcodec_find_decoder((*parameters).codec_id);
            if codec.is_null() {
                return Err(OpenError::Codec(crate::ffi::codec_name((*parameters).codec_id)));
            }
            let context = ff::avcodec_alloc_context3(codec);
            if context.is_null() {
                return Err(OpenError::Read("out of memory".into()));
            }
            let mut this = AudioDecoder {
                context,
                frame: std::ptr::null_mut(),
                resampler: std::ptr::null_mut(),
                input: (0, 0, -1),
                out_rate,
                scratch: Vec::new(),
                next_pts: None,
                skip: 0,
                trim_to: None,
                time_base,
                start,
            };
            let copied = ff::avcodec_parameters_to_context(context, parameters);
            if copied < 0 {
                return Err(OpenError::Format(av_error(copied)));
            }
            (*context).thread_count = 1;
            (*context).pkt_timebase = time_base;
            let opened = ff::avcodec_open2(context, codec, std::ptr::null_mut());
            if opened < 0 {
                return Err(OpenError::Codec(format!(
                    "{}: {}",
                    crate::ffi::codec_name((*parameters).codec_id),
                    av_error(opened)
                )));
            }
            this.frame = ff::av_frame_alloc();
            if this.frame.is_null() {
                return Err(OpenError::Read("out of memory".into()));
            }
            Ok(this)
        }
    }

    /// A seek: forget what is buffered; the next samples are trimmed so that the first one
    /// given out is the one at `target` (microseconds).
    pub fn reset(&mut self, target: i64) {
        // Safety: the context is open.
        unsafe {
            ff::avcodec_flush_buffers(self.context);
            if !self.resampler.is_null() {
                ff::swr_free(&mut self.resampler);
            }
        }
        self.input = (0, 0, -1);
        self.next_pts = None;
        self.skip = 0;
        self.trim_to = Some(target);
    }

    /// Hands the decoder a packet (`null`: the end of the file).
    pub fn send(&mut self, packet: *const ff::AVPacket) -> Result<(), String> {
        // Safety: the context is open; the packet is the demuxer's or null.
        let sent = unsafe { ff::avcodec_send_packet(self.context, packet) };
        if sent < 0 && sent != ff::AVERROR_EOF && sent != ff::AVERROR(ff::EAGAIN) {
            // Damaged packets happen (a truncated upload): the decoder resynchronises.
            if sent == ff::AVERROR_INVALIDDATA {
                return Ok(());
            }
            return Err(av_error(sent));
        }
        Ok(())
    }

    /// The next decoded frame, resampled; `None` when the decoder wants another packet.
    /// `packet_pts`: the media time of the packet last sent, for a frame that carries none.
    pub fn receive(&mut self, packet_pts: Option<i64>) -> Result<Option<Decoded<'_>>, String> {
        // Safety: context, frame and resampler are ours; buffers are sized before each call.
        unsafe {
            loop {
                let received = ff::avcodec_receive_frame(self.context, self.frame);
                if received == ff::AVERROR(ff::EAGAIN) || received == ff::AVERROR_EOF {
                    return Ok(None);
                }
                if received == ff::AVERROR_INVALIDDATA {
                    continue;
                }
                if received < 0 {
                    return Err(av_error(received));
                }
                let frame = self.frame;
                let samples = (*frame).nb_samples;
                let rate = (*frame).sample_rate;
                let channels = (*frame).ch_layout.nb_channels;
                let format = std::ptr::addr_of!((*frame).format).read();
                if samples <= 0 || rate <= 0 || channels <= 0 {
                    ff::av_frame_unref(frame);
                    continue;
                }
                if self.input != (rate, channels, format) {
                    self.configure(rate, channels, format)?;
                }
                // Where this frame is in the file: its own time when it has one, else it
                // follows the last.
                let frame_pts = if (*frame).pts != ff::AV_NOPTS_VALUE {
                    Some(crate::ffi::to_micros((*frame).pts, self.time_base) - self.start)
                } else {
                    None
                };
                if self.next_pts.is_none() {
                    let pts = frame_pts.or(packet_pts).unwrap_or(0);
                    self.next_pts = Some(pts);
                    if let Some(target) = self.trim_to.take()
                        && target > pts
                    {
                        self.skip = (target - pts) as u64 * self.out_rate as u64 / 1_000_000;
                    }
                }
                let capacity = ff::swr_get_out_samples(self.resampler, samples).max(0) as usize;
                if self.scratch.len() < capacity * CHANNELS {
                    self.scratch.resize(capacity * CHANNELS, 0.0);
                }
                let out = self.scratch.as_mut_ptr().cast::<u8>();
                let converted = ff::swr_convert(
                    self.resampler,
                    &out,
                    capacity as c_int,
                    (*frame).extended_data.cast::<*const u8>(),
                    samples,
                );
                ff::av_frame_unref(frame);
                if converted < 0 {
                    return Err(av_error(converted));
                }
                let mut produced = converted as usize;
                if self.input.1 == 1 {
                    // One channel in: the same sample on both sides at its own level, as
                    // Chromium plays a mono file (the resampler's own up-mix would take 3 dB
                    // off). Resampled as mono, spread here, back to front in place.
                    for frame in (0..produced).rev() {
                        let sample = self.scratch[frame];
                        self.scratch[frame * CHANNELS] = sample;
                        self.scratch[frame * CHANNELS + 1] = sample;
                    }
                }
                let mut from = 0;
                let mut pts = self.next_pts.unwrap_or(0);
                self.next_pts = Some(pts + produced as i64 * 1_000_000 / self.out_rate as i64);
                if self.skip > 0 {
                    let dropped = (self.skip as usize).min(produced);
                    self.skip -= dropped as u64;
                    from = dropped;
                    produced -= dropped;
                    pts += dropped as i64 * 1_000_000 / self.out_rate as i64;
                }
                if produced == 0 {
                    continue;
                }
                return Ok(Some(Decoded {
                    samples: &self.scratch[from * CHANNELS..(from + produced) * CHANNELS],
                    pts,
                }));
            }
        }
    }

    unsafe fn configure(&mut self, rate: c_int, channels: c_int, format: c_int) -> Result<(), String> {
        // Safety: the caller holds a decoded frame in `self.frame`.
        unsafe {
            if !self.resampler.is_null() {
                ff::swr_free(&mut self.resampler);
            }
            let mut input = std::mem::zeroed::<ff::AVChannelLayout>();
            let frame_layout = std::ptr::addr_of!((*self.frame).ch_layout);
            if (*frame_layout).order == ff::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC {
                ff::av_channel_layout_default(&mut input, channels);
            } else {
                ff::av_channel_layout_copy(&mut input, frame_layout);
            }
            let mut output = std::mem::zeroed::<ff::AVChannelLayout>();
            // Mono stays mono through the resampler (see `receive`).
            ff::av_channel_layout_default(&mut output, if channels == 1 { 1 } else { CHANNELS as c_int });
            let made = ff::swr_alloc_set_opts2(
                &mut self.resampler,
                &output,
                ff::AVSampleFormat::AV_SAMPLE_FMT_FLT,
                self.out_rate as c_int,
                &input,
                std::mem::transmute::<c_int, ff::AVSampleFormat>(format),
                rate,
                0,
                std::ptr::null_mut(),
            );
            ff::av_channel_layout_uninit(&mut input);
            if made < 0 || self.resampler.is_null() {
                return Err(format!("resampler: {}", av_error(made)));
            }
            let started = ff::swr_init(self.resampler);
            if started < 0 {
                return Err(format!("resampler: {}", av_error(started)));
            }
            self.input = (rate, channels, format);
            Ok(())
        }
    }
}

impl Drop for AudioDecoder {
    fn drop(&mut self) {
        // Safety: each pointer is null or ours.
        unsafe {
            ff::swr_free(&mut self.resampler);
            ff::av_frame_free(&mut self.frame);
            ff::avcodec_free_context(&mut self.context);
        }
    }
}

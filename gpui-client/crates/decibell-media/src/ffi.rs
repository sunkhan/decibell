//! Small FFmpeg helpers shared by the demuxer and the audio decoder.

use std::ffi::{CStr, c_char, c_int};

use ffmpeg_sys_next as ff;

pub(crate) const MICROS: ff::AVRational = ff::AVRational {
    num: 1,
    den: 1_000_000,
};

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

/// `value` in `base` units as microseconds (rounded to nearest).
pub(crate) fn to_micros(value: i64, base: ff::AVRational) -> i64 {
    // Safety: pure arithmetic.
    unsafe { ff::av_rescale_q(value, base, MICROS) }
}

pub(crate) fn from_micros(micros: i64, base: ff::AVRational) -> i64 {
    // Safety: pure arithmetic.
    unsafe { ff::av_rescale_q(micros, MICROS, base) }
}

/// FFmpeg's name of a codec ("h264", "mpeg4"), for messages.
pub(crate) fn codec_name(id: ff::AVCodecID) -> String {
    // Safety: returns a static string for any id.
    unsafe {
        CStr::from_ptr(ff::avcodec_get_name(id))
            .to_string_lossy()
            .into_owned()
    }
}

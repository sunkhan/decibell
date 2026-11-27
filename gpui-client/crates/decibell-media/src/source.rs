//! Where a player's bytes come from, and the FFmpeg I/O context over it.
//!
//! A [`Source`] is anything that reads and seeks: the app gives the player
//! `decibell_attachments::media::MediaReader::blocking()` (range requests on demand, sealed
//! attachments decrypted in memory, 256 KiB read ahead; nothing on disk), the tests a
//! `Cursor`. FFmpeg reads it through a custom `AVIOContext` with a 64 KiB buffer: no URL, no
//! protocol, no temp file.

use std::ffi::{c_int, c_void};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ffmpeg_sys_next as ff;

/// A player's bytes. `Seek::seek(SeekFrom::End(0))` must answer with the size.
pub trait Source: Read + Seek + Send + 'static {}
impl<T: Read + Seek + Send + 'static> Source for T {}

/// The bytes FFmpeg buffers per read of the source.
pub const IO_BUFFER: usize = 64 * 1024;

struct Io {
    source: Box<dyn Source>,
    /// The read or seek error FFmpeg only sees as "I/O error".
    error: Option<String>,
    /// The player is closing: reads end at once.
    abort: Arc<AtomicBool>,
}

/// An `AVIOContext` reading a [`Source`].
pub(crate) struct Avio {
    pub context: *mut ff::AVIOContext,
    io: *mut Io,
}

// Safety: used by the one thread that owns the demuxer.
unsafe impl Send for Avio {}

unsafe extern "C" fn read_packet(opaque: *mut c_void, buffer: *mut u8, size: c_int) -> c_int {
    // Safety: `opaque` is the `Io` this context was made with; FFmpeg gives a writable buffer.
    let (io, buffer) = unsafe {
        (
            &mut *opaque.cast::<Io>(),
            std::slice::from_raw_parts_mut(buffer, size.max(0) as usize),
        )
    };
    if io.abort.load(Ordering::Relaxed) {
        return ff::AVERROR_EXIT;
    }
    // A panic must not cross into C; the read fails and the player shows why (and the panic hook
    // reports it as recovered: `decibell_panic::catch` marks the section).
    let read = decibell_panic::catch(|| io.source.read(buffer));
    match read {
        Ok(Ok(0)) => ff::AVERROR_EOF,
        Ok(Ok(count)) => count as c_int,
        Ok(Err(error)) => {
            io.error = Some(error.to_string());
            ff::AVERROR(ff::EIO)
        }
        Err(_) => {
            io.error = Some("the reader panicked".into());
            ff::AVERROR(ff::EIO)
        }
    }
}

unsafe extern "C" fn seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    // Safety: as in `read_packet`.
    let io = unsafe { &mut *opaque.cast::<Io>() };
    let result = decibell_panic::catch(|| {
        if whence & ff::AVSEEK_SIZE as c_int != 0 {
            let here = io.source.stream_position()?;
            let size = io.source.seek(SeekFrom::End(0))?;
            io.source.seek(SeekFrom::Start(here))?;
            return Ok(size);
        }
        let to = match whence & 3 {
            0 => SeekFrom::Start(offset.max(0) as u64),
            1 => SeekFrom::Current(offset),
            2 => SeekFrom::End(offset),
            _ => return Err(std::io::Error::other("unknown seek origin")),
        };
        io.source.seek(to)
    });
    match result {
        Ok(Ok(position)) => position as i64,
        Ok(Err(error)) => {
            io.error = Some(error.to_string());
            ff::AVERROR(ff::EIO) as i64
        }
        Err(_) => ff::AVERROR(ff::EIO) as i64,
    }
}

impl Avio {
    pub fn new(source: Box<dyn Source>, abort: Arc<AtomicBool>) -> Option<Avio> {
        let io = Box::into_raw(Box::new(Io {
            source,
            error: None,
            abort,
        }));
        // Safety: the buffer is FFmpeg's from here on (it may replace it; the context's
        // `buffer` field is what is freed); `io` lives until `drop`.
        unsafe {
            let buffer = ff::av_malloc(IO_BUFFER).cast::<u8>();
            if buffer.is_null() {
                drop(Box::from_raw(io));
                return None;
            }
            let context = ff::avio_alloc_context(
                buffer,
                IO_BUFFER as c_int,
                0,
                io.cast(),
                Some(read_packet),
                None,
                Some(seek),
            );
            if context.is_null() {
                ff::av_free(buffer.cast());
                drop(Box::from_raw(io));
                return None;
            }
            Some(Avio { context, io })
        }
    }

    /// The source's own error behind FFmpeg's last "I/O error", once.
    pub fn take_error(&mut self) -> Option<String> {
        // Safety: `io` is alive; FFmpeg is not inside a callback (single thread).
        unsafe { (*self.io).error.take() }
    }
}

impl Drop for Avio {
    fn drop(&mut self) {
        // Safety: both are ours; the format context that used them is closed first.
        unsafe {
            if !self.context.is_null() {
                ff::av_freep(std::ptr::addr_of_mut!((*self.context).buffer).cast());
                ff::avio_context_free(&mut self.context);
            }
            drop(Box::from_raw(self.io));
        }
    }
}

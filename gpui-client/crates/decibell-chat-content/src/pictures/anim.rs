//! Animated pictures (GIF, animated WebP): frames composited one at a time, off the UI thread.
//!
//! [`FrameSource`] owns the encoded bytes and the decoder's position; it is `Send`, so a frame is
//! decoded in a background task that takes the source and gives it back with the frame. GIF
//! frames are composited here (the `gif` crate gives each frame's rectangle; the disposal
//! methods are applied as Chromium and the `image` crate apply them: "background" clears to
//! transparent); animated WebP comes composited from `image-webp`.

use std::io::Cursor;
use std::time::Duration;

use bytes::Bytes;
use image::{Rgba, RgbaImage};

use super::decode::{Kind, chromium_delay};

/// How often the animation plays (`LoopCount`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loops {
    Forever,
    Times(u32),
}

enum Decoder {
    Gif {
        decoder: Box<gif::Decoder<Cursor<Bytes>>>,
        canvas: RgbaImage,
        /// The canvas before the last frame, when that frame is to be undone ("previous").
        saved: Option<RgbaImage>,
        /// The last frame's disposal and rectangle, applied before the next frame.
        dispose: Option<(gif::DisposalMethod, (u32, u32, u32, u32))>,
    },
    WebP {
        decoder: Box<image_webp::WebPDecoder<Cursor<Bytes>>>,
        buffer: Vec<u8>,
        alpha: bool,
    },
}

/// The frames of one animation, in order; see the module doc.
pub struct FrameSource {
    bytes: Bytes,
    decoder: Decoder,
    pub size: (u32, u32),
    pub loops: Loops,
    /// Frames handed out since the last restart.
    pub index: usize,
}

impl FrameSource {
    /// `None` for a file that is not an animation (a one-frame GIF is still an animation here:
    /// it simply has one frame).
    pub fn open(bytes: Bytes) -> Result<Option<FrameSource>, String> {
        match super::decode::sniff(&bytes) {
            Kind::Gif => {
                let mut options = gif::DecodeOptions::new();
                options.set_color_output(gif::ColorOutput::RGBA);
                let decoder = options.read_info(Cursor::new(bytes.clone())).map_err(|e| e.to_string())?;
                let (w, h) = (decoder.width() as u32, decoder.height() as u32);
                let loops = match decoder.repeat() {
                    gif::Repeat::Infinite => Loops::Forever,
                    // NETSCAPE loop count n: the animation shows n + 1 times (Chromium).
                    gif::Repeat::Finite(n) => Loops::Times(n as u32 + 1),
                };
                Ok(Some(FrameSource {
                    decoder: Decoder::Gif {
                        decoder: Box::new(decoder),
                        canvas: RgbaImage::from_pixel(w.max(1), h.max(1), Rgba([0, 0, 0, 0])),
                        saved: None,
                        dispose: None,
                    },
                    bytes,
                    size: (w.max(1), h.max(1)),
                    loops,
                    index: 0,
                }))
            }
            Kind::WebP => {
                let decoder = image_webp::WebPDecoder::new(Cursor::new(bytes.clone())).map_err(|e| e.to_string())?;
                if !decoder.is_animated() {
                    return Ok(None);
                }
                let size = decoder.dimensions();
                let loops = match decoder.loop_count() {
                    image_webp::LoopCount::Forever => Loops::Forever,
                    image_webp::LoopCount::Times(n) => Loops::Times(n.get() as u32),
                };
                let alpha = decoder.has_alpha();
                let len = decoder.output_buffer_size().ok_or("an animation too large to decode")?;
                Ok(Some(FrameSource {
                    decoder: Decoder::WebP { decoder: Box::new(decoder), buffer: vec![0; len], alpha },
                    bytes,
                    size,
                    loops,
                    index: 0,
                }))
            }
            Kind::Other => Ok(None),
        }
    }

    /// Back to the first frame (the next loop).
    pub fn restart(&mut self) -> Result<(), String> {
        let fresh = FrameSource::open(self.bytes.clone())?.ok_or("not an animation any more")?;
        *self = fresh;
        Ok(())
    }

    /// The next whole frame and how long it shows (Chromium's minimum applied). `None` at the
    /// end of the file.
    pub fn next_frame(&mut self) -> Result<Option<(RgbaImage, Duration)>, String> {
        let frame = match &mut self.decoder {
            Decoder::Gif { decoder, canvas, saved, dispose } => {
                // Undo what the previous frame asked to be undone.
                match dispose.take() {
                    Some((gif::DisposalMethod::Background, (x, y, w, h))) => {
                        for yy in y..(y + h).min(canvas.height()) {
                            for xx in x..(x + w).min(canvas.width()) {
                                canvas.put_pixel(xx, yy, Rgba([0, 0, 0, 0]));
                            }
                        }
                    }
                    Some((gif::DisposalMethod::Previous, _)) => {
                        if let Some(previous) = saved.take() {
                            *canvas = previous;
                        }
                    }
                    _ => {}
                }
                let Some(frame) = decoder.read_next_frame().map_err(|e| e.to_string())? else { return Ok(None) };
                let rect = (frame.left as u32, frame.top as u32, frame.width as u32, frame.height as u32);
                if frame.dispose == gif::DisposalMethod::Previous {
                    *saved = Some(canvas.clone());
                }
                let (cw, ch) = canvas.dimensions();
                let buffer = &frame.buffer;
                for fy in 0..rect.3 {
                    let y = rect.1 + fy;
                    if y >= ch {
                        break;
                    }
                    for fx in 0..rect.2 {
                        let x = rect.0 + fx;
                        if x >= cw {
                            break;
                        }
                        let i = ((fy * rect.2 + fx) * 4) as usize;
                        let Some(px) = buffer.get(i..i + 4) else { break };
                        // GIF transparency is all or nothing: a transparent pixel keeps the canvas.
                        if px[3] != 0 {
                            canvas.put_pixel(x, y, Rgba([px[0], px[1], px[2], px[3]]));
                        }
                    }
                }
                *dispose = Some((frame.dispose, rect));
                let delay = Duration::from_millis(frame.delay as u64 * 10);
                (canvas.clone(), delay)
            }
            Decoder::WebP { decoder, buffer, alpha } => {
                let delay = match decoder.read_frame(buffer) {
                    Ok(ms) => ms,
                    Err(image_webp::DecodingError::NoMoreFrames) => return Ok(None),
                    Err(e) => return Err(e.to_string()),
                };
                let (w, h) = self.size;
                let rgba = if *alpha {
                    RgbaImage::from_raw(w, h, buffer.clone()).ok_or("a frame of the wrong size")?
                } else {
                    let mut out = RgbaImage::new(w, h);
                    for (d, s) in out.as_mut().as_chunks_mut::<4>().0.iter_mut().zip(buffer.as_chunks::<3>().0) {
                        *d = [s[0], s[1], s[2], 255];
                    }
                    out
                };
                (rgba, Duration::from_millis(delay as u64))
            }
        };
        self.index += 1;
        Ok(Some((frame.0, chromium_delay(frame.1))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encodes a GIF with the `image` crate's encoder (red, green, blue frames).
    pub(crate) fn three_frames() -> Vec<u8> {
        use image::codecs::gif::{GifEncoder, Repeat};
        use image::{Delay, Frame};
        let mut out = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut out);
            enc.set_repeat(Repeat::Infinite).unwrap();
            for (i, color) in [[250u8, 0, 0, 255], [0, 250, 0, 255], [0, 0, 250, 255]].into_iter().enumerate() {
                let frame = Frame::from_parts(RgbaImage::from_pixel(40, 30, Rgba(color)), 0, 0, Delay::from_numer_denom_ms(if i == 0 { 0 } else { 50 }, 1));
                enc.encode_frame(frame).unwrap();
            }
        }
        out
    }

    #[test]
    fn frames_come_in_order_and_loop() {
        let mut source = FrameSource::open(Bytes::from(three_frames())).unwrap().expect("an animation");
        assert_eq!(source.size, (40, 30));
        assert_eq!(source.loops, Loops::Forever);
        let (first, delay) = source.next_frame().unwrap().unwrap();
        assert!(first.get_pixel(5, 5)[0] > 200);
        // A zero delay plays as 100 ms.
        assert_eq!(delay, Duration::from_millis(100));
        let (second, delay) = source.next_frame().unwrap().unwrap();
        assert!(second.get_pixel(5, 5)[1] > 200);
        assert_eq!(delay, Duration::from_millis(50));
        assert!(source.next_frame().unwrap().is_some());
        assert!(source.next_frame().unwrap().is_none());
        source.restart().unwrap();
        assert!(source.next_frame().unwrap().unwrap().0.get_pixel(1, 1)[0] > 200);
    }

    #[test]
    fn stills_are_not_animations() {
        let mut png = Vec::new();
        RgbaImage::from_pixel(4, 4, Rgba([1, 2, 3, 255])).write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        assert!(FrameSource::open(Bytes::from(png)).unwrap().is_none());
    }
}

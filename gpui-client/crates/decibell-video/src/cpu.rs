//! The backend without a GPU: software decode into a sink that counts pictures.
//!
//! This is what a platform gets until its renderer can draw surfaces (Windows today: see
//! docs/video-design.md §8 for what `gpui_windows` needs and the interface expected of it), and
//! what the unit tests decode with. Streams are decoded, counted and timed exactly as on the
//! GPU path; nothing is drawn.

use std::sync::atomic::{AtomicI64, Ordering};

use crate::codec::{Codec, CodecSupport, DecodePath};
use crate::decode::{
    Backend, DecodeResult, FrameCx, LumaRow, PictureDecoder, SoftwareDecoder, SwPicture, Uploader,
    has_software_decoder,
};
use crate::hub::StreamTarget;

#[derive(Default)]
pub(crate) struct CpuBackend {
    /// Decoders open (stands in for "textures alive").
    live: std::sync::Arc<AtomicI64>,
}

struct CountingUploader {
    live: std::sync::Arc<AtomicI64>,
}

impl Uploader for CountingUploader {
    fn upload(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if let Some((row, sender)) = cx.capture.take(picture.height) {
            let _ = sender.send(LumaRow {
                serial: cx.serial,
                width: picture.width,
                height: picture.height,
                row,
                bytes: picture
                    .luma
                    .row(row as usize, picture.width as usize)
                    .to_vec(),
            });
        }
        Ok(true)
    }
}

impl Drop for CountingUploader {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Backend for CpuBackend {
    fn open(
        &self,
        codec: Codec,
        description: Option<&[u8]>,
        _allow_hardware: bool,
        _target: &StreamTarget,
    ) -> DecodeResult<Box<dyn PictureDecoder>> {
        self.live.fetch_add(1, Ordering::Relaxed);
        let uploader = Box::new(CountingUploader {
            live: self.live.clone(),
        });
        Ok(Box::new(SoftwareDecoder::open(
            codec,
            description,
            uploader,
        )?))
    }

    fn ready(&self) -> bool {
        true
    }

    fn simulate_device_loss(&self) {}

    fn support(&self, codec: Codec) -> CodecSupport {
        CodecSupport {
            codec,
            path: has_software_decoder(codec).then_some(DecodePath::Software),
            max_size: None,
        }
    }

    fn live_textures(&self) -> i64 {
        self.live.load(Ordering::Relaxed)
    }

    fn collect(&self) {}
}

//! Software pictures into textures: `UpdateSubresource` of the luma plane into an `R8` texture
//! and of the interleaved chroma into an `R8G8` one. Direct3D copies the bytes during the call
//! (the one copy of the picture on the CPU; planar chroma is interleaved into a scratch buffer
//! first) and runs the upload in order with everything else on the context, so the picture is
//! complete for any draw issued after it is published.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use gpui_windows::D3d11Surface;

use super::slots::{SlotRing, SlotTextures, read_luma_row};
use super::{GpuDevice, classify, surface_color};
use crate::decode::{Chroma, DecodeError, DecodeResult, FrameCx, SwPicture, Uploader};

pub(crate) struct D3d11Uploader {
    slots: Option<SlotRing>,
    /// Planar chroma interleaved before the upload; reused.
    chroma_scratch: Vec<u8>,
    surface: Arc<D3d11Surface>,
    device: Arc<GpuDevice>,
    narrow: crate::decode::Narrow,
}

impl D3d11Uploader {
    pub fn new(device: Arc<GpuDevice>, surface: Arc<D3d11Surface>) -> Self {
        Self {
            slots: None,
            chroma_scratch: Vec::new(),
            surface,
            device,
            narrow: Default::default(),
        }
    }

    fn upload_inner(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        // A 10 / 12-bit picture (a file): its 8 most significant bits, the slots here being
        // 8-bit.
        let mut narrow = std::mem::take(&mut self.narrow);
        let narrowed;
        let picture = if picture.sample_bytes() == 2 {
            narrowed = narrow.picture(picture);
            &narrowed
        } else {
            picture
        };
        let uploaded = self.upload_eight_bit(picture, cx);
        self.narrow = narrow;
        uploaded
    }

    fn upload_eight_bit(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        let (width, height) = (picture.width, picture.height);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let color = surface_color(picture.color);
        if self
            .slots
            .as_ref()
            .is_none_or(|slots| slots.size != (width, height))
        {
            self.slots = None;
            self.slots = Some(SlotRing::new(&self.device, width, height, color, false)?);
            self.chroma_scratch
                .resize(chroma_width as usize * 2 * chroma_height as usize, 0);
        }
        let Some(slot) = self.slots.as_mut().and_then(|slots| slots.free(color)) else {
            cx.counters.no_slot.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        };
        let SlotTextures::Planes { luma, chroma } = &slot.textures else {
            return Err(DecodeError::decoder("a software slot without plane textures"));
        };
        let chroma_row = chroma_width as usize * 2;
        let (chroma_bytes, chroma_stride): (&[u8], usize) = match &picture.chroma {
            Chroma::Interleaved(plane) => (plane.bytes, plane.stride),
            Chroma::Planar(u, v) => {
                let half = chroma_width as usize;
                for (y, out) in self.chroma_scratch.chunks_exact_mut(chroma_row).enumerate() {
                    let (u, v) = (u.row(y, half), v.row(y, half));
                    for ((pair, u), v) in out.as_chunks_mut::<2>().0.iter_mut().zip(u).zip(v) {
                        pair[0] = *u;
                        pair[1] = *v;
                    }
                }
                (&self.chroma_scratch, chroma_row)
            }
        };
        // What `UpdateSubresource` reads of each plane: `stride * (rows - 1) + row bytes`.
        let luma_needed = picture.luma.stride * (height as usize - 1) + width as usize;
        let chroma_needed = chroma_stride * (chroma_height as usize - 1) + chroma_row;
        if picture.luma.bytes.len() < luma_needed || chroma_bytes.len() < chroma_needed {
            return Err(DecodeError::decoder(
                "the decoder returned a picture smaller than its size",
            ));
        }
        {
            let _held = self.device.lock();
            let context = self.device.context();
            // Safety: each source holds the bytes the call reads (checked above) for the
            // texture's whole size at the given row pitch; the textures are ours and free.
            unsafe {
                context.UpdateSubresource(
                    luma,
                    0,
                    None,
                    picture.luma.bytes.as_ptr().cast(),
                    picture.luma.stride as u32,
                    0,
                );
                context.UpdateSubresource(
                    chroma,
                    0,
                    None,
                    chroma_bytes.as_ptr().cast(),
                    chroma_stride as u32,
                    0,
                );
            }
        }
        if self.device.lost() {
            return Err(DecodeError::device_lost(
                "the renderer's GPU device was removed",
            ));
        }
        if let Some((row, sender)) = cx.capture.take(height) {
            let _ = sender.send(read_luma_row(
                &self.device,
                slot,
                width,
                height,
                row,
                cx.serial,
            )?);
        }
        self.surface.set_frame(slot.frame.clone(), cx.serial);
        Ok(true)
    }
}

impl Uploader for D3d11Uploader {
    fn upload(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if self.device.lost() {
            return Err(DecodeError::device_lost(
                "the renderer's GPU device was removed",
            ));
        }
        self.upload_inner(picture, cx)
            .map_err(|error| classify(&self.device, error))
    }
}

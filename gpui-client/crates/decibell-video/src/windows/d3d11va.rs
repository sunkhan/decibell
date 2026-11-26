//! Hardware decode: FFmpeg's D3D11VA hwaccels on the renderer's device, then one GPU copy of
//! the picture (`CopySubresourceRegion` of the decoder's array slice) into a slot the renderer
//! samples. No picture data touches the CPU, and the decoder thread never waits for the GPU:
//! why the published picture is complete all the same is in the module comment of
//! `windows/mod.rs`.
//!
//! Why the copy and not the decoder's own texture as the picture: FFmpeg's D3D11VA frames are
//! slices of one array texture created with `D3D11_BIND_DECODER` only. Sampling them would need
//! `BIND_DECODER | BIND_SHADER_RESOURCE` array textures, which is exactly what some drivers
//! refuse or decode wrongly into, and the pool would have to grow by every picture the renderer
//! still shows. A copy into an ordinary sampled texture works wherever the decoder does, and it
//! is the same hand-over the Linux backends use.

use std::ffi::c_int;
use std::sync::Arc;

use ffmpeg_sys_next as ff;
use gpui_windows::D3d11Surface;
use gpui_windows::windows::Win32::Graphics::Direct3D11::{D3D11_BOX, ID3D11Texture2D};
use gpui_windows::windows::core::Interface;

use super::slots::{SlotRing, SlotTextures, even, read_luma_row};
use super::{GpuDevice, classify, surface_color};
use crate::codec::{Codec, DecodeBackend};
use crate::decode::{DecodeError, DecodeResult, FfDecoder, FrameCx, FrameInfo, PictureDecoder};
use crate::hub::StreamTarget;
use crate::packet::Packet;
use crate::stats::Counters;

/// FFmpeg's `get_format`: the D3D11VA hwaccel or nothing.
unsafe extern "C" fn pick_d3d11(
    _context: *mut ff::AVCodecContext,
    formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // Safety: FFmpeg's list.
    unsafe { crate::decode::pick_format(formats, ff::AVPixelFormat::AV_PIX_FMT_D3D11) }
}

pub(crate) struct D3d11vaDecoder {
    // Field order is drop order: the decoder (and its frames pool) before our GPU objects, and
    // the device (whose FFmpeg context the decoder refers to) last.
    decoder: FfDecoder,
    slots: Option<SlotRing>,
    surface: Arc<D3d11Surface>,
    device: Arc<GpuDevice>,
}

impl D3d11vaDecoder {
    pub fn open(
        device: Arc<GpuDevice>,
        hw_device: *mut ff::AVBufferRef,
        codec: Codec,
        description: Option<&[u8]>,
        target: &StreamTarget,
    ) -> DecodeResult<Self> {
        let decoder = FfDecoder::open(codec, description, Some((hw_device, pick_d3d11)), 1)?;
        Ok(Self {
            decoder,
            slots: None,
            surface: target.surface.clone(),
            device,
        })
    }

    /// Copies the decoded picture into a free slot and makes it the surface's newest. False: no
    /// slot was free.
    fn hand_over(&mut self, info: &FrameInfo, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if info.format != ff::AVPixelFormat::AV_PIX_FMT_D3D11 as c_int {
            return Err(DecodeError::decoder(format!(
                "the hardware decoder returned a software frame (format {})",
                info.format
            )));
        }
        if info.sw_format != ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
            // 10-bit streams decode to P010: the same path with 16-bit planes. The streamer
            // sends 8-bit; anything else goes to the software decoder, which says so too.
            return Err(DecodeError::decoder(format!(
                "unsupported picture format {} (only 8-bit 4:2:0)",
                info.sw_format
            )));
        }
        let (width, height) = (info.width, info.height);
        // The copy region is even (NV12) and inside the decoder's texture, which is the coded
        // size rounded up to the codec's alignment.
        let (copy_width, copy_height) = (even(width), even(height));
        if width == 0
            || height == 0
            || copy_width > info.coded_width
            || copy_height > info.coded_height
        {
            return Err(DecodeError::decoder(format!(
                "unexpected decoded picture: {width}x{height} in a {}x{} texture",
                info.coded_width, info.coded_height
            )));
        }
        // AVFrame.data[0] is the ID3D11Texture2D, data[1] the index of the frame's slice in it
        // (libavutil/hwcontext_d3d11va.h, AVD3D11FrameDescriptor).
        let texture_pointer = info.data[0].cast::<std::ffi::c_void>();
        let slice = info.data[1] as usize as u32;
        // Safety: a decoded AV_PIX_FMT_D3D11 frame carries a live texture for as long as the
        // frame is referenced (the caller unrefs it after this returns); borrowed, not owned.
        let Some(source) = (unsafe { ID3D11Texture2D::from_raw_borrowed(&texture_pointer) })
        else {
            return Err(DecodeError::decoder(
                "the hardware decoder returned a frame without a texture",
            ));
        };

        let color = surface_color(info.color);
        if self
            .slots
            .as_ref()
            .is_none_or(|slots| slots.size != (width, height))
        {
            // Slots of the old size live on until the renderer lets go of them.
            self.slots = None;
            self.slots = Some(SlotRing::new(&self.device, width, height, color, true)?);
            cx.counters.set_size(width, height);
        }
        let Some(slot) = self.slots.as_mut().and_then(|slots| slots.free(color)) else {
            cx.counters
                .no_slot
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(false);
        };
        let SlotTextures::Nv12(destination) = &slot.textures else {
            return Err(DecodeError::decoder("a hardware slot without an NV12 texture"));
        };
        let region = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: copy_width,
            bottom: copy_height,
            back: 1,
        };
        {
            // The context is held as FFmpeg holds it around its own calls (the same critical
            // section), so the copy is not issued in the middle of a frame the renderer draws.
            let _held = self.device.lock();
            // Safety: both textures are NV12 on this device; the region is even, inside the
            // source slice (checked above) and the size of the destination, which is ours and
            // free (no draw issued after this samples its old content).
            unsafe {
                self.device.context().CopySubresourceRegion(
                    destination,
                    0,
                    0,
                    0,
                    0,
                    source,
                    slice,
                    Some(&region),
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
        // Issued, in order, after the decode: any draw from here on samples the whole picture.
        self.surface.set_frame(slot.frame.clone(), cx.serial);
        Ok(true)
    }

    fn decode_inner(
        &mut self,
        packet: Packet,
        keyframe: bool,
        pts: i64,
        cx: &FrameCx<'_>,
    ) -> DecodeResult<u32> {
        self.decoder.send(packet, keyframe, pts)?;
        let mut published = 0;
        while self.decoder.receive()? {
            Counters::frame_decoded(cx.counters, cx.serial);
            let info = self.decoder.frame_info();
            let handed = self.hand_over(&info, cx);
            self.decoder.unref_frame();
            if handed? {
                published += 1;
            }
        }
        Ok(published)
    }
}

impl PictureDecoder for D3d11vaDecoder {
    fn backend(&self) -> DecodeBackend {
        DecodeBackend::D3d11va
    }

    fn decode(
        &mut self,
        packet: Packet,
        keyframe: bool,
        pts: i64,
        cx: &FrameCx<'_>,
    ) -> DecodeResult<u32> {
        if self.device.lost() {
            return Err(DecodeError::device_lost(
                "the renderer's GPU device was removed",
            ));
        }
        self.decode_inner(packet, keyframe, pts, cx)
            .map_err(|error| classify(&self.device, error))
    }
}

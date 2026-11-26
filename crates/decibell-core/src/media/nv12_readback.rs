//! Captured frames → CPU NV12, for the software encoder on Windows.
//!
//! The capture hands the encoder thread BGRA D3D11 textures. Hardware
//! encoders take them on the GPU; SVT-AV1 needs the pixels in memory. The
//! conversion and the scale to the encode size still happen on the GPU (the
//! same D3D11 video processor the hardware path uses, into our own NV12
//! texture); only the finished NV12 picture is read back.
//!
//! **Nothing here waits on the GPU** (HANDOFF §5 "no GPU sync in capture
//! loops": a `Map` that waits stalls behind whatever the shared application
//! has queued). Each frame is copied into the next of `RING` staging
//! textures and `Flush`ed; staging textures are mapped with
//! `D3D11_MAP_FLAG_DO_NOT_WAIT`, oldest first, on later calls. A picture is
//! therefore encoded when the GPU has finished it, normally by the next
//! poll. If every staging texture is still in flight the new frame is
//! dropped (the caller counts drops), never queued behind a wait.
//!
//! The pattern is `thumbnail.rs`'s (one staging texture, DO_NOT_WAIT), with
//! a ring so a 60 fps stream does not lose every second frame.
//!
//! **Not compiled or run on this machine** (Linux): written against
//! windows-rs 0.61 as `thumbnail.rs` and `video_processor.rs` use it, and
//! compiled by the Windows Native Check workflow.

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::{
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_NV12, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::DXGI_ERROR_WAS_STILL_DRAWING;

use super::gpu_pipeline::GpuDevice;
use super::video_processor::VideoProcessor;

/// Staging textures in flight at once. Three covers a GPU that is two
/// frames behind; beyond that frames are dropped.
const RING: usize = 3;

/// A readback not finished after this long is abandoned with an error (the
/// device is gone or wedged), as `thumbnail.rs` does.
const READBACK_STALE_AFTER: Duration = Duration::from_secs(5);

struct InFlight {
    slot: usize,
    pts: i64,
    force_keyframe: bool,
    since: Instant,
}

pub struct Nv12Readback {
    gpu: GpuDevice,
    processor: VideoProcessor,
    /// The video processor's output: NV12 at the encode size.
    target: ID3D11Texture2D,
    staging: Vec<ID3D11Texture2D>,
    next_slot: usize,
    in_flight: VecDeque<InFlight>,
    width: u32,
    height: u32,
}

// Created and used on the single encoder thread.
unsafe impl Send for Nv12Readback {}

fn nv12_texture(
    gpu: &GpuDevice,
    width: u32,
    height: u32,
    staging: bool,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: if staging { D3D11_USAGE_STAGING } else { D3D11_USAGE_DEFAULT },
        // A video processor output view needs a render-target texture.
        BindFlags: if staging { 0 } else { D3D11_BIND_RENDER_TARGET.0 as u32 },
        CPUAccessFlags: if staging { D3D11_CPU_ACCESS_READ.0 as u32 } else { 0 },
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
        .map_err(|e| format!("CreateTexture2D (NV12 {}): {e:?}", if staging { "staging" } else { "target" }))?;
    tex.ok_or_else(|| "CreateTexture2D (NV12) returned None".to_string())
}

impl Nv12Readback {
    /// `width`×`height` is the encode size; both must be even (NV12).
    pub fn new(gpu: &GpuDevice, width: u32, height: u32, fps: u32) -> Result<Self, String> {
        if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
            return Err(format!("NV12 readback needs an even size, got {width}x{height}"));
        }
        let processor = VideoProcessor::new(&gpu.device, width, height, fps)?;
        let target = nv12_texture(gpu, width, height, false)?;
        let mut staging = Vec::with_capacity(RING);
        for _ in 0..RING {
            staging.push(nv12_texture(gpu, width, height, true)?);
        }
        Ok(Self {
            gpu: gpu.clone(),
            processor,
            target,
            staging,
            next_slot: 0,
            in_flight: VecDeque::with_capacity(RING),
            width,
            height,
        })
    }

    /// Readbacks queued and not yet delivered.
    pub fn pending(&self) -> usize {
        self.in_flight.len()
    }

    /// Convert `bgra` to NV12 at the encode size on the GPU and queue its
    /// readback. Never waits. `Ok(false)`: every staging texture is still
    /// in flight, the frame was not taken. `Err`: the conversion failed.
    pub fn submit(&mut self, bgra: &ID3D11Texture2D, pts: i64, force_keyframe: bool) -> Result<bool, String> {
        if self.in_flight.len() >= RING {
            return Ok(false);
        }
        self.processor.blit_into(bgra, &self.target)?;
        let slot = self.next_slot;
        self.next_slot = (self.next_slot + 1) % RING;
        unsafe {
            self.gpu.context.CopyResource(&self.staging[slot], &self.target);
            // Submit now so the copy is in flight by the next poll; Flush
            // does not wait for the GPU.
            self.gpu.context.Flush();
        }
        self.in_flight.push_back(InFlight { slot, pts, force_keyframe, since: Instant::now() });
        Ok(true)
    }

    /// Deliver every readback the GPU has finished, oldest first, without
    /// waiting: `sink(y, y_stride, uv, uv_stride, pts, force_keyframe)`
    /// with the planes valid for the call only. Stops at the first one
    /// still in flight (order is kept). Returns how many were delivered.
    pub fn poll<F>(&mut self, mut sink: F) -> Result<usize, String>
    where
        F: FnMut(&[u8], usize, &[u8], usize, i64, bool) -> Result<(), String>,
    {
        let mut delivered = 0;
        while let Some(front) = self.in_flight.front() {
            let texture = &self.staging[front.slot];
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let hr = unsafe {
                self.gpu.context.Map(
                    texture,
                    0,
                    D3D11_MAP_READ,
                    D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32,
                    Some(&mut mapped),
                )
            };
            match hr {
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                    if front.since.elapsed() >= READBACK_STALE_AFTER {
                        return Err("NV12 readback timed out".to_string());
                    }
                    break;
                }
                Err(e) => return Err(format!("Map NV12 staging: {e:?}")),
                Ok(()) => {
                    let (pts, force_keyframe) = (front.pts, front.force_keyframe);
                    let result = self.deliver(&mapped, pts, force_keyframe, &mut sink);
                    unsafe { self.gpu.context.Unmap(texture, 0) };
                    self.in_flight.pop_front();
                    result?;
                    delivered += 1;
                }
            }
        }
        Ok(delivered)
    }

    /// Hand the mapped picture to the sink. Must run between Map and Unmap.
    fn deliver<F>(
        &self,
        mapped: &D3D11_MAPPED_SUBRESOURCE,
        pts: i64,
        force_keyframe: bool,
        sink: &mut F,
    ) -> Result<(), String>
    where
        F: FnMut(&[u8], usize, &[u8], usize, i64, bool) -> Result<(), String>,
    {
        let (w, h) = (self.width as usize, self.height as usize);
        let pitch = mapped.RowPitch as usize;
        if mapped.pData.is_null() || pitch < w {
            return Err(format!("bad NV12 staging mapping (pitch {pitch} < {w} or null)"));
        }
        // A mapped NV12 staging texture is the luma plane (`h` rows of
        // `RowPitch` bytes) followed directly by the interleaved chroma
        // plane (`h / 2` rows of the same pitch). Only the picture's width
        // is guaranteed on each plane's last row.
        let (y_len, uv_len) = plane_lens(pitch, w, h);
        // SAFETY: Map succeeded on a `w`×`h` NV12 staging texture, so the
        // mapping covers `pitch * h` bytes of luma and `pitch * (h / 2 - 1)
        // + w` of chroma after it, valid until Unmap (after this returns).
        let (y, uv) = unsafe {
            let base = mapped.pData as *const u8;
            (std::slice::from_raw_parts(base, y_len), std::slice::from_raw_parts(base.add(pitch * h), uv_len))
        };
        sink(y, pitch, uv, pitch, pts, force_keyframe)
    }
}

/// Readable bytes of the luma and chroma planes of a mapped NV12 texture:
/// full pitch for every row but each plane's last, which has its pixels.
fn plane_lens(pitch: usize, width: usize, height: usize) -> (usize, usize) {
    let rows_uv = height / 2;
    (pitch * (height - 1) + width, pitch * (rows_uv - 1) + width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plane_lengths_exclude_the_last_rows_padding() {
        assert_eq!(plane_lens(2048, 1920, 1080), (2048 * 1079 + 1920, 2048 * 539 + 1920));
        assert_eq!(plane_lens(64, 64, 2), (64 + 64, 64));
    }
}

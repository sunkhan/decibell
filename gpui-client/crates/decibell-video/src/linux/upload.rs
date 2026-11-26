//! Software pictures into textures.
//!
//! [`VulkanUploader`] (the renderer is on Vulkan, with or without Vulkan Video): the picture's
//! planes are copied once, into a staging buffer that is mapped for the decoder's life, and
//! from there by the GPU (`vkCmdCopyBufferToImage` on the aux queue) into the same kind of slot
//! the hardware path fills. One CPU copy per plane, no allocation per picture, and the picture
//! is complete before it is published.
//!
//! [`WgpuUploader`] (the renderer is on OpenGL: no raw device to share): `Queue::write_texture`
//! into ordinary wgpu textures. wgpu stages each write itself and runs it with the renderer's
//! next submission.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ash::vk;
use gpui_wgpu::{
    SurfaceColor, WgpuContext, WgpuSurface, WgpuSurfaceFrame, WgpuSurfacePlanes, wgpu,
};

use super::slots::{COLOR_LAYER, HostBuffer, SLOT_COUNT, SlotRing, Work, barrier, read_luma_row};
use super::{GpuDevice, LIVE_PLANES, classify, surface_color};
use crate::decode::{Chroma, DecodeError, DecodeResult, FrameCx, LumaRow, SwPicture, Uploader};

pub(crate) struct VulkanUploader {
    slots: Option<SlotRing>,
    staging: Option<HostBuffer>,
    work: Work,
    surface: Arc<WgpuSurface>,
}

impl VulkanUploader {
    pub fn new(device: Arc<GpuDevice>, surface: Arc<WgpuSurface>) -> DecodeResult<Self> {
        Ok(Self {
            slots: None,
            staging: None,
            work: Work::new(device)?,
            surface,
        })
    }

    fn upload_inner(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        let (width, height) = (picture.width, picture.height);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let color = surface_color(picture.color);
        // Luma rows packed, then chroma rows of interleaved pairs, at an offset every queue
        // family accepts for a buffer-to-image copy.
        let luma_bytes = width as usize * height as usize;
        let chroma_offset = luma_bytes.next_multiple_of(4);
        let chroma_row = chroma_width as usize * 2;
        let needed = chroma_offset + chroma_row * chroma_height as usize;
        if self
            .slots
            .as_ref()
            .is_none_or(|slots| slots.size != (width, height))
        {
            self.slots = None;
            self.slots = Some(SlotRing::new(&self.work, width, height, color, false)?);
        }
        if self
            .staging
            .as_ref()
            .is_none_or(|staging| staging.size < needed)
        {
            self.staging = None;
            self.staging = Some(HostBuffer::new(self.work.device.clone(), needed)?);
        }
        let (Some(slots), Some(staging)) = (self.slots.as_mut(), self.staging.as_mut()) else {
            return Ok(false);
        };
        let Some(slot) = slots.free(color) else {
            cx.counters.no_slot.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        };

        // The one copy of the picture on the CPU.
        let staged = staging.bytes();
        let (luma, chroma) = staged.split_at_mut(chroma_offset);
        let row = width as usize;
        if picture.luma.stride == row {
            luma[..luma_bytes].copy_from_slice(&picture.luma.bytes[..luma_bytes]);
        } else {
            for (y, out) in luma[..luma_bytes].chunks_exact_mut(row).enumerate() {
                out.copy_from_slice(picture.luma.row(y, row));
            }
        }
        match &picture.chroma {
            Chroma::Interleaved(plane) => {
                for (y, out) in chroma.chunks_exact_mut(chroma_row).enumerate() {
                    out.copy_from_slice(plane.row(y, chroma_row));
                }
            }
            Chroma::Planar(u, v) => {
                let half = chroma_width as usize;
                for (y, out) in chroma.chunks_exact_mut(chroma_row).enumerate() {
                    let (u, v) = (u.row(y, half), v.row(y, half));
                    for ((pair, u), v) in out.as_chunks_mut::<2>().0.iter_mut().zip(u).zip(v) {
                        pair[0] = *u;
                        pair[1] = *v;
                    }
                }
            }
        }

        let work = &self.work;
        // Safety: a copy from our staging buffer into slot images that are ours and free,
        // between layout changes that leave them as the renderer expects; waited for.
        unsafe {
            work.begin()?;
            work.barriers(&[
                barrier(
                    slot.luma,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::empty(),
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
                barrier(
                    slot.chroma,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::empty(),
                    vk::AccessFlags::TRANSFER_WRITE,
                ),
            ]);
            let region = |offset: usize, width, height| {
                vk::BufferImageCopy::default()
                    .buffer_offset(offset as u64)
                    .image_subresource(COLOR_LAYER)
                    .image_extent(vk::Extent3D {
                        width,
                        height,
                        depth: 1,
                    })
            };
            work.device.raw.cmd_copy_buffer_to_image(
                work.commands,
                staging.buffer,
                slot.luma,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region(0, width, height)],
            );
            work.device.raw.cmd_copy_buffer_to_image(
                work.commands,
                staging.buffer,
                slot.chroma,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region(chroma_offset, chroma_width, chroma_height)],
            );
            work.barriers(&[
                barrier(
                    slot.luma,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::empty(),
                ),
                barrier(
                    slot.chroma,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::empty(),
                ),
            ]);
            work.run()?;
        }
        if let Some((row, sender)) = cx.capture.take(height) {
            let _ = sender.send(read_luma_row(
                work, slot.luma, width, height, row, cx.serial,
            )?);
        }
        self.surface.set_frame(slot.frame.clone(), cx.serial);
        Ok(true)
    }
}

impl Uploader for VulkanUploader {
    fn upload(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if self.work.device.lost() {
            return Err(DecodeError::device_lost(
                "the renderer's GPU device was lost",
            ));
        }
        self.upload_inner(picture, cx)
            .map_err(|error| classify(&self.work.device, error))
    }
}

struct WgpuSlot {
    frame: Arc<WgpuSurfaceFrame>,
    luma: wgpu::Texture,
    chroma: wgpu::Texture,
    /// Counts the slot's two textures as alive until wgpu lets go of the frame.
    _live: LivePlanes,
}

struct LivePlanes;

impl LivePlanes {
    fn new() -> Self {
        LIVE_PLANES.fetch_add(2, Ordering::Relaxed);
        Self
    }
}

impl Drop for LivePlanes {
    fn drop(&mut self) {
        LIVE_PLANES.fetch_sub(2, Ordering::Relaxed);
    }
}

pub(crate) struct WgpuUploader {
    context: WgpuContext,
    slots: Vec<WgpuSlot>,
    slot_size: (u32, u32),
    /// Planar chroma interleaved before the upload; reused.
    chroma_scratch: Vec<u8>,
    surface: Arc<WgpuSurface>,
}

impl WgpuUploader {
    pub fn new(context: WgpuContext, surface: Arc<WgpuSurface>) -> Self {
        Self {
            context,
            slots: Vec::new(),
            slot_size: (0, 0),
            chroma_scratch: Vec::new(),
            surface,
        }
    }

    fn create_slots(&mut self, width: u32, height: u32, color: SurfaceColor) {
        self.slots.clear();
        self.slot_size = (width, height);
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        self.chroma_scratch
            .resize((chroma_width * 2 * chroma_height) as usize, 0);
        let texture = |format, width, height| {
            self.context
                .device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("video plane (software)"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                })
        };
        for _ in 0..SLOT_COUNT {
            let luma = texture(wgpu::TextureFormat::R8Unorm, width, height);
            let chroma = texture(wgpu::TextureFormat::Rg8Unorm, chroma_width, chroma_height);
            self.slots.push(WgpuSlot {
                frame: Arc::new(WgpuSurfaceFrame {
                    planes: Arc::new(WgpuSurfacePlanes {
                        luma: luma.create_view(&Default::default()),
                        chroma: chroma.create_view(&Default::default()),
                    }),
                    color,
                    device: (*self.context.device).clone(),
                    in_use: None,
                }),
                luma,
                chroma,
                _live: LivePlanes::new(),
            });
        }
    }
}

impl Uploader for WgpuUploader {
    fn upload(&mut self, picture: &SwPicture<'_>, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if self.context.device_lost() {
            return Err(DecodeError::device_lost(
                "the renderer's GPU device was lost",
            ));
        }
        let (width, height) = (picture.width, picture.height);
        let color = surface_color(picture.color);
        if self.slot_size != (width, height) {
            self.create_slots(width, height, color);
        }
        let Some(slot) = self
            .slots
            .iter()
            .find(|slot| Arc::strong_count(&slot.frame) == 1)
        else {
            cx.counters.no_slot.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        };
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let write = |texture: &wgpu::Texture, data: &[u8], stride: usize, width, height| {
            self.context.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride as u32),
                    rows_per_image: None,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        };
        write(
            &slot.luma,
            picture.luma.bytes,
            picture.luma.stride,
            width,
            height,
        );
        match &picture.chroma {
            Chroma::Interleaved(plane) => {
                write(
                    &slot.chroma,
                    plane.bytes,
                    plane.stride,
                    chroma_width,
                    chroma_height,
                );
            }
            Chroma::Planar(u, v) => {
                let half = chroma_width as usize;
                for (y, out) in self.chroma_scratch.chunks_exact_mut(half * 2).enumerate() {
                    let (u, v) = (u.row(y, half), v.row(y, half));
                    for ((pair, u), v) in out.as_chunks_mut::<2>().0.iter_mut().zip(u).zip(v) {
                        pair[0] = *u;
                        pair[1] = *v;
                    }
                }
                write(
                    &slot.chroma,
                    &self.chroma_scratch,
                    half * 2,
                    chroma_width,
                    chroma_height,
                );
            }
        }
        if let Some((row, sender)) = cx.capture.take(height) {
            // There is no cheap way to read a wgpu texture back from here: the row is the one
            // that was uploaded.
            let _ = sender.send(LumaRow {
                serial: cx.serial,
                width,
                height,
                row,
                bytes: picture.luma.row(row as usize, width as usize).to_vec(),
            });
        }
        self.surface.set_frame(slot.frame.clone(), cx.serial);
        Ok(true)
    }
}

#[allow(dead_code)]
fn _assert_device_is_send_sync(device: &GpuDevice) -> &(dyn Send + Sync) {
    device
}

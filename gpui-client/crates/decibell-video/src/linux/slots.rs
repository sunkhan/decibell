//! The textures a stream's pictures are handed to the renderer in.
//!
//! A slot is a luma image (`R8`, picture size) and a chroma image (`RG8`, half size) made with
//! raw Vulkan (so the aux queue can write them) and wrapped as ordinary wgpu textures (so the
//! renderer samples them like any other). wgpu only ever samples them and so never records a
//! barrier for them; our writes leave them in the layout wgpu assumes.
//!
//! Each slot has one long-lived `Arc<WgpuSurfaceFrame>`. Publishing a picture clones that Arc
//! into the surface; the renderer clones it into each submission that samples it. The slot is
//! free to be written when the decoder holds the only reference: nothing is allocated per
//! picture and nobody waits for anybody.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ash::vk;
use gpui_wgpu::wgpu::hal::api::Vulkan;
use gpui_wgpu::{SurfaceColor, WgpuSurfaceFrame, WgpuSurfacePlanes, wgpu};

use super::{GpuDevice, LIVE_PLANES, vk_error};
use crate::decode::{DecodeResult, LumaRow};

/// More than the renderer can hold at once: the newest published picture, plus those sampled by
/// submissions the GPU has not finished (two in practice), plus one to write.
pub(crate) const SLOT_COUNT: usize = 5;

pub(crate) const COLOR: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

pub(crate) const COLOR_LAYER: vk::ImageSubresourceLayers = vk::ImageSubresourceLayers {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    mip_level: 0,
    base_array_layer: 0,
    layer_count: 1,
};

pub(crate) fn barrier<'a>(
    image: vk::Image,
    old: vk::ImageLayout,
    new: vk::ImageLayout,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
) -> vk::ImageMemoryBarrier<'a> {
    vk::ImageMemoryBarrier::default()
        .image(image)
        .old_layout(old)
        .new_layout(new)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .subresource_range(COLOR)
}

/// A decoder's command buffer and fence on the aux queue family: one submission at a time.
pub(crate) struct Work {
    pub device: Arc<GpuDevice>,
    pool: vk::CommandPool,
    pub commands: vk::CommandBuffer,
    pub fence: vk::Fence,
}

impl Work {
    pub fn new(device: Arc<GpuDevice>) -> DecodeResult<Self> {
        // Safety: plain object creation on a live device; destroyed in `Drop`.
        unsafe {
            let raw = &device.raw;
            let pool = raw
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(device.aux_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .map_err(|e| vk_error("vkCreateCommandPool", e))?;
            let commands = raw
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                )
                .map_err(|e| {
                    raw.destroy_command_pool(pool, None);
                    vk_error("vkAllocateCommandBuffers", e)
                })?[0];
            let fence = raw
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .map_err(|e| {
                    raw.destroy_command_pool(pool, None);
                    vk_error("vkCreateFence", e)
                })?;
            Ok(Self {
                device,
                pool,
                commands,
                fence,
            })
        }
    }

    /// Starts recording the one command buffer.
    ///
    /// # Safety
    /// The previous submission of this command buffer has completed.
    pub unsafe fn begin(&self) -> DecodeResult<()> {
        // Safety: the caller's contract.
        unsafe {
            let raw = &self.device.raw;
            raw.reset_command_buffer(self.commands, vk::CommandBufferResetFlags::empty())
                .map_err(|e| vk_error("vkResetCommandBuffer", e))?;
            raw.begin_command_buffer(
                self.commands,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .map_err(|e| vk_error("vkBeginCommandBuffer", e))
        }
    }

    /// Ends the recording, submits it and sleeps until the GPU has executed it.
    ///
    /// # Safety
    /// The recorded commands are valid for the aux queue family.
    pub unsafe fn run(&self) -> DecodeResult<()> {
        // Safety: the caller's contract.
        unsafe {
            self.device
                .raw
                .end_command_buffer(self.commands)
                .map_err(|e| vk_error("vkEndCommandBuffer", e))?;
            let commands = [self.commands];
            self.device.submit_and_wait(
                &vk::SubmitInfo::default().command_buffers(&commands),
                self.fence,
            )
        }
    }

    /// Records the layout changes of several images at once.
    ///
    /// # Safety
    /// Between `begin` and `run`.
    pub unsafe fn barriers(&self, barriers: &[vk::ImageMemoryBarrier<'_>]) {
        // Safety: the caller's contract.
        unsafe {
            self.device.raw.cmd_pipeline_barrier(
                self.commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                barriers,
            );
        }
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        // Safety: every submission of the command buffer was waited for (or the device is lost,
        // where destroying is still valid).
        unsafe {
            self.device.raw.destroy_fence(self.fence, None);
            self.device.raw.destroy_command_pool(self.pool, None);
        }
    }
}

/// A buffer the CPU reads or writes and the aux queue copies from or to, mapped for its life.
pub(crate) struct HostBuffer {
    device: Arc<GpuDevice>,
    pub buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    mapped: *mut u8,
    pub size: usize,
}

// Safety: the mapping is only touched by the decoder thread that owns the buffer.
unsafe impl Send for HostBuffer {}

impl HostBuffer {
    pub fn new(device: Arc<GpuDevice>, size: usize) -> DecodeResult<Self> {
        // Safety: plain object creation; everything made is destroyed on the error paths.
        unsafe {
            let raw = &device.raw;
            let buffer = raw
                .create_buffer(
                    &vk::BufferCreateInfo::default()
                        .size(size as u64)
                        .usage(
                            vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST,
                        )
                        .sharing_mode(vk::SharingMode::EXCLUSIVE),
                    None,
                )
                .map_err(|e| vk_error("vkCreateBuffer", e))?;
            let requirements = raw.get_buffer_memory_requirements(buffer);
            let allocated = device
                .memory_type(
                    requirements.memory_type_bits,
                    vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
                )
                .and_then(|memory_type| {
                    raw.allocate_memory(
                        &vk::MemoryAllocateInfo::default()
                            .allocation_size(requirements.size)
                            .memory_type_index(memory_type),
                        None,
                    )
                    .map_err(|e| vk_error("vkAllocateMemory", e))
                });
            let memory = match allocated {
                Ok(memory) => memory,
                Err(error) => {
                    raw.destroy_buffer(buffer, None);
                    return Err(error);
                }
            };
            let mapped = raw.bind_buffer_memory(buffer, memory, 0).and_then(|()| {
                raw.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
            });
            match mapped {
                Ok(mapped) => Ok(Self {
                    device,
                    buffer,
                    memory,
                    mapped: mapped.cast(),
                    size,
                }),
                Err(error) => {
                    raw.destroy_buffer(buffer, None);
                    raw.free_memory(memory, None);
                    Err(vk_error("mapping a staging buffer", error))
                }
            }
        }
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        // Safety: `size` bytes are mapped for the buffer's life and only we touch them.
        unsafe { std::slice::from_raw_parts_mut(self.mapped, self.size) }
    }
}

impl Drop for HostBuffer {
    fn drop(&mut self) {
        // Safety: no submission using the buffer is pending (each one was waited for).
        unsafe {
            self.device.raw.destroy_buffer(self.buffer, None);
            self.device.raw.free_memory(self.memory, None);
        }
    }
}

/// A luma / chroma pair of images the renderer samples.
pub(crate) struct Slot {
    /// What is published: reused for every picture written into this slot.
    pub frame: Arc<WgpuSurfaceFrame>,
    pub luma: vk::Image,
    pub chroma: vk::Image,
    /// The images' memory and its size, luma then chroma: alive as long as `frame`'s planes
    /// are. For exporting it to another API ([`SlotRing::new`] with `exportable`).
    pub memory: [(vk::DeviceMemory, u64); 2],
}

/// A stream's slots for one picture size.
pub(crate) struct SlotRing {
    slots: Vec<Slot>,
    pub size: (u32, u32),
}

impl SlotRing {
    /// Creates the slots, every image in `SHADER_READ_ONLY_OPTIMAL`, wrapped for wgpu.
    /// `exportable`: each image gets a dedicated allocation that can be exported as an opaque
    /// file descriptor (`VK_KHR_external_memory_fd`), for the NVDEC path's CUDA import.
    pub fn new(
        work: &Work,
        width: u32,
        height: u32,
        color: SurfaceColor,
        exportable: bool,
    ) -> DecodeResult<Self> {
        let device = &work.device;
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        let mut images: Vec<[(vk::Image, vk::DeviceMemory, u64); 2]> =
            Vec::with_capacity(SLOT_COUNT);
        // Safety: raw image creation and one layout-change submission, on a live device; on
        // failure every image made so far is destroyed.
        unsafe {
            let created = (|| -> DecodeResult<()> {
                for _ in 0..SLOT_COUNT {
                    let luma =
                        create_plane_image(device, vk::Format::R8_UNORM, width, height, exportable)?;
                    let chroma = match create_plane_image(
                        device,
                        vk::Format::R8G8_UNORM,
                        chroma_width,
                        chroma_height,
                        exportable,
                    ) {
                        Ok(chroma) => chroma,
                        Err(error) => {
                            device.raw.destroy_image(luma.0, None);
                            device.raw.free_memory(luma.1, None);
                            return Err(error);
                        }
                    };
                    images.push([luma, chroma]);
                }
                work.begin()?;
                let barriers: Vec<_> = images
                    .iter()
                    .flatten()
                    .map(|(image, _, _)| {
                        barrier(
                            *image,
                            vk::ImageLayout::UNDEFINED,
                            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                            vk::AccessFlags::empty(),
                            vk::AccessFlags::empty(),
                        )
                    })
                    .collect();
                work.barriers(&barriers);
                work.run()
            })();
            if let Err(error) = created {
                for (image, memory, _) in images.into_iter().flatten() {
                    device.raw.destroy_image(image, None);
                    device.raw.free_memory(memory, None);
                }
                return Err(error);
            }

            let slots = images
                .into_iter()
                .map(|[(luma, luma_memory, luma_size), (chroma, chroma_memory, chroma_size)]| {
                    let luma_texture = wrap_plane(
                        device,
                        luma,
                        luma_memory,
                        wgpu::TextureFormat::R8Unorm,
                        width,
                        height,
                    );
                    let chroma_texture = wrap_plane(
                        device,
                        chroma,
                        chroma_memory,
                        wgpu::TextureFormat::Rg8Unorm,
                        chroma_width,
                        chroma_height,
                    );
                    Slot {
                        frame: Arc::new(WgpuSurfaceFrame {
                            planes: Arc::new(WgpuSurfacePlanes {
                                luma: luma_texture.create_view(&Default::default()),
                                chroma: chroma_texture.create_view(&Default::default()),
                            }),
                            color,
                            device: (*device.context.device).clone(),
                            in_use: None,
                        }),
                        luma,
                        chroma,
                        memory: [(luma_memory, luma_size), (chroma_memory, chroma_size)],
                    }
                })
                .collect();
            Ok(Self {
                slots,
                size: (width, height),
            })
        }
    }

    /// A slot nobody else holds: not the newest published picture, and not sampled by a frame
    /// the GPU has not finished. Its published frame carries `color`.
    pub fn free(&mut self, color: SurfaceColor) -> Option<&Slot> {
        self.free_indexed(color).map(|(_, slot)| slot)
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// [`Self::free`], with the slot's index in [`Self::slots`].
    pub fn free_indexed(&mut self, color: SurfaceColor) -> Option<(usize, &Slot)> {
        let (index, slot) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| Arc::strong_count(&slot.frame) == 1)?;
        if slot.frame.color != color {
            // The stream's colour description changed (it does not, in practice): the one
            // allocation a picture can cost.
            slot.frame = Arc::new(WgpuSurfaceFrame {
                planes: slot.frame.planes.clone(),
                color,
                device: slot.frame.device.clone(),
                in_use: None,
            });
        }
        Some((index, slot))
    }
}

/// A sampled image the aux queue writes and the renderer's queue reads, its memory and the
/// memory's size.
unsafe fn create_plane_image(
    device: &GpuDevice,
    format: vk::Format,
    width: u32,
    height: u32,
    exportable: bool,
) -> DecodeResult<(vk::Image, vk::DeviceMemory, u64)> {
    let families = [device.render_family, device.aux_family];
    let (sharing_mode, families) = if device.shared {
        (vk::SharingMode::EXCLUSIVE, &families[..0])
    } else {
        // Two queue families use it and neither transfers ownership.
        (vk::SharingMode::CONCURRENT, &families[..])
    };
    let info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        // TRANSFER_SRC: tests read a row back (`read_luma_row`).
        .usage(
            vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_DST
                | vk::ImageUsageFlags::TRANSFER_SRC,
        )
        .sharing_mode(sharing_mode)
        .queue_family_indices(families)
        .initial_layout(vk::ImageLayout::UNDEFINED);
    let mut external = vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
    let info = if exportable {
        info.push_next(&mut external)
    } else {
        info
    };
    // Safety: plain creation on a live device; destroyed on the error paths.
    unsafe {
        let raw = &device.raw;
        let image = raw
            .create_image(&info, None)
            .map_err(|e| vk_error("vkCreateImage", e))?;
        let requirements = raw.get_image_memory_requirements(image);
        let memory = device
            .memory_type(
                requirements.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .and_then(|memory_type| {
                let mut export = vk::ExportMemoryAllocateInfo::default()
                    .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
                // An image another API maps gets memory of its own: the importer is told the
                // allocation is dedicated and maps the image at offset 0.
                let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
                let allocation = vk::MemoryAllocateInfo::default()
                    .allocation_size(requirements.size)
                    .memory_type_index(memory_type);
                let allocation = if exportable {
                    allocation.push_next(&mut export).push_next(&mut dedicated)
                } else {
                    allocation
                };
                raw.allocate_memory(&allocation, None)
                    .map_err(|e| vk_error("vkAllocateMemory", e))
            });
        let memory = match memory {
            Ok(memory) => memory,
            Err(error) => {
                raw.destroy_image(image, None);
                return Err(error);
            }
        };
        if let Err(error) = raw.bind_image_memory(image, memory, 0) {
            raw.destroy_image(image, None);
            raw.free_memory(memory, None);
            return Err(vk_error("vkBindImageMemory", error));
        }
        Ok((image, memory, requirements.size))
    }
}

/// Hands a plane image to wgpu as an ordinary sampled texture. wgpu destroys it (through the
/// callback) once nothing references it and the GPU is done with it.
unsafe fn wrap_plane(
    device: &GpuDevice,
    image: vk::Image,
    memory: vk::DeviceMemory,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::Texture {
    let size = wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    };
    let raw = device.raw.clone();
    LIVE_PLANES.fetch_add(1, Ordering::Relaxed);
    // Safety: the image is in the layout wgpu's RESOURCE state means, with defined contents,
    // and the renderer only ever samples it: wgpu never records a barrier for it.
    unsafe {
        let hal_device = device
            .context
            .device
            .as_hal::<Vulkan>()
            .expect("a GpuDevice is on Vulkan");
        let hal_texture = hal_device.texture_from_raw(
            image,
            &wgpu::hal::TextureDescriptor {
                label: Some("video plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: vec![],
            },
            Some(Box::new(move || {
                raw.destroy_image(image, None);
                raw.free_memory(memory, None);
                LIVE_PLANES.fetch_sub(1, Ordering::Relaxed);
            })),
            wgpu::hal::vulkan::TextureMemory::External,
        );
        device.context.device.create_texture_from_hal::<Vulkan>(
            hal_texture,
            &wgpu::TextureDescriptor {
                label: Some("video plane"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            },
            wgpu::TextureUses::RESOURCE,
            true,
        )
    }
}

/// Reads one row of a slot's luma image back (tests). The image returns to the layout the
/// renderer samples it in before this returns.
pub(crate) fn read_luma_row(
    work: &Work,
    luma: vk::Image,
    width: u32,
    height: u32,
    row: u32,
    serial: u64,
) -> DecodeResult<LumaRow> {
    let mut buffer = HostBuffer::new(work.device.clone(), width as usize)?;
    // Safety: a copy out of an image we own, between two layout changes, waited for.
    unsafe {
        work.begin()?;
        work.barriers(&[barrier(
            luma,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::AccessFlags::empty(),
            vk::AccessFlags::TRANSFER_READ,
        )]);
        work.device.raw.cmd_copy_image_to_buffer(
            work.commands,
            luma,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            buffer.buffer,
            &[vk::BufferImageCopy::default()
                .image_subresource(COLOR_LAYER)
                .image_offset(vk::Offset3D {
                    x: 0,
                    y: row as i32,
                    z: 0,
                })
                .image_extent(vk::Extent3D {
                    width,
                    height: 1,
                    depth: 1,
                })],
        );
        work.barriers(&[barrier(
            luma,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::AccessFlags::TRANSFER_READ,
            vk::AccessFlags::empty(),
        )]);
        work.run()?;
    }
    Ok(LumaRow {
        serial,
        width,
        height,
        row,
        bytes: buffer.bytes().to_vec(),
    })
}

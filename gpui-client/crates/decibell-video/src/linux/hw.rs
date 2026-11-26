//! Hardware decode: FFmpeg's Vulkan Video hwaccel on the renderer's device, then a GPU copy of
//! the picture (two `vkCmdCopyImage` regions on the aux queue) into a slot the renderer samples.
//! No picture data touches the CPU.

use std::ffi::c_int;
use std::sync::Arc;

use ash::vk;
use ffmpeg_sys_next as ff;
use gpui_wgpu::WgpuSurface;

use super::slots::{COLOR_LAYER, SlotRing, Work, barrier, read_luma_row};
use super::{GpuDevice, classify, surface_color};
use crate::codec::{Codec, DecodeBackend};
use crate::decode::{DecodeError, DecodeResult, FfDecoder, FrameCx, FrameInfo, PictureDecoder};
use crate::hub::StreamTarget;
use crate::packet::Packet;
use crate::stats::Counters;

#[repr(C)]
#[derive(Default)]
struct DbvVkFrame {
    image: vk::Image,
    format: vk::Format,
    layout: vk::ImageLayout,
    sem: vk::Semaphore,
    sem_value: u64,
    queue_family: u32,
    nb_images: c_int,
}

unsafe extern "C" {
    fn dbv_vkframe_lock(f: *mut ff::AVFrame, out: *mut DbvVkFrame);
    fn dbv_vkframe_unlock(f: *mut ff::AVFrame, signalled: c_int);
}

/// FFmpeg's `get_format`: the Vulkan hwaccel or nothing.
unsafe extern "C" fn pick_vulkan(
    _context: *mut ff::AVCodecContext,
    formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // Safety: FFmpeg's list.
    unsafe { crate::decode::pick_format(formats, ff::AVPixelFormat::AV_PIX_FMT_VULKAN) }
}

pub(crate) struct VulkanDecoder {
    // Field order is drop order: the decoder (and its frames pool) before our GPU objects.
    decoder: FfDecoder,
    slots: Option<SlotRing>,
    work: Work,
    surface: Arc<WgpuSurface>,
}

impl VulkanDecoder {
    pub fn open(
        device: Arc<GpuDevice>,
        hw_device: *mut ff::AVBufferRef,
        codec: Codec,
        description: Option<&[u8]>,
        target: &StreamTarget,
    ) -> DecodeResult<Self> {
        let work = Work::new(device)?;
        let decoder = FfDecoder::open(
            codec,
            description,
            Some((hw_device, pick_vulkan)),
            1,
            target.reorder(),
        )?;
        Ok(Self {
            decoder,
            slots: None,
            work,
            surface: target.surface.clone(),
        })
    }

    /// Copies the decoded picture into a free slot and makes it the surface's newest. False: no
    /// slot was free.
    fn hand_over(&mut self, info: &FrameInfo, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if info.format != ff::AVPixelFormat::AV_PIX_FMT_VULKAN as c_int {
            return Err(DecodeError::decoder(format!(
                "the hardware decoder returned a software frame (format {})",
                info.format
            )));
        }
        if info.sw_format != ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
            // 10-bit streams decode to P010: the same path with R16 / RG16 planes. The streamer
            // sends 8-bit; anything else goes to the software decoder, which says so too.
            return Err(DecodeError::decoder(format!(
                "unsupported picture format {} (only 8-bit 4:2:0)",
                info.sw_format
            )));
        }
        let (width, height) = (info.width, info.height);
        let color = surface_color(info.color);
        if self
            .slots
            .as_ref()
            .is_none_or(|slots| slots.size != (width, height))
        {
            // Slots of the old size live on until the renderer lets go of them.
            self.slots = None;
            self.slots = Some(SlotRing::new(&self.work, width, height, color, false)?);
            cx.counters.set_size(width, height);
        }
        let Some(slot) = self.slots.as_mut().and_then(|slots| slots.free(color)) else {
            cx.counters
                .no_slot
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(false);
        };
        let work = &self.work;
        let raw = &work.device.raw;
        let frame = self.decoder.frame;

        // Safety: the AVVkFrame protocol (libavutil/hwcontext_vulkan.h): the frame is locked
        // while we read its state and record a submission that waits for its semaphore at the
        // current value and signals the next; the image is returned to the layout FFmpeg
        // believes it has. The slot images are ours and free.
        unsafe {
            let mut source = DbvVkFrame::default();
            dbv_vkframe_lock(frame, &mut source);
            let recorded = (|| -> DecodeResult<()> {
                if source.nb_images != 1 || source.format != vk::Format::G8_B8R8_2PLANE_420_UNORM {
                    return Err(DecodeError::decoder(format!(
                        "unexpected decoded image: {} images, {:?}",
                        source.nb_images, source.format
                    )));
                }
                work.begin()?;
                work.barriers(&[
                    barrier(
                        source.image,
                        source.layout,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::AccessFlags::empty(),
                        vk::AccessFlags::TRANSFER_READ,
                    ),
                    // The whole plane is overwritten: no need to keep what the slot held.
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
                // The decoded image is coded-size (e.g. 1920x1088); only the picture is copied.
                let plane_copy = |aspect, width, height| {
                    vk::ImageCopy::default()
                        .src_subresource(vk::ImageSubresourceLayers {
                            aspect_mask: aspect,
                            ..COLOR_LAYER
                        })
                        .dst_subresource(COLOR_LAYER)
                        .extent(vk::Extent3D {
                            width,
                            height,
                            depth: 1,
                        })
                };
                raw.cmd_copy_image(
                    work.commands,
                    source.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    slot.luma,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[plane_copy(vk::ImageAspectFlags::PLANE_0, width, height)],
                );
                raw.cmd_copy_image(
                    work.commands,
                    source.image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    slot.chroma,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[plane_copy(
                        vk::ImageAspectFlags::PLANE_1,
                        width.div_ceil(2),
                        height.div_ceil(2),
                    )],
                );
                work.barriers(&[
                    // Back to the layout FFmpeg left it in: it may be a reference picture.
                    barrier(
                        source.image,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        source.layout,
                        vk::AccessFlags::TRANSFER_READ,
                        vk::AccessFlags::empty(),
                    ),
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
                raw.end_command_buffer(work.commands)
                    .map_err(|e| super::vk_error("vkEndCommandBuffer", e))?;

                let semaphores = [source.sem];
                let wait_values = [source.sem_value];
                let signal_values = [source.sem_value + 1];
                let wait_stages = [vk::PipelineStageFlags::ALL_COMMANDS];
                let commands = [work.commands];
                let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
                    .wait_semaphore_values(&wait_values)
                    .signal_semaphore_values(&signal_values);
                let submit = vk::SubmitInfo::default()
                    .wait_semaphores(&semaphores)
                    .wait_dst_stage_mask(&wait_stages)
                    .command_buffers(&commands)
                    .signal_semaphores(&semaphores)
                    .push_next(&mut timeline);
                // The picture is complete when the copy is: the decoder thread sleeps here
                // (decode + copy, 1-3 ms). The frame stays locked meanwhile; nobody else wants
                // it (the next decode starts after this returns).
                work.device.submit_and_wait(&submit, work.fence)
            })();
            dbv_vkframe_unlock(frame, recorded.is_ok() as c_int);
            recorded?;
        }

        if let Some((row, sender)) = cx.capture.take(height) {
            let _ = sender.send(read_luma_row(
                work, slot.luma, width, height, row, cx.serial,
            )?);
        }
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
            if !cx.admit(self.decoder.frame_pts()) {
                self.decoder.unref_frame();
                continue;
            }
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

impl PictureDecoder for VulkanDecoder {
    fn backend(&self) -> DecodeBackend {
        DecodeBackend::VulkanVideo
    }

    fn decode(
        &mut self,
        packet: Packet,
        keyframe: bool,
        pts: i64,
        cx: &FrameCx<'_>,
    ) -> DecodeResult<u32> {
        if self.work.device.lost() {
            return Err(DecodeError::device_lost(
                "the renderer's GPU device was lost",
            ));
        }
        self.decode_inner(packet, keyframe, pts, cx)
            .map_err(|error| classify(&self.work.device, error))
    }

    fn flush(&mut self) -> bool {
        self.decoder.flush();
        true
    }
}

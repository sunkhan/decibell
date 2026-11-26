//! VA-API: the hardware decoders of AMD and Intel GPUs (Mesa's radeonsi, Intel's media
//! driver) where Vulkan Video is missing or fails, through FFmpeg's `vaapi` hwaccel. No picture
//! data touches the CPU.
//!
//! ```text
//! avcodec_send_packet / receive_frame       the driver decodes into a VA surface
//!   -> AV_PIX_FMT_VAAPI frame
//! av_hwframe_map -> AV_PIX_FMT_DRM_PRIME    vaSyncSurface (the decode has finished) and
//!                                           vaExportSurfaceHandle: the surface's DMA-BUF(s),
//!                                           one layer per plane, with offsets, pitches and
//!                                           the DRM format modifier
//! each plane's DMA-BUF imported into the    VK_EXT_external_memory_dma_buf +
//! renderer's device as an R8 / R8G8 image   VK_EXT_image_drm_format_modifier; once per VA
//!                                           surface (decoders cycle through a small pool):
//!                                           found again by the DMA-BUF's inode
//! one submission on the aux queue: take the planes over from the "foreign" queue family,
//! vkCmdCopyImage x 2 into a free slot, give them back; the thread sleeps on its fence, then
//! publishes the slot
//! ```
//!
//! **Why a GPU copy into slots and not sampling the imported image itself** (both were on the
//! table; docs/video-design.md §3 has the comparison and the measurements):
//!
//! - A decoder writes a VA surface again as soon as nothing references it, and Vulkan has no
//!   implicit synchronisation with it. Sampling the surface directly means holding the
//!   `AVFrame` for as long as any renderer submission samples it, enlarging the decoder's
//!   fixed surface pool to make room, and handing the image back and forth across the
//!   "foreign" queue family on the renderer's own schedule, which wgpu cannot express (it
//!   never records a barrier for an image it only samples). With the copy, ownership changes
//!   hands inside one submission of ours and the renderer only ever sees slot images, as on
//!   the other two hardware paths.
//! - What a driver exports is whatever its decoder likes (Intel: Y- or 4-tiled NV12; radeonsi:
//!   its own tiling modes). A transfer from such an image is the least a Vulkan driver can be
//!   asked for; sampling it with linear filtering is a format feature that has to be there
//!   per modifier and per plane format.
//! - The copy is 1.5 bytes per pixel (663 MB/s at 1440p120): what the Vulkan Video path does
//!   too. The import is the expensive part (0.4 to 0.8 ms per surface, measured) and is done
//!   once per surface in either design: 4 to 7 times in a 1,200-picture run.
//!
//! **Which GPU.** VA-API is opened on the DRM render node of the GPU the window is drawn
//! with (`VK_EXT_physical_device_drm`, or the PCI address), never on "the default display":
//! on a machine with two GPUs the default may be the other one, and a DMA-BUF of one GPU
//! imported into the other is at best a copy through system memory. If that GPU's VA driver
//! cannot decode the codec, this decoder is skipped (software is next); decoding on the
//! second GPU is deliberately not attempted.
//!
//! Only the NVDEC-backed VA driver (`nvidia-vaapi-driver`) could be run when this was written:
//! everything said about Mesa and the Intel driver below is from their source and the VA-API /
//! Vulkan specifications, cited where it matters. `gpui-client/docs/video-owner-checks.md`
//! lists what to run on real AMD and Intel hardware.

use std::ffi::{CString, c_int};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;
use std::sync::Arc;

use ash::vk;
use ffmpeg_sys_next as ff;
use gpui_wgpu::WgpuSurface;

use super::slots::{COLOR_LAYER, SlotRing, Work, barrier, read_luma_row};
use super::{GpuDevice, classify, surface_color, vk_error};
use crate::codec::{Codec, DecodeBackend};
use crate::decode::{
    DecodeError, DecodeResult, FfDecoder, FrameCx, FrameInfo, PictureDecoder, av_error,
};
use crate::hub::StreamTarget;
use crate::packet::Packet;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DbvDrmPlane {
    fd: c_int,
    format: u32,
    size: u64,
    modifier: u64,
    offset: u64,
    pitch: u64,
}

unsafe extern "C" {
    fn dbv_drm_planes(frame: *const ff::AVFrame, out: *mut DbvDrmPlane, max: c_int) -> c_int;
}

const fn fourcc(code: &[u8; 4]) -> u32 {
    code[0] as u32 | (code[1] as u32) << 8 | (code[2] as u32) << 16 | (code[3] as u32) << 24
}
const DRM_FORMAT_R8: u32 = fourcc(b"R8  ");
const DRM_FORMAT_GR88: u32 = fourcc(b"GR88");
const DRM_FORMAT_RG88: u32 = fourcc(b"RG88");
const DRM_FORMAT_NV12: u32 = fourcc(b"NV12");
/// `DRM_FORMAT_MOD_INVALID`: the exporter does not know the buffer's layout (a driver without
/// modifier support).
const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

/// FFmpeg's VA-API device context on the render node of the renderer's GPU. Alive while a
/// VA-API decoder is.
pub(crate) struct VaDevice {
    hw: *mut ff::AVBufferRef,
    pub node: PathBuf,
}

// Safety: an AVBufferRef to a hardware device context is made to be shared between threads
// (libva displays are thread-safe).
unsafe impl Send for VaDevice {}
unsafe impl Sync for VaDevice {}

impl VaDevice {
    pub fn open(device: &GpuDevice) -> DecodeResult<Arc<Self>> {
        let node = device
            .render_node()
            .map_err(|reason| DecodeError::decoder(format!("VA-API: {reason}")))?;
        let path = CString::new(node.as_os_str().as_bytes())
            .map_err(|_| DecodeError::decoder("VA-API: the render node's path has a NUL"))?;
        let mut hw = std::ptr::null_mut();
        // Safety: plain FFmpeg call; `hw` is ours on success. With a path FFmpeg opens that DRM
        // node and uses `vaGetDisplayDRM` on it (libavutil/hwcontext_vaapi.c,
        // vaapi_device_create): no X11 or Wayland connection is made.
        let created = unsafe {
            ff::av_hwdevice_ctx_create(
                &mut hw,
                ff::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                path.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if created < 0 {
            return Err(DecodeError::decoder(format!(
                "VA-API: no usable driver on {}: {}",
                node.display(),
                av_error(created)
            )));
        }
        log::info!("video: VA-API on {}", node.display());
        Ok(Arc::new(Self { hw, node }))
    }
}

impl Drop for VaDevice {
    fn drop(&mut self) {
        // Safety: our reference.
        unsafe { ff::av_buffer_unref(&mut self.hw) };
    }
}

/// What identifies a VA surface's exported planes: the same DMA-BUFs (by inode: every export
/// of a buffer is a new fd of the same file) at the same place, shape and size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ImportKey {
    /// `(st_dev, st_ino)` of the luma and the chroma plane's DMA-BUF.
    files: [(u64, u64); 2],
    offsets: [u64; 2],
    pitches: [u64; 2],
    modifier: u64,
    coded: (u32, u32),
}

struct PlaneImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
}

/// One VA surface's two planes as images of the renderer's device.
struct Import {
    key: ImportKey,
    planes: [PlaneImage; 2],
    /// The decoder's picture count when it was last used (the oldest is dropped first).
    used: u64,
}

/// Surfaces kept imported. FFmpeg's VA-API pools are fixed-size: the reference pictures the
/// stream needs (at most 16 for H.264 and HEVC, 8 for AV1) plus a few, 22 at most in practice.
/// A stream that changes resolution gets a new pool; the old imports age out here.
const MAX_IMPORTS: usize = 32;

unsafe extern "C" fn pick_vaapi(
    _context: *mut ff::AVCodecContext,
    formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // Safety: FFmpeg's list.
    unsafe { crate::decode::pick_format(formats, ff::AVPixelFormat::AV_PIX_FMT_VAAPI) }
}

pub(crate) struct VaapiDecoder {
    // Field order is drop order: the decoder (and its surfaces) first.
    decoder: FfDecoder,
    /// The DRM PRIME view of the frame being handed over; reused.
    mapped: *mut ff::AVFrame,
    imports: Vec<Import>,
    slots: Option<SlotRing>,
    work: Work,
    surface: Arc<WgpuSurface>,
    pictures: u64,
    /// Imports made (statistics: a steady stream makes one per pool surface and no more), and
    /// the time they took.
    imported: u64,
    import_time: std::time::Duration,
    described: bool,
    _va: Arc<VaDevice>,
}

// Safety: the raw frame is owned and used by the one decoder thread.
unsafe impl Send for VaapiDecoder {}

impl VaapiDecoder {
    pub fn open(
        device: Arc<GpuDevice>,
        va: Arc<VaDevice>,
        codec: Codec,
        description: Option<&[u8]>,
        target: &StreamTarget,
    ) -> DecodeResult<Self> {
        for (name, extension) in [
            ("VK_EXT_external_memory_dma_buf", super::EXTERNAL_MEMORY_DMA_BUF),
            (
                "VK_EXT_image_drm_format_modifier",
                super::IMAGE_DRM_FORMAT_MODIFIER,
            ),
            ("VK_KHR_external_memory_fd", super::EXTERNAL_MEMORY_FD),
        ] {
            if !device.has_extension(extension) {
                return Err(DecodeError::decoder(format!(
                    "VA-API: the GPU device has no {name} (DMA-BUFs cannot be imported)"
                )));
            }
        }
        let work = Work::new(device)?;
        let decoder = FfDecoder::open(
            codec,
            description,
            Some((va.hw, pick_vaapi)),
            1,
            target.reorder(),
        )?;
        // Safety: plain allocation; freed in `Drop`.
        let mapped = unsafe { ff::av_frame_alloc() };
        if mapped.is_null() {
            return Err(DecodeError::decoder("out of memory"));
        }
        Ok(Self {
            decoder,
            mapped,
            imports: Vec::with_capacity(MAX_IMPORTS),
            slots: None,
            work,
            surface: target.surface.clone(),
            pictures: 0,
            imported: 0,
            import_time: std::time::Duration::ZERO,
            described: false,
            _va: va,
        })
    }

    /// The imported planes of the surface `planes` describes: from the cache, or imported now.
    fn import(&mut self, planes: &[DbvDrmPlane; 2], coded: (u32, u32)) -> DecodeResult<usize> {
        let mut files = [(0, 0); 2];
        for (file, plane) in files.iter_mut().zip(planes) {
            // Safety: a zeroed stat buffer and a valid fd.
            let mut stat: libc::stat = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstat(plane.fd, &mut stat) } != 0 {
                return Err(DecodeError::decoder("VA-API: fstat on a DMA-BUF failed"));
            }
            *file = (stat.st_dev as u64, stat.st_ino as u64);
        }
        let key = ImportKey {
            files,
            offsets: [planes[0].offset, planes[1].offset],
            pitches: [planes[0].pitch, planes[1].pitch],
            modifier: planes[0].modifier,
            coded,
        };
        if let Some(index) = self.imports.iter().position(|import| import.key == key) {
            return Ok(index);
        }
        if self.imports.len() == MAX_IMPORTS {
            let oldest = self
                .imports
                .iter()
                .enumerate()
                .min_by_key(|(_, import)| import.used)
                .map(|(index, _)| index)
                .unwrap_or(0);
            let import = self.imports.swap_remove(oldest);
            self.destroy(import);
        }
        let started = std::time::Instant::now();
        let device = &self.work.device;
        let (chroma_width, chroma_height) = (coded.0.div_ceil(2), coded.1.div_ceil(2));
        // Safety: the fds are the frame's DMA-BUFs, alive for this call; what is created is
        // destroyed on the error path and in `destroy`.
        let luma = unsafe {
            import_plane(
                device,
                &planes[0],
                vk::Format::R8_UNORM,
                coded.0,
                coded.1,
            )?
        };
        let chroma = match unsafe {
            import_plane(
                device,
                &planes[1],
                vk::Format::R8G8_UNORM,
                chroma_width,
                chroma_height,
            )
        } {
            Ok(chroma) => chroma,
            Err(error) => {
                // Safety: ours, never submitted.
                unsafe {
                    device.raw.destroy_image(luma.image, None);
                    device.raw.free_memory(luma.memory, None);
                }
                return Err(error);
            }
        };
        self.imported += 1;
        self.import_time += started.elapsed();
        self.imports.push(Import {
            key,
            planes: [luma, chroma],
            used: 0,
        });
        Ok(self.imports.len() - 1)
    }

    fn destroy(&self, import: Import) {
        for plane in import.planes {
            // Safety: every submission that used the images was waited for.
            unsafe {
                self.work.device.raw.destroy_image(plane.image, None);
                self.work.device.raw.free_memory(plane.memory, None);
            }
        }
    }

    fn hand_over(&mut self, info: &FrameInfo, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if info.format != ff::AVPixelFormat::AV_PIX_FMT_VAAPI as c_int {
            return Err(DecodeError::decoder(format!(
                "the VA-API decoder returned a software frame (format {})",
                info.format
            )));
        }
        if info.sw_format != ff::AVPixelFormat::AV_PIX_FMT_NV12 as c_int {
            return Err(DecodeError::decoder(format!(
                "unsupported picture format {} (only 8-bit 4:2:0)",
                info.sw_format
            )));
        }
        let (width, height) = (info.width, info.height);
        let coded = (info.coded_width.max(width), info.coded_height.max(height));
        if width == 0 || height == 0 {
            return Err(DecodeError::decoder("the decoder returned an empty picture"));
        }

        // Safety: `mapped` is a blank frame of ours; FFmpeg fills it with a reference to the
        // VA frame and the descriptor of its DMA-BUFs. For a VA-API source this is
        // `vaapi_map_to_drm_esh` (libavutil/hwcontext_vaapi.c): `vaSyncSurface`, so the
        // decode has completed, then `vaExportSurfaceHandle(DRM_PRIME_2, READ_ONLY |
        // SEPARATE_LAYERS)`; a driver without that entry point gets the older
        // `vaDeriveImage` + `vaAcquireBufferHandle` path, one layer of two planes.
        unsafe {
            (*self.mapped).format = ff::AVPixelFormat::AV_PIX_FMT_DRM_PRIME as c_int;
            let mapped = ff::av_hwframe_map(
                self.mapped,
                self.decoder.frame,
                ff::AV_HWFRAME_MAP_READ as c_int,
            );
            if mapped < 0 {
                return Err(DecodeError::decoder(format!(
                    "VA-API: the driver did not export the decoded surface as DMA-BUFs: {}",
                    av_error(mapped)
                )));
            }
        }
        let handed = self.copy_mapped(info, coded, cx);
        // Closes the exported fds (our imports hold their own).
        // Safety: ours.
        unsafe { ff::av_frame_unref(self.mapped) };
        handed
    }

    fn copy_mapped(
        &mut self,
        info: &FrameInfo,
        coded: (u32, u32),
        cx: &FrameCx<'_>,
    ) -> DecodeResult<bool> {
        let (width, height) = (info.width, info.height);
        let mut planes = [DbvDrmPlane::default(); 4];
        // Safety: a mapped DRM PRIME frame and room for four planes.
        let count = unsafe { dbv_drm_planes(self.mapped, planes.as_mut_ptr(), 4) };
        let formats = [planes[0].format, planes[1].format];
        // NV12 as the export describes it: a layer per plane (R8, then GR88: Mesa's
        // `vlVaExportSurfaceHandle` and the Intel media driver with SEPARATE_LAYERS), or one
        // NV12 layer with two planes (the derive-image path). RG88 for the chroma plane is
        // what some exporters call the same bytes (Cb first): the plane is always the
        // surface's interleaved CbCr, whatever the name.
        let nv12 = count == 2
            && (formats == [DRM_FORMAT_NV12; 2]
                || (formats[0] == DRM_FORMAT_R8
                    && (formats[1] == DRM_FORMAT_GR88 || formats[1] == DRM_FORMAT_RG88)));
        if !nv12 {
            return Err(DecodeError::decoder(format!(
                "VA-API: unexpected surface export: {count} planes, formats {:?}",
                formats.map(|format| String::from_utf8_lossy(&format.to_le_bytes()).into_owned())
            )));
        }
        let planes = [planes[0], planes[1]];
        if !self.described {
            self.described = true;
            log::info!(
                "video: VA-API surfaces: {}x{} coded for {width}x{height}, modifier {:#x}, luma {} @ {} pitch {}, chroma {} @ {} pitch {} ({})",
                coded.0,
                coded.1,
                planes[0].modifier,
                String::from_utf8_lossy(&formats[0].to_le_bytes()),
                planes[0].offset,
                planes[0].pitch,
                String::from_utf8_lossy(&formats[1].to_le_bytes()),
                planes[1].offset,
                planes[1].pitch,
                if planes[0].fd == planes[1].fd {
                    "one DMA-BUF"
                } else {
                    "a DMA-BUF per plane"
                },
            );
        }
        let index = self.import(&planes, coded)?;
        self.pictures += 1;
        self.imports[index].used = self.pictures;

        let color = surface_color(info.color);
        if self
            .slots
            .as_ref()
            .is_none_or(|slots| slots.size != (width, height))
        {
            self.slots = None;
            self.slots = Some(SlotRing::new(&self.work, width, height, color, false, false)?);
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
        let import = &self.imports[index];
        let foreign = work.device.foreign_queue_family();
        // The planes belong to the VA driver between pictures. Vulkan's way to say "another
        // API wrote this image, keep what it holds" is a queue family ownership transfer from
        // VK_QUEUE_FAMILY_FOREIGN_EXT (VK_EXT_queue_family_foreign; Vulkan spec, "Queue Family
        // Ownership Transfer": the contents are preserved when both halves of the transfer
        // name the same layouts). The layout a foreign owner leaves an image in is not
        // something Vulkan can know: GENERAL is the convention (wlroots' Vulkan renderer,
        // render/vulkan/pass.c, acquires client DMA-BUFs exactly so), and for a DRM-modifier
        // image it describes the memory as the modifier defines it.
        let acquire = |image| {
            barrier(
                image,
                vk::ImageLayout::GENERAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_READ,
            )
            .src_queue_family_index(foreign)
            .dst_queue_family_index(work.device.aux_family)
        };
        let release = |image| {
            barrier(
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::GENERAL,
                vk::AccessFlags::TRANSFER_READ,
                vk::AccessFlags::empty(),
            )
            .src_queue_family_index(work.device.aux_family)
            .dst_queue_family_index(foreign)
        };
        let to_slot = |image| {
            barrier(
                image,
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            )
        };
        let sampled = |image| {
            barrier(
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::empty(),
            )
        };
        let copy = |width, height| {
            [vk::ImageCopy::default()
                .src_subresource(COLOR_LAYER)
                .dst_subresource(COLOR_LAYER)
                .extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })]
        };
        // Safety: the imported planes hold a finished picture (`vaSyncSurface` ran in the
        // map) that nothing writes while the frame is referenced; the slot images are ours
        // and free; every image returns to the state its other user expects; waited for.
        unsafe {
            work.begin()?;
            work.barriers(&[
                acquire(import.planes[0].image),
                acquire(import.planes[1].image),
                to_slot(slot.luma),
                to_slot(slot.chroma),
            ]);
            // The surface is coded-size (1920x1088 for 1080p); only the picture is copied.
            raw.cmd_copy_image(
                work.commands,
                import.planes[0].image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                slot.luma,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &copy(width, height),
            );
            raw.cmd_copy_image(
                work.commands,
                import.planes[1].image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                slot.chroma,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &copy(width.div_ceil(2), height.div_ceil(2)),
            );
            work.barriers(&[
                release(import.planes[0].image),
                release(import.planes[1].image),
                sampled(slot.luma),
                sampled(slot.chroma),
            ]);
            work.run()?;
        }
        if let Some((row, sender)) = cx.capture.take(height) {
            let _ = sender.send(read_luma_row(
                work, slot.luma, width, height, row, cx.serial, false,
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
            cx.counters.frame_decoded(cx.serial);
            if !cx.admit(self.decoder.frame_pts()) {
                self.decoder.unref_frame();
                continue;
            }
            let info = self.decoder.frame_info();
            let handed = self.hand_over(&info, cx);
            // The surface goes back to the decoder's pool: the picture is in a slot.
            self.decoder.unref_frame();
            if handed? {
                published += 1;
            }
        }
        Ok(published)
    }
}

impl Drop for VaapiDecoder {
    fn drop(&mut self) {
        for import in std::mem::take(&mut self.imports) {
            self.destroy(import);
        }
        // Safety: ours.
        unsafe { ff::av_frame_free(&mut self.mapped) };
        if self.pictures > 0 {
            log::info!(
                "video: VA-API decoder closed: {} pictures, {} surface imports ({:.2} ms in all)",
                self.pictures,
                self.imported,
                self.import_time.as_secs_f64() * 1000.0
            );
        }
    }
}

impl PictureDecoder for VaapiDecoder {
    fn backend(&self) -> DecodeBackend {
        DecodeBackend::Vaapi
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

/// One plane of a VA surface as an image of the renderer's device: `format` texels of
/// `width` x `height` at `plane.offset` in the DMA-BUF, rows `plane.pitch` bytes apart, laid
/// out as `plane.modifier` says. The image is only ever a transfer source.
///
/// # Safety
/// `plane.fd` is a DMA-BUF that stays open for the call.
unsafe fn import_plane(
    device: &GpuDevice,
    plane: &DbvDrmPlane,
    format: vk::Format,
    width: u32,
    height: u32,
) -> DecodeResult<PlaneImage> {
    let unsupported = |what: &str| {
        DecodeError::decoder(format!(
            "VA-API: {what} ({format:?} {width}x{height}, DRM modifier {:#x})",
            plane.modifier
        ))
    };
    if plane.modifier == DRM_FORMAT_MOD_INVALID {
        // Without a modifier the buffer's layout is the exporting driver's secret (a driver
        // built without DRM format modifier support): it cannot be described to Vulkan.
        return Err(unsupported(
            "the VA driver exports surfaces without a DRM format modifier",
        ));
    }
    let usage = vk::ImageUsageFlags::TRANSFER_SRC;
    // Safety: the caller's contract; plain queries and object creation, undone on failure.
    unsafe {
        // Can this device make such an image from a DMA-BUF at all?
        // (VK_EXT_image_drm_format_modifier: the modifier is part of the format query.)
        let mut modifier_info = vk::PhysicalDeviceImageDrmFormatModifierInfoEXT::default()
            .drm_format_modifier(plane.modifier)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let mut external_info = vk::PhysicalDeviceExternalImageFormatInfo::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let format_info = vk::PhysicalDeviceImageFormatInfo2::default()
            .format(format)
            .ty(vk::ImageType::TYPE_2D)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(usage)
            .push_next(&mut external_info)
            .push_next(&mut modifier_info);
        let mut external_properties = vk::ExternalImageFormatProperties::default();
        let mut properties =
            vk::ImageFormatProperties2::default().push_next(&mut external_properties);
        if device
            .instance
            .get_physical_device_image_format_properties2(
                device.physical_device,
                &format_info,
                &mut properties,
            )
            .is_err()
        {
            return Err(unsupported(
                "the Vulkan driver cannot use a plane of the VA driver's surfaces",
            ));
        }
        if !external_properties
            .external_memory_properties
            .external_memory_features
            .contains(vk::ExternalMemoryFeatureFlags::IMPORTABLE)
        {
            return Err(unsupported(
                "the Vulkan driver cannot import the VA driver's surfaces",
            ));
        }

        // The plane's place in the buffer, as exported. `size` must be 0 and the array and
        // depth pitches are ignored for a 2D image
        // (VkImageDrmFormatModifierExplicitCreateInfoEXT).
        let layouts = [vk::SubresourceLayout {
            offset: plane.offset,
            size: 0,
            row_pitch: plane.pitch,
            array_pitch: 0,
            depth_pitch: 0,
        }];
        let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(plane.modifier)
            .plane_layouts(&layouts);
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let raw = &device.raw;
        let image = raw
            .create_image(
                &vk::ImageCreateInfo::default()
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
                    .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
                    .usage(usage)
                    // Only the aux queue family touches it (ownership moves between it and
                    // the foreign family around each copy).
                    .sharing_mode(vk::SharingMode::EXCLUSIVE)
                    .initial_layout(vk::ImageLayout::UNDEFINED)
                    .push_next(&mut external)
                    .push_next(&mut explicit),
                None,
            )
            .map_err(|e| vk_error("VA-API: vkCreateImage for a surface plane", e))?;
        let imported = (|| -> DecodeResult<vk::DeviceMemory> {
            let export = device
                .external_memory_fd()
                .ok_or_else(|| unsupported("no VK_KHR_external_memory_fd"))?;
            let mut dedicated_requirements = vk::MemoryDedicatedRequirements::default();
            let mut requirements =
                vk::MemoryRequirements2::default().push_next(&mut dedicated_requirements);
            raw.get_image_memory_requirements2(
                &vk::ImageMemoryRequirementsInfo2::default().image(image),
                &mut requirements,
            );
            let requirements = requirements.memory_requirements;
            let mut fd_properties = vk::MemoryFdPropertiesKHR::default();
            export
                .get_memory_fd_properties(
                    vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT,
                    plane.fd,
                    &mut fd_properties,
                )
                .map_err(|e| vk_error("VA-API: vkGetMemoryFdPropertiesKHR", e))?;
            let types = requirements.memory_type_bits & fd_properties.memory_type_bits;
            if types == 0 {
                return Err(unsupported(
                    "no memory type takes both the image and the DMA-BUF",
                ));
            }
            // The whole buffer is imported (several planes may share it, each import with an
            // fd of its own): its size is the file's.
            let file_size = libc::lseek(plane.fd, 0, libc::SEEK_END);
            let size = if file_size > 0 {
                file_size as u64
            } else {
                plane.size
            };
            if size < requirements.size {
                return Err(unsupported(&format!(
                    "the image needs {} bytes and the DMA-BUF has {size}",
                    requirements.size
                )));
            }
            // Vulkan takes ownership of the fd it imports (VkImportMemoryFdInfoKHR): it gets
            // a duplicate, the frame keeps its own.
            let fd = libc::fcntl(plane.fd, libc::F_DUPFD_CLOEXEC, 0);
            if fd < 0 {
                return Err(DecodeError::decoder(
                    "VA-API: duplicating a DMA-BUF fd failed",
                ));
            }
            let mut import = vk::ImportMemoryFdInfoKHR::default()
                .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
                .fd(fd);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let mut allocation = vk::MemoryAllocateInfo::default()
                .allocation_size(size)
                .memory_type_index(types.trailing_zeros())
                .push_next(&mut import);
            if dedicated_requirements.requires_dedicated_allocation != 0
                || dedicated_requirements.prefers_dedicated_allocation != 0
            {
                allocation = allocation.push_next(&mut dedicated);
            }
            let memory = raw.allocate_memory(&allocation, None).map_err(|e| {
                libc::close(fd);
                vk_error("VA-API: importing a DMA-BUF (vkAllocateMemory)", e)
            })?;
            if let Err(error) = raw.bind_image_memory(image, memory, 0) {
                raw.free_memory(memory, None);
                return Err(vk_error("VA-API: vkBindImageMemory", error));
            }
            Ok(memory)
        })();
        match imported {
            Ok(memory) => Ok(PlaneImage { image, memory }),
            Err(error) => {
                raw.destroy_image(image, None);
                Err(error)
            }
        }
    }
}

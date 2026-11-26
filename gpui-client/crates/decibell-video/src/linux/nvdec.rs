//! NVDEC: NVIDIA's decoder through FFmpeg's `nvdec` hwaccel, for NVIDIA GPUs and drivers where
//! Vulkan Video is missing or fails. No picture data touches the CPU.
//!
//! ```text
//! avcodec_send_packet / receive_frame       cuvid decodes; the frame is the decoder's own
//!   -> AV_PIX_FMT_CUDA frame                 output surface, mapped (pitch-linear NV12 in
//!                                            CUDA device memory)
//! cuMemcpy2DAsync x 2, device to device  --> a free slot's luma and chroma images: Vulkan
//!                                            images of the renderer's device whose memory was
//!                                            imported into CUDA once, when the slots were made
//! cuSignalExternalSemaphoresAsync        --> a Vulkan semaphore, also imported once
//! vkQueueSubmit (no commands) waiting on that semaphore, with a fence; the thread sleeps on
//! the fence, then publishes the slot
//! ```
//!
//! **Interop: `VK_KHR_external_memory_fd` + `cuImportExternalMemory`**, the mechanism FFmpeg's
//! own `hwupload` from CUDA to Vulkan and mpv / libplacebo use, and the only one CUDA documents
//! for Vulkan (`cuGraphicsVkRegisterImage` does not exist; the `cuGraphics*` family is OpenGL
//! and Direct3D). Each slot image has a dedicated, exportable allocation; its fd is handed to
//! CUDA, which maps the image as a `CUarray` (it understands the driver's own image layout, so
//! the copy lands in the image whatever its tiling). Decoding straight into such an image is
//! not possible: cuvid owns its output surfaces.
//!
//! **Synchronisation is explicit on both sides.** CUDA writes a slot only when nothing samples
//! it (the slot rule: the decoder holds the only reference, i.e. every renderer submission that
//! sampled it has completed). Vulkan samples it only after the copy: the copy and the semaphore
//! signal are queued on one CUDA stream, in that order, and the picture is published when a
//! Vulkan submission that waits on the semaphore has completed. A CUDA library that cannot
//! import semaphores (none is known for a driver new enough for external memory) is given
//! `cuStreamSynchronize` instead.
//!
//! Per frame: no allocation of ours (the slots, the imports, the semaphore and the fence are
//! made once), one device-to-device copy of the picture.
//!
//! What it costs that Vulkan Video does not: a CUDA context in the process (host and GPU
//! memory: docs/video-design.md §11). It is created at the first NVDEC stream and freed with
//! the last.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::Arc;

use ash::vk;
use ffmpeg_sys_next as ff;
use gpui_wgpu::WgpuSurface;

use super::slots::{SlotRing, Work, read_luma_row};
use super::{GpuDevice, classify, surface_color, vk_error};
use crate::codec::{Codec, DecodeBackend};
use crate::decode::{DecodeError, DecodeResult, FfDecoder, FrameCx, FrameInfo, PictureDecoder};
use crate::hub::StreamTarget;
use crate::packet::Packet;

#[repr(C)]
struct DbvCuda {
    _private: [u8; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct DbvCudaPlane {
    memory: *mut c_void,
    mipmap: *mut c_void,
    array: *mut c_void,
}

impl Default for DbvCudaPlane {
    fn default() -> Self {
        Self {
            memory: std::ptr::null_mut(),
            mipmap: std::ptr::null_mut(),
            array: std::ptr::null_mut(),
        }
    }
}

unsafe extern "C" {
    fn dbv_cuda_open(uuid: *const u8, err: *mut c_char, err_len: c_int) -> *mut DbvCuda;
    fn dbv_cuda_close(cuda: *mut DbvCuda);
    fn dbv_cuda_device(cuda: *mut DbvCuda) -> *mut ff::AVBufferRef;
    fn dbv_cuda_has_semaphores(cuda: *mut DbvCuda) -> c_int;
    fn dbv_cuda_import_plane(
        cuda: *mut DbvCuda,
        fd: c_int,
        size: u64,
        width: c_int,
        height: c_int,
        channels: c_int,
        out: *mut DbvCudaPlane,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
    fn dbv_cuda_free_plane(cuda: *mut DbvCuda, plane: *mut DbvCudaPlane);
    fn dbv_cuda_import_semaphore(
        cuda: *mut DbvCuda,
        fd: c_int,
        out: *mut *mut c_void,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
    fn dbv_cuda_free_semaphore(cuda: *mut DbvCuda, semaphore: *mut c_void);
    fn dbv_cuda_copy(
        cuda: *mut DbvCuda,
        frame: *const ff::AVFrame,
        width: c_int,
        height: c_int,
        luma: *const DbvCudaPlane,
        chroma: *const DbvCudaPlane,
        semaphore: *mut c_void,
        err: *mut c_char,
        err_len: c_int,
    ) -> c_int;
}

/// Runs a shim call that reports failures as text.
fn cuda_call(call: impl FnOnce(*mut c_char, c_int) -> c_int) -> Result<(), (c_int, String)> {
    let mut message = [0 as c_char; 160];
    let result = call(message.as_mut_ptr(), message.len() as c_int);
    if result == 0 {
        return Ok(());
    }
    // Safety: the shim terminated what it wrote; the buffer started zeroed.
    let text = unsafe { CStr::from_ptr(message.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Err((result, text))
}

/// The process's CUDA context on the renderer's GPU, with FFmpeg's device context around it.
/// Alive while an NVDEC decoder is.
pub(crate) struct CudaDevice {
    raw: *mut DbvCuda,
    /// The GPU it is on (`VkPhysicalDeviceIDProperties::deviceUUID`).
    pub uuid: [u8; 16],
}

// Safety: every shim call makes the CUDA context current for its own duration (push / pop), and
// CUDA contexts may be used from any thread that way.
unsafe impl Send for CudaDevice {}
unsafe impl Sync for CudaDevice {}

impl CudaDevice {
    /// Loads CUDA and creates the context on the GPU `device` renders with.
    pub fn open(device: &GpuDevice) -> DecodeResult<Arc<Self>> {
        let uuid = device.uuid().ok_or_else(|| {
            DecodeError::decoder("the Vulkan driver does not report the GPU's UUID")
        })?;
        let started = std::time::Instant::now();
        let mut message = [0 as c_char; 160];
        // Safety: 16 readable bytes and a writable message buffer.
        let raw =
            unsafe { dbv_cuda_open(uuid.as_ptr(), message.as_mut_ptr(), message.len() as c_int) };
        if raw.is_null() {
            // Safety: the shim wrote a terminated message.
            let text = unsafe { CStr::from_ptr(message.as_ptr()) }.to_string_lossy();
            return Err(DecodeError::decoder(format!("NVDEC: {text}")));
        }
        log::info!(
            "video: CUDA context created for NVDEC in {:.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
        Ok(Arc::new(Self { raw, uuid }))
    }
}

impl Drop for CudaDevice {
    fn drop(&mut self) {
        let started = std::time::Instant::now();
        // Safety: ours; every decoder (and with it every import) is gone.
        unsafe { dbv_cuda_close(self.raw) };
        log::info!(
            "video: CUDA context released in {:.0} ms",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// A stream's slots with each image imported into CUDA.
struct CudaSlots {
    // Dropped after the imports below (`Drop`), and the imports before the CUDA context.
    ring: SlotRing,
    /// Luma and chroma of `ring.slots()[i]`.
    planes: Vec<[DbvCudaPlane; 2]>,
    cuda: Arc<CudaDevice>,
}

impl CudaSlots {
    fn new(
        work: &Work,
        cuda: &Arc<CudaDevice>,
        width: u32,
        height: u32,
        color: gpui_wgpu::SurfaceColor,
    ) -> DecodeResult<Self> {
        let device = &work.device;
        let export = device.external_memory_fd().ok_or_else(|| {
            DecodeError::decoder("NVDEC: the GPU device has no VK_KHR_external_memory_fd")
        })?;
        let ring = SlotRing::new(work, width, height, color, true)?;
        let mut slots = Self {
            planes: Vec::with_capacity(ring.slots().len()),
            ring,
            cuda: cuda.clone(),
        };
        let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
        for index in 0..slots.ring.slots().len() {
            let memory = slots.ring.slots()[index].memory;
            let mut planes = [DbvCudaPlane::default(); 2];
            for (plane, (width, height, channels)) in
                [(width, height, 1), (chroma_width, chroma_height, 2)]
                    .into_iter()
                    .enumerate()
            {
                let (memory, size) = memory[plane];
                // Safety: the memory was allocated exportable as an opaque fd and is alive.
                let fd = unsafe {
                    export.get_memory_fd(
                        &vk::MemoryGetFdInfoKHR::default()
                            .memory(memory)
                            .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
                    )
                }
                .map_err(|e| vk_error("vkGetMemoryFdKHR", e))?;
                // Safety: a valid fd and an out-struct; the shim says who owns the fd after.
                let imported = cuda_call(|err, err_len| unsafe {
                    dbv_cuda_import_plane(
                        cuda.raw,
                        fd,
                        size,
                        width as c_int,
                        height as c_int,
                        channels,
                        &mut planes[plane],
                        err,
                        err_len,
                    )
                });
                if let Err((code, message)) = imported {
                    if code == -1 {
                        // The import did not take the fd.
                        // Safety: ours, open.
                        unsafe { libc::close(fd) };
                    }
                    // The planes imported so far go with `slots`.
                    slots.planes.push(planes);
                    return Err(DecodeError::decoder(format!(
                        "NVDEC: importing a {width}x{height} image into CUDA: {message}"
                    )));
                }
            }
            slots.planes.push(planes);
        }
        Ok(slots)
    }
}

// Safety: CUDA handles, used under the context by the one decoder thread.
unsafe impl Send for CudaSlots {}

impl Drop for CudaSlots {
    fn drop(&mut self) {
        for planes in &mut self.planes {
            for plane in planes {
                // Safety: imported by this context, or zero (the shim ignores those).
                unsafe { dbv_cuda_free_plane(self.cuda.raw, plane) };
            }
        }
    }
}

/// The Vulkan semaphore CUDA signals when a picture's copy has run, and CUDA's import of it.
struct CopyDone {
    vulkan: vk::Semaphore,
    cuda_handle: *mut c_void,
    cuda: Arc<CudaDevice>,
    device: Arc<GpuDevice>,
}

// Safety: handles; used by the one decoder thread.
unsafe impl Send for CopyDone {}

impl CopyDone {
    /// `Ok(None)`: this CUDA or this device cannot share a semaphore; the copy is waited for
    /// on the CUDA side.
    fn new(device: &Arc<GpuDevice>, cuda: &Arc<CudaDevice>) -> DecodeResult<Option<Self>> {
        // Safety: a live shim handle.
        if unsafe { dbv_cuda_has_semaphores(cuda.raw) } == 0 {
            return Ok(None);
        }
        let Some(export) = device.external_semaphore_fd() else {
            return Ok(None);
        };
        // Safety: plain object creation; destroyed on the error paths and in `Drop`.
        unsafe {
            let mut exportable = vk::ExportSemaphoreCreateInfo::default()
                .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
            let vulkan = device
                .raw
                .create_semaphore(
                    &vk::SemaphoreCreateInfo::default().push_next(&mut exportable),
                    None,
                )
                .map_err(|e| vk_error("vkCreateSemaphore", e))?;
            let fd = match export.get_semaphore_fd(
                &vk::SemaphoreGetFdInfoKHR::default()
                    .semaphore(vulkan)
                    .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
            ) {
                Ok(fd) => fd,
                Err(error) => {
                    device.raw.destroy_semaphore(vulkan, None);
                    return Err(vk_error("vkGetSemaphoreFdKHR", error));
                }
            };
            let mut cuda_handle = std::ptr::null_mut();
            let imported = cuda_call(|err, err_len| {
                dbv_cuda_import_semaphore(cuda.raw, fd, &mut cuda_handle, err, err_len)
            });
            if let Err((_, message)) = imported {
                libc::close(fd);
                device.raw.destroy_semaphore(vulkan, None);
                log::warn!(
                    "video: NVDEC: {message}; copies are waited for with cuStreamSynchronize"
                );
                return Ok(None);
            }
            Ok(Some(Self {
                vulkan,
                cuda_handle,
                cuda: cuda.clone(),
                device: device.clone(),
            }))
        }
    }
}

impl Drop for CopyDone {
    fn drop(&mut self) {
        // Safety: ours; no submission waiting on the semaphore is pending (each was waited for).
        unsafe {
            dbv_cuda_free_semaphore(self.cuda.raw, self.cuda_handle);
            self.device.raw.destroy_semaphore(self.vulkan, None);
        }
    }
}

unsafe extern "C" fn pick_cuda(
    _context: *mut ff::AVCodecContext,
    formats: *const ff::AVPixelFormat,
) -> ff::AVPixelFormat {
    // Safety: FFmpeg's list.
    unsafe { crate::decode::pick_format(formats, ff::AVPixelFormat::AV_PIX_FMT_CUDA) }
}

pub(crate) struct NvdecDecoder {
    // Field order is drop order: the decoder (cuvid, in the CUDA context), then our imports,
    // then the Vulkan objects, then the CUDA context's last reference.
    decoder: FfDecoder,
    slots: Option<CudaSlots>,
    copy_done: Option<CopyDone>,
    work: Work,
    surface: Arc<WgpuSurface>,
    cuda: Arc<CudaDevice>,
}

impl NvdecDecoder {
    pub fn open(
        device: Arc<GpuDevice>,
        cuda: Arc<CudaDevice>,
        codec: Codec,
        description: Option<&[u8]>,
        target: &StreamTarget,
    ) -> DecodeResult<Self> {
        let work = Work::new(device.clone())?;
        let copy_done = CopyDone::new(&device, &cuda)?;
        // Safety: a live shim handle; the reference is ours until unreferenced below (the
        // codec context takes its own).
        let mut hw_device = unsafe { dbv_cuda_device(cuda.raw) };
        if hw_device.is_null() {
            return Err(DecodeError::decoder("out of memory"));
        }
        let decoder = FfDecoder::open(
            codec,
            description,
            Some((hw_device, pick_cuda)),
            1,
            target.reorder(),
        );
        // Safety: our reference.
        unsafe { ff::av_buffer_unref(&mut hw_device) };
        Ok(Self {
            decoder: decoder?,
            slots: None,
            copy_done,
            work,
            surface: target.surface.clone(),
            cuda,
        })
    }

    fn hand_over(&mut self, info: &FrameInfo, cx: &FrameCx<'_>) -> DecodeResult<bool> {
        if info.format != ff::AVPixelFormat::AV_PIX_FMT_CUDA as c_int {
            return Err(DecodeError::decoder(format!(
                "the NVDEC decoder returned a software frame (format {})",
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
        if width == 0 || height == 0 || info.data[0].is_null() || info.data[1].is_null() {
            return Err(DecodeError::decoder("the decoder returned an empty picture"));
        }
        let color = surface_color(info.color);
        if self
            .slots
            .as_ref()
            .is_none_or(|slots| slots.ring.size != (width, height))
        {
            self.slots = None;
            self.slots = Some(CudaSlots::new(&self.work, &self.cuda, width, height, color)?);
            cx.counters.set_size(width, height);
        }
        let Some(slots) = self.slots.as_mut() else {
            return Ok(false);
        };
        let Some((index, slot)) = slots.ring.free_indexed(color) else {
            cx.counters
                .no_slot
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(false);
        };
        let planes = &slots.planes[index];
        let semaphore = self
            .copy_done
            .as_ref()
            .map_or(std::ptr::null_mut(), |done| done.cuda_handle);
        // Safety: the frame is a mapped CUDA frame of at least `width` x `height`; the planes
        // are imports of the free slot's images, which nothing reads now.
        cuda_call(|err, err_len| unsafe {
            dbv_cuda_copy(
                self.cuda.raw,
                self.decoder.frame,
                width as c_int,
                height as c_int,
                &planes[0],
                &planes[1],
                semaphore,
                err,
                err_len,
            )
        })
        .map_err(|(_, message)| DecodeError::decoder(format!("NVDEC: {message}")))?;
        if let Some(done) = &self.copy_done {
            // The picture is complete when CUDA has signalled: a submission with nothing in it
            // but the wait, so that our fence (and anything submitted after) is behind the copy.
            let semaphores = [done.vulkan];
            let stages = [vk::PipelineStageFlags::ALL_COMMANDS];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&semaphores)
                .wait_dst_stage_mask(&stages);
            // Safety: a valid submission for any queue; the semaphore has a signal pending.
            unsafe { self.work.device.submit_and_wait(&submit, self.work.fence)? };
        }
        if let Some((row, sender)) = cx.capture.take(height) {
            let _ = sender.send(read_luma_row(
                &self.work, slot.luma, width, height, row, cx.serial,
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
            // Unmaps the decoder's surface: it is free for the next picture.
            self.decoder.unref_frame();
            if handed? {
                published += 1;
            }
        }
        Ok(published)
    }
}

impl PictureDecoder for NvdecDecoder {
    fn backend(&self) -> DecodeBackend {
        DecodeBackend::Nvdec
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

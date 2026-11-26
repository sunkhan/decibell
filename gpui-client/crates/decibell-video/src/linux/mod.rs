//! Linux: the renderer's own Vulkan device, shared with FFmpeg.
//!
//! - [`install_device_hook`] runs when gpui_wgpu creates its `VkDevice`: it adds the Vulkan Video
//!   extensions and one queue in each of two families the renderer never submits on (decode,
//!   and "aux": compute + transfer). It costs a few extension-name comparisons at start-up and
//!   two idle queues; nothing else exists until a stream is displayed.
//! - [`GpuDevice`] is that device as the decoders use it: the aux queue for picture copies and
//!   uploads, and (made on the first hardware decode) the FFmpeg `AVHWDeviceContext` around it.
//! - [`slots`]: the plane textures a stream's pictures are handed over in.
//! - [`hw`]: Vulkan Video decode + a GPU copy into a slot. [`upload`]: software pictures into
//!   the same slots through a persistent staging buffer.
//!
//! Why a copy and not the decoded image itself: docs/video-design.md §3.

mod hw;
mod slots;
mod upload;

use std::ffi::{CStr, c_char, c_int};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use ash::vk;
use ffmpeg_sys_next as ff;
use gpui_wgpu::wgpu::hal::api::Vulkan;
use gpui_wgpu::{WgpuContext, wgpu};
use parking_lot::Mutex;

use crate::codec::{Codec, CodecSupport, DecodePath};
use crate::decode::{
    Backend, ColorInfo, DecodeError, DecodeResult, Matrix, PictureDecoder, SoftwareDecoder,
    av_error, has_software_decoder,
};
use crate::hub::StreamTarget;

unsafe extern "C" {
    fn dbv_vk_device(
        inst: vk::Instance,
        gipa: vk::PFN_vkGetInstanceProcAddr,
        phys: vk::PhysicalDevice,
        dev: vk::Device,
        inst_ext: *const *const c_char,
        nb_inst_ext: c_int,
        dev_ext: *const *const c_char,
        nb_dev_ext: c_int,
        aux_family: c_int,
        decode_family: c_int,
        device_queue_lock: extern "C" fn(c_int),
        err: *mut c_int,
    ) -> *mut ff::AVBufferRef;
}

/// The queue families the hook settled on for the device it last ran for.
#[derive(Clone, Copy, Debug)]
struct VideoQueues {
    /// Compute + transfer: FFmpeg's pool-image setup and our picture copies. A family the
    /// renderer never uses when the device has one; otherwise the renderer's own.
    aux_family: u32,
    /// The aux queue is the renderer's own queue (the device has no other compute queue).
    shared: bool,
    decode_family: u32,
}

static QUEUES: Mutex<Option<VideoQueues>> = Mutex::new(None);

const VIDEO_QUEUE: &CStr = c"VK_KHR_video_queue";
const VIDEO_DECODE_QUEUE: &CStr = c"VK_KHR_video_decode_queue";
const OPTIONAL_EXTENSIONS: [&CStr; 5] = [
    c"VK_KHR_video_decode_h264",
    c"VK_KHR_video_decode_h265",
    c"VK_KHR_video_decode_av1",
    c"VK_KHR_video_maintenance1",
    // Vulkan 1.3 core; named for devices that only reach 1.2.
    c"VK_KHR_synchronization2",
];
static QUEUE_PRIORITY: [f32; 1] = [1.0];

fn decode_extension(codec: Codec) -> &'static CStr {
    match codec {
        Codec::H264 => c"VK_KHR_video_decode_h264",
        Codec::Hevc => c"VK_KHR_video_decode_h265",
        Codec::Av1 => c"VK_KHR_video_decode_av1",
    }
}

/// Registers the device hook with gpui_wgpu. Must run before the first window is opened.
/// `DECIBELL_VIDEO=software` leaves the device alone (every stream then decodes in software).
pub(crate) fn install_device_hook() -> bool {
    if std::env::var("DECIBELL_VIDEO").is_ok_and(|value| value == "software") {
        return false;
    }
    gpui_wgpu::set_vulkan_device_hook(Box::new(device_hook))
}

/// Leaves the device as wgpu would make it when the adapter can't decode video (streams then
/// decode in software).
fn device_hook(
    adapter: &wgpu::Adapter,
    args: &mut wgpu::hal::vulkan::CreateDeviceCallbackArgs<'_, '_, '_>,
) {
    *QUEUES.lock() = None;
    // Safety: reading the adapter's raw handles; nothing is created or destroyed.
    let Some(hal_adapter) = (unsafe { adapter.as_hal::<Vulkan>() }) else {
        return;
    };
    let physical_device = hal_adapter.raw_physical_device();
    let instance = hal_adapter.shared_instance().raw_instance();
    // Safety: a valid physical device of this instance.
    let supported = unsafe { instance.enumerate_device_extension_properties(physical_device) }
        .unwrap_or_default();
    let has = |name: &CStr| {
        supported
            .iter()
            .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
    };
    if !has(VIDEO_QUEUE) || !has(VIDEO_DECODE_QUEUE) {
        log::info!("video: the GPU has no Vulkan Video decode; streams decode in software");
        return;
    }
    // Safety: as above.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let render_family = args.queue_create_infos[0].queue_family_index;
    let Some(decode_family) = families
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR))
    else {
        log::info!("video: no video decode queue family; streams decode in software");
        return;
    };
    // A second family with compute + transfer (NVIDIA has one) keeps FFmpeg and our copies off
    // the renderer's queue altogether. A device without one (RADV and ANV expose a single
    // general-purpose queue) shares the renderer's queue under its lock.
    // DECIBELL_VIDEO_SHARED_QUEUE=1 forces that mode, to test it on any device.
    let spare_family = families
        .iter()
        .enumerate()
        .position(|(index, f)| {
            index as u32 != render_family
                && f.queue_flags
                    .contains(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER)
                && !f.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR)
        })
        .filter(|_| std::env::var_os("DECIBELL_VIDEO_SHARED_QUEUE").is_none());
    let shared = spare_family.is_none();
    let aux_family = spare_family.unwrap_or(render_family as usize) as u32;

    for name in [VIDEO_QUEUE, VIDEO_DECODE_QUEUE]
        .into_iter()
        .chain(OPTIONAL_EXTENSIONS.into_iter().filter(|name| has(name)))
    {
        if !args.extensions.contains(&name) {
            args.extensions.push(name);
        }
    }
    let mut new_families = vec![decode_family as u32];
    if !shared {
        new_families.push(aux_family);
    }
    for family in new_families {
        args.queue_create_infos.push(
            vk::DeviceQueueCreateInfo::default()
                .queue_family_index(family)
                .queue_priorities(&QUEUE_PRIORITY),
        );
    }
    // FFmpeg records every command with the synchronization2 entry points; wgpu doesn't enable
    // the feature. Leaked: once per device creation, 24 bytes.
    let synchronization2: &'static mut vk::PhysicalDeviceSynchronization2Features<'static> =
        Box::leak(Box::new(
            vk::PhysicalDeviceSynchronization2Features::default().synchronization2(true),
        ));
    let create_info = std::mem::take(args.create_info);
    *args.create_info = create_info.push_next(synchronization2);

    *QUEUES.lock() = Some(VideoQueues {
        aux_family,
        shared,
        decode_family: decode_family as u32,
    });
}

/// FFmpeg's (and our) lock / unlock around a queue submission: the renderer's device-wide lock.
extern "C" fn device_queue_lock(lock: c_int) {
    if lock != 0 {
        std::mem::forget(gpui_wgpu::device_queue_lock().lock());
    } else {
        // Safety: paired with the forgotten guard above, on the same thread (FFmpeg holds the
        // lock around one submit call).
        unsafe { gpui_wgpu::device_queue_lock().force_unlock() };
    }
}

/// Extension-name pointer arrays FFmpeg keeps for a device context's life. Leaked, but one per
/// distinct list: a device recreated after a loss has the same extensions.
fn leaked_names(names: &[&'static CStr]) -> (&'static [*const c_char], c_int) {
    struct Lists(Vec<&'static [*const c_char]>);
    // Safety: the pointers are to 'static strings and never written through.
    unsafe impl Send for Lists {}
    static LISTS: Mutex<Lists> = Mutex::new(Lists(Vec::new()));
    let pointers: Vec<*const c_char> = names.iter().map(|name| name.as_ptr()).collect();
    let mut lists = LISTS.lock();
    let list = match lists.0.iter().find(|list| ***list == *pointers) {
        Some(list) => *list,
        None => {
            let list: &'static [*const c_char] = Vec::leak(pointers);
            lists.0.push(list);
            list
        }
    };
    (list, list.len() as c_int)
}

/// FFmpeg's device context around the renderer's device.
struct HwDevice(*mut ff::AVBufferRef);

// Safety: an AVBufferRef to a hardware device context is made to be shared between threads.
unsafe impl Send for HwDevice {}
unsafe impl Sync for HwDevice {}

impl Drop for HwDevice {
    fn drop(&mut self) {
        // Safety: our reference.
        unsafe { ff::av_buffer_unref(&mut self.0) };
    }
}

/// The renderer's device as the decoders use it. One per renderer device, alive while a decoder
/// is.
pub(crate) struct GpuDevice {
    /// FFmpeg's device context. First field: dropped before `context`, which may hold the last
    /// reference to the device (after a device loss the renderer has let go of it) and FFmpeg
    /// frees its own objects on the device when its context goes.
    hw: OnceLock<Result<HwDevice, String>>,
    pub context: WgpuContext,
    pub raw: ash::Device,
    pub render_family: u32,
    pub aux_family: u32,
    /// The aux queue is the renderer's own.
    pub shared: bool,
    /// `Some`: the device was created with the Vulkan Video extensions and a decode queue.
    decode_family: Option<u32>,
    aux_queue: vk::Queue,
    memory_properties: vk::PhysicalDeviceMemoryProperties,
}

// Safety: Vulkan handles are plain values; the queue is used under the device-wide lock.
unsafe impl Send for GpuDevice {}
unsafe impl Sync for GpuDevice {}

pub(crate) fn vk_error(what: &str, result: vk::Result) -> DecodeError {
    if result == vk::Result::ERROR_DEVICE_LOST {
        DecodeError::device_lost(format!("{what}: the GPU device was lost"))
    } else {
        DecodeError::decoder(format!("{what}: {result}"))
    }
}

impl GpuDevice {
    /// `None`: the renderer is not on Vulkan (the OpenGL fallback).
    fn new(context: WgpuContext) -> Option<Arc<Self>> {
        // Safety: reading raw handles of a live device.
        let hal_device = unsafe { context.device.as_hal::<Vulkan>() }?;
        let raw = hal_device.raw_device().clone();
        let physical_device = hal_device.raw_physical_device();
        let render_family = hal_device.queue_family_index();
        // The hook ran for this device unless the renderer fell back to a device it did not
        // prepare: the extension list says which.
        let queues = (*QUEUES.lock()).filter(|_| {
            hal_device
                .enabled_device_extensions()
                .contains(&VIDEO_QUEUE)
        });
        let (aux_family, shared, decode_family) = match queues {
            Some(queues) => (queues.aux_family, queues.shared, Some(queues.decode_family)),
            None => (render_family, true, None),
        };
        // Safety: the families and index 0 exist: the hook created those queues, and queue 0 of
        // the render family is wgpu's.
        let aux_queue = if shared {
            hal_device.raw_queue()
        } else {
            unsafe { raw.get_device_queue(aux_family, 0) }
        };
        // Safety: a valid physical device.
        let memory_properties = unsafe {
            hal_device
                .shared_instance()
                .raw_instance()
                .get_physical_device_memory_properties(physical_device)
        };
        drop(hal_device);
        Some(Arc::new(Self {
            hw: OnceLock::new(),
            context,
            raw,
            render_family,
            aux_family,
            shared,
            decode_family,
            aux_queue,
            memory_properties,
        }))
    }

    pub fn lost(&self) -> bool {
        self.context.device_lost()
    }

    /// Whether the device really is gone, asked of the driver (after a failure that does not
    /// say). Takes the queue lock: waiting for the device to idle excludes every submission.
    pub fn probe_lost(&self) -> bool {
        if self.lost() {
            return true;
        }
        let _queues = gpui_wgpu::device_queue_lock().lock();
        // Safety: no thread submits while the lock is held.
        matches!(
            unsafe { self.raw.device_wait_idle() },
            Err(vk::Result::ERROR_DEVICE_LOST)
        )
    }

    /// FFmpeg's device context, made on first use.
    fn hw_device(&self) -> DecodeResult<*mut ff::AVBufferRef> {
        let made = self.hw.get_or_init(|| {
            let decode_family = self
                .decode_family
                .ok_or("the device was created without Vulkan Video")?;
            // Safety: raw handles of a live device, handed to FFmpeg for the context's life
            // (the context holds no ownership: `self.context` keeps the device alive).
            let hal_device = unsafe { self.context.device.as_hal::<Vulkan>() }
                .ok_or("the renderer is not on Vulkan")?;
            let shared_instance = hal_device.shared_instance();
            let (instance_extensions, instance_extension_count) =
                leaked_names(shared_instance.extensions());
            let (device_extensions, device_extension_count) =
                leaked_names(hal_device.enabled_device_extensions());
            let mut error = 0;
            // Safety: see above; the name arrays are 'static.
            let hw_device = unsafe {
                dbv_vk_device(
                    shared_instance.raw_instance().handle(),
                    shared_instance.entry().static_fn().get_instance_proc_addr,
                    hal_device.raw_physical_device(),
                    self.raw.handle(),
                    instance_extensions.as_ptr(),
                    instance_extension_count,
                    device_extensions.as_ptr(),
                    device_extension_count,
                    self.aux_family as c_int,
                    decode_family as c_int,
                    device_queue_lock,
                    &mut error,
                )
            };
            if hw_device.is_null() {
                return Err(format!(
                    "av_hwdevice_ctx_init on the renderer's device: {}",
                    av_error(error)
                ));
            }
            log::info!(
                "video: Vulkan Video on the renderer's device (aux family {}{}, decode family {decode_family})",
                self.aux_family,
                if self.shared { ", shared with the renderer" } else { "" },
            );
            Ok(HwDevice(hw_device))
        });
        match made {
            Ok(device) => Ok(device.0),
            Err(message) => Err(DecodeError::decoder(message.clone())),
        }
    }

    /// Submits on the aux queue under the device's queue lock and sleeps until the GPU has
    /// executed it. The lock is held for the submit call only.
    ///
    /// # Safety
    /// `submit` must be a valid submission for the aux queue family and `fence` unsignalled.
    pub unsafe fn submit_and_wait(
        &self,
        submit: &vk::SubmitInfo<'_>,
        fence: vk::Fence,
    ) -> DecodeResult<()> {
        // Safety: the caller's contract; the lock makes the queue ours for the call.
        unsafe {
            {
                let _queues = gpui_wgpu::device_queue_lock().lock();
                self.raw
                    .queue_submit(self.aux_queue, std::slice::from_ref(submit), fence)
            }
            .map_err(|e| vk_error("vkQueueSubmit", e))?;
            self.raw
                .wait_for_fences(&[fence], true, 5_000_000_000)
                .map_err(|e| vk_error("waiting for the GPU", e))?;
            self.raw
                .reset_fences(&[fence])
                .map_err(|e| vk_error("vkResetFences", e))
        }
    }

    fn memory_type(&self, type_bits: u32, flags: vk::MemoryPropertyFlags) -> DecodeResult<u32> {
        (0..self.memory_properties.memory_type_count)
            .find(|&index| {
                type_bits & (1 << index) != 0
                    && self.memory_properties.memory_types[index as usize]
                        .property_flags
                        .contains(flags)
            })
            .ok_or_else(|| DecodeError::decoder("no suitable GPU memory type"))
    }
}

pub(crate) fn surface_color(color: ColorInfo) -> gpui_wgpu::SurfaceColor {
    gpui_wgpu::SurfaceColor {
        matrix: match color.matrix {
            Matrix::Bt601 => gpui_wgpu::SurfaceMatrix::Bt601,
            Matrix::Bt709 => gpui_wgpu::SurfaceMatrix::Bt709,
            Matrix::Bt2020 => gpui_wgpu::SurfaceMatrix::Bt2020,
        },
        full_range: color.full_range,
        chroma_offset: if color.chroma_centered {
            [0.0, 0.0]
        } else {
            [0.25, 0.0]
        },
    }
}

/// Plane textures alive in the process.
pub(crate) static LIVE_PLANES: AtomicI64 = AtomicI64::new(0);

pub(crate) struct LinuxBackend {
    /// The device the current decoders are on. Weak: with the last decoder it goes, and with
    /// it FFmpeg's device context.
    device: Mutex<Weak<GpuDevice>>,
    /// Hardware support per codec, for the device it was probed on.
    support: Mutex<Option<(wgpu::Device, [CodecSupport; 3])>>,
}

impl LinuxBackend {
    pub fn new() -> Self {
        Self {
            device: Mutex::new(Weak::new()),
            support: Mutex::new(None),
        }
    }

    /// The renderer's current device, wrapped. `Ok(None)`: the renderer is not on Vulkan.
    fn device(&self) -> DecodeResult<Option<Arc<GpuDevice>>> {
        let context = gpui_wgpu::shared_context()
            .filter(|context| !context.device_lost())
            .ok_or_else(|| DecodeError::device_lost("the renderer has no GPU device"))?;
        let mut cached = self.device.lock();
        if let Some(device) = cached.upgrade()
            && device.context.device == context.device
        {
            return Ok(Some(device));
        }
        let device = GpuDevice::new(context);
        *cached = device.as_ref().map(Arc::downgrade).unwrap_or_default();
        Ok(device)
    }

    fn probe(&self, context: &WgpuContext) -> [CodecSupport; 3] {
        let mut cached = self.support.lock();
        if let Some((device, support)) = &*cached
            && *device == *context.device
        {
            return *support;
        }
        let support = Codec::ALL.map(|codec| {
            let hardware = hardware_support(context, codec);
            CodecSupport {
                codec,
                path: match hardware {
                    Some(_) => Some(DecodePath::Hardware),
                    None if has_software_decoder(codec) => Some(DecodePath::Software),
                    None => None,
                },
                max_size: hardware,
            }
        });
        log::info!("video: decode support {support:?}");
        *cached = Some(((*context.device).clone(), support));
        support
    }
}

/// The driver's answer for 8-bit 4:2:0 decode of `codec` on the renderer's device: the largest
/// picture, or `None` when the device cannot (or was not created to) decode it.
fn hardware_support(context: &WgpuContext, codec: Codec) -> Option<(u32, u32)> {
    // Safety: raw handles of a live device and its instance; the query structs are chained as
    // the Vulkan Video extensions require.
    unsafe {
        let hal_device = context.device.as_hal::<Vulkan>()?;
        if !hal_device
            .enabled_device_extensions()
            .contains(&decode_extension(codec))
        {
            return None;
        }
        let shared_instance = hal_device.shared_instance();
        let video = ash::khr::video_queue::Instance::new(
            shared_instance.entry(),
            shared_instance.raw_instance(),
        );
        let mut h264_profile = vk::VideoDecodeH264ProfileInfoKHR::default()
            .std_profile_idc(vk::native::StdVideoH264ProfileIdc_STD_VIDEO_H264_PROFILE_IDC_HIGH)
            .picture_layout(vk::VideoDecodeH264PictureLayoutFlagsKHR::PROGRESSIVE);
        let mut h265_profile = vk::VideoDecodeH265ProfileInfoKHR::default()
            .std_profile_idc(vk::native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN);
        let mut av1_profile = vk::VideoDecodeAV1ProfileInfoKHR::default()
            .std_profile(vk::native::StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_MAIN)
            .film_grain_support(false);
        let profile = vk::VideoProfileInfoKHR::default()
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8)
            .chroma_bit_depth(vk::VideoComponentBitDepthFlagsKHR::TYPE_8);
        let mut h264_caps = vk::VideoDecodeH264CapabilitiesKHR::default();
        let mut h265_caps = vk::VideoDecodeH265CapabilitiesKHR::default();
        let mut av1_caps = vk::VideoDecodeAV1CapabilitiesKHR::default();
        let mut decode_caps = vk::VideoDecodeCapabilitiesKHR::default();
        let caps = vk::VideoCapabilitiesKHR::default().push_next(&mut decode_caps);
        let (profile, mut caps) = match codec {
            Codec::H264 => (
                profile
                    .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_H264)
                    .push_next(&mut h264_profile),
                caps.push_next(&mut h264_caps),
            ),
            Codec::Hevc => (
                profile
                    .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_H265)
                    .push_next(&mut h265_profile),
                caps.push_next(&mut h265_caps),
            ),
            Codec::Av1 => (
                profile
                    .video_codec_operation(vk::VideoCodecOperationFlagsKHR::DECODE_AV1)
                    .push_next(&mut av1_profile),
                caps.push_next(&mut av1_caps),
            ),
        };
        (video.fp().get_physical_device_video_capabilities_khr)(
            hal_device.raw_physical_device(),
            &profile,
            &mut caps,
        )
        .result()
        .ok()?;
        Some((caps.max_coded_extent.width, caps.max_coded_extent.height))
    }
}

impl Backend for LinuxBackend {
    fn open(
        &self,
        codec: Codec,
        description: Option<&[u8]>,
        allow_hardware: bool,
        target: &StreamTarget,
    ) -> DecodeResult<Box<dyn PictureDecoder>> {
        let device = self.device()?;
        let Some(device) = device else {
            // Not Vulkan: software decode, uploaded through wgpu.
            let context = gpui_wgpu::shared_context()
                .ok_or_else(|| DecodeError::device_lost("the renderer has no GPU device"))?;
            let uploader = upload::WgpuUploader::new(context, target.surface.clone());
            return Ok(Box::new(SoftwareDecoder::open(
                codec,
                description,
                Box::new(uploader),
            )?));
        };
        let hardware = allow_hardware
            && self.probe(&device.context)[codec.index()].path == Some(DecodePath::Hardware);
        if hardware {
            let hw_device = device.hw_device()?;
            let decoder =
                hw::VulkanDecoder::open(device.clone(), hw_device, codec, description, target)
                    .map_err(|error| classify(&device, error))?;
            return Ok(Box::new(decoder));
        }
        let uploader: Box<dyn crate::decode::Uploader> =
            if std::env::var("DECIBELL_VIDEO_UPLOAD").is_ok_and(|value| value == "wgpu") {
                Box::new(upload::WgpuUploader::new(
                    device.context.clone(),
                    target.surface.clone(),
                ))
            } else {
                Box::new(
                    upload::VulkanUploader::new(device.clone(), target.surface.clone())
                        .map_err(|error| classify(&device, error))?,
                )
            };
        Ok(Box::new(SoftwareDecoder::open(
            codec,
            description,
            uploader,
        )?))
    }

    fn ready(&self) -> bool {
        gpui_wgpu::shared_context().is_some_and(|context| !context.device_lost())
    }

    fn simulate_device_loss(&self) {
        if let Some(context) = gpui_wgpu::shared_context() {
            context.simulate_device_loss();
        }
    }

    fn support(&self, codec: Codec) -> CodecSupport {
        match gpui_wgpu::shared_context().filter(|context| !context.device_lost()) {
            Some(context) => self.probe(&context)[codec.index()],
            None => CodecSupport {
                codec,
                path: has_software_decoder(codec).then_some(DecodePath::Software),
                max_size: None,
            },
        }
    }

    fn live_textures(&self) -> i64 {
        LIVE_PLANES.load(Ordering::Relaxed)
    }

    fn collect(&self) {
        if let Some(context) = gpui_wgpu::shared_context() {
            // Runs the "submission finished" callbacks of frames the GPU has completed, which
            // is what releases a closed stream's last pictures. Does not wait.
            let _ = context.device.poll(wgpu::PollType::Poll);
        }
    }
}

/// A failure that does not say whether the device is gone: ask the driver.
pub(crate) fn classify(device: &GpuDevice, error: DecodeError) -> DecodeError {
    if error.failure == crate::decode::Failure::Decoder && device.probe_lost() {
        return DecodeError::device_lost(error.message);
    }
    error
}

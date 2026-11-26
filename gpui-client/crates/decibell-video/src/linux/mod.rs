//! Linux: the renderer's own Vulkan device, shared with FFmpeg.
//!
//! - [`install_device_hook`] runs when gpui_wgpu creates its `VkDevice`: it adds the Vulkan Video
//!   extensions and one queue in each of two families the renderer never submits on (decode,
//!   and "aux": compute + transfer). It costs a few extension-name comparisons at start-up and
//!   two idle queues; nothing else exists until a stream is displayed.
//! - [`GpuDevice`] is that device as the decoders use it: the aux queue for picture copies and
//!   uploads, and (made on the first hardware decode) the FFmpeg `AVHWDeviceContext` around it.
//! - [`slots`]: the plane textures a stream's pictures are handed over in.
//! - [`hw`]: Vulkan Video decode + a GPU copy into a slot. [`nvdec`]: NVIDIA's decoder through
//!   CUDA, copied device to device into slots whose memory CUDA imported. [`vaapi`]: VA-API
//!   surfaces imported as DMA-BUFs and copied into slots. [`upload`]: software pictures into
//!   the same slots through a persistent staging buffer.
//!
//! Which of them a stream gets: `select.rs` walks [`LinuxBackend::order`]. Why every path
//! ends in a copy into a slot and not in the decoder's own image: docs/video-design.md §3.

mod hw;
mod nvdec;
mod slots;
mod upload;
mod vaapi;

use std::ffi::{CStr, c_char, c_int};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use ash::vk;
use ffmpeg_sys_next as ff;
use gpui_wgpu::wgpu::hal::api::Vulkan;
use gpui_wgpu::{WgpuContext, wgpu};
use parking_lot::Mutex;

use crate::codec::{Codec, CodecSupport, DecodeBackend, DecodePath};
use crate::decode::{
    Backend, ColorInfo, DecodeError, DecodeResult, Matrix, PictureDecoder, SoftwareDecoder, Transfer,
    av_error, software_support,
};
use crate::hub::StreamTarget;
use crate::select::Order;

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
const VIDEO_DECODE_VP9: &CStr = c"VK_KHR_video_decode_vp9";
const OPTIONAL_EXTENSIONS: [&CStr; 6] = [
    c"VK_KHR_video_decode_h264",
    c"VK_KHR_video_decode_h265",
    c"VK_KHR_video_decode_av1",
    VIDEO_DECODE_VP9,
    c"VK_KHR_video_maintenance1",
    // Vulkan 1.3 core; named for devices that only reach 1.2.
    c"VK_KHR_synchronization2",
];
static QUEUE_PRIORITY: [f32; 1] = [1.0];

pub(crate) const EXTERNAL_MEMORY_FD: &CStr = c"VK_KHR_external_memory_fd";
pub(crate) const EXTERNAL_SEMAPHORE_FD: &CStr = c"VK_KHR_external_semaphore_fd";
pub(crate) const EXTERNAL_MEMORY_DMA_BUF: &CStr = c"VK_EXT_external_memory_dma_buf";
pub(crate) const IMAGE_DRM_FORMAT_MODIFIER: &CStr = c"VK_EXT_image_drm_format_modifier";
pub(crate) const QUEUE_FAMILY_FOREIGN: &CStr = c"VK_EXT_queue_family_foreign";
/// What the hardware fallbacks need of the device, each enabled when the GPU has it: sharing
/// slot memory and a semaphore with CUDA (NVDEC), importing a VA surface's DMA-BUFs with their
/// DRM format modifier (VA-API). No queue, no feature struct, nothing at run time until one of
/// those decoders is opened.
const INTEROP_EXTENSIONS: [&CStr; 6] = [
    EXTERNAL_MEMORY_FD,
    EXTERNAL_SEMAPHORE_FD,
    EXTERNAL_MEMORY_DMA_BUF,
    IMAGE_DRM_FORMAT_MODIFIER,
    QUEUE_FAMILY_FOREIGN,
    // Required by VK_EXT_image_drm_format_modifier; Vulkan 1.2 core, named for 1.1 devices.
    c"VK_KHR_image_format_list",
];
const NVIDIA: u32 = 0x10DE;

fn decode_extension(codec: Codec) -> &'static CStr {
    match codec {
        Codec::H264 => c"VK_KHR_video_decode_h264",
        Codec::Hevc => c"VK_KHR_video_decode_h265",
        Codec::Av1 => c"VK_KHR_video_decode_av1",
        Codec::Vp9 => VIDEO_DECODE_VP9,
        // Files only, software only (`Codec::has_hardware_path`): an extension no driver has.
        Codec::Vp8 => c"",
    }
}

/// Registers the device hook with gpui_wgpu. Must run before the first window is opened.
/// `DECIBELL_VIDEO=software` leaves the device alone (every stream then decodes in software;
/// `DECIBELL_VIDEO_DECODER=software` decodes in software on the device as the hook makes it).
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
    for name in INTEROP_EXTENSIONS {
        if has(name) && !args.extensions.contains(&name) {
            args.extensions.push(name);
        }
    }
    if !has(VIDEO_QUEUE) || !has(VIDEO_DECODE_QUEUE) {
        log::info!("video: the GPU has no Vulkan Video decode");
        return;
    }
    // Safety: as above.
    let families = unsafe { instance.get_physical_device_queue_family_properties(physical_device) };
    let render_family = args.queue_create_infos[0].queue_family_index;
    let Some(decode_family) = families
        .iter()
        .position(|f| f.queue_flags.contains(vk::QueueFlags::VIDEO_DECODE_KHR))
    else {
        log::info!("video: the GPU has no video decode queue family");
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
    let mut create_info = create_info.push_next(synchronization2);
    if has(VIDEO_DECODE_VP9) {
        // VP9 decode is the one codec extension with a feature bit of its own. Leaked like the
        // struct above.
        let vp9: &'static mut DecodeVp9Features = Box::leak(Box::new(DecodeVp9Features {
            s_type: vk::StructureType::from_raw(1_000_514_000),
            p_next: std::ptr::null_mut(),
            video_decode_vp9: vk::TRUE,
        }));
        create_info = create_info.push_next(vp9);
    }
    *args.create_info = create_info;

    *QUEUES.lock() = Some(VideoQueues {
        aux_family,
        shared,
        decode_family: decode_family as u32,
    });
}

// VK_KHR_video_decode_vp9 (Vulkan 1.4.317) is newer than the pinned `ash`: its three structs
// and two constants, from vulkan_core.h.

/// `VkPhysicalDeviceVideoDecodeVP9FeaturesKHR`.
#[repr(C)]
struct DecodeVp9Features {
    s_type: vk::StructureType,
    p_next: *mut std::ffi::c_void,
    video_decode_vp9: vk::Bool32,
}
// Safety: the struct's layout and structure type are the extension's; it extends
// VkDeviceCreateInfo by the specification.
unsafe impl vk::ExtendsDeviceCreateInfo for DecodeVp9Features {}
unsafe impl vk::TaggedStructure for DecodeVp9Features {
    const STRUCTURE_TYPE: vk::StructureType = vk::StructureType::from_raw(1_000_514_000);
}

/// `VkVideoDecodeVP9ProfileInfoKHR`.
#[repr(C)]
struct DecodeVp9Profile {
    s_type: vk::StructureType,
    p_next: *const std::ffi::c_void,
    /// `StdVideoVP9Profile`: 0 is profile 0 (8-bit 4:2:0), 2 is profile 2 (10 / 12-bit 4:2:0).
    std_profile: u32,
}
unsafe impl vk::ExtendsVideoProfileInfoKHR for DecodeVp9Profile {}
unsafe impl vk::TaggedStructure for DecodeVp9Profile {
    const STRUCTURE_TYPE: vk::StructureType = vk::StructureType::from_raw(1_000_514_003);
}

/// `VkVideoDecodeVP9CapabilitiesKHR`.
#[repr(C)]
struct DecodeVp9Capabilities {
    s_type: vk::StructureType,
    p_next: *mut std::ffi::c_void,
    max_level: u32,
}
unsafe impl vk::ExtendsVideoCapabilitiesKHR for DecodeVp9Capabilities {}
unsafe impl vk::TaggedStructure for DecodeVp9Capabilities {
    const STRUCTURE_TYPE: vk::StructureType = vk::StructureType::from_raw(1_000_514_001);
}

/// `VK_VIDEO_CODEC_OPERATION_DECODE_VP9_BIT_KHR`.
const DECODE_VP9: vk::VideoCodecOperationFlagsKHR = vk::VideoCodecOperationFlagsKHR::from_raw(0x8);

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
    pub instance: ash::Instance,
    pub physical_device: vk::PhysicalDevice,
    /// The device extensions that are enabled.
    extensions: Vec<&'static CStr>,
    /// PCI vendor id of the GPU.
    pub vendor: u32,
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
        let instance = hal_device.shared_instance().raw_instance().clone();
        // Safety: a valid physical device.
        let memory_properties =
            unsafe { instance.get_physical_device_memory_properties(physical_device) };
        let extensions = hal_device.enabled_device_extensions().to_vec();
        drop(hal_device);
        let vendor = context.adapter.get_info().vendor;
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
            instance,
            physical_device,
            extensions,
            vendor,
        }))
    }

    /// Whether the renderer's device has 16-bit normalised textures (GPUI patch `0031` asks
    /// for them): the planes of a 10 / 12-bit picture.
    pub fn deep_planes(&self) -> bool {
        self.context
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_FORMAT_16BIT_NORM)
    }

    pub fn has_extension(&self, name: &CStr) -> bool {
        self.extensions.contains(&name)
    }

    /// `vkGetMemoryFdKHR` and friends, when the device has them.
    pub fn external_memory_fd(&self) -> Option<ash::khr::external_memory_fd::Device> {
        self.has_extension(EXTERNAL_MEMORY_FD)
            .then(|| ash::khr::external_memory_fd::Device::new(&self.instance, &self.raw))
    }

    pub fn external_semaphore_fd(&self) -> Option<ash::khr::external_semaphore_fd::Device> {
        self.has_extension(EXTERNAL_SEMAPHORE_FD)
            .then(|| ash::khr::external_semaphore_fd::Device::new(&self.instance, &self.raw))
    }

    /// The queue family that stands for "another API owns this image's contents".
    /// `VK_QUEUE_FAMILY_EXTERNAL` is the core 1.1 stand-in on a device without
    /// `VK_EXT_queue_family_foreign` (meant for other Vulkan instances of the same driver).
    pub fn foreign_queue_family(&self) -> u32 {
        if self.has_extension(QUEUE_FAMILY_FOREIGN) {
            vk::QUEUE_FAMILY_FOREIGN_EXT
        } else {
            vk::QUEUE_FAMILY_EXTERNAL
        }
    }

    fn physical_device_has(&self, name: &CStr) -> bool {
        // Safety: a valid physical device of this instance.
        unsafe {
            self.instance
                .enumerate_device_extension_properties(self.physical_device)
        }
        .unwrap_or_default()
        .iter()
        .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
    }

    /// The GPU's UUID (`VkPhysicalDeviceIDProperties`, Vulkan 1.1), which is its CUDA UUID
    /// on NVIDIA.
    pub fn uuid(&self) -> Option<[u8; 16]> {
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        // Safety: a valid physical device; a core 1.1 query.
        unsafe {
            self.instance
                .get_physical_device_properties2(self.physical_device, &mut properties)
        };
        (id.device_uuid != [0; 16]).then_some(id.device_uuid)
    }

    /// The DRM render node of this GPU (`/dev/dri/renderD<n>`): where VA-API is opened, so
    /// that the decoder is the GPU the window is drawn with.
    pub fn render_node(&self) -> Result<std::path::PathBuf, String> {
        use std::os::unix::fs::MetadataExt as _;
        let nodes = || {
            std::fs::read_dir("/dev/dri")
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("renderD"))
                })
        };
        // The driver's own answer (VK_EXT_physical_device_drm; NVIDIA 620 has it, and so do
        // current Mesa drivers as far as their extension lists say). A physical-device-level
        // query: the extension need not be enabled on the device.
        if self.physical_device_has(c"VK_EXT_physical_device_drm") {
            let mut drm = vk::PhysicalDeviceDrmPropertiesEXT::default();
            let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut drm);
            // Safety: a valid physical device that supports the extension.
            unsafe {
                self.instance
                    .get_physical_device_properties2(self.physical_device, &mut properties)
            };
            if drm.has_render == vk::TRUE {
                let wanted = libc::makedev(drm.render_major as _, drm.render_minor as _);
                return nodes()
                    .find(|path| {
                        std::fs::metadata(path).is_ok_and(|meta| meta.rdev() == wanted as u64)
                    })
                    .ok_or_else(|| {
                        format!(
                            "the GPU's DRM render node ({}:{}) is not in /dev/dri",
                            drm.render_major, drm.render_minor
                        )
                    });
            }
            return Err("the GPU has no DRM render node".into());
        }
        // Else by PCI address (VK_EXT_pci_bus_info): the node sysfs lists under the device.
        if self.physical_device_has(c"VK_EXT_pci_bus_info") {
            let mut pci = vk::PhysicalDevicePCIBusInfoPropertiesEXT::default();
            let mut properties = vk::PhysicalDeviceProperties2::default().push_next(&mut pci);
            // Safety: as above.
            unsafe {
                self.instance
                    .get_physical_device_properties2(self.physical_device, &mut properties)
            };
            let sysfs = format!(
                "/sys/bus/pci/devices/{:04x}:{:02x}:{:02x}.{}/drm",
                pci.pci_domain, pci.pci_bus, pci.pci_device, pci.pci_function
            );
            return std::fs::read_dir(&sysfs)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.file_name())
                .find(|name| name.to_string_lossy().starts_with("renderD"))
                .map(|name| std::path::Path::new("/dev/dri").join(name))
                .ok_or_else(|| format!("no DRM render node under {sysfs}"));
        }
        Err("the Vulkan driver does not say which DRM render node the GPU is".into())
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
        depth: color.depth,
        transfer: match color.transfer {
            Transfer::Sdr => gpui_wgpu::SurfaceTransfer::Sdr,
            Transfer::Pq => gpui_wgpu::SurfaceTransfer::Pq,
            Transfer::Hlg => gpui_wgpu::SurfaceTransfer::Hlg,
            Transfer::Bt2020Sdr => gpui_wgpu::SurfaceTransfer::Bt2020Sdr,
        },
    }
}

/// Plane textures alive in the process.
pub(crate) static LIVE_PLANES: AtomicI64 = AtomicI64::new(0);

/// Per codec: the largest picture Vulkan Video decodes, `None` when it does not decode it.
type VulkanVideoSupport = [Option<(u32, u32)>; Codec::EVERY.len()];

pub(crate) struct LinuxBackend {
    /// The device the current decoders are on. Weak: with the last decoder it goes, and with
    /// it FFmpeg's device context.
    device: Mutex<Weak<GpuDevice>>,
    /// What Vulkan Video does with each codec (the driver's largest picture), for the device
    /// it was asked of.
    support: Mutex<Option<(wgpu::Device, VulkanVideoSupport)>>,
    /// The CUDA context of the NVDEC decoders, and the VA display of the VA-API ones: each
    /// alive while one of its decoders is.
    cuda: Mutex<Weak<nvdec::CudaDevice>>,
    vaapi: Mutex<Weak<vaapi::VaDevice>>,
}

impl LinuxBackend {
    pub fn new() -> Self {
        Self {
            device: Mutex::new(Weak::new()),
            support: Mutex::new(None),
            cuda: Mutex::new(Weak::new()),
            vaapi: Mutex::new(Weak::new()),
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

    /// The driver's Vulkan Video answer per codec.
    fn vulkan_video(&self, context: &WgpuContext) -> VulkanVideoSupport {
        let mut cached = self.support.lock();
        if let Some((device, support)) = &*cached
            && *device == *context.device
        {
            return *support;
        }
        let support = Codec::EVERY.map(|codec| hardware_support(context, codec));
        log::info!("video: Vulkan Video decode (largest picture per codec): {support:?}");
        *cached = Some(((*context.device).clone(), support));
        support
    }

    fn cuda(&self, device: &GpuDevice) -> DecodeResult<Arc<nvdec::CudaDevice>> {
        let mut cached = self.cuda.lock();
        if let Some(cuda) = cached.upgrade()
            && Some(cuda.uuid) == device.uuid()
        {
            return Ok(cuda);
        }
        let cuda = nvdec::CudaDevice::open(device)?;
        *cached = Arc::downgrade(&cuda);
        Ok(cuda)
    }

    fn va(&self, device: &GpuDevice) -> DecodeResult<Arc<vaapi::VaDevice>> {
        let mut cached = self.vaapi.lock();
        if let Some(va) = cached.upgrade()
            && device.render_node().is_ok_and(|node| node == va.node)
        {
            return Ok(va);
        }
        let va = vaapi::VaDevice::open(device)?;
        *cached = Arc::downgrade(&va);
        Ok(va)
    }
}

/// The driver's answer for 8-bit 4:2:0 decode of `codec` on the renderer's device: the largest
/// picture, or `None` when the device cannot (or was not created to) decode it.
fn hardware_support(context: &WgpuContext, codec: Codec) -> Option<(u32, u32)> {
    hardware_support_at(context, codec, false)
}

/// The same for the codec's 4:2:0 profile at `deep` (10 bits: HEVC Main 10, AV1 Main, VP9
/// profile 2) or 8 bits. H.264 has no 10-bit profile a GPU decodes.
fn hardware_support_at(context: &WgpuContext, codec: Codec, deep: bool) -> Option<(u32, u32)> {
    if deep && codec == Codec::H264 {
        return None;
    }
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
            .std_profile_idc(if deep {
                vk::native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN_10
            } else {
                vk::native::StdVideoH265ProfileIdc_STD_VIDEO_H265_PROFILE_IDC_MAIN
            });
        let mut av1_profile = vk::VideoDecodeAV1ProfileInfoKHR::default()
            .std_profile(vk::native::StdVideoAV1Profile_STD_VIDEO_AV1_PROFILE_MAIN)
            .film_grain_support(false);
        let profile = vk::VideoProfileInfoKHR::default()
            .chroma_subsampling(vk::VideoChromaSubsamplingFlagsKHR::TYPE_420)
            .luma_bit_depth(if deep {
                vk::VideoComponentBitDepthFlagsKHR::TYPE_10
            } else {
                vk::VideoComponentBitDepthFlagsKHR::TYPE_8
            })
            .chroma_bit_depth(if deep {
                vk::VideoComponentBitDepthFlagsKHR::TYPE_10
            } else {
                vk::VideoComponentBitDepthFlagsKHR::TYPE_8
            });
        let mut h264_caps = vk::VideoDecodeH264CapabilitiesKHR::default();
        let mut h265_caps = vk::VideoDecodeH265CapabilitiesKHR::default();
        let mut av1_caps = vk::VideoDecodeAV1CapabilitiesKHR::default();
        let mut vp9_profile = DecodeVp9Profile {
            s_type: vk::StructureType::from_raw(1_000_514_003),
            p_next: std::ptr::null(),
            std_profile: if deep { 2 } else { 0 },
        };
        let mut vp9_caps = DecodeVp9Capabilities {
            s_type: vk::StructureType::from_raw(1_000_514_001),
            p_next: std::ptr::null_mut(),
            max_level: 0,
        };
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
            Codec::Vp9 => (
                profile
                    .video_codec_operation(DECODE_VP9)
                    .push_next(&mut vp9_profile),
                caps.push_next(&mut vp9_caps),
            ),
            Codec::Vp8 => return None,
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

/// The decoders of a GPU of PCI vendor `vendor`, best first. `vulkan_video`: its device was
/// created with a video decode queue.
fn platform_order(vulkan_video: bool, vendor: u32) -> Order {
    let vendor_decoder = if vendor == NVIDIA {
        DecodeBackend::Nvdec
    } else {
        DecodeBackend::Vaapi
    };
    if vulkan_video {
        Order::new(&[
            DecodeBackend::VulkanVideo,
            vendor_decoder,
            DecodeBackend::Software,
        ])
    } else {
        Order::new(&[vendor_decoder, DecodeBackend::Software])
    }
}

impl Backend for LinuxBackend {
    /// Vulkan Video when the device was created for it; then the GPU vendor's own decoder
    /// (NVDEC on NVIDIA, VA-API on everything else: AMD, Intel); then software. A renderer
    /// that is not on Vulkan (the OpenGL fallback) has no device to share: software.
    fn order(&self) -> Order {
        let Ok(Some(device)) = self.device() else {
            return Order::SOFTWARE;
        };
        platform_order(device.decode_family.is_some(), device.vendor)
    }

    fn open(
        &self,
        backend: DecodeBackend,
        codec: Codec,
        description: Option<&[u8]>,
        target: &StreamTarget,
    ) -> DecodeResult<Box<dyn PictureDecoder>> {
        let device = self.device()?;
        let Some(device) = device else {
            if backend != DecodeBackend::Software {
                return Err(DecodeError::decoder(
                    "the renderer is not on Vulkan: there is no GPU device to decode on",
                ));
            }
            // Software decode, uploaded through wgpu.
            let context = gpui_wgpu::shared_context()
                .ok_or_else(|| DecodeError::device_lost("the renderer has no GPU device"))?;
            let uploader = upload::WgpuUploader::new(context, target.surface.clone());
            return Ok(Box::new(SoftwareDecoder::open(
                codec,
                description,
                Box::new(uploader),
                target.reorder(),
            )?));
        };
        let opened = (|| -> DecodeResult<Box<dyn PictureDecoder>> {
            match backend {
            DecodeBackend::VulkanVideo => {
                if device.decode_family.is_none() {
                    return Err(DecodeError::decoder(
                        "the GPU device was created without Vulkan Video (the driver has no video decode queue)",
                    ));
                }
                if self.vulkan_video(&device.context)[codec.index()].is_none() {
                    return Err(DecodeError::decoder(format!(
                        "the driver does not decode {} with Vulkan Video",
                        codec.label()
                    )));
                }
                if target.deep() {
                    // A 10-bit file: the driver's answer for the codec's 10-bit profile, and
                    // 16-bit planes to hand the pictures over in.
                    if !device.deep_planes() {
                        return Err(DecodeError::decoder(
                            "the GPU has no 16-bit normalised textures for 10-bit pictures",
                        ));
                    }
                    if hardware_support_at(&device.context, codec, true).is_none() {
                        return Err(DecodeError::decoder(format!(
                            "the driver does not decode 10-bit {} with Vulkan Video",
                            codec.label()
                        )));
                    }
                }
                let hw_device = device.hw_device()?;
                Ok(Box::new(hw::VulkanDecoder::open(
                    device.clone(),
                    hw_device,
                    codec,
                    description,
                    target,
                )?))
            }
            DecodeBackend::Nvdec | DecodeBackend::Vaapi if target.deep() => Err(DecodeError::decoder(
                "10-bit pictures are not handed over by this decoder yet (Vulkan Video and software are)",
            )),
            DecodeBackend::Nvdec => {
                let cuda = self.cuda(&device)?;
                Ok(Box::new(nvdec::NvdecDecoder::open(
                    device.clone(),
                    cuda,
                    codec,
                    description,
                    target,
                )?))
            }
            DecodeBackend::Vaapi => {
                let va = self.va(&device)?;
                Ok(Box::new(vaapi::VaapiDecoder::open(
                    device.clone(),
                    va,
                    codec,
                    description,
                    target,
                )?))
            }
            DecodeBackend::D3d11va => Err(DecodeError::decoder(
                "D3D11VA decode is not built for this platform",
            )),
            DecodeBackend::Software => {
                let uploader: Box<dyn crate::decode::Uploader> =
                    if std::env::var("DECIBELL_VIDEO_UPLOAD").is_ok_and(|value| value == "wgpu") {
                        Box::new(upload::WgpuUploader::new(
                            device.context.clone(),
                            target.surface.clone(),
                        ))
                    } else {
                        Box::new(upload::VulkanUploader::new(
                            device.clone(),
                            target.surface.clone(),
                        )?)
                    };
                Ok(Box::new(SoftwareDecoder::open(
                    codec,
                    description,
                    uploader,
                    target.reorder(),
                )?))
            }
            }
        })();
        opened.map_err(|error| classify(&device, error))
    }

    fn ready(&self) -> bool {
        gpui_wgpu::shared_context().is_some_and(|context| !context.device_lost())
    }

    fn simulate_device_loss(&self) {
        if let Some(context) = gpui_wgpu::shared_context() {
            context.simulate_device_loss();
        }
    }

    fn support(&self, backend: DecodeBackend, codec: Codec) -> CodecSupport {
        if backend == DecodeBackend::Software {
            return software_support(codec);
        }
        let Ok(Some(device)) = self.device() else {
            return CodecSupport::none(codec);
        };
        let hardware = |max_size| CodecSupport {
            codec,
            path: Some(DecodePath::Hardware),
            backend: Some(backend),
            probed: false,
            max_size,
        };
        // Vulkan Video: the driver's capability query. The other two only say whether the
        // device could take their pictures; whether the vendor's decoder takes the codec is
        // found by opening it (the probe, or the first stream).
        match backend {
            DecodeBackend::VulkanVideo => match self.vulkan_video(&device.context)[codec.index()]
            {
                Some(size) => hardware(Some(size)),
                None => CodecSupport::none(codec),
            },
            DecodeBackend::Nvdec => hardware(None)
                .filter(device.vendor == NVIDIA && device.has_extension(EXTERNAL_MEMORY_FD)),
            DecodeBackend::Vaapi => hardware(None).filter(
                device.has_extension(EXTERNAL_MEMORY_DMA_BUF)
                    && device.has_extension(IMAGE_DRM_FORMAT_MODIFIER),
            ),
            DecodeBackend::D3d11va => CodecSupport::none(codec),
            DecodeBackend::Software => software_support(codec),
        }
    }

    fn device_key(&self) -> Option<String> {
        let context = gpui_wgpu::shared_context().filter(|context| !context.device_lost())?;
        let info = context.adapter.get_info();
        Some(format!(
            "{:04x}:{:04x} {:?} {} {}",
            info.vendor, info.device, info.backend, info.driver, info.driver_info
        ))
    }

    fn live_textures(&self) -> i64 {
        LIVE_PLANES.load(Ordering::Relaxed)
    }

    fn deep_planes(&self) -> bool {
        matches!(self.device(), Ok(Some(device)) if device.deep_planes())
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

#[cfg(test)]
mod tests {
    use super::*;
    use DecodeBackend::*;

    #[test]
    fn the_order_follows_the_gpu_vendor() {
        const AMD: u32 = 0x1002;
        const INTEL: u32 = 0x8086;
        assert_eq!(
            platform_order(true, NVIDIA).as_slice(),
            [VulkanVideo, Nvdec, Software]
        );
        for vendor in [AMD, INTEL] {
            assert_eq!(
                platform_order(true, vendor).as_slice(),
                [VulkanVideo, Vaapi, Software]
            );
            // An older Mesa, or a GPU whose Vulkan driver has no video decode: VA-API first.
            assert_eq!(platform_order(false, vendor).as_slice(), [Vaapi, Software]);
        }
        assert_eq!(platform_order(false, NVIDIA).as_slice(), [Nvdec, Software]);
    }
}

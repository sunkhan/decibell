//! Windows: the renderer's own Direct3D 11 device, shared with FFmpeg.
//!
//! - gpui_windows creates its device with multithread protection and hands it out
//!   (`gpui_windows::shared_devices`, GPUI patch `0020`): there is no device hook to install,
//!   Direct3D 11 needs nothing added to a device for video.
//! - [`GpuDevice`] is that device as the decoders use it, and (made on the first hardware
//!   decode) FFmpeg's `AVHWDeviceContext` around it: `AV_HWDEVICE_TYPE_D3D11VA`, whose lock
//!   callbacks enter the immediate context's own critical section.
//! - [`slots`]: the plane textures a stream's pictures are handed over in.
//! - [`d3d11va`]: hardware decode + one GPU copy of the decoded array slice into a slot.
//!   [`upload`]: software pictures into the same kind of slot.
//!
//! # The immediate context, from two threads
//!
//! A Direct3D 11 device has one immediate context and it is not free-threaded. Three users
//! share it here: the renderer (main thread), FFmpeg's decoders and our copies (decoder
//! threads). What keeps them apart:
//!
//! 1. The device is multithread-protected from its creation (`ID3D11Multithread::
//!    SetMultithreadProtected`, in gpui_windows): every single context call takes the context's
//!    critical section, whichever thread makes it.
//! 2. Sequences take that same critical section for their whole length
//!    (`ID3D11Multithread::Enter` / `Leave`, recursive): the renderer around each frame it
//!    draws and presents, FFmpeg around each decode submission (its `lock` / `unlock`
//!    callbacks are ours and enter it), and every sequence of ours ([`GpuDevice::lock`]). So a
//!    frame's draws are never interleaved with a decode submission or a copy, and nothing of
//!    ours runs between FFmpeg's `DecoderBeginFrame` and `DecoderEndFrame`.
//!
//! # Why a published picture is complete
//!
//! Direct3D 11 executes an immediate context's commands in the order they were issued. A
//! decode is issued by `avcodec_send_packet`, the copy of its output slice into a slot after
//! it, and the picture is published (`D3d11Surface::set_frame`) only after the copy call has
//! returned: any draw that can sample the slot is issued later than the copy, on the same
//! context, and therefore runs after it. No query, fence or flush is needed for that, and none
//! is made: the decoder thread never waits for the GPU (unlike the Vulkan paths, where two
//! queues have no order between them and the copy is waited for). The renderer's `Present`
//! flushes the lot.
//!
//! The other direction: a slot is written only while the decoder holds the only reference to
//! its frame. The renderer holds one only while it issues a frame's draws, so a copy into the
//! slot is issued after every draw that sampled its previous picture.

mod d3d11va;
mod slots;
mod upload;

use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use ffmpeg_sys_next as ff;
use gpui_windows::windows::Win32::Graphics::Direct3D11::{
    D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0, D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
    D3D11_DECODER_PROFILE_HEVC_VLD_MAIN, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    ID3D11VideoDevice,
};
use gpui_windows::windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use gpui_windows::windows::Win32::Graphics::Dxgi::IDXGIDevice;
use gpui_windows::windows::core::{GUID, Interface};
use gpui_windows::{D3d11ContextGuard, D3d11Devices};
use parking_lot::Mutex;

use crate::codec::{Codec, CodecSupport, DecodeBackend, DecodePath};
use crate::decode::{
    Backend, ColorInfo, DecodeError, DecodeResult, Matrix, PictureDecoder, SoftwareDecoder,
    av_error, software_support,
};
use crate::hub::StreamTarget;
use crate::select::Order;

/// `AVD3D11VADeviceContext` (libavutil/hwcontext_d3d11va.h, FFmpeg 8.0 and 8.1: the same nine
/// fields in this order). ffmpeg-sys-next binds none of that header (it needs `d3d11.h`), so
/// it is declared here, field for field. The COM pointers are raw: FFmpeg owns one reference
/// to each that is set and releases it with the context.
#[repr(C)]
struct AvD3d11vaDeviceContext {
    /// `ID3D11Device *device`: must be set by the user.
    device: *mut c_void,
    /// `ID3D11DeviceContext *device_context`: set from the device on init if unset.
    device_context: *mut c_void,
    /// `ID3D11VideoDevice *video_device`: set from the device on init if unset.
    video_device: *mut c_void,
    /// `ID3D11VideoContext *video_context`: set from the device context on init if unset.
    video_context: *mut c_void,
    /// `void (*lock)(void *lock_ctx)`: protects `device_context` and `video_context` calls;
    /// must be recursive.
    lock: Option<unsafe extern "C" fn(*mut c_void)>,
    /// `void (*unlock)(void *lock_ctx)`.
    unlock: Option<unsafe extern "C" fn(*mut c_void)>,
    /// `void *lock_ctx`.
    lock_ctx: *mut c_void,
    /// `UINT BindFlags`: for frames allocated from this device context.
    bind_flags: u32,
    /// `UINT MiscFlags`.
    misc_flags: u32,
}

/// FFmpeg's `lock`: enters the immediate context's critical section.
unsafe extern "C" fn lock_context(lock_ctx: *mut c_void) {
    // Safety: `lock_ctx` is the `ID3D11Multithread` pointer `GpuDevice::hw_device` installed,
    // alive for as long as the FFmpeg device context is (the `GpuDevice` owns both and frees
    // FFmpeg's first).
    unsafe {
        if let Some(multithread) = ID3D11Multithread::from_raw_borrowed(&lock_ctx) {
            multithread.Enter();
        }
    }
}

/// FFmpeg's `unlock`.
unsafe extern "C" fn unlock_context(lock_ctx: *mut c_void) {
    // Safety: as `lock_context`; called on the thread that locked.
    unsafe {
        if let Some(multithread) = ID3D11Multithread::from_raw_borrowed(&lock_ctx) {
            multithread.Leave();
        }
    }
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

/// The renderer's device as the decoders use it. One per renderer device, alive while a
/// decoder is.
pub(crate) struct GpuDevice {
    /// FFmpeg's device context. First field: dropped before `multithread`, which its lock
    /// callbacks borrow.
    hw: OnceLock<Result<HwDevice, String>>,
    multithread: Option<ID3D11Multithread>,
    pub devices: D3d11Devices,
}

/// A failed Direct3D call: a device loss when the device says it was removed.
pub(crate) fn d3d_error(
    device: &GpuDevice,
    what: &str,
    error: gpui_windows::windows::core::Error,
) -> DecodeError {
    if device.lost() {
        DecodeError::device_lost(format!("{what}: the GPU device was removed"))
    } else {
        DecodeError::decoder(format!("{what}: {error}"))
    }
}

impl GpuDevice {
    fn new(devices: D3d11Devices) -> Arc<Self> {
        Arc::new(Self {
            hw: OnceLock::new(),
            multithread: devices.context.cast().ok(),
            devices,
        })
    }

    pub fn device(&self) -> &ID3D11Device {
        &self.devices.device
    }

    pub fn context(&self) -> &ID3D11DeviceContext {
        &self.devices.context
    }

    /// Holds the immediate context for a sequence of calls (see the module comment). The same
    /// critical section FFmpeg's decoders and the renderer's frame take.
    pub fn lock(&self) -> Option<D3d11ContextGuard> {
        self.devices.lock_context()
    }

    /// The device was removed, or the renderer has moved to another one.
    pub fn lost(&self) -> bool {
        self.devices.removed()
            || gpui_windows::shared_devices().is_none_or(|current| current.device != self.devices.device)
    }

    /// FFmpeg's device context, made on first use.
    fn hw_device(&self) -> DecodeResult<*mut ff::AVBufferRef> {
        let made = self.hw.get_or_init(|| {
            let multithread = self.multithread.as_ref().ok_or(
                "the Direct3D context has no multithread interface: it cannot be shared with a decoder thread",
            )?;
            // Safety: plain FFmpeg object setup. The device context takes one reference to the
            // device and one to its immediate context (`into_raw` of a clone: an AddRef that
            // FFmpeg's uninit releases); the lock context is borrowed and outlives it (field
            // order of `GpuDevice`).
            unsafe {
                let mut buffer =
                    ff::av_hwdevice_ctx_alloc(ff::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
                if buffer.is_null() {
                    return Err(
                        "FFmpeg was built without D3D11VA (av_hwdevice_ctx_alloc failed)".into(),
                    );
                }
                let context = (*buffer).data.cast::<ff::AVHWDeviceContext>();
                let d3d = (*context).hwctx.cast::<AvD3d11vaDeviceContext>();
                (*d3d).device = self.devices.device.clone().into_raw();
                (*d3d).device_context = self.devices.context.clone().into_raw();
                (*d3d).lock = Some(lock_context);
                (*d3d).unlock = Some(unlock_context);
                (*d3d).lock_ctx = multithread.as_raw();
                let initialised = ff::av_hwdevice_ctx_init(buffer);
                if initialised < 0 {
                    ff::av_buffer_unref(&mut buffer);
                    return Err(format!(
                        "av_hwdevice_ctx_init on the renderer's device: {} (the driver has no Direct3D 11 video device?)",
                        av_error(initialised)
                    ));
                }
                log::info!("video: D3D11VA on the renderer's device");
                Ok(HwDevice(buffer))
            }
        });
        match made {
            Ok(device) => Ok(device.0),
            Err(message) => Err(DecodeError::decoder(message.clone())),
        }
    }
}

pub(crate) fn surface_color(color: ColorInfo) -> gpui_windows::SurfaceColor {
    gpui_windows::SurfaceColor {
        matrix: match color.matrix {
            Matrix::Bt601 => gpui_windows::SurfaceMatrix::Bt601,
            Matrix::Bt709 => gpui_windows::SurfaceMatrix::Bt709,
            Matrix::Bt2020 => gpui_windows::SurfaceMatrix::Bt2020,
        },
        full_range: color.full_range,
        chroma_offset: if color.chroma_centered {
            [0.0, 0.0]
        } else {
            [0.25, 0.0]
        },
    }
}

/// Plane textures alive in the process (an `NV12` slot counts as two, like the two textures
/// of a software slot).
pub(crate) static LIVE_PLANES: AtomicI64 = AtomicI64::new(0);

/// The DXVA decoder profile a stream of `codec` is decoded with (8-bit 4:2:0).
fn decoder_profile(codec: Codec) -> GUID {
    match codec {
        Codec::H264 => D3D11_DECODER_PROFILE_H264_VLD_NOFGT,
        Codec::Hevc => D3D11_DECODER_PROFILE_HEVC_VLD_MAIN,
        Codec::Av1 => D3D11_DECODER_PROFILE_AV1_VLD_PROFILE0,
    }
}

/// Whether the driver lists a decoder for `codec` that outputs NV12. Listing is not decoding:
/// the probe opens the decoder and decodes a frame.
fn driver_decodes(device: &ID3D11Device, codec: Codec) -> Result<(), String> {
    let video: ID3D11VideoDevice = device
        .cast()
        .map_err(|error| format!("the device has no Direct3D 11 video interface: {error}"))?;
    let wanted = decoder_profile(codec);
    // Safety: queries on the free-threaded video device; the index is below the count it gave.
    unsafe {
        let listed = (0..video.GetVideoDecoderProfileCount())
            .any(|index| video.GetVideoDecoderProfile(index).is_ok_and(|profile| profile == wanted));
        if !listed {
            return Err(format!(
                "the GPU driver has no {} decoder (DXVA profile not listed)",
                codec.label()
            ));
        }
        match video.CheckVideoDecoderFormat(&wanted, DXGI_FORMAT_NV12) {
            Ok(supported) if supported.as_bool() => Ok(()),
            _ => Err(format!(
                "the GPU driver's {} decoder does not output NV12",
                codec.label()
            )),
        }
    }
}

pub(crate) struct WindowsBackend {
    /// The device the current decoders are on. Weak: with the last decoder it goes, and with
    /// it FFmpeg's device context.
    device: Mutex<Weak<GpuDevice>>,
}

impl WindowsBackend {
    pub fn new() -> Self {
        Self {
            device: Mutex::new(Weak::new()),
        }
    }

    /// The renderer's current device, wrapped.
    fn device(&self) -> DecodeResult<Arc<GpuDevice>> {
        let devices = gpui_windows::shared_devices()
            .filter(|devices| !devices.removed())
            .ok_or_else(|| DecodeError::device_lost("the renderer has no GPU device"))?;
        let mut cached = self.device.lock();
        if let Some(device) = cached.upgrade()
            && device.devices.device == devices.device
        {
            return Ok(device);
        }
        let device = GpuDevice::new(devices);
        *cached = Arc::downgrade(&device);
        Ok(device)
    }
}

impl Backend for WindowsBackend {
    /// D3D11VA (every vendor's hardware decoder on Windows), then software. Without a device
    /// there is nothing to upload to either: software, which then waits for one.
    fn order(&self) -> Order {
        if self.ready() {
            Order::new(&[DecodeBackend::D3d11va, DecodeBackend::Software])
        } else {
            Order::SOFTWARE
        }
    }

    fn open(
        &self,
        backend: DecodeBackend,
        codec: Codec,
        description: Option<&[u8]>,
        target: &StreamTarget,
    ) -> DecodeResult<Box<dyn PictureDecoder>> {
        let device = self.device()?;
        let opened = (|| -> DecodeResult<Box<dyn PictureDecoder>> {
            match backend {
                DecodeBackend::D3d11va => {
                    driver_decodes(device.device(), codec).map_err(DecodeError::decoder)?;
                    let hw_device = device.hw_device()?;
                    Ok(Box::new(d3d11va::D3d11vaDecoder::open(
                        device.clone(),
                        hw_device,
                        codec,
                        description,
                        target,
                    )?))
                }
                DecodeBackend::Software => {
                    let uploader =
                        upload::D3d11Uploader::new(device.clone(), target.surface.clone());
                    Ok(Box::new(SoftwareDecoder::open(
                        codec,
                        description,
                        Box::new(uploader),
                    )?))
                }
                DecodeBackend::VulkanVideo | DecodeBackend::Nvdec | DecodeBackend::Vaapi => {
                    Err(DecodeError::decoder(format!(
                        "{} decode is not built for this platform",
                        backend.label()
                    )))
                }
            }
        })();
        opened.map_err(|error| classify(&device, error))
    }

    fn ready(&self) -> bool {
        gpui_windows::shared_devices().is_some_and(|devices| !devices.removed())
    }

    fn simulate_device_loss(&self) {
        gpui_windows::simulate_device_loss();
    }

    fn support(&self, backend: DecodeBackend, codec: Codec) -> CodecSupport {
        match backend {
            DecodeBackend::Software => software_support(codec),
            DecodeBackend::D3d11va => {
                let listed = gpui_windows::shared_devices()
                    .filter(|devices| !devices.removed())
                    .is_some_and(|devices| driver_decodes(&devices.device, codec).is_ok());
                CodecSupport {
                    codec,
                    path: Some(DecodePath::Hardware),
                    backend: Some(backend),
                    probed: false,
                    // Direct3D 11 has no query for a decoder's largest picture.
                    max_size: None,
                }
                .filter(listed)
            }
            DecodeBackend::VulkanVideo | DecodeBackend::Nvdec | DecodeBackend::Vaapi => {
                CodecSupport::none(codec)
            }
        }
    }

    fn device_key(&self) -> Option<String> {
        let devices = gpui_windows::shared_devices().filter(|devices| !devices.removed())?;
        // Safety: queries on the adapter the device was created on.
        unsafe {
            let description = devices.adapter.GetDesc1().ok()?;
            // The user-mode driver's version, as `gpu_specs` reads it.
            let driver = devices
                .adapter
                .CheckInterfaceSupport(&IDXGIDevice::IID)
                .map(|version| {
                    format!(
                        "{}.{}.{}.{}",
                        version >> 48,
                        (version >> 32) & 0xFFFF,
                        (version >> 16) & 0xFFFF,
                        version & 0xFFFF
                    )
                })
                .unwrap_or_else(|_| "unknown".into());
            Some(format!(
                "{:04x}:{:04x} D3D11 {driver}",
                description.VendorId, description.DeviceId
            ))
        }
    }

    fn live_textures(&self) -> i64 {
        LIVE_PLANES.load(Ordering::Relaxed)
    }

    /// Nothing to do: the renderer lets go of a picture as soon as it has issued the draws
    /// that sample it, and Direct3D keeps what queued commands still need by itself.
    fn collect(&self) {}
}

/// A failure that does not say whether the device is gone: ask the device.
pub(crate) fn classify(device: &GpuDevice, error: DecodeError) -> DecodeError {
    if error.failure == crate::decode::Failure::Decoder && device.lost() {
        return DecodeError::device_lost(error.message);
    }
    error
}

//! Shared D3D11 device for capture + video processor + FFmpeg encoder.
//!
//! All three pipeline stages run on the encoder thread and share one
//! ID3D11Device + ID3D11DeviceContext. WGC capture writes BGRA
//! textures into D3D11; ID3D11VideoProcessor converts to NV12;
//! FFmpeg's `AV_PIX_FMT_D3D11` codec context consumes the NV12
//! texture zero-copy.

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0,
    D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
};

/// PCI vendor ids of the encoder vendors.
pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;

#[derive(Clone)]
pub struct GpuDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

impl GpuDevice {
    /// Create a D3D11 device on the DEFAULT adapter with VIDEO_SUPPORT for
    /// the video processor and BGRA_SUPPORT so it can interoperate with the
    /// WGC capture pool's BGRA8 textures. MULTITHREADED protection is
    /// enabled because FFmpeg's NVENC/AMF bindings can submit work from
    /// their own worker threads while we still hold the device on the
    /// encoder thread.
    pub fn create() -> Result<Self, String> {
        let gpu = Self::create_on(None)?;
        log::info!(
            "[gpu] D3D11 device on the default adapter: {}",
            describe_device_adapter(&gpu.device)
        );
        Ok(gpu)
    }

    /// Create the device on the first hardware adapter whose PCI vendor id
    /// is `vendor_id`, so the hardware encoder (NVENC on 0x10DE, AMF on
    /// 0x1002) is opened on its own GPU. On hybrid laptops the default
    /// adapter is often the other vendor's iGPU, and NVENC/AMF fail to open
    /// on a foreign device. Falls back to the default adapter when no such
    /// adapter exists or device creation on it fails.
    pub fn create_for_vendor(vendor_id: u32) -> Result<Self, String> {
        if vendor_id != 0 {
            match find_adapter_for_vendor(vendor_id) {
                Some((adapter, name)) => match Self::create_on(Some(&adapter)) {
                    Ok(gpu) => {
                        log::info!(
                            "[gpu] D3D11 device on adapter '{}' (vendor {:#06x})",
                            name,
                            vendor_id
                        );
                        return Ok(gpu);
                    }
                    Err(e) => log::warn!(
                        "[gpu] device creation on adapter '{}' (vendor {:#06x}) failed ({}); \
                         falling back to the default adapter",
                        name,
                        vendor_id,
                        e
                    ),
                },
                None => log::info!(
                    "[gpu] no hardware adapter with vendor {:#06x}; using the default adapter",
                    vendor_id
                ),
            }
        }
        Self::create()
    }

    /// `adapter = None` → default adapter (D3D_DRIVER_TYPE_HARDWARE).
    /// An explicit adapter requires D3D_DRIVER_TYPE_UNKNOWN.
    fn create_on(adapter: Option<&IDXGIAdapter1>) -> Result<Self, String> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        let created = unsafe {
            match adapter {
                Some(adapter) => D3D11CreateDevice(
                    adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    flags,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                ),
                None => D3D11CreateDevice(
                    None,
                    D3D_DRIVER_TYPE_HARDWARE,
                    HMODULE::default(),
                    flags,
                    Some(&feature_levels),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                ),
            }
        };
        created.map_err(|e| format!("D3D11CreateDevice: {e:?}"))?;
        let device = device.ok_or("D3D11CreateDevice returned None device")?;
        let context = context.ok_or("D3D11CreateDevice returned None context")?;
        enable_multithread_protection(&device, &context);
        Ok(Self { device, context })
    }
}

/// Turn on D3D11 multithread protection so FFmpeg's NVENC/AMF bindings can
/// safely call into the device from their worker threads. ID3D11Multithread
/// is documented on the immediate context; some runtimes also answer the QI
/// on the device — try the device first (historical behaviour), then the
/// context. Logs the resulting state: an unprotected device shared with the
/// encoder runtime is a crash/corruption suspect worth seeing in a log.
fn enable_multithread_protection(device: &ID3D11Device, context: &ID3D11DeviceContext) {
    let mt = device
        .cast::<ID3D11Multithread>()
        .or_else(|_| context.cast::<ID3D11Multithread>());
    match mt {
        Ok(mt) => unsafe {
            let _previous = mt.SetMultithreadProtected(true);
            let on = mt.GetMultithreadProtected().as_bool();
            if on {
                log::info!("[gpu] D3D11 multithread protection enabled");
            } else {
                log::warn!("[gpu] D3D11 multithread protection did NOT stick");
            }
        },
        Err(e) => log::warn!("[gpu] ID3D11Multithread unavailable ({e:?}); device is unprotected"),
    }
}

/// First non-software adapter with the given PCI vendor id, plus its
/// description for logging.
fn find_adapter_for_vendor(vendor_id: u32) -> Option<(IDXGIAdapter1, String)> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut i = 0u32;
        // EnumAdapters1 returns DXGI_ERROR_NOT_FOUND past the last adapter.
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            // desc.Flags is u32 in windows-rs 0.61; the flag's inner value
            // is i32 — cast before AND.
            if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                continue;
            }
            if desc.VendorId == vendor_id {
                return Some((adapter, utf16_name(&desc.Description)));
            }
        }
        None
    }
}

/// "<description> (vendor 0x....)" of the adapter a device lives on.
fn describe_device_adapter(device: &ID3D11Device) -> String {
    unsafe {
        let desc = device
            .cast::<IDXGIDevice>()
            .and_then(|d| d.GetAdapter())
            .and_then(|a| a.GetDesc());
        match desc {
            Ok(d) => format!("'{}' (vendor {:#06x})", utf16_name(&d.Description), d.VendorId),
            Err(e) => format!("<unknown adapter: {e:?}>"),
        }
    }
}

fn utf16_name(raw: &[u16]) -> String {
    let len = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
    String::from_utf16_lossy(&raw[..len])
}

/// PCI vendor id of the hardware encoder family an FFmpeg encoder name
/// belongs to (0 = no preference).
pub fn vendor_for_encoder(encoder_name: &str) -> u32 {
    if encoder_name.ends_with("_nvenc") {
        VENDOR_NVIDIA
    } else if encoder_name.ends_with("_amf") {
        VENDOR_AMD
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_d3d11_device() {
        let gpu = GpuDevice::create()
            .expect("D3D11 device creation should succeed on the dev box");
        // Smoke: confirm the device + context are non-null COM pointers.
        let _ = gpu.device;
        let _ = gpu.context;
    }

    #[test]
    fn vendor_from_encoder_name() {
        assert_eq!(vendor_for_encoder("h264_nvenc"), VENDOR_NVIDIA);
        assert_eq!(vendor_for_encoder("av1_amf"), VENDOR_AMD);
        assert_eq!(vendor_for_encoder("h264_qsv"), 0);
    }
}

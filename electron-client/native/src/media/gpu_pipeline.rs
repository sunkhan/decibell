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
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_SDK_VERSION,
};

#[derive(Clone)]
pub struct GpuDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

impl GpuDevice {
    /// Create a D3D11 device with VIDEO_SUPPORT for the video processor
    /// and BGRA_SUPPORT so it can interoperate with the WGC capture
    /// pool's BGRA8 textures. MULTITHREADED protection is enabled
    /// because FFmpeg's NVENC binding can submit work from its own
    /// worker thread while we still hold the device on the encoder
    /// thread.
    pub fn create() -> Result<Self, String> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice: {e:?}"))?;
        }
        let device = device.ok_or("D3D11CreateDevice returned None device")?;
        let context = context.ok_or("D3D11CreateDevice returned None context")?;
        // Enable multithread protection so FFmpeg's NVENC binding
        // can safely call into the device from its worker thread.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            unsafe {
                mt.SetMultithreadProtected(true);
            }
        }
        Ok(Self { device, context })
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
}

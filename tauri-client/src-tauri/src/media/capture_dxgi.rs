use super::capture::{CaptureConfig, CaptureSource, CaptureSourceType, RawFrame};

use windows::{
    core::Interface,
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Dxgi::*,
    Win32::Graphics::Dxgi::Common::*,
    Win32::Foundation::*,
};

/// List available monitors via DXGI enumeration.
pub fn list_sources() -> Result<Vec<CaptureSource>, String> {
    let mut sources = Vec::new();

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;

        let mut adapter_idx: u32 = 0;
        loop {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(adapter_idx) {
                Ok(a) => a,
                Err(_) => break,
            };

            let mut output_idx: u32 = 0;
            loop {
                let output: IDXGIOutput = match adapter.EnumOutputs(output_idx) {
                    Ok(o) => o,
                    Err(_) => break,
                };

                let desc = match output.GetDesc() {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[capture-dxgi] GetDesc failed: {}", e);
                        output_idx += 1;
                        continue;
                    }
                };

                let coords = desc.DesktopCoordinates;
                let width = (coords.right - coords.left).unsigned_abs();
                let height = (coords.bottom - coords.top).unsigned_abs();

                let name_raw = &desc.DeviceName;
                let end = name_raw.iter().position(|&c| c == 0).unwrap_or(name_raw.len());
                let name = if end > 0 {
                    String::from_utf16_lossy(&name_raw[..end])
                } else {
                    format!("Monitor {}", sources.len() + 1)
                };

                sources.push(CaptureSource {
                    id: format!("monitor:{}:{}", adapter_idx, output_idx),
                    name,
                    source_type: CaptureSourceType::Screen,
                    width,
                    height,
                });

                output_idx += 1;
            }

            adapter_idx += 1;
        }
    }

    Ok(sources)
}

/// Start DXGI Desktop Duplication capture on a monitor.
pub fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let _ = (source_id, config);
    Err("Not yet implemented".to_string())
}

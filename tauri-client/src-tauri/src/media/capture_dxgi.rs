use super::capture::{CaptureConfig, CaptureOutput, CaptureSource, CaptureSourceType, RawFrame};

use windows::{
    core::{Interface, BOOL},
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Dxgi::*,
    Win32::Graphics::Dxgi::Common::*,
    Win32::Graphics::Gdi::*,
    Win32::Foundation::*,
};
use base64::Engine;

/// Encode raw BGRA pixels as a 32-bit BMP and return a base64 data URI.
fn bgra_to_bmp_data_uri(width: u32, height: u32, bgra_pixels: &[u8]) -> String {
    let pixel_bytes = (width * height * 4) as u32;
    let file_size = 54 + pixel_bytes;
    let mut bmp = Vec::with_capacity(file_size as usize);

    // BMP file header (14 bytes)
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]); // reserved
    bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel data offset

    // DIB header — BITMAPINFOHEADER (40 bytes)
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&height.to_le_bytes()); // positive = bottom-up (matches GetDIBits)
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&32u16.to_le_bytes()); // bpp
    bmp.extend_from_slice(&[0u8; 4]); // compression (BI_RGB)
    bmp.extend_from_slice(&pixel_bytes.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 16]); // ppm + colors

    bmp.extend_from_slice(bgra_pixels);

    format!(
        "data:image/bmp;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bmp)
    )
}

/// Capture a GDI thumbnail of a screen region.
fn capture_screen_thumbnail(left: i32, top: i32, width: u32, height: u32) -> Option<String> {
    const MAX_THUMB_W: u32 = 240;
    let (tw, th) = if width > MAX_THUMB_W {
        let scale = MAX_THUMB_W as f64 / width as f64;
        (MAX_THUMB_W, (height as f64 * scale) as u32)
    } else {
        (width, height)
    };

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() { return None; }
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        if mem_dc.is_invalid() {
            ReleaseDC(None, screen_dc);
            return None;
        }
        let bmp_handle = CreateCompatibleBitmap(screen_dc, tw as i32, th as i32);
        if bmp_handle.is_invalid() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            return None;
        }
        let old = SelectObject(mem_dc, bmp_handle.into());

        let _ = SetStretchBltMode(mem_dc, HALFTONE);
        let _ = StretchBlt(
            mem_dc, 0, 0, tw as i32, th as i32,
            Some(screen_dc), left, top, width as i32, height as i32,
            SRCCOPY,
        );

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: tw as i32,
                biHeight: th as i32,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut pixels = vec![0u8; (tw * th * 4) as usize];
        GetDIBits(
            mem_dc, bmp_handle, 0, th,
            Some(pixels.as_mut_ptr() as _),
            &mut bi, DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bmp_handle.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);

        Some(bgra_to_bmp_data_uri(tw, th, &pixels))
    }
}

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

                let thumbnail = capture_screen_thumbnail(
                    coords.left, coords.top, width, height,
                );

                sources.push(CaptureSource {
                    id: format!("monitor:{}:{}", adapter_idx, output_idx),
                    name,
                    source_type: CaptureSourceType::Screen,
                    width,
                    height,
                    thumbnail,
                });

                output_idx += 1;
            }

            adapter_idx += 1;
        }
    }

    Ok(sources)
}

// ─── D3D11 Device Setup ─────────────────────────────────────────────────────

fn create_device_for_adapter(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut actual_level = D3D_FEATURE_LEVEL_11_0;

    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut actual_level),
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice: {}", e))?;
    }

    Ok((
        device.ok_or("D3D11CreateDevice returned None")?,
        context.ok_or("D3D11CreateDevice context None")?,
    ))
}

// ─── VideoProcessor ─────────────────────────────────────────────────────────

struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    output_view: ID3D11VideoProcessorOutputView,
    nv12_texture: ID3D11Texture2D,
    staging_texture: ID3D11Texture2D,
    output_width: u32,
    output_height: u32,
}

// Safety: VideoProcessor is created and used on a single capture thread.
// COM objects are moved once from the spawning thread to the capture thread.
unsafe impl Send for VideoProcessor {}

impl VideoProcessor {
    fn new(
        device: &ID3D11Device,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self, String> {
        unsafe {
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoDevice: {}", e))?;

            let base_context = device
                .GetImmediateContext()
                .map_err(|e| format!("GetImmediateContext: {}", e))?;

            let video_context: ID3D11VideoContext = base_context
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoContext: {}", e))?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: src_w,
                InputHeight: src_h,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: dst_w,
                OutputHeight: dst_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator: {}", e))?;

            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {}", e))?;

            // Set explicit color spaces for correct BGRA→NV12 conversion.
            // Input is always BGRA/sRGB (DuplicateOutput1 handles HDR→SDR).
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor, 0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }

            let nv12_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let mut nv12_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))
                .map_err(|e| format!("CreateTexture2D (NV12): {}", e))?;
            let nv12_texture = nv12_texture.ok_or("CreateTexture2D (NV12) returned None")?;

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))
                .map_err(|e| format!("CreateTexture2D (staging): {}", e))?;
            let staging_texture = staging_texture.ok_or("CreateTexture2D (staging) returned None")?;

            let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };

            let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
            video_device
                .CreateVideoProcessorOutputView(
                    &nv12_texture,
                    &enumerator,
                    &output_view_desc,
                    Some(&mut output_view),
                )
                .map_err(|e| format!("CreateVideoProcessorOutputView: {}", e))?;
            let output_view = output_view.ok_or("CreateVideoProcessorOutputView returned None")?;

            Ok(VideoProcessor {
                video_device,
                video_context,
                processor,
                enumerator,
                output_view,
                nv12_texture,
                staging_texture,
                output_width: dst_w,
                output_height: dst_h,
            })
        }
    }

    fn convert_and_readback(
        &self,
        context: &ID3D11DeviceContext,
        bgra_texture: &ID3D11Texture2D,
    ) -> Result<Vec<u8>, String> {
        unsafe {
            let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };

            let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    bgra_texture,
                    &self.enumerator,
                    &input_view_desc,
                    Some(&mut input_view),
                )
                .map_err(|e| format!("CreateVideoProcessorInputView: {}", e))?;
            let input_view = input_view.ok_or("CreateVideoProcessorInputView returned None")?;

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };

            self.video_context
                .VideoProcessorBlt(
                    &self.processor,
                    &self.output_view,
                    0,
                    std::slice::from_ref(&stream),
                )
                .map_err(|e| format!("VideoProcessorBlt: {}", e))?;

            // Release the COM reference that ManuallyDrop prevented from being dropped
            std::mem::ManuallyDrop::into_inner(std::ptr::read(&stream.pInputSurface));

            context.CopyResource(&self.staging_texture, &self.nv12_texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(
                    &self.staging_texture,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped),
                )
                .map_err(|e| format!("Map staging: {}", e))?;

            let w = self.output_width as usize;
            let h = self.output_height as usize;
            let row_pitch = mapped.RowPitch as usize;
            let data_ptr = mapped.pData as *const u8;

            let y_size = w * h;
            let uv_size = w * h / 2;
            let mut nv12 = Vec::with_capacity(y_size + uv_size);

            for row in 0..h {
                let src_row = std::slice::from_raw_parts(data_ptr.add(row * row_pitch), w);
                nv12.extend_from_slice(src_row);
            }

            let uv_base = data_ptr.add(h * row_pitch);
            for row in 0..(h / 2) {
                let src_row = std::slice::from_raw_parts(uv_base.add(row * row_pitch), w);
                nv12.extend_from_slice(src_row);
            }

            context.Unmap(&self.staging_texture, 0);

            Ok(nv12)
        }
    }
}

// ─── Public Capture API ─────────────────────────────────────────────────────

/// Start DXGI Desktop Duplication capture on a monitor.
/// source_id format: "monitor:{adapter_idx}:{output_idx}"
pub fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<CaptureOutput, String> {
    let rest = source_id
        .strip_prefix("monitor:")
        .ok_or_else(|| format!("DXGI DD only supports monitor sources, got: {}", source_id))?;

    let mut parts = rest.splitn(2, ':');
    let adapter_idx: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or("Invalid adapter_idx")?;
    let output_idx: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or("Invalid output_idx")?;

    // Resolve actual output dimensions (needed so the encoder matches the capture)
    let (out_w, out_h) = unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapters1(adapter_idx)
            .map_err(|e| format!("EnumAdapters1: {}", e))?;
        let output: IDXGIOutput = adapter.EnumOutputs(output_idx)
            .map_err(|e| format!("EnumOutputs: {}", e))?;
        let desc = output.GetDesc().map_err(|e| format!("GetDesc: {}", e))?;
        let coords = desc.DesktopCoordinates;
        let src_w = (coords.right - coords.left).unsigned_abs();
        let src_h = (coords.bottom - coords.top).unsigned_abs();
        let w = if config.target_width == 0 { src_w } else { config.target_width };
        let h = if config.target_height == 0 { src_h } else { config.target_height };
        (w, h)
    };

    let target_fps = config.target_fps;
    let target_w = config.target_width;
    let target_h = config.target_height;

    let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);

    // The capture thread signals whether it started successfully (first frame acquired)
    // or failed (e.g. access lost due to legacy exclusive fullscreen).
    // This lets the caller fall back to WGC if DXGI DD can't capture.
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);

    std::thread::Builder::new()
        .name("decibell-dxgi-capture".to_string())
        .spawn(move || {
            if let Err(e) = dxgi_capture_thread(adapter_idx, output_idx, target_fps, target_w, target_h, tx, ready_tx.clone()) {
                eprintln!("[capture-dxgi] Fatal error: {}", e);
                let _ = ready_tx.try_send(Err(e));
            }
        })
        .map_err(|e| format!("Spawn DXGI capture thread: {}", e))?;

    // Wait up to 3 seconds for the capture thread to confirm it can acquire frames
    match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(CaptureOutput { receiver: rx, width: out_w, height: out_h }),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("DXGI DD timed out waiting for first frame".to_string()),
    }
}

// ─── HDR SDR White Level Query ──────────────────────────────────────────────

/// Query the SDR white level for a display and return a brightness correction
/// factor if HDR is active. Uses `DisplayConfigGetDeviceInfo` to read the
/// user's "SDR content brightness" slider value.
///
/// Returns `Some(factor)` where factor = 80.0 / sdr_white_nits if correction
/// is needed, or `None` for SDR displays.
fn query_sdr_white_level(gdi_device_name: &[u16]) -> Option<f32> {
    use windows::Win32::Devices::Display::*;
    use windows::Win32::Foundation::ERROR_SUCCESS;

    // Convert the GDI device name (null-terminated u16) to a Rust string for matching
    let gdi_name: String = gdi_device_name
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8 as char)
        .collect();

    unsafe {
        // Get all active display paths
        let mut path_count = 0u32;
        let mut mode_count = 0u32;
        let flags = QDC_ONLY_ACTIVE_PATHS;
        let result = GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count);
        if result != ERROR_SUCCESS {
            eprintln!("[capture-dxgi] GetDisplayConfigBufferSizes failed: {:?}", result);
            return None;
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];

        let result = QueryDisplayConfig(
            flags,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        );
        if result != ERROR_SUCCESS {
            eprintln!("[capture-dxgi] QueryDisplayConfig failed: {:?}", result);
            return None;
        }
        paths.truncate(path_count as usize);

        // Find the path that matches our DXGI output's GDI device name
        for path in &paths {
            // Get the source device name to match against our GDI name
            let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source_name.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source_name.header.adapterId = path.sourceInfo.adapterId;
            source_name.header.id = path.sourceInfo.id;

            if DisplayConfigGetDeviceInfo(&mut source_name.header) != 0 {
                continue;
            }

            let source_gdi: String = source_name.viewGdiDeviceName
                .iter()
                .take_while(|&&c| c != 0)
                .map(|&c| c as u8 as char)
                .collect();

            if source_gdi != gdi_name {
                continue;
            }

            // Found matching path — query the SDR white level
            let mut sdr_white = DISPLAYCONFIG_SDR_WHITE_LEVEL::default();
            sdr_white.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
            sdr_white.header.size = std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>() as u32;
            sdr_white.header.adapterId = path.targetInfo.adapterId;
            sdr_white.header.id = path.targetInfo.id;

            if DisplayConfigGetDeviceInfo(&mut sdr_white.header) != 0 {
                eprintln!("[capture-dxgi] SDR white level query failed for '{}'", gdi_name);
                return None;
            }

            // SDRWhiteLevel is encoded as (nits / 80) * 1000
            // Value 1000 = 80 nits (standard sRGB), no correction needed
            let raw = sdr_white.SDRWhiteLevel;
            let sdr_nits = (raw as f32) * 80.0 / 1000.0;
            eprintln!("[capture-dxgi] SDR white level for '{}': {} nits (raw={})", gdi_name, sdr_nits, raw);

            if sdr_nits > 80.0 + 1.0 {
                // HDR is active with boosted SDR brightness.
                // DuplicateOutput1 already does partial tone mapping, so a full
                // 80/sdr correction over-darkens. Use a softer curve: square root
                // of the raw ratio preserves natural brightness while fixing the
                // blown-out look on SDR displays.
                let factor = (80.0f32 / sdr_nits).sqrt();
                eprintln!("[capture-dxgi] HDR→SDR brightness correction: factor={:.3} ({:.0}nits, sqrt curve)", factor, sdr_nits);
                return Some(factor);
            } else {
                // SDR display or HDR with no boost — no correction needed
                return None;
            }
        }

        eprintln!("[capture-dxgi] Could not find display config for '{}'", gdi_name);
        None
    }
}

// ─── Capture Thread ─────────────────────────────────────────────────────────

fn dxgi_capture_thread(
    adapter_idx: u32,
    output_idx: u32,
    target_fps: u32,
    target_w: u32,
    target_h: u32,
    tx: std::sync::mpsc::SyncSender<RawFrame>,
    ready_tx: std::sync::mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;

        let adapter: IDXGIAdapter1 = factory
            .EnumAdapters1(adapter_idx)
            .map_err(|e| format!("EnumAdapters1({}): {}", adapter_idx, e))?;

        let output: IDXGIOutput = adapter
            .EnumOutputs(output_idx)
            .map_err(|e| format!("EnumOutputs({}): {}", output_idx, e))?;

        let desc = output.GetDesc().map_err(|e| format!("GetDesc: {}", e))?;
        let coords = desc.DesktopCoordinates;
        let src_w = (coords.right - coords.left).unsigned_abs();
        let src_h = (coords.bottom - coords.top).unsigned_abs();

        let dst_w = if target_w == 0 { src_w } else { target_w };
        let dst_h = if target_h == 0 { src_h } else { target_h };

        // Query SDR white level for HDR→SDR brightness correction.
        // On HDR displays, DuplicateOutput1 tone-maps to SDR but the pixel
        // values are relative to the display's SDR white level (often 200+
        // nits). We need to scale down to the standard 80-nit sRGB reference
        // so the stream looks correct on SDR displays.
        let hdr_sdr_correction: Option<f32> = query_sdr_white_level(&desc.DeviceName);

        let (device, context) = create_device_for_adapter(&adapter)?;

        // Use DuplicateOutput1 (IDXGIOutput5, Win10 1803+) to request BGRA.
        // On HDR desktops the default format is R16G16B16A16_FLOAT which the
        // D3D11 Video Processor can't use as input. DuplicateOutput1 tells the
        // DWM to tone-map HDR→SDR and deliver BGRA frames instead.
        // Falls back to DuplicateOutput (IDXGIOutput1) on older Windows.
        let duplication = if let Ok(output5) = output.cast::<IDXGIOutput5>() {
            let supported = [DXGI_FORMAT_B8G8R8A8_UNORM];
            match output5.DuplicateOutput1(&device, 0, &supported) {
                Ok(dup) => {
                    eprintln!("[capture-dxgi] Using DuplicateOutput1 (BGRA, DWM handles HDR tone mapping)");
                    dup
                }
                Err(e) => {
                    eprintln!("[capture-dxgi] DuplicateOutput1 failed ({}), falling back", e);
                    let output1: IDXGIOutput1 = output.cast()
                        .map_err(|e| format!("Cast to IDXGIOutput1: {}", e))?;
                    output1.DuplicateOutput(&device)
                        .map_err(|e| format!("DuplicateOutput: {}", e))?
                }
            }
        } else {
            let output1: IDXGIOutput1 = output.cast()
                .map_err(|e| format!("Cast to IDXGIOutput1: {}", e))?;
            output1.DuplicateOutput(&device)
                .map_err(|e| format!("DuplicateOutput: {}", e))?
        };

        // The desktop duplication texture can't be used directly as a video
        // processor input (KeyedMutex, restricted bind flags). Create an
        // intermediate BGRA texture for the copy.
        let intermediate_desc = D3D11_TEXTURE2D_DESC {
            Width: src_w,
            Height: src_h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: 0,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut intermediate_tex: Option<ID3D11Texture2D> = None;
        device
            .CreateTexture2D(&intermediate_desc, None, Some(&mut intermediate_tex))
            .map_err(|e| format!("CreateTexture2D (intermediate): {}", e))?;
        let intermediate_tex = intermediate_tex.ok_or("CreateTexture2D (intermediate) returned None")?;

        let video_proc = VideoProcessor::new(&device, src_w, src_h, dst_w, dst_h)?;

        eprintln!(
            "[capture-dxgi] Started: monitor {}:{}, {}x{} → {}x{} @ {}fps",
            adapter_idx, output_idx, src_w, src_h, dst_w, dst_h, target_fps
        );

        let frame_interval = std::time::Duration::from_micros(1_000_000 / target_fps as u64);
        let start = std::time::Instant::now();
        let mut frame_count: u64 = 0;
        let acquire_timeout_ms = (frame_interval.as_millis() as u32).max(1);
        let mut last_nv12: Option<Vec<u8>> = None;
        let mut signalled_ready = false;

        loop {
            let loop_start = std::time::Instant::now();

            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;

            let hr = duplication.AcquireNextFrame(
                acquire_timeout_ms,
                &mut frame_info,
                &mut desktop_resource,
            );

            // Try to acquire a new desktop frame. If the desktop hasn't changed,
            // re-send the last converted NV12 data to keep the encoder fed.
            let got_new_frame = match hr {
                Ok(()) => desktop_resource.is_some(),
                Err(e) => {
                    let code = e.code().0 as u32;
                    if code == 0x887A0027 {
                        false // DXGI_ERROR_WAIT_TIMEOUT — no desktop change
                    } else if code == 0x887A0026 {
                        eprintln!("[capture-dxgi] Access lost, stopping");
                        if !signalled_ready {
                            let _ = ready_tx.try_send(Err("DXGI access lost (exclusive fullscreen app?)".to_string()));
                        }
                        break;
                    } else {
                        eprintln!("[capture-dxgi] AcquireNextFrame error: {}", e);
                        if !signalled_ready {
                            let _ = ready_tx.try_send(Err(format!("AcquireNextFrame: {}", e)));
                        }
                        break;
                    }
                }
            };

            let nv12 = if got_new_frame {
                let resource = desktop_resource.unwrap();
                let bgra_texture: ID3D11Texture2D = resource
                    .cast()
                    .map_err(|e| format!("Cast to ID3D11Texture2D: {}", e))?;

                context.CopyResource(&intermediate_tex, &bgra_texture);

                let mut data = match video_proc.convert_and_readback(&context, &intermediate_tex) {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("[capture-dxgi] convert error: {}", e);
                        let _ = duplication.ReleaseFrame();
                        continue;
                    }
                };

                let _ = duplication.ReleaseFrame();

                // Apply HDR→SDR brightness correction on the Y plane.
                // NV12 layout: Y plane (w*h bytes) then UV plane (w*h/2 bytes).
                // Y is in studio range [16..235]. We scale: Y' = 16 + (Y-16)*factor
                // UV (chrominance) is left unchanged — pure brightness scaling.
                if let Some(factor) = hdr_sdr_correction {
                    let y_size = dst_w as usize * dst_h as usize;
                    for y_val in data[..y_size].iter_mut() {
                        let y = *y_val as f32;
                        let corrected = 16.0 + (y - 16.0) * factor;
                        *y_val = corrected.clamp(16.0, 235.0) as u8;
                    }
                }

                last_nv12 = Some(data.clone());
                data
            } else {
                // No new frame — re-send last frame to keep encoder producing output
                match &last_nv12 {
                    Some(data) => data.clone(),
                    None => continue,
                }
            };

            frame_count += 1;
            let timestamp_us = start.elapsed().as_micros() as u64;

            if frame_count <= 3 || frame_count % 300 == 0 {
                eprintln!(
                    "[capture-dxgi] Frame {} ({:.1}s)",
                    frame_count,
                    start.elapsed().as_secs_f64()
                );
            }

            let raw_frame = RawFrame {
                data: nv12,
                width: dst_w,
                height: dst_h,
                stride: dst_w as usize,
                pixel_format: super::capture::PixelFormat::NV12,
                timestamp_us,
            };

            match tx.try_send(raw_frame) {
                Ok(()) => {
                    if !signalled_ready {
                        signalled_ready = true;
                        let _ = ready_tx.try_send(Ok(()));
                    }
                }
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    if !signalled_ready {
                        signalled_ready = true;
                        let _ = ready_tx.try_send(Ok(()));
                    }
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    eprintln!("[capture-dxgi] Channel closed, stopping");
                    break;
                }
            }

            // Frame pacing: sleep for remainder of frame interval
            let elapsed = loop_start.elapsed();
            if elapsed < frame_interval {
                std::thread::sleep(frame_interval - elapsed);
            }
        }

        eprintln!("[capture-dxgi] Capture loop exited after {} frames", frame_count);
        Ok(())
    }
}

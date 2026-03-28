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
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let rest = source_id
        .strip_prefix("monitor:")
        .ok_or_else(|| format!("DXGI DD only supports monitor sources, got: {}", source_id))?;

    let mut parts = rest.splitn(2, ':');
    let adapter_idx: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or("Invalid adapter_idx")?;
    let output_idx: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or("Invalid output_idx")?;

    let target_fps = config.target_fps;
    let target_w = config.target_width;
    let target_h = config.target_height;

    let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);

    std::thread::Builder::new()
        .name("decibell-dxgi-capture".to_string())
        .spawn(move || {
            if let Err(e) = dxgi_capture_thread(adapter_idx, output_idx, target_fps, target_w, target_h, tx) {
                eprintln!("[capture-dxgi] Fatal error: {}", e);
            }
        })
        .map_err(|e| format!("Spawn DXGI capture thread: {}", e))?;

    Ok(rx)
}

// ─── Capture Thread ─────────────────────────────────────────────────────────

fn dxgi_capture_thread(
    adapter_idx: u32,
    output_idx: u32,
    target_fps: u32,
    target_w: u32,
    target_h: u32,
    tx: std::sync::mpsc::SyncSender<RawFrame>,
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
                        break;
                    } else {
                        eprintln!("[capture-dxgi] AcquireNextFrame error: {}", e);
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

                let data = match video_proc.convert_and_readback(&context, &intermediate_tex) {
                    Ok(data) => data,
                    Err(e) => {
                        eprintln!("[capture-dxgi] convert error: {}", e);
                        let _ = duplication.ReleaseFrame();
                        continue;
                    }
                };

                let _ = duplication.ReleaseFrame();
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
                timestamp_us,
            };

            match tx.try_send(raw_frame) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {}
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

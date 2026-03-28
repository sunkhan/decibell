use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Instant;

use super::capture::{CaptureConfig, CaptureSource, CaptureSourceType, RawFrame};

use windows::{
    core::*,
    Graphics::Capture::*,
    Graphics::DirectX::Direct3D11::IDirect3DDevice,
    Graphics::DirectX::DirectXPixelFormat,
    Win32::Foundation::*,
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Dxgi::*,
    Win32::System::WinRT::Direct3D11::*,
    Win32::System::WinRT::Graphics::Capture::*,
    Win32::UI::WindowsAndMessaging::*,
};

// ─── Public API ──────────────────────────────────────────────────────────────

/// List available capture sources (monitors and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    tokio::task::spawn_blocking(|| {
        let mut sources = Vec::new();
        enumerate_monitors(&mut sources);
        enumerate_windows(&mut sources);
        Ok(sources)
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}

/// Start capturing from the given source. Returns a Receiver of RawFrames (NV12).
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let source_id = source_id.to_string();
    let config = config.clone();

    tokio::task::spawn_blocking(move || {
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(4);

        // D3D11 device + WinRT device
        let (device, context) = create_d3d11_device()?;
        let winrt_device = create_winrt_device(&device)?;

        // Capture item
        let item = create_capture_item(&source_id)?;

        // Determine source size
        let item_size = unsafe {
            item.Size().map_err(|e| format!("item.Size: {}", e))?
        };
        let src_w = item_size.Width as u32;
        let src_h = item_size.Height as u32;

        let dst_w = if config.target_width == 0 { src_w } else { config.target_width };
        let dst_h = if config.target_height == 0 { src_h } else { config.target_height };

        // Video processor for BGRA → NV12 conversion + scale
        let video_proc = VideoProcessor::new(&device, src_w, src_h, dst_w, dst_h)?;

        // Spawn capture thread
        std::thread::Builder::new()
            .name("decibell-capture".to_string())
            .spawn(move || {
                if let Err(e) = wgc_capture_loop(
                    device, context, winrt_device, item, video_proc,
                    tx, config, dst_w, dst_h,
                ) {
                    eprintln!("[capture-wgc] Capture loop error: {}", e);
                }
            })
            .map_err(|e| format!("Spawn capture thread: {}", e))?;

        Ok(rx)
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}

// ─── Source Enumeration ───────────────────────────────────────────────────────

fn enumerate_monitors(sources: &mut Vec<CaptureSource>) {
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[capture-wgc] CreateDXGIFactory1 failed: {}", e);
                return;
            }
        };

        let mut adapter_idx: u32 = 0;
        loop {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(adapter_idx) {
                Ok(a) => a,
                Err(_) => break, // DXGI_ERROR_NOT_FOUND
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
                        eprintln!("[capture-wgc] GetDesc failed: {}", e);
                        output_idx += 1;
                        continue;
                    }
                };

                let coords = desc.DesktopCoordinates;
                let width = (coords.right - coords.left).unsigned_abs();
                let height = (coords.bottom - coords.top).unsigned_abs();

                // Decode null-terminated UTF-16 name
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
}

fn enumerate_windows(sources: &mut Vec<CaptureSource>) {
    let mut hwnds: Vec<HWND> = Vec::new();

    unsafe {
        let _ = EnumWindows(
            Some(enum_windows_callback),
            LPARAM(&mut hwnds as *mut Vec<HWND> as isize),
        );
    }

    for hwnd in hwnds {
        unsafe {
            // Get window title
            let mut title_buf = [0u16; 512];
            let len = GetWindowTextW(hwnd, &mut title_buf);
            if len == 0 {
                continue;
            }
            let title = String::from_utf16_lossy(&title_buf[..len as usize]);

            // Get window rect
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                continue;
            }
            let width = (rect.right - rect.left) as u32;
            let height = (rect.bottom - rect.top) as u32;

            if width < 100 || height < 100 {
                continue;
            }

            sources.push(CaptureSource {
                id: format!("window:{}", hwnd.0 as usize),
                name: title,
                source_type: CaptureSourceType::Window,
                width,
                height,
            });
        }
    }
}

unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let hwnds = &mut *(lparam.0 as *mut Vec<HWND>);

    // Must be visible
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }

    // Must not be minimized
    if IsIconic(hwnd).as_bool() {
        return TRUE;
    }

    // Must have a non-empty title
    let title_len = GetWindowTextLengthW(hwnd);
    if title_len == 0 {
        return TRUE;
    }

    // Must have WS_CAPTION style (both WS_BORDER and WS_DLGFRAME bits)
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CAPTION.0 != WS_CAPTION.0 {
        return TRUE;
    }

    // Must NOT be a tool window
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
        return TRUE;
    }

    hwnds.push(hwnd);
    TRUE
}

// ─── D3D11 Device Setup ───────────────────────────────────────────────────────

fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut actual_level = D3D_FEATURE_LEVEL_11_0;

    unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
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

    let device = device.ok_or("D3D11CreateDevice returned no device")?;
    let context = context.ok_or("D3D11CreateDevice returned no context")?;

    Ok((device, context))
}

fn create_winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice, String> {
    unsafe {
        let dxgi_device: IDXGIDevice = device
            .cast()
            .map_err(|e| format!("Cast to IDXGIDevice: {}", e))?;

        let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
            .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {}", e))?;

        let winrt_device: IDirect3DDevice = inspectable
            .cast()
            .map_err(|e| format!("Cast to IDirect3DDevice: {}", e))?;

        Ok(winrt_device)
    }
}

// ─── Capture Item Creation ────────────────────────────────────────────────────

fn create_capture_item(source_id: &str) -> Result<GraphicsCaptureItem, String> {
    if let Some(rest) = source_id.strip_prefix("monitor:") {
        create_capture_item_for_monitor(rest)
    } else if let Some(rest) = source_id.strip_prefix("window:") {
        create_capture_item_for_window(rest)
    } else {
        Err(format!("Unknown source id format: {}", source_id))
    }
}

fn create_capture_item_for_monitor(id: &str) -> Result<GraphicsCaptureItem, String> {
    // id is "adapter_idx:output_idx"
    let mut parts = id.splitn(2, ':');
    let adapter_idx: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or("Invalid adapter_idx")?;
    let output_idx: u32 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or("Invalid output_idx")?;

    // Re-enumerate DXGI to get the HMONITOR
    let hmonitor = unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;

        let adapter: IDXGIAdapter1 = factory
            .EnumAdapters1(adapter_idx)
            .map_err(|e| format!("EnumAdapters1({}): {}", adapter_idx, e))?;

        let output: IDXGIOutput = adapter
            .EnumOutputs(output_idx)
            .map_err(|e| format!("EnumOutputs({}): {}", output_idx, e))?;

        let desc = output
            .GetDesc()
            .map_err(|e| format!("output.GetDesc: {}", e))?;

        desc.Monitor
    };

    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("IGraphicsCaptureItemInterop factory: {}", e))?;

    unsafe {
        interop
            .CreateForMonitor(hmonitor)
            .map_err(|e| format!("CreateForMonitor: {}", e))
    }
}

fn create_capture_item_for_window(id: &str) -> Result<GraphicsCaptureItem, String> {
    let hwnd_val: usize = id.parse().map_err(|_| format!("Invalid HWND: {}", id))?;
    let hwnd = HWND(hwnd_val as *mut _);

    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return Err(format!("HWND {} is no longer valid", hwnd_val));
        }
    }

    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("IGraphicsCaptureItemInterop factory: {}", e))?;

    unsafe {
        interop
            .CreateForWindow(hwnd)
            .map_err(|e| format!("CreateForWindow: {}", e))
    }
}

// ─── VideoProcessor ───────────────────────────────────────────────────────────

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

impl VideoProcessor {
    fn new(
        device: &ID3D11Device,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self, String> {
        unsafe {
            // Cast to video device and context
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoDevice: {}", e))?;

            let mut context_ptr: Option<ID3D11DeviceContext> = None;
            device.GetImmediateContext(&mut context_ptr);
            let base_context = context_ptr.ok_or("GetImmediateContext returned None")?;

            let video_context: ID3D11VideoContext = base_context
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoContext: {}", e))?;

            // Video processor content description
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

            // NV12 output texture (D3D11_USAGE_DEFAULT, bind as render target)
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

            let nv12_texture = device
                .CreateTexture2D(&nv12_desc, None)
                .map_err(|e| format!("CreateTexture2D (NV12): {}", e))?;

            // Staging texture for CPU readback
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

            let staging_texture = device
                .CreateTexture2D(&staging_desc, None)
                .map_err(|e| format!("CreateTexture2D (staging): {}", e))?;

            // Output view on the NV12 texture
            let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };

            let output_view = video_device
                .CreateVideoProcessorOutputView(
                    &nv12_texture,
                    &enumerator,
                    &output_view_desc,
                )
                .map_err(|e| format!("CreateVideoProcessorOutputView: {}", e))?;

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
            // Input view for BGRA source texture
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

            let input_view = self
                .video_device
                .CreateVideoProcessorInputView(
                    bgra_texture,
                    &self.enumerator,
                    &input_view_desc,
                )
                .map_err(|e| format!("CreateVideoProcessorInputView: {}", e))?;

            // Build the stream descriptor
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };

            // Run video processor blit
            self.video_context
                .VideoProcessorBlt(
                    &self.processor,
                    &self.output_view,
                    0,
                    &[stream],
                )
                .map_err(|e| format!("VideoProcessorBlt: {}", e))?;

            // Copy NV12 → staging for CPU readback
            context.CopyResource(&self.staging_texture, &self.nv12_texture);

            // Map staging texture
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

            // Copy Y plane: h rows of w bytes each
            let y_size = w * h;
            let uv_size = w * h / 2;
            let mut nv12 = Vec::with_capacity(y_size + uv_size);

            for row in 0..h {
                let src_row = std::slice::from_raw_parts(data_ptr.add(row * row_pitch), w);
                nv12.extend_from_slice(src_row);
            }

            // Copy UV plane: starts at h * row_pitch in the mapped data
            // NV12 UV plane is h/2 rows of w bytes (interleaved U+V)
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

// ─── Capture Loop ─────────────────────────────────────────────────────────────

fn wgc_capture_loop(
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    item: GraphicsCaptureItem,
    video_proc: VideoProcessor,
    tx: SyncSender<RawFrame>,
    config: CaptureConfig,
    dst_w: u32,
    dst_h: u32,
) -> Result<(), String> {
    let _ = config; // fps is controlled by WGC frame pool

    unsafe {
        // Closed flag shared between handler and poll loop
        let closed_flag = Arc::new(AtomicBool::new(false));
        let closed_flag_handler = closed_flag.clone();

        // Register closed event handler
        let _closed_token = item
            .Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
                move |_, _| {
                    closed_flag_handler.store(true, Ordering::Relaxed);
                    Ok(())
                },
            ))
            .map_err(|e| format!("item.Closed: {}", e))?;

        // Get source size for frame pool
        let item_size = item.Size().map_err(|e| format!("item.Size: {}", e))?;

        // Create free-threaded frame pool (2 frames, BGRA8)
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            item_size,
        )
        .map_err(|e| format!("CreateFreeThreaded: {}", e))?;

        // Create capture session
        let session = frame_pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession: {}", e))?;

        // Try to enable cursor capture (IGraphicsCaptureSession2)
        if let Ok(s2) = session.cast::<IGraphicsCaptureSession2>() {
            let _ = s2.SetIsCursorCaptureEnabled(true);
        }

        // Try to disable yellow border (IGraphicsCaptureSession3)
        if let Ok(s3) = session.cast::<IGraphicsCaptureSession3>() {
            let _ = s3.SetIsBorderRequired(false);
        }

        // Start capturing
        session
            .StartCapture()
            .map_err(|e| format!("StartCapture: {}", e))?;

        eprintln!(
            "[capture-wgc] Capture started (source={}x{}, dst={}x{})",
            item_size.Width, item_size.Height, dst_w, dst_h
        );

        let start = Instant::now();
        let mut frame_count: u64 = 0;

        // Poll loop
        loop {
            // Check if the capture source was closed
            if closed_flag.load(Ordering::Relaxed) {
                eprintln!("[capture-wgc] Capture item closed, stopping");
                break;
            }

            // Try to get next frame — non-blocking
            let frame = match frame_pool.TryGetNextFrame() {
                Ok(f) => f,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
            };

            // Get the underlying D3D11 texture from the frame surface
            let surface = frame
                .Surface()
                .map_err(|e| format!("frame.Surface: {}", e))?;

            let dxgi_access: IDirect3DDxgiInterfaceAccess = surface
                .cast()
                .map_err(|e| format!("Cast to IDirect3DDxgiInterfaceAccess: {}", e))?;

            let bgra_texture: ID3D11Texture2D = dxgi_access
                .GetInterface()
                .map_err(|e| format!("GetInterface (ID3D11Texture2D): {}", e))?;

            frame_count += 1;
            if frame_count <= 3 || frame_count % 120 == 0 {
                eprintln!(
                    "[capture-wgc] Frame {} ({:.1}s)",
                    frame_count,
                    start.elapsed().as_secs_f64()
                );
            }

            // Convert BGRA → NV12 via D3D11 video processor
            let nv12 = match video_proc.convert_and_readback(&context, &bgra_texture) {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("[capture-wgc] convert_and_readback error: {}", e);
                    // Drop frame explicitly before continuing
                    drop(frame);
                    continue;
                }
            };

            let timestamp_us = start.elapsed().as_micros() as u64;

            // Drop WGC frame before sending to avoid holding GPU resources
            drop(frame);

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
                    eprintln!("[capture-wgc] Frame channel closed, stopping");
                    break;
                }
            }
        }

        // Cleanup
        let _ = session.Close();
        let _ = frame_pool.Close();

        eprintln!("[capture-wgc] Capture loop exited");
        Ok(())
    }
}

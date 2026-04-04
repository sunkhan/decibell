use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::time::Instant;

use super::capture::{CaptureConfig, CaptureOutput, CaptureSource, CaptureSourceType, RawFrame};

use windows::{
    core::{IInspectable, Interface, BOOL},
    Foundation::TypedEventHandler,
    Graphics::Capture::*,
    Graphics::DirectX::Direct3D11::IDirect3DDevice,
    Graphics::DirectX::DirectXPixelFormat,
    Win32::Foundation::*,
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Dxgi::*,
    Win32::Graphics::Dxgi::Common::*,
    Win32::Graphics::Gdi::*,
    Win32::Storage::Xps::*,
    Win32::System::WinRT::Direct3D11::*,
    Win32::System::WinRT::Graphics::Capture::*,
    Win32::UI::WindowsAndMessaging::*,
};
use base64::Engine;

// Wrapper to send COM pointers across threads.
// Safety: WGC capture objects are created on one thread and used on one thread;
// we only move them once (from spawn_blocking �� capture thread).
struct SendPtr<T> {
    inner: T,
}
impl<T> SendPtr<T> {
    fn new(val: T) -> Self { Self { inner: val } }
    fn into_inner(self) -> T { self.inner }
}
unsafe impl<T> Send for SendPtr<T> {}

// ─── Public API ──────────────────────────────────────────────────────────────

/// List available window capture sources (monitors handled by DXGI DD).
pub fn list_window_sources() -> Result<Vec<CaptureSource>, String> {
    let mut sources = Vec::new();
    enumerate_windows(&mut sources);
    Ok(sources)
}

/// Start capturing from the given source. Returns a CaptureOutput with receiver and dimensions.
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<CaptureOutput, String> {
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
        let item_size = item.Size().map_err(|e| format!("item.Size: {}", e))?;
        let src_w = item_size.Width as u32;
        let src_h = item_size.Height as u32;

        let dst_w = if config.target_width == 0 { src_w } else { config.target_width };
        let dst_h = if config.target_height == 0 { src_h } else { config.target_height };

        // Video processor for BGRA → NV12 conversion + scale
        let video_proc = VideoProcessor::new(&device, src_w, src_h, dst_w, dst_h)?;

        // Wrap COM objects for Send — they are created here and moved to one thread
        let device = SendPtr::new(device);
        let context = SendPtr::new(context);
        let winrt_device = SendPtr::new(winrt_device);
        let item = SendPtr::new(item);
        let video_proc = SendPtr::new(video_proc);

        // Extract the HWND value from source_id so the capture loop can poll IsWindow().
        // Store as usize to avoid Send issues with raw pointers; reconstruct HWND in the loop.
        let capture_hwnd_val: Option<usize> = source_id.strip_prefix("window:")
            .and_then(|s| s.parse::<usize>().ok());

        // Spawn capture thread
        std::thread::Builder::new()
            .name("decibell-capture".to_string())
            .spawn(move || {
                if let Err(e) = wgc_capture_loop(
                    device.into_inner(), context.into_inner(),
                    winrt_device.into_inner(), item.into_inner(),
                    video_proc.into_inner(), tx, config, dst_w, dst_h,
                    capture_hwnd_val,
                ) {
                    eprintln!("[capture-wgc] Capture loop error: {}", e);
                }
            })
            .map_err(|e| format!("Spawn capture thread: {}", e))?;

        Ok(CaptureOutput { receiver: rx, width: dst_w, height: dst_h })
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}

// ─── Source Enumeration ───────────────────────────────────────────────────────

/// Encode raw BGRA pixels as a 32-bit BMP and return a base64 data URI.
fn bgra_to_bmp_data_uri(width: u32, height: u32, bgra_pixels: &[u8]) -> String {
    let pixel_bytes = (width * height * 4) as u32;
    let file_size = 54 + pixel_bytes;
    let mut bmp = Vec::with_capacity(file_size as usize);

    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&file_size.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());

    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&width.to_le_bytes());
    bmp.extend_from_slice(&height.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 4]);
    bmp.extend_from_slice(&pixel_bytes.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 16]);

    bmp.extend_from_slice(bgra_pixels);

    format!(
        "data:image/bmp;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bmp)
    )
}

/// Capture a thumbnail of a window via PrintWindow (avoids occlusion artifacts).
fn capture_window_thumbnail(hwnd: HWND, width: u32, height: u32) -> Option<String> {
    const MAX_THUMB_W: u32 = 240;
    let (tw, th) = if width > MAX_THUMB_W {
        let scale = MAX_THUMB_W as f64 / width as f64;
        (MAX_THUMB_W, (height as f64 * scale) as u32)
    } else {
        (width, height)
    };
    if tw == 0 || th == 0 { return None; }

    unsafe {
        let screen_dc = GetDC(None);
        if screen_dc.is_invalid() { return None; }

        // Full-size bitmap for PrintWindow (renders at native window size)
        let full_dc = CreateCompatibleDC(Some(screen_dc));
        if full_dc.is_invalid() {
            ReleaseDC(None, screen_dc);
            return None;
        }
        let full_bmp = CreateCompatibleBitmap(screen_dc, width as i32, height as i32);
        if full_bmp.is_invalid() {
            let _ = DeleteDC(full_dc);
            ReleaseDC(None, screen_dc);
            return None;
        }
        let old_full = SelectObject(full_dc, full_bmp.into());

        // PW_RENDERFULLCONTENT (0x2) captures DWM-composited content correctly
        let ok = PrintWindow(hwnd, full_dc, PRINT_WINDOW_FLAGS(2));
        if !ok.as_bool() {
            // Fallback without the flag (older windows)
            if !PrintWindow(hwnd, full_dc, PRINT_WINDOW_FLAGS(0)).as_bool() {
                SelectObject(full_dc, old_full);
                let _ = DeleteObject(full_bmp.into());
                let _ = DeleteDC(full_dc);
                ReleaseDC(None, screen_dc);
                return None;
            }
        }

        // Scale down to thumbnail size
        let thumb_dc = CreateCompatibleDC(Some(screen_dc));
        if thumb_dc.is_invalid() {
            SelectObject(full_dc, old_full);
            let _ = DeleteObject(full_bmp.into());
            let _ = DeleteDC(full_dc);
            ReleaseDC(None, screen_dc);
            return None;
        }
        let thumb_bmp = CreateCompatibleBitmap(screen_dc, tw as i32, th as i32);
        if thumb_bmp.is_invalid() {
            let _ = DeleteDC(thumb_dc);
            SelectObject(full_dc, old_full);
            let _ = DeleteObject(full_bmp.into());
            let _ = DeleteDC(full_dc);
            ReleaseDC(None, screen_dc);
            return None;
        }
        let old_thumb = SelectObject(thumb_dc, thumb_bmp.into());

        let _ = SetStretchBltMode(thumb_dc, HALFTONE);
        let _ = StretchBlt(
            thumb_dc, 0, 0, tw as i32, th as i32,
            Some(full_dc), 0, 0, width as i32, height as i32,
            SRCCOPY,
        );

        // Read back thumbnail pixels
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
            thumb_dc, thumb_bmp, 0, th,
            Some(pixels.as_mut_ptr() as _),
            &mut bi, DIB_RGB_COLORS,
        );

        // Cleanup
        SelectObject(thumb_dc, old_thumb);
        let _ = DeleteObject(thumb_bmp.into());
        let _ = DeleteDC(thumb_dc);
        SelectObject(full_dc, old_full);
        let _ = DeleteObject(full_bmp.into());
        let _ = DeleteDC(full_dc);
        ReleaseDC(None, screen_dc);

        Some(bgra_to_bmp_data_uri(tw, th, &pixels))
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

            let thumbnail = capture_window_thumbnail(hwnd, width, height);

            sources.push(CaptureSource {
                id: format!("window:{}", hwnd.0 as usize),
                name: title,
                source_type: CaptureSourceType::Window,
                width,
                height,
                thumbnail,
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

    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;

    // Must NOT be a tool window
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
        return TRUE;
    }

    // Accept windows with WS_CAPTION (normal apps) OR WS_POPUP (fullscreen/borderless games).
    // Many older games (DX9 era) use WS_POPUP without WS_CAPTION.
    let has_caption = style & WS_CAPTION.0 == WS_CAPTION.0;
    let has_popup = style & WS_POPUP.0 != 0;
    if !has_caption && !has_popup {
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
    if let Some(rest) = source_id.strip_prefix("window:") {
        create_capture_item_for_window(rest)
    } else if let Some(rest) = source_id.strip_prefix("monitor:") {
        create_capture_item_for_monitor(rest)
    } else {
        Err(format!("WGC capture: unsupported source: {}", source_id))
    }
}

fn create_capture_item_for_monitor(id: &str) -> Result<GraphicsCaptureItem, String> {
    // id format: "adapter_idx:output_idx"
    let mut parts = id.splitn(2, ':');
    let adapter_idx: u32 = parts.next().and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("Invalid adapter index in monitor id: {}", id))?;
    let output_idx: u32 = parts.next().and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("Invalid output index in monitor id: {}", id))?;

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapters1(adapter_idx)
            .map_err(|e| format!("EnumAdapters1: {}", e))?;
        let output: IDXGIOutput = adapter.EnumOutputs(output_idx)
            .map_err(|e| format!("EnumOutputs: {}", e))?;
        let desc = output.GetDesc().map_err(|e| format!("GetDesc: {}", e))?;
        let hmonitor = desc.Monitor;

        let interop: IGraphicsCaptureItemInterop =
            windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
                .map_err(|e| format!("IGraphicsCaptureItemInterop factory: {}", e))?;

        interop
            .CreateForMonitor::<GraphicsCaptureItem>(hmonitor)
            .map_err(|e| format!("CreateForMonitor: {}", e))
    }
}

fn create_capture_item_for_window(id: &str) -> Result<GraphicsCaptureItem, String> {
    let hwnd_val: usize = id.parse().map_err(|_| format!("Invalid HWND: {}", id))?;
    let hwnd = HWND(hwnd_val as *mut _);

    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return Err(format!("HWND {} is no longer valid", hwnd_val));
        }
    }

    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("IGraphicsCaptureItemInterop factory: {}", e))?;

    unsafe {
        interop
            .CreateForWindow::<GraphicsCaptureItem>(hwnd)
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

// Safety: VideoProcessor is created and used on a single capture thread
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
            // Cast to video device and context
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoDevice: {}", e))?;

            let base_context = device
                .GetImmediateContext()
                .map_err(|e| format!("GetImmediateContext: {}", e))?;

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

            let mut nv12_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))
                .map_err(|e| format!("CreateTexture2D (NV12): {}", e))?;
            let nv12_texture = nv12_texture.ok_or("CreateTexture2D (NV12) returned None")?;

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

            let mut staging_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))
                .map_err(|e| format!("CreateTexture2D (staging): {}", e))?;
            let staging_texture = staging_texture.ok_or("CreateTexture2D (staging) returned None")?;

            // Output view on the NV12 texture
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

            // Build the stream descriptor
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

            // Run video processor blit
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
    mut video_proc: VideoProcessor,
    tx: SyncSender<RawFrame>,
    config: CaptureConfig,
    dst_w: u32,
    dst_h: u32,
    capture_hwnd_val: Option<usize>,
) -> Result<(), String> {
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
        let mut current_src_w = item_size.Width as u32;
        let mut current_src_h = item_size.Height as u32;

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
        let _ = session.SetIsCursorCaptureEnabled(true);

        // Try to disable yellow border (IGraphicsCaptureSession3)
        let _ = session.SetIsBorderRequired(false);

        // Start capturing
        session
            .StartCapture()
            .map_err(|e| format!("StartCapture: {}", e))?;

        eprintln!(
            "[capture-wgc] Capture started (source={}x{}, dst={}x{})",
            item_size.Width, item_size.Height, dst_w, dst_h
        );

        let frame_interval = std::time::Duration::from_micros(1_000_000 / config.target_fps as u64);
        let start = Instant::now();
        let mut frame_count: u64 = 0;
        let mut last_nv12: Option<Vec<u8>> = None;

        // Poll loop
        loop {
            let loop_start = Instant::now();

            // Check if the capture source was closed (WinRT event)
            if closed_flag.load(Ordering::Relaxed) {
                eprintln!("[capture-wgc] Capture item closed (event), stopping");
                break;
            }

            // Actively check if the window handle is still valid (reliable fallback
            // for when the WinRT Closed event doesn't fire due to COM threading)
            if let Some(val) = capture_hwnd_val {
                if !IsWindow(Some(HWND(val as *mut _))).as_bool() {
                    eprintln!("[capture-wgc] Window destroyed (IsWindow), stopping");
                    break;
                }
            }

            // Try to get next frame — non-blocking
            let got_new_frame = match frame_pool.TryGetNextFrame() {
                Ok(frame) => {
                    // Detect resize via ContentSize
                    let content_size = frame.ContentSize()
                        .map_err(|e| format!("ContentSize: {}", e))?;
                    let new_w = content_size.Width as u32;
                    let new_h = content_size.Height as u32;

                    if new_w > 0 && new_h > 0 && (new_w != current_src_w || new_h != current_src_h) {
                        eprintln!(
                            "[capture-wgc] Resize detected: {}x{} → {}x{}",
                            current_src_w, current_src_h, new_w, new_h
                        );
                        current_src_w = new_w;
                        current_src_h = new_h;

                        // Recreate frame pool with new size
                        let new_size = windows::Graphics::SizeInt32 {
                            Width: new_w as i32,
                            Height: new_h as i32,
                        };
                        frame_pool.Recreate(
                            &winrt_device,
                            DirectXPixelFormat::B8G8R8A8UIntNormalized,
                            2,
                            new_size,
                        ).map_err(|e| format!("Recreate frame pool: {}", e))?;

                        // Recreate video processor for new source dimensions
                        video_proc = VideoProcessor::new(&device, new_w, new_h, dst_w, dst_h)?;

                        // Invalidate cached frame (old dimensions)
                        last_nv12 = None;

                        // Drop this frame (from old pool size), next iteration gets correct one
                        drop(frame);
                        false
                    } else {
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

                        // Convert BGRA → NV12 via D3D11 video processor
                        match video_proc.convert_and_readback(&context, &bgra_texture) {
                            Ok(data) => {
                                last_nv12 = Some(data);
                                drop(frame);
                                true
                            }
                            Err(e) => {
                                eprintln!("[capture-wgc] convert error: {}", e);
                                drop(frame);
                                false
                            }
                        }
                    }
                }
                Err(_) => false, // No new frame available
            };

            // Send frame (new or cached) to keep encoder continuously fed
            if let Some(ref nv12_data) = last_nv12 {
                frame_count += 1;
                if frame_count <= 3 || frame_count % 300 == 0 {
                    eprintln!(
                        "[capture-wgc] Frame {} ({:.1}s, src={}x{})",
                        frame_count, start.elapsed().as_secs_f64(),
                        current_src_w, current_src_h,
                    );
                }

                let raw_frame = RawFrame {
                    data: nv12_data.clone(),
                    width: dst_w,
                    height: dst_h,
                    stride: dst_w as usize,
                    pixel_format: super::capture::PixelFormat::NV12,
                    timestamp_us: start.elapsed().as_micros() as u64,
                };

                match tx.try_send(raw_frame) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        eprintln!("[capture-wgc] Channel closed, stopping");
                        break;
                    }
                }
            }

            // Frame pacing: sleep for remainder of frame interval
            let elapsed = loop_start.elapsed();
            if elapsed < frame_interval {
                std::thread::sleep(frame_interval - elapsed);
            }
        }

        // Cleanup
        let _ = session.Close();
        let _ = frame_pool.Close();

        eprintln!("[capture-wgc] Capture loop exited");
        Ok(())
    }
}

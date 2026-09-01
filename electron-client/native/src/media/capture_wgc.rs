//! Windows Graphics Capture source.
//!
//! Opens a capture session on either an HMONITOR (full screen) or an
//! HWND (single window), runs a TryGetNextFrame poll loop, and pushes
//! BGRA D3D11 textures into a bounded mpsc::SyncSender for the encoder
//! thread, paced to the stream's target fps (a held "latest" frame is
//! copied into a small texture ring on each deadline; static content
//! re-sends the last slot so the encoder keeps its cadence). Yellow
//! border disabled where the OS allows. Cursor capture enabled.
//!
//! Mining `tauri-client/src-tauri/src/media/capture_wgc.rs` (969 LOC);
//! we keep just the pool + poll path. The resize-handling that Tauri
//! had is deferred — for first ship, monitor/window resolution changes
//! force a stream restart (uncommon for screen-share use cases).

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Foundation::TimeSpan;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows_core::BOOL;

use super::gpu_pipeline::GpuDevice;
use super::source_id::CaptureTarget;

pub struct Capture {
    stop: Arc<AtomicBool>,
    frames_dropped: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(
        gpu: &GpuDevice,
        target: CaptureTarget,
        tx: mpsc::SyncSender<ID3D11Texture2D>,
        include_cursor: bool,
        fps: u32,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let frames_dropped = Arc::new(AtomicU32::new(0));
        let device = gpu.device.clone();
        let context = gpu.context.clone();
        let stop_t = stop.clone();
        let drops_t = frames_dropped.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-wgc-capture".to_string())
            .spawn(move || {
                if let Err(e) = run_capture_thread(
                    &device, &context, target, tx, stop_t, drops_t, include_cursor, fps,
                ) {
                    log::error!("[capture_wgc] thread error: {e}");
                }
            })
            .map_err(|e| format!("spawn capture thread: {e}"))?;
        Ok(Self {
            stop,
            frames_dropped,
            thread: Some(thread),
        })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn frames_dropped(&self) -> u32 {
        self.frames_dropped.load(Ordering::Relaxed)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_capture_thread(
    d3d11_device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    target: CaptureTarget,
    tx: mpsc::SyncSender<ID3D11Texture2D>,
    stop: Arc<AtomicBool>,
    drops: Arc<AtomicU32>,
    include_cursor: bool,
    fps: u32,
) -> Result<(), String> {
    let item = open_capture_item(target)?;
    let winrt_device = winrt_device_from_d3d11(d3d11_device)?;

    let item_size = item.Size().map_err(|e| format!("item.Size: {e:?}"))?;
    log::info!(
        "[capture_wgc] item size = {}x{}",
        item_size.Width, item_size.Height
    );

    // 3 buffers: the pacer holds the latest frame between polls, so
    // the compositor still has two to alternate between.
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        3,
        item_size,
    )
    .map_err(|e| format!("CreateFreeThreaded: {e:?}"))?;

    let session = pool
        .CreateCaptureSession(&item)
        .map_err(|e| format!("CreateCaptureSession: {e:?}"))?;

    // Yellow border off. Cursor per the user's toggle. Both are best-
    // effort — older Win10 builds don't expose IGraphicsCaptureSession3 so
    // SetIsBorderRequired returns Err there and we keep the border
    // (acceptable degradation).
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(include_cursor);
    // Win11 24H2+: ask the compositor not to produce frames faster than
    // the target — saves the composition work up front. Best-effort;
    // the pacer below is what guarantees the rate everywhere else.
    // TimeSpan is in 100 ns units.
    let _ = session.SetMinUpdateInterval(TimeSpan {
        Duration: (10_000_000u64 / fps.max(1).min(240) as u64) as i64,
    });

    // Own texture ring. Pool textures are only stable while their frame
    // is held (the pool recycles the buffer once the frame is closed),
    // so each send copies the held frame into a slot of ours; that also
    // makes "re-send the last frame" for static content safe.
    let ring_desc = D3D11_TEXTURE2D_DESC {
        Width: item_size.Width.max(1) as u32,
        Height: item_size.Height.max(1) as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut ring: Vec<ID3D11Texture2D> = Vec::with_capacity(4);
    for _ in 0..4 {
        let mut t: Option<ID3D11Texture2D> = None;
        unsafe { d3d11_device.CreateTexture2D(&ring_desc, None, Some(&mut t)) }
            .map_err(|e| format!("CreateTexture2D (ring): {e:?}"))?;
        ring.push(t.ok_or("CreateTexture2D (ring) returned None")?);
    }

    session
        .StartCapture()
        .map_err(|e| format!("StartCapture: {e:?}"))?;

    log::info!("[capture_wgc] StartCapture OK; entering poll loop ({fps} fps)");

    // Pacing: one send per frame interval from the newest frame the
    // compositor has delivered. Without this the encoder received every
    // pool frame — a 144 Hz monitor drove ~144 copies + encodes/s into a
    // 60 fps stream. Same deadline pacer as capture_dxgi.
    let frame_interval = Duration::from_micros(1_000_000 / fps.max(1).min(240) as u64);
    let mut next_due = Instant::now();
    let mut latest: Option<(Direct3D11CaptureFrame, ID3D11Texture2D)> = None;
    let mut fresh = false;
    let mut ring_idx = 0usize;
    let mut last_slot: Option<usize> = None;

    while !stop.load(Ordering::Relaxed) {
        match pool.TryGetNextFrame() {
            Ok(frame) => {
                let texture = frame_texture(&frame)?;
                if let Some((old, _)) = latest.replace((frame, texture)) {
                    let _ = old.Close();
                }
                fresh = true;
            }
            Err(_) => {
                // Nothing new: nap until the deadline (≤ 2 ms polls).
                let remaining = next_due.saturating_duration_since(Instant::now());
                std::thread::sleep(remaining.min(Duration::from_millis(2)));
            }
        }

        let now = Instant::now();
        if now >= next_due {
            if fresh {
                if let Some((_, tex)) = latest.as_ref() {
                    let slot = ring_idx % ring.len();
                    ring_idx += 1;
                    unsafe { context.CopyResource(&ring[slot], tex) };
                    last_slot = Some(slot);
                }
                fresh = false;
            }
            if let Some(slot) = last_slot {
                match tx.try_send(ring[slot].clone()) {
                    Ok(_) => {}
                    Err(mpsc::TrySendError::Full(_)) => {
                        drops.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => break,
                }
                // Fixed cadence; re-anchor after a stall so overdue
                // slots don't fire back-to-back.
                next_due = if now > next_due + frame_interval * 4 {
                    now + frame_interval
                } else {
                    next_due + frame_interval
                };
            }
        }
    }

    // Teardown.
    if let Some((frame, _)) = latest.take() {
        let _ = frame.Close();
    }
    let _ = session.Close();
    let _ = pool.Close();
    Ok(())
}

/// The D3D11 texture behind a pool frame (same device as the pool).
fn frame_texture(frame: &Direct3D11CaptureFrame) -> Result<ID3D11Texture2D, String> {
    let surface = frame
        .Surface()
        .map_err(|e| format!("frame.Surface: {e:?}"))?;
    let access: IDirect3DDxgiInterfaceAccess = surface
        .cast()
        .map_err(|e| format!("cast IDirect3DDxgiInterfaceAccess: {e:?}"))?;
    unsafe { access.GetInterface() }
        .map_err(|e| format!("GetInterface ID3D11Texture2D: {e:?}"))
}

fn open_capture_item(target: CaptureTarget) -> Result<GraphicsCaptureItem, String> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("IGraphicsCaptureItemInterop factory: {e:?}"))?;
    match target {
        CaptureTarget::Monitor(idx) => {
            let hmon = monitor_at_index(idx)?;
            unsafe { interop.CreateForMonitor::<GraphicsCaptureItem>(hmon) }
                .map_err(|e| format!("CreateForMonitor: {e:?}"))
        }
        CaptureTarget::Window(hwnd) => {
            let hwnd = HWND(hwnd as *mut _);
            unsafe { interop.CreateForWindow::<GraphicsCaptureItem>(hwnd) }
                .map_err(|e| format!("CreateForWindow: {e:?}"))
        }
    }
}

/// True when this Windows build exposes
/// `GraphicsCaptureSession.IsBorderRequired` (Win11 / Server 2022+) —
/// i.e. WGC itself can capture without the yellow border, and monitor
/// capture should stay on WGC (native cursor compositing, HDR and
/// rotation handling) instead of the DXGI duplication path that exists
/// for border-less capture on Windows 10. Queries the WinRT metadata
/// for the property rather than sniffing OS build numbers.
pub(crate) fn borderless_supported() -> bool {
    use windows::core::HSTRING;
    use windows::Foundation::Metadata::ApiInformation;
    ApiInformation::IsPropertyPresent(
        &HSTRING::from("Windows.Graphics.Capture.GraphicsCaptureSession"),
        &HSTRING::from("IsBorderRequired"),
    )
    .unwrap_or(false)
}

// Shared with capture_dxgi so both backends resolve Chromium's
// `screen:N:0` index to the same monitor.
pub(crate) fn monitor_at_index(idx: u32) -> Result<HMONITOR, String> {
    // EnumDisplayMonitors callback collects HMONITORs in left-to-right
    // top-to-bottom order. Chromium's desktopCapturer numbers them in
    // the same order.
    let mut monitors: Vec<HMONITOR> = Vec::new();
    unsafe extern "system" fn cb(
        hmon: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        list.push(hmon);
        BOOL(1)
    }
    let lparam = LPARAM(&mut monitors as *mut _ as isize);
    unsafe {
        EnumDisplayMonitors(None, None, Some(cb), lparam);
    }
    monitors
        .get(idx as usize)
        .copied()
        .ok_or_else(|| format!("monitor index {idx} out of range (have {})", monitors.len()))
}

fn winrt_device_from_d3d11(
    d3d11: &ID3D11Device,
) -> Result<windows::Graphics::DirectX::Direct3D11::IDirect3DDevice, String> {
    let dxgi: IDXGIDevice = d3d11
        .cast()
        .map_err(|e| format!("cast to IDXGIDevice: {e:?}"))?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e:?}"))?;
    inspectable
        .cast()
        .map_err(|e| format!("cast inspectable to IDirect3DDevice: {e:?}"))
}

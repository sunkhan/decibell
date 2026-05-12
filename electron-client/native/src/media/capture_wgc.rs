//! Windows Graphics Capture source.
//!
//! Opens a capture session on either an HMONITOR (full screen) or an
//! HWND (single window), runs a TryGetNextFrame poll loop, and pushes
//! BGRA D3D11 textures into a bounded mpsc::SyncSender for the encoder
//! thread. Yellow border disabled. Cursor capture enabled.
//!
//! Mining `tauri-client/src-tauri/src/media/capture_wgc.rs` (969 LOC);
//! we keep just the pool + poll path. The resize-handling that Tauri
//! had is deferred — for first ship, monitor/window resolution changes
//! force a stream restart (uncommon for screen-share use cases).

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use windows::core::Interface;
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
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
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let frames_dropped = Arc::new(AtomicU32::new(0));
        let device = gpu.device.clone();
        let stop_t = stop.clone();
        let drops_t = frames_dropped.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-wgc-capture".to_string())
            .spawn(move || {
                if let Err(e) = run_capture_thread(&device, target, tx, stop_t, drops_t) {
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

fn run_capture_thread(
    d3d11_device: &ID3D11Device,
    target: CaptureTarget,
    tx: mpsc::SyncSender<ID3D11Texture2D>,
    stop: Arc<AtomicBool>,
    drops: Arc<AtomicU32>,
) -> Result<(), String> {
    let item = open_capture_item(target)?;
    let winrt_device = winrt_device_from_d3d11(d3d11_device)?;

    let item_size = item.Size().map_err(|e| format!("item.Size: {e:?}"))?;
    eprintln!(
        "[capture_wgc] item size = {}x{}",
        item_size.Width, item_size.Height
    );

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        item_size,
    )
    .map_err(|e| format!("CreateFreeThreaded: {e:?}"))?;

    let session = pool
        .CreateCaptureSession(&item)
        .map_err(|e| format!("CreateCaptureSession: {e:?}"))?;

    // Yellow border off. Cursor on. Both are best-effort — older Win10
    // builds don't expose IGraphicsCaptureSession3 so SetIsBorderRequired
    // returns Err there and we keep the border (acceptable degradation).
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(true);

    session
        .StartCapture()
        .map_err(|e| format!("StartCapture: {e:?}"))?;

    eprintln!("[capture_wgc] StartCapture OK; entering poll loop");

    while !stop.load(Ordering::Relaxed) {
        let frame = match pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
        };
        let surface = frame
            .Surface()
            .map_err(|e| format!("frame.Surface: {e:?}"))?;
        let access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| format!("cast IDirect3DDxgiInterfaceAccess: {e:?}"))?;
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }
            .map_err(|e| format!("GetInterface ID3D11Texture2D: {e:?}"))?;
        match tx.try_send(texture) {
            Ok(_) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                drops.fetch_add(1, Ordering::Relaxed);
                // Drop oldest by receiving one (if we could), then this
                // frame is dropped on the floor. Simpler than re-sending.
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break,
        }
    }

    // Teardown.
    let _ = session.Close();
    let _ = pool.Close();
    Ok(())
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

fn monitor_at_index(idx: u32) -> Result<HMONITOR, String> {
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

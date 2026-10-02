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
//! Lifecycle:
//! - `start` blocks (≤ READY_TIMEOUT) until the capture thread has the
//!   item, pool and session up and StartCapture succeeded, and returns
//!   the setup error otherwise — so a closed/minimized window or a bad
//!   monitor id fails `start_windows` and the renderer falls back to its
//!   own encoder instead of announcing a stream that never produces.
//! - No frame within FIRST_FRAME_TIMEOUT of StartCapture, the source
//!   going away (GraphicsCaptureItem.Closed, or IsWindow turning false
//!   for a window), or a TryGetNextFrame error ends the thread; dropping
//!   the frame sender reports the failure through the encoder thread.
//! - Size changes (window resize, game resolution switch, rotation, DPI
//!   change) are followed: when a frame's ContentSize differs from the
//!   pool's, the pool is Recreated at the new size and the ring rebuilt
//!   to the valid content region. Downstream textures may therefore
//!   change size mid-stream; the video processor follows the input.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::{IInspectable, Interface, PCWSTR};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureItem,
    GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW,
    DISPLAY_DEVICE_ACTIVE, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::WinRT::Direct3D11::{
    CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess,
};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindow};
use windows_core::BOOL;

use super::gpu_pipeline::GpuDevice;
use super::source_id::{wide_name_eq, CaptureTarget};

/// How long `start` waits for the thread's setup verdict.
const READY_TIMEOUT: Duration = Duration::from_secs(3);
/// No frame at all this long after StartCapture → give up.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// IsWindow poll period for window targets.
const SOURCE_CHECK_INTERVAL: Duration = Duration::from_millis(250);
/// Longest nap between polls (also the nap when nothing is due).
const MAX_NAP: Duration = Duration::from_millis(2);
/// 3 buffers: the pacer holds the latest frame between polls, so the
/// compositor still has two to alternate between.
const POOL_BUFFERS: i32 = 3;
/// 4 slots + the depth-2 channel keep a queued texture from being
/// rewritten while the encoder still reads it.
const RING_SLOTS: usize = 4;
const POOL_FORMAT: DirectXPixelFormat = DirectXPixelFormat::B8G8R8A8UIntNormalized;

pub struct Capture {
    stop: Arc<AtomicBool>,
    frames_dropped: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Start capturing `target`. Returns once the capture session is
    /// running, or the reason it couldn't start (bounded by
    /// READY_TIMEOUT).
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

        // The thread reports whether the session came up (or the exact
        // setup failure), mirroring capture_dxgi.
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let thread = std::thread::Builder::new()
            .name("decibell-wgc-capture".to_string())
            .spawn(move || {
                // catch_unwind: a panic here must end the capture (the
                // dropped sender reports it), not take the process down
                // silently mid-unwind through COM.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_capture_thread(
                        &device,
                        &context,
                        target,
                        tx,
                        stop_t,
                        drops_t,
                        include_cursor,
                        fps,
                        &ready_tx,
                    )
                }));
                let err = match outcome {
                    Ok(Ok(())) => None,
                    Ok(Err(e)) => Some(e),
                    Err(_) => Some("capture thread panicked".to_string()),
                };
                if let Some(e) = err {
                    log::error!("[capture_wgc] thread error: {e}");
                    // No-op once setup already reported success.
                    let _ = ready_tx.try_send(Err(e));
                }
            })
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match ready_rx.recv_timeout(READY_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                stop,
                frames_dropped,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                // Setup wedged — tell the thread to stop and leave it
                // detached rather than blocking the caller.
                stop.store(true, Ordering::Relaxed);
                Err("window capture setup timed out".to_string())
            }
        }
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

/// Our own texture ring. Pool textures are only stable while their frame
/// is held (the pool recycles the buffer once the frame is closed), so
/// each send copies the held frame into a slot of ours; that also makes
/// "re-send the last frame" for static content safe.
struct Ring {
    slots: Vec<ID3D11Texture2D>,
    width: u32,
    height: u32,
    next: usize,
}

impl Ring {
    fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width.max(1),
            Height: height.max(1),
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut slots = Vec::with_capacity(RING_SLOTS);
        for _ in 0..RING_SLOTS {
            let mut t: Option<ID3D11Texture2D> = None;
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }
                .map_err(|e| format!("CreateTexture2D (ring {width}x{height}): {e:?}"))?;
            slots.push(t.ok_or("CreateTexture2D (ring) returned None")?);
        }
        Ok(Self {
            slots,
            width,
            height,
            next: 0,
        })
    }
}

/// A pool frame we're holding on to, with the valid content region of
/// its surface. Dropping it closes the frame (returns the buffer).
struct HeldFrame {
    frame: Direct3D11CaptureFrame,
    texture: ID3D11Texture2D,
    tex_w: u32,
    tex_h: u32,
    valid_w: u32,
    valid_h: u32,
}

impl Drop for HeldFrame {
    fn drop(&mut self) {
        let _ = self.frame.Close();
    }
}

/// Closes the session/pool and unhooks the Closed handler however the
/// loop exits (including `?`).
struct SessionGuard {
    item: GraphicsCaptureItem,
    session: GraphicsCaptureSession,
    pool: Direct3D11CaptureFramePool,
    closed_token: Option<i64>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        if let Some(token) = self.closed_token.take() {
            let _ = self.item.RemoveClosed(token);
        }
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// Copy the held frame's valid region into the next ring slot, rebuilding
/// the ring first when that region's size changed. Returns the slot.
fn copy_into_ring(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    ring: &mut Option<Ring>,
    held: &HeldFrame,
) -> Result<usize, String> {
    let fits = matches!(ring.as_ref(), Some(r) if r.width == held.valid_w && r.height == held.valid_h);
    if !fits {
        log::info!(
            "[capture_wgc] ring → {}x{}",
            held.valid_w, held.valid_h
        );
        *ring = None;
        *ring = Some(Ring::new(device, held.valid_w, held.valid_h)?);
    }
    let Some(r) = ring.as_mut() else {
        return Err("ring missing".to_string());
    };
    let slot = r.next % r.slots.len();
    r.next = r.next.wrapping_add(1);
    let Some(dst) = r.slots.get(slot) else {
        return Err("ring slot out of range".to_string());
    };
    if held.tex_w == held.valid_w && held.tex_h == held.valid_h {
        unsafe { context.CopyResource(dst, &held.texture) };
    } else {
        // Content smaller than the surface (the item shrank and the pool
        // hasn't been recreated yet): copy just the valid top-left region.
        let src_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: held.valid_w,
            bottom: held.valid_h,
            back: 1,
        };
        unsafe {
            context.CopySubresourceRegion(dst, 0, 0, 0, 0, &held.texture, 0, Some(&src_box))
        };
    }
    Ok(slot)
}

fn texture_size(tex: &ID3D11Texture2D) -> (u32, u32) {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { tex.GetDesc(&mut desc) };
    (desc.Width, desc.Height)
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
    ready_tx: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let hwnd = match target {
        CaptureTarget::Window(h) => Some(HWND(h as *mut _)),
        CaptureTarget::Monitor(_) => None,
    };
    if let Some(h) = hwnd {
        if !unsafe { IsWindow(Some(h)) }.as_bool() {
            return Err("window no longer exists".to_string());
        }
        // A minimized window produces no frames until restored; fail the
        // native start so the renderer path (which tolerates that) takes it.
        if unsafe { IsIconic(h) }.as_bool() {
            return Err("window is minimized".to_string());
        }
    }

    let item = open_capture_item(&target)?;
    let winrt_device = winrt_device_from_d3d11(d3d11_device)?;

    let item_size = item.Size().map_err(|e| format!("item.Size: {e:?}"))?;
    log::info!(
        "[capture_wgc] item size = {}x{}",
        item_size.Width, item_size.Height
    );
    if item_size.Width <= 0 || item_size.Height <= 0 {
        return Err(format!(
            "capture item has no area ({}x{})",
            item_size.Width, item_size.Height
        ));
    }

    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        POOL_FORMAT,
        POOL_BUFFERS,
        item_size,
    )
    .map_err(|e| format!("CreateFreeThreaded: {e:?}"))?;

    let session = pool
        .CreateCaptureSession(&item)
        .map_err(|e| format!("CreateCaptureSession: {e:?}"))?;

    // Source end: the item's Closed event (window closed, monitor
    // unplugged). The handler runs on a WinRT thread and only flips a
    // flag — nothing in it can panic (a panic in a COM callback aborts).
    let source_closed = Arc::new(AtomicBool::new(false));
    let closed_flag = source_closed.clone();
    let closed_handler = TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(
        move |_, _| {
            closed_flag.store(true, Ordering::Relaxed);
            Ok(())
        },
    );
    let closed_token = match item.Closed(&closed_handler) {
        Ok(t) => Some(t),
        Err(e) => {
            log::warn!("[capture_wgc] item.Closed subscribe failed ({e:?}); relying on polling");
            None
        }
    };
    let guard = SessionGuard {
        item,
        session,
        pool,
        closed_token,
    };

    // Yellow border off. Cursor per the user's toggle. Both are best-
    // effort — older Win10 builds don't expose IGraphicsCaptureSession3 so
    // SetIsBorderRequired returns Err there and we keep the border
    // (acceptable degradation).
    let _ = guard.session.SetIsBorderRequired(false);
    let _ = guard.session.SetIsCursorCaptureEnabled(include_cursor);
    // Win11 24H2+: ask the compositor not to produce frames faster than
    // the target — saves the composition work up front. Best-effort;
    // the pacer below is what guarantees the rate everywhere else.
    // TimeSpan is in 100 ns units.
    let _ = guard.session.SetMinUpdateInterval(TimeSpan {
        Duration: (10_000_000u64 / fps.clamp(1, 240) as u64) as i64,
    });

    guard
        .session
        .StartCapture()
        .map_err(|e| format!("StartCapture: {e:?}"))?;

    log::info!("[capture_wgc] StartCapture OK; entering poll loop ({fps} fps)");
    let _ = ready_tx.try_send(Ok(()));

    poll_loop(
        d3d11_device,
        context,
        &guard.pool,
        &winrt_device,
        item_size,
        hwnd,
        &source_closed,
        &tx,
        &stop,
        &drops,
        fps,
    )
    // `guard` drops here: Closed unhooked, session + pool closed.
}

#[allow(clippy::too_many_arguments)]
fn poll_loop(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    pool: &Direct3D11CaptureFramePool,
    winrt_device: &IDirect3DDevice,
    initial_size: SizeInt32,
    hwnd: Option<HWND>,
    source_closed: &AtomicBool,
    tx: &mpsc::SyncSender<ID3D11Texture2D>,
    stop: &AtomicBool,
    drops: &AtomicU32,
    fps: u32,
) -> Result<(), String> {
    // Pacing: one send per frame interval from the newest frame the
    // compositor has delivered. Without this the encoder received every
    // pool frame — a 144 Hz monitor drove ~144 copies + encodes/s into a
    // 60 fps stream. Same deadline pacer as capture_dxgi.
    let frame_interval = Duration::from_micros(1_000_000 / fps.clamp(1, 240) as u64);
    let started = Instant::now();
    let mut next_due = started;
    let mut pool_size = initial_size;
    let mut ring: Option<Ring> = None;
    // Declared after `pool`'s owner, so it's closed before the pool is.
    let mut latest: Option<HeldFrame> = None;
    let mut fresh = false;
    let mut last_slot: Option<usize> = None;
    // Newest content size that differs from the pool's, applied after
    // the next copy.
    let mut pending_resize: Option<SizeInt32> = None;
    let mut got_frame = false;
    let mut last_source_check = started;

    while !stop.load(Ordering::Relaxed) {
        if source_closed.load(Ordering::Relaxed) {
            return Err("capture source closed".to_string());
        }
        if let Some(h) = hwnd {
            if last_source_check.elapsed() >= SOURCE_CHECK_INTERVAL {
                last_source_check = Instant::now();
                if !unsafe { IsWindow(Some(h)) }.as_bool() {
                    return Err("captured window was closed".to_string());
                }
            }
        }
        if !got_frame && started.elapsed() >= FIRST_FRAME_TIMEOUT {
            return Err(format!(
                "no frame within {}s of StartCapture",
                FIRST_FRAME_TIMEOUT.as_secs()
            ));
        }

        match pool.TryGetNextFrame() {
            Ok(frame) => {
                got_frame = true;
                let content = frame.ContentSize().unwrap_or(pool_size);
                if content.Width <= 0 || content.Height <= 0 {
                    // Nothing to show (e.g. minimizing): keep re-sending
                    // the last good frame.
                    let _ = frame.Close();
                } else {
                    let texture = match frame_texture(&frame) {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = frame.Close();
                            return Err(e);
                        }
                    };
                    let (tex_w, tex_h) = texture_size(&texture);
                    let held = HeldFrame {
                        frame,
                        texture,
                        tex_w,
                        tex_h,
                        valid_w: (content.Width as u32).min(tex_w),
                        valid_h: (content.Height as u32).min(tex_h),
                    };
                    if held.valid_w == 0 || held.valid_h == 0 {
                        drop(held);
                    } else {
                        // The source changed size (or changed back):
                        // this frame's buffer is still the pool's size.
                        // Resize the pool once the frame has been copied
                        // (at the next send), so a drag-resize costs one
                        // Recreate + ring rebuild per sent frame, not per
                        // composed one.
                        pending_resize = (content.Width != pool_size.Width
                            || content.Height != pool_size.Height)
                            .then_some(content);
                        // Replacing drops (closes) the older frame.
                        latest = Some(held);
                        fresh = true;
                    }
                }
            }
            Err(e) if e.code().is_ok() => {
                // No new frame. Nap until the deadline when there's
                // something to send, else a short fixed nap — a zero-
                // length sleep here spun a core for a source that never
                // produces (minimized window).
                let can_send = fresh || last_slot.is_some();
                let nap = if can_send {
                    next_due.saturating_duration_since(Instant::now()).min(MAX_NAP)
                } else {
                    MAX_NAP
                };
                if !nap.is_zero() {
                    std::thread::sleep(nap);
                }
            }
            Err(e) => {
                // Device lost, item gone, pool closed underneath us.
                return Err(format!("TryGetNextFrame: {e:?}"));
            }
        }

        let now = Instant::now();
        if now >= next_due {
            if fresh {
                if let Some(held) = latest.as_ref() {
                    last_slot = Some(copy_into_ring(device, context, &mut ring, held)?);
                }
                fresh = false;
            }
            if let Some(size) = pending_resize.take() {
                // After the copy: the held frame (old buffer size) has
                // been consumed; frames handed out before Recreate stay
                // valid until closed, and we never read it again.
                if size.Width != pool_size.Width || size.Height != pool_size.Height {
                    log::info!(
                        "[capture_wgc] content {}x{} → {}x{}; recreating pool",
                        pool_size.Width, pool_size.Height, size.Width, size.Height
                    );
                    if let Err(e) = pool.Recreate(winrt_device, POOL_FORMAT, POOL_BUFFERS, size) {
                        // Keep streaming the valid region rather than
                        // end; don't retry every frame.
                        log::warn!("[capture_wgc] pool.Recreate failed: {e:?}");
                    }
                    pool_size = size;
                }
            }
            let send = ring
                .as_ref()
                .and_then(|r| last_slot.and_then(|s| r.slots.get(s)))
                .cloned();
            if let Some(tex) = send {
                match tx.try_send(tex) {
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

    drop(latest);
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

fn open_capture_item(target: &CaptureTarget) -> Result<GraphicsCaptureItem, String> {
    let interop: IGraphicsCaptureItemInterop =
        windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("IGraphicsCaptureItemInterop factory: {e:?}"))?;
    match *target {
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

/// A Chromium `screen:N` id resolved to a monitor.
pub(crate) struct ResolvedMonitor {
    pub hmon: HMONITOR,
    /// GDI device name (`\\.\DISPLAYn`) when the id resolved through
    /// EnumDisplayDevicesW; None when it fell back to enumeration order.
    pub device_name: Option<[u16; 32]>,
}

// Shared with capture_dxgi so both backends resolve Chromium's
// `screen:N:0` id to the same monitor.
pub(crate) fn monitor_at_index(idx: u32) -> Result<HMONITOR, String> {
    resolve_monitor(idx).map(|m| m.hmon)
}

/// Resolve Chromium's `screen:N` the way WebRTC numbers screens
/// (screen_capture_utils.cc GetScreenList): N is the
/// `EnumDisplayDevicesW(NULL, N, ..)` device index of an ACTIVE display.
/// That device's name is matched against each monitor's
/// MONITORINFOEXW.szDevice. If the lookup finds nothing, falls back to
/// the old N-th-in-EnumDisplayMonitors-order behaviour (and says so).
pub(crate) fn resolve_monitor(idx: u32) -> Result<ResolvedMonitor, String> {
    let monitors = enumerate_monitors();
    let device = active_display_device_name(idx);
    if let Some(name) = device {
        if let Some((hmon, _)) = monitors.iter().find(|(_, dev)| wide_name_eq(dev, &name)) {
            return Ok(ResolvedMonitor {
                hmon: *hmon,
                device_name: Some(name),
            });
        }
        log::warn!(
            "[capture] screen:{idx} → {} matches no monitor; falling back to enumeration order",
            wide_to_string(&name)
        );
    } else {
        log::warn!(
            "[capture] screen:{idx} is not an active display device; falling back to enumeration order"
        );
    }
    monitors
        .get(idx as usize)
        .map(|(hmon, _)| ResolvedMonitor {
            hmon: *hmon,
            device_name: None,
        })
        .ok_or_else(|| format!("monitor index {idx} out of range (have {})", monitors.len()))
}

/// `EnumDisplayDevicesW(NULL, idx)`'s device name, if that device exists
/// and is active (part of the desktop).
fn active_display_device_name(idx: u32) -> Option<[u16; 32]> {
    let mut dd = DISPLAY_DEVICEW {
        cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
        ..Default::default()
    };
    let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0) };
    if !ok.as_bool() || !dd.StateFlags.contains(DISPLAY_DEVICE_ACTIVE) {
        return None;
    }
    Some(dd.DeviceName)
}

/// Every monitor with its GDI device name (zeros if GetMonitorInfoW
/// failed), in EnumDisplayMonitors order.
fn enumerate_monitors() -> Vec<(HMONITOR, [u16; 32])> {
    let mut handles: Vec<HMONITOR> = Vec::new();
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
    let lparam = LPARAM(&mut handles as *mut _ as isize);
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(cb), lparam) };
    handles
        .into_iter()
        .map(|hmon| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            let ok = unsafe {
                GetMonitorInfoW(hmon, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO)
            };
            let name = if ok.as_bool() { info.szDevice } else { [0u16; 32] };
            (hmon, name)
        })
        .collect()
}

pub(crate) fn wide_to_string(name: &[u16]) -> String {
    let end = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end])
}

fn winrt_device_from_d3d11(d3d11: &ID3D11Device) -> Result<IDirect3DDevice, String> {
    let dxgi: IDXGIDevice = d3d11
        .cast()
        .map_err(|e| format!("cast to IDXGIDevice: {e:?}"))?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e:?}"))?;
    inspectable
        .cast()
        .map_err(|e| format!("cast inspectable to IDirect3DDevice: {e:?}"))
}

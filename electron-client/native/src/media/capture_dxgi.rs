//! DXGI Desktop Duplication capture source (monitors only).
//!
//! Windows.Graphics.Capture draws a mandatory yellow border around the
//! captured surface on Windows 10 — `SetIsBorderRequired(false)` needs
//! IGraphicsCaptureSession3, which consumer Win10 builds don't have.
//! Desktop Duplication has no border (it's what Discord uses), so
//! monitor capture goes through this module first and falls back to
//! WGC (capture_wgc.rs) when duplication can't start: cross-adapter
//! outputs, rotated displays, HDR desktops where DuplicateOutput1
//! isn't available, exclusive-fullscreen access loss at startup.
//!
//! Mined from `tauri-client/src-tauri/src/media/capture_dxgi.rs`, but
//! reshaped to the electron pipeline's contract: BGRA D3D11 textures
//! pushed into the encoder thread's SyncSender, same as capture_wgc.
//! Two deltas vs. WGC: frames land in a small ring of our own textures
//! (the duplication surface is only valid until ReleaseFrame), and the
//! mouse pointer is composited by us — duplication frames don't include
//! the hardware cursor; it arrives as shape + position metadata, which
//! cursor_gpu.rs blends on with one small draw. Nothing in this loop ever
//! waits on the GPU: a busy game's queue would turn any such wait into a
//! frame-rate cap.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutput5,
    IDXGIOutputDuplication, IDXGIResource, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};

use super::cursor_blend::{self, CursorImage};
use super::cursor_gpu::CursorCompositor;
use super::gpu_pipeline::GpuDevice;

const DXGI_ERROR_WAIT_TIMEOUT_CODE: u32 = 0x887A0027;
const DXGI_ERROR_ACCESS_LOST_CODE: u32 = 0x887A0026;

pub struct Capture {
    stop: Arc<AtomicBool>,
    frames_dropped: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Start duplication on the Chromium monitor index (the same
    /// EnumDisplayMonitors ordering capture_wgc uses). Fails fast —
    /// the thread reports whether duplication actually started, so the
    /// caller can fall back to WGC — and streams paced to `fps`.
    pub fn start(
        gpu: &GpuDevice,
        monitor_idx: u32,
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

        // The thread signals whether duplication came up (or the exact
        // failure) so monitor capture can degrade to WGC.
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);

        let thread = std::thread::Builder::new()
            .name("decibell-dxgi-capture".to_string())
            .spawn(move || {
                if let Err(e) = run_capture_thread(
                    &device,
                    &context,
                    monitor_idx,
                    tx,
                    stop_t,
                    drops_t,
                    include_cursor,
                    fps,
                    &ready_tx,
                ) {
                    log::error!("[capture_dxgi] thread error: {e}");
                    let _ = ready_tx.try_send(Err(e));
                }
            })
            .map_err(|e| format!("spawn capture thread: {e}"))?;

        match ready_rx.recv_timeout(Duration::from_secs(3)) {
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
                // Thread wedged during setup — tell it to stop and
                // leave it detached rather than blocking the caller.
                stop.store(true, Ordering::Relaxed);
                Err("desktop duplication setup timed out".to_string())
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

/// Find the DXGI output whose HMONITOR matches Chromium's Nth monitor.
fn open_duplication(
    device: &ID3D11Device,
    monitor_idx: u32,
) -> Result<IDXGIOutputDuplication, String> {
    let hmon = super::capture_wgc::monitor_at_index(monitor_idx)?;
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| format!("CreateDXGIFactory1: {e:?}"))?;

    let mut adapter_idx = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(adapter_idx) } {
            Ok(a) => a,
            Err(_) => break,
        };
        let mut output_idx = 0u32;
        loop {
            let output: IDXGIOutput = match unsafe { adapter.EnumOutputs(output_idx) } {
                Ok(o) => o,
                Err(_) => break,
            };
            output_idx += 1;
            let desc = match unsafe { output.GetDesc() } {
                Ok(d) => d,
                Err(_) => continue,
            };
            if desc.Monitor.0 as usize != hmon.0 as usize {
                continue;
            }
            // Prefer DuplicateOutput1 (Win10 1803+) requesting BGRA:
            // on HDR desktops the native duplication format is FP16,
            // which the BGRA ring below can't CopyResource from —
            // DuplicateOutput1 makes the DWM tone-map to BGRA for us.
            if let Ok(output5) = output.cast::<IDXGIOutput5>() {
                let supported = [DXGI_FORMAT_B8G8R8A8_UNORM];
                match unsafe { output5.DuplicateOutput1(device, 0, &supported) } {
                    Ok(dup) => return Ok(dup),
                    Err(e) => {
                        log::info!("[capture_dxgi] DuplicateOutput1 failed ({e:?}), trying DuplicateOutput");
                    }
                }
            }
            let output1: IDXGIOutput1 = output
                .cast()
                .map_err(|e| format!("cast IDXGIOutput1: {e:?}"))?;
            return unsafe { output1.DuplicateOutput(device) }
                .map_err(|e| format!("DuplicateOutput: {e:?}"));
        }
        adapter_idx += 1;
    }
    Err(format!("no DXGI output matches monitor index {monitor_idx}"))
}

/// Cursor state accumulated across frames: DXGI only reports the
/// position/shape when they change, so both persist here.
struct CursorState {
    image: Option<CursorImage>,
    /// A new shape arrived and hasn't been uploaded to the compositor.
    shape_dirty: bool,
    pos_x: i32,
    pos_y: i32,
    visible: bool,
    shape_buf: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
fn run_capture_thread(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    monitor_idx: u32,
    tx: mpsc::SyncSender<ID3D11Texture2D>,
    stop: Arc<AtomicBool>,
    drops: Arc<AtomicU32>,
    include_cursor: bool,
    fps: u32,
    ready_tx: &mpsc::SyncSender<Result<(), String>>,
) -> Result<(), String> {
    let duplication = open_duplication(device, monitor_idx)?;

    // 0.61's projection returns the desc by value (no out-pointer).
    let dup_desc: DXGI_OUTDUPL_DESC = unsafe { duplication.GetDesc() };
    let width = dup_desc.ModeDesc.Width;
    let height = dup_desc.ModeDesc.Height;
    if width == 0 || height == 0 {
        return Err("duplication reports zero-sized output".to_string());
    }
    if dup_desc.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        // FP16 HDR surface with no DuplicateOutput1 — the BGRA ring
        // can't CopyResource from it. Let WGC (which converts) handle it.
        return Err(format!(
            "unsupported duplication format {:?} (HDR desktop?)",
            dup_desc.ModeDesc.Format
        ));
    }
    if dup_desc.Rotation != DXGI_MODE_ROTATION_IDENTITY {
        // A rotated display would need a transform pass; WGC already
        // delivers it upright.
        return Err("rotated display — using WGC instead".to_string());
    }

    // Textures. `clean` always holds the latest desktop image with no
    // cursor (updated on every content-changed frame); the 4-slot ring
    // receives clean + cursor at each paced send. The duplication surface
    // is only valid until ReleaseFrame, so it's never held across loop
    // iterations; 4 slots + the depth-2 channel keep a queued texture
    // from being rewritten while the encoder still reads it.
    let tex_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let make_tex = |what: &str| -> Result<ID3D11Texture2D, String> {
        let mut t: Option<ID3D11Texture2D> = None;
        unsafe { device.CreateTexture2D(&tex_desc, None, Some(&mut t)) }
            .map_err(|e| format!("CreateTexture2D ({what}): {e:?}"))?;
        t.ok_or_else(|| format!("CreateTexture2D ({what}) returned None"))
    };
    let clean = make_tex("clean")?;
    let mut ring: Vec<ID3D11Texture2D> = Vec::with_capacity(4);
    for _ in 0..4 {
        ring.push(make_tex("ring")?);
    }

    log::info!(
        "[capture_dxgi] duplication started: monitor {} {}x{} (borderless)",
        monitor_idx, width, height
    );

    let mut cursor = CursorState {
        image: None,
        shape_dirty: false,
        pos_x: 0,
        pos_y: 0,
        visible: false,
        shape_buf: Vec::new(),
    };
    // GPU cursor compositor. If shader compilation is unavailable on this
    // machine the stream simply goes out without a pointer.
    let mut compositor: Option<CursorCompositor> = if include_cursor {
        match CursorCompositor::new(device) {
            Ok(c) => Some(c),
            Err(e) => {
                log::warn!("[capture_dxgi] cursor compositor unavailable ({e}); streaming without cursor");
                None
            }
        }
    } else {
        None
    };

    // Pacing: one send per frame interval, from whatever the desktop
    // looks like right then. Content frames are folded into `clean` as
    // they arrive (never discarded — duplication only hands a frame out
    // once, so anything released without copying is gone for good; an
    // earlier version dropped frames that arrived before the send was
    // due and went choppy whenever the game's presents ran out of phase
    // with the schedule). Mouse-only updates cost nothing but metadata.
    let frame_interval = Duration::from_micros(1_000_000 / fps.max(1).min(240) as u64);
    let mut ring_idx = 0usize;
    let mut have_clean = false;
    let mut next_due = Instant::now();
    let mut signalled_ready = false;
    let signal_ready = |ok: Result<(), String>, flag: &mut bool| {
        if !*flag {
            *flag = true;
            let _ = ready_tx.try_send(ok);
        }
    };

    // Per-second telemetry so a field report comes with numbers: how
    // often duplication woke us and why, what we sent, what the encoder
    // couldn't take, and the worst time spent in one send.
    let mut tele_last = Instant::now();
    let mut tele_content = 0u32;
    let mut tele_mouse = 0u32;
    let mut tele_timeouts = 0u32;
    let mut tele_sends = 0u32;
    let mut tele_drops = 0u32;
    let mut tele_send_max_us = 0u128;

    while !stop.load(Ordering::Relaxed) {
        // Wake for the next deadline (≤ 8ms) or a desktop change,
        // whichever comes first — a fixed 8ms wait would let sends slip
        // by up to that much every frame.
        let remaining = next_due.saturating_duration_since(Instant::now());
        let wait_ms = (remaining.as_micros() as u64).div_ceil(1000).min(8) as u32;

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut desktop_resource: Option<IDXGIResource> = None;
        let hr = unsafe {
            duplication.AcquireNextFrame(wait_ms, &mut frame_info, &mut desktop_resource)
        };

        match hr {
            Ok(()) => {
                signal_ready(Ok(()), &mut signalled_ready);
                if frame_info.LastMouseUpdateTime != 0 {
                    cursor.visible = frame_info.PointerPosition.Visible.as_bool();
                    cursor.pos_x = frame_info.PointerPosition.Position.x;
                    cursor.pos_y = frame_info.PointerPosition.Position.y;
                }
                if frame_info.PointerShapeBufferSize > 0 {
                    fetch_cursor_shape(&duplication, &mut cursor, frame_info.PointerShapeBufferSize);
                }
                // A present happened (or this is the very first frame):
                // fold the new desktop image into `clean`. Mouse-only
                // updates report LastPresentTime == 0 and are skipped —
                // the image hasn't changed.
                let content_changed = frame_info.LastPresentTime != 0
                    || frame_info.AccumulatedFrames > 0
                    || !have_clean;
                if content_changed {
                    tele_content += 1;
                } else {
                    tele_mouse += 1;
                }
                if content_changed {
                    if let Some(resource) = desktop_resource.as_ref() {
                        let src: ID3D11Texture2D = resource
                            .cast()
                            .map_err(|e| format!("cast IDXGIResource: {e:?}"))?;
                        unsafe { context.CopyResource(&clean, &src) };
                        have_clean = true;
                    }
                }
                let _ = unsafe { duplication.ReleaseFrame() };
            }
            Err(e) => {
                let code = e.code().0 as u32;
                if code == DXGI_ERROR_WAIT_TIMEOUT_CODE {
                    signal_ready(Ok(()), &mut signalled_ready);
                    tele_timeouts += 1;
                } else if code == DXGI_ERROR_ACCESS_LOST_CODE {
                    // Exclusive-fullscreen handoff or desktop switch.
                    // Before the first frame the caller falls back to
                    // WGC; mid-stream it ends like a WGC device loss.
                    signal_ready(
                        Err("duplication access lost (exclusive fullscreen?)".to_string()),
                        &mut signalled_ready,
                    );
                    log::error!("[capture_dxgi] access lost, stopping");
                    break;
                } else {
                    signal_ready(Err(format!("AcquireNextFrame: {e:?}")), &mut signalled_ready);
                    log::error!("[capture_dxgi] AcquireNextFrame: {e:?}");
                    break;
                }
            }
        }

        let now = Instant::now();
        if have_clean && now >= next_due {
            let send_started = Instant::now();
            let slot = ring_idx % ring.len();
            ring_idx += 1;
            unsafe { context.CopyResource(&ring[slot], &clean) };
            if let Some(comp) = compositor.as_mut() {
                if cursor.shape_dirty {
                    if let Some(img) = cursor.image.as_ref() {
                        if let Err(e) = comp.set_shape(img) {
                            log::warn!("[capture_dxgi] cursor shape upload failed: {e}");
                        }
                    }
                    cursor.shape_dirty = false;
                }
                if cursor.visible {
                    if let Err(e) =
                        comp.draw(context, &ring[slot], width, height, cursor.pos_x, cursor.pos_y)
                    {
                        log::warn!("[capture_dxgi] cursor draw failed: {e}");
                    }
                }
            }
            match tx.try_send(ring[slot].clone()) {
                Ok(_) => tele_sends += 1,
                Err(mpsc::TrySendError::Full(_)) => {
                    drops.fetch_add(1, Ordering::Relaxed);
                    tele_drops += 1;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
            tele_send_max_us = tele_send_max_us.max(send_started.elapsed().as_micros());
            // Fixed cadence, but re-anchor after a stall so a burst of
            // overdue slots doesn't fire back-to-back.
            next_due = if now > next_due + frame_interval * 4 {
                now + frame_interval
            } else {
                next_due + frame_interval
            };
        }

        if tele_last.elapsed() >= Duration::from_secs(1) {
            log::info!(
                "[capture_dxgi] 1s: content={} mouse_only={} timeouts={} sends={} drops={} send_max={}us",
                tele_content, tele_mouse, tele_timeouts, tele_sends, tele_drops, tele_send_max_us
            );
            tele_content = 0;
            tele_mouse = 0;
            tele_timeouts = 0;
            tele_sends = 0;
            tele_drops = 0;
            tele_send_max_us = 0;
            tele_last = Instant::now();
        }
    }

    Ok(())
}

/// Pull an updated pointer shape out of the duplication object.
fn fetch_cursor_shape(
    duplication: &IDXGIOutputDuplication,
    cursor: &mut CursorState,
    buf_size: u32,
) {
    cursor.shape_buf.resize(buf_size as usize, 0);
    let mut required = 0u32;
    let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
    let ok = unsafe {
        duplication.GetFramePointerShape(
            cursor.shape_buf.len() as u32,
            cursor.shape_buf.as_mut_ptr() as *mut _,
            &mut required,
            &mut info,
        )
    };
    if ok.is_err() {
        return;
    }
    let visual_height = if info.Type == cursor_blend::SHAPE_MONOCHROME {
        (info.Height / 2) as usize
    } else {
        info.Height as usize
    };
    if info.Width == 0 || visual_height == 0 {
        cursor.image = None;
        cursor.shape_dirty = true;
        return;
    }
    cursor.image = Some(CursorImage {
        shape_type: info.Type,
        data: cursor.shape_buf.clone(),
        pitch: info.Pitch as usize,
        width: info.Width as usize,
        visual_height,
    });
    cursor.shape_dirty = true;
}

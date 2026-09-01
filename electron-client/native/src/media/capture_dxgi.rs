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
//! mouse pointer is composited manually (duplication frames don't
//! include the hardware cursor — it arrives as shape + position
//! metadata; see cursor_blend.rs).

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_CPU_ACCESS_WRITE,
    D3D11_MAP_READ_WRITE, D3D11_MAPPED_SUBRESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_USAGE_STAGING,
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
    pos_x: i32,
    pos_y: i32,
    visible: bool,
    shape_buf: Vec<u8>,
    staging: Option<(ID3D11Texture2D, usize, usize)>, // (tex, w, h)
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
        pos_x: 0,
        pos_y: 0,
        visible: false,
        shape_buf: Vec::new(),
        staging: None,
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
            let slot = ring_idx % ring.len();
            ring_idx += 1;
            unsafe { context.CopyResource(&ring[slot], &clean) };
            if include_cursor && cursor.visible {
                composite_cursor_on(device, context, &ring[slot], width, height, &mut cursor);
            }
            match tx.try_send(ring[slot].clone()) {
                Ok(_) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    drops.fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::TrySendError::Disconnected(_)) => break,
            }
            // Fixed cadence, but re-anchor after a stall so a burst of
            // overdue slots doesn't fire back-to-back.
            next_due = if now > next_due + frame_interval * 4 {
                now + frame_interval
            } else {
                next_due + frame_interval
            };
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
        return;
    }
    cursor.image = Some(CursorImage {
        shape_type: info.Type,
        data: cursor.shape_buf.clone(),
        pitch: info.Pitch as usize,
        width: info.Width as usize,
        visual_height,
    });
}

/// Blend the cached cursor onto `frame_tex` via a small staging
/// round-trip: copy the cursor-sized region out, blend on the CPU
/// (cursor_blend.rs), copy back. A few KB per frame — negligible next
/// to the encode, and it avoids hand-rolling a D3D blend pass.
fn composite_cursor_on(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    frame_tex: &ID3D11Texture2D,
    frame_w: u32,
    frame_h: u32,
    cursor: &mut CursorState,
) {
    let Some(image) = cursor.image.as_ref() else { return };
    let Some((px, py, pw, ph, sx, sy)) = cursor_blend::clip_cursor_rect(
        cursor.pos_x,
        cursor.pos_y,
        image.width,
        image.visual_height,
        frame_w as usize,
        frame_h as usize,
    ) else {
        return;
    };

    // (Re)create the staging texture when the cursor size grows.
    let need_w = image.width;
    let need_h = image.visual_height;
    let recreate = match &cursor.staging {
        Some((_, w, h)) => *w < need_w || *h < need_h,
        None => true,
    };
    if recreate {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: need_w as u32,
            Height: need_h as u32,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: (D3D11_CPU_ACCESS_READ.0 | D3D11_CPU_ACCESS_WRITE.0) as u32,
            MiscFlags: 0,
        };
        let mut t: Option<ID3D11Texture2D> = None;
        if unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }.is_err() {
            return;
        }
        let Some(t) = t else { return };
        cursor.staging = Some((t, need_w, need_h));
    }
    let Some((staging, _, _)) = cursor.staging.as_ref() else { return };

    unsafe {
        // Frame region → staging top-left.
        let src_box = D3D11_BOX {
            left: px,
            top: py,
            front: 0,
            right: px + pw as u32,
            bottom: py + ph as u32,
            back: 1,
        };
        context.CopySubresourceRegion(staging, 0, 0, 0, 0, frame_tex, 0, Some(&src_box));

        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        if context
            .Map(staging, 0, D3D11_MAP_READ_WRITE, 0, Some(&mut mapped))
            .is_err()
        {
            return;
        }
        let pitch = mapped.RowPitch as usize;
        let patch =
            std::slice::from_raw_parts_mut(mapped.pData as *mut u8, pitch * ph);
        cursor_blend::composite_cursor(patch, pitch, pw, ph, image, sx, sy);
        context.Unmap(staging, 0);

        // Blended staging region → back into the frame.
        let back_box = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: pw as u32,
            bottom: ph as u32,
            back: 1,
        };
        context.CopySubresourceRegion(frame_tex, 0, px, py, 0, staging, 0, Some(&back_box));
    }
}

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
//!
//! Access loss mid-stream is routine (UAC prompt, Win+L, Ctrl+Alt+Del,
//! display mode change, a game going exclusive-fullscreen, resume), and
//! the documented response is to release the duplication and create a
//! new one. So after DXGI_ERROR_ACCESS_LOST the loop keeps re-sending the
//! last good frame at the pacing rate and retries every RETRY_INTERVAL
//! for up to RECOVERY_TIMEOUT (re-creation fails with E_ACCESSDENIED /
//! DXGI_ERROR_UNSUPPORTED while the secure desktop is up), rebuilding the
//! frame textures if the mode size changed. Only the timeout or a removed
//! device ends the stream.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::LUID;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_MODE_ROTATION_IDENTITY, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutput5, IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST,
    DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_DESC, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};

use super::capture_wgc::{resolve_monitor, wide_to_string};
use super::cursor_blend::{self, CursorImage};
use super::cursor_gpu::{multithread_of, ContextLock, CursorCompositor};
use super::gpu_pipeline::GpuDevice;
use super::source_id::wide_name_eq;

/// How long `start` waits for the thread's setup verdict.
const READY_TIMEOUT: Duration = Duration::from_secs(3);
/// Re-acquire attempt period after access loss.
const RETRY_INTERVAL: Duration = Duration::from_millis(250);
/// Give up re-acquiring after this long without a duplication.
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// 4 slots + the depth-2 channel keep a queued texture from being
/// rewritten while the encoder still reads it.
const RING_SLOTS: usize = 4;

pub struct Capture {
    stop: Arc<AtomicBool>,
    frames_dropped: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Start duplication on Chromium's `screen:N` monitor (resolved the
    /// same way capture_wgc does). Fails fast — the thread reports
    /// whether duplication actually started, so the caller can fall back
    /// to WGC — and streams paced to `fps`.
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
                // A panic must end the capture (the dropped sender reports
                // it downstream), not unwind silently.
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_capture_thread(
                        &device,
                        &context,
                        monitor_idx,
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
                    log::error!("[capture_dxgi] thread error: {e}");
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

/// A validated duplication and its desktop mode size.
struct Duplication {
    dup: IDXGIOutputDuplication,
    width: u32,
    height: u32,
}

/// Duplicate Chromium's Nth monitor and check we can consume it as-is
/// (BGRA, unrotated, non-empty).
fn open_duplication(device: &ID3D11Device, monitor_idx: u32) -> Result<Duplication, String> {
    let dup = duplicate_output(device, monitor_idx)?;
    // 0.61's projection returns the desc by value (no out-pointer).
    let desc: DXGI_OUTDUPL_DESC = unsafe { dup.GetDesc() };
    let width = desc.ModeDesc.Width;
    let height = desc.ModeDesc.Height;
    if width == 0 || height == 0 {
        return Err("duplication reports zero-sized output".to_string());
    }
    if desc.ModeDesc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
        // FP16 HDR surface with no DuplicateOutput1 — the BGRA ring
        // can't CopyResource from it. Let WGC (which converts) handle it.
        return Err(format!(
            "unsupported duplication format {:?} (HDR desktop?)",
            desc.ModeDesc.Format
        ));
    }
    if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY {
        // A rotated display would need a transform pass; WGC already
        // delivers it upright.
        return Err("rotated display — using WGC instead".to_string());
    }
    Ok(Duplication { dup, width, height })
}

/// LUID of the adapter the capture device lives on.
fn device_adapter_luid(device: &ID3D11Device) -> Option<LUID> {
    let dxgi: IDXGIDevice = device.cast().ok()?;
    let adapter = unsafe { dxgi.GetAdapter() }.ok()?;
    unsafe { adapter.GetDesc() }.ok().map(|d| d.AdapterLuid)
}

/// Find the DXGI output for Chromium's `screen:N` — by GDI device name
/// when the id resolved to one (DXGI_OUTPUT_DESC.DeviceName), else by
/// HMONITOR — and duplicate it. An output driven by a different adapter
/// than the capture device can't be duplicated by it; that fails cleanly
/// so start_windows falls back to WGC.
fn duplicate_output(
    device: &ID3D11Device,
    monitor_idx: u32,
) -> Result<IDXGIOutputDuplication, String> {
    let monitor = resolve_monitor(monitor_idx)?;
    let device_luid = device_adapter_luid(device);
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| format!("CreateDXGIFactory1: {e:?}"))?;

    let mut adapter_idx = 0u32;
    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(adapter_idx) } {
            Ok(a) => a,
            Err(_) => break,
        };
        adapter_idx += 1;
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
            let matches = match monitor.device_name.as_ref() {
                Some(name) => wide_name_eq(&desc.DeviceName, name),
                None => std::ptr::eq(desc.Monitor.0, monitor.hmon.0),
            };
            if !matches {
                continue;
            }
            if let (Some(dev_luid), Ok(adesc)) = (device_luid, unsafe { adapter.GetDesc1() }) {
                if adesc.AdapterLuid != dev_luid {
                    return Err(format!(
                        "{} is driven by another adapter than the capture device",
                        wide_to_string(&desc.DeviceName)
                    ));
                }
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

/// Size-dependent textures. `clean` always holds the latest desktop image
/// with no cursor (updated on every content-changed frame); the ring
/// receives clean + cursor at each paced send. The duplication surface is
/// only valid until ReleaseFrame, so it's never held across iterations.
struct Surfaces {
    width: u32,
    height: u32,
    clean: ID3D11Texture2D,
    ring: Vec<ID3D11Texture2D>,
    next: usize,
}

impl Surfaces {
    fn new(device: &ID3D11Device, width: u32, height: u32) -> Result<Self, String> {
        let desc = D3D11_TEXTURE2D_DESC {
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
            unsafe { device.CreateTexture2D(&desc, None, Some(&mut t)) }
                .map_err(|e| format!("CreateTexture2D ({what} {width}x{height}): {e:?}"))?;
            t.ok_or_else(|| format!("CreateTexture2D ({what}) returned None"))
        };
        let clean = make_tex("clean")?;
        let mut ring = Vec::with_capacity(RING_SLOTS);
        for _ in 0..RING_SLOTS {
            ring.push(make_tex("ring")?);
        }
        Ok(Self {
            width,
            height,
            clean,
            ring,
            next: 0,
        })
    }
}

enum SendOutcome {
    Sent,
    Full,
    Disconnected,
}

/// Copy `clean` into the next ring slot, blend the cursor on, and offer
/// it to the encoder. Queues GPU work only; the copy + composite run as
/// one sequence under the device lock so no other thread's context work
/// lands between them.
fn composite_and_send(
    context: &ID3D11DeviceContext,
    mt: Option<&ID3D11Multithread>,
    surfaces: &mut Surfaces,
    compositor: &mut Option<CursorCompositor>,
    cursor: &mut CursorState,
    tx: &mpsc::SyncSender<ID3D11Texture2D>,
) -> SendOutcome {
    let slot = surfaces.next % surfaces.ring.len().max(1);
    surfaces.next = surfaces.next.wrapping_add(1);
    let Some(target) = surfaces.ring.get(slot).cloned() else {
        return SendOutcome::Full;
    };
    // Shape upload is device-only work (texture creation) — outside the
    // context lock.
    if let Some(comp) = compositor.as_mut() {
        if cursor.shape_dirty {
            if let Some(img) = cursor.image.as_ref() {
                if let Err(e) = comp.set_shape(img) {
                    log::warn!("[capture_dxgi] cursor shape upload failed: {e}");
                }
            }
            cursor.shape_dirty = false;
        }
    }
    {
        let _lock = ContextLock::enter(mt);
        unsafe { context.CopyResource(&target, &surfaces.clean) };
        if let Some(comp) = compositor.as_mut() {
            if cursor.visible {
                if let Err(e) = comp.draw(
                    context,
                    &target,
                    surfaces.width,
                    surfaces.height,
                    cursor.pos_x,
                    cursor.pos_y,
                ) {
                    log::warn!("[capture_dxgi] cursor draw failed: {e}");
                }
            }
        }
    }
    match tx.try_send(target) {
        Ok(_) => SendOutcome::Sent,
        Err(mpsc::TrySendError::Full(_)) => SendOutcome::Full,
        Err(mpsc::TrySendError::Disconnected(_)) => SendOutcome::Disconnected,
    }
}

fn device_removed(device: &ID3D11Device) -> bool {
    unsafe { device.GetDeviceRemovedReason() }.is_err()
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
    let first = open_duplication(device, monitor_idx)?;
    let mut surfaces = Surfaces::new(device, first.width, first.height)?;
    log::info!(
        "[capture_dxgi] duplication started: monitor {} {}x{} (borderless)",
        monitor_idx, first.width, first.height
    );
    let mut dup: Option<Duplication> = Some(first);
    let mt = multithread_of(device);

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
    let frame_interval = Duration::from_micros(1_000_000 / fps.clamp(1, 240) as u64);
    let mut have_clean = false;
    // Fold the next frame whatever its present info says (first frame of
    // a fresh duplication carries the whole desktop).
    let mut need_full = true;
    let mut next_due = Instant::now();
    let mut signalled_ready = false;
    let signal_ready = |ok: Result<(), String>, flag: &mut bool| {
        if !*flag {
            *flag = true;
            let _ = ready_tx.try_send(ok);
        }
    };

    // Access-loss recovery state (dup == None while recovering).
    let mut lost_since: Option<Instant> = None;
    let mut next_retry = Instant::now();
    let mut last_retry_err = String::new();

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
        if let Some(d) = dup.as_ref() {
            // Wake for the next deadline (≤ 8ms) or a desktop change,
            // whichever comes first — a fixed 8ms wait would let sends
            // slip by up to that much every frame.
            let remaining = next_due.saturating_duration_since(Instant::now());
            let wait_ms = (remaining.as_micros() as u64).div_ceil(1000).min(8) as u32;

            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;
            let hr = unsafe {
                d.dup
                    .AcquireNextFrame(wait_ms, &mut frame_info, &mut desktop_resource)
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
                        fetch_cursor_shape(&d.dup, &mut cursor, frame_info.PointerShapeBufferSize);
                    }
                    // A present happened (or this is the very first frame):
                    // fold the new desktop image into `clean`. Mouse-only
                    // updates report LastPresentTime == 0 and are skipped —
                    // the image hasn't changed.
                    let content_changed = need_full
                        || frame_info.LastPresentTime != 0
                        || frame_info.AccumulatedFrames > 0
                        || !have_clean;
                    if content_changed {
                        tele_content += 1;
                    } else {
                        tele_mouse += 1;
                    }
                    if content_changed {
                        if let Some(resource) = desktop_resource.as_ref() {
                            match resource.cast::<ID3D11Texture2D>() {
                                Ok(src) => {
                                    unsafe { context.CopyResource(&surfaces.clean, &src) };
                                    have_clean = true;
                                    need_full = false;
                                }
                                Err(e) => {
                                    log::warn!("[capture_dxgi] cast IDXGIResource: {e:?}");
                                }
                            }
                        }
                    }
                    drop(desktop_resource);
                    let _ = unsafe { d.dup.ReleaseFrame() };
                }
                Err(e) => {
                    let code = e.code();
                    if code == DXGI_ERROR_WAIT_TIMEOUT {
                        signal_ready(Ok(()), &mut signalled_ready);
                        tele_timeouts += 1;
                    } else if !signalled_ready {
                        // Before the first frame the caller falls back
                        // to WGC instead.
                        let msg = if code == DXGI_ERROR_ACCESS_LOST {
                            "duplication access lost at start (exclusive fullscreen?)".to_string()
                        } else {
                            format!("AcquireNextFrame: {e:?}")
                        };
                        signal_ready(Err(msg.clone()), &mut signalled_ready);
                        return Err(msg);
                    } else if device_removed(device) {
                        return Err(format!("D3D device removed ({e:?})"));
                    } else {
                        // Routine: UAC / lock screen / mode change /
                        // exclusive fullscreen. Release and re-create.
                        log::warn!(
                            "[capture_dxgi] duplication lost ({e:?}); re-acquiring, holding the last frame"
                        );
                        dup = None;
                        lost_since = Some(Instant::now());
                        next_retry = Instant::now();
                        last_retry_err.clear();
                    }
                }
            }
        } else {
            // Recovering from access loss.
            let since = *lost_since.get_or_insert_with(Instant::now);
            if device_removed(device) {
                return Err("D3D device removed while re-acquiring duplication".to_string());
            }
            let now = Instant::now();
            if now >= next_retry {
                next_retry = now + RETRY_INTERVAL;
                match open_duplication(device, monitor_idx) {
                    Ok(d) => {
                        log::info!(
                            "[capture_dxgi] duplication re-acquired after {} ms ({}x{})",
                            since.elapsed().as_millis(),
                            d.width,
                            d.height
                        );
                        if d.width != surfaces.width || d.height != surfaces.height {
                            log::info!(
                                "[capture_dxgi] desktop {}x{} → {}x{}; rebuilding frame textures",
                                surfaces.width, surfaces.height, d.width, d.height
                            );
                            surfaces = Surfaces::new(device, d.width, d.height)?;
                            have_clean = false;
                            if let Some(c) = compositor.as_mut() {
                                c.forget_targets();
                            }
                        }
                        need_full = true;
                        dup = Some(d);
                        lost_since = None;
                        continue;
                    }
                    Err(e) => {
                        if since.elapsed() >= RECOVERY_TIMEOUT {
                            return Err(format!(
                                "duplication not re-acquired within {}s: {e}",
                                RECOVERY_TIMEOUT.as_secs()
                            ));
                        }
                        if e != last_retry_err {
                            log::info!("[capture_dxgi] re-acquire failed (will retry): {e}");
                            last_retry_err = e;
                        }
                    }
                }
            }
        }

        let now = Instant::now();
        if have_clean && now >= next_due {
            let send_started = Instant::now();
            match composite_and_send(
                context,
                mt.as_ref(),
                &mut surfaces,
                &mut compositor,
                &mut cursor,
                &tx,
            ) {
                SendOutcome::Sent => tele_sends += 1,
                SendOutcome::Full => {
                    drops.fetch_add(1, Ordering::Relaxed);
                    tele_drops += 1;
                }
                SendOutcome::Disconnected => break,
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

        if dup.is_none() {
            // No AcquireNextFrame to block in while recovering: nap until
            // the next send or retry, whichever comes first.
            let now = Instant::now();
            let until_send = if have_clean {
                next_due.saturating_duration_since(now)
            } else {
                RETRY_INTERVAL
            };
            let nap = until_send
                .min(next_retry.saturating_duration_since(now))
                .clamp(Duration::from_millis(1), Duration::from_millis(8));
            std::thread::sleep(nap);
        }

        if tele_last.elapsed() >= Duration::from_secs(1) {
            let recovering_ms = lost_since.map(|t| t.elapsed().as_millis()).unwrap_or(0);
            log::info!(
                "[capture_dxgi] 1s: content={} mouse_only={} timeouts={} sends={} drops={} send_max={}us recovering={}ms",
                tele_content, tele_mouse, tele_timeouts, tele_sends, tele_drops, tele_send_max_us, recovering_ms
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

//! Direct wlr-screencopy-unstable-v1 capture backend.
//!
//! Bypasses xdg-desktop-portal entirely on wlroots compositors (Niri, Sway,
//! Hyprland, river). We allocate the SHM buffer ourselves so the compositor
//! writes BGRA bytes that are immediately CPU-readable — no DMA-BUF modifier
//! negotiation, no NVIDIA EGL/CUDA bridge, no portal proxy in the middle.
//!
//! The portal path in `capture_pipewire.rs` stays as the fallback for
//! GNOME (mutter) and KDE Plasma (KWin); `is_available()` decides at
//! runtime which to use by checking whether the running compositor
//! advertises `zwlr_screencopy_manager_v1` in its Wayland registry.
//!
//! Resource-light by design:
//!   * Two SHM buffers, recycled forever — no per-frame allocations.
//!   * One memcpy per delivered frame (compositor writes our SHM, we hand
//!     the bytes to the encoder which uploads to GPU; NVENC does its own
//!     BGRA→NV12 conversion internally).
//!   * Capture pacing matches the user's target FPS — we don't request a
//!     new frame until the previous one delivered AND the FPS interval has
//!     elapsed, so 30fps targets cost half what 60fps does.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_buffer, wl_output, wl_registry, wl_shm, wl_shm_pool};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1 as wlr_frame, zwlr_screencopy_manager_v1 as wlr_mgr,
};

use super::capture::{CaptureConfig, CaptureOutput, CaptureSource, CaptureSourceType, PixelFormat, RawFrame};

const SOURCE_PREFIX: &str = "wlr:";

// ── Public entry points ────────────────────────────────────────────────────

/// Returns true when the current Wayland session exposes
/// `zwlr_screencopy_manager_v1`. False on GNOME, KDE Plasma, and X11
/// sessions; the caller should fall back to the portal path.
pub fn is_available() -> bool {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => return false, // not on Wayland
    };
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let mut probe = ProbeState::default();
    let _registry = display.get_registry(&qh, ());
    // Single roundtrip: registry advertises all globals at once.
    if event_queue.roundtrip(&mut probe).is_err() {
        return false;
    }
    probe.has_screencopy
}

/// List wl_outputs as capture sources. Each becomes `wlr:<name>` in
/// CaptureSource.id. Names match what the compositor reports (DP-1, eDP-1,
/// HDMI-A-1, etc.) so users see the same labels other Linux tools show.
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    tokio::task::spawn_blocking(|| -> Result<Vec<CaptureSource>, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wl connect: {}", e))?;
        let display = conn.display();
        let mut event_queue = conn.new_event_queue();
        let qh = event_queue.handle();
        let mut state = EnumState::default();
        let _ = display.get_registry(&qh, ());
        // Two roundtrips: first the registry, then the wl_output info events.
        event_queue.roundtrip(&mut state).map_err(|e| format!("wl roundtrip: {}", e))?;
        event_queue.roundtrip(&mut state).map_err(|e| format!("wl roundtrip: {}", e))?;
        Ok(state.outputs.into_iter().map(|o| CaptureSource {
            id: format!("{}{}", SOURCE_PREFIX, o.name),
            name: o.label,
            source_type: CaptureSourceType::Screen,
            width: o.width as u32,
            height: o.height as u32,
            thumbnail: None,
        }).collect())
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}

/// Returns true when the source id was produced by `list_sources` here.
pub fn owns_source(source_id: &str) -> bool {
    source_id.starts_with(SOURCE_PREFIX)
}

/// Spawn the capture thread. The returned receiver yields RawFrame BGRA
/// frames at the requested FPS until the consumer drops the receiver,
/// which signals the thread to exit and clean up Wayland state.
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<CaptureOutput, String> {
    let output_name = source_id
        .strip_prefix(SOURCE_PREFIX)
        .ok_or_else(|| format!("not a wlr source id: {}", source_id))?
        .to_string();
    let config = config.clone();
    tokio::task::spawn_blocking(move || -> Result<CaptureOutput, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wl connect: {}", e))?;
        // Resolve the output handle and dimensions on the spawning thread —
        // the capture thread needs to know what size to allocate before it
        // can request the first frame.
        let (output, output_w, output_h) = resolve_output(&conn, &output_name)?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);

        // Decide capture dimensions. wlr-screencopy gives us the source
        // resolution as-is — any downscale to target_width/height happens
        // at encode time (NVENC takes BGRA at any size and scales for free).
        let width = output_w as u32;
        let height = output_h as u32;

        std::thread::Builder::new()
            .name("decibell-wlr-screencopy".to_string())
            .spawn(move || {
                if let Err(e) = capture_loop(conn, output, config, tx) {
                    eprintln!("[wlr-screencopy] capture loop ended: {}", e);
                }
            })
            .map_err(|e| format!("spawn capture thread: {}", e))?;

        Ok(CaptureOutput {
            receiver: rx,
            width,
            height,
            #[cfg(target_os = "linux")]
            gpu_receiver: None,
        })
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}

// ── Probe state (just checks whether screencopy is advertised) ─────────────

#[derive(Default)]
struct ProbeState {
    has_screencopy: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        _: &wl_registry::WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { interface, .. } = ev {
            if interface == "zwlr_screencopy_manager_v1" {
                state.has_screencopy = true;
            }
        }
    }
}

// ── Output enumeration ─────────────────────────────────────────────────────

#[derive(Default)]
struct EnumState {
    outputs: Vec<EnumOutput>,
    pending: Vec<(u32, wl_output::WlOutput)>,
}

struct EnumOutput {
    name: String,
    label: String,
    width: i32,
    height: i32,
}

impl Dispatch<wl_registry::WlRegistry, ()> for EnumState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            if interface == "wl_output" {
                let v = version.min(4);
                let output = registry.bind::<wl_output::WlOutput, _, _>(name, v, qh, ());
                state.pending.push((name, output));
                state.outputs.push(EnumOutput {
                    name: format!("output-{}", name),
                    label: format!("Output {}", name),
                    width: 0,
                    height: 0,
                });
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for EnumState {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        ev: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let idx = state.pending.iter().position(|(_, o)| o == output);
        let Some(idx) = idx else { return };
        match ev {
            wl_output::Event::Mode { width, height, flags, .. } => {
                if flags.into_result().map(|f| f.contains(wl_output::Mode::Current)).unwrap_or(false) {
                    if let Some(o) = state.outputs.get_mut(idx) {
                        o.width = width;
                        o.height = height;
                    }
                }
            }
            wl_output::Event::Name { name } => {
                if let Some(o) = state.outputs.get_mut(idx) {
                    o.name = name;
                }
            }
            wl_output::Event::Description { description } => {
                if let Some(o) = state.outputs.get_mut(idx) {
                    o.label = description;
                }
            }
            _ => {}
        }
    }
}

// ── Resolve a named output to a live handle (used by start_capture) ────────

fn resolve_output(conn: &Connection, target_name: &str) -> Result<(wl_output::WlOutput, i32, i32), String> {
    let display = conn.display();
    let mut event_queue = conn.new_event_queue();
    let qh = event_queue.handle();
    let mut state = EnumState::default();
    let _ = display.get_registry(&qh, ());
    event_queue.roundtrip(&mut state).map_err(|e| format!("wl roundtrip: {}", e))?;
    event_queue.roundtrip(&mut state).map_err(|e| format!("wl roundtrip: {}", e))?;
    let pos = state.outputs.iter().position(|o| o.name == target_name);
    let pos = pos.ok_or_else(|| format!("wl_output '{}' not found", target_name))?;
    let info = &state.outputs[pos];
    let (_, output) = state.pending.remove(pos);
    if info.width <= 0 || info.height <= 0 {
        return Err(format!("output '{}' has unknown size", target_name));
    }
    Ok((output, info.width, info.height))
}

// ── Capture loop ───────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct BufferSpec {
    width: u32,
    height: u32,
    stride: u32,
    /// wl_shm format value (ARGB8888 / XRGB8888). Maps 1:1 to our
    /// PixelFormat::BGRA — wl_shm format names are little-endian, which
    /// in memory means BGRA byte order.
    wl_format: u32,
}

/// Pre-allocated SHM-backed buffer. Mapped once, reused forever.
struct ShmBuffer {
    spec: BufferSpec,
    /// memfd-backed file; the kernel owns the storage.
    _fd: OwnedFd,
    /// Length of the mmap region.
    size: usize,
    /// Pointer to the start of the mmap'd bytes. Valid for the buffer's
    /// entire lifetime — we never remap.
    ptr: *mut u8,
    /// wl_shm_pool tied to the same memfd.
    _pool: wl_shm_pool::WlShmPool,
    /// wl_buffer the compositor writes into.
    buffer: wl_buffer::WlBuffer,
}

unsafe impl Send for ShmBuffer {}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { libc::munmap(self.ptr as *mut _, self.size) };
        }
    }
}

impl ShmBuffer {
    fn new(shm: &wl_shm::WlShm, qh: &QueueHandle<CaptureState>, spec: BufferSpec) -> Result<Self, String> {
        let size = (spec.stride as usize) * (spec.height as usize);
        let opts = memfd::MemfdOptions::default().close_on_exec(true);
        let mfd = opts.create("decibell-wlr-shm").map_err(|e| format!("memfd_create: {}", e))?;
        mfd.as_file().set_len(size as u64).map_err(|e| format!("ftruncate: {}", e))?;
        let fd = mfd.into_file().into();
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err("mmap failed".to_string());
        }
        let pool = shm.create_pool(unsafe { std::os::fd::BorrowedFd::borrow_raw(std::os::fd::AsRawFd::as_raw_fd(&fd)) }, size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            spec.width as i32,
            spec.height as i32,
            spec.stride as i32,
            wl_shm_format_from_u32(spec.wl_format),
            qh,
            (),
        );
        Ok(ShmBuffer {
            spec,
            _fd: fd,
            size,
            ptr: ptr as *mut u8,
            _pool: pool,
            buffer,
        })
    }

    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.size) }
    }
}

fn wl_shm_format_from_u32(v: u32) -> wl_shm::Format {
    match v {
        0 => wl_shm::Format::Argb8888,
        1 => wl_shm::Format::Xrgb8888,
        // Default to the most common 8-bit BGRA layout. The compositor
        // also negotiates by passing the format in the Buffer event,
        // so unsupported formats get caught earlier in practice.
        _ => wl_shm::Format::Xrgb8888,
    }
}

#[derive(Default)]
struct CaptureState {
    screencopy_mgr: Option<wlr_mgr::ZwlrScreencopyManagerV1>,
    shm: Option<wl_shm::WlShm>,
    /// Set by the screencopy frame's `buffer` event — what size/format the
    /// compositor wants this frame at. We allocate (or reuse) an SHM buffer
    /// to match.
    pending_spec: Option<BufferSpec>,
    /// Toggled true by the frame's `ready` event. Set false again after we
    /// destroy the frame.
    frame_ready: bool,
    /// If the frame fails (screencopy_failed), bail.
    frame_failed: bool,
    /// Y-flip flag from the frame's `flags` event.
    y_invert: bool,
    /// Capture timestamp from the `ready` event.
    timestamp_us: u64,
}

impl Dispatch<wl_registry::WlRegistry, ()> for CaptureState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            match interface.as_str() {
                "zwlr_screencopy_manager_v1" => {
                    state.screencopy_mgr = Some(registry.bind::<wlr_mgr::ZwlrScreencopyManagerV1, _, _>(
                        name, version.min(3), qh, (),
                    ));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for CaptureState {
    fn event(_: &mut Self, _: &wl_output::WlOutput, _: wl_output::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for CaptureState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for CaptureState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for CaptureState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wlr_mgr::ZwlrScreencopyManagerV1, ()> for CaptureState {
    fn event(_: &mut Self, _: &wlr_mgr::ZwlrScreencopyManagerV1, _: wlr_mgr::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<wlr_frame::ZwlrScreencopyFrameV1, ()> for CaptureState {
    fn event(
        state: &mut Self,
        _: &wlr_frame::ZwlrScreencopyFrameV1,
        ev: wlr_frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            wlr_frame::Event::Buffer { format, width, height, stride } => {
                let fmt = match format {
                    WEnum::Value(f) => f as u32,
                    WEnum::Unknown(v) => v,
                };
                state.pending_spec = Some(BufferSpec {
                    width,
                    height,
                    stride,
                    wl_format: fmt,
                });
            }
            wlr_frame::Event::Flags { flags } => {
                let f = match flags {
                    WEnum::Value(f) => f.bits(),
                    WEnum::Unknown(v) => v,
                };
                state.y_invert = (f & wlr_frame::Flags::YInvert.bits()) != 0;
            }
            wlr_frame::Event::Ready { tv_sec_hi, tv_sec_lo, tv_nsec } => {
                let secs = ((tv_sec_hi as u64) << 32) | (tv_sec_lo as u64);
                state.timestamp_us = secs.saturating_mul(1_000_000) + (tv_nsec as u64) / 1_000;
                state.frame_ready = true;
            }
            wlr_frame::Event::Failed => {
                state.frame_failed = true;
            }
            _ => {}
        }
    }
}

fn capture_loop(
    conn: Connection,
    output: wl_output::WlOutput,
    config: CaptureConfig,
    tx: SyncSender<RawFrame>,
) -> Result<(), String> {
    let display = conn.display();
    let mut event_queue = conn.new_event_queue::<CaptureState>();
    let qh = event_queue.handle();
    let mut state = CaptureState::default();
    let _ = display.get_registry(&qh, ());
    event_queue.roundtrip(&mut state).map_err(|e| format!("wl roundtrip: {}", e))?;

    let mgr = state.screencopy_mgr.clone()
        .ok_or("zwlr_screencopy_manager_v1 not advertised")?;
    let shm = state.shm.clone().ok_or("wl_shm not advertised")?;

    // Pacing: target frame interval. Sleeps before each capture request
    // when the budget allows. At high target_fps (60+) on 1440p the budget
    // is usually consumed by compositor readback + our memcpy, so the
    // sleep effectively becomes zero and we run at the natural cycle rate.
    let frame_interval = if config.target_fps > 0 {
        Duration::from_micros(1_000_000 / config.target_fps as u64)
    } else {
        Duration::from_millis(16)
    };
    let mut last_emit = Instant::now() - frame_interval;

    // Double-buffered SHM pool. Two buffers let the compositor's readback
    // for frame N+1 overlap with our processing of frame N — without this
    // overlap, every cycle is (compositor readback ~15ms at 1440p) +
    // (our memcpy ~3ms) + (dispatch overhead) ≈ 22ms, capping us at ~45fps
    // even when the user picks 60. With overlap, the cycle becomes
    // max(readback, processing) ≈ readback ≈ 15ms, which lands on 60fps.
    //
    // Allocated lazily from the first frame's Buffer event so we know the
    // exact stride/format the compositor wants. Reallocated only on
    // resolution change.
    let mut buffers: [Option<ShmBuffer>; 2] = [None, None];
    let mut next_buf_idx: usize = 0;

    // The frame we have queued with the compositor but haven't fully
    // consumed yet. After Ready we read the buffer it filled, then queue
    // the *next* frame before processing — that's where the pipelining
    // happens. None on the first iteration only.
    let mut in_flight: Option<(wlr_frame::ZwlrScreencopyFrameV1, usize)> = None;

    let mut frames_emitted: u64 = 0;
    let start = Instant::now();
    let mut consecutive_failures = 0u32;

    // Rolling per-stage timing accumulators. Reset every TIMING_REPORT_EVERY
    // frames so the printout reflects current behavior, not lifetime average.
    const TIMING_REPORT_EVERY: u64 = 60;
    let mut sum_buffer_us: u64 = 0;
    let mut sum_copy_to_ready_us: u64 = 0;
    let mut sum_process_us: u64 = 0;
    let mut sum_pace_sleep_us: u64 = 0;
    let mut sum_cycle_us: u64 = 0;
    let mut max_cycle_us: u64 = 0;
    let mut sample_count: u64 = 0;
    let mut last_cycle_end = Instant::now();

    eprintln!("[wlr-screencopy] capture loop started, target {}fps", config.target_fps);

    loop {
        // First-iteration bootstrap: queue the very first capture.
        if in_flight.is_none() {
            let elapsed = Instant::now().duration_since(last_emit);
            if elapsed < frame_interval {
                std::thread::sleep(frame_interval - elapsed);
            }
            let f = mgr.capture_output(0, &output, &qh, ());
            in_flight = Some((f, next_buf_idx));
            next_buf_idx ^= 1;
        }
        let cycle_start = Instant::now();

        // Reset per-frame state. The events below all belong to
        // `in_flight`; the next-frame's events arrive in a later loop
        // iteration after we've reset state again.
        state.pending_spec = None;
        state.frame_ready = false;
        state.frame_failed = false;
        state.y_invert = false;
        state.timestamp_us = 0;

        // Wait for the Buffer event (so we know the spec). blocking_dispatch
        // delivers a batch per call, so we may need a few iterations.
        let buffer_wait_start = Instant::now();
        loop {
            event_queue.blocking_dispatch(&mut state).map_err(|e| format!("wl dispatch: {}", e))?;
            if state.pending_spec.is_some() || state.frame_failed { break; }
        }
        let buffer_wait_us = buffer_wait_start.elapsed().as_micros() as u64;
        if state.frame_failed {
            consecutive_failures += 1;
            if consecutive_failures > 30 {
                return Err("too many consecutive screencopy failures".to_string());
            }
            let (frame, _) = in_flight.take().unwrap();
            frame.destroy();
            continue;
        }

        let spec = state.pending_spec.unwrap();
        let (frame, cur_idx) = in_flight.take().unwrap();

        // Allocate or reuse the buffer for this slot.
        let need_alloc = match &buffers[cur_idx] {
            Some(b) => b.spec.width != spec.width
                || b.spec.height != spec.height
                || b.spec.stride != spec.stride
                || b.spec.wl_format != spec.wl_format,
            None => true,
        };
        if need_alloc {
            buffers[cur_idx] = Some(ShmBuffer::new(&shm, &qh, spec)?);
        }

        // Bind the buffer to this frame (compositor begins readback).
        let copy_to_ready_start = Instant::now();
        frame.copy(&buffers[cur_idx].as_ref().unwrap().buffer);

        // Wait for Ready (compositor finished writing into our buffer).
        loop {
            event_queue.blocking_dispatch(&mut state).map_err(|e| format!("wl dispatch: {}", e))?;
            if state.frame_ready || state.frame_failed { break; }
        }
        let copy_to_ready_us = copy_to_ready_start.elapsed().as_micros() as u64;
        if state.frame_failed {
            consecutive_failures += 1;
            if consecutive_failures > 30 {
                return Err("too many consecutive screencopy failures".to_string());
            }
            frame.destroy();
            continue;
        }
        consecutive_failures = 0;

        // ── Pipeline kickoff ────────────────────────────────────────────
        // Queue the NEXT capture into the OTHER buffer NOW, before we do
        // any of the per-frame processing below. While we're memcpy'ing
        // and shipping this frame to the encoder, the compositor will be
        // doing GPU readback for the next frame. That overlap is the
        // whole reason we keep two buffers — it's the difference between
        // 45fps and 60fps at 1440p.
        let pace_sleep_start = Instant::now();
        let elapsed = pace_sleep_start.duration_since(last_emit);
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
        let pace_sleep_us = pace_sleep_start.elapsed().as_micros() as u64;
        let next_frame = mgr.capture_output(0, &output, &qh, ());
        in_flight = Some((next_frame, next_buf_idx));
        next_buf_idx ^= 1;

        // ── Process current frame's bytes (in parallel with next readback)
        let process_start = Instant::now();
        let buffer = buffers[cur_idx].as_ref().unwrap();
        let mut data = buffer.bytes().to_vec();
        if state.y_invert {
            let stride = buffer.spec.stride as usize;
            let height = buffer.spec.height as usize;
            for y in 0..height / 2 {
                let top = y * stride;
                let bot = (height - 1 - y) * stride;
                let (a, b) = data.split_at_mut(bot);
                a[top..top + stride].swap_with_slice(&mut b[..stride]);
            }
        }

        let pixel_format = match spec.wl_format {
            // Format::Argb8888 = 0, Format::Xrgb8888 = 1 — both "BGRA"
            // in memory byte order (wl_shm names are little-endian).
            0 | 1 => PixelFormat::BGRA,
            _ => PixelFormat::BGRA,
        };

        let raw = RawFrame {
            data,
            width: buffer.spec.width,
            height: buffer.spec.height,
            stride: buffer.spec.stride as usize,
            pixel_format,
            timestamp_us: state.timestamp_us,
        };

        let process_us = process_start.elapsed().as_micros() as u64;

        match tx.try_send(raw) {
            Ok(()) => {
                frames_emitted += 1;
                last_emit = Instant::now();
            }
            Err(std::sync::mpsc::TrySendError::Full(_)) => {
                // Encoder is keeping up — drop the frame, advance the pace
                // counter so we don't busy-loop.
                last_emit = Instant::now();
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                eprintln!("[wlr-screencopy] consumer disconnected, exiting");
                frame.destroy();
                return Ok(());
            }
        }

        // Cycle bookkeeping — measured emit-to-emit so it includes
        // dispatch overhead between iterations and reflects what the
        // downstream encoder actually sees.
        let cycle_us = cycle_start.duration_since(last_cycle_end).as_micros() as u64
            + cycle_start.elapsed().as_micros() as u64;
        last_cycle_end = Instant::now();
        sum_buffer_us += buffer_wait_us;
        sum_copy_to_ready_us += copy_to_ready_us;
        sum_process_us += process_us;
        sum_pace_sleep_us += pace_sleep_us;
        sum_cycle_us += cycle_us;
        if cycle_us > max_cycle_us { max_cycle_us = cycle_us; }
        sample_count += 1;

        if frames_emitted % TIMING_REPORT_EVERY == 0 && sample_count > 0 {
            let n = sample_count as f64;
            let avg_buffer = (sum_buffer_us as f64 / n) / 1000.0;
            let avg_copy = (sum_copy_to_ready_us as f64 / n) / 1000.0;
            let avg_process = (sum_process_us as f64 / n) / 1000.0;
            let avg_pace = (sum_pace_sleep_us as f64 / n) / 1000.0;
            let avg_cycle = (sum_cycle_us as f64 / n) / 1000.0;
            let max_cycle = (max_cycle_us as f64) / 1000.0;
            let actual_fps = if avg_cycle > 0.0 { 1000.0 / avg_cycle } else { 0.0 };
            eprintln!(
                "[wlr-screencopy] timing avg over {} frames: \
                 buffer={:.2}ms copy→ready={:.2}ms process={:.2}ms \
                 pace_sleep={:.2}ms cycle={:.2}ms (max={:.2}ms) → {:.1}fps actual",
                sample_count, avg_buffer, avg_copy, avg_process,
                avg_pace, avg_cycle, max_cycle, actual_fps,
            );
            sum_buffer_us = 0;
            sum_copy_to_ready_us = 0;
            sum_process_us = 0;
            sum_pace_sleep_us = 0;
            sum_cycle_us = 0;
            max_cycle_us = 0;
            sample_count = 0;
        }

        frame.destroy();
    }
}

//! WASAPI process-loopback capture for share-audio (Windows 10 2004+).
//!
//! One `IAudioClient` activated on `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`
//! captures exactly one process *tree*: INCLUDE (that tree only) or
//! EXCLUDE (everything but that tree). The multi-app picker therefore runs
//! a `ProcessLoopbackMixer` — either one exclude-self client (the filter
//! lets everything through) or one include-client per allowed app root —
//! and sums them with `stream_audio_mixer::Mixer` into a single stream
//! for `audio_stream_pipeline`. The client set is re-planned from a WASAPI
//! session snapshot every 2 s and immediately on a filter change
//! (`stream_audio_filter::plan_clients`).
//!
//! Threads:
//!  - `decibell-audio-mixer`: owns the clients + mixer; 10 ms tick; makes
//!    no COM calls inside the tick (planning input arrives as messages).
//!  - `decibell-audio-scan`: session enumeration + parent-PID map, so a
//!    slow `OpenProcess` never stalls the audio tick.
//!  - `decibell-audio-src-<pid>`: one WASAPI capture loop per client.
//!
//! Every client initialises with the default render endpoint's mix format
//! (read once at start — the process-loopback virtual device has no
//! `GetMixFormat` of its own), so the mixer sums like with like.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::capture::AudioFrame;
use super::stream_audio_filter::{
    diff_pids, group_sessions, identity_from_exe_path, plan_clients, AppEntry, AudioSessionInfo,
    ClientPlan, StreamAudioCapture, StreamAudioFilter,
};
use super::stream_audio_mixer::{Mixer, MixerConfig};

use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Com::StructuredStorage::*;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
use windows::Win32::Foundation::*;
use windows::core::{implement, Interface, Error, IUnknown, GUID, HRESULT, PWSTR, Ref};

// Constants not always exported by the windows crate
const WAVE_FORMAT_EXTENSIBLE_TAG: u16 = 0xFFFE;
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT_GUID: GUID = GUID::from_values(
    0x00000003, 0x0000, 0x0010,
    [0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71],
);

/// Mixer source key for the single EXCLUDE-self client.
const EXCLUDE_SELF_KEY: u32 = u32::MAX;
const TICK: Duration = Duration::from_millis(10);
const SCAN_INTERVAL: Duration = Duration::from_secs(2);
/// A client whose thread died within a second of starting (activation or
/// Initialize failed, protected process, …) is not retried for this long.
const SPAWN_COOLDOWN: Duration = Duration::from_secs(15);
/// 20 ms in 100 ns units — the shared-mode buffer each client asks for.
const CLIENT_BUFFER_100NS: i64 = 200_000;

// ─── Public surface ────────────────────────────────────────────────────────

enum MixerCtl {
    SetFilter(StreamAudioFilter),
    /// Session snapshot + parent-PID map from the scan thread.
    Snapshot(Vec<AudioSessionInfo>, BTreeMap<u32, u32>),
    Shutdown,
}

/// Live handle to the Windows share-audio capture. `set_filter` re-plans
/// the client set on the mixer thread; dropping it stops every client and
/// joins all threads.
pub struct ProcessLoopbackMixer {
    ctl_tx: Sender<MixerCtl>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ProcessLoopbackMixer {
    /// Bring up the mixer thread (reads the default render format, spawns
    /// the scan thread, starts the first clients) and return the summed
    /// frame stream. Fails only if the format can't be read.
    pub fn start(filter: StreamAudioFilter) -> Result<(Receiver<AudioFrame>, Self), String> {
        let (out_tx, out_rx) = std::sync::mpsc::sync_channel::<AudioFrame>(16);
        let (ctl_tx, ctl_rx) = std::sync::mpsc::channel::<MixerCtl>();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<u32, String>>(1);
        let scan_ctl = ctl_tx.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-audio-mixer".to_string())
            .spawn(move || run_mixer(filter, ctl_rx, scan_ctl, out_tx, ready_tx))
            .map_err(|e| format!("Spawn audio mixer thread: {}", e))?;
        let mut me = ProcessLoopbackMixer { ctl_tx, thread: Some(thread) };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(rate)) => {
                log::info!("[audio-capture] process-loopback mixer ready @ {} Hz", rate);
                Ok((out_rx, me))
            }
            Ok(Err(e)) => {
                me.shutdown();
                Err(e)
            }
            Err(_) => {
                me.shutdown();
                Err("Timeout starting the stream-audio mixer".to_string())
            }
        }
    }

    fn shutdown(&mut self) {
        let _ = self.ctl_tx.send(MixerCtl::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl StreamAudioCapture for ProcessLoopbackMixer {
    fn set_filter(&self, filter: StreamAudioFilter) {
        let _ = self.ctl_tx.send(MixerCtl::SetFilter(filter));
    }
}

impl Drop for ProcessLoopbackMixer {
    fn drop(&mut self) {
        log::info!("[audio-capture] Cleanup: stopping process-loopback clients");
        self.shutdown();
    }
}

/// One picker row plus the "owns the picked window" flag.
pub struct ListedApp {
    pub entry: AppEntry,
    pub owns_window_source: bool,
}

/// Applications with an audio session (playing or paused), grouped by exe
/// identity, Decibell excluded. `source_id` of the form `window:HWND:0`
/// marks the window's owner; if that app has no session yet it is appended
/// as an inactive row so a silent-so-far game can still be ticked. Runs on
/// its own COM thread.
pub fn list_apps(source_id: Option<&str>) -> Result<Vec<ListedApp>, String> {
    let hwnd = source_id
        .and_then(|id| super::source_id::parse(id).ok())
        .and_then(|t| match t {
            super::source_id::CaptureTarget::Window(h) => Some(h),
            _ => None,
        });
    let worker = std::thread::Builder::new()
        .name("decibell-audio-list".to_string())
        .spawn(move || -> Result<Vec<ListedApp>, String> {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .map_err(|e| format!("CoInitializeEx: {}", e))?;
            }
            let result = (|| {
                let (sessions, names) = enumerate_sessions_with_names()?;
                let owner = hwnd.and_then(window_owner_identity);
                let mut rows: Vec<ListedApp> = group_sessions(&sessions, |id| {
                    names.get(id).cloned().unwrap_or_else(|| id.to_string())
                })
                .into_iter()
                .map(|entry| ListedApp {
                    owns_window_source: owner.as_ref().map(|o| o.identity == entry.id).unwrap_or(false),
                    entry,
                })
                .collect();
                if let Some(o) = owner {
                    if !rows.iter().any(|r| r.owns_window_source) {
                        rows.push(ListedApp {
                            entry: AppEntry { id: o.identity, name: o.display, pids: vec![o.pid], active: false },
                            owns_window_source: true,
                        });
                    }
                }
                Ok(rows)
            })();
            unsafe { CoUninitialize() };
            result
        })
        .map_err(|e| format!("Spawn app list thread: {}", e))?;
    worker.join().map_err(|_| "app list thread panicked".to_string())?
}

// ─── Mixer thread ──────────────────────────────────────────────────────────

/// The default render endpoint's mix format, copied so every client can
/// `Initialize` with the identical block without holding COM memory.
#[derive(Clone)]
struct CaptureFormat {
    channels: u32,
    sample_rate: u32,
    bits_per_sample: u16,
    block_align: u16,
    is_float: bool,
    /// Raw WAVEFORMATEX (+EXTENSIBLE tail) bytes. WAVEFORMATEX is
    /// `repr(C, packed(1))`, so a byte buffer is correctly aligned.
    wfx: Vec<u8>,
}

struct SourceHandle {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    started: Instant,
}

impl SourceHandle {
    fn is_finished(&self) -> bool {
        self.thread.as_ref().map(|t| t.is_finished()).unwrap_or(true)
    }

    fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Snapshot {
    sessions: Vec<AudioSessionInfo>,
    parent_of: BTreeMap<u32, u32>,
}

fn run_mixer(
    initial: StreamAudioFilter,
    ctl_rx: Receiver<MixerCtl>,
    scan_ctl: Sender<MixerCtl>,
    out_tx: SyncSender<AudioFrame>,
    ready_tx: SyncSender<Result<u32, String>>,
) {
    unsafe {
        if let Err(e) = CoInitializeEx(None, COINIT_MULTITHREADED).ok() {
            let _ = ready_tx.send(Err(format!("CoInitializeEx: {}", e)));
            return;
        }
    }
    let fmt = match unsafe { read_default_render_format() } {
        Ok(f) => f,
        Err(e) => {
            let _ = ready_tx.send(Err(e));
            unsafe { CoUninitialize() };
            return;
        }
    };
    log::info!(
        "[audio-capture] Mix format: {}ch, {}Hz, {}bit{}, block_align={}",
        fmt.channels,
        fmt.sample_rate,
        fmt.bits_per_sample,
        if fmt.is_float { " float" } else { "" },
        fmt.block_align
    );
    let _ = ready_tx.send(Ok(fmt.sample_rate));

    let self_pid = std::process::id();
    let self_identity = self_identity();

    // Scan thread: first snapshot immediately, then every 2 s or on wake.
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
    let scan = std::thread::Builder::new()
        .name("decibell-audio-scan".to_string())
        .spawn(move || run_scan(wake_rx, scan_ctl))
        .ok();

    let (src_tx, src_rx) = std::sync::mpsc::sync_channel::<(u32, Vec<f32>)>(64);
    let mut mixer = Mixer::new(MixerConfig::for_rate(fmt.sample_rate));
    let mut clients: BTreeMap<u32, SourceHandle> = BTreeMap::new();
    let mut cooldown: BTreeMap<u32, Instant> = BTreeMap::new();
    let mut filter = initial;
    let mut snapshot: Option<Snapshot> = None;
    let start = Instant::now();
    let mut last_log = Instant::now();
    let mut emitted: u64 = 0;

    // A pass-through filter needs no snapshot: exclude-self right away so
    // the stream has audio from its first frame.
    replan(&filter, snapshot.as_ref(), self_pid, &self_identity, &fmt, &src_tx, &mut mixer, &mut clients, &mut cooldown);

    'run: loop {
        loop {
            match ctl_rx.try_recv() {
                Ok(MixerCtl::SetFilter(f)) => {
                    log::info!("[audio-capture] filter → {} ({} apps)", f.mode.as_str(), f.apps.len());
                    filter = f;
                    replan(&filter, snapshot.as_ref(), self_pid, &self_identity, &fmt, &src_tx, &mut mixer, &mut clients, &mut cooldown);
                    // A fresh snapshot follows shortly (apps that started since).
                    let _ = wake_tx.send(());
                }
                Ok(MixerCtl::Snapshot(sessions, parent_of)) => {
                    snapshot = Some(Snapshot { sessions, parent_of });
                    replan(&filter, snapshot.as_ref(), self_pid, &self_identity, &fmt, &src_tx, &mut mixer, &mut clients, &mut cooldown);
                }
                Ok(MixerCtl::Shutdown) | Err(TryRecvError::Disconnected) => break 'run,
                Err(TryRecvError::Empty) => break,
            }
        }

        while let Ok((key, chunk)) = src_rx.try_recv() {
            mixer.push(key, &chunk);
        }

        // Reap clients whose thread ended (target exited, device
        // invalidated, activation failed). The next snapshot re-adds them
        // if the session is still there; a fast death goes on cooldown.
        let dead: Vec<u32> = clients.iter().filter(|(_, h)| h.is_finished()).map(|(k, _)| *k).collect();
        for k in dead {
            if let Some(h) = clients.remove(&k) {
                let short_lived = h.started.elapsed() < Duration::from_secs(1);
                h.stop_and_join();
                mixer.remove_source(k);
                if short_lived {
                    cooldown.insert(k, Instant::now());
                }
                log::warn!(
                    "[audio-capture] source {} ended{}",
                    describe_key(k),
                    if short_lived { " — on cooldown" } else { "" }
                );
            }
        }

        let due = mixer.frames_due(start.elapsed());
        if let Some(data) = mixer.mix(due) {
            emitted += (data.len() / 2) as u64;
            match out_tx.try_send(AudioFrame { data, channels: 2, sample_rate: fmt.sample_rate }) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    log::info!("[audio-capture] pipeline gone, stopping mixer");
                    break 'run;
                }
            }
        }

        if last_log.elapsed() >= Duration::from_secs(60) {
            log::info!(
                "[audio-capture] mixer: {} source(s), {} frames emitted, {} trims",
                mixer.source_count(),
                emitted,
                mixer.trims()
            );
            last_log = Instant::now();
        }

        std::thread::sleep(TICK);
    }

    drop(wake_tx); // scan thread's recv returns Disconnected
    for (_, h) in std::mem::take(&mut clients) {
        h.stop_and_join();
    }
    if let Some(s) = scan {
        let _ = s.join();
    }
    unsafe { CoUninitialize() };
    log::info!("[audio-capture] mixer stopped after {} frames", emitted);
}

fn describe_key(k: u32) -> String {
    if k == EXCLUDE_SELF_KEY {
        "exclude-self".to_string()
    } else {
        format!("pid {}", k)
    }
}

/// Move the running client set to what `filter` × `snapshot` calls for.
/// Without a snapshot only a pass-through filter can be planned; anything
/// else waits for the first scan (≤ a few hundred ms).
#[allow(clippy::too_many_arguments)]
fn replan(
    filter: &StreamAudioFilter,
    snapshot: Option<&Snapshot>,
    self_pid: u32,
    self_identity: &str,
    fmt: &CaptureFormat,
    src_tx: &SyncSender<(u32, Vec<f32>)>,
    mixer: &mut Mixer,
    clients: &mut BTreeMap<u32, SourceHandle>,
    cooldown: &mut BTreeMap<u32, Instant>,
) {
    if snapshot.is_none() && !filter.is_pass_through() {
        return;
    }
    let empty_sessions: Vec<AudioSessionInfo> = Vec::new();
    let empty_parents: BTreeMap<u32, u32> = BTreeMap::new();
    let (sessions, parent_of) = match snapshot {
        Some(s) => (&s.sessions, &s.parent_of),
        None => (&empty_sessions, &empty_parents),
    };
    let wanted: BTreeSet<u32> = match plan_clients(filter, sessions, parent_of, self_pid, self_identity) {
        ClientPlan::ExcludeSelf => [EXCLUDE_SELF_KEY].into_iter().collect(),
        ClientPlan::Include { pids, suppressed } => {
            for p in suppressed {
                log::info!(
                    "[audio-capture] pid {} not captured: a blocked app runs inside its process tree",
                    p
                );
            }
            pids
        }
    };
    let current: BTreeSet<u32> = clients.keys().copied().collect();
    let (add, remove) = diff_pids(&current, &wanted);
    for k in remove {
        if let Some(h) = clients.remove(&k) {
            h.stop_and_join();
        }
        mixer.remove_source(k);
        log::info!("[audio-capture] source {} removed", describe_key(k));
    }
    let now = Instant::now();
    cooldown.retain(|_, t| now.duration_since(*t) < SPAWN_COOLDOWN);
    for k in add {
        if cooldown.contains_key(&k) {
            continue;
        }
        let (pid, exclude) = if k == EXCLUDE_SELF_KEY { (self_pid, true) } else { (k, false) };
        match spawn_source(k, pid, exclude, fmt.clone(), src_tx.clone()) {
            Ok(h) => {
                clients.insert(k, h);
                mixer.add_source(k);
                log::info!("[audio-capture] source {} added", describe_key(k));
            }
            Err(e) => {
                log::warn!("[audio-capture] source {} failed to start: {}", describe_key(k), e);
                cooldown.insert(k, now);
            }
        }
    }
}

fn spawn_source(
    key: u32,
    target_pid: u32,
    exclude: bool,
    fmt: CaptureFormat,
    src_tx: SyncSender<(u32, Vec<f32>)>,
) -> Result<SourceHandle, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name(format!("decibell-audio-src-{}", target_pid))
        .spawn(move || {
            if let Err(e) = run_source(key, target_pid, exclude, &fmt, src_tx, stop_flag) {
                log::warn!("[audio-capture] source pid={} error: {}", target_pid, e);
            }
        })
        .map_err(|e| format!("Spawn source thread: {}", e))?;
    Ok(SourceHandle { stop, thread: Some(thread), started: Instant::now() })
}

/// One process-loopback client: activate, initialise with the shared
/// format, poll every 10 ms, push stereo f32 chunks to the mixer. Ends on
/// the stop flag, when the mixer is gone, or on the first client error
/// (the mixer reaps the thread and re-plans).
fn run_source(
    key: u32,
    target_pid: u32,
    exclude: bool,
    fmt: &CaptureFormat,
    src_tx: SyncSender<(u32, Vec<f32>)>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(|e| format!("CoInitializeEx: {}", e))?;
        let result = (|| -> Result<(), String> {
            let mode = if exclude { "EXCLUDE_TARGET_PROCESS_TREE" } else { "INCLUDE_TARGET_PROCESS_TREE" };
            log::info!("[audio-capture] Starting WASAPI Process Loopback: pid={}, mode={}", target_pid, mode);

            // Process-loopback Initialize requires AUDCLNT_STREAMFLAGS_LOOPBACK —
            // without it: AUDCLNT_E_INVALID_STREAM_FLAG (0x88890021).
            let audio_client = activate_loopback_client(target_pid, exclude)?;
            audio_client
                .Initialize(
                    AUDCLNT_SHAREMODE_SHARED,
                    AUDCLNT_STREAMFLAGS_LOOPBACK,
                    CLIENT_BUFFER_100NS,
                    0,
                    fmt.wfx.as_ptr() as *const WAVEFORMATEX,
                    None,
                )
                .map_err(|e| format!("Initialize (LOOPBACK): {}", e))?;
            let capture_client: IAudioCaptureClient = audio_client
                .GetService()
                .map_err(|e| format!("GetService IAudioCaptureClient: {}", e))?;
            audio_client.Start().map_err(|e| format!("Start: {}", e))?;

            let mut chunks: u64 = 0;
            'capture: while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(TICK);
                loop {
                    let packet_length = capture_client
                        .GetNextPacketSize()
                        .map_err(|e| format!("GetNextPacketSize: {}", e))?;
                    if packet_length == 0 {
                        break;
                    }
                    let mut buffer_ptr: *mut u8 = std::ptr::null_mut();
                    let mut num_frames: u32 = 0;
                    let mut flags: u32 = 0;
                    capture_client
                        .GetBuffer(&mut buffer_ptr, &mut num_frames, &mut flags, None, None)
                        .map_err(|e| format!("GetBuffer: {}", e))?;
                    if num_frames > 0 && !buffer_ptr.is_null() {
                        let stereo = convert_packet(buffer_ptr, num_frames, flags, fmt);
                        chunks += 1;
                        if chunks == 1 {
                            log::info!("[audio-capture] pid={} first packet: {} frames", target_pid, num_frames);
                        }
                        match src_tx.try_send((key, stereo)) {
                            Ok(()) => {}
                            Err(TrySendError::Full(_)) => {}
                            Err(TrySendError::Disconnected(_)) => {
                                let _ = capture_client.ReleaseBuffer(num_frames);
                                break 'capture;
                            }
                        }
                    }
                    capture_client
                        .ReleaseBuffer(num_frames)
                        .map_err(|e| format!("ReleaseBuffer: {}", e))?;
                }
            }
            let _ = audio_client.Stop();
            log::info!("[audio-capture] pid={} loopback stopped after {} packets", target_pid, chunks);
            Ok(())
        })();
        CoUninitialize();
        result
    }
}

/// Convert one WASAPI packet to interleaved stereo f32 in the shared format.
unsafe fn convert_packet(buffer_ptr: *mut u8, num_frames: u32, flags: u32, fmt: &CaptureFormat) -> Vec<f32> {
    let is_silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
    if is_silent {
        return vec![0.0f32; num_frames as usize * 2];
    }
    let buffer_bytes = (num_frames * fmt.block_align as u32) as usize;
    let raw = std::slice::from_raw_parts(buffer_ptr, buffer_bytes);
    let ch = fmt.channels as usize;
    if fmt.is_float && fmt.bits_per_sample == 32 {
        convert_float_to_stereo(raw, ch)
    } else if !fmt.is_float && fmt.bits_per_sample == 16 {
        convert_s16_to_stereo(raw, ch)
    } else if !fmt.is_float && fmt.bits_per_sample == 32 {
        convert_s32_to_stereo(raw, ch)
    } else {
        vec![0.0f32; num_frames as usize * 2]
    }
}

/// Mix format of the default render endpoint (the process-loopback
/// virtual device mirrors it but has no `GetMixFormat` of its own).
unsafe fn read_default_render_format() -> Result<CaptureFormat, String> {
    let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
        .map_err(|e| format!("CoCreateInstance MMDeviceEnumerator: {}", e))?;
    let default_device = enumerator
        .GetDefaultAudioEndpoint(eRender, eConsole)
        .map_err(|e| format!("GetDefaultAudioEndpoint: {}", e))?;
    let default_client: IAudioClient = default_device
        .Activate(CLSCTX_ALL, None)
        .map_err(|e| format!("Activate default device: {}", e))?;
    let mix_format_ptr = default_client
        .GetMixFormat()
        .map_err(|e| format!("GetMixFormat: {}", e))?;
    let mix_format = &*mix_format_ptr;

    let channels = mix_format.nChannels as u32;
    let sample_rate = mix_format.nSamplesPerSec;
    let bits_per_sample = mix_format.wBitsPerSample;
    let block_align = mix_format.nBlockAlign;
    let cb_size = mix_format.cbSize as usize;
    let is_float = if mix_format.wFormatTag == WAVE_FORMAT_EXTENSIBLE_TAG {
        let ext = &*(mix_format_ptr as *const WAVEFORMATEXTENSIBLE);
        let sub_format = std::ptr::addr_of!(ext.SubFormat).read_unaligned();
        sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT_GUID
    } else {
        mix_format.wFormatTag == 3
    };
    let total = std::mem::size_of::<WAVEFORMATEX>() + cb_size;
    let wfx = std::slice::from_raw_parts(mix_format_ptr as *const u8, total).to_vec();
    CoTaskMemFree(Some(mix_format_ptr as *const _));

    if channels == 0 || sample_rate == 0 || block_align == 0 {
        return Err("default render endpoint reported an empty mix format".to_string());
    }
    Ok(CaptureFormat { channels, sample_rate, bits_per_sample, block_align, is_float, wfx })
}

// ─── Scan thread: sessions + process tree ──────────────────────────────────

fn run_scan(wake_rx: Receiver<()>, ctl_tx: Sender<MixerCtl>) {
    unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            log::warn!("[audio-capture] scan thread: CoInitializeEx failed");
            return;
        }
    }
    let mut logged_error = false;
    loop {
        match enumerate_sessions() {
            Ok(sessions) => {
                logged_error = false;
                let parent_of = process_parent_map();
                if ctl_tx.send(MixerCtl::Snapshot(sessions, parent_of)).is_err() {
                    break;
                }
            }
            Err(e) => {
                if !logged_error {
                    log::warn!("[audio-capture] session scan failed: {}", e);
                    logged_error = true;
                }
            }
        }
        match wake_rx.recv_timeout(SCAN_INTERVAL) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                while wake_rx.try_recv().is_ok() {}
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    unsafe { CoUninitialize() };
}

/// Every audio render session across all active endpoints, merged by PID.
/// Skips PID 0, the system-sounds session, expired sessions, processes we
/// can't open, and our own executable (Electron's child processes share
/// the binary). Caller must be on a COM-initialised thread.
pub fn enumerate_sessions() -> Result<Vec<AudioSessionInfo>, String> {
    enumerate_sessions_with_names().map(|(s, _)| s)
}

/// `enumerate_sessions` plus identity → display name (the exe stem in its
/// original casing, e.g. `Spotify`, `Discord`, `chrome`).
fn enumerate_sessions_with_names() -> Result<(Vec<AudioSessionInfo>, HashMap<String, String>), String> {
    let self_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut by_pid: BTreeMap<u32, AudioSessionInfo> = BTreeMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            .map_err(|e| format!("CoCreateInstance MMDeviceEnumerator: {}", e))?;
        let devices = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .map_err(|e| format!("EnumAudioEndpoints: {}", e))?;
        let count = devices.GetCount().map_err(|e| format!("IMMDeviceCollection::GetCount: {}", e))?;
        for i in 0..count {
            let Ok(device) = devices.Item(i) else { continue };
            let manager: IAudioSessionManager2 = match device.Activate(CLSCTX_ALL, None) {
                Ok(m) => m,
                Err(e) => {
                    log::debug!("[audio-capture] session manager unavailable on endpoint {}: {}", i, e);
                    continue;
                }
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            let n = sessions.GetCount().unwrap_or(0);
            for j in 0..n {
                let Ok(control) = sessions.GetSession(j) else { continue };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                let pid = control2.GetProcessId().unwrap_or(0);
                if pid == 0 {
                    continue;
                }
                // S_OK = yes, S_FALSE = no — both are "success" HRESULTs.
                if control2.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                let state = control.GetState().unwrap_or(AudioSessionStateInactive);
                if state == AudioSessionStateExpired {
                    continue;
                }
                let active = state == AudioSessionStateActive;
                let Some(path) = exe_path(pid) else { continue };
                if !self_exe.is_empty() && path.to_lowercase() == self_exe {
                    continue;
                }
                let Some(identity) = identity_from_exe_path(&path) else { continue };
                names.entry(identity.clone()).or_insert_with(|| display_from_exe_path(&path));
                by_pid
                    .entry(pid)
                    .and_modify(|s| s.active |= active)
                    .or_insert(AudioSessionInfo { pid, identity, active });
            }
        }
    }
    Ok((by_pid.into_values().collect(), names))
}

/// PID → parent PID for every running process (Toolhelp snapshot). Empty
/// on failure — the planner then simply skips tree dedup.
pub fn process_parent_map() -> BTreeMap<u32, u32> {
    let mut map = BTreeMap::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return map };
        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                map.insert(entry.th32ProcessID, entry.th32ParentProcessID);
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    map
}

/// The app owning a captured window: (pid, identity, display).
pub struct WindowOwner {
    pub pid: u32,
    pub identity: String,
    pub display: String,
}

/// Resolve a Chromium `window:HWND:0` source's HWND to its owning process.
pub fn window_owner_identity(hwnd: u64) -> Option<WindowOwner> {
    let mut pid: u32 = 0;
    let tid = unsafe { GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid)) };
    if tid == 0 || pid == 0 {
        return None;
    }
    let path = exe_path(pid)?;
    Some(WindowOwner { pid, identity: identity_from_exe_path(&path)?, display: display_from_exe_path(&path) })
}

/// Full image path of a process, or None if it can't be opened.
fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(handle);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// Exe stem with its original casing (`C:\…\Spotify.exe` → `Spotify`).
fn display_from_exe_path(path: &str) -> String {
    let base = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .or_else(|| base.strip_suffix(".Exe"))
        .unwrap_or(base);
    if stem.is_empty() { base.to_string() } else { stem.to_string() }
}

fn self_identity() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| identity_from_exe_path(&p.to_string_lossy()))
        .unwrap_or_default()
}

// ─── Activation helper ─────────────────────────────────────────────────────

/// Activate an IAudioClient for process loopback capture.
unsafe fn activate_loopback_client(
    target_pid: u32,
    exclude: bool,
) -> Result<IAudioClient, String> {
    let loopback_mode = if exclude {
        PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
    } else {
        PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
    };

    let activation_params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: target_pid,
                ProcessLoopbackMode: loopback_mode,
            },
        },
    });

    // ManuallyDrop prevents PROPVARIANT's Drop (PropVariantClear) from calling
    // CoTaskMemFree on our Rust-allocated blob — that would cause heap corruption.
    let mut prop_variant: std::mem::ManuallyDrop<PROPVARIANT> =
        std::mem::ManuallyDrop::new(std::mem::zeroed());
    {
        let inner = &mut prop_variant.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob.cbSize = std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32;
        inner.Anonymous.blob.pBlobData = &*activation_params as *const _ as *mut u8;
    }

    let inner = Arc::new(AudioActivationInner {
        result: Mutex::new(None),
        condvar: Condvar::new(),
    });
    let handler: IActivateAudioInterfaceCompletionHandler = AudioActivationHandlerCom {
        inner: inner.clone(),
    }.into();
    let waiter = AudioActivationWaiter { inner };

    let _operation = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&*prop_variant as *const PROPVARIANT),
        &handler,
    )
    .map_err(|e| format!("ActivateAudioInterfaceAsync: {}", e))?;

    let client = waiter
        .wait_for_completion(std::time::Duration::from_secs(5))
        .map_err(|e| format!("Wait for audio client: {}", e))?;
    drop(activation_params);

    Ok(client)
}

// ─── IActivateAudioInterfaceCompletionHandler implementation ────────────────

struct AudioActivationInner {
    result: Mutex<Option<std::result::Result<IUnknown, Error>>>,
    condvar: Condvar,
}

struct AudioActivationWaiter {
    inner: Arc<AudioActivationInner>,
}

impl AudioActivationWaiter {
    fn wait_for_completion(
        &self,
        timeout: std::time::Duration,
    ) -> std::result::Result<IAudioClient, String> {
        let mut guard = self.inner.result.lock().unwrap();
        let start = std::time::Instant::now();
        while guard.is_none() {
            let remaining = timeout.checked_sub(start.elapsed()).unwrap_or_default();
            if remaining.is_zero() {
                return Err("Timeout waiting for audio activation".to_string());
            }
            let (new_guard, _) = self.inner.condvar.wait_timeout(guard, remaining).unwrap();
            guard = new_guard;
        }

        match guard.take().unwrap() {
            Ok(unknown) => unknown
                .cast::<IAudioClient>()
                .map_err(|e| format!("Cast to IAudioClient: {}", e)),
            Err(e) => Err(format!("Activation failed: {}", e)),
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct AudioActivationHandlerCom {
    inner: Arc<AudioActivationInner>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for AudioActivationHandlerCom_Impl {
    fn ActivateCompleted(
        &self,
        activateoperation: Ref<'_, IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        let operation: &IActivateAudioInterfaceAsyncOperation =
            activateoperation.ok().map_err(|_| Error::from(E_POINTER))?;

        let mut activate_result = HRESULT(0);
        let mut activated_interface: Option<IUnknown> = None;

        unsafe {
            operation.GetActivateResult(&mut activate_result, &mut activated_interface)?;
        }

        let result = if activate_result.is_ok() {
            match activated_interface {
                Some(iface) => Ok(iface),
                None => Err(Error::from(E_POINTER)),
            }
        } else {
            Err(Error::from(activate_result))
        };

        let mut guard = self.inner.result.lock().unwrap();
        *guard = Some(result);
        self.inner.condvar.notify_all();

        Ok(())
    }
}

// ─── Format conversion helpers ──────────────────────────────────────────────

fn convert_float_to_stereo(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        if channels == 1 {
            stereo.push(frame[0]);
            stereo.push(frame[0]);
        } else {
            stereo.push(frame[0]);
            stereo.push(frame[1]);
        }
    }

    stereo
}

fn convert_s16_to_stereo(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<i16> = raw
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        let l = frame[0] as f32 / 32768.0;
        let r = if channels > 1 {
            frame[1] as f32 / 32768.0
        } else {
            l
        };
        stereo.push(l);
        stereo.push(r);
    }

    stereo
}

fn convert_s32_to_stereo(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<i32> = raw
        .chunks_exact(4)
        .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        let l = frame[0] as f32 / 2147483648.0;
        let r = if channels > 1 {
            frame[1] as f32 / 2147483648.0
        } else {
            l
        };
        stereo.push(l);
        stereo.push(r);
    }

    stereo
}

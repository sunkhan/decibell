use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;

use super::capture::AudioFrame;

use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

/// Private sink that non-Decibell application audio is *tapped* into (via extra
/// PipeWire links — the apps keep playing to the real output untouched). We
/// capture this sink's monitor for the stream, so Decibell's own voice output —
/// which we never link here — is excluded, while the streamer still hears it
/// normally on their real device.
const CAPTURE_SINK: &str = "decibell_capture";

struct AudioCaptureData {
    tx: SyncSender<AudioFrame>,
    format: spa::param::audio::AudioInfoRaw,
    channels: u32,
    sample_rate: u32,
    quit_mainloop: pw::main_loop::MainLoopWeak,
    frame_count: u64,
}

/// Start capturing system audio for the stream, EXCLUDING Decibell's own output
/// (so watchers don't hear the voice chat — and themselves — echoed back).
///
/// Non-disruptive tap approach: rather than rerouting the streamer's audio, we
/// create a private sink (`decibell_capture`) and add EXTRA PipeWire links from
/// every non-Decibell application's output ports into it. That's a *tap* — the
/// apps keep playing to the real output untouched while a copy accumulates in
/// `decibell_capture`, whose monitor we capture. Decibell's own nodes (its whole
/// process tree) are never linked, so the voice chat is kept out of the stream
/// while the streamer still hears it normally.
///
/// A 2s poller re-taps applications that start (or expose new streams) after
/// capture begins, and prunes links for apps that have gone. Cleanup removes the
/// sink, which drops every tap link with it.
pub fn start_system_audio_capture() -> Result<(std::sync::mpsc::Receiver<AudioFrame>, Box<dyn FnOnce() + Send>), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<AudioFrame>(16);

    // Decibell is Electron — several processes. Its native (CPAL) voice output
    // lives in the main process, but UI/Web-Audio blips play through Chromium's
    // separate audio process, so we exclude the whole process tree, not just our
    // own PID.
    let decibell_pids = build_decibell_pids(std::process::id());
    log::info!("[audio-capture] Excluding Decibell PIDs: {:?}", decibell_pids);

    // Create the private capture sink, then wait for it (and its playback ports)
    // to register before linking any taps into it.
    let null_module_id = create_null_sink()?;
    let capture_ports = match wait_for_capture_sink_ports() {
        Ok(p) => p,
        Err(e) => {
            let _ = remove_null_sink(null_module_id);
            return Err(e);
        }
    };

    // Tap all current non-Decibell app outputs into the capture sink.
    tap_non_decibell_apps(&decibell_pids, &capture_ports);

    // Poller: re-tap on every tick — catches apps that appear after capture
    // starts and self-heals any tap WirePlumber tears down. Cheap (one pw-dump
    // + a few pw-link spawns per 2s).
    let poller_stop = Arc::new(AtomicBool::new(false));
    let poller_handle = {
        let stop = poller_stop.clone();
        let ports = capture_ports.clone();
        let our_pid = std::process::id();
        std::thread::Builder::new()
            .name("decibell-audio-tap-poller".to_string())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if stop.load(Ordering::Relaxed) { break; }
                    // Rebuild the exclusion set each tick: Decibell's Chromium
                    // audio-service / renderer processes can restart mid-stream,
                    // and a stale snapshot would let the new PID's audio (UI
                    // blips, or voice routed through Chromium) leak into the tap.
                    let pids = build_decibell_pids(our_pid);
                    tap_non_decibell_apps(&pids, &ports);
                }
            })
            .map_err(|e| format!("Spawn audio tap poller: {}", e))?
    };

    // Capture the private sink's monitor.
    let monitor_target = match find_sink_monitor_target(CAPTURE_SINK) {
        Ok(t) => t,
        Err(e) => {
            poller_stop.store(true, Ordering::Relaxed);
            let _ = poller_handle.join();
            let _ = remove_null_sink(null_module_id);
            return Err(e);
        }
    };

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("decibell-audio-capture".to_string())
        .spawn(move || {
            match run_audio_capture_loop(tx, monitor_target, ready_tx.clone()) {
                Ok(()) => {}
                Err(e) => {
                    log::warn!("[audio-capture] Capture loop error: {}", e);
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| format!("Spawn audio capture thread: {}", e))?;

    // Wait for the capture to be ready (or fail). Removing the sink on error
    // drops every tap link automatically — no per-node restore needed.
    let cleanup_on_err = |poller_stop: &Arc<AtomicBool>, null_module_id: u32| {
        poller_stop.store(true, Ordering::Relaxed);
        let _ = remove_null_sink(null_module_id);
    };
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            cleanup_on_err(&poller_stop, null_module_id);
            return Err(e);
        }
        Err(_) => {
            cleanup_on_err(&poller_stop, null_module_id);
            return Err("Timeout waiting for audio capture to start".to_string());
        }
    }

    let cleanup_poller_stop = poller_stop.clone();
    let cleanup = Box::new(move || {
        log::info!("[audio-capture] Cleanup: removing capture sink + tap links");
        cleanup_poller_stop.store(true, Ordering::Relaxed);
        // Removing the null-sink drops all the tap links we created into it.
        let _ = remove_null_sink(null_module_id);
    }) as Box<dyn FnOnce() + Send>;

    Ok((rx, cleanup))
}

/// Build the set of PIDs belonging to Decibell — our own PID plus every
/// descendant process — by walking `/proc` `ppid` links. Used to decide which
/// PipeWire playback nodes to EXCLUDE from the stream capture.
fn build_decibell_pids(our_pid: u32) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let pid: u32 = match fname.to_string_lossy().parse() {
                Ok(p) => p,
                Err(_) => continue, // non-numeric /proc entry
            };
            // /proc/<pid>/stat: "pid (comm) state ppid ...". comm can contain
            // spaces and parens, so ppid is the 2nd whitespace field AFTER the
            // final ')'.
            if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) {
                if let Some(rparen) = stat.rfind(')') {
                    let fields: Vec<&str> = stat[rparen + 1..].split_whitespace().collect();
                    if let Some(ppid) = fields.get(1).and_then(|s| s.parse::<u32>().ok()) {
                        children.entry(ppid).or_default().push(pid);
                    }
                }
            }
        }
    }
    let mut set = HashSet::new();
    let mut stack = vec![our_pid];
    while let Some(pid) = stack.pop() {
        if set.insert(pid) {
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids);
            }
        }
    }
    set
}

/// Parse a PipeWire `application.process.id` prop, which pw-dump emits as a JSON
/// number on most builds (older ones used a string) — handle both.
fn parse_pid_value(v: Option<&serde_json::Value>) -> Option<u32> {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_u64().map(|x| x as u32),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    }
}

/// Run `pw-dump` and parse it to JSON, or return an error the caller can retry.
fn pw_dump_json() -> Result<serde_json::Value, String> {
    let out = Command::new("pw-dump")
        .output()
        .map_err(|e| format!("pw-dump: {}", e))?;
    if !out.status.success() {
        return Err("pw-dump failed".to_string());
    }
    serde_json::from_str(&String::from_utf8_lossy(&out.stdout))
        .map_err(|e| format!("parse pw-dump: {}", e))
}

/// Wait (up to ~2s) for the private capture sink's playback ports to register,
/// returning a map of audio channel ("FL"/"FR") → port object id.
fn wait_for_capture_sink_ports() -> Result<HashMap<String, u32>, String> {
    for _ in 0..20 {
        if let Ok(dump) = pw_dump_json() {
            let ports = capture_sink_playback_ports(&dump);
            if ports.contains_key("FL") && ports.contains_key("FR") {
                return Ok(ports);
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("capture sink '{}' did not register its ports", CAPTURE_SINK))
}

/// Extract the capture sink's input (playback) port ids keyed by audio channel.
fn capture_sink_playback_ports(dump: &serde_json::Value) -> HashMap<String, u32> {
    let mut sink_node_id: Option<u64> = None;
    if let Some(arr) = dump.as_array() {
        for obj in arr {
            if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
                continue;
            }
            let props = match obj.get("info").and_then(|i| i.get("props")) {
                Some(p) => p,
                None => continue,
            };
            if props.get("node.name").and_then(|v| v.as_str()) == Some(CAPTURE_SINK) {
                sink_node_id = obj.get("id").and_then(|v| v.as_u64());
                break;
            }
        }
    }
    let mut ports = HashMap::new();
    let Some(sink_id) = sink_node_id else { return ports };
    if let Some(arr) = dump.as_array() {
        for obj in arr {
            if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Port") {
                continue;
            }
            let props = match obj.get("info").and_then(|i| i.get("props")) {
                Some(p) => p,
                None => continue,
            };
            if props.get("node.id").and_then(|v| v.as_u64()) != Some(sink_id) {
                continue;
            }
            if props.get("port.direction").and_then(|v| v.as_str()) != Some("in") {
                continue;
            }
            if let (Some(ch), Some(id)) = (
                props.get("audio.channel").and_then(|v| v.as_str()),
                obj.get("id").and_then(|v| v.as_u64()),
            ) {
                ports.insert(ch.to_string(), id as u32);
            }
        }
    }
    ports
}

/// Find every output port belonging to a non-Decibell `Stream/Output/Audio`
/// node — i.e. real application audio we want in the stream. Returns
/// (port_id, audio_channel). Decibell's own nodes (process tree) are excluded.
fn find_non_decibell_output_ports(decibell_pids: &HashSet<u32>) -> Vec<(u32, String)> {
    let dump = match pw_dump_json() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[audio-capture] tap scan: {}", e);
            return Vec::new();
        }
    };
    let arr = match dump.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    // node id → is this a non-Decibell Stream/Output/Audio node?
    let mut app_nodes: HashMap<u64, bool> = HashMap::new();
    for obj in arr {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let props = match obj.get("info").and_then(|i| i.get("props")) {
            Some(p) => p,
            None => continue,
        };
        if props.get("media.class").and_then(|v| v.as_str()) != Some("Stream/Output/Audio") {
            continue;
        }
        let Some(id) = obj.get("id").and_then(|v| v.as_u64()) else { continue };
        let pid = parse_pid_value(props.get("application.process.id"));
        // A node with no PID is definitely not Decibell (our nodes always carry
        // our PID); one whose PID is in our tree is excluded.
        let is_decibell = pid.map(|p| decibell_pids.contains(&p)).unwrap_or(false);
        app_nodes.insert(id, !is_decibell);
    }

    let mut ports = Vec::new();
    for obj in arr {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Port") {
            continue;
        }
        let props = match obj.get("info").and_then(|i| i.get("props")) {
            Some(p) => p,
            None => continue,
        };
        if props.get("port.direction").and_then(|v| v.as_str()) != Some("out") {
            continue;
        }
        let node_id = props.get("node.id").and_then(|v| v.as_u64());
        let is_app = node_id.and_then(|n| app_nodes.get(&n)).copied().unwrap_or(false);
        if !is_app {
            continue;
        }
        if let (Some(ch), Some(id)) = (
            props.get("audio.channel").and_then(|v| v.as_str()),
            obj.get("id").and_then(|v| v.as_u64()),
        ) {
            ports.push((id as u32, ch.to_string()));
        }
    }
    ports
}

/// Tap every non-Decibell application output port into the capture sink by
/// adding an extra PipeWire link. Called on start and on every poller tick;
/// re-linking is cheap and idempotent (pw-link returns "File exists" when the
/// tap is already present), which also makes it self-healing if WirePlumber
/// ever tears a manual link down — the next tick recreates it.
fn tap_non_decibell_apps(decibell_pids: &HashSet<u32>, capture_ports: &HashMap<String, u32>) {
    for (port_id, channel) in find_non_decibell_output_ports(decibell_pids) {
        // Map the app port's channel onto the stereo capture sink; anything
        // that isn't a plain L/R (mono, centre, surround) folds into both.
        let dest_channels: &[&str] = match channel.as_str() {
            "FL" => &["FL"],
            "FR" => &["FR"],
            _ => &["FL", "FR"],
        };
        for dest_ch in dest_channels {
            let Some(&dst_id) = capture_ports.get(*dest_ch) else { continue };
            match Command::new("pw-link")
                .arg(port_id.to_string())
                .arg(dst_id.to_string())
                .output()
            {
                Ok(o) if o.status.success() => {
                    log::info!("[audio-capture] Tapped app port {} ({}) -> {}", port_id, channel, CAPTURE_SINK);
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    // "File exists" just means the tap is already there — fine.
                    if !err.contains("File exists") && !err.contains("EEXIST") {
                        log::warn!("[audio-capture] pw-link {} -> {} failed: {}", port_id, dst_id, err.trim());
                    }
                }
                Err(e) => log::warn!("[audio-capture] pw-link spawn failed: {}", e),
            }
        }
    }
}

/// Create the private stereo capture sink that non-Decibell app audio is
/// tapped into. Uses pactl's module-null-sink (works via PipeWire's PulseAudio
/// compat). Explicit stereo FL/FR so the tap channel mapping is predictable.
fn create_null_sink() -> Result<u32, String> {
    let output = Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            &format!("sink_name={}", CAPTURE_SINK),
            "channels=2",
            "channel_map=front-left,front-right",
            "sink_properties=device.description=Decibell_Capture",
        ])
        .output()
        .map_err(|e| format!("pactl load-module: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "Failed to create null-sink: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let module_id: u32 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "Failed to parse null-sink module ID".to_string())?;

    log::info!("[audio-capture] Created null-sink module {}", module_id);
    Ok(module_id)
}

/// Remove the null-sink module.
fn remove_null_sink(module_id: u32) -> Result<(), String> {
    let output = std::process::Command::new("pactl")
        .args(["unload-module", &module_id.to_string()])
        .output()
        .map_err(|e| format!("pactl unload-module: {}", e))?;

    if !output.status.success() {
        log::info!(
            "[audio-capture] Warning: failed to remove null-sink module {}: {}",
            module_id,
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        log::info!("[audio-capture] Removed null-sink module {}", module_id);
    }
    Ok(())
}

/// Find the PipeWire target node ID for a sink's monitor by name.
fn find_sink_monitor_target(sink_name: &str) -> Result<u32, String> {
    // pw-dump to find the sink node by name, then get its ID for monitor capture
    let pw_dump = std::process::Command::new("pw-dump")
        .output()
        .map_err(|e| format!("pw-dump: {}", e))?;

    let dump_str = String::from_utf8_lossy(&pw_dump.stdout);
    let dump: serde_json::Value =
        serde_json::from_str(&dump_str).map_err(|e| format!("Parse pw-dump: {}", e))?;

    if let Some(arr) = dump.as_array() {
        for obj in arr {
            let obj_type = obj.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if obj_type != "PipeWire:Interface:Node" {
                continue;
            }
            let props = match obj.get("info").and_then(|i| i.get("props")) {
                Some(p) => p,
                None => continue,
            };
            let node_name = props
                .get("node.name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let media_class = props
                .get("media.class")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if node_name == sink_name
                && (media_class == "Audio/Sink" || media_class == "Audio/Duplex")
            {
                if let Some(id) = obj.get("id").and_then(|v| v.as_u64()) {
                    log::info!("[audio-capture] Found sink '{}' at node {}", sink_name, id);
                    return Ok(id as u32);
                }
            }
        }
    }

    Err(format!(
        "Could not find PipeWire node for sink '{}'",
        sink_name
    ))
}

/// Run the PipeWire audio capture loop targeting a specific node's monitor.
fn run_audio_capture_loop(
    tx: SyncSender<AudioFrame>,
    target_node_id: u32,
    ready_tx: SyncSender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();

    let mainloop =
        pw::main_loop::MainLoopRc::new(None).map_err(|e| format!("PW MainLoop: {:?}", e))?;
    let context =
        pw::context::ContextRc::new(&mainloop, None).map_err(|e| format!("PW Context: {:?}", e))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("PW connect: {:?}", e))?;

    let data = AudioCaptureData {
        tx,
        format: Default::default(),
        channels: 0,
        sample_rate: 0,
        quit_mainloop: mainloop.downgrade(),
        frame_count: 0,
    };

    let stream = pw::stream::StreamRc::new(
        core,
        "decibell-audio-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Music",
            "stream.capture.sink" => "true",
        },
    )
    .map_err(|e| format!("PW Stream: {:?}", e))?;

    let ready_tx_clone = ready_tx.clone();
    let mainloop_weak = mainloop.downgrade();

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_stream, _data, old, new| {
            log::info!("[audio-capture] Stream: {:?} -> {:?}", old, new);
            match &new {
                pw::stream::StreamState::Error(msg) => {
                    log::warn!("[audio-capture] Stream error: {}", msg);
                    let _ = ready_tx_clone.send(Err(format!("Stream error: {}", msg)));
                    if let Some(ml) = mainloop_weak.upgrade() {
                        ml.quit();
                    }
                }
                pw::stream::StreamState::Streaming => {
                    let _ = ready_tx_clone.send(Ok(()));
                }
                _ => {}
            }
        })
        .param_changed(|_stream, data, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }

            let (media_type, media_subtype) =
                match spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };

            if media_type != spa::param::format::MediaType::Audio
                || media_subtype != spa::param::format::MediaSubtype::Raw
            {
                return;
            }

            // Runs on PipeWire's realtime thread via a C trampoline — a
            // panic across the extern "C" boundary is UB/abort. Log and
            // bail on a malformed format pod instead of .expect().
            if let Err(e) = data.format.parse(param) {
                log::warn!("[audio-capture] failed to parse AudioInfoRaw: {:?}; ignoring format change", e);
                return;
            }

            data.channels = data.format.channels();
            data.sample_rate = data.format.rate();

            log::info!(
                "[audio-capture] Negotiated: {:?} {}ch @ {}Hz",
                data.format.format(),
                data.channels,
                data.sample_rate,
            );
        })
        .process(|stream, data| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };

            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }

            let d = &mut datas[0];
            let chunk_size = d.chunk().size() as usize;
            let chunk_offset = d.chunk().offset() as usize;

            if chunk_size == 0 {
                return;
            }

            let Some(raw_data) = d.data() else { return };
            // Guard against malformed chunk metadata: a bad offset/size would
            // panic on this slice, and we run on PipeWire's realtime thread
            // across an extern "C" trampoline where a panic aborts the process.
            let end = match chunk_offset.checked_add(chunk_size) {
                Some(e) if e <= raw_data.len() => e,
                _ => {
                    log::warn!(
                        "[audio-capture] chunk out of bounds (offset={}, size={}, buf={}); skipping",
                        chunk_offset, chunk_size, raw_data.len()
                    );
                    return;
                }
            };
            let raw_data = &raw_data[chunk_offset..end];

            let channels = data.channels.max(1) as usize;
            let format = data.format.format();

            // Convert to interleaved stereo f32
            let stereo_f32 = match format {
                spa::param::audio::AudioFormat::F32LE => {
                    convert_to_stereo_f32(raw_data, channels)
                }
                spa::param::audio::AudioFormat::S16LE => {
                    convert_s16_to_stereo_f32(raw_data, channels)
                }
                spa::param::audio::AudioFormat::S32LE => {
                    convert_s32_to_stereo_f32(raw_data, channels)
                }
                _ => {
                    if data.frame_count == 0 {
                        log::info!("[audio-capture] Unsupported audio format: {:?}", format);
                    }
                    data.frame_count += 1;
                    return;
                }
            };

            data.frame_count += 1;
            if data.frame_count == 1 || data.frame_count % 2400 == 0 {
                log::info!(
                    "[audio-capture] Frame {}: {} stereo samples",
                    data.frame_count,
                    stereo_f32.len() / 2
                );
            }

            let frame = AudioFrame {
                data: stereo_f32,
                channels: 2,
                sample_rate: data.sample_rate,
            };

            match data.tx.try_send(frame) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {}
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    log::info!("[audio-capture] Channel closed, stopping");
                    if let Some(ml) = data.quit_mainloop.upgrade() {
                        ml.quit();
                    }
                }
            }
        })
        .register()
        .map_err(|e| format!("PW listener: {:?}", e))?;

    // Negotiate stereo F32 at 48kHz.
    // Note: AudioRate and AudioChannels use plain Int values (not Choice/Range)
    // because the spa pod macro doesn't support `Int` as a Range type —
    // `spa::utils::Int` doesn't exist (only Rectangle, Fraction, Id, Fd).
    let format_obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Audio
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioFormat,
            Choice,
            Enum,
            Id,
            spa::param::audio::AudioFormat::F32LE,
            spa::param::audio::AudioFormat::F32LE,
            spa::param::audio::AudioFormat::S16LE,
            spa::param::audio::AudioFormat::S32LE
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioRate,
            Int,
            48000i32
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::AudioChannels,
            Int,
            2i32
        ),
    );

    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(format_obj),
    )
    .unwrap()
    .0
    .into_inner();

    let mut params = [Pod::from_bytes(&values).unwrap()];

    stream
        .connect(
            spa::utils::Direction::Input,
            Some(target_node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| format!("PW stream connect: {:?}", e))?;

    log::info!("[audio-capture] PipeWire stream connected, running main loop");
    mainloop.run();
    log::info!("[audio-capture] PipeWire main loop exited");

    Ok(())
}

// ─── Format conversion helpers ──────────────────────────────────────────────

/// Convert interleaved f32 audio (any channel count) to interleaved stereo f32.
fn convert_to_stereo_f32(raw: &[u8], channels: usize) -> Vec<f32> {
    let samples: Vec<f32> = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();

    let frame_count = samples.len() / channels;
    let mut stereo = Vec::with_capacity(frame_count * 2);

    for frame in samples.chunks_exact(channels) {
        if channels == 1 {
            // Mono → stereo: duplicate
            stereo.push(frame[0]);
            stereo.push(frame[0]);
        } else {
            // Take first two channels (L, R)
            stereo.push(frame[0]);
            stereo.push(frame[1]);
        }
    }

    stereo
}

/// Convert interleaved S16LE audio to interleaved stereo f32.
fn convert_s16_to_stereo_f32(raw: &[u8], channels: usize) -> Vec<f32> {
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

/// Convert interleaved S32LE audio to interleaved stereo f32.
fn convert_s32_to_stereo_f32(raw: &[u8], channels: usize) -> Vec<f32> {
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

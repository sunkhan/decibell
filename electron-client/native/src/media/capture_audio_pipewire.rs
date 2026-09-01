use std::collections::{HashMap, HashSet};
use std::process::Command;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use super::capture::AudioFrame;
use super::stream_audio_filter::{
    group_sessions, identity_from_exe_path, normalize_identity, AppEntry, AudioSessionInfo,
    StreamAudioCapture, StreamAudioFilter,
};

use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

/// Private sink that application audio is *tapped* into (via extra PipeWire
/// links — the apps keep playing to the real output untouched). We capture
/// this sink's monitor for the stream. Decibell's own nodes are never linked
/// here, so the voice chat stays out of the stream while the streamer still
/// hears it normally on their real device.
const CAPTURE_SINK: &str = "decibell_capture";

/// How often the tap is reconciled against the live PipeWire graph when no
/// filter change wakes it earlier: catches apps that start / stop playing
/// and self-heals links WirePlumber tears down.
const RECONCILE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

struct AudioCaptureData {
    tx: SyncSender<AudioFrame>,
    format: spa::param::audio::AudioInfoRaw,
    channels: u32,
    sample_rate: u32,
    quit_mainloop: pw::main_loop::MainLoopWeak,
    frame_count: u64,
}

/// Live handle to the PipeWire tap: owns the null sink, the reconcile
/// thread and the current app filter. `set_filter` re-links within
/// milliseconds; dropping it removes the sink (and every tap link with it).
pub struct PipewireTap {
    filter: Arc<Mutex<StreamAudioFilter>>,
    /// Wakes the reconcile thread early; dropping it (in `Drop`) is the
    /// thread's stop signal.
    wake_tx: Option<std::sync::mpsc::Sender<()>>,
    poller: Option<std::thread::JoinHandle<()>>,
    null_module_id: u32,
}

impl StreamAudioCapture for PipewireTap {
    fn set_filter(&self, filter: StreamAudioFilter) {
        if let Ok(mut f) = self.filter.lock() {
            *f = filter;
        }
        if let Some(tx) = &self.wake_tx {
            let _ = tx.send(());
        }
    }
}

impl Drop for PipewireTap {
    fn drop(&mut self) {
        log::info!("[audio-capture] Cleanup: removing capture sink + tap links");
        // Dropping the sender makes the poller's recv return Disconnected.
        self.wake_tx.take();
        if let Some(h) = self.poller.take() {
            let _ = h.join();
        }
        // Removing the null-sink drops all the tap links we created into it.
        let _ = remove_null_sink(self.null_module_id);
    }
}

/// Start capturing application audio for the stream, filtered by `filter`
/// and always EXCLUDING Decibell's own output (so watchers don't hear the
/// voice chat — and themselves — echoed back).
///
/// Non-disruptive tap approach: rather than rerouting the streamer's audio,
/// we create a private sink (`decibell_capture`) and add EXTRA PipeWire links
/// from every allowed application's output ports into it. That's a *tap* —
/// the apps keep playing to the real output untouched while a copy
/// accumulates in `decibell_capture`, whose monitor we capture.
///
/// A reconcile thread re-evaluates the link set every 2 s and immediately on
/// `set_filter`: links allowed apps that appeared, unlinks apps the filter no
/// longer allows. Cleanup removes the sink, which drops every tap link.
pub fn start_system_audio_capture(
    filter: StreamAudioFilter,
) -> Result<(std::sync::mpsc::Receiver<AudioFrame>, PipewireTap), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<AudioFrame>(16);

    // Decibell is Electron — several processes. Its native (CPAL) voice output
    // lives in the main process, but UI/Web-Audio blips play through Chromium's
    // separate audio process, so we exclude the whole process tree, not just our
    // own PID. The binary-name check covers nodes that carry no PID at all
    // (PipeWire-ALSA puts the PID on the Client object, not the Node).
    let our_pid = std::process::id();
    let decibell_pids = build_decibell_pids(our_pid);
    let self_id = self_identity();
    log::info!(
        "[audio-capture] Excluding Decibell PIDs {:?} / identity {:?}; filter {} ({} apps)",
        decibell_pids,
        self_id,
        filter.mode.as_str(),
        filter.apps.len()
    );

    // Create the private capture sink, then wait for it (and its playback ports)
    // to register before linking any taps into it.
    let null_module_id = create_null_sink()?;
    let (sink_node_id, capture_ports) = match wait_for_capture_sink_ports() {
        Ok(p) => p,
        Err(e) => {
            let _ = remove_null_sink(null_module_id);
            return Err(e);
        }
    };

    // Tap the currently allowed app outputs into the capture sink.
    reconcile_taps(&decibell_pids, &self_id, &filter, sink_node_id, &capture_ports);

    let filter = Arc::new(Mutex::new(filter));
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
    let poller = {
        let filter = filter.clone();
        let ports = capture_ports.clone();
        let self_id = self_id.clone();
        std::thread::Builder::new()
            .name("decibell-audio-tap-poller".to_string())
            .spawn(move || loop {
                match wake_rx.recv_timeout(RECONCILE_INTERVAL) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
                // Coalesce a burst of ticks (the UI toggles several boxes).
                while wake_rx.try_recv().is_ok() {}
                // Rebuild the exclusion set each tick: Decibell's Chromium
                // audio-service / renderer processes can restart mid-stream,
                // and a stale snapshot would let the new PID's audio (UI
                // blips, or voice routed through Chromium) leak into the tap.
                let pids = build_decibell_pids(our_pid);
                let f = match filter.lock() {
                    Ok(f) => f.clone(),
                    Err(_) => break,
                };
                reconcile_taps(&pids, &self_id, &f, sink_node_id, &ports);
            })
            .map_err(|e| format!("Spawn audio tap poller: {}", e))?
    };
    let tap = PipewireTap {
        filter,
        wake_tx: Some(wake_tx),
        poller: Some(poller),
        null_module_id,
    };

    // Capture the private sink's monitor.
    let monitor_target = match find_sink_monitor_target(CAPTURE_SINK) {
        Ok(t) => t,
        Err(e) => {
            drop(tap);
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

    // Wait for the capture to be ready (or fail). Dropping the tap on error
    // removes the sink, which drops every tap link automatically.
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            drop(tap);
            return Err(e);
        }
        Err(_) => {
            drop(tap);
            return Err("Timeout waiting for audio capture to start".to_string());
        }
    }
    Ok((rx, tap))
}

/// The applications that currently have an audio output node (playing or
/// idle), grouped by identity, Decibell itself excluded. Backs the picker.
pub fn list_apps() -> Result<Vec<AppEntry>, String> {
    let dump = pw_dump_json()?;
    let decibell_pids = build_decibell_pids(std::process::id());
    let self_id = self_identity();
    let nodes = parse_app_nodes(&dump);
    let mut names: HashMap<String, String> = HashMap::new();
    let sessions: Vec<AudioSessionInfo> = nodes
        .iter()
        .filter(|n| !is_self(n, &decibell_pids, &self_id))
        .map(|n| {
            names.entry(n.identity.clone()).or_insert_with(|| n.display.clone());
            AudioSessionInfo { pid: n.pid.unwrap_or(0), identity: n.identity.clone(), active: n.active }
        })
        .collect();
    Ok(group_sessions(&sessions, |id| {
        names.get(id).cloned().unwrap_or_else(|| id.to_string())
    }))
}

/// Our own program name as an identity (`decibell`, or `electron` in a dev
/// checkout) — matched against `application.process.binary`.
fn self_identity() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .map(|n| normalize_identity(&n))
        .unwrap_or_default()
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
/// returning the sink's node id and a map of audio channel ("FL"/"FR") → port
/// object id.
fn wait_for_capture_sink_ports() -> Result<(u32, HashMap<String, u32>), String> {
    for _ in 0..20 {
        if let Ok(dump) = pw_dump_json() {
            if let Some(sink_id) = capture_sink_node_id(&dump) {
                let ports = capture_sink_playback_ports(&dump);
                if ports.contains_key("FL") && ports.contains_key("FR") {
                    return Ok((sink_id as u32, ports));
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("capture sink '{}' did not register its ports", CAPTURE_SINK))
}

/// Node id of the private capture sink, if it has registered.
pub fn capture_sink_node_id(dump: &serde_json::Value) -> Option<u64> {
    let arr = dump.as_array()?;
    arr.iter().find_map(|obj| {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            return None;
        }
        let props = obj.get("info")?.get("props")?;
        if props.get("node.name").and_then(|v| v.as_str()) == Some(CAPTURE_SINK) {
            obj.get("id").and_then(|v| v.as_u64())
        } else {
            None
        }
    })
}

/// Extract the capture sink's input (playback) port ids keyed by audio channel.
pub fn capture_sink_playback_ports(dump: &serde_json::Value) -> HashMap<String, u32> {
    let sink_node_id = capture_sink_node_id(dump);
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

/// One `Stream/Output/Audio` node as seen in `pw-dump`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppNode {
    pub node_id: u32,
    pub pid: Option<u32>,
    /// Normalised identity (binary name, else application.name, else node.name).
    pub identity: String,
    /// Label for the picker (application.name, else binary, else node.name).
    pub display: String,
    /// `info.state == "running"` — actually rendering right now.
    pub active: bool,
}

/// A link INTO the capture sink — i.e. one of our taps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TapLink {
    pub link_id: u32,
    pub out_node_id: u32,
}

fn prop_str<'a>(props: Option<&'a serde_json::Value>, key: &str) -> Option<&'a str> {
    props.and_then(|p| p.get(key)).and_then(|v| v.as_str()).filter(|s| !s.is_empty())
}

/// Every application output node in the dump. Identity / PID come from the
/// node's own props first and fall back to its Client object (`client.id`):
/// PipeWire-ALSA clients (e.g. Decibell's own CPAL output) carry
/// `pipewire.sec.pid` / `application.process.binary` only on the Client.
pub fn parse_app_nodes(dump: &serde_json::Value) -> Vec<AppNode> {
    let Some(arr) = dump.as_array() else { return Vec::new() };
    let clients: HashMap<u64, &serde_json::Value> = arr
        .iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Client"))
        .filter_map(|o| Some((o.get("id")?.as_u64()?, o.get("info")?.get("props")?)))
        .collect();

    let mut out = Vec::new();
    for obj in arr {
        if obj.get("type").and_then(|t| t.as_str()) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let info = obj.get("info");
        let props = info.and_then(|i| i.get("props"));
        if prop_str(props, "media.class") != Some("Stream/Output/Audio") {
            continue;
        }
        let Some(node_id) = obj.get("id").and_then(|v| v.as_u64()) else { continue };
        let client = props
            .and_then(|p| p.get("client.id"))
            .and_then(|v| v.as_u64())
            .and_then(|cid| clients.get(&cid).copied());

        let pid = parse_pid_value(props.and_then(|p| p.get("application.process.id")))
            .or_else(|| parse_pid_value(client.and_then(|c| c.get("pipewire.sec.pid"))))
            .or_else(|| parse_pid_value(client.and_then(|c| c.get("application.process.id"))));
        let binary = prop_str(props, "application.process.binary")
            .or_else(|| prop_str(client, "application.process.binary"));
        let app_name = prop_str(props, "application.name").or_else(|| prop_str(client, "application.name"));
        let node_name = prop_str(props, "node.name");

        let identity = binary
            .and_then(identity_from_exe_path)
            .or_else(|| app_name.map(normalize_identity).filter(|s| !s.is_empty()))
            .or_else(|| node_name.map(normalize_identity).filter(|s| !s.is_empty()));
        let Some(identity) = identity else { continue };
        let display = app_name
            .or(binary)
            .or(node_name)
            .unwrap_or(identity.as_str())
            .to_string();
        let active = info.and_then(|i| i.get("state")).and_then(|v| v.as_str()) == Some("running");
        out.push(AppNode { node_id: node_id as u32, pid, identity, display, active });
    }
    out
}

/// Links whose input side is the capture sink — our taps.
pub fn parse_tap_links(dump: &serde_json::Value, sink_node_id: u32) -> Vec<TapLink> {
    let Some(arr) = dump.as_array() else { return Vec::new() };
    arr.iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Link"))
        .filter_map(|o| {
            let info = o.get("info")?;
            let input = info.get("input-node-id")?.as_u64()? as u32;
            if input != sink_node_id {
                return None;
            }
            Some(TapLink {
                link_id: o.get("id")?.as_u64()? as u32,
                out_node_id: info.get("output-node-id")?.as_u64()? as u32,
            })
        })
        .collect()
}

/// `(node_id, port_id, audio.channel)` for every output port of `nodes`.
pub fn output_ports_of(dump: &serde_json::Value, nodes: &HashSet<u32>) -> Vec<(u32, u32, String)> {
    let Some(arr) = dump.as_array() else { return Vec::new() };
    arr.iter()
        .filter(|o| o.get("type").and_then(|t| t.as_str()) == Some("PipeWire:Interface:Port"))
        .filter_map(|o| {
            let props = o.get("info")?.get("props")?;
            if props.get("port.direction")?.as_str()? != "out" {
                return None;
            }
            let node_id = props.get("node.id")?.as_u64()? as u32;
            if !nodes.contains(&node_id) {
                return None;
            }
            let ch = props.get("audio.channel")?.as_str()?.to_string();
            Some((node_id, o.get("id")?.as_u64()? as u32, ch))
        })
        .collect()
}

/// Is this node Decibell's own output? PID in our process tree, or the same
/// program name as us (belt and braces for PID-less nodes).
fn is_self(node: &AppNode, decibell_pids: &HashSet<u32>, self_identity: &str) -> bool {
    node.pid.map(|p| decibell_pids.contains(&p)).unwrap_or(false)
        || (!self_identity.is_empty() && node.identity == self_identity)
}

/// Bring the tap links in line with `filter`: link every output port of an
/// allowed, non-Decibell app into the capture sink; unlink every existing
/// tap whose source is no longer allowed (or has become ours). One pw-dump,
/// then a few pw-link spawns. Idempotent — "File exists" is success.
fn reconcile_taps(
    decibell_pids: &HashSet<u32>,
    self_identity: &str,
    filter: &StreamAudioFilter,
    sink_node_id: u32,
    capture_ports: &HashMap<String, u32>,
) {
    let dump = match pw_dump_json() {
        Ok(d) => d,
        Err(e) => {
            log::warn!("[audio-capture] tap scan: {}", e);
            return;
        }
    };
    let desired: HashSet<u32> = parse_app_nodes(&dump)
        .into_iter()
        .filter(|n| !is_self(n, decibell_pids, self_identity) && filter.allows(&n.identity))
        .map(|n| n.node_id)
        .collect();

    for (_node, port_id, channel) in output_ports_of(&dump, &desired) {
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

    for link in parse_tap_links(&dump, sink_node_id) {
        if desired.contains(&link.out_node_id) {
            continue;
        }
        match Command::new("pw-link").arg("-d").arg(link.link_id.to_string()).output() {
            Ok(o) if o.status.success() => {
                log::info!("[audio-capture] Untapped node {} (link {})", link.out_node_id, link.link_id);
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                // Already gone (the app quit between dump and unlink) — fine.
                if !err.contains("No such") && !err.contains("ENOENT") {
                    log::warn!("[audio-capture] pw-link -d {} failed: {}", link.link_id, err.trim());
                }
            }
            Err(e) => log::warn!("[audio-capture] pw-link -d spawn failed: {}", e),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::stream_audio_filter::StreamAudioMode;

    /// Modelled on a live `pw-dump`: Firefox with two nodes (one running) whose
    /// PID/binary sit on the node; Decibell's own PipeWire-ALSA node whose
    /// PID/binary sit only on the Client object; the capture sink with its
    /// two playback ports; two existing tap links (one is the self-leak).
    fn fixture() -> serde_json::Value {
        serde_json::json!([
            { "id": 60, "type": "PipeWire:Interface:Client", "info": { "props": {
                "pipewire.sec.pid": 1748, "application.process.binary": "firefox", "application.name": "Firefox" } } },
            { "id": 124, "type": "PipeWire:Interface:Client", "info": { "props": {
                "pipewire.sec.pid": 180492, "application.process.binary": "decibell", "application.name": "PipeWire ALSA [decibell]" } } },
            { "id": 84, "type": "PipeWire:Interface:Node", "info": { "state": "idle", "props": {
                "media.class": "Stream/Output/Audio", "client.id": 60, "node.name": "Firefox",
                "application.process.id": 2002, "application.process.binary": "firefox", "application.name": "Firefox" } } },
            { "id": 100, "type": "PipeWire:Interface:Node", "info": { "state": "running", "props": {
                "media.class": "Stream/Output/Audio", "client.id": 60, "node.name": "Firefox",
                "application.process.id": "2002", "application.process.binary": "firefox", "application.name": "Firefox" } } },
            { "id": 106, "type": "PipeWire:Interface:Node", "info": { "state": "running", "props": {
                "media.class": "Stream/Output/Audio", "client.id": 124, "node.name": "alsa_playback.decibell",
                "application.name": "PipeWire ALSA [decibell]" } } },
            { "id": 150, "type": "PipeWire:Interface:Node", "info": { "state": "suspended", "props": {
                "media.class": "Stream/Output/Audio", "node.name": "spotify", "application.name": "Spotify",
                "application.process.id": 3000 } } },
            { "id": 200, "type": "PipeWire:Interface:Node", "info": { "state": "running", "props": {
                "media.class": "Audio/Sink", "node.name": "decibell_capture" } } },
            { "id": 201, "type": "PipeWire:Interface:Port", "info": { "props": { "node.id": 200, "port.direction": "in", "audio.channel": "FL" } } },
            { "id": 202, "type": "PipeWire:Interface:Port", "info": { "props": { "node.id": 200, "port.direction": "in", "audio.channel": "FR" } } },
            { "id": 85, "type": "PipeWire:Interface:Port", "info": { "props": { "node.id": 84, "port.direction": "out", "audio.channel": "FL" } } },
            { "id": 86, "type": "PipeWire:Interface:Port", "info": { "props": { "node.id": 84, "port.direction": "out", "audio.channel": "FR" } } },
            { "id": 87, "type": "PipeWire:Interface:Port", "info": { "props": { "node.id": 84, "port.direction": "in", "audio.channel": "FL" } } },
            { "id": 107, "type": "PipeWire:Interface:Port", "info": { "props": { "node.id": 106, "port.direction": "out", "audio.channel": "MONO" } } },
            { "id": 300, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 106, "output-port-id": 107, "input-node-id": 200, "input-port-id": 201 } },
            { "id": 301, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 84, "output-port-id": 85, "input-node-id": 200, "input-port-id": 201 } },
            { "id": 302, "type": "PipeWire:Interface:Link", "info": { "output-node-id": 200, "output-port-id": 203, "input-node-id": 400, "input-port-id": 401 } }
        ])
    }

    #[test]
    fn parses_nodes_with_client_fallback_and_state() {
        let nodes = parse_app_nodes(&fixture());
        let by_id: HashMap<u32, &AppNode> = nodes.iter().map(|n| (n.node_id, n)).collect();
        assert_eq!(nodes.len(), 4);

        let ff = by_id[&84];
        assert_eq!((ff.pid, ff.identity.as_str(), ff.display.as_str(), ff.active), (Some(2002), "firefox", "Firefox", false));
        assert!(by_id[&100].active);
        assert_eq!(by_id[&100].pid, Some(2002), "string-typed pid parses");

        // PID-less node → resolved through its Client object.
        let me = by_id[&106];
        assert_eq!((me.pid, me.identity.as_str(), me.active), (Some(180492), "decibell", true));

        // No binary anywhere → application.name is the identity.
        let sp = by_id[&150];
        assert_eq!((sp.pid, sp.identity.as_str(), sp.display.as_str(), sp.active), (Some(3000), "spotify", "Spotify", false));
    }

    #[test]
    fn self_detection_by_pid_tree_or_identity() {
        let nodes = parse_app_nodes(&fixture());
        let me = nodes.iter().find(|n| n.node_id == 106).unwrap();
        let ff = nodes.iter().find(|n| n.node_id == 84).unwrap();
        let tree: HashSet<u32> = [180492u32].into_iter().collect();
        assert!(is_self(me, &tree, ""));
        assert!(is_self(me, &HashSet::new(), "decibell"));
        assert!(!is_self(me, &HashSet::new(), "electron"));
        assert!(!is_self(ff, &tree, "decibell"));
    }

    #[test]
    fn sink_ports_links_and_output_ports() {
        let dump = fixture();
        assert_eq!(capture_sink_node_id(&dump), Some(200));
        let ports = capture_sink_playback_ports(&dump);
        assert_eq!((ports.get("FL"), ports.get("FR")), (Some(&201), Some(&202)));

        let mut links = parse_tap_links(&dump, 200);
        links.sort_by_key(|l| l.link_id);
        assert_eq!(links, vec![
            TapLink { link_id: 300, out_node_id: 106 },
            TapLink { link_id: 301, out_node_id: 84 },
        ]);

        let wanted: HashSet<u32> = [84u32, 106].into_iter().collect();
        let mut out = output_ports_of(&dump, &wanted);
        out.sort();
        assert_eq!(out, vec![(84, 85, "FL".into()), (84, 86, "FR".into()), (106, 107, "MONO".into())]);
    }

    #[test]
    fn desired_set_follows_filter_and_excludes_self() {
        let nodes = parse_app_nodes(&fixture());
        let tree: HashSet<u32> = [180492u32].into_iter().collect();
        let desired = |f: &StreamAudioFilter| -> HashSet<u32> {
            nodes.iter().filter(|n| !is_self(n, &tree, "decibell") && f.allows(&n.identity)).map(|n| n.node_id).collect()
        };
        let all = StreamAudioFilter::default();
        assert_eq!(desired(&all), [84u32, 100, 150].into_iter().collect());
        // Built the way the napi command builds it, so spelling is normalised.
        let only_spotify = StreamAudioFilter::from_args(Some("selected"), Some(&["Spotify".to_string()]));
        assert_eq!(only_spotify.mode, StreamAudioMode::Selected);
        assert_eq!(desired(&only_spotify), [150u32].into_iter().collect());
        let not_firefox = StreamAudioFilter::from_args(Some("all_except"), Some(&["Firefox".to_string()]));
        assert_eq!(desired(&not_firefox), [150u32].into_iter().collect());
    }
}

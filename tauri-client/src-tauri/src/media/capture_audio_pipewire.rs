use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

use super::capture::AudioFrame;

use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

struct AudioCaptureData {
    tx: SyncSender<AudioFrame>,
    format: spa::param::audio::AudioInfoRaw,
    channels: u32,
    sample_rate: u32,
    quit_mainloop: pw::main_loop::MainLoopWeak,
    frame_count: u64,
}

/// Start capturing system audio (loopback of default sink) minus Decibell's own output.
///
/// Returns a receiver for `AudioFrame`s and a cleanup closure that restores
/// Decibell's audio routing when called (must be called on stop).
///
/// Strategy:
/// 1. Find the default audio sink's monitor node via PipeWire registry
/// 2. Find ALL of Decibell's own playback nodes (by our PID) and redirect
///    each to a null-sink so our output doesn't appear in the loopback
///    capture. Decibell creates one CPAL output for voice and (when the
///    user picks a separate stream-output device) a second CPAL output
///    for stream audio — finding only the first leaks the other into
///    the capture.
/// 3. Spawn a small poller that catches nodes created *after* this point
///    (e.g. user changes output device mid-stream → CPAL builds a new
///    PipeWire node) and redirects them too.
/// 4. Capture the default sink's monitor as stereo f32 48kHz
/// 5. Cleanup: stop the poller and restore routing for every node we
///    ever redirected.
pub fn start_system_audio_capture() -> Result<(std::sync::mpsc::Receiver<AudioFrame>, Box<dyn FnOnce() + Send>), String> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<AudioFrame>(16);

    let our_pid = std::process::id();

    // Step 1: Find the default sink name and all current playback nodes
    let (default_sink_name, initial_node_ids) = find_default_sink_and_our_nodes(our_pid)?;

    // Step 2: Always create the null-sink even if no nodes exist yet —
    // the poller spawned below may discover one later (e.g. CPAL hasn't
    // built its output stream by the time we get here).
    let null_module_id = create_null_sink()?;

    // Track every node we've ever redirected so cleanup restores them all.
    let redirected: Arc<Mutex<HashSet<u32>>> = Arc::new(Mutex::new(HashSet::new()));
    {
        let mut set = redirected.lock().unwrap();
        for id in &initial_node_ids {
            if let Err(e) = redirect_node_to_sink(*id, "decibell_private") {
                eprintln!("[audio-capture] Failed to redirect node {}: {}", id, e);
            } else {
                set.insert(*id);
            }
        }
    }

    // Step 3: Poller for late-arriving Decibell playback nodes. Polls
    // every 2s. Cheap (one pw-dump invocation) and idempotent — only
    // redirects nodes we haven't already redirected.
    let poller_stop = Arc::new(AtomicBool::new(false));
    let poller_handle = {
        let stop = poller_stop.clone();
        let redirected = redirected.clone();
        std::thread::Builder::new()
            .name("decibell-self-exclude-poller".to_string())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                    if stop.load(Ordering::Relaxed) { break; }
                    let nodes = match find_our_playback_nodes(our_pid) {
                        Ok(n) => n,
                        Err(e) => {
                            eprintln!("[audio-capture] Poller scan failed: {}", e);
                            continue;
                        }
                    };
                    let mut set = redirected.lock().unwrap();
                    for id in nodes {
                        if set.contains(&id) { continue; }
                        match redirect_node_to_sink(id, "decibell_private") {
                            Ok(()) => {
                                eprintln!("[audio-capture] Poller redirected late node {}", id);
                                set.insert(id);
                            }
                            Err(e) => {
                                eprintln!("[audio-capture] Poller redirect of {} failed: {}", id, e);
                            }
                        }
                    }
                }
            })
            .map_err(|e| format!("Spawn self-exclude poller: {}", e))?
    };

    // Step 4: Find the default sink's monitor node ID for capture
    let monitor_target = match find_sink_monitor_target(&default_sink_name) {
        Ok(t) => t,
        Err(e) => {
            poller_stop.store(true, Ordering::Relaxed);
            let _ = poller_handle.join();
            for id in redirected.lock().unwrap().iter() {
                let _ = restore_node_routing(*id);
            }
            let _ = remove_null_sink(null_module_id);
            return Err(e);
        }
    };

    // Step 5: Start PipeWire capture stream targeting the monitor
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<(), String>>(1);

    std::thread::Builder::new()
        .name("decibell-audio-capture".to_string())
        .spawn(move || {
            match run_audio_capture_loop(tx, monitor_target, ready_tx.clone()) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("[audio-capture] Capture loop error: {}", e);
                    let _ = ready_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| format!("Spawn audio capture thread: {}", e))?;

    // Wait for the capture to be ready (or fail)
    let do_cleanup_on_err = |poller_stop: &Arc<AtomicBool>,
                             redirected: &Arc<Mutex<HashSet<u32>>>,
                             null_module_id: u32| {
        poller_stop.store(true, Ordering::Relaxed);
        for id in redirected.lock().unwrap().iter() {
            let _ = restore_node_routing(*id);
        }
        let _ = remove_null_sink(null_module_id);
    };
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            do_cleanup_on_err(&poller_stop, &redirected, null_module_id);
            return Err(e);
        }
        Err(_) => {
            do_cleanup_on_err(&poller_stop, &redirected, null_module_id);
            return Err("Timeout waiting for audio capture to start".to_string());
        }
    }

    // Build cleanup closure
    let cleanup_redirected = redirected.clone();
    let cleanup_poller_stop = poller_stop.clone();
    let cleanup = Box::new(move || {
        eprintln!("[audio-capture] Cleanup: restoring audio routing");
        cleanup_poller_stop.store(true, Ordering::Relaxed);
        for id in cleanup_redirected.lock().unwrap().iter() {
            let _ = restore_node_routing(*id);
        }
        let _ = remove_null_sink(null_module_id);
    }) as Box<dyn FnOnce() + Send>;

    Ok((rx, cleanup))
}

/// Find the default audio sink name and every Decibell-PID playback node
/// currently registered in PipeWire. Decibell may have multiple Stream/
/// Output/Audio nodes (voice + separate stream-output device, plus any
/// nodes from a previous CPAL stream that hasn't been GC'd yet) — the
/// caller must redirect all of them, not just the first.
fn find_default_sink_and_our_nodes(our_pid: u32) -> Result<(String, Vec<u32>), String> {
    let output = std::process::Command::new("wpctl")
        .args(["inspect", "@DEFAULT_AUDIO_SINK@"])
        .output()
        .map_err(|e| format!("wpctl inspect: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "wpctl inspect failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let inspect_out = String::from_utf8_lossy(&output.stdout);
    let mut default_sink_name = String::new();

    // Parse "node.name = ..." from wpctl output.
    // Lines may have a leading "* " prefix (indicating the default/active property).
    for line in inspect_out.lines() {
        let trimmed = line.trim().trim_start_matches("* ");
        if trimmed.starts_with("node.name") {
            if let Some(val) = trimmed.split('=').nth(1) {
                default_sink_name = val.trim().trim_matches('"').to_string();
                break;
            }
        }
    }

    if default_sink_name.is_empty() {
        return Err("Could not find default sink name".to_string());
    }

    eprintln!("[audio-capture] Default sink: {}", default_sink_name);

    let nodes = find_our_playback_nodes(our_pid).unwrap_or_else(|e| {
        eprintln!("[audio-capture] Initial node scan failed (poller will retry): {}", e);
        Vec::new()
    });
    if nodes.is_empty() {
        eprintln!("[audio-capture] No Decibell playback nodes found yet — poller will pick them up when they appear.");
    } else {
        eprintln!("[audio-capture] Found {} Decibell playback node(s): {:?}", nodes.len(), nodes);
    }

    Ok((default_sink_name, nodes))
}

/// Scan PipeWire for all Stream/Output/Audio nodes belonging to our PID.
/// Used both at startup and by the poller that catches nodes created
/// after capture begins (CPAL output device swap, audio stream restart,
/// etc.). Returns an empty vec if pw-dump fails — the caller should
/// retry on the next tick rather than abort.
fn find_our_playback_nodes(our_pid: u32) -> Result<Vec<u32>, String> {
    let pw_dump = std::process::Command::new("pw-dump")
        .output()
        .map_err(|e| format!("pw-dump: {}", e))?;
    if !pw_dump.status.success() {
        return Err("pw-dump failed".to_string());
    }
    let dump_str = String::from_utf8_lossy(&pw_dump.stdout);
    let dump: serde_json::Value = serde_json::from_str(&dump_str)
        .map_err(|e| format!("Parse pw-dump: {}", e))?;

    let our_pid_str = our_pid.to_string();
    let mut ids = Vec::new();

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
            let pid = props
                .get("application.process.id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let media_class = props
                .get("media.class")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if pid == our_pid_str && media_class == "Stream/Output/Audio" {
                if let Some(id) = obj.get("id").and_then(|v| v.as_u64()) {
                    ids.push(id as u32);
                }
            }
        }
    }
    Ok(ids)
}

/// Create a null-sink named "decibell_private" using pw-loopback or pactl.
fn create_null_sink() -> Result<u32, String> {
    // Use pactl to load a null-sink module — widely available and works with PipeWire's PulseAudio compat
    let output = std::process::Command::new("pactl")
        .args([
            "load-module",
            "module-null-sink",
            "sink_name=decibell_private",
            "sink_properties=device.description=Decibell_Private",
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

    eprintln!("[audio-capture] Created null-sink module {}", module_id);
    Ok(module_id)
}

/// Redirect a PipeWire node's output to a specific sink by name.
fn redirect_node_to_sink(node_id: u32, sink_name: &str) -> Result<(), String> {
    // Use pw-metadata to set the target.node for our stream
    let output = std::process::Command::new("pw-metadata")
        .args([
            "-n",
            "default",
            &node_id.to_string(),
            "target.node",
            &format!("{{ \"name\": \"{}\" }}", sink_name),
            "Spa:String:JSON",
        ])
        .output()
        .map_err(|e| format!("pw-metadata set target.node: {}", e))?;

    if !output.status.success() {
        // Fallback: try pactl move-sink-input
        eprintln!(
            "[audio-capture] pw-metadata failed, trying pactl move-sink-input: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let pactl_out = std::process::Command::new("pactl")
            .args([
                "move-sink-input",
                &node_id.to_string(),
                sink_name,
            ])
            .output()
            .map_err(|e| format!("pactl move-sink-input: {}", e))?;

        if !pactl_out.status.success() {
            return Err(format!(
                "Failed to redirect node {} to {}: {}",
                node_id,
                sink_name,
                String::from_utf8_lossy(&pactl_out.stderr)
            ));
        }
    }

    eprintln!(
        "[audio-capture] Redirected node {} to sink '{}'",
        node_id, sink_name
    );
    Ok(())
}

/// Restore a node's routing by clearing the target.node metadata override.
fn restore_node_routing(node_id: u32) -> Result<(), String> {
    // Clear the metadata override
    let output = std::process::Command::new("pw-metadata")
        .args([
            "-n",
            "default",
            "-d",
            &node_id.to_string(),
            "target.node",
        ])
        .output()
        .map_err(|e| format!("pw-metadata delete: {}", e))?;

    if !output.status.success() {
        // Fallback: move back to default sink using pactl
        let _ = std::process::Command::new("pactl")
            .args([
                "move-sink-input",
                &node_id.to_string(),
                "@DEFAULT_SINK@",
            ])
            .output();
    }

    eprintln!("[audio-capture] Restored routing for node {}", node_id);
    Ok(())
}

/// Remove the null-sink module.
fn remove_null_sink(module_id: u32) -> Result<(), String> {
    let output = std::process::Command::new("pactl")
        .args(["unload-module", &module_id.to_string()])
        .output()
        .map_err(|e| format!("pactl unload-module: {}", e))?;

    if !output.status.success() {
        eprintln!(
            "[audio-capture] Warning: failed to remove null-sink module {}: {}",
            module_id,
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        eprintln!("[audio-capture] Removed null-sink module {}", module_id);
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
                    eprintln!("[audio-capture] Found sink '{}' at node {}", sink_name, id);
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
            eprintln!("[audio-capture] Stream: {:?} -> {:?}", old, new);
            match &new {
                pw::stream::StreamState::Error(msg) => {
                    eprintln!("[audio-capture] Stream error: {}", msg);
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

            data.format
                .parse(param)
                .expect("Failed to parse AudioInfoRaw");

            data.channels = data.format.channels();
            data.sample_rate = data.format.rate();

            eprintln!(
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
            let raw_data = &raw_data[chunk_offset..][..chunk_size];

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
                        eprintln!("[audio-capture] Unsupported audio format: {:?}", format);
                    }
                    data.frame_count += 1;
                    return;
                }
            };

            data.frame_count += 1;
            if data.frame_count == 1 || data.frame_count % 2400 == 0 {
                eprintln!(
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
                    eprintln!("[audio-capture] Channel closed, stopping");
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

    eprintln!("[audio-capture] PipeWire stream connected, running main loop");
    mainloop.run();
    eprintln!("[audio-capture] PipeWire main loop exited");

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

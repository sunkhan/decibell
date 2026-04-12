//! Linux-only helpers to route *our app's* PulseAudio/PipeWire streams to a
//! specific sink/source without changing the system-wide default — other apps
//! stay on whatever sink/source they're already using.

#![cfg(target_os = "linux")]

use std::process::Command;

/// Move all sink-inputs (playback streams) owned by our process to `target_sink`.
pub fn route_outputs_to(target_sink: &str) {
    move_streams("sink-inputs", "move-sink-input", target_sink);
}

/// Move all source-outputs (capture streams) owned by our process to `target_source`.
pub fn route_inputs_to(target_source: &str) {
    move_streams("source-outputs", "move-source-output", target_source);
}

fn move_streams(list_kind: &str, move_verb: &str, target: &str) {
    let output = match Command::new("pactl").args(["list", list_kind]).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(_) => return,
    };

    let block_prefix = if list_kind == "sink-inputs" {
        "Sink Input #"
    } else {
        "Source Output #"
    };
    let our_pid = std::process::id().to_string();

    let mut stream_id: Option<String> = None;
    let mut stream_pid: Option<String> = None;

    let mut flush = |id: Option<String>, pid: Option<String>| {
        if let (Some(id), Some(pid)) = (id, pid) {
            if pid == our_pid {
                let _ = Command::new("pactl").args([move_verb, &id, target]).status();
            }
        }
    };

    for line in output.lines() {
        if let Some(rest) = line.strip_prefix(block_prefix) {
            flush(stream_id.take(), stream_pid.take());
            stream_id = Some(rest.trim().to_string());
            stream_pid = None;
        } else {
            let t = line.trim();
            if let Some(rest) = t.strip_prefix("application.process.id = ") {
                stream_pid = Some(rest.trim_matches('"').to_string());
            }
        }
    }
    flush(stream_id, stream_pid);
}

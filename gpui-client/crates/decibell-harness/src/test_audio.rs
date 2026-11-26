//! Sound in self-tests without a sound: a PipeWire / PulseAudio **null sink** to play on and its
//! monitor source to record from, so a test of the interface sounds or the microphone test never
//! reaches the desktop's real output device or microphone (the owner may be sitting at it).
//!
//! [`NullSink::load`] makes one (`pactl load-module module-null-sink`) and unloads it when
//! dropped; the default devices are never touched (checked on load and again on drop). Hand its
//! name to the app's child in `DECIBELL_CUE_SINK` (`decibell-app/src/audio.rs`), **unload it only
//! after every process that played on it has gone** (a stream whose sink disappears is moved to
//! the default output by the sound server: the parent test owns the sink and outlives the child).
//! [`streams_of`] says where a process's streams really sit, for the checks to assert it.
//!
//! Linux with `pactl` only; elsewhere [`NullSink::load`] fails and the tests skip.

use std::process::Command;

use anyhow::{Context as _, Result, bail, ensure};

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).output().context("running pactl (is PipeWire / PulseAudio installed?)")?;
    ensure!(out.status.success(), "pactl {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The desktop's default sink and source (never changed by a test).
pub fn defaults() -> Result<(String, String)> {
    Ok((pactl(&["get-default-sink"])?.trim().to_string(), pactl(&["get-default-source"])?.trim().to_string()))
}

/// A null sink, unloaded when dropped.
pub struct NullSink {
    name: String,
    module: u32,
    defaults: (String, String),
}

impl NullSink {
    /// `decibell-selftest-<pid><tag>`; fails where there is no `pactl`.
    pub fn load(tag: &str) -> Result<NullSink> {
        ensure!(cfg!(target_os = "linux"), "null sinks are made with pactl (Linux only)");
        let defaults = defaults()?;
        let name = format!("decibell-selftest-{}{tag}", std::process::id());
        let module = pactl(&[
            "load-module",
            "module-null-sink",
            &format!("sink_name={name}"),
            "rate=48000",
            "channels=2",
            &format!("sink_properties=device.description={name}"),
        ])?;
        let module: u32 = module.trim().parse().with_context(|| format!("pactl answered {module:?}, not a module id"))?;
        let sink = NullSink { name, module, defaults };
        // Loading a sink must not have moved the desktop's defaults (dropping `sink` unloads it).
        let now = self::defaults()?;
        ensure!(now == sink.defaults, "the default devices moved when the null sink loaded: {:?} → {now:?}", sink.defaults);
        Ok(sink)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The source that records what plays on the sink.
    pub fn monitor(&self) -> String {
        format!("{}.monitor", self.name)
    }

    pub fn module(&self) -> u32 {
        self.module
    }

    /// Unload now (dropping does the same) and say whether the module went.
    pub fn unload(self) -> Result<()> {
        let module = self.module;
        pactl(&["unload-module", &module.to_string()])?;
        let (name, defaults) = (self.name.clone(), self.defaults.clone());
        std::mem::forget(self);
        ensure!(!pactl(&["list", "short", "sinks"])?.contains(&name), "{name} is still listed after unloading module {module}");
        let now = self::defaults()?;
        ensure!(now == defaults, "the default devices moved while the null sink existed: {defaults:?} → {now:?}");
        Ok(())
    }
}

impl Drop for NullSink {
    fn drop(&mut self) {
        if let Err(e) = pactl(&["unload-module", &self.module.to_string()]) {
            eprintln!("decibell: could not unload null sink {} (module {}): {e:#}", self.name, self.module);
        }
    }
}

/// Where a process's audio streams sit: the sinks its playback streams play on and the sources
/// its capture streams record from, by name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Streams {
    pub sinks: Vec<String>,
    pub sources: Vec<String>,
    /// Streams the sound server has not linked to a device yet (just opened: they carry
    /// nothing until they are).
    pub unlinked: usize,
}

/// The device index of a stream that is not linked yet (`PA_INVALID_INDEX`).
const UNLINKED: &str = "4294967295";

fn names(kind: &str) -> Result<Vec<(String, String)>> {
    Ok(pactl(&["list", "short", kind])?
        .lines()
        .filter_map(|l| {
            let mut cols = l.split('\t');
            Some((cols.next()?.to_string(), cols.next()?.to_string()))
        })
        .collect())
}

/// The `"key":"value"` or `"key":number` after `key` in one JSON object's text.
fn field<'a>(object: &'a str, key: &str) -> Option<&'a str> {
    let at = object.find(&format!("\"{key}\":"))? + key.len() + 3;
    let rest = &object[at..];
    match rest.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next(),
        None => rest.split([',', '}']).next(),
    }
}

fn targets(listing: &str, device_key: &str, pid: u32, names: &[(String, String)]) -> Vec<String> {
    let pid = pid.to_string();
    listing
        .split("{\"index\":")
        .skip(1)
        .filter(|object| field(object, "application.process.id") == Some(pid.as_str()))
        .map(|object| {
            let index = field(object, device_key).unwrap_or("?");
            names.iter().find(|(i, _)| i == index).map_or_else(|| format!("#{index}"), |(_, name)| name.clone())
        })
        .collect()
}

fn split_unlinked(all: Vec<String>) -> (Vec<String>, usize) {
    let unlinked = format!("#{UNLINKED}");
    let (pending, linked): (Vec<String>, Vec<String>) = all.into_iter().partition(|name| *name == unlinked);
    (linked, pending.len())
}

/// The streams of process `pid` as the sound server has them now.
pub fn streams_of(pid: u32) -> Result<Streams> {
    let sinks = pactl(&["-f", "json", "list", "sink-inputs"])?;
    let sources = pactl(&["-f", "json", "list", "source-outputs"])?;
    let (sinks, pending_out) = split_unlinked(targets(&sinks, "sink", pid, &names("sinks")?));
    let (sources, pending_in) = split_unlinked(targets(&sources, "source", pid, &names("sources")?));
    Ok(Streams { sinks, sources, unlinked: pending_out + pending_in })
}

/// Fails unless every linked stream of this process plays on the sink named `sink` / records
/// from its monitor: the assertion that a test's sound stayed off the real devices.
pub fn ensure_only_on(sink: &str) -> Result<Streams> {
    let streams = streams_of(std::process::id())?;
    if let Some(other) = streams.sinks.iter().find(|s| *s != sink) {
        bail!("a playback stream of this process is on {other}, not on the test sink {sink}");
    }
    let monitor = format!("{sink}.monitor");
    if let Some(other) = streams.sources.iter().find(|s| **s != monitor) {
        bail!("a capture stream of this process records {other}, not the test sink's monitor");
    }
    Ok(streams)
}

/// The names of this process's threads (`/proc/self/task/*/comm`; empty off Linux).
pub fn thread_names() -> Vec<String> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else { return Vec::new() };
    tasks
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .map(|name| name.trim().to_string())
        .collect()
}

/// A thread whose name starts with `prefix` exists (Linux keeps 15 bytes of a thread's name:
/// `decibell-audio-devices` reads `decibell-audio-`).
pub fn has_thread(prefix: &str) -> bool {
    let prefix = &prefix[..prefix.len().min(15)];
    thread_names().iter().any(|name| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_streams_are_found_by_pid_and_named() {
        let listing = r#"[{"index":3787,"driver":"PipeWire","client":"112","sink":54,"corked":true,"properties":{"application.name":"Firefox","application.process.id":"2051","node.name":"Firefox"}},{"index":3790,"client":"115","sink":812,"properties":{"application.process.id":"777"}},{"index":3791,"client":"115","sink":54,"properties":{"application.process.id":"777"}}]"#;
        let names = vec![("54".to_string(), "speakers".to_string()), ("812".to_string(), "null".to_string())];
        assert_eq!(targets(listing, "sink", 777, &names), vec!["null".to_string(), "speakers".to_string()]);
        assert_eq!(targets(listing, "sink", 2051, &names), vec!["speakers".to_string()]);
        assert!(targets(listing, "sink", 1, &names).is_empty());
        assert!(targets("[]", "sink", 777, &names).is_empty());
        // A stream opened a moment ago has no device yet.
        let opening = r#"[{"index":9,"sink":4294967295,"properties":{"application.process.id":"777"}}]"#;
        assert_eq!(split_unlinked(targets(opening, "sink", 777, &names)), (Vec::new(), 1));
        assert_eq!(split_unlinked(targets(listing, "sink", 777, &names)).1, 0);
    }
}

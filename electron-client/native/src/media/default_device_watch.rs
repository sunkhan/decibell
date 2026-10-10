//! Follows the OS default audio devices for a pipeline whose selection is
//! "Default".
//!
//! On Windows (WASAPI) and macOS (CoreAudio) a stream is opened on the
//! endpoint that was the default *at that moment* and stays on it: switch
//! the default (plug in a headset, change it in Sound settings) and
//! Decibell kept talking to the old one. PipeWire / PulseAudio move
//! unpinned streams to the new default by themselves, so Linux needs none
//! of this and gets no watcher.
//!
//! A thread asks for the current defaults about once a second and, when
//! one is a different device than before, sends the pipeline
//! `DefaultInputChanged` / `DefaultOutputChanged`. The pipeline re-opens
//! only what follows the default (an explicitly chosen device stays put),
//! through the same hot-swap as picking a device in Settings. No signal
//! for anything else, so device-list churn (wireless headsets, virtual
//! sinks coming and going) never swaps a stream mid-call.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use super::pipeline::ControlMessage;

#[cfg_attr(target_os = "linux", allow(dead_code))]
const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct DefaultDeviceWatch {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DefaultDeviceWatch {
    /// Start following the defaults for one pipeline. None on Linux.
    pub fn start(control_tx: Sender<ControlMessage>) -> Option<Self> {
        #[cfg(target_os = "linux")]
        {
            let _ = control_tx;
            None
        }
        #[cfg(not(target_os = "linux"))]
        {
            Self::spawn(make_probe, POLL_INTERVAL, move |input| {
                let msg = if input {
                    ControlMessage::DefaultInputChanged
                } else {
                    ControlMessage::DefaultOutputChanged
                };
                control_tx.send(msg).is_ok()
            })
        }
    }

    /// `make_probe` runs on the watcher thread (COM objects aren't `Send`)
    /// and returns the probe: `probe(input)` → an identity for the current
    /// default capture (`true`) / render (`false`) device. `on_change(input)`
    /// returns false once nobody is listening, which ends the thread.
    #[cfg_attr(target_os = "linux", allow(dead_code))]
    fn spawn<M, P, C>(make_probe: M, interval: Duration, on_change: C) -> Option<Self>
    where
        M: FnOnce() -> P + Send + 'static,
        P: FnMut(bool) -> Option<String>,
        C: Fn(bool) -> bool + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("decibell-default-device".into())
            .spawn(move || {
                let mut probe = make_probe();
                let mut last = [probe(true), probe(false)];
                while !stop_thread.load(Ordering::Acquire) {
                    std::thread::park_timeout(interval);
                    if stop_thread.load(Ordering::Acquire) {
                        break;
                    }
                    for (i, input) in [true, false].into_iter().enumerate() {
                        let now = probe(input);
                        // No default at all (the last device just went away):
                        // nothing to move to yet; wait for the next one.
                        if now.is_some() && now != last[i] {
                            log::info!(
                                "[audio] default {} device changed: {:?} -> {:?}",
                                if input { "input" } else { "output" },
                                last[i],
                                now
                            );
                            last[i] = now;
                            if !on_change(input) {
                                return;
                            }
                        }
                    }
                }
            })
            .map_err(|e| log::warn!("[audio] default-device watcher not started: {}", e))
            .ok()?;
        Some(Self { stop, thread: Some(thread) })
    }
}

impl Drop for DefaultDeviceWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

/// Windows: endpoint IDs of the default communications and default
/// console device per direction — what `get_default_device` picks from
/// (it prefers the communications device). An ID is exact where friendly
/// names can repeat. One enumerator for the thread's lifetime.
#[cfg(target_os = "windows")]
fn make_probe() -> impl FnMut(bool) -> Option<String> {
    use windows::Win32::Media::Audio::{
        eCapture, eCommunications, eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let enumerator: Option<IMMDeviceEnumerator> =
        unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.ok();
    move |input| {
        let enumerator = enumerator.as_ref()?;
        let flow = if input { eCapture } else { eRender };
        let id = |role| -> Option<String> {
            unsafe {
                let device = enumerator.GetDefaultAudioEndpoint(flow, role).ok()?;
                let raw = device.GetId().ok()?;
                let id = raw.to_string().ok();
                CoTaskMemFree(Some(raw.0 as *const _));
                id
            }
        };
        let (comms, console) = (id(eCommunications), id(eConsole));
        if comms.is_none() && console.is_none() {
            return None;
        }
        Some(format!("{}|{}", comms.unwrap_or_default(), console.unwrap_or_default()))
    }
}

/// macOS (and anything else CPAL-backed): the default device's name.
#[cfg(all(not(target_os = "windows"), not(target_os = "linux")))]
fn make_probe() -> impl FnMut(bool) -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    move |input| {
        let device = if input { host.default_input_device() } else { host.default_output_device() };
        device.and_then(|d| d.name().ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Drives the watcher with scripted defaults and records what it sends.
    #[test]
    fn signals_only_real_changes_per_direction() {
        // Each probe call pops the next scripted identity for that side.
        let script_in = vec![Some("mic-a"), Some("mic-a"), Some("mic-b"), None, Some("mic-b"), Some("mic-c")];
        let script_out = vec![Some("spk-a"), Some("spk-b"), Some("spk-b"), Some("spk-b"), Some("spk-b"), Some("spk-b")];
        let fired = Arc::new(Mutex::new(Vec::new()));
        let fired_cb = Arc::clone(&fired);
        let watch = DefaultDeviceWatch::spawn(
            move || {
                let (mut i, mut o) = (script_in.into_iter(), script_out.into_iter());
                move |input: bool| {
                    let next = if input { i.next() } else { o.next() };
                    next.flatten().map(str::to_string)
                }
            },
            Duration::from_millis(5),
            move |input| {
                fired_cb.lock().unwrap().push(input);
                true
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));
        drop(watch);
        // Baseline mic-a / spk-a. Then: out → spk-b; in → mic-b; in None
        // (ignored, no device to move to); in mic-b again (no change);
        // in → mic-c. Exhausted script reads None afterwards: ignored.
        assert_eq!(*fired.lock().unwrap(), vec![false, true, true]);
    }

    #[test]
    fn stops_when_nobody_listens() {
        let calls = Arc::new(Mutex::new(0));
        let calls_cb = Arc::clone(&calls);
        let mut n = 0;
        let watch = DefaultDeviceWatch::spawn(
            move || {
                move |_input: bool| {
                    n += 1;
                    Some(n.to_string())
                }
            },
            Duration::from_millis(5),
            move |_| {
                *calls_cb.lock().unwrap() += 1;
                false
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(*calls.lock().unwrap(), 1, "the first refused send ends the thread");
        drop(watch);
    }
}

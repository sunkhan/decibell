//! Audio devices appearing and disappearing, from the platform's own
//! notifications: never a periodic re-enumeration.
//!
//! A client that shows device lists subscribes while it shows them
//! ([`subscribe`] / [`unsubscribe`], ref-counted) and gets
//! `CoreEvent::AudioDevicesChanged` (`audio_devices_changed`, no payload)
//! whenever a capture or playback device was added or removed, or a default
//! device changed; it then asks `list_audio_devices` again. Several events can
//! come for one plug (a device is several objects): the client coalesces them.
//! Electron's renderer has Chromium's `devicechange` for this and never
//! subscribes.
//!
//! - Linux: a PipeWire registry listener (the library the stream-audio
//!   capture already uses) on its own thread, asleep in PipeWire's loop
//!   between graph changes. Without PipeWire: `pactl subscribe`, the
//!   PulseAudio server's event stream (the device list itself comes from
//!   `pactl`), read by a thread asleep on its pipe.
//! - Windows: `IMMNotificationClient` on the device enumerator.
//! - macOS: CoreAudio property listeners on the system object.
//!
//! The thread (`decibell-audio-devices`) exists only while someone is
//! subscribed. [`OsNotifications`] (Windows / macOS) is also what wakes
//! `default_device_watch`, which follows the default device for a live
//! session.

use std::sync::Mutex;

/// What changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) enum Change {
    /// A device was added or removed, or its state changed.
    Devices,
    DefaultInput,
    DefaultOutput,
}

struct Watch {
    subscribers: usize,
    listener: Option<Listener>,
}

static WATCH: Mutex<Watch> = Mutex::new(Watch { subscribers: 0, listener: None });

/// Start emitting `AudioDevicesChanged` (if not already). Each call is
/// undone by one [`unsubscribe`]. False when this platform's notifications
/// couldn't be started (the list then only updates when asked for).
pub fn subscribe() -> bool {
    let mut w = WATCH.lock().unwrap_or_else(|e| e.into_inner());
    w.subscribers += 1;
    if w.listener.is_none() {
        w.listener = Listener::start(|| crate::events::emit_audio_devices_changed());
    }
    w.listener.is_some()
}

/// Undo one [`subscribe`]; the last one ends the listener and its thread.
pub fn unsubscribe() {
    let listener = {
        let mut w = WATCH.lock().unwrap_or_else(|e| e.into_inner());
        w.subscribers = w.subscribers.saturating_sub(1);
        if w.subscribers == 0 { w.listener.take() } else { None }
    };
    drop(listener); // joins its thread, outside the lock
}

// ── Linux ────────────────────────────────────────────────────────────────────

/// A PipeWire node of this media class is a device `pactl list sinks /
/// sources` shows (streams are `Stream/…`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_device_class(media_class: &str) -> bool {
    media_class.starts_with("Audio/Sink") || media_class.starts_with("Audio/Source") || media_class == "Audio/Duplex"
}

/// A line of `pactl subscribe` that means the device list changed:
/// `Event 'new' on sink #57`, `Event 'remove' on source #58`, or a `change`
/// on the server (its default sink / source). Stream events (`sink-input`,
/// `source-output`) and `change` on a device (volume) are not.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn pactl_line_is_device_change(line: &str) -> bool {
    let Some(rest) = line.trim().strip_prefix("Event '") else { return false };
    let Some((kind, rest)) = rest.split_once("' on ") else { return false };
    let object = rest.split_whitespace().next().unwrap_or("");
    match kind {
        "new" | "remove" => object == "sink" || object == "source",
        "change" => object == "server",
        _ => false,
    }
}

#[cfg(target_os = "linux")]
use linux::Listener;

#[cfg(target_os = "linux")]
mod linux {
    use super::{is_device_class, pactl_line_is_device_change};
    use pipewire as pw;
    use std::cell::{Cell, RefCell};
    use std::collections::HashSet;
    use std::io::BufRead;
    use std::rc::Rc;
    use std::sync::mpsc;
    use std::thread::JoinHandle;

    pub(super) enum Listener {
        PipeWire { quit: pw::channel::Sender<()>, thread: Option<JoinHandle<()>> },
        Pactl { child: std::process::Child, thread: Option<JoinHandle<()>> },
    }

    impl Listener {
        pub(super) fn start(changed: impl Fn() + Send + Clone + 'static) -> Option<Self> {
            let (quit, quit_rx) = pw::channel::channel::<()>();
            let (ready_tx, ready_rx) = mpsc::channel::<bool>();
            let on_change = changed.clone();
            let thread = std::thread::Builder::new()
                .name("decibell-audio-devices".into())
                .spawn(move || {
                    if let Err(e) = run_pipewire(quit_rx, &ready_tx, on_change) {
                        log::info!("[audio-devices] PipeWire registry not available: {}", e);
                        let _ = ready_tx.send(false);
                    }
                })
                .ok()?;
            if ready_rx.recv().unwrap_or(false) {
                return Some(Listener::PipeWire { quit, thread: Some(thread) });
            }
            let _ = thread.join();
            // No PipeWire: the PulseAudio server's event stream.
            let mut child = std::process::Command::new("pactl")
                .arg("subscribe")
                .env("LC_ALL", "C")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| log::info!("[audio-devices] pactl subscribe not available: {}", e))
                .ok()?;
            let out = child.stdout.take()?;
            let thread = std::thread::Builder::new()
                .name("decibell-audio-devices".into())
                .spawn(move || {
                    for line in std::io::BufReader::new(out).lines() {
                        let Ok(line) = line else { break };
                        if pactl_line_is_device_change(&line) {
                            changed();
                        }
                    }
                })
                .ok();
            Some(Listener::Pactl { child, thread })
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            match self {
                Listener::PipeWire { quit, thread } => {
                    let _ = quit.send(());
                    if let Some(t) = thread.take() {
                        let _ = t.join();
                    }
                }
                Listener::Pactl { child, thread } => {
                    let _ = child.kill();
                    let _ = child.wait();
                    if let Some(t) = thread.take() {
                        let _ = t.join();
                    }
                }
            }
        }
    }

    fn run_pipewire(quit_rx: pw::channel::Receiver<()>, ready: &mpsc::Sender<bool>, changed: impl Fn() + Clone + 'static) -> Result<(), String> {
        pw::init();
        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|e| format!("main loop: {e:?}"))?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| format!("context: {e:?}"))?;
        let core = context.connect_rc(None).map_err(|e| format!("connect: {e:?}"))?;
        let registry = core.get_registry_rc().map_err(|e| format!("registry: {e:?}"))?;

        let _quit = quit_rx.attach(mainloop.loop_(), {
            let ml = mainloop.downgrade();
            move |()| {
                if let Some(ml) = ml.upgrade() {
                    ml.quit();
                }
            }
        });

        // The device nodes there are. Everything that exists when we connect
        // is announced first: those are not changes (`primed`, set by the
        // `done` of the sync sent after the listener is in place).
        let known: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));
        let primed = Rc::new(Cell::new(false));
        let _registry_listener = registry
            .add_listener_local()
            .global({
                let (known, primed, changed) = (known.clone(), primed.clone(), changed.clone());
                move |global| {
                    if global.type_ != pw::types::ObjectType::Node {
                        return;
                    }
                    let is_device = global.props.and_then(|p| p.get("media.class")).is_some_and(is_device_class);
                    if is_device && known.borrow_mut().insert(global.id) && primed.get() {
                        changed();
                    }
                }
            })
            .global_remove({
                let (known, changed) = (known.clone(), changed.clone());
                move |id| {
                    if known.borrow_mut().remove(&id) {
                        changed();
                    }
                }
            })
            .register();
        let pending = core.sync(0).map_err(|e| format!("sync: {e:?}"))?;
        let _core_listener = core
            .add_listener_local()
            .done({
                let primed = primed.clone();
                move |id, seq| {
                    if id == pw::core::PW_ID_CORE && seq == pending {
                        primed.set(true);
                    }
                }
            })
            .error({
                let ml = mainloop.downgrade();
                move |id, _seq, res, message| {
                    if id == pw::core::PW_ID_CORE {
                        log::warn!("[audio-devices] PipeWire connection error {res}: {message}; device changes are no longer reported");
                        if let Some(ml) = ml.upgrade() {
                            ml.quit();
                        }
                    }
                }
            })
            .register();
        let _ = ready.send(true);
        mainloop.run();
        Ok(())
    }
}

// ── Windows / macOS ──────────────────────────────────────────────────────────

/// Device notifications there: a thread that registers with the OS and then
/// sleeps until the listener is dropped (the registration lives on it; COM
/// objects stay on the thread that made them).
#[cfg(not(target_os = "linux"))]
struct Listener {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_os = "linux"))]
impl Listener {
    fn start(changed: impl Fn() + Send + Sync + Clone + 'static) -> Option<Self> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_thread = std::sync::Arc::clone(&stop);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
        let thread = std::thread::Builder::new()
            .name("decibell-audio-devices".into())
            .spawn(move || {
                let notifications = OsNotifications::start(move |_change| changed());
                let _ = ready_tx.send(notifications.is_some());
                if notifications.is_none() {
                    return;
                }
                while !stop_thread.load(Ordering::Acquire) {
                    std::thread::park();
                }
                drop(notifications);
            })
            .ok()?;
        if !ready_rx.recv().unwrap_or(false) {
            let _ = thread.join();
            return None;
        }
        Some(Listener { stop, thread: Some(thread) })
    }
}

#[cfg(not(target_os = "linux"))]
impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(t) = self.thread.take() {
            t.thread().unpark();
            let _ = t.join();
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) use windows_impl::OsNotifications;

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::Change;
    use windows::core::{implement, PCWSTR};
    use windows::Win32::Foundation::PROPERTYKEY;
    use windows::Win32::Media::Audio::{
        eCapture, EDataFlow, ERole, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
        MMDeviceEnumerator, DEVICE_STATE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    /// The endpoint notifications of the device enumerator. Its methods are
    /// called on a system thread; they only hand the change on.
    #[implement(IMMNotificationClient)]
    struct Client {
        on: Box<dyn Fn(Change) + Send + Sync>,
    }

    impl IMMNotificationClient_Impl for Client_Impl {
        fn OnDeviceStateChanged(&self, _device: &PCWSTR, _state: DEVICE_STATE) -> windows::core::Result<()> {
            (self.on)(Change::Devices);
            Ok(())
        }
        fn OnDeviceAdded(&self, _device: &PCWSTR) -> windows::core::Result<()> {
            (self.on)(Change::Devices);
            Ok(())
        }
        fn OnDeviceRemoved(&self, _device: &PCWSTR) -> windows::core::Result<()> {
            (self.on)(Change::Devices);
            Ok(())
        }
        fn OnDefaultDeviceChanged(&self, flow: EDataFlow, _role: ERole, _device: &PCWSTR) -> windows::core::Result<()> {
            (self.on)(if flow == eCapture { Change::DefaultInput } else { Change::DefaultOutput });
            Ok(())
        }
        fn OnPropertyValueChanged(&self, _device: &PCWSTR, _key: &PROPERTYKEY) -> windows::core::Result<()> {
            Ok(())
        }
    }

    /// Registered while it lives. Made and dropped on one thread.
    pub(crate) struct OsNotifications {
        enumerator: IMMDeviceEnumerator,
        client: IMMNotificationClient,
    }

    impl OsNotifications {
        pub(crate) fn start(on: impl Fn(Change) + Send + Sync + 'static) -> Option<Self> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
                let client: IMMNotificationClient = Client { on: Box::new(on) }.into();
                enumerator
                    .RegisterEndpointNotificationCallback(&client)
                    .map_err(|e| log::warn!("[audio-devices] endpoint notifications not registered: {}", e))
                    .ok()?;
                Some(OsNotifications { enumerator, client })
            }
        }
    }

    impl Drop for OsNotifications {
        fn drop(&mut self) {
            unsafe {
                let _ = self.enumerator.UnregisterEndpointNotificationCallback(&self.client);
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use macos_impl::OsNotifications;

#[cfg(target_os = "macos")]
mod macos_impl {
    //! CoreAudio's property listeners on the system object, declared here
    //! (three functions of the CoreAudio framework; no binding crate).
    use super::Change;
    use std::ffi::c_void;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    type ListenerProc = unsafe extern "C" fn(object: u32, count: u32, addresses: *const AudioObjectPropertyAddress, client: *mut c_void) -> i32;

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectAddPropertyListener(object: u32, address: *const AudioObjectPropertyAddress, listener: ListenerProc, client: *mut c_void) -> i32;
        fn AudioObjectRemovePropertyListener(object: u32, address: *const AudioObjectPropertyAddress, listener: ListenerProc, client: *mut c_void) -> i32;
    }

    const fn fourcc(c: &[u8; 4]) -> u32 {
        ((c[0] as u32) << 24) | ((c[1] as u32) << 16) | ((c[2] as u32) << 8) | c[3] as u32
    }
    const SYSTEM_OBJECT: u32 = 1; // kAudioObjectSystemObject
    const DEVICES: u32 = fourcc(b"dev#"); // kAudioHardwarePropertyDevices
    const DEFAULT_INPUT: u32 = fourcc(b"dIn "); // kAudioHardwarePropertyDefaultInputDevice
    const DEFAULT_OUTPUT: u32 = fourcc(b"dOut"); // kAudioHardwarePropertyDefaultOutputDevice
    const SCOPE_GLOBAL: u32 = fourcc(b"glob"); // kAudioObjectPropertyScopeGlobal
    const ELEMENT_MAIN: u32 = 0; // kAudioObjectPropertyElementMain
    const SELECTORS: [u32; 3] = [DEVICES, DEFAULT_INPUT, DEFAULT_OUTPUT];

    type Callback = Box<dyn Fn(Change) + Send + Sync>;

    /// Called on a CoreAudio thread.
    unsafe extern "C" fn on_property(_object: u32, count: u32, addresses: *const AudioObjectPropertyAddress, client: *mut c_void) -> i32 {
        if client.is_null() || addresses.is_null() {
            return 0;
        }
        let on = unsafe { &*(client as *const Callback) };
        for a in unsafe { std::slice::from_raw_parts(addresses, count as usize) } {
            match a.selector {
                DEVICES => on(Change::Devices),
                DEFAULT_INPUT => on(Change::DefaultInput),
                DEFAULT_OUTPUT => on(Change::DefaultOutput),
                _ => {}
            }
        }
        0
    }

    /// Registered while it lives.
    pub(crate) struct OsNotifications {
        client: *mut Callback,
    }

    impl OsNotifications {
        pub(crate) fn start(on: impl Fn(Change) + Send + Sync + 'static) -> Option<Self> {
            let callback: Callback = Box::new(on);
            let client = Box::into_raw(Box::new(callback));
            let mut registered = 0;
            for selector in SELECTORS {
                let address = AudioObjectPropertyAddress { selector, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN };
                if unsafe { AudioObjectAddPropertyListener(SYSTEM_OBJECT, &address, on_property, client as *mut c_void) } == 0 {
                    registered += 1;
                }
            }
            let this = OsNotifications { client };
            if registered == 0 {
                log::warn!("[audio-devices] CoreAudio property listeners not registered");
                return None; // drops `this`: removes (nothing) and frees the callback
            }
            Some(this)
        }
    }

    impl Drop for OsNotifications {
        fn drop(&mut self) {
            for selector in SELECTORS {
                let address = AudioObjectPropertyAddress { selector, scope: SCOPE_GLOBAL, element: ELEMENT_MAIN };
                unsafe { AudioObjectRemovePropertyListener(SYSTEM_OBJECT, &address, on_property, self.client as *mut c_void) };
            }
            // No listener is left that could be running the callback.
            drop(unsafe { Box::from_raw(self.client) });
        }
    }
}

/// Anything else cpal runs on: no notifications.
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
pub(crate) struct OsNotifications;

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
impl OsNotifications {
    pub(crate) fn start(_on: impl Fn(Change) + Send + Sync + 'static) -> Option<Self> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_classes() {
        for class in ["Audio/Sink", "Audio/Source", "Audio/Duplex", "Audio/Source/Virtual", "Audio/Sink/Virtual"] {
            assert!(is_device_class(class), "{class}");
        }
        for class in ["Stream/Output/Audio", "Stream/Input/Audio", "Video/Source", "Midi/Bridge", ""] {
            assert!(!is_device_class(class), "{class}");
        }
    }

    #[test]
    fn pactl_events_that_change_the_list() {
        for line in ["Event 'new' on sink #57", "Event 'remove' on source #58", "Event 'change' on server #0", "  Event 'new' on source #3\n"] {
            assert!(pactl_line_is_device_change(line), "{line}");
        }
        for line in [
            "Event 'new' on sink-input #91",
            "Event 'remove' on source-output #12",
            "Event 'change' on sink #57",
            "Event 'new' on client #101",
            "Event 'new' on module #9",
            "garbage",
            "",
        ] {
            assert!(!pactl_line_is_device_change(line), "{line}");
        }
    }

    /// Against the desktop's own PipeWire: a null sink appearing and going is
    /// two bursts of events, and nothing is emitted while nothing changes.
    /// Creates and removes `decibell-test-devwatch-<pid>` (not audible: a
    /// null sink plays nowhere). Ignored by default: it needs PipeWire with
    /// pipewire-pulse and `pactl`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn a_null_sink_appearing_and_going_is_reported() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};
        let count = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&count);
        let listener = Listener::start(move || {
            c.fetch_add(1, Ordering::AcqRel);
        })
        .expect("listener");
        assert!(matches!(listener, Listener::PipeWire { .. }), "PipeWire's registry is the source here");
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(count.load(Ordering::Acquire), 0, "what existed at start is not a change");
        let name = format!("decibell-test-devwatch-{}", std::process::id());
        let out = std::process::Command::new("pactl")
            .args(["load-module", "module-null-sink", &format!("sink_name={name}")])
            .output()
            .expect("pactl");
        let module = String::from_utf8_lossy(&out.stdout).trim().to_string();
        assert!(out.status.success() && !module.is_empty(), "load-module failed");
        let wait_for = |n: usize| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while count.load(Ordering::Acquire) < n && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            count.load(Ordering::Acquire)
        };
        let added = wait_for(1);
        let _ = std::process::Command::new("pactl").args(["unload-module", &module]).status();
        assert!(added >= 1, "the new sink was not reported");
        let removed = wait_for(added + 1);
        assert!(removed > added, "the sink's removal was not reported");
        println!("null sink: {added} event(s) on add, {} on remove", removed - added);
        drop(listener);
        let after = count.load(Ordering::Acquire);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(count.load(Ordering::Acquire), after, "nothing after the listener is dropped");
    }
}

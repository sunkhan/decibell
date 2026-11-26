//! Where the player's sound goes: an output device through cpal, the one Settings → Audio
//! says (owner, 2026-11-26: attachments are routed like other people's stream audio: the
//! separate stream-output device when that option is on, else the voice output device; no
//! name means the system's default). The Electron client's `<audio>` / `<video>` elements
//! play on Chromium's default sink whatever Settings say; this client differs on purpose
//! (`docs/electron-quirks.md` row 19). The caller picks the name; [`Device`] resolves it the
//! way the core's voice path resolves a saved device (`resolve_playback_device`).
//! Nothing here touches the voice path: this is a stream of its own, opened when a player has
//! sound to make and dropped when it has been silent for a quarter of a second.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::render::{CHANNELS, Renderer};

/// The renderer a device stream calls. One player has one for its whole life: a new device's
/// stream takes it over from the old one's (which is gone by then), so the envelope carries
/// across the change.
pub type RendererCell = Arc<Mutex<Renderer>>;

/// A place sound can go.
pub trait AudioOutput: Send + Sync + 'static {
    /// The sample rate a stream opened now would have.
    fn rate(&self) -> Result<u32, String>;
    /// Opens a stream that pulls from `renderer` until it is dropped. Called on, and the stream
    /// kept by, the player's control thread. `on_failure` is called (from any thread, must not
    /// block) when the stream stops working; [`OutputStream::failed`] is true from then on.
    fn open(
        &self,
        renderer: RendererCell,
        rate: u32,
        on_failure: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn OutputStream>, String>;
}

pub trait OutputStream {
    /// The device reported an error (unplugged, the server went away): open another.
    fn failed(&self) -> bool {
        false
    }
}

/// An output device by its saved name (`None`: the system's default). Resolved at every open,
/// as the core does for the voice path: on Linux the name is a PulseAudio / PipeWire sink,
/// reached through ALSA's `pulse` (else `default`) PCM with `PULSE_SINK` naming it, and a sink
/// that is not there leaves the stream on the server's default; elsewhere it is cpal's device
/// name, and a device that is not connected falls back to the default one.
pub struct Device {
    name: Option<String>,
}

/// The system's default output device.
#[allow(non_upper_case_globals)]
pub const DefaultDevice: Device = Device { name: None };

impl Device {
    pub fn named(name: Option<String>) -> Device {
        Device { name: name.filter(|name| !name.is_empty()) }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn config(&self) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
        let host = cpal::default_host();
        let device = match &self.name {
            None => {
                // As the core: no name, no routing left over from a named device.
                #[cfg(target_os = "linux")]
                // Safety: the same process-wide variable the core's voice path sets and clears
                // from its pipeline thread; a stream opened in the same instant by the other
                // may land on this one's sink (the core documents the same edge).
                unsafe {
                    std::env::remove_var("PULSE_SINK")
                };
                host.default_output_device()
            }
            Some(name) => named_device(&host, name),
        }
        .ok_or_else(|| "No audio output device".to_string())?;
        let config = device
            .default_output_config()
            .map_err(|error| format!("The audio output device can't be used ({error})"))?;
        Ok((device, config))
    }
}

#[cfg(target_os = "linux")]
fn named_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    // Safety: see `Device::config`.
    unsafe { std::env::set_var("PULSE_SINK", name) };
    let mut fallback = None;
    for device in host.output_devices().ok()? {
        match device.name().ok().as_deref() {
            Some("pulse") => return Some(device),
            Some("default") if fallback.is_none() => fallback = Some(device),
            _ => {}
        }
    }
    fallback.or_else(|| host.default_output_device())
}

#[cfg(not(target_os = "linux"))]
fn named_device(host: &cpal::Host, name: &str) -> Option<cpal::Device> {
    host.output_devices()
        .ok()?
        .find(|device| device.name().is_ok_and(|found| found == name))
        .or_else(|| {
            log::info!("media: output device {name:?} is not connected: the default one");
            host.default_output_device()
        })
}

struct CpalStream {
    _stream: cpal::Stream,
    failed: Arc<AtomicBool>,
}

impl OutputStream for CpalStream {
    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

/// Renders stereo into `scratch` and spreads it over the device's channels.
fn fill<T: Copy>(
    renderer: &RendererCell,
    scratch: &mut Vec<f32>,
    data: &mut [T],
    channels: usize,
    plays_at: Instant,
    convert: impl Fn(f32) -> T,
    silence: T,
) {
    let frames = data.len() / channels.max(1);
    if scratch.len() < frames * CHANNELS {
        // Only if the device asks for more than it ever did: not in the steady state.
        scratch.resize(frames * CHANNELS, 0.0);
    }
    let stereo = &mut scratch[..frames * CHANNELS];
    match renderer.try_lock() {
        Some(mut renderer) => renderer.render(stereo, plays_at),
        None => stereo.fill(0.0),
    }
    for (frame, out) in stereo.as_chunks::<CHANNELS>().0.iter().zip(data.chunks_exact_mut(channels)) {
        match channels {
            1 => out[0] = convert((frame[0] + frame[1]) * 0.5),
            _ => {
                out[0] = convert(frame[0]);
                out[1] = convert(frame[1]);
                for extra in &mut out[2..] {
                    *extra = silence;
                }
            }
        }
    }
}

fn plays_at(info: &cpal::OutputCallbackInfo) -> Instant {
    let stamp = info.timestamp();
    let ahead = stamp
        .playback
        .duration_since(&stamp.callback)
        .unwrap_or(Duration::ZERO);
    // A device that reports nonsense must not move the clock by seconds.
    Instant::now() + ahead.min(Duration::from_millis(500))
}

impl AudioOutput for Device {
    fn rate(&self) -> Result<u32, String> {
        Ok(self.config()?.1.sample_rate().0)
    }

    fn open(
        &self,
        renderer: RendererCell,
        rate: u32,
        on_failure: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn OutputStream>, String> {
        let (device, supported) = self.config()?;
        if supported.sample_rate().0 != rate {
            return Err(format!(
                "the output device's rate changed ({} Hz, was {rate})",
                supported.sample_rate().0
            ));
        }
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels as usize;
        let failed = Arc::new(AtomicBool::new(false));
        let on_error = {
            let failed = failed.clone();
            move |error: cpal::StreamError| {
                log::warn!("media: audio output: {error}");
                failed.store(true, Ordering::Relaxed);
                on_failure();
            }
        };
        let mut scratch = vec![0.0f32; 8192 * CHANNELS];
        let stream = match format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                &config,
                move |data: &mut [f32], info: &cpal::OutputCallbackInfo| {
                    fill(&renderer, &mut scratch, data, channels, plays_at(info), |x| x, 0.0)
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], info: &cpal::OutputCallbackInfo| {
                    fill(
                        &renderer,
                        &mut scratch,
                        data,
                        channels,
                        plays_at(info),
                        |x| (x.clamp(-1.0, 1.0) * 32767.0) as i16,
                        0,
                    )
                },
                on_error,
                None,
            ),
            other => return Err(format!("The audio output device's sample format ({other:?}) isn't supported")),
        }
        .map_err(|error| format!("The audio output device can't be opened ({error})"))?;
        stream
            .play()
            .map_err(|error| format!("The audio output device can't be started ({error})"))?;
        Ok(Box::new(CpalStream {
            _stream: stream,
            failed,
        }))
    }
}

/// What a [`TestOutput`] rendered.
#[derive(Default)]
pub struct Recording {
    /// The left channel of every block, in order.
    pub samples: Vec<f32>,
    /// For each block: when its first sample "played" and its offset in `samples`.
    pub blocks: Vec<(Instant, usize)>,
    /// The offset in `samples` at which each stream that was opened starts.
    pub streams: Vec<usize>,
}

impl Recording {
    /// When sample `index` played.
    pub fn played_at(&self, index: usize, rate: u32) -> Option<Instant> {
        let block = self.blocks.partition_point(|&(_, offset)| offset <= index);
        let (at, offset) = *self.blocks.get(block.checked_sub(1)?)?;
        Some(at + Duration::from_nanos((index - offset) as u64 * 1_000_000_000 / rate as u64))
    }
}

/// A test stream's failure flag and the player's callback for it.
type Unplug = (Arc<AtomicBool>, Arc<dyn Fn() + Send + Sync>);

/// An output with no device behind it: a thread that pulls 10 ms blocks in real time, as a
/// sound card would, and either keeps them (the tests' sink: sample-exact checks of what would
/// have been heard) or throws them away (`DECIBELL_MEDIA_OUTPUT=null`: self-tests and
/// measurements on a machine somebody is sitting at).
pub struct TestOutput {
    rate: u32,
    recording: Option<Arc<Mutex<Recording>>>,
    /// Streams opened so far.
    pub opened: Arc<std::sync::atomic::AtomicU32>,
    /// Streams open now.
    pub live: Arc<std::sync::atomic::AtomicU32>,
    /// The open stream's "device was unplugged" switch.
    unplug: Mutex<Option<Unplug>>,
}

impl TestOutput {
    /// Keeps up to `seconds` of what is rendered.
    pub fn recording(rate: u32, seconds: u32) -> (Arc<TestOutput>, Arc<Mutex<Recording>>) {
        let recording = Arc::new(Mutex::new(Recording {
            samples: Vec::with_capacity((rate * seconds) as usize),
            blocks: Vec::with_capacity(seconds as usize * 100 + 16),
            streams: Vec::with_capacity(64),
        }));
        let output = TestOutput {
            rate,
            recording: Some(recording.clone()),
            opened: Default::default(),
            live: Default::default(),
            unplug: Mutex::new(None),
        };
        (Arc::new(output), recording)
    }

    /// Renders and discards.
    pub fn null(rate: u32) -> Arc<TestOutput> {
        Arc::new(TestOutput {
            rate,
            recording: None,
            opened: Default::default(),
            live: Default::default(),
            unplug: Mutex::new(None),
        })
    }
}

impl TestOutput {
    /// The open stream stops being called and reports a failure, as a device that was
    /// unplugged does.
    pub fn unplug(&self) {
        if let Some((failed, on_failure)) = self.unplug.lock().take() {
            failed.store(true, Ordering::Relaxed);
            on_failure();
        }
    }
}

struct TestStream {
    failed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    live: Arc<std::sync::atomic::AtomicU32>,
}

impl OutputStream for TestStream {
    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

impl Drop for TestStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}

impl AudioOutput for TestOutput {
    fn rate(&self) -> Result<u32, String> {
        Ok(self.rate)
    }

    fn open(
        &self,
        renderer: RendererCell,
        rate: u32,
        on_failure: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Box<dyn OutputStream>, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        *self.unplug.lock() = Some((failed.clone(), on_failure));
        if let Some(recording) = &self.recording {
            let mut recording = recording.lock();
            let offset = recording.samples.len();
            recording.streams.push(offset);
        }
        let recording = self.recording.clone();
        self.opened.fetch_add(1, Ordering::Relaxed);
        self.live.fetch_add(1, Ordering::Relaxed);
        let thread = std::thread::Builder::new()
            .name("decibell-msink".into())
            .spawn({
                let stop = stop.clone();
                let failed = failed.clone();
                move || {
                    let frames = rate as usize / 100;
                    let mut block = vec![0.0f32; frames * CHANNELS];
                    let mut due = Instant::now();
                    while !stop.load(Ordering::Relaxed) && !failed.load(Ordering::Relaxed) {
                        renderer.lock().render(&mut block, due);
                        if let Some(recording) = &recording {
                            let mut recording = recording.lock();
                            let offset = recording.samples.len();
                            if offset + frames <= recording.samples.capacity() {
                                recording.blocks.push((due, offset));
                                recording.samples.extend(block.iter().step_by(CHANNELS));
                            }
                        }
                        due += Duration::from_millis(10);
                        // A sound card's clock: the next block is wanted when this one has
                        // played, whatever the scheduler did meanwhile.
                        std::thread::sleep(due.saturating_duration_since(Instant::now()));
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Box::new(TestStream {
            failed,
            stop,
            thread: Some(thread),
            live: self.live.clone(),
        }))
    }
}

/// The output the process's players use: the device saved under `name` (`None`: the system's
/// default), or the null sink when `DECIBELL_MEDIA_OUTPUT=null` (self-tests and measurements:
/// nothing is audible).
pub fn device_output(name: Option<String>) -> Arc<dyn AudioOutput> {
    if std::env::var("DECIBELL_MEDIA_OUTPUT").is_ok_and(|value| value == "null") {
        return TestOutput::null(48_000);
    }
    Arc::new(Device::named(name))
}

/// [`device_output`] on the system's default device.
pub fn system_output() -> Arc<dyn AudioOutput> {
    device_output(None)
}

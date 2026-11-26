//! The microphone test of Settings → Audio outside a voice session: the
//! chosen input device through the voice path's own capture and processing
//! (`capture_chain.rs`: resampling, AEC / AGC, noise suppression), metered as
//! the voice pipeline meters it (`voice_input_level`: RMS dB of every third
//! 20 ms frame, about 16 a second, -96 for silence). Nothing is encoded, sent
//! or played: there is no output stream.
//!
//! One thread (`decibell-mic-test`) exists while a test runs. It owns the
//! capture stream and sleeps until the capture callback has queued samples
//! (the device's own clock; no timer). `stop` ends and joins it, which closes
//! the device. A voice session or call holds the microphone itself: a test is
//! refused while one runs and stopped when one starts.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use ringbuf::{traits::Split, HeapProd, HeapRb};

use super::capture_chain::{rms_db, CaptureChain};
use super::codec::FRAME_SIZE;

/// Where the level goes: dB, from the test's thread.
pub type LevelSink = Arc<dyn Fn(f32) + Send + Sync>;

/// The voice processing to meter through (the user's Audio settings).
#[derive(Debug, Clone, Copy, Default)]
pub struct Processing {
    pub echo_cancellation: bool,
    /// 0 = off … 4, as `set_noise_suppression_level`.
    pub noise_suppression_level: u8,
    pub auto_gain: bool,
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

static RUNNING: Mutex<Option<Running>> = Mutex::new(None);
/// Voice engines alive (a channel session or a call): they hold the mic.
static VOICE_SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// A voice session or call is starting: it takes the microphone, so a
/// running test ends (its meter is the session's from here).
pub(crate) fn voice_session_started() {
    VOICE_SESSIONS.fetch_add(1, Ordering::AcqRel);
    stop();
}

pub(crate) fn voice_session_ended() {
    // Never below zero (an engine's `stop` runs once, but be safe).
    let mut n = VOICE_SESSIONS.load(Ordering::Acquire);
    while n > 0 {
        match VOICE_SESSIONS.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(now) => n = now,
        }
    }
}

pub fn is_running() -> bool {
    RUNNING.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Start metering `device` (None: the OS default input, as a voice session
/// opens it). Returns once the test's thread is started; the device opens on
/// that thread, and a device that can't be opened is logged and reports
/// nothing. Refused while a voice session or call holds the microphone.
/// A test already running is replaced.
pub fn start(device: Option<String>, processing: Processing, level: LevelSink) -> Result<(), String> {
    let mut running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    if VOICE_SESSIONS.load(Ordering::Acquire) > 0 {
        return Err("The microphone is in use by a voice session".into());
    }
    if let Some(old) = running.take() {
        end(old);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("decibell-mic-test".into())
        .spawn(move || run(device, processing, level, stop_thread))
        .map_err(|e| format!("Couldn't start the microphone test: {}", e))?;
    *running = Some(Running { stop, thread });
    Ok(())
}

/// Stop the test and close the device. Idempotent; returns once the thread
/// has ended.
pub fn stop() {
    let old = RUNNING.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Some(old) = old {
        end(old);
    }
}

fn end(r: Running) {
    r.stop.store(true, Ordering::Release);
    r.thread.thread().unpark();
    let _ = r.thread.join();
}

/// How the capture stream is opened: mono i16 at the returned rate into
/// `prod`, the thread woken after each callback. The stream is closed by
/// dropping what is returned.
type Opener = fn(Option<&str>, Arc<Mutex<HeapProd<i16>>>, std::thread::Thread) -> Option<(Box<dyn std::any::Any>, u32)>;

fn open_device(device: Option<&str>, prod: Arc<Mutex<HeapProd<i16>>>, wake: std::thread::Thread) -> Option<(Box<dyn std::any::Any>, u32)> {
    let host = cpal::default_host();
    super::audio_device::build_input_stream(&host, device, prod, Some(wake))
        .map(|(stream, rate)| (Box::new(stream) as Box<dyn std::any::Any>, rate))
}

#[cfg(not(test))]
fn opener() -> Opener {
    open_device
}

#[cfg(test)]
fn opener() -> Opener {
    tests::TEST_OPENER.lock().unwrap().unwrap_or(open_device)
}

fn run(device: Option<String>, processing: Processing, level: LevelSink, stop: Arc<AtomicBool>) {
    // As the voice pipeline: ~1 s of capture ring at 48 kHz.
    let (prod, mut cons) = HeapRb::<i16>::new(FRAME_SIZE * 48).split();
    let prod = Arc::new(Mutex::new(prod));
    let Some((stream, rate)) = opener()(device.as_deref(), prod, std::thread::current()) else {
        log::warn!("[mic-test] couldn't open the input device ({:?})", device);
        return;
    };
    let mut chain = CaptureChain::new(rate);
    if processing.echo_cancellation {
        chain.set_aec(true);
    }
    if processing.noise_suppression_level > 0 {
        chain.set_ns_level(processing.noise_suppression_level);
    }
    if processing.auto_gain {
        chain.set_agc(true);
    }
    // The voice pipeline's cadence: every third frame's level.
    let mut frames = 0u32;
    while !stop.load(Ordering::Acquire) {
        chain.pull(&mut cons);
        while let Some(mut frame) = chain.next_frame() {
            chain.process(&mut frame);
            frames += 1;
            if frames >= 3 {
                frames = 0;
                level(rms_db(&frame));
            }
        }
        // Until the capture callback queues more, or `stop`.
        std::thread::park();
    }
    drop(stream);
    // The meter rests (the renderer's test set -96 as it ended).
    level(-96.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ringbuf::traits::Producer;
    use std::time::{Duration, Instant};

    pub(super) static TEST_OPENER: Mutex<Option<Opener>> = Mutex::new(None);
    /// The test (a process-wide static) one test at a time.
    static ONE: Mutex<()> = Mutex::new(());

    struct Fake(Arc<AtomicBool>, Option<JoinHandle<()>>);
    impl Drop for Fake {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
            let _ = self.1.take().unwrap().join();
            OPEN.fetch_sub(1, Ordering::AcqRel);
        }
    }
    static OPEN: AtomicUsize = AtomicUsize::new(0);

    /// What the fake microphone "hears", per sample index.
    static SIGNAL: Mutex<fn(usize, u32) -> i16> = Mutex::new(|_, _| 0);
    static RATE: Mutex<u32> = Mutex::new(48_000);

    /// A microphone for the tests: 10 ms callbacks (pushed far faster than
    /// real time), each followed by the wake the real callback gives.
    fn fake_open(_d: Option<&str>, prod: Arc<Mutex<HeapProd<i16>>>, wake: std::thread::Thread) -> Option<(Box<dyn std::any::Any>, u32)> {
        let rate = *RATE.lock().unwrap();
        let signal = *SIGNAL.lock().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        OPEN.fetch_add(1, Ordering::AcqRel);
        let t = std::thread::spawn(move || {
            let mut i = 0usize;
            while !stop_t.load(Ordering::Acquire) {
                {
                    let mut p = prod.lock().unwrap();
                    for _ in 0..rate / 100 {
                        let _ = p.try_push(signal(i, rate));
                        i += 1;
                    }
                }
                wake.unpark();
                std::thread::sleep(Duration::from_micros(300));
            }
        });
        Some((Box::new(Fake(stop, Some(t))), rate))
    }

    fn levels_after(start: impl FnOnce(LevelSink) -> Result<(), String>, n: usize) -> Vec<f32> {
        let got: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let sink_got = Arc::clone(&got);
        start(Arc::new(move |db| sink_got.lock().unwrap().push(db))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while got.lock().unwrap().len() < n {
            assert!(Instant::now() < deadline, "no levels reported");
            std::thread::sleep(Duration::from_millis(2));
        }
        stop();
        let v = got.lock().unwrap().clone();
        v
    }

    fn sine(i: usize, rate: u32) -> i16 {
        // 440 Hz at -20 dBFS peak.
        ((2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64).sin() * 3276.8) as i16
    }

    fn noise(i: usize, _rate: u32) -> i16 {
        // White noise, about -26 dBFS RMS.
        let x = (i as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (((x >> 33) as i32 % 6000) - 3000) as i16
    }

    /// A -20 dBFS tone meters at its RMS (-23 dB), at the device's rate and
    /// through the capture resampler; silence is -96; the last report after a
    /// stop is -96; stop is idempotent, closes the device and leaves no
    /// thread; nothing is reported after it.
    #[test]
    fn the_meter_reads_what_the_capture_path_delivers() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_OPENER.lock().unwrap() = Some(fake_open);
        for rate in [48_000u32, 44_100] {
            *RATE.lock().unwrap() = rate;
            *SIGNAL.lock().unwrap() = sine;
            let levels = levels_after(|sink| start(None, Processing::default(), sink), 12);
            // The first reports at 44.1 kHz include the resampler's ramp-in.
            for &db in &levels[2..levels.len() - 1] {
                assert!((db - -23.01).abs() < 0.15, "{rate} Hz: {db} dB");
            }
            assert_eq!(*levels.last().unwrap(), -96.0, "the meter rests after the stop");
            assert_eq!(OPEN.load(Ordering::Acquire), 0, "the device is closed");
            assert!(!is_running());
        }
        *RATE.lock().unwrap() = 48_000;
        *SIGNAL.lock().unwrap() = |_, _| 0;
        let levels = levels_after(|sink| start(None, Processing::default(), sink), 5);
        assert!(levels.iter().all(|&db| db == -96.0));

        // Nothing after the stop; a second stop is a no-op.
        let got: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let g = Arc::clone(&got);
        start(None, Processing::default(), Arc::new(move |db| g.lock().unwrap().push(db))).unwrap();
        std::thread::sleep(Duration::from_millis(30));
        stop();
        let n = got.lock().unwrap().len();
        stop();
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(got.lock().unwrap().len(), n);
        *TEST_OPENER.lock().unwrap() = None;
    }

    /// The meter shows the processed signal, as the voice session's does:
    /// with noise suppression on, steady noise reads far lower than raw.
    #[test]
    fn the_meter_is_after_the_voice_processing() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_OPENER.lock().unwrap() = Some(fake_open);
        *RATE.lock().unwrap() = 48_000;
        *SIGNAL.lock().unwrap() = noise;
        let raw = levels_after(|sink| start(None, Processing::default(), sink), 40);
        let ns = Processing { noise_suppression_level: 2, ..Default::default() };
        let cleaned = levels_after(|sink| start(None, ns, sink), 40);
        let (raw, cleaned) = (raw[30], cleaned[30]);
        println!("steady noise: raw {raw:.1} dB, after noise suppression {cleaned:.1} dB");
        assert!(raw > -30.0 && cleaned < raw - 10.0, "raw {raw}, suppressed {cleaned}");
        *TEST_OPENER.lock().unwrap() = None;
    }

    /// A voice session holds the microphone: a test is refused while one
    /// runs, and a running test ends when one starts.
    #[test]
    fn voice_takes_the_microphone() {
        let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
        *TEST_OPENER.lock().unwrap() = Some(fake_open);
        *SIGNAL.lock().unwrap() = sine;
        start(None, Processing::default(), Arc::new(|_| {})).unwrap();
        assert!(is_running());
        voice_session_started();
        assert!(!is_running(), "the test ended when voice started");
        assert_eq!(OPEN.load(Ordering::Acquire), 0);
        let refused = start(None, Processing::default(), Arc::new(|_| {}));
        assert!(refused.is_err() && !is_running());
        voice_session_ended();
        start(None, Processing::default(), Arc::new(|_| {})).unwrap();
        stop();
        *TEST_OPENER.lock().unwrap() = None;
    }
}

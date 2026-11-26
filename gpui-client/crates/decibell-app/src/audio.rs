//! The app's audio glue: the interface sounds and the microphone test, both the core's
//! (`decibell_core::api::audio`). The client has no audio layer of its own.
//!
//! **Sounds** (`utils/sounds.ts`). The model says what should sound (`model.live.cues`: a
//! [`Cue`] per `playSound` / `loopSound` / `stopRing` of the renderer); [`install`] subscribes
//! and hands each to the core: `Cue::Play` → `play_sound`, `Cue::StartRing` → `start_ring` with
//! `call_ring.wav`, `Cue::StopRing` → `stop_ring`. While a voice session or a call has an output
//! stream the core mixes the sound into it (it plays on the voice output device); otherwise it
//! opens a stream on the OS default output (where the renderer's `AudioContext` played) that
//! closes by itself a quarter of a second after the last sound. Nothing of ours runs between
//! sounds: no thread, no timer, no open stream.
//!
//! The WAVs are the renderer's (`assets/sounds/`, embedded here: 368 KB). One is parsed the
//! first time it is played and kept. The two 44.1 kHz files (the ring, the call's end) are
//! resampled to the usual device rate on a background thread before their first play, so the
//! UI thread never runs the resampler; the ring starts when that is done, unless it was
//! stopped meanwhile.
//!
//! **Never audible in tests** ([`CueOutput`]). Under `--check` (a self-test's signed-in child)
//! the player hands the core nothing, so a voice self-test, whose session's output stream is on
//! the desktop's real device, stays silent. A self-test that is about sound makes a PipeWire
//! null sink, names it in `DECIBELL_CUE_SINK` and saves it as the output device: sounds then
//! play only while the saved output device is that sink (in voice the core plays them on the
//! voice output, which is the saved one), and the microphone test opens only that sink's
//! monitor. Development builds (`dev-local`, debug) honour the variable outside tests too
//! (`off`: no sounds, for measurements); a shipped build ignores it.
//!
//! **The microphone test** of Settings → Audio is `decibell_settings::CoreMicTest`, installed
//! here once the core runs.

use std::cell::Cell;
use std::rc::Rc;

use decibell_core::api::audio;
use decibell_model::Model;
use decibell_model::live::{Cue, Sound};
use decibell_settings::{CoreMicTest, LevelSink, MicTest, MicTestError, Processing, SettingsHooks};
use gpui::App;

/// The variable that names the sink sounds may play on in tests and development builds.
pub const CUE_SINK_ENV: &str = "DECIBELL_CUE_SINK";

/// The rate the sounds are prepared for before their first play (PipeWire's and WASAPI's
/// usual mix rate; the core resamples by itself for a device at another).
const USUAL_DEVICE_RATE: u32 = 48_000;

/// Where the interface sounds go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CueOutput {
    /// The shipped behaviour: the voice output while in voice, else the OS default output.
    System,
    /// Tests: this sink only, and only while it is also the saved output device.
    Sink(String),
    /// Nothing is handed to the core.
    Silent,
}

impl CueOutput {
    /// `test_mode`: the app runs a self-test's check (`--check`).
    pub fn from_env(test_mode: bool) -> Self {
        let dev_build = cfg!(any(feature = "dev-local", debug_assertions));
        Self::choose(test_mode, dev_build, std::env::var(CUE_SINK_ENV).ok().as_deref())
    }

    fn choose(test_mode: bool, dev_build: bool, sink: Option<&str>) -> Self {
        let sink = sink.filter(|s| !s.is_empty() && (test_mode || dev_build));
        match sink {
            Some("off") => CueOutput::Silent,
            Some(name) => CueOutput::Sink(name.to_string()),
            None if test_mode => CueOutput::Silent,
            None => CueOutput::System,
        }
    }

    /// The device to hand the core with a sound (`Some(None)`: the OS default), or `None`
    /// when the sound must not play. `saved_output` is the output device in the settings: a
    /// voice session plays on it, sounds included.
    fn device(&self, saved_output: Option<&str>) -> Option<Option<&str>> {
        match self {
            CueOutput::System => Some(None),
            CueOutput::Silent => None,
            CueOutput::Sink(name) => (saved_output == Some(name.as_str())).then_some(Some(name.as_str())),
        }
    }
}

const SOUNDS: usize = 12;

/// The renderer's file for a sound (`Sound::asset()` names the same file under `assets/`).
fn wav(sound: Sound) -> &'static [u8] {
    macro_rules! file {
        ($name:literal) => {
            include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/sounds/", $name, ".wav"))
        };
    }
    match sound {
        Sound::Mute => file!("mute"),
        Sound::Unmute => file!("unmute"),
        Sound::Deafen => file!("deafen"),
        Sound::Undeafen => file!("undeafen"),
        Sound::UserJoin => file!("user_join"),
        Sound::UserLeave => file!("user_leave"),
        Sound::StreamStart => file!("stream_start"),
        Sound::StreamStop => file!("stream_stop"),
        Sound::Connect => file!("connect"),
        Sound::Disconnect => file!("disconnect"),
        Sound::CallRing => file!("call_ring"),
        Sound::CallEnd => file!("call_end"),
    }
}

/// A sound once it was first asked for.
enum Loaded {
    /// The file didn't parse (logged once).
    Failed,
    Ready {
        pcm: audio::Sound,
        /// At the usual device rate already: playing it is only a hand-off to the callback.
        prepared: Rc<Cell<bool>>,
    },
}

struct Player {
    output: CueOutput,
    model: Model,
    sounds: [Option<Loaded>; SOUNDS],
    /// The ring wanted now (0: none): a ring whose sound was still being prepared starts
    /// only if it is still the one wanted.
    ring: Rc<Cell<u64>>,
    rings: u64,
}

impl Player {
    fn handle(&mut self, cue: Cue, cx: &mut App) {
        match cue {
            Cue::Play(sound) => self.play(sound, None, cx),
            Cue::StartRing => {
                self.rings += 1;
                self.ring.set(self.rings);
                self.play(Sound::CallRing, Some(self.rings), cx);
            }
            Cue::StopRing => {
                self.ring.set(0);
                if self.output != CueOutput::Silent {
                    audio::stop_ring();
                }
            }
            // TODO(attention): the taskbar flash / dock bounce (GPUI has no API for it at the pin).
            Cue::Flash => log::debug!("[cues] Flash (no window attention yet)"),
        }
    }

    /// Play `sound` once, or loop it as ring number `ring`.
    fn play(&mut self, sound: Sound, ring: Option<u64>, cx: &mut App) {
        let saved_output = self.model.settings.read(cx).get().output_device.as_deref();
        let Some(device) = self.output.device(saved_output) else {
            log::debug!("[cues] {sound:?} not played ({:?})", self.output);
            return;
        };
        let device = device.map(str::to_string);
        let slot = self.sounds[sound as usize].get_or_insert_with(|| match audio::load_sound(wav(sound)) {
            Ok(pcm) => Loaded::Ready { prepared: Rc::new(Cell::new(pcm.rate() == USUAL_DEVICE_RATE)), pcm },
            Err(e) => {
                log::error!("[cues] {} did not load: {e}", sound.asset());
                Loaded::Failed
            }
        });
        let Loaded::Ready { pcm, prepared } = slot else { return };
        let wanted = self.ring.clone();
        let start = move |pcm: &audio::Sound| match ring {
            None => audio::play_sound(pcm, device.as_deref()),
            Some(ring) if wanted.get() == ring => audio::start_ring(pcm, device.as_deref()),
            Some(_) => {}
        };
        if prepared.get() {
            start(pcm);
            return;
        }
        // Its first play: resample off the UI thread (the core keeps the result), then play.
        let (pcm, prepared) = (pcm.clone(), prepared.clone());
        cx.spawn(async move |cx| {
            let resampling = pcm.clone();
            cx.background_executor()
                .spawn(async move {
                    resampling.at_rate(USUAL_DEVICE_RATE);
                })
                .await;
            prepared.set(true);
            start(&pcm);
        })
        .detach();
    }
}

/// The microphone test of a self-test: only the test sink's monitor is ever opened.
struct TestInputMicTest {
    input: String,
}

impl MicTest for TestInputMicTest {
    fn start(&self, device: Option<&str>, processing: Processing, level: LevelSink) -> Result<(), MicTestError> {
        if device != Some(self.input.as_str()) {
            return Err(MicTestError::Failed(format!(
                "a test opens only {} (asked for {device:?}): the real microphone stays closed",
                self.input
            )));
        }
        CoreMicTest.start(device, processing, level)
    }

    fn stop(&self) {
        CoreMicTest.stop();
    }
}

/// Wire the sounds and the microphone test to the core. Called once, with the core started
/// (screens and the self-tests' own window have no core and get neither).
pub fn install(model: &Model, output: CueOutput, cx: &mut App) {
    match &output {
        CueOutput::System => SettingsHooks::set_mic_test(Rc::new(CoreMicTest), cx),
        CueOutput::Sink(sink) => SettingsHooks::set_mic_test(Rc::new(TestInputMicTest { input: format!("{sink}.monitor") }), cx),
        // No sound in or out: the tab keeps `UnavailableMicTest`.
        CueOutput::Silent => {}
    }
    log::debug!("[cues] output: {output:?}");
    let mut player =
        Player { output, model: model.clone(), sounds: [const { None }; SOUNDS], ring: Rc::new(Cell::new(0)), rings: 0 };
    cx.subscribe(&model.live.cues, move |_, cue: &Cue, cx| player.handle(*cue, cx)).detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Sound; SOUNDS] = [
        Sound::Mute,
        Sound::Unmute,
        Sound::Deafen,
        Sound::Undeafen,
        Sound::UserJoin,
        Sound::UserLeave,
        Sound::StreamStart,
        Sound::StreamStop,
        Sound::Connect,
        Sound::Disconnect,
        Sound::CallRing,
        Sound::CallEnd,
    ];

    #[test]
    fn every_sound_is_its_own_file_and_loads() {
        let mut seen = [false; SOUNDS];
        for sound in ALL {
            assert!(!std::mem::replace(&mut seen[sound as usize], true), "{sound:?} shares an index");
            // The embedded bytes are the file `Sound::asset()` names.
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets").join(sound.asset());
            assert_eq!(std::fs::read(&path).unwrap(), wav(sound), "{}", path.display());
            let pcm = audio::load_sound(wav(sound)).unwrap_or_else(|e| panic!("{}: {e}", sound.asset()));
            assert!(!pcm.is_empty() && matches!(pcm.rate(), 44_100 | 48_000), "{}", sound.asset());
        }
    }

    #[test]
    fn a_test_never_plays_on_the_real_output() {
        // A self-test's check: silent, unless it named a sink.
        assert_eq!(CueOutput::choose(true, true, None), CueOutput::Silent);
        assert_eq!(CueOutput::choose(true, false, None), CueOutput::Silent);
        assert_eq!(CueOutput::choose(true, true, Some("")), CueOutput::Silent);
        assert_eq!(CueOutput::choose(true, false, Some("null-1")), CueOutput::Sink("null-1".into()));
        // The app: the system's output; the variable counts in development builds only.
        assert_eq!(CueOutput::choose(false, false, None), CueOutput::System);
        assert_eq!(CueOutput::choose(false, false, Some("null-1")), CueOutput::System);
        assert_eq!(CueOutput::choose(false, false, Some("off")), CueOutput::System);
        assert_eq!(CueOutput::choose(false, true, Some("null-1")), CueOutput::Sink("null-1".into()));
        assert_eq!(CueOutput::choose(false, true, Some("off")), CueOutput::Silent);

        assert_eq!(CueOutput::Silent.device(Some("null-1")), None);
        assert_eq!(CueOutput::System.device(Some("speakers")), Some(None));
        // The sink only while a voice session would play on it too.
        let sink = CueOutput::Sink("null-1".into());
        assert_eq!(sink.device(Some("null-1")), Some(Some("null-1")));
        assert_eq!(sink.device(Some("speakers")), None);
        assert_eq!(sink.device(None), None);
    }

    #[test]
    fn a_test_microphone_is_the_sinks_monitor_only() {
        let test = TestInputMicTest { input: "null-1.monitor".into() };
        let sink: LevelSink = std::sync::Arc::new(|_| {});
        // Refused before the core is asked (there is none in a unit test).
        assert!(matches!(test.start(None, Processing::default(), sink.clone()), Err(MicTestError::Failed(_))));
        assert!(matches!(test.start(Some("alsa_input.usb-mic"), Processing::default(), sink), Err(MicTestError::Failed(_))));
    }
}

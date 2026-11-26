//! The Audio tab's mic test outside voice (`AudioTab.tsx` `VoiceThresholdBar`: "Test mic" ran
//! `getUserMedia` → an `AnalyserNode` → RMS dB in a `requestAnimationFrame` loop, the mic
//! released the moment the tab or the test went).
//!
//! The tab talks to the narrow [`MicTest`] trait. [`CoreMicTest`] is the real one, over the
//! core's `api::audio::start_mic_test` / `stop_mic_test`: the chosen input device through the
//! voice path's own capture and processing (the user's echo cancellation, noise suppression and
//! gain control, [`Processing`]), with no output stream, so the meter shows the signal a voice
//! session would send, as the in-voice meter (`voice_input_level`) does. The core reports the
//! level about 16 times a second from the test's own thread (−96 = silence, and once more when
//! the test ends); that thread exists only while a test runs and `stop` closes the device (the
//! OS's microphone indicator goes out). The core refuses a test while a voice session or a call
//! holds the microphone and ends a running one when a session starts: the tab uses the
//! session's meter then.
//!
//! The app installs [`CoreMicTest`] once the core runs ([`crate::SettingsHooks::set_mic_test`]);
//! until then, and in screens drawn from fixtures, [`UnavailableMicTest`] stands in.

use std::sync::Arc;

/// Where the level goes: dB (−96 = silence), from the capture thread, about 16 times a second.
pub type LevelSink = Arc<dyn Fn(f32) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MicTestError {
    /// This build or platform cannot open the mic outside voice.
    Unavailable,
    /// The device could not be opened (the platform's words).
    Failed(String),
}

impl std::fmt::Display for MicTestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MicTestError::Unavailable => f.write_str("the mic test outside voice is not available in this build"),
            MicTestError::Failed(e) => f.write_str(e),
        }
    }
}

/// The voice processing the test meters through (Settings → Audio's own switches).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Processing {
    pub echo_cancellation: bool,
    /// 0 = off … 4 (`noise_suppression_level`).
    pub noise_suppression_level: u8,
    pub auto_gain: bool,
}

/// Opens the mic for the Audio tab's meter while no voice session does.
pub trait MicTest: 'static {
    /// Start capturing `device` (`None`: the OS default) through `processing` and report its
    /// level to `level` until [`MicTest::stop`]. A second start replaces the first.
    fn start(&self, device: Option<&str>, processing: Processing, level: LevelSink) -> Result<(), MicTestError>;
    /// Close the device. Idempotent.
    fn stop(&self);
}

/// No mic test (no core: a screen drawn from fixtures, a self-test without a test input). The
/// tab then behaves as the renderer did when `getUserMedia` failed (the test shows as running,
/// the meter stays at rest, the failure is only logged).
pub struct UnavailableMicTest;

impl MicTest for UnavailableMicTest {
    fn start(&self, _: Option<&str>, _: Processing, _: LevelSink) -> Result<(), MicTestError> {
        Err(MicTestError::Unavailable)
    }

    fn stop(&self) {}
}

/// The core's microphone test (see the module doc). Needs the core running.
pub struct CoreMicTest;

impl MicTest for CoreMicTest {
    fn start(&self, device: Option<&str>, processing: Processing, level: LevelSink) -> Result<(), MicTestError> {
        use decibell_core::api::audio;
        let processing = audio::MicProcessing {
            echo_cancellation: processing.echo_cancellation,
            noise_suppression_level: processing.noise_suppression_level,
            auto_gain: processing.auto_gain,
        };
        // Refused while a voice session or call holds the microphone. A device that can't be
        // opened is not an error here: it opens on the test's thread and is only logged.
        audio::start_mic_test(device.map(str::to_string), processing, level).map_err(|e| MicTestError::Failed(e.to_string()))
    }

    fn stop(&self) {
        decibell_core::api::audio::stop_mic_test();
    }
}

/// The meter's smoothing (`VoiceThresholdBar`'s rAF loop: fast attack 0.5, slow decay 0.15 per
/// 60 Hz frame), applied once per level report instead of every frame, with the factors that give
/// the same time constants at the reports' ~16 Hz (`1 − (1 − k)^(60 / 16)`): the meter then
/// draws one frame per report and none in between.
pub fn smooth(previous: f32, target: f32) -> f32 {
    const REPORTS_PER_SECOND: f32 = 16.;
    let per_report = |k: f32| 1. - (1. - k).powf(60. / REPORTS_PER_SECOND);
    if (target - previous).abs() < 0.001 {
        return target;
    }
    let speed = if target > previous { per_report(0.5) } else { per_report(0.15) };
    previous + (target - previous) * speed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_attacks_fast_and_decays_slowly() {
        let up = smooth(-96., -20.);
        let down = smooth(-20., -96.);
        assert!(up > -30., "attack: {up}");
        assert!(down > -60. && down < -20., "decay: {down}");
        assert_eq!(smooth(-40., -40.0005), -40.0005, "snaps when close");
    }

    #[test]
    fn the_unavailable_test_refuses() {
        let sink: LevelSink = Arc::new(|_| {});
        assert_eq!(UnavailableMicTest.start(None, Processing::default(), sink), Err(MicTestError::Unavailable));
        UnavailableMicTest.stop();
    }
}

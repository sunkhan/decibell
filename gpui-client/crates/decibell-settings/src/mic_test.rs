//! The Audio tab's mic test outside voice (`AudioTab.tsx` `VoiceThresholdBar`: "Test mic" ran
//! `getUserMedia` → an `AnalyserNode` → RMS dB in a `requestAnimationFrame` loop, the mic
//! released the moment the tab or the test went).
//!
//! The core meters the mic only inside a voice session (`voice_input_level`); it has no capture
//! of its own outside one yet (docs/model.md M3.7 gap 2). So the tab talks to this narrow trait,
//! and the app installs [`UnavailableMicTest`] until the core grows the API
//! ([`crate::SettingsHooks::set_mic_test`]). What the core needs to provide, for an
//! implementation of [`MicTest`] over it:
//!
//! - `start_mic_test(device: Option<String>)`: open the selected input device (`None`: the OS
//!   default, as a voice session does) through the same capture path as voice (cpal / the
//!   platform backend, the same AEC / NS / AGC settings so the meter matches what voice sends),
//!   without joining anything and without the output stream;
//! - report the level as the voice pipeline does (`voice_input_level`'s RMS dB, −96 = silence,
//!   ~16 per second), through a callback or the existing event while the test runs;
//! - `stop_mic_test()`: close the device (the OS's mic indicator goes out); idempotent;
//! - refuse a test while a voice session or a call holds the mic (the session's meter is used
//!   then), and stop the test when one starts.

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

/// Opens the mic for the Audio tab's meter while no voice session does.
pub trait MicTest: 'static {
    /// Start capturing `device` (`None`: the OS default) and report its level to `level` until
    /// [`MicTest::stop`].
    fn start(&self, device: Option<&str>, level: LevelSink) -> Result<(), MicTestError>;
    /// Close the device. Idempotent.
    fn stop(&self);
}

/// No mic test: the core can't capture outside voice yet. The tab then behaves as the renderer
/// did when `getUserMedia` failed (the test shows as running, the meter stays at rest, the
/// failure is only logged).
pub struct UnavailableMicTest;

impl MicTest for UnavailableMicTest {
    fn start(&self, _: Option<&str>, _: LevelSink) -> Result<(), MicTestError> {
        Err(MicTestError::Unavailable)
    }

    fn stop(&self) {}
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
        assert_eq!(UnavailableMicTest.start(None, sink), Err(MicTestError::Unavailable));
        UnavailableMicTest.stop();
    }
}

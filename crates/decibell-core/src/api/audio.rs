//! Audio a client asks for outside the voice path's own flow: interface
//! sounds (join / leave, mute, the call ring, …), the microphone test of
//! Settings → Audio, and watching the audio devices.
//!
//! For a client with no audio layer of its own (the native client). The
//! Electron addon exports none of this: its renderer plays its sounds through
//! Web Audio, tests the mic with `getUserMedia` and listens to `devicechange`.

use crate::api::CoreError;
use crate::media::cue;

pub use crate::media::cue::Sound;

/// Load an interface sound from a WAVE file's bytes (16-bit PCM, mono or
/// stereo: the renderer's `assets/sounds/*.wav`). Keep the result; playing
/// it is then only a hand-off to the audio callback.
pub fn load_sound(wav: &[u8]) -> Result<Sound, CoreError> {
    Sound::from_wav(wav).map_err(CoreError::other)
}

/// Play `sound` once, at unity gain, over whatever else is playing (sounds
/// overlap, as the renderer's do). While a voice session or a call has an
/// output stream the sound is mixed into it (it plays on the voice output
/// device); otherwise a short-lived stream opens on `device` (None: the OS
/// default output, where the renderer's sounds play) and closes by itself.
/// Returns at once; never fails (a sound that can't play is logged).
pub fn play_sound(sound: &Sound, device: Option<&str>) {
    cue::play(sound, device);
}

/// Loop `sound` (the ringtone / ringback) until [`stop_ring`]. Looped by the
/// audio callback: no timer per repeat. Replaces a ring already playing.
pub fn start_ring(sound: &Sound, device: Option<&str>) {
    cue::start_ring(sound, device);
}

/// Stop the ring (a 5 ms fade). Idempotent.
pub fn stop_ring() {
    cue::stop_ring();
}

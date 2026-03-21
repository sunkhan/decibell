use super::codec::FRAME_SIZE;

const NOISE_GATE_MULTIPLIER: f32 = 3.0;
const NOISE_FLOOR_ALPHA: f32 = 0.01;
const SPEAKING_TRIGGER_FRAMES: u32 = 3;
const SILENCE_CLEAR_FRAMES: u32 = 5;

pub struct SpeakingDetector {
    noise_floor: f32,
    speaking: bool,
    consecutive_speaking: u32,
    consecutive_silent: u32,
}

impl SpeakingDetector {
    pub fn new() -> Self {
        SpeakingDetector {
            noise_floor: 100.0,
            speaking: false,
            consecutive_speaking: 0,
            consecutive_silent: 0,
        }
    }

    pub fn process(&mut self, pcm: &[i16; FRAME_SIZE]) -> Option<bool> {
        let rms = compute_rms(pcm);
        self.noise_floor =
            self.noise_floor * (1.0 - NOISE_FLOOR_ALPHA) + rms * NOISE_FLOOR_ALPHA;
        let threshold = self.noise_floor * NOISE_GATE_MULTIPLIER;
        let is_loud = rms > threshold;

        if is_loud {
            self.consecutive_speaking += 1;
            self.consecutive_silent = 0;
        } else {
            self.consecutive_silent += 1;
            self.consecutive_speaking = 0;
        }

        if !self.speaking && self.consecutive_speaking >= SPEAKING_TRIGGER_FRAMES {
            self.speaking = true;
            return Some(true);
        }
        if self.speaking && self.consecutive_silent >= SILENCE_CLEAR_FRAMES {
            self.speaking = false;
            return Some(false);
        }
        None
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    pub fn reset(&mut self) {
        self.noise_floor = 100.0;
        self.speaking = false;
        self.consecutive_speaking = 0;
        self.consecutive_silent = 0;
    }
}

fn compute_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples
        .iter()
        .map(|&s| (s as f64) * (s as f64))
        .sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

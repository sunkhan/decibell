const SPEAKING_TRIGGER_FRAMES: u32 = 3;
const SILENCE_CLEAR_FRAMES: u32 = 5;

pub struct SpeakingDetector {
    speaking: bool,
    consecutive_speaking: u32,
    consecutive_silent: u32,
}

impl SpeakingDetector {
    pub fn new() -> Self {
        SpeakingDetector {
            speaking: false,
            consecutive_speaking: 0,
            consecutive_silent: 0,
        }
    }

    /// Threshold-based speaking detection: caller provides whether audio is above threshold
    pub fn process_threshold(&mut self, above_threshold: bool) -> Option<bool> {
        if above_threshold {
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
        self.speaking = false;
        self.consecutive_speaking = 0;
        self.consecutive_silent = 0;
    }
}

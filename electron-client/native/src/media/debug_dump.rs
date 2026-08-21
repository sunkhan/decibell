//! Opt-in raw-audio capture for diagnosing playback artifacts.
//!
//! Set `DECIBELL_AUDIO_DUMP=/some/dir` before launching. While a voice
//! engine runs, the pipeline writes:
//!   - `peer-<name>.wav`  — each peer's decoded voice as pushed into its
//!                          playback ring (mono i16 @ 48 kHz, gain applied,
//!                          pre-resample). What the *decoder* produced.
//!   - `output.wav`       — the mono mix the output callback actually handed
//!                          to the device (i16 @ device rate), including any
//!                          zero-fill from a short ring. What the *ears* got.
//!   - `events.log`       — timestamped jitter-buffer / ring events.
//! Comparing the two WAVs tells a codec/sender artifact (present in both)
//! from a ring/device artifact (only in output.wav).

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Instant;

pub struct WavWriter {
    w: BufWriter<File>,
    data_bytes: u32,
}

impl WavWriter {
    pub fn create(path: PathBuf, rate: u32) -> std::io::Result<Self> {
        let mut w = BufWriter::new(File::create(path)?);
        // 44-byte PCM header, sizes patched in finish().
        w.write_all(b"RIFF")?;
        w.write_all(&0u32.to_le_bytes())?;
        w.write_all(b"WAVEfmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&1u16.to_le_bytes())?; // PCM
        w.write_all(&1u16.to_le_bytes())?; // mono
        w.write_all(&rate.to_le_bytes())?;
        w.write_all(&(rate * 2).to_le_bytes())?;
        w.write_all(&2u16.to_le_bytes())?;
        w.write_all(&16u16.to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&0u32.to_le_bytes())?;
        Ok(Self { w, data_bytes: 0 })
    }

    pub fn write(&mut self, samples: &[i16]) {
        for &s in samples {
            let _ = self.w.write_all(&s.to_le_bytes());
        }
        self.data_bytes = self.data_bytes.saturating_add((samples.len() * 2) as u32);
    }

    /// Flush buffered samples and patch the header sizes so the file is a
    /// valid WAV even if the process dies before a clean shutdown.
    pub fn finish(&mut self) {
        let _ = self.w.flush();
        let f = self.w.get_mut();
        let _ = f.seek(SeekFrom::Start(4));
        let _ = f.write_all(&(36 + self.data_bytes).to_le_bytes());
        let _ = f.seek(SeekFrom::Start(40));
        let _ = f.write_all(&self.data_bytes.to_le_bytes());
        let _ = f.seek(SeekFrom::End(0));
        let _ = f.flush();
    }
}

impl Drop for WavWriter {
    fn drop(&mut self) {
        self.finish();
    }
}

pub struct AudioDump {
    dir: PathBuf,
    t0: Instant,
    peers: std::collections::HashMap<String, WavWriter>,
    output: Option<WavWriter>,
    events: BufWriter<File>,
}

impl AudioDump {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("DECIBELL_AUDIO_DUMP")?);
        if std::fs::create_dir_all(&dir).is_err() {
            return None;
        }
        let events = BufWriter::new(File::create(dir.join("events.log")).ok()?);
        log::info!("[pipeline] Audio dump enabled → {}", dir.display());
        Some(Self { dir, t0: Instant::now(), peers: Default::default(), output: None, events })
    }

    pub fn event(&mut self, what: &str) {
        let ms = self.t0.elapsed().as_secs_f64() * 1000.0;
        let _ = writeln!(self.events, "{:10.1} {}", ms, what);
    }

    /// A frame as pushed into `name`'s ring (48 kHz mono, post-gain).
    pub fn peer_frame(&mut self, name: &str, pcm: &[f32]) {
        let dir = &self.dir;
        let w = self.peers.entry(name.to_string()).or_insert_with(|| {
            WavWriter::create(dir.join(format!("peer-{}.wav", name)), 48000).expect("dump file")
        });
        let buf: Vec<i16> = pcm.iter().map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect();
        w.write(&buf);
    }

    /// Samples the device actually received (mono, device rate).
    pub fn output_samples(&mut self, samples: &[i16], rate: u32) {
        let dir = &self.dir;
        let w = self.output.get_or_insert_with(|| {
            WavWriter::create(dir.join("output.wav"), rate).expect("dump file")
        });
        w.write(samples);
    }

    pub fn flush(&mut self) {
        let _ = self.events.flush();
        for w in self.peers.values_mut() { w.finish(); }
        if let Some(w) = self.output.as_mut() { w.finish(); }
    }
}

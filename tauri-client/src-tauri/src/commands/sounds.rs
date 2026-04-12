use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Embedded WAV files (mono 16-bit PCM 48kHz).
static SOUNDS: &[(&str, &[u8])] = &[
    ("mute", include_bytes!("../../sounds/mute.wav")),
    ("unmute", include_bytes!("../../sounds/unmute.wav")),
    ("deafen", include_bytes!("../../sounds/deafen.wav")),
    ("undeafen", include_bytes!("../../sounds/undeafen.wav")),
    ("user_join", include_bytes!("../../sounds/user_join.wav")),
    ("user_leave", include_bytes!("../../sounds/user_leave.wav")),
    ("stream_start", include_bytes!("../../sounds/stream_start.wav")),
    ("stream_stop", include_bytes!("../../sounds/stream_stop.wav")),
    ("connect", include_bytes!("../../sounds/connect.wav")),
    ("disconnect", include_bytes!("../../sounds/disconnect.wav")),
];

/// Parse a 16-bit PCM WAV file into f32 samples + sample rate.
fn parse_wav(data: &[u8]) -> Option<(Vec<f32>, u32)> {
    if data.len() < 44 { return None; }
    let mut offset = 12usize;
    let mut sample_rate = 48000u32;
    let mut bits_per_sample = 16u16;
    let mut channels = 1u16;
    let mut data_offset = 0usize;
    let mut data_size = 0usize;

    while offset + 8 <= data.len() {
        let id = &data[offset..offset + 4];
        let size = u32::from_le_bytes([data[offset+4], data[offset+5], data[offset+6], data[offset+7]]) as usize;
        if id == b"fmt " && offset + 24 <= data.len() {
            channels = u16::from_le_bytes([data[offset+10], data[offset+11]]);
            sample_rate = u32::from_le_bytes([data[offset+12], data[offset+13], data[offset+14], data[offset+15]]);
            bits_per_sample = u16::from_le_bytes([data[offset+22], data[offset+23]]);
        } else if id == b"data" {
            data_offset = offset + 8;
            data_size = size;
            break;
        }
        offset += 8 + size;
    }

    if data_offset == 0 || bits_per_sample != 16 { return None; }
    let bytes_per_sample = (bits_per_sample / 8) as usize;
    let total_samples = data_size / bytes_per_sample;

    // Mix down to mono if stereo
    let num_frames = total_samples / channels as usize;
    let mut samples = Vec::with_capacity(num_frames);
    for i in 0..num_frames {
        let mut sum = 0f32;
        for ch in 0..channels as usize {
            let pos = data_offset + (i * channels as usize + ch) * bytes_per_sample;
            if pos + 1 < data.len() {
                let s = i16::from_le_bytes([data[pos], data[pos + 1]]);
                sum += s as f32 / 32768.0;
            }
        }
        samples.push(sum / channels as f32);
    }

    Some((samples, sample_rate))
}

#[tauri::command]
pub fn play_sound(name: String) {
    let wav_data = match SOUNDS.iter().find(|(n, _)| *n == name.as_str()) {
        Some((_, data)) => *data,
        None => return,
    };

    thread::spawn(move || {
        let (mut samples, wav_rate) = match parse_wav(wav_data) {
            Some(v) => v,
            None => return,
        };

        let host = cpal::default_host();
        let device = match host.default_output_device() {
            Some(d) => d,
            None => return,
        };
        let config = match device.default_output_config() {
            Ok(c) => c,
            Err(_) => return,
        };

        let device_rate = config.sample_rate().0;
        let device_channels = config.channels() as usize;

        // Resample if rates differ
        if wav_rate != device_rate {
            let ratio = device_rate as f64 / wav_rate as f64;
            let out_len = (samples.len() as f64 * ratio) as usize;
            let mut out = Vec::with_capacity(out_len);
            for i in 0..out_len {
                let src_pos = i as f64 / ratio;
                let idx = src_pos as usize;
                let frac = (src_pos - idx as f64) as f32;
                let a = samples[idx.min(samples.len() - 1)];
                let b = samples[(idx + 1).min(samples.len() - 1)];
                out.push(a + (b - a) * frac);
            }
            samples = out;
        }

        // Add 100ms silence tail so the audio device can drain fully
        let tail_samples = device_rate as usize / 10;
        samples.extend(std::iter::repeat(0.0f32).take(tail_samples));

        // Apply short fade-in (2ms) and fade-out (5ms) to avoid pops
        let fade_in = (device_rate as usize * 2) / 1000;
        let fade_out = (device_rate as usize * 5) / 1000;
        let len = samples.len();
        for i in 0..fade_in.min(len) {
            samples[i] *= i as f32 / fade_in as f32;
        }
        for i in 0..fade_out.min(len) {
            samples[len - 1 - i] *= i as f32 / fade_out as f32;
        }

        let samples = std::sync::Arc::new(samples);
        let samples2 = samples.clone();
        let pos = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let pos2 = pos.clone();

        let stream = device.build_output_stream(
            &config.into(),
            move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let mut i = pos2.load(std::sync::atomic::Ordering::Relaxed);
                for frame in output.chunks_mut(device_channels) {
                    let val = if i < samples2.len() { samples2[i] } else { 0.0 };
                    for sample in frame.iter_mut() {
                        *sample = val;
                    }
                    if i < samples2.len() { i += 1; }
                }
                pos2.store(i, std::sync::atomic::Ordering::Relaxed);
            },
            |err| eprintln!("[sounds] playback error: {}", err),
            None,
        );

        if let Ok(s) = stream {
            let _ = s.play();
            // Poll until all samples (including silence tail) have been consumed
            let total = samples.len();
            loop {
                let current = pos.load(std::sync::atomic::Ordering::Relaxed);
                if current >= total { break; }
                thread::sleep(std::time::Duration::from_millis(10));
            }
            // Small extra sleep to let the device buffer drain
            thread::sleep(std::time::Duration::from_millis(30));
            // Stream dropped here — all audio has been played
        }
    });
}

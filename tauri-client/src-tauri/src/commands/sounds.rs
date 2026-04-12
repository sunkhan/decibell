//! UI sound effects.
//!
//! One persistent output stream is created on the first `play_sound` call and
//! kept open for the lifetime of the app — so it appears as a single long-lived
//! playback stream in `pavucontrol` / `wpctl`, not a flickering per-effect one.
//! Subsequent calls just push samples into a shared mixer queue.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

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

struct ActiveClip {
    samples: Arc<Vec<f32>>,
    pos: usize,
}

struct Mixer {
    bank: HashMap<&'static str, Arc<Vec<f32>>>,
    active: Arc<Mutex<Vec<ActiveClip>>>,
}

static MIXER: OnceLock<Option<Mixer>> = OnceLock::new();

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

fn resample_and_shape(mut samples: Vec<f32>, wav_rate: u32, device_rate: u32) -> Vec<f32> {
    if wav_rate != device_rate && !samples.is_empty() {
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

    let fade_in = (device_rate as usize * 2) / 1000;
    let fade_out = (device_rate as usize * 5) / 1000;
    let len = samples.len();
    for i in 0..fade_in.min(len) {
        samples[i] *= i as f32 / fade_in as f32;
    }
    for i in 0..fade_out.min(len) {
        samples[len - 1 - i] *= i as f32 / fade_out as f32;
    }
    samples
}

fn init_mixer() -> Option<Mixer> {
    let host = cpal::default_host();
    let device = host.default_output_device()?;
    let config = device.default_output_config().ok()?;
    let device_rate = config.sample_rate().0;
    let device_channels = config.channels() as usize;

    let mut bank: HashMap<&'static str, Arc<Vec<f32>>> = HashMap::new();
    for (name, data) in SOUNDS {
        if let Some((samples, rate)) = parse_wav(data) {
            let shaped = resample_and_shape(samples, rate, device_rate);
            bank.insert(*name, Arc::new(shaped));
        }
    }

    let active: Arc<Mutex<Vec<ActiveClip>>> = Arc::new(Mutex::new(Vec::new()));
    let active_cb = active.clone();

    // cpal::Stream is !Send; own it on a dedicated parked thread.
    std::thread::Builder::new()
        .name("decibell-sound-mixer".to_string())
        .spawn(move || {
            let stream_cfg: cpal::StreamConfig = config.into();
            let stream = device.build_output_stream(
                &stream_cfg,
                move |output: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    for s in output.iter_mut() { *s = 0.0; }
                    let Ok(mut clips) = active_cb.lock() else { return };
                    clips.retain(|c| c.pos < c.samples.len());
                    for clip in clips.iter_mut() {
                        let mut i = clip.pos;
                        for frame in output.chunks_mut(device_channels) {
                            if i >= clip.samples.len() { break; }
                            let v = clip.samples[i];
                            for sample in frame.iter_mut() { *sample += v; }
                            i += 1;
                        }
                        clip.pos = i;
                    }
                },
                |err| eprintln!("[sounds] mixer stream error: {}", err),
                None,
            );
            match stream {
                Ok(s) => {
                    if let Err(e) = s.play() {
                        eprintln!("[sounds] mixer play failed: {}", e);
                        return;
                    }
                    std::thread::park();
                    drop(s);
                }
                Err(e) => eprintln!("[sounds] mixer build failed: {}", e),
            }
        })
        .ok()?;

    Some(Mixer { bank, active })
}

#[tauri::command]
pub fn play_sound(name: String) {
    let mixer = MIXER.get_or_init(init_mixer);
    let Some(mixer) = mixer.as_ref() else { return };
    let Some(samples) = mixer.bank.get(name.as_str()) else { return };
    if let Ok(mut clips) = mixer.active.lock() {
        clips.push(ActiveClip { samples: samples.clone(), pos: 0 });
    }
}

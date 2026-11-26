//! Shared by the integration tests: the assets, a WAV writer, waiting.
#![allow(dead_code)]

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use decibell_media::{AudioOutput, Open, Phase, Player, Source};
use decibell_video::VideoHub;

pub const RATE: u32 = 48_000;

pub fn asset(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets").join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// A 48 kHz mono 16-bit WAV of `samples`.
pub fn wav(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    let bytes = samples.len() as u32 * 2;
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + bytes).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&bytes.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

pub fn open(
    source: impl Source,
    hub: Option<&VideoHub>,
    key: &str,
    output: Arc<dyn AudioOutput>,
    play: bool,
) -> Player {
    Player::open(Open {
        source: Box::new(source),
        video: hub.cloned(),
        key: key.into(),
        output,
        start_at: 0,
        play,
        volume: 1.0,
        muted: false,
        on_change: Box::new(|| {}),
    })
}

pub fn open_bytes(bytes: Vec<u8>, hub: Option<&VideoHub>, key: &str, output: Arc<dyn AudioOutput>, play: bool) -> Player {
    open(Cursor::new(bytes), hub, key, output, play)
}

pub fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
    let started = Instant::now();
    while !done() {
        assert!(started.elapsed() < limit, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn wait_phase(player: &Player, phase: Phase, limit: Duration) {
    let started = Instant::now();
    loop {
        let status = player.status();
        if status.phase == phase {
            return;
        }
        if let Phase::Failed(reason) = &status.phase {
            panic!("the player failed: {reason}");
        }
        assert!(started.elapsed() < limit, "timed out waiting for {phase:?}: {status:?}, at {}", player.position());
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// The sample indices where a click starts: a sample above `level` with none in the 50 ms
/// before it.
pub fn clicks(samples: &[f32], level: f32) -> Vec<usize> {
    let mut found = Vec::new();
    let mut last_loud: Option<usize> = None;
    for (index, sample) in samples.iter().enumerate() {
        if sample.abs() > level {
            if last_loud.is_none_or(|last| index - last >= RATE as usize / 20) {
                found.push(index);
            }
            last_loud = Some(index);
        }
    }
    found
}

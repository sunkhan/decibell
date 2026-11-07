//! The `stream-dump` cargo feature only: record watched streams as the renderer receives them, for the Qt client's video spike
//! (docs/superpowers/specs/2026-11-07-qt-native-client-design.md, D1: real streams, replayed with their
//! original timing into the Qt decode path).
//!
//! Build with `npx napi build --platform --release --features stream-dump` (optimized: a debug build is too slow to
//! keep up with a 120 fps stream) and set `DECIBELL_STREAM_DUMP=<dir>` before launching. Every frame handed to the stream bus
//! (`events::send_stream_frame`: remote streams after decryption, and the self-preview) is appended to
//! `<dir>/<media key>-<unix seconds>-<codec>.dbv`, a new file when the codec changes:
//!
//! ```text
//! "DBV2", u8 codec (the wire VideoCodec: 1/2 H.264, 3 H.265, 4 AV1), 3 zero bytes, u32 width 0, u32 height 0,
//! u32 fps 0 (unknown here: the bitstream says), then per frame: u32 size, u32 flags (bit 0 keyframe, bit 1 a
//! description follows), u64 arrival in µs since the file's first frame, [u32 size, the description (hvcC /
//! av1C)], the frame's bytes.
//! ```
//!
//! Little-endian. Read by `qt-client/src/video/streamfile.h`. Not in any shipped build (the feature is off).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Dump {
    codec: u8,
    start: Instant,
    out: BufWriter<File>,
}

struct Dumps {
    dir: PathBuf,
    open: HashMap<String, Dump>,
}

fn dumps() -> Option<&'static Mutex<Dumps>> {
    static D: OnceLock<Option<Mutex<Dumps>>> = OnceLock::new();
    D.get_or_init(|| {
        let dir = std::env::var_os("DECIBELL_STREAM_DUMP")?;
        let dir = PathBuf::from(dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("[stream-dump] cannot create {}: {e}", dir.display());
            return None;
        }
        log::info!("[stream-dump] recording watched streams into {}", dir.display());
        Some(Mutex::new(Dumps { dir, open: HashMap::new() }))
    })
    .as_ref()
}

fn open(dir: &PathBuf, key: &str, codec: u8) -> Option<Dump> {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let name = match codec {
        3 => "hevc",
        4 => "av1",
        _ => "h264",
    };
    let safe: String = key.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).collect();
    let path = dir.join(format!("{safe}-{secs}-{name}.dbv"));
    let mut out = BufWriter::new(File::create(&path).ok()?);
    let mut head = [0u8; 20];
    head[..4].copy_from_slice(b"DBV2");
    head[4] = codec;
    out.write_all(&head).ok()?;
    log::info!("[stream-dump] {} -> {}", key, path.display());
    Some(Dump { codec, start: Instant::now(), out })
}

/// Appends one frame of `key`'s stream (no-op unless `DECIBELL_STREAM_DUMP` is set).
pub fn record(key: &str, codec: u8, keyframe: bool, data: &[u8], description: Option<&[u8]>) {
    let Some(d) = dumps() else { return };
    let Ok(mut d) = d.lock() else { return };
    let Dumps { dir, open: files } = &mut *d;
    if files.get(key).is_some_and(|f| f.codec != codec) {
        files.remove(key); // the streamer switched codec: a new file
    }
    if !files.contains_key(key) {
        match open(dir, key, codec) {
            Some(f) => {
                files.insert(key.to_string(), f);
            }
            None => return,
        }
    }
    let f = files.get_mut(key).expect("inserted above");
    let at = f.start.elapsed().as_micros() as u64;
    let flags = u32::from(keyframe) | if description.is_some() { 2 } else { 0 };
    let mut w = || -> std::io::Result<()> {
        f.out.write_all(&(data.len() as u32).to_le_bytes())?;
        f.out.write_all(&flags.to_le_bytes())?;
        f.out.write_all(&at.to_le_bytes())?;
        if let Some(desc) = description {
            f.out.write_all(&(desc.len() as u32).to_le_bytes())?;
            f.out.write_all(desc)?;
        }
        f.out.write_all(data)?;
        // Flush per keyframe so a killed process still leaves a playable file.
        if keyframe {
            f.out.flush()?;
        }
        Ok(())
    };
    if let Err(e) = w() {
        log::warn!("[stream-dump] {key}: write failed ({e}); stopping this file");
        files.remove(key);
    }
}

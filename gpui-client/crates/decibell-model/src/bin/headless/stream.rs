//! `stream-file`: a test-only streamer for `decibell-headless`. It announces a stream in the
//! connected voice channel exactly as the renderer's WebCodecs path does (`start_screen_share`
//! with `native_encode: false`: the core makes the stream's `VideoEngine`, MLS-sealed like every
//! stream of a channel session), or, in an active DM call, to the call's peer as the renderer's
//! call share does (the same start with no community, then STREAM_START over central; the frames
//! go sealed over the call's own socket), then feeds a `.dbv` recording's pre-encoded frames through the
//! core's `send_video_frame` at the recording's timing, looping. A watcher's keyframe request
//! (the core's `keyframe_requested`) makes it jump to the recording's next keyframe, as the
//! renderer forced one in its encoder. No capture, no encoder: the tool `tools/perf/streambot.sh`
//! and the voice self-tests use it to put real streams on the local stack.
//!
//! `.dbv`: see `gpui-client/crates/decibell-video/src/dbv.rs` (DBV1 synthetic, played at its
//! fps; DBV2 a recording, played on its arrival times). Only for the local stack (the binary
//! refuses any other central).

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use decibell_core::api::streaming;
use decibell_core::events::CoreEvent;
use decibell_model::live::calls::CallStatus;
use decibell_model::live::voice::VideoCodec;
use decibell_model::live::voice::streams::{LiveStream, OwnStreamPhase};
use decibell_model::{Model, actions};
use gpui::{AsyncApp, SharedString, Subscription};
use serde_json::json;

pub const USAGE: &str = "\
Streaming (test only):
  stream-file <file.dbv>          stream a .dbv recording into the connected voice channel, or
                                  to the peer of an active DM call: pre-encoded frames through the
                                  core (sealed), looped, at the file's timing; keyframe requests
                                  jump to its next keyframe
  stream-pause                    stop sending frames (the stream stays announced: what a
                                  watcher sees when the streamer's frames no longer arrive)
  stream-resume                   send again, from where it stopped
  stream-stop                     end it (announced as ended)";

#[derive(Debug, Clone)]
pub enum StreamStep {
    File(PathBuf),
    Pause(bool),
    Stop,
}

pub fn parse(arg: &str, it: &mut std::vec::IntoIter<String>) -> Result<Option<StreamStep>, String> {
    Ok(Some(match arg {
        "stream-file" => StreamStep::File(PathBuf::from(it.next().ok_or("stream-file: missing the file")?)),
        "stream-pause" => StreamStep::Pause(true),
        "stream-resume" => StreamStep::Pause(false),
        "stream-stop" => StreamStep::Stop,
        _ => return Ok(None),
    }))
}

/// A `.dbv` frame index: where each frame starts and whether it is a keyframe.
struct Recording {
    path: PathBuf,
    version: u8,
    /// The wire's VideoCodec byte.
    codec: u8,
    width: u32,
    height: u32,
    fps: u32,
    /// (offset of the frame's header, keyframe).
    frames: Vec<(u64, bool)>,
}

const HEADER: u64 = 20;

impl Recording {
    fn open(path: &Path) -> Result<Recording, String> {
        let mut file = BufReader::new(File::open(path).map_err(|e| format!("{}: {e}", path.display()))?);
        let mut head = [0u8; HEADER as usize];
        file.read_exact(&mut head).map_err(|e| format!("{}: {e}", path.display()))?;
        if &head[..3] != b"DBV" || !(head[3] == b'1' || head[3] == b'2') {
            return Err(format!("{}: not a .dbv file", path.display()));
        }
        let version = head[3] - b'0';
        let codec = match (version, head[4]) {
            (1, 1) => 1,
            (1, 2) => 3,
            (1, 3) => 4,
            (2, c @ 1..=4) => c,
            (_, c) => return Err(format!("{}: unknown codec id {c}", path.display())),
        };
        let word = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
        let (width, height, fps) = (word(8), word(12), word(16));
        let mut frames = Vec::new();
        let mut at = HEADER;
        loop {
            let mut fh = [0u8; 8];
            if file.read_exact(&mut fh).is_err() {
                break;
            }
            let size = u32::from_le_bytes([fh[0], fh[1], fh[2], fh[3]]) as u64;
            let flags = u32::from_le_bytes([fh[4], fh[5], fh[6], fh[7]]);
            let mut skip = size;
            let mut header = 8;
            if version == 2 {
                header += 8;
                let mut arrival = [0u8; 8];
                file.read_exact(&mut arrival).map_err(|e| e.to_string())?;
                if flags & 2 != 0 {
                    let mut len = [0u8; 4];
                    file.read_exact(&mut len).map_err(|e| e.to_string())?;
                    let len = u32::from_le_bytes(len) as u64;
                    header += 4 + len;
                    skip += len;
                }
            }
            frames.push((at, flags & 1 != 0));
            file.seek_relative(skip as i64).map_err(|e| e.to_string())?;
            at += header + size;
        }
        if frames.is_empty() || !frames[0].1 {
            return Err(format!("{}: no frames, or the first is not a keyframe", path.display()));
        }
        Ok(Recording { path: path.to_path_buf(), version, codec, width, height, fps: fps.max(1), frames })
    }
}

struct Bot {
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    sent: Arc<AtomicU64>,
    jumps: Arc<AtomicU64>,
    _requests: Subscription,
}

thread_local! {
    static BOT: RefCell<Option<Bot>> = const { RefCell::new(None) };
}

/// The feeding thread: frame `i` of a DBV1 file is due at `i / fps` (no drift), DBV2 frames at
/// their recorded arrival; a keyframe request jumps to the next keyframe and re-bases the clock.
fn feed(
    rec: Recording,
    stop: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
    want_key: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
    jumps: Arc<AtomicU64>,
) {
    let Ok(file) = File::open(&rec.path) else { return };
    let mut file = BufReader::with_capacity(1 << 20, file);
    let mut data: Vec<u8> = Vec::with_capacity(256 << 10);
    let mut description: Vec<u8> = Vec::new();
    let frame_time = Duration::from_secs_f64(1. / rec.fps as f64);
    let mut base = Instant::now();
    let mut played: u64 = 0;
    let mut index = 0usize;
    let mut first_arrival: Option<u64> = None;
    let mut arrival_base = 0u64;
    while !stop.load(Ordering::Relaxed) {
        if paused.load(Ordering::Acquire) {
            // Parked until `stream-resume` (or the end); the clock starts over from there.
            while paused.load(Ordering::Acquire) && !stop.load(Ordering::Relaxed) {
                std::thread::park();
            }
            base = Instant::now();
            played = 0;
            first_arrival = None;
            arrival_base = 0;
            continue;
        }
        if want_key.swap(false, Ordering::AcqRel) {
            // The next keyframe after this frame (wrapping to the first).
            let next = rec.frames.iter().enumerate().skip(index).find(|(_, f)| f.1).map(|(i, _)| i).unwrap_or(0);
            if next != index {
                index = next;
                jumps.fetch_add(1, Ordering::Relaxed);
            }
            base = Instant::now();
            played = 0;
            first_arrival = None;
        }
        if index >= rec.frames.len() {
            index = 0;
            if rec.version == 2 {
                arrival_base = played * frame_time.as_micros() as u64;
                first_arrival = None;
            }
        }
        let (offset, keyframe) = rec.frames[index];
        if file.seek(SeekFrom::Start(offset)).is_err() {
            break;
        }
        let mut fh = [0u8; 8];
        if file.read_exact(&mut fh).is_err() {
            break;
        }
        let size = u32::from_le_bytes([fh[0], fh[1], fh[2], fh[3]]) as usize;
        let flags = u32::from_le_bytes([fh[4], fh[5], fh[6], fh[7]]);
        let mut due = base + frame_time * played as u32;
        description.clear();
        let mut has_description = false;
        if rec.version == 2 {
            let mut arrival = [0u8; 8];
            if file.read_exact(&mut arrival).is_err() {
                break;
            }
            let at = u64::from_le_bytes(arrival);
            let first = *first_arrival.get_or_insert(at);
            due = base + Duration::from_micros(arrival_base + at.saturating_sub(first));
            if flags & 2 != 0 {
                let mut len = [0u8; 4];
                if file.read_exact(&mut len).is_err() {
                    break;
                }
                description.resize(u32::from_le_bytes(len) as usize, 0);
                if file.read_exact(&mut description).is_err() {
                    break;
                }
                has_description = true;
            }
        }
        data.resize(size, 0);
        if file.read_exact(&mut data).is_err() {
            break;
        }
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        let desc = has_description.then_some(description.as_slice());
        if streaming::send_video_frame(rec.codec, keyframe, &data, desc).is_ok() {
            sent.fetch_add(1, Ordering::Relaxed);
        }
        index += 1;
        played += 1;
    }
}

pub async fn run(step: &StreamStep, model: &Model, cx: &mut AsyncApp) -> Result<(), String> {
    match step {
        StreamStep::File(path) => {
            if BOT.with(|b| b.borrow().is_some()) {
                return Err("stream-file: already streaming".into());
            }
            let rec = Recording::open(path)?;
            let (key, in_call) = cx.update(|cx| {
                let session = model.live.voice.session.read(cx);
                let call = model.live.calls.read(cx).status == CallStatus::Active && session.call_peer.is_some();
                (session.connected(), call)
            });
            if key.is_none() && !in_call {
                return Err("stream-file: not in a voice channel or an active call (voice-join or a call first)".into());
            }
            let args = streaming::StartScreenShareArgs {
                server_id: key.as_ref().map(|k| k.0.wire()),
                channel_id: key.as_ref().map(|k| k.1.to_string()),
                fps: rec.fps,
                width: rec.width,
                height: rec.height,
                // What the recording was encoded at, roughly (the server only relays it).
                video_bitrate_kbps: 20_000,
                share_audio: false,
                audio_bitrate_kbps: 128,
                initial_codec: rec.codec,
                enforced_codec: 0,
                source_id: None,
                // The renderer's path: the core announces and packetises; we bring the frames.
                native_encode: Some(false),
                include_cursor: None,
                detail: None,
                audio_mode: None,
                audio_apps: None,
            };
            let started = streaming::start_screen_share(args).await.map_err(|e| format!("stream-file: {e}"))?;
            if in_call {
                // No community to announce it: the peer hears STREAM_START over central, and our
                // own entry is listed, as the app's Go Live does in a call.
                let live = LiveStream {
                    codec: VideoCodec::from_wire(rec.codec as i32),
                    // A recording is one codec from start to end: announced locked to it.
                    enforced: VideoCodec::from_wire(rec.codec as i32),
                    width: started.width,
                    height: started.height,
                    fps: rec.fps,
                    has_audio: false,
                    encoder: SharedString::default(),
                    server: None,
                    channel: None,
                };
                cx.update(|cx| {
                    model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Live(live.clone()), cx));
                    actions::calls::announce_stream_start(model, &live, cx);
                });
            }
            let stop = Arc::new(AtomicBool::new(false));
            let paused = Arc::new(AtomicBool::new(false));
            let want_key = Arc::new(AtomicBool::new(false));
            let sent = Arc::new(AtomicU64::new(0));
            let jumps = Arc::new(AtomicU64::new(0));
            let requests = cx.update(|cx| {
                let want_key = want_key.clone();
                cx.subscribe(&model.answers, move |_, event: &CoreEvent, _| {
                    if matches!(event, CoreEvent::KeyframeRequested) {
                        want_key.store(true, Ordering::Release);
                    }
                })
            });
            crate::say(format!(
                "STREAMING {} {}x{} {} fps codec {} (announced {}x{})",
                rec.path.display(),
                rec.width,
                rec.height,
                rec.fps,
                rec.codec,
                started.width,
                started.height
            ));
            let thread = std::thread::Builder::new()
                .name("decibell-streambot".into())
                .spawn({
                    let (stop, want_key, sent, jumps) = (stop.clone(), want_key.clone(), sent.clone(), jumps.clone());
                    let paused = paused.clone();
                    move || feed(rec, stop, paused, want_key, sent, jumps)
                })
                .map_err(|e| format!("stream-file: {e}"))?;
            BOT.with(|b| *b.borrow_mut() = Some(Bot { stop, paused, thread: Some(thread), sent, jumps, _requests: requests }));
        }
        StreamStep::Pause(on) => {
            let found = BOT.with(|b| {
                let bot = b.borrow();
                let Some(bot) = bot.as_ref() else { return false };
                bot.paused.store(*on, Ordering::Release);
                if let Some(thread) = &bot.thread {
                    thread.thread().unpark();
                }
                true
            });
            if !found {
                return Err("stream-pause / stream-resume: not streaming".into());
            }
            crate::say(if *on { "STREAM-PAUSED" } else { "STREAM-RESUMED" }.to_string());
        }
        StreamStep::Stop => stop(model, cx).await,
    }
    Ok(())
}

/// End the stream (also at exit): the thread stops, the core stops and announces it ended.
pub async fn stop(model: &Model, cx: &mut AsyncApp) {
    let Some(mut bot) = BOT.with(|b| b.borrow_mut().take()) else { return };
    bot.stop.store(true, Ordering::Relaxed);
    if let Some(thread) = bot.thread.take() {
        thread.thread().unpark();
        let _ = thread.join();
    }
    let key = cx.update(|cx| model.live.voice.session.read(cx).connected());
    let _ = streaming::stop_screen_share(streaming::StopScreenShareArgs {
        server_id: key.as_ref().map(|k| k.0.wire()),
        channel_id: key.as_ref().map(|k| k.1.to_string()),
    })
    .await;
    // In a call: the peer hears STREAM_STOP (a no-op outside one, or after the call ended).
    cx.update(|cx| {
        if model.live.voice.own_stream.read(cx).is_live() {
            model.live.voice.own_stream.update(cx, |o, cx| o.set_phase(OwnStreamPhase::Idle, cx));
        }
        actions::calls::announce_stream_stop(model, cx);
    });
    crate::say(format!(
        "STREAM-STOPPED sent {} frames, {} keyframe jumps",
        bot.sent.load(Ordering::Relaxed),
        bot.jumps.load(Ordering::Relaxed)
    ));
}

/// `dump streambot`.
pub fn dump() -> serde_json::Value {
    BOT.with(|b| match &*b.borrow() {
        Some(bot) => json!({ "streaming": true, "sent": bot.sent.load(Ordering::Relaxed), "jumps": bot.jumps.load(Ordering::Relaxed) }),
        None => json!({ "streaming": false }),
    })
}

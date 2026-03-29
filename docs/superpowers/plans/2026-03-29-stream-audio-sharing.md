# Stream Audio Sharing — Implementation Plan

**Spec:** `docs/superpowers/specs/2026-03-29-stream-audio-sharing-design.md`
**Date:** 2026-03-29

---

## Step 1: Add `PACKET_TYPE_STREAM_AUDIO` and packet constructor

**File:** `tauri-client/src-tauri/src/media/packet.rs`

Add `pub const PACKET_TYPE_STREAM_AUDIO: u8 = 4;` alongside the other packet types.

Add `new_stream_audio()` constructor to `UdpAudioPacket`:

```rust
pub fn new_stream_audio(sender_id_str: &str, sequence: u16, opus_data: &[u8]) -> Self {
    let mut sender_id = [0u8; SENDER_ID_SIZE];
    let bytes = sender_id_str.as_bytes();
    let len = bytes.len().min(SENDER_ID_SIZE);
    sender_id[..len].copy_from_slice(&bytes[..len]);

    let mut payload = [0u8; MAX_PAYLOAD_SIZE];
    let data_len = opus_data.len().min(MAX_PAYLOAD_SIZE);
    payload[..data_len].copy_from_slice(&opus_data[..data_len]);

    UdpAudioPacket {
        packet_type: PACKET_TYPE_STREAM_AUDIO,
        sender_id,
        sequence,
        payload_size: data_len as u16,
        payload,
    }
}
```

---

## Step 2: Add stereo Opus encoder/decoder to `codec.rs`

**File:** `tauri-client/src-tauri/src/media/codec.rs`

Add stereo constants and stereo encoder/decoder structs:

```rust
pub const STEREO_CHANNELS: u16 = 2;
pub const STEREO_FRAME_SIZE: usize = 960; // 20ms at 48kHz — samples PER CHANNEL
pub const STEREO_FRAME_SAMPLES: usize = STEREO_FRAME_SIZE * 2; // total i16 samples (interleaved L,R)
```

**`StereoOpusEncoder`**:
- `Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::Audio)` — Audio profile (not VoIP) for music/game content
- `set_bitrate(Bitrate::BitsPerSecond(bitrate))` in constructor, taking `bitrate_bps: i32` param
- `encode(&self, pcm: &[i16], output: &mut [u8; MAX_OPUS_FRAME_SIZE]) -> Result<usize, String>` — expects 1920 interleaved i16 samples (960 per channel)
- `encode_silence()` — encodes 1920 zeros

**`StereoOpusDecoder`**:
- `Decoder::new(SampleRate::Hz48000, Channels::Stereo)`
- `decode(&mut self, opus_data: &[u8], output: &mut [i16; STEREO_FRAME_SAMPLES]) -> Result<usize, String>` — outputs 1920 interleaved i16 samples

---

## Step 3: Create `AudioFrame` and `audio_stream_pipeline.rs`

**File:** `tauri-client/src-tauri/src/media/capture.rs` — add `AudioFrame`:

```rust
#[derive(Debug)]
pub struct AudioFrame {
    pub data: Vec<f32>,      // interleaved stereo f32 PCM
    pub channels: u16,       // always 2
    pub sample_rate: u32,    // always 48000
}
```

**New file:** `tauri-client/src-tauri/src/media/audio_stream_pipeline.rs`

This pipeline runs on a dedicated thread, mirroring `video_pipeline.rs`:

```rust
pub enum AudioStreamControl {
    Shutdown,
}

pub enum AudioStreamEvent {
    Started,
    Stopped,
    Error(String),
}

pub fn run_audio_stream_pipeline(
    frame_rx: std::sync::mpsc::Receiver<AudioFrame>,
    control_rx: std::sync::mpsc::Receiver<AudioStreamControl>,
    event_tx: std::sync::mpsc::Sender<AudioStreamEvent>,
    socket: Arc<UdpSocket>,
    sender_id: String,
    bitrate_kbps: u32,
)
```

**Pipeline logic:**

1. Create `StereoOpusEncoder` with `bitrate_kbps * 1000`
2. Emit `AudioStreamEvent::Started`
3. Main loop:
   - Check `control_rx.try_recv()` for `Shutdown` / `Disconnected`
   - `frame_rx.recv_timeout(50ms)`:
     - On `Ok(frame)`: convert f32 interleaved stereo to i16 interleaved stereo (multiply by 32767, clamp). Accumulate in an internal buffer. While buffer has >= `STEREO_FRAME_SAMPLES` (1920) samples, drain 1920 samples, Opus-encode, create `UdpAudioPacket::new_stream_audio()`, send via socket. Increment sequence.
     - On `Timeout`: continue
     - On `Disconnected`: break
4. Emit `AudioStreamEvent::Stopped`

**Key details:**
- Sequence number: `u16`, wrapping
- f32→i16 conversion: `(sample * 32767.0).clamp(-32768.0, 32767.0) as i16`
- Buffer accumulation is needed because platform capture may deliver arbitrary chunk sizes, but Opus needs exactly 960 samples per channel (1920 interleaved i16s)
- Log at start and every 500 frames for monitoring

---

## Step 4: Linux audio capture — `capture_audio_pipewire.rs`

**New file:** `tauri-client/src-tauri/src/media/capture_audio_pipewire.rs`

Two public functions:

### `start_process_audio_capture(pid: u32) -> Result<Receiver<AudioFrame>, String>`

Captures audio from a specific process by PID.

**Implementation:**
1. `pw::init()`
2. Create `MainLoop`, `Context`, connect to default PipeWire server
3. Use the PipeWire registry to find the audio sink-input node whose `application.process.id` property matches the target PID
4. Create a PipeWire stream connecting to that node as a capture stream
5. Negotiate format: F32LE stereo 48kHz (PipeWire's native format)
6. In `process` callback: read buffer data as interleaved f32 stereo, send as `AudioFrame` via `SyncSender`
7. Run the mainloop on the current thread (called from a spawned thread)

**Finding the right node:**
- Use `pw::registry::Registry` to enumerate objects
- Filter for `PipeWireObject::Node` where `media.class` is `"Stream/Output/Audio"` and `application.process.id` matches the PID
- If no match found within 5 seconds, return error "No audio stream found for PID {pid}"

### `start_system_audio_capture() -> Result<(Receiver<AudioFrame>, Box<dyn FnOnce()>), String>`

Captures system audio (loopback of default sink) minus Decibell.

Returns the receiver AND a cleanup closure that restores Decibell's audio routing.

**Implementation:**
1. Find the default audio sink node via PipeWire registry (`media.class` == `"Audio/Sink"` with highest priority or marked as default)
2. Find Decibell's own playback node (our CPAL output — match by `application.process.id` == our PID)
3. Create a temporary private sink (a PipeWire null-sink via `pw::node::Node` with `factory.name` = `"support.null-audio-sink"`) named `"decibell-private"`
4. Use `pw::link::Link` or module-link to redirect Decibell's playback output to the private sink (unlink from default, link to private)
5. Create a capture stream connected to the default sink's monitor (loopback)
6. Negotiate F32LE stereo 48kHz
7. In `process` callback: read interleaved f32 stereo → `AudioFrame` → `SyncSender`
8. The cleanup closure: restore Decibell's link to the default sink, destroy the private sink

**Alternative simpler approach (preferred):** Instead of creating a virtual sink and re-linking, use PipeWire's `stream.capture.sink` with `target.object` set to the default sink's monitor node. Then handle Decibell exclusion by: before capturing, use `wpctl` or PipeWire's metadata to move Decibell's stream to a null sink. On cleanup, move it back.

Actually, the **simplest correct approach**:
1. Use `pipewire` crate to create a capture stream targeting the default sink's `.monitor` port
2. Before starting: use `libpipewire` API to find our own CPAL output node by PID, then set its `target.node` metadata to redirect to a newly created null-sink
3. On cleanup: remove the metadata override so CPAL goes back to the default sink

This avoids complex re-linking. PipeWire's `target.node` property on a stream overrides its routing.

---

## Step 5: Windows audio capture — `capture_audio_wasapi.rs`

**New file:** `tauri-client/src-tauri/src/media/capture_audio_wasapi.rs`

Two public functions:

### `start_process_audio_capture(pid: u32) -> Result<Receiver<AudioFrame>, String>`

Uses WASAPI Process Loopback to capture audio from a specific process.

**Implementation:**
1. Create activation params:
   ```rust
   AUDIOCLIENT_ACTIVATION_PARAMS {
       ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
       ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
           TargetProcessId: pid,
           ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
       },
   }
   ```
2. Call `ActivateAudioInterfaceAsync` with `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` as device ID
3. Implement `IActivateAudioInterfaceCompletionHandler` callback to receive the `IAudioClient`
4. Initialize the audio client: `Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK, ...)`
5. Get `IAudioCaptureClient`
6. Create event handle, set on audio client
7. Spawn capture loop: wait on event, `GetBuffer()`, convert to f32 stereo 48kHz, send as `AudioFrame`
8. Handle format negotiation: WASAPI may provide any format — use `GetMixFormat()` to check, resample if needed using a simple linear resampler or by requesting 48kHz stereo f32 directly

**Windows crate features needed** (add to `Cargo.toml`):
- `Win32_Media_Audio`
- `Win32_System_Com`
- `Win32_System_Threading` (for event handles)

### `start_system_audio_capture() -> Result<Receiver<AudioFrame>, String>`

Same as above but with `PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE` and `TargetProcessId` set to `std::process::id()`.

**Implementation identical to per-process**, just two param differences:
- `TargetProcessId: std::process::id()`
- `ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE`

---

## Step 6: Wire audio capture into `AudioEngine` in `mod.rs`

**File:** `tauri-client/src-tauri/src/media/mod.rs`

Add module declarations:
```rust
pub mod audio_stream_pipeline;
#[cfg(target_os = "linux")]
pub mod capture_audio_pipewire;
#[cfg(target_os = "windows")]
pub mod capture_audio_wasapi;
```

Add `AudioStreamEngine` struct (analogous to `VideoEngine`):

```rust
pub struct AudioStreamEngine {
    pipeline_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<audio_stream_pipeline::AudioStreamControl>,
    #[cfg(target_os = "linux")]
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}
```

**`AudioStreamEngine::start()`**:
- Takes `frame_rx: Receiver<AudioFrame>`, `socket`, `sender_id`, `bitrate_kbps`, `app`
- Spawns `audio_stream_pipeline::run_audio_stream_pipeline` on a dedicated thread
- Bridges events to Tauri (errors only, same pattern as VideoEngine)
- Optionally stores a Linux cleanup closure

**`AudioStreamEngine::stop()`**:
- Sends `AudioStreamControl::Shutdown`
- Joins thread
- Aborts event bridge
- On Linux: calls cleanup closure if present (restores PipeWire routing)

---

## Step 7: Add `audio_stream_engine` to `AppState`

**File:** `tauri-client/src-tauri/src/state.rs`

Add field:
```rust
pub audio_stream_engine: Option<crate::media::AudioStreamEngine>,
```

---

## Step 8: Wire audio capture into `start_screen_share` / `stop_screen_share`

**File:** `tauri-client/src-tauri/src/commands/streaming.rs`

**`start_screen_share`** — already has `share_audio: bool` parameter. Add `audio_bitrate_kbps: u32` parameter (default 128 from frontend).

After the video engine is started, if `share_audio` is true:

```rust
if share_audio {
    let audio_frame_rx = start_audio_capture(&source_id).await?;
    let audio_engine = AudioStreamEngine::start(
        audio_frame_rx,
        socket.clone(),
        sender_id.clone(),
        audio_bitrate_kbps,
        app.clone(),
    );
    s.audio_stream_engine = Some(audio_engine);
}
```

**`start_audio_capture()`** — new helper function in `streaming.rs` or in `capture.rs`:

```rust
async fn start_audio_capture(source_id: &str) -> Result<Receiver<AudioFrame>, String> {
    let is_window = !source_id.starts_with("monitor:") && source_id != "portal";
    // On Linux, portal-selected sources don't tell us if it's a window or screen,
    // so we need a way to determine this. For now, Linux screen shares always use
    // system-minus-self since the portal handles both screens and windows.

    #[cfg(target_os = "linux")]
    {
        if is_window {
            // For PipeWire window captures, we need the PID.
            // The portal doesn't directly give us PID — we'd need to get it from
            // the PipeWire node properties. For Linux, since the portal handles
            // everything, use system-minus-self for all captures for now,
            // and add per-process capture when we implement our own window picker.
            let (rx, cleanup) = super::media::capture_audio_pipewire::start_system_audio_capture()?;
            // Store cleanup... (handled by AudioStreamEngine)
            Ok(rx)
        } else {
            let (rx, cleanup) = super::media::capture_audio_pipewire::start_system_audio_capture()?;
            Ok(rx)
        }
    }
    #[cfg(target_os = "windows")]
    {
        if is_window {
            // Extract PID from the window handle in source_id
            let pid = get_pid_from_source_id(source_id)?;
            super::media::capture_audio_wasapi::start_process_audio_capture(pid)
        } else {
            super::media::capture_audio_wasapi::start_system_audio_capture()
        }
    }
}
```

On Windows, the source_id for windows encodes an HWND — we need a helper to get the PID from HWND using `GetWindowThreadProcessId`.

**`stop_screen_share`** — add audio engine teardown before video:

```rust
if let Some(mut engine) = s.audio_stream_engine.take() {
    engine.stop();
}
```

Also add the same to `leave_voice_channel` in `voice.rs` and the `on_window_event` shutdown handler in `lib.rs`.

---

## Step 9: Receiver — handle `PACKET_TYPE_STREAM_AUDIO` in `pipeline.rs`

**File:** `tauri-client/src-tauri/src/media/pipeline.rs`

**Changes to `RemotePeer`:**
```rust
struct RemotePeer {
    decoder: OpusDecoder,
    speaking: SpeakingDetector,
    last_seq: u16,
    last_packet_time: Instant,
    // Stream audio
    stream_audio_decoder: Option<StereoOpusDecoder>,
    stream_audio_seq: u16,
}
```

**Changes to the recv loop (section 4):**

After the existing audio packet handling block (around line 429), add a new branch for `PACKET_TYPE_STREAM_AUDIO`:

```rust
} else if pkt.packet_type == PACKET_TYPE_STREAM_AUDIO {
    let username = pkt.sender_username();
    if username == sender_id {
        // Ignore our own reflected stream audio
    } else {
        let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
            RemotePeer { /* ... default init ... */ }
        });

        // Lazy-init stereo decoder on first stream audio packet
        if peer.stream_audio_decoder.is_none() {
            peer.stream_audio_decoder = StereoOpusDecoder::new().ok();
        }

        // Sequence check (same logic as voice)
        let diff = pkt.sequence.wrapping_sub(peer.stream_audio_seq);
        if diff == 0 || diff > 32768 {
            // skip stale/duplicate
        } else {
            peer.stream_audio_seq = pkt.sequence;
            peer.last_packet_time = Instant::now();

            if let Some(ref mut decoder) = peer.stream_audio_decoder {
                let opus_data = pkt.payload_data();
                let mut pcm = [0i16; STEREO_FRAME_SAMPLES]; // 1920
                match decoder.decode(opus_data, &mut pcm) {
                    Ok(_) => {
                        if !deafened {
                            // Apply stream audio volume (from shared atomic)
                            // Mix stereo into playback buffer
                            let mut pbuf = playback_buf.lock().unwrap();
                            let remaining = BUF_CAP.saturating_sub(pbuf.len());
                            // Downmix stereo to mono for the current mono playback path:
                            // (L + R) / 2
                            let mono_samples = STEREO_FRAME_SIZE; // 960
                            let take = mono_samples.min(remaining);
                            for i in 0..take {
                                let l = pcm[i * 2] as i32;
                                let r = pcm[i * 2 + 1] as i32;
                                let mono = ((l + r) / 2) as i16;
                                // Apply volume scalar
                                let scaled = ((mono as f32) * stream_volume) as i16;
                                pbuf.push_back(scaled);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("[stream-audio-recv] Decode error: {}", e);
                    }
                }
            }
        }
    }
}
```

**Stream volume control:**
- Add `stream_volume: Arc<AtomicU32>` (stores f32 bits via `f32::to_bits()` / `f32::from_bits()`) shared between the pipeline thread and a new Tauri command
- Read in the recv loop: `let stream_volume = f32::from_bits(stream_volume_atomic.load(Relaxed));`
- Exposed via `ControlMessage::SetStreamVolume(f32)`

**Import updates:** Add `PACKET_TYPE_STREAM_AUDIO` to the import from `packet`, and `StereoOpusDecoder, STEREO_FRAME_SIZE, STEREO_FRAME_SAMPLES` from `codec`.

---

## Step 10: Add `set_stream_volume` command

**File:** `tauri-client/src-tauri/src/commands/voice.rs`

```rust
#[tauri::command]
pub async fn set_stream_volume(
    volume: f32, // 0.0 to 1.0
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_stream_volume(volume.clamp(0.0, 1.0));
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}
```

**In `VoiceEngine`:** Add `set_stream_volume()` that sends `ControlMessage::SetStreamVolume(f32)`.

**In `pipeline.rs`:** Handle `ControlMessage::SetStreamVolume(v)` by storing the value in a local `stream_volume: f32` variable.

---

## Step 11: Register new command and update `Cargo.toml`

**File:** `tauri-client/src-tauri/src/lib.rs`

Add `set_stream_volume` to the Tauri command list in `invoke_handler`.

Add audio engine teardown to the `on_window_event` shutdown handler:
```rust
if let Some(mut engine) = s.audio_stream_engine.take() {
    engine.stop();
}
```

**File:** `tauri-client/src-tauri/Cargo.toml`

Add Windows features needed for WASAPI:
```toml
"Win32_Media_Audio",
"Win32_System_Com",
"Win32_System_Threading",
```

---

## Step 12: Update `leave_voice_channel` teardown order

**File:** `tauri-client/src-tauri/src/commands/voice.rs`

In `leave_voice_channel`, add audio stream engine stop before video:

```rust
// Stop audio stream engine first
if let Some(mut engine) = s.audio_stream_engine.take() {
    engine.stop();
}
// Then video engine
if let Some(mut engine) = s.video_engine.take() {
    engine.stop();
}
// Then voice engine
if let Some(mut engine) = s.voice_engine.take() {
    engine.stop();
}
```

---

## Implementation Order

1. **Step 1** — packet type (trivial, foundational)
2. **Step 2** — stereo codec (foundational, no dependencies)
3. **Step 3** — AudioFrame + pipeline (depends on 1, 2)
4. **Step 6** — AudioStreamEngine in mod.rs (depends on 3)
5. **Step 7** — state (trivial)
6. **Step 9** — receiver in pipeline.rs (depends on 1, 2)
7. **Step 10** — volume command (depends on 9)
8. **Step 4** — Linux capture (depends on 3)
9. **Step 5** — Windows capture (depends on 3)
10. **Step 8** — wire into streaming commands (depends on 4, 5, 6, 7)
11. **Step 11** — registration + Cargo.toml
12. **Step 12** — teardown ordering

Steps 4 and 5 are platform-independent and can be developed separately.

---

## Testing

- **Linux:** Start a screen share with audio, play music in another app, verify the watcher hears the music but not Decibell's own voice playback
- **Linux window:** Stream a specific window, verify only that window's audio is captured
- **Windows:** Same tests using WASAPI process loopback
- **Volume:** Verify the stream volume slider scales audio from 0 (silent) to 1 (full)
- **Teardown:** Verify no hangs on stop_stream, leave_voice, and app close
- **No audio:** Verify streaming with `share_audio: false` works unchanged

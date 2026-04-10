# Voice Pipeline Quality Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix robotic audio, loud pops, and delay in voice chat — especially on Linux — by moving all DSP out of real-time CPAL callbacks, improving Opus encoder settings, and enabling FEC decode.

**Architecture:** CPAL audio callbacks become trivial sample copiers (try_lock + memcpy). All resampling moves to the main pipeline loop thread which has no real-time deadline. Opus encoder gets explicit 64kbps bitrate, complexity 5, and DTX. Jitter buffer depth reduced from 100ms to 60ms.

**Tech Stack:** Rust, cpal 0.15, audiopus 0.3.0-rc.0, rubato 0.16, ringbuf 0.4

**Files:**
- Modify: `tauri-client/src-tauri/src/media/codec.rs`
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs`

---

### Task 1: Opus Encoder — Bitrate, Complexity, DTX

**Files:**
- Modify: `tauri-client/src-tauri/src/media/codec.rs:21-29`

This is a self-contained change with no dependencies on later tasks.

- [ ] **Step 1: Set voice encoder bitrate to 64kbps, complexity to 5, enable DTX**

In `tauri-client/src-tauri/src/media/codec.rs`, replace the `OpusEncoder::new()` method (lines 21-29):

```rust
    pub fn new() -> Result<Self, String> {
        let mut encoder =
            Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
                .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
        // Enable in-band FEC so the decoder can recover from packet loss.
        // The ~10% bitrate overhead is negligible for voice.
        let _ = encoder.set_inband_fec(true);
        let _ = encoder.set_packet_loss_perc(10);
        Ok(OpusEncoder { encoder })
    }
```

With:

```rust
    pub fn new() -> Result<Self, String> {
        let mut encoder =
            Encoder::new(SampleRate::Hz48000, Channels::Mono, Application::Voip)
                .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
        // 64kbps — matches Discord, major clarity improvement over the ~24kbps default
        let _ = encoder.set_bitrate(audiopus::Bitrate::BitsPerSecond(64000));
        // Complexity 5 — sweet spot for real-time voice: half the CPU of 10,
        // no perceptible quality difference for speech.
        let _ = encoder.set_complexity(5);
        // DTX — encoder emits near-zero bytes during silence, saving CPU + bandwidth.
        let _ = encoder.set_dtx(true);
        // In-band FEC so the decoder can recover from packet loss.
        let _ = encoder.set_inband_fec(true);
        let _ = encoder.set_packet_loss_perc(10);
        Ok(OpusEncoder { encoder })
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors (warnings OK)

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/codec.rs
git commit -m "feat(voice): opus encoder 64kbps bitrate, complexity 5, DTX"
```

---

### Task 2: Opus Decoder — FEC Support

**Files:**
- Modify: `tauri-client/src-tauri/src/media/codec.rs:121-154`

Add a `decode_fec` method to `OpusDecoder` that passes `fec=true` to the underlying decoder. The existing `decode` method keeps `fec=false` for normal operation.

- [ ] **Step 1: Add `decode_fec` method to `OpusDecoder`**

In `tauri-client/src-tauri/src/media/codec.rs`, add the following method inside `impl OpusDecoder` after the existing `decode` method (after line 153):

```rust
    /// Decode using Forward Error Correction: pass the NEXT packet's data
    /// to recover a lost packet. The Opus encoder embeds redundant data from
    /// the previous frame, so decoding packet N+1 with fec=true reconstructs
    /// an approximation of lost packet N.
    pub fn decode_fec(
        &mut self,
        next_packet_data: &[u8],
        output: &mut [i16; FRAME_SIZE],
    ) -> Result<usize, String> {
        let packet = Packet::try_from(next_packet_data)
            .map_err(|e| format!("Invalid Opus packet for FEC: {}", e))?;
        let mut_signals = MutSignals::try_from(output.as_mut_slice())
            .map_err(|e| format!("MutSignals error: {}", e))?;
        self.decoder
            .decode(Some(packet), mut_signals, true)
            .map_err(|e| format!("Opus FEC decode error: {}", e))
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/codec.rs
git commit -m "feat(voice): add OpusDecoder::decode_fec for packet loss recovery"
```

---

### Task 3: Jitter Buffer — Reduce Depth + Add FEC-Aware Drain

**Files:**
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:84`
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:87-155`

Reduce JITTER_DEPTH from 5 to 3 (100ms → 60ms). Add a `peek_next` method so the caller can check if the next packet is available for FEC decode when the current one is missing.

- [ ] **Step 1: Reduce jitter depth**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace line 84:

```rust
const JITTER_DEPTH: usize = 5; // packets to buffer before starting playback (100ms)
```

With:

```rust
const JITTER_DEPTH: usize = 3; // packets to buffer before starting playback (60ms)
```

- [ ] **Step 2: Add `peek_next` method to JitterBuffer**

In `tauri-client/src-tauri/src/media/pipeline.rs`, add the following method inside `impl JitterBuffer` after the `drain` method (after line 154, before the closing `}`):

```rust
    /// Peek at the next packet (next_seq) without consuming it.
    /// Used for FEC: when current packet is missing, check if the next
    /// packet is available to decode with fec=true.
    fn peek_next(&self) -> Option<&Vec<u8>> {
        self.packets.get(&self.next_seq)
    }
```

Note: after `drain()` advances `next_seq`, `peek_next()` looks at the new `next_seq` — which is the packet AFTER the one just drained. This is exactly what we need: when drain returns `Some(None)` (missing packet), peek_next checks if the following packet exists for FEC recovery.

- [ ] **Step 3: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles (peek_next will show a dead_code warning until Task 6 uses it — that's fine)

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "feat(voice): reduce jitter depth 100ms→60ms, add peek_next for FEC"
```

---

### Task 4: Lighten Resampler Parameters

**Files:**
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:23-38`

Reduce sinc resampler from mastering-grade to real-time-voice-grade. Still excellent quality for speech frequencies (300Hz–8kHz), ~8x less CPU.

- [ ] **Step 1: Update `make_sinc_resampler` parameters**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace the `make_sinc_resampler` function (lines 23-38):

```rust
fn make_sinc_resampler(from_rate: u32, to_rate: u32, chunk_size: usize, channels: usize) -> SincFixedOut<f64> {
    let params = SincInterpolationParameters {
        sinc_len: 64,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 128,
        window: WindowFunction::Blackman2,
    };
    SincFixedOut::<f64>::new(
        to_rate as f64 / from_rate as f64,
        1.1, // max relative input size variation
        params,
        chunk_size,
        channels,
    ).expect("failed to create sinc resampler")
}
```

With:

```rust
fn make_sinc_resampler(from_rate: u32, to_rate: u32, chunk_size: usize, channels: usize) -> SincFixedOut<f64> {
    let params = SincInterpolationParameters {
        sinc_len: 24,
        f_cutoff: 0.925,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 32,
        window: WindowFunction::Blackman2,
    };
    SincFixedOut::<f64>::new(
        to_rate as f64 / from_rate as f64,
        1.1,
        params,
        chunk_size,
        channels,
    ).expect("failed to create sinc resampler")
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "perf(voice): lighten sinc resampler params for real-time voice"
```

---

### Task 5: Simplify CPAL Callbacks — Move Resampling to Main Loop

**Files:**
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:234-889` (all four stream builders)
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:897-1648` (main loop)

This is the core fix. Four sub-parts:
- **5A:** Rewrite `build_input_stream` — remove resampling, return sample rate
- **5B:** Rewrite `build_output_stream` — remove resampling, keep passthrough-only
- **5C:** Rewrite `build_voice_output_stream` and `build_stream_output_stream` — same simplification
- **5D:** Update `run_audio_pipeline` main loop — add resamplers, update callers, update capture + playback sections

---

#### Step 5A: Rewrite `build_input_stream`

- [ ] **Step 1: Replace `build_input_stream` function**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace the entire `build_input_stream` function (lines 234–368) — from the comment `// ── Input stream builder` through the closing `}` of the function:

```rust
// ── Input stream builder ─────────────────────────────────────────────────────

/// Build a CPAL input (capture) stream that pushes mono i16 samples at the
/// device's native sample rate into `capture_prod`.
/// Returns (stream, device_sample_rate) or None if no usable device is found.
///
/// The callback does NO resampling — just downmixes to mono and converts to i16.
/// Resampling from device rate → 48kHz happens in the main pipeline loop.
fn build_input_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    capture_prod: Arc<std::sync::Mutex<HeapProd<i16>>>,
) -> Option<(cpal::Stream, u32)> {
    let input_device = match device_name {
        Some(name) => {
            let found = host.input_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Input device '{}' not found, falling back to default", name);
                    get_default_device(host, true)?
                }
            }
        }
        None => get_default_device(host, true)?,
    };

    let (input_cfg, input_channels) = match input_device.default_input_config() {
        Ok(default_cfg) => {
            let rate = default_cfg.sample_rate();
            let channels = default_cfg.channels();
            eprintln!(
                "[pipeline] Input device: {}ch @ {}Hz (sample format: {:?})",
                channels, rate.0, default_cfg.sample_format()
            );
            (cpal::StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            }, channels)
        }
        Err(_) => {
            (cpal::StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Default,
            }, 1u16)
        }
    };

    let in_ch = input_channels;
    let input_sample_rate = input_cfg.sample_rate.0;
    let cap_prod = capture_prod;

    // The callback only does: downmix to mono + f32→i16. No resampling, no allocations.
    // Uses try_lock to never block the real-time audio thread.
    match input_device.build_input_stream(
        &input_cfg,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let Ok(mut prod) = cap_prod.try_lock() else { return };
            if in_ch == 1 {
                for &s in data {
                    let _ = prod.try_push((s * 32767.0).clamp(-32768.0, 32767.0) as i16);
                }
            } else {
                for frame in data.chunks_exact(in_ch as usize) {
                    let sum: f32 = frame.iter().sum();
                    let mono = sum / in_ch as f32;
                    let _ = prod.try_push((mono * 32767.0).clamp(-32768.0, 32767.0) as i16);
                }
            }
        },
        |e| {
            eprintln!("[pipeline] capture stream error: {}", e);
        },
        None,
    ) {
        Ok(stream) => {
            if let Err(e) = stream.play() {
                eprintln!("[pipeline] failed to start capture stream: {}", e);
                None
            } else {
                eprintln!("[pipeline] Capture stream started: mono @ {}Hz (no callback resampling)", input_sample_rate);
                Some((stream, input_sample_rate))
            }
        }
        Err(e) => {
            eprintln!("[pipeline] build_input_stream failed: {}", e);
            None
        }
    }
}
```

Key changes from old version:
- Removed `output_sample_rate` parameter
- Removed all resampler code (SincFixedOut, Arc<Mutex<>>, accumulators)
- Callback uses `try_lock` (was `lock`)
- Returns `(cpal::Stream, u32)` (was `Option<cpal::Stream>`)

---

#### Step 5B: Rewrite `build_output_stream`

- [ ] **Step 2: Replace `build_output_stream` function**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace the entire `build_output_stream` function (lines 370–604) — from the comment `// ── Output stream builder` through the closing `}`:

```rust
// ── Output stream builder ────────────────────────────────────────────────────

/// Build a CPAL output (playback) stream that mixes voice + stream audio from
/// their respective ring buffer consumers. The ring buffers carry i16 samples
/// at the output device's native rate — all resampling happens in the main loop.
fn build_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    voice_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    _render_ref_prod: Arc<std::sync::Mutex<HeapProd<f32>>>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = match device_name {
        Some(name) => {
            let found = host.output_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Output device '{}' not found, falling back to default", name);
                    get_default_device(host, false)?
                }
            }
        }
        None => get_default_device(host, false)?,
    };

    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Output device: {}ch @ {}Hz (sample format: {:?})",
                cfg.channels, cfg.sample_rate.0, default_cfg.sample_format()
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            eprintln!("[pipeline] default_output_config failed ({}), trying 48kHz stereo", e);
            let cfg = cpal::StreamConfig {
                channels: 2,
                sample_rate: cpal::SampleRate(SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Default,
            };
            (cfg, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;

    let voice_cons_out = voice_cons;
    let stream_cons_out = stream_cons;
    let pb_stream_stereo = stream_stereo;
    let out_ch = output_channels;

    // No resampling in the callback — ring buffers already carry samples at device rate.
    // Just read i16, convert to f32, mix voice+stream, write to device.
    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let Ok(mut voice_guard) = voice_cons_out.try_lock() else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };
            let Ok(mut stream_guard) = stream_cons_out.try_lock() else {
                drop(voice_guard);
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };

            if out_ch == 1 {
                for sample in data.iter_mut() {
                    let v = voice_guard.try_pop().unwrap_or(0) as i32;
                    let s = stream_guard.try_pop().unwrap_or(0) as i32;
                    let mixed = (v + s).clamp(-32768, 32767);
                    *sample = mixed as f32 / 32768.0;
                }
            } else {
                for frame in data.chunks_exact_mut(out_ch as usize) {
                    let v = voice_guard.try_pop().unwrap_or(0) as i32;
                    if pb_stream_stereo.load(std::sync::atomic::Ordering::Relaxed) && out_ch >= 2 {
                        let sl = stream_guard.try_pop().unwrap_or(0) as i32;
                        let sr = stream_guard.try_pop().unwrap_or(0) as i32;
                        let left = (v + sl).clamp(-32768, 32767) as f32 / 32768.0;
                        let right = (v + sr).clamp(-32768, 32767) as f32 / 32768.0;
                        frame[0] = left;
                        frame[1] = right;
                        for ch in &mut frame[2..] {
                            *ch = left;
                        }
                    } else {
                        let s = stream_guard.try_pop().unwrap_or(0) as i32;
                        let mixed = (v + s).clamp(-32768, 32767) as f32 / 32768.0;
                        for ch in frame.iter_mut() {
                            *ch = mixed;
                        }
                    }
                }
            }
        },
        |e| {
            eprintln!("[pipeline] playback stream error: {}", e);
        },
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!(
                "Failed to build output stream: {}", e
            )));
            return None;
        }
    };
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!(
            "Failed to start output stream: {}", e
        )));
        return None;
    }

    Some((stream, output_sample_rate, output_channels))
}
```

Key changes: removed all resampler state (pb_voice_resampler, pb_stream_resampler, accumulators, VecDeques, passthrough branching). The old passthrough branch is now the only code path.

---

#### Step 5C: Rewrite `build_voice_output_stream` and `build_stream_output_stream`

- [ ] **Step 3: Replace `build_voice_output_stream` function**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace the entire `build_voice_output_stream` function (from `// ── Voice-only output stream builder` through its closing `}`) with:

```rust
// ── Voice-only output stream builder ─────────────────────────────────────────

/// Build a CPAL output stream that plays only voice audio (no stream mixing).
/// Used when stream audio is routed to a separate device.
/// Ring buffer carries i16 at device native rate — no callback resampling.
fn build_voice_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    voice_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    _render_ref_prod: Arc<std::sync::Mutex<HeapProd<f32>>>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = match device_name {
        Some(name) => {
            let found = host.output_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Voice output device '{}' not found, falling back to default", name);
                    get_default_device(host, false)?
                }
            }
        }
        None => get_default_device(host, false)?,
    };

    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Voice output device: {}ch @ {}Hz",
                cfg.channels, cfg.sample_rate.0
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            eprintln!("[pipeline] voice output default_output_config failed ({}), trying 48kHz stereo", e);
            (cpal::StreamConfig { channels: 2, sample_rate: cpal::SampleRate(SAMPLE_RATE), buffer_size: cpal::BufferSize::Default }, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;
    let out_ch = output_channels;
    let voice_cons_out = voice_cons;

    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let Ok(mut guard) = voice_cons_out.try_lock() else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };
            for frame in data.chunks_exact_mut(out_ch as usize) {
                let v = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                for ch in frame.iter_mut() { *ch = v; }
            }
        },
        |e| eprintln!("[pipeline] voice output stream error: {}", e),
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build voice output stream: {}", e)));
            return None;
        }
    };
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!("Failed to start voice output stream: {}", e)));
        return None;
    }

    Some((stream, output_sample_rate, output_channels))
}
```

- [ ] **Step 4: Replace `build_stream_output_stream` function**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace the entire `build_stream_output_stream` function (from `// ── Stream-only output stream builder` through its closing `}`) with:

```rust
// ── Stream-only output stream builder ────────────────────────────────────────

/// Build a CPAL output stream that plays only stream audio (with stereo support).
/// Used when stream audio is routed to a separate device.
/// Ring buffer carries i16 at device native rate — no callback resampling.
fn build_stream_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = match device_name {
        Some(name) => {
            let found = host.output_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Stream output device '{}' not found, falling back to default", name);
                    get_default_device(host, false)?
                }
            }
        }
        None => get_default_device(host, false)?,
    };

    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Stream output device: {}ch @ {}Hz",
                cfg.channels, cfg.sample_rate.0
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            eprintln!("[pipeline] stream output default_output_config failed ({}), trying 48kHz stereo", e);
            (cpal::StreamConfig { channels: 2, sample_rate: cpal::SampleRate(SAMPLE_RATE), buffer_size: cpal::BufferSize::Default }, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;
    let out_ch = output_channels;
    let stream_cons_out = stream_cons;
    let pb_stereo = stream_stereo;

    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let Ok(mut guard) = stream_cons_out.try_lock() else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };
            let is_stereo = pb_stereo.load(std::sync::atomic::Ordering::Relaxed);

            if out_ch == 1 {
                for sample in data.iter_mut() {
                    let s = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                    if is_stereo { let _ = guard.try_pop(); } // discard R for mono
                    *sample = s;
                }
            } else {
                for frame in data.chunks_exact_mut(out_ch as usize) {
                    if is_stereo && out_ch >= 2 {
                        let sl = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                        let sr = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                        frame[0] = sl;
                        frame[1] = sr;
                        for ch in &mut frame[2..] { *ch = sl; }
                    } else {
                        let s = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                        for ch in frame.iter_mut() { *ch = s; }
                    }
                }
            }
        },
        |e| eprintln!("[pipeline] stream output error: {}", e),
        None,
    ) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build stream output: {}", e)));
            return None;
        }
    };
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!("Failed to start stream output: {}", e)));
        return None;
    }

    Some((stream, output_sample_rate, output_channels))
}
```

- [ ] **Step 5: Verify it compiles (callbacks will have unused import warnings — fine for now)**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -10`
Expected: compiles. There may be warnings about unused variables — those get fixed in Step 5D when we update the callers.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "refactor(voice): remove resampling from all CPAL callbacks

Callbacks now only copy samples (try_lock + i16→f32). All DSP moves
to the main pipeline thread in the next commit."
```

---

#### Step 5D: Update Main Loop — Add Resamplers, Update Callers

This is the largest sub-task. The main loop in `run_audio_pipeline` needs:
1. Updated `build_input_stream` call (new signature, returns sample rate)
2. Capture resampler (input_device_rate → 48kHz) for the encode path
3. Playback resamplers (48kHz → output_device_rate) for voice and stream decode paths
4. Updated hot-swap handlers to rebuild resamplers
5. FEC-aware jitter drain

- [ ] **Step 7: Update initial stream building and add resampler state**

Find the section in `run_audio_pipeline` that builds the output stream and input stream (around lines 949–984). Replace from `// ── Build output stream first` through the end of the input stream section (ending at `};` after `None`):

Replace:

```rust
    // ── Build output stream first (we need its sample rate for input matching) ─
    let stream_stereo = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (mut output_stream, mut output_sample_rate) = match build_output_stream(
        &host,
        None, // system default on startup
        Arc::clone(&voice_cons),
        Arc::clone(&stream_cons),
        Arc::clone(&stream_stereo),
        Arc::clone(&render_ref_prod),
        &event_tx,
    ) {
        Some((stream, rate, _ch)) => (Some(stream), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No audio output device found".to_string(),
            ));
            return;
        }
    };

    // ── Build input (capture) stream ──────────────────────────────────────────
    let mut input_stream_opt: Option<cpal::Stream> = match build_input_stream(
        &host,
        None, // system default on startup
        output_sample_rate,
        Arc::clone(&capture_prod),
    ) {
        Some(s) => Some(s),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No microphone found — running in listen-only mode".to_string(),
            ));
            None
        }
    };
```

With:

```rust
    // ── Build output stream first (we need its sample rate for playback resampler) ─
    let stream_stereo = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (mut output_stream, mut output_sample_rate) = match build_output_stream(
        &host,
        None, // system default on startup
        Arc::clone(&voice_cons),
        Arc::clone(&stream_cons),
        Arc::clone(&stream_stereo),
        Arc::clone(&render_ref_prod),
        &event_tx,
    ) {
        Some((stream, rate, _ch)) => (Some(stream), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No audio output device found".to_string(),
            ));
            return;
        }
    };

    // ── Build input (capture) stream ──────────────────────────────────────────
    let (mut input_stream_opt, mut input_sample_rate): (Option<cpal::Stream>, u32) = match build_input_stream(
        &host,
        None, // system default on startup
        Arc::clone(&capture_prod),
    ) {
        Some((s, rate)) => (Some(s), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No microphone found — running in listen-only mode".to_string(),
            ));
            (None, SAMPLE_RATE)
        }
    };

    // ── Main-loop resamplers (all DSP off the audio callback threads) ─────────
    // Capture: input_device_rate → 48kHz (for Opus encoding)
    let capture_passthrough = input_sample_rate == SAMPLE_RATE;
    let mut capture_resampler: Option<SincFixedOut<f64>> = if capture_passthrough {
        None
    } else {
        eprintln!("[pipeline] Capture resampler: {}Hz → {}Hz", input_sample_rate, SAMPLE_RATE);
        Some(make_sinc_resampler(input_sample_rate, SAMPLE_RATE, 480, 1))
    };
    let mut capture_accum: Vec<f64> = Vec::new();

    // Playback voice: 48kHz → output_device_rate (mono)
    let playback_passthrough = output_sample_rate == SAMPLE_RATE;
    let mut playback_voice_resampler: Option<SincFixedOut<f64>> = if playback_passthrough {
        None
    } else {
        eprintln!("[pipeline] Playback voice resampler: {}Hz → {}Hz", SAMPLE_RATE, output_sample_rate);
        Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 1))
    };
    let mut playback_voice_accum: Vec<f64> = Vec::new();

    // Playback stream: 48kHz → output_device_rate (stereo)
    let mut playback_stream_resampler: Option<SincFixedOut<f64>> = if playback_passthrough {
        None
    } else {
        eprintln!("[pipeline] Playback stream resampler: {}Hz → {}Hz (stereo)", SAMPLE_RATE, output_sample_rate);
        Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2))
    };
    let mut playback_stream_accum_l: Vec<f64> = Vec::new();
    let mut playback_stream_accum_r: Vec<f64> = Vec::new();
```

- [ ] **Step 8: Update capture & encode section to use main-loop resampler**

Find the capture section in the main loop (around line 1242, starting `// 2. Capture & encode → send UDP`). Replace the block from `// 2. Capture & encode → send UDP` up to and including `}` (the closing brace of the outer block before `// 3. Send ping every 3s`):

Replace:

```rust
        // 2. Capture & encode → send UDP ──────────────────────────────────────
        {
            let frame_opt: Option<[i16; FRAME_SIZE]> = {
                let mut cons = capture_cons.lock().unwrap();
                if cons.occupied_len() >= FRAME_SIZE {
                    let mut frame = [0i16; FRAME_SIZE];
                    for s in frame.iter_mut() {
                        *s = cons.try_pop().unwrap();
                    }
                    Some(frame)
                } else {
                    None
                }
            };
```

With:

```rust
        // 2. Capture & encode → send UDP ──────────────────────────────────────
        //
        // The capture ring buffer carries i16 at the input device's native rate.
        // We drain available samples, resample to 48kHz if needed, accumulate
        // into a PCM frame, and Opus-encode when we have a full 20ms frame.
        {
            // Drain all available capture samples and resample to 48kHz
            let frame_opt: Option<[i16; FRAME_SIZE]> = {
                let mut cons = capture_cons.lock().unwrap();
                let avail = cons.occupied_len();
                if avail > 0 {
                    if capture_passthrough {
                        // Input device is 48kHz — read directly
                        if avail >= FRAME_SIZE {
                            let mut frame = [0i16; FRAME_SIZE];
                            for s in frame.iter_mut() {
                                *s = cons.try_pop().unwrap();
                            }
                            Some(frame)
                        } else {
                            None
                        }
                    } else {
                        // Drain raw samples into the resampler accumulator
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_accum.push(s as f64 / 32768.0);
                            }
                        }
                        drop(cons); // release lock before resampling

                        // Resample accumulated samples: input_rate → 48kHz
                        if let Some(ref mut resampler) = capture_resampler {
                            let mut needed = resampler.input_frames_next();
                            while capture_accum.len() >= needed {
                                let chunk: Vec<f64> = capture_accum.drain(..needed).collect();
                                if let Ok(out) = resampler.process(&[&chunk], None) {
                                    for &s in &out[0] {
                                        let i16_val = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                                        // Push into a local buffer to assemble a FRAME_SIZE frame
                                        // We re-use capture_cons's lock-free side by pushing back.
                                        // But that would feed back — instead use a separate local vec.
                                        // We'll handle this below.
                                        let _ = capture_prod.lock().map(|mut p| { let _ = p.try_push(i16_val); });
                                    }
                                }
                                needed = resampler.input_frames_next();
                            }
                        }

                        // Now try to read a full frame of resampled 48kHz data
                        let mut cons = capture_cons.lock().unwrap();
                        if cons.occupied_len() >= FRAME_SIZE {
                            let mut frame = [0i16; FRAME_SIZE];
                            for s in frame.iter_mut() {
                                *s = cons.try_pop().unwrap();
                            }
                            Some(frame)
                        } else {
                            None
                        }
                    }
                } else {
                    None
                }
            };
```

Wait — this approach has a problem. I'm pushing resampled data back into the capture ring buffer, but the producer is shared with the CPAL callback. That creates a dual-producer situation on a SPSC ring buffer.

Let me rethink. The cleaner approach: use a **separate local accumulator** for the resampled 48kHz PCM. The capture ring buffer stays as the raw-from-device buffer. The main loop drains it, resamples, and accumulates into a local `Vec<i16>` that persists across iterations.

Let me rewrite this section properly.

- [ ] **Step 8 (revised): Replace the capture section with a proper two-buffer approach**

We need a persistent `capture_48k_buf: Vec<i16>` that accumulates resampled samples across loop iterations. Add it with the other local state variables (near line 999, after `let mut local_speaking`):

First, find this line (around line 999-1001):

```rust
    let mut local_speaking = SpeakingDetector::new();
    let mut input_level_counter: u32 = 0; // throttle InputLevel events (~every 3 frames = 60ms)
    let mut remote_peers: HashMap<String, RemotePeer> = HashMap::new();
```

And add after it:

```rust
    // Accumulator for resampled 48kHz capture PCM — persists across loop iterations
    let mut capture_48k_buf: Vec<i16> = Vec::with_capacity(FRAME_SIZE * 4);
```

Then replace the capture section. Find the block starting at `// 2. Capture & encode → send UDP` and ending just before `// 3. Send ping every 3s`. Replace the entire section (from `// 2.` through the matching closing `}`):

```rust
        // 2. Capture & encode → send UDP ──────────────────────────────────────
        //
        // The capture ring buffer carries i16 at the input device's native rate.
        // We drain available samples, resample to 48kHz if needed, and accumulate
        // in capture_48k_buf. When we have a full 960-sample frame, encode + send.
        {
            // Drain all available samples from the capture ring buffer
            {
                let mut cons = capture_cons.lock().unwrap();
                let avail = cons.occupied_len();
                if avail > 0 {
                    if capture_passthrough {
                        // Input device is 48kHz — copy directly to the 48k buffer
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_48k_buf.push(s);
                            }
                        }
                    } else {
                        // Drain raw samples into the resampler accumulator
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_accum.push(s as f64 / 32768.0);
                            }
                        }
                    }
                }
            } // release capture_cons lock

            // Resample accumulated raw samples: input_rate → 48kHz
            if !capture_passthrough {
                if let Some(ref mut resampler) = capture_resampler {
                    let mut needed = resampler.input_frames_next();
                    while capture_accum.len() >= needed {
                        let chunk: Vec<f64> = capture_accum.drain(..needed).collect();
                        if let Ok(out) = resampler.process(&[&chunk], None) {
                            for &s in &out[0] {
                                capture_48k_buf.push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                            }
                        }
                        needed = resampler.input_frames_next();
                    }
                }
            }

            // Try to assemble a full Opus frame (960 samples at 48kHz = 20ms)
            let frame_opt: Option<[i16; FRAME_SIZE]> = if capture_48k_buf.len() >= FRAME_SIZE {
                let mut frame = [0i16; FRAME_SIZE];
                frame.copy_from_slice(&capture_48k_buf[..FRAME_SIZE]);
                capture_48k_buf.drain(..FRAME_SIZE);
                Some(frame)
            } else {
                None
            };
```

Note: the rest of the capture section (from `if let Some(mut frame) = frame_opt {` through the end of the block) stays UNCHANGED — the RMS computation, voice threshold, AEC processing, Opus encode, and UDP send all remain exactly as they are. Only the frame acquisition code above is replaced.

- [ ] **Step 9: Update playback mix section to resample before pushing to ring buffer**

Find the voice mixing section (around line 1569, `// 4c. Mix decoded voice`). Replace the entire block from `// 4c.` through `}` (before `// 5. Clean up stale remote peers`):

```rust
        // 4c. Mix decoded voice from all peers → resample → push to playback buffer
        // Each peer's jitter drain accumulated f32 samples in decoded_voice.
        // Sum them sample-by-sample, then resample from 48kHz to the output device
        // rate before pushing to the voice ring buffer.
        {
            let max_samples = remote_peers.values().map(|p| p.decoded_voice.len()).max().unwrap_or(0);
            if max_samples > 0 {
                mix_buffer.clear();
                mix_buffer.resize(max_samples, 0.0);
                for peer in remote_peers.values() {
                    for (i, &s) in peer.decoded_voice.iter().enumerate() {
                        mix_buffer[i] += s;
                    }
                }

                // Feed mixed voice to AEC render reference (what the speaker actually plays)
                if aec_enabled {
                    if let Ok(mut rr_prod) = render_ref_prod.lock() {
                        for &s in &mix_buffer {
                            let _ = rr_prod.try_push(s);
                        }
                    }
                }

                // Resample 48kHz → output device rate, then push to playback ring buffer
                if playback_passthrough {
                    if let Ok(mut prod) = voice_prod.lock() {
                        for &s in &mix_buffer {
                            let _ = prod.try_push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                        }
                    }
                } else if let Some(ref mut resampler) = playback_voice_resampler {
                    for &s in &mix_buffer {
                        playback_voice_accum.push(s as f64);
                    }
                    let mut needed = resampler.input_frames_next();
                    while playback_voice_accum.len() >= needed {
                        let chunk: Vec<f64> = playback_voice_accum.drain(..needed).collect();
                        if let Ok(out) = resampler.process(&[&chunk], None) {
                            if let Ok(mut prod) = voice_prod.lock() {
                                for &s in &out[0] {
                                    let _ = prod.try_push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                }
                            }
                        }
                        needed = resampler.input_frames_next();
                    }
                }

                // Clear per-peer buffers
                for peer in remote_peers.values_mut() {
                    peer.decoded_voice.clear();
                }
            }
        }
```

- [ ] **Step 10: Update stream audio decode section to resample before pushing**

Find the stream audio jitter buffer section (around line 1519, `// ── Stream audio jitter buffer ──`). Replace the block starting from `// ── Stream audio jitter buffer ──` through the end of the `if let Some(ref mut decoder)` block (ending just before the closing `}` of the per-peer for loop):

```rust
            // ── Stream audio jitter buffer ──
            if let Some(ref mut decoder) = peer.stream_audio_decoder {
                while drain_now.duration_since(peer.stream_drain_time) >= frame_dur {
                    peer.stream_drain_time += frame_dur;
                    let opus_opt = match peer.stream_jitter.drain() {
                        Some(v) => v,
                        None => break,
                    };
                    let mut pcm = [0i16; STEREO_FRAME_SAMPLES];
                    let decode_ok = match &opus_opt {
                        Some(data) => decoder.decode(data, &mut pcm).is_ok(),
                        None => decoder.decode(&[], &mut pcm).is_ok(), // PLC
                    };
                    if decode_ok {
                        if playback_passthrough {
                            // Output device is 48kHz — push directly
                            if let Ok(mut prod) = stream_prod.lock() {
                                for i in 0..STEREO_FRAME_SIZE {
                                    let l = pcm[i * 2] as i32;
                                    let r = pcm[i * 2 + 1] as i32;
                                    if stream_stereo.load(std::sync::atomic::Ordering::Relaxed) {
                                        let sl = ((l as f32) * stream_volume) as i32;
                                        let sr = ((r as f32) * stream_volume) as i32;
                                        let _ = prod.try_push(sl.clamp(-32768, 32767) as i16);
                                        let _ = prod.try_push(sr.clamp(-32768, 32767) as i16);
                                    } else {
                                        let mono = (l + r) / 2;
                                        let scaled = ((mono as f32) * stream_volume) as i32;
                                        let _ = prod.try_push(scaled.clamp(-32768, 32767) as i16);
                                    }
                                }
                            }
                        } else if let Some(ref mut resampler) = playback_stream_resampler {
                            // Resample stereo 48kHz → output device rate, then push
                            for i in 0..STEREO_FRAME_SIZE {
                                let l = pcm[i * 2] as f32 * stream_volume / 32768.0;
                                let r = pcm[i * 2 + 1] as f32 * stream_volume / 32768.0;
                                playback_stream_accum_l.push(l as f64);
                                playback_stream_accum_r.push(r as f64);
                            }
                            let mut needed = resampler.input_frames_next();
                            while playback_stream_accum_l.len() >= needed && playback_stream_accum_r.len() >= needed {
                                let cl: Vec<f64> = playback_stream_accum_l.drain(..needed).collect();
                                let cr: Vec<f64> = playback_stream_accum_r.drain(..needed).collect();
                                if let Ok(out) = resampler.process(&[&cl, &cr], None) {
                                    if let Ok(mut prod) = stream_prod.lock() {
                                        let is_stereo = stream_stereo.load(std::sync::atomic::Ordering::Relaxed);
                                        let len = out[0].len().min(out[1].len());
                                        for i in 0..len {
                                            if is_stereo {
                                                let _ = prod.try_push((out[0][i] * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                                let _ = prod.try_push((out[1][i] * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                            } else {
                                                let mono = (out[0][i] + out[1][i]) / 2.0;
                                                let _ = prod.try_push((mono * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                            }
                                        }
                                    }
                                }
                                needed = resampler.input_frames_next();
                            }
                        }
                        // Feed stream audio to AEC render reference (mono, with volume applied)
                        if aec_enabled {
                            if let Ok(mut rr_prod) = render_ref_prod.lock() {
                                for i in 0..STEREO_FRAME_SIZE {
                                    let l = pcm[i * 2] as f32;
                                    let r = pcm[i * 2 + 1] as f32;
                                    let mono = ((l + r) / 2.0) * stream_volume / 32768.0;
                                    let _ = rr_prod.try_push(mono);
                                }
                            }
                        }
                    }
                }
            }
```

- [ ] **Step 11: Update hot-swap handlers for new `build_input_stream` signature and resamplers**

Find the `SetInputDevice` handler (around line 1113). Replace the entire handler body:

```rust
                Ok(ControlMessage::SetInputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping input device to: {:?}", name);
                    input_stream_opt = None; // drop old stream
                    { let mut g = capture_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    capture_48k_buf.clear();
                    capture_accum.clear();
                    match build_input_stream(&host, name.as_deref(), Arc::clone(&capture_prod)) {
                        Some((stream, rate)) => {
                            input_stream_opt = Some(stream);
                            input_sample_rate = rate;
                            if rate == SAMPLE_RATE {
                                capture_resampler = None;
                                eprintln!("[pipeline] Input device {}Hz — passthrough", rate);
                            } else {
                                capture_resampler = Some(make_sinc_resampler(rate, SAMPLE_RATE, 480, 1));
                                eprintln!("[pipeline] Input device {}Hz — resampler to {}Hz", rate, SAMPLE_RATE);
                            }
                        }
                        None => {
                            eprintln!("[pipeline] Warning: no input device after hot-swap");
                        }
                    }
                    // Reset voice processor state on device change
                    if voice_processor.is_some() {
                        voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                        render_ref_accum.clear();
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
```

Find the `SetOutputDevice` handler (around line 1133). Replace the entire handler body:

```rust
                Ok(ControlMessage::SetOutputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping output device to: {:?}", name);
                    output_stream = None; // drop old stream
                    { let mut g = voice_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    playback_voice_accum.clear();
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
                    if separate_stream_enabled {
                        match build_voice_output_stream(&host, name.as_deref(), Arc::clone(&voice_cons), Arc::clone(&render_ref_prod), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => eprintln!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    } else {
                        match build_output_stream(&host, name.as_deref(), Arc::clone(&voice_cons), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => eprintln!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    }
                    // Rebuild playback resamplers for new output rate
                    if output_sample_rate == SAMPLE_RATE {
                        playback_voice_resampler = None;
                        playback_stream_resampler = None;
                    } else {
                        playback_voice_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 1));
                        playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2));
                    }
                }
```

Find the `SetSeparateStreamOutput` handler (around line 1158). After the existing ring buffer drain lines and before the `if enabled {` branch, add resampler drain:

```rust
                    playback_voice_accum.clear();
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
```

And after each branch that sets `output_sample_rate`, add resampler rebuild:

After the `if enabled {` branch's voice output build (after `output_sample_rate = rate;`), and also after the `else` branch's mixed output build (after `output_sample_rate = rate;`), add at the end of the handler (before the closing `}`):

```rust
                    // Rebuild playback resamplers for (potentially new) output rate
                    if output_sample_rate == SAMPLE_RATE {
                        playback_voice_resampler = None;
                        playback_stream_resampler = None;
                    } else {
                        playback_voice_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 1));
                        playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2));
                    }
```

- [ ] **Step 12: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -20`
Expected: compiles with no errors. If there are unused variable warnings for `capture_passthrough`/`playback_passthrough` being shadowed, that's OK — they're used as `let` bindings that get updated on hot-swap.

Note: `capture_passthrough` is a `let` binding (not `let mut`) in the initial setup. Since input device hot-swap replaces `capture_resampler` but doesn't update the `capture_passthrough` boolean, we should make the capture section check `capture_resampler.is_none()` instead of the boolean. Update the capture section to use:

```rust
if capture_resampler.is_none() {
```

instead of:

```rust
if capture_passthrough {
```

And similarly for playback, use `playback_voice_resampler.is_none()` instead of `playback_passthrough`. This way hot-swap automatically works because the resampler Option is the single source of truth.

- [ ] **Step 13: Commit**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "feat(voice): move all resampling from CPAL callbacks to main loop

- Capture callback: trivial f32→i16 downmix + try_lock (no resampling)
- Output callbacks: trivial i16→f32 read + mix (no resampling)
- Main loop: resample capture (device→48k) and playback (48k→device)
- Persistent resamplers rebuilt on device hot-swap
- Ring buffers carry i16 at device native rate"
```

---

### Task 6: FEC-Aware Voice Jitter Drain

**Files:**
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs` (voice jitter drain section, ~line 1489)

When a voice packet is missing and the next packet is available in the jitter buffer, use Opus FEC decode (from Task 2) to recover the lost packet instead of basic PLC.

- [ ] **Step 1: Update the voice jitter drain to use FEC**

Find the voice jitter drain section (starts with `// ── Voice jitter buffer ──`). Replace the decode logic:

```rust
            // ── Voice jitter buffer ──
            while drain_now.duration_since(peer.voice_drain_time) >= frame_dur {
                peer.voice_drain_time += frame_dur;
                let opus_opt = match peer.voice_jitter.drain() {
                    Some(v) => v,
                    None => break, // not ready or empty
                };
                let mut pcm = [0i16; FRAME_SIZE];
                let decode_ok = match &opus_opt {
                    Some(data) => peer.decoder.decode(data, &mut pcm).is_ok(),
                    None => peer.decoder.decode(&[], &mut pcm).is_ok(), // PLC
                };
```

With:

```rust
            // ── Voice jitter buffer ──
            while drain_now.duration_since(peer.voice_drain_time) >= frame_dur {
                peer.voice_drain_time += frame_dur;
                let opus_opt = match peer.voice_jitter.drain() {
                    Some(v) => v,
                    None => break, // not ready or empty
                };
                let mut pcm = [0i16; FRAME_SIZE];
                let decode_ok = match &opus_opt {
                    Some(data) => peer.decoder.decode(data, &mut pcm).is_ok(),
                    None => {
                        // Packet lost — try FEC recovery using the next packet
                        if let Some(next_data) = peer.voice_jitter.peek_next() {
                            peer.decoder.decode_fec(next_data, &mut pcm).is_ok()
                        } else {
                            // No next packet available — fall back to basic PLC
                            peer.decoder.decode(&[], &mut pcm).is_ok()
                        }
                    }
                };
```

The rest of the block (RMS computation, speaking detection, gain, decoded_voice push) remains unchanged.

- [ ] **Step 2: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors. The `peek_next` dead_code warning from Task 3 should be gone now.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "feat(voice): FEC-aware jitter drain for better packet loss recovery"
```

---

### Task 7: Final Verification — Build + Test

- [ ] **Step 1: Full cargo build (release mode)**

Run: `cd tauri-client/src-tauri && cargo build --release 2>&1 | tail -20`
Expected: compiles with no errors.

- [ ] **Step 2: Check for unused import warnings and clean up**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | grep "warning:"`

If there are unused imports (e.g., the `rubato` imports that were only used in callbacks), remove them. The `SincFixedOut`, `SincInterpolationParameters`, etc. are still used in the main loop, so they should stay. If `Resampler` trait import is flagged as unused, remove it.

- [ ] **Step 3: Commit any cleanup**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "chore: clean up unused imports after voice pipeline refactor"
```

- [ ] **Step 4: Verify the full Tauri app builds**

Run: `cd tauri-client && npm run tauri build 2>&1 | tail -20`

If this is a Linux dev machine and full bundle build isn't needed, `cargo build` from step 1 is sufficient — the npm tauri build just wraps it with frontend bundling.

# Voice Pipeline Quality Fix — Design Spec

**Date:** 2026-04-10
**Problem:** Voice chat has robotic audio, loud pops, and delay. Worst on Linux (PipeWire/CachyOS), less severe on Windows.

## Root Causes

1. **Sinc resampling inside real-time CPAL audio callbacks** — f64 math, Vec allocations, mutex locks in callbacks with strict RT deadlines. PipeWire drops the buffer when deadline is missed → gaps → robotic sound, pops.
2. **Blocking mutex (`lock()`) in capture callback** — priority inversion on RT audio thread.
3. **Overkill resampler params** — sinc_len=64, oversampling_factor=128 is mastering-grade, wastes CPU in RT context.
4. **Jitter buffer too deep** — JITTER_DEPTH=5 (100ms). Discord uses 40–60ms.
5. **No explicit voice bitrate** — Opus defaults to ~24kbps. Discord uses 64kbps.
6. **Opus FEC enabled but never used on decode** — paying 10% bitrate overhead for nothing.
7. **Opus complexity at max (10)** — double the encode CPU vs complexity=5 with no perceptible quality difference for voice.

## Architecture Change

### Before (broken)
```
Capture callback → [mutex lock, resample device→48k, f64 math, Vec alloc] → ring buffer (i16@48k)
Ring buffer (i16@48k) → output callback → [mutex lock, resample 48k→device, f64 math, Vec alloc] → speakers
```

### After (fix)
```
Capture callback → [try_lock, f32→i16 downmix only] → ring buffer (i16@device rate)
                                                            ↓
Main loop thread → [resample device→48k] → Opus encode → UDP
UDP recv → Opus decode → [resample 48k→device] → ring buffer (i16@device rate)
                                                            ↓
Output callback → [try_lock, i16→f32 only] → speakers
```

CPAL callbacks become trivial sample copiers. All DSP runs on the main pipeline thread (no RT deadline).

## Detailed Changes

### 1. pipeline.rs — `build_input_stream`
- Remove all resampler code from callback
- Callback: downmix to mono f32→i16, push via `try_lock` (never block)
- Return `(cpal::Stream, u32)` — stream + input sample rate for main loop resampler
- Remove `output_sample_rate` parameter (no longer matching rates in callback)

### 2. pipeline.rs — `build_output_stream`
- Remove all resampler code (accumulators, VecDeque, sinc resampler state)
- Ring buffer now carries i16 at output device's native rate
- Callback: read i16, convert to f32, write to device (the existing "passthrough" branch)
- Same simplification for `build_voice_output_stream` and `build_stream_output_stream`

### 3. pipeline.rs — Main loop capture section
- Read i16 from capture ring buffer at input_device_rate
- Resample to 48kHz using persistent `SincFixedOut` resampler (created once, reused)
- Convert to i16, Opus encode, send UDP
- If input rate == 48kHz, skip resampling (passthrough)

### 4. pipeline.rs — Main loop playback section
- After Opus decode + peer mixing (f32 mono @ 48kHz):
  - Resample from 48kHz → output_device_rate using persistent resampler
  - Convert to i16, push to voice ring buffer
- Same for stream audio (stereo)
- If output rate == 48kHz, skip resampling (passthrough)

### 5. pipeline.rs — Resampler params
- `sinc_len: 64 → 24`
- `oversampling_factor: 128 → 32`
- `f_cutoff: 0.95 → 0.925`
- Still excellent quality for voice (300Hz–8kHz). ~8x less CPU.

### 6. pipeline.rs — Jitter buffer
- `JITTER_DEPTH: 5 → 3` (100ms → 60ms)
- Matches Discord's typical range. Reduces perceived delay.

### 7. codec.rs — Voice encoder improvements
- Set bitrate to 64kbps (`set_bitrate(Bitrate::BitsPerSecond(64000))`)
- Set complexity to 5 (`set_complexity(5)`)
- Enable DTX (`set_dtx(true)`) — near-zero bytes during silence
- Keep in-band FEC enabled

### 8. codec.rs + pipeline.rs — FEC decode
- Add `decode_fec` method to `OpusDecoder` that passes `fec=true`
- In jitter buffer drain: when packet N is missing and N+1 is available, decode N+1 with FEC to recover N, then decode N+1 normally on next drain

### 9. Hot-swap handling
- Input device hot-swap: rebuild capture resampler with new device rate, drain capture ring buffer
- Output device hot-swap: rebuild playback resamplers with new device rate, drain voice/stream ring buffers
- Same pattern as current code, just updating resampler references

## What stays the same
- Ring buffer type: `HeapRb<i16>` (no type change)
- UDP packet format, protocol, packet types
- AEC/NS/AGC processing (already on main loop thread)
- Video pipeline — untouched
- Community server C++ code — untouched
- All Tauri commands and React frontend — untouched
- Stream audio pipeline (`audio_stream_pipeline.rs`) — untouched

## Files Modified
- `src-tauri/src/media/pipeline.rs` — callbacks, main loop, jitter buffer, resamplers
- `src-tauri/src/media/codec.rs` — encoder config, FEC decode method

## Performance Budget
Main loop iteration: 5ms budget
- Capture resample (~240 samples): <0.1ms
- Opus encode: ~0.3ms (complexity 5, down from ~0.6ms at 10)
- Opus decode × 3 peers: ~0.5ms
- Playback resample (3 peers × 240 samples): <0.3ms
- Total: ~1.2ms — well within budget with headroom for more peers

## Quality Expectations
- **Clarity:** 64kbps Opus matches/exceeds Discord free tier (64kbps)
- **Latency:** 60ms jitter buffer, competitive with Discord
- **Resampling:** Sinc interpolation (better than WebRTC's linear resampler)
- **Packet loss:** FEC recovery instead of basic PLC
- **Gap vs Discord:** AI noise suppression (Krisp) — separate future project

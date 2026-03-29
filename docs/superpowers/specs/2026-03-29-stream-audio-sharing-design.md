# Stream Audio Sharing — Design Spec

**Date:** 2026-03-29
**Status:** Approved

## Overview

Add audio sharing to Decibell's screen/window streaming. When a user streams with audio enabled:
- **Window capture**: capture audio only from that window's process
- **Screen capture**: capture system-wide audio excluding Decibell's own output

Audio is sent as a separate stereo Opus stream alongside video, giving viewers independent volume control.

## Requirements

1. Per-process audio capture for window streams (Linux + Windows)
2. System-wide-minus-self audio capture for screen streams (Linux + Windows)
3. Stereo Opus encoding at 128 kbps (default) or 192 kbps (user-selectable)
4. Separate UDP packet type for stream audio (independent from voice)
5. Viewer-side volume slider for stream audio (0–100%)
6. Audio capture lifecycle tied to video capture (starts/stops together)
7. No explicit A/V sync — natural sync is sufficient for live streaming

## Architecture

### New Packet Type

`PACKET_TYPE_STREAM_AUDIO = 4` in `packet.rs`. Same packet structure as voice audio packets (1-byte type, 32-byte sender_id, 2-byte sequence, 2-byte payload_size, payload) but carrying stereo Opus data. Stereo Opus at 128 kbps produces ~320 bytes per 20ms frame — well within the 1400-byte payload limit. The server forwards these the same way it forwards voice/video packets — by sender address.

### New Modules

| Module | Platform | Purpose |
|--------|----------|---------|
| `capture_audio_pipewire.rs` | Linux | PipeWire per-process and loopback-minus-self capture |
| `capture_audio_wasapi.rs` | Windows | WASAPI process loopback for both modes |
| `audio_stream_pipeline.rs` | All | Opus encode + packetize + send stream audio |

### Platform Audio Capture

#### Linux — PipeWire

**Window mode (per-process):**
- Enumerate PipeWire nodes to find the audio output node matching the target window's PID
- Connect a capture stream to that specific node
- Output stereo f32 PCM at 48 kHz

**Screen mode (system-minus-self):**
- Capture the monitor of the default audio sink (loopback capture)
- Before starting: redirect Decibell's own CPAL playback node to a temporary private PipeWire sink, so Decibell's output doesn't appear in the loopback
- On stop: restore Decibell's playback to the default sink
- Output stereo f32 PCM at 48 kHz

#### Windows — WASAPI Process Loopback

**Window mode (per-process):**
- `ActivateAudioInterfaceAsync` with `PROCESS_LOOPBACK` + `INCLUDE_PROCESS_TREE` using the target window's process ID
- Captures all audio from that process and its child processes
- Requires Windows 10 2004+

**Screen mode (system-minus-self):**
- `ActivateAudioInterfaceAsync` with `PROCESS_LOOPBACK` + `EXCLUDE_PROCESS_TREE` using Decibell's own process ID
- Captures all system audio except Decibell's output
- Requires Windows 10 2004+

Both modes output stereo f32 PCM at 48 kHz via a channel: `Receiver<AudioFrame>`.

### Stream Audio Pipeline — `audio_stream_pipeline.rs`

Mirrors `video_pipeline.rs` in structure:

1. Receive `AudioFrame` from platform capture channel
2. Opus-encode as stereo at configured bitrate (128 or 192 kbps)
3. Packetize as `PACKET_TYPE_STREAM_AUDIO` with sender_id and sequence number
4. Send via the shared UDP socket (same socket as voice + video)
5. Controlled via `StreamAudioControl::Shutdown`

**Opus encoder config:**
- Channels: 2 (stereo)
- Sample rate: 48,000 Hz
- Frame size: 960 samples (20ms)
- Application profile: Audio (not VoIP — better for music/game audio)
- Bitrate: 128,000 or 192,000 bps

### Receiver Side — Changes to `pipeline.rs`

1. Detect `PACKET_TYPE_STREAM_AUDIO` in the recv loop
2. Maintain a separate stereo Opus decoder per streamer in the peers map
3. Decode stereo Opus → stereo f32 PCM (960 samples × 2 channels)
4. Apply viewer's volume scalar (0.0–1.0, default 1.0)
5. Mix stereo stream audio into the output device buffer alongside voice
6. Handle the stereo→device channel mapping (same as voice but with 2 source channels)

### New Data Types

```
AudioFrame {
    data: Vec<f32>,    // interleaved stereo f32 PCM
    channels: u16,     // always 2
    sample_rate: u32,  // always 48000
}

StreamAudioConfig {
    bitrate_kbps: u32, // 128 or 192
}

StreamAudioControl {
    Shutdown,
}
```

## Data Flow

```
Sender:
  Platform capture (PipeWire / WASAPI)
    → stereo f32 PCM @ 48kHz
    → audio_stream_pipeline
    → Opus encode (stereo, 128/192 kbps)
    → UDP packet (type=4, sender_id, seq, opus payload)
    → shared socket → server

Receiver:
  UDP recv loop (pipeline.rs)
    → detect type=4
    → stereo Opus decode (per-streamer)
    → apply volume scalar
    → mix into output buffer with voice
```

## UI Changes

**Sender (stream start dialog):**
- "Audio" toggle (default: off)
- Audio quality selector: "Standard (128 kbps)" / "High (192 kbps)"
- Both controls passed as parameters to `start_stream` command

**Viewer:**
- Stream audio volume slider (0–100%, default 100%)
- Slider appears when a stream with audio is active

## Lifecycle

- Stream audio capture and pipeline start when `start_stream` is called with `audio_enabled: true`
- They stop when `stop_stream` is called or voice disconnects
- Teardown order: audio stream pipeline → video pipeline → voice engine (mirrors existing video teardown)

## A/V Sync

No explicit sync mechanism. Audio and video are independent UDP streams played on arrival. Natural sync is sufficient for live screen sharing — both originate from the same machine at the same time, and the encoding latency difference is imperceptible for live content. Timestamp-based sync can be added later if needed.

## Platform Requirements

- **Linux**: PipeWire (already a dependency)
- **Windows**: Windows 10 version 2004+ (for WASAPI process loopback)
- **Opus**: Already available via `audiopus` crate — need stereo encoder instance

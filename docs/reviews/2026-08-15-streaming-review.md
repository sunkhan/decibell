# Streaming code review (2026-08-15)

Scope: renderer send (`StreamCapture.ts`), watcher (`StreamVideoPlayer.tsx`),
native wire (`video_pipeline.rs`, `video_packet.rs`, `video_receiver.rs`,
`mod.rs` recv thread), IPC fan-out (`events.rs`, `addon.ts`, preload).
`B` = bug, `R` = robustness, `O` = optimization. Priority order.

> ## Fix round, same day — status
>
> Everything below EXCEPT B4 (deferred by request) and O1/O3-as-written
> is **fixed**; 63 native tests + typecheck + Vite build green. Details
> that differ from the original prescriptions:
>
> - **B1**: fixed renderer-side without a wire change (the packet header
>   carries no timestamp and the C++ server must not change). The
>   synthetic-clock lag logic is deleted; pacing is now real
>   backpressure only — the bounded native→JS queue (B3) sheds load at
>   the source, `decodeQueueSize` covers decode overload, and both drop
>   paths re-request a keyframe (R3).
> - **B2/B5**: `VideoReceiver` reworked — all state keyed per sender
>   (`[u8;32]` sender_id, zero-alloc); completed frames route through an
>   ordered-delivery gate with an 80ms reorder hold; late keyframes
>   can't rewind the cursor but genuine stream restarts (ids re-zeroed)
>   are accepted. NACK/PLI are per-streamer, PLI throttling moved into
>   the receiver.
> - **R3** landed *with* its native command (`request_stream_keyframe`)
>   — B4 proper (fire it on watch-subscribe) remains one call, noted in
>   StreamVideoPlayer.
> - **O2**: canvas now sized to `visibleRect` (correct for both the
>   HEVC display-halving bug and legit conformance crops).
> - **O3**: recv maintenance cadence 100ms → 25ms.
> - **O1** deferred: per-frame `webContents.send` fan-out stands; the
>   MessagePort pipe is follow-up work.
>
> ### The two user-reported symptoms (investigated same day)
>
> - **Stream audio "a couple seconds" behind video — root-caused and
>   fixed.** `peer.rs::STREAM_AUDIO_DELAY_MS = 1000` (Linux-only) was a
>   fossil compensating for the Tauri/WebKitGTK MSE player's ~1s video
>   buffer. Electron's WebCodecs path paints immediately, so the hold
>   had become pure audio lag. Delay queue deleted end-to-end.
> - **Windows + AMD GPU crash on stream start — mitigated, root cause
>   needs a crash dump.** Every reachable Rust path in the Windows
>   pipeline (encoder.rs, encoder_thread.rs, capture_wgc.rs,
>   video_processor.rs, gpu_pipeline.rs, WASAPI) is Result-based — no
>   panic path found by reading, so the crash is an access violation
>   inside driver/AMF/FFmpeg code (pipeline only ever tested on NVENC).
>   Landed: (1) native start failure now **falls back to renderer
>   WebCodecs/OpenH264** instead of failing Go Live (start_screen_share
>   honours `nativeEncode:false` on Windows now); (2) escape hatch
>   `localStorage decibell.win_native_encode = "0"` forces the WebCodecs
>   path outright — give this to the AMD user immediately; (3) encoder
>   thread body wrapped in catch_unwind + emits `native_stream_failed`
>   so a mid-stream death tears down UI state with a toast instead of
>   streaming into the void. **Get the friend's crash dump / Sentry
>   native event to find the actual AV.** Windows-only code changed
>   blind on Linux — the Windows CI build must compile it before
>   release.

## Bugs — likely behind user-visible streaming problems

### B1 — Receiver timestamps hardcode 30fps and break watcher pacing
`mod.rs:228` synthesizes `timestamp = frame_id * 33_333` (33.3ms/frame).
`StreamVideoPlayer.tsx:268-299` compares that clock to wall-clock and
drops frames >500ms "late". Any stream *effectively* below 30fps —
damage-driven window capture of static content, encoder drops, 15fps
setting — makes `lagUs` balloon, so every delta is dropped and the
keyframe gate re-arms: the watcher degrades to a keyframe-only
slideshow between GOPs. Above 30fps, lag goes negative and the late-drop
safety never engages. Fix: carry the real capture timestamp on the wire
(header has room) or drive pacing off arrival time, not synthetic ids.

### B2 — Two concurrent streams corrupt each other in the receiver
`video_receiver.rs` keys assemblies by `frame_id` alone and both
senders start at 0 (`video_pipeline.rs:32`), so fragments from two
streamers interleave into the same `FrameAssembly`. The recv thread
(`mod.rs:395`) also tracks a single `video_streamer_username` — NACK/PLI
go only to the last-seen sender, and `has_received_keyframe` resets on
every sender flip. The preload (`streamFrameSubsByUser` Map) and UI are
multi-stream ready; native is single-stream. Key assemblies (and NACK
state) by `(sender, frame_id)`. This is the thing the R9 pending
multi-stream test would have caught.

### B3 — Stream TSFN queue is unbounded; the drop counter can never fire
`events.rs:69` `create_threadsafe_function(0, …)` = unbounded queue, so
`NonBlocking` never reports saturation: `STREAM_FRAME_DROPS` is dead
code, and a busy renderer buffers encoded frames in main-process memory
instead of dropping — latency grows without bound and never recovers.
Give it a small bound (~8) so stalls shed frames and the counter works.

### B4 — Watcher mid-stream join never requests a keyframe
Acknowledged TODO at `StreamVideoPlayer.tsx:360-366`: a remote watcher
joining mid-GOP waits on a spinner until a *loss-triggered* PLI or the
next natural IDR. Expose a `request_keyframe`-style command that sends
`UdpKeyframeRequest` when a wire subscription starts (native already has
the packet + path; only the renderer→native command is missing).

### B5 — Reassembled frames can be delivered out of order
A NACK/FEC-completed older frame is emitted *after* a newer one
(`video_receiver.rs:263-278` has no monotonicity check —
`last_complete_frame_id` is tracked but unused). The decoder then gets
an out-of-order delta → corruption/decoder error → reset → wait for
PLI. Drop completed non-keyframes with `frame_id <=` the last delivered
id (mind u32 wrap).

### B6 — Stop/start race in `startActiveStream`
`StreamCapture.ts:748-754`: `void active.stop()` then immediately
constructs the new session. On the native path the old
`stop_screen_share` can land *after* the new `start_screen_share` and
tear down the new stream (settings-change → quick restart). Await the
old stop, or sequence stop→start in native.

## Robustness

### R1 — FEC recovers at most one packet per frame, ever
`FrameAssembly.fec_recovered` is frame-global (`video_receiver.rs:108`),
so a frame missing one packet in each of two *different* FEC groups
recovers the first and then FEC is disabled for that frame — even
though the groups are independent. Track recovery per group (or loop
until no progress). Also `fec_groups` is unbounded per frame
(`process_fec_packet` pushes without cap) — cap at
`total_packets / FEC_GROUP_SIZE + 1`.

### R2 — First-frame wait can hang `start()` forever
`StreamCapture.ts:261` awaits the first `VideoFrame` with no timeout; a
fully-occluded/minimized window on some compositors produces none, and
the caller's await never resolves — no error, no toast. Add a timeout
(~5s) with a clear error.

### R3 — Late-delta lag drop re-arms the keyframe gate but nobody asks for one
`StreamVideoPlayer.tsx:279-293`: on queue/lag drops it sets
`needsKeyframeRef` and waits — but renderer-side drops don't trigger the
native PLI path, so the stall lasts until the next natural IDR. After
B4 lands, fire the keyframe request here too.

### R4 — Two inconsistent stale-frame lifetimes
`check_missing` discards assemblies at `buffer_depth * 3` = 150ms
(`video_receiver.rs:326`) while `cleanup_stale` uses 500ms — whichever
runs first wins, which makes the effective NACK window unpredictable.
Pick one number.

## Optimizations / smaller items

- **O1 — Per-frame `webContents.send` fan-out** (`addon.ts:160`): every
  encoded frame is structured-cloned main→renderer (and to *every*
  window) at up to 60fps. A `MessagePort` pipe straight to the renderer
  would skip the main-process hop; at minimum, target only the main
  window.
- **O2 — HEVC draw stretches conformance-cropped streams**
  (`StreamVideoPlayer.tsx:185-199`): drawing `visibleRect` up to
  `codedWidth×codedHeight` distorts legit crops (1080p coded as
  1920×1088 → 8px vertical stretch). Size the canvas to the visible
  rect; keep the coded-size workaround only for the HEVC half-size
  `displayWidth` case it was added for.
- **O3 — NACK reaction latency**: maintenance runs every 100ms against a
  50ms `nack_timeout` (`mod.rs:543`), so retransmit requests go out
  100–150ms after loss. Tighten the maintenance interval (~25ms) for
  faster recovery at negligible cost.
- **O4 — Dead/dusty bits**: `StreamCapture.frameCounter` is written,
  never read; `send_video_frame` rejects with an error (logged per
  frame) when the sink is cleared mid-drain — return Ok/no-op instead;
  `StreamVideoPlayer` early-returns before its hooks when `VideoDecoder`
  is missing (stable in practice, still a rules-of-hooks violation).

## Addendum — 120fps path audit (same day)

**B7 — encoder codec-string levels were wrong at 120fps `✓ FIXED`.**
`webCodecsStringForCodec` picked H.264/HEVC levels off a binary
`fps > 30` split, valid at 60fps but under the spec throughput limits
at 120: H.264 720p120 declared L3.2 (needs 4.2), 1080p120 declared L4.2
(needs 5.1), 1440p120 declared L5.1 (needs 5.2); HEVC 720p120 declared
L3.1 (needs 4.1) — and HEVC 720p**60** already exceeded L3.1's
MaxLumaSr, broken at 60fps too. Per §5.3 of the handoff, an
under-declared level fails `isConfigSupported` at the real resolution,
so the renderer path's soft preflight threw before the HW ladder could
rescue it: 120fps Go Live died on the WebCodecs path. Now table-driven
from the specs' A-1 / A.8 limits (MaxFS+MaxMBPS, MaxLumaPs+MaxLumaSr);
verified to reproduce or validly refine every 30/60fps pick.

Everything else on the 120fps path checks out by reading: UI offers
120, encode/decode ceilings advertise it (AV1/HEVC/H264_HW), watcher
decoder strings are max-level, AV1's single L4.0 string covers 4K120,
native pts/GOP are fps-derived, and the bounded frame queue holds 66ms
at 120. Two non-bug caveats: nothing clamps the fps picker to the
selected codec's maxFps (H264_SW honestly caps at 60 — picking 120
just backpressure-drops to whatever OpenH264 sustains), and the
send side still has no NACK retransmit / FEC (watcher NACKs are dead
letters — the recv loop has no PACKET_TYPE_NACK branch on the streamer
side), so every loss costs a PLI+IDR; at 120fps the packet rate doubles
and this will be felt sooner. Both are candidates for a follow-up.

**B8 — fresh launch locked all streams as "codec unsupported"
`✓ FIXED` (user-reported, both platforms).** The Watch gate
(`canWatchStream` via VoicePanel / UserProfilePopup) reads `decodeCaps`
from `codecSettingsStore`, but that store's `load()` only ran when the
Settings → Codecs tab mounted — so on a fresh launch the gate compared
streams against an empty list and locked everything until the user
visited that tab. Two-part fix: `main.tsx` hydrates the store at boot
(sequenced after both probes ship caps to native, since `load()` reads
them back from `get_caps`), and `canWatchStream` now fails open on an
empty caps list — provably "not loaded yet", never "nothing supported",
because `probeDecoders` unconditionally appends the §3.3 H.264
fallback.

## Healthy — reviewed, no action

Packet parsing is hardened and well-tested (payload-size clamp, packed
struct copies, fragment caps, bounded partial frames); the
`send_video_frame` hot path avoids the AppState lock via the frame-sink
slot; TSFN payloads are zero-copy out of Rust; thumbnail path and
`useStreamThumbnails` are clean.

# Client code review — bugs & optimization targets (2026-06-11)

> **Status (same day):** B1–B8 fixed; O1–O4 implemented. During O1 a new
> remotely-triggerable panic was found and fixed: `UdpVideoPacket::from_bytes`
> didn't clamp the wire-claimed `payload_size`, so a datagram claiming
> >1400 bytes panicked `payload_data()`'s slice on the video-recv thread.
> Also fixed in passing: a pre-existing broken native test (LoginResponse
> missing `memberships`) that had blocked `cargo test` since auto-rejoin.
> ChatPanel's per-row shouldGroup was kept (documented deliberate tradeoff);
> a WeakMap epoch cache removes its re-parse cost instead of precomputing.
> Still open: B9 deep-link validation, security-posture items (TOFU cert
> pinning, sandbox revisit, IPC sender check), O5 roster-diff volume
> re-applies, O6 bundle splitting, O7 arc-swap frame sink, O8 per-window
> frame gating.

Five-agent review of `electron-client/` (renderer, main+preload, native Rust), all
headline findings hand-verified against source. Items marked ✗REJECTED were agent
claims that did not survive verification — kept here so they don't get re-reported.

## Bugs (priority order)

### B1. Media proxy never notices client abort — `electron/main/mediaServer.ts`
The streaming loop (`reader.read()` → `res.write`) has only a `drain` handler; there is
no `req`/`res` close monitoring and no `reader.cancel()`. When the user scrubs a video
or closes playback, the proxy keeps pulling the remainder of the upstream response —
for a full-file GET of a large video that's the entire file over TLS for nothing.
Reader errors mid-stream are also uncaught.
**Fix:** `res.on("close", () => reader.cancel().catch(() => {}))` + try/catch around the
read loop. ~10 lines.

### B2. `config.json` written non-atomically — `native/src/config.rs:246-279`
`save()` does read-modify-write with a direct `std::fs::write` to the target path. A
crash mid-write tears the file (losing saved credentials + settings); two concurrent
saves can interleave (login saving credentials while a settings save preserves the old
ones it read earlier).
**Fix:** write to `config.json.tmp`, then `fs::rename` (atomic). Optionally serialize
saves behind a small mutex. Also a blocking FS call on the async command path —
`spawn_blocking` while at it.

### B3. Pending oneshot maps leak on disconnect — `native/src/net/central.rs`
`pending_invite_resolves` / `pending_avatar_fetches` / `pending_thumbnail_fetches`
entries are only removed when their response arrives. When `route_packets` exits on
connection loss, in-flight entries are stranded: callers burn their full 5s timeout
instead of failing fast, and entries persist until overwritten by a same-key retry.
**Fix:** on route_packets exit, drain all pending maps (dropping the senders is enough —
receivers see `RecvError` immediately).

### B4. Silent video frame drops at the TSFN boundary — `native/src/events.rs`
`STREAM_BUS.call(frame, NonBlocking)` drops frames silently when the JS queue is
saturated. Voice counts its drops; video has no counter, so a slow renderer manifests
as "stream stutters, no evidence anywhere".
**Fix:** count `Status != Ok` returns, fold into the existing periodic stats event.

### B5. `fileRegistry.sweepStale()` is dead code — `electron/main/fileRegistry.ts`
Defined, never called. Abandoned upload registrations linger in the whitelist.
**Fix:** `setInterval(sweepStale, 5 * 60_000)` in main init (unref the timer).

### B6. FEC robustness on truncated packets — `native/src/media/mod.rs:~480`
The FEC packet copy is `mem::zeroed()` + bounded copy, so a truncated datagram is NOT
UB (agent overclaimed), but it yields a zero-filled payload tail that silently corrupts
XOR recovery. **Fix:** require `n >= header + claimed payload window` before processing,
or have `process_fec_packet` validate against `payload_size_xor`.

### B7. `eprintln!` in media threads — `media/mod.rs` video-recv loop, `audio_stream_pipeline.rs`, `commands/streaming.rs`
Post-O_NONBLOCK-fix these now block instead of panic, so the old crash class is gone,
but the video-recv loop still prints per keyframe / per FEC recovery / every 300th
frame on a thread with a 5ms recv budget, and stream-audio prints per error frame.
**Fix:** mechanical `log::debug!`/`log::warn!` conversion in `media/` + `commands/`.

### B8. Update-check `setTimeout` never cleared — `electron/main/update.ts`
5s boot timer can fire during early quit. Store the handle, clear in `before-quit`. Minor.

### B9. Deep-link URL forwarded to renderer unvalidated — `electron/main/index.ts`
`decibell://` argv/open-url strings go straight to the renderer event. Parse + validate
shape in main (reject `..`, null bytes, non-invite paths) as defense-in-depth. Minor.

### Security posture (flagged, known design choices)
- Accept-all `setCertificateVerifyProc` + `certificate-error` bypass: consider TOFU
  pinning per server (store first-seen cert hash, warn on change). Cheap, big MITM win.
- `webSecurity: false` + `sandbox: false`: revisit whether the Windows MF constraint
  still forces this on all platforms; consider enabling sandbox on Linux/macOS at least.
- `ipcMain.handle("decibell:invoke")` doesn't validate `event.sender` — one-line check
  against the main window's webContents id.

## Optimizations (ranked by expected real-world impact)

### O1. Video reassembly allocation storm — `native/src/media/video_receiver.rs`
Hottest path in the client (~2000 pkts/sec watching 1080p60). Per fragment:
`payload_data().to_vec()` into a `HashMap<u16, Vec<u8>>`, then `reassemble()` grows an
unsized `Vec::new()` with `extend_from_slice`, then the result is copied again into the
TSFN buffer. Three copies + 2 allocs per fragment.
**Fix:** per-frame contiguous buffer `Vec::with_capacity(total_packets * 1400)` (or
exact via payload sizes), write fragments at `index * MAX_PAYLOAD` offsets, track a
received-bitmap; reassembly becomes truncate-and-hand-off. Bonus in `media/mod.rs`
recv loop: `pkt.sender_username()` + `username.clone()` allocate per packet — compare
raw bytes against the cached streamer name instead.

### O2. Audio-path buffer reuse — `native/src/media/pipeline.rs`, `peer.rs`
- voice recv: `buf[..n].to_vec()` per packet through the mpsc (50/sec/speaker) — use a
  fixed-size buffer pool or `ArrayVec`-style payload.
- resampler feeds: `drain(..n).collect::<Vec<f64>>()` per chunk in `peer.rs:156` and
  the stream-audio path (`pipeline.rs:~991`, both channels) — keep a scratch Vec per
  peer/stream, `clear()` + `extend` instead of fresh `collect()`.

### O3. Upload-progress re-render fan-out — renderer
`PendingAttachmentsRow.tsx:8` and `BubbleInflightAttachments.tsx:12` subscribe to the
entire `pendings` record → every progress tick of any upload re-renders them all.
Select per-channel slices (store-side selector + `useShallow`), or throttle
`updateProgress` to ~4Hz store commits (transferredBytes doesn't need 60Hz fidelity).

### O4. Chat list row work — renderer
`DmChatPanel.tsx:291` rebuilds `bubbleMessages` (full map + spreads) every render —
wrap in `useMemo`. `shouldGroup()` re-parses timestamps (2× `new Date` per visible row
per render) in both chat panels — precompute a `grouped` flag when messages are merged
into the store, or memoize per message id.

### O5. Presence-update volume re-applies — `useVoiceEvents.ts:~94`
Deliberate re-apply loop, but it invokes for every custom-volume/muted user on every
roster event. Diff against the previous roster and only invoke for newly-seen users.

### O6. Bundle: 9.3MB single JS chunk
`twemoji-data.json` and the emoji picker inflate the main chunk. Dynamic-import the
picker (`React.lazy`) and fetch emoji data on first open; also consider
`build.rollupOptions.manualChunks` for sentry/virtuoso. Faster boot, lower memory.

### O7. Frame-sink Mutex → arc-swap — `native/src/media/video_pipeline.rs`
60-120 locks/sec, nanoseconds each — micro. `arc-swap` is already in Cargo.toml; swap
when convenient. Also use `unwrap_or_else(|e| e.into_inner())` to avoid poison panics.

### O8. Main-process event fan-out
`BrowserWindow.getAllWindows()` loop per event/frame is fine today (single window), but
if a stream pop-out window ever lands, add per-window subscriber gating for
`decibell:stream_frame` first.

## ✗REJECTED agent claims (verified false — don't re-report)
- "StreamCapture swallows keyframe requests under backpressure" — code already gates
  `wantKeyframe` consumption inside the queue check (comment documents exactly this).
- "StreamVideoPlayer description ArrayBuffer can detach via GC" — `descriptionRef`
  holds the copied buffer; GC never detaches ArrayBuffers.
- "EncodedVideoChunk aliases the IPC buffer" — WebCodecs spec copies `data` at
  construction.
- "useStreamThumbnails subscription pile-up" — cleanup is correct; max one
  subscription; callback reads `getState()` so `.length` dep is deliberate.
- "FEC copy is uninitialized-memory UB" — struct is `mem::zeroed()` first (downgraded
  to B6 robustness).
- "Stream frames fan out O(N·M) across windows" — app creates exactly one window;
  frames only arrive for watched streams (kept as O8 future-proofing).

## Verified-clean areas
Renderer event-listener cleanup, blob URL revocation, upload AbortController handling,
chatStore LRU eviction, message dedup/merge, jitter buffer + adaptive voice path design.

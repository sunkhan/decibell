import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  streamerUsername: string;
  className?: string;
}

interface MseInitPayload {
  username: string;
  mime: string;
  data: string; // base64 fMP4 init segment
}

interface MseSegmentPayload {
  username: string;
  data: string; // base64 fMP4 media segment
  keyframe: boolean;
}

function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// MSE-backed `<video>` player for Linux. Used for both remote streams
// and self-preview. Receives fMP4 init + media segments emitted from
// Rust (`stream_mse_init` / `stream_mse_segment`) and feeds them to a
// SourceBuffer. The browser (WebKitGTK + GStreamer) handles decode and
// renders directly via the compositor — no Rust decode, no NV12 IPC,
// no WebGL upload.
export default function MseStreamVideoPlayer({ streamerUsername, className }: Props) {
  const videoRef = useRef<HTMLVideoElement>(null);
  const [hasFirstFrame, setHasFirstFrame] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    if (typeof MediaSource === "undefined") {
      setError("MediaSource not supported");
      return;
    }

    let stopped = false;
    let mediaSource: MediaSource | null = null;
    let sourceBuffer: SourceBuffer | null = null;
    let pendingMime: string | null = null;
    // Backpressure queue: appendBuffer is async (updateend completes the
    // call). We can't append while one is in flight, so segments arriving
    // during an in-flight append get queued. Cap the queue to drop old
    // segments under sustained backpressure rather than building unbounded
    // memory.
    const queue: Uint8Array[] = [];
    const QUEUE_MAX = 30;
    let unlistenInit: (() => void) | undefined;
    let unlistenSegment: (() => void) | undefined;

    // Live-edge management.
    //
    // The numbers below are tuned for WebKitGTK's MSE pipeline, which
    // has two hard cliffs that bit us at lower target latencies:
    //
    // 1. `MediaSource::monitorSourceBuffers` only reports
    //    HAVE_ENOUGH_DATA when ≥3s of forward buffer is present
    //    (`kHaveEnoughDataThreshold`). Below 3s you're in
    //    HAVE_FUTURE_DATA, where any micro-event drops you to
    //    HAVE_CURRENT_DATA → `waiting` event → stutter.
    // 2. `MediaSourcePrivate::hasFutureTime` uses an 83.4ms
    //    `timeFudgeFactor` to decide whether the playhead is "inside"
    //    a buffered range. A buffered-range split smaller than that
    //    is invisible; larger and `waiting` fires even with data
    //    ahead.
    //
    // 1.0s of cushion sits well above the fudge factor with ~6 frames
    // of headroom at 60fps. SEEK at 4.0s catches catastrophic drift.
    // No discrete CATCHUP threshold: the playback rate is driven by
    // a proportional controller (rate = 1 + k·(lag − target)) instead
    // — modelled on dash.js's LiveCatchupController, but linear+clamped
    // rather than sigmoid for simplicity. The discrete band the player
    // had before allowed lag to drift unchecked between target and
    // CATCHUP_LAG_S, which produced the visible "creeping latency"
    // even though no `waiting` ever fired.
    const TARGET_LATENCY_S = 1.0;
    const SEEK_LAG_S = 4.0;
    const RATE_GAIN = 0.4;
    const RATE_MAX = 1.20;
    const RATE_MIN = 1.0;
    const RETENTION_S = 12;
    const EVICT_HIGH_WATER_S = 20;
    const EVICT_INTERVAL_MS = 2000;
    // chaseLiveEdge is throttled so per-frame `playbackRate` writes
    // don't churn the GStreamer pipeline (each rate change cycles a
    // qos / rate-change event downstream).
    const CHASE_INTERVAL_MS = 200;
    // EWMA of recent lag — chase decisions read this rather than the
    // raw current lag, so a single jittery frame doesn't twitch the
    // playback rate.
    const LAG_EWMA_ALPHA = 0.25;
    let smoothedLag = 0;
    let lastChaseMs = 0;
    let lastEvictMs = 0;
    let initialSeekDone = false;

    function chaseLiveEdge() {
      if (!sourceBuffer || video!.buffered.length === 0) return;
      const nowMs = performance.now();
      if (nowMs - lastChaseMs < CHASE_INTERVAL_MS) return;
      lastChaseMs = nowMs;

      const liveEdge = video!.buffered.end(video!.buffered.length - 1);
      const lag = liveEdge - video!.currentTime;
      smoothedLag = smoothedLag === 0
        ? lag
        : smoothedLag * (1 - LAG_EWMA_ALPHA) + lag * LAG_EWMA_ALPHA;

      // Initial seek: HTML5 video starts at currentTime=0; without an
      // initial seek to (liveEdge - target) we'd play from the first
      // buffered frame and stay full-buffer behind forever. Snap to a
      // 90 kHz tick so we land exactly on a sample boundary — WebKit
      // rounds to the nearest sample's PTS otherwise, which can put
      // currentTime up to one frame off and on the wrong side of
      // `timeFudgeFactor`.
      if (!initialSeekDone && liveEdge >= TARGET_LATENCY_S) {
        const target = liveEdge - TARGET_LATENCY_S;
        video!.currentTime = Math.round(target * 90000) / 90000;
        video!.playbackRate = 1.0;
        smoothedLag = TARGET_LATENCY_S;
        initialSeekDone = true;
        return;
      }

      if (smoothedLag > SEEK_LAG_S) {
        // Far enough behind that catch-up rate alone won't bridge it
        // in any reasonable time — hard seek to the target lag.
        const target = Math.max(0, liveEdge - TARGET_LATENCY_S);
        video!.currentTime = Math.round(target * 90000) / 90000;
        video!.playbackRate = 1.0;
        smoothedLag = TARGET_LATENCY_S;
      } else {
        // Proportional rate control: above target, accelerate
        // proportionally to the error (capped at RATE_MAX). At target
        // or below, rate = 1.0 (we never slow down — that warps audio
        // pitch and feels worse than a tiny lag undershoot).
        const lagError = smoothedLag - TARGET_LATENCY_S;
        const targetRate = lagError > 0
          ? Math.min(RATE_MAX, RATE_MIN + lagError * RATE_GAIN)
          : RATE_MIN;
        if (Math.abs(video!.playbackRate - targetRate) > 0.005) {
          video!.playbackRate = targetRate;
        }
      }

      // Eviction sits on a high-water mark instead of running every
      // tick: removing samples near the playhead is a known cause of
      // buffered-range splits in WebKit (bug 167834 / commit 48b51f0),
      // and the splits in turn fire `waiting` once the gap exceeds
      // the 83ms fudge factor.
      if (nowMs - lastEvictMs >= EVICT_INTERVAL_MS && !sourceBuffer.updating) {
        const bufStart = video!.buffered.start(0);
        const totalSpan = liveEdge - bufStart;
        if (totalSpan > EVICT_HIGH_WATER_S) {
          const evictTo = video!.currentTime - RETENTION_S;
          if (evictTo > bufStart + 0.5) {
            try {
              sourceBuffer.remove(bufStart, evictTo);
              lastEvictMs = nowMs;
            } catch {}
          }
        }
      }
    }

    function pump() {
      if (!sourceBuffer || sourceBuffer.updating) return;
      const next = queue.shift();
      if (!next) return;
      try {
        sourceBuffer.appendBuffer(next);
      } catch (e) {
        console.error("[MseStreamVideoPlayer] appendBuffer threw:", e);
      }
    }

    // Recreate the SourceBuffer (and MediaSource if needed) after a
    // fatal error. Without this, a single transient SourceBuffer error
    // bricked the player until the next mount; now we tear down,
    // rebuild a fresh MediaSource, and ask the Rust bridge to drop
    // its persisted muxer state for this streamer so the next received
    // frame triggers a fresh init segment we can bootstrap from.
    //
    // Recovery is rate-limited: WebKitGTK's GStreamer-backed MSE can
    // get into states where teardown + recreate doesn't actually clear
    // the broken pipeline. Above MAX_RECOVERIES_IN_WINDOW failures in
    // RECOVERY_WINDOW_MS, we surface the error and stop trying — the
    // alternative was a tight loop of teardown/rebuild that locked up
    // the WebProcess and required killing the app.
    let pendingRebuildMime: string | null = null;
    const MAX_RECOVERIES_IN_WINDOW = 4;
    const RECOVERY_WINDOW_MS = 30_000;
    const recoveryTimes: number[] = [];
    let recoveryGivenUp = false;
    function recoverFromError(reason: string) {
      if (recoveryGivenUp) return;
      const nowMs = performance.now();
      while (recoveryTimes.length > 0
             && nowMs - recoveryTimes[0] > RECOVERY_WINDOW_MS) {
        recoveryTimes.shift();
      }
      if (recoveryTimes.length >= MAX_RECOVERIES_IN_WINDOW) {
        recoveryGivenUp = true;
        console.error(
          `[MseStreamVideoPlayer] giving up after ${recoveryTimes.length} ` +
          `recoveries in ${RECOVERY_WINDOW_MS}ms — last reason: ${reason}`,
        );
        setError("Stream playback failed — please retry");
        try {
          window.clearInterval(stallPoll);
        } catch {}
        return;
      }
      recoveryTimes.push(nowMs);
      console.warn(`[MseStreamVideoPlayer] recovering from: ${reason}`);

      // Tear down both SourceBuffer and MediaSource. Just removing the
      // SourceBuffer leaves the MediaSource attached to a video element
      // whose blob URL may be in a half-broken state; recreating both
      // gives the GStreamer playbin a clean slate.
      if (sourceBuffer) {
        try { mediaSource?.removeSourceBuffer(sourceBuffer); } catch {}
        sourceBuffer = null;
      }
      try {
        if (mediaSource && mediaSource.readyState === "open") {
          mediaSource.endOfStream();
        }
      } catch {}
      try {
        video!.removeAttribute("src");
        video!.load();
      } catch {}
      queue.length = 0;
      pendingMime = null;
      pendingRebuildMime = null;
      initialSeekDone = false;
      smoothedLag = 0;
      nudgeRetry = 0;

      // Force the Rust bridge to drop its muxer state for this
      // streamer so the next received frame triggers a fresh init
      // segment. Also fire a keyframe request so we don't wait for
      // the natural keyframe interval.
      invoke("reset_mse_state", { targetUsername: streamerUsername }).catch(() => {});
      invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

      mediaSource = attachMediaSource();
    }

    function setupSourceBuffer(mime: string) {
      if (!mediaSource) return;
      if (sourceBuffer) {
        try { mediaSource.removeSourceBuffer(sourceBuffer); } catch {}
        sourceBuffer = null;
      }
      try {
        const sb = mediaSource.addSourceBuffer(mime);
        sb.mode = "segments"; // explicit-timestamp segments, low latency
        sb.addEventListener("updateend", () => {
          pump();
          chaseLiveEdge();
        });
        sb.addEventListener("error", (_e) => {
          recoverFromError("SourceBuffer error event");
        });
        sourceBuffer = sb;
        pump();
      } catch (e) {
        setError(`SourceBuffer not supported for ${mime}: ${e}`);
      }
    }

    function attachMediaSource(): MediaSource {
      const ms = new MediaSource();
      video!.src = URL.createObjectURL(ms);
      ms.addEventListener("sourceopen", () => {
        // Tell the browser this is a live stream — without
        // duration=Infinity, the video element treats it as a
        // finite-duration clip and changes its scheduling/preload
        // behaviour in ways that cause periodic stalls.
        try { ms.duration = Number.POSITIVE_INFINITY; } catch {}
        if (pendingMime) {
          const mime = pendingMime;
          pendingMime = null;
          setupSourceBuffer(mime);
        } else if (pendingRebuildMime) {
          const mime = pendingRebuildMime;
          pendingRebuildMime = null;
          setupSourceBuffer(mime);
        }
      });
      ms.addEventListener("sourceended", () => {
        console.warn("[MseStreamVideoPlayer] MediaSource ended unexpectedly");
      });
      return ms;
    }

    // Stall recovery — modelled on hls.js's gap-controller and Shaka's
    // gap-jumping controller. WebKit fires `waiting` for *both*
    // network underflow and decoder pipeline glitches, so a `waiting`
    // with data buffered ahead is normal and routinely recovers if we
    // give the renderer a tiny seek to cycle the readyState machine.
    //
    // Strategy, in order of escalation:
    //   1. If a *next* buffered range exists past a sub-fudge-factor
    //      gap, hop to its start + 1ms (Shaka gap-jump pattern).
    //   2. If the playhead is inside a range with data ahead, do a
    //      1µs forward seek — too small to be visible, large enough
    //      to flush WebKit's `monitorSourceBuffers` state machine
    //      (hls.js's `nudgeOnVideoHole` pattern).
    //   3. If the µs-flush doesn't unstick, escalate to 0.1s, 0.2s,
    //      0.3s nudges (hls.js's `nudgeOffset` ladder).
    //
    // FUDGE_FACTOR_S mirrors WebKit's `MediaSourcePrivate::timeFudgeFactor`
    // = 2002/24000 ≈ 0.0834s.
    const FUDGE_FACTOR_S = 0.0834;
    const NUDGE_OFFSET = 0.1;
    const NUDGE_MAX_RETRY = 3;
    const PIPELINE_FLUSH_NUDGE = 0.000001;
    const STALL_DETECT_MS = 1250;
    let nudgeRetry = 0;
    let lastNudgeMs = 0;
    let lastProgressMs = performance.now();
    let lastProgressTime = 0;

    function recoverStall() {
      if (video!.seeking || video!.paused) return;
      const buf = video!.buffered;
      if (buf.length === 0) return;
      const ct = video!.currentTime;

      let containingEnd = -1;
      let nextStart = -1;
      for (let i = 0; i < buf.length; i++) {
        const s = buf.start(i);
        const e = buf.end(i);
        if (s - FUDGE_FACTOR_S <= ct && ct <= e + FUDGE_FACTOR_S) {
          containingEnd = e;
        } else if (s > ct && (nextStart < 0 || s < nextStart)) {
          nextStart = s;
        }
      }

      // Gap-jump: a later range exists, our current position has no
      // forward data (or only sub-half-second worth). Shaka adds a
      // 1ms padding past the gap so we land inside the new range.
      if (nextStart > 0 && (containingEnd < 0 || containingEnd - ct < 0.05)) {
        video!.currentTime = nextStart + 0.001;
        return;
      }

      // Inside a range with data ahead → WebKit pipeline glitch.
      // Cheap µs flush first; throttle so a stuck stream doesn't spin.
      const nowMs = performance.now();
      if (containingEnd > ct && nowMs - lastNudgeMs > 250) {
        video!.currentTime += PIPELINE_FLUSH_NUDGE;
        lastNudgeMs = nowMs;
        return;
      }

      // Last resort: escalating nudge-forward.
      if (nudgeRetry < NUDGE_MAX_RETRY) {
        nudgeRetry++;
        video!.currentTime = ct + nudgeRetry * NUDGE_OFFSET;
        lastNudgeMs = nowMs;
      }
    }

    video.addEventListener("playing", () => {
      nudgeRetry = 0;
    });
    video.addEventListener("waiting", () => {
      recoverStall();
    });
    // The video element can fire its own `error` event when the
    // GStreamer pipeline crashes (codec error, broken sample,
    // hardware decoder fault). When that happens the SourceBuffer's
    // `error` event may not fire, so we'd otherwise sit stuck.
    video.addEventListener("error", () => {
      const code = video.error?.code ?? 0;
      const msg = video.error?.message ?? "";
      recoverFromError(`<video> error code=${code} ${msg}`);
    });

    // Poll watchdog — covers the case where WebKit silently stops
    // advancing currentTime without firing `waiting`. 500ms cadence
    // (slower than hls.js's 200ms) because each tick on WebKitGTK can
    // trigger a `monitorSourceBuffers` cascade, and we'd rather miss
    // a 250ms stutter than churn the pipeline.
    const stallPoll = window.setInterval(() => {
      if (stopped || recoveryGivenUp || !video || video.paused || video.seeking) return;
      const nowMs = performance.now();
      if (video.currentTime !== lastProgressTime) {
        lastProgressTime = video.currentTime;
        lastProgressMs = nowMs;
        return;
      }
      if (nowMs - lastProgressMs > STALL_DETECT_MS) {
        recoverStall();
        lastProgressMs = nowMs;
      }
    }, 500);

    mediaSource = attachMediaSource();

    listen<MseInitPayload>("stream_mse_init", (event) => {
      if (stopped || event.payload.username !== streamerUsername) return;
      // New init segment = new codec config. Tear the old SourceBuffer
      // down (if any) and rebuild for the new mime. The init segment
      // itself goes into the queue right after.
      const mime = event.payload.mime;
      const initBytes = b64ToBytes(event.payload.data);

      if (mediaSource && mediaSource.readyState === "open") {
        setupSourceBuffer(mime);
        queue.push(initBytes);
        pump();
      } else {
        // Source isn't open yet — stash the mime and the init segment
        // for the sourceopen handler to consume.
        pendingMime = mime;
        queue.push(initBytes);
      }
    }).then((fn) => {
      if (stopped) fn(); else unlistenInit = fn;
    });

    // Per-N-segments timing log so resource cost is measured on the
    // user's actual hardware, not estimated.
    let timingSamples = 0;
    let sumDecodeMs = 0;
    let sumQueueMs = 0;
    let sumChaseMs = 0;
    let sumTotalMs = 0;
    let maxTotalMs = 0;
    let sumBytes = 0;

    listen<MseSegmentPayload>("stream_mse_segment", (event) => {
      if (stopped || event.payload.username !== streamerUsername) return;
      const totalStart = performance.now();
      const decodeStart = performance.now();
      const bytes = b64ToBytes(event.payload.data);
      const decodeMs = performance.now() - decodeStart;

      const queueStart = performance.now();
      // Drop oldest under sustained backpressure rather than blow up.
      if (queue.length >= QUEUE_MAX) queue.shift();
      queue.push(bytes);
      pump();
      const queueMs = performance.now() - queueStart;

      const chaseStart = performance.now();
      // chaseLiveEdge is also called inside updateend; this is just a hint
      // that we have new data to potentially chase to.
      const chaseMs = performance.now() - chaseStart;
      const totalMs = performance.now() - totalStart;

      if (!hasFirstFrame) setHasFirstFrame(true);

      timingSamples++;
      sumDecodeMs += decodeMs;
      sumQueueMs += queueMs;
      sumChaseMs += chaseMs;
      sumTotalMs += totalMs;
      sumBytes += bytes.byteLength;
      if (totalMs > maxTotalMs) maxTotalMs = totalMs;

      // 600 segments ≈ 10s at 60fps. The previous 1Hz cadence filled
      // WebKit's devtools console buffer quickly enough that on long
      // streams (multiple hours) memory pressure became a stall vector.
      if (timingSamples >= 600) {
        const n = timingSamples;
        const avgDecode = sumDecodeMs / n;
        const avgQueue = sumQueueMs / n;
        const avgChase = sumChaseMs / n;
        const avgTotal = sumTotalMs / n;
        const avgKb = sumBytes / n / 1024;
        const v = videoRef.current;
        const buffered = v?.buffered;
        const ct = v?.currentTime ?? 0;
        // Per-range info — buffered.end(last) - buffered.start(0)
        // overstates the buffer when there are gaps. The "current"
        // range is the one containing the playhead (within the
        // WebKit fudge factor), which is what actually keeps
        // playback alive.
        const ranges = buffered ? buffered.length : 0;
        let totalSpan = 0;
        let currentRangeEnd = 0;
        if (buffered && ranges > 0) {
          totalSpan = buffered.end(ranges - 1) - buffered.start(0);
          for (let i = 0; i < ranges; i++) {
            const s = buffered.start(i);
            const e = buffered.end(i);
            if (s - 0.0834 <= ct && ct <= e + 0.0834) currentRangeEnd = e;
          }
        }
        const liveLag = buffered && ranges > 0
          ? buffered.end(ranges - 1) - ct
          : 0;
        const playLag = currentRangeEnd > 0 ? currentRangeEnd - ct : 0;
        // CPU% rough estimate: time spent per second of frames (assume ~60fps stream).
        const cpuPct = (sumTotalMs / 1000) * 100;
        console.log(
          `[MseStreamVideoPlayer] over ${n} segments: ` +
          `b64decode=${avgDecode.toFixed(2)}ms queue=${avgQueue.toFixed(2)}ms ` +
          `chase=${avgChase.toFixed(2)}ms total=${avgTotal.toFixed(2)}ms (max=${maxTotalMs.toFixed(2)}ms) | ` +
          `avg seg ${avgKb.toFixed(1)}KB | ranges=${ranges} span=${totalSpan.toFixed(2)}s ` +
          `liveLag=${liveLag.toFixed(3)}s playLag=${playLag.toFixed(3)}s rate=${(v?.playbackRate ?? 1).toFixed(2)} | ` +
          `JS handler CPU≈${cpuPct.toFixed(2)}%`
        );
        timingSamples = 0;
        sumDecodeMs = 0;
        sumQueueMs = 0;
        sumChaseMs = 0;
        sumTotalMs = 0;
        sumBytes = 0;
        maxTotalMs = 0;
      }
    }).then((fn) => {
      if (stopped) fn(); else unlistenSegment = fn;
    });

    // Tell the Rust receiver bridge to drop any persisted muxer state
    // for this streamer. The bridge keys muxer state by username and
    // keeps it across watch sessions; without this reset, a stop +
    // rewatch never re-emits an init segment (the codec hasn't
    // changed), the new SourceBuffer is never created, and we sit on
    // the spinner forever.
    invoke("reset_mse_state", { targetUsername: streamerUsername }).catch(() => {});

    // Ask the streamer for a keyframe so the SourceBuffer can configure
    // immediately rather than waiting for the natural keyframe interval.
    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

    // Auto-play. Browsers may block silent autoplay until first interaction
    // but our video has no audio track in this path, so it usually works.
    video.muted = true;
    video.autoplay = true;
    video.playsInline = true;
    video.play().catch(() => {});

    return () => {
      stopped = true;
      window.clearInterval(stallPoll);
      unlistenInit?.();
      unlistenSegment?.();
      try {
        if (mediaSource && mediaSource.readyState === "open") {
          mediaSource.endOfStream();
        }
      } catch {}
      video.removeAttribute("src");
      video.load();
    };
  }, [streamerUsername]);

  return (
    <div className="relative h-full w-full">
      {!hasFirstFrame && !error && (
        <div className="absolute inset-0 flex items-center justify-center">
          <svg className="h-8 w-8 animate-spin text-[#00bfff]" viewBox="0 0 24 24" fill="none">
            <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" />
            <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
          </svg>
        </div>
      )}
      {error && (
        <div className="absolute inset-0 flex items-center justify-center p-4 text-center text-[12px] text-error">
          {error}
        </div>
      )}
      <video
        ref={videoRef}
        className={`${className ?? "h-full w-full object-contain"} ${hasFirstFrame ? "" : "opacity-0"}`}
      />
    </div>
  );
}

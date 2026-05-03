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
    // The previous implementation seeked to (liveEdge - 0.15s) on every
    // updateend whenever lag > 0.4s. Each seek forces WebKit to flush
    // its decoder and re-decode from the previous keyframe, which is
    // what was causing the "3 seconds of video, 5 seconds of freeze"
    // pattern: the chase fired, decoder flushed, video stalled while
    // re-decoding, played for a bit, chase fired again, etc.
    //
    // New strategy:
    //   * For moderate lag (0.3s..2s): nudge playbackRate up to 1.05
    //     so the playhead catches up smoothly. Decoder doesn't flush.
    //   * For severe lag (>2s — happens on initial buffering or
    //     after a long stall): hard seek as a last resort.
    //   * For low lag (<0.15s): playbackRate back to 1.0.
    //
    // Eviction is throttled to once per second so SourceBuffer.remove
    // doesn't fire on every append (each remove blocks subsequent
    // appends until updateend fires, contributing to backpressure).
    const TARGET_LATENCY_S = 0.15;
    const CATCHUP_LAG_S = 0.30;
    const SEEK_LAG_S = 2.0;
    const RETENTION_S = 4;
    const EVICT_INTERVAL_MS = 1000;
    let lastEvictMs = 0;

    function chaseLiveEdge() {
      if (!sourceBuffer || video!.buffered.length === 0) return;
      const liveEdge = video!.buffered.end(video!.buffered.length - 1);
      const lag = liveEdge - video!.currentTime;
      if (lag > SEEK_LAG_S) {
        video!.currentTime = Math.max(0, liveEdge - TARGET_LATENCY_S);
        video!.playbackRate = 1.0;
      } else if (lag > CATCHUP_LAG_S) {
        if (video!.playbackRate !== 1.05) video!.playbackRate = 1.05;
      } else if (lag < TARGET_LATENCY_S) {
        if (video!.playbackRate !== 1.0) video!.playbackRate = 1.0;
      }

      // Throttled eviction. Skip when the SourceBuffer is busy — calling
      // remove() while updating throws, and removes themselves are slow
      // enough to matter at 60fps.
      const nowMs = performance.now();
      if (nowMs - lastEvictMs >= EVICT_INTERVAL_MS && !sourceBuffer.updating) {
        const bufStart = video!.buffered.start(0);
        const evictTo = video!.currentTime - RETENTION_S;
        if (evictTo > bufStart + 0.5) {
          try {
            sourceBuffer.remove(bufStart, evictTo);
            lastEvictMs = nowMs;
          } catch {}
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
    // bricked the player until the next mount; now we tear down and
    // rebuild on the next init segment.
    let pendingRebuildMime: string | null = null;
    function recoverFromError(reason: string) {
      console.warn(`[MseStreamVideoPlayer] recovering from: ${reason}`);
      if (sourceBuffer) {
        try { mediaSource?.removeSourceBuffer(sourceBuffer); } catch {}
        sourceBuffer = null;
      }
      // Drop queued segments — they're tied to the dead SourceBuffer's
      // timeline. Wait for the next init segment to rebuild.
      queue.length = 0;
      pendingRebuildMime = null;
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

    // Diagnostic: video element stall events. WebKit fires `waiting` when
    // the playhead has nothing to play; `stalled` when no progress for a
    // while. Logging these lets us tell whether a freeze is "no data
    // available" (network/bridge issue) or "decoder backed up" (player
    // issue).
    video.addEventListener("waiting", () => {
      const buf = video.buffered;
      const bufEnd = buf.length > 0 ? buf.end(buf.length - 1) : 0;
      console.warn(`[MseStreamVideoPlayer] waiting at currentTime=${video.currentTime.toFixed(3)}s, bufferedEnd=${bufEnd.toFixed(3)}s, lag=${(bufEnd - video.currentTime).toFixed(3)}s`);
    });
    video.addEventListener("stalled", () => {
      console.warn(`[MseStreamVideoPlayer] stalled at currentTime=${video.currentTime.toFixed(3)}s`);
    });

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

      if (timingSamples >= 60) {
        const n = timingSamples;
        const avgDecode = sumDecodeMs / n;
        const avgQueue = sumQueueMs / n;
        const avgChase = sumChaseMs / n;
        const avgTotal = sumTotalMs / n;
        const avgKb = sumBytes / n / 1024;
        const buffered = videoRef.current?.buffered;
        const lag = buffered && buffered.length > 0
          ? buffered.end(buffered.length - 1) - (videoRef.current?.currentTime ?? 0)
          : 0;
        const bufferedSec = buffered && buffered.length > 0
          ? buffered.end(buffered.length - 1) - buffered.start(0)
          : 0;
        // CPU% rough estimate: time spent per second of frames (assume ~60fps stream).
        const cpuPct = (sumTotalMs / 1000) * 100;
        console.log(
          `[MseStreamVideoPlayer] over ${n} segments: ` +
          `b64decode=${avgDecode.toFixed(2)}ms queue=${avgQueue.toFixed(2)}ms ` +
          `chase=${avgChase.toFixed(2)}ms total=${avgTotal.toFixed(2)}ms (max=${maxTotalMs.toFixed(2)}ms) | ` +
          `avg seg ${avgKb.toFixed(1)}KB | buffered=${bufferedSec.toFixed(2)}s lag=${lag.toFixed(3)}s | ` +
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

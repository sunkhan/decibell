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

    // Live-edge chasing: HTML5 `<video>` defaults to playing from
    // currentTime=0 at 1.0x. With a live MSE source we want the
    // playhead near the latest buffered timestamp instead. Without
    // chasing, every frame we append into the future grows the gap
    // between playhead and live edge → lag accumulates linearly.
    //
    // Strategy: after every successful append, if the playhead is
    // more than TARGET_LATENCY_S behind the latest buffered range,
    // snap it to (liveEdge - TARGET_LATENCY_S). Small overshoot keeps
    // a tiny jitter cushion; aggressive enough to feel real-time but
    // not so aggressive that single-packet jitter causes stalls.
    const TARGET_LATENCY_S = 0.15;
    const SEEK_THRESHOLD_S = 0.4;
    // Buffer eviction: drop everything before (currentTime - RETENTION).
    // Without this, an hour-long stream accumulates ~hours of frames in
    // browser memory (~hundreds of MB at 1080p H.264).
    const RETENTION_S = 4;

    function chaseLiveEdge() {
      if (!sourceBuffer || sourceBuffer.updating) return;
      if (video!.buffered.length === 0) return;
      const liveEdge = video!.buffered.end(video!.buffered.length - 1);
      const lag = liveEdge - video!.currentTime;
      if (lag > SEEK_THRESHOLD_S) {
        video!.currentTime = Math.max(0, liveEdge - TARGET_LATENCY_S);
      }
      // Evict stale buffer ranges so memory doesn't grow without bound.
      const bufStart = video!.buffered.start(0);
      const evictTo = video!.currentTime - RETENTION_S;
      if (evictTo > bufStart + 0.5) {
        try { sourceBuffer.remove(bufStart, evictTo); } catch {}
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
        sb.addEventListener("error", (e) => {
          console.error("[MseStreamVideoPlayer] SourceBuffer error:", e);
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
        // Nothing to do here — setupSourceBuffer happens whenever a
        // pending mime arrives. If we got an init before sourceopen
        // fired (race), apply it now.
        if (pendingMime) {
          const mime = pendingMime;
          pendingMime = null;
          setupSourceBuffer(mime);
        }
      });
      return ms;
    }

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

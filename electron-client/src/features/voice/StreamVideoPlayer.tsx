import { useEffect, useRef, useCallback, useState } from "react";
import { invoke } from "../../lib/ipc";
import { VideoCodec } from "../../types";
import { videoCodecToWebCodecsString } from "../../utils/codecMap";
import { useAuthStore } from "../../stores/authStore";
import {
  subscribeLocalFrames,
  activeStreamCapture,
} from "./streaming/StreamCapture";
import { isNativeEncodeActive } from "../../utils/encoderProbe";
import { useStreamStatsStore } from "./streamStats";

function codecLabel(codec: VideoCodec): string {
  const s = videoCodecToWebCodecsString(codec);
  if (s.startsWith("avc")) return "H.264";
  if (s.startsWith("hev") || s.startsWith("hvc")) return "HEVC";
  if (s.startsWith("av01")) return "AV1";
  if (s.startsWith("vp09") || s.startsWith("vp9")) return "VP9";
  return s;
}

interface Props {
  streamerUsername: string;
  className?: string;
}

// PR7c: encoded video frames arrive as native Uint8Array via the
// dedicated stream bus, no JSON or base64 round-trip. Shape matches
// `events::StreamFrame` on the native side.
interface StreamFrame {
  username: string;
  codec: number;
  keyframe: boolean;
  timestamp: number;
  data: Uint8Array;
  description: Uint8Array | null;
}

/// WebCodecs-only StreamVideoPlayer — single path for Linux + Windows in
/// the Electron port. The Linux MseStreamVideoPlayer (fMP4 + WebKitGTK
/// MSE) is gone for good — Chromium WebCodecs handles every codec we
/// care about with consistent per-frame semantics.
// Defensive: on some Castlabs Electron 33 / Windows configurations the
// `VideoDecoder` global isn't exposed (Media Foundation init fails
// partway and Chromium drops the WebCodecs API surface). Module-level
// constant: it can't change within a process lifetime, and hoisting it
// lets the component keep an unconditional hook order (the old early
// return sat above the hooks — stable in practice, still a
// rules-of-hooks violation).
const DECODER_AVAILABLE = typeof VideoDecoder !== "undefined";

function u8Equals(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

export default function StreamVideoPlayer({ streamerUsername, className }: Props) {
  const ownUsername = useAuthStore((s) => s.username);
  const isOwnStream = ownUsername !== null && streamerUsername === ownUsername;
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  const descriptionRef = useRef<ArrayBuffer | null>(null);
  const needsKeyframeRef = useRef(true);
  const lastKeyframeRequestRef = useRef(0);
  const [hasFirstFrame, setHasFirstFrame] = useState(false);

  // Active codec for the stream. Updated when stream_frame events
  // carry a different codec byte mid-stream (Plan C codec swap).
  const activeCodecRef = useRef<VideoCodec>(VideoCodec.H264_HW);

  // Decoder is configured lazily on the first frame (once its codec is known)
  // rather than speculatively as H.264 at creation.
  const configuredRef = useRef(false);
  // Paused while the window is hidden/minimized — WebCodecs decode is not
  // rAF-throttled, so it would otherwise run at full rate off-screen.
  const hiddenRef = useRef(
    typeof document !== "undefined" && document.hidden,
  );
  // On-screen size of the canvas (CSS px), tracked via ResizeObserver so the
  // per-frame output callback never reads layout. Caps the canvas backing store
  // so a 4K stream doesn't keep a 4K buffer to paint into a 320×180 mini player.
  const displaySizeRef = useRef({ w: 0, h: 0 });
  // Stats counters, published to useStreamStatsStore on an interval.
  const framesDecodedRef = useRef(0);
  const framesDroppedRef = useRef(0);
  const srcDimsRef = useRef({ w: 0, h: 0 });

  // Ask the streamer for a fresh IDR, throttled to 1/s. Renderer-side
  // frame drops (decoder-queue backpressure, decoder errors, frames
  // shed by the bounded native→JS queue) leave holes the native PLI
  // machinery can't see — it only fires on *reassembly* gaps — so
  // without this, a renderer drop froze the picture until the next
  // natural GOP keyframe. Route by path: remote streams get the
  // wire-level UdpKeyframeRequest; own-stream native self-preview
  // signals the native encoder; own-stream renderer encode is direct.
  const requestKeyframe = useCallback(() => {
    const now = performance.now();
    if (now - lastKeyframeRequestRef.current < 1000) return;
    lastKeyframeRequestRef.current = now;
    if (!isOwnStream) {
      invoke("request_stream_keyframe", { username: streamerUsername }).catch(
        () => {},
      );
    } else if (isNativeEncodeActive()) {
      invoke("force_keyframe", {}).catch(() => {});
    } else {
      activeStreamCapture()?.forceKeyframe();
    }
  }, [isOwnStream, streamerUsername]);

  const configureDecoder = useCallback(
    (decoder: VideoDecoder, description?: ArrayBuffer) => {
      // Don't pass hardwareAcceleration here. In isConfigSupported it's
      // a hard constraint (returns supported:false on platforms without
      // hardware decode, even when software decode works fine — same
      // gotcha as the encoder side). In configure() prefer-hardware can
      // also fail outright on Linux + NVIDIA without nvidia-vaapi-driver
      // because Chromium can't allocate any decoder. Letting Chromium
      // pick lets it transparently fall back to software.
      const config: VideoDecoderConfig = {
        codec: videoCodecToWebCodecsString(activeCodecRef.current),
      };
      if (description) {
        config.description = description;
      }
      // No isConfigSupported() here: it was async work on the hot path (fired on
      // every configure/reconfigure/error-recovery) purely to console.log, and
      // the config codec is already known-supported from decoderProbe.
      try {
        decoder.configure(config);
      } catch (e) {
        console.error("[StreamVideoPlayer] Configure error:", e);
      }
    },
    [],
  );

  const handleDecoderError = useCallback(
    (e: DOMException) => {
      console.error("[StreamVideoPlayer] Decoder error:", e);
      needsKeyframeRef.current = true;
      // A decoder error usually means a broken reference chain — a
      // dropped delta somewhere the native PLI machinery couldn't see.
      // Reconfigure AND ask for a keyframe; waiting for the natural
      // GOP boundary froze the picture for seconds.
      requestKeyframe();
      if (decoderRef.current && decoderRef.current.state !== "closed") {
        decoderRef.current.reset();
        configureDecoder(
          decoderRef.current,
          descriptionRef.current ?? undefined,
        );
      }
    },
    [configureDecoder, requestKeyframe],
  );

  useEffect(() => {
    if (!DECODER_AVAILABLE) return;
    const canvas = canvasRef.current;
    if (!canvas) return;

    // Opaque (video has no transparency) + desynchronized skips per-frame alpha
    // compositing against the page and cuts present latency.
    ctxRef.current = canvas.getContext("2d", {
      alpha: false,
      desynchronized: true,
    });

    let firstFrameSignalled = false;

    let decoder: VideoDecoder | null = null;
    {
      decoder = new VideoDecoder({
        output: (frame: VideoFrame) => {
          try {
            framesDecodedRef.current++;
            // visibleRect (not display*/coded*): Chromium's WebCodecs HEVC
            // reports displayWidth = coded / SubWidthC for streams with no
            // conformance window (half the picture), and coded includes crop
            // padding (a 1080p HEVC coded 1920×1088 gained an 8px stretch).
            // visibleRect is correct in both cases.
            const vr = frame.visibleRect;
            const srcX = vr ? vr.x : 0;
            const srcY = vr ? vr.y : 0;
            const srcW = vr ? vr.width : frame.codedWidth;
            const srcH = vr ? vr.height : frame.codedHeight;
            srcDimsRef.current = { w: srcW, h: srcH };
            const ctx = ctxRef.current;
            // Skip painting when the canvas isn't in the document (the persistent
            // host is parked/warm off-screen, e.g. on the streams grid) — keep
            // decoding so a return to view is seamless, but don't blit.
            if (ctx && canvas && canvas.isConnected) {
              // Cap the canvas backing store to the on-screen size (× dpr),
              // never upscaling: a 4K stream painted into the 320×180 mini keeps
              // a 320×180 buffer, not a 3840×2160 one. Falls back to source res
              // until the display size has been observed.
              const dpr = window.devicePixelRatio || 1;
              const dw = displaySizeRef.current.w;
              const dh = displaySizeRef.current.h;
              let dstW = srcW;
              let dstH = srcH;
              if (dw > 0 && dh > 0) {
                const scale = Math.min(1, (dw * dpr) / srcW, (dh * dpr) / srcH);
                dstW = Math.max(1, Math.round(srcW * scale));
                dstH = Math.max(1, Math.round(srcH * scale));
              }
              if (canvas.width !== dstW || canvas.height !== dstH) {
                canvas.width = dstW;
                canvas.height = dstH;
              }
              ctx.drawImage(frame, srcX, srcY, srcW, srcH, 0, 0, dstW, dstH);
            }
          } catch (err) {
            // drawImage can throw (detached canvas / invalid state). Swallow
            // it so the VideoFrame is still closed below — an unclosed frame
            // pins a scarce GPU resource and stalls the decoder once the pool
            // drains, and an exception here escapes the WebCodecs callback.
            console.error("[StreamVideoPlayer] draw error:", err);
          } finally {
            frame.close();
          }
          if (!firstFrameSignalled) {
            firstFrameSignalled = true;
            setHasFirstFrame(true);
          }
        },
        error: handleDecoderError,
      });
      // Configure lazily on the first frame once its codec is known, rather than
      // speculatively as H.264 here (which cost an extra reset+reconfigure for
      // AV1/HEVC — see the first-frame branch in handleFrame).
      decoderRef.current = decoder;
      needsKeyframeRef.current = true;
    }

    // Per-frame handler shared between the wire and self-preview paths.
    // `data` and `description` are read but never mutated — the wire
    // path's structured-clone copy and the local path's shared
    // reference are both safe.
    const handleFrame = (
      data: Uint8Array,
      timestamp: number,
      keyframe: boolean,
      description: Uint8Array | null,
      codec: number,
    ) => {
      if (!decoder || decoder.state === "closed") return;

      // Paused while the window is hidden/minimized: don't spend decode on
      // frames nobody can see. On becoming visible the listener requests a
      // keyframe and we resume from it.
      if (hiddenRef.current) {
        needsKeyframeRef.current = true;
        return;
      }

      const incomingCodec = (codec ?? VideoCodec.H264_HW) as VideoCodec;
      if (!configuredRef.current) {
        // First frame: do the initial configure now that the codec is known.
        // No reset — the decoder is fresh.
        activeCodecRef.current = incomingCodec;
        descriptionRef.current = null;
        needsKeyframeRef.current = true;
        configuredRef.current = true;
        configureDecoder(decoder);
      } else if (incomingCodec !== activeCodecRef.current) {
        console.log(
          "[StreamVideoPlayer] codec change",
          activeCodecRef.current,
          "→",
          incomingCodec,
        );
        activeCodecRef.current = incomingCodec;
        descriptionRef.current = null;
        needsKeyframeRef.current = true;
        // Reconfigure with the new codec_string immediately. Codecs whose
        // Chromium encoder emits self-contained frames (AV1 inline seq header,
        // H.264 Annex B) never send a description, so waiting for the
        // description branch would leave the decoder on the old codec_string.
        decoder.reset();
        configureDecoder(decoder);
      }

      if (keyframe && description) {
        // Reconfigure when the description first arrives OR changes mid-stream.
        // A same-codec parameter-set change (e.g. a native HEVC/AV1 resolution
        // change emits new VPS/SPS/PPS) ships a different description on a later
        // keyframe; the old `!descriptionRef.current` guard honored only the
        // first one, so decode corrupted/errored until a codec swap. Compare
        // bytes and reconfigure on any change (keyframe-gated).
        const stored = descriptionRef.current;
        const changed =
          !stored ||
          stored.byteLength !== description.byteLength ||
          !u8Equals(new Uint8Array(stored), description);
        if (changed) {
          // Copy out of the IPC-shared buffer into a fresh ArrayBuffer the
          // decoder can hang onto — Electron may recycle the structured-clone
          // buffer once this handler returns.
          const descCopy = new Uint8Array(description);
          descriptionRef.current = descCopy.buffer;
          decoder.reset();
          configureDecoder(decoder, descCopy.buffer);
          needsKeyframeRef.current = false;
        }
      }

      if (needsKeyframeRef.current && !keyframe) return;
      if (keyframe) needsKeyframeRef.current = false;

      // Backpressure: if the decoder queue is backing up, drop deltas
      // and gate on the next keyframe (a dropped delta breaks the
      // reference chain — decoding past it smears until an IDR).
      //
      // This queue check is the only pacing left. The old wall-clock
      // "lag" logic compared arrival time against timestamps the
      // receive thread synthesizes as frame_id × 33.3ms — a hardcoded
      // 30fps clock. Any stream effectively below 30fps (damage-driven
      // window capture of static content, encoder drops) fell "behind"
      // that fictional schedule at ~33ms per frame, and once past
      // 500ms the player dropped every delta: the watcher degraded to
      // a keyframe-per-GOP slideshow exactly when the content was
      // static. Above 30fps the check never engaged at all. Real
      // stalls are now handled where they occur: the native→JS queue
      // is bounded (drops shed load before latency accumulates) and
      // the decoder queue check here covers decode overload.
      if (decoder.decodeQueueSize > 3 && !keyframe) {
        needsKeyframeRef.current = true;
        framesDroppedRef.current++;
        // The hole this drop leaves is invisible to the native PLI
        // machinery — ask the streamer for a fresh IDR (throttled).
        requestKeyframe();
        return;
      }

      try {
        const chunk = new EncodedVideoChunk({
          type: keyframe ? "key" : "delta",
          timestamp,
          data,
        });
        decoder.decode(chunk);
      } catch (e) {
        console.error("[StreamVideoPlayer] Decode error:", e);
      }
    };

    // For our own stream, subscribe directly to the local encoder's
    // output — no wire round-trip. For everyone else, subscribe to the
    // binary stream bus and filter by username (the broadcaster fans
    // out every active watcher's frames; multiple players coexist by
    // each filtering independently).
    //
    // Native pipelines (Windows always; Linux when a HW encoder was used)
    // are the exception: there is no renderer-side WebCodecs encoder, so
    // subscribeLocalFrames is silent. The native encoder thread fans into
    // the same stream_frame TSFN remote streams use (keyed by our local
    // username), so own-stream self-preview subscribes through that path
    // too. (Non-native Linux/macOS still paint from the renderer encoder's
    // local fan-out via subscribeLocalFrames.)
    const isNative = isNativeEncodeActive();
    let unsubscribe: () => void;
    if (isOwnStream && !isNative) {
      unsubscribe = subscribeLocalFrames((frame) => {
        handleFrame(
          frame.data,
          frame.timestamp,
          frame.keyframe,
          frame.description,
          frame.codec,
        );
      });
      // Mid-stream join: ask the local encoder to emit an immediate
      // IDR so the decoder doesn't have to sit on a black canvas
      // until the encoder's next natural GOP boundary (which can be
      // several seconds away with OpenH264's default cadence).
      activeStreamCapture()?.forceKeyframe();
    } else {
      // Per-username subscribe: only frames for *this* streamer wake
      // the callback. The preload bridge handles the dispatch via Map
      // lookup so other streamers' frames don't run through this
      // closure at all.
      unsubscribe = window.decibell.streamFrames.subscribe(
        streamerUsername,
        (frame: StreamFrame) => {
          handleFrame(
            frame.data,
            frame.timestamp,
            frame.keyframe,
            frame.description,
            frame.codec,
          );
        },
      );
      // Wire-side mid-stream join keyframe request (B4) is still
      // deliberately deferred, but the command for it now exists —
      // `request_stream_keyframe`, used by requestKeyframe() above for
      // drop recovery. Landing B4 is one call here when subscribing.
      //
      // Mid-stream join: ask for an immediate keyframe so the decoder paints
      // quickly instead of sitting on a black canvas until the next natural GOP
      // (seconds away with typical cadences). This matters now that the player
      // re-mounts on view changes (e.g. popping into the floating mini-player).
      // requestKeyframe() routes correctly: remote → request_stream_keyframe;
      // own native self-preview → force_keyframe.
      requestKeyframe();
    }

    // Pause/resume decode on window visibility (WebCodecs decode isn't
    // rAF-throttled, so it would run at full rate while minimized/occluded).
    const onVisibility = () => {
      const hidden = document.hidden;
      hiddenRef.current = hidden;
      if (!hidden) {
        // Resuming: the reference chain broke while paused — request a fresh
        // keyframe and gate deltas until it arrives.
        needsKeyframeRef.current = true;
        requestKeyframe();
      }
    };
    document.addEventListener("visibilitychange", onVisibility);

    // Track the canvas's on-screen size for backing-store capping without
    // reading layout in the per-frame output callback.
    const ro = new ResizeObserver((entries) => {
      const cr = entries[0]?.contentRect;
      if (cr) displaySizeRef.current = { w: cr.width, h: cr.height };
    });
    ro.observe(canvas);

    // Publish decode stats ~2/s for the stats overlay.
    let lastDecoded = 0;
    let lastSample = performance.now();
    const statsTimer = window.setInterval(() => {
      const now = performance.now();
      const dt = (now - lastSample) / 1000;
      const decoded = framesDecodedRef.current;
      const fps = dt > 0 ? (decoded - lastDecoded) / dt : 0;
      lastDecoded = decoded;
      lastSample = now;
      useStreamStatsStore.getState().publishStats(streamerUsername, {
        codecLabel: codecLabel(activeCodecRef.current),
        width: srcDimsRef.current.w,
        height: srcDimsRef.current.h,
        fps: Math.round(fps),
        queue: decoder?.decodeQueueSize ?? 0,
        dropped: framesDroppedRef.current,
      });
    }, 500);

    return () => {
      unsubscribe();
      document.removeEventListener("visibilitychange", onVisibility);
      ro.disconnect();
      window.clearInterval(statsTimer);
      useStreamStatsStore.getState().clearStats(streamerUsername);
      if (decoder && decoder.state !== "closed") {
        decoder.close();
      }
      decoderRef.current = null;
      descriptionRef.current = null;
      configuredRef.current = false;
      setHasFirstFrame(false);
    };
  }, [streamerUsername, isOwnStream, handleDecoderError, configureDecoder, requestKeyframe]);

  if (!DECODER_AVAILABLE) {
    return (
      <div
        className={`flex h-full w-full items-center justify-center bg-bg-darkest text-center text-[12px] text-text-muted ${className ?? ""}`}
      >
        Stream preview unavailable on this system
      </div>
    );
  }

  return (
    <div className="relative h-full w-full">
      {!hasFirstFrame && (
        <div className="absolute inset-0 flex items-center justify-center">
          <svg
            className="h-8 w-8 animate-spin text-accent"
            viewBox="0 0 24 24"
            fill="none"
          >
            <circle
              className="opacity-25"
              cx="12"
              cy="12"
              r="10"
              stroke="currentColor"
              strokeWidth="3"
            />
            <path
              className="opacity-75"
              fill="currentColor"
              d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
            />
          </svg>
        </div>
      )}
      <canvas
        ref={canvasRef}
        className={`${className ?? "h-full w-full object-contain"} ${hasFirstFrame ? "" : "opacity-0"}`}
      />
    </div>
  );
}

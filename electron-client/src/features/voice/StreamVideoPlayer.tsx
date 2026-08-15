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
      VideoDecoder.isConfigSupported(config)
        .then((res) => {
          if (!res.supported) {
            console.error(
              "[StreamVideoPlayer] WebCodecs reports codec NOT supported:",
              config.codec,
              "— full check:",
              res,
            );
          } else {
            console.log("[StreamVideoPlayer] WebCodecs supports", config.codec);
          }
        })
        .catch((e) =>
          console.error("[StreamVideoPlayer] isConfigSupported threw:", e),
        );
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

    ctxRef.current = canvas.getContext("2d");

    let firstFrameSignalled = false;
    let dimsLogged = false;

    let decoder: VideoDecoder | null = null;
    {
      decoder = new VideoDecoder({
        output: (frame: VideoFrame) => {
          const ctx = ctxRef.current;
          if (ctx && canvas) {
            // Canvas is sized to the *visible* rect, and the visible
            // region is drawn 1:1 into it.
            //
            // Not displayWidth/displayHeight: Chromium's WebCodecs HEVC
            // implementation reports displayWidth = coded / SubWidthC
            // for streams with no conformance window — half the actual
            // picture. Not codedWidth/codedHeight either: stretching
            // visible → coded distorted legitimately-cropped streams
            // (1080p HEVC coded as 1920×1088 gained an 8px vertical
            // stretch). visibleRect is correct in both cases — it
            // equals coded when there's no crop, including the buggy-
            // display HEVC case, and equals the true picture when
            // there is one.
            const vr = frame.visibleRect;
            const srcX = vr ? vr.x : 0;
            const srcY = vr ? vr.y : 0;
            const srcW = vr ? vr.width : frame.codedWidth;
            const srcH = vr ? vr.height : frame.codedHeight;
            if (
              !dimsLogged ||
              canvas.width !== srcW ||
              canvas.height !== srcH
            ) {
              console.log(
                "[StreamVideoPlayer] frame dims",
                "coded=",
                frame.codedWidth,
                "x",
                frame.codedHeight,
                "display=",
                frame.displayWidth,
                "x",
                frame.displayHeight,
                "visible=",
                srcX,
                srcY,
                srcW,
                "x",
                srcH,
              );
              dimsLogged = true;
            }
            if (canvas.width !== srcW || canvas.height !== srcH) {
              canvas.width = srcW;
              canvas.height = srcH;
            }
            ctx.drawImage(frame, srcX, srcY, srcW, srcH, 0, 0, srcW, srcH);
          }
          frame.close();
          if (!firstFrameSignalled) {
            firstFrameSignalled = true;
            setHasFirstFrame(true);
          }
        },
        error: handleDecoderError,
      });
      configureDecoder(decoder);
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

      const incomingCodec = (codec ?? VideoCodec.H264_HW) as VideoCodec;
      if (incomingCodec !== activeCodecRef.current) {
        console.log(
          "[StreamVideoPlayer] codec change",
          activeCodecRef.current,
          "→",
          incomingCodec,
        );
        activeCodecRef.current = incomingCodec;
        descriptionRef.current = null;
        needsKeyframeRef.current = true;
        // Reconfigure the decoder with the new codec_string immediately.
        // For codecs whose Chromium WebCodecs encoder emits frames in a
        // self-contained format (AV1's sequence header inline in the
        // bitstream, H.264 in Annex B), no description ever arrives —
        // waiting for the description-bearing branch below would leave
        // the decoder stuck on the previous codec_string. For codecs
        // that DO emit a description (HEVC, AV1 in some builds), the
        // description branch below will reconfigure a second time.
        decoder.reset();
        configureDecoder(decoder);
      }

      if (keyframe && description && !descriptionRef.current) {
        // Copy out of the IPC-shared buffer into a fresh ArrayBuffer
        // the decoder can hang onto — Electron may recycle the
        // structured-clone buffer once this handler returns.
        const descCopy = new Uint8Array(description);
        descriptionRef.current = descCopy.buffer;
        decoder.reset();
        configureDecoder(decoder, descCopy.buffer);
        needsKeyframeRef.current = false;
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
      // Native self-preview (Windows + Linux) takes the wire-shaped path
      // too (isOwnStream && isNative falls into this branch). For that we
      // force an immediate keyframe via the force_keyframe IPC, which
      // signals the native encoder thread's AtomicBool — so the decoder
      // doesn't sit on a spinner until the next ~2s GOP boundary.
      if (isOwnStream && isNative) {
        invoke("force_keyframe", {}).catch(() => {});
      }
    }

    return () => {
      unsubscribe();
      if (decoder && decoder.state !== "closed") {
        decoder.close();
      }
      decoderRef.current = null;
      descriptionRef.current = null;
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

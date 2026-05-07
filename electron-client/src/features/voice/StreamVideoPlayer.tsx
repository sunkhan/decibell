import { useEffect, useRef, useCallback, useState } from "react";
import { invoke } from "../../lib/ipc";
import { VideoCodec } from "../../types";
import { videoCodecToWebCodecsString } from "../../utils/codecMap";

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
export default function StreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  const descriptionRef = useRef<ArrayBuffer | null>(null);
  const needsKeyframeRef = useRef(true);
  const [hasFirstFrame, setHasFirstFrame] = useState(false);

  // Active codec for the stream. Updated when stream_frame events
  // carry a different codec byte mid-stream (Plan C codec swap).
  const activeCodecRef = useRef<VideoCodec>(VideoCodec.H264_HW);

  const configureDecoder = useCallback(
    (decoder: VideoDecoder, description?: ArrayBuffer) => {
      const config: VideoDecoderConfig = {
        codec: videoCodecToWebCodecsString(activeCodecRef.current),
        hardwareAcceleration: "prefer-hardware",
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
      // viewer-side: no-op; the natural PLI from video_recv_thread will
      // ask the streamer for a keyframe shortly. For self-preview this
      // forces an immediate IDR.
      invoke("force_keyframe").catch(() => {});
      if (decoderRef.current && decoderRef.current.state !== "closed") {
        decoderRef.current.reset();
        configureDecoder(
          decoderRef.current,
          descriptionRef.current ?? undefined,
        );
      }
    },
    [configureDecoder],
  );

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    ctxRef.current = canvas.getContext("2d");

    let firstRemoteTs = -1;
    let firstLocalTs = -1;
    let firstFrameSignalled = false;
    let dimsLogged = false;

    let decoder: VideoDecoder | null = null;
    {
      decoder = new VideoDecoder({
        output: (frame: VideoFrame) => {
          const ctx = ctxRef.current;
          if (ctx && canvas) {
            // Use codedWidth/codedHeight rather than displayWidth/displayHeight.
            // Chromium's WebCodecs HEVC implementation reports displayWidth =
            // coded / SubWidthC for streams with no conformance window —
            // half the actual picture size. Use visibleRect as the source
            // rect and scale up to codedWidth × codedHeight. For H.264 / AV1
            // the visible region equals the coded region, so this is a no-op.
            const codedW = frame.codedWidth;
            const codedH = frame.codedHeight;
            const vr = frame.visibleRect;
            const srcX = vr ? vr.x : 0;
            const srcY = vr ? vr.y : 0;
            const srcW = vr ? vr.width : codedW;
            const srcH = vr ? vr.height : codedH;
            if (
              !dimsLogged ||
              canvas.width !== codedW ||
              canvas.height !== codedH
            ) {
              console.log(
                "[StreamVideoPlayer] frame dims",
                "coded=",
                codedW,
                "x",
                codedH,
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
                "canvas was",
                canvas.width,
                "x",
                canvas.height,
                "rect",
                canvas.getBoundingClientRect().width.toFixed(0),
                "x",
                canvas.getBoundingClientRect().height.toFixed(0),
              );
              dimsLogged = true;
            }
            if (canvas.width !== codedW || canvas.height !== codedH) {
              canvas.width = codedW;
              canvas.height = codedH;
            }
            ctx.drawImage(
              frame,
              srcX,
              srcY,
              srcW,
              srcH,
              0,
              0,
              codedW,
              codedH,
            );
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

    invoke("force_keyframe").catch(() => {});

    // PR7c: subscribe to the binary stream bus (Uint8Array payloads,
    // no JSON parse, no base64 decode). The preload broadcaster fires
    // for every active watcher; we filter on `frame.username` to pick
    // out our target.
    const unsubscribe = window.decibell.streamFrames.subscribe((frame: StreamFrame) => {
      const { username, data, timestamp, keyframe, description, codec } = frame;
      if (username !== streamerUsername) return;

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
        invoke("force_keyframe").catch(() => {});
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
        firstRemoteTs = -1;
        firstLocalTs = -1;
      }

      if (needsKeyframeRef.current && !keyframe) return;
      if (keyframe) needsKeyframeRef.current = false;

      const nowUs = performance.now() * 1000;

      if (firstRemoteTs < 0) {
        firstRemoteTs = timestamp;
        firstLocalTs = nowUs;
      }

      const expectedLocalTs = firstLocalTs + (timestamp - firstRemoteTs);
      const lagUs = nowUs - expectedLocalTs;

      // If the decoder queue is backing up, drop non-keyframes to catch up.
      if (decoder.decodeQueueSize > 3 && !keyframe) {
        return;
      }

      // If this frame is more than 500ms behind wall-clock, drop it
      // (unless it's a keyframe — we need those to keep decoding).
      if (lagUs > 500_000 && !keyframe) {
        return;
      }

      // Re-sync the clock baseline if we're more than 2s behind.
      if (lagUs > 2_000_000 && keyframe) {
        firstRemoteTs = timestamp;
        firstLocalTs = nowUs;
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
    });

    return () => {
      unsubscribe();
      if (decoder && decoder.state !== "closed") {
        decoder.close();
      }
      decoderRef.current = null;
      descriptionRef.current = null;
      setHasFirstFrame(false);
    };
  }, [streamerUsername, handleDecoderError, configureDecoder]);

  return (
    <div className="relative h-full w-full">
      {!hasFirstFrame && (
        <div className="absolute inset-0 flex items-center justify-center">
          <svg
            className="h-8 w-8 animate-spin text-[#00bfff]"
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

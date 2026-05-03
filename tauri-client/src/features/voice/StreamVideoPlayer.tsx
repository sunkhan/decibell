import { useEffect, useRef, useCallback, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { VideoCodec } from "../../types";
import { videoCodecToWebCodecsString } from "../../utils/codecMap";
import LinuxStreamVideoPlayer from "./LinuxStreamVideoPlayer";
import MseStreamVideoPlayer from "./MseStreamVideoPlayer";

interface Props {
  streamerUsername: string;
  className?: string;
}

interface StreamFramePayload {
  username: string;
  // Always "h264" (or other WebCodecs-recognised codec) — the legacy
  // "jpeg" Linux path was removed in 0.5.5 in favour of the WebGL2 NV12
  // pull pipeline (see LinuxStreamVideoPlayer).
  format?: "h264";
  data: string; // base64-encoded frame data
  timestamp: number;
  keyframe: boolean;
  description: string | null; // base64-encoded codec description (avcC/hvcC/av1C)
  // Codec byte from the per-packet UdpVideoPacket header (Plan B Group 4).
  // Drives WebCodecs decoder configuration. Optional for back-compat with
  // self-preview events emitted before this field was added.
  codec?: number;
}

function base64ToBytes(b64: string): Uint8Array {
  const binaryStr = atob(b64);
  const bytes = new Uint8Array(binaryStr.length);
  for (let i = 0; i < binaryStr.length; i++) {
    bytes[i] = binaryStr.charCodeAt(i);
  }
  return bytes;
}

// Check if WebCodecs VideoDecoder is available (Chromium-based webviews only)
const hasWebCodecs = typeof VideoDecoder !== "undefined";
const isLinux = typeof navigator !== "undefined" && /Linux/i.test(navigator.userAgent);

export default function StreamVideoPlayer({ streamerUsername, className }: Props) {
  // Linux: MSE-backed `<video>` is the right pipeline. Encoded frames
  // travel as small fMP4 segments (KBs, not MBs), the browser decodes
  // them via WebKitGTK's GStreamer backend (hardware-accelerated where
  // available), and the compositor renders directly. Saves us the
  // huge NV12 IPC transfer + Rust decode + WebGL upload of the old
  // LinuxStreamVideoPlayer path.
  if (isLinux) {
    return (
      <MseStreamVideoPlayer
        streamerUsername={streamerUsername}
        className={className}
      />
    );
  }
  if (!hasWebCodecs) {
    // Some non-Linux webview without WebCodecs — fall back to the
    // WebGL2 NV12 path (kept around for this case).
    return (
      <LinuxStreamVideoPlayer
        streamerUsername={streamerUsername}
        className={className}
      />
    );
  }

  return (
    <WebCodecsStreamVideoPlayer
      streamerUsername={streamerUsername}
      className={className}
    />
  );
}

function WebCodecsStreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  const descriptionRef = useRef<ArrayBuffer | null>(null);
  const needsKeyframeRef = useRef(true);
  const [hasFirstFrame, setHasFirstFrame] = useState(false);

  // Codec for the stream this player is rendering. Updated when the per-packet
  // codec byte arrives in stream_frame events (Plan C will reconfigure the
  // decoder mid-stream when this changes). For Plan B, the codec is set once
  // from the first frame and stays put.
  const activeCodecRef = useRef<VideoCodec>(VideoCodec.H264_HW);

  const configureDecoder = useCallback((decoder: VideoDecoder, description?: ArrayBuffer) => {
    const config: VideoDecoderConfig = {
      codec: videoCodecToWebCodecsString(activeCodecRef.current),
      hardwareAcceleration: "prefer-hardware",
    };
    if (description) {
      config.description = description;
    }
    // Diagnostic: explicitly check support before configure so DevTools
    // shows "your webview can't decode this codec at all" vs. some other
    // decode failure. HEVC on Windows requires the Microsoft Store "HEVC
    // Video Extensions" — without it Chromium reports supported=false
    // for any hvc1.* / hev1.* string.
    VideoDecoder.isConfigSupported(config).then((res) => {
      if (!res.supported) {
        console.error(
          "[StreamVideoPlayer] WebCodecs reports codec NOT supported:",
          config.codec,
          "— full check:", res,
        );
      } else {
        console.log("[StreamVideoPlayer] WebCodecs supports", config.codec);
      }
    }).catch((e) => console.error("[StreamVideoPlayer] isConfigSupported threw:", e));
    try {
      decoder.configure(config);
    } catch (e) {
      console.error("[StreamVideoPlayer] Configure error:", e);
    }
  }, []);

  const handleDecoderError = useCallback((e: DOMException) => {
    console.error("[StreamVideoPlayer] Decoder error:", e);
    needsKeyframeRef.current = true;
    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});
    if (decoderRef.current && decoderRef.current.state !== "closed") {
      decoderRef.current.reset();
      configureDecoder(decoderRef.current, descriptionRef.current ?? undefined);
    }
  }, [streamerUsername, configureDecoder]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    ctxRef.current = canvas.getContext("2d");

    // Clock sync: map encoder timestamps to local wall-clock time.
    // firstRemoteTs/firstLocalTs establish the baseline; we drop frames
    // whose local-adjusted timestamp is too far behind wall-clock (i.e. stale).
    let firstRemoteTs = -1;
    let firstLocalTs = -1;

    let firstFrameSignalled = false;

    let decoder: VideoDecoder | null = null;
    let dimsLogged = false;
    {
      decoder = new VideoDecoder({
        output: (frame: VideoFrame) => {
          const ctx = ctxRef.current;
          if (ctx && canvas) {
            // Use codedWidth/codedHeight rather than displayWidth/displayHeight.
            // Chromium's WebCodecs HEVC implementation (verified against an
            // SPS with conformance_window_flag = 0) reports displayWidth =
            // coded / SubWidthC, i.e. half the actual picture size for 4:2:0
            // streams. The decoded picture data IS the full coded size; only
            // the display-dim metadata is wrong. Drawing the codedWidth ×
            // codedHeight source rectangle explicitly bypasses the bug.
            // Chromium's WebCodecs HEVC decoder mis-reports dims for streams
            // with no conformance window. Use visibleRect (the *valid pixel*
            // region as Chromium sees it) as the source rect, then scale up
            // to codedWidth × codedHeight on the canvas. For H.264 / AV1 the
            // valid region equals the coded region, so this scale is a no-op.
            const codedW = frame.codedWidth;
            const codedH = frame.codedHeight;
            const vr = frame.visibleRect;
            const srcX = vr ? vr.x : 0;
            const srcY = vr ? vr.y : 0;
            const srcW = vr ? vr.width : codedW;
            const srcH = vr ? vr.height : codedH;
            if (!dimsLogged
                || canvas.width !== codedW
                || canvas.height !== codedH) {
              console.log("[StreamVideoPlayer] frame dims",
                "coded=", codedW, "x", codedH,
                "display=", frame.displayWidth, "x", frame.displayHeight,
                "visible=", srcX, srcY, srcW, "x", srcH,
                "canvas was", canvas.width, "x", canvas.height,
                "rect", canvas.getBoundingClientRect().width.toFixed(0),
                "x", canvas.getBoundingClientRect().height.toFixed(0));
              dimsLogged = true;
            }
            if (canvas.width !== codedW || canvas.height !== codedH) {
              canvas.width = codedW;
              canvas.height = codedH;
            }
            // 9-arg drawImage: pull the visibleRect from the source, scale
            // it up to fill the codedWidth × codedHeight canvas. For non-
            // HEVC codecs this is identity (visibleRect == coded). For
            // HEVC where Chromium populates only the visibleRect with valid
            // pixels, this stretches that area to fill the full canvas.
            ctx.drawImage(frame, srcX, srcY, srcW, srcH, 0, 0, codedW, codedH);
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

    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

    const unlisten = listen<StreamFramePayload>("stream_frame", (event) => {
      const { username, data, timestamp, keyframe, description, codec } = event.payload;
      if (username !== streamerUsername) return;

      if (!decoder || decoder.state === "closed") return;

      // Track the per-packet codec byte. When codec changes mid-stream
      // (Plan C codec swap), reset the decoder and wait for the new
      // codec's first keyframe + description before decoding again.
      // Defensively request a keyframe so we don't sit blank if the
      // notify-then-keyframe ordering races on the wire.
      const incomingCodec = (codec ?? VideoCodec.H264_HW) as VideoCodec;
      if (incomingCodec !== activeCodecRef.current) {
        console.log("[StreamVideoPlayer] codec change",
          activeCodecRef.current, "→", incomingCodec);
        activeCodecRef.current = incomingCodec;
        descriptionRef.current = null;
        needsKeyframeRef.current = true; // drop deltas until new-codec keyframe
        invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});
      }

      if (keyframe && description && !descriptionRef.current) {
        const descBytes = base64ToBytes(description);
        descriptionRef.current = descBytes.buffer;
        decoder.reset();
        configureDecoder(decoder, descBytes.buffer);
        needsKeyframeRef.current = false;
        // Reset clock sync on first configure
        firstRemoteTs = -1;
        firstLocalTs = -1;
      }

      if (needsKeyframeRef.current && !keyframe) return;
      if (keyframe) needsKeyframeRef.current = false;

      const nowUs = performance.now() * 1000; // microseconds

      // Establish clock baseline on first frame
      if (firstRemoteTs < 0) {
        firstRemoteTs = timestamp;
        firstLocalTs = nowUs;
      }

      // How old is this frame relative to where playback should be?
      const expectedLocalTs = firstLocalTs + (timestamp - firstRemoteTs);
      const lagUs = nowUs - expectedLocalTs;

      // If the decoder queue is backing up, drop non-keyframes to catch up.
      // A queue > 3 frames means we're falling behind.
      if (decoder.decodeQueueSize > 3 && !keyframe) {
        return; // drop this delta frame to let the decoder catch up
      }

      // If this frame is more than 500ms behind wall-clock, drop it
      // (unless it's a keyframe — we need those to keep decoding)
      if (lagUs > 500_000 && !keyframe) {
        return;
      }

      // If we're very far behind (>2s), reset the clock baseline
      // so we re-sync from this point forward
      if (lagUs > 2_000_000 && keyframe) {
        firstRemoteTs = timestamp;
        firstLocalTs = nowUs;
      }

      try {
        const bytes = base64ToBytes(data);
        const chunk = new EncodedVideoChunk({
          type: keyframe ? "key" : "delta",
          timestamp,
          data: bytes,
        });
        decoder.decode(chunk);
      } catch (e) {
        console.error("[StreamVideoPlayer] Decode error:", e);
      }
    });

    return () => {
      unlisten.then((fn) => fn());
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
          <svg className="h-8 w-8 animate-spin text-[#00bfff]" viewBox="0 0 24 24" fill="none">
            <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" />
            <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
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

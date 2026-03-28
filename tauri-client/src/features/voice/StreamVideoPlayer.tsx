import { useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  streamerUsername: string;
  className?: string;
}

interface StreamFramePayload {
  username: string;
  data: string; // base64-encoded H.264 AVCC frame
  timestamp: number;
  keyframe: boolean;
  description: string | null; // base64-encoded avcC record (on keyframes)
}

function base64ToBytes(b64: string): Uint8Array {
  const binaryStr = atob(b64);
  const bytes = new Uint8Array(binaryStr.length);
  for (let i = 0; i < binaryStr.length; i++) {
    bytes[i] = binaryStr.charCodeAt(i);
  }
  return bytes;
}

export default function StreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);
  const descriptionRef = useRef<ArrayBuffer | null>(null);
  const needsKeyframeRef = useRef(true);

  const configureDecoder = useCallback((decoder: VideoDecoder, description?: ArrayBuffer) => {
    const config: VideoDecoderConfig = {
      codec: "avc1.640033", // High Profile, Level 5.1 (supports up to 4K@60fps)
      hardwareAcceleration: "prefer-hardware",
    };
    if (description) {
      config.description = description;
    }
    try {
      decoder.configure(config);
      console.log("[StreamVideoPlayer] Decoder configured", description ? `with avcC (${description.byteLength} bytes)` : "without description");
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

    const decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        const ctx = ctxRef.current;
        if (ctx && canvas) {
          if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
            canvas.width = frame.displayWidth;
            canvas.height = frame.displayHeight;
            console.log(`[StreamVideoPlayer] Canvas resized to ${frame.displayWidth}x${frame.displayHeight}`);
          }
          ctx.drawImage(frame, 0, 0);
        }
        frame.close();
      },
      error: handleDecoderError,
    });

    // Initial configure without description (will reconfigure when first keyframe arrives)
    configureDecoder(decoder);
    decoderRef.current = decoder;
    needsKeyframeRef.current = true;

    let frameCount = 0;
    const unlisten = listen<StreamFramePayload>("stream_frame", (event) => {
      const { username, data, timestamp, keyframe, description } = event.payload;
      if (username !== streamerUsername) return;
      if (decoder.state === "closed") return;

      // Configure decoder with avcC description on first keyframe only
      // (reconfiguring on every keyframe causes visible freezes)
      if (keyframe && description && !descriptionRef.current) {
        const descBytes = base64ToBytes(description);
        descriptionRef.current = descBytes.buffer;
        decoder.reset();
        configureDecoder(decoder, descBytes.buffer);
        needsKeyframeRef.current = false;
        console.log(`[StreamVideoPlayer] Configured with avcC description (${descBytes.length} bytes)`);
      }

      // Skip delta frames until we have a keyframe
      if (needsKeyframeRef.current && !keyframe) return;
      if (keyframe) needsKeyframeRef.current = false;

      try {
        const bytes = base64ToBytes(data);
        frameCount++;
        if (frameCount <= 5 || frameCount % 60 === 0) {
          console.log(`[StreamVideoPlayer] Frame #${frameCount}: ${bytes.length} bytes, keyframe=${keyframe}, ts=${timestamp}`);
        }

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
      if (decoder.state !== "closed") {
        decoder.close();
      }
      decoderRef.current = null;
      descriptionRef.current = null;
    };
  }, [streamerUsername, handleDecoderError, configureDecoder]);

  return (
    <canvas
      ref={canvasRef}
      className={className ?? "h-full w-full object-contain"}
    />
  );
}

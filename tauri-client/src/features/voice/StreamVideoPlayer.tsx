import { useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  streamerUsername: string;
  className?: string;
}

interface StreamFramePayload {
  username: string;
  data: number[];
  timestamp: number;
  keyframe: boolean;
}

export default function StreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);

  const handleDecoderError = useCallback((e: DOMException) => {
    console.error("[StreamVideoPlayer] Decoder error:", e);
    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(console.error);
    if (decoderRef.current && decoderRef.current.state !== "closed") {
      decoderRef.current.reset();
      decoderRef.current.configure({
        codec: "avc1.640028",
        hardwareAcceleration: "prefer-hardware",
      });
    }
  }, [streamerUsername]);

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
          }
          ctx.drawImage(frame, 0, 0);
        }
        frame.close();
      },
      error: handleDecoderError,
    });

    decoder.configure({
      codec: "avc1.640028",
      hardwareAcceleration: "prefer-hardware",
    });

    decoderRef.current = decoder;

    const unlisten = listen<StreamFramePayload>("stream_frame", (event) => {
      const { username, data, timestamp, keyframe } = event.payload;
      if (username !== streamerUsername) return;
      if (decoder.state === "closed") return;

      try {
        const chunk = new EncodedVideoChunk({
          type: keyframe ? "key" : "delta",
          timestamp,
          data: new Uint8Array(data),
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
    };
  }, [streamerUsername, handleDecoderError]);

  return (
    <canvas
      ref={canvasRef}
      className={className ?? "h-full w-full object-contain"}
    />
  );
}

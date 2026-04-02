import { useEffect, useRef, useCallback, useState } from "react";
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
  const [hasFirstFrame, setHasFirstFrame] = useState(false);

  const configureDecoder = useCallback((decoder: VideoDecoder, description?: ArrayBuffer) => {
    const config: VideoDecoderConfig = {
      codec: "avc1.640033",
      hardwareAcceleration: "prefer-hardware",
    };
    if (description) {
      config.description = description;
    }
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

    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

    const unlisten = listen<StreamFramePayload>("stream_frame", (event) => {
      const { username, data, timestamp, keyframe, description } = event.payload;
      if (username !== streamerUsername) return;
      if (decoder.state === "closed") return;

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
      if (decoder.state !== "closed") {
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

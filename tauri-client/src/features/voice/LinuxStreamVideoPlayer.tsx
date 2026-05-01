import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  uploadStreamFrame,
  renderStream,
  dropStream,
} from "./sharedStreamRenderer";

interface Props {
  streamerUsername: string;
  className?: string;
}

// Per-player visible 2D canvas. The actual NV12 → RGB conversion happens
// in a single shared WebGL2 context held by sharedStreamRenderer; we
// just drawImage the rendered frame onto our 2D canvas. This dodges the
// WebKitGTK limit on concurrent WebGL2 contexts that broke multi-stream
// watching previously.
export default function LinuxStreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [hasFirstFrame, setHasFirstFrame] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      setError("2D canvas unavailable");
      return;
    }

    let stopped = false;
    let rafHandle = 0;
    let lastSequence = 0;
    let pulling = false;
    let loggedFrameShape = false;

    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

    const loop = async () => {
      if (stopped) return;
      rafHandle = requestAnimationFrame(loop);

      if (pulling) return;
      pulling = true;
      try {
        const raw = await invoke("pull_video_frame_yuv", {
          streamerUsername,
          lastSeenSequence: lastSequence,
        });
        if (stopped) return;

        let buf: ArrayBuffer;
        let shapeName: string;
        if (raw instanceof ArrayBuffer) {
          buf = raw;
          shapeName = "ArrayBuffer";
        } else if (raw instanceof Uint8Array) {
          buf = raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength);
          shapeName = "Uint8Array";
        } else if (Array.isArray(raw)) {
          buf = new Uint8Array(raw as number[]).buffer;
          shapeName = "Array";
        } else {
          console.error("[LinuxStreamVideoPlayer] unexpected IPC response shape:",
            typeof raw, raw);
          return;
        }

        const view = new DataView(buf);
        if (view.byteLength === 0 || view.getUint8(0) === 0) {
          return;
        }

        if (!loggedFrameShape) {
          loggedFrameShape = true;
          console.log("[LinuxStreamVideoPlayer] first frame:",
            shapeName, "bytes=", buf.byteLength,
            "user=", streamerUsername);
        }

        const w = view.getUint32(4, true);
        const h = view.getUint32(8, true);
        const seq = Number(view.getBigUint64(12, true));
        const yLen = view.getUint32(28, true);
        const uvLen = view.getUint32(32, true);

        const headerLen = 36;
        const yPlane = new Uint8Array(buf, headerLen, yLen);
        const uvPlane = new Uint8Array(buf, headerLen + yLen, uvLen);

        // Upload + render via the shared renderer, then drawImage onto
        // our visible 2D canvas. The drawImage is synchronous and
        // GPU-accelerated (most browsers); WebGL state stays in the
        // shared context so other streams aren't disturbed.
        if (!uploadStreamFrame(streamerUsername, w, h, yPlane, uvPlane)) {
          // shared GL unavailable — give up on this player
          setError("Shared WebGL2 renderer unavailable");
          stopped = true;
          return;
        }
        const sharedCanvas = renderStream(streamerUsername);
        if (!sharedCanvas) return;

        if (canvas.width !== w || canvas.height !== h) {
          canvas.width = w;
          canvas.height = h;
        }
        ctx.drawImage(sharedCanvas, 0, 0, w, h, 0, 0, w, h);

        lastSequence = seq;
        if (!hasFirstFrame) setHasFirstFrame(true);
      } catch (e) {
        console.error("[LinuxStreamVideoPlayer] pull failed:", e);
      } finally {
        pulling = false;
      }
    };

    rafHandle = requestAnimationFrame(loop);

    return () => {
      stopped = true;
      cancelAnimationFrame(rafHandle);
      // Release the shared renderer's per-stream textures (~5MB at 1080p)
      // so they don't pile up after every watch session.
      dropStream(streamerUsername);
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
      <canvas
        ref={canvasRef}
        className={`${className ?? "h-full w-full object-contain"} ${hasFirstFrame ? "" : "opacity-0"}`}
      />
    </div>
  );
}

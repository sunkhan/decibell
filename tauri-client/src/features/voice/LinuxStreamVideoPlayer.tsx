import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  streamerUsername: string;
  className?: string;
}

// ── NV12 → RGB shader (BT.709 limited range) ────────────────────────────────
//
// NV12 layout in WebGL2: Y plane in an R8 texture sampled at full size, UV
// plane in an RG8 texture sampled at half size with hardware bilinear.
// BT.709 is the right matrix for HD content (1080p+); BT.601 only matters
// for SD which we don't ship. Limited range (16..235 luma, 16..240 chroma)
// matches what NVENC/AMF/QSV emit by default.
const VERT_SRC = `#version 300 es
in vec2 a_pos;
out vec2 v_uv;
void main() {
  // Map clipspace [-1..1] → uv [0..1], flip Y so top of frame draws at top
  // of canvas (WebGL's clipspace +Y is up; image-space +Y is down).
  v_uv = vec2((a_pos.x + 1.0) * 0.5, 1.0 - (a_pos.y + 1.0) * 0.5);
  gl_Position = vec4(a_pos, 0.0, 1.0);
}`;

const FRAG_SRC = `#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_y;
uniform sampler2D u_uv;
out vec4 o_color;
void main() {
  // Pull luma + chroma. UV texture is half-res; bilinear filter handles
  // the upscale, so no manual interpolation in the shader.
  float y = texture(u_y, v_uv).r;
  vec2 uv = texture(u_uv, v_uv).rg;
  // BT.709 limited-range YCbCr → linear RGB. Constants pre-baked from the
  // standard inverse matrix with the (Y - 16/255) and (UV - 128/255) bias.
  y  = (y  - 0.0625) * 1.1643836;
  float cb = uv.r - 0.5;
  float cr = uv.g - 0.5;
  float r = y                + 1.7927411 * cr;
  float g = y - 0.2132486 * cb - 0.5329093 * cr;
  float b = y + 2.1124018 * cb;
  o_color = vec4(r, g, b, 1.0);
}`;

interface GLState {
  gl: WebGL2RenderingContext;
  program: WebGLProgram;
  yTex: WebGLTexture;
  uvTex: WebGLTexture;
  yLoc: WebGLUniformLocation;
  uvLoc: WebGLUniformLocation;
  texWidth: number;   // current texture allocation; reallocated on resolution change
  texHeight: number;
}

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const sh = gl.createShader(type)!;
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(sh);
    gl.deleteShader(sh);
    throw new Error(`Shader compile failed: ${log}`);
  }
  return sh;
}

function initGL(canvas: HTMLCanvasElement): GLState | null {
  // `desynchronized: true` lets the compositor present our frames without
  // waiting for the next browser commit — measurably lower latency on
  // WebKitGTK. `preserveDrawingBuffer: false` is the default; explicit so
  // a future reader doesn't second-guess it.
  const gl = canvas.getContext("webgl2", {
    antialias: false,
    preserveDrawingBuffer: false,
    desynchronized: true,
    premultipliedAlpha: false,
  }) as WebGL2RenderingContext | null;
  if (!gl) {
    console.error("[LinuxStreamVideoPlayer] WebGL2 unavailable");
    return null;
  }

  const vs = compile(gl, gl.VERTEX_SHADER, VERT_SRC);
  const fs = compile(gl, gl.FRAGMENT_SHADER, FRAG_SRC);
  const program = gl.createProgram()!;
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(program);
    throw new Error(`Program link failed: ${log}`);
  }
  gl.deleteShader(vs);
  gl.deleteShader(fs);

  // Fullscreen triangle (single primitive, no index buffer, no quad seam).
  // Three vertices cover clipspace; the fragment outside the canvas is
  // discarded by the rasterizer for free.
  const vbo = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([
    -1, -1,
     3, -1,
    -1,  3,
  ]), gl.STATIC_DRAW);
  const posLoc = gl.getAttribLocation(program, "a_pos");
  gl.enableVertexAttribArray(posLoc);
  gl.vertexAttribPointer(posLoc, 2, gl.FLOAT, false, 0, 0);

  const yTex = gl.createTexture()!;
  gl.bindTexture(gl.TEXTURE_2D, yTex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

  const uvTex = gl.createTexture()!;
  gl.bindTexture(gl.TEXTURE_2D, uvTex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

  gl.useProgram(program);
  const yLoc = gl.getUniformLocation(program, "u_y")!;
  const uvLoc = gl.getUniformLocation(program, "u_uv")!;
  gl.uniform1i(yLoc, 0);
  gl.uniform1i(uvLoc, 1);

  // Tight pixel rows — our planes are packed without stride padding.
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

  return { gl, program, yTex, uvTex, yLoc, uvLoc, texWidth: 0, texHeight: 0 };
}

/** Reallocate Y (R8) + UV (RG8) textures for a new resolution. Cheap;
 *  one texture-storage call per plane, no upload. Called on first frame
 *  and whenever the streamer changes resolution mid-session. */
function allocTextures(state: GLState, w: number, h: number) {
  const { gl, yTex, uvTex } = state;
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, yTex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, null);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, uvTex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, w / 2, h / 2, 0, gl.RG, gl.UNSIGNED_BYTE, null);
  state.texWidth = w;
  state.texHeight = h;
}

/** Upload Y and UV planes via texSubImage2D (avoids reallocation). */
function uploadFrame(state: GLState, w: number, h: number, y: Uint8Array, uv: Uint8Array) {
  const { gl, yTex, uvTex } = state;
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, yTex);
  gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w, h, gl.RED, gl.UNSIGNED_BYTE, y);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, uvTex);
  gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, w / 2, h / 2, gl.RG, gl.UNSIGNED_BYTE, uv);
}

export default function LinuxStreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [hasFirstFrame, setHasFirstFrame] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    let state: GLState | null;
    try {
      state = initGL(canvas);
    } catch (e) {
      setError(String(e));
      return;
    }
    if (!state) {
      setError("WebGL2 not available — Linux video playback needs WebGL2 support.");
      return;
    }

    let stopped = false;
    let rafHandle = 0;
    // Number, not BigInt: Tauri IPC serializes via JSON, which doesn't
    // support BigInt. Number is safe for integers up to 2^53; at 120fps
    // overflow is ~2.4 million years away.
    let lastSequence = 0;
    let pulling = false; // prevents overlapping pulls if one tick takes >16ms
    let loggedShape = false; // log the IPC response shape exactly once

    // Kick a keyframe so we don't sit on a black canvas while we wait for
    // the streamer's natural keyframe interval.
    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

    const loop = async () => {
      if (stopped) return;
      rafHandle = requestAnimationFrame(loop);

      if (pulling) return;
      pulling = true;
      try {
        // Single Tauri IPC per RAF tick. Returns binary ArrayBuffer
        // (no JSON, no base64) — see commands/voice.rs:pull_video_frame_yuv.
        const raw = await invoke("pull_video_frame_yuv", {
          streamerUsername,
          lastSeenSequence: lastSequence,
        });
        if (stopped || !state) return;

        // Normalise across IPC transports. Tauri 2's `Response::new(Vec<u8>)`
        // returns ArrayBuffer on Webview2/WKWebView. WebKitGTK on Linux has
        // historically returned the same payload as a Uint8Array or even a
        // plain number array — accept all three so the renderer doesn't
        // silently die on the first tick.
        let buf: ArrayBuffer;
        if (raw instanceof ArrayBuffer) {
          buf = raw;
        } else if (raw instanceof Uint8Array) {
          // Slice into a fresh ArrayBuffer so DataView reads start at byte 0.
          buf = raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength);
        } else if (Array.isArray(raw)) {
          buf = new Uint8Array(raw as number[]).buffer;
        } else {
          if (!loggedShape) {
            loggedShape = true;
            console.error("[LinuxStreamVideoPlayer] unexpected IPC response shape:",
              typeof raw, raw);
          }
          return;
        }
        if (!loggedShape) {
          loggedShape = true;
          console.log("[LinuxStreamVideoPlayer] IPC response shape OK:",
            raw instanceof ArrayBuffer ? "ArrayBuffer" :
            raw instanceof Uint8Array ? "Uint8Array" : "Array",
            "bytes=", buf.byteLength);
        }

        const view = new DataView(buf);
        if (view.byteLength === 0 || view.getUint8(0) === 0) {
          // No new frame — keep last frame on screen. Skip the redraw too;
          // WebGL preserves the back buffer until the next swap.
          return;
        }

        const w = view.getUint32(4, true);
        const h = view.getUint32(8, true);
        // BigInt → Number conversion: see lastSequence comment for the
        // safety argument. The cast is explicit so a future reviewer
        // doesn't think we're relying on implicit coercion.
        const seq = Number(view.getBigUint64(12, true));
        const yLen = view.getUint32(28, true);
        const uvLen = view.getUint32(32, true);

        const headerLen = 36;
        // Slice the planes as Uint8Array views into the same ArrayBuffer
        // (no copy — the Uint8Array constructor here just describes a
        // window into `buf`).
        const yPlane = new Uint8Array(buf, headerLen, yLen);
        const uvPlane = new Uint8Array(buf, headerLen + yLen, uvLen);

        if (state.texWidth !== w || state.texHeight !== h) {
          // Resolution change (first frame, or streamer renegotiated).
          // Resize the canvas backing store too so CSS object-contain
          // gives us 1:1 pixel mapping at native resolution.
          canvas.width = w;
          canvas.height = h;
          state.gl.viewport(0, 0, w, h);
          allocTextures(state, w, h);
        }

        uploadFrame(state, w, h, yPlane, uvPlane);
        state.gl.drawArrays(state.gl.TRIANGLES, 0, 3);

        lastSequence = seq;
        if (!hasFirstFrame) setHasFirstFrame(true);
      } catch (e) {
        // Surface the error once so a regression here doesn't silently
        // freeze the player. Don't kill the loop — the next RAF tick may
        // recover (e.g. the engine just hadn't published yet).
        console.error("[LinuxStreamVideoPlayer] pull failed:", e);
      } finally {
        pulling = false;
      }
    };

    rafHandle = requestAnimationFrame(loop);

    return () => {
      stopped = true;
      cancelAnimationFrame(rafHandle);
      if (state) {
        const { gl, program, yTex, uvTex } = state;
        gl.deleteTexture(yTex);
        gl.deleteTexture(uvTex);
        gl.deleteProgram(program);
        // Force GL cleanup — WebKitGTK is conservative about releasing
        // GPU memory tied to lost contexts otherwise.
        const ext = gl.getExtension("WEBGL_lose_context");
        ext?.loseContext();
      }
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

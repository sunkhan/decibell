import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  streamerUsername: string;
  className?: string;
}

// NV12 → RGB renderer, one WebGL2 context per visible player.
//
// Earlier we routed everything through a single shared offscreen GL
// context to dodge a WebKitGTK ceiling on concurrent contexts (the
// second mount's getContext returned a broken context — createShader
// returned null, etc.). The fix worked but added a `drawImage` from
// the shared canvas to each player's visible 2D canvas — and on
// WebKitGTK that drawImage triggers a GPU→CPU readback, ~12ms per
// frame at 1440p. Combined with the IPC cost it pinned self-preview
// at 20fps.
//
// We've since added a single-active-stream policy on Linux, so only
// one of these mounts at a time. The shared-context dodge isn't
// needed and the readback cost goes away. Per-canvas WebGL2 is the
// right architecture here now.

const VERT_SRC = `#version 300 es
in vec2 a_pos;
out vec2 v_uv;
void main() {
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
  float y = texture(u_y, v_uv).r;
  vec2 uv = texture(u_uv, v_uv).rg;
  // BT.709 limited-range YCbCr → linear RGB.
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
  texWidth: number;
  texHeight: number;
}

function compileShader(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const sh = gl.createShader(type);
  if (!sh) throw new Error(`createShader(${type}) returned null`);
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(sh);
    gl.deleteShader(sh);
    throw new Error(`shader compile failed: ${log}`);
  }
  return sh;
}

function initGL(canvas: HTMLCanvasElement): GLState | null {
  const gl = canvas.getContext("webgl2", {
    antialias: false,
    preserveDrawingBuffer: false,
    desynchronized: true,
    premultipliedAlpha: false,
  }) as WebGL2RenderingContext | null;
  if (!gl) return null;

  const vs = compileShader(gl, gl.VERTEX_SHADER, VERT_SRC);
  const fs = compileShader(gl, gl.FRAGMENT_SHADER, FRAG_SRC);
  const program = gl.createProgram()!;
  gl.attachShader(program, vs);
  gl.attachShader(program, fs);
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
    throw new Error(`program link failed: ${gl.getProgramInfoLog(program)}`);
  }
  gl.deleteShader(vs);
  gl.deleteShader(fs);

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
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, yTex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);

  const uvTex = gl.createTexture()!;
  gl.activeTexture(gl.TEXTURE1);
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

  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

  return { gl, program, yTex, uvTex, yLoc, uvLoc, texWidth: 0, texHeight: 0 };
}

function reallocTextures(s: GLState, w: number, h: number) {
  const { gl, yTex, uvTex } = s;
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, yTex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, null);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, uvTex);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, w / 2, h / 2, 0, gl.RG, gl.UNSIGNED_BYTE, null);
  s.texWidth = w;
  s.texHeight = h;
}

function uploadFrame(s: GLState, w: number, h: number, y: Uint8Array, uv: Uint8Array) {
  const { gl, yTex, uvTex } = s;
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
      setError("WebGL2 unavailable");
      return;
    }

    let stopped = false;
    let rafHandle = 0;
    let lastSequence = 0;
    let pulling = false;

    let timingSamples = 0;
    let sumPullMs = 0;
    let sumGlMs = 0;
    let sumDrawMs = 0;
    let sumRafGapMs = 0;
    let lastRenderEnd = 0;

    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(() => {});

    const loop = async () => {
      if (stopped) return;
      const rafStart = performance.now();
      rafHandle = requestAnimationFrame(loop);

      if (pulling) return;
      pulling = true;
      try {
        const pullStart = performance.now();
        const raw = await invoke("pull_video_frame_yuv", {
          streamerUsername,
          lastSeenSequence: lastSequence,
        });
        const pullMs = performance.now() - pullStart;
        if (stopped || !state) return;

        let buf: ArrayBuffer;
        if (raw instanceof ArrayBuffer) {
          buf = raw;
        } else if (raw instanceof Uint8Array) {
          buf = raw.buffer.slice(raw.byteOffset, raw.byteOffset + raw.byteLength);
        } else if (Array.isArray(raw)) {
          buf = new Uint8Array(raw as number[]).buffer;
        } else {
          console.error("[LinuxStreamVideoPlayer] unexpected IPC response shape:", typeof raw, raw);
          return;
        }

        const view = new DataView(buf);
        if (view.byteLength === 0 || view.getUint8(0) === 0) {
          return;
        }

        const w = view.getUint32(4, true);
        const h = view.getUint32(8, true);
        const seq = Number(view.getBigUint64(12, true));
        const yLen = view.getUint32(28, true);
        const uvLen = view.getUint32(32, true);

        const headerLen = 36;
        const yPlane = new Uint8Array(buf, headerLen, yLen);
        const uvPlane = new Uint8Array(buf, headerLen + yLen, uvLen);

        const drawStart = performance.now();
        if (state.texWidth !== w || state.texHeight !== h) {
          canvas.width = w;
          canvas.height = h;
          state.gl.viewport(0, 0, w, h);
          state.gl.useProgram(state.program);
          state.gl.uniform1i(state.yLoc, 0);
          state.gl.uniform1i(state.uvLoc, 1);
          reallocTextures(state, w, h);
        }

        const glStart = performance.now();
        uploadFrame(state, w, h, yPlane, uvPlane);
        state.gl.drawArrays(state.gl.TRIANGLES, 0, 3);
        const glMs = performance.now() - glStart;
        const drawMs = performance.now() - drawStart;

        lastSequence = seq;
        if (!hasFirstFrame) setHasFirstFrame(true);

        const rafGapMs = lastRenderEnd > 0 ? rafStart - lastRenderEnd : 0;
        lastRenderEnd = performance.now();
        sumPullMs += pullMs;
        sumGlMs += glMs;
        sumDrawMs += drawMs;
        sumRafGapMs += rafGapMs;
        timingSamples++;
        if (timingSamples >= 60) {
          const n = timingSamples;
          const avgPull = sumPullMs / n;
          const avgGl = sumGlMs / n;
          const avgDraw = sumDrawMs / n;
          const avgRafGap = sumRafGapMs / n;
          const cycleMs = avgPull + avgDraw + avgRafGap;
          const fps = cycleMs > 0 ? 1000 / cycleMs : 0;
          console.log(
            `[LinuxStreamVideoPlayer] timing avg over ${n} renders: ` +
            `pull=${avgPull.toFixed(2)}ms gl=${avgGl.toFixed(2)}ms ` +
            `draw=${avgDraw.toFixed(2)}ms raf_gap=${avgRafGap.toFixed(2)}ms ` +
            `cycle=${cycleMs.toFixed(2)}ms → ${fps.toFixed(1)}fps`
          );
          timingSamples = 0;
          sumPullMs = 0;
          sumGlMs = 0;
          sumDrawMs = 0;
          sumRafGapMs = 0;
        }
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
      if (state) {
        const { gl, program, yTex, uvTex } = state;
        gl.deleteTexture(yTex);
        gl.deleteTexture(uvTex);
        gl.deleteProgram(program);
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

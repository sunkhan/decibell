// Shared WebGL2 NV12→RGB renderer for the Linux stream pipeline.
//
// Why a singleton: WebKitGTK caps the number of concurrent WebGL2
// contexts very low (often 1). Creating a context per
// LinuxStreamVideoPlayer made `gl.createShader` return null on the
// second mount and threw a TypeError on `shaderSource`. We need
// multiple streams playing at once (cards view + big view), so the
// rendering pipeline owns ONE hidden canvas with ONE GL2 context;
// each visible player has its own 2D canvas and just drawImage's
// the rendered frame off the shared canvas.
//
// Per-stream state (Y/UV textures, dimensions, last sequence) lives
// in this module so streams don't disturb each other when one
// renders.

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
  // BT.709 limited range
  y  = (y  - 0.0625) * 1.1643836;
  float cb = uv.r - 0.5;
  float cr = uv.g - 0.5;
  float r = y                + 1.7927411 * cr;
  float g = y - 0.2132486 * cb - 0.5329093 * cr;
  float b = y + 2.1124018 * cb;
  o_color = vec4(r, g, b, 1.0);
}`;

interface SharedState {
  canvas: HTMLCanvasElement;
  gl: WebGL2RenderingContext;
  program: WebGLProgram;
  yLoc: WebGLUniformLocation;
  uvLoc: WebGLUniformLocation;
}

interface StreamTextures {
  yTex: WebGLTexture;
  uvTex: WebGLTexture;
  width: number;
  height: number;
}

let shared: SharedState | null = null;
const streams = new Map<string, StreamTextures>();

function compile(gl: WebGL2RenderingContext, type: number, src: string): WebGLShader {
  const sh = gl.createShader(type);
  if (!sh) throw new Error(`createShader(${type}) returned null`);
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(sh);
    gl.deleteShader(sh);
    throw new Error(`Shader compile failed: ${log}`);
  }
  return sh;
}

/** Lazily initialise the shared WebGL2 canvas+context. Returns null if
 *  WebGL2 isn't available; the caller falls back to "no video". */
function ensureShared(): SharedState | null {
  if (shared) return shared;
  const canvas = document.createElement("canvas");
  // Sized when a stream registers — start tiny.
  canvas.width = 16;
  canvas.height = 16;
  const gl = canvas.getContext("webgl2", {
    antialias: false,
    preserveDrawingBuffer: false,
    desynchronized: true,
    premultipliedAlpha: false,
  }) as WebGL2RenderingContext | null;
  if (!gl) {
    console.error("[sharedStreamRenderer] WebGL2 unavailable");
    return null;
  }

  let program: WebGLProgram;
  try {
    const vs = compile(gl, gl.VERTEX_SHADER, VERT_SRC);
    const fs = compile(gl, gl.FRAGMENT_SHADER, FRAG_SRC);
    program = gl.createProgram()!;
    gl.attachShader(program, vs);
    gl.attachShader(program, fs);
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) {
      throw new Error(`Program link failed: ${gl.getProgramInfoLog(program)}`);
    }
    gl.deleteShader(vs);
    gl.deleteShader(fs);
  } catch (e) {
    console.error("[sharedStreamRenderer] init failed:", e);
    return null;
  }

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

  gl.useProgram(program);
  const yLoc = gl.getUniformLocation(program, "u_y")!;
  const uvLoc = gl.getUniformLocation(program, "u_uv")!;
  gl.uniform1i(yLoc, 0);
  gl.uniform1i(uvLoc, 1);

  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

  shared = { canvas, gl, program, yLoc, uvLoc };
  return shared;
}

/** Create or grow per-stream texture pair. Texture allocation is
 *  cheap; reallocating only on resolution change (typical: never). */
function ensureStreamTextures(s: SharedState, username: string, w: number, h: number): StreamTextures {
  let st = streams.get(username);
  if (st && st.width === w && st.height === h) return st;
  const { gl } = s;
  if (st) {
    gl.deleteTexture(st.yTex);
    gl.deleteTexture(st.uvTex);
  }
  const yTex = gl.createTexture()!;
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, yTex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.R8, w, h, 0, gl.RED, gl.UNSIGNED_BYTE, null);

  const uvTex = gl.createTexture()!;
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, uvTex);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RG8, w / 2, h / 2, 0, gl.RG, gl.UNSIGNED_BYTE, null);

  st = { yTex, uvTex, width: w, height: h };
  streams.set(username, st);
  return st;
}

/** Upload a stream's NV12 planes to its texture pair. Caller pre-checks
 *  that the sequence advanced; this just does the upload. */
export function uploadStreamFrame(
  username: string,
  width: number,
  height: number,
  yPlane: Uint8Array,
  uvPlane: Uint8Array,
): boolean {
  const s = ensureShared();
  if (!s) return false;
  const st = ensureStreamTextures(s, username, width, height);
  const { gl } = s;
  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, st.yTex);
  gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width, height, gl.RED, gl.UNSIGNED_BYTE, yPlane);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, st.uvTex);
  gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, width / 2, height / 2, gl.RG, gl.UNSIGNED_BYTE, uvPlane);
  return true;
}

/** Render the named stream to the shared canvas at its native resolution.
 *  The canvas backing store is grown to fit if needed. Caller copies the
 *  result to its visible canvas via `drawImage(sharedCanvas, ...)` right
 *  after this returns, before any other stream renders over it. */
export function renderStream(username: string): HTMLCanvasElement | null {
  const s = ensureShared();
  if (!s) return null;
  const st = streams.get(username);
  if (!st) return null;
  const { gl, canvas, program, yLoc, uvLoc } = s;

  if (canvas.width !== st.width || canvas.height !== st.height) {
    canvas.width = st.width;
    canvas.height = st.height;
    gl.viewport(0, 0, st.width, st.height);
    // Setting canvas.width can disturb GL state on WebKitGTK — re-assert
    // program + sampler uniforms before drawing.
    gl.useProgram(program);
    gl.uniform1i(yLoc, 0);
    gl.uniform1i(uvLoc, 1);
  } else {
    gl.viewport(0, 0, st.width, st.height);
  }

  gl.activeTexture(gl.TEXTURE0);
  gl.bindTexture(gl.TEXTURE_2D, st.yTex);
  gl.activeTexture(gl.TEXTURE1);
  gl.bindTexture(gl.TEXTURE_2D, st.uvTex);
  gl.drawArrays(gl.TRIANGLES, 0, 3);
  // Force the GL pipeline to flush so a synchronous `drawImage` from
  // this canvas reads the just-drawn pixels rather than a stale buffer.
  gl.flush();

  return canvas;
}

/** Drop a stream's textures when its player unmounts. Frees ~5MB at 1080p. */
export function dropStream(username: string): void {
  if (!shared) return;
  const st = streams.get(username);
  if (!st) return;
  shared.gl.deleteTexture(st.yTex);
  shared.gl.deleteTexture(st.uvTex);
  streams.delete(username);
}

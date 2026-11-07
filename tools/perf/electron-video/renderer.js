// StreamVideoPlayer's decode + paint path (electron-client/src/features/voice/StreamVideoPlayer.tsx), reduced to
// what a steady stream exercises: configure on the first frame (the codec strings of utils/codecMap.ts, prefer-
// hardware, optimizeForLatency), drop deltas while the decode queue is over 3, drawImage into a canvas whose
// backing store is the on-screen size (x dpr) capped at the source size, close every frame.
const { ipcRenderer } = require('electron');

const CODEC = { 1: 'avc1.64003E', 2: 'hvc1.1.6.L186.B0', 3: 'av01.0.19M.08' };
const NAME = { 1: 'H.264', 2: 'HEVC', 3: 'AV1' };
const now = () => performance.timeOrigin + performance.now();
const pct = (v, p) => {
  if (!v.length) return 0;
  const s = [...v].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor(p * s.length))];
};

let players = [];
let sampling = false;

ipcRenderer.on('streams', (_e, streams) => {
  const grid = document.getElementById('grid');
  if (streams.length > 1) grid.classList.add('multi');
  players = streams.map((s) => {
    const tile = document.createElement('div');
    tile.className = 'tile';
    const canvas = document.createElement('canvas');
    tile.appendChild(canvas);
    grid.appendChild(tile);
    const ctx = canvas.getContext('2d', { alpha: false, desynchronized: true });
    const p = {
      s, tile, canvas, ctx, configured: false, needKey: true,
      fed: 0, decoded: 0, drawn: 0, dropped: 0, errors: 0,
      base: null, latency: [], drawTimes: [], pending: new Map(),
    };
    // CSS size: the video's rectangle inside the tile (object-fit: contain).
    const layout = () => {
      const r = tile.getBoundingClientRect();
      const k = Math.min(r.width / s.width, r.height / s.height);
      canvas.style.width = `${s.width * k}px`;
      canvas.style.height = `${s.height * k}px`;
      p.display = { w: s.width * k, h: s.height * k };
    };
    new ResizeObserver(layout).observe(tile);
    layout();
    p.decoder = new VideoDecoder({
      output: (frame) => {
        try {
          p.decoded++;
          const vr = frame.visibleRect;
          const srcW = vr ? vr.width : frame.codedWidth, srcH = vr ? vr.height : frame.codedHeight;
          const dpr = window.devicePixelRatio || 1;
          const scale = Math.min(1, (p.display.w * dpr) / srcW, (p.display.h * dpr) / srcH);
          const dstW = Math.max(1, Math.round(srcW * scale)), dstH = Math.max(1, Math.round(srcH * scale));
          if (canvas.width !== dstW || canvas.height !== dstH) { canvas.width = dstW; canvas.height = dstH; }
          ctx.drawImage(frame, vr ? vr.x : 0, vr ? vr.y : 0, srcW, srcH, 0, 0, dstW, dstH);
          p.drawn++;
          const fedAt = p.pending.get(frame.timestamp);
          p.pending.delete(frame.timestamp);
          if (sampling) {
            const t = now();
            if (fedAt !== undefined) p.latency.push(t - fedAt);
            p.drawTimes.push(t);
          }
        } finally {
          frame.close();
        }
      },
      error: (e) => { p.errors++; console.error(e); p.needKey = true; p.configured = false; },
    });
    // The feeder: a frame every 1/fps on the wall clock, drift-corrected.
    const period = 1000 / s.fps;
    let next = performance.now();
    const tick = () => {
      ipcRenderer.send('want-frame', players.indexOf(p), now());
      next += period;
      let wait = next - performance.now();
      if (wait < -4 * period) { next = performance.now(); wait = 0; }
      setTimeout(tick, Math.max(0, wait));
    };
    setTimeout(tick, 0);
    return p;
  });
});

let seq = 0;
ipcRenderer.on('frame', (_e, f) => {
  const p = players[f.index];
  if (!p) return;
  p.fed++;
  if (p.decoder.state === 'closed') return;
  if (!p.configured) {
    p.decoder.configure({ codec: CODEC[p.s.codec], optimizeForLatency: true, hardwareAcceleration: 'prefer-hardware' });
    p.configured = true;
    p.needKey = true;
  }
  if (p.needKey && !f.key) return;
  if (f.key) p.needKey = false;
  if (p.decoder.decodeQueueSize > 3 && !f.key) { p.needKey = true; p.dropped++; return; }
  const timestamp = ++seq;
  p.pending.set(timestamp, f.fedAt);
  try {
    p.decoder.decode(new EncodedVideoChunk({ type: f.key ? 'key' : 'delta', timestamp, data: f.data }));
  } catch (e) {
    p.errors++;
  }
});

ipcRenderer.on('bench-start', () => {
  sampling = true;
  for (const p of players) {
    p.base = { fed: p.fed, decoded: p.decoded, drawn: p.drawn, dropped: p.dropped };
    p.latency = [];
    p.drawTimes = [];
  }
});

ipcRenderer.on('bench-stop', () => {
  const report = players.map((p) => {
    const iv = [];
    for (let i = 1; i < p.drawTimes.length; i++) iv.push(p.drawTimes[i] - p.drawTimes[i - 1]);
    return {
      codec: NAME[p.s.codec], video: `${p.s.width}x${p.s.height}@${p.s.fps}`,
      canvas: `${p.canvas.width}x${p.canvas.height}`,
      fed: p.fed - p.base.fed, decoded: p.decoded - p.base.decoded, drawn: p.drawn - p.base.drawn,
      dropped: p.dropped - p.base.dropped, errors: p.errors,
      draw_interval_ms_p50: pct(iv, 0.5), draw_interval_ms_p95: pct(iv, 0.95), draw_interval_ms_p99: pct(iv, 0.99),
      feed_to_draw_ms_p50: pct(p.latency, 0.5), feed_to_draw_ms_p99: pct(p.latency, 0.99),
    };
  });
  ipcRenderer.send('bench-report', report);
});

// Measures Electron's smooth wheel scrolling: injects mouseWheel events (CDP Input.dispatchMouseEvent, the real
// input pipeline) over the message list and samples its scrollTop every animation frame, so the Qt client can
// match the distance and the easing curve.
//   node --experimental-websocket electron-wheel.mjs <cdp port> <x> <y> <deltaY> <notches> <gap ms>
// notches = 0: inject nothing, only sample for 1.5 s (real WM_MOUSEWHEEL messages are posted from outside; CDP
// wheel events skip Chromium's smooth scrolling and jump).
const [, , port, x, y, deltaY = '-100', notches = '1', gap = '50'] = process.argv;
const page = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let id = 0; const pending = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
const call = (method, params) => new Promise((r) => { const n = ++id; pending.set(n, r); ws.send(JSON.stringify({ id: n, method, params })); });
const evaluate = async (expression) => (await call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;

// Start sampling in the page (rAF timestamps + the list's scrollTop), then inject the notches.
await evaluate(`(() => {
  const all = [...document.querySelectorAll('*')].filter((e) => { const s = getComputedStyle(e); return (s.overflowY === 'auto' || s.overflowY === 'scroll') && e.scrollHeight > e.clientHeight + 100; });
  const sc = all.sort((a, b) => b.clientWidth * b.clientHeight - a.clientWidth * a.clientHeight)[0];
  window.__wheel = { sc, samples: [], start: performance.now() };
  const step = (t) => { window.__wheel.samples.push([t - window.__wheel.start, sc.scrollTop]); if (t - window.__wheel.start < 1500) requestAnimationFrame(step); };
  requestAnimationFrame(step);
  return true;
})()`);
await new Promise((r) => setTimeout(r, 50));
for (let i = 0; i < Number(notches); i++) {
  await call('Input.dispatchMouseEvent', { type: 'mouseWheel', x: Number(x), y: Number(y), deltaX: 0, deltaY: Number(deltaY) });
  if (i + 1 < Number(notches)) await new Promise((r) => setTimeout(r, Number(gap)));
}
await new Promise((r) => setTimeout(r, Number(notches) === 0 ? 1700 : 1600));
const samples = await evaluate('window.__wheel.samples');
const y0 = samples[0][1];
// Print (ms, distance travelled) whenever the position changes, plus the end.
const out = [];
let last = null;
for (const [t, top] of samples) { const d = +(y0 - top).toFixed(2); if (d !== last) { out.push([+t.toFixed(1), d]); last = d; } }
console.log(JSON.stringify({ total: out.length ? out[out.length - 1][1] : 0, points: out }));
ws.close();

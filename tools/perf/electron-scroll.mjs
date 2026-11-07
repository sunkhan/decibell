// Scroll benchmark for the Electron client, the counterpart of `decibell-qt --bench-scroll`: scrolls the open
// channel's message list into history at a constant speed (px/s) for N seconds from requestAnimationFrame and
// prints the frame-interval statistics as one JSON line.
//   node --experimental-websocket electron-scroll.mjs <cdp port> <seconds> <px/s>
// Start Electron with --remote-debugging-port=<port> and open a channel first (open-channel.mjs).
const [, , port, seconds = '6', speed = '1000'] = process.argv;
const page = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let id = 0; const pending = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
const evaluate = (expression) => new Promise((r) => { const n = ++id; pending.set(n, (d) => r(d.result?.result?.value ?? d)); ws.send(JSON.stringify({ id: n, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } })); });

const page_fn = async (seconds, speed) => {
  // The message list's scroller: the tallest overflow-y scroll container holding message rows.
  const scrollers = [...document.querySelectorAll('*')].filter((e) => {
    const s = getComputedStyle(e);
    return (s.overflowY === 'auto' || s.overflowY === 'scroll') && e.scrollHeight > e.clientHeight + 100;
  });
  const sc = scrollers.sort((a, b) => b.clientWidth * b.clientHeight - a.clientWidth * a.clientHeight)[0];
  if (!sc) return { error: 'no scroller' };
  const times = [];
  const start = performance.now();
  let last = start;
  await new Promise((done) => {
    const step = (t) => {
      times.push(t);
      sc.scrollTop -= (speed / 1000) * (t - last);
      last = t;
      if (t - start < seconds * 1000) requestAnimationFrame(step); else done();
    };
    requestAnimationFrame(step);
  });
  const iv = times.slice(1).map((t, i) => t - times[i]).sort((a, b) => a - b);
  const p = (q) => +iv[Math.min(iv.length - 1, Math.floor((iv.length - 1) * q))].toFixed(2);
  const period = 1000 / 280;
  return { frames: times.length, scroller: `${sc.clientWidth}x${sc.clientHeight}`, interval_ms: { p50: p(0.5), p95: p(0.95), p99: p(0.99), max: p(1) },
           missed_vsyncs: iv.filter((d) => d > period * 1.5).length };
};
const out = await evaluate(`(${page_fn.toString()})(${Number(seconds)}, ${Number(speed)})`);
console.log(JSON.stringify(out));
ws.close();

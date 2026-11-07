// Selects text across two adjacent messages in the running Electron client's open channel (over CDP) and saves a
// screenshot, plus the colours Chromium paints the selection with: what the Qt client's selection must match.
//   node --experimental-websocket electron-selection.mjs <cdp port> <out.png>
import fs from 'node:fs';

const [, , port, out] = process.argv;
const page = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let id = 0; const pending = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
const call = (method, params) => new Promise((r) => { const n = ++id; pending.set(n, r); ws.send(JSON.stringify({ id: n, method, params })); });
const evaluate = async (expression) => (await call('Runtime.evaluate', { expression, returnByValue: true })).result?.result?.value;

const info = await evaluate(`(() => {
  // Message bodies on screen: the select-text containers' text, in document order.
  const vh = window.innerHeight;
  const bodies = [...document.querySelectorAll('.select-text')].filter((e) => {
    const r = e.getBoundingClientRect();
    return r.height > 0 && r.top > 80 && r.bottom < vh - 90 && e.innerText.trim().length > 12;
  });
  if (bodies.length < 2) return { error: 'fewer than two messages on screen' };
  const a = bodies[bodies.length - 2], b = bodies[bodies.length - 1];
  const textNode = (el, last) => {
    const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, { acceptNode: (n) => n.data.trim().length > 4 ? 1 : 3 });
    let n, found = null;
    while ((n = w.nextNode())) { found = n; if (!last) break; }
    return found;
  };
  const start = textNode(a, true), end = textNode(b, true);
  const range = document.createRange();
  range.setStart(start, Math.min(3, start.data.length));
  range.setEnd(end, Math.min(10, end.data.length));
  const sel = window.getSelection();
  sel.removeAllRanges();
  sel.addRange(range);
  const s = getComputedStyle(start.parentElement, '::selection');
  return { selected: sel.toString(), selection_bg: s.backgroundColor, selection_fg: s.color, text_color: getComputedStyle(start.parentElement).color };
})()`);
await new Promise((r) => setTimeout(r, 300));
const shot = await call('Page.captureScreenshot', { format: 'png' });
fs.writeFileSync(out, Buffer.from(shot.result.data, 'base64'));
console.log(JSON.stringify(info));
ws.close();

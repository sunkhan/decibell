// Measures the live Electron client for the Qt client's fidelity work (qt-client/HANDOFF.md §2a).
// Start Electron with --remote-debugging-port=<port> (and e.g. tools/perf/open-channel.mjs to open a channel), then:
//   node --experimental-websocket inspect.mjs <port> <out-prefix>            <out>.png screenshot + <out>.txt layout outline
//   node --experimental-websocket inspect.mjs <port> - visible.js            on-screen boxes / colours / fonts of the main area
// The outline walks the DOM: every block's box [x,y,w,h], background, radius, border, padding and text style.
// Screenshots hold real community content: keep them out of the repo.
// Saves <out-prefix>.png (screenshot) and <out-prefix>.txt (layout outline); with a JS file, evaluates it
// in the page and prints the result instead of the outline.
import fs from 'node:fs';
const [, , port, out, jsFile] = process.argv;
const t = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).filter((x) => x.type === 'page')[0];
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let id = 0; const pend = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pend.has(d.id)) { pend.get(d.id)(d); pend.delete(d.id); } };
const call = (method, params = {}) => new Promise((r) => { const n = ++id; pend.set(n, r); ws.send(JSON.stringify({ id: n, method, params })); });
const evaluate = async (expression) => (await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result?.result?.value;

if (jsFile) {
  console.log(JSON.stringify(await evaluate(fs.readFileSync(jsFile, 'utf8')), null, 1));
} else {
  const shot = await call('Page.captureScreenshot', { format: 'png' });
  fs.writeFileSync(out + '.png', Buffer.from(shot.result.data, 'base64'));
function pageOutline() {
    const lines = [`viewport ${innerWidth}x${innerHeight} dpr ${devicePixelRatio} theme ${document.documentElement.dataset.theme}`];
    const R = (el) => el.getBoundingClientRect();
    const walk = (el, depth, parentKey) => {
      if (lines.length > 400) return;
      const r = R(el); const cs = getComputedStyle(el);
      if (r.width < 24 || r.height < 12 || cs.display === 'none' || cs.visibility === 'hidden') return;
      const key = [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)].join(',');
      let d = depth;
      if (key !== parentKey) {
        const cls = (el.getAttribute('class') || '').split(/\s+/).filter(Boolean).slice(0, 6).join('.');
        const bits = [];
        if (cs.backgroundColor !== 'rgba(0, 0, 0, 0)') bits.push('bg ' + cs.backgroundColor);
        if (cs.borderRadius !== '0px') bits.push('r ' + cs.borderRadius);
        if (cs.borderTopWidth !== '0px' || cs.borderLeftWidth !== '0px') bits.push('border ' + cs.borderTopWidth + '/' + cs.borderLeftWidth + ' ' + cs.borderTopColor);
        if (cs.boxShadow !== 'none') bits.push('shadow');
        const txt = [...el.childNodes].filter((n) => n.nodeType === 3 && n.textContent.trim()).map((n) => n.textContent.trim()).join(' ').slice(0, 30);
        if (txt) bits.push(`"${txt}" ${cs.fontFamily.split(',')[0]} ${cs.fontSize}/${cs.lineHeight} w${cs.fontWeight} ${cs.color}`);
        lines.push('  '.repeat(depth) + `${el.tagName.toLowerCase()}${cls ? '.' + cls : ''} [${key}] ${bits.join(' | ')}`);
        d = depth + 1;
      }
      for (const c of el.children) walk(c, d, key);
    };
    walk(document.body, 0, '');
    return lines.join('\n');
}
  const outline = await evaluate(`(${pageOutline.toString()})()`);
  fs.writeFileSync(out + '.txt', outline);
  console.log(outline.split('\n').length + ' lines; ' + outline.split('\n')[0]);
}
ws.close();

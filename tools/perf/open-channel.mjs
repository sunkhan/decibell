// Drives the running Electron client over CDP: clicks the server tile titled argv[3], then a TEXT
// channel (button[data-channel-id]; voice rows never carry that attribute, so this can't join voice).
// Usage: node --experimental-websocket open-channel.mjs <port> <server title> [channel name]
const [, , port, serverTitle, wantChannel] = process.argv;

async function targets() {
  for (let i = 0; i < 60; i++) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json`);
      const t = (await r.json()).filter((x) => x.type === 'page');
      if (t.length) return t;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error('no CDP page target');
}

const page = (await targets())[0];
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
let id = 0;
const pending = new Map();
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
};
const evaluate = (expression) => new Promise((res) => {
  const n = ++id;
  pending.set(n, (msg) => res(msg.result?.result?.value ?? msg));
  ws.send(JSON.stringify({ id: n, method: 'Runtime.evaluate', params: { expression, awaitPromise: true, returnByValue: true } }));
});

const result = await evaluate(`(async () => {
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const until = async (fn, ms) => { const t = Date.now(); while (Date.now() - t < ms) { const v = fn(); if (v) return v; await sleep(250); } return null; };
  const tile = await until(() => { const b = document.querySelector('button[title=${JSON.stringify(serverTitle)}]'); return b && !b.disabled ? b : null; }, 45000);
  if (!tile) return { error: 'server tile not found or still connecting', titles: [...document.querySelectorAll('button[title]')].map((b) => b.title) };
  tile.click();
  const rows = await until(() => { const r = [...document.querySelectorAll('button[data-channel-id]')]; return r.length ? r : null; }, 20000);
  if (!rows) return { error: 'no text channels' };
  const names = rows.map((r) => r.textContent.trim());
  const want = ${JSON.stringify(wantChannel ?? '')};
  const row = (want && rows.find((r) => r.textContent.trim().toLowerCase().includes(want.toLowerCase()))) || rows[0];
  row.click();
  await sleep(3000);
  return { server: ${JSON.stringify(serverTitle)}, channels: names, opened: row.textContent.trim(),
           rowsInList: document.querySelectorAll('[data-message-id], [data-msg-id]').length,
           active: document.activeElement && (document.activeElement.tagName + '.' + (document.activeElement.className || '').toString().slice(0, 40)) };
})()`);
console.log(JSON.stringify(result, null, 1));
ws.close();

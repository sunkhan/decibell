// Small CDP driver for the Electron client (electron-local.sh): nothing here types into the page except `signin`.
//   node electron-cdp.mjs <port> state                 what the page shows (signed in? view, channel, focus) as JSON
//   node electron-cdp.mjs <port> wait-page [s]         wait for a page target that has loaded
//   node electron-cdp.mjs <port> wait-main [s]         wait for the main layout (signed in: the Home button exists)
//   node electron-cdp.mjs <port> wait-server <title> [s]  wait for a server tile that is connected (not "Connecting…")
//   node electron-cdp.mjs <port> signin <user> <pass>  fill the sign-in form, submit, wait for the main layout
//   node electron-cdp.mjs <port> home                  click Home, then blur whatever has keyboard focus
//   node electron-cdp.mjs <port> blur                  blur the focused element (no caret, nothing focused)
//   node electron-cdp.mjs <port> minimize              the title bar's Minimize (the page goes hidden, as a covered
//                                                     window does on Windows through occlusion tracking)
//   node electron-cdp.mjs <port> dismiss-banner        close the first-run crash-report notice (a used profile has)
//   node electron-cdp.mjs <port> screenshot <out.png>  Page.captureScreenshot of the window
// Node >= 22 (global WebSocket); older Node needs --experimental-websocket.
import { writeFileSync } from 'node:fs';

const [, , port, cmd, ...args] = process.argv;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function pageTarget(timeoutMs = 30000) {
  const t0 = Date.now();
  let last;
  while (Date.now() - t0 < timeoutMs) {
    try {
      const r = await fetch(`http://127.0.0.1:${port}/json`);
      const pages = (await r.json()).filter((x) => x.type === 'page' && !x.url.startsWith('devtools://'));
      if (pages.length) return pages[0];
    } catch (e) { last = e; }
    await sleep(250);
  }
  throw new Error(`no CDP page target on port ${port}${last ? ` (${last.message})` : ''}`);
}

async function connect(timeoutMs) {
  const page = await pageTarget(timeoutMs);
  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  };
  const send = (method, params = {}) => new Promise((res) => {
    const n = ++id;
    pending.set(n, res);
    ws.send(JSON.stringify({ id: n, method, params }));
  });
  const evaluate = async (expression) => {
    const msg = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (msg.result?.exceptionDetails) throw new Error(JSON.stringify(msg.result.exceptionDetails).slice(0, 500));
    return msg.result?.result?.value;
  };
  return { ws, send, evaluate, close: () => ws.close() };
}

// Runs in the page.
const DISMISS_FN = `(() => {
  const p = [...document.querySelectorAll('p, div, span')].find((e) => e.childElementCount === 0 && /anonymous crash reports/.test(e.textContent || ''));
  const b = p && p.closest('div') && (p.closest('div').parentElement || p.closest('div')).querySelector('button[title="Dismiss"]');
  if (!b) return false;
  b.click();
  return true;
})()`;

const STATE_FN = `(() => {
  const ae = document.activeElement;
  const tiles = [...document.querySelectorAll('button[title]')]
    .filter((b) => b.closest('div') && b.title && !['Home', 'Browse servers'].includes(b.title) && b.offsetParent);
  const activeChannel = document.querySelector('button[data-channel-id][aria-current], button[data-channel-id].bg-surface-active');
  return {
    url: location.href.replace(/^.*\\/renderer\\//, 'renderer/'),
    readyState: document.readyState,
    signedIn: !!document.querySelector('button[title="Home"]'),
    loginForm: !!document.querySelector('input[type="password"]'),
    visibility: document.visibilityState,
    hasFocus: document.hasFocus(),
    activeElement: ae ? (ae.tagName + (ae.getAttribute('contenteditable') ? '[contenteditable]' : '') + (ae.id ? '#' + ae.id : '')) : null,
    channelRows: document.querySelectorAll('button[data-channel-id]').length,
    messageRows: document.querySelectorAll('[data-message-id], [data-msg-id]').length,
    connecting: [...document.querySelectorAll('button[title="Connecting…"]')].length,
    serverTiles: [...new Set(tiles.map((b) => b.title))].slice(0, 20),
    title: document.title,
    window: { w: innerWidth, h: innerHeight, dpr: devicePixelRatio },
  };
})()`;

async function waitFor(c, expr, timeoutS, what) {
  const t0 = Date.now();
  while (Date.now() - t0 < timeoutS * 1000) {
    try { if (await c.evaluate(expr)) return true; } catch {}
    await sleep(250);
  }
  throw new Error(`timed out after ${timeoutS} s waiting for ${what}`);
}

const c = await connect(cmd === 'wait-page' ? Number(args[0] ?? 30) * 1000 : 30000);
try {
  switch (cmd) {
    case 'state':
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    case 'wait-page':
      await waitFor(c, `document.readyState === 'complete'`, Number(args[0] ?? 30), 'the page');
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    case 'wait-main':
      await waitFor(c, `!!document.querySelector('button[title="Home"]')`, Number(args[0] ?? 60), 'the main layout');
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    case 'wait-server': {
      const t = JSON.stringify(args[0]);
      await waitFor(c, `(() => { const b = document.querySelector('button[title=' + JSON.stringify(${t}) + ']'); return !!b && !b.disabled; })()`,
        Number(args[1] ?? 60), `server tile ${args[0]}`);
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    }
    case 'signin': {
      const [user, pass] = args;
      await waitFor(c, `!!document.querySelector('button[title="Home"]') || !!document.querySelector('input[type="password"]')`, 60, 'the sign-in form or the main layout');
      if (await c.evaluate(`!!document.querySelector('button[title="Home"]')`)) {
        console.log(JSON.stringify({ already: true, ...(await c.evaluate(STATE_FN)) }));
        break;
      }
      const r = await c.evaluate(`(async () => {
        const set = (el, v) => {
          Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set.call(el, v);
          el.dispatchEvent(new Event('input', { bubbles: true }));
        };
        const user = document.querySelector('form input[type="text"]');
        const pass = document.querySelector('form input[type="password"]');
        if (!user || !pass) return { error: 'no sign-in form' };
        set(user, ${JSON.stringify(user)});
        set(pass, ${JSON.stringify(pass)});
        await new Promise((r) => setTimeout(r, 100));
        document.querySelector('form button[type="submit"]').click();
        return { submitted: true };
      })()`);
      if (!r?.submitted) throw new Error(`sign-in form: ${JSON.stringify(r)}`);
      const t0 = Date.now();
      while (Date.now() - t0 < 60000) {
        const s = await c.evaluate(`(() => ({ main: !!document.querySelector('button[title="Home"]'),
          err: [...document.querySelectorAll('form p, form div')].map((e) => e.textContent).find((t) => /error|invalid|refused|failed|wrong/i.test(t || '')) || null }))()`);
        if (s?.main) break;
        if (s?.err) throw new Error(`sign-in refused: ${s.err}`);
        await sleep(250);
      }
      await sleep(1500);
      const dismissed = await c.evaluate(DISMISS_FN);
      await c.evaluate(`document.activeElement && document.activeElement.blur && document.activeElement.blur()`);
      console.log(JSON.stringify({ signedIn: true, bannerDismissed: dismissed, ...(await c.evaluate(STATE_FN)) }));
      break;
    }
    case 'home':
      await waitFor(c, `!!document.querySelector('button[title="Home"]')`, 60, 'the main layout');
      await c.evaluate(`(async () => { document.querySelector('button[title="Home"]').click(); await new Promise((r) => setTimeout(r, 1500));
        document.activeElement && document.activeElement !== document.body && document.activeElement.blur && document.activeElement.blur(); })()`);
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    case 'minimize': {
      const ok = await c.evaluate(`(() => { const b = document.querySelector('button[title="Minimize"]'); if (!b) return false; b.click(); return true; })()`);
      if (!ok) throw new Error('no Minimize button');
      await waitFor(c, `document.visibilityState === 'hidden'`, 10, 'the page to go hidden');
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    }
    case 'dismiss-banner':
      console.log(JSON.stringify({ dismissed: await c.evaluate(DISMISS_FN) }));
      break;
    case 'blur':
      await c.evaluate(`document.activeElement && document.activeElement !== document.body && document.activeElement.blur && document.activeElement.blur()`);
      console.log(JSON.stringify(await c.evaluate(STATE_FN)));
      break;
    case 'screenshot': {
      const out = args[0];
      if (!out) throw new Error('screenshot <out.png>');
      const r = await c.send('Page.captureScreenshot', { format: 'png' });
      if (!r.result?.data) throw new Error(`captureScreenshot: ${JSON.stringify(r).slice(0, 300)}`);
      writeFileSync(out, Buffer.from(r.result.data, 'base64'));
      console.log(JSON.stringify({ screenshot: out, ...(await c.evaluate(STATE_FN)) }));
      break;
    }
    default:
      throw new Error(`unknown command ${cmd}`);
  }
} catch (e) {
  console.error(`electron-cdp: ${e.message}`);
  process.exitCode = 1;
} finally {
  c.close();
}

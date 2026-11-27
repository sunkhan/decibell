// Attachment playback in the Electron client over CDP, for scenarios.sh electron media-play-* (README: "Attachment
// playback"). The clip is a message in the open channel (posted by the GPUI client's `--check media-perf`); this
// finds its video card, plays it MUTED and reads Chromium's own counters. Nothing is typed.
//   node electron-media.mjs <port> find <filename> [s]   wait for the card (button[title=<filename>]), scroll it in
//   node electron-media.mjs <port> play <filename>       mute (below), click the card, wait until it plays; the
//                                                        decoder from CDP's Media domain (kVideoDecoderName, ...)
//   node electron-media.mjs <port> stats [label]         getVideoPlaybackQuality(), position, state: one JSON line
//   node electron-media.mjs <port> restart               currentTime = 0 (a measurement window begins behind it)
//   node electron-media.mjs <port> stop                  pause, give the mute setting back
//   node electron-media.mjs <port> gone <filename> [s]   wait until the card is gone (its message was deleted)
// Muting: the client is started with Chromium's --mute-audio (scenarios.sh does), and before the click this installs
// a MutationObserver that sets `muted` on every <video> the page adds, in the microtask after React inserts it, so
// the element is muted before a sample is rendered. (The player saves a mute change as the profile's setting: `stop`
// puts the element's original value back, so the isolated profile keeps what it had.)
// Node >= 22 (global WebSocket); older Node needs --experimental-websocket.
const [, , port, cmd, ...args] = process.argv;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function connect() {
  let pages = [];
  for (let i = 0; i < 40 && !pages.length; i++) {
    try {
      pages = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).filter((x) => x.type === 'page' && !x.url.startsWith('devtools://'));
    } catch {}
    if (!pages.length) await sleep(250);
  }
  if (!pages.length) throw new Error(`no CDP page target on port ${port}`);
  const ws = new WebSocket(pages[0].webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let id = 0;
  const pending = new Map();
  const events = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); } else if (msg.method) events.push(msg);
  };
  const send = (method, params = {}) => new Promise((res) => {
    const n = ++id;
    pending.set(n, res);
    ws.send(JSON.stringify({ id: n, method, params }));
  });
  const evaluate = async (expression) => {
    const msg = await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (msg.result?.exceptionDetails) throw new Error(JSON.stringify(msg.result.exceptionDetails).slice(0, 600));
    return msg.result?.result?.value;
  };
  return { send, evaluate, events, close: () => ws.close() };
}

const CARD = (name) => `[...document.querySelectorAll('button[title=' + JSON.stringify(${JSON.stringify(name)}) + ']')].filter((b) => b.querySelector('svg') || b.disabled).pop()`;

// Runs in the page: what Chromium counts for the playing <video>.
const STATS = (label) => `(async () => {
  const v = document.querySelector('video');
  if (!v) return { label: ${JSON.stringify(label)}, video: false, t_ms: performance.now() };
  const q = v.getVideoPlaybackQuality();
  // One requestVideoFrameCallback: the compositor's own count of frames it was given (not left running).
  const presented = await new Promise((res) => {
    const t = setTimeout(() => res(null), 400);
    if (!v.requestVideoFrameCallback || v.paused) { clearTimeout(t); res(null); return; }
    v.requestVideoFrameCallback((_, m) => { clearTimeout(t); res(m.presentedFrames); });
  });
  return { label: ${JSON.stringify(label)}, video: true, t_ms: performance.now(), position_ms: Math.round(v.currentTime * 1000),
    duration_ms: Number.isFinite(v.duration) ? Math.round(v.duration * 1000) : null, paused: v.paused, ended: v.ended,
    muted: v.muted, volume: v.volume, ready_state: v.readyState, width: v.videoWidth, height: v.videoHeight,
    total_frames: q.totalVideoFrames, dropped_frames: q.droppedVideoFrames, corrupted_frames: q.corruptedVideoFrames,
    presented_frames: presented, visibility: document.visibilityState, has_focus: document.hasFocus(),
    error: v.error ? v.error.message : null };
})()`;

// Media domain: the newest value of every property of every player, and the pipeline's messages.
function mediaFacts(events) {
  const players = {};
  for (const e of events) {
    if (e.method === 'Media.playerPropertiesChanged') {
      const p = (players[e.params.playerId] ??= {});
      for (const { name, value } of e.params.properties) p[name] = value;
    }
  }
  const out = [];
  for (const p of Object.values(players)) {
    const pick = (k) => p[k] ?? null;
    let tracks = null;
    try { tracks = JSON.parse(p.kVideoTracks ?? 'null'); } catch {}
    const t = Array.isArray(tracks) ? tracks[0] : null;
    out.push({ video_decoder: pick('kVideoDecoderName'), platform_video_decoder: pick('kIsPlatformVideoDecoder'),
      audio_decoder: pick('kAudioDecoderName'), renderer: pick('kRendererName'), resolution: pick('kResolution'),
      framerate: pick('kFramerate'), codec: t ? (t.codec ?? null) : null, profile: t ? (t.profile ?? null) : null,
      video_playback_roughness: pick('kVideoPlaybackRoughness'), video_playback_freezing: pick('kVideoPlaybackFreezing') });
  }
  return out;
}

const c = await connect();
try {
  switch (cmd) {
    case 'find': {
      const [name, secs = '60'] = args;
      const t0 = Date.now();
      let r = null;
      while (!r && Date.now() - t0 < Number(secs) * 1000) {
        r = await c.evaluate(`(() => { const b = ${CARD(name)}; if (!b) return null; b.scrollIntoView({ block: 'center' });
          const q = b.getBoundingClientRect(); return { found: true, x: q.x, y: q.y, w: q.width, h: q.height, disabled: b.disabled,
          rows: document.querySelectorAll('button[data-channel-id]').length }; })()`);
        if (!r) await sleep(250);
      }
      if (!r) throw new Error(`no video card titled ${name} after ${secs} s`);
      console.log(JSON.stringify(r));
      break;
    }
    case 'play': {
      const [name] = args;
      await c.send('Media.enable');
      const armed = await c.evaluate(`(() => {
        if (!window.__perfMute) {
          window.__perfMute = { original: null, observer: new MutationObserver(() => {
            for (const v of document.querySelectorAll('video')) {
              if (window.__perfMute.original === null) window.__perfMute.original = v.muted;
              if (!v.muted) v.muted = true;
            }
          }) };
          window.__perfMute.observer.observe(document.documentElement, { childList: true, subtree: true });
        }
        const b = ${CARD(name)};
        if (!b) return { error: 'no card' };
        b.scrollIntoView({ block: 'center' });
        if (!b.disabled) b.click();
        return { clicked: !b.disabled, at_ms: performance.now() };
      })()`);
      if (armed?.error) throw new Error(`no video card titled ${name}`);
      const started = await c.evaluate(`(async () => {
        const t0 = performance.now();
        while (performance.now() - t0 < 30000) {
          const v = document.querySelector('video');
          if (v && v.muted && !v.paused && v.readyState >= 2 && v.currentTime > 0.05) return { first_picture_ms: Math.round(performance.now() - ${armed.at_ms}), muted: v.muted };
          if (v && v.error) return { error: v.error.message };
          await new Promise((r) => setTimeout(r, 20));
        }
        const v = document.querySelector('video');
        return { error: 'the video did not start within 30 s', video: !!v, muted: v ? v.muted : null, paused: v ? v.paused : null, ready_state: v ? v.readyState : null };
      })()`);
      if (started?.error) throw new Error(started.error + ' ' + JSON.stringify(started));
      await sleep(2500);   // the pipeline reports its decoder once the first frames are through
      const stats = await c.evaluate(STATS('playing'));
      console.log(JSON.stringify({ ...stats, ...started, clip: name, players: mediaFacts(c.events) }));
      break;
    }
    case 'stats':
      console.log(JSON.stringify(await c.evaluate(STATS(args[0] ?? ''))));
      break;
    case 'restart':
      await c.evaluate(`(async () => { const v = document.querySelector('video'); if (!v) throw new Error('no video'); v.currentTime = 0;
        if (v.paused) await v.play(); await new Promise((r) => setTimeout(r, 300)); })()`);
      console.log(JSON.stringify(await c.evaluate(STATS('restarted'))));
      break;
    case 'stop': {
      const r = await c.evaluate(`(async () => {
        const v = document.querySelector('video');
        if (v) v.pause();
        await new Promise((r) => setTimeout(r, 200));
        const m = window.__perfMute;
        if (m) { m.observer.disconnect(); if (v && m.original !== null && v.muted !== m.original) v.muted = m.original; delete window.__perfMute; }
        await new Promise((r) => setTimeout(r, 300));
        return { video: !!v, paused: v ? v.paused : null, muted: v ? v.muted : null };
      })()`);
      console.log(JSON.stringify({ ...(await c.evaluate(STATS('stopped'))), ...r }));
      break;
    }
    case 'gone': {
      const [name, secs = '30'] = args;
      const t0 = Date.now();
      let there = true;
      while (there && Date.now() - t0 < Number(secs) * 1000) {
        there = await c.evaluate(`!!(${CARD(name)})`);
        if (there) await sleep(250);
      }
      console.log(JSON.stringify({ card_gone: !there, video_element: await c.evaluate(`!!document.querySelector('video')`) }));
      break;
    }
    default:
      throw new Error(`unknown command ${cmd}`);
  }
} finally {
  c.close();
}

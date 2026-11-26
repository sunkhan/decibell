#!/usr/bin/env python3
"""electron-wheel-linux.py: how Chromium (the Electron client) scrolls the message list on a mouse-wheel notch and on
scroll keys, on Linux / X11 (XWayland), measured so a native client can reproduce it (gpui-client/docs/wheel-scroll.md).

  electron-wheel-linux.py [--notches N --spacing-ms M --direction down|up]      wheel notches (default: 1 down)
  electron-wheel-linux.py --pattern 'd@0,d@60,u@120'                            explicit notches (direction@ms)
  electron-wheel-linux.py --key PageDown [--count N --spacing-ms M]             key presses
  electron-wheel-linux.py --key ArrowDown --hold-ms 1500                        a held key (auto-repeat)
  electron-wheel-linux.py --inject cdp [--cdp-delta 12 --notches 10 --spacing-ms 8]   precise-pixel deltas instead
  electron-wheel-linux.py --reanalyze DIR                                       re-run the analysis, no input
  common: --runs R --start keep|bottom|middle|bottom-minus:PX|top-plus:PX|scroll:PX --label TEXT --trace --no-fit
          --port N --pid PID --no-place --bypass-app-keys --out DIR

Needs the Electron client running with a CDP port and a text channel open (tools/perf/electron-local.sh run, open
"Daily Gaming" general); --port / --pid default to electron-local.sh's instance. Results and how they were derived:
gpui-client/docs/wheel-scroll.md.

How it works
- Input: core X11 ButtonPress 4 / 5 (wheel up / down) and KeyPress / KeyRelease events sent with XSendEvent straight to
  the client's X window, stamped with the current CLOCK_MONOTONIC millisecond (the X server's clock and Chromium's
  TimeTicks). Chromium turns a core wheel button into the same ui::MouseWheelEvent a real notch makes (offset 120,
  kScrollByPixel: ui/events/x/events_x_utils.cc), so the scroll path from there on is the real one. Not XTest: under
  KWin, Xwayland forwards XTest through libei and KWin asks the owner for permission (kwin_eis_prompter), and XTest
  moves the real pointer. XSendEvent needs neither the pointer nor focus for the wheel. CDP Input.dispatchMouseEvent
  sends precise-pixel deltas, which Chromium never animates: --inject cdp uses it on purpose, to show that path.
- Keys need keyboard focus: the window is activated through KWin for key runs, and the message list is focused
  programmatically (tabindex=-1 + focus()), which is what Blink's keyboard scrolling needs (it starts from the focused
  node, or the last mouse-pressed one). The previous window gets focus back afterwards. --bypass-app-keys stops the
  app's window keydown listener (useTypeToFocusComposer: ArrowUp edits the last own message, printable keys such as
  Space focus the composer) so Chromium's own ArrowUp / Space scrolling can be measured.
- The window is placed (1280x800, centre of the primary output) and kept above for the burst (--no-place: as is), so
  it is visible: requestAnimationFrame runs.
- Recording: a requestAnimationFrame loop in the page stores (frame time, scrollTop, scrollHeight, an anchor row's
  offsetTop) every frame; wheel / keydown listeners (capture, passive) store each event's timeStamp, deltaY,
  deltaMode. The content position is scrollTop corrected by the anchor row's movement, so a page of older history
  prepended mid-animation (scroll anchoring) does not show up as a jump. --trace also records Chromium's own trace
  events (cc ScrollAnimationCreate / ScrollAnimationUpdateTarget durations, InputHandlerProxy::HandleGestureScroll*
  processing times, Scheduler::BeginImplFrame frame_time + receipt).
- Clocks: rAF timestamps count from navigation start, performance.now() / event.timeStamp from performance.timeOrigin
  (52 ms later in this Electron page). Every invocation first calibrates the offset exactly (a short trace: rAF times
  snapped onto BeginImplFrame frame_times, tied to performance.now() by a performance.mark); frame times are then
  converted into the event clock. Without that, onsets look ~52 ms late.
- Analysis per run: distance, onset latency (event -> first moved frame), duration, dropped frames; free fits of a
  single notch / key (cubic-bezier ease-in-out with free start and duration, smoothstep, exponential); and the
  reference model: a re-implementation of cc::ScrollOffsetAnimationCurve (Chromium 130) driven by the run's own
  event times and frame times, with its error at every frame: once with Chromium's traced processing times and frame
  receipts (--trace, "cc model with traced P"), once with a fitted per-event processing latency ("cc model").

Writes <out>/run-N.json (everything recorded + the analysis) and <out>/summary.json, prints one summary line per run.
Results go to tools/perf/results/wheel-linux-<time>[-label]/ (gitignored).
"""
import argparse
import base64
import json
import math
import os
import socket
import struct
import sys
import time
import urllib.request
from urllib.parse import urlparse

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, 'linux'))
import common  # noqa: E402

DEV_DIR = os.environ.get('DECIBELL_DEV_DIR') or os.path.expanduser('~/.cache/decibell-dev')
EL_STATE = os.path.join(DEV_DIR, 'electron-local')

# ---------------------------------------------------------------------------------------------------- CDP (stdlib)


class CDP:
    """Minimal Chrome DevTools Protocol client over a stdlib WebSocket."""

    def __init__(self, ws_url):
        u = urlparse(ws_url)
        self.s = socket.create_connection((u.hostname, u.port), timeout=60)
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.sendall(('GET %s HTTP/1.1\r\nHost: %s:%d\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n'
                        'Sec-WebSocket-Key: %s\r\nSec-WebSocket-Version: 13\r\n\r\n'
                        % (u.path, u.hostname, u.port, key)).encode())
        buf = b''
        while b'\r\n\r\n' not in buf:
            c = self.s.recv(4096)
            if not c:
                raise ConnectionError('CDP handshake: connection closed')
            buf += c
        head, self.buf = buf.split(b'\r\n\r\n', 1)
        if b' 101 ' not in head.split(b'\r\n')[0]:
            raise ConnectionError('CDP handshake refused: %r' % head[:200])
        self.n = 0
        self.events = []

    @staticmethod
    def page(port):
        ts = json.load(urllib.request.urlopen('http://127.0.0.1:%d/json' % port, timeout=5))
        pages = [t for t in ts if t.get('type') == 'page' and not t.get('url', '').startswith('devtools://')]
        if not pages:
            raise RuntimeError('no page target on CDP port %d' % port)
        return CDP(pages[0]['webSocketDebuggerUrl'])

    @staticmethod
    def browser(port):
        v = json.load(urllib.request.urlopen('http://127.0.0.1:%d/json/version' % port, timeout=5))
        return CDP(v['webSocketDebuggerUrl']), v

    def _read(self, n):
        while len(self.buf) < n:
            c = self.s.recv(1 << 20)
            if not c:
                raise ConnectionError('CDP connection closed')
            self.buf += c
        d, self.buf = self.buf[:n], self.buf[n:]
        return d

    def _send(self, data, op=1):
        n = len(data)
        hdr = bytes([0x80 | op])
        if n < 126:
            hdr += bytes([0x80 | n])
        elif n < 65536:
            hdr += bytes([0x80 | 126]) + struct.pack('>H', n)
        else:
            hdr += bytes([0x80 | 127]) + struct.pack('>Q', n)
        m = os.urandom(4)
        mask = (m * (n // 4 + 1))[:n]
        self.s.sendall(hdr + m + bytes(a ^ b for a, b in zip(data, mask)))

    def _recv(self):
        msg = b''
        while True:
            b0, b1 = self._read(2)
            op, n = b0 & 15, b1 & 127
            if n == 126:
                n = struct.unpack('>H', self._read(2))[0]
            elif n == 127:
                n = struct.unpack('>Q', self._read(8))[0]
            p = self._read(n)
            if op == 9:
                self._send(p, 10)
                continue
            if op == 8:
                raise ConnectionError('CDP websocket closed')
            if op == 10:
                continue
            msg += p
            if b0 & 0x80:
                return json.loads(msg)

    def call(self, method, params=None):
        self.n += 1
        i = self.n
        self._send(json.dumps({'id': i, 'method': method, 'params': params or {}}).encode())
        while True:
            m = self._recv()
            if m.get('id') == i:
                if 'error' in m:
                    raise RuntimeError('%s: %s' % (method, m['error']))
                return m.get('result', {})
            if 'method' in m:
                self.events.append(m)

    def wait_event(self, method, timeout=30):
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout:
            for k, m in enumerate(self.events):
                if m.get('method') == method:
                    return self.events.pop(k)
            m = self._recv()
            if 'method' in m:
                self.events.append(m)
        raise TimeoutError(method)

    def ev(self, expr):
        r = self.call('Runtime.evaluate', {'expression': expr, 'awaitPromise': True, 'returnByValue': True})
        if 'exceptionDetails' in r:
            raise RuntimeError('page: %s' % json.dumps(r['exceptionDetails'])[:1000])
        return r.get('result', {}).get('value')

    def close(self):
        try:
            self.s.close()
        except OSError:
            pass


# ---------------------------------------------------------------------------------------------------- page side

PAGE_JS = r"""
(() => {
  const old = window.__wl;
  if (old && old.cleanup) old.cleanup();
  // The message list's scroller: the nearest overflow-y auto/scroll ancestor of a message row (RealMessageList rows
  // carry data-key), else the largest scroll container (electron-scroll.mjs's rule).
  let sc = document.querySelector('[data-key]');
  while (sc && !['auto', 'scroll'].includes(getComputedStyle(sc).overflowY)) sc = sc.parentElement;
  if (!sc) {
    const all = [...document.querySelectorAll('*')].filter((e) => { const s = getComputedStyle(e);
      return (s.overflowY === 'auto' || s.overflowY === 'scroll') && e.scrollHeight > e.clientHeight + 100; });
    sc = all.sort((a, b) => b.clientWidth * b.clientHeight - a.clientWidth * a.clientHeight)[0];
  }
  if (!sc) return {error: 'no scroller'};
  const R = window.__wl = {sc, frames: [], wheel: [], keys: [], recording: false, bypass: false, lastChange: 0};
  const onWheel = (e) => { if (R.recording) R.wheel.push([e.timeStamp, e.deltaY, e.deltaMode, performance.now(), e.deltaX, e.wheelDeltaY]); };
  const onKey = (e) => {
    if (R.recording) R.keys.push([e.timeStamp, e.key, e.repeat, performance.now(), e.shiftKey, e.type]);
    // --bypass-app-keys: keep the app's window keydown listener (type-to-focus composer, ArrowUp = edit last own
    // message) from seeing the key, so Chromium's default scrolling runs. Default actions are not prevented.
    if (R.bypass && e.type === 'keydown' && (e.key === 'ArrowUp' || e.key.length === 1)) e.stopPropagation();
  };
  addEventListener('wheel', onWheel, {capture: true, passive: true});
  addEventListener('keydown', onKey, {capture: true});
  addEventListener('keyup', onKey, {capture: true});
  R.cleanup = () => { removeEventListener('wheel', onWheel, {capture: true}); removeEventListener('keydown', onKey, {capture: true});
    removeEventListener('keyup', onKey, {capture: true}); R.recording = false; R.unfocus && R.unfocus(); };
  const anchorRow = () => {
    const rows = [...sc.querySelectorAll('[data-key]')];
    const mid = sc.scrollTop + sc.clientHeight / 2;
    let best = null, bd = 1e18;
    for (const r of rows) { const d = Math.abs(r.offsetTop + r.offsetHeight / 2 - mid); if (d < bd) { bd = d; best = r; } }
    return best;
  };
  R.info = () => {
    const s = getComputedStyle(sc), r = sc.getBoundingClientRect();
    return {rect: [r.x, r.y, r.width, r.height], clientHeight: sc.clientHeight, clientWidth: sc.clientWidth,
            scrollTop: sc.scrollTop, scrollHeight: sc.scrollHeight, rows: sc.querySelectorAll('[data-key]').length,
            overflowAnchor: s.overflowAnchor, scrollBehavior: s.scrollBehavior, overscrollBehaviorY: s.overscrollBehaviorY,
            scrollSnapType: s.scrollSnapType, scrollPaddingTop: s.scrollPaddingTop, dpr: devicePixelRatio,
            inner: [innerWidth, innerHeight], screen: [screenX, screenY], visibility: document.visibilityState,
            hasFocus: document.hasFocus(), active: document.activeElement ? document.activeElement.tagName : null,
            ua: navigator.userAgent, now: performance.now(),
            // rAF timestamps count from navigation start, performance.now() / event.timeStamp from timeOrigin
            docOffsetLegacy: performance.timeOrigin - performance.timing.navigationStart};
  };
  R.position = async (spec) => {
    const max = () => sc.scrollHeight - sc.clientHeight;
    if (spec === 'keep') return R.info();
    let v;
    if (spec === 'bottom') v = max();
    else if (spec === 'middle') v = max() / 2;
    else if (spec.startsWith('bottom-minus:')) v = max() - Number(spec.split(':')[1]);
    else if (spec.startsWith('top-plus:')) v = Number(spec.split(':')[1]);
    else if (spec.startsWith('scroll:')) v = Number(spec.split(':')[1]);
    else throw new Error('unknown --start ' + spec);
    sc.scrollTop = v;
    // Let paging / trimming triggered by the jump settle: quiet for 400 ms (max 5 s).
    let last = [sc.scrollTop, sc.scrollHeight], quiet = performance.now(); const t0 = performance.now();
    while (performance.now() - t0 < 5000) {
      await new Promise((r) => setTimeout(r, 50));
      const cur = [sc.scrollTop, sc.scrollHeight];
      if (cur[0] !== last[0] || cur[1] !== last[1]) { last = cur; quiet = performance.now(); }
      else if (performance.now() - quiet > 400) break;
    }
    if (spec === 'bottom' || spec.startsWith('bottom-minus:')) { sc.scrollTop = spec === 'bottom' ? max() : max() - Number(spec.split(':')[1]); await new Promise((r) => setTimeout(r, 200)); }
    return R.info();
  };
  R.focusList = () => {
    // What a press in the list does for Blink's keyboard scrolling: the scroll chain starts at the list.
    const had = sc.hasAttribute('tabindex');
    if (document.activeElement && document.activeElement !== document.body) document.activeElement.blur();
    if (!had) sc.setAttribute('tabindex', '-1');
    sc.focus({preventScroll: true});
    R.unfocus = () => { if (document.activeElement === sc) sc.blur(); if (!had) sc.removeAttribute('tabindex'); R.unfocus = null; };
    return document.activeElement === sc;
  };
  R.start = (bypass) => {
    R.frames = []; R.wheel = []; R.keys = []; R.bypass = !!bypass;
    const a = anchorRow();
    R.anchor = a; R.anchorKey = a ? a.dataset.key : null;
    R.recording = true; R.t0 = performance.now(); R.lastChange = R.t0;
    let prev = null;
    const step = (t) => {
      if (!R.recording) return;
      let at = NaN;
      if (R.anchor && R.anchor.isConnected) at = R.anchor.offsetTop;
      const f = [t, sc.scrollTop, sc.scrollHeight, at, performance.now()];
      R.frames.push(f);
      if (!prev || prev[1] !== f[1] || prev[2] !== f[2]) R.lastChange = performance.now();
      prev = f;
      requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
    return {t0: R.t0, anchorKey: R.anchorKey, info: R.info()};
  };
  // Resolves once nothing moved for quietMs (at least minMs after the call), or after maxMs.
  R.settle = (quietMs, minMs, maxMs) => new Promise((res) => {
    const t0 = performance.now();
    const poll = () => {
      const now = performance.now();
      if ((now - t0 >= minMs && now - R.lastChange >= quietMs) || now - t0 >= maxMs) { R.recording = false; res(now - t0); }
      else setTimeout(poll, 25);
    };
    poll();
  });
  R.take = () => JSON.stringify({frames: R.frames, wheel: R.wheel, keys: R.keys, anchorKey: R.anchorKey, info: R.info()});
  return R.info();
})()
"""

# ---------------------------------------------------------------------------------------------------- X11


class X11Target:
    """The client's X window and XSendEvent injection (no XTest: no pointer movement, no input-emulation prompt)."""

    def __init__(self, pid):
        from Xlib import X, XK, display
        from Xlib.protocol import event
        self.X, self.XK, self.event = X, XK, event
        self.d = display.Display()
        self.root = self.d.screen().root
        a_list = self.d.intern_atom('_NET_CLIENT_LIST')
        a_pid = self.d.intern_atom('_NET_WM_PID')
        wins = []
        prop = self.root.get_full_property(a_list, X.AnyPropertyType)
        for xid in (prop.value if prop else []):
            w = self.d.create_resource_object('window', xid)
            try:
                p = w.get_full_property(a_pid, X.AnyPropertyType)
            except Exception:
                continue
            if p and len(p.value) and int(p.value[0]) == pid:
                g = w.get_geometry()
                wins.append((g.width * g.height, w))
        if not wins:
            raise RuntimeError('no X11 window with _NET_WM_PID %d (is the client an XWayland client?)' % pid)
        self.win = sorted(wins, key=lambda x: -x[0])[0][1]
        self.refresh()

    def refresh(self):
        g = self.win.get_geometry()
        tc = self.win.translate_coords(self.root, 0, 0)
        self.wx, self.wy, self.ww, self.wh = -tc.x, -tc.y, g.width, g.height

    @staticmethod
    def xtime():
        # Xwayland stamps events with CLOCK_MONOTONIC milliseconds; Chromium maps X time onto TimeTicks (the same clock).
        return int(time.clock_gettime_ns(time.CLOCK_MONOTONIC) // 1_000_000) & 0xffffffff

    def button(self, detail, x, y):
        X, ev = self.X, self.event
        t = self.xtime()
        for typ, mask in ((ev.ButtonPress, X.ButtonPressMask), (ev.ButtonRelease, X.ButtonReleaseMask)):
            e = typ(time=t, root=self.root, window=self.win, same_screen=1, child=X.NONE,
                    root_x=self.wx + x, root_y=self.wy + y, event_x=x, event_y=y, state=0, detail=detail)
            self.win.send_event(e, event_mask=mask, propagate=False)
        self.d.flush()
        return t

    def keycode(self, name):
        ks = self.XK.string_to_keysym(name)
        if not ks:
            raise ValueError('unknown keysym %s' % name)
        kc = self.d.keysym_to_keycode(ks)
        if not kc:
            raise ValueError('no keycode for %s' % name)
        return kc

    def key(self, keycode, press, state=0, x=10, y=10):
        X, ev = self.X, self.event
        t = self.xtime()
        typ, mask = (ev.KeyPress, X.KeyPressMask) if press else (ev.KeyRelease, X.KeyReleaseMask)
        e = typ(time=t, root=self.root, window=self.win, same_screen=1, child=X.NONE,
                root_x=self.wx + x, root_y=self.wy + y, event_x=x, event_y=y, state=state, detail=keycode)
        self.win.send_event(e, event_mask=mask, propagate=False)
        self.d.flush()
        return t


# ---------------------------------------------------------------------------------------------------- KWin

KWIN_JS = r"""
var PIDS = %(pids)s, ARGS = %(args)s;
var all = workspace.windowList(), w = null;
for (var i = 0; i < all.length; i++) {
  var c = all[i];
  if (PIDS[c.pid] === true && c.normalWindow && (!w || c.stackingOrder > w.stackingOrder)) w = c;
}
var res = {prevActive: info(workspace.activeWindow)};
if (ARGS.activateId) {
  for (var i = 0; i < all.length; i++) if (String(all[i].internalId) == ARGS.activateId) { workspace.activeWindow = all[i]; res.reactivated = info(all[i]); }
}
if (!w) { res.error = "no window"; }
else {
  if (ARGS.place) {
    if (w.minimized) w.minimized = false;
    if (w.fullScreen) w.fullScreen = false;
    if (w.maximizeMode) w.setMaximize(false, false);
    w.frameGeometry = {x: ARGS.place[0], y: ARGS.place[1], width: ARGS.place[2], height: ARGS.place[3]};
  }
  if (ARGS.keepAbove === true || ARGS.keepAbove === false) w.keepAbove = ARGS.keepAbove;
  if (ARGS.activate) workspace.activeWindow = w;
  res.window = info(w);
  res.keepAbove = w.keepAbove;
}
res.activeNow = info(workspace.activeWindow);
send("result", res);
"""


def kwin_do(pid, **args):
    import kwin
    if not kwin.available():
        return {'error': 'KWin scripting not available'}
    return kwin.run_once('wheel-' + '-'.join(sorted(args)), KWIN_JS % {
        'pids': json.dumps({str(pid): True}), 'args': json.dumps(args)})


def primary_centre(w, h):
    disp = common.displays()
    if not disp:
        return None, None
    prim = sorted(disp, key=lambda d: (d.get('priority') or 99))[0]
    w0, h0 = [int(x) for x in prim['mode'].split('x')]
    scale = prim.get('scale') or 1
    lw, lh = int(w0 / scale), int(h0 / scale)
    return [int(prim['pos']['x'] + (lw - w) / 2), int(prim['pos']['y'] + (lh - h) / 2), w, h], prim


# ---------------------------------------------------------------------------------------------------- model

class Bezier:
    """gfx::CubicBezier for control points inside [0,1] in x (ui/gfx/geometry/cubic_bezier.cc)."""

    def __init__(self, x1, y1, x2, y2):
        self.p = (x1, y1, x2, y2)
        self.cx = 3.0 * x1
        self.bx = 3.0 * (x2 - x1) - self.cx
        self.ax = 1.0 - self.cx - self.bx
        self.cy = 3.0 * y1
        self.by = 3.0 * (y2 - y1) - self.cy
        self.ay = 1.0 - self.cy - self.by

    def _x(self, t):
        return ((self.ax * t + self.bx) * t + self.cx) * t

    def _y(self, t):
        return ((self.ay * t + self.by) * t + self.cy) * t

    def _dx(self, t):
        return (3.0 * self.ax * t + 2.0 * self.bx) * t + self.cx

    def _dy(self, t):
        return (3.0 * self.ay * t + 2.0 * self.by) * t + self.cy

    def _solve(self, x):
        t = x
        for _ in range(8):
            e = self._x(t) - x
            if abs(e) < 1e-9:
                return t
            d = self._dx(t)
            if abs(d) < 1e-9:
                break
            t -= e / d
        lo, hi, t = 0.0, 1.0, x
        for _ in range(60):
            v = self._x(t)
            if abs(v - x) < 1e-10:
                break
            if v < x:
                lo = t
            else:
                hi = t
            t = (lo + hi) / 2
        return t

    def value(self, x):
        if x <= 0:
            return 0.0
        if x >= 1:
            return 1.0
        return self._y(self._solve(x))

    def slope(self, x):
        x = min(1.0, max(0.0, x))
        t = self._solve(x)
        dx, dy = self._dx(t), self._dy(t)
        if not dx and not dy:
            return 0.0
        return dy / dx if dx else float('inf')


EASE_IN_OUT = (0.42, 0.0, 0.58, 1.0)


class CCCurve:
    """cc::ScrollOffsetAnimationCurve, kEaseInOut (Chromium 130, cc/animation/scroll_offset_animation_curve.cc),
    one axis, times in ms. behavior: 'inverse_delta' (wheel), 'constant' (keyboard), 'delta_based' (programmatic)."""
    EPS = 0.01

    def __init__(self, target, behavior):
        self.target = target
        self.behavior = behavior
        self.tf = Bezier(*EASE_IN_OUT)
        self.initial = 0.0
        self.total = 0.0
        self.last_retarget = 0.0

    @staticmethod
    def segment_ms(delta, behavior, delayed_by=0.0):
        d = abs(delta)
        if behavior == 'constant':
            frames = 9.0
        elif behavior == 'delta_based':
            frames = min(math.sqrt(d), 0.7 * 60.0)
        else:  # inverse_delta: 12 frames up to 120 px, 6 frames from 480 px, linear between
            slope = (6.0 - 12.0) / (480.0 - 120.0)
            frames = min(max(12.0 - 120.0 * slope + d * slope, 6.0), 12.0)
        return max(0.0, frames / 60.0 * 1000.0 - delayed_by)

    def set_initial(self, value, delayed_by=0.0):
        self.initial = value
        self.total = self.segment_ms(self.target - value, self.behavior, delayed_by)

    def value(self, t):
        dur = self.total - self.last_retarget
        t -= self.last_retarget
        if dur <= 0 or t >= dur:
            return self.target
        if t <= 0:
            return self.initial
        return self.initial + self.tf.value(t / dur) * (self.target - self.initial)

    def velocity(self, t):  # px per ms
        dur = self.total - self.last_retarget
        if dur <= 0:
            return 0.0
        return self.tf.slope((t - self.last_retarget) / dur) * (self.target - self.initial) / dur

    def update_target(self, t, new_target):
        delayed_by = max(0.0, self.last_retarget - t)
        t = max(t, self.last_retarget)
        if abs(self.target - new_target) < self.EPS:
            self.target = new_target
            return
        cur = self.value(t)
        new_delta = new_target - cur
        if abs(new_delta) < self.EPS:
            self.last_retarget = t
            self.total = t
            self.target = new_target
            return
        if self.total - self.last_retarget == 0:
            self.total = self.segment_ms(new_delta, self.behavior, delayed_by)
            self.target = new_target
            return
        vel = self.velocity(t)
        seg = self.segment_ms(new_delta, self.behavior, delayed_by)
        if abs(vel) < self.EPS / 1000.0:
            bound = float('inf')
        else:
            b = new_delta / vel * 2.5  # VelocityBasedDurationBound: (delta / velocity) * 2.5, in ms here
            bound = float('inf') if b < 0 else b
        new_dur = min(seg, bound)
        if new_dur / 1000.0 < self.EPS:
            self.target = new_target
            self.total = t
            return
        slope = max(-1000.0, min(1000.0, vel * new_dur / new_delta))
        self.tf = Bezier(0.42, 0.42 * slope, 0.58, 1.0)
        self.initial = cur
        self.target = new_target
        self.total = t + new_dur
        self.last_retarget = t


def simulate(kind, events, frame_ts, y0, limits, interval, lats, rho=0.0, receipts=None, within=0.0):
    """Positions at each frame time from the cc model.
    kind 'wheel': impl-thread animation (cc InputHandler::ScrollLatchedScroller): created at processing time P = E + L
      with duration segment(delta) - (P - E), first ticked at the next BeginImplFrame with local time = one frame
      interval; retargeted at P with t = local(current frame) - (P - E) and target = old target + delta (clamped).
      The "current frame" is the last BeginImplFrame the compositor received by P (CurrentBeginFrameArgs), which
      arrives rho ms after its frame_time (receipts: exact receipt times per frame from a trace).
    kind 'key': Blink ScrollAnimator (main thread, run on the compositor): created at P with start_time = P, constant
      150 ms; retargeted at P + L with t = P + L - start_time, target = old target + step (clamped).
    events: [(E ms, delta px)], limits: [(lo, hi)] per event (content coordinates), lats: [L ms] per event."""
    out = []
    pos = y0
    curve = None
    start = None
    last_frame = None
    k = 0
    evs = sorted(zip([e[0] + lats[i] for i, e in enumerate(events)], range(len(events))))
    for fi, F in enumerate(frame_ts):
        R = receipts[fi] if receipts is not None else F + rho
        # within: input dispatched at the start of a BeginImplFrame is handled before that frame's args become
        # current and before its Animate, i.e. as if it had arrived just before the frame
        while k < len(evs) and evs[k][0] < R + within:
            P, i = evs[k]
            E, delta = events[i]
            lo, hi = limits[i]
            k += 1
            if curve is not None:
                if kind == 'wheel':
                    t = (last_frame - start + interval) - (P - E) if (start is not None and last_frame is not None) \
                        else -(P - E)
                else:
                    t = P - start
                curve.update_target(t, min(hi, max(lo, curve.target + delta)))
            else:
                target = min(hi, max(lo, pos + delta))
                if abs(target - pos) <= 0.1:
                    pos = target
                    continue
                if kind == 'wheel':
                    curve = CCCurve(target, 'inverse_delta')
                    curve.set_initial(pos, P - E)
                    start = None
                else:
                    curve = CCCurve(target, 'constant')
                    curve.set_initial(pos, 0.0)
                    start = P
        if curve is not None:
            if kind == 'wheel':
                if start is None:
                    start = F
                local = F - start + interval
            else:
                local = F - start
            if local >= curve.total:
                pos = curve.target
                curve = None
            else:
                pos = curve.value(local)
        out.append(pos)
        last_frame = F
    return out


def fit_model(kind, events, frames, y, limits, interval, refine=True, rho=0.5, impl_interval=None):
    """Fits the processing latency of the events (P = E + L): first a joint fit of L_first and one shared L_rest
    (and a 0 / 1 frame main-thread lag of the sampled scrollTop), then a per-event refinement (coordinate descent,
    +-12 ms around it). Errors are over every recorded frame."""
    ts = [f[0] for f in frames]
    n = len(events)
    ii = impl_interval or interval
    grid = impl_grid(ts, ii)

    def sse(lats, lag):
        m = shift(simulate_sampled(kind, events, ts, grid, y[0], limits, ii, lats,
                                   rho if kind == 'wheel' else 0.0), lag)
        return sum((a - b) ** 2 for a, b in zip(y, m)), m

    hi = 120.0 if kind == 'wheel' else 40.0
    coarse = [x * 4.0 for x in range(int(hi / 4) + 1)]
    best = None
    for lag in (0, 1):
        for L1 in coarse:
            for L2 in (coarse if n > 1 else [0.0]):
                e, _ = sse([L1] + [L2] * (n - 1), lag)
                if best is None or e < best[0]:
                    best = (e, lag, L1, L2)
    _, lag, L1, L2 = best
    for _ in range(2):  # refine the pair at 0.25 ms
        for which in (0, 1):
            if which == 1 and n == 1:
                continue
            c = L1 if which == 0 else L2
            for L in [c + (k - 16) * 0.25 for k in range(33)]:
                if L < 0:
                    continue
                lats = [L] + [L2] * (n - 1) if which == 0 else [L1] + [L] * (n - 1)
                e, _ = sse(lats, lag)
                if e < best[0]:
                    best = (e, lag, lats[0], lats[-1] if n > 1 else 0.0)
            _, lag, L1, L2 = best
    lats = [L1] + [L2] * (n - 1)
    e_joint, m_joint = sse(lats, lag)
    rms_joint = math.sqrt(e_joint / len(ts))
    res = {'lag_frames': lag, 'latency_first_ms': L1, 'latency_rest_ms': L2 if n > 1 else None,
           'rms_joint_px': rms_joint, 'max_abs_joint_px': max(abs(a - b) for a, b in zip(y, m_joint))}
    cur_e, m = e_joint, m_joint
    if refine and n > 1:
        for _ in range(2):
            for i in range(n):
                c = lats[i]
                for L in [c + (k - 12) * 1.0 for k in range(25)]:
                    if L < 0:
                        continue
                    trial = list(lats)
                    trial[i] = L
                    e, mm = sse(trial, lag)
                    if e < cur_e - 1e-9:
                        cur_e, m, lats = e, mm, trial
    res.update({'latency_ms': lats, 'rms_px': math.sqrt(cur_e / len(ts)),
                'max_abs_px': max(abs(a - b) for a, b in zip(y, m)), 'model': m})
    return res


def shift(m, lag):
    return m if lag == 0 else ([m[0]] * lag + m[:-lag])


def impl_grid(ts, interval):
    """The compositor's frame times: the rAF frame times plus the impl frames between them that the main thread
    skipped (cc ticks the scroll animation every BeginImplFrame; rAF may run at half rate)."""
    out = [ts[0]]
    for a, b in zip(ts, ts[1:]):
        n = int(round((b - a) / interval))
        for j in range(1, n):
            out.append(a + (b - a) * j / n)
        out.append(b)
    return out


def sample(impl_ts, vals, ts):
    """Values of the impl-frame series at the rAF frame times (same frame_time within 0.6 ms, else the latest before)."""
    import bisect
    out = []
    for t in ts:
        j = bisect.bisect_right(impl_ts, t + 0.6) - 1
        out.append(vals[max(0, j)])
    return out


def simulate_sampled(kind, events, ts, impl_ts, y0, limits, interval, lats, rho=0.0, receipts=None, within=0.0):
    return sample(impl_ts, simulate(kind, events, impl_ts, y0, limits, interval, lats, rho, receipts, within), ts)


def free_fits(ts, y, y0, y1, onset_t, end_t):
    """Single-movement fits over the frames around the movement: ease-in-out bezier, smoothstep, exponential."""
    delta = y1 - y0
    if abs(delta) < 1:
        return {}
    win = [(t, v) for t, v in zip(ts, y) if onset_t - 20 <= t <= end_t + 30]
    bz = Bezier(*EASE_IN_OUT)

    def err_for(fn, t0, p):
        e = [v - (y0 + delta * fn((t - t0), p)) for t, v in win]
        return math.sqrt(sum(x * x for x in e) / len(e)), max(abs(x) for x in e)

    def ease(dt, D):
        return bz.value(dt / D)

    def smooth(dt, D):
        x = min(1.0, max(0.0, dt / D))
        return x * x * (3 - 2 * x)

    def expo(dt, tau):
        return 0.0 if dt <= 0 else 1.0 - math.exp(-dt / tau)

    res = {}
    for name, fn, prange in (('ease_in_out_bezier', ease, (40.0, 400.0, 2.0)),
                             ('smoothstep', smooth, (40.0, 400.0, 2.0)),
                             ('exponential', expo, (5.0, 150.0, 1.0))):
        best = None
        t_lo = onset_t - 40
        for stage in range(3):
            if best is None:
                ps = [prange[0] + k * prange[2] for k in range(int((prange[1] - prange[0]) / prange[2]) + 1)]
                t0s = [t_lo + k * 1.0 for k in range(45)]
            else:
                _, bt0, bp = best
                step_p = prange[2] / (5 ** stage)
                step_t = 1.0 / (5 ** stage)
                ps = [bp + (k - 10) * step_p for k in range(21)]
                t0s = [bt0 + (k - 10) * step_t for k in range(21)]
            for p in ps:
                if p <= 0:
                    continue
                for t0 in t0s:
                    r, _ = err_for(fn, t0, p)
                    if best is None or r < best[0]:
                        best = (r, t0, p)
        r, mx = err_for(fn, best[1], best[2])
        res[name] = {'t0_ms': round(best[1], 2), ('tau_ms' if name == 'exponential' else 'duration_ms'): round(best[2], 2),
                     'rms_px': round(r, 3), 'max_abs_px': round(mx, 3)}
    return res


# ---------------------------------------------------------------------------------------------------- analysis

def content_positions(frames):
    """scrollTop corrected by the anchor row's movement (prepends above it shift scrollTop and the row by the same)."""
    a0 = next((f[3] for f in frames if f[3] is not None and not (isinstance(f[3], float) and math.isnan(f[3]))), None)
    y = []
    last_shift = 0.0
    for f in frames:
        a = f[3]
        if a is not None and not (isinstance(a, float) and math.isnan(a)) and a0 is not None:
            last_shift = a - a0
        y.append(f[1] - last_shift)
    return y


def calibrate_frame_clock(port, page, guess):
    """Exact rAF-clock offset (timeOrigin - navigationStart) for this page: rAF times of ~40 frames snapped onto a
    short trace's BeginImplFrame frame_times; a performance.mark() ties the trace clock to performance.now()."""
    b, _ = CDP.browser(port)
    b.call('Tracing.start', {'transferMode': 'ReturnAsStream', 'traceConfig': {
        'recordMode': 'recordAsMuchAsPossible', 'includedCategories': ['cc', 'blink.user_timing']}})
    cal = page.ev("""new Promise((r) => { const out = []; const mark = performance.mark('wl-cal').startTime;
        const s = (t) => { out.push(t); if (out.length < 40) requestAnimationFrame(s); else r({mark, raf: out}); };
        requestAnimationFrame(s); })""")
    b.call('Tracing.end')
    ev = b.wait_event('Tracing.tracingComplete', 60)
    data = read_stream(b, ev['params'].get('stream'))
    b.close()
    try:
        tr = json.loads(data)
    except ValueError:
        return None
    evs = tr['traceEvents'] if isinstance(tr, dict) else tr
    mark = [e for e in evs if e.get('name') == 'wl-cal']
    if not mark:
        return None
    origin = mark[0]['ts'] / 1000.0 - cal['mark']  # TimeTicks ms of performance.now() == 0
    fts = []
    for e in evs:
        if e.get('name') == 'Scheduler::BeginImplFrame':
            a = e.get('args') or {}
            ft = (a.get('args') or a).get('frame_time_us')
            if ft:
                fts.append(ft / 1000.0 - origin)
    fts.sort()
    off = snap_offset(cal['raf'], fts, guess) if fts else None
    sp = sorted(b - a for a, b in zip(fts, fts[1:]))
    return off, (sp[len(sp) // 2] if sp else None)


def read_stream(b, handle):
    data = b''
    while handle:
        r = b.call('IO.read', {'handle': handle, 'size': 1 << 20})
        chunk = r.get('data', '')
        data += base64.b64decode(chunk) if r.get('base64Encoded') else chunk.encode()
        if r.get('eof'):
            break
    if handle:
        b.call('IO.close', {'handle': handle})
    return data


def frame_clock_offset(run):
    """rAF timestamps are relative to navigation start, performance.now() and event.timeStamp to
    performance.timeOrigin. offset = timeOrigin - navigationStart: (legacy - 1, legacy] from the integer-ms
    performance.timing.navigationStart, and >= max(rAF time - performance.now() in the callback)."""
    legacy = run['info'].get('docOffsetLegacy') or 0.0
    lower = max(f[0] - f[4] for f in run['frames'])
    off = min(legacy, max(lower, legacy - 1.0)) if legacy else lower
    if run.get('frame_clock_offset_calibrated') is not None:
        off = run['frame_clock_offset_calibrated']
    return off, legacy, lower


def trace_begin_frames(run):
    """[(frame_time, receipt)] in the performance.now() clock from the trace's Scheduler::BeginImplFrame events."""
    origin = run.get('perf_origin_mono_ms')
    if not run.get('trace') or origin is None:
        return []
    out = []
    for e in run['trace']:
        if e.get('name') != 'Scheduler::BeginImplFrame':
            continue
        a = e.get('args') or {}
        ft = (a.get('args') or a).get('frame_time_us')
        if ft:
            out.append((ft / 1000.0 - origin, e['ts'] / 1000.0 - origin))
    return sorted(out)


def snap_offset(raw_ts, frame_times, guess):
    """The rAF clock offset that puts the rAF times onto the traced BeginImplFrame frame_times."""
    import bisect
    best = None
    sample = raw_ts[::3] or raw_ts
    for k in range(-250, 251):
        cand = guess + k * 0.01
        e = []
        for t in sample:
            x = t - cand
            j = bisect.bisect_left(frame_times, x)
            d = min(abs(frame_times[q] - x) for q in (j - 1, j) if 0 <= q < len(frame_times))
            e.append(d)
        e.sort()
        med = e[len(e) // 2]
        if best is None or med < best[0]:
            best = (med, cand)
    return best[1] if best and best[0] < 0.2 else None


def analyze(run, kind, step_px=None, fit=True):
    frames = run['frames']
    if len(frames) < 3:
        return {'error': 'no frames'}
    off, legacy, lower = frame_clock_offset(run)
    bif = trace_begin_frames(run)
    off_trace = snap_offset([f[0] for f in frames], [f for f, _ in bif], off) if bif else None
    if off_trace is not None:
        off = off_trace
    # Frames in the event clock (performance.now()); the raw rAF times stay in run['frames'].
    frames = [[f[0] - off] + list(f[1:]) for f in frames]
    ts = [f[0] for f in frames]
    y = content_positions(frames)
    ivs = [b - a for a, b in zip(ts, ts[1:])]
    nominal = sorted(ivs)[len(ivs) // 2]
    dropped = sum(max(0, round(d / nominal) - 1) for d in ivs if d > 1.5 * nominal)
    ev_times = [w[0] for w in run['wheel']] if kind == 'wheel' else \
        [k[3] for k in run['keys'] if k[5] == 'keydown']
    if not ev_times:
        return {'error': 'no input events reached the page', 'frame_interval_ms': nominal}
    e0 = ev_times[0]
    i0 = max([k for k, t in enumerate(ts) if t <= e0] or [0])
    y0 = y[i0]
    y1 = y[-1]
    onset = next((k for k in range(i0, len(y)) if abs(y[k] - y0) >= 0.5), None)
    end = None
    if onset is not None:
        end = len(y) - 1
        while end > onset and abs(y[end - 1] - y1) < 0.5:
            end -= 1
    # dropped frames inside the movement only
    mv_drop = 0
    if onset is not None:
        for a, b in zip(ts[max(0, onset - 1):end + 1], ts[max(0, onset):end + 1]):
            d = b - a
            if d > 1.5 * nominal:
                mv_drop += round(d / nominal) - 1
    out = {
        'frame_clock_offset_ms': round(off, 3), 'frame_clock_offset_bounds_ms': [round(lower, 3), round(legacy, 3)],
        'frames': len(frames), 'frame_interval_ms': round(nominal, 3), 'refresh_hz_from_frames': round(1000 / nominal, 1),
        'dropped_frames_total': dropped, 'dropped_frames_in_motion': mv_drop,
        'events': len(ev_times), 'distance_px': round(y1 - y0, 2),
        'scrolltop_fractional': any(abs(f[1] - round(f[1])) > 1e-6 for f in frames),
    }
    if onset is not None:
        out['onset_latency_ms'] = round(ts[onset] - e0, 2)
        out['last_change_after_event_ms'] = round(ts[end] - e0, 2)
        out['visible_duration_ms'] = round(ts[end] - ts[onset], 2)
    if kind == 'wheel':
        out['wheel'] = [{'timeStamp': round(w[0], 3), 'deltaY': w[1], 'deltaMode': w[2], 'wheelDeltaY': w[5],
                         'dispatch_after_ms': round(w[3] - w[0], 2)} for w in run['wheel']]
    else:
        out['keys'] = [{'timeStamp': round(k[0], 3), 'key': k[1], 'repeat': k[2], 'type': k[5],
                        'dispatch_after_ms': round(k[3] - k[0], 2)} for k in run['keys']]
    if fit and len(ev_times) == 1 and onset is not None:
        out['free_fits'] = free_fits(ts, y, y0, y1, ts[onset], ts[end])
    # cc reference model
    lims = []
    for t in ev_times:
        k = max([j for j, tt in enumerate(ts) if tt <= t] or [0])
        f = frames[k]
        shift_now = f[1] - y[k]
        lims.append((0.0 - shift_now, (f[2] - run['info']['clientHeight']) - shift_now))
    if kind == 'wheel':
        evs = [(w[0], w[1]) for w in run['wheel']]
    else:
        evs = []
        for k in run['keys']:
            if k[5] != 'keydown':
                continue
            evs.append((k[3], step_px(k[1], k[4], run['info'])))
    if fit and evs and kind == 'wheel' and run.get('trace'):
        # Processing times straight from Chromium: each InputHandlerProxy::HandleGestureScrollUpdate (impl thread).
        origin = run.get('perf_origin_mono_ms')
        gsu = sorted(e['ts'] / 1000.0 - origin for e in run['trace']
                     if e.get('name') == 'InputHandlerProxy::HandleGestureScrollUpdate')
        receipts = None
        if bif:
            rhos = sorted(r - f for f, r in bif)
            med_rho = rhos[len(rhos) // 2]
            out['begin_impl_frame_receipt_ms'] = {'median': round(med_rho, 2), 'min': round(rhos[0], 2),
                                                  'max': round(rhos[-1], 2), 'n': len(rhos)}
            out['frame_clock_offset_trace_ms'] = None if off_trace is None else round(off_trace, 3)
            win = [(f, r) for f, r in bif if ts[0] - 20 <= f <= ts[-1] + 5]
            impl_ts = [f for f, _ in win]
            receipts = [r for _, r in win]
            sp = sorted(b - a for a, b in zip(impl_ts, impl_ts[1:]))
            impl_iv = sp[len(sp) // 2] if sp else nominal
            out['impl_frame_interval_ms'] = round(impl_iv, 3)
        if len(gsu) == len(evs):
            lats = [p - e[0] for p, e in zip(gsu, evs)]
            best = None
            for lag in (0, 1):
                if receipts:
                    mm = shift(simulate_sampled(kind, evs, ts, impl_ts, y[0], lims, impl_iv, lats, receipts=receipts), lag)
                else:
                    mm = shift(simulate(kind, evs, ts, y[0], lims, nominal, lats, 0.5), lag)
                errs = [a - b for a, b in zip(y, mm)]
                rms = math.sqrt(sum(x * x for x in errs) / len(errs))
                if best is None or rms < best[0]:
                    best = (rms, lag, max(abs(x) for x in errs), mm)
            out['cc_model_trace'] = {'latency_ms': [round(x, 2) for x in lats], 'lag_frames': best[1],
                                     'rms_px': round(best[0], 3), 'max_abs_px': round(best[2], 3)}
            run['model_positions_trace'] = [round(v, 3) for v in best[3]]
    if fit and evs and not (out.get('cc_model_trace') and len(evs) > 6):  # the traced model is enough there
        iv = out.get('impl_frame_interval_ms') or run.get('impl_interval_calibrated')
        if not iv:
            sp = sorted(b - a for a, b in zip(ts, ts[1:]))
            iv = sp[len(sp) // 10]
        m = fit_model(kind, evs, frames, y, lims, nominal, impl_interval=iv)
        model = m.pop('model')
        out['cc_model'] = {k: (round(v, 3) if isinstance(v, float) else v) for k, v in m.items()}
        out['cc_model']['latency_ms'] = [round(x, 1) for x in m['latency_ms']]
        run['model_positions'] = [round(v, 3) for v in model]
    run['content_positions'] = [round(v, 3) for v in y]
    return out


def key_step(info):
    ch = info['clientHeight']

    def step(key, shift, _info=None):
        if key == 'ArrowDown':
            return 40.0
        if key == 'ArrowUp':
            return -40.0
        page = float(int(ch * 0.875))
        if key == 'PageDown' or (key == ' ' and not shift):
            return page
        if key == 'PageUp' or (key == ' ' and shift):
            return -page
        if key == 'End':
            return 1e9
        if key == 'Home':
            return -1e9
        return 0.0
    return step


# ---------------------------------------------------------------------------------------------------- input plans

KEYSYMS = {'ArrowDown': 'Down', 'ArrowUp': 'Up', 'PageDown': 'Page_Down', 'PageUp': 'Page_Up', 'Home': 'Home',
           'End': 'End', 'space': 'space', 'Space': 'space', 'shift+space': 'space', 'Shift+Space': 'space'}


def plan(a):
    """[(t_ms, action, arg)]: ('wheel', 4|5), ('kp'|'kr', (keysym, state))."""
    out = []
    if a.key:
        ks = KEYSYMS.get(a.key, a.key)
        state = 1 if a.key.lower().startswith('shift+') else 0
        if a.hold_ms:
            out.append((0.0, 'kp', (ks, state)))
            t = a.repeat_delay
            while t < a.hold_ms:
                out.append((t, 'kp', (ks, state)))  # detectable auto-repeat: presses without releases
                t += 1000.0 / a.repeat_rate
            out.append((float(a.hold_ms), 'kr', (ks, state)))
        else:
            for i in range(a.count):
                t = i * a.spacing_ms
                out.append((t, 'kp', (ks, state)))
                out.append((t + min(30.0, a.spacing_ms / 2 if a.count > 1 else 30.0), 'kr', (ks, state)))
        return sorted(out, key=lambda x: x[0])
    if a.pattern:
        for item in a.pattern.split(','):
            d, _, t = item.strip().partition('@')
            out.append((float(t or 0), 'wheel', 5 if d.lower().startswith('d') else 4))
        return sorted(out, key=lambda x: x[0])
    for i in range(a.notches):
        out.append((i * a.spacing_ms, 'wheel', 5 if a.direction == 'down' else 4))
    return out


def sleep_until(t_ns):
    while True:
        now = time.monotonic_ns()
        left = t_ns - now
        if left <= 0:
            return
        if left > 2_000_000:
            time.sleep((left - 1_500_000) / 1e9)


# ---------------------------------------------------------------------------------------------------- trace

TRACE_NAMES = ('Scheduler::BeginImplFrame', 'ScrollAnimationCreate', 'ScrollAnimationUpdateTarget', 'InputHandlerProxy::HandleGestureScrollUpdate',
               'InputHandlerProxy::HandleGestureScrollBegin', 'ScrollAnimator::scroll', 'ScrollAnimationAbort',
               'scroll animation adjusted', 'InputHandler::ScrollUpdate', 'CreateNewAnimation',
               'UpdateExistingAnimation', 'Didn\'t Update Animation', 'no scroll animation due to small delta')


def trace_start(port):
    b, _ = CDP.browser(port)
    b.call('Tracing.start', {'transferMode': 'ReturnAsStream',
                             'traceConfig': {'recordMode': 'recordAsMuchAsPossible',
                                             'includedCategories': ['cc', 'input', 'blink']}})
    return b


def trace_stop(b):
    b.call('Tracing.end')
    ev = b.wait_event('Tracing.tracingComplete', 60)
    data = read_stream(b, ev['params'].get('stream'))
    b.close()
    try:
        tr = json.loads(data)
    except ValueError:
        return []
    evs = tr['traceEvents'] if isinstance(tr, dict) else tr
    return [e for e in evs if e.get('name') in TRACE_NAMES]


# ---------------------------------------------------------------------------------------------------- main

def read_state(name):
    try:
        with open(os.path.join(EL_STATE, name)) as f:
            return f.read().strip()
    except OSError:
        return ''


def main(argv):
    ap = argparse.ArgumentParser(prog='electron-wheel-linux.py', description=__doc__.split('\n\n')[0],
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--port', type=int, default=int(read_state('cdp.port') or 0) or None)
    ap.add_argument('--pid', type=int, default=int(read_state('client.pid') or 0) or None)
    ap.add_argument('--notches', type=int, default=1)
    ap.add_argument('--spacing-ms', type=float, default=60.0)
    ap.add_argument('--direction', choices=['down', 'up'], default='down')
    ap.add_argument('--pattern', help="explicit notches, e.g. 'd@0,d@60,u@120'")
    ap.add_argument('--key', help='ArrowDown|ArrowUp|PageDown|PageUp|space|shift+space|Home|End')
    ap.add_argument('--count', type=int, default=1, help='key presses (with --key)')
    ap.add_argument('--hold-ms', type=float, default=0, help='hold the key this long (auto-repeat)')
    ap.add_argument('--repeat-delay', type=float, default=600.0, help='auto-repeat delay (KDE default 600 ms)')
    ap.add_argument('--repeat-rate', type=float, default=25.0, help='auto-repeat rate (KDE default 25 /s)')
    ap.add_argument('--bypass-app-keys', action='store_true',
                    help="stop the app's window keydown listener (ArrowUp edit / type-to-focus) during key runs")
    ap.add_argument('--runs', type=int, default=1)
    ap.add_argument('--start', default='keep', help='keep|bottom|middle|bottom-minus:PX|top-plus:PX|scroll:PX')
    ap.add_argument('--gap-ms', type=float, default=400.0, help='pause between runs')
    ap.add_argument('--trace', action='store_true', help="also record Chromium's cc / input trace events")
    ap.add_argument('--no-place', action='store_true', help='leave the window where it is (no keep-above)')
    ap.add_argument('--size', default='1280x800')
    ap.add_argument('--x', type=float, default=0.5, help='wheel position across the list (0..1)')
    ap.add_argument('--y', type=float, default=0.5, help='wheel position down the list (0..1)')
    ap.add_argument('--label', default='')
    ap.add_argument('--out')
    ap.add_argument('--settle-quiet-ms', type=float, default=400.0)
    ap.add_argument('--max-ms', type=float, default=8000.0)
    ap.add_argument('--no-fit', action='store_true', help='skip the model / curve fits (record only)')
    ap.add_argument('--inject', choices=['x11', 'cdp'], default='x11',
                    help='wheel input: x11 (XSendEvent, a real notch: 120 px, animated) or cdp (Input.dispatchMouseEvent: '
                         'precise-pixel deltas, the touchpad class of input, which Chromium never animates)')
    ap.add_argument('--cdp-delta', type=float, default=120.0, help='--inject cdp: |deltaY| per event')
    ap.add_argument('--reanalyze', metavar='DIR', help='re-run the analysis on a results directory (no input)')
    a = ap.parse_args(argv)
    if a.reanalyze:
        return reanalyze(a.reanalyze, fit=not a.no_fit)
    if not a.port or not a.pid:
        ap.error('no --port / --pid and no running electron-local.sh instance')
    kind = 'key' if a.key else 'wheel'
    steps = plan(a)

    stamp = time.strftime('%Y%m%d-%H%M%S')
    out = a.out or os.path.join(HERE, 'results', 'wheel-linux-%s%s' % (stamp, ('-' + common.slug(a.label)) if a.label else ''))
    os.makedirs(out, exist_ok=True)

    page = CDP.page(a.port)
    info0 = page.ev(PAGE_JS)
    if not isinstance(info0, dict) or info0.get('error'):
        raise SystemExit('page: %s (open a text channel first)' % info0)
    x11 = X11Target(a.pid)

    placed = None
    prev_active_id = None
    activated = False
    try:
        if not a.no_place:
            w, h = [int(v) for v in a.size.split('x')]
            geo, prim = primary_centre(w, h)
            before = kwin_do(a.pid)
            if before.get('error'):
                raise SystemExit('kwin: %s' % before['error'])
            r = kwin_do(a.pid, place=geo, keepAbove=True, activate=(kind == 'key'))
            if r.get('error'):
                raise SystemExit('kwin: %s' % r['error'])
            prev_active_id = (r.get('prevActive') or {}).get('id')
            placed = {'geometry': geo, 'output': prim, 'was': (before.get('window') or {}).get('g')}
            activated = kind == 'key'
            time.sleep(0.6)
            x11.refresh()
        elif kind == 'key':
            r = kwin_do(a.pid, activate=True)
            prev_active_id = (r.get('prevActive') or {}).get('id')
            activated = True
            time.sleep(0.3)
        info = page.ev('__wl.info()')
        rx, ry, rw, rh = info['rect']
        dpr = info['dpr'] or 1
        ex = int((rx + rw * a.x) * dpr)
        ey = int((ry + rh * a.y) * dpr)
        meta = {
            'tool': 'electron-wheel-linux.py', 'time': common.iso(), 'argv': argv, 'kind': kind, 'plan': steps,
            'chrome': (info.get('ua') or ''), 'page_info': info, 'x11_window': [x11.wx, x11.wy, x11.ww, x11.wh],
            'event_point_window_px': [ex, ey], 'placed': placed, 'displays': common.displays(), 'inject': a.inject,
            'kde': kde_settings(),
        }
        guess = info.get('docOffsetLegacy') or 52.0
        try:
            cal_off, cal_iv = calibrate_frame_clock(a.port, page, guess - 0.5)
        except Exception as e:
            cal_off, cal_iv = None, None
            print('warning: frame clock calibration failed: %s' % e, file=sys.stderr)
        meta['frame_clock_offset_calibrated'] = cal_off
        meta['impl_interval_calibrated'] = cal_iv
        results = []
        for run_i in range(a.runs):
            pinfo = page.ev('__wl.position(%s)' % json.dumps(a.start))
            if kind == 'key':
                # Another window may have taken focus since the last run: activate again and wait for the page.
                for _attempt in range(3):
                    if page.ev('document.hasFocus()'):
                        break
                    kwin_do(a.pid, activate=True)
                    t_f = time.monotonic()
                    while time.monotonic() - t_f < 1.0 and not page.ev('document.hasFocus()'):
                        time.sleep(0.05)
                if not page.ev('document.hasFocus()'):
                    print('warning: the client window does not have keyboard focus', file=sys.stderr)
                if not page.ev('__wl.focusList()'):
                    print('warning: could not focus the list', file=sys.stderr)
            tracer = trace_start(a.port) if a.trace else None
            st = page.ev('__wl.start(%s)' % ('true' if a.bypass_app_keys else 'false'))
            time.sleep(0.15)
            sent = []
            base = time.monotonic_ns() + 5_000_000
            for t_ms, act, arg in steps:
                sleep_until(base + int(t_ms * 1e6))
                t_send = time.monotonic_ns()
                if act == 'wheel' and a.inject == 'cdp':
                    xt = None
                    page.call('Input.dispatchMouseEvent', {'type': 'mouseWheel', 'x': ex / dpr, 'y': ey / dpr,
                                                           'deltaX': 0, 'deltaY': a.cdp_delta * (1 if arg == 5 else -1)})
                elif act == 'wheel':
                    xt = x11.button(arg, ex, ey)
                else:
                    ks, state = arg
                    xt = x11.key(x11.keycode(ks), act == 'kp', state, ex, ey)
                sent.append({'t_plan_ms': t_ms, 'action': act, 'arg': arg, 'mono_ms': t_send / 1e6, 'xtime': xt})
            page.ev('__wl.settle(%f, %f, %f)' % (a.settle_quiet_ms, 300.0, a.max_ms))
            run = json.loads(page.ev('__wl.take()'))
            if kind == 'key':
                page.ev('__wl.unfocus && __wl.unfocus()')
            trace = trace_stop(tracer) if tracer else None
            run.update({'run': run_i + 1, 'start': a.start, 'start_info': pinfo, 'sent': sent, 'trace': trace,
                        'frame_clock_offset_calibrated': cal_off, 'impl_interval_calibrated': cal_iv})
            # performance.now() <-> CLOCK_MONOTONIC: an X event's time is its TimeTicks (ms), the JS timeStamp is
            # TimeTicks - timeOrigin, so timeOrigin (monotonic ms) = xtime - timeStamp.
            evs = run['wheel'] if kind == 'wheel' else [k for k in run['keys'] if k[5] == 'keydown']
            xts = [s['xtime'] for s in sent if s['action'] in ('wheel', 'kp') and s['xtime'] is not None]
            if evs and xts:
                run['perf_origin_mono_ms'] = xts[0] - evs[0][0]
                run['send_to_dispatch_ms'] = [round(e[3] - (s['mono_ms'] - run['perf_origin_mono_ms']), 2)
                                              for e, s in zip(evs, [s for s in sent if s['action'] in ('wheel', 'kp')])]
            run['analysis'] = analyze(run, kind, key_step(run['info']) if kind == 'key' else None, fit=not a.no_fit)
            if trace:
                run['analysis']['trace'] = summarize_trace(trace, run)
            results.append(run)
            with open(os.path.join(out, 'run-%d.json' % (run_i + 1)), 'w') as f:
                json.dump({'meta': meta, 'run': run}, f)
            print(summary_line(run_i + 1, a.runs, kind, steps, run['analysis']), flush=True)
            time.sleep(a.gap_ms / 1000.0)
    finally:
        try:
            page.ev('window.__wl && window.__wl.cleanup && window.__wl.cleanup()')
        except Exception:
            pass
        if placed is not None or activated:
            try:
                back = {'keepAbove': False, 'activateId': prev_active_id if activated else None}
                if placed and placed.get('was'):
                    back['place'] = placed['was']  # put the window back where it was
                r = kwin_do(a.pid, **back)
            except Exception as e:  # never leave the window above the owner's
                print('warning: kwin restore failed: %s' % e, file=sys.stderr)
        page.close()

    agg = aggregate(results, kind)
    common.write_json(os.path.join(out, 'summary.json'), {'meta': meta, 'aggregate': agg,
                                                          'runs': [r['analysis'] for r in results]})
    print('summary: %s -> %s' % (json.dumps(agg), out))
    return 0


def reanalyze(d, fit=True):
    import glob
    results, meta, kind = [], None, None
    files = sorted(glob.glob(os.path.join(d, 'run-*.json')), key=lambda p: int(p.rsplit('-', 1)[1].split('.')[0]))
    for i, p in enumerate(files):
        with open(p) as f:
            doc = json.load(f)
        meta, run = doc['meta'], doc['run']
        kind = meta['kind']
        run['analysis'] = analyze(run, kind, key_step(run['info']) if kind == 'key' else None, fit=fit)
        if run.get('trace'):
            run['analysis']['trace'] = summarize_trace(run['trace'], run)
        with open(p, 'w') as f:
            json.dump({'meta': meta, 'run': run}, f)
        print(summary_line(i + 1, len(files), kind, meta['plan'], run['analysis']), flush=True)
        results.append(run)
    agg = aggregate(results, kind)
    common.write_json(os.path.join(d, 'summary.json'), {'meta': meta, 'aggregate': agg,
                                                        'runs': [r['analysis'] for r in results]})
    print('summary: %s -> %s' % (json.dumps(agg), d))
    return 0


def summarize_trace(trace, run):
    origin = run.get('perf_origin_mono_ms')
    out = []
    for e in trace:
        if e.get('name') == 'Scheduler::BeginImplFrame':
            continue
        t = e['ts'] / 1000.0 - origin if origin is not None else e['ts'] / 1000.0
        args = e.get('args') or {}
        out.append({'name': e['name'], 't_perf_ms': round(t, 3), 'args': args})
    return sorted(out, key=lambda x: x['t_perf_ms'])


def aggregate(results, kind):
    def med(vals):
        vals = [v for v in vals if v is not None]
        if not vals:
            return None
        s = sorted(vals)
        return {'median': round(s[len(s) // 2] if len(s) % 2 else (s[len(s) // 2 - 1] + s[len(s) // 2]) / 2, 3),
                'min': round(s[0], 3), 'max': round(s[-1], 3), 'n': len(s)}
    an = [r['analysis'] for r in results]
    agg = {k: med([x.get(k) for x in an]) for k in ('distance_px', 'onset_latency_ms', 'visible_duration_ms',
                                                    'last_change_after_event_ms', 'dropped_frames_in_motion')}
    agg['cc_model_trace_rms_px'] = med([(x.get('cc_model_trace') or {}).get('rms_px') for x in an])
    agg['cc_model_trace_max_abs_px'] = med([(x.get('cc_model_trace') or {}).get('max_abs_px') for x in an])
    agg['cc_model_rms_px'] = med([(x.get('cc_model') or {}).get('rms_px') for x in an])
    agg['cc_model_max_abs_px'] = med([(x.get('cc_model') or {}).get('max_abs_px') for x in an])
    for name in ('ease_in_out_bezier', 'smoothstep', 'exponential'):
        fits = [(x.get('free_fits') or {}).get(name) for x in an]
        if any(fits):
            agg[name] = {k: med([f.get(k) for f in fits if f]) for k in
                         ('duration_ms' if name != 'exponential' else 'tau_ms', 'rms_px', 'max_abs_px')}
    return agg


def summary_line(i, n, kind, steps, an):
    if an.get('error'):
        return 'run %d/%d: %s' % (i, n, an['error'])
    s = 'run %d/%d: %s x%d: ' % (i, n, kind, sum(1 for x in steps if x[1] in ('wheel', 'kp')))
    if kind == 'wheel' and an.get('wheel'):
        w = an['wheel'][0]
        s += 'deltaY %s mode %s, ' % (w['deltaY'], w['deltaMode'])
    s += 'distance %.1f px' % an['distance_px']
    if 'onset_latency_ms' in an:
        s += ', onset +%.1f ms, last change +%.1f ms (visible %.1f ms)' % (
            an['onset_latency_ms'], an['last_change_after_event_ms'], an['visible_duration_ms'])
    s += ', frames %.2f ms, dropped %d in motion' % (an['frame_interval_ms'], an['dropped_frames_in_motion'])
    ff = an.get('free_fits') or {}
    if 'ease_in_out_bezier' in ff:
        b, sm = ff['ease_in_out_bezier'], ff['smoothstep']
        s += '; ease-in-out D=%.1f rms %.2f max %.2f px; smoothstep D=%.1f rms %.2f max %.2f px' % (
            b['duration_ms'], b['rms_px'], b['max_abs_px'], sm['duration_ms'], sm['rms_px'], sm['max_abs_px'])
    ct = an.get('cc_model_trace')
    if ct:
        s += '; cc model with traced P: rms %.2f max %.2f px (lag %d)' % (ct['rms_px'], ct['max_abs_px'], ct['lag_frames'])
    cc = an.get('cc_model')
    if cc:
        s += '; cc model rms %.2f max %.2f px (lag %d, latency %s ms)' % (
            cc['rms_px'], cc['max_abs_px'], cc['lag_frames'], cc['latency_ms'])
    return s


def kde_settings():
    def rd(f, g, k):
        try:
            import subprocess
            r = subprocess.run(['kreadconfig6', '--file', f, '--group', g, '--key', k], stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True, timeout=5)
            return r.stdout.strip()
        except Exception:
            return None
    out = {'kcminputrc': {}, 'gtk': {}}
    try:
        with open(os.path.expanduser('~/.config/kcminputrc')) as f:
            out['kcminputrc_text'] = f.read()
    except OSError:
        out['kcminputrc_text'] = None
    for k in ('RepeatDelay', 'RepeatRate', 'KeyRepeat'):
        out['kcminputrc']['Keyboard.' + k] = rd('kcminputrc', 'Keyboard', k)
    for p in ('~/.config/gtk-3.0/settings.ini',):
        try:
            with open(os.path.expanduser(p)) as f:
                out['gtk'][p] = [l.strip() for l in f if 'animation' in l]
        except OSError:
            pass
    out['xwayland_scale'] = rd('kwinrc', 'Xwayland', 'Scale')
    return out


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

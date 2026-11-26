"""KWin bridge: a generated JavaScript KWin script reports window events back to a D-Bus service this
process hosts, so the tools see windows of any toolkit (native Wayland or XWayland) without root.

How it works (KWin 6, verified on Plasma 6.8 Wayland): the script is written to a temp file, loaded with
org.kde.kwin.Scripting.loadScript(path, name) and started with /Scripting/Script<id> run(). It calls back with
callDBus(<our unique bus name>, "/", IFACE, "event", <json>) (KWin's callDBus is asynchronous: the compositor
never waits on us) and asks isMember(<pid>) with a reply callback where it needs to know whether a window
belongs to the measured tree. stop() unloads the script and checks isScriptLoaded() is false.

Facts the tools rely on (measured, KWin 6.8.3):
- workspace.windowAdded fires once the window has content: for Wayland clients when the first buffer is
  committed, for XWayland clients just before the first damage (glxgears: damaged 1 ms after added). The
  window object has no readyForPainting property and readyForPaintingChanged does not fire after the add.
- window.pid is right for Wayland and XWayland clients (Wayland: the socket's peer credentials).
- window.damaged fires per surface commit with damage: the count is the frames the app gave the compositor. Exact for
  Wayland clients (es2gears_wayland, vkcube: 280/s on a 280 Hz panel) and matched glxgears' own count through XWayland
  (279.7/s against its 279.9 FPS). XWayland damage reaches KWin as X damage reports, which can coalesce while KWin is
  stalled: treat a dip there with suspicion.
- QTimer is available to scripts (`new QTimer()`), used to flush frame timestamps 250 ms after a burst starts.

A script whose owner died keeps running inside KWin (its callbacks then fail quietly). Every bridge records
its script name in $XDG_RUNTIME_DIR/decibell-perf-kwin-scripts.json and every tool unloads stale entries on
start (`winctl.sh cleanup` does it by hand).
"""
import json
import os
import tempfile
import threading
import time

KWIN = 'org.kde.KWin'
IFACE = 'org.decibell.perf.Bridge'
SCRIPT_PREFIX = 'decibell-perf-'
RUNTIME_DIR = os.environ.get('XDG_RUNTIME_DIR') or tempfile.gettempdir()
STATE_FILE = os.path.join(RUNTIME_DIR, 'decibell-perf-kwin-scripts.json')

# Helpers prepended to every script. SVC is substituted per bridge.
JS_PRELUDE = r"""
var SVC = "%(svc)s", OBJ = "/", IFACE = "%(iface)s";
function geo(w) {
  var g = w.frameGeometry;
  return [Math.round(g.x), Math.round(g.y), Math.round(g.width), Math.round(g.height)];
}
function info(w) {
  if (!w) return {pid: 0};
  return {pid: w.pid, caption: w.caption, cls: w.resourceClass, id: String(w.internalId),
          normal: w.normalWindow, dialog: w.dialog, splash: w.splash, utility: w.utility,
          popup: w.popupWindow, special: w.specialWindow, desktop: w.desktopWindow, dock: w.dock,
          minimized: w.minimized, active: w.active, fullScreen: w.fullScreen, maximize: w.maximizeMode,
          output: w.output ? w.output.name : "", stacking: w.stackingOrder, g: geo(w)};
}
function send(kind, obj) {
  obj.k = kind; obj.t = Date.now();
  callDBus(SVC, OBJ, IFACE, "event", JSON.stringify(obj));
}
"""


def _jeepney():
    try:
        from jeepney import DBusAddress, HeaderFields, MessageType, new_method_call, new_method_return
        from jeepney.bus_messages import message_bus
        from jeepney.io.blocking import open_dbus_connection
    except ImportError as e:  # pragma: no cover - depends on the machine
        raise RuntimeError('python3-jeepney is required for the KWin bridge (%s)' % e)
    return DBusAddress, HeaderFields, MessageType, new_method_call, new_method_return, message_bus, \
        open_dbus_connection


def _call(conn, path, iface, method, sig=None, body=(), dest=KWIN):
    DBusAddress, _, MessageType, new_method_call, _, _, _ = _jeepney()
    addr = DBusAddress(path, bus_name=dest, interface=iface)
    msg = new_method_call(addr, method, sig, body) if sig else new_method_call(addr, method)
    reply = conn.send_and_get_reply(msg, timeout=5)
    if reply.header.message_type == MessageType.error:
        raise RuntimeError('%s.%s failed: %s' % (iface, method, reply.body))
    return reply.body


def available():
    """True when a KWin with the scripting interface is on the session bus."""
    if not os.environ.get('DBUS_SESSION_BUS_ADDRESS') and not os.path.exists(
            '/run/user/%d/bus' % os.getuid()):
        return False
    try:
        open_dbus_connection = _jeepney()[6]
        conn = open_dbus_connection(bus='SESSION')
    except Exception:
        return False
    try:
        _call(conn, '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's', ('decibell-perf-probe',))
        return True
    except Exception:
        return False
    finally:
        conn.close()


def _read_state():
    try:
        with open(STATE_FILE) as f:
            return json.load(f)
    except (OSError, ValueError):
        return {}


def _write_state(state):
    tmp = STATE_FILE + '.%d' % os.getpid()
    with open(tmp, 'w') as f:
        json.dump(state, f)
    os.replace(tmp, STATE_FILE)


def _start_time(pid):
    """Process start time in clock ticks since boot (tells a reused pid apart), or None if it is gone."""
    try:
        with open('/proc/%d/stat' % pid) as f:
            st = f.read()
        return int(st[st.rfind(')') + 2:].split()[19])
    except (OSError, ValueError, IndexError):
        return None


def _owner_alive(owner):
    pid, start = (owner if isinstance(owner, list) else [owner, None])[:2]
    now = _start_time(pid)
    return now is not None and (start is None or now == start)


def cleanup_stale(verbose=False, everything=False):
    """Unloads scripts recorded by bridges whose process is gone (all recorded ones with everything=True).
    Returns the names unloaded."""
    # Script files of dead owners (a SIGKILLed tool), whether or not they made it into the state file.
    import re
    for f in os.listdir(RUNTIME_DIR):
        m = re.match(re.escape(SCRIPT_PREFIX) + r'(\d+)-.*\.js$', f)
        if m and (everything or _start_time(int(m.group(1))) is None) and int(m.group(1)) != os.getpid():
            try:
                os.unlink(os.path.join(RUNTIME_DIR, f))
            except OSError:
                pass
    state = _read_state()
    if not state:
        return []
    open_dbus_connection = _jeepney()[6]
    conn = open_dbus_connection(bus='SESSION')
    gone = []
    try:
        for name, owner in list(state.items()):
            if not everything and _owner_alive(owner):
                continue
            try:
                loaded = _call(conn, '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's', (name,))[0]
                if loaded:
                    _call(conn, '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', (name,))
                    gone.append(name)
                    if verbose:
                        print('unloaded KWin script %s (owner %s)' % (name, owner))
            except Exception as e:
                if verbose:
                    print('could not unload %s: %s' % (name, e))
                continue
            state.pop(name, None)
            path = owner[2] if isinstance(owner, list) and len(owner) > 2 else None
            if path and os.path.basename(path).startswith(SCRIPT_PREFIX):
                try:
                    os.unlink(path)
                except OSError:
                    pass
        _write_state(state)
    finally:
        conn.close()
    return gone


class Bridge:
    """One generated KWin script plus the D-Bus service it reports to.

    on_event(dict) runs on the bridge's receive thread for every event the script sends; is_member(pid) (optional)
    answers the script's isMember queries. Use as a context manager, or call start()/stop() (stop is idempotent and
    also registered with atexit)."""

    _seq = 0

    def __init__(self, purpose, js_body, on_event, is_member=None):
        Bridge._seq += 1
        self.name = '%s%d-%s-%d' % (SCRIPT_PREFIX, os.getpid(), purpose, Bridge._seq)
        self.svc = 'org.decibell.perf.p%d_%d' % (os.getpid(), Bridge._seq)
        self.js = JS_PRELUDE % {'svc': self.svc, 'iface': IFACE} + js_body
        self.on_event = on_event
        self.is_member = is_member or (lambda pid: False)
        self.events = 0
        self._srv = None
        self._ctl = None
        self._thread = None
        self._stop = threading.Event()
        self._path = None
        self._loaded = False
        self.errors = []

    # -- lifecycle ---------------------------------------------------------------------------------------------
    def start(self):
        import atexit
        (_, _, _, _, _, message_bus, open_dbus_connection) = _jeepney()
        cleanup_stale()
        self._srv = open_dbus_connection(bus='SESSION')
        reply = self._srv.send_and_get_reply(message_bus.RequestName(self.svc, 4), timeout=5)  # 4: do not queue
        if reply.body[0] != 1:
            raise RuntimeError('could not own bus name %s (reply %s)' % (self.svc, reply.body))
        self._thread = threading.Thread(target=self._serve, name='kwin-bridge', daemon=True)
        self._thread.start()
        self._ctl = open_dbus_connection(bus='SESSION')
        fd, self._path = tempfile.mkstemp(prefix=self.name + '-', suffix='.js', dir=RUNTIME_DIR)
        with os.fdopen(fd, 'w') as f:
            f.write(self.js)
        atexit.register(self.stop)
        state = _read_state()
        state[self.name] = [os.getpid(), _start_time(os.getpid()), self._path]
        _write_state(state)
        sid = _call(self._ctl, '/Scripting', 'org.kde.kwin.Scripting', 'loadScript', 'ss', (self._path, self.name))[0]
        if sid < 0:
            raise RuntimeError('KWin refused to load the script (%s)' % sid)
        self._loaded = True
        _call(self._ctl, '/Scripting/Script%d' % sid, 'org.kde.kwin.Script', 'run')
        return self

    def stop(self):
        if self._ctl is not None and self._loaded:
            try:
                _call(self._ctl, '/Scripting', 'org.kde.kwin.Scripting', 'unloadScript', 's', (self.name,))
                still = _call(self._ctl, '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's',
                              (self.name,))[0]
                if still:
                    self.errors.append('script %s still loaded after unloadScript' % self.name)
            except Exception as e:
                self.errors.append('unload failed: %s' % e)
            self._loaded = False
            state = _read_state()
            if state.pop(self.name, None) is not None:
                _write_state(state)
        self._stop.set()
        if self._thread is not None:
            self._thread.join(2)
            self._thread = None
        for c in (self._ctl, self._srv):
            if c is not None:
                try:
                    c.close()
                except Exception:
                    pass
        self._ctl = self._srv = None
        if self._path:
            try:
                os.unlink(self._path)
            except OSError:
                pass
            self._path = None

    def is_loaded(self):
        """Asks KWin whether this bridge's script is loaded (used by the tools' self-checks)."""
        open_dbus_connection = _jeepney()[6]
        conn = open_dbus_connection(bus='SESSION')
        try:
            return bool(_call(conn, '/Scripting', 'org.kde.kwin.Scripting', 'isScriptLoaded', 's', (self.name,))[0])
        finally:
            conn.close()

    def __enter__(self):
        return self.start()

    def __exit__(self, *exc):
        self.stop()

    # -- receive thread ----------------------------------------------------------------------------------------
    def _serve(self):
        (_, HeaderFields, MessageType, _, new_method_return, _, _) = _jeepney()
        while not self._stop.is_set():
            try:
                msg = self._srv.receive(timeout=0.5)
            except TimeoutError:
                continue
            except Exception:
                if self._stop.is_set():
                    return
                time.sleep(0.05)
                continue
            if msg.header.message_type != MessageType.method_call:
                continue
            member = msg.header.fields.get(HeaderFields.member)
            try:
                if member == 'event':
                    self.events += 1
                    ev = json.loads(msg.body[0])
                    ev['rx'] = time.time()
                    self.on_event(ev)
                    self._srv.send(new_method_return(msg))
                elif member == 'isMember':
                    pid = int(float(msg.body[0]))
                    self._srv.send(new_method_return(msg, 'b', (bool(self.is_member(pid)),)))
                else:
                    self._srv.send(new_method_return(msg))
            except Exception as e:  # never let a bad event kill the thread
                self.errors.append('%s: %s' % (member, e))
                try:
                    self._srv.send(new_method_return(msg))
                except Exception:
                    pass


def run_once(purpose, js_body, timeout=5.0):
    """Loads a one-shot script that ends with send("result", {...}), waits for that result and unloads it."""
    got = {}
    done = threading.Event()

    def on_event(ev):
        if ev.get('k') == 'result':
            got.update(ev)
            done.set()

    b = Bridge(purpose, js_body, on_event)
    try:
        b.start()
        if not done.wait(timeout):
            raise RuntimeError('KWin script sent no result within %.0f s' % timeout)
    finally:
        b.stop()
    if b.errors:
        got.setdefault('bridge_errors', b.errors)
    return got


# ---- scripts used by the tools ---------------------------------------------------------------------------------

# Watcher for startup.sh / measure.sh. Flags are substituted as JS booleans.
WATCH_JS = r"""
var ADDS = %(adds)s, FOCUS = %(focus)s, FRAMES = %(frames)s;
var counters = {};
function flush(id, pid) {
  var c = counters[id];
  if (c && c.ts.length) { send("frames", {id: id, pid: pid, ts: c.ts}); c.ts = []; }
}
function attach(w) {
  var id = String(w.internalId);
  if (counters[id]) return;
  // Every damage event's time goes out, batched by a 250 ms single-shot timer armed by the first event of a batch:
  // nothing runs while the window is idle, and a continuously rendering window costs 4 calls a second.
  var c = {ts: [], timer: new QTimer()};
  c.timer.singleShot = true;
  c.timer.interval = 250;
  c.timer.timeout.connect(function () { flush(id, w.pid); });
  c.fn = function () { c.ts.push(Date.now()); if (!c.timer.active) c.timer.start(); };
  counters[id] = c;
  w.damaged.connect(c.fn);
  send("attached", info(w));
}
function consider(w) {
  if (!FRAMES || !w || w.desktopWindow || w.dock) return;
  callDBus(SVC, OBJ, IFACE, "isMember", String(w.pid), function (ok) { if (ok) attach(w); });
}
var wins = workspace.windowList();
for (var i = 0; i < wins.length; i++) {
  if (ADDS) send("existing", info(wins[i]));
  consider(wins[i]);
}
workspace.windowAdded.connect(function (w) {
  if (ADDS) {
    send("added", info(w));
    var d = function () { w.damaged.disconnect(d); send("first_damage", {id: String(w.internalId), pid: w.pid}); };
    w.damaged.connect(d);
  }
  consider(w);
});
workspace.windowRemoved.connect(function (w) {
  var id = String(w.internalId), c = counters[id];
  if (c) {
    w.damaged.disconnect(c.fn);
    c.timer.stop();
    flush(id, w.pid);
    delete counters[id];
  }
  if (ADDS || c) send("removed", {id: id, pid: w.pid, caption: w.caption});
});
if (FOCUS) {
  workspace.windowActivated.connect(function (w) { send("activated", info(w)); });
  send("activated", info(workspace.activeWindow));
}
send("ready", {});
"""


def watch_js(adds=False, focus=False, frames=False):
    b = lambda v: 'true' if v else 'false'  # noqa: E731
    return WATCH_JS % {'adds': b(adds), 'focus': b(focus), 'frames': b(frames)}

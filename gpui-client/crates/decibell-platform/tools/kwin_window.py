"""What KWin says about a process's normal windows: active, demanding attention, minimised, geometry.

    python3 -I kwin_window.py <tools/perf/linux> <pid>

Prints one JSON object: {"windows": [{pid, caption, active, demandsAttention, minimized, g: [x, y, w, h]}, ...],
"active": {...the active window...}}. Uses tools/perf/linux/kwin.py's one-shot script bridge (KWin 6, Wayland or
X11; no root, no input).
"""
import json
import sys

sys.path.insert(0, sys.argv[1])
import kwin  # noqa: E402

pid = int(sys.argv[2])
js = r"""
var PID = %d;
var all = workspace.windowList(), res = {windows: []};
for (var i = 0; i < all.length; i++) {
  var w = all[i];
  if (w.pid == PID && w.normalWindow) { var o = info(w); o.demandsAttention = w.demandsAttention; res.windows.push(o); }
}
res.active = info(workspace.activeWindow);
send("result", res);
""" % pid
if not kwin.available():
    print(json.dumps({"error": "KWin scripting is not on the session bus"}))
    sys.exit(2)
print(json.dumps(kwin.run_once('platform-query', js)))

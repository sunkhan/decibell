"""winctl: list and drive windows on KDE Plasma (Wayland or X11) through a one-shot KWin script.

A <pid> selects the windows of that process and its descendants (Electron's windows belong to its main process,
but the tree rule keeps launcher wrappers working). With several windows, the topmost normal window is used.
"""
import argparse
import json
import sys
import time

import kwin
import procfs

VERB_JS = r"""
var VERB = "%(verb)s", PIDS = %(pids)s, ARGS = %(args)s;
function mine(w) { return PIDS[w.pid] === true && !w.desktopWindow && !w.dock && !w.popupWindow && !w.tooltip; }
var all = workspace.windowList(), targets = [];
for (var i = 0; i < all.length; i++) if (mine(all[i])) targets.push(all[i]);
targets.sort(function (a, b) {
  if (a.normalWindow != b.normalWindow) return a.normalWindow ? -1 : 1;
  return b.stackingOrder - a.stackingOrder;
});
var res = {verb: VERB, matched: targets.length};
function others() {
  var o = [];
  for (var i = 0; i < all.length; i++) {
    var w = all[i];
    if (!mine(w) && w.normalWindow && !w.minimized && !w.skipTaskbar) o.push(w);
  }
  o.sort(function (a, b) { return b.stackingOrder - a.stackingOrder; });
  return o;
}
function main() {
  if (VERB == "list") { res.windows = []; for (var i = 0; i < all.length; i++) res.windows.push(info(all[i])); return; }
  if (!targets.length) { res.error = "no window belongs to that process tree"; return; }
  var w = targets[0];
  if (VERB == "activate") {
    if (w.minimized) w.minimized = false;
    workspace.activeWindow = w;
    res.window = info(w);
  } else if (VERB == "unfocus") {
    var focused = false;
    for (var i = 0; i < targets.length; i++) if (targets[i].active) focused = true;
    if (!focused) { res.note = "not focused already"; return; }
    var pick = null;
    if (ARGS.to == "desktop") {
      for (var i = 0; i < all.length; i++) {
        var d = all[i];
        if (d.desktopWindow && (!pick || (w.output && d.output && d.output.name == w.output.name))) pick = d;
      }
    } else if (ARGS.to) {
      for (var i = 0; i < all.length; i++) if (all[i].pid == ARGS.to && all[i].normalWindow) { pick = all[i]; break; }
    }
    if (!pick) { var o = others(); if (o.length) pick = o[0]; }
    if (!pick) { res.error = "nothing else to focus"; return; }
    workspace.activeWindow = pick;
    res.focused = info(pick);
  } else if (VERB == "geometry") {
    if (w.fullScreen) w.fullScreen = false;
    if (w.maximizeMode) w.setMaximize(false, false);
    w.frameGeometry = {x: ARGS.x, y: ARGS.y, width: ARGS.w, height: ARGS.h};
    res.window = info(w);
  } else if (VERB == "close") {
    res.closed = [];
    for (var i = 0; i < targets.length; i++) {
      var t = targets[i];
      if (t.closeable && (t.normalWindow || t.dialog || t.utility)) { res.closed.push(info(t)); t.closeWindow(); }
    }
  }
}
try { main(); } catch (e) { res.error = String(e); }
res.active = info(workspace.activeWindow);
send("result", res);
"""


def run_verb(verb, pids=(), **args):
    js = VERB_JS % {'verb': verb, 'pids': json.dumps({str(p): True for p in pids}), 'args': json.dumps(args)}
    return kwin.run_once('winctl-' + verb, js)


def tree_pids(target):
    """A pid (and its descendants), or a regex over /proc/*/cmdline (every match and its descendants, never this tool,
    its shells or its pipeline mates)."""
    if target.isdigit():
        t = procfs.Tree(roots=[int(target)], exclude_self=False)
    else:
        t = procfs.Tree(match=target)
    return sorted(t.scan())


def fmt_window(w, mark_pids=()):
    g = w.get('g') or [0, 0, 0, 0]
    flags = ''.join([
        '*' if w.get('active') else ' ',
        'm' if w.get('minimized') else ' ',
        '+' if w.get('pid') in mark_pids else ' '])
    kind = 'normal' if w.get('normal') else 'dialog' if w.get('dialog') else 'desktop' if w.get('desktop') \
        else 'dock' if w.get('dock') else 'popup' if w.get('popup') else 'other'
    return '%-3s %7s  %-7s %5d,%-5d %5dx%-5d %-6s %-22s %s' % (
        flags, w.get('pid'), kind, g[0], g[1], g[2], g[3], w.get('output', ''), (w.get('cls') or '')[:22],
        (w.get('caption') or '')[:70])


def main(argv):
    ap = argparse.ArgumentParser(
        prog='winctl.sh', formatter_class=argparse.RawDescriptionHelpFormatter,
        description='List and drive windows through KWin scripting (KDE Plasma 6, Wayland or X11). No root, no '
                    'input injection.',
        epilog='<pid> is a pid (its tree) or a regex over /proc/*/cmdline (every match and its tree).\nverbs:\n'
               '  list [--all] [--json]          windows: flags (* active, m minimized), pid, type, geometry, '
               'output, class, caption\n'
               '  activate <pid>                 focus (and unminimize) the topmost window of pid\'s tree\n'
               '  unfocus <pid> [--to desktop|<pid>]  if pid\'s window is active, focus the desktop of its output\n'
               '                                 (default) or another app; nothing of the measured app keeps focus\n'
               '  geometry <pid> <x> <y> <w> <h> move/resize (frame geometry, logical px; unmaximizes first)\n'
               '  close <pid>                    ask the tree\'s windows to close (like clicking X)\n'
               '  cleanup [--all]                unload KWin scripts left by a killed tool (--all: every one '
               'recorded)\n')
    ap.add_argument('verb', choices=['list', 'activate', 'unfocus', 'geometry', 'close', 'cleanup'])
    ap.add_argument('args', nargs='*')
    ap.add_argument('--all', action='store_true', help='list: include panels, desktop and popups')
    ap.add_argument('--json', action='store_true', help='print the raw result as JSON')
    ap.add_argument('--to', default='desktop', help='unfocus: "desktop" (default) or a pid to focus instead')
    a = ap.parse_args(argv)

    if a.verb == 'cleanup':
        gone = kwin.cleanup_stale(verbose=True, everything=a.all)
        print('unloaded %d script(s)' % len(gone))
        return 0
    if not kwin.available():
        print('winctl: KWin scripting is not on the session bus (not KDE Plasma?). Only KWin is supported.',
              file=sys.stderr)
        return 2

    if a.verb == 'list':
        res = run_verb('list')
        if a.json:
            print(json.dumps(res, indent=2))
            return 0
        print('%-3s %7s  %-7s %11s %11s %-6s %-22s %s' % ('', 'pid', 'type', 'x,y', 'w x h', 'output', 'class',
                                                         'caption'))
        for w in sorted(res.get('windows', []), key=lambda w: -w.get('stacking', 0)):
            if not a.all and (w.get('desktop') or w.get('dock') or w.get('popup') or w.get('special')):
                continue
            print(fmt_window(w))
        return 0

    if not a.args:
        ap.error('%s needs a pid' % a.verb)
    pids = tree_pids(a.args[0])
    if not pids:
        print('winctl: no process matches %s' % a.args[0], file=sys.stderr)
        return 1
    extra = {}
    if a.verb == 'geometry':
        if len(a.args) != 5:
            ap.error('geometry <pid> <x> <y> <w> <h>')
        extra = dict(zip('xywh', (int(v) for v in a.args[1:])))
    if a.verb == 'unfocus':
        extra = {'to': a.to if a.to == 'desktop' else int(a.to)}
    res = run_verb(a.verb, pids, **extra)
    if a.verb == 'geometry' and not res.get('error'):
        time.sleep(0.3)  # the client acks the configure asynchronously; report where it ended up
        after = [w for w in run_verb('list').get('windows', []) if w.get('id') == res['window']['id']]
        if after:
            res['window_after'] = after[0]
    if a.json:
        print(json.dumps(res, indent=2))
    else:
        if res.get('error'):
            print('error: %s' % res['error'])
        for k in ('window', 'window_after', 'focused'):
            if res.get(k):
                print('%-12s %s' % (k, fmt_window(res[k], pids)))
        for w in res.get('closed', []):
            print('%-12s %s' % ('closed', fmt_window(w, pids)))
        if res.get('note'):
            print(res['note'])
        print('%-12s %s' % ('active now', fmt_window(res.get('active', {}), pids)))
    return 1 if res.get('error') else 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

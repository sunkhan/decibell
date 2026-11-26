"""Helpers for scenarios.sh (client-agnostic):

  scenario_util.py display-server (--pid PID | --match RE) [--desktop-file F]
        per process of the client: Chromium's --ozone-platform (if any) and which display servers its unix sockets
        reach (Wayland compositor socket / X11 socket = XWayland on a Wayland session), plus the .desktop Exec line
  scenario_util.py place --pid PID [--w 1280 --h 800]
        moves the client's window to the centre of the primary output (kscreen priority 1) at w x h, prints the output
  scenario_util.py refresh --pid PID      the refresh rate (Hz) of the output the client's window is on
  scenario_util.py scroll-summary DIR     DIR/scroll.jsonl -> DIR/scroll.json (medians over the runs)
  scenario_util.py meta DIR [--set k=v]... [--json k=<json>]... [--json-file k=<path>]... [--note TEXT]... [--facts]
        merge keys into DIR/scenario.json (--facts adds machine facts + the load at the time)
  scenario_util.py load      one JSON line: load average + the busiest processes right now (is the machine quiet?)
  scenario_util.py table CLIENT [--quick] [--out FILE] DIR...
        one Markdown table over the scenario directories (each has scenario.json + the tool's own output)
"""
import argparse
import glob
import json
import os
import re
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common  # noqa: E402
import procfs  # noqa: E402

PERF = common.PERF_DIR


def pids_of(pid=None, match=None):
    out = set()
    if pid:
        out.add(int(pid))
    allp = [int(p) for p in os.listdir('/proc') if p.isdigit()]
    if match:
        rx = re.compile(match)
        for p in allp:
            try:
                if rx.search(procfs.cmdline(p) or ''):
                    out.add(p)
            except OSError:
                pass
    # descendants
    changed = True
    while changed:
        changed = False
        for p in allp:
            if p in out:
                continue
            try:
                st = procfs.stat(p)
            except (OSError, ValueError):
                continue
            if st and st.ppid in out:
                out.add(p)
                changed = True
    return sorted(out)


def unix_peers():
    """inode -> bound path for every unix socket; plus (pid -> [peer inode]) from ss -xpH."""
    raw = common._run(['ss', '-xpH'])
    path_of, peers_of = {}, {}
    for line in raw.splitlines():
        f = line.split()
        if len(f) < 8:
            continue
        local, ino, peer_ino = f[4], f[5], f[7]
        if local != '*':
            path_of[ino] = local
        for m in re.finditer(r'pid=(\d+)', line):
            peers_of.setdefault(int(m.group(1)), []).append(peer_ino)
    return path_of, peers_of


def display_server(a):
    pids = pids_of(a.pid, a.match)
    path_of, peers_of = unix_peers()
    wl = os.path.join(os.environ.get('XDG_RUNTIME_DIR', ''), os.environ.get('WAYLAND_DISPLAY', 'wayland-0'))
    procs = []
    for p in pids:
        try:
            cmd = procfs.cmdline(p) or ''
            comm = open('/proc/%d/comm' % p).read().strip()
        except OSError:
            continue
        targets = {path_of.get(i, '') for i in peers_of.get(p, [])}
        m = re.search(r'--ozone-platform=(\S+)', cmd)
        procs.append({'pid': p, 'label': procfs.label(comm, cmd), 'ozone_platform': m.group(1) if m else None,
                      'wayland': any(t == wl or t.endswith('/' + os.environ.get('WAYLAND_DISPLAY', 'wayland-0'))
                                     for t in targets),
                      'x11': any('.X11-unix' in t for t in targets)})
    ozone = sorted({x['ozone_platform'] for x in procs if x['ozone_platform']})
    # Who draws: Chromium's GPU process when there is one (the browser process may also hold a Wayland connection of
    # its own, e.g. the native addon's ext-idle-notify watcher), else the root process.
    gpu = [x for x in procs if x['label'] == 'gpu-process']
    drawer = gpu or [x for x in procs if x['pid'] == pids[0]] if pids else []
    if ozone:
        verdict = 'XWayland (X11 client of Xwayland)' if ozone == ['x11'] else \
            ('native Wayland' if ozone == ['wayland'] else '/'.join(ozone))
    elif any(x['x11'] for x in drawer) and not any(x['wayland'] for x in drawer):
        verdict = 'XWayland (X11 client of Xwayland)'
    elif any(x['wayland'] for x in drawer):
        verdict = 'native Wayland'
    else:
        verdict = 'unknown'
    others = sorted({x['label'] for x in procs if x not in drawer and x['wayland']})
    if others:
        verdict += '; also a Wayland connection in: ' + ', '.join(others)
    exec_line = None
    if a.desktop_file and os.path.exists(a.desktop_file):
        for line in open(a.desktop_file):
            if line.startswith('Exec='):
                exec_line = line.strip()
                break
    root_cmd = procfs.cmdline(pids[0]) if pids else ''
    res = {'display_server': verdict, 'ozone_platform': ozone, 'session': os.environ.get('XDG_SESSION_TYPE', ''),
           'desktop_file': a.desktop_file, 'desktop_exec': exec_line,
           'root_cmdline': root_cmd, 'processes': procs}
    print(json.dumps(res))
    return 0


def _windows():
    out = subprocess.run([os.path.join(PERF, 'winctl.sh'), 'list', '--json'], stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, text=True, timeout=20).stdout
    return json.loads(out).get('windows', [])


def _client_window(pid):
    tree = set(pids_of(pid))
    wins = [w for w in _windows() if w.get('pid') in tree and w.get('normal')]
    return wins[0] if wins else None


def place(a):
    disp = common.displays()
    if not disp:
        print(json.dumps({'error': 'no displays from kscreen-doctor'}))
        return 1
    prim = sorted(disp, key=lambda d: (d.get('priority') or 99))[0]
    w0, h0 = [int(x) for x in prim['mode'].split('x')]
    scale = prim.get('scale') or 1
    lw, lh = int(w0 / scale), int(h0 / scale)
    x = int(prim['pos']['x'] + (lw - a.w) / 2)
    y = int(prim['pos']['y'] + (lh - a.h) / 2)
    for _ in range(40):
        if _client_window(a.pid):
            break
        time.sleep(0.25)
    r = subprocess.run([os.path.join(PERF, 'winctl.sh'), 'geometry', str(a.pid), str(x), str(y), str(a.w), str(a.h)],
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=20)
    time.sleep(0.5)
    w = _client_window(a.pid)
    print(json.dumps({'output': prim['name'], 'refresh_hz': prim['refresh_hz'], 'requested': [x, y, a.w, a.h],
                      'window': w.get('g') if w else None, 'window_output': w.get('output') if w else None,
                      'winctl': r.stdout.strip()[-300:]}))
    return 0


def refresh(a):
    w = _client_window(a.pid)
    disp = {d['name']: d for d in common.displays()}
    out = (w or {}).get('output')
    hz = disp.get(out, {}).get('refresh_hz')
    print(json.dumps({'output': out, 'refresh_hz': hz, 'active': (w or {}).get('active'),
                      'minimized': (w or {}).get('minimized'), 'geometry': (w or {}).get('g')}))
    return 0 if hz else 1


def scroll_summary(a):
    runs = []
    for line in open(os.path.join(a.dir, 'scroll.jsonl')):
        line = line.strip()
        if line:
            runs.append(json.loads(line))
    ok = [r for r in runs if 'interval_ms' in r]

    def med(key):
        return common.median([r['interval_ms'][key] for r in ok])
    s = {'runs': len(runs), 'ok_runs': len(ok),
         'interval_ms_median_over_runs': {k: med(k) for k in ('p50', 'p95', 'p99', 'max')} if ok else None,
         'missed_vsyncs': [r.get('missed_vsyncs') for r in ok],
         'frames': [r.get('frames') for r in ok],
         'expected_frames': [r.get('expected_frames') for r in ok],
         'refresh_hz': ok[0].get('refresh_hz') if ok else None,
         'visible': [r.get('visibility') for r in ok], 'has_focus': [r.get('has_focus') for r in ok],
         'scroller': ok[0].get('scroller') if ok else None,
         'scroll_top_delta_px': [r.get('scroll_top_delta_px') for r in ok]}
    common.write_json(os.path.join(a.dir, 'scroll.json'), s)
    print(json.dumps(s))
    return 0


# ---------------------------------------------------------------------------------------------------------- table

def _f(v, nd=1):
    if v is None:
        return '–'
    if isinstance(v, float):
        return ('%.' + str(nd) + 'f') % v
    return str(v)


def _m(summary, key, stat='mean', nd=1):
    return _f((summary.get(key) or {}).get(stat), nd)


def _top_wakeups(threads_csv, n=5):
    import csv
    rows = []
    try:
        with open(threads_csv) as f:
            for r in csv.DictReader(f):
                if r.get('kind') in ('unnamed', 'driver'):
                    continue  # reported on their own row
                try:
                    rows.append((float(r.get('vol_s') or 0), r))
                except ValueError:
                    pass
    except OSError:
        return '–'
    rows.sort(key=lambda x: -x[0])
    return ', '.join('%s %s %s/s' % (r['process'], r['name'], _f(v, 1)) for v, r in rows[:n])


def _unnamed_summary(threads_csv):
    import csv
    n, tot = 0, 0.0
    try:
        with open(threads_csv) as f:
            for r in csv.DictReader(f):
                if r.get('kind') == 'unnamed':
                    n += 1
                    tot += float(r.get('vol_s') or 0)
    except OSError:
        return None
    return n, tot


def table(a):
    scen = {}
    for d in a.dirs:
        try:
            meta = json.load(open(os.path.join(d, 'scenario.json')))
        except OSError:
            continue
        scen[meta['scenario']] = (d, meta)
    lines = []
    any_meta = next(iter(scen.values()))[1] if scen else {}
    facts = any_meta.get('machine') or common.machine_facts()
    title = '%s baseline (Linux)%s' % (a.client, ' — QUICK validation run, not a baseline' if a.quick else '')
    lines.append('# %s' % title)
    lines.append('')
    cmds = sorted({m.get('command') for _, m in scen.values() if m.get('command')})
    lines.append('Generated %s from `tools/perf/%s`.' % (common.iso(), '`, `tools/perf/'.join(cmds) or 'scenarios.sh'))
    lines.append('')
    lines.append('- **Client**: %s' % any_meta.get('client_describe', '?'))
    g = facts.get('gpus') or [{}]
    lines.append('- **Machine**: %s, %s logical cores, %s GB; %s (driver %s); %s, kernel %s' % (
        facts.get('cpu'), facts.get('logical_cores'), facts.get('memory_gb'), g[0].get('name'), g[0].get('driver'),
        facts.get('os'), facts.get('kernel')))
    lines.append('- **Displays**: %s' % '; '.join('%s %s @ %s Hz scale %s%s' % (
        d['name'], d['mode'], d['refresh_hz'], d.get('scale'), ' (primary)' if d.get('priority') == 1 else '')
        for d in facts.get('displays') or []))
    sess = facts.get('session') or {}
    lines.append('- **Session**: %s, %s, %s' % (sess.get('type'), sess.get('desktop'), sess.get('compositor')))
    ds = next((m.get('client_facts') for _, m in scen.values() if m.get('client_facts')), None)
    if ds:
        lines.append('- **Display server used by the client**: %s (ozone %s); shipped launcher: `%s`' % (
            ds.get('display_server'), ','.join(ds.get('ozone_platform') or []) or 'n/a', ds.get('desktop_exec')))
    lines.append('')
    lines.append('| Scenario | Metric | %s |' % a.client)
    lines.append('|---|---|---|')

    if 'install-size' in scen:
        d, meta = scen['install-size']
        try:
            j = json.load(open(os.path.join(d, 'install-size.json')))
            labels = meta.get('labels') or []
            for i, p in enumerate(j['paths']):
                lab = labels[i] if i < len(labels) else p['path']
                lines.append('| Install: %s | Size on disk / apparent / files | **%s MB** / %s MB / %d |' % (
                    lab, _f(p['disk_mb']), _f(p['apparent_mb']), p['files']))
        except (OSError, KeyError, ValueError):
            lines.append('| Install | – | (no install-size.json) |')

    if 'startup' in scen:
        d, meta = scen['startup']
        try:
            s = json.load(open(os.path.join(d, 'startup.json')))['summary']
            lines.append('| Startup (warm, ×%s after %s discarded, saved sign-in) | Launch → window visible / → settled '
                         '(median, min–max) | **%s ms** (%s–%s) / **%s ms** (%s–%s) |' % (
                             s.get('runs'), s.get('discarded'), _f(s.get('visible_ms_median'), 0),
                             _f(s.get('visible_ms_min'), 0), _f(s.get('visible_ms_max'), 0),
                             _f(s.get('settled_ms_median'), 0), _f(s.get('settled_ms_min'), 0),
                             _f(s.get('settled_ms_max'), 0)))
        except (OSError, KeyError, ValueError):
            lines.append('| Startup | – | (no startup.json) |')

    for name, desc in (('idle-home', 'Idle, signed in, Home (window focused)'),
                       ('idle-channel', 'Idle, Daily Gaming #general open (10k messages), another window focused, '
                                        'client visible'),
                       ('idle-channel-hidden', 'Idle, #general open, client minimized (page hidden; what Windows '
                                               'occlusion tracking does to a covered window)')):
        if name not in scen:
            continue
        d, meta = scen[name]
        try:
            j = json.load(open(os.path.join(d, 'summary.json')))
        except (OSError, ValueError):
            lines.append('| %s | – | (no summary.json) |' % desc)
            continue
        s = j['summary']
        w = j.get('window', {})
        foc = (j.get('focus') or {}).get('focused_fraction')
        m = j['meta']
        head = '%s; settle %s s, %s samples at %s s; focused %s of the time' % (
            desc, _f(m.get('settle_s'), 0), m.get('samples'), _f(m.get('interval_s'), 0),
            '–' if foc is None else '%d %%' % round(foc * 100))
        lines.append('| %s | Private (Private_Clean + Private_Dirty) / private dirty / PSS / RSS, MB (mean) | '
                     '**%s** / %s / %s / %s |' % (head, _m(s, 'private_mb'), _m(s, 'private_dirty_mb'),
                                                  _m(s, 'pss_mb'), _m(s, 'rss_mb')))
        lines.append('| | CPU, %% of one core (mean / p95) | **%s %%** / %s %% |' % (
            _m(s, 'cpu_core_pct', 'mean', 2), _m(s, 'cpu_core_pct', 'p95', 2)))
        vol = s.get('ctxsw_vol_s') or {}
        own = s.get('own_wakeups_s') or {}
        un = _unnamed_summary(os.path.join(d, 'threads.csv'))
        lines.append('| | Voluntary context switches / s (p50 / p95); own threads\' wakeups (p50 / p95); all '
                     'switches incl. preemptions (p50) | **%s** / %s; own %s / %s; all %s |' % (
                         _f(vol.get('p50'), 0), _f(vol.get('p95'), 0), _f(own.get('p50'), 0), _f(own.get('p95'), 0),
                         _m(s, 'ctx_switches_s', 'p50', 0)))
        if un:
            lines.append('| | Of which unnamed threads (NVIDIA GL driver threads in the GPU process, see README) '
                         '| %d threads, %s wakeups / s |' % (un[0], _f(un[1], 0)))
        lines.append('| | Top named threads by wakeups (process thread voluntary/s) | %s |' % _top_wakeups(
            os.path.join(d, 'threads.csv')))
        lines.append('| | Processes / threads (mean) | %s / %s |' % (_m(s, 'procs', 'mean', 0),
                                                                    _m(s, 'threads', 'mean', 0)))
        lines.append('| | GPU memory (fb MB, mean) / GPU SM %% (mean / p95) | %s MB / %s %% / %s %% |' % (
            _m(s, 'gpu_fb_mb'), _m(s, 'gpu_sm_pct', 'mean', 1), _m(s, 'gpu_sm_pct', 'p95', 1)))
        lines.append('| | Frames handed to the compositor / s (mean) | %s |' % _m(s, 'frames_s', 'mean', 2))
    if 'scroll' in scen:
        d, meta = scen['scroll']
        try:
            s = json.load(open(os.path.join(d, 'scroll.json')))
            iv = s.get('interval_ms_median_over_runs') or {}
            lines.append('| Scroll #general into history, %s px/s for %s s, ×%s (rAF-driven) | Frame interval p50 / '
                         'p95 / p99 / max, ms (median over runs) | **%s** / %s / %s / %s |' % (
                             meta.get('speed'), meta.get('seconds'), s.get('ok_runs'), _f(iv.get('p50'), 2),
                             _f(iv.get('p95'), 2), _f(iv.get('p99'), 2), _f(iv.get('max'), 2)))
            lines.append('| | Missed vsyncs (interval > 1.5 refresh periods) per run; frames / expected; refresh; '
                         'window visible + focused | %s; %s / %s; %s Hz; %s, focus %s |' % (
                             s.get('missed_vsyncs'), s.get('frames'), s.get('expected_frames'), s.get('refresh_hz'),
                             s.get('visible'), s.get('has_focus')))
        except (OSError, ValueError):
            lines.append('| Scroll | – | (no scroll.json) |')
    lines.append('')
    lines.append('Result directories:')
    for name, (d, _) in scen.items():
        lines.append('- %s: `%s`' % (name, os.path.relpath(d, os.path.dirname(os.path.dirname(PERF)))))
    notes = ['%s: %s' % (k, n) for k, (_, m) in scen.items() for n in (m.get('notes') or [])]
    if notes:
        lines.append('')
        lines.append('Notes:')
        for n in notes:
            lines.append('- %s' % n)
    text = '\n'.join(lines) + '\n'
    if a.out:
        with open(a.out, 'w') as f:
            f.write(text)
    print(text)
    return 0


def load_now():
    """loadavg + top CPU users over 1 s (by schedstat-free /proc/<pid>/stat ticks)."""
    def snap():
        d = {}
        for p in os.listdir('/proc'):
            if p.isdigit():
                st = procfs.stat(int(p))
                if st:
                    d[int(p)] = (st.cpu_ticks, st.comm)
        return d
    a = snap()
    time.sleep(1.0)
    b = snap()
    tck = os.sysconf('SC_CLK_TCK')
    busy = sorted(((b[p][0] - a[p][0]) * 100.0 / tck, b[p][1], p) for p in b if p in a and b[p][0] > a[p][0])[::-1]
    with open('/proc/loadavg') as f:
        la = f.read().split()[:3]
    return {'loadavg': [float(x) for x in la],
            'busiest': [{'comm': c, 'pid': p, 'cpu_core_pct': round(v, 1)} for v, c, p in busy[:6]],
            'total_cpu_core_pct': round(sum(v for v, _, _ in busy), 1)}


def meta(a):
    path = os.path.join(a.dir, 'scenario.json')
    d = {}
    if os.path.exists(path):
        d = json.load(open(path))
    for kv in a.set or []:
        k, v = kv.split('=', 1)
        d[k] = v
    for kv in a.json or []:
        k, v = kv.split('=', 1)
        try:
            d[k] = json.loads(v) if v.strip() else None
        except ValueError:
            d[k] = v
    for kv in a.json_file or []:
        k, v = kv.split('=', 1)
        try:
            lines = [x for x in open(v).read().splitlines() if x.strip()]
            vals = []
            for x in lines:
                try:
                    vals.append(json.loads(x))
                except ValueError:
                    vals.append(x)
            d[k] = vals[-1] if len(vals) == 1 else vals
        except OSError:
            d[k] = None
    if a.note:
        d.setdefault('notes', []).extend(a.note)
    if a.facts:
        d['machine'] = common.machine_facts()
        d['load_at_start'] = load_now()
    common.write_json(path, d)
    return 0


def main(argv):
    ap = argparse.ArgumentParser(prog='scenario_util.py', description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest='cmd', required=True)
    p = sub.add_parser('display-server')
    p.add_argument('--pid', type=int)
    p.add_argument('--match')
    p.add_argument('--desktop-file')
    p = sub.add_parser('place')
    p.add_argument('--pid', type=int, required=True)
    p.add_argument('--w', type=int, default=1280)
    p.add_argument('--h', type=int, default=800)
    p = sub.add_parser('refresh')
    p.add_argument('--pid', type=int, required=True)
    p = sub.add_parser('scroll-summary')
    p.add_argument('dir')
    p = sub.add_parser('meta')
    p.add_argument('dir')
    p.add_argument('--set', action='append')
    p.add_argument('--json', action='append')
    p.add_argument('--json-file', action='append')
    p.add_argument('--note', action='append')
    p.add_argument('--facts', action='store_true')
    p = sub.add_parser('load')
    p = sub.add_parser('table')
    p.add_argument('client')
    p.add_argument('--quick', action='store_true')
    p.add_argument('--out')
    p.add_argument('dirs', nargs='*')
    a = ap.parse_args(argv)
    if a.cmd == 'load':
        print(json.dumps(load_now()))
        return 0
    return {'display-server': display_server, 'place': place, 'refresh': refresh, 'scroll-summary': scroll_summary,
            'meta': meta, 'table': table}[a.cmd](a)


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

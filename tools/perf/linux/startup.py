"""startup: times a desktop client's startup on Linux: launch -> first visible window of its process tree, and
launch -> settled (the tree under --settle-cpu % of one core for --settle-secs). The Linux twin of startup.ps1.

Each run starts the command in a new process group, waits for the first window owned by a process of its tree
(KWin: workspace.windowAdded, which fires when the window has its first buffer on Wayland, or is mapped with content
through XWayland; X11 fallback: _NET_CLIENT_LIST), then for the settle rule, then closes the app: SIGTERM to the root
process (an Electron app quits cleanly on it in ~70 ms; a SIGTERM to its whole group wedges it), SIGTERM to the
group and the rest of the tree after half of --grace, SIGKILL after --grace, and waits until every process of the tree
is gone before the next run. The first --discard runs pay the cold file cache and are left out of the medians.

--ready-marker REGEX watches the app's stdout + stderr: the arrival time of the first matching line is "marker_ms".
If the regex has a named group ns, us, ms or s, its value is read as a CLOCK_MONOTONIC timestamp (Rust:
libc::clock_gettime(CLOCK_MONOTONIC); Python: time.monotonic()) and reported as "first_frame_ms" from launch: exact even
when the app's output is block-buffered. Example for an app printing "first frame presented monotonic_ns=123456789":
  --ready-marker 'first frame presented monotonic_ns=(?P<ns>\\d+)'
"""
import argparse
import os
import re
import signal
import subprocess
import sys
import threading
import time

import common
import procfs

WINDOW_KINDS = ('normal', 'dialog', 'splash', 'utility')


def parse_args(argv):
    if '--' in argv:
        i = argv.index('--')
        opts, cmd = argv[:i], argv[i + 1:]
    else:
        opts, cmd = argv, []
    ap = argparse.ArgumentParser(
        prog='startup.sh', usage='startup.sh [options] -- <command> [args...]',
        formatter_class=argparse.RawDescriptionHelpFormatter, description=__doc__,
        epilog='examples:\n'
               '  startup.sh --runs 5 -- kcalc\n'
               '  startup.sh --runs 5 --absent "electron-client/node_modules/electron" -- \\\n'
               '      electron-client/node_modules/electron/dist/electron electron-client --user-data-dir=/tmp/x\n'
               '  startup.sh --ready-marker "first frame monotonic_ns=(?P<ns>\\d+)" -- ./target/release/decibell\n')
    ap.add_argument('--runs', type=int, default=5, help='kept runs (default 5)')
    ap.add_argument('--discard', type=int, default=1, help='extra first runs left out of the medians (default 1)')
    ap.add_argument('--settle-cpu', type=float, default=2.0, help='%% of one core (default 2)')
    ap.add_argument('--settle-secs', type=float, default=2.0, help='seconds under --settle-cpu (default 2)')
    ap.add_argument('--timeout', type=float, default=60, help='seconds per run before giving up (default 60)')
    ap.add_argument('--ready-marker', help='regex over the app\'s stdout/stderr lines (see above)')
    ap.add_argument('--pause', type=float, default=5, help='seconds between runs (default 5)')
    ap.add_argument('--grace', type=float, default=10, help='seconds before SIGKILL when closing (default 10)')
    ap.add_argument('--window-backend', choices=['auto', 'kwin', 'x11', 'none'], default='auto',
                    help='how windows are seen (auto: KWin scripting, else X11 if DISPLAY is set)')
    ap.add_argument('--also-match', help='regex: processes outside the tree that belong to the app (crashpad)')
    ap.add_argument('--absent', help='regex over /proc/*/cmdline: refuse to start while a match runs (a running '
                                     'instance would take the launch, or be summed in)')
    ap.add_argument('--label', default='')
    ap.add_argument('--out', help='output directory (default tools/perf/results/startup-<label>-<time>)')
    a = ap.parse_args(opts)
    if not cmd:
        ap.error('no command: put it after --')
    a.cmd = cmd
    return a


class Run:
    def __init__(self, i, discarded):
        self.i = i
        self.discarded = discarded
        self.tree = None
        self.t0_wall = None
        self.t0_mono = None
        self.window = None
        self.first_damage_ms = None
        self.visible = threading.Event()
        self.marker_ms = None
        self.first_frame_ms = None
        self.marker_line = None
        self.windows_seen = []
        self.lock = threading.Lock()


class Startup:
    def __init__(self, a):
        self.a = a
        self.cur = None
        self.backend = 'none'
        self.bridge = None
        self.xwatch = None
        self.marker = re.compile(a.ready_marker) if a.ready_marker else None
        self.notes = []

    # ---- window events (bridge / X11 thread) ----------------------------------------------------------------------
    def on_window(self, ev):
        k = ev.get('k')
        run = self.cur
        if run is None or run.tree is None:
            return
        if k == 'added':
            pid = int(ev.get('pid') or 0)
            if not pid or not run.tree.contains(pid):
                return
            kind = next((x for x in WINDOW_KINDS if ev.get(x)), 'other')
            ms = ev['t'] - run.t0_wall * 1000.0
            rx_ms = ev['rx'] * 1000.0 - run.t0_wall * 1000.0
            w = {'ms': round(ms, 1), 'rx_ms': round(rx_ms, 1), 'pid': pid, 'caption': ev.get('caption', ''),
                 'class': ev.get('cls', ''), 'kind': kind, 'id': ev.get('id'), 'geometry': ev.get('g')}
            with run.lock:
                run.windows_seen.append(w)
                if kind != 'other' and run.window is None:
                    run.window = w
                    run.visible.set()
        elif k == 'first_damage':
            with run.lock:
                if run.window and ev.get('id') == run.window['id'] and run.first_damage_ms is None:
                    run.first_damage_ms = round(ev['t'] - run.t0_wall * 1000.0, 1)

    def start_backend(self):
        want = self.a.window_backend
        if want in ('auto', 'kwin'):
            import kwin
            if kwin.available():
                self.bridge = kwin.Bridge('startup', kwin.watch_js(adds=True), self.on_window)
                self.bridge.start()
                self.backend = 'kwin'
                return
            if want == 'kwin':
                raise SystemExit('startup: KWin scripting is not on the session bus')
        if want in ('auto', 'x11') and os.environ.get('DISPLAY'):
            import x11
            if x11.X11Watch.available():
                self.xwatch = x11.X11Watch(on_window=self.on_window)
                self.backend = 'x11'
                if os.environ.get('WAYLAND_DISPLAY'):
                    self.notes.append('X11 backend under Wayland: only XWayland windows are seen')
                return
            if want == 'x11':
                raise SystemExit('startup: cannot open the X display')
        self.backend = 'none'
        self.notes.append('no window backend: visible_ms is not measured')

    def stop_backend(self):
        if self.bridge:
            self.bridge.stop()
            if self.bridge.errors:
                self.notes.extend('kwin: %s' % e for e in self.bridge.errors)
            self.bridge = None
        if self.xwatch:
            self.xwatch.stop()
            self.xwatch = None

    # ---- one run --------------------------------------------------------------------------------------------------
    def _reader(self, run, pipe, log):
        for raw in iter(pipe.readline, b''):
            now = time.monotonic()
            log.write(raw)
            if self.marker and run.marker_ms is None:
                line = raw.decode('utf-8', 'replace')
                m = self.marker.search(line)
                if m:
                    run.marker_ms = round((now - run.t0_mono) * 1000.0, 1)
                    run.marker_line = line.strip()[:200]
                    g = m.groupdict()
                    for unit, scale in (('ns', 1e-9), ('us', 1e-6), ('ms', 1e-3), ('s', 1.0)):
                        if g.get(unit):
                            run.first_frame_ms = round((float(g[unit]) * scale - run.t0_mono) * 1000.0, 1)
                            break
        log.close()

    def cpu_ns(self, run, seen):
        """Cumulative CPU ns of the tree: per-thread schedstat, keeping the last value of threads that exited."""
        for pid in run.tree.scan():
            st = procfs.stat(pid)
            if st is None:
                continue
            seen['procs'].add((pid, st.start))
            for tid in procfs.tids(pid):
                r = procfs.thread_sample(pid, tid)
                if r:
                    seen['threads'][(pid, st.start, tid)] = r[3]
        return sum(seen['threads'].values())

    def run_once(self, run, out):
        a = self.a
        log = open(os.path.join(out, 'run-%d.log' % run.i), 'wb')
        run.t0_wall = time.time()
        run.t0_mono = time.monotonic()
        proc = subprocess.Popen(a.cmd, start_new_session=True, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT)
        run.tree = procfs.Tree(roots=[proc.pid], also_match=a.also_match)
        self.cur = run
        reader = threading.Thread(target=self._reader, args=(run, proc.stdout, log), daemon=True)
        reader.start()
        seen = {'procs': set(), 'threads': {}}
        res = {'run': run.i, 'discarded': run.discarded, 'pid': proc.pid}
        deadline = run.t0_mono + a.timeout
        try:
            # Visible (the settle clock starts there, as in startup.ps1; without a backend, at launch).
            if self.backend != 'none':
                while not run.visible.wait(0.25):
                    self.cpu_ns(run, seen)
                    if time.monotonic() > deadline or proc.poll() is not None and not run.tree.scan():
                        break
            # Settled: CPU of the tree over the last settle-secs under the threshold; settled = start of that window.
            hist = []
            settled = None
            cpu_visible = cpu_quiet = None
            while time.monotonic() < deadline:
                now = time.monotonic()
                hist.append((now, self.cpu_ns(run, seen)))
                if cpu_visible is None:
                    cpu_visible = hist[-1][1]
                while len(hist) > 2 and hist[1][0] <= now - a.settle_secs:
                    hist.pop(0)
                t_old, c_old = hist[0]
                if now - t_old >= a.settle_secs - 1e-3:
                    pct = 100.0 * (hist[-1][1] - c_old) / 1e9 / (now - t_old)
                    if pct < a.settle_cpu:
                        settled = (t_old - run.t0_mono) * 1000.0
                        cpu_quiet = hist[-1][1] - c_old
                        break
                time.sleep(0.25)
            with run.lock:
                w = run.window
            res.update({
                'visible_ms': w['ms'] if w else None, 'visible_rx_ms': w['rx_ms'] if w else None,
                'first_damage_ms': run.first_damage_ms, 'marker_ms': run.marker_ms,
                'first_frame_ms': run.first_frame_ms, 'settled_ms': round(settled) if settled is not None else None,
                'window': {k: w[k] for k in ('pid', 'caption', 'class', 'kind', 'geometry')} if w else None,
                'windows_seen': len(run.windows_seen), 'processes_seen': len(seen['procs']),
                'cpu_ms_at_visible': round(cpu_visible / 1e6, 1) if cpu_visible is not None else None,
                'cpu_ms_quiet_window': round(cpu_quiet / 1e6, 1) if cpu_quiet is not None else None,
                'exited_early': proc.poll() is not None and not run.tree.scan(),
            })
            if run.marker_line:
                res['marker_line'] = run.marker_line
        finally:
            res['close'] = self.close(proc, run, seen)
            self.cur = None
            reader.join(5)
        return res

    def close(self, proc, run, seen):
        """SIGTERM the root; after grace/2 SIGTERM the group + tree; after grace SIGKILL; wait until all are gone."""
        a = self.a
        t0 = time.monotonic()
        try:
            run.tree.scan()
        except Exception:
            pass
        for pid, st in run.tree.members.items():
            seen['procs'].add((pid, st))

        def alive():
            return [(p, s) for (p, s) in seen['procs'] if (procfs.stat(p) or _Gone).start == s
                    and (procfs.stat(p) or _Gone).state != 'Z']

        def sig_all(sig):
            try:
                os.killpg(proc.pid, sig)
            except (ProcessLookupError, PermissionError):
                pass
            for p, _ in alive():
                try:
                    os.kill(p, sig)
                except (ProcessLookupError, PermissionError):
                    pass

        how = 'exited'
        if proc.poll() is None or alive():
            how = 'sigterm-root'
            try:
                proc.send_signal(signal.SIGTERM)
            except ProcessLookupError:
                pass
            stage = 0
            while True:
                proc.poll()
                for pid in run.tree.scan():  # children forked during shutdown
                    st = procfs.stat(pid)
                    if st:
                        seen['procs'].add((pid, st.start))
                if not alive() and proc.poll() is not None:
                    break
                el = time.monotonic() - t0
                if stage == 0 and el > a.grace / 2:
                    how, stage = 'sigterm-group', 1
                    sig_all(signal.SIGTERM)
                elif stage == 1 and el > a.grace:
                    how, stage = 'sigkill', 2
                    sig_all(signal.SIGKILL)
                elif stage == 2 and el > a.grace + 10:
                    how = 'sigkill (processes left: %s)' % [p for p, _ in alive()]
                    break
                time.sleep(0.05)
        try:
            proc.wait(5)
        except subprocess.TimeoutExpired:
            pass
        return {'how': how, 'ms': round((time.monotonic() - t0) * 1000)}


class _Gone:
    start = None
    state = 'Z'


def main(argv):
    a = parse_args(argv)

    def on_term(signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, on_term)

    if a.absent:
        rx = re.compile(a.absent)
        mine = set(procfs.ancestors(os.getpid()))
        hits = [p for p in (int(e) for e in os.listdir('/proc') if e.isdigit())
                if p not in mine and p != os.getpid() and rx.search(procfs.cmdline(p))]
        if hits:
            print('startup: %d process(es) match --absent (%s), e.g. %d: %s; close them first' % (
                len(hits), a.absent, hits[0], procfs.cmdline(hits[0])[:120]), file=sys.stderr)
            return 2
    out = common.out_dir(a.out, 'startup-' + (a.label or os.path.basename(a.cmd[0])))
    s = Startup(a)
    results = []
    started = time.time()
    try:
        s.start_backend()
        print('startup: %s  (window backend: %s, %d + %d runs)' % (' '.join(a.cmd), s.backend, a.discard, a.runs))
        for i in range(a.discard + a.runs):
            run = Run(i, i < a.discard)
            r = s.run_once(run, out)
            results.append(r)
            print('run %d%s: visible %s ms%s%s, settled %s ms  (window: %s; closed by %s in %d ms)' % (
                i, ' (discarded)' if run.discarded else '', _ms(r.get('visible_ms')),
                ('' if r.get('marker_ms') is None else ', marker %s ms' % _ms(r['marker_ms'])),
                ('' if r.get('first_frame_ms') is None else ', first frame %s ms' % _ms(r['first_frame_ms'])),
                _ms(r.get('settled_ms')),
                ('%s "%s" pid %d' % (r['window']['kind'], r['window']['caption'][:40], r['window']['pid'])
                 if r.get('window') else 'none'), r['close']['how'], r['close']['ms']))
            if i < a.discard + a.runs - 1:
                time.sleep(a.pause)
    except KeyboardInterrupt:
        print('startup: interrupted', file=sys.stderr)
    finally:
        s.stop_backend()

    kept = [r for r in results if not r['discarded']]
    summary = {'command': a.cmd, 'label': a.label, 'runs': len(kept), 'discarded': a.discard,
               'window_backend': s.backend,
               'settle_rule': 'tree under %g %% of one core for %g s' % (a.settle_cpu, a.settle_secs),
               'ready_marker': a.ready_marker}
    for k in ('visible_ms', 'first_damage_ms', 'marker_ms', 'first_frame_ms', 'settled_ms'):
        vals = [r.get(k) for r in kept if r.get(k) is not None]
        if vals:
            summary[k + '_median'] = common.median(vals)
            summary[k + '_min'] = min(vals)
            summary[k + '_max'] = max(vals)
    meta = {'tool': 'tools/perf/startup.sh', 'started': common.iso(started), 'ended': common.iso(),
            'machine': common.machine_facts(), 'notes': s.notes, 'cwd': os.getcwd()}
    common.write_json(os.path.join(out, 'startup.json'), {'summary': summary, 'runs': results, 'meta': meta})
    line = 'median of %d: visible %s ms (%s-%s)' % (len(kept), _ms(summary.get('visible_ms_median')),
                                                   _ms(summary.get('visible_ms_min')), _ms(summary.get('visible_ms_max')))
    if 'first_frame_ms_median' in summary:
        line += ', first frame %s ms' % _ms(summary['first_frame_ms_median'])
    if 'marker_ms_median' in summary:
        line += ', marker %s ms' % _ms(summary['marker_ms_median'])
    line += ', settled %s ms (%s-%s)' % (_ms(summary.get('settled_ms_median')), _ms(summary.get('settled_ms_min')),
                                         _ms(summary.get('settled_ms_max')))
    print(line)
    print('wrote %s/startup.json (+ run-N.log: each run\'s stdout/stderr)' % out)
    return 0 if kept and all(r.get('visible_ms') is not None or s.backend == 'none' for r in kept) else 1


def _ms(v):
    return '-' if v is None else ('%.0f' % v if v >= 100 else '%.1f' % v)


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

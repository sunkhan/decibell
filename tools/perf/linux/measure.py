"""measure: samples one desktop client's resource use (its whole process tree) on Linux for a fixed time.

The Linux twin of measure.ps1 + threads.ps1. Everything comes from /proc (no polling inside the measured app):
  CPU         per-thread schedstat (sum_exec_runtime, ns) summed over the tree: cpu_core_pct (% of one core).
              cpu_ticks_core_pct is the same from /proc/<pid>/stat utime+stime (what top/ps show: 10 ms ticks, but it
              also keeps the time of threads that exited between two samples).
  Memory      /proc/<pid>/smaps_rollup summed over the tree: private_mb = Private_Clean + Private_Dirty (headline),
              private_dirty_mb, pss_mb, rss_mb, anon_mb, swap_mb; vmrss_mb / shmem_mb from /proc/<pid>/status (the RSS
              ps and top show: it adds shmem pages the smaps walk cannot see, e.g. Wayland shm and GPU buffers).
  Wakeups     voluntary + involuntary context switches per second, from /proc/<pid>/task/<tid>/status, kept per
              thread so the summary ranks threads (threads.ps1) and splits our own threads from GPU-driver threads.
  GPU         nvidia-smi pmon stream (sm / mem / enc / dec %, fb MB per process) or DRM fdinfo (AMD / Intel).
  Frames      KWin: damage events of the tree's windows per second (frames the app handed the compositor).
  Focus       KWin: whether the active window belongs to the tree (--focus-log, logged every 5 s).
Writes <out>/samples.csv, <out>/threads.csv, <out>/summary.json (+ focus.csv) and prints a summary.
"""
import argparse
import bisect
import csv
import os
import re
import resource
import signal
import sys
import threading
import time

import common
import gpu as gpumod
import procfs

# NVIDIA's Vulkan threads are "[vkps] Update", "[vkcf] Analysis", "[vkrt] Analysis"; its CUDA thread "cuda0000a80002b".
# NVIDIA's GL driver threads have no name at all (they show the process name): see the "unnamed" kind.
DEFAULT_DRIVER_RE = r'^\[(vk|gl|cu|nv)\w*\]|^cuda[0-9a-f]{8,}$'

METRIC_ORDER = [
    'procs', 'threads', 'fds', 'cpu_core_pct', 'cpu_machine_pct', 'cpu_ticks_core_pct',
    'private_mb', 'private_dirty_mb', 'pss_mb', 'rss_mb', 'anon_mb', 'swap_mb', 'vmrss_mb', 'shmem_mb',
    'ctx_switches_s', 'ctxsw_vol_s', 'ctxsw_invol_s', 'own_ctx_switches_s', 'driver_ctx_switches_s',
    'unnamed_ctx_switches_s', 'own_wakeups_s', 'driver_wakeups_s', 'unnamed_wakeups_s',
    'gpu_sm_pct', 'gpu_mem_pct', 'gpu_enc_pct', 'gpu_dec_pct', 'gpu_fb_mb',
    'frames_s', 'focused', 'compositor_cpu_core_pct', 'compositor_gpu_sm_pct',
]


def parse_args(argv):
    ap = argparse.ArgumentParser(
        prog='measure.sh', formatter_class=argparse.RawDescriptionHelpFormatter,
        description=__doc__,
        epilog='examples:\n'
               '  measure.sh --pid 12345 --settle 120 --duration 300 --label "gpui idle channel"\n'
               '  measure.sh --match "/electron-client/node_modules/electron/dist/electron" '
               '--also-match crashpad_handler \\\n'
               '      --settle 120 --duration 300 --focus-log --label "electron idle channel"\n')
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument('--pid', type=int, action='append', help='root pid (repeatable); its descendants are included')
    g.add_argument('--match', help='regex over /proc/*/cmdline: every matching process and its descendants (not '
                                   'processes in measure.sh\'s own process group, e.g. a grep it is piped into)')
    ap.add_argument('--also-match', help='regex: add these processes and their descendants too (e.g. a crashpad '
                                         'handler that was reparented away from the root)')
    ap.add_argument('--settle', type=float, default=0, help='seconds to wait before sampling (baseline used 120)')
    ap.add_argument('--duration', type=float, default=120, help='seconds of sampling (default 120)')
    ap.add_argument('--interval', type=float, default=1.0, help='seconds between samples (default 1)')
    ap.add_argument('--mem-interval', type=float, help='seconds between smaps_rollup reads (default: every sample; '
                                                       'each read takes the target\'s mmap lock briefly)')
    ap.add_argument('--label', default='', help='free text stored with the results')
    ap.add_argument('--out', help='output directory (default tools/perf/results/<label>-<time>)')
    ap.add_argument('--top', type=int, default=15, help='threads to list in the summary (default 15)')
    ap.add_argument('--gpu', choices=['auto', 'nvml', 'pmon', 'fdinfo', 'none'], default='auto',
                    help='per-process GPU source: nvml (NVIDIA, in-process, cheap), pmon (a long-lived nvidia-smi pmon '
                         'stream, ~10x the cost), fdinfo (DRM fdinfo: AMD / Intel); auto = nvml, else fdinfo')
    ap.add_argument('--focus-log', action='store_true',
                    help='record whether the tree\'s window is the active one (KWin; every 5 s + per sample)')
    ap.add_argument('--no-frames', action='store_true', help='do not count window damage through KWin')
    ap.add_argument('--include-compositor', action='store_true',
                    help='also record kwin_wayland\'s CPU and GPU sm %% (the composition cost both clients pay)')
    ap.add_argument('--driver-threads', default=DEFAULT_DRIVER_RE,
                    help='regex over thread names counted as GPU-driver threads, not ours (default: %(default)s). '
                         'Threads still named like their process (not the main thread) are reported apart as '
                         '"unnamed": NVIDIA\'s GL driver threads, and any thread an app spawned without a name')
    ap.add_argument('--wait', type=float, default=0, help='with --match: wait up to this many seconds for a match')
    ap.add_argument('--quiet', action='store_true', help='no progress lines')
    return ap.parse_args(argv)


class Proc:
    __slots__ = ('pid', 'start', 'comm', 'label', 'cmd', 'last_ticks', 'ticks', 'cpu_ns', 'ctx', 'mem', 'mem_src',
                 'fds', 'threads', 'first', 'last', 'gone')

    def __init__(self, pid, st, born):
        self.pid = pid
        self.start = st.start
        self.comm = st.comm
        self.cmd = procfs.cmdline(pid)
        self.label = procfs.label(st.comm, self.cmd)
        self.last_ticks = 0 if born else st.cpu_ticks
        self.ticks = 0
        self.cpu_ns = 0
        self.ctx = 0
        self.mem = {}
        self.mem_src = ''
        self.fds = 0
        self.threads = 0
        self.first = self.last = None
        self.gone = False


class Thread:
    __slots__ = ('pid', 'tid', 'proc', 'name', 'vol', 'invol', 'ns', 'svol', 'sinvol', 'sns', 'first', 'last',
                 'kind', 'alive', 'wchan')

    def __init__(self, pid, tid, proc, name, vol, invol, ns, born):
        self.pid, self.tid, self.proc, self.name = pid, tid, proc, name
        self.vol, self.invol, self.ns = (0, 0, 0) if born else (vol, invol, ns)
        self.svol = self.sinvol = self.sns = 0
        self.first = self.last = None
        self.kind = 'own'
        self.alive = True
        self.wchan = ''


class KWinWatch:
    """Frames (window damage) and focus from a KWin script; all state behind one lock."""

    def __init__(self, tree, frames, focus):
        import kwin
        self.lock = threading.Lock()
        self.frame_ts = {}   # window id -> [wall-clock seconds of each damage event] (KWin's Date.now())
        self.windows = {}
        self.active = {'pid': 0}
        self.tree = tree
        self.bridge = kwin.Bridge('measure', kwin.watch_js(adds=False, focus=focus, frames=frames), self.on_event,
                                  is_member=tree.contains)
        self.bridge.start()

    def on_event(self, ev):
        k = ev.get('k')
        with self.lock:
            if k == 'frames':
                self.frame_ts.setdefault(ev['id'], []).extend(t / 1000.0 for t in ev.get('ts', ()))
            elif k == 'attached':
                self.windows[ev['id']] = {'pid': ev.get('pid'), 'caption': ev.get('caption'), 'cls': ev.get('cls')}
            elif k == 'activated':
                self.active = ev

    def frames(self):
        """{window id: sorted damage times}. Batches arrive up to 250 ms after their first frame: bucket after the run."""
        with self.lock:
            return {wid: sorted(ts) for wid, ts in self.frame_ts.items()}

    def stop(self):
        self.bridge.stop()


class Sampler:
    def __init__(self, a, tree, pmon, fdinfo, watch):
        self.a = a
        self.tree = tree
        self.pmon = pmon
        self.fdinfo = fdinfo
        self.watch = watch
        self.driver_re = re.compile(a.driver_threads) if a.driver_threads else None
        self.procs = {}
        self.ended = []
        self.threads = {}
        self.prev_mono = None
        self.prev_boot = None
        self.last_mem = -1e9
        self.mem_interval = a.mem_interval if a.mem_interval else a.interval
        self.cores = os.cpu_count() or 1
        self.loop_ms = []
        self.smaps_ms = []
        self.comp = None
        self.comp_ticks = None
        self.base_wall = None
        if a.include_compositor:
            self.comp = [p for p in (_pid_of('kwin_wayland'), _pid_of('Xwayland'), _pid_of('kwin_x11')) if p]

    def _kind(self, pid, tid, name, comm):
        """main (the process's first thread), driver (--driver-threads), unnamed (still named like the process), own."""
        if tid == pid:
            return 'main'
        if self.driver_re and self.driver_re.search(name):
            return 'driver'
        if name == comm:
            return 'unnamed'
        return 'own'

    def collect(self):
        """One sample. Returns a row (None for the first call, which only sets the baseline)."""
        t_start = time.perf_counter()
        now_mono = time.monotonic()
        now_wall = time.time()
        prev_boot = self.prev_boot
        self.prev_boot = procfs.boot_ticks()
        first = self.prev_mono is None
        dt = None if first else now_mono - self.prev_mono
        members = self.tree.scan()

        mem_due = now_mono - self.last_mem >= self.mem_interval - 0.05
        smaps_t = 0.0
        tot = dict(ticks=0, ns=0, vol=0, invol=0, dctx=0, uctx=0, dvol=0, uvol=0, threads=0, fds=0)
        mem = dict(rss=0, pss=0, private_clean=0, private_dirty=0, anon=0, swap=0, vmrss=0, shmem=0)
        live = set()
        for pid in members:
            st = procfs.stat(pid)
            if st is None:
                continue
            p = self.procs.get(pid)
            new = p is None or p.start != st.start
            if new:
                if p is not None:
                    p.gone = True
                    self.ended.append(p)
                born = (not first) and prev_boot is not None and st.start >= prev_boot
                p = Proc(pid, st, born)
                p.first = now_mono
                self.procs[pid] = p
            live.add(pid)
            p.last = now_mono
            d_ticks = max(0, st.cpu_ticks - p.last_ticks)
            p.last_ticks = st.cpu_ticks
            if not first:
                p.ticks += d_ticks
                tot['ticks'] += d_ticks
            p.threads = st.threads
            tot['threads'] += st.threads
            fds = procfs.fd_count(pid)
            p.fds = fds or 0
            tot['fds'] += p.fds
            if mem_due or not p.mem:
                t0 = time.perf_counter()
                try:
                    p.mem = procfs.smaps_rollup(pid)
                    p.mem_src = 'smaps_rollup'
                except PermissionError:
                    try:
                        p.mem = procfs.status_mem(pid)
                        p.mem_src = 'status'
                    except OSError:
                        p.mem = {}
                except OSError:
                    pass
                smaps_t += time.perf_counter() - t0
                try:
                    p.mem['vmrss'], p.mem['shmem'] = procfs.status_rss(pid)
                except (OSError, ValueError):
                    pass
            for k in mem:
                mem[k] += p.mem.get(k, 0)
            # Threads: a thread first seen in a process we already sampled was born in this interval, so all of its
            # counts are new; in a process seen for the first time they are new only if the process was born after the
            # previous sample.
            for tid in procfs.tids(pid):
                r = procfs.thread_sample(pid, tid)
                if r is None:
                    continue
                name, vol, invol, ns, _runs = r
                key = (pid, tid)
                t = self.threads.get(key)
                if t is None or t.proc is not p:
                    born_t = (not first) and (not new or (prev_boot is not None and st.start >= prev_boot))
                    t = Thread(pid, tid, p, name, vol, invol, ns, born_t)
                    t.first = now_mono
                    t.kind = self._kind(pid, tid, name, st.comm)
                    self.threads[key] = t
                dv, di, dn = max(0, vol - t.vol), max(0, invol - t.invol), max(0, ns - t.ns)
                t.vol, t.invol, t.ns = vol, invol, ns
                t.last = now_mono
                t.alive = True
                if name != t.name:
                    t.name = name
                    t.kind = self._kind(pid, tid, name, st.comm)
                if first:
                    continue
                t.svol += dv
                t.sinvol += di
                t.sns += dn
                p.cpu_ns += dn
                p.ctx += dv + di
                tot['ns'] += dn
                tot['vol'] += dv
                tot['invol'] += di
                if t.kind == 'driver':
                    tot['dctx'] += dv + di
                    tot['dvol'] += dv
                elif t.kind == 'unnamed':
                    tot['uctx'] += dv + di
                    tot['uvol'] += dv
        for pid in list(self.procs):
            if pid not in live:
                p = self.procs.pop(pid)
                p.gone = True
                self.ended.append(p)
        for t in self.threads.values():
            if t.last != now_mono:
                t.alive = False
        if mem_due:
            self.last_mem = now_mono
            self.smaps_ms.append(smaps_t * 1000)

        gpu = None
        if self.pmon:
            gpu, _ = self.pmon.sum(live)
        fd_util = fd_mem = None
        if self.fdinfo:
            fd_util, fd_mem = self.fdinfo.sample(live)
        comp_cpu = comp_sm = None
        if self.comp:
            sts = [procfs.stat(c) for c in self.comp]
            ticks = sum(s.cpu_ticks for s in sts if s)
            if self.comp_ticks is not None and dt:
                comp_cpu = 100.0 * (ticks - self.comp_ticks) / procfs.CLK_TCK / dt
            self.comp_ticks = ticks
            if self.pmon:
                comp_sm = self.pmon.sum_last(self.comp)['sm']
        focused = None
        if self.watch and self.a.focus_log:
            focused = 1 if self.tree.contains(int(self.watch.active.get('pid') or 0)) else 0

        self.prev_mono = now_mono
        self.loop_ms.append((time.perf_counter() - t_start) * 1000)
        if first:
            self.base_wall = now_wall
            return None

        kb = 1024.0
        ctx = tot['vol'] + tot['invol']
        dctx, uctx = tot['dctx'], tot['uctx']
        row = {
            't': common.iso(now_wall), 'elapsed_s': None, '_wall': now_wall,
            'procs': len(live), 'threads': tot['threads'], 'fds': tot['fds'],
            'cpu_core_pct': 100.0 * tot['ns'] / 1e9 / dt,
            'cpu_machine_pct': 100.0 * tot['ns'] / 1e9 / dt / self.cores,
            'cpu_ticks_core_pct': 100.0 * tot['ticks'] / procfs.CLK_TCK / dt,
            'private_mb': (mem['private_clean'] + mem['private_dirty']) / kb,
            'private_dirty_mb': mem['private_dirty'] / kb,
            'pss_mb': mem['pss'] / kb, 'rss_mb': mem['rss'] / kb,
            'anon_mb': mem['anon'] / kb, 'swap_mb': mem['swap'] / kb,
            'vmrss_mb': mem['vmrss'] / kb, 'shmem_mb': mem['shmem'] / kb,
            'ctx_switches_s': ctx / dt, 'ctxsw_vol_s': tot['vol'] / dt, 'ctxsw_invol_s': tot['invol'] / dt,
            'own_ctx_switches_s': (ctx - dctx - uctx) / dt, 'driver_ctx_switches_s': dctx / dt,
            'unnamed_ctx_switches_s': uctx / dt,
            # Wakeups = voluntary switches only (the thread blocked and was woken), by kind: the D12 figure is own.
            'own_wakeups_s': (tot['vol'] - tot['dvol'] - tot['uvol']) / dt, 'driver_wakeups_s': tot['dvol'] / dt,
            'unnamed_wakeups_s': tot['uvol'] / dt,
        }
        if gpu is not None:
            row.update({'gpu_sm_pct': gpu['sm'], 'gpu_mem_pct': gpu['mem'], 'gpu_enc_pct': gpu['enc'],
                        'gpu_dec_pct': gpu['dec'], 'gpu_fb_mb': gpu['fb']})
        if fd_util is not None:
            for e, v in fd_util.items():
                row['gpu_%s_pct' % e] = v
            for r, v in fd_mem.items():
                row['gpu_%s_mb' % r] = v
        if focused is not None:
            row['focused'] = focused
        if comp_cpu is not None:
            row['compositor_cpu_core_pct'] = comp_cpu
        if comp_sm is not None:
            row['compositor_gpu_sm_pct'] = comp_sm
        return row


def _pid_of(comm):
    for e in os.scandir('/proc'):
        if e.name.isdigit():
            st = procfs.stat(int(e.name))
            if st and st.comm == comm:
                return st.pid
    return None


def _self_ctx():
    n = 0
    for tid in procfs.tids(os.getpid()):
        r = procfs.thread_sample(os.getpid(), tid)
        if r:
            n += r[1] + r[2]
    return n


def main(argv):
    a = parse_args(argv)
    stop = threading.Event()
    first_signal = []

    def on_signal(signum, _frame):
        # One signal stops sampling and still writes the results. A second one 2 s later aborts (a group-wide signal
        # from `timeout` or a terminal arrives twice at once: that is still one request).
        if first_signal and time.monotonic() - first_signal[0] > 2:
            raise KeyboardInterrupt
        if not first_signal:
            first_signal.append(time.monotonic())
            print('\nstopping early (signal %d); writing what was sampled' % signum, file=sys.stderr)
        stop.set()

    signal.signal(signal.SIGINT, on_signal)
    signal.signal(signal.SIGTERM, on_signal)

    tree = procfs.Tree(roots=a.pid or (), match=a.match, also_match=a.also_match)
    members = tree.scan()
    deadline = time.monotonic() + a.wait
    while not members and time.monotonic() < deadline and not stop.is_set():
        stop.wait(0.5)
        members = tree.scan()
    if not members:
        print('measure: no process matches (%s). Processes in measure.sh\'s own process group (a pipeline or script it '
              'runs in without job control) are never matched: start the app with setsid, or pass --pid.' % (
                  a.match or a.pid), file=sys.stderr)
        return 2
    if a.pid:
        root = a.pid[0]
    else:
        root = min(members, key=lambda p: (members[p], p))  # the earliest started matching process
    root_info = {'pid': root, 'cmdline': procfs.cmdline(root), 'exe': procfs.exe(root)}
    out = common.out_dir(a.out, a.label or 'measure')

    notes = []
    pmon = fdinfo = watch = None
    gpu_route = 'none'
    try:
        if a.gpu in ('auto', 'nvml') and gpumod.Nvml.available():
            pmon = gpumod.Nvml(a.interval)
            gpu_route = 'nvml (in-process: nvmlDeviceGetProcessUtilization + Get*RunningProcesses_v3)'
        elif a.gpu == 'pmon' and gpumod.NvidiaPmon.available():
            pmon = gpumod.NvidiaPmon(a.interval)
            tree.exclude(pmon.proc.pid)
            gpu_route = 'nvidia-smi pmon -d %d -s um' % pmon.interval
        elif a.gpu in ('nvml', 'pmon'):
            notes.append('NVIDIA route %s not usable: no GPU columns' % a.gpu)
        if a.gpu == 'fdinfo' or (a.gpu == 'auto' and pmon is None):
            fdinfo = gpumod.DrmFdinfo()
            gpu_route = 'drm fdinfo'
        want_kwin = a.focus_log or not a.no_frames
        if want_kwin:
            import kwin
            if kwin.available():
                watch = KWinWatch(tree, frames=not a.no_frames, focus=a.focus_log)
            else:
                notes.append('KWin scripting not on the session bus: no frames_s / focus columns')
                if a.focus_log:
                    print('measure: --focus-log needs KWin (KDE Plasma); continuing without it', file=sys.stderr)

        if not a.quiet:
            print('measuring %d process(es) of %s (root %d):' % (len(members), a.match or 'pid %s' % a.pid, root))
            for pid in sorted(members)[:20]:
                print('  %7d  %s' % (pid, procfs.cmdline(pid)[:110]))
            print('output: %s' % out)
        if a.settle > 0:
            if not a.quiet:
                print('settle %g s' % a.settle)
            stop.wait(a.settle)
        if isinstance(pmon, gpumod.NvidiaPmon):  # make sure the first GPU sample is in before the baseline
            t_wait = time.monotonic() + 3
            while pmon.latest()[1] is None and time.monotonic() < t_wait:
                time.sleep(0.1)

        sampler = Sampler(a, tree, pmon, fdinfo, watch)
        n = max(1, int(round(a.duration / a.interval)))
        if not a.quiet:
            print('sampling every %g s, %d samples' % (a.interval, n))
        ru0 = resource.getrusage(resource.RUSAGE_SELF)
        ctx0 = _self_ctx()
        pmon_t0 = pmon.cpu_ticks() if pmon else None
        sampler.collect()
        t0 = time.monotonic()
        wall0 = time.time()
        rows = []
        focus_log = []
        next_focus = 0.0
        for k in range(1, n + 1):
            if stop.wait(max(0.0, t0 + k * a.interval - time.monotonic())):
                break
            row = sampler.collect()
            row['elapsed_s'] = round(time.monotonic() - t0, 3)
            rows.append(row)
            if a.focus_log and watch and row['elapsed_s'] >= next_focus:
                act = dict(watch.active)
                mine = tree.contains(int(act.get('pid') or 0))
                focus_log.append({'t': row['t'], 'elapsed_s': row['elapsed_s'], 'focused': int(mine),
                                  'active_pid': act.get('pid'), 'active_class': act.get('cls', ''),
                                  'active_caption': act.get('caption', '') if mine else ''})
                next_focus += 5.0
            if not sampler.procs:
                notes.append('every measured process exited after %.0f s' % row['elapsed_s'])
                print('measure: every measured process exited', file=sys.stderr)
                break
            if not a.quiet and k % max(1, int(30 / a.interval)) == 0:
                print('  %4.0f s  cpu %.2f %%  private %.1f MB  ctx/s %.0f' % (
                    row['elapsed_s'], row['cpu_core_pct'], row['private_mb'], row['ctx_switches_s']))
        window = time.monotonic() - t0
        ru1 = resource.getrusage(resource.RUSAGE_SELF)
        ctx1 = _self_ctx()
        pmon_t1 = pmon.cpu_ticks() if pmon else None
        wall1 = time.time()
        frames_by_window = None
        if watch and not a.no_frames:
            time.sleep(0.4)  # the last batch of damage times leaves KWin 250 ms after its first frame
            frames_by_window = watch.frames()
    finally:
        if watch:
            watch.stop()
            if watch.bridge.errors:
                notes.extend('kwin: %s' % e for e in watch.bridge.errors)
        if pmon:
            pmon.stop()

    if not rows:
        print('measure: no samples', file=sys.stderr)
        return 1

    # Frames: every damage time bucketed into the sample intervals (KWin's Date.now() and our time.time() are both
    # CLOCK_REALTIME).
    frame_total = None
    if frames_by_window is not None:
        all_ts = sorted(t for ts in frames_by_window.values() for t in ts)
        prev = sampler.base_wall
        for r in rows:
            n = bisect.bisect_right(all_ts, r['_wall']) - bisect.bisect_right(all_ts, prev)
            r['frames_s'] = n / max(1e-9, r['_wall'] - prev)
            prev = r['_wall']
        frame_total = bisect.bisect_right(all_ts, rows[-1]['_wall']) - bisect.bisect_right(all_ts, sampler.base_wall)

    # ---- outputs --------------------------------------------------------------------------------------------------
    for r in rows:
        r.pop('_wall', None)
    cols = ['t', 'elapsed_s'] + [m for m in METRIC_ORDER if any(m in r for r in rows)]
    extra = sorted({k for r in rows for k in r} - set(cols))
    cols += extra
    with open(os.path.join(out, 'samples.csv'), 'w', newline='') as f:
        w = csv.writer(f)
        w.writerow(cols)
        for r in rows:
            w.writerow([_fmt(r.get(c)) for c in cols])

    threads = list(sampler.threads.values())
    for t in threads:
        t.wchan = procfs.wchan(t.pid, t.tid) if t.alive else ''
    threads.sort(key=lambda t: -(t.svol + t.sinvol))
    with open(os.path.join(out, 'threads.csv'), 'w', newline='') as f:
        w = csv.writer(f)
        w.writerow(['pid', 'tid', 'process', 'name', 'kind', 'seen_s', 'ctx_switches_s', 'vol_s', 'invol_s',
                    'cpu_core_pct', 'vol_total', 'invol_total', 'cpu_ms', 'wchan_at_end', 'alive_at_end'])
        for t in threads:
            seen = (t.last - t.first) if (t.last and t.first) else 0
            w.writerow([t.pid, t.tid, t.proc.label, t.name, t.kind, round(seen, 1),
                        _r((t.svol + t.sinvol) / window), _r(t.svol / window), _r(t.sinvol / window),
                        _r(100.0 * t.sns / 1e9 / window, 4), t.svol, t.sinvol, round(t.sns / 1e6, 3), t.wchan,
                        int(t.alive)])
    if focus_log:
        with open(os.path.join(out, 'focus.csv'), 'w', newline='') as f:
            w = csv.DictWriter(f, fieldnames=list(focus_log[0]))
            w.writeheader()
            w.writerows(focus_log)

    metrics = [c for c in cols if c not in ('t', 'elapsed_s')]
    summary = {m: common.stat([r.get(m) for r in rows]) for m in metrics}
    tot_ctx = sum(t.svol + t.sinvol for t in threads)
    drv_ctx = sum(t.svol + t.sinvol for t in threads if t.kind == 'driver')
    unn_ctx = sum(t.svol + t.sinvol for t in threads if t.kind == 'unnamed')
    vol = {k: sum(t.svol for t in threads if t.kind == k) for k in ('main', 'own', 'driver', 'unnamed')}
    whole = {
        'seconds': round(window, 3),
        'cpu_core_pct': _r(100.0 * sum(t.sns for t in threads) / 1e9 / window, 4),
        'cpu_ticks_core_pct': _r(100.0 * (sum(p.ticks for p in list(sampler.procs.values()) + sampler.ended))
                                 / procfs.CLK_TCK / window, 4),
        'ctx_switches_s': _r(tot_ctx / window), 'own_ctx_switches_s': _r((tot_ctx - drv_ctx - unn_ctx) / window),
        'driver_ctx_switches_s': _r(drv_ctx / window), 'unnamed_ctx_switches_s': _r(unn_ctx / window),
        'own_wakeups_s': _r((vol['main'] + vol['own']) / window), 'driver_wakeups_s': _r(vol['driver'] / window),
        'unnamed_wakeups_s': _r(vol['unnamed'] / window),
        'threads_by_kind': {k: sum(1 for t in threads if t.kind == k and t.alive)
                            for k in ('main', 'own', 'driver', 'unnamed')},
    }
    if frame_total is not None:
        whole['frames_s'] = _r(frame_total / window, 3)
        whole['frames'] = frame_total
    procs_out = []
    for p in sorted(list(sampler.procs.values()) + sampler.ended, key=lambda p: -p.cpu_ns):
        life = max(1e-9, (p.last or 0) - (p.first or 0)) if p.gone else window
        procs_out.append({
            'pid': p.pid, 'label': p.label, 'comm': p.comm, 'exited': p.gone,
            'cpu_core_pct': _r(100.0 * p.cpu_ns / 1e9 / window, 4),
            'ctx_switches_s': _r(p.ctx / window), 'threads': p.threads, 'fds': p.fds,
            'private_mb': _r((p.mem.get('private_clean', 0) + p.mem.get('private_dirty', 0)) / 1024),
            'private_dirty_mb': _r(p.mem.get('private_dirty', 0) / 1024), 'pss_mb': _r(p.mem.get('pss', 0) / 1024),
            'rss_mb': _r(p.mem.get('rss', 0) / 1024), 'mem_source': p.mem_src, 'seen_s': _r(life),
            'cmdline': p.cmd[:300]})
    top = []
    for t in threads[:a.top]:
        top.append({'pid': t.pid, 'tid': t.tid, 'process': t.proc.label, 'name': t.name, 'kind': t.kind,
                    'ctx_switches_s': _r((t.svol + t.sinvol) / window), 'vol_s': _r(t.svol / window),
                    'invol_s': _r(t.sinvol / window), 'cpu_core_pct': _r(100.0 * t.sns / 1e9 / window, 4),
                    'wchan': t.wchan, 'alive': t.alive})
    self_cpu = (ru1.ru_utime + ru1.ru_stime - ru0.ru_utime - ru0.ru_stime)
    overhead = {
        'cpu_core_pct': _r(100.0 * self_cpu / window, 4),
        'ctx_switches_s': _r((ctx1 - ctx0) / window),
        'nvidia_smi_cpu_core_pct': _r(100.0 * (pmon_t1 - pmon_t0) / procfs.CLK_TCK / window, 4)
        if pmon and pmon_t0 is not None and pmon_t1 is not None else None,
        'loop_ms': common.stat(sampler.loop_ms[1:]),
        'smaps_rollup_ms': common.stat(sampler.smaps_ms[1:]),
        'tree_scan_ms_mean': _r(1000.0 * tree.scan_s / max(1, len(sampler.loop_ms)), 3),
        'kwin_events': watch.bridge.events if watch else None,
        'gpu_query_ms_mean': _r(1000.0 * (getattr(pmon, 'cost_s', 0) + getattr(fdinfo, 'cost_s', 0))
                                / max(1, len(sampler.loop_ms)), 3),
    }
    frames_info = None
    if frames_by_window is not None:
        frames_info = []
        for wid, info in watch.windows.items():
            ts = frames_by_window.get(wid, [])
            n = bisect.bisect_right(ts, wall1) - bisect.bisect_right(ts, sampler.base_wall)
            frames_info.append({'id': wid, 'pid': info.get('pid'), 'caption': info.get('caption'),
                                'class': info.get('cls'), 'frames': n, 'frames_s': _r(n / window, 3)})
    meta = {
        'tool': 'tools/perf/measure.sh', 'label': a.label, 'started': common.iso(wall0), 'ended': common.iso(wall1),
        'settle_s': a.settle, 'duration_s': a.duration, 'interval_s': a.interval,
        'mem_interval_s': a.mem_interval or a.interval, 'samples': len(rows),
        'root': root_info, 'pids': a.pid, 'match': a.match, 'also_match': a.also_match,
        'gpu_route': gpu_route, 'kwin_frames': bool(watch and not a.no_frames), 'focus_log': bool(watch and a.focus_log),
        'driver_thread_regex': a.driver_threads, 'machine': common.machine_facts(), 'notes': notes,
        'command': ' '.join([os.path.join(common.PERF_DIR, 'measure.sh')] + argv),
    }
    result = {'meta': meta, 'summary': summary, 'window': whole, 'top_threads': top, 'processes': procs_out,
              'windows_counted': frames_info, 'sampler_overhead': overhead}
    if focus_log:
        result['focus'] = {'focused_fraction': _r(sum(f['focused'] for f in focus_log) / len(focus_log)),
                           'checks': len(focus_log)}
    common.write_json(os.path.join(out, 'summary.json'), result)

    print_report(a, rows, metrics, summary, whole, procs_out, top, overhead, frames_info, result.get('focus'), out)
    return 0


def _r(v, nd=2):
    return None if v is None else round(v, nd)


def _fmt(v):
    if isinstance(v, float):
        return '%.4f' % v if abs(v) < 10 else '%.2f' % v
    return '' if v is None else v


def print_report(a, rows, metrics, summary, whole, procs, top, overhead, frames_info, focus, out):
    print()
    print('%s  (%d samples x %gs)' % (a.label or '(no label)', len(rows), a.interval))
    print('%-24s %10s %10s %10s %10s' % ('metric', 'mean', 'p50', 'p95', 'max'))
    for m in metrics:
        x = summary[m]
        if x:
            print('%-24s %10s %10s %10s %10s' % (m, _num(x['mean']), _num(x['p50']), _num(x['p95']), _num(x['max'])))
    print('whole window: cpu %.4f %% of one core (ticks: %s %%), frames %s/s' % (
        whole['cpu_core_pct'], whole['cpu_ticks_core_pct'], whole.get('frames_s', '-')))
    print('context switches %.1f/s = own %.1f (main + named threads) + driver %.1f (--driver-threads) + unnamed %.1f '
          '(threads still named like their process)' % (
              whole['ctx_switches_s'], whole['own_ctx_switches_s'], whole['driver_ctx_switches_s'],
              whole['unnamed_ctx_switches_s']))
    print('wakeups (voluntary switches) by kind: own %.1f/s, driver %.1f/s, unnamed %.1f/s' % (
        whole['own_wakeups_s'], whole['driver_wakeups_s'], whole['unnamed_wakeups_s']))
    if focus:
        print('focus: the measured window was active in %.0f %% of %d checks' % (
            100 * focus['focused_fraction'], focus['checks']))
    print()
    print('processes (cpu and wakeups averaged over the window; memory at the last read)')
    print('  %7s %-28s %8s %8s %8s %9s %7s' % ('pid', 'process', 'cpu %', 'ctx/s', 'thr', 'priv MB', 'pss MB'))
    for p in procs:
        print('  %7d %-28s %8.3f %8.1f %8d %9.1f %7.1f%s' % (
            p['pid'], p['label'][:28], p['cpu_core_pct'], p['ctx_switches_s'], p['threads'], p['private_mb'],
            p['pss_mb'], '  (exited)' if p['exited'] else ''))
    print()
    print('top %d threads by context switches/s' % len(top))
    print('  %8s %8s %8s %8s  %-22s %8s  %-16s %-8s %s' % ('ctx/s', 'vol/s', 'invol/s', 'cpu %', 'process', 'tid',
                                                         'name', 'kind', 'wchan at end'))
    for t in top:
        print('  %8.1f %8.1f %8.1f %8.3f  %-22s %8d  %-16s %-8s %s' % (
            t['ctx_switches_s'], t['vol_s'], t['invol_s'], t['cpu_core_pct'], t['process'][:22], t['tid'],
            t['name'][:16], t['kind'], t['wchan'][:26]))
    if frames_info:
        print()
        print('windows counted for frames_s:')
        for f in frames_info:
            print('  pid %-7s %8.2f frames/s  %s' % (f['pid'], f['frames_s'], (f['caption'] or '')[:60]))
    print()
    o = overhead
    print('sampler overhead: %.3f %% of one core, %.1f wakeups/s%s; per sample %.2f ms mean / %.2f max (smaps_rollup '
          '%.2f ms of it)' % (
              o['cpu_core_pct'], o['ctx_switches_s'],
              (', nvidia-smi pmon %.3f %%' % o['nvidia_smi_cpu_core_pct']) if o['nvidia_smi_cpu_core_pct'] is not None
              else '',
              o['loop_ms']['mean'] if o['loop_ms'] else 0, o['loop_ms']['max'] if o['loop_ms'] else 0,
              o['smaps_rollup_ms']['mean'] if o['smaps_rollup_ms'] else 0))
    print('wrote %s/{samples.csv,threads.csv,summary.json}' % out)


def _num(v):
    if v is None:
        return '-'
    if abs(v) >= 100:
        return '%.1f' % v
    if abs(v) >= 1:
        return '%.2f' % v
    return '%.3f' % v


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))

"""Cheap /proc readers and process-tree discovery for the Linux measurement tools.

Costs measured on the dev box (Python 3.14, kernel 7.3): one /proc/<pid>/task/<tid>/status read ~3 us, schedstat
~1.3 us, stat ~1.7 us, a scandir of /proc ~0.2 ms for 520 processes. smaps_rollup walks every mapping of the
process under its mmap read lock: ~0.1-4 ms per process (Firefox's 3.3 ms is the worst seen), so measure.py can
read it less often than the rest (--mem-interval).
"""
import os
import re
import time

CLK_TCK = os.sysconf('SC_CLK_TCK')
_O_RDONLY = os.O_RDONLY | getattr(os, 'O_CLOEXEC', 0)


def read(path, size=65536):
    """Whole small file as bytes (one read: /proc files are generated per read call). Raises OSError."""
    fd = os.open(path, _O_RDONLY)
    try:
        out = os.read(fd, size)
        if len(out) < size:
            return out
        chunks = [out]
        while True:
            more = os.read(fd, size)
            if not more:
                return b''.join(chunks)
            chunks.append(more)
    finally:
        os.close(fd)


def boot_ticks():
    """Now, in the clock /proc/<pid>/stat starttime uses (CLOCK_BOOTTIME, in clock ticks)."""
    return time.clock_gettime(time.CLOCK_BOOTTIME) * CLK_TCK


class Stat:
    __slots__ = ('pid', 'comm', 'state', 'ppid', 'pgrp', 'utime', 'stime', 'threads', 'start')

    def __init__(self, pid, data):
        lp = data.find(b'(')
        rp = data.rfind(b')')
        f = data[rp + 2:].split()
        self.pid = pid
        self.comm = data[lp + 1:rp].decode('utf-8', 'replace')
        self.state = f[0].decode()
        self.ppid = int(f[1])
        self.pgrp = int(f[2])
        self.utime = int(f[11])
        self.stime = int(f[12])
        self.threads = int(f[17])
        self.start = int(f[19])

    @property
    def cpu_ticks(self):
        return self.utime + self.stime


def stat(pid):
    try:
        return Stat(pid, read('/proc/%d/stat' % pid))
    except (OSError, ValueError, IndexError):
        return None


def cmdline(pid):
    try:
        raw = read('/proc/%d/cmdline' % pid)
    except OSError:
        return ''
    return raw.rstrip(b'\0').replace(b'\0', b' ').decode('utf-8', 'replace')


def exe(pid):
    try:
        return os.readlink('/proc/%d/exe' % pid)
    except OSError:
        return ''


_TYPE = re.compile(r'--type=(\S+)')
_SUBTYPE = re.compile(r'--utility-sub-type=(?:[\w.]+\.)?(\w+)')


def label(comm, cmd):
    """Chromium/Electron helpers by --type (renderer, gpu-process, utility:NetworkService, zygote), else comm."""
    m = _TYPE.search(cmd)
    if not m:
        return comm
    t = m.group(1)
    s = _SUBTYPE.search(cmd)
    return '%s:%s' % (t, s.group(1)) if s else t


_ROLLUP_KEYS = {b'Rss': 'rss', b'Pss': 'pss', b'Private_Clean': 'private_clean', b'Private_Dirty': 'private_dirty',
                b'Anonymous': 'anon', b'Swap': 'swap', b'Shared_Clean': 'shared_clean',
                b'Shared_Dirty': 'shared_dirty'}


def smaps_rollup(pid):
    """kB values from /proc/<pid>/smaps_rollup (takes the target's mmap read lock while it walks the VMAs)."""
    data = read('/proc/%d/smaps_rollup' % pid)
    out = {}
    for line in data.split(b'\n')[1:]:
        k, _, v = line.partition(b':')
        key = _ROLLUP_KEYS.get(k)
        if key:
            out[key] = int(v.split()[0])
    return out


def status_rss(pid):
    """(VmRSS, RssShmem) kB from /proc/<pid>/status: what ps/top call RSS. It also counts shmem pages mapped through
    special mappings that the smaps walk skips (Wayland shm buffers, GPU driver mappings): RssShmem is that share."""
    data = read('/proc/%d/status' % pid)
    rss = shmem = 0
    i = data.find(b'\nVmRSS:')
    if i >= 0:
        rss = int(data[i + 7:data.find(b'\n', i + 1)].split()[0])
    j = data.find(b'\nRssShmem:')
    if j >= 0:
        shmem = int(data[j + 10:data.find(b'\n', j + 1)].split()[0])
    return rss, shmem


def status_mem(pid):
    """Fallback when smaps_rollup is not readable (non-dumpable process): kB from /proc/<pid>/status."""
    data = read('/proc/%d/status' % pid)
    out = {}
    for line in data.split(b'\n'):
        k, _, v = line.partition(b':')
        if k in (b'VmRSS', b'RssAnon', b'VmSwap'):
            out[k.decode()] = int(v.split()[0])
    return {'rss': out.get('VmRSS', 0), 'anon': out.get('RssAnon', 0), 'swap': out.get('VmSwap', 0),
            'private_dirty': out.get('RssAnon', 0), 'private_clean': 0, 'pss': out.get('VmRSS', 0)}


def fd_count(pid):
    try:
        return len(os.listdir('/proc/%d/fd' % pid))
    except OSError:
        return None


_VOL = b'\nvoluntary_ctxt_switches:\t'
_INVOL = b'\nnonvoluntary_ctxt_switches:\t'


def thread_sample(pid, tid):
    """(name, voluntary, involuntary, cpu_ns, runs) for one thread, or None if it is gone.
    status gives the name and the context-switch counters; schedstat the CPU time in ns (sum_exec_runtime) and how
    many times the thread was put on a CPU."""
    base = '/proc/%d/task/%d/' % (pid, tid)
    try:
        st = read(base + 'status')
        ss = read(base + 'schedstat')
    except OSError:
        return None
    nl = st.find(b'\n')
    name = st[6:nl].decode('utf-8', 'replace')  # "Name:\t"
    i = st.find(_VOL)
    j = st.find(_INVOL)
    vol = int(st[i + len(_VOL):st.find(b'\n', i + 1)]) if i >= 0 else 0
    invol = int(st[j + len(_INVOL):st.find(b'\n', j + 1)]) if j >= 0 else 0
    f = ss.split()
    return name, vol, invol, int(f[0]), int(f[2])


def tids(pid):
    try:
        return [int(t) for t in os.listdir('/proc/%d/task' % pid)]
    except OSError:
        return []


def wchan(pid, tid):
    try:
        return read('/proc/%d/task/%d/wchan' % (pid, tid)).decode('utf-8', 'replace').strip()
    except OSError:
        return ''


def ancestors(pid):
    out = []
    seen = set()
    while pid > 1 and pid not in seen:
        seen.add(pid)
        s = stat(pid)
        if s is None:
            break
        out.append(pid)
        pid = s.ppid
    return out


class Tree:
    """The measured process set: the root pid(s) and every descendant (ppid chains), plus processes whose cmdline
    matches `match` / `also_match` and their descendants. Membership is sticky: a process seen as a member stays one
    after it is reparented (crashpad-style double forks), until it exits. Rediscovered on every scan()."""

    def __init__(self, roots=(), match=None, also_match=None, exclude_self=True):
        self.roots = {}
        for pid in roots:
            s = stat(pid)
            if s is None:
                raise ValueError('no process %d' % pid)
            self.roots[pid] = s.start
        self.match = re.compile(match) if match else None
        self.also_match = re.compile(also_match) if also_match else None
        self.excluded = set()
        # Regex matches skip processes in our own process group: the other members of a pipeline the tool runs in
        # (`measure.sh --match foo | grep foo`: that grep's command line matches too). Explicit roots are kept.
        self.own_pgrp = os.getpgrp() if exclude_self else None
        if exclude_self:
            # Never measure ourselves or the shell(s) that started us (their command line may match --match). Helper
            # processes of the tool (nvidia-smi pmon) are excluded with exclude(); an app startup.sh launched is our
            # child and must stay in.
            self.excluded.update(ancestors(os.getpid()))
        self.cache = {}       # pid -> (start, ppid, matched)
        self.members = {}     # pid -> start
        self.scan_s = 0.0

    def exclude(self, pid):
        self.excluded.add(pid)

    def _matched(self, pid, st=None):
        if not (self.match or self.also_match):
            return False
        if self.own_pgrp is not None:
            st = st or stat(pid)
            if st is None or st.pgrp == self.own_pgrp:
                return False
        c = cmdline(pid)
        return bool((self.match and self.match.search(c)) or (self.also_match and self.also_match.search(c)))

    def scan(self):
        t0 = time.perf_counter()
        alive = set()
        for e in os.scandir('/proc'):
            n = e.name
            if not n.isdigit():
                continue
            pid = int(n)
            alive.add(pid)
            if pid not in self.cache:
                s = stat(pid)
                if s is None:
                    alive.discard(pid)
                    continue
                self.cache[pid] = (s.start, s.ppid, self._matched(pid, s))
        for pid in list(self.cache):
            if pid not in alive:
                del self.cache[pid]
        children = {}
        for pid, (_, ppid, _) in self.cache.items():
            children.setdefault(ppid, []).append(pid)
        seeds = [p for p, st in self.roots.items() if p in self.cache and self.cache[p][0] == st]
        seeds += [p for p, st in self.members.items() if p in self.cache and self.cache[p][0] == st]
        seeds += [p for p, (_, _, m) in self.cache.items() if m]
        members = {}
        stack = [p for p in seeds if p not in self.excluded]
        while stack:
            p = stack.pop()
            if p in members:
                continue
            members[p] = self.cache[p][0]
            for c in children.get(p, ()):
                if c not in members and c not in self.excluded:
                    stack.append(c)
        self.members = members
        self.scan_s += time.perf_counter() - t0
        return members

    def contains(self, pid):
        """Live membership check (used by the KWin bridge's isMember): walks the ppid chain from pid."""
        if pid in self.members:
            return True
        seen = 0
        p = pid
        while p > 1 and seen < 64:
            seen += 1
            if p in self.roots or p in self.members:
                return True
            s = stat(p)
            if s is None:
                return False
            if (self.match or self.also_match) and self._matched(p, s):
                return True
            p = s.ppid
        return False

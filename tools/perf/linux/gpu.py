"""Per-process GPU use for the Linux tools.

NVIDIA (proprietary driver): one long-lived `nvidia-smi pmon -d <s> -s um` stream read on a thread. Per process and
sample period it gives sm % (3D + compute), mem % (memory-controller busy), enc %, dec %, jpg %, ofa % and fb MB
(framebuffer memory this process allocated). '-' means no sample (reported as 0). The proprietary driver exposes no
drm-* keys in fdinfo, so this is the only per-process route there.

AMD (amdgpu), Intel (i915 / xe) and other DRM drivers: the generic fdinfo route. Every DRM client fd carries
drm-client-id, drm-pdev, drm-engine-<engine>: <ns> ns (or drm-cycles-<e> + drm-total-cycles-<e> on xe),
drm-engine-capacity-<e> and drm-resident-<region> / drm-memory-<region> / drm-total-<region> in KiB
(Documentation/gpu/drm-usage-stats.rst). One client can be open on several fds (and shared across processes), so
clients are counted once by (pdev, client id). `intel_gpu_top -J` and `amdgpu_top --json` read the same counters.
"""
import os
import subprocess
import threading
import time

from procfs import read


class Nvml:
    """NVIDIA through NVML in-process (ctypes, libnvidia-ml.so.1): the calls `nvidia-smi pmon` makes, without its cost
    (measured on the dev box: 2.5-3.5 ms a call, ~0.3 % of one core at 1 Hz, against 2-3 % for a long-lived pmon -d 1).

    Per call: nvmlDeviceGetProcessUtilization (one sample per process that had non-zero utilisation in the driver's
    last sample period: sm / mem / enc / dec %, the numbers pmon prints) and nvmlDeviceGet{Graphics,Compute}
    RunningProcesses_v3 (usedGpuMemory: pmon's fb MB)."""
    FIELDS = ('sm', 'mem', 'enc', 'dec', 'jpg', 'ofa', 'fb')

    def __init__(self, interval_s=1):
        import ctypes
        self.ct = ctypes
        self.lib = ctypes.CDLL('libnvidia-ml.so.1')
        if self.lib.nvmlInit_v2() != 0:
            raise RuntimeError('nvmlInit failed')

        class Util(ctypes.Structure):
            _fields_ = [('pid', ctypes.c_uint), ('timeStamp', ctypes.c_ulonglong), ('smUtil', ctypes.c_uint),
                        ('memUtil', ctypes.c_uint), ('encUtil', ctypes.c_uint), ('decUtil', ctypes.c_uint)]

        class PInfo(ctypes.Structure):
            _fields_ = [('pid', ctypes.c_uint), ('usedGpuMemory', ctypes.c_ulonglong),
                        ('gpuInstanceId', ctypes.c_uint), ('computeInstanceId', ctypes.c_uint)]
        self.Util, self.PInfo = Util, PInfo
        n = ctypes.c_uint(0)
        self.lib.nvmlDeviceGetCount_v2(ctypes.byref(n))
        self.devices = []
        for i in range(n.value):
            h = ctypes.c_void_p()
            if self.lib.nvmlDeviceGetHandleByIndex_v2(i, ctypes.byref(h)) == 0:
                self.devices.append(h)
        self.last_ts = {i: 0 for i in range(len(self.devices))}
        self.interval = interval_s
        self.cost_s = 0.0

    @staticmethod
    def available():
        try:
            import ctypes
            lib = ctypes.CDLL('libnvidia-ml.so.1')
            ok = lib.nvmlInit_v2() == 0
            if ok:
                lib.nvmlShutdown()
            return ok
        except OSError:
            return False

    def _query(self):
        ct = self.ct
        out = {}
        for i, dev in enumerate(self.devices):
            arr = (self.Util * 256)()
            n = ct.c_uint(256)
            # Samples newer than the previous call's newest: the driver's last period(s) since we asked.
            r = self.lib.nvmlDeviceGetProcessUtilization(dev, arr, ct.byref(n), ct.c_ulonglong(self.last_ts[i]))
            if r == 0:
                newest = self.last_ts[i]
                for k in range(n.value):
                    u = arr[k]
                    v = out.setdefault(u.pid, dict.fromkeys(self.FIELDS, 0.0))
                    # Several samples for one pid since last call: keep the highest (pmon shows one per period).
                    v['sm'] = max(v['sm'], u.smUtil)
                    v['mem'] = max(v['mem'], u.memUtil)
                    v['enc'] = max(v['enc'], u.encUtil)
                    v['dec'] = max(v['dec'], u.decUtil)
                    newest = max(newest, u.timeStamp)
                self.last_ts[i] = newest
            for fn in ('nvmlDeviceGetGraphicsRunningProcesses_v3', 'nvmlDeviceGetComputeRunningProcesses_v3'):
                cnt = ct.c_uint(512)
                infos = (self.PInfo * 512)()
                if getattr(self.lib, fn)(dev, ct.byref(cnt), infos) != 0:
                    continue
                for k in range(cnt.value):
                    p = infos[k]
                    mem = p.usedGpuMemory
                    if mem >= (1 << 63):  # NVML_VALUE_NOT_AVAILABLE
                        continue
                    v = out.setdefault(p.pid, dict.fromkeys(self.FIELDS, 0.0))
                    v['fb'] = max(v['fb'], mem / (1024.0 * 1024.0))
        return out

    def sum(self, pids):
        t0 = time.perf_counter()
        block = self._query()
        self.cost_s += time.perf_counter() - t0
        self._last_block = block
        out = dict.fromkeys(self.FIELDS, 0.0)
        for p in pids:
            v = block.get(p)
            if v:
                for k in self.FIELDS:
                    out[k] += v[k]
        return out, time.monotonic()

    def sum_last(self, pids):
        """Sum over pids from the block the previous sum() fetched (no new NVML call)."""
        out = dict.fromkeys(self.FIELDS, 0.0)
        for p in pids:
            v = getattr(self, '_last_block', {}).get(p)
            if v:
                for k in self.FIELDS:
                    out[k] += v[k]
        return out

    def latest(self):
        return {}, time.monotonic()

    def cpu_ticks(self):
        return None

    def stop(self):
        try:
            self.lib.nvmlShutdown()
        except Exception:
            pass


class NvidiaPmon:
    FIELDS = ('sm', 'mem', 'enc', 'dec', 'jpg', 'ofa', 'fb')

    def __init__(self, interval_s=1):
        self.interval = max(1, min(10, int(round(interval_s))))
        self.proc = subprocess.Popen(['nvidia-smi', 'pmon', '-d', str(self.interval), '-s', 'um'],
                                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self._cols = None
        self._cur = {}
        self._cur_t = None
        self._last_line = 0.0
        self._latest = ({}, None)
        self._lock = threading.Lock()
        self.blocks = 0
        self.thread = threading.Thread(target=self._read, name='nvidia-pmon', daemon=True)
        self.thread.start()

    @staticmethod
    def available():
        try:
            return subprocess.run(['nvidia-smi', '-L'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                  timeout=10).returncode == 0
        except (OSError, subprocess.SubprocessError):
            return False

    def _publish(self):
        if self._cur:
            with self._lock:
                self._latest = (self._cur, self._cur_t)
                self.blocks += 1
            self._cur = {}

    def _read(self):
        for line in self.proc.stdout:
            now = time.monotonic()
            if line.startswith('#'):
                if self._cols is None:
                    self._cols = line[1:].split()
                continue
            if not self._cols:
                continue
            # Lines of one sample arrive in a burst; a gap means the previous sample is complete.
            if now - self._last_line > 0.25:
                self._publish()
                self._cur_t = now
            self._last_line = now
            f = line.split()
            if len(f) < len(self._cols) - 1:
                continue
            row = dict(zip(self._cols, f))
            pid = row.get('pid', '-')
            if not pid.isdigit():
                continue
            vals = {}
            for k in self.FIELDS:
                v = row.get(k, '-')
                try:
                    vals[k] = float(v)
                except ValueError:
                    vals[k] = 0.0
            vals['cmd'] = row.get('command', '')
            prev = self._cur.get(int(pid))
            if prev:  # several GPUs: sum
                for k in self.FIELDS:
                    prev[k] += vals[k]
            else:
                self._cur[int(pid)] = vals

    def latest(self):
        """({pid: {sm, mem, enc, dec, jpg, ofa, fb}}, monotonic time the sample arrived)."""
        if self._cur and time.monotonic() - self._last_line > 0.25:
            self._publish()
        with self._lock:
            return self._latest

    def sum(self, pids):
        block, t = self.latest()
        out = {k: 0.0 for k in self.FIELDS}
        for p in pids:
            v = block.get(p)
            if v:
                for k in self.FIELDS:
                    out[k] += v[k]
        return out, t

    def sum_last(self, pids):
        return self.sum(pids)[0]

    def cpu_ticks(self):
        try:
            data = read('/proc/%d/stat' % self.proc.pid)
            f = data[data.rfind(b')') + 2:].split()
            return int(f[11]) + int(f[12])
        except (OSError, ValueError, IndexError):
            return None

    def stop(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()


def parse_fdinfo(text):
    """drm-* keys of one fdinfo file -> dict (numbers as int, units dropped). Empty if not a DRM client fd."""
    out = {}
    for line in text.splitlines():
        if not line.startswith('drm-'):
            continue
        k, _, v = line.partition(':')
        v = v.strip()
        parts = v.split()
        if k in ('drm-driver', 'drm-pdev', 'drm-client-name'):
            out[k] = v
            continue
        if not parts:
            continue
        try:
            n = int(parts[0])
        except ValueError:
            out[k] = v
            continue
        unit = parts[1] if len(parts) > 1 else ''
        if unit == 'KiB':
            n *= 1024
        elif unit == 'MiB':
            n *= 1024 * 1024
        elif unit == 'GiB':
            n *= 1024 ** 3
        out[k] = n
    return out


class DrmFdinfo:
    """Engine utilisation (% of the engine, summed over the tree's DRM clients) and memory from fdinfo."""

    def __init__(self):
        self._prev = {}   # (pdev, client) -> (mono_ns, {engine: busy_ns or (cycles, total)})
        self.engines = set()
        self.regions = set()
        self.cost_s = 0.0

    def sample(self, pids):
        t0 = time.perf_counter()
        now = time.monotonic_ns()
        clients = {}
        for pid in pids:
            try:
                fds = os.listdir('/proc/%d/fd' % pid)
            except OSError:
                continue
            for fd in fds:
                try:
                    if not os.readlink('/proc/%d/fd/%s' % (pid, fd)).startswith('/dev/dri/'):
                        continue
                    info = parse_fdinfo(read('/proc/%d/fdinfo/%s' % (pid, fd)).decode('utf-8', 'replace'))
                except OSError:
                    continue
                cid = info.get('drm-client-id')
                if cid is None:
                    continue
                clients[(info.get('drm-pdev', ''), cid)] = info
        util = {}
        mem = {}
        cur = {}
        for key, info in clients.items():
            eng = {}
            resident, legacy = {}, {}
            for k, v in info.items():
                if k.startswith('drm-engine-capacity-') or not isinstance(v, int):
                    continue
                if k.startswith('drm-engine-'):
                    eng[k[len('drm-engine-'):]] = v
                elif k.startswith('drm-cycles-'):
                    e = k[len('drm-cycles-'):]
                    eng[e] = (v, info.get('drm-total-cycles-' + e, 0))
                elif k.startswith('drm-resident-'):
                    resident[k[len('drm-resident-'):]] = v
                elif k.startswith('drm-memory-'):   # older amdgpu name for resident memory
                    legacy[k[len('drm-memory-'):]] = v
            for region, v in dict(legacy, **resident).items():
                mem[region] = mem.get(region, 0) + v
                self.regions.add(region)
            cur[key] = (now, eng)
            prev = self._prev.get(key)
            if not prev:
                continue
            dt = now - prev[0]
            for e, v in eng.items():
                p = prev[1].get(e)
                if p is None:
                    continue
                cap = info.get('drm-engine-capacity-' + e, 1) or 1
                if isinstance(v, tuple):
                    dc, dtot = v[0] - p[0], v[1] - p[1]
                    pct = 100.0 * dc / dtot / cap if dtot > 0 else 0.0
                else:
                    pct = 100.0 * (v - p) / dt / cap if dt > 0 else 0.0
                util[e] = util.get(e, 0.0) + max(0.0, pct)
                self.engines.add(e)
        self._prev = cur
        self.cost_s += time.perf_counter() - t0
        return util, {r: b / (1024 * 1024) for r, b in mem.items()}

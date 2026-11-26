"""Shared helpers: statistics, output paths, machine facts."""
import json
import os
import platform
import re
import socket
import subprocess
import time

HERE = os.path.dirname(os.path.abspath(__file__))
PERF_DIR = os.path.dirname(HERE)
RESULTS = os.path.join(PERF_DIR, 'results')


def stat(values):
    """mean / p50 / p95 / max like measure.ps1 (nearest-rank on the sorted samples, floor((n-1)q))."""
    a = sorted(v for v in values if v is not None)
    if not a:
        return None
    n = len(a)
    return {'mean': round(sum(a) / n, 3), 'p50': round(a[int((n - 1) * 0.5)], 3),
            'p95': round(a[int((n - 1) * 0.95)], 3), 'max': round(a[-1], 3)}


def median(values):
    a = sorted(v for v in values if v is not None)
    return a[(len(a) - 1) // 2] if a else None


def slug(s):
    s = re.sub(r'[^A-Za-z0-9._-]+', '-', s).strip('-')
    return s[:60] or 'run'


def out_dir(out, default_name):
    path = out or os.path.join(RESULTS, '%s-%s' % (slug(default_name), time.strftime('%Y%m%d-%H%M%S')))
    os.makedirs(path, exist_ok=True)
    return os.path.abspath(path)


def iso(t=None):
    import datetime
    t = time.time() if t is None else t
    return datetime.datetime.fromtimestamp(t).astimezone().isoformat(timespec='milliseconds')


def write_json(path, obj):
    with open(path, 'w') as f:
        json.dump(obj, f, indent=2)
        f.write('\n')


def _run(cmd, timeout=10):
    try:
        r = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=timeout)
        return r.stdout if r.returncode == 0 else ''
    except (OSError, subprocess.SubprocessError):
        return ''


def displays():
    """Enabled outputs with mode and refresh (kscreen-doctor -j on KDE)."""
    out = []
    raw = _run(['kscreen-doctor', '-j'])
    if raw:
        try:
            d = json.loads(raw)
            for o in d.get('outputs', []):
                if not o.get('enabled'):
                    continue
                m = next((m for m in o.get('modes', []) if m.get('id') == o.get('currentModeId')), {})
                size = m.get('size', {})
                out.append({'name': o.get('name'), 'mode': '%sx%s' % (size.get('width'), size.get('height')),
                            'refresh_hz': round(m.get('refreshRate', 0), 2), 'scale': o.get('scale'),
                            'priority': o.get('priority'), 'pos': o.get('pos')})
        except (ValueError, KeyError, TypeError):
            pass
    return out


def machine_facts():
    cpu = ''
    try:
        with open('/proc/cpuinfo') as f:
            for line in f:
                if line.startswith('model name'):
                    cpu = line.split(':', 1)[1].strip()
                    break
    except OSError:
        pass
    mem_kb = 0
    try:
        with open('/proc/meminfo') as f:
            for line in f:
                if line.startswith('MemTotal:'):
                    mem_kb = int(line.split()[1])
                    break
    except OSError:
        pass
    os_name = ''
    try:
        with open('/etc/os-release') as f:
            for line in f:
                if line.startswith('PRETTY_NAME='):
                    os_name = line.split('=', 1)[1].strip().strip('"')
    except OSError:
        pass
    gpus = []
    for line in _run(['nvidia-smi', '--query-gpu=name,driver_version,memory.total',
                      '--format=csv,noheader']).splitlines():
        p = [x.strip() for x in line.split(',')]
        if len(p) >= 3:
            gpus.append({'name': p[0], 'driver': p[1], 'memory': p[2]})
    if not gpus:
        for card in sorted(os.listdir('/sys/class/drm')) if os.path.isdir('/sys/class/drm') else []:
            if re.fullmatch(r'card\d+', card):
                drv = os.path.basename(os.path.realpath('/sys/class/drm/%s/device/driver' % card))
                gpus.append({'name': card, 'driver': drv})
    kwin = _run(['kwin_wayland', '--version']).strip()
    return {
        'machine': socket.gethostname(), 'os': os_name, 'kernel': platform.release(), 'cpu': cpu,
        'logical_cores': os.cpu_count(), 'memory_gb': round(mem_kb / 1048576, 1), 'gpus': gpus,
        'displays': displays(),
        'session': {'type': os.environ.get('XDG_SESSION_TYPE', ''),
                    'desktop': os.environ.get('XDG_CURRENT_DESKTOP', ''),
                    'wayland_display': os.environ.get('WAYLAND_DISPLAY', ''),
                    'display': os.environ.get('DISPLAY', ''), 'compositor': kwin},
        'clk_tck': os.sysconf('SC_CLK_TCK'),
    }

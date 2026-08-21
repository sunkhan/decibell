#!/usr/bin/env python3
"""Find clicks/splices in a DECIBELL_AUDIO_DUMP directory.

    DECIBELL_AUDIO_DUMP=/tmp/dump npm run dev     # run a call, hang up
    python3 native/scripts/analyze_audio_dump.py /tmp/dump

For every WAV it lists sample-to-sample jumps that are far larger than the
local slope (a step discontinuity = a click), plus, for output.wav, runs of
exact digital zero that cut into audio (zeros spliced by a short ring or an
xrun). Each finding is cross-referenced with the nearest events.log line.

A click present in output.wav but NOT in any peer-*.wav is a ring/device
problem. A click present in peer-<name>.wav came out of the decoder — a
sender/codec/gate artifact (or PLC). Needs numpy.
"""
import sys, os, bisect
import numpy as np

WIN_MS = 20          # local-slope window
RATIO = 8.0          # jump must exceed RATIO × local slope RMS
FLOOR = 0.0015       # ... and this absolute size (≈ -56 dBFS)
ZERO_RUN_MS = 1.0    # shortest zero run reported as a splice
AUDIO_LSB = 64       # |x| above this counts as "audio" next to a zero run


def load(path):
    """Read a 16-bit mono PCM WAV, tolerating an unpatched (streaming) header
    whose size fields are still 0 — the dump is finalised only when the
    voice engine shuts down cleanly."""
    raw = open(path, "rb").read()
    if len(raw) < 44 or raw[:4] != b"RIFF" or raw[8:12] != b"WAVE":
        raise ValueError(f"{path}: not a WAV file")
    rate = int.from_bytes(raw[24:28], "little")
    body = raw[44:]
    body = body[: len(body) - (len(body) % 2)]
    data = np.frombuffer(body, dtype="<i2").astype(np.float32) / 32768.0
    return rate, data


def load_events(path):
    ev = []
    if os.path.exists(path):
        for line in open(path):
            line = line.rstrip("\n")
            try:
                t = float(line.split()[0])
            except (ValueError, IndexError):
                continue
            ev.append((t, line.strip()))
    ev.sort()
    return ev


def nearest_events(ev, t_ms, k=2, window_ms=250):
    if not ev:
        return []
    ts = [e[0] for e in ev]
    i = bisect.bisect_left(ts, t_ms)
    cands = ev[max(0, i - k):i + k]
    return [e[1] for e in cands if abs(e[0] - t_ms) <= window_ms]


def find_clicks(x, rate):
    d = np.diff(x)
    win = max(8, int(rate * WIN_MS / 1000))
    # local RMS of the first difference, excluding the sample itself (approx.)
    sq = d * d
    kernel = np.ones(win) / win
    local = np.sqrt(np.convolve(sq, kernel, mode="same") + 1e-12)
    mag = np.abs(d)
    idx = np.where((mag > RATIO * local) & (mag > FLOOR))[0]
    # merge neighbours within 5 ms
    out = []
    last = -10 ** 9
    for i in idx:
        if i - last > rate * 0.005:
            out.append(i)
        last = i
    return [(i / rate * 1000.0, 20 * np.log10(mag[i] + 1e-9), 20 * np.log10(local[i] + 1e-9)) for i in out]


def find_zero_splices(x, rate):
    z = (x == 0.0)
    edges = np.diff(z.astype(np.int8))
    starts = np.where(edges == 1)[0] + 1
    ends = np.where(edges == -1)[0] + 1
    if z[0]:
        starts = np.insert(starts, 0, 0)
    if z[-1]:
        ends = np.append(ends, len(x))
    out = []
    min_len = int(rate * ZERO_RUN_MS / 1000)
    thr = AUDIO_LSB / 32768.0
    for s, e in zip(starts, ends):
        if e - s < min_len:
            continue
        before = abs(x[s - 1]) if s > 0 else 0.0
        after = abs(x[e]) if e < len(x) else 0.0
        if before > thr or after > thr:
            out.append((s / rate * 1000.0, (e - s) / rate * 1000.0, before, after))
    return out


def main():
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    d = sys.argv[1]
    ev = load_events(os.path.join(d, "events.log"))
    for name in sorted(os.listdir(d)):
        if not name.endswith(".wav"):
            continue
        rate, x = load(os.path.join(d, name))
        dur = len(x) / rate
        print(f"\n=== {name}: {dur:.1f}s @ {rate} Hz, peak {20*np.log10(np.max(np.abs(x))+1e-9):.1f} dBFS")
        clicks = find_clicks(x, rate)
        print(f"  step discontinuities (> {RATIO}x local slope): {len(clicks)}")
        for t, mag, loc in clicks[:40]:
            evs = nearest_events(ev, t)
            print(f"    t={t/1000:8.3f}s  jump {mag:6.1f} dBFS  (local slope {loc:6.1f})  {' | '.join(evs)}")
        if len(clicks) > 40:
            print(f"    ... {len(clicks)-40} more")
        if name.startswith("output"):
            zs = find_zero_splices(x, rate)
            print(f"  zero runs cutting into audio (>= {ZERO_RUN_MS} ms): {len(zs)}")
            for t, ln, b, a in zs[:40]:
                evs = nearest_events(ev, t)
                print(f"    t={t/1000:8.3f}s  {ln:6.1f} ms of zeros  before {20*np.log10(b+1e-9):6.1f} / after {20*np.log10(a+1e-9):6.1f} dBFS  {' | '.join(evs)}")
    if ev:
        kinds = {}
        for _, line in ev:
            key = " ".join(line.split()[1:3])
            kinds[key] = kinds.get(key, 0) + 1
        print("\n=== events.log summary")
        for k, v in sorted(kinds.items(), key=lambda kv: -kv[1]):
            print(f"  {v:5d}  {k}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""media_summary.py: the media-play scenarios' numbers (scenarios.sh, sc_media / media_table).

  media_summary.py run DIR             DIR/media.json from one run's measure.sh windows (before, focused, unfocused,
                                       after) and the client's own counters at each window's ends; one line per window
  media_summary.py table OUT DIR...    a Markdown table of several runs (every client), one row per window

Per window: CPU of the process tree (% of one core), wakeups / s (ours, the driver's), the top threads by wakeups,
private memory, GPU decoder utilisation (the whole GPU's, sampled by nvidia-smi: other programs' decode included) and
the per-process figure when measure.sh has one, and from the client's counters pictures shown per second against the
file's rate, pictures late or dropped, and the frames the window drew and presented.
"""
import json, os, sys


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def last_json_line(path):
    """The client's hooks print one JSON object per line; the last one counts."""
    try:
        lines = [l for l in open(path).read().splitlines() if l.strip().startswith("{")]
        return json.loads(lines[-1]) if lines else None
    except (OSError, ValueError):
        return None


def gpu_csv(path):
    dec, gpu = [], []
    try:
        for line in open(path):
            parts = [p.strip() for p in line.split(",")]
            if len(parts) >= 2 and parts[0].isdigit() and parts[1].isdigit():
                dec.append(int(parts[0])); gpu.append(int(parts[1]))
    except OSError:
        pass
    mean = lambda xs: round(sum(xs) / len(xs), 1) if xs else None
    return {"decoder_pct_mean": mean(dec), "decoder_pct_max": max(dec) if dec else None, "gpu_pct_mean": mean(gpu), "samples": len(dec)}


def diff(a, b, key):
    if not a or not b or a.get(key) is None or b.get(key) is None:
        return None
    return b[key] - a[key]


def window(d, name):
    s = load(os.path.join(d, name, "summary.json"))
    if not s:
        return None
    w, summ = s.get("window", {}), s.get("summary", {})
    out = {
        "seconds": w.get("seconds"),
        "cpu_core_pct": w.get("cpu_core_pct"),
        "own_wakeups_s": w.get("own_wakeups_s"),
        "driver_wakeups_s": w.get("driver_wakeups_s"),
        "unnamed_wakeups_s": w.get("unnamed_wakeups_s"),
        "ctx_switches_s": w.get("ctx_switches_s"),
        "compositor_frames_s": w.get("frames_s"),
        "private_mb": (summ.get("private_mb") or {}).get("p50"),
        "threads": (summ.get("threads") or {}).get("max"),
        "top_threads": [
            {"name": t.get("name"), "kind": t.get("kind"), "wakeups_s": t.get("vol_s"), "cpu_core_pct": t.get("cpu_core_pct")}
            for t in (s.get("top_threads") or [])[:8]
        ],
        "focused_fraction": (s.get("focus") or {}).get("focused_fraction"),
        "gpu_process": summ.get("gpu_dec_pct") or summ.get("gpu_decoder_pct"),
    }
    if name in ("focused", "unfocused"):
        out["gpu"] = gpu_csv(os.path.join(d, f"{name}-gpu.csv"))
        out["load"] = load(os.path.join(d, f"{name}-load.json"))
        a, b = last_json_line(os.path.join(d, f"{name}-start.json")), last_json_line(os.path.join(d, f"{name}-end.json"))
        if a and b:
            ms = diff(a, b, "position_ms")
            secs = (ms / 1000.0) if ms else out["seconds"]
            c = {"position_s": round(secs, 2) if secs else None, "file_fps": b.get("fps") or ((load(os.path.join(d, "clip.json")) or {}).get("fps")),
                 "decoder": b.get("decoder") or b.get("hub_decoder") or b.get("decoder_name"), "codec": b.get("codec"),
                 "size": f"{b.get('width')}x{b.get('height')}"}
            for key in ("shown", "late", "skipped", "published", "presented", "dropped", "replaced", "frames_drawn", "frames_presented", "errors", "fallbacks",
                        "total_frames", "dropped_frames", "presented_frames", "corrupted_frames"):
                v = diff(a, b, key)
                if v is not None:
                    c[key] = v
            # Chromium: getVideoPlaybackQuality (decoded, dropped) and requestVideoFrameCallback (presented).
            if "shown" not in c and "total_frames" in c:
                c["shown"] = c.get("presented_frames", c["total_frames"] - c.get("dropped_frames", 0))
                c["dropped"] = c.get("dropped_frames")
                c["late"] = None
            shown = c.get("shown")
            if shown is not None and secs:
                c["shown_fps"] = round(shown / secs, 2)
            out["client"] = c
    return out


def run(d):
    meta = load(os.path.join(d, "scenario.json")) or {}
    result = {"client": meta.get("client"), "scenario": meta.get("scenario"), "dir": os.path.basename(d), "clip": load(os.path.join(d, "clip.json")),
              "play": last_json_line(os.path.join(d, "play.json")), "windows": {}}
    for name in ("before", "focused", "unfocused", "after"):
        w = window(d, name)
        if w:
            result["windows"][name] = w
    with open(os.path.join(d, "media.json"), "w") as f:
        json.dump(result, f, indent=1)
    for name, w in result["windows"].items():
        c = w.get("client") or {}
        print(f"[media] {result['client']} {result['scenario']} {name}: cpu {w.get('cpu_core_pct')} % of a core, wakeups own {w.get('own_wakeups_s')} / driver "
              f"{w.get('driver_wakeups_s')} per s, private {w.get('private_mb')} MB"
              + (f", decoder {c.get('decoder')}, {c.get('shown_fps')} pictures/s of {c.get('file_fps')}, late {c.get('late')}, dropped {c.get('dropped')}, "
                 f"window frames drawn {c.get('frames_drawn')}, GPU decoder {(w.get('gpu') or {}).get('decoder_pct_mean')} %" if c else ""))
    return result


def cell(v, fmt="{}"):
    return "–" if v is None else fmt.format(v)


def table(out, dirs):
    rows = []
    for d in dirs:
        r = load(os.path.join(d, "media.json")) or run(d)
        for name in ("before", "focused", "unfocused", "after"):
            w = r["windows"].get(name)
            if not w:
                continue
            c, g, l = w.get("client") or {}, w.get("gpu") or {}, w.get("load") or {}
            top = ", ".join(f"{t['name']} {t['wakeups_s']:.0f}" for t in w.get("top_threads", [])[:4] if t.get("wakeups_s"))
            rows.append("| " + " | ".join([
                str(r.get("client")), str(r.get("scenario")), name, cell(c.get("decoder")),
                cell(w.get("cpu_core_pct"), "{:.1f}"), cell(w.get("own_wakeups_s"), "{:.0f}") + " / " + cell(w.get("driver_wakeups_s"), "{:.0f}"), top or "–",
                cell(g.get("decoder_pct_mean")), cell(w.get("private_mb"), "{:.1f}"),
                (cell(c.get("shown_fps")) + " of " + cell(c.get("file_fps"))) if c else "–",
                (cell(c.get("late")) + " / " + cell(c.get("dropped"))) if c else "–",
                (cell(c.get("frames_drawn")) + " / " + cell(c.get("frames_presented"))) if c else "–",
                cell(l.get("load1") if isinstance(l, dict) else None), r.get("dir", ""),
            ]) + " |")
    head = ("| Client | Scenario | Window | Decoder | CPU, % of a core | Wakeups / s, ours / driver | Top threads (wakeups / s) | GPU decoder %, whole GPU | "
            "Private MB | Pictures / s shown, of the file's | Late / dropped | Window frames drawn / presented | Load | Run |\n" + "|---|" * 1 + "---|" * 13)
    text = ("# Attachment playback: a clip played from its card\n\nFrom `tools/perf/scenarios.sh <client> media-play-h264 | media-play-vp9` (README, \"Attachment playback\"). "
            "Each run: 5 s before playing, the clip playing with the client's window focused and then unfocused (the clip sought back to its start before each), "
            "5 s after the player was dropped. Nothing is audible (the GPUI client renders into its null sink; Electron runs muted).\n\n" + head + "\n" + "\n".join(rows) + "\n")
    with open(out, "w") as f:
        f.write(text)
    print(text)


if __name__ == "__main__":
    if len(sys.argv) >= 3 and sys.argv[1] == "run":
        run(sys.argv[2])
    elif len(sys.argv) >= 4 and sys.argv[1] == "table":
        table(sys.argv[2], sys.argv[3:])
    else:
        sys.exit(__doc__)

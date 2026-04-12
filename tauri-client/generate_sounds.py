#!/usr/bin/env python3
"""Generate UI sound effects for Decibell client."""

import numpy as np
import wave
import struct
import os

SAMPLE_RATE = 48000
OUT_DIR = os.path.join(os.path.dirname(__file__), "public", "sounds")


def save_wav(filename: str, samples: np.ndarray):
    """Save float64 samples [-1, 1] as 16-bit WAV."""
    path = os.path.join(OUT_DIR, filename)
    samples = np.clip(samples, -1.0, 1.0)
    int_samples = (samples * 32767).astype(np.int16)
    with wave.open(path, "w") as wf:
        wf.setnchannels(1)
        wf.setsampwidth(2)
        wf.setframerate(SAMPLE_RATE)
        wf.writeframes(int_samples.tobytes())
    print(f"  {filename} ({len(int_samples) / SAMPLE_RATE * 1000:.0f}ms)")


def sine(freq: float, duration: float) -> np.ndarray:
    t = np.linspace(0, duration, int(SAMPLE_RATE * duration), endpoint=False)
    return np.sin(2 * np.pi * freq * t)


def triangle(freq: float, duration: float) -> np.ndarray:
    t = np.linspace(0, duration, int(SAMPLE_RATE * duration), endpoint=False)
    return 2 * np.abs(2 * (t * freq - np.floor(t * freq + 0.5))) - 1


def envelope(samples: np.ndarray, attack_ms: float = 5, decay_ms: float = 50,
             sustain: float = 0.6, release_ms: float = 80) -> np.ndarray:
    """ADSR envelope. Sustain portion is everything between attack+decay and release."""
    n = len(samples)
    attack = int(SAMPLE_RATE * attack_ms / 1000)
    decay = int(SAMPLE_RATE * decay_ms / 1000)
    release = int(SAMPLE_RATE * release_ms / 1000)
    sustain_len = max(0, n - attack - decay - release)

    env = np.concatenate([
        np.linspace(0, 1, attack),                    # attack
        np.linspace(1, sustain, decay),                # decay
        np.full(sustain_len, sustain),                 # sustain
        np.linspace(sustain, 0, release),              # release
    ])
    return samples[:len(env)] * env[:len(samples)]


def fade(samples: np.ndarray, fade_in_ms: float = 3, fade_out_ms: float = 30) -> np.ndarray:
    """Simple fade in/out to avoid clicks."""
    fi = int(SAMPLE_RATE * fade_in_ms / 1000)
    fo = int(SAMPLE_RATE * fade_out_ms / 1000)
    out = samples.copy()
    if fi > 0:
        out[:fi] *= np.linspace(0, 1, fi)
    if fo > 0:
        out[-fo:] *= np.linspace(1, 0, fo)
    return out


def concat(*parts, gap_ms: float = 0) -> np.ndarray:
    """Concatenate audio parts with optional silence gaps."""
    gap = np.zeros(int(SAMPLE_RATE * gap_ms / 1000))
    result = []
    for i, p in enumerate(parts):
        if i > 0 and len(gap) > 0:
            result.append(gap)
        result.append(p)
    return np.concatenate(result)


def mix(*parts) -> np.ndarray:
    """Mix audio parts (zero-padded to longest)."""
    max_len = max(len(p) for p in parts)
    out = np.zeros(max_len)
    for p in parts:
        out[:len(p)] += p
    return out


def lowpass(samples: np.ndarray, cutoff_hz: float) -> np.ndarray:
    """Simple single-pole lowpass filter."""
    rc = 1.0 / (2 * np.pi * cutoff_hz)
    dt = 1.0 / SAMPLE_RATE
    alpha = dt / (rc + dt)
    out = np.zeros_like(samples)
    out[0] = alpha * samples[0]
    for i in range(1, len(samples)):
        out[i] = out[i - 1] + alpha * (samples[i] - out[i - 1])
    return out


# ---------------------------------------------------------------------------
# Sound definitions
# ---------------------------------------------------------------------------

def gen_mute():
    """Mute: short low-mid click, feels like something closing."""
    tone = sine(320, 0.10) * 0.7 + sine(240, 0.10) * 0.3
    return envelope(tone, attack_ms=2, decay_ms=30, sustain=0.2, release_ms=50) * 0.7


def gen_unmute():
    """Unmute: short bright pop, feels like something opening."""
    tone = sine(660, 0.10) * 0.6 + sine(880, 0.10) * 0.25 + triangle(660, 0.10) * 0.15
    return envelope(tone, attack_ms=2, decay_ms=25, sustain=0.15, release_ms=45) * 0.7


def gen_deafen():
    """Deafen: deeper than mute, two descending tones."""
    t1 = envelope(sine(350, 0.08), attack_ms=2, decay_ms=20, sustain=0.3, release_ms=30)
    t2 = envelope(sine(220, 0.10), attack_ms=2, decay_ms=20, sustain=0.2, release_ms=50)
    return concat(t1, t2, gap_ms=15) * 0.7


def gen_undeafen():
    """Undeafen: brighter than unmute, two ascending tones."""
    t1 = envelope(sine(520, 0.08), attack_ms=2, decay_ms=20, sustain=0.3, release_ms=30)
    t2 = envelope(sine(780, 0.10), attack_ms=2, decay_ms=20, sustain=0.2, release_ms=50)
    return concat(t1, t2, gap_ms=15) * 0.7


def gen_user_join():
    """User join: warm ascending two-tone chime."""
    t1 = sine(523, 0.12) * 0.6 + triangle(523, 0.12) * 0.2  # C5
    t1 = envelope(t1, attack_ms=3, decay_ms=30, sustain=0.4, release_ms=60)
    t2 = sine(659, 0.15) * 0.6 + triangle(659, 0.15) * 0.2  # E5
    t2 = envelope(t2, attack_ms=3, decay_ms=30, sustain=0.3, release_ms=80)
    return concat(t1, t2, gap_ms=30) * 0.6


def gen_user_leave():
    """User leave: soft descending two-tone."""
    t1 = sine(659, 0.12) * 0.5 + triangle(659, 0.12) * 0.2  # E5
    t1 = envelope(t1, attack_ms=3, decay_ms=30, sustain=0.35, release_ms=60)
    t2 = sine(440, 0.15) * 0.5 + triangle(440, 0.15) * 0.2  # A4
    t2 = envelope(t2, attack_ms=3, decay_ms=30, sustain=0.25, release_ms=80)
    return concat(t1, t2, gap_ms=30) * 0.55


def gen_stream_start():
    """Stream start: three ascending tones, energetic."""
    freqs = [440, 554, 659]  # A4, C#5, E5 (A major triad)
    parts = []
    for i, f in enumerate(freqs):
        tone = sine(f, 0.10) * 0.55 + triangle(f, 0.10) * 0.2
        tone = envelope(tone, attack_ms=3, decay_ms=20, sustain=0.35, release_ms=40)
        parts.append(tone)
    result = parts[0]
    for p in parts[1:]:
        result = concat(result, p, gap_ms=25)
    return result * 0.65


def gen_stream_stop():
    """Stream stop: three descending tones, gentle."""
    freqs = [659, 554, 440]  # E5, C#5, A4
    parts = []
    for i, f in enumerate(freqs):
        tone = sine(f, 0.10) * 0.5 + triangle(f, 0.10) * 0.2
        vol = 0.6 - i * 0.1
        tone = envelope(tone, attack_ms=3, decay_ms=20, sustain=0.3, release_ms=50) * vol
        parts.append(tone)
    result = parts[0]
    for p in parts[1:]:
        result = concat(result, p, gap_ms=25)
    return result * 0.6


def gen_connect():
    """Connect to voice: satisfying confirmation sound."""
    # Rich chord: C5 + E5 + G5 layered with slight delays
    c = envelope(sine(523, 0.25), attack_ms=5, decay_ms=40, sustain=0.5, release_ms=120) * 0.35
    e = envelope(sine(659, 0.22), attack_ms=5, decay_ms=40, sustain=0.45, release_ms=100) * 0.3
    g = envelope(sine(784, 0.20), attack_ms=5, decay_ms=40, sustain=0.4, release_ms=80) * 0.25
    # Stagger the chord tones slightly
    pad_e = np.concatenate([np.zeros(int(SAMPLE_RATE * 0.02)), e])
    pad_g = np.concatenate([np.zeros(int(SAMPLE_RATE * 0.04)), g])
    return mix(c, pad_e, pad_g) * 0.7


def gen_disconnect():
    """Disconnect from voice: short falling tone."""
    duration = 0.18
    t = np.linspace(0, duration, int(SAMPLE_RATE * duration), endpoint=False)
    # Frequency sweeps down from 500 to 250 Hz
    freq = np.linspace(500, 250, len(t))
    phase = np.cumsum(2 * np.pi * freq / SAMPLE_RATE)
    tone = np.sin(phase) * 0.6
    return envelope(tone, attack_ms=3, decay_ms=30, sustain=0.4, release_ms=60) * 0.65


# ---------------------------------------------------------------------------
# Generate all sounds
# ---------------------------------------------------------------------------

if __name__ == "__main__":
    os.makedirs(OUT_DIR, exist_ok=True)
    print("Generating sounds...")

    sounds = {
        "mute.wav": gen_mute,
        "unmute.wav": gen_unmute,
        "deafen.wav": gen_deafen,
        "undeafen.wav": gen_undeafen,
        "user_join.wav": gen_user_join,
        "user_leave.wav": gen_user_leave,
        "stream_start.wav": gen_stream_start,
        "stream_stop.wav": gen_stream_stop,
        "connect.wav": gen_connect,
        "disconnect.wav": gen_disconnect,
    }

    for name, gen_fn in sounds.items():
        save_wav(name, gen_fn())

    print(f"\nDone — {len(sounds)} sounds written to {OUT_DIR}")

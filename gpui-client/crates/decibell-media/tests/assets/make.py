#!/usr/bin/env python3
"""Makes the test media of decibell-media (run once; the results are committed).

Everything is synthetic: the picture is a 16-cell barcode of the frame number over a grey field
(`decibell_video::barcode` reads it back), the sound is a click track (a 2 ms burst of a 1 kHz
sine every half second, starting at 0.5 s) over silence. Nothing is downloaded and nothing here
carries anybody's copyright. Needs the system's `ffmpeg` (any build with libx264, libx265,
libsvtav1, libvpx, libopus, libvorbis, libmp3lame) and numpy; the tests need neither.

    python3 make.py            # writes the files beside this script
"""
import math, os, struct, subprocess, sys, tempfile
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
W, H, FPS, RATE = 128, 72, 25, 48000


def frames(count, w=W, h=H):
    out = bytearray()
    for n in range(count):
        y = np.full((h, w), 96, np.uint8)
        for cell in range(16):
            bit = (n >> (15 - cell)) & 1
            y[: h // 2, cell * w // 16 : (cell + 1) * w // 16] = 235 if bit else 16
        out += y.tobytes()
        out += bytes([128]) * (w // 2 * (h // 2) * 2)
    return bytes(out)


def clicks(seconds, channels=1, every=0.5):
    n = int(seconds * RATE)
    x = np.zeros(n, np.float32)
    t = every
    while t < seconds - 0.01:
        start = int(round(t * RATE))
        k = np.arange(int(0.002 * RATE))
        x[start : start + len(k)] = 0.8 * np.sin(2 * math.pi * 1000 * k / RATE)
        t += every
    pcm = (x * 32767).astype("<i2")
    if channels == 2:
        pcm = np.repeat(pcm, 2)
    return pcm.tobytes()


def run(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def make(seconds, name, vargs, aargs, fps=FPS, w=W, h=H, extra=()):
    with tempfile.TemporaryDirectory() as tmp:
        ins = []
        if vargs is not None:
            raw = os.path.join(tmp, "v.yuv")
            open(raw, "wb").write(frames(int(seconds * fps), w, h))
            ins += ["-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", f"{w}x{h}", "-r", str(fps), "-i", raw]
        if aargs is not None:
            pcm = os.path.join(tmp, "a.pcm")
            open(pcm, "wb").write(clicks(seconds))
            ins += ["-f", "s16le", "-ar", str(RATE), "-ac", "1", "-i", pcm]
        run(*ins, *(vargs or []), *(aargs or []), "-map_metadata", "-1", "-fflags", "+bitexact",
            *extra, os.path.join(HERE, name))
    print(f"{os.path.getsize(os.path.join(HERE, name)):8d}  {name}")


X264 = ["-c:v", "libx264", "-preset", "veryslow", "-crf", "30", "-g", "25", "-bf", "2", "-pix_fmt", "yuv420p"]
X264_NOB = ["-c:v", "libx264", "-preset", "veryslow", "-crf", "30", "-g", "25", "-bf", "0", "-pix_fmt", "yuv420p"]
X265 = ["-c:v", "libx265", "-preset", "slow", "-crf", "32", "-g", "25", "-x265-params", "log-level=error", "-tag:v", "hvc1"]
SVT = ["-c:v", "libsvtav1", "-preset", "6", "-crf", "45", "-g", "25", "-svtav1-params", "tune=0"]
VP9 = ["-c:v", "libvpx-vp9", "-crf", "45", "-b:v", "0", "-g", "25", "-deadline", "good", "-cpu-used", "2"]
VP8 = ["-c:v", "libvpx", "-crf", "30", "-b:v", "60k", "-g", "25"]
AAC = ["-c:a", "aac", "-b:a", "32k"]
OPUS = ["-c:a", "libopus", "-b:a", "24k"]
VORBIS = ["-c:a", "libvorbis", "-q:a", "0"]

if __name__ == "__main__":
    S = 2
    FAST = ["-movflags", "+faststart"]
    # Video containers and codecs (the matrix of docs/media-design.md §2).
    make(S, "h264_aac.mp4", X264, AAC, extra=FAST)          # B-frames: reordering
    make(S, "h264_aac_tail.mp4", X264_NOB, AAC)             # moov at the end: a seeking open
    make(S, "hevc_aac.mp4", X265, AAC, extra=FAST)
    make(S, "av1_opus.mp4", SVT, OPUS, extra=FAST)
    make(S, "h264_pcm.mov", X264_NOB, ["-c:a", "pcm_s16le", "-ar", "8000"])
    make(S, "vp9_opus.webm", VP9, OPUS)
    make(S, "vp8_vorbis.webm", VP8, VORBIS)
    make(S, "av1_opus.webm", SVT, OPUS)
    make(S, "h264_aac.mkv", X264, AAC)
    make(S, "h264_only.mp4", X264_NOB, None, extra=FAST)    # no sound: the monotonic clock
    # Audio.
    make(S, "cbr.mp3", None, ["-c:a", "libmp3lame", "-b:a", "32k"])
    make(S, "vbr_noxing.mp3", None, ["-c:a", "libmp3lame", "-q:a", "9", "-write_xing", "0"])
    make(S, "vorbis.ogg", None, VORBIS)
    make(S, "opus.ogg", None, OPUS)
    make(S, "opus.opus", None, OPUS)
    make(S, "flac.flac", None, ["-c:a", "flac", "-sample_fmt", "s16"])
    make(S, "s16.wav", None, ["-c:a", "pcm_s16le", "-ar", "4000"])
    make(S, "f32.wav", None, ["-c:a", "pcm_f32le", "-ar", "4000"])
    make(S, "s24.wav", None, ["-c:a", "pcm_s24le", "-ar", "4000"])
    make(S, "aac.m4a", None, AAC)
    make(S, "adts.aac", None, AAC)
    # The 60 s A/V sync file: 64 x 16, 25 fps, a keyframe every 10 s (seek accuracy across a
    # long group of pictures), clicks every half second.
    make(60, "sync60.mp4", ["-c:v", "libx264", "-preset", "veryslow", "-crf", "34", "-g", "250", "-bf", "2",
                              "-pix_fmt", "yuv420p"], ["-c:a", "aac", "-b:a", "24k"], w=64, h=16, extra=FAST)

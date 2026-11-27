#!/usr/bin/env python3
"""Makes the media probe's test files (run once; the results are committed).

Each picture is a flat grey field whose level tells the frame number (16 + 8 n; 16 + 4 n in sar.webm) with a white
block in the stored picture's top-left corner, so a test reads back which frame the poster is
and which way it was turned. The sound is silence. Synthetic: nothing is downloaded. Needs the
system's `ffmpeg` (libx264, libsvtav1, AAC) and numpy; the tests need neither.

    python3 make.py            # writes the files beside this script

- rot90.mp4: H.264 (two B-frames) + AAC, stored 160 x 90 at 25 fps for 1 s, tagged to be shown
  turned a quarter (`-display_rotation 90`, counter-clockwise): shown 90 x 160.
- sar.webm: AV1 (decoded by dav1d) at 120 x 90 with a 2:1 sample aspect ratio, 25 fps for 2 s,
  no sound: shown 240 x 90.
- noaudio.mp4: H.264 160 x 90, 9 fps for 3 s, no sound: the poster's moment, 0.3 s (a tenth
  of the length), falls between frames 2 (0.222 s) and 3 (0.333 s).
- ends2.mp4 / ends25.mp4: H.264 64 x 36 at 25 fps + AAC, 2 s and 2.5 s: the last AAC frame ends
  after the container's length; a `<video>` reads that far in the first, not in the second.
- tone1.m4a: 1 s of AAC alone (an `<audio>` reads nothing ahead).
- vp9.webm: VP9 120 x 90 at 25 fps for 2 s (WebM): frame 4 ends exactly on the poster's moment.
"""
import os, subprocess, tempfile
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))


def frames(count, w, h, step=8):
    out = bytearray()
    for n in range(count):
        y = np.full((h, w), 16 + step * n, np.uint8)
        y[: h // 5, : w // 5] = 235
        out += y.tobytes()
        out += bytes([128]) * (w // 2 * (h // 2) * 2)
    return bytes(out)


def run(*args):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-y", *args], check=True)


def main():
    with tempfile.TemporaryDirectory() as tmp:
        raw = os.path.join(tmp, "a.yuv")
        with open(raw, "wb") as f:
            f.write(frames(25, 160, 90))
        plain = os.path.join(tmp, "plain.mp4")
        run("-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "160x90", "-r", "25", "-i", raw,
            "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono", "-t", "1",
            "-c:v", "libx264", "-preset", "veryslow", "-bf", "2", "-g", "25", "-crf", "18",
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "32k", "-shortest", plain)
        run("-display_rotation:v:0", "90", "-i", plain, "-c", "copy", os.path.join(HERE, "rot90.mp4"))

        with open(raw, "wb") as f:
            f.write(frames(50, 120, 90, step=4))
        run("-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "120x90", "-r", "25", "-i", raw,
            "-c:v", "libsvtav1", "-preset", "8", "-crf", "30", "-g", "50", "-pix_fmt", "yuv420p",
            "-aspect", "8:3", os.path.join(HERE, "sar.webm"))

        with open(raw, "wb") as f:
            f.write(frames(27, 160, 90))
        run("-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "160x90", "-r", "9", "-i", raw,
            "-r", "9", "-c:v", "libx264", "-preset", "veryslow", "-g", "30", "-crf", "18",
            "-pix_fmt", "yuv420p", os.path.join(HERE, "noaudio.mp4"))

        for name, seconds in (("ends2.mp4", "2"), ("ends25.mp4", "2.5")):
            with open(raw, "wb") as f:
                f.write(frames(63, 64, 36, step=3))
            run("-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "64x36", "-r", "25", "-i", raw,
                "-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono", "-t", seconds,
                "-c:v", "libx264", "-preset", "veryslow", "-g", "50", "-crf", "18", "-pix_fmt", "yuv420p",
                "-c:a", "aac", "-b:a", "16k", "-shortest", os.path.join(HERE, name))

        run("-f", "lavfi", "-i", "anullsrc=r=48000:cl=mono", "-t", "1", "-c:a", "aac", "-b:a", "16k",
            os.path.join(HERE, "tone1.m4a"))

        with open(raw, "wb") as f:
            f.write(frames(50, 120, 90, step=4))
        run("-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", "120x90", "-r", "25", "-i", raw,
            "-c:v", "libvpx-vp9", "-crf", "30", "-b:v", "0", "-g", "50", os.path.join(HERE, "vp9.webm"))


if __name__ == "__main__":
    main()

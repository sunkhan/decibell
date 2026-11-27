#!/usr/bin/env python3
"""Cuts the hicolor PNGs the Linux packages install (and the X11 window icon the app embeds) from
the app icon, electron-client/resources/icon.png (512 x 512), with Pillow's Lanczos filter.

    python3 gpui-client/packaging/gen-icons.py           # write gpui-client/assets/linux/icons/
    python3 gpui-client/packaging/gen-icons.py --check   # exit 1 if a committed file differs

The output is committed (CI needs no Pillow); rerun after the icon changes."""
import io
import pathlib
import sys

from PIL import Image

ROOT = pathlib.Path(__file__).resolve().parents[2]
SOURCE = ROOT / "electron-client/resources/icon.png"
OUT = ROOT / "gpui-client/assets/linux/icons/hicolor"
SIZES = [16, 22, 24, 32, 48, 64, 128, 256, 512]


def render(size: int) -> bytes:
    image = Image.open(SOURCE).convert("RGBA")
    if image.size != (size, size):
        image = image.resize((size, size), Image.LANCZOS)
    out = io.BytesIO()
    # No timestamps or text chunks: the same bytes on every run.
    image.save(out, format="PNG", optimize=True)
    return out.getvalue()


def main() -> int:
    check = "--check" in sys.argv[1:]
    stale = []
    for size in SIZES:
        path = OUT / f"{size}x{size}/apps/decibell.png"
        data = render(size)
        if check:
            if not path.is_file() or path.read_bytes() != data:
                stale.append(str(path))
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    if stale:
        print("gen-icons: stale:", *stale, sep="\n  ")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())

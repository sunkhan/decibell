#!/usr/bin/env python3
"""Compares Chromium's captures of the pixel cases with the engine's.

    pixels.py <chromium-dir> <engine-dir> <scale> <out-dir>

Per case: a side-by-side PNG (Chromium, the engine, their difference ×4) and one line of
numbers: ink coverage of each (the sum of how far pixels are from the background, in
full-ink pixels), their ratio, the mean and maximum per-pixel difference (0–255, luma), and
whether Chromium's picture has coloured fringes (LCD antialiasing) or only greys.
Needs Pillow and numpy.
"""
import os
import sys

import numpy as np
from PIL import Image

chromium_dir, engine_dir, scale, out_dir = sys.argv[1:5]
os.makedirs(out_dir, exist_ok=True)
BACKGROUND = np.array([0x17, 0x14, 0x1C], dtype=float)
INK = np.array([0xEC, 0xE6, 0xDA], dtype=float)
names = sorted(name for name in os.listdir(chromium_dir) if name.endswith(f"@{scale}x.png") and name.startswith("p"))
rows = []
for name in names:
    engine_name = name.replace(f"@{scale}x", f"@{float(scale):g}x")
    engine_path = os.path.join(engine_dir, engine_name)
    if not os.path.exists(engine_path):
        engine_path = os.path.join(engine_dir, name)
    theirs = np.asarray(Image.open(os.path.join(chromium_dir, name)).convert("RGB")).astype(float)
    ours = np.asarray(Image.open(engine_path).convert("RGB")).astype(float)
    height, width = min(theirs.shape[0], ours.shape[0]), min(theirs.shape[1], ours.shape[1])
    theirs, ours = theirs[:height, :width], ours[:height, :width]
    luma = lambda image: image @ np.array([0.2126, 0.7152, 0.0722])
    span = luma(INK[None, None, :])[0, 0] - luma(BACKGROUND[None, None, :])[0, 0]
    coverage = lambda image: np.clip((luma(image) - luma(BACKGROUND[None, None, :])) / span, -1, 2)
    cover_theirs, cover_ours = coverage(theirs), coverage(ours)
    difference = np.abs(luma(theirs) - luma(ours))
    # Colour fringes: pixels whose channels disagree by more than the palette's own tint.
    tint = INK - BACKGROUND
    chroma = lambda image: np.abs((image - BACKGROUND) / tint - ((image - BACKGROUND) / tint).mean(axis=2, keepdims=True)).max(axis=2)
    fringed = float((chroma(theirs) > 0.08).mean())
    rows.append((name, cover_theirs.sum(), cover_ours.sum(), difference.mean(), difference.max(), fringed))
    sheet = np.zeros((height * 3 + 8, width, 3), dtype=np.uint8)
    sheet[:height] = theirs.astype(np.uint8)
    sheet[height + 4 : 2 * height + 4] = ours.astype(np.uint8)
    sheet[2 * height + 8 :] = np.clip(difference * 4, 0, 255).astype(np.uint8)[:, :, None]
    Image.fromarray(sheet).save(os.path.join(out_dir, name))

print(f"{'case':<22} {'chromium ink':>12} {'engine ink':>11} {'ratio':>6} {'mean diff':>9} {'max':>5} {'fringed px':>10}")
for name, theirs, ours, mean, maximum, fringed in rows:
    print(f"{name:<22} {theirs:12.0f} {ours:11.0f} {ours / theirs:6.3f} {mean:9.2f} {maximum:5.0f} {fringed * 100:9.2f}%")
total_theirs = sum(row[1] for row in rows)
total_ours = sum(row[2] for row in rows)
print(f"{'all':<22} {total_theirs:12.0f} {total_ours:11.0f} {total_ours / total_theirs:6.3f} {np.mean([row[3] for row in rows]):9.2f}")

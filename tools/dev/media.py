"""Pictures and files for the seeded attachments: drawn stand-ins at the fixture's sizes, thumbnails made
the way the Electron uploader makes them (longer edge capped at 320 / 640 / 1280, JPEG q 82), and the
ThumbHash placeholder (a port of evanw/thumbhash's rgbaToThumbHash, as `thumbhash` on npm encodes it)."""
import base64
import io
import math
import struct
import zlib

from PIL import Image, ImageDraw, ImageFont

PALETTES = [
    ((32, 18, 54), (226, 112, 85)), ((12, 46, 70), (120, 220, 200)), ((60, 22, 22), (250, 196, 90)),
    ((20, 40, 24), (170, 230, 120)), ((44, 30, 70), (150, 170, 255)), ((70, 48, 18), (255, 230, 170)),
]


def _round(x):
    return int(math.floor(x + 0.5))


def draw_picture(key, w, h, label=""):
    """A deterministic picture: a two-colour gradient, a few discs and bars, and its size as a caption."""
    lo, hi = PALETTES[key % len(PALETTES)]
    img = Image.new("RGB", (w, h))
    grad = Image.linear_gradient("L").resize((w, h))
    img = Image.composite(Image.new("RGB", (w, h), hi), Image.new("RGB", (w, h), lo), grad.rotate(
        (key * 37) % 360, expand=False, fillcolor=128))
    d = ImageDraw.Draw(img)
    m = min(w, h)
    for i in range(5):
        cx = (key * 97 + i * 211) % w
        cy = (key * 53 + i * 137) % h
        r = m // (5 + i * 2)
        c = tuple((lo[j] * (i + 1) + hi[j] * (5 - i)) // 6 for j in range(3))
        d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=c)
    for i in range(3):
        y = h * (i + 1) // 4
        d.rectangle([0, y, w, y + max(2, h // 60)], fill=tuple(255 - v for v in lo))
    text = label or f"{w}x{h}"
    font = ImageFont.load_default(size=max(14, m // 12))
    x0 = y0 = m // 24
    box = d.textbbox((x0, y0), text, font=font)
    pad = max(4, m // 80)
    d.rectangle([box[0] - pad, box[1] - pad, box[2] + pad, box[3] + pad], fill=(0, 0, 0))
    d.text((x0, y0), text, fill=(255, 255, 255), font=font)
    return img


def jpeg(img, quality=82):
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=quality)
    return buf.getvalue()


def png(img):
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def thumbnails(img):
    """{320: jpeg, 640: jpeg, 1280: jpeg} like uploadAttachment.ts's generateThumbnail."""
    out = {}
    w0, h0 = img.size
    ratio = w0 / h0
    for size in (320, 640, 1280):
        if w0 >= h0:
            w = min(size, w0)
            h = _round(w / ratio)
        else:
            h = min(size, h0)
            w = _round(h * ratio)
        out[size] = jpeg(img.resize((max(1, w), max(1, h)), Image.LANCZOS))
    return out


def thumbhash(img):
    """base64 ThumbHash of the picture scaled to fit 100x100 (thumbhash.ts's encodeThumbHash)."""
    w0, h0 = img.size
    scale = min(1, 100 / max(w0, h0))
    w, h = max(1, _round(w0 * scale)), max(1, _round(h0 * scale))
    rgba = img.convert("RGBA").resize((w, h), Image.BILINEAR).tobytes()
    return base64.b64encode(bytes(rgba_to_thumbhash(w, h, rgba))).decode()


def rgba_to_thumbhash(w, h, rgba):
    if w > 100 or h > 100:
        raise ValueError(f"{w}x{h} doesn't fit in 100x100")
    pi, cos = math.pi, math.cos
    avg_r = avg_g = avg_b = avg_a = 0.0
    for i in range(w * h):
        j = i * 4
        alpha = rgba[j + 3] / 255
        avg_r += alpha / 255 * rgba[j]
        avg_g += alpha / 255 * rgba[j + 1]
        avg_b += alpha / 255 * rgba[j + 2]
        avg_a += alpha
    if avg_a:
        avg_r /= avg_a
        avg_g /= avg_a
        avg_b /= avg_a
    has_alpha = avg_a < w * h
    l_limit = 5 if has_alpha else 7
    lx = max(1, _round(l_limit * w / max(w, h)))
    ly = max(1, _round(l_limit * h / max(w, h)))
    L, P, Q, A = [], [], [], []
    for i in range(w * h):
        j = i * 4
        alpha = rgba[j + 3] / 255
        r = avg_r * (1 - alpha) + alpha / 255 * rgba[j]
        g = avg_g * (1 - alpha) + alpha / 255 * rgba[j + 1]
        b = avg_b * (1 - alpha) + alpha / 255 * rgba[j + 2]
        L.append((r + g + b) / 3)
        P.append((r + g) / 2 - b)
        Q.append(r - g)
        A.append(alpha)

    def encode_channel(channel, nx, ny):
        dc, ac, scale = 0.0, [], 0.0
        for cy in range(ny):
            cx = 0
            while cx * ny < nx * (ny - cy):
                fx = [cos(pi / w * cx * (x + 0.5)) for x in range(w)]
                f = 0.0
                for y in range(h):
                    fy = cos(pi / h * cy * (y + 0.5))
                    row = y * w
                    for x in range(w):
                        f += channel[x + row] * fx[x] * fy
                f /= w * h
                if cx or cy:
                    ac.append(f)
                    scale = max(scale, abs(f))
                else:
                    dc = f
                cx += 1
        if scale:
            ac = [0.5 + 0.5 / scale * v for v in ac]
        return dc, ac, scale

    l_dc, l_ac, l_scale = encode_channel(L, max(3, lx), max(3, ly))
    p_dc, p_ac, p_scale = encode_channel(P, 3, 3)
    q_dc, q_ac, q_scale = encode_channel(Q, 3, 3)
    a_dc, a_ac, a_scale = encode_channel(A, 5, 5) if has_alpha else (0, [], 0)
    is_landscape = w > h
    header24 = (_round(63 * l_dc) | (_round(31.5 + 31.5 * p_dc) << 6) | (_round(31.5 + 31.5 * q_dc) << 12)
                | (_round(31 * l_scale) << 18) | (int(has_alpha) << 23))
    header16 = ((ly if is_landscape else lx) | (_round(63 * p_scale) << 3) | (_round(63 * q_scale) << 9)
                | (int(is_landscape) << 15))
    out = [header24 & 255, (header24 >> 8) & 255, header24 >> 16, header16 & 255, header16 >> 8]
    ac_start = 6 if has_alpha else 5
    if has_alpha:
        out.append(_round(15 * a_dc) | (_round(15 * a_scale) << 4))
    idx = 0
    for ac in ([l_ac, p_ac, q_ac, a_ac] if has_alpha else [l_ac, p_ac, q_ac]):
        for f in ac:
            pos = ac_start + (idx >> 1)
            while len(out) <= pos:
                out.append(0)
            out[pos] |= _round(15 * f) << ((idx & 1) << 2)
            idx += 1
    return out


def wav_tone(seconds=3.0, freq=440.0, rate=22050):
    """A short mono 16-bit WAV (a soft chord, fading out)."""
    n = int(seconds * rate)
    frames = bytearray()
    for i in range(n):
        t = i / rate
        env = min(1.0, t * 20) * (1 - i / n)
        v = (math.sin(2 * math.pi * freq * t) + 0.6 * math.sin(2 * math.pi * freq * 1.25 * t)
             + 0.4 * math.sin(2 * math.pi * freq * 1.5 * t)) / 2.0
        frames += struct.pack("<h", int(max(-1, min(1, v * env)) * 20000))
    hdr = b"RIFF" + struct.pack("<I", 36 + len(frames)) + b"WAVEfmt " + struct.pack(
        "<IHHIIHH", 16, 1, 1, rate, rate * 2, 2, 16) + b"data" + struct.pack("<I", len(frames))
    return hdr + bytes(frames)


def zip_of(files):
    """A tiny stored ZIP (no compression) of {name: bytes}."""
    local, central, off = b"", b"", 0
    for name, data in files.items():
        crc = zlib.crc32(data)
        nb = name.encode()
        lh = struct.pack("<IHHHHHIIIHH", 0x04034B50, 20, 0, 0, 0, 0, crc, len(data), len(data), len(nb), 0) + nb
        central += struct.pack("<IHHHHHHIIIHHHHHII", 0x02014B50, 20, 20, 0, 0, 0, 0, crc, len(data), len(data),
                               len(nb), 0, 0, 0, 0, 0, off) + nb
        local += lh + data
        off += len(lh) + len(data)
    end = struct.pack("<IHHHHIIH", 0x06054B50, 0, 0, len(files), len(files), len(central), off, 0)
    return local + central + end

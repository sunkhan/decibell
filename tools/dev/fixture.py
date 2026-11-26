"""The synthetic busy channel of qt-client/tools/gen-fixture.mjs, ported to Python.

Same PRNG, same draw order, same content rules: with the default seed (7) the message contents, spacing,
replies and image attachments are exactly those of qt-client/assets/fixture/chat.json. What differs is
only who sends them (the dev stack's member list, ~40 senders weighted the same way: four heavy talkers,
four regulars, the rest occasional) and the absolute times (anchored to the seed run, not 2026-11-07).
Edited flags come from a second stream so they leave the base sequence untouched.
"""
import math


class Rng:
    """gen-fixture.mjs's `rnd` (a mulberry32 variant), bit-exact."""

    def __init__(self, seed):
        self.seed = seed & 0xFFFFFFFF

    @staticmethod
    def _imul(a, b):
        return (a * b) & 0xFFFFFFFF

    def __call__(self):
        self.seed = (self.seed + 0x6D2B79F5) & 0xFFFFFFFF
        t = self.seed
        t = self._imul(t ^ (t >> 15), t | 1)
        t ^= (t + self._imul(t ^ (t >> 7), t | 61)) & 0xFFFFFFFF
        t &= 0xFFFFFFFF
        return ((t ^ (t >> 14)) & 0xFFFFFFFF) / 4294967296

    def pick(self, a):
        return a[int(math.floor(self() * len(a)))]

    def between(self, lo, hi):
        return lo + int(math.floor(self() * (hi - lo + 1)))


# Pictures: (picsum id, width, height); the dev stack draws its own stand-ins at these sizes.
IMAGES = [
    (10, 1600, 900), (11, 1280, 960), (15, 900, 1200), (16, 1920, 1080), (17, 800, 800), (18, 2000, 800),
    (19, 1200, 1600), (20, 1600, 1066), (22, 640, 480), (24, 1080, 1350), (25, 1500, 1000), (26, 1000, 1500),
    (27, 1920, 1200), (28, 1200, 675), (29, 3000, 2000), (30, 720, 1280), (31, 1400, 1400), (32, 1024, 576),
]

WORDS = ('the a stream tonight build new server patch voice channel lag fps frame bitrate encoder works fine '
         'broken again fixed it now ok yes no maybe later tomorrow today game ranked match lobby queue join leave win lost '
         'gg nice clip watch this look at that why how when where who what config driver update reboot test release '
         'linux windows wayland nvidia audio mic headset echo noise loud quiet sorry thanks great cool wow lol '
         'selam naber tamam evet hayır sonra bugün yarın oyun maç ses kanal güncelleme').split(' ')
EMOJI = ['😂', '🔥', '👍', '❤️', '🎮', '✨', '😭', '🙏', '👀', '💀', '🎉', '😅']
LANGS = {
    'rust': 'fn main() {\n    let rate = 120;\n    println!("{rate} fps");\n}',
    'ts': 'const fps = await probe();\nif (fps < 60) retry();',
    'cpp': 'int main() {\n  return decibell::run();\n}',
    'sh': 'cargo test --lib\nnpx napi build --platform',
}


def _js_upper_first(s):
    return s[:1].upper() + s[1:] if s else s


class ContentGen:
    def __init__(self, rnd):
        self.rnd = rnd

    def sentence(self, lo, hi):
        r = self.rnd
        n = r.between(lo, hi)
        return _js_upper_first(' '.join(r.pick(WORDS) for _ in range(n)))

    def content(self):
        r = self.rnd
        x = r()
        if x < 0.40:
            return self.sentence(1, 8)
        if x < 0.58:
            s = self.sentence(10, 30)
            return s + r.pick(['.', '?', '!', ''])
        if x < 0.64:
            p = []
            i = 0
            while i < r.between(2, 4):   # the bound is redrawn every turn, as in the JS
                p.append(self.sentence(15, 40) + '.')
                i += 1
            return ' '.join(p)
        if x < 0.71:
            w = self.sentence(3, 10).split(' ')
            i = r.between(0, len(w) - 1)
            w[i] = '`' + r.pick(['cargo build', 'fps=120', 'nvenc', 'd3d11va', '--release', 'config.json', 'v0.9.4']) + '`'
            return ' '.join(w)
        if x < 0.74:
            lang = r.pick(list(LANGS.keys()))
            s = self.sentence(2, 8)
            return s + ':\n```' + lang + '\n' + LANGS[lang] + '\n```'
        if x < 0.80:
            w = self.sentence(4, 14).split(' ')
            i = r.between(0, len(w) - 1)
            m = r.pick([('**', '**'), ('*', '*'), ('__', '__'), ('~~', '~~')])
            w[i] = m[0] + w[i] + m[1]
            return ' '.join(w)
        if x < 0.88:
            s = self.sentence(2, 10)
            e = r.pick(EMOJI)
            extra = r.pick(EMOJI) if r() < 0.3 else ''
            return s + ' ' + e + extra
        if x < 0.90:
            n = r.between(1, 3)
            return ''.join(r.pick(EMOJI) for _ in range(n))
        if x < 0.95:
            pre = r.pick(['check this ', 'link: ', '', 'docs here '])
            tail = r.pick(['patch-notes', 'clips/' + str(r.between(100, 999)), 'docs/voice',
                           'issue/' + str(r.between(1, 500))])
            return pre + 'https://example.com/' + tail
        a = self.sentence(1, 6)
        b = self.sentence(2, 8).lower()
        return a + '\nand ' + b


def generate(count=10000, seed=7, senders=None, weights=None, span_days=90, end_ms=None, edit_rate=0.04):
    """Messages oldest first: dicts with sender (index into `senders`), ts_ms, content, reply_to (index of
    an earlier message or None), images (list of IMAGES entries), edited_after_s (0 = never edited).

    `senders` defaults to 16 slots like the JS fixture; `weights` to 12 / 4 / 1 for the first four, next
    four and the rest. Times keep the JS spacing and are shifted so the newest is 4 minutes before `end_ms`."""
    n_senders = len(senders) if senders else 16
    if weights is None:
        weights = [12 if i < 4 else 4 if i < 8 else 1 for i in range(n_senders)]
    total_w = sum(weights)
    rnd = Rng(seed)
    gen = ContentGen(rnd)
    edits = Rng(seed ^ 0x5EED)

    def sender():
        r = rnd() * total_w
        for i, w in enumerate(weights):
            r -= w
            if r < 0:
                return i
        return 0

    now = 1794072600000 if end_ms is None else end_ms   # Date.UTC(2026, 10, 7, 17, 30)
    ts = now - span_days * 24 * 3600e3
    out = []
    for i in range(count):
        if rnd() < 0.08:
            ts += rnd.between(1, 10) * 3600e3
        elif rnd() < 0.5:
            ts += rnd.between(5, 90) * 1000
        else:
            ts += rnd.between(2, 15) * 60e3
        m = {"sender": sender(), "ts_ms": int(math.floor(ts + 0.5)), "content": None, "reply_to": None, "images": [],
             "edited_after_s": 0}
        m["content"] = gen.content()
        if i > 5 and rnd() < 0.06:
            m["reply_to"] = len(out) - rnd.between(1, 5)
        if rnd() < 0.07:
            n = 1 if rnd() < 0.7 else rnd.between(2, 3)
            for _ in range(n):
                m["images"].append(rnd.pick(IMAGES))
            if rnd() < 0.4:
                m["content"] = ''
        if m["content"] and edits() < edit_rate:
            m["edited_after_s"] = 15 + int(edits() * 900)
        out.append(m)
    shift = now - 4 * 60000 - out[-1]["ts_ms"]
    for m in out:
        m["ts_ms"] += shift
    return out

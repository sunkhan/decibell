// The generated corpora, shared by gen.mjs (which runs them through the renderer) and mirrored by
// tests/common/corpus.rs (which regenerates the same inputs for the Rust side; the input hashes in
// suites.json prove both produce the same strings).
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));

// mulberry32 variant from qt-client/tools/gen-fixture.mjs.
export function rng(seed) {
  let s = seed >>> 0;
  return () => {
    s = (s + 0x6d2b79f5) >>> 0;
    let t = s;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// ── matrix: every marker pair around every content in every context ──
export function matrix() {
  const m = JSON.parse(fs.readFileSync(path.join(here, "matrix.json"), "utf8"));
  const out = [];
  for (const [open, close] of m.markers)
    for (const c of m.contents)
      for (const p of m.prefixes)
        for (const s of m.suffixes) out.push(p + open + c + close + s);
  return out;
}

// ── fuzz: random concatenations of weighted fragments ──
export function fuzz(over = {}) {
  const f = { ...JSON.parse(fs.readFileSync(path.join(here, "fuzz.json"), "utf8")), ...over };
  const pool = [];
  for (const [frag, weight] of f.fragments) for (let k = 0; k < weight; k++) pool.push(frag);
  const rnd = rng(f.seed);
  const out = [];
  for (let i = 0; i < f.count; i++) {
    const n = 1 + Math.floor(rnd() * f.maxFragments);
    let s = "";
    for (let k = 0; k < n; k++) s += pool[Math.floor(rnd() * pool.length)];
    out.push(s);
  }
  return out;
}

// ── fixture: the 10,000 message texts of qt-client/tools/gen-fixture.mjs --seed 7 ──
// (tools/dev/fixture.py is the Python port). Same draw order; only the contents are kept.
export function fixture(count = 10000, seedIn = 7) {
  let seed = seedIn >>> 0;
  const rnd = () => { seed = (seed + 0x6d2b79f5) >>> 0; let t = seed; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; };
  const pick = (a) => a[Math.floor(rnd() * a.length)];
  const between = (lo, hi) => lo + Math.floor(rnd() * (hi - lo + 1));
  const IMAGES = [
    [10, 1600, 900], [11, 1280, 960], [15, 900, 1200], [16, 1920, 1080], [17, 800, 800], [18, 2000, 800],
    [19, 1200, 1600], [20, 1600, 1066], [22, 640, 480], [24, 1080, 1350], [25, 1500, 1000], [26, 1000, 1500],
    [27, 1920, 1200], [28, 1200, 675], [29, 3000, 2000], [30, 720, 1280], [31, 1400, 1400], [32, 1024, 576],
  ];
  const NMEMBERS = 16;
  const WEIGHTS = Array.from({ length: NMEMBERS }, (_, i) => (i < 4 ? 12 : i < 8 ? 4 : 1));
  const totalW = WEIGHTS.reduce((a, b) => a + b, 0);
  const sender = () => { let r = rnd() * totalW; for (let i = 0; i < NMEMBERS; i++) { r -= WEIGHTS[i]; if (r < 0) return i; } return 0; };
  const WORDS = ('the a stream tonight build new server patch voice channel lag fps frame bitrate encoder works fine ' +
    'broken again fixed it now ok yes no maybe later tomorrow today game ranked match lobby queue join leave win lost ' +
    'gg nice clip watch this look at that why how when where who what config driver update reboot test release ' +
    'linux windows wayland nvidia audio mic headset echo noise loud quiet sorry thanks great cool wow lol ' +
    'selam naber tamam evet hayır sonra bugün yarın oyun maç ses kanal güncelleme').split(' ');
  const EMOJI = ['😂', '🔥', '👍', '❤️', '🎮', '✨', '😭', '🙏', '👀', '💀', '🎉', '😅'];
  const LANGS = { rust: 'fn main() {\n    let rate = 120;\n    println!("{rate} fps");\n}', ts: 'const fps = await probe();\nif (fps < 60) retry();', cpp: 'int main() {\n  return decibell::run();\n}', sh: 'cargo test --lib\nnpx napi build --platform' };
  const sentence = (lo, hi) => { const n = between(lo, hi); const w = []; for (let i = 0; i < n; i++) w.push(pick(WORDS)); const s = w.join(' '); return s[0].toUpperCase() + s.slice(1); };
  function content() {
    const r = rnd();
    if (r < 0.40) return sentence(1, 8);
    if (r < 0.58) return sentence(10, 30) + pick(['.', '?', '!', '']);
    if (r < 0.64) { const p = []; for (let i = 0; i < between(2, 4); i++) p.push(sentence(15, 40) + '.'); return p.join(' '); }
    if (r < 0.71) { const w = sentence(3, 10).split(' '); const i = between(0, w.length - 1); w[i] = '`' + pick(['cargo build', 'fps=120', 'nvenc', 'd3d11va', '--release', 'config.json', 'v0.9.4']) + '`'; return w.join(' '); }
    if (r < 0.74) { const lang = pick(Object.keys(LANGS)); return sentence(2, 8) + ':\n```' + lang + '\n' + LANGS[lang] + '\n```'; }
    if (r < 0.80) { const w = sentence(4, 14).split(' '); const i = between(0, w.length - 1); const m = pick([['**', '**'], ['*', '*'], ['__', '__'], ['~~', '~~']]); w[i] = m[0] + w[i] + m[1]; return w.join(' '); }
    if (r < 0.88) return sentence(2, 10) + ' ' + pick(EMOJI) + (rnd() < 0.3 ? pick(EMOJI) : '');
    if (r < 0.90) return Array.from({ length: between(1, 3) }, () => pick(EMOJI)).join('');
    if (r < 0.95) return pick(['check this ', 'link: ', '', 'docs here ']) + `https://example.com/${pick(['patch-notes', 'clips/' + between(100, 999), 'docs/voice', 'issue/' + between(1, 500)])}`;
    return sentence(1, 6) + '\nand ' + sentence(2, 8).toLowerCase();
  }
  const out = [];
  let n = 0;
  for (let i = 0; i < count; i++) {
    // ts draw (same order as the generator: rnd() < 0.08 ? … : rnd() < 0.5 ? … : …)
    if (rnd() < 0.08) between(1, 10); else if (rnd() < 0.5) between(5, 90); else between(2, 15);
    sender();
    let c = content();
    if (i > 5 && rnd() < 0.06) between(1, 5);
    if (rnd() < 0.07) {
      const k = rnd() < 0.7 ? 1 : between(2, 3);
      for (let j = 0; j < k; j++) pick(IMAGES);
      if (rnd() < 0.4) c = '';
    }
    out.push(c);
    n++;
  }
  return out;
}

// ── urlfuzz: URL-shaped strings, slot by slot (prefix, scheme, userinfo, host parts, port, path, tail) ──
export function slotFuzz(file, over = {}) {
  const f = { ...JSON.parse(fs.readFileSync(path.join(here, file), "utf8")), ...over };
  const pools = f.slots.map((s) => { const p = []; for (const [frag, w] of s.items) for (let k = 0; k < w; k++) p.push(frag); return { min: s.min, max: s.max, p }; });
  const rnd = rng(f.seed);
  const out = [];
  for (let i = 0; i < f.count; i++) {
    let s = "";
    for (const { min, max, p } of pools) {
      const n = min + Math.floor(rnd() * (max - min + 1));
      for (let k = 0; k < n; k++) s += p[Math.floor(rnd() * p.length)];
    }
    out.push(s);
  }
  return out;
}

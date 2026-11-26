#!/usr/bin/env node
// Fetches and refreshes the GPUI client's third-party assets in gpui-client/assets/, pinned and hash-checked,
// and keeps the assets derived from the Electron renderer (UI sounds, the lucide icon set, the inline-SVG icon
// inventory) in step with electron-client/. Zero dependencies (Node ≥ 18: global fetch, zlib).
//
//   node gpui-client/tools/fetch-assets.mjs           fetch what's missing or stale, refresh the derived files, verify
//   node gpui-client/tools/fetch-assets.mjs --check   verify only: no network, no writes; exit 1 if anything is stale
//   node gpui-client/tools/fetch-assets.mjs --print   also print the sha256 of every pinned file (to update the pins)
//
// Committed (see assets/README.md): fonts/ (TTF + OFL), icons/lucide/ (SVG + manifest + licence),
// icons/inline-svg-components.md, sounds/ (WAV). Fetched and gitignored: twemoji/ (SVGs + index.json),
// emoji-data/ (emoji-mart's dataset). `--check` reports a missing gitignored folder as "not fetched" without failing.
//
// Fonts. The renderer gets its faces from fontsource, which repackages the Google Fonts API's output (unicode-range
// .woff2 subsets). GPUI's text stacks don't read WOFF2 or merge subsets, so the client bundles the unsplit TTFs of the
// same builds, checked glyph-for-glyph against the renderer's subsets (assets/README.md):
//   - Inter 4.001 and Mona Sans 2.000: the variable TTFs in google/fonts (the files fontsource's subsets are cut
//     from; unchanged there since 2024).
//   - JetBrains Mono 2.211: the four static faces the renderer imports (400/500/600/700), from the Google Fonts API
//     (fonts.gstatic.com v24, the files fontsource's static subsets are cut from). Not the variable TTF: its named
//     instances have no SemiBold, and a text stack that matches named instances only (DirectWrite font sets) would
//     draw 600 as 700.
// Only upright faces: the renderer loads no italic face (Chromium synthesises the slant).
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const GPUI = path.resolve(here, '..');
const REPO = path.resolve(GPUI, '..');
const ASSETS = path.join(GPUI, 'assets');
const RENDERER = path.join(REPO, 'electron-client');
const RENDERER_SRC = path.join(RENDERER, 'src');
const LOCK = JSON.parse(fs.readFileSync(path.join(RENDERER, 'package-lock.json'), 'utf8')).packages;

const CHECK = process.argv.includes('--check');
const PRINT = process.argv.includes('--print');
const problems = [];
const notes = [];
const say = (s) => console.log(s);

// ── pins ──────────────────────────────────────────────────────────────────────────────────────────────
const GF = 'https://raw.githubusercontent.com/google/fonts/8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5/ofl';
const GS = 'https://fonts.gstatic.com/s/jetbrainsmono/v24';
const FONTS = [
  { out: 'fonts/InterVariable.ttf', url: `${GF}/inter/Inter%5Bopsz%2Cwght%5D.ttf`, sha256: '29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031' },
  { out: 'fonts/MonaSansVariable.ttf', url: `${GF}/monasans/MonaSans%5Bwdth%2Cwght%5D.ttf`, sha256: 'fd6e79634b5ae804a45aac7e2e3c2a325b41291fba59034f4732b0135b8475b3' },
  { out: 'fonts/JetBrainsMono-Regular.ttf', url: `${GS}/tDbY2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8yKxjPQ.ttf`, sha256: '44ce4a84f20d60f24539bd0cef11f79c29e38609e0f8adf18551c9794a5d9dc3' },
  { out: 'fonts/JetBrainsMono-Medium.ttf', url: `${GS}/tDbY2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8-qxjPQ.ttf`, sha256: '3386a05f6ece969e4537de6be894170d20558e82f7d56c8c5d332972ef172160' },
  { out: 'fonts/JetBrainsMono-SemiBold.ttf', url: `${GS}/tDbY2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8FqtjPQ.ttf`, sha256: 'df54dbfafba61d4911eb3dab9bba2d20531fb009f01d64dd42fa96ab862584d8' },
  { out: 'fonts/JetBrainsMono-Bold.ttf', url: `${GS}/tDbY2o-flEEny0FZhsfKu5WU4zr3E_BX0PnT8RD8L6tjPQ.ttf`, sha256: 'a1d92abc6b02a87faed23d98067ab1027e8e95242fd7c9978a072ea383b89d1a' },
  { out: 'fonts/OFL-Inter.txt', url: `${GF}/inter/OFL.txt`, sha256: '5b9321a4298cfeb6b34354164a1c3afc3db114569984c502b9b35d988fd58c57' },
  { out: 'fonts/OFL-MonaSans.txt', url: `${GF}/monasans/OFL.txt`, sha256: '9261dcb61fb5e3c587d50d7a9fdae12bc7422d8822d7ac06b8f34550479575de' },
  { out: 'fonts/OFL-JetBrainsMono.txt', url: `${GF}/jetbrainsmono/OFL.txt`, sha256: 'b2fe5e8987594e9ffd1d2ca52a2f5d73eb8335243893c5d6254b5ad69269591d' },
];
// The fontsource releases the font pins above were checked against. A renderer bump changes what it draws, so the
// pins (and the glyph comparison in assets/README.md) have to be redone: fail until they are.
const FONTSOURCE = {
  '@fontsource-variable/inter': '5.2.8',
  '@fontsource-variable/mona-sans': '5.3.0',
  '@fontsource/jetbrains-mono': '5.3.0',
};
// lucide-static (the release's SVG files) isn't in the renderer's lockfile; its pin must follow lucide-react's.
const LUCIDE_STATIC = {
  version: '1.14.0',
  integrity: 'sha512-QnAHXC1nk8IXRBmO5ee9JcPiKEVeqf06tKF0bZipxlV++bwUVqvvliC3KA+beDg2lod+WXYIhHde5pQRHkUmqA==',
};

// ── helpers ───────────────────────────────────────────────────────────────────────────────────────────
const sha256 = (buf) => crypto.createHash('sha256').update(buf).digest('hex');
const rel = (p) => path.relative(REPO, p);

function lockEntry(name) {
  const e = LOCK[`node_modules/${name}`];
  if (!e) throw new Error(`${name} is not in electron-client/package-lock.json`);
  return e;
}

async function download(url) {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: HTTP ${r.status}`);
  return Buffer.from(await r.arrayBuffer());
}

async function npmTarball(url, integrity) {
  const tgz = await download(url);
  const [algo, want] = integrity.split('-');
  if (crypto.createHash(algo).update(tgz).digest('base64') !== want) throw new Error(`${url}: integrity mismatch`);
  return untar(zlib.gunzipSync(tgz));
}

// Minimal tar reader (ustar + pax `path` + GNU long names): regular files only, as a Map name → bytes.
function untar(buf) {
  const out = new Map();
  let longName = null;
  for (let off = 0; off + 512 <= buf.length; ) {
    const h = buf.subarray(off, off + 512);
    if (h.every((b) => b === 0)) break;
    const str = (a, b) => h.subarray(a, b).toString('utf8').replace(/\0.*$/s, '');
    const size = parseInt(str(124, 136).trim() || '0', 8);
    const type = String.fromCharCode(h[156] || 48);
    const body = buf.subarray(off + 512, off + 512 + size);
    const prefix = str(345, 500);
    let name = longName ?? (prefix ? prefix + '/' : '') + str(0, 100);
    longName = null;
    if (type === 'x') {
      const m = body.toString('utf8').match(/^\d+ path=(.*)$/m);
      if (m) longName = m[1];
    } else if (type === 'L') longName = body.toString('utf8').replace(/\0.*$/s, '');
    else if (type === '0' || type === '\0') out.set(name, Buffer.from(body));
    off += 512 + Math.ceil(size / 512) * 512;
  }
  return out;
}

// Writes `body` to `file` unless it already holds exactly that; in --check mode records a problem instead.
function sync(file, body, what) {
  const cur = fs.existsSync(file) ? fs.readFileSync(file) : null;
  const buf = Buffer.isBuffer(body) ? body : Buffer.from(body);
  if (cur && cur.equals(buf)) return false;
  if (CHECK) {
    problems.push(`${what ?? rel(file)}: ${cur ? 'stale' : 'missing'}`);
    return false;
  }
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, buf);
  say(`${cur ? 'updated' : 'wrote'} ${rel(file)}`);
  return true;
}

function walk(dir, filter, out = []) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, filter, out);
    else if (filter(e.name)) out.push(p);
  }
  return out;
}

// ── 1. fonts (committed) ──────────────────────────────────────────────────────────────────────────────
for (const [name, want] of Object.entries(FONTSOURCE)) {
  const got = lockEntry(name).version;
  if (got !== want) problems.push(`${name} is ${got} in the renderer's lockfile, the font pins were checked against ${want}: re-pin (assets/README.md)`);
}
for (const f of FONTS) {
  const p = path.join(ASSETS, f.out);
  if (!fs.existsSync(p)) {
    if (CHECK) { problems.push(`${f.out}: missing`); continue; }
    const buf = await download(f.url);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, buf);
    say(`fetched ${f.out}`);
  }
  const h = sha256(fs.readFileSync(p));
  if (PRINT) say(`${h}  ${f.out}`);
  if (h !== f.sha256) problems.push(`${f.out}: sha256 ${h}, pinned ${f.sha256}`);
}

// ── 2. twemoji + emoji data (fetched, gitignored) ─────────────────────────────────────────────────────
// The renderer inlines @twemoji/svg's files (scripts/build-twemoji-map.cjs) and reads names, categories, keywords and
// skin variants from @emoji-mart/data's package main (sets/15/native.json). Both come from the npm tarballs the
// renderer's lockfile pins, checked against its SRI integrity, so a renderer bump moves the client with it.
async function npmPackage({ name, dir, extract }) {
  const e = lockEntry(name);
  const outDir = path.join(ASSETS, dir);
  const marker = path.join(outDir, '.integrity');
  if (fs.existsSync(marker) && fs.readFileSync(marker, 'utf8') === e.integrity) return say(`${dir}/: ${name}@${e.version} ok`);
  if (CHECK) return notes.push(`${dir}/ (gitignored): ${fs.existsSync(marker) ? `not ${name}@${e.version}` : 'not fetched'}; run the script without --check`);
  const files = await npmTarball(e.resolved, e.integrity);
  fs.rmSync(outDir, { recursive: true, force: true });
  fs.mkdirSync(outDir, { recursive: true });
  const n = extract(files, outDir, e);
  fs.writeFileSync(marker, e.integrity);
  say(`${dir}/: unpacked ${n} files from ${name}@${e.version}`);
}

await npmPackage({
  name: '@twemoji/svg',
  dir: 'twemoji',
  extract(files, outDir, e) {
    const seqs = [];
    for (const [name, data] of files) {
      const m = name.match(/^package\/([0-9a-f]+(?:-[0-9a-f]+)*)\.svg$/);
      if (!m) continue;
      fs.writeFileSync(path.join(outDir, `${m[1]}.svg`), data);
      seqs.push(m[1]);
    }
    fs.writeFileSync(path.join(outDir, 'LICENSE-package.txt'), files.get('package/license'));
    seqs.sort();
    // `sequences`: every file name without `.svg`, lower-case hex code points joined by '-', e.g. `1f44d-1f3fd`.
    const index = {
      package: `@twemoji/svg@${e.version}`,
      count: seqs.length,
      file: '<sequence>.svg',
      note: 'Code points in lower-case hex joined by "-". Twemoji keeps U+FE0F inside ZWJ sequences and drops it elsewhere; the renderer (Twemoji.tsx toCodePoints) drops it from every multi-code-point sequence, see assets/README.md.',
      sequences: seqs,
    };
    fs.writeFileSync(path.join(outDir, 'index.json'), JSON.stringify(index, null, 1) + '\n');
    return seqs.length + 2;
  },
});

await npmPackage({
  name: '@emoji-mart/data',
  dir: 'emoji-data',
  extract(files, outDir) {
    const main = JSON.parse(files.get('package/package.json')).main; // what `import data from "@emoji-mart/data"` loads
    fs.writeFileSync(path.join(outDir, 'native.json'), files.get(`package/${main}`));
    fs.writeFileSync(path.join(outDir, 'LICENSE'), files.get('package/LICENSE'));
    return 2;
  },
});

// ── 3. lucide (committed) ─────────────────────────────────────────────────────────────────────────────
// Every icon the renderer imports from lucide-react, as lucide-static's SVG of the same release. Import names
// resolve to icon files through lucide-react's own export list (aliases such as `AlertTriangle` → triangle-alert),
// and each SVG's geometry is checked against lucide-react's icon node, which is what the renderer draws.
const sources = walk(RENDERER_SRC, (n) => /\.(tsx?|jsx?|mts|mjs)$/.test(n));
const lucideImports = new Map(); // import name → Set(importing files)
const lucideUses = []; // { name, file, attrs }
for (const file of sources) {
  const s = fs.readFileSync(file, 'utf8');
  const locals = new Map();
  for (const m of s.matchAll(/import\s+(type\s+)?\{([^}]*)\}\s*from\s*['"]lucide-react['"]/g)) {
    for (let part of m[2].split(',')) {
      part = part.trim().replace(/^type\s+/, '');
      if (!part) continue;
      const [orig, alias] = part.split(/\s+as\s+/).map((x) => x.trim());
      if (!lucideImports.has(orig)) lucideImports.set(orig, new Set());
      lucideImports.get(orig).add(rel(file).replace(/^electron-client\/src\//, ''));
      locals.set(alias ?? orig, orig);
    }
  }
  if (/['"]lucide-react['"]/.test(s.replace(/import\s+(type\s+)?\{[^}]*\}\s*from\s*['"]lucide-react['"]/g, ''))) {
    problems.push(`${rel(file)}: imports lucide-react in a form this script doesn't read (only named imports)`);
  }
  for (const [local, orig] of locals) {
    for (const m of s.matchAll(new RegExp(`<${local}\\b`, 'g'))) {
      const tag = jsxTag(s, m.index);
      lucideUses.push({ name: orig, file: rel(file).replace(/^electron-client\/src\//, ''), attrs: tag.attrs });
    }
  }
}

const lucideReact = lockEntry('lucide-react');
const LUCIDE_DIR = path.join(ASSETS, 'icons/lucide');
const MANIFEST = path.join(LUCIDE_DIR, 'manifest.json');
let manifest = fs.existsSync(MANIFEST) ? JSON.parse(fs.readFileSync(MANIFEST, 'utf8')) : null;
const wantNames = [...lucideImports.keys()].sort();
const haveMap = manifest?.lucideReact === lucideReact.version ? manifest.importNames ?? {} : null;
let needFetch = !haveMap || wantNames.some((n) => !(n in haveMap));
if (!needFetch) {
  for (const icon of new Set(wantNames.map((n) => haveMap[n]))) {
    const p = path.join(LUCIDE_DIR, `${icon}.svg`);
    if (!fs.existsSync(p) || sha256(fs.readFileSync(p)) !== manifest.icons?.[icon]?.sha256) needFetch = true;
  }
}
let importNames, iconFiles; // name → icon; icon → svg bytes
if (needFetch) {
  if (lucideReact.version !== LUCIDE_STATIC.version) {
    throw new Error(`lucide-react is ${lucideReact.version} in the renderer's lockfile; pin lucide-static ${lucideReact.version} (LUCIDE_STATIC)`);
  }
  if (CHECK) {
    problems.push('icons/lucide/: stale (the renderer imports icons the manifest lacks, or an SVG changed)');
  } else {
    const react = await npmTarball(lucideReact.resolved, lucideReact.integrity);
    const stat = await npmTarball(`https://registry.npmjs.org/lucide-static/-/lucide-static-${LUCIDE_STATIC.version}.tgz`, LUCIDE_STATIC.integrity);
    const exportsSrc = react.get('package/dist/esm/lucide-react.mjs').toString('utf8');
    const byName = new Map();
    for (const m of exportsSrc.matchAll(/export \{([^}]*)\} from '\.\/icons\/([a-z0-9-]+)\.mjs';/g)) {
      for (const n of m[1].matchAll(/default as (\w+)/g)) byName.set(n[1], m[2]);
    }
    importNames = {};
    iconFiles = new Map();
    for (const n of wantNames) {
      const icon = byName.get(n);
      if (!icon) throw new Error(`lucide-react@${lucideReact.version} exports no icon named ${n}`);
      importNames[n] = icon;
      const svg = stat.get(`package/icons/${icon}.svg`);
      if (!svg) throw new Error(`lucide-static@${LUCIDE_STATIC.version} has no icons/${icon}.svg`);
      const node = react.get(`package/dist/esm/icons/${icon}.mjs`).toString('utf8');
      const a = svgElements(svg.toString('utf8')), b = iconNodeElements(node);
      if (JSON.stringify(a) !== JSON.stringify(b)) throw new Error(`${icon}: lucide-static's SVG differs from lucide-react's icon node`);
      iconFiles.set(icon, svg);
    }
    iconFiles.set('LICENSE', stat.get('package/LICENSE'));
  }
} else {
  importNames = Object.fromEntries(wantNames.map((n) => [n, haveMap[n]]));
  iconFiles = new Map([...new Set(Object.values(importNames))].map((i) => [i, fs.readFileSync(path.join(LUCIDE_DIR, `${i}.svg`))]));
  if (fs.existsSync(path.join(LUCIDE_DIR, 'LICENSE'))) iconFiles.set('LICENSE', fs.readFileSync(path.join(LUCIDE_DIR, 'LICENSE')));
  else problems.push('icons/lucide/LICENSE: missing');
}
if (importNames) {
  for (const [icon, data] of iconFiles) sync(path.join(LUCIDE_DIR, icon === 'LICENSE' ? 'LICENSE' : `${icon}.svg`), data);
  if (fs.existsSync(LUCIDE_DIR)) {
    for (const f of fs.readdirSync(LUCIDE_DIR)) {
      if (!f.endsWith('.svg') || iconFiles.has(f.slice(0, -4))) continue;
      if (CHECK) problems.push(`icons/lucide/${f}: no longer imported by the renderer`);
      else { fs.rmSync(path.join(LUCIDE_DIR, f)); say(`removed icons/lucide/${f}`); }
    }
  }
  const icons = {};
  for (const icon of [...new Set(Object.values(importNames))].sort()) {
    const uses = lucideUses
      .filter((u) => importNames[u.name] === icon)
      .map((u) => {
        const size = numAttr(u.attrs.size) ?? 24;
        const sw = numAttr(u.attrs.strokeWidth) ?? 2;
        const abs = 'absoluteStrokeWidth' in u.attrs;
        return {
          file: u.file,
          as: u.name,
          size: u.attrs.size ?? '24 (default)',
          strokeWidth: u.attrs.strokeWidth ?? '2 (default)',
          ...(abs ? { absoluteStrokeWidth: true } : {}),
          ...(u.attrs.className ? { className: u.attrs.className } : {}),
          // Lucide scales the stroke with the icon unless absoluteStrokeWidth is set.
          strokePx: abs ? sw : +((sw * size) / 24).toFixed(4),
        };
      })
      .sort((a, b) => (JSON.stringify(a) < JSON.stringify(b) ? -1 : 1));
    icons[icon] = {
      sha256: sha256(iconFiles.get(icon)),
      importedAs: wantNames.filter((n) => importNames[n] === icon),
      importedBy: [...new Set(wantNames.filter((n) => importNames[n] === icon).flatMap((n) => [...lucideImports.get(n)]))].sort(),
      uses,
    };
  }
  manifest = {
    note: 'Generated by gpui-client/tools/fetch-assets.mjs from the renderer\'s lucide-react imports. Do not edit.',
    lucideReact: lucideReact.version,
    lucideStatic: LUCIDE_STATIC.version,
    svgDefaults: { viewBox: '0 0 24 24', fill: 'none', stroke: 'currentColor', strokeWidth: 2, strokeLinecap: 'round', strokeLinejoin: 'round' },
    importNames,
    icons,
    strokeWidthsUsed: [...new Set(Object.values(icons).flatMap((i) => i.uses.map((u) => numAttr(u.strokeWidth) ?? 2)))].sort((a, b) => a - b),
  };
  sync(MANIFEST, JSON.stringify(manifest, null, 2) + '\n');
  say(`icons/lucide/: ${Object.keys(icons).length} icons (${wantNames.join(', ')})`);
}

// ── 4. inline-SVG icon inventory (committed, generated) ───────────────────────────────────────────────
sync(path.join(ASSETS, 'icons/inline-svg-components.md'), inlineSvgInventory());

// ── 5. UI sounds (committed copies) ───────────────────────────────────────────────────────────────────
// Copied rather than referenced across the tree so the client's include paths stay inside gpui-client/; kept
// byte-identical to the renderer's (utils/sounds.ts plays them as-is: one-shot, unity gain; call_ring loops).
{
  const src = path.join(RENDERER_SRC, 'assets/sounds');
  const dst = path.join(ASSETS, 'sounds');
  const names = fs.readdirSync(src).filter((f) => f.endsWith('.wav')).sort();
  for (const f of names) sync(path.join(dst, f), fs.readFileSync(path.join(src, f)), `sounds/${f}`);
  if (fs.existsSync(dst)) {
    for (const f of fs.readdirSync(dst)) {
      if (!f.endsWith('.wav') || names.includes(f)) continue;
      if (CHECK) problems.push(`sounds/${f}: no longer in the renderer`);
      else { fs.rmSync(path.join(dst, f)); say(`removed sounds/${f}`); }
    }
  }
  say(`sounds/: ${names.length} WAV files`);
}

for (const n of notes) console.error(`note: ${n}`);
if (problems.length) {
  for (const p of problems) console.error(`error: ${p}`);
  process.exit(1);
}
say(CHECK ? 'assets current' : 'assets ok');

// ── JSX / SVG readers ─────────────────────────────────────────────────────────────────────────────────
// Reads the JSX opening tag that starts at `at` (`<Name …>`): its attributes as source text, `{…}` unwrapped.
function jsxTag(s, at) {
  const attrs = {};
  let end = at;
  let i = s.slice(at + 1).search(/[\s/>]/) + at + 1;
  while (i < s.length) {
    while (/\s/.test(s[i])) i++;
    if (s[i] === '/' || s[i] === '>') { end = s.indexOf('>', i) + 1; break; }
    if (s[i] === '{') { // spread
      const j = balanced(s, i);
      attrs['...'] = (attrs['...'] ? attrs['...'] + ' ' : '') + s.slice(i + 1, j - 1).trim();
      i = j;
      continue;
    }
    const m = s.slice(i).match(/^([A-Za-z_:][\w:.-]*)/);
    if (!m) { end = i; break; }
    const name = m[1];
    i += name.length;
    if (s[i] === '=') {
      i++;
      if (s[i] === '"' || s[i] === "'") {
        const q = s[i], j = s.indexOf(q, i + 1);
        attrs[name] = s.slice(i + 1, j);
        i = j + 1;
      } else if (s[i] === '{') {
        const j = balanced(s, i);
        attrs[name] = s.slice(i + 1, j - 1).trim().replace(/^["'`](.*)["'`]$/s, '$1').replace(/\s+/g, ' ');
        i = j;
      }
    } else attrs[name] = true;
  }
  return { attrs, end };
}
function balanced(s, i) {
  let d = 0;
  for (; i < s.length; i++) {
    if (s[i] === '{') d++;
    else if (s[i] === '}' && --d === 0) return i + 1;
  }
  return s.length;
}
function numAttr(v) {
  if (v === undefined || v === true) return undefined;
  const m = String(v).match(/^\s*(-?[\d.]+)/);
  return m ? +m[1] : undefined;
}
function kebab(k) {
  return k.replace(/[A-Z]/g, (c) => '-' + c.toLowerCase());
}
function svgElements(svg) {
  const body = svg.replace(/<!--[\s\S]*?-->/g, '').replace(/^[\s\S]*?<svg[^>]*>/, '').replace(/<\/svg>[\s\S]*$/, '');
  return [...body.matchAll(/<([a-z]+)\s([^>]*?)\/?>/g)].map((m) => [m[1], Object.fromEntries([...m[2].matchAll(/([\w-]+)="([^"]*)"/g)].map((a) => [a[1], a[2]]).sort())]);
}
function iconNodeElements(src) {
  const arr = src.slice(src.indexOf('__iconNode = ['), src.indexOf('];', src.indexOf('__iconNode = [')));
  return [...arr.matchAll(/\[\s*"([a-z]+)",\s*\{([^}]*)\}\s*\]/g)].map((m) => [
    m[1],
    Object.fromEntries([...m[2].matchAll(/(\w+):\s*"([^"]*)"/g)].filter((a) => a[1] !== 'key').map((a) => [kebab(a[1]), a[2]]).sort()),
  ]);
}

// The renderer draws almost all of its icons as hand-written inline SVG, not lucide. This lists them so the UI port
// knows what exists: components whose job is a glyph (by name, or because they live in an icons.tsx), then every
// other <svg> drawn inline inside a larger component. Line numbers are left out on purpose (they churn); grep the name.
function inlineSvgInventory() {
  const DECL = /^(export\s+)?(?:default\s+)?(?:function\s+(\w+)|const\s+(\w+)\s*(?::[^=]+)?=)/;
  const ICONISH = /(icon|glyph|mark|dot|spinner|art|chevron)$/i;
  const icons = [], inline = [];
  const strokeWidths = new Map();
  for (const file of walk(RENDERER_SRC, (n) => n.endsWith('.tsx'))) {
    const s = fs.readFileSync(file, 'utf8');
    const lines = s.split('\n');
    const relf = path.relative(RENDERER_SRC, file);
    const offsets = [];
    let o = 0;
    for (const l of lines) { offsets.push(o); o += l.length + 1; }
    const decls = [];
    lines.forEach((l, i) => { const m = l.match(DECL); if (m) decls.push({ name: m[2] || m[3], exported: !!m[1], line: i }); });
    const wrapper = decls.find((d) => d.name === 'Svg');
    let wrapperAttrs = null;
    let seen = 0;
    decls.forEach((d, k) => {
      const from = offsets[d.line], to = k + 1 < decls.length ? offsets[decls[k + 1].line] : s.length;
      const body = s.slice(from, to);
      const svgs = [...body.matchAll(/<(svg|Svg)\b/g)].map((m) => ({ tag: m[1], ...jsxTag(body, m.index) }));
      seen += svgs.filter((g) => g.tag === 'svg').length;
      if (svgs.length) for (const m of body.matchAll(/strokeWidth=\{?["']?([\d.]+)/g)) strokeWidths.set(m[1], (strokeWidths.get(m[1]) ?? 0) + 1);
      if (d === wrapper) { wrapperAttrs = svgs[0]?.attrs ?? null; return; }
      const isIcon = ICONISH.test(d.name) || path.basename(file) === 'icons.tsx' || d.name === 'BellMark';
      if (!svgs.length && !(isIcon && d.name === 'BellMark')) return;
      if (isIcon) {
        const first = svgs.find((g) => g.tag === 'svg') ?? svgs[0];
        let a = first?.tag === 'Svg' && wrapperAttrs ? { ...wrapperAttrs, ...first.attrs } : { ...(first?.attrs ?? {}) };
        if (a['...']) {
          // Attributes spread from a helper in the same file (the voice stage's `{...props(p)}`): read the helper's
          // object literal, and a `...stroke(<width>)` call for the stroke width.
          for (const k of ['viewBox', 'fill', 'stroke']) a[k] ??= s.match(new RegExp(`\\b${k}:\\s*"([^"]*)"`))?.[1];
          a.strokeWidth ??= s.match(/\.\.\.stroke\(([^)]*)\)/)?.[1];
          a.width ??= s.match(/\bsize\s*=\s*([\d.]+)\s*,\s*bold/)?.[1];
        }
        // Stroke set on the children rather than the <svg> (Spinner).
        a.stroke ??= body.slice(first?.end ?? 0).match(/\bstroke="([^"]+)"/)?.[1];
        a.strokeWidth ??= body.slice(first?.end ?? 0).match(/\bstrokeWidth=\{?"?([\d.]+)/)?.[1];
        const shapes = {};
        for (const m of body.matchAll(/<(path|circle|rect|line|polyline|polygon|ellipse)\b/g)) shapes[m[1]] = (shapes[m[1]] ?? 0) + 1;
        let draws = Object.entries(shapes).map(([t, n]) => (n > 1 ? `${n} ${t}` : t)).join(', ');
        if (d.name === 'BellMark') draws = 'rounded <div> bars on a Gaussian (no SVG)';
        else if (first && 'dangerouslySetInnerHTML' in first.attrs) draws = 'SVG markup strings passed in (dangerouslySetInnerHTML)';
        else if (!draws) draws = 'children passed in';
        const size = body.match(/size\s*=\s*([\d.]+)/)?.[1];
        let doc = [];
        for (let i = d.line - 1; i >= 0 && /^\s*\/\//.test(lines[i]); i--) doc.unshift(lines[i].replace(/^\s*\/\/\/?\s?/, ''));
        icons.push({
          name: d.name, file: relf, exported: d.exported, draws,
          viewBox: a.viewBox ?? '', paint: a.fill && a.fill !== 'none' ? `fill ${a.fill}` : a.stroke ? `stroke ${a.stroke}` : '',
          strokeWidth: a.strokeWidth ?? '', size: size ?? a.width ?? '', doc: doc.join(' ').replace(/\s+/g, ' ').slice(0, 160),
        });
      } else {
        const n = svgs.filter((g) => g.tag === 'svg').length;
        if (n) inline.push({ name: d.name, file: relf, count: n });
      }
    });
    const total = (s.match(/<svg\b/g) ?? []).length;
    if (total !== seen) problems.push(`${relf}: ${total - seen} <svg> outside any top-level declaration; teach inlineSvgInventory`);
  }
  const esc = (x) => String(x).replace(/\|/g, '\\|');
  let md = '<!-- Generated by gpui-client/tools/fetch-assets.mjs from electron-client/src. Do not edit: rerun the script. -->\n';
  md += '# Inline SVG icons in the renderer\n\n';
  md += 'The renderer imports only a handful of lucide icons (`icons/lucide/manifest.json`); nearly every glyph is a\n';
  md += 'hand-written inline SVG. Nothing here is converted: port each one from its source (paths relative to\n';
  md += '`electron-client/src/`, grep the component name). Most share one idiom: a 24-unit viewBox, `fill="none"`,\n';
  md += '`stroke="currentColor"`, round caps and joins, so the glyph takes the text colour.\n\n';
  md += `## Icon components (${icons.length})\n\n`;
  md += 'Components whose job is a glyph: named `…Icon` / `…Glyph` / `…Mark` / `…Dot` / `…Spinner` / `…Art` / `Chevron`,\n';
  md += 'or exported from an `icons.tsx`. `Svg` wrappers in an `icons.tsx` supply the viewBox and stroke of the icons in that file.\n\n';
  md += '| Component | File | Draws | viewBox | Paint | Stroke width | Size | Notes |\n|---|---|---|---|---|---|---|---|\n';
  for (const i of icons) md += `| \`${i.name}\`${i.exported ? '' : ' (local)'} | ${i.file} | ${esc(i.draws)} | ${esc(i.viewBox)} | ${esc(i.paint)} | ${esc(i.strokeWidth)} | ${esc(i.size)} | ${esc(i.doc)} |\n`;
  md += `\n## Inline \`<svg>\` inside larger components (${inline.reduce((a, b) => a + b.count, 0)} in ${inline.length} components)\n\n`;
  md += 'Glyphs drawn in place (buttons, menus, headers). Port them with the component that draws them.\n\n';
  md += '| Component | File | `<svg>` count |\n|---|---|---|\n';
  for (const i of inline) md += `| \`${i.name}\` | ${i.file} | ${i.count} |\n`;
  md += '\n## Stroke widths written on inline SVGs\n\n';
  md += 'Literal `strokeWidth` attributes in the components above (on `<svg>` elements and their children) and how often\n';
  md += 'each appears; an expression counts under its first number, and the voice stage\'s `...stroke(bold ? 2.5 : 2)` helper\n';
  md += 'is not counted. Lucide icons scale their stroke with the icon size (`icons/lucide/manifest.json`); these do too,\n';
  md += 'since the stroke is in viewBox units.\n\n';
  md += [...strokeWidths].sort((a, b) => +a[0] - +b[0]).map(([w, n]) => `\`${w}\` ×${n}`).join(', ') + '\n';
  return md;
}

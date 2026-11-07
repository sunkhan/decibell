#!/usr/bin/env node
// Downloads the Qt client's third-party assets into qt-client/assets/ (gitignored), pinned and hash-checked.
//
//   node qt-client/tools/fetch-assets.mjs            fetch what's missing, verify everything
//   node qt-client/tools/fetch-assets.mjs --print    print the sha256 of each fetched file (to update the pins)
//
// Fonts: the renderer gets them from fontsource, which serves Google Fonts' builds split into unicode-range
// .woff2 subsets (Mona Sans v4, Inter v20, JetBrains Mono v24, all built from these files). Qt can't merge
// subsets into one family, so the client bundles the unsplit variable TTFs from google/fonts at a commit where
// those files are the ones Google serves (the files last changed in 2024; later commits touch metadata only).
// Only upright faces: the renderer loads no italic face (Chromium synthesises the slant), so neither does Qt.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const ASSETS = path.resolve(here, '../assets');
const GF = 'https://raw.githubusercontent.com/google/fonts/8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5/ofl';
const FILES = [
  { out: 'fonts/MonaSans.ttf', url: `${GF}/monasans/MonaSans%5Bwdth%2Cwght%5D.ttf`, sha256: 'fd6e79634b5ae804a45aac7e2e3c2a325b41291fba59034f4732b0135b8475b3' },
  { out: 'fonts/Inter.ttf', url: `${GF}/inter/Inter%5Bopsz%2Cwght%5D.ttf`, sha256: '29160a80ff49ddcab2c97711247e08b1fab27a484a329ce8b813d820dc559031' },
  { out: 'fonts/JetBrainsMono.ttf', url: `${GF}/jetbrainsmono/JetBrainsMono%5Bwght%5D.ttf`, sha256: '48715a42ec242c21e9f02692891e147d022299a52e48d5e413e1a942193ffeda' },
  { out: 'fonts/OFL-MonaSans.txt', url: `${GF}/monasans/OFL.txt`, sha256: '9261dcb61fb5e3c587d50d7a9fdae12bc7422d8822d7ac06b8f34550479575de' },
  { out: 'fonts/OFL-Inter.txt', url: `${GF}/inter/OFL.txt`, sha256: '5b9321a4298cfeb6b34354164a1c3afc3db114569984c502b9b35d988fd58c57' },
  { out: 'fonts/OFL-JetBrainsMono.txt', url: `${GF}/jetbrainsmono/OFL.txt`, sha256: 'b2fe5e8987594e9ffd1d2ca52a2f5d73eb8335243893c5d6254b5ad69269591d' },
];

// Twemoji: the same @twemoji/svg release the renderer uses (electron-client/package.json), as its npm tarball
// (checked against the registry's SRI integrity), unpacked to assets/twemoji/<codepoints>.svg.
const TARBALLS = [
  {
    dir: 'twemoji',
    url: 'https://registry.npmjs.org/@twemoji/svg/-/svg-15.0.0.tgz',
    integrity: 'sha512-ZSPef2B6nBaYnfgdTbAy4jgW95o7pi2xPGwGCU+WMTxo7J6B1lMPTWwSq/wTuiMq+N0khQ90CcvYp1wFoQpo/w==',
    keep: (name) => /^package\/[0-9a-f-]+\.svg$/.test(name) || name === 'package/license',
  },
];

// Minimal ustar reader: regular files only, names up to 255 bytes (prefix + name).
function* untar(buf) {
  for (let off = 0; off + 512 <= buf.length; ) {
    const h = buf.subarray(off, off + 512);
    if (h.every((b) => b === 0)) return;
    const str = (a, b) => h.subarray(a, b).toString('utf8').replace(/\0.*$/s, '');
    const size = parseInt(str(124, 136).trim() || '0', 8);
    const prefix = str(345, 500);
    const name = (prefix ? prefix + '/' : '') + str(0, 100);
    const type = String.fromCharCode(h[156] || 48);
    if (type === '0' || type === '\0') yield { name, data: buf.subarray(off + 512, off + 512 + size) };
    off += 512 + Math.ceil(size / 512) * 512;
  }
}

for (const t of TARBALLS) {
  const dir = path.join(ASSETS, t.dir);
  const marker = path.join(dir, '.integrity');
  if (fs.existsSync(marker) && fs.readFileSync(marker, 'utf8') === t.integrity) continue;
  const r = await fetch(t.url);
  if (!r.ok) throw new Error(`${t.url}: HTTP ${r.status}`);
  const tgz = Buffer.from(await r.arrayBuffer());
  const [algo, want] = t.integrity.split('-');
  const got = crypto.createHash(algo).update(tgz).digest('base64');
  if (got !== want) throw new Error(`${t.url}: integrity mismatch`);
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  let n = 0;
  for (const e of untar(zlib.gunzipSync(tgz))) {
    if (!t.keep(e.name)) continue;
    fs.writeFileSync(path.join(dir, path.basename(e.name)), e.data);
    n++;
  }
  fs.writeFileSync(marker, t.integrity);
  console.log(`unpacked ${n} files into ${t.dir}/`);
}

const print = process.argv.includes('--print');
let bad = 0;
for (const f of FILES) {
  const p = path.join(ASSETS, f.out);
  if (!fs.existsSync(p)) {
    const r = await fetch(f.url);
    if (!r.ok) throw new Error(`${f.url}: HTTP ${r.status}`);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, Buffer.from(await r.arrayBuffer()));
    console.log(`fetched ${f.out}`);
  }
  const h = crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
  if (print) console.log(`${h}  ${f.out}`);
  else if (f.sha256 && h !== f.sha256) { console.error(`sha256 mismatch: ${f.out} (${h})`); bad++; }
  else if (!f.sha256) { console.error(`no pin for ${f.out}: run with --print and pin it`); bad++; }
}
if (bad) process.exit(1);
if (!print) console.log('assets ok');

#!/usr/bin/env node
// GPUI vs Electron, side by side:  node pairs.mjs [--gpui <dir>] [--electron <dir>] [--out <dir>]
//                                                  [--fuzz 2%]
// Pairs <screen>-<theme>.png from both dirs, crops each GPUI capture to its content area (a
// 1300 × 820 capture is a 1280 × 800 window plus a 10 px margin with a 32 px title bar, so the
// content is 1280x768+10+42; another size W × H crops (W−20)x(H−52)+10+42), and writes
// <out>/index.html with GPUI | Electron | difference per screen and theme, plus the number of
// pixels that differ by more than the fuzz (magick compare -metric AE). Self-contained: the images
// are copied under <out>, paths are relative. Zero npm deps; needs ImageMagick 7 (`magick`).
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const GPUI_ROOT = path.resolve(HERE, "../..");
const THEMES = ["nocturne", "matinee", "graphite", "graphite-light", "console", "console-light", "console-split"];

const opts = {
  gpui: path.join(GPUI_ROOT, "target-kit/captures"),
  electron: path.join(GPUI_ROOT, "target/electron-captures"),
  out: null,
  fuzz: "2%",
};
const argv = process.argv.slice(2);
for (let i = 0; i < argv.length; i++) {
  const [k, inline] = argv[i].split(/=(.*)/s);
  const v = inline ?? argv[++i];
  if (k === "--gpui") opts.gpui = path.resolve(v);
  else if (k === "--electron") opts.electron = path.resolve(v);
  else if (k === "--out") opts.out = path.resolve(v);
  else if (k === "--fuzz") opts.fuzz = v;
  else {
    console.error(`pairs.mjs: unknown option ${k}`);
    process.exit(2);
  }
}
opts.out ??= path.join(opts.electron, "pairs");

function magick(args) {
  const r = spawnSync("magick", args, { encoding: "utf8" });
  if (r.error) throw new Error(`magick: ${r.error.message}`);
  return r;
}
function size(file) {
  const r = magick(["identify", "-format", "%w %h", file]);
  if (r.status !== 0) throw new Error(`identify ${file}: ${r.stderr}`);
  const [w, h] = r.stdout.trim().split(" ").map(Number);
  return { w, h };
}

/** "<screen>-<theme>" → { screen, theme }; themes matched longest first (console-light before console). */
function parseName(base) {
  const theme = [...THEMES].sort((a, b) => b.length - a.length).find((t) => base.endsWith(`-${t}`));
  return theme ? { screen: base.slice(0, -theme.length - 1), theme } : null;
}
function listPngs(dir) {
  if (!fs.existsSync(dir)) return new Map();
  const m = new Map();
  for (const f of fs.readdirSync(dir)) {
    if (!f.endsWith(".png")) continue;
    const p = parseName(f.slice(0, -4));
    if (p) m.set(`${p.screen}-${p.theme}`, { ...p, file: path.join(dir, f) });
  }
  return m;
}

const gpui = listPngs(opts.gpui);
const electron = listPngs(opts.electron);
const keys = [...new Set([...gpui.keys(), ...electron.keys()])];
const meta = (k) => gpui.get(k) ?? electron.get(k);
keys.sort((a, b) => {
  const x = meta(a);
  const y = meta(b);
  return x.screen.localeCompare(y.screen) || THEMES.indexOf(x.theme) - THEMES.indexOf(y.theme);
});

for (const sub of ["gpui", "electron", "diff"]) {
  fs.rmSync(path.join(opts.out, sub), { recursive: true, force: true });
  fs.mkdirSync(path.join(opts.out, sub), { recursive: true });
}

const rows = [];
for (const k of keys) {
  const row = { key: k, ...meta(k), gpui: null, electron: null, diff: null, ae: null, total: null, note: "" };
  const g = gpui.get(k);
  const e = electron.get(k);
  if (g) {
    const { w, h } = size(g.file);
    const crop = w === 1300 && h === 820 ? "1280x768+10+42" : `${w - 20}x${h - 52}+10+42`;
    if (!(w === 1300 && h === 820)) row.note += `GPUI capture is ${w}×${h}: cropped ${crop}. `;
    row.gpui = `gpui/${k}.png`;
    const r = magick([g.file, "-crop", crop, "+repage", "-background", "black", "-alpha", "remove", "-alpha", "off", path.join(opts.out, row.gpui)]);
    if (r.status !== 0) throw new Error(`crop ${g.file}: ${r.stderr}`);
  }
  if (e) {
    row.electron = `electron/${k}.png`;
    // A page drawn as a whole window (`frame` in pages/_frame.tsx: a 32 px title bar band over
    // the content, so overlays place themselves against the whole window as in the app) is
    // cropped to its content area like the GPUI capture.
    const { w: ew, h: eh } = size(e.file);
    const json = e.file.replace(/\.png$/, ".json");
    const framed = fs.existsSync(json) && JSON.parse(fs.readFileSync(json, "utf8"))?.framed === true;
    const ecrop = framed ? ["-crop", `${ew}x${eh - 32}+0+32`, "+repage"] : [];
    const r = magick([e.file, ...ecrop, "-background", "black", "-alpha", "remove", "-alpha", "off", path.join(opts.out, row.electron)]);
    if (r.status !== 0) throw new Error(`copy ${e.file}: ${r.stderr}`);
  }
  if (g && e) {
    let a = path.join(opts.out, row.gpui);
    let b = path.join(opts.out, row.electron);
    const sa = size(a);
    const sb = size(b);
    if (sa.w !== sb.w || sa.h !== sb.h) {
      const w = Math.min(sa.w, sb.w);
      const h = Math.min(sa.h, sb.h);
      row.note += `Sizes differ (GPUI ${sa.w}×${sa.h}, Electron ${sb.w}×${sb.h}): compared the top-left ${w}×${h}. `;
      for (const [src, name] of [[a, "a"], [b, "b"]]) {
        const tmp = path.join(opts.out, "diff", `.${k}-${name}.png`);
        magick([src, "-crop", `${w}x${h}+0+0`, "+repage", tmp]);
        if (name === "a") a = tmp;
        else b = tmp;
      }
    }
    row.diff = `diff/${k}.png`;
    const r = magick(["compare", "-metric", "AE", "-fuzz", opts.fuzz, a, b, path.join(opts.out, row.diff)]);
    const m = /^\s*([\d.e+]+)/.exec(r.stderr);
    if (r.status > 1 || !m) row.note += `compare failed: ${r.stderr.trim()} `;
    else {
      row.ae = Math.round(Number(m[1]));
      const s = size(a);
      row.total = s.w * s.h;
    }
    for (const f of fs.readdirSync(path.join(opts.out, "diff"))) if (f.startsWith(".")) fs.rmSync(path.join(opts.out, "diff", f));
  } else if (!g) row.note += "No GPUI capture. ";
  else row.note += "No Electron capture. ";
  rows.push(row);
}

const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const pct = (r) => (r.ae == null ? "–" : `${((100 * r.ae) / r.total).toFixed(2)} %`);
const img = (src, label) =>
  src
    ? `<figure><figcaption>${label}</figcaption><a href="${esc(src)}"><img src="${esc(src)}" alt="${label}" loading="lazy"></a></figure>`
    : `<figure class="missing"><figcaption>${label}</figcaption><div>missing</div></figure>`;

const screens = [...new Set(rows.map((r) => r.screen))];
const summary = screens
  .map((s) => {
    const cells = THEMES.map((t) => {
      const r = rows.find((x) => x.screen === s && x.theme === t);
      return `<td>${r ? `<a href="#${esc(r.key)}">${r.ae == null ? (r.gpui ? "GPUI only" : "Electron only") : pct(r)}</a>` : ""}</td>`;
    }).join("");
    return `<tr><th>${esc(s)}</th>${cells}</tr>`;
  })
  .join("\n");
const sections = rows
  .map(
    (r) => `<section id="${esc(r.key)}">
  <h2>${esc(r.screen)} · ${esc(r.theme)} <span class="ae">${r.ae == null ? "" : `${r.ae.toLocaleString("en")} px differ (${pct(r)}, fuzz ${esc(opts.fuzz)})`}</span></h2>
  ${r.note ? `<p class="note">${esc(r.note.trim())}</p>` : ""}
  <div class="row">${img(r.gpui, "GPUI")}${img(r.electron, "Electron")}${img(r.diff, "Difference")}</div>
</section>`,
  )
  .join("\n");

const html = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>GPUI vs Electron</title>
<style>
  :root { color-scheme: light dark; --bg: #f4f4f6; --fg: #1d1d22; --muted: #6b6b76; --line: #d6d6de; --card: #fff; }
  @media (prefers-color-scheme: dark) { :root { --bg: #131317; --fg: #e6e6ea; --muted: #9a9aa6; --line: #2c2c34; --card: #1b1b21; } }
  body { margin: 0; padding: 16px; background: var(--bg); color: var(--fg); font: 14px/1.45 system-ui, sans-serif; }
  h1 { font-size: 18px; margin: 0 0 4px; }
  .sub { color: var(--muted); margin: 0 0 16px; }
  table { border-collapse: collapse; margin-bottom: 24px; }
  th, td { border: 1px solid var(--line); padding: 4px 8px; text-align: left; font-variant-numeric: tabular-nums; }
  thead th { color: var(--muted); font-weight: 500; }
  section { background: var(--card); border: 1px solid var(--line); border-radius: 6px; padding: 12px; margin-bottom: 16px; }
  h2 { font-size: 15px; margin: 0 0 8px; }
  .ae { color: var(--muted); font-weight: 400; margin-left: 8px; }
  .note { color: var(--muted); margin: 0 0 8px; }
  .row { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; }
  figure { margin: 0; }
  figcaption { color: var(--muted); font-size: 12px; margin-bottom: 4px; }
  img { display: block; width: 100%; height: auto; border: 1px solid var(--line); }
  .missing div { aspect-ratio: 1280 / 768; display: grid; place-items: center; border: 1px dashed var(--line); color: var(--muted); }
  @media (max-width: 900px) { .row { grid-template-columns: 1fr; } }
</style>
</head>
<body>
<h1>GPUI vs Electron captures</h1>
<p class="sub">Content area only (GPUI crop ${"1280×768+10+42"}). GPUI: ${esc(opts.gpui)} (${gpui.size} PNGs) · Electron: ${esc(opts.electron)} (${electron.size} PNGs) · difference = pixels off by more than ${esc(opts.fuzz)} (red). Click an image for full size.</p>
<table><thead><tr><th>screen</th>${THEMES.map((t) => `<th>${t}</th>`).join("")}</tr></thead><tbody>
${summary}
</tbody></table>
${sections || "<p>No captures found.</p>"}
</body>
</html>
`;
fs.writeFileSync(path.join(opts.out, "index.html"), html);
console.log(`pairs: ${rows.length} entries (${rows.filter((r) => r.ae != null).length} compared) → ${path.join(opts.out, "index.html")}`);
if (!fs.existsSync(opts.gpui)) console.log(`pairs: no GPUI captures at ${opts.gpui} (Electron only)`);

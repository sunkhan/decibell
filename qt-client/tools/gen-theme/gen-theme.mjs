#!/usr/bin/env node
// Generates the Qt client's theme from the Electron renderer's design tokens, so the two never drift.
//
//   node qt-client/tools/gen-theme/gen-theme.mjs           write qt-client/qml/Decibell/Theme/{ThemePalette,Palettes}.qml
//   node qt-client/tools/gen-theme/gen-theme.mjs --check   exit 1 if the committed files are stale (verification bar)
//
// Source: electron-client/src/styles/globals.css. Model:
//   - Every custom property declared in `@theme`, bare `:root`, `:root[data-theme="X"]` (comma lists included)
//     is resolved per theme as the browser would: bare `:root` (specificity 0,1,0) loses to
//     `:root[data-theme=…]` (0,2,0) whatever the order; equal specificity → later wins. `@theme` is Tailwind's
//     theme layer, below everything unlayered.
//   - `var(--x, fallback)` chains are substituted per theme. Values become typed QML properties: colours
//     (#AARRGGBB), px lengths and unitless numbers (real), em (real, suffix Em), % (fraction, suffix Frac), font
//     stacks (the first family we bundle), shadows (a list of layers), weights.
//   - `calc()` / `round()` that depend on the user's text size (`--ui-text-size-n`, set at runtime by
//     Settings → Appearance) become QML bindings over other generated properties and `uiTextSizeN`.
//   - `.chrome-scope` (+ `:root[data-theme="X"] .chrome-scope`) re-points `--color-*` at `--tc-*` chrome roles
//     (console-split's dark chrome): each theme gets a `chrome` palette, the same object as the base one
//     unless something differs.
//   - Component-local scopes (`.image-viewer`, `.picture-editor`, `.list-row`, `.dsel-menu…`, `.theme-preview`)
//     are emitted as small per-theme maps in `scopes`; their components read them.
// Property names are the CSS names camel-cased (`--color-bg-mid` → colorBgMid), so a token greps both ways.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const CSS = path.join(repo, 'electron-client/src/styles/globals.css');
const OUT = path.join(repo, 'qt-client/qml/Decibell/Theme');
const THEMES = ['nocturne', 'matinee', 'graphite', 'graphite-light', 'console', 'console-light', 'console-split'];
// CSS family name → the family we bundle (qt-client/assets/fonts). First match in a stack wins.
const BUNDLED = {
  'Mona Sans Variable': 'Mona Sans', 'Mona Sans': 'Mona Sans',
  'Inter Variable': 'Inter', 'Inter': 'Inter',
  'JetBrains Mono': 'JetBrains Mono',
};
const RUNTIME = { '--ui-text-size-n': 'uiTextSizeN', '--ui-row-scale': 'uiRowScale' };

// ── parse ────────────────────────────────────────────────────────────────────────────────────────────
const src = fs.readFileSync(CSS, 'utf8').replace(/\/\*[\s\S]*?\*\//g, '');
const blocks = []; // { selectors: string[], decls: [name, value][], order, layer }
{
  const stack = []; let start = 0, order = 0;
  for (let k = 0; k < src.length; k++) {
    const c = src[k];
    if (c === '{') { stack.push(src.slice(start, k).trim()); start = k + 1; }
    else if (c === '}') {
      const sel = stack.pop(); const body = src.slice(start, k); start = k + 1;
      const decls = [...body.matchAll(/(--[a-z0-9-]+)\s*:\s*([^;]+);/g)].map((m) => [m[1], m[2].trim()]);
      if (!decls.length || stack.length) continue; // nested (@layer/@media) blocks hold component rules only
      blocks.push({ selectors: sel.split(',').map((s) => s.trim().replace(/\s+/g, ' ')), decls, order: order++ });
    } else if (c === ';' && stack.length === 0) start = k + 1;
  }
}

// selector → { theme: null | name | prefix, scope: null | class }
function classify(sel) {
  if (sel === '@theme') return { theme: '*', scope: null, spec: 0 };
  let m;
  if (sel === ':root') return { theme: '*', scope: null, spec: 1 };
  if ((m = sel.match(/^:root\[data-theme="([a-z-]+)"\]$/))) return { theme: m[1], scope: null, spec: 2 };
  if ((m = sel.match(/^\.([a-z-]+)(\[.*\])?$/))) return { theme: '*', scope: m[1] + (m[2] ?? ''), spec: 1 };
  if ((m = sel.match(/^:root\[data-theme="([a-z-]+)"\] \.([a-z-]+)$/))) return { theme: m[1], scope: m[2], spec: 3 };
  throw new Error(`unhandled selector for custom properties: ${sel}`);
}

// Declarations that apply to (theme, scope) in cascade order.
function cascade(theme, scope) {
  const rules = [];
  for (const b of blocks) {
    let best = -1;
    for (const s of b.selectors) {
      const c = classify(s);
      if (c.theme !== '*' && c.theme !== theme) continue;
      if (c.scope && c.scope !== scope) continue;
      // scoped rules sit above unscoped ones (the element is inside the scope)
      best = Math.max(best, c.spec + (c.scope ? 10 : 0));
    }
    if (best >= 0) rules.push({ spec: best, order: b.order, decls: b.decls });
  }
  rules.sort((a, b) => a.spec - b.spec || a.order - b.order);
  const vars = new Map();
  for (const r of rules) for (const [n, v] of r.decls) vars.set(n, v);
  return vars;
}

// ── resolve ──────────────────────────────────────────────────────────────────────────────────────────
// Substitute var() recursively. Runtime vars stay as markers; a reference to one makes the value a binding.
function substitute(vars, value, seen = new Set()) {
  let out = '', i = 0;
  while (i < value.length) {
    const j = value.indexOf('var(', i);
    if (j < 0) { out += value.slice(i); break; }
    out += value.slice(i, j);
    let depth = 0, k = j + 3;
    for (; k < value.length; k++) { if (value[k] === '(') depth++; else if (value[k] === ')' && --depth === 0) break; }
    const inner = value.slice(j + 4, k);
    const comma = topLevelComma(inner);
    const name = (comma < 0 ? inner : inner.slice(0, comma)).trim();
    const fallback = comma < 0 ? null : inner.slice(comma + 1).trim();
    if (RUNTIME[name]) out += `@@${name}@@${fallback !== null ? `(${substitute(vars, fallback, seen)})` : ''}`;
    else if (vars.has(name) && !seen.has(name)) out += substitute(vars, vars.get(name), new Set([...seen, name]));
    else if (fallback !== null) out += substitute(vars, fallback, seen);
    else out += `@@UNRESOLVED${name}@@`;
    i = k + 1;
  }
  return out;
}
function topLevelComma(s) { let d = 0; for (let i = 0; i < s.length; i++) { if (s[i] === '(') d++; else if (s[i] === ')') d--; else if (s[i] === ',' && d === 0) return i; } return -1; }

// `--text-reply--line-height` (Tailwind's double-hyphen modifier) → textReplyLineHeight
const camel = (n) => n.replace(/^--/, '').replace(/-+([a-z0-9])/g, (_, c) => c.toUpperCase());
const hex2 = (x) => Math.round(x).toString(16).padStart(2, '0');

function parseColor(v) {
  v = v.trim();
  let m;
  if ((m = v.match(/^#([0-9a-f]{3})$/i))) return '#' + [...m[1]].map((c) => c + c).join('').toLowerCase();
  if ((m = v.match(/^#([0-9a-f]{6})$/i))) return '#' + m[1].toLowerCase();
  if ((m = v.match(/^#([0-9a-f]{6})([0-9a-f]{2})$/i))) return '#' + m[2].toLowerCase() + m[1].toLowerCase();
  if ((m = v.match(/^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)\s*(?:[,/]\s*([\d.]+%?))?\s*\)$/))) {
    let a = m[4] === undefined ? 1 : m[4].endsWith('%') ? parseFloat(m[4]) / 100 : parseFloat(m[4]);
    const rgb = hex2(+m[1]) + hex2(+m[2]) + hex2(+m[3]);
    return a >= 1 ? '#' + rgb : '#' + hex2(a * 255) + rgb;
  }
  if (v === 'transparent') return '#00000000';
  if (v === 'white') return '#ffffff';
  if (v === 'black') return '#000000';
  if ((m = v.match(/^color-mix\(\s*in srgb\s*,(.*)\)$/))) return colorMix(m[1]);
  return null;
}
// color-mix(in srgb, A p%, B [q%]): CSS Color 5 — premultiplied sRGB interpolation, missing % = the rest.
function colorMix(args) {
  const [a, b] = splitTop(args, ',').map((x) => { const mm = x.match(/^(.*?)\s+([\d.]+)%$/); return mm ? { c: mm[1], p: +mm[2] / 100 } : { c: x, p: null }; });
  if (!a || !b) return null;
  let pa = a.p, pb = b.p;
  if (pa === null && pb === null) pa = pb = 0.5; else if (pa === null) pa = 1 - pb; else if (pb === null) pb = 1 - pa;
  const ca = rgba(parseColor(a.c)), cb = rgba(parseColor(b.c));
  if (!ca || !cb) return null;
  const sum = pa + pb; pa /= sum; pb /= sum;
  const alpha = ca[3] * pa + cb[3] * pb;
  if (alpha === 0) return '#00000000';
  const ch = (i) => (ca[i] * ca[3] * pa + cb[i] * cb[3] * pb) / alpha;
  const rgb = hex2(ch(0)) + hex2(ch(1)) + hex2(ch(2));
  return alpha >= 1 ? '#' + rgb : '#' + hex2(alpha * 255) + rgb;
}
function rgba(hex) {
  if (!hex) return null;
  const h = hex.slice(1);
  return h.length === 8
    ? [parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16), parseInt(h.slice(6, 8), 16), parseInt(h.slice(0, 2), 16) / 255]
    : [parseInt(h.slice(0, 2), 16), parseInt(h.slice(2, 4), 16), parseInt(h.slice(4, 6), 16), 1];
}
function splitTop(s, sep) { const out = []; let d = 0, last = 0; for (let i = 0; i < s.length; i++) { if (s[i] === '(') d++; else if (s[i] === ')') d--; else if (s[i] === sep && d === 0) { out.push(s.slice(last, i)); last = i + 1; } } out.push(s.slice(last)); return out.map((x) => x.trim()).filter(Boolean); }
function parseShadow(v) {
  if (v === 'none') return [];
  const layers = [];
  for (const layer of splitTop(v, ',')) {
    const parts = splitTop(layer.replace(/\s+/g, ' '), ' ');
    let inset = false, color = null; const lens = [];
    for (const p of parts) {
      if (p === 'inset') inset = true;
      else if (/^-?[\d.]+(px)?$/.test(p)) lens.push(parseFloat(p));
      else { const c = parseColor(p); if (!c) return null; color = c; }
    }
    if (lens.length < 2 || !color) return null;
    layers.push({ x: lens[0], y: lens[1], blur: lens[2] ?? 0, spread: lens[3] ?? 0, color, inset });
  }
  return layers;
}
const STRETCH = { normal: 100, 'semi-condensed': 87.5, condensed: 75, 'semi-expanded': 112.5, expanded: 125, 'extra-expanded': 150 };

// calc()/round() over px/unitless numbers and runtime vars → a JS expression (or a number if constant).
function calcExpr(v, own) {
  let e = v.trim();
  const runtimeRefs = [];
  e = e.replace(/@@(--[a-z0-9-]+)@@(\((?:[^()]|\([^()]*\))*\))?/g, (_, name, fb) => {
    runtimeRefs.push(name);
    const id = RUNTIME[name];
    return fb ? `(${id} > 0 ? ${id} : ${fb})` : id;
  });
  if (/@@UNRESOLVED/.test(e)) return null;
  e = e.replace(/round\(\s*up\s*,/g, 'Math.ceil(').replace(/round\(\s*down\s*,/g, 'Math.floor(').replace(/round\(/g, 'Math.round(');
  e = e.replace(/,\s*1px\s*\)/g, ')');
  e = e.replace(/calc\(/g, '(');
  e = e.replace(/(-?[\d.]+)px/g, '$1');
  if (/[^\s\d.+\-*/()?:<>=a-zA-Z_]/.test(e) || /\b(?!Math\b|ceil\b|floor\b|round\b|uiTextSizeN\b|uiRowScale\b)[a-zA-Z_]\w*/.test(e)) return null;
  if (!runtimeRefs.length) { try { return { kind: 'real', value: Function(`return (${e});`)() }; } catch { return null; } }
  return { kind: 'binding', expr: e };
}

function typed(name, raw) {
  const v = raw.trim();
  if (/@@UNRESOLVED/.test(v)) return null;
  if (name.includes('shadow')) { const s = parseShadow(v); if (s) return { kind: 'shadow', value: s }; }
  const c = parseColor(v); if (c) return { kind: 'color', value: c };
  let m;
  if ((m = v.match(/^(-?[\d.]+)px$/))) return { kind: 'real', value: +m[1] };
  if ((m = v.match(/^(-?[\d.]+)em$/))) return { kind: 'real', value: +m[1], suffix: 'Em' };
  if ((m = v.match(/^(-?[\d.]+)%$/))) return { kind: 'real', value: +m[1] / 100, suffix: 'Frac' };
  if ((m = v.match(/^(-?[\d.]+)(ms|s)$/))) return { kind: 'real', value: m[2] === 's' ? +m[1] * 1000 : +m[1], suffix: 'Ms' };
  if ((m = v.match(/^-?[\d.]+$/))) return { kind: 'real', value: +v };
  if (/stretch/.test(name) && STRETCH[v] !== undefined) return { kind: 'real', value: STRETCH[v], suffix: 'Wdth' };
  if (/^font-stack|^t-font|^tc-font|^font-/.test(name.replace(/^--/, '')) && v.includes(',') || /"/.test(v)) {
    const fams = splitTop(v, ',').map((f) => f.replace(/^["']|["']$/g, ''));
    const bundled = fams.map((f) => BUNDLED[f]).find(Boolean);
    return { kind: 'font', value: bundled ?? fams[0], stack: fams };
  }
  if (/calc\(|round\(|@@--/.test(v)) return calcExpr(v);
  if (/^[a-z-]+$/.test(v)) return { kind: 'string', value: v };
  return null;
}

// ── build palettes ───────────────────────────────────────────────────────────────────────────────────
const baseNames = new Set(); const scopeNames = new Set();
for (const b of blocks) for (const s of b.selectors) { const c = classify(s); for (const [n] of b.decls) (c.scope ? scopeNames : baseNames).add(c.scope ?? n); }

function resolveAll(theme, scope, names) {
  const vars = cascade(theme, scope);
  const out = new Map();
  // Runtime variables (the user's text size / row scale) come from ThemeSettings, never from a palette.
  for (const n of names) { if (!vars.has(n) || RUNTIME[n]) continue; const t = typed(n, substitute(vars, vars.get(n))); out.set(n, t); }
  return out;
}

const props = new Map(); // qml name → kind (stable across themes)
const skipped = new Map();
const palettes = {}; const chromes = {}; const scopes = {};
for (const theme of THEMES) {
  const base = resolveAll(theme, null, baseNames);
  palettes[theme] = base;
  const chromeScope = resolveAll(theme, 'chrome-scope', baseNames);
  const differs = [...chromeScope].some(([n, t]) => JSON.stringify(t) !== JSON.stringify(base.get(n)));
  chromes[theme] = differs ? chromeScope : null;
  scopes[theme] = {};
  for (const sc of scopeNames) {
    if (sc === 'chrome-scope') continue;
    const own = new Set(); for (const b of blocks) for (const s of b.selectors) { const c = classify(s); if (c.scope === sc) for (const [n] of b.decls) own.add(n); }
    const vals = resolveAll(theme, sc, own); const map = {};
    for (const [n, t] of vals) {
      if (t && t.kind !== 'binding') map[camel(n) + (t.suffix ?? '')] = t.value;
      else skipped.set(`${n} (.${sc})`, t ? 'runtime binding' : 'unresolved');
    }
    scopes[theme][camel(sc.replace(/\[.*$/, '')) + (sc.includes('[') ? 'Above' : '')] = map;
  }
  for (const pal of [base, chromes[theme]].filter(Boolean)) for (const [n, t] of pal) {
    if (!t) { skipped.set(n, pal === base ? substitute(cascade(theme, null), cascade(theme, null).get(n)) : '(chrome)'); continue; }
    const key = camel(n) + (t.suffix ?? '');
    const prev = props.get(key);
    if (prev && prev !== t.kind) throw new Error(`${n}: kind ${t.kind} in ${theme}, ${prev} elsewhere`);
    props.set(key, t.kind);
  }
}

// ── emit ─────────────────────────────────────────────────────────────────────────────────────────────
const HEADER = '// Generated by qt-client/tools/gen-theme/gen-theme.mjs from electron-client/src/styles/globals.css.\n// Do not edit: change globals.css and regenerate (the verification bar checks it is current).\n';
const QTYPE = { color: 'color', real: 'real', font: 'string', string: 'string', shadow: 'var', binding: 'real' };
const keys = [...props.keys()].sort();

function lit(t) {
  if (t.kind === 'color' || t.kind === 'font' || t.kind === 'string') return JSON.stringify(t.value);
  if (t.kind === 'real') return String(+t.value.toFixed(6));
  if (t.kind === 'shadow') return '[' + t.value.map((l) => `{ x: ${l.x}, y: ${l.y}, blur: ${l.blur}, spread: ${l.spread}, color: "${l.color}", inset: ${l.inset} }`).join(', ') + ']';
  if (t.kind === 'binding') return t.expr.replace(/@@--([a-z0-9-]+)@@/g, (_, n) => camel('--' + n));
}

let palette = HEADER + 'import QtQuick\n\n// Every design token of one theme, resolved. Bindings (the type scale) follow the user\'s text size.\nQtObject {\n    property string name\n    // Settings → Appearance (ThemeSettings); 0 = the theme\'s own body size, as an unset --ui-text-size-n.\n    property real uiTextSizeN: ThemeSettings.textSizePx\n    property real uiRowScale: ThemeSettings.rowScale\n';
for (const k of keys) palette += `    property ${QTYPE[props.get(k)]} ${k}\n`;
palette += '}\n';

function instance(theme, pal, indent) {
  const pad = ' '.repeat(indent);
  let s = `ThemePalette {\n${pad}    name: "${theme}"\n`;
  for (const [n, t] of [...pal].sort(([a], [b]) => (camel(a) < camel(b) ? -1 : 1))) {
    if (!t) continue;
    const key = camel(n) + (t.suffix ?? '');
    let v = lit(t);
    // A binding refers to other tokens of this palette by name; qualify them through the instance.
    if (t.kind === 'binding') v = v.replace(/\b([a-z][A-Za-z0-9]*)\b/g, (w) => (props.has(w) ? `this.${w}` : w));
    s += `${pad}    ${key}: ${v}\n`;
  }
  return s + `${pad}}`;
}

let pals = HEADER + 'pragma Singleton\nimport QtQuick\n\n// One ThemePalette per theme (and a chrome palette where `.chrome-scope` differs), plus each theme\'s\n// component-local scopes. Theme.qml picks the active ones.\nQtObject {\n';
for (const theme of THEMES) {
  const id = camel('--' + theme);
  // <theme>Palette, not <theme>: `console` is the JS global and can't be a QML property name.
  pals += `    readonly property ThemePalette ${id}Palette: ${instance(theme, palettes[theme], 4)}\n`;
  pals += chromes[theme] ? `    readonly property ThemePalette ${id}Chrome: ${instance(theme, chromes[theme], 4)}\n` : `    readonly property ThemePalette ${id}Chrome: ${id}Palette\n`;
  pals += `    readonly property var ${id}Scopes: (${JSON.stringify(scopes[theme])})\n\n`;
}
pals += '    readonly property list<string> names: [' + THEMES.map((t) => `"${t}"`).join(', ') + ']\n\n';
for (const [fn, suffix] of [['palette', 'Palette'], ['chrome', 'Chrome'], ['scopes', 'Scopes']]) {
  pals += `    function ${fn}(theme: string)${suffix === 'Scopes' ? ': var' : ': ThemePalette'} {\n        switch (theme) {\n`;
  for (const theme of THEMES) pals += `        case "${theme}": return ${camel('--' + theme)}${suffix}\n`;
  pals += `        default: return nocturne${suffix}\n        }\n    }\n`;
}
pals += '}\n';

const files = { 'ThemePalette.qml': palette, 'Palettes.qml': pals };
const check = process.argv.includes('--check');
let stale = false;
for (const [f, body] of Object.entries(files)) {
  const p = path.join(OUT, f);
  const cur = fs.existsSync(p) ? fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n') : null;
  if (cur !== body) { stale = true; if (!check) { fs.mkdirSync(OUT, { recursive: true }); fs.writeFileSync(p, body); } }
}
if (skipped.size) console.error(`not representable (left out): ${[...skipped.keys()].join(', ')}`);
console.log(`${THEMES.length} themes, ${keys.length} tokens${check ? (stale ? ': STALE' : ': current') : ''}`);
if (check && stale) process.exit(1);

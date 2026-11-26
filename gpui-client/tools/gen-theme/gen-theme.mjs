#!/usr/bin/env node
// Generates the native client's design tokens from the Electron renderer's CSS, so the two never drift.
//
//   node gpui-client/tools/gen-theme/gen-theme.mjs           write the outputs below
//   node gpui-client/tools/gen-theme/gen-theme.mjs --check   exit 1 if a committed output is stale (check.sh)
//   node gpui-client/tools/gen-theme/gen-theme.mjs --report  also print token counts and per-theme diffs
//
// Inputs (read only):
//   electron-client/src/styles/globals.css                    every token, the seven themes, the scopes
//   electron-client/node_modules/tailwindcss/theme.css        Tailwind's default theme (spacing, text-sm, …),
//                                                             checked against the version package-lock pins
//   electron-client/src/stores/uiStore.ts                     theme ids, text-size / density ranges + defaults
//   electron-client/src/features/settings/tabs/AppearanceTab.tsx  display names, Dark/Light, preview swatches,
//                                                             the density slider's step
// Outputs:
//   gpui-client/crates/decibell-theme/src/generated.rs        Palette + 7 statics, chrome palettes, Scaled +
//                                                             scaled(), scopes, Tailwind defaults, settings consts
//   gpui-client/crates/decibell-theme/src/generated_tests.rs  scaled() fixture, evaluated here from the CSS text
//   gpui-client/docs/css-component-rules.md                   every globals.css rule that is not a token
//
// The CSS model (what a browser does, reproduced):
//   - Custom properties are resolved per element as computed values: a declaration's var()s are substituted at
//     the element that declares it, and an element that declares nothing inherits its parent's computed value.
//     The theme root is <html data-theme=X>; scopes are elements directly under it (they are all portaled to
//     <body> or sit in the main tree) carrying their classes, e.g. the image viewer is `.image-viewer
//     .chrome-scope` on one element, so its stage colour sees the chrome palette.
//   - Cascade: Tailwind's `@theme default` < our `@theme` (both `@layer theme`) < unlayered rules; within a
//     layer, specificity then source order. `:root` is (0,1,0), `:root[data-theme="X"]` (0,2,0), `^=` prefix
//     selectors and comma lists match per selector. Inline styles (the runtime knobs uiStore writes on <html>,
//     the --p-* the theme-preview cards set) beat everything.
//   - Tailwind compiles the `shadow-*` utilities to `--tw-shadow: var(--t-shadow-*)` (it inlines the @theme
//     value instead of reading --shadow-*), so a scope's `shadow_*` is `--t-shadow-*` as resolved inside it.
//   - `--ui-text-size-n` / `--ui-row-scale` (Settings → Appearance) are runtime: tokens that reach them become
//     `Scaled`, computed by a generated `scaled()` that reproduces the calc()/round() arithmetic in f64.
// Nothing is guessed: an unknown selector shape, colour syntax, unit, value kind or a token missing from a
// theme stops the generator with the token's name.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const GPUI = path.resolve(here, '../..');
const REPO = path.resolve(GPUI, '..');
const rel = (p) => path.relative(REPO, p).split(path.sep).join('/');
const IN = {
  css: path.join(REPO, 'electron-client/src/styles/globals.css'),
  twTheme: path.join(REPO, 'electron-client/node_modules/tailwindcss/theme.css'),
  twPkg: path.join(REPO, 'electron-client/node_modules/tailwindcss/package.json'),
  lock: path.join(REPO, 'electron-client/package-lock.json'),
  uiStore: path.join(REPO, 'electron-client/src/stores/uiStore.ts'),
  appearance: path.join(REPO, 'electron-client/src/features/settings/tabs/AppearanceTab.tsx'),
};
const OUT = {
  rs: path.join(GPUI, 'crates/decibell-theme/src/generated.rs'),
  tests: path.join(GPUI, 'crates/decibell-theme/src/generated_tests.rs'),
  doc: path.join(GPUI, 'docs/css-component-rules.md'),
};
const read = (p) => fs.readFileSync(p, 'utf8').replace(/\r\n/g, '\n');
const fail = (msg) => { console.error(`gen-theme: ${msg}`); process.exit(2); };

// CSS family (as the renderer's @font-face rules name it) → the family name of the font file the native
// client bundles (gpui-client/assets/fonts). A stack resolves to its first family, which must be one of these.
const BUNDLED = {
  'Mona Sans Variable': 'Mona Sans',
  'Inter Variable': 'Inter',
  'JetBrains Mono': 'JetBrains Mono',
};
// The knobs uiStore.applyUiScale writes as inline styles on <html> → the parameter of scaled().
const RUNTIME = { '--ui-text-size-n': 'text_size_n', '--ui-row-scale': 'row_scale' };

// Scopes: elements under the theme root whose classes re-declare custom properties. `own` is the class whose
// declarations become the scope's fields; `classes` is everything on that element (as the TSX writes it).
const SCOPES = [
  { key: 'chrome', classes: ['chrome-scope'], palette: true,
    where: 'Titlebar, ServerBar, the channel/DM sidebars group in MainLayout (console-split\'s dark chrome)' },
  { key: 'image_viewer', own: 'image-viewer', classes: ['image-viewer', 'chrome-scope'],
    where: 'features/chat/ImageViewer.tsx: `image-viewer chrome-scope` on one element, portaled to the document body' },
  { key: 'picture_editor', own: 'picture-editor', classes: ['picture-editor'],
    where: 'components/pictureEditor/PictureEditor.tsx, portaled to the document body' },
  { key: 'dsel_menu_above', own: 'dsel-menu', classes: ['dsel-menu'], attrs: { 'data-side': 'above' },
    where: 'components/Select.tsx: the menu when it opens above its trigger (below: --dsel-dy unset, the keyframe falls back to -4px)' },
  { key: 'list_row', own: 'list-row', classes: ['list-row'],
    where: 'list rows (members, DMs, channels); only runtime tokens, see `Scaled`' },
  { key: 'theme_preview', own: 'theme-preview', classes: ['theme-preview'], preview: true,
    where: 'Settings → Appearance theme cards: each card paints its own theme from the --p-* it sets inline (AppearanceTab.tsx THEMES[].preview)' },
];

// ── CSS parsing (positions kept for the component-rule listing) ──────────────────────────────────────
function parseCss(text) {
  const src = text.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, ' '));
  const lineStarts = [0];
  for (let i = 0; i < src.length; i++) if (src[i] === '\n') lineStarts.push(i + 1);
  const lineOf = (off) => { let lo = 0, hi = lineStarts.length - 1; while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (lineStarts[mid] <= off) lo = mid; else hi = mid - 1; } return lo + 1; };
  const root = { prelude: null, children: [], decls: [], parent: null };
  const stack = [root];
  let seg = 0, quote = null, paren = 0;
  const skipWs = (k) => { while (k < src.length && /\s/.test(src[k])) k++; return k; };
  const pushDecl = (from, to) => {
    const t = src.slice(from, to).trim();
    if (!t) return;
    const top = stack[stack.length - 1];
    const off = skipWs(from);
    const c = t.indexOf(':');
    if (t.startsWith('@') || c < 0) { top.decls.push({ statement: t, line: lineOf(off) }); return; }
    let value = t.slice(c + 1).trim().replace(/\s+/g, ' ');
    let important = false;
    if (/!\s*important$/.test(value)) { important = true; value = value.replace(/\s*!\s*important$/, ''); }
    top.decls.push({ name: t.slice(0, c).trim(), value, important, line: lineOf(off) });
  };
  for (let i = 0; i < src.length; i++) {
    const c = src[i];
    if (quote) { if (c === '\\') i++; else if (c === quote) quote = null; continue; }
    if (c === '"' || c === "'") { quote = c; continue; }
    if (c === '(') paren++;
    else if (c === ')') paren--;
    else if (paren === 0 && c === '{') {
      const start = skipWs(seg);
      const node = { prelude: src.slice(seg, i).trim().replace(/\s+/g, ' '), children: [], decls: [], parent: stack[stack.length - 1], startLine: lineOf(start) };
      stack[stack.length - 1].children.push(node);
      stack.push(node);
      seg = i + 1;
    } else if (paren === 0 && c === ';') { pushDecl(seg, i); seg = i + 1; }
    else if (paren === 0 && c === '}') {
      pushDecl(seg, i);
      const node = stack.pop();
      if (!node.parent) fail('unbalanced } in CSS');
      node.endLine = lineOf(i);
      seg = i + 1;
    }
  }
  if (stack.length !== 1) fail('unbalanced { in CSS');
  return root;
}

function splitTop(s, sep) {
  const out = []; let d = 0, last = 0, q = null;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (q) { if (c === q) q = null; continue; }
    if (c === '"' || c === "'") q = c;
    else if (c === '(') d++;
    else if (c === ')') d--;
    else if (d === 0 && (sep === ' ' ? /\s/.test(c) : c === sep)) { out.push(s.slice(last, i)); last = i + 1; }
  }
  out.push(s.slice(last));
  return out.map((x) => x.trim()).filter(Boolean);
}

// ── selectors (token blocks only) ────────────────────────────────────────────────────────────────────
function parseCompound(s, ctx) {
  const comp = { root: false, classes: [], attrs: [] };
  let rest = s;
  while (rest.length) {
    let m;
    if ((m = rest.match(/^:root/))) comp.root = true;
    else if ((m = rest.match(/^\.([A-Za-z0-9_-]+)/))) comp.classes.push(m[1]);
    else if ((m = rest.match(/^\[([a-z-]+)(?:([\^]?=)"([^"]*)")?\]/))) comp.attrs.push({ name: m[1], op: m[2] ?? null, value: m[3] ?? null });
    else fail(`unhandled selector in a custom-property block (${ctx}): "${s}"`);
    rest = rest.slice(m[0].length);
  }
  return comp;
}
function parseSelector(sel, ctx) {
  if (/[>+~]/.test(sel)) fail(`unhandled combinator in a custom-property block (${ctx}): "${sel}"`);
  const comps = sel.split(' ').map((c) => parseCompound(c, ctx));
  if (comps.length > 2 || (comps.length === 2 && !comps[0].root)) fail(`unhandled selector depth (${ctx}): "${sel}"`);
  const spec = comps.reduce((n, c) => n + (c.root ? 1 : 0) + c.classes.length + c.attrs.length, 0);
  return { comps, spec };
}
const attrMatch = (a, attrs) => {
  const v = attrs[a.name];
  if (v === undefined) return false;
  if (a.op === null) return true;
  return a.op === '=' ? v === a.value : v.startsWith(a.value);
};
const matchRoot = (c, theme) => c.root && !c.classes.length && c.attrs.every((a) => attrMatch(a, { 'data-theme': theme }));
const matchEl = (c, el) => !c.root && c.classes.every((k) => el.classes.includes(k)) && c.attrs.every((a) => attrMatch(a, el.attrs ?? {}));
// → specificity if `sel` matches (theme root | element under it), else -1. `own` restricts to the scope's class.
function selMatch(sel, theme, el, own) {
  const { comps, spec } = sel;
  const last = comps[comps.length - 1];
  if (!el) return comps.length === 1 && matchRoot(last, theme) ? spec : -1;
  if (!matchEl(last, el)) return -1;
  if (own && !last.classes.includes(own)) return -1;
  if (comps.length === 2 && !matchRoot(comps[0], theme)) return -1;
  return spec;
}

// ── token blocks ─────────────────────────────────────────────────────────────────────────────────────
// layer: 0 Tailwind `@theme default`, 1 our `@theme`, 2 unlayered. Higher wins, then specificity, then order.
let ORDER = 0;
function tokenBlocks(tree, layerOf, file) {
  const out = [];
  const visit = (node, depth) => {
    for (const ch of node.children) {
      const custom = ch.decls.filter((d) => d.name?.startsWith('--'));
      if (custom.length && depth > 0) fail(`${file}:${ch.startLine}: custom properties inside a nested block (${ch.parent.prelude} › ${ch.prelude}) are not modelled`);
      if (custom.length) {
        for (const d of custom) if (d.important) fail(`${file}:${d.line}: !important on ${d.name} is not modelled`);
        const layer = layerOf(ch.prelude);
        const selectors = ch.prelude.startsWith('@theme') ? [parseSelector(':root', ch.prelude)] : splitTop(ch.prelude, ',').map((s) => parseSelector(s, `${file}:${ch.startLine}`));
        out.push({ layer, selectors, decls: custom, order: ORDER++, node: ch, file });
      }
      visit(ch, depth + 1);
    }
  };
  visit(tree, 0);
  return out;
}

// Declarations that apply to the root (el = null) or an element, in cascade order (last wins).
function declared(blocks, theme, el, own = null) {
  const hits = [];
  for (const b of blocks) {
    let best = -1;
    for (const s of b.selectors) best = Math.max(best, selMatch(s, theme, el, own));
    if (best >= 0) hits.push({ layer: b.layer, spec: best, order: b.order, decls: b.decls });
  }
  hits.sort((a, b) => a.layer - b.layer || a.spec - b.spec || a.order - b.order);
  const m = new Map();
  for (const h of hits) for (const d of h.decls) m.set(d.name, d.value);
  return m;
}

// ── computed values ──────────────────────────────────────────────────────────────────────────────────
// A computed value is the declaration's text with every var() substituted; a runtime knob stays as
// `@@R<name>@@`. `inherited` is the parent's computed map (null at the root).
const R = (name) => `@@R${name}@@`;
function computeElement(decl, inherited, runtimeInline) {
  const memo = new Map(), visiting = new Set();
  const get = (name) => {
    if (runtimeInline && RUNTIME[name]) return R(name);
    if (!decl.has(name)) return inherited ? inherited.get(name) : undefined;
    if (memo.has(name)) return memo.get(name);
    if (visiting.has(name)) return undefined; // a cycle makes the property guaranteed-invalid
    visiting.add(name);
    const v = substitute(decl.get(name), get);
    visiting.delete(name);
    memo.set(name, v);
    return v;
  };
  const out = new Map(inherited ?? []);
  for (const n of decl.keys()) { const v = get(n); if (v === undefined) out.delete(n); else out.set(n, v); }
  if (runtimeInline) for (const n of Object.keys(RUNTIME)) out.set(n, R(n));
  return out;
}
// var(--x, fallback) substitution. `get` returns a computed value or undefined (guaranteed-invalid).
function substitute(value, get) {
  let out = '', i = 0;
  for (;;) {
    const j = value.indexOf('var(', i);
    if (j < 0) { out += value.slice(i); break; }
    out += value.slice(i, j);
    const k = closeParen(value, j + 3);
    const inner = value.slice(j + 4, k);
    const parts = splitTop(inner, ',');
    const name = parts[0];
    const comma = inner.indexOf(',');
    const fallback = comma < 0 ? null : inner.slice(comma + 1).trim();
    const v = get(name);
    if (v !== undefined) out += v;
    else if (fallback !== null) { const f = substitute(fallback, get); if (f === undefined) return undefined; out += f; }
    else return undefined; // invalid at computed-value time
    i = k + 1;
  }
  return out;
}
function closeParen(s, open) {
  let d = 0;
  for (let k = open; k < s.length; k++) { if (s[k] === '(') d++; else if (s[k] === ')' && --d === 0) return k; }
  fail(`unbalanced parentheses in "${s}"`);
}

// ── values ───────────────────────────────────────────────────────────────────────────────────────────
const NUM = String.raw`[+-]?(?:\d+\.?\d*|\.\d+)(?:e[+-]?\d+)?`;
const isNum = (s) => new RegExp(`^${NUM}$`, 'i').test(s);

// Colours as { r, g, b (0–255, float), a (0–1) }, quantised only when emitted.
const NAMED = { transparent: [0, 0, 0, 0], black: [0, 0, 0, 1], white: [255, 255, 255, 1] };
function parseColor(v) {
  v = v.trim();
  let m;
  if ((m = v.match(/^#([0-9a-f]+)$/i))) {
    let h = m[1];
    if (h.length === 3 || h.length === 4) h = [...h].map((c) => c + c).join('');
    if (h.length !== 6 && h.length !== 8) return null;
    const n = (i) => parseInt(h.slice(i, i + 2), 16);
    return { r: n(0), g: n(2), b: n(4), a: h.length === 8 ? n(6) / 255 : 1 };
  }
  if (NAMED[v.toLowerCase()]) { const [r, g, b, a] = NAMED[v.toLowerCase()]; return { r, g, b, a }; }
  if ((m = v.match(/^(rgba?|hsla?|oklch|oklab|color-mix)\((.*)\)$/i))) {
    const fn = m[1].toLowerCase(), body = m[2].trim();
    if (fn === 'color-mix') return colorMix(body, v);
    const { comps, alpha } = colorArgs(body, v);
    const a = alpha === null ? 1 : alpha.endsWith('%') ? parseFloat(alpha) / 100 : parseFloat(alpha);
    if (fn.startsWith('rgb')) {
      const ch = comps.map((c) => (c.endsWith('%') ? (parseFloat(c) / 100) * 255 : parseFloat(c)));
      return { r: ch[0], g: ch[1], b: ch[2], a: clamp01(a) };
    }
    if (fn.startsWith('hsl')) {
      const h = angle(comps[0]), s = parseFloat(comps[1]) / 100, l = parseFloat(comps[2]) / 100;
      const [r, g, b] = hslToRgb(h, s, l);
      return { r: r * 255, g: g * 255, b: b * 255, a: clamp01(a) };
    }
    // oklab / oklch literals: exact conversion; outside sRGB the browser gamut-maps, which we don't guess.
    let L = comps[0].endsWith('%') ? parseFloat(comps[0]) / 100 : parseFloat(comps[0]);
    let A, B;
    if (fn === 'oklch') {
      const C = comps[1].endsWith('%') ? (parseFloat(comps[1]) / 100) * 0.4 : parseFloat(comps[1]);
      const H = (angle(comps[2]) * Math.PI) / 180;
      A = C * Math.cos(H); B = C * Math.sin(H);
    } else {
      A = comps[1].endsWith('%') ? (parseFloat(comps[1]) / 100) * 0.4 : parseFloat(comps[1]);
      B = comps[2].endsWith('%') ? (parseFloat(comps[2]) / 100) * 0.4 : parseFloat(comps[2]);
    }
    const rgb = oklabToSrgb(L, A, B);
    if (rgb.some((c) => c < -1e-4 || c > 1 + 1e-4)) fail(`${v} is outside sRGB; the browser gamut-maps it and the generator does not guess how`);
    return { r: rgb[0] * 255, g: rgb[1] * 255, b: rgb[2] * 255, a: clamp01(a) };
  }
  return null;
}
const clamp01 = (x) => Math.min(1, Math.max(0, x));
function colorArgs(body, whole) {
  let comps, alpha = null;
  if (body.includes(',')) { comps = splitTop(body, ','); if (comps.length === 4) alpha = comps.pop(); }
  else { const [c, a] = body.split('/').map((s) => s.trim()); comps = splitTop(c, ' '); alpha = a ?? null; }
  if (comps.length !== 3 || [...comps, alpha ?? '0'].some((c) => c === 'none' || !/^[+-]?(\d+\.?\d*|\.\d+)(%|deg)?$/.test(c))) fail(`unhandled colour syntax: ${whole}`);
  return { comps, alpha };
}
function angle(s) { return s.endsWith('deg') ? parseFloat(s) : parseFloat(s); }
function hslToRgb(h, s, l) {
  h = ((h % 360) + 360) % 360;
  const f = (n) => { const k = (n + h / 30) % 12; const a = s * Math.min(l, 1 - l); return l - a * Math.max(-1, Math.min(k - 3, 9 - k, 1)); };
  return [f(0), f(8), f(4)];
}
const toLinear = (c) => (Math.abs(c) <= 0.04045 ? c / 12.92 : Math.sign(c) * Math.pow((Math.abs(c) + 0.055) / 1.055, 2.4));
const toGamma = (c) => (Math.abs(c) <= 0.0031308 ? 12.92 * c : Math.sign(c) * (1.055 * Math.pow(Math.abs(c), 1 / 2.4) - 0.055));
function srgbToOklab(r, g, b) {
  const [lr, lg, lb] = [r, g, b].map(toLinear);
  const l = Math.cbrt(0.4122214708 * lr + 0.5363325363 * lg + 0.0514459929 * lb);
  const m = Math.cbrt(0.2119034982 * lr + 0.6806995451 * lg + 0.1073969566 * lb);
  const s = Math.cbrt(0.0883024619 * lr + 0.2817188376 * lg + 0.6299787005 * lb);
  return [0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s, 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s, 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s];
}
function oklabToSrgb(L, A, B) {
  const l = (L + 0.3963377774 * A + 0.2158037573 * B) ** 3;
  const m = (L - 0.1055613458 * A - 0.0638541728 * B) ** 3;
  const s = (L - 0.0894841775 * A - 1.291485548 * B) ** 3;
  return [4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s, -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s, -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s].map(toGamma);
}
// color-mix(in srgb|oklab, A [p%], B [q%]): CSS Color 5 — percentages normalised (a sum under 100% scales the
// alpha), premultiplied interpolation in the named space.
function colorMix(body, whole) {
  const parts = splitTop(body, ',');
  if (parts.length !== 3) fail(`unhandled color-mix: ${whole}`);
  const space = parts[0].match(/^in\s+([a-z-]+)$/)?.[1];
  if (space !== 'srgb' && space !== 'oklab') fail(`color-mix in "${parts[0]}" is not modelled: ${whole}`);
  const arg = (x) => { const m = x.match(/^(.*?)\s+([\d.]+)%$/) ?? x.match(/^([\d.]+)%\s+(.*)$/)?.slice(0).reverse(); return m ? { c: m[1].trim(), p: parseFloat(m[2]) / 100 } : { c: x, p: null }; };
  const a = arg(parts[1]), b = arg(parts[2]);
  let pa = a.p, pb = b.p;
  if (pa === null && pb === null) pa = pb = 0.5;
  else if (pa === null) pa = 1 - pb;
  else if (pb === null) pb = 1 - pa;
  const sum = pa + pb;
  if (sum <= 0) fail(`color-mix percentages sum to 0: ${whole}`);
  const mult = sum < 1 ? sum : 1;
  pa /= sum; pb /= sum;
  const ca = parseColor(a.c), cb = parseColor(b.c);
  if (!ca || !cb) fail(`unhandled colour inside color-mix: ${whole}`);
  const conv = space === 'srgb' ? (c) => [c.r / 255, c.g / 255, c.b / 255] : (c) => srgbToOklab(c.r / 255, c.g / 255, c.b / 255);
  const back = space === 'srgb' ? (x) => x : (x) => oklabToSrgb(...x);
  const xa = conv(ca), xb = conv(cb);
  const alpha = ca.a * pa + cb.a * pb;
  if (alpha === 0) return { r: 0, g: 0, b: 0, a: 0 };
  const mixed = back([0, 1, 2].map((i) => (xa[i] * ca.a * pa + xb[i] * cb.a * pb) / alpha));
  return { r: mixed[0] * 255, g: mixed[1] * 255, b: mixed[2] * 255, a: alpha * mult };
}
function quantise(c) {
  const ch = (x) => Math.min(255, Math.max(0, Math.round(x)));
  return ((ch(c.r) << 24) | (ch(c.g) << 16) | (ch(c.b) << 8) | ch(c.a * 255)) >>> 0;
}

function parseShadow(v, name) {
  if (v === 'none') return [];
  return splitTop(v, ',').map((layer) => {
    let inset = false, color = null; const lens = [];
    for (const p of splitTop(layer, ' ')) {
      if (p === 'inset') inset = true;
      else if (/^-?(\d+\.?\d*|\.\d+)(px)?$/.test(p)) { if (!p.endsWith('px') && parseFloat(p) !== 0) fail(`${name}: unitless shadow length ${p}`); lens.push(parseFloat(p)); }
      else { const c = parseColor(p); if (!c) fail(`${name}: unhandled shadow part "${p}" in ${v}`); color = c; }
    }
    if (lens.length < 2 || lens.length > 4) fail(`${name}: shadow needs 2–4 lengths: ${v}`);
    if (!color) fail(`${name}: a shadow without a colour paints currentColor, which is not a token: ${v}`);
    return { x: lens[0], y: lens[1], blur: lens[2] ?? 0, spread: lens[3] ?? 0, color: quantise(color), inset };
  });
}
const FILTERS = { blur: 'Blur', saturate: 'Saturate', brightness: 'Brightness' };
function parseFilter(v, name) {
  return splitTop(v, ' ').map((f) => {
    const m = f.match(/^([a-z-]+)\((.*)\)$/);
    if (!m || !FILTERS[m[1]]) fail(`${name}: unhandled filter function "${f}"`);
    const a = m[2].trim();
    let n;
    if (m[1] === 'blur') { if (!/px$/.test(a)) fail(`${name}: blur() needs px: ${f}`); n = parseFloat(a); }
    else n = a.endsWith('%') ? parseFloat(a) / 100 : parseFloat(a);
    return { fn: FILTERS[m[1]], v: n };
  });
}
const STRETCH = { 'ultra-condensed': 50, 'extra-condensed': 62.5, condensed: 75, 'semi-condensed': 87.5, normal: 100, 'semi-expanded': 112.5, expanded: 125, 'extra-expanded': 150, 'ultra-expanded': 200 };
const GENERIC_FAMILIES = /^(ui-sans-serif|ui-serif|ui-monospace|ui-rounded|system-ui|sans-serif|serif|monospace|cursive|fantasy|emoji|math)$/;

// One theme's value of one token → { kind, value }. Kinds unify across themes later.
function typed(name, v) {
  if (v.includes('@@R')) return { kind: 'runtime' };
  if (/shadow/.test(name)) return { kind: 'shadow', value: parseShadow(v, name) };
  if (/stretch/.test(name)) {
    if (STRETCH[v] !== undefined) return { kind: 'stretch', value: STRETCH[v] };
    const m = v.match(/^([\d.]+)%$/); if (m) return { kind: 'stretch', value: +m[1] };
    fail(`${name}: unhandled font-stretch ${v}`);
  }
  if (/filter/.test(name)) return { kind: 'filter', value: parseFilter(v, name) };
  const c = parseColor(v); if (c) return { kind: 'color', value: quantise(c) };
  let m;
  if ((m = v.match(new RegExp(`^(${NUM})px$`)))) return { kind: 'px', value: +m[1] };
  if ((m = v.match(new RegExp(`^(${NUM})rem$`)))) return { kind: 'px', value: +m[1] * 16 };
  if ((m = v.match(new RegExp(`^(${NUM})em$`)))) return { kind: 'em', value: +m[1] };
  if ((m = v.match(new RegExp(`^(${NUM})%$`)))) return { kind: 'pct', value: +m[1] / 100 };
  if ((m = v.match(new RegExp(`^(${NUM})(ms|s)$`)))) return { kind: 'ms', value: m[2] === 's' ? +m[1] * 1000 : +m[1] };
  if (isNum(v)) return { kind: 'num', value: +v };
  if ((m = v.match(/^cubic-bezier\((.*)\)$/))) { const p = splitTop(m[1], ',').map(Number); if (p.length !== 4 || p.some(isNaN)) fail(`${name}: ${v}`); return { kind: 'easing', value: p }; }
  if (/^calc\(/.test(v)) { const r = evalMath(v, {}); return { kind: r.unit === 'px' ? 'px' : 'num', value: r.v }; }
  const fams = splitTop(v, ',').map((f) => f.replace(/^["']|["']$/g, ''));
  if (fams.length > 1 || /^["']/.test(v)) {
    if (!fams.some((f) => GENERIC_FAMILIES.test(f)) && !/^["']/.test(v)) fail(`${name}: unhandled value ${v}`);
    return { kind: 'font', value: fams[0], stack: fams };
  }
  fail(`${name}: unhandled value "${v}" (not a colour, length, number, shadow, font stack, filter or easing)`);
}

// ── CSS math: calc() / round() / min() / max() / clamp() over px and numbers ─────────────────────────
// parseMath(text, varAtom) → AST. varAtom(name, fallbackText) handles var() (codegen); literal text may also
// carry @@R<name>@@ runtime markers. Node kinds: num {v, unit}, rt {name}, field {name}, local {name},
// bin {op, a, b}, round {strategy, a, b}, fn {name, args}.
function parseMath(text, varAtom) {
  let s = text, i = 0;
  const ws = () => { while (i < s.length && /\s/.test(s[i])) i++; };
  const expect = (c) => { ws(); if (s[i] !== c) fail(`math: expected "${c}" at ${i} in ${text}`); i++; };
  function sum() {
    let a = product();
    for (;;) { ws(); const c = s[i]; if ((c === '+' || c === '-') && /\s/.test(s[i - 1] ?? ' ')) { i++; a = { t: 'bin', op: c, a, b: product() }; } else return a; }
  }
  function product() {
    let a = atom();
    for (;;) { ws(); const c = s[i]; if (c === '*' || c === '/') { i++; a = { t: 'bin', op: c, a, b: atom() }; } else return a; }
  }
  function args() { const out = [sum()]; for (;;) { ws(); if (s[i] === ',') { i++; out.push(sum()); } else break; } expect(')'); return out; }
  function atom() {
    ws();
    let m;
    const rest = s.slice(i);
    if ((m = rest.match(/^@@R(--[a-z0-9-]+)@@/))) { i += m[0].length; return { t: 'rt', name: m[1] }; }
    if ((m = rest.match(/^var\(/))) {
      const k = closeParen(s, i + 3);
      const inner = s.slice(i + 4, k); i = k + 1;
      const comma = inner.indexOf(',');
      return varAtom((comma < 0 ? inner : inner.slice(0, comma)).trim(), comma < 0 ? null : inner.slice(comma + 1).trim());
    }
    if ((m = rest.match(/^(calc)?\(/))) { i += m[0].length; const e = sum(); expect(')'); return e; }
    if ((m = rest.match(/^round\(\s*(nearest|up|down|to-zero)\s*,/))) { i += m[0].length; const a = args(); return { t: 'round', strategy: m[1], a: a[0], b: a[1] ?? { t: 'num', v: 1, unit: 'num' } }; }
    if ((m = rest.match(/^round\(/))) { i += m[0].length; const a = args(); return { t: 'round', strategy: 'nearest', a: a[0], b: a[1] ?? { t: 'num', v: 1, unit: 'num' } }; }
    if ((m = rest.match(/^(min|max|clamp)\(/))) { i += m[0].length; return { t: 'fn', name: m[1], args: args() }; }
    if ((m = rest.match(new RegExp(`^(${NUM})(px|rem)?`, 'i')))) {
      i += m[0].length;
      if (/^[a-z%]/i.test(s.slice(i))) fail(`math: unhandled unit at "${s.slice(i - m[0].length, i + 4)}" in ${text}`);
      return { t: 'num', v: m[2] === 'rem' ? +m[1] * 16 : +m[1], unit: m[2] ? 'px' : 'num' };
    }
    fail(`math: unexpected "${rest.slice(0, 12)}" in ${text}`);
  }
  const e = sum(); ws();
  if (i !== s.length) fail(`math: trailing "${s.slice(i)}" in ${text}`);
  return e;
}
// Unit algebra: px ± px, px × num, px ÷ num, num ÷ num; round(px, px) and round(num, num).
function unitOf(n, units) {
  switch (n.t) {
    case 'num': return n.unit;
    case 'rt': return 'num';
    case 'field': case 'local': return units(n);
    case 'bin': {
      const a = unitOf(n.a, units), b = unitOf(n.b, units);
      if (n.op === '+' || n.op === '-') { if (a !== b) fail(`math: ${a} ${n.op} ${b}`); return a; }
      if (n.op === '*') { if (a === 'px' && b === 'px') fail('math: px × px'); return a === 'px' || b === 'px' ? 'px' : 'num'; }
      if (b === 'px') { if (a !== 'px') fail('math: num ÷ px'); return 'num'; }
      return a;
    }
    case 'round': { const a = unitOf(n.a, units), b = unitOf(n.b, units); if (a !== b && !(b === 'num' && n.b.t === 'num')) fail(`math: round(${a}, ${b})`); return a; }
    case 'fn': { const us = n.args.map((x) => unitOf(x, units)); if (new Set(us).size !== 1) fail(`math: ${n.name}() mixes units`); return us[0]; }
  }
}
// CSS <rounding-strategy> as Chromium evaluates it (css_math_expression_node.cc): ties of `nearest` go up.
function cssRound(strategy, a, b) {
  if (b === 0) return NaN;
  b = Math.abs(b);
  if (a % b === 0) return a;
  const lo = Math.floor(a / b) * b, hi = lo + b;
  switch (strategy) {
    case 'up': return hi;
    case 'down': return lo;
    case 'to-zero': return a < 0 ? hi : lo;
    default: return Math.abs(a - lo) < Math.abs(hi - a) ? lo : hi;
  }
}
function evalAst(n, env) {
  switch (n.t) {
    case 'num': return n.v;
    case 'rt': if (env[n.name] === undefined) fail(`math: runtime ${n.name} without a value`); return env[n.name];
    case 'bin': { const a = evalAst(n.a, env), b = evalAst(n.b, env); return n.op === '+' ? a + b : n.op === '-' ? a - b : n.op === '*' ? a * b : a / b; }
    case 'round': return cssRound(n.strategy, evalAst(n.a, env), evalAst(n.b, env));
    case 'fn': { const v = n.args.map((x) => evalAst(x, env)); return n.name === 'min' ? Math.min(...v) : n.name === 'max' ? Math.max(...v) : Math.max(v[0], Math.min(v[1], v[2])); }
    default: fail(`math: cannot evaluate ${n.t}`);
  }
}
function evalMath(text, env) {
  const ast = parseMath(text, (name) => fail(`math: unresolved var(${name}) in ${text}`));
  return { v: evalAst(ast, env), unit: unitOf(ast, () => fail('unreachable')) };
}

// ── inputs: uiStore, AppearanceTab, Tailwind ─────────────────────────────────────────────────────────
const uiStore = read(IN.uiStore);
const numConst = (name) => {
  const m = uiStore.match(new RegExp(`export const ${name} = (${NUM});`));
  if (!m) fail(`${rel(IN.uiStore)}: ${name} not found`);
  return +m[1];
};
const SETTINGS = {
  TEXT_SIZE_MIN_PX: numConst('TEXT_SIZE_MIN_PX'), TEXT_SIZE_MAX_PX: numConst('TEXT_SIZE_MAX_PX'),
  TEXT_SIZE_STEP_PX: numConst('TEXT_SIZE_STEP_PX'), DEFAULT_TEXT_SIZE_PX: numConst('DEFAULT_TEXT_SIZE_PX'),
  ROW_SCALE_MIN: numConst('ROW_SCALE_MIN'), ROW_SCALE_MAX: numConst('ROW_SCALE_MAX'), DEFAULT_ROW_SCALE: numConst('DEFAULT_ROW_SCALE'),
};
const THEME_IDS = (() => {
  const m = uiStore.match(/export const THEME_IDS: readonly ThemeId\[\] = \[([^\]]*)\]/);
  if (!m) fail(`${rel(IN.uiStore)}: THEME_IDS not found`);
  return [...m[1].matchAll(/"([a-z-]+)"/g)].map((x) => x[1]);
})();
const DEFAULT_THEME = uiStore.match(/export const DEFAULT_THEME: ThemeId = "([a-z-]+)";/)?.[1] ?? fail('DEFAULT_THEME not found');
if (!THEME_IDS.includes(DEFAULT_THEME)) fail(`DEFAULT_THEME ${DEFAULT_THEME} is not in THEME_IDS`);

const appearance = read(IN.appearance);
const THEME_INFO = (() => {
  const start = appearance.indexOf('const THEMES: ThemeOption[] = [');
  if (start < 0) fail(`${rel(IN.appearance)}: THEMES not found`);
  const open = appearance.indexOf('[', start + 'const THEMES: ThemeOption[] ='.length);
  let d = 0, k = open;
  for (; k < appearance.length; k++) { if (appearance[k] === '[') d++; else if (appearance[k] === ']' && --d === 0) break; }
  const body = appearance.slice(open + 1, k);
  const entries = [];
  let depth = 0, from = -1;
  for (let i = 0; i < body.length; i++) {
    if (body[i] === '{') { if (depth++ === 0) from = i; }
    else if (body[i] === '}' && --depth === 0) entries.push(body.slice(from, i + 1));
  }
  return entries.map((e) => {
    const str = (key) => e.match(new RegExp(`\\b${key}: "([^"]*)"`))?.[1] ?? fail(`${rel(IN.appearance)}: a THEMES entry has no ${key}`);
    const pv = e.match(/preview: \{([^}]*)\}/)?.[1] ?? fail(`${rel(IN.appearance)}: a THEMES entry has no preview`);
    const preview = Object.fromEntries([...pv.matchAll(/(\w+): "([^"]*)"/g)].map((m) => [m[1], m[2]]));
    return { id: str('id'), name: str('name'), mode: str('mode'), preview };
  });
})();
const THEMES = THEME_IDS;
if (JSON.stringify(THEME_INFO.map((t) => t.id)) !== JSON.stringify(THEMES)) fail(`AppearanceTab THEMES order (${THEME_INFO.map((t) => t.id)}) differs from uiStore THEME_IDS (${THEMES})`);
for (const t of THEME_INFO) if (t.mode !== 'Dark' && t.mode !== 'Light') fail(`${t.id}: mode "${t.mode}"`);
// The --p-* custom properties each preview card sets inline, from `"--p-x": option.preview.y`.
const PREVIEW_VARS = Object.fromEntries([...appearance.matchAll(/"(--p-[a-z-]+)": option\.preview\.(\w+)/g)].map((m) => [m[1], m[2]]));
if (!Object.keys(PREVIEW_VARS).length) fail(`${rel(IN.appearance)}: no --p-* inline variables found`);
const ROW_SCALE_STEP = (() => {
  const m = appearance.match(new RegExp(String.raw`label="List density"[\s\S]*?step=\{(${NUM})\}`));
  if (!m) fail(`${rel(IN.appearance)}: the List density slider's step not found`);
  return +m[1];
})();

const twVersion = JSON.parse(read(IN.twPkg)).version;
const lockVersion = JSON.parse(read(IN.lock)).packages?.['node_modules/tailwindcss']?.version;
if (!fs.existsSync(IN.twTheme)) fail(`${rel(IN.twTheme)} is missing: run \`npm ci\` in electron-client (Tailwind's theme is an input)`);
if (twVersion !== lockVersion) fail(`electron-client/node_modules has tailwindcss ${twVersion} but package-lock pins ${lockVersion}: run \`npm ci\` in electron-client`);

// ── resolve ──────────────────────────────────────────────────────────────────────────────────────────
const cssText = read(IN.css);
const cssTree = parseCss(cssText);
const twTree = parseCss(read(IN.twTheme));
ORDER = 0;
const twBlocks = tokenBlocks(twTree, (p) => (p.startsWith('@theme') ? 0 : fail(`theme.css: unexpected token block ${p}`)), 'theme.css');
const ourBlocks = tokenBlocks(cssTree, (p) => (p.startsWith('@theme') ? 1 : 2), 'globals.css');
const BLOCKS = [...twBlocks, ...ourBlocks];

// Every selector that declares custom properties must be the root or one of SCOPES.
for (const b of ourBlocks) for (const s of b.selectors) {
  const last = s.comps[s.comps.length - 1];
  if (last.root) continue;
  if (!SCOPES.some((sc) => matchEl(last, { classes: sc.classes, attrs: sc.attrs }) && last.classes.includes(sc.own ?? 'chrome-scope')))
    fail(`globals.css:${b.node.startLine}: "${b.node.prelude}" declares custom properties on an element the generator has no scope for; add it to SCOPES`);
}

const ourNames = []; // first-declaration order in globals.css
for (const b of ourBlocks) for (const d of b.decls) if (!ourNames.includes(d.name)) ourNames.push(d.name);
const scopeOwn = (sc) => { const names = []; for (const b of ourBlocks) for (const s of b.selectors) { const last = s.comps[s.comps.length - 1]; if (!last.root && last.classes.includes(sc.own)) for (const d of b.decls) if (!names.includes(d.name)) names.push(d.name); } return names; };

const ROOT = {}; // theme → { decl, computed }
const SCOPE_VALUES = {}; // theme → key → computed map
for (const theme of THEMES) {
  const decl = declared(BLOCKS, theme, null);
  const computed = computeElement(decl, null, true);
  ROOT[theme] = { decl, computed };
  SCOPE_VALUES[theme] = {};
  for (const sc of SCOPES) {
    const el = { classes: sc.classes, attrs: sc.attrs ?? {} };
    const d = declared(BLOCKS, theme, el);
    // Tailwind inlines the shadow utilities as var(--t-shadow-*): evaluate them at the element.
    for (const [n, v] of declared(BLOCKS, theme, null)) if (n.startsWith('--shadow-') && !d.has(n) && BLOCKS.some((b) => b.layer === 1 && b.decls.some((x) => x.name === n))) d.set(n, v);
    if (sc.preview) {
      const info = THEME_INFO.find((t) => t.id === theme);
      for (const [pv, key] of Object.entries(PREVIEW_VARS)) { if (info.preview[key] === undefined) fail(`${theme}: preview.${key} missing`); d.set(pv, info.preview[key]); }
    }
    SCOPE_VALUES[theme][sc.key] = computeElement(d, computed, false);
  }
}

// Kind unification across themes.
function unify(name, perTheme) {
  const kinds = new Set(perTheme.map((t) => t.kind));
  if (kinds.size === 1) return [...kinds][0];
  const k = [...kinds].sort().join('+');
  if (k === 'pct+px') return 'lengthpct';
  if ([...kinds].every((x) => x === 'num' || x === 'px') && perTheme.every((t) => t.kind === 'px' || t.value === 0)) return 'px';
  fail(`${name}: kinds differ between themes (${k})`);
}

const TYPES = { color: 'Color', px: 'f32', em: 'f32', num: 'f32', pct: 'f32', ms: 'f32', stretch: 'f32', font: "&'static str", shadow: "&'static [ShadowLayer]", lengthpct: 'LengthPct', filter: "&'static [FilterFn]", easing: 'Easing' };
const UNIT_DOC = { color: '', px: ' (px)', em: ' (em)', num: '', pct: ' (a % as a fraction, 0–1)', ms: ' (ms)', stretch: ' (font-stretch %, = the wdth axis value)', font: ' (bundled family name)', shadow: ' (layers, first on top)', lengthpct: ' (px, or a % of the box)', filter: '', easing: '' };
const snake = (n) => n.replace(/^--/, '').replace(/-+/g, '_');

// Palette fields: every token computed at the root in every theme that is not runtime-dependent. `--tc-*`
// (console-split's chrome ramp) exist in one theme only and reach components through the chrome palette.
const paletteFields = []; const onlySome = []; const scaledNames = [];
for (const name of ourNames) {
  if (RUNTIME[name]) continue;
  const vals = THEMES.map((t) => ROOT[t].computed.get(name));
  if (vals.some((v) => v === undefined)) {
    if (vals.every((v) => v === undefined)) continue; // scope-only names
    if (!name.startsWith('--tc-')) fail(`${name} resolves in some themes only (${THEMES.filter((t, i) => vals[i] === undefined).join(', ')} lack it)`);
    onlySome.push(name); continue;
  }
  const ts = THEMES.map((t, i) => typed(name, vals[i]));
  if (ts.some((t) => t.kind === 'runtime')) {
    if (!ts.every((t) => t.kind === 'runtime')) fail(`${name} is runtime-dependent in some themes only`);
    scaledNames.push(name); continue;
  }
  paletteFields.push({ name, field: snake(name), kind: unify(name, ts) });
}
const seen = new Map();
for (const f of paletteFields) { if (seen.has(f.field)) fail(`field name collision: ${f.name} and ${seen.get(f.field)}`); seen.set(f.field, f.name); }
const fieldOf = new Map(paletteFields.map((f) => [f.name, f]));

function tokenValue(name, kind, v) {
  const t = typed(name, v);
  if (kind === 'lengthpct') return t.kind === 'pct' ? { lp: 'Fraction', v: t.value } : { lp: 'Px', v: t.value };
  if (kind === 'font') { const fam = BUNDLED[t.value]; if (!fam) fail(`${name}: the stack starts with "${t.value}", which the client does not bundle (${v})`); return fam; }
  return t.value;
}
const PALETTES = {}; const CHROME = {};
for (const theme of THEMES) {
  PALETTES[theme] = new Map(paletteFields.map((f) => [f.name, tokenValue(f.name, f.kind, ROOT[theme].computed.get(f.name))]));
  const ch = SCOPE_VALUES[theme].chrome;
  CHROME[theme] = new Map(paletteFields.map((f) => { const v = ch.get(f.name); if (v === undefined) fail(`${theme}: ${f.name} unresolved in .chrome-scope`); return [f.name, tokenValue(f.name, f.kind, v)]; }));
}
const same = (a, b) => JSON.stringify([...a]) === JSON.stringify([...b]);

// Scopes other than chrome: typed structs of the scope's own declarations; runtime ones go to Scaled.
const SCOPE_STRUCTS = [];
const scaledScoped = [];
for (const sc of SCOPES) {
  if (sc.palette) continue;
  const names = scopeOwn(sc);
  const fields = [];
  for (const name of names) {
    const vals = THEMES.map((t) => SCOPE_VALUES[t][sc.key].get(name));
    if (vals.some((v) => v === undefined)) fail(`.${sc.own}: ${name} unresolved in ${THEMES.filter((t, i) => vals[i] === undefined).join(', ')}`);
    const ts = THEMES.map((t, i) => typed(name, vals[i]));
    if (ts.some((t) => t.kind === 'runtime')) { scaledScoped.push({ sc, name }); continue; }
    fields.push({ name, field: snake(name), kind: unify(name, ts), values: THEMES.map((t, i) => tokenValue(name, unify(name, ts), vals[i])) });
  }
  if (fields.length) SCOPE_STRUCTS.push({ sc, fields, struct: sc.key.split('_').map((w) => w[0].toUpperCase() + w.slice(1)).join('') + 'Scope' });
}

// ── Scaled: runtime-dependent tokens as Rust expressions over palette fields ─────────────────────────
// Built from the winning *declaration* (not the substituted value), so palette tokens become field reads and
// one function serves every theme; it must be the same expression in every theme.
const scaledDefs = []; // { name, field, ast, unit, scope? }
function symbolic(theme, declMap, name, localNames, stack = []) {
  const text = declMap.get(name);
  if (stack.includes(name)) fail(`cycle through ${name}`);
  const varAtom = (ref, fallback) => {
    if (RUNTIME[ref]) {
      if (fallback !== null) {
        const fb = substitute(fallback, (n) => ROOT[theme].computed.get(n));
        const want = ref === '--ui-text-size-n' ? SETTINGS.DEFAULT_TEXT_SIZE_PX : SETTINGS.DEFAULT_ROW_SCALE;
        if (fb === undefined || Math.abs(evalMath(fb, {}).v - want) > 1e-9) fail(`${name}: var(${ref}, ${fallback}) falls back to ${fb}, not uiStore's default ${want}`);
      }
      return { t: 'rt', name: ref };
    }
    if (localNames.includes(ref)) return { t: 'local', name: ref };
    const f = fieldOf.get(ref);
    if (f) { if (!['px', 'num'].includes(f.kind)) fail(`${name}: var(${ref}) is a ${f.kind}, not a number`); return { t: 'field', name: ref, unit: f.kind === 'px' ? 'px' : 'num' }; }
    fail(`${name}: var(${ref}) is neither a runtime knob, a palette number nor an earlier scaled token`);
  };
  return parseMath(text, varAtom);
}
{
  // Root-level scaled tokens in dependency order (a token may use an earlier one as a local).
  const pending = [...scaledNames];
  const order = [];
  const deps = (name) => {
    const text = ROOT[THEMES[0]].decl.get(name);
    return [...text.matchAll(/var\((--[a-z0-9-]+)/g)].map((m) => m[1]).filter((n) => scaledNames.includes(n));
  };
  while (pending.length) {
    const i = pending.findIndex((n) => deps(n).every((d) => order.includes(d)));
    if (i < 0) fail(`scaled tokens depend on each other in a cycle: ${pending}`);
    order.push(pending.splice(i, 1)[0]);
  }
  const units = (n) => (n.t === 'field' ? n.unit : scaledDefs.find((d) => d.name === n.name).unit);
  for (const name of order) {
    const asts = THEMES.map((t) => JSON.stringify(symbolic(t, ROOT[t].decl, name, order.slice(0, order.indexOf(name)))));
    if (new Set(asts).size !== 1) fail(`${name}: its expression differs between themes; scaled() would need a per-theme branch`);
    const ast = JSON.parse(asts[0]);
    scaledDefs.push({ name, field: snake(name), ast, unit: unitOf(ast, units), decl: ROOT[THEMES[0]].decl.get(name) });
  }
  for (const { sc, name } of scaledScoped) {
    const el = { classes: sc.classes, attrs: sc.attrs ?? {} };
    const asts = THEMES.map((t) => JSON.stringify(symbolic(t, declared(BLOCKS, t, el), name, order)));
    if (new Set(asts).size !== 1) fail(`.${sc.own} ${name}: expression differs between themes`);
    const ast = JSON.parse(asts[0]);
    scaledDefs.push({ name, field: `${sc.key}_${snake(name)}`, ast, unit: unitOf(ast, units), decl: declared(BLOCKS, THEMES[0], el).get(name), scope: sc });
  }
}
// The independent path for the fixture: the theme's *substituted* text (literal numbers, runtime markers),
// evaluated by the CSS math evaluator.
function literalScaled(theme, def, n, r) {
  const v = def.scope ? SCOPE_VALUES[theme][def.scope.key].get(def.name) : ROOT[theme].computed.get(def.name);
  return evalMath(v, { '--ui-text-size-n': n, '--ui-row-scale': r }).v;
}

// ── Tailwind defaults (the namespaces the renderer's utilities read and globals.css leaves alone) ─────
const TW_NAMESPACES = ['--spacing', '--breakpoint-', '--container-', '--text-', '--font-weight-', '--tracking-', '--leading-', '--radius-', '--ease-', '--blur-', '--default-transition-timing-function'];
const twTokens = [];
for (const b of twBlocks) for (const d of b.decls) {
  if (!TW_NAMESPACES.some((p) => d.name === p || d.name.startsWith(p))) continue;
  if (ourNames.includes(d.name)) continue; // overridden by globals.css: it is a Palette field
  if (/^--text-/.test(d.name) && !/^--text-(xs|sm|base|lg|[2-9]?xl)(--line-height)?$/.test(d.name)) continue;
  const t = typed(d.name, d.value);
  twTokens.push({ name: d.name, kind: t.kind, value: t.value, raw: d.value });
}

// ── emit Rust ────────────────────────────────────────────────────────────────────────────────────────
// The shortest decimal that parses to the same f32 (so clippy sees no excessive precision).
const f32 = (x) => {
  if (!Number.isFinite(x)) fail(`non-finite number ${x}`);
  const want = Math.fround(x);
  let s = String(want);
  for (let p = 1; p <= 9; p++) { const c = String(+want.toPrecision(p)); if (Math.fround(+c) === want) { s = c; break; } }
  return /[.e]/.test(s) ? s : `${s}.0`;
};
const f64 = (x) => { const s = String(x); return /[.e]/.test(s) ? s : `${s}.0`; };
const hex = (u) => `Color(0x${u.toString(16).padStart(8, '0')})`;
const shadowLit = (layers) => (layers.length ? `&[${layers.map((l) => `ShadowLayer { x: ${f32(l.x)}, y: ${f32(l.y)}, blur: ${f32(l.blur)}, spread: ${f32(l.spread)}, color: ${hex(l.color)}, inset: ${l.inset} }`).join(', ')}]` : '&[]');
function lit(kind, v) {
  switch (kind) {
    case 'color': return hex(v);
    case 'font': return JSON.stringify(v);
    case 'shadow': return shadowLit(v);
    case 'lengthpct': return `LengthPct::${v.lp}(${f32(v.v)})`;
    case 'filter': return `&[${v.map((x) => `FilterFn::${x.fn}(${f32(x.v)})`).join(', ')}]`;
    case 'easing': return `Easing::CubicBezier(${v.map(f32).join(', ')})`;
    default: return f32(v);
  }
}
const SCREAM = (id) => id.toUpperCase().replace(/-/g, '_');
const docEsc = (s) => s.replace(/`/g, "'");
// The declaration text of a token when it is the same in every theme (the alias it reads), else null.
function sharedDecl(name, getMap) {
  const vs = new Set(THEMES.map((t) => getMap(t).get(name)));
  return vs.size === 1 ? [...vs][0] : null;
}

const HEADER = `// @generated by gpui-client/tools/gen-theme/gen-theme.mjs — do not edit.
// Source: ${rel(IN.css)} (+ tailwindcss ${twVersion} theme.css, ${rel(IN.uiStore)},
// ${rel(IN.appearance)}). Change the CSS and run \`node gpui-client/tools/gen-theme/gen-theme.mjs\`;
// gpui-client/tools/gen-theme/check.sh fails while this file is stale.
`;

const usedKinds = new Set([...paletteFields.map((f) => f.kind), ...SCOPE_STRUCTS.flatMap((s) => s.fields.map((f) => f.kind))]);
const imports = ['Color', ...[['easing', 'Easing'], ['filter', 'FilterFn'], ['lengthpct', 'LengthPct'], ['shadow', 'ShadowLayer']].filter(([k]) => usedKinds.has(k)).map(([, t]) => t), 'ThemeMode'];
let rs = HEADER + `
use crate::{${imports.join(', ')}};

/// Every design token of one theme, resolved as the browser resolves it on \`<html data-theme=…>\`
/// (or, for a chrome palette, inside \`.chrome-scope\`). Field names are the CSS names snake-cased.
#[derive(Debug, Clone, PartialEq)]
pub struct Palette {
`;
for (const f of paletteFields) {
  const d = sharedDecl(f.name, (t) => ROOT[t].decl);
  rs += `    /// \`${f.name}\`${d && (d.includes('var(') || f.kind === 'font') ? `: \`${docEsc(d)}\`` : ''}${UNIT_DOC[f.kind]}\n    pub ${f.field}: ${TYPES[f.kind]},\n`;
}
rs += '}\n\n';

const staticName = (theme, chrome) => SCREAM(theme) + (chrome ? '_CHROME' : '');
function paletteLit(theme, map, chrome) {
  let s = `/// \`${theme}\`${chrome ? ' inside `.chrome-scope` (title bar, server bar, sidebars; the image viewer)' : ''}.\npub static ${staticName(theme, chrome)}: Palette = Palette {\n`;
  for (const f of paletteFields) s += `    ${f.field}: ${lit(f.kind, map.get(f.name))},\n`;
  return s + '};\n\n';
}
for (const theme of THEMES) {
  rs += paletteLit(theme, PALETTES[theme], false);
  if (!same(PALETTES[theme], CHROME[theme])) rs += paletteLit(theme, CHROME[theme], true);
}
rs += `/// The themes in Settings → Appearance order: (\`data-theme\` id = the persisted setting, display name, palette).
pub static THEMES: [(&str, &str, &Palette); ${THEMES.length}] = [\n`;
for (const t of THEME_INFO) rs += `    (${JSON.stringify(t.id)}, ${JSON.stringify(t.name)}, &${staticName(t.id, false)}),\n`;
rs += '];\n\n';
rs += `/// Each theme's palette inside \`.chrome-scope\`, indexed like [\`THEMES\`]; the base palette where nothing differs.\npub static CHROME_PALETTES: [&Palette; ${THEMES.length}] = [\n`;
for (const t of THEMES) rs += `    &${staticName(t, !same(PALETTES[t], CHROME[t]))},\n`;
rs += '];\n\n';
rs += `/// Dark or Light, as the theme picker labels each theme, indexed like [\`THEMES\`].\npub static THEME_MODES: [ThemeMode; ${THEMES.length}] = [\n${THEME_INFO.map((t) => `    ThemeMode::${t.mode},\n`).join('')}];\n\n`;
rs += `/// uiStore's \`DEFAULT_THEME\`.\npub const DEFAULT_THEME_ID: &str = ${JSON.stringify(DEFAULT_THEME)};\n\n`;
rs += `// Settings → Appearance (uiStore.ts; the density step is AppearanceTab's slider step).
/// Text size: the message body's size in px; every role of the type scale follows it.
pub const TEXT_SIZE_MIN_PX: f32 = ${f32(SETTINGS.TEXT_SIZE_MIN_PX)};
pub const TEXT_SIZE_MAX_PX: f32 = ${f32(SETTINGS.TEXT_SIZE_MAX_PX)};
pub const TEXT_SIZE_STEP_PX: f32 = ${f32(SETTINGS.TEXT_SIZE_STEP_PX)};
pub const DEFAULT_TEXT_SIZE_PX: f32 = ${f32(SETTINGS.DEFAULT_TEXT_SIZE_PX)};
/// List density: a multiplier on list-row padding, gaps and avatars.
pub const ROW_SCALE_MIN: f32 = ${f32(SETTINGS.ROW_SCALE_MIN)};
pub const ROW_SCALE_MAX: f32 = ${f32(SETTINGS.ROW_SCALE_MAX)};
pub const ROW_SCALE_STEP: f32 = ${f32(ROW_SCALE_STEP)};
pub const DEFAULT_ROW_SCALE: f32 = ${f32(SETTINGS.DEFAULT_ROW_SCALE)};

`;

// Scaled
function rustExpr(n) {
  switch (n.t) {
    case 'num': return `${f64(n.v)}_f64`;
    case 'rt': return RUNTIME[n.name];
    case 'field': return `(p.${fieldOf.get(n.name).field} as f64)`;
    case 'local': return snake(n.name);
    case 'bin': return `(${rustExpr(n.a)} ${n.op} ${rustExpr(n.b)})`;
    case 'round': return `crate::css_round(crate::RoundingStrategy::${{ nearest: 'Nearest', up: 'Up', down: 'Down', 'to-zero': 'ToZero' }[n.strategy]}, ${strip(rustExpr(n.a))}, ${strip(rustExpr(n.b))})`;
    case 'fn': return n.name === 'clamp' ? `${rustExpr(n.args[1])}.min(${strip(rustExpr(n.args[2]))}).max(${strip(rustExpr(n.args[0]))})` : n.args.map(rustExpr).reduce((a, b) => `${a}.${n.name}(${strip(b)})`);
  }
}
const strip = (e) => (e.startsWith('(') && closeParen(e, 0) === e.length - 1 ? e.slice(1, -1) : e);
rs += `/// The tokens that follow Settings → Appearance (\`--ui-text-size-n\`, \`--ui-row-scale\`), from [\`scaled\`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scaled {
`;
for (const d of scaledDefs) rs += `    /// \`${d.scope ? `.${d.scope.own} { ${d.name}: ${docEsc(d.decl)} }` : `${d.name}: ${docEsc(d.decl)}`}\`${d.unit === 'px' ? ' (px)' : ''}\n    pub ${d.field}: f32,\n`;
rs += `}

/// The runtime-dependent tokens of \`p\` at a text size (px, e.g. [\`DEFAULT_TEXT_SIZE_PX\`]) and a list density
/// (e.g. [\`DEFAULT_ROW_SCALE\`]): the CSS calc() / round() evaluated in f64 as Chromium does, stored as f32.
pub fn scaled(p: &Palette, text_size_n: f32, row_scale: f32) -> Scaled {
    let text_size_n = text_size_n as f64;
    let row_scale = row_scale as f64;
`;
const usesRow = scaledDefs.some((d) => JSON.stringify(d.ast).includes('--ui-row-scale'));
const usesP = scaledDefs.some((d) => JSON.stringify(d.ast).includes('"field"'));
if (!usesRow) rs += '    let _ = row_scale;\n';
if (!usesP) rs += '    let _ = p;\n';
for (const d of scaledDefs) rs += `    let ${d.field} = ${strip(rustExpr(d.ast))};\n`;
rs += `    Scaled {\n${scaledDefs.map((d) => `        ${d.field}: ${d.field} as f32,\n`).join('')}    }\n}\n\n`;

// Scopes
for (const s of SCOPE_STRUCTS) {
  rs += `/// \`.${s.sc.own}${s.sc.attrs ? Object.entries(s.sc.attrs).map(([k, v]) => `[${k}="${v}"]`).join('') : ''}\`: ${docEsc(s.sc.where)}.\n#[derive(Debug, Clone, PartialEq)]\npub struct ${s.struct} {\n`;
  for (const f of s.fields) {
    const d = sharedDecl(f.name, (t) => declared(BLOCKS, t, { classes: s.sc.classes, attrs: s.sc.attrs ?? {} }, s.sc.own));
    rs += `    /// \`${f.name}\`${d && /var\(|color-mix/.test(d) ? `: \`${docEsc(d)}\`` : ''}${UNIT_DOC[f.kind]}\n    pub ${f.field}: ${TYPES[f.kind]},\n`;
  }
  rs += '}\n\n';
}
rs += `/// The component-local scopes of one theme, indexed like [\`THEMES\`] in [\`SCOPES\`].\n#[derive(Debug, Clone, PartialEq)]\npub struct Scopes {\n`;
for (const s of SCOPE_STRUCTS) rs += `    pub ${s.sc.key}: ${s.struct},\n`;
rs += `}\n\npub static SCOPES: [Scopes; ${THEMES.length}] = [\n`;
THEMES.forEach((theme, ti) => {
  rs += `    // ${theme}\n    Scopes {\n`;
  for (const s of SCOPE_STRUCTS) rs += `        ${s.sc.key}: ${s.struct} {\n${s.fields.map((f) => `            ${f.field}: ${lit(f.kind, f.values[ti])},\n`).join('')}        },\n`;
  rs += '    },\n';
});
rs += '];\n\n';

// Visitor (tests, inspectors, diffs)
rs += `impl Palette {
    /// Every token as (CSS name, value), in declaration order.
    pub fn tokens(&self) -> Vec<(&'static str, crate::TokenValue)> {
        use crate::TokenValue as V;
        vec![
${paletteFields.map((f) => `            (${JSON.stringify(f.name)}, V::${{ color: 'Color', font: 'Family', shadow: 'Shadow', lengthpct: 'LengthPct', filter: 'Filter', easing: 'Easing' }[f.kind] ?? 'Number'}(self.${f.field})),\n`).join('')}        ]
    }
}

/// Tailwind CSS ${twVersion}'s default theme (\`tailwindcss/theme.css\`): the namespaces the renderer's
/// utilities read that globals.css doesn't override (it overrides --radius-sm…xl, --font-sans/mono and
/// --default-transition-duration: those are [\`Palette\`] fields). rem = 16 px. The default colour palette
/// (oklch) is left out: the renderer uses only black / white and red-400 / red-500 once each.
pub mod tailwind {
${twTokens.some((t) => t.kind === 'easing') ? '    use crate::Easing;\n' : ''}`;
for (const t of twTokens) rs += `    /// \`${t.name}: ${docEsc(t.raw)}\`${t.kind === 'px' ? ' (px)' : t.kind === 'em' ? ' (em)' : ''}\n    pub const ${SCREAM(snake(t.name))}: ${TYPES[t.kind]} = ${lit(t.kind, t.value)};\n`;
rs += '}\n';

// ── fixture: scaled() at every text size step × three densities, every theme ────────────────────────
const steps = [];
for (let n = SETTINGS.TEXT_SIZE_MIN_PX; n <= SETTINGS.TEXT_SIZE_MAX_PX + 1e-9; n += SETTINGS.TEXT_SIZE_STEP_PX) steps.push(+n.toFixed(4));
const rows = [SETTINGS.ROW_SCALE_MIN, SETTINGS.DEFAULT_ROW_SCALE, SETTINGS.ROW_SCALE_MAX];
let tests = HEADER + `
//! scaled() against the CSS: each expected value is the theme's custom property with every var() substituted
//! (literal numbers), evaluated by the generator's CSS math evaluator in f64 — not through the palette fields.

use crate::*;

/// (theme index, text size, row scale, [${scaledDefs.map((d) => d.field).join(', ')}])
const CASES: &[(usize, f32, f32, [f64; ${scaledDefs.length}])] = &[
`;
THEMES.forEach((theme, ti) => {
  for (const n of steps) for (const r of rows) tests += `    (${ti}, ${f32(n)}, ${f32(r)}, [${scaledDefs.map((d) => f64(literalScaled(theme, d, n, r))).join(', ')}]),\n`;
});
tests += `];

#[test]
fn scaled_matches_the_css_at_every_step() {
    for &(ti, n, r, want) in CASES {
        let s = scaled(THEMES[ti].2, n, r);
        let got = [${scaledDefs.map((d) => `s.${d.field}`).join(', ')}];
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert!((*g as f64 - w).abs() <= 1e-5 * w.abs().max(1.0), "{} @ {n}px x{r}: field {i} = {g}, CSS says {w}", THEMES[ti].0);
        }
    }
}
`;

// ── component-rule listing ───────────────────────────────────────────────────────────────────────────
const NOTES = [
  [/^@import "@fontsource/, 'Fonts: the bundled TTFs in gpui-client/assets/fonts (see assets/README.md).'],
  [/^@import "tailwindcss"/, 'Tailwind: preflight (html line-height 1.5, box-sizing, margins reset), the default theme (`tailwind` module in decibell-theme) and the utilities.'],
  [/^\.font-tile$|^\.tracking-wordmark$/, 'Role → font-stretch: `font-tile` text uses `t_tile_stretch`, `tracking-wordmark` text `t_wordmark_stretch` (Mona Sans wdth axis).'],
  [/^\.chrome-ground$/, 'Background = `chrome_palette(id).color_bg_darkest` (the `--tc-chrome, --t-chrome` fallback), without re-scoping the palette.'],
  [/^\.list-row$/, 'padding = base × row_scale, base from the list container (`--list-row-pad-y/x`, `--list-row-gap`, default 6/10/10 px; MembersList 7/8/10, channel list 7/10/8, DMs 8/10/10, friends 8/8/12, AppearanceTab 7/8/10); `--avatar-scale` = row_scale (`Scaled::list_row_avatar_scale`).'],
  [/^\.sidebar-above-panel$/, 'padding-bottom = live UserPanel height (default 56 px) + 16 px.'],
  [/^\.fade-end$/, 'Bottom 10 px of the list fades out (mask).'],
  [/^\.channel-icon/, 'Channel-row glyph slot: 1.45 em square of the row text size, weight 500; the mic svg 0.82 em.'],
  [/^\.avatar-dot$/, 'Presence dot = (--avatar-dot-size, default 13 px; DMs 12 px) × --avatar-scale.'],
  [/video-meta-size/, 'Console themes colour the video meta size stamp with the accent.'],
  [/^\.theme-preview$/, 'Generated: `Scopes::theme_preview` (from AppearanceTab\'s preview data).'],
  [/^\.chrome-scope$|^:root\[data-theme="console-split"\] \.chrome-scope$/, 'Generated: `chrome_palette(id)`.'],
  [/^\.picture-editor$/, 'Generated: `Scopes::picture_editor`.'],
  [/^\.image-viewer$|\.image-viewer$/, 'Generated: `Scopes::image_viewer`.'],
  [/^button:not/, 'Pointer cursor on every enabled button / role=button.'],
  [/^body$/, 'Base text: `font-sans` (Palette::font_sans), weight 400, letter-spacing `t_ls_body`, `bg-bg-primary text-text-primary`, antialiased. The cv11/ss01/ss03 features do nothing in the renderer (its fontsource subsets lack them): leave them off on the bundled full fonts.'],
  [/^h1, h2/, 'Headings: letter-spacing -0.02em.'],
  [/scrollbar|^\* \{|^\*$/, 'Scrollbars: thin, thumb `t_scrollbar` (hover `t_scrollbar_hover`), 6 px wide, radius 3 px, transparent track. Note `--t-scrollbar` is not re-pointed by .chrome-scope (console-split chrome keeps the light thumb).'],
  [/^\*, \*::before|^input, textarea/, 'Text is not selectable except inputs, contenteditable and `.select-text`.'],
  [/password/, 'Web-only (password reveal icon tint).'],
  [/custom-slider/, 'Range slider thumb: 14 px circle, accent fill, 2 px ring of --slider-ring (default bg-light).'],
  [/picture-zoom/, 'Picture editor zoom slider: accent fill up to the thumb, the rest color-mix(text-muted 32%, transparent); thumb 14 px bg-light with 2 px accent border, glow 0 0 6px accent-mid, scale 1.15 while pressed; disabled: muted border, no glow.'],
  [/^html, body, #root$/, 'Full-height root.'],
  [/drop-pulse/, 'Drop-target glow: 1.5 px accent 60% ring + 22 px/4 px accent 22% bloom on a pseudo-layer whose opacity breathes (breathe, --pulse-dur default 1.6 s).'],
  [/bell-rise|bellRise/, 'BellMark entrance (login): bellRise 0.75 s cubic-bezier(0.2, 0.8, 0.2, 1); none under reduced motion.'],
  [/dsel/, 'Select (components/Select.tsx): per-theme-family styling — lacquer (nocturne/matinee), crisp (graphite*), terminal (console*); port by hand.'],
  [/rich-input/, 'Composer (contentEditable): min-height 1.4em, line-height 1.4, placeholder in text-muted with font-channel / text-channel at weight 400; inline emoji 1.375 em, vertical-align -0.3 em, 1 px side margins.'],
  [/tauri-drag-region/, 'Web-only: window drag regions (native: the title bar handles dragging).'],
  [/rich-codeblock/, 'Code-block syntax colours mapped onto palette tokens (accent-bright, accent, success, warning, text-muted italic, error, text-secondary).'],
  [/katex/, 'Display math left-aligned, no margin.'],
  [/viewer-/, 'Image viewer stage dressing (wash opacity/filter from the scope, vignette/head gradients mix --viewer-stage with transparent in oklab: = the stage colour at 75% / 80% / 45% alpha).'],
  [/prefers-reduced-motion/, 'Reduced motion overrides.'],
  [/^@layer base$/, 'Tailwind base layer (the rules under it).'],
  [/^@keyframes (fadeUp|dropIn|barNudge|cardIn|pulse-dot|waveBar|dropTargetIn|toastIn|pickerIn|tooltipIn)$/, 'Keyframes used by components through Tailwind `animate-[…]` classes; grep the name in the renderer for durations and easings.'],
  [/^@keyframes breathe$/, 'Keyframes of the opacity breathing (drop targets, the active server tile glow, the update dot).'],
];
const noteFor = (sel) => NOTES.find(([re]) => re.test(sel))?.[1] ?? '';
let doc = `<!-- @generated by gpui-client/tools/gen-theme/gen-theme.mjs from ${rel(IN.css)} — do not edit; check.sh fails while stale. -->
# globals.css: what is not a token

\`decibell-theme\` carries every custom property of \`${rel(IN.css)}\` (Palette, chrome palettes, Scaled, Scopes). Everything
below is **not** a token: component rules, base styles, keyframes and web-only rules that whoever ports the component
re-creates by hand. Line ranges are in globals.css as of this generation; \`var(--x)\` in a value is the token \`x\`
snake-cased (\`var(--color-accent)\` → \`Palette::color_accent\`). A note applies to the rows under it until the next note.

Also by hand, from the TSX rather than this file: inline \`style={{ … }}\` calc()s (e.g. MembersList's
\`--member-row-h: round(calc((AVATAR_PX + 2 × pad-y) × row-scale), 1px)\`: use \`decibell_theme::css_round\`), and Tailwind
utilities that bypass the theme: \`rounded\` = 4 px (not themed, unlike \`rounded-sm…xl\`), \`rounded-full\` = a pill,
\`animate-spin\` (1 s linear), \`animate-pulse\` (2 s cubic-bezier(0.4, 0, 0.6, 1), opacity 1 → 0.5 → 1),
\`text-red-400\` / \`bg-red-500\` (Tailwind oklch reds, used once each).

| Lines | Rule | Declarations | Note |
|---|---|---|---|
`;
const cell = (s) => s.replace(/\|/g, '\\|');
const declText = (decls) => decls.map((d) => (d.statement ? docEsc(d.statement) : `${d.name}: ${docEsc(d.value)}${d.important ? ' !important' : ''}`)).join('; ');
const lines = (n) => (n.startLine === n.endLine ? `${n.startLine}` : `${n.startLine}–${n.endLine}`);
const isTokenOnly = (node) => node.decls.length && node.decls.every((d) => d.name?.startsWith('--')) && !node.children.length
  && (node.prelude === '@theme' || splitTop(node.prelude, ',').every((s) => { const ps = parseSelector(s, 'doc'); const last = ps.comps[ps.comps.length - 1]; return last.root || SCOPES.some((sc) => last.classes.includes(sc.own ?? 'chrome-scope')); }));
let tokenBlocksCount = 0, lastNote = null;
const row = (ln, rule, decls, note) => {
  const shown = note && note !== lastNote ? note : '';
  if (note) lastNote = note;
  doc += `| ${ln} | ${rule} | ${cell(decls)} | ${cell(shown)} |\n`;
};
function listNode(node, depth) {
  for (const d of node.decls) if (d.statement && depth === 0) row(`${d.line}`, `\`${cell(docEsc(d.statement))}\``, '', noteFor(d.statement));
  for (const ch of node.children) {
    if (depth === 0 && isTokenOnly(ch) && !/list-row|dsel/.test(ch.prelude)) {
      tokenBlocksCount++;
      const note = noteFor(ch.prelude);
      if (note.startsWith('Generated')) row(lines(ch), `\`${cell(ch.prelude)}\``, `${ch.decls.length} custom properties`, note);
      continue;
    }
    const indent = depth ? '↳ '.repeat(depth) : '';
    const kf = ch.prelude.startsWith('@keyframes');
    const body = kf ? ch.children.map((c) => `**${c.prelude}** ${declText(c.decls)}`).join(' · ') : declText(ch.decls);
    row(lines(ch), `${indent}\`${cell(docEsc(ch.prelude))}\``, body, noteFor(ch.prelude) || (kf ? lastNote ?? '' : ''));
    if (!kf) listNode(ch, depth + 1);
  }
}
listNode(cssTree, 0);
doc += `\nToken-only blocks (\`@theme\`, \`:root\`, the theme palettes) are not listed: ${tokenBlocksCount} blocks, all in \`decibell-theme\`.\n`;

// ── sanity: the AppearanceTab swatches restate the palettes; warn when they drift ────────────────────
const warnings = [];
for (const t of THEME_INFO) {
  const pal = PALETTES[t.id], ch = CHROME[t.id];
  const want = { chrome: ch.get('--color-bg-darkest'), sidebar: ch.get('--color-bg-dark'), content: pal.get('--color-bg-mid'), divider: pal.get('--color-border-divider'), accent: pal.get('--color-accent'), textSecondary: pal.get('--color-text-secondary'), textMuted: pal.get('--color-text-muted') };
  for (const [k, v] of Object.entries(want)) { const got = quantise(parseColor(t.preview[k]) ?? fail(`${t.id} preview.${k}`)); if (got !== v) warnings.push(`AppearanceTab preview of ${t.id}: ${k} ${t.preview[k]} ≠ the palette's ${hex(v)}`); }
}

// ── write / check ────────────────────────────────────────────────────────────────────────────────────
const files = [[OUT.rs, rs], [OUT.tests, tests], [OUT.doc, doc]];
const check = process.argv.includes('--check');
const stale = [];
for (const [p, body] of files) {
  const cur = fs.existsSync(p) ? read(p) : null;
  if (cur !== body) { stale.push(rel(p)); if (!check) { fs.mkdirSync(path.dirname(p), { recursive: true }); fs.writeFileSync(p, body); } }
}
for (const w of warnings) console.error(`warning: ${w}`);
const chromeDiffs = THEMES.filter((t) => !same(PALETTES[t], CHROME[t]));
console.log(`${THEMES.length} themes · ${paletteFields.length} palette tokens · ${scaledDefs.length} scaled · ${SCOPE_STRUCTS.length} scopes (${SCOPE_STRUCTS.map((s) => `${s.sc.key}: ${s.fields.length}`).join(', ')}) · chrome palettes differ in ${chromeDiffs.join(', ') || 'none'} · ${twTokens.length} Tailwind defaults · skipped (one theme only): ${onlySome.length} --tc-*`);
if (process.argv.includes('--report')) {
  const kinds = {};
  for (const f of paletteFields) kinds[f.kind] = (kinds[f.kind] ?? 0) + 1;
  console.log('palette tokens by kind:', JSON.stringify(kinds));
  const fmt = (v) => JSON.stringify(v);
  console.log('tokens that differ between two themes (of ' + paletteFields.length + '):');
  console.log('                ' + THEMES.map((t) => t.slice(0, 13).padStart(14)).join(''));
  for (const a of THEMES) console.log(a.padEnd(16) + THEMES.map((b) => String(paletteFields.filter((f) => fmt(PALETTES[a].get(f.name)) !== fmt(PALETTES[b].get(f.name))).length).padStart(14)).join(''));
  for (const t of chromeDiffs) console.log(`${t}: chrome palette differs from the base in ${paletteFields.filter((f) => fmt(PALETTES[t].get(f.name)) !== fmt(CHROME[t].get(f.name))).length} tokens`);
}
if (check) {
  if (stale.length) { console.error(`STALE: ${stale.join(', ')} — run node gpui-client/tools/gen-theme/gen-theme.mjs`); process.exit(1); }
  console.log('current');
} else if (stale.length) console.log(`wrote ${stale.join(', ')}`);

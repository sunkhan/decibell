#!/usr/bin/env node
// Generates src/tables.rs from the renderer's own data, so the native client segments emoji,
// expands :shortcodes: and recognises code-block languages exactly as Electron does:
//
//   emoji-regex (electron-client/node_modules/emoji-regex, what MessageText / RichInput /
//     StatusMessageText call)   → a code-point matcher program (EMOJI_*), same backtracking order
//   @twemoji/svg file names (what scripts/build-twemoji-map.cjs keys twemoji-data.json with)
//                               → TWEMOJI_* (only keys Twemoji.tsx's toCodePoints can produce)
//   @emoji-mart/data (package main = sets/15/native.json) emojis[id].skins[0].native
//                               → SHORTCODE_*
//   CodeBlock.tsx's lowlight.register / registerAlias      → LANG_*
//
//   node gpui-client/crates/decibell-richtext/tools/gen-tables.mjs          (rewrite src/tables.rs)
//   node gpui-client/crates/decibell-richtext/tools/gen-tables.mjs --check  (exit 1 if stale)
//
// Zero dependencies beyond the renderer's node_modules. Run after an electron-client dependency
// bump (emoji-regex, @twemoji/svg, @emoji-mart/data, highlight.js) or a CodeBlock.tsx grammar change.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const crate = path.resolve(here, "..");
const repo = path.resolve(crate, "../../..");
const electron = path.join(repo, "electron-client");
const nm = path.join(electron, "node_modules");
const require = createRequire(path.join(electron, "package.json"));
const OUT = path.join(crate, "src", "tables.rs");

// ── emoji-regex → code-point program ────────────────────────────────────────────────────────
// The pattern is UTF-16 (no `u` flag) and uses only: literals (\uXXXX, \xXX, plain chars),
// classes [..] with ranges, non-capturing groups, `|` and greedy `?`. Every high surrogate is
// followed by a mandatory low surrogate, so the pattern converts losslessly to code points:
// `\uD83C(?:A|B)` becomes `(?:<D83C·A>|<D83C·B>)`, which keeps the backtracking order.
const reSource = require("emoji-regex")().source;
const reVersion = JSON.parse(fs.readFileSync(path.join(nm, "emoji-regex/package.json"), "utf8")).version;

function parseRegex(src) {
  let i = 0;
  const unit = () => {
    if (src[i] === "\\") {
      const k = src[i + 1];
      if (k === "u") { const v = parseInt(src.slice(i + 2, i + 6), 16); i += 6; return v; }
      if (k === "x") { const v = parseInt(src.slice(i + 2, i + 4), 16); i += 4; return v; }
      throw new Error(`unsupported escape \\${k} at ${i}`);
    }
    if ("()[]|?*+{}^$.".includes(src[i]) && false) throw new Error("meta");
    return src.charCodeAt(i++);
  };
  const alt = () => {
    const alts = [seq()];
    while (src[i] === "|") { i++; alts.push(seq()); }
    return alts.length === 1 ? alts[0] : { t: "alt", alts };
  };
  const seq = () => {
    const items = [];
    while (i < src.length && src[i] !== "|" && src[i] !== ")") {
      let a = atom();
      if (src[i] === "?") { i++; a = { t: "opt", x: a }; }
      if (src[i] === "*" || src[i] === "+" || src[i] === "{") throw new Error(`unsupported quantifier at ${i}`);
      items.push(a);
    }
    return { t: "seq", items };
  };
  const atom = () => {
    if (src.startsWith("(?:", i)) {
      i += 3;
      const x = alt();
      if (src[i] !== ")") throw new Error(`expected ) at ${i}`);
      i++;
      return x;
    }
    if (src[i] === "(" ) throw new Error(`capturing group at ${i}`);
    if (src[i] === "[") {
      i++;
      if (src[i] === "^") throw new Error("negated class");
      const r = [];
      while (src[i] !== "]") {
        const lo = unit();
        if (src[i] === "-" && src[i + 1] !== "]") { i++; r.push([lo, unit()]); } else r.push([lo, lo]);
      }
      i++;
      return { t: "cls", r };
    }
    if ("|)?*+{}^$.".includes(src[i])) throw new Error(`unexpected ${src[i]} at ${i}`);
    const u = unit();
    return { t: "cls", r: [[u, u]] };
  };
  const root = alt();
  if (i !== src.length) throw new Error(`trailing input at ${i}`);
  return root;
}

const isHigh = (u) => u >= 0xd800 && u <= 0xdbff;
const isLow = (u) => u >= 0xdc00 && u <= 0xdfff;
const clsKind = (c) => {
  const k = new Set(c.r.map(([a, b]) => (isHigh(a) && isHigh(b) ? "h" : isLow(a) && isLow(b) ? "l" : a > 0xdfff || b < 0xd800 ? "b" : "mixed")));
  if (k.size !== 1 || k.has("mixed")) throw new Error("class mixes surrogates and other units: " + JSON.stringify(c.r));
  return [...k][0];
};

// UTF-16 AST → code-point AST. A construct whose paths end on a high surrogate (the pattern has
// `\uD83D(?:\uDC8B‍\uD83D)?\uDC68`) gets the rest of its sequence distributed into it:
// `X? rest` → `(?:X rest|rest)`, `(?:A|B) rest` → `(?:A rest|B rest)`; both keep the order in
// which the backtracking engine tries the paths, so the first match found is the same.
class Dangling extends Error {}
function conv(n) {
  switch (n.t) {
    case "cls":
      if (clsKind(n) !== "b") throw new Dangling("surrogate at a construct boundary");
      return n;
    case "opt": return { t: "opt", x: conv(n.x) };
    case "alt": return { t: "alt", alts: n.alts.map(conv) };
    case "seq": return convList(n.items);
  }
  throw new Error(n.t);
}
function convList(items) {
  const out = [];
  for (let k = 0; k < items.length; k++) {
    const it = items[k];
    const rest = items.slice(k + 1);
    if (it.t === "cls" && clsKind(it) === "h") { out.push(prefixHigh(it.r, rest)); break; }
    if (it.t === "cls" && clsKind(it) === "l") throw new Dangling("low surrogate without a high one");
    let c;
    try { c = conv(it); } catch (e) {
      if (!(e instanceof Dangling) || rest.length === 0) throw e;
      out.push(distribute(it, rest));
      break;
    }
    out.push(c);
  }
  return { t: "seq", items: out };
}
function distribute(it, rest) {
  if (it.t === "opt") return { t: "alt", alts: [convList([it.x, ...rest]), convList(rest)] };
  if (it.t === "alt") return { t: "alt", alts: it.alts.map((a) => convList([a, ...rest])) };
  if (it.t === "seq") return convList([...it.items, ...rest]);
  throw new Error("cannot distribute " + it.t);
}
function prefixHigh(highs, rest) {
  if (rest.length === 0) throw new Dangling("high surrogate at end of sequence");
  const [first, ...tail] = rest;
  if (first.t === "cls") {
    if (clsKind(first) !== "l") throw new Error("high surrogate not followed by low");
    const r = [];
    for (const [h1, h2] of highs)
      for (let h = h1; h <= h2; h++)
        for (const [l1, l2] of first.r)
          r.push([(h - 0xd800) * 0x400 + (l1 - 0xdc00) + 0x10000, (h - 0xd800) * 0x400 + (l2 - 0xdc00) + 0x10000]);
    return { t: "seq", items: [{ t: "cls", r }, ...convList(tail).items] };
  }
  if (first.t === "opt") return { t: "alt", alts: [prefixHigh(highs, [first.x, ...tail]), prefixHigh(highs, tail)] };
  if (first.t === "alt") return { t: "alt", alts: first.alts.map((a) => prefixHigh(highs, [a, ...tail])) };
  if (first.t === "seq") return prefixHigh(highs, [...first.items, ...tail]);
  throw new Error("unexpected " + first.t);
}

// Simplify: flatten single-item seqs and nested seqs, merge class ranges.
function simplify(n) {
  if (n.t === "cls") {
    const r = n.r.slice().sort((a, b) => a[0] - b[0]);
    const m = [];
    for (const [a, b] of r) {
      if (m.length && a <= m[m.length - 1][1] + 1) m[m.length - 1][1] = Math.max(m[m.length - 1][1], b);
      else m.push([a, b]);
    }
    return { t: "cls", r: m };
  }
  if (n.t === "opt") return { t: "opt", x: simplify(n.x) };
  if (n.t === "alt") return { t: "alt", alts: n.alts.map(simplify) };
  const items = [];
  for (const it of n.items.map(simplify)) {
    if (it.t === "seq") items.push(...it.items);
    else items.push(it);
  }
  return items.length === 1 ? items[0] : { t: "seq", items };
}

// Sanity: no alternative may match the empty string (matchAll would loop / advance by one).
function nullable(n) {
  switch (n.t) {
    case "cls": return false;
    case "opt": return true;
    case "alt": return n.alts.some(nullable);
    case "seq": return n.items.every(nullable);
  }
}
function firstSet(n, acc) {
  switch (n.t) {
    case "cls": acc.push(...n.r); return false; // not nullable
    case "opt": firstSet(n.x, acc); return true;
    case "alt": { let nl = false; for (const a of n.alts) nl = firstSet(a, acc) || nl; return nl; }
    case "seq": { for (const it of n.items) if (!firstSet(it, acc)) return false; return true; }
  }
}

const utf16Root = parseRegex(reSource);
const root = simplify(conv(utf16Root));
if (nullable(root)) throw new Error("emoji regex can match empty");
const first = [];
firstSet(root, first);
const firstMerged = simplify({ t: "cls", r: first }).r;

// Flatten into tables: node = [kind, a, b]; kind 0 = class(ranges[a..a+b]), 1 = seq(kids[a..a+b]),
// 2 = alt(kids[a..a+b]), 3 = opt(child a).
const nodes = [];
const kids = [];
const ranges = [];
const memoCls = new Map();
function emit(n) {
  if (n.t === "cls") {
    const key = JSON.stringify(n.r);
    if (memoCls.has(key)) return memoCls.get(key);
    const at = ranges.length;
    ranges.push(...n.r);
    nodes.push([0, at, n.r.length]);
    memoCls.set(key, nodes.length - 1);
    return nodes.length - 1;
  }
  if (n.t === "opt") {
    const c = emit(n.x);
    nodes.push([3, c, 0]);
    return nodes.length - 1;
  }
  const list = n.t === "seq" ? n.items : n.alts;
  const ids = list.map(emit);
  const at = kids.length;
  kids.push(...ids);
  nodes.push([n.t === "seq" ? 1 : 2, at, ids.length]);
  return nodes.length - 1;
}
const rootId = emit(root);
// Per root alternative, the code points it can start with (match_at skips the others), and the
// check behind the ASCII fast path: only the keycap alternative `[#*0-9]\uFE0F?\u20E3` starts
// with ASCII.
if (root.t !== "alt") throw new Error("root is not an alternation");
const rootFirst = root.alts.map((a) => { const acc = []; firstSet(a, acc); return simplify({ t: "cls", r: acc }).r; });
rootFirst.forEach((r, k) => {
  const ascii = r.filter(([a]) => a < 0x80);
  if (k === 0) {
    if (JSON.stringify(ascii) !== JSON.stringify([[0x23, 0x23], [0x2a, 0x2a], [0x30, 0x39]])) throw new Error("keycap alternative changed");
  } else if (ascii.length) throw new Error("an alternative other than the keycap starts with ASCII");
});
const ka = root.alts[0];
if (JSON.stringify(ka) !== JSON.stringify({ t: "seq", items: [{ t: "cls", r: [[0x23, 0x23], [0x2a, 0x2a], [0x30, 0x39]] }, { t: "opt", x: { t: "cls", r: [[0xfe0f, 0xfe0f]] } }, { t: "cls", r: [[0x20e3, 0x20e3]] }] }))
  throw new Error("keycap alternative is not [#*0-9]\\uFE0F?\\u20E3: " + JSON.stringify(ka));
if (nodes.length > 0xffff || kids.length > 0xffff || ranges.length > 0xffff) throw new Error("table too big for u16 indices");

// ── Twemoji asset keys ──────────────────────────────────────────────────────────────────────
// toCodePoints(match) drops U+FE0F from every multi-code-point sequence, so a file name with
// "fe0f" in a multi-part name can never be looked up; those are left out (and counted).
const twemojiDir = path.join(nm, "@twemoji/svg");
const twemojiVersion = JSON.parse(fs.readFileSync(path.join(twemojiDir, "package.json"), "utf8")).version;
const allKeys = fs.readdirSync(twemojiDir).filter((f) => f.endsWith(".svg")).map((f) => f.slice(0, -4));
const reachable = allKeys.filter((k) => { const p = k.split("-"); return p.length === 1 || !p.includes("fe0f"); });
const keyChars = (k) => String.fromCodePoint(...k.split("-").map((h) => parseInt(h, 16)));
const twemoji = reachable.map((k) => ({ k, s: keyChars(k), b: Buffer.from(keyChars(k), "utf8") }));
twemoji.sort((a, b) => Buffer.compare(a.b, b.b));

// ── Shortcodes ──────────────────────────────────────────────────────────────────────────────
const martPkg = JSON.parse(fs.readFileSync(path.join(nm, "@emoji-mart/data/package.json"), "utf8"));
const mart = JSON.parse(fs.readFileSync(path.join(nm, "@emoji-mart/data", martPkg.main), "utf8"));
const shortcodes = [];
for (const [id, e] of Object.entries(mart.emojis)) {
  const native = e.skins?.[0]?.native;
  if (native) shortcodes.push({ id, native });
}
shortcodes.sort((a, b) => Buffer.compare(Buffer.from(a.id), Buffer.from(b.id)));

// ── Code languages (CodeBlock.tsx) ──────────────────────────────────────────────────────────
const codeBlockSrc = fs.readFileSync(path.join(electron, "src/features/chat/CodeBlock.tsx"), "utf8");
const imports = [...codeBlockSrc.matchAll(/import (\w+) from "highlight\.js\/lib\/languages\/([\w-]+)";/g)].map((m) => ({ name: m[1], file: m[2] }));
const regBlock = codeBlockSrc.match(/lowlight\.register\(\{([^}]*)\}\);/);
if (!regBlock) throw new Error("lowlight.register block not found");
const registered = regBlock[1].split(",").map((s) => s.trim()).filter(Boolean);
const extraAliases = [...codeBlockSrc.matchAll(/lowlight\.registerAlias\(\{\s*(\w+):\s*\[([^\]]*)\]\s*\}\)/g)].map((m) => ({ lang: m[1], aliases: m[2].split(",").map((s) => s.trim().replace(/^"|"$/g, "")) }));
const hljs = (await import(pathToFileURL(path.join(nm, "highlight.js/lib/core.js")).href)).default;
const langMap = new Map(); // alias or name (lowercase) → grammar name
for (const name of registered) {
  const imp = imports.find((x) => x.name === name);
  if (!imp) throw new Error(`no import for ${name}`);
  const def = (await import(pathToFileURL(path.join(nm, `highlight.js/lib/languages/${imp.file}.js`)).href)).default;
  hljs.registerLanguage(name, def);
}
for (const { lang, aliases } of extraAliases) hljs.registerAliases(aliases, { languageName: lang });
// lowlight's registered(x) is hljs.getLanguage(x): languages[x] || languages[aliases[x]] on
// null-prototype objects after toLowerCase (a name beats an alias; a later alias registration
// overwrites an earlier one). Ask hljs itself for every candidate key.
const byObject = new Map(registered.map((name) => [hljs.getLanguage(name), name]));
const candidates = new Set(registered.map((n) => n.toLowerCase()));
for (const name of registered) for (const a of hljs.getLanguage(name).aliases ?? []) candidates.add(a.toLowerCase());
for (const { aliases } of extraAliases) for (const a of aliases) candidates.add(a.toLowerCase());
for (const key of candidates) {
  const obj = hljs.getLanguage(key);
  if (!obj) continue;
  if (!byObject.has(obj)) throw new Error(`alias ${key} resolves to an unregistered grammar`);
  langMap.set(key, byObject.get(obj));
}
const langs = [...langMap.entries()].map(([alias, name]) => ({ alias, name })).sort((a, b) => (a.alias < b.alias ? -1 : a.alias > b.alias ? 1 : 0));
const grammarNames = [...new Set(langs.map((l) => l.name))].sort();
const hlVersion = JSON.parse(fs.readFileSync(path.join(nm, "highlight.js/package.json"), "utf8")).version;

// ── Emit ────────────────────────────────────────────────────────────────────────────────────
const hex = (v) => "0x" + v.toString(16).toUpperCase();
const lines = [];
const w = (s) => lines.push(s);
w("// @generated by tools/gen-tables.mjs — do not edit. Regenerate after an electron-client");
w("// dependency bump (emoji-regex, @twemoji/svg, @emoji-mart/data, highlight.js) or a CodeBlock.tsx change.");
w(`// Sources: emoji-regex ${reVersion}, @twemoji/svg ${twemojiVersion}, @emoji-mart/data ${martPkg.version} (${martPkg.main}),`);
w(`// highlight.js ${hlVersion} grammars registered in CodeBlock.tsx.`);
w("#![allow(clippy::unreadable_literal)]");
w("");
w("// ── emoji-regex as a code-point program ──");
w(`// ${nodes.length} nodes, ${kids.length} child links, ${ranges.length} ranges; root ${rootId}.`);
w("// Node: (kind, a, b). kind 0 = class ranges[a..a+b], 1 = sequence kids[a..a+b],");
w("// 2 = ordered alternation kids[a..a+b], 3 = greedy optional of node a.");
w(`pub(crate) const EMOJI_ROOT: u16 = ${rootId};`);
w(`pub(crate) static EMOJI_NODES: [(u8, u16, u16); ${nodes.length}] = [`);
for (let k = 0; k < nodes.length; k += 8) w("    " + nodes.slice(k, k + 8).map(([a, b, c]) => `(${a},${b},${c})`).join(", ") + ",");
w("];");
w(`pub(crate) static EMOJI_KIDS: [u16; ${kids.length}] = [`);
for (let k = 0; k < kids.length; k += 20) w("    " + kids.slice(k, k + 20).join(",") + ",");
w("];");
w(`pub(crate) static EMOJI_RANGES: [(u32, u32); ${ranges.length}] = [`);
for (let k = 0; k < ranges.length; k += 6) w("    " + ranges.slice(k, k + 6).map(([a, b]) => `(${hex(a)},${hex(b)})`).join(", ") + ",");
w("];");
w("/// Per root alternative (in order): the code points it can start with.");
w(`pub(crate) static EMOJI_ROOT_FIRST: [&[(u32, u32)]; ${rootFirst.length}] = [`);
for (const r of rootFirst) w("    &[" + r.map(([a, b]) => `(${hex(a)},${hex(b)})`).join(", ") + "],");
w("];");
w("/// Code points that can start a match (union of the program's first classes).");
w(`pub(crate) static EMOJI_FIRST: [(u32, u32); ${firstMerged.length}] = [`);
for (let k = 0; k < firstMerged.length; k += 6) w("    " + firstMerged.slice(k, k + 6).map(([a, b]) => `(${hex(a)},${hex(b)})`).join(", ") + ",");
w("];");
w("");

const twBlob = Buffer.concat(twemoji.map((t) => t.b));
const twOff = [0];
for (const t of twemoji) twOff.push(twOff[twOff.length - 1] + t.b.length);
if (twBlob.length > 0xffff) throw new Error("twemoji blob too big for u16 offsets");
w("// ── Twemoji assets ──");
w(`// ${twemoji.length} of ${allKeys.length} @twemoji/svg ${twemojiVersion} files: the ones Twemoji.tsx's toCodePoints can name`);
w(`// (${allKeys.length - twemoji.length} multi-code-point names keep U+FE0F and are unreachable). Keys are the sequences`);
w("// as UTF-8 (U+FE0F dropped from multi-code-point ones), sorted bytewise; asset id = index.");
w(`pub(crate) static TWEMOJI_BLOB: &str = ${JSON.stringify(twemoji.map((t) => t.s).join(""))};`);
w(`pub(crate) static TWEMOJI_OFF: [u16; ${twOff.length}] = [`);
for (let k = 0; k < twOff.length; k += 20) w("    " + twOff.slice(k, k + 20).join(",") + ",");
w("];");
w("");

const scIds = Buffer.from(shortcodes.map((s) => s.id).join(""), "utf8");
const scNat = Buffer.from(shortcodes.map((s) => s.native).join(""), "utf8");
const scIdOff = [0], scNatOff = [0];
for (const s of shortcodes) { scIdOff.push(scIdOff[scIdOff.length - 1] + Buffer.byteLength(s.id)); scNatOff.push(scNatOff[scNatOff.length - 1] + Buffer.byteLength(s.native)); }
if (scIds.length > 0xffff || scNat.length > 0xffff) throw new Error("shortcode blob too big");
w("// ── :shortcodes: ──");
w(`// ${shortcodes.length} @emoji-mart/data ids (sorted bytewise) → emojis[id].skins[0].native.`);
w(`pub(crate) static SHORTCODE_IDS: &str = ${JSON.stringify(shortcodes.map((s) => s.id).join(""))};`);
w(`pub(crate) static SHORTCODE_ID_OFF: [u16; ${scIdOff.length}] = [`);
for (let k = 0; k < scIdOff.length; k += 20) w("    " + scIdOff.slice(k, k + 20).join(",") + ",");
w("];");
w(`pub(crate) static SHORTCODE_NATIVES: &str = ${JSON.stringify(shortcodes.map((s) => s.native).join(""))};`);
w(`pub(crate) static SHORTCODE_NATIVE_OFF: [u16; ${scNatOff.length}] = [`);
for (let k = 0; k < scNatOff.length; k += 20) w("    " + scNatOff.slice(k, k + 20).join(",") + ",");
w("];");
w("");

w("// ── Code-block languages (CodeBlock.tsx: lowlight.registered) ──");
w(`/// Grammar names registered in CodeBlock.tsx (highlight.js ${hlVersion}), sorted.`);
w(`pub static GRAMMARS: [&str; ${grammarNames.length}] = [${grammarNames.map((s) => JSON.stringify(s)).join(", ")}];`);
w("/// Every name or alias `lowlight.registered()` accepts (lower-case, sorted) → index into GRAMMARS.");
w(`pub(crate) static LANG_ALIASES: [(&str, u8); ${langs.length}] = [`);
for (let k = 0; k < langs.length; k += 6) w("    " + langs.slice(k, k + 6).map((l) => `(${JSON.stringify(l.alias)}, ${grammarNames.indexOf(l.name)})`).join(", ") + ",");
w("];");
w("");

const text = lines.join("\n");
if (process.argv.includes("--check")) {
  const cur = fs.existsSync(OUT) ? fs.readFileSync(OUT, "utf8") : "";
  if (cur !== text) { console.error("src/tables.rs is stale: run tools/gen-tables.mjs"); process.exit(1); }
  console.log("src/tables.rs is current");
} else {
  fs.writeFileSync(OUT, text);
}
const sz = nodes.length * 6 + kids.length * 2 + ranges.length * 8 + firstMerged.length * 8;
console.error(`emoji program: ${nodes.length} nodes, ${kids.length} kids, ${ranges.length} ranges, first ${firstMerged.length} → ${sz} bytes`);
console.error(`twemoji: ${twemoji.length}/${allKeys.length} keys → ${twBlob.length + twOff.length * 2} bytes`);
console.error(`shortcodes: ${shortcodes.length} → ${scIds.length + scNat.length + (scIdOff.length + scNatOff.length) * 2} bytes`);
console.error(`languages: ${langs.length} aliases, ${grammarNames.length} grammars`);

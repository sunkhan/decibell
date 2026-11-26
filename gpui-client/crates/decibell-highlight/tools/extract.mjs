#!/usr/bin/env node
// Build-time extractor: loads the real highlight.js grammars the Electron
// client registers (CodeBlock.tsx), lets highlight.js compile them, and
// dumps the compiled mode graphs as data for the Rust engine.
//
//   node tools/extract.mjs            regenerate data/*.hlg, src/generated.rs,
//                                     tests/vectors/joined.txt
//   node tools/extract.mjs --check    fail if the committed output is stale
//   node tools/extract.mjs --update-pins   accept new highlight.js sources
//                                     (after reviewing their callbacks)
//   node tools/extract.mjs --json DIR also write readable JSON per language
//
// Functions can't be serialised, so every function met in a compiled grammar
// must be listed in CALLBACKS below (matched by its exact source text) and
// has a hand-written twin in src/callbacks.rs. An unknown function, an
// unknown mode field or a changed source file fails the run.

import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { resolve } from "node:path";
import {
  CRATE_DIR, TOOLS_DIR, HLJS_DIR, LOWLIGHT_DIR, CORE_FILE,
  grammarFile, loadCore, loadGrammar, pkgVersion, readCodeBlock, sha256,
} from "./common.mjs";

const args = process.argv.slice(2);
const CHECK = args.includes("--check");
const UPDATE_PINS = args.includes("--update-pins");
const JSON_DIR = args.includes("--json") ? args[args.indexOf("--json") + 1] : null;

// ───────────────────────── callbacks ─────────────────────────
// id → { slot kinds it may appear in, exact source text(s) }. The Rust enum
// `Callback` is generated from this table in this order; ids are stable.
const CALLBACKS = [
  {
    id: "SkipIfHasPrecedingDot",
    doc: "core.js skipIfHasPrecedingDot: the internal __beforeBegin of every beginKeywords mode",
    sources: [
      `function skipIfHasPrecedingDot(match, response) {
  const before = match.input[match.index - 1];
  if (before === ".") {
    response.ignoreMatch();
  }
}`,
    ],
  },
  {
    id: "ShebangAtStart",
    doc: "hljs.SHEBANG on:begin: only at index 0",
    sources: [
      `(m, resp) => {
      if (m.index !== 0) resp.ignoreMatch();
    }`,
    ],
  },
  {
    id: "SaveBeginMatch",
    doc: "hljs.END_SAME_AS_BEGIN on:begin: data._beginMatch = m[1]",
    sources: [`(m, resp) => { resp.data._beginMatch = m[1]; }`],
  },
  {
    id: "EndSameAsBegin",
    doc: "hljs.END_SAME_AS_BEGIN on:end (php HEREDOC uses the same text): ignore unless m[1] === data._beginMatch",
    sources: [`(m, resp) => { if (resp.data._beginMatch !== m[1]) resp.ignoreMatch(); }`],
  },
  {
    id: "SaveBeginMatch1Or2",
    doc: "php HEREDOC on:begin: data._beginMatch = m[1] || m[2]",
    sources: [`(m, resp) => { resp.data._beginMatch = m[1] || m[2]; }`],
  },
  {
    id: "JsIsTrulyOpeningTag",
    doc: "javascript / typescript XML_TAG.isTrulyOpeningTag (JSX vs generics look-ahead)",
    sources: [
      `(match, response) => {
      const afterMatchIndex = match[0].length + match.index;
      const nextChar = match.input[afterMatchIndex];
      if (
        // HTML should not include another raw \`<\` inside a tag
        // nested type?
        // \`<Array<Array<number>>\`, etc.
        nextChar === "<" ||
        // the , gives away that this is not HTML
        // \`<T, A extends keyof T, V>\`
        nextChar === ","
        ) {
        response.ignoreMatch();
        return;
      }

      // \`<something>\`
      // Quite possibly a tag, lets look for a matching closing tag...
      if (nextChar === ">") {
        // if we cannot find a matching closing tag, then we
        // will ignore it
        if (!hasClosingTag(match, { after: afterMatchIndex })) {
          response.ignoreMatch();
        }
      }

      // \`<blah />\` (self-closing)
      // handled by simpleSelfClosing rule

      let m;
      const afterMatch = match.input.substring(afterMatchIndex);

      // some more template typing stuff
      //  <T = any>(key?: string) => Modify<
      if ((m = afterMatch.match(/^\\s*=/))) {
        response.ignoreMatch();
        return;
      }

      // \`<From extends string>\`
      // technically this could be HTML, but it smells like a type
      // NOTE: This is ugh, but added specifically for https://github.com/highlightjs/highlight.js/issues/3276
      if ((m = afterMatch.match(/^\\s+extends\\s+/))) {
        if (m.index === 0) {
          response.ignoreMatch();
          // eslint-disable-next-line no-useless-return
          return;
        }
      }
    }`,
    ],
  },
];
const CALLBACK_BY_SOURCE = new Map();
for (const cb of CALLBACKS) for (const s of cb.sources) CALLBACK_BY_SOURCE.set(s, cb.id);

function callbackId(fn, where) {
  if (fn == null) return 0;
  if (typeof fn !== "function") throw new Error(`${where}: expected a function`);
  const id = CALLBACK_BY_SOURCE.get(fn.toString());
  if (!id) {
    throw new Error(
      `${where}: a callback this extractor has no identifier for. Port it to ` +
        `src/callbacks.rs and add it to CALLBACKS in tools/extract.mjs.\n--- source ---\n${fn.toString()}\n--------------`,
    );
  }
  return CALLBACKS.findIndex((c) => c.id === id) + 1;
}

// ───────────────────────── pins ─────────────────────────
const { names: LANGS, aliases: EXTRA_ALIASES } = readCodeBlock();
const hljsVersion = pkgVersion(HLJS_DIR);
const lowlightVersion = pkgVersion(LOWLIGHT_DIR);
const pinsPath = resolve(TOOLS_DIR, "pins.json");
const pinsNow = {
  "highlight.js": hljsVersion,
  lowlight: lowlightVersion,
  files: Object.fromEntries([
    ["lib/core.js", sha256(readFileSync(resolve(HLJS_DIR, "lib/core.js")))],
    ["es/core.js", sha256(readFileSync(CORE_FILE))],
    ...LANGS.map((n) => [`es/languages/${n}.js`, sha256(readFileSync(grammarFile(n)))]),
    ["lowlight/lib/index.js", sha256(readFileSync(resolve(LOWLIGHT_DIR, "lib/index.js")))],
  ]),
};
if (UPDATE_PINS || !existsSync(pinsPath)) {
  writeFileSync(pinsPath, JSON.stringify(pinsNow, null, 2) + "\n");
} else {
  const pinned = JSON.parse(readFileSync(pinsPath, "utf8"));
  const diffs = [];
  for (const k of ["highlight.js", "lowlight"]) {
    if (pinned[k] !== pinsNow[k]) diffs.push(`${k}: pinned ${pinned[k]}, installed ${pinsNow[k]}`);
  }
  for (const f of new Set([...Object.keys(pinned.files), ...Object.keys(pinsNow.files)])) {
    if (pinned.files[f] !== pinsNow.files[f]) diffs.push(`${f}: changed`);
  }
  if (diffs.length) {
    throw new Error(
      "The installed highlight.js / lowlight differ from tools/pins.json:\n  " +
        diffs.join("\n  ") +
        "\nReview the changed files (callbacks and engine behaviour: see README, 'Upgrading'), " +
        "then rerun with --update-pins.",
    );
  }
}

// ───────────────────────── load + compile ─────────────────────────
// Records the pattern of the latest `new RegExp(pattern, flags)`.
let lastPattern = null;
const NativeRegExp = globalThis.RegExp;
globalThis.RegExp = new Proxy(NativeRegExp, {
  construct(target, args, newTarget) {
    lastPattern = args[0];
    return Reflect.construct(target, args, newTarget === globalThis.RegExp ? target : newTarget);
  },
});
const HighlightJs = await loadCore();
const high = HighlightJs.newInstance();
for (const n of LANGS) high.registerLanguage(n, await loadGrammar(n));
for (const [lang, list] of Object.entries(EXTRA_ALIASES)) {
  high.registerAliases(list, { languageName: lang });
}
// compileLanguage runs inside the first highlight of each language
for (const n of LANGS) {
  const r = high.highlight("", { language: n, ignoreIllegals: true });
  if (r.errorRaised) throw r.errorRaised;
}

// countMatchGroups, verbatim from core.js
function countMatchGroups(re) {
  return new RegExp(re.toString() + "|").exec("").length - 1;
}
function source(re) {
  if (!re) return null;
  if (typeof re === "string") return re;
  return re.source;
}

const MODE_KEYS = new Set([
  // read by the engine at run time
  "scope", "begin", "end", "beginRe", "endRe", "terminatorEnd", "illegal", "illegalRe",
  "keywords", "keywordPatternRe", "relevance", "contains", "starts", "matcher",
  "skip", "excludeBegin", "returnBegin", "excludeEnd", "returnEnd",
  "endsWithParent", "endsParent", "beginScope", "endScope", "subLanguage",
  "on:begin", "on:end", "__beforeBegin", "data",
  // compile-time only (already applied) or unused by the highlighter
  "isCompiled", "variants", "cachedVariants", "label", "match", "className",
  "beginKeywords", "beforeMatch", "lexemes",
  "binary", // hljs.SHEBANG option, folded into `begin` when the mode is built
]);
const LANG_KEYS = new Set([
  ...MODE_KEYS,
  "name", "aliases", "case_insensitive", "unicodeRegex", "disableAutodetect",
  "classNameAliases", "compilerExtensions", "rawDefinition", "exports", "supersetOf",
]);

// Relevance is a JS number (php has a 0.1); written in thousandths, and
// `n / 1000` in f64 is the same double as the literal.
function isInt(n) {
  return typeof n === "number" && n >= 0 && n < 1000 && Math.round(n * 1000) / 1000 === n;
}
const milli = (n) => Math.round(n * 1000);

class Out {
  constructor() { this.chunks = []; }
  u8(n) { this.chunks.push(Buffer.from([n])); }
  varint(n) {
    if (!Number.isInteger(n) || n < 0) throw new Error(`varint: ${n}`);
    const bytes = [];
    do {
      let b = n & 0x7f;
      n = Math.floor(n / 128);
      if (n) b |= 0x80;
      bytes.push(b);
    } while (n);
    this.chunks.push(Buffer.from(bytes));
  }
  str(s) {
    if (!s.isWellFormed()) throw new Error("lone surrogate in a grammar string");
    const b = Buffer.from(s, "utf8");
    this.varint(b.length);
    this.chunks.push(b);
  }
  bytes() { return Buffer.concat(this.chunks); }
}

const allScopes = new Set();
const usedCallbacks = new Map(); // id → Set(lang)
const joinedVectors = [];
const report = [];

function extractLanguage(langName) {
  const lang = high.getLanguage(langName);
  if (!lang.isCompiled) throw new Error(`${langName}: not compiled`);
  if (lang.__emitTokens) throw new Error(`${langName}: __emitTokens grammars are not supported`);
  if (lang.compilerExtensions && lang.compilerExtensions.length) {
    throw new Error(`${langName}: compilerExtensions present; review them (they are functions)`);
  }
  if (lang.supersetOf) {
    throw new Error(`${langName}: supersetOf is set; port the tie-break in auto-detection first`);
  }
  for (const k of Object.keys(lang)) {
    if (!LANG_KEYS.has(k)) throw new Error(`${langName}: unknown language field "${k}"`);
  }

  const strings = [];
  const stringIdx = new Map();
  const S = (s) => {
    if (typeof s !== "string") throw new Error(`${langName}: expected a string, got ${typeof s}`);
    let i = stringIdx.get(s);
    if (i === undefined) {
      i = strings.length;
      strings.push(s);
      stringIdx.set(s, i);
    }
    return i;
  };
  const scope = (s) => {
    allScopes.add(s);
    return S(s);
  };

  const kwTables = [];
  const kwIdx = new Map();
  const modes = [];
  const modeIdx = new Map();
  const queue = [];
  const idOf = (m) => {
    let i = modeIdx.get(m);
    if (i === undefined) {
      i = modes.length;
      modes.push(null);
      modeIdx.set(m, i);
      queue.push(m);
    }
    return i;
  };
  idOf(lang);

  const classNameAliases = Object.entries(lang.classNameAliases || {});
  for (const [, v] of classNameAliases) allScopes.add(v);

  while (queue.length) {
    const m = queue.shift();
    const id = modeIdx.get(m);
    const isRoot = m === lang;
    const where = `${langName} mode #${id}${m.scope ? ` (${m.scope})` : ""}`;
    for (const k of Object.keys(m)) {
      if (!(isRoot ? LANG_KEYS : MODE_KEYS).has(k)) throw new Error(`${where}: unknown mode field "${k}"`);
      if (typeof m[k] === "function" && !["on:begin", "on:end", "__beforeBegin", "rawDefinition"].includes(k)) {
        throw new Error(`${where}: function-valued field "${k}"`);
      }
    }
    if (!m.isCompiled) throw new Error(`${where}: not compiled`);
    if (m.scope != null && typeof m.scope !== "string") throw new Error(`${where}: non-string scope`);
    if (!isInt(m.relevance)) throw new Error(`${where}: relevance ${m.relevance}`);
    // a mode whose text would be emitted twice by the engine; the Rust port
    // tracks the buffer as a source range and can't express that
    if (m.skip && (m.returnBegin || m.returnEnd)) throw new Error(`${where}: skip with returnBegin/returnEnd`);
    if (m.excludeBegin && m.returnBegin) throw new Error(`${where}: excludeBegin with returnBegin`);
    if (m.returnEnd && (m.excludeEnd || m.endScope)) throw new Error(`${where}: returnEnd with excludeEnd/endScope`);
    for (const sub of [].concat(m.subLanguage ?? [])) {
      // a sub-language shows up in the output under the name written here;
      // the engine reports it as a `Language`, so it must be a registered name
      if (high.getLanguage(sub) && !LANGS.includes(sub)) throw new Error(`${where}: subLanguage "${sub}" is an alias`);
    }

    const o = { id };
    o.flags =
      (m.skip ? 1 : 0) | (m.excludeBegin ? 2 : 0) | (m.returnBegin ? 4 : 0) |
      (m.excludeEnd ? 8 : 0) | (m.returnEnd ? 16 : 0) | (m.endsWithParent ? 32 : 0) |
      (m.endsParent ? 64 : 0);
    o.scope = m.scope ? scope(m.scope) + 1 : 0;
    o.relevance = m.relevance;

    const re = (v) => {
      if (v == null || v === "") return { s: 0, groups: 0 };
      return { s: S(source(v)) + 1, groups: countMatchGroups(v) };
    };
    if (isRoot) {
      o.begin = { s: 0, groups: 0 };
      o.endRe = 0;
      o.term = { s: 0, groups: 0 };
    } else {
      o.begin = re(m.begin);
      o.endRe = m.endRe ? S(m.endRe.source) + 1 : 0;
      // buildModeRegex adds the end rule only when terminatorEnd is truthy
      o.term = m.terminatorEnd ? { s: S(m.terminatorEnd) + 1, groups: countMatchGroups(m.terminatorEnd) } : { s: 0, groups: 0 };
    }
    o.illegal = m.illegal ? re(m.illegal) : { s: 0, groups: 0 };

    // keywords (compiled: word → [scope, relevance])
    if (m.keywords) {
      const words = Object.keys(m.keywords).sort();
      const rows = words.map((w) => {
        const [kind, rel] = m.keywords[w];
        if (!isInt(rel)) throw new Error(`${where}: keyword relevance ${rel}`);
        if (!kind.startsWith("_")) allScopes.add(kind);
        return [w, kind, rel];
      });
      const pattern = m.keywordPatternRe.source;
      const key = JSON.stringify([pattern, rows]);
      let k = kwIdx.get(key);
      if (k === undefined) {
        k = kwTables.length;
        kwTables.push({ pattern: S(pattern), rows: rows.map(([w, kind, rel]) => [w, S(kind), rel]) });
        kwIdx.set(key, k);
      }
      o.keywords = k + 1;
    } else {
      o.keywords = 0;
    }

    const scopeSpec = (sc, what) => {
      if (!sc) return { kind: 0 };
      if (sc._wrap) return { kind: 1, wrap: scope(sc._wrap) };
      if (sc._multi) {
        const list = [];
        for (const k of Object.keys(sc)) {
          if (k.startsWith("_")) continue;
          const n = Number(k);
          if (!Number.isInteger(n) || !sc._emit[n]) throw new Error(`${where}: odd ${what} key ${k}`);
          const name = sc[k];
          if (name !== undefined && typeof name !== "string") throw new Error(`${where}: odd ${what} value`);
          list.push([n, name === undefined ? 0 : scope(name) + 1]);
        }
        if (list.length !== Object.keys(sc._emit).length) throw new Error(`${where}: ${what} _emit mismatch`);
        return { kind: 2, list };
      }
      throw new Error(`${where}: unknown ${what} shape`);
    };
    o.beginScope = scopeSpec(m.beginScope, "beginScope");
    o.endScope = scopeSpec(m.endScope, "endScope");

    o.contains = m.contains.map(idOf);
    o.starts = m.starts ? idOf(m.starts) + 1 : 0;

    if (m.subLanguage == null) o.sub = { kind: 0 };
    else if (typeof m.subLanguage === "string") o.sub = { kind: 1, name: S(m.subLanguage) };
    else if (Array.isArray(m.subLanguage)) o.sub = { kind: 2, names: m.subLanguage.map(S) };
    else throw new Error(`${where}: odd subLanguage`);

    o.beforeBegin = callbackId(m.__beforeBegin, `${where} __beforeBegin`);
    o.onBegin = callbackId(m["on:begin"], `${where} on:begin`);
    o.onEnd = callbackId(m["on:end"], `${where} on:end`);
    for (const c of [o.beforeBegin, o.onBegin, o.onEnd]) {
      if (c) {
        const name = CALLBACKS[c - 1].id;
        if (!usedCallbacks.has(name)) usedCallbacks.set(name, new Set());
        usedCallbacks.get(name).add(langName);
      }
    }

    // the matcher's rule list, as buildModeRegex builds it; cross-checked
    // against highlight.js's own joined regex for the Rust port's join
    const rules = m.matcher.rules.map(([r, opts]) => [source(r), opts.type]);
    const expect = [
      ...m.contains.map((c) => [source(c.begin), "begin"]),
      ...(m.terminatorEnd ? [[m.terminatorEnd, "end"]] : []),
      ...(m.illegal ? [[source(m.illegal), "illegal"]] : []),
    ];
    if (JSON.stringify(rules) !== JSON.stringify(expect)) throw new Error(`${where}: matcher rules differ from the mode's fields`);
    if (m.matcher.count !== m.contains.length) throw new Error(`${where}: matcher.count`);
    if (rules.length) {
      // getMatcher(n) reassigns the shared `position` fields (a highlight.js
      // quirk the engine reproduces per call); restore them afterwards
      const saved = m.matcher.rules.map(([, opts]) => opts.position);
      const fresh = Object.create(Object.getPrototypeOf(m.matcher));
      Object.assign(fresh, m.matcher, { multiRegexes: [], rules: m.matcher.rules.map(([r, opts]) => [r, { ...opts }]) });
      const pick = [0];
      if (rules.length > 1) pick.push(1);
      if (rules.length > 3) pick.push(rules.length - 1);
      for (const at of pick) {
        // the string highlight.js hands to `new RegExp` (`.source` would
        // show it with V8's escaping of "/")
        lastPattern = null;
        fresh.getMatcher(at);
        if (typeof lastPattern !== "string") throw new Error(`${where}: joined regex not captured`);
        joinedVectors.push(`${langName}\t${id}\t${at}\t${JSON.stringify(lastPattern)}`);
      }
      m.matcher.rules.forEach(([, opts], i) => { opts.position = saved[i]; });
    }
    modes[id] = o;
  }

  // ── serialise ──
  const out = new Out();
  out.chunks.push(Buffer.from("DHLG"));
  out.u8(1);
  out.str(langName);
  out.u8((lang.case_insensitive ? 1 : 0) | (lang.unicodeRegex ? 2 : 0) | (lang.disableAutodetect ? 4 : 0));
  out.varint(classNameAliases.length);
  for (const [k, v] of classNameAliases) { out.str(k); out.str(v); }
  // strings are interned first, so write them after building everything
  out.varint(strings.length);
  for (const s of strings) out.str(s);
  out.varint(kwTables.length);
  for (const t of kwTables) {
    out.varint(t.pattern);
    out.varint(t.rows.length);
    for (const [w, kind, rel] of t.rows) { out.str(w); out.varint(kind); out.varint(milli(rel)); }
  }
  out.varint(modes.length);
  const reOut = (r) => { out.varint(r.s); if (r.s) out.varint(r.groups); };
  const scopeOut = (s) => {
    out.varint(s.kind);
    if (s.kind === 1) out.varint(s.wrap);
    if (s.kind === 2) {
      out.varint(s.list.length);
      for (const [n, name] of s.list) { out.varint(n); out.varint(name); }
    }
  };
  for (const o of modes) {
    out.varint(o.flags);
    out.varint(o.scope);
    out.varint(milli(o.relevance));
    reOut(o.begin);
    out.varint(o.endRe);
    reOut(o.term);
    reOut(o.illegal);
    out.varint(o.keywords);
    scopeOut(o.beginScope);
    scopeOut(o.endScope);
    out.varint(o.contains.length);
    for (const c of o.contains) out.varint(c);
    out.varint(o.starts);
    out.varint(o.sub.kind);
    if (o.sub.kind === 1) out.varint(o.sub.name);
    if (o.sub.kind === 2) { out.varint(o.sub.names.length); for (const n of o.sub.names) out.varint(n); }
    out.u8(o.beforeBegin);
    out.u8(o.onBegin);
    out.u8(o.onEnd);
  }
  const bytes = out.bytes();
  report.push({
    lang: langName, bytes: bytes.length, modes: modes.length, strings: strings.length,
    keywords: kwTables.reduce((a, t) => a + t.rows.length, 0),
  });
  if (JSON_DIR) {
    mkdirSync(JSON_DIR, { recursive: true });
    writeFileSync(resolve(JSON_DIR, `${langName}.json`), JSON.stringify({ strings, kwTables, modes, classNameAliases }, null, 1));
  }
  return {
    bytes,
    display: lang.name,
    aliases: lang.aliases || [],
    autodetect: !lang.disableAutodetect,
  };
}

const extracted = new Map();
for (const n of LANGS) extracted.set(n, extractLanguage(n));

// aliases as highlight.js resolves them: registration order, later wins,
// lower-cased; a language's own name wins over any alias (getLanguage)
const aliasMap = new Map();
for (const n of LANGS) {
  for (const a of extracted.get(n).aliases) aliasMap.set(a.toLowerCase(), n);
}
for (const [lang, list] of Object.entries(EXTRA_ALIASES)) {
  if (!LANGS.includes(lang)) throw new Error(`alias target ${lang} is not registered`);
  for (const a of list) aliasMap.set(a.toLowerCase(), lang);
}
for (const n of LANGS) aliasMap.delete(n);
// cross-check against the instance
for (const [a, n] of aliasMap) {
  if (high.getLanguage(a) !== high.getLanguage(n)) throw new Error(`alias ${a} resolves differently`);
}

for (const cb of CALLBACKS) {
  if (!usedCallbacks.has(cb.id)) {
    throw new Error(`callback ${cb.id} is no longer used by any registered grammar; remove it and its Rust twin`);
  }
}

// ───────────────────────── generated.rs ─────────────────────────
const scopes = [...allScopes].sort();
const variant = (s) =>
  s.split(/[._-]/).map((p) => p[0].toUpperCase() + p.slice(1)).join("");
const langVariant = (s) => s[0].toUpperCase() + s.slice(1);
let rs = `// Generated by tools/extract.mjs from highlight.js ${hljsVersion} (lowlight ${lowlightVersion}). Do not edit.

/// The highlight.js version the embedded grammars come from.
pub const HIGHLIGHT_JS_VERSION: &str = "${hljsVersion}";
/// The lowlight version whose behaviour the engine reproduces.
pub const LOWLIGHT_VERSION: &str = "${lowlightVersion}";

/// A registered grammar, in CodeBlock.tsx's registration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Language {
${LANGS.map((n) => `    ${langVariant(n)},`).join("\n")}
}

pub(crate) struct LanguageEntry {
    pub name: &'static str,
    pub display_name: &'static str,
    pub autodetect: bool,
    pub data: &'static [u8],
}

pub(crate) static LANGUAGES: [LanguageEntry; ${LANGS.length}] = [
${LANGS.map((n) => {
  const e = extracted.get(n);
  return `    LanguageEntry { name: ${JSON.stringify(n)}, display_name: ${JSON.stringify(e.display)}, autodetect: ${e.autodetect}, data: include_bytes!("../data/${n}.hlg") },`;
}).join("\n")}
];

/// Aliases (lower case, sorted) → language, as highlight.js resolves them
/// after CodeBlock.tsx's registerAlias calls.
pub(crate) static ALIASES: [(&str, Language); ${aliasMap.size}] = [
${[...aliasMap].sort((a, b) => (a[0] < b[0] ? -1 : 1)).map(([a, n]) => `    (${JSON.stringify(a)}, Language::${langVariant(n)}),`).join("\n")}
];

/// Every scope the registered grammars can emit (highlight.js scope names;
/// \`title.function\` renders as class \`hljs-title function_\`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Scope {
${scopes.map((s) => `    /// \`${s}\`\n    ${variant(s)},`).join("\n")}
}

pub(crate) static SCOPE_NAMES: [&str; ${scopes.length}] = [
${scopes.map((s) => `    ${JSON.stringify(s)},`).join("\n")}
];

pub(crate) static SCOPES: [Scope; ${scopes.length}] = [
${scopes.map((s) => `    Scope::${variant(s)},`).join("\n")}
];

pub(crate) static ALL_LANGUAGES: [Language; ${LANGS.length}] = [
${LANGS.map((n) => `    Language::${langVariant(n)},`).join("\n")}
];

/// The grammar callbacks (functions in the highlight.js sources) that have a
/// hand-written twin in callbacks.rs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Callback {
${CALLBACKS.map((c, i) => `    /// ${c.doc}\n    ${c.id} = ${i + 1},`).join("\n")}
}

impl Callback {
    pub(crate) fn from_id(id: u8) -> Option<Callback> {
        Some(match id {
${CALLBACKS.map((c, i) => `            ${i + 1} => Callback::${c.id},`).join("\n")}
            _ => return None,
        })
    }
}
`;
if (new Set(scopes.map(variant)).size !== scopes.length) throw new Error("scope variant names collide");
if (scopes.length > 255) throw new Error("more than 255 scopes");

// ───────────────────────── write / check ─────────────────────────
const outputs = new Map();
for (const n of LANGS) outputs.set(resolve(CRATE_DIR, "data", `${n}.hlg`), extracted.get(n).bytes);
outputs.set(resolve(CRATE_DIR, "src/generated.rs"), Buffer.from(rs));
outputs.set(resolve(CRATE_DIR, "tests/vectors/joined.txt"), Buffer.from(joinedVectors.join("\n") + "\n"));

let stale = 0;
for (const [path, data] of outputs) {
  const same = existsSync(path) && readFileSync(path).equals(data);
  if (CHECK) {
    if (!same) { console.error(`stale: ${path}`); stale++; }
  } else if (!same) {
    mkdirSync(resolve(path, ".."), { recursive: true });
    writeFileSync(path, data);
  }
}
if (CHECK && stale) process.exit(1);

console.log(`highlight.js ${hljsVersion}, lowlight ${lowlightVersion}: ${LANGS.length} grammars`);
let total = 0;
for (const r of report) {
  total += r.bytes;
  console.log(`  ${r.lang.padEnd(11)} ${String(r.bytes).padStart(7)} B  ${String(r.modes).padStart(4)} modes  ${String(r.strings).padStart(4)} strings  ${String(r.keywords).padStart(5)} keywords`);
}
console.log(`  total ${total} B; ${scopes.length} scopes; callbacks: ${[...usedCallbacks].map(([k, v]) => `${k} (${[...v].join(", ")})`).join("; ")}`);

#!/usr/bin/env node
// Differential test vectors: runs the real lowlight, set up exactly as
// CodeBlock.tsx sets it up, over a corpus and writes canonical token trees
// for the Rust tests to compare with (tests/differential.rs).
//
//   node tools/vectors.mjs            write tests/vectors/corpus.tsv.gz and
//                                     tests/vectors/fuzz.tsv.gz (committed)
//   node tools/vectors.mjs --full     also tests/vectors/full/fuzz-full.tsv.gz
//                                     (gitignored), checked against
//                                     tests/vectors/full.sha256
//   node tools/vectors.mjs --full --update-hash    record a new hash
//   node tools/vectors.mjs --check    regenerate the committed files in memory
//                                     and fail if they differ
//
// Corpus: tests/corpus/samples/*.txt (written for this crate), highlight.js's
// own test inputs (tools/fetch-fixtures.sh), the code blocks of the 10k-message
// chat fixture, hand-picked edge cases, and seeded mutation fuzz of all of
// those (truncations, splices, character flips: broken code is what people
// paste).
//
// A row is: language \t source \t flag \t relevance \t tree \t JSON string of the code
//   flag  ok     lowlight returned a tree
//         throw  lowlight threw; CodeBlock.tsx then shows plain text
//         text   lowlight's tree does not spell the input: same length,
//                other characters (see README, "Where highlight.js's output
//                is not the input"); the tree is still compared
//         textlen  the same, and the length differs too: not comparable
//   relevance  lowlight's `data.relevance` (a JS number)
//   tree  O<classes>; opens a span, C; closes it, T<n>; is n UTF-16 units of text

import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { resolve } from "node:path";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";
import { cpus } from "node:os";
import { fileURLToPath } from "node:url";
import { CRATE_DIR, ELECTRON_DIR, makeLowlight, readCodeBlock, sha256 } from "./common.mjs";

const FUZZ_PER_LANGUAGE = 300;
const FULL_PER_LANGUAGE = 5000;
const SEED = 0x0dec1be1;

// ───────────────────────── reference ─────────────────────────

// highlight.js builds a regex (`new RegExp`) only when it compiles a language,
// when a mode first runs, and when its resumable matcher makes a sub-matcher
// after an ignored match.
// The latter also rewrites state shared with the mode's other matchers, so a
// later highlight can come out differently (README, "History dependence").
// Counting constructions lets the reference always run on an instance in the
// state a fresh one would be in.
let regexpsBuilt = 0;
const NativeRegExp = globalThis.RegExp;
globalThis.RegExp = new Proxy(NativeRegExp, {
  construct(target, args, newTarget) {
    regexpsBuilt++;
    return Reflect.construct(target, args, newTarget === globalThis.RegExp ? target : newTarget);
  },
});

function canon(root) {
  const out = [];
  let text = "";
  let pending = 0;
  const flush = () => {
    if (pending) out.push(`T${pending};`);
    pending = 0;
  };
  const walk = (node) => {
    for (const child of node.children) {
      if (child.type === "text") {
        const v = String(child.value);
        text += v;
        pending += v.length;
      } else {
        flush();
        out.push(`O${child.properties.className.join(" ")};`);
        walk(child);
        flush();
        out.push("C;");
      }
    }
  };
  walk(root);
  flush();
  return { tree: out.join(""), text };
}

async function makeReference() {
  const create = await makeLowlight();
  const { names } = readCodeBlock();
  const fresh = () => {
    const { lowlight, languages } = create.withLanguages();
    // compile every language and build every mode's first matcher now, so
    // that a later construction can only be a sub-matcher
    for (const n of names) {
      lowlight.highlight(n, "");
      const seen = new Set();
      const walk = (m) => {
        if (seen.has(m)) return;
        seen.add(m);
        m.matcher.getMatcher(0);
        m.contains.forEach(walk);
        if (m.starts) walk(m.starts);
      };
      walk(languages[n]);
    }
    return lowlight;
  };
  let clean = fresh();
  const longLived = fresh();
  const stats = { cases: 0, rebuilt: 0, historyDependent: 0, historyExamples: [] };

  // CodeBlock.tsx `highlight(lang, code)`
  const viaCodeBlock = (lowlight, lang, code) => {
    let flag = "ok";
    let tree;
    try {
      tree = lang && lowlight.registered(lang)
        ? lowlight.highlight(lang, code)
        : lowlight.highlight("plaintext", code);
    } catch {
      flag = "throw";
      tree = lowlight.highlight("plaintext", code);
    }
    const c = canon(tree);
    if (c.text !== code) flag = c.text.length === code.length ? "text" : "textlen";
    return { flag, tree: c.tree, relevance: tree.data.relevance };
  };

  return {
    stats,
    run(lang, code) {
      stats.cases++;
      const before = regexpsBuilt;
      const r = viaCodeBlock(clean, lang, code);
      if (regexpsBuilt !== before) {
        clean = fresh();
        stats.rebuilt++;
      }
      const stale = viaCodeBlock(longLived, lang, code);
      if (stale.tree !== r.tree || stale.flag !== r.flag || stale.relevance !== r.relevance) {
        stats.historyDependent++;
        if (stats.historyExamples.length < 5) stats.historyExamples.push({ lang, code });
      }
      return r;
    },
  };
}

// ───────────────────────── corpus ─────────────────────────

const SAMPLES_DIR = resolve(CRATE_DIR, "tests/corpus/samples");
const HLJS_DIR = resolve(CRATE_DIR, "tests/corpus/hljs");
// sample file → the language name to highlight it with (aliases on purpose)
const SAMPLE_LANG = { html: "html", toml: "toml", jsx: "jsx", tsx: "tsx" };

function readText(path) {
  return readFileSync(path, "utf8").replace(/\r\n/g, "\n");
}

function loadBases() {
  const { names } = readCodeBlock();
  const bases = []; // { lang (name to pass), grammar (registered name), source, code }
  const grammarOf = { html: "xml", toml: "ini", jsx: "javascript", tsx: "typescript" };
  for (const f of readdirSync(SAMPLES_DIR).sort()) {
    if (!f.endsWith(".txt")) continue;
    const stem = f.slice(0, -4);
    const lang = SAMPLE_LANG[stem] || stem;
    const grammar = grammarOf[stem] || stem;
    if (!names.includes(grammar)) throw new Error(`sample ${f}: ${grammar} is not registered`);
    bases.push({ lang, grammar, source: `sample/${f}`, code: readText(resolve(SAMPLES_DIR, f)) });
  }
  if (!existsSync(HLJS_DIR)) throw new Error("tests/corpus/hljs is missing: run tools/fetch-fixtures.sh");
  for (const n of names) {
    const dir = resolve(HLJS_DIR, "markup", n);
    if (existsSync(dir)) {
      for (const f of readdirSync(dir).sort()) {
        if (!f.endsWith(".txt")) continue;
        bases.push({ lang: n, grammar: n, source: `hljs/markup/${n}/${f}`, code: readText(resolve(dir, f)) });
      }
    }
    const detect = resolve(HLJS_DIR, "detect", n);
    if (existsSync(detect)) {
      for (const f of readdirSync(detect).sort()) {
        bases.push({ lang: n, grammar: n, source: `hljs/detect/${n}/${f}`, code: readText(resolve(detect, f)) });
      }
    }
  }
  return bases;
}

// The code blocks of the 10k-message chat fixture (qt-client/tools/gen-fixture.mjs
// and tools/dev/fixture.py use the same table).
function chatFixtureBlocks() {
  const src = readFileSync(resolve(ELECTRON_DIR, "../qt-client/tools/gen-fixture.mjs"), "utf8");
  const m = /^const LANGS = (\{.*\});$/m.exec(src);
  if (!m) throw new Error("gen-fixture.mjs: LANGS table not found");
  const table = new Function(`return ${m[1]}`)();
  return Object.entries(table).map(([lang, code]) => ({ lang, source: `chat-fixture/${lang}`, code }));
}

// Small cases aimed at engine corners.
function edgeCases() {
  const c = (lang, code, name) => ({ lang, source: `edge/${name}`, code });
  return [
    c("javascript", "", "empty"),
    c("JS", "const a = 1;", "upper-case alias"),
    c("nosuchlang", "const a = 1;", "unknown language"),
    c("", "const a = 1;", "no language"),
    c("plaintext", "<b>not</b> highlighted & \"quoted\"", "plaintext"),
    c("text", "plain", "plaintext alias"),
    c("toml", "[a]\nb = 1 # c\n", "toml alias"),
    c("svg", "<svg><path d=\"M0 0\"/></svg>", "svg alias"),
    c("html", "<script>let a = `x${1}`;</script><style>a{b:c}</style>", "html sub-languages"),
    c("html", "<script type=\"text/plain\">not js</script>", "html script unknown type"),
    c("html", "<style>\n</style><script>\n</script>", "html empty-ish sub-languages"),
    c("xml", "<a>\n<script>if (a < b) { }</script>\n<script>var x = '</script>'</script>", "script continuation"),
    c("markdown", "# t\n\n<div>\n*a*\n</div>\n\n```js\nlet a\n```\n", "markdown html"),
    c("javascript", "const a = html`<p class=\"x\">${b}</p>`; const s = css`a{color:red}`; gql`{ a }`", "js tagged templates"),
    c("javascript", "a.function.b; x.class; y.if(1)", "preceding dot"),
    c("javascript", "x = 1;\n#!/usr/bin/env node\n", "shebang not at start"),
    c("javascript", "#!/usr/bin/env node\nx = 1;\n", "shebang"),
    c("typescript", "const f = <T,>(x: T) => x; const g = <T extends object>(x: T) => <div>{x}</div>;", "tsx generics"),
    c("javascript", "const el = <Foo>bar</Foo>; a <b> c; x = <Bar>;", "jsx closing tag search"),
    c("cpp", "auto s = R\"abc(raw )\" )abc\"; auto t = R\"x(never ends )y\";", "cpp raw string"),
    c("php", "<?php\n$a = <<<EOT\nhi $x\nEOTX\nEOT;\n$b = <<<'N'\nraw\nN;\n", "php heredoc"),
    c("ruby", "a = <<~EOS\n  text \#{x}\n  EOSX\nEOS\nb = <<-'X'\n raw\n X\n", "ruby heredoc"),
    c("bash", "cat <<EOF\n$x\nEOFX\nEOF\ncat <<-'E'\n\traw\n\tE\n", "bash heredoc"),
    c("rust", "let s = r#\"a \"quoted\" b\"#; let c = 'a'; fn f<'a>(x: &'a str) {}", "rust raw string"),
    c("python", "x = f\"{a!r:>{w}} {b['k']}\"\nidentité = 1\n", "python unicode ident"),
    c("xml", "<größe attr=\"1\">ß</größe>", "xml unicode names"),
    c("sql", "SELECT İ, ı, K FROM t WHERE a = 'x' -- KELVİN\n", "sql case folding"),
    c("css", "A { COLOR: RED; Background: URL(x.png) }", "css upper case"),
    c("javascript", "const s = \"a\\\u{1F600}b\"; // \u{1F600}\n", "astral after backslash"),
    c("javascript", "let 🎉 = '🎉🎉'; /* 🎉 */ x = `🎉${y}🎉`", "astral characters"),
    c("javascript", "a = 1;\r\nb = 'x';\r\n// c\r\n", "crlf"),
    c("python", "def f():\r\n    return 'x'\r\n", "crlf python"),
    c("javascript", "a\u2028b // c\u2029d", "line separators"),
    c("json", "{\"a\": [1, 2.5e3, true, null], // c\n \"b\": \"\\u00e9\"}", "json"),
    c("diff", "--- a\n+++ b\n@@ -1 +1 @@\n-x\n+y\n", "diff"),
    c("dockerfile", "FROM a AS b\nRUN echo \"$X\" && \\\n  ls -la | grep 'x'\nCMD [\"a\", \"b\"]\n", "dockerfile bash"),
    c("yaml", "a: |\n  text\nb: !!str 1\nc: &x {d: 1}\n", "yaml"),
    c("swift", "let r = #/a+b/#; let s = #\"raw \\(x)\"#; let t = \"\\(a + \"\\(b)\")\"", "swift literals"),
    c("kotlin", "val s = \"${a.b} $c ${\"n${x}\"}\"; val r = \"\"\"raw $x\"\"\"", "kotlin templates"),
    c("csharp", "var s = $@\"a {b:F2} \"\"q\"\" {{c}}\"; var r = \"\"\"raw\"\"\";", "csharp strings"),
    c("go", "s := `raw\n` + \"x\\n\" + string('a')", "go strings"),
    c("lua", "s = [==[ long ]] ]==] -- c\n--[[ block\n]] x = 1", "lua long brackets"),
    c("powershell", "$x = @\"\n$($a.b) text\n\"@\n<# .SYNOPSIS x #>", "powershell here-string"),
    c("scss", "$a: 1px; .b { &:hover { c: #{$a} } @include d(1); }", "scss"),
    c("ini", "[s]\na=1 ; c\nb = \"x\"\n", "ini"),
    c("java", "@A(b = 1) class C<T extends D<? super E>> { String s = \"\"\"\n  t\n  \"\"\"; }", "java"),
    c("c", "#define X(a) \\\n  ((a) + 1)\n#include <stdio.h>\nint main(void) { return 0x1F; }", "c preprocessor"),
    c("typescript", "x".repeat(40) + "\n".repeat(30), "blank lines"),
    c("javascript", "/".repeat(300), "slashes"),
    c("javascript", "`".repeat(51) + "${".repeat(40), "template nesting"),
    c("markdown", "*".repeat(200) + "\n" + "_".repeat(200), "markdown emphasis run"),
    c("xml", "<".repeat(200) + ">".repeat(200), "angle brackets"),
  ];
}

// ───────────────────────── fuzz ─────────────────────────

function mulberry32(seed) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const PUNCT = [..."\"\"''``//\\\\<<>>{{}}(())[]##$$@*-=+;:,.!?|&%^~_ \n\n\t0159azAZ"];
const UNICODE = ["é", "ı", "İ", "ß", "Σ", "\u212a", "\u017f", "中", "🎉", "\u{1F600}", "\u2028", "\u00a0", "\u200b", "\r\n", "\r", "\ufeff", "ǅ"];

function mutate(rnd, base, others) {
  const int = (n) => Math.floor(rnd() * n);
  const pick = (a) => a[int(a.length)];
  // a window of lines: chat-sized more often than whole files
  let lines = base.split("\n");
  if (lines.length > 3 && rnd() < 0.85) {
    const len = 1 + int(Math.min(40, lines.length));
    const start = int(lines.length - len + 1);
    lines = lines.slice(start, start + len);
  } else if (lines.length > 150) {
    lines = lines.slice(0, 150);
  }
  // code points, so a mutation never splits a surrogate pair (the input is
  // always well-formed: it arrives as UTF-8)
  let cp = Array.from(lines.join("\n"));
  const n = 1 + int(4);
  for (let i = 0; i < n; i++) {
    const at = int(cp.length + 1);
    switch (int(14)) {
      case 0: cp = cp.slice(0, at); break; // truncate the end
      case 1: cp = cp.slice(at); break; // truncate the start
      case 2: cp.splice(at, 1 + int(30)); break; // delete a range
      case 3: { // duplicate a range
        const piece = cp.slice(at, at + 1 + int(40));
        cp.splice(int(cp.length + 1), 0, ...piece);
        break;
      }
      case 4: { // splice in a piece of another text (any language)
        const o = Array.from(pick(others));
        const from = int(o.length);
        cp.splice(at, 0, ...o.slice(from, from + 5 + int(80)));
        break;
      }
      case 5: case 6: if (cp.length) cp[int(cp.length)] = pick(PUNCT); break; // flip
      case 7: cp.splice(at, 0, pick(PUNCT)); break; // insert
      case 8: cp.splice(at, 0, ...Array.from(pick(UNICODE))); break;
      case 9: { // swap two lines
        const ls = cp.join("").split("\n");
        const a = int(ls.length), b = int(ls.length);
        [ls[a], ls[b]] = [ls[b], ls[a]];
        cp = Array.from(ls.join("\n"));
        break;
      }
      case 10: { // change the case of a range
        const len = 1 + int(20);
        const piece = cp.slice(at, at + len).join("");
        cp.splice(at, len, ...Array.from(rnd() < 0.5 ? piece.toUpperCase() : piece.toLowerCase()));
        break;
      }
      case 12: { // a dot before a word: `x.class`, `a.function` (beginKeywords veto)
        const text = cp.join("");
        const words = [...text.matchAll(/\b[A-Za-z_]\w*/g)];
        if (words.length) {
          const w = pick(words);
          cp = Array.from(text.slice(0, w.index) + pick([".", "a.", "..", ". "]) + text.slice(w.index));
        }
        break;
      }
      case 13: // things other callbacks look at
        cp.splice(at, 0, ...Array.from(pick(["#!/usr/bin/env node\n", "<T,", "<A<B>>", "<div>", "</div>", "<T = any>", "<T extends U>", "<<EOF\n", "EOF\n", "<<<'X'\n", "R\"d(", ")d\"", ".class", ".function ", ".if(", ".import "])));
        break;
      case 11: // join lines / CRLF
        if (rnd() < 0.5) cp = Array.from(cp.join("").replace(/\n/g, (m) => (rnd() < 0.3 ? " " : m)));
        else cp = Array.from(cp.join("").replace(/\n/g, "\r\n"));
        break;
    }
  }
  return cp.join("");
}

function fuzzCases(bases, grammar, count, seedTag) {
  const mine = bases.filter((b) => b.grammar === grammar);
  if (!mine.length) return [];
  const others = bases.map((b) => b.code);
  let h = SEED ^ seedTag;
  for (const ch of grammar) h = (Math.imul(h, 31) + ch.charCodeAt(0)) >>> 0;
  const rnd = mulberry32(h);
  const out = [];
  for (let i = 0; i < count; i++) {
    const base = mine[Math.floor(rnd() * mine.length)];
    out.push({ lang: base.lang, source: `fuzz/${grammar}/${i}`, code: mutate(rnd, base.code, others) });
  }
  return out;
}

// ───────────────────────── output ─────────────────────────

function row(c, r) {
  if (/[\t\n]/.test(c.lang + c.source)) throw new Error("tab in a field");
  return `${c.lang}\t${c.source}\t${r.flag}\t${r.relevance}\t${r.tree}\t${JSON.stringify(c.code)}\n`;
}

async function runCases(cases) {
  const ref = await makeReference();
  let out = "";
  for (const c of cases) out += row(c, ref.run(c.lang, c.code));
  return { out, stats: ref.stats };
}

if (!isMainThread) {
  const { grammar, count, seedTag } = workerData;
  const bases = loadBases();
  const r = await runCases(fuzzCases(bases, grammar, count, seedTag));
  parentPort.postMessage(r);
} else {
  const args = process.argv.slice(2);
  const FULL = args.includes("--full");
  const CHECK = args.includes("--check");
  const bases = loadBases();
  const { names } = readCodeBlock();
  const fuzzLangs = names.filter((n) => n !== "plaintext");

  const corpus = [
    ...edgeCases(),
    ...chatFixtureBlocks(),
    ...bases.map((b) => ({ lang: b.lang, source: b.source, code: b.code })),
  ];
  const stats = { cases: 0, rebuilt: 0, historyDependent: 0, historyExamples: [] };
  const add = (s) => {
    stats.cases += s.cases;
    stats.rebuilt += s.rebuilt;
    stats.historyDependent += s.historyDependent;
    stats.historyExamples.push(...s.historyExamples);
  };
  const corpusRun = await runCases(corpus);
  add(corpusRun.stats);

  const fuzz = async (count, seedTag) => {
    const jobs = [...fuzzLangs];
    const results = new Map();
    const workers = Math.max(1, Math.min(cpus().length - 1, 8));
    await Promise.all(Array.from({ length: workers }, async () => {
      while (jobs.length) {
        const grammar = jobs.shift();
        const r = await new Promise((ok, fail) => {
          const w = new Worker(fileURLToPath(import.meta.url), { workerData: { grammar, count, seedTag } });
          w.once("message", ok);
          w.once("error", fail);
        });
        results.set(grammar, r);
      }
    }));
    let out = "";
    for (const g of fuzzLangs) {
      out += results.get(g).out;
      add(results.get(g).stats);
    }
    return out;
  };

  const files = new Map();
  files.set("tests/vectors/corpus.tsv.gz", corpusRun.out);
  files.set("tests/vectors/fuzz.tsv.gz", await fuzz(FUZZ_PER_LANGUAGE, 1));

  let bad = 0;
  for (const [rel, text] of files) {
    const path = resolve(CRATE_DIR, rel);
    const rows = text.split("\n").length - 1;
    if (CHECK) {
      const { gunzipSync } = await import("node:zlib");
      const same = existsSync(path) && gunzipSync(readFileSync(path)).toString("utf8") === text;
      console.log(`${rel}: ${rows} cases, ${same ? "up to date" : "STALE"}`);
      if (!same) bad++;
    } else {
      mkdirSync(resolve(path, ".."), { recursive: true });
      const gz = gzipSync(Buffer.from(text), { level: 9 });
      writeFileSync(path, gz);
      console.log(`${rel}: ${rows} cases, ${text.length} bytes, ${gz.length} gzipped`);
    }
  }

  if (FULL) {
    const text = await fuzz(FULL_PER_LANGUAGE, 2);
    const hash = sha256(Buffer.from(text));
    const hashPath = resolve(CRATE_DIR, "tests/vectors/full.sha256");
    const path = resolve(CRATE_DIR, "tests/vectors/full/fuzz-full.tsv.gz");
    mkdirSync(resolve(path, ".."), { recursive: true });
    writeFileSync(path, gzipSync(Buffer.from(text), { level: 6 }));
    console.log(`tests/vectors/full/fuzz-full.tsv.gz: ${text.split("\n").length - 1} cases, ${text.length} bytes, sha256 ${hash}`);
    if (args.includes("--update-hash") || !existsSync(hashPath)) {
      writeFileSync(hashPath, hash + "\n");
    } else if (readFileSync(hashPath, "utf8").trim() !== hash) {
      console.error("the full fuzz set differs from tests/vectors/full.sha256 (different highlight.js, corpus or Node?)");
      bad++;
    }
  }

  const flags = {};
  for (const text of files.values()) {
    for (const line of text.split("\n")) {
      const f = line.split("\t")[2];
      if (f) flags[f] = (flags[f] || 0) + 1;
    }
  }
  console.log(`flags in the committed sets: ${JSON.stringify(flags)}`);
  console.log(
    `${stats.cases} cases; ${stats.rebuilt} dirtied the instance (sub-matcher built); ` +
      `${stats.historyDependent} came out differently on a long-lived instance`,
  );
  for (const e of stats.historyExamples.slice(0, 3)) console.log(`  history-dependent: ${e.lang} ${JSON.stringify(e.code.slice(0, 120))}`);
  if (bad) process.exit(1);
}

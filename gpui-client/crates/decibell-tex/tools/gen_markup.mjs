#!/usr/bin/env node
// Markup vectors: renders every corpus formula with the repo's KaTeX (electron-client/
// node_modules/katex, the app's version) and the app's options (MathTex.tsx; errorColor is
// "#cc0000" here because the app's "var(--color-error)" is a theme token), and writes
// tests/data/markup.jsonl.gz, one JSON object per line:
//   {"id","tex","display","status":"ok"|"error"|"throw","html","ms"}
// ok:    renderToString's output with the <span class="katex-mathml">…</span> part removed
//        (only the HTML half is visible; the MathML half is screen-reader-only).
// error: the whole katex-error span (KaTeX's throwOnError:false rendering of a ParseError).
// throw: renderToString threw (a non-ParseError, e.g. missing font metrics); html is
//        "<name>: <message>". The app shows the raw TeX in a <code> then.
// ms:    wall time of one renderToString call after a warm-up pass (adversarial cost).
//
// Usage: node tools/gen_markup.mjs [corpus.jsonl] [out.jsonl.gz]

import fs from "node:fs";
import path from "node:path";
import zlib from "node:zlib";
import { fileURLToPath } from "node:url";
import { loadKatex, APP_OPTIONS } from "./corpus/katex_internals.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const corpusPath = process.argv[2] || path.join(here, "corpus/corpus.jsonl");
const outPath = process.argv[3] || path.join(here, "../tests/data/markup.jsonl.gz");

const katex = loadKatex();
const corpus = fs.readFileSync(corpusPath, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));

const MATHML_OPEN = '<span class="katex-mathml">';

function stripMathml(html) {
    const a = html.indexOf(MATHML_OPEN);
    if (a < 0) throw new Error("no katex-mathml span");
    const b = html.indexOf("</span>", a);
    const inner = html.slice(a + MATHML_OPEN.length, b);
    if (inner.includes("<span")) throw new Error("nested span inside katex-mathml");
    return html.slice(0, a) + html.slice(b + "</span>".length);
}

function render(e) {
    const opts = { ...APP_OPTIONS, displayMode: e.display };
    try {
        const html = katex.renderToString(e.tex, opts);
        if (html.startsWith('<span class="katex-error"')) return { status: "error", html };
        return { status: "ok", html: stripMathml(html) };
    } catch (err) {
        return { status: "throw", html: `${err && err.name}: ${err && err.message}` };
    }
}

// KaTeX prints through the console: strict-mode warnings (console.warn), \message and \show
// (console.log), \errmessage (console.error). Silence all three while rendering.
const saved = { warn: console.warn, log: console.log, error: console.error };
console.warn = console.log = console.error = () => {};
// warm-up so the timings below are of jitted code
for (const e of corpus.slice(0, 300)) render(e);

const lines = [];
const counts = { ok: 0, error: 0, throw: 0 };
const slow = [];
for (const e of corpus) {
    const t0 = process.hrtime.bigint();
    const r = render(e);
    const ms = Number(process.hrtime.bigint() - t0) / 1e6;
    counts[r.status]++;
    if (ms > 20) slow.push([e.id, ms.toFixed(1), r.status]);
    lines.push(JSON.stringify({ id: e.id, tex: e.tex, display: e.display, status: r.status, html: r.html, ms: Math.round(ms * 1000) / 1000 }));
}
Object.assign(console, saved);

fs.mkdirSync(path.dirname(outPath), { recursive: true });
fs.writeFileSync(outPath, zlib.gzipSync(lines.join("\n") + "\n", { level: 9 }));
console.error(`wrote ${lines.length} vectors to ${outPath} (${fs.statSync(outPath).size} bytes)`, counts);
if (slow.length) console.error("slower than 20 ms:", slow);

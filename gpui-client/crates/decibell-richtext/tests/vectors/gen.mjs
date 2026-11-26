#!/usr/bin/env node
// Regenerates the differential vectors by running the Electron renderer's own code.
//
//   node gpui-client/crates/decibell-richtext/tests/vectors/gen.mjs            all suites
//   node …/gen.mjs --sweep                                                     also re-probe Blink's URL
//                                                                              host verdict per code point
//
// Needs electron-client/node_modules (esbuild, electron, the renderer's deps) and a display (the
// renderer runs in a hidden offscreen window). Writes, next to this file:
//   edge.jsonl        the hand-written suite (edge-cases.mjs): [input, record] per line, in full
//   suites.json       matrix / emoji / fixture / fuzz / urlfuzz: case counts + SHA-256 per 1000-case chunk of
//                     the inputs and of the records (the Rust test regenerates the inputs itself)
//   emoji-inputs.txt  the emoji suite's inputs (every Twemoji file's sequence and every picker native)
//   host-sweep.json   (--sweep) Blink's URL host verdicts, as code-point ranges
// and the full records of the hashed suites into gpui-client/target-richtext/vectors/<suite>.jsonl
// (gitignored), which tests/differential.rs diffs case by case when present.
import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { execFileSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { cases as edgeCases, urlCases } from "./edge-cases.mjs";
import { matrix, fuzz, fixture, slotFuzz } from "./corpus.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../../../../..");
const electronDir = path.join(repo, "electron-client");
const src = path.join(electronDir, "src");
const nm = path.join(electronDir, "node_modules");
const require = createRequire(path.join(electronDir, "package.json"));
const esbuild = require("esbuild");
const fullDir = path.join(repo, "gpui-client/target-richtext/vectors");
const work = path.join(repo, "gpui-client/target-richtext/harness");
fs.mkdirSync(fullDir, { recursive: true });
fs.mkdirSync(work, { recursive: true });
const H = path.join(here, "harness");

// ── bundle the renderer modules ──
const twemojiKeys = fs.readdirSync(path.join(nm, "@twemoji/svg")).filter((f) => f.endsWith(".svg")).map((f) => f.slice(0, -4));
const stubFor = (p) => {
  const rel = path.relative(src, p).replace(/\\/g, "/");
  if (rel.startsWith("..")) return null;
  if (rel.startsWith("stores/")) return path.join(H, "stub-any.cjs");
  if (/^lib\/openExternal\./.test(rel)) return path.join(H, "stub-openexternal.js");
  if (/^lib\/(ipc|certMismatch)\./.test(rel)) return path.join(H, "stub-any.cjs");
  if (/^features\/chat\/MathTex\./.test(rel)) return path.join(H, "stub-mathtex.js");
  if (/^features\/chat\/(InviteEmbed|EmojiInfoPopover)\./.test(rel)) return path.join(H, "stub-any.cjs");
  if (/^features\/chat\/imageViewer\//.test(rel)) return path.join(H, "stub-any.cjs");
  return null;
};
const plugin = {
  name: "renderer",
  setup(build) {
    build.onResolve({ filter: /^@app\// }, async (a) => {
      const r = await build.resolve("./" + a.path.slice(5), { resolveDir: src, kind: a.kind, pluginData: "inner" });
      return { path: stubFor(r.path) ?? r.path };
    });
    build.onResolve({ filter: /^\./ }, async (a) => {
      if (a.pluginData === "inner" || !a.importer.startsWith(src)) return undefined;
      if (/twemoji-data\.json$/.test(a.path)) return { path: "twemoji-data", namespace: "virtual" };
      const r = await build.resolve(a.path, { resolveDir: a.resolveDir, kind: a.kind, pluginData: "inner" });
      if (r.errors.length) return undefined;
      const stub = stubFor(r.path);
      return stub ? { path: stub } : { path: r.path };
    });
    build.onLoad({ filter: /.*/, namespace: "virtual" }, () => ({
      contents: JSON.stringify(Object.fromEntries(twemojiKeys.map((k) => [k, `<i>${k}</i>`]))),
      loader: "json",
    }));
    // Module-private functions the harness calls: append exports in memory (files untouched).
    const extra = {
      "components/editor/RichInput.tsx": "export { tokenize as __tokenize, hasTokenizable as __hasTokenizable, hasOpenCodeFence as __hasOpenCodeFence };",
      "features/chat/LinkEmbeds.tsx": "export { isGifUrl as __isGifUrl };",
    };
    build.onLoad({ filter: /\.tsx$/ }, (a) => {
      const rel = path.relative(src, a.path).replace(/\\/g, "/");
      if (!extra[rel]) return undefined;
      return { contents: fs.readFileSync(a.path, "utf8") + "\n" + extra[rel], loader: "tsx", resolveDir: path.dirname(a.path) };
    });
  },
};
const bundle = await esbuild.build({
  entryPoints: [path.join(H, "entry.js")],
  bundle: true,
  write: false,
  format: "iife",
  platform: "browser",
  target: "chrome130",
  jsx: "automatic",
  nodePaths: [nm],
  alias: {
    react: path.join(H, "stub-react.js"),
    "react/jsx-runtime": path.join(H, "stub-jsx-runtime.js"),
    "react/jsx-dev-runtime": path.join(H, "stub-jsx-runtime.js"),
    "react-dom": path.join(H, "stub-any.cjs"),
  },
  loader: { ".css": "empty" },
  define: { "import.meta.env.DEV": "false", "process.env.NODE_ENV": '"production"' },
  plugins: [plugin],
  logLevel: "warning",
});
const bundlePath = path.join(work, "bundle.js");
fs.writeFileSync(bundlePath, bundle.outputFiles[0].text);

function runElectron(job) {
  const jobPath = path.join(work, "job.json");
  const outPath = path.join(work, "out.json");
  fs.writeFileSync(jobPath, JSON.stringify(job));
  if (fs.existsSync(outPath)) fs.rmSync(outPath);
  execFileSync(path.join(nm, ".bin/electron"), [path.join(H, "electron-main.cjs"), bundlePath, jobPath, outPath], {
    stdio: ["ignore", "ignore", "inherit"],
    maxBuffer: 1 << 30,
  });
  return JSON.parse(fs.readFileSync(outPath, "utf8"));
}

// ── corpora ──
const emojiInputs = (() => {
  const set = [];
  const seen = new Set();
  const push = (s) => { if (!seen.has(s)) { seen.add(s); set.push(s); } };
  for (const k of twemojiKeys.slice().sort()) push(String.fromCodePoint(...k.split("-").map((h) => parseInt(h, 16))));
  const mart = JSON.parse(fs.readFileSync(path.join(nm, "@emoji-mart/data/sets/15/native.json"), "utf8"));
  for (const e of Object.values(mart.emojis)) for (const s of e.skins) push(s.native);
  // with FE0F stripped / added, and in pairs
  const base = set.slice();
  for (const s of base) { push(s.replace(/️/g, "")); push(s + "️"); }
  for (let i = 0; i + 1 < base.length; i += 7) push(base[i] + base[i + 1]);
  return set;
})();
fs.writeFileSync(path.join(here, "emoji-inputs.txt"), emojiInputs.map((s) => JSON.stringify(s)).join("\n") + "\n");

// --stress=N: N more fuzz and URL-fuzz cases on other seeds, full records only (never committed);
// tests/differential.rs `stress` (#[ignore]) compares them.
const stressArg = process.argv.find((a) => a.startsWith("--stress="));
if (stressArg) {
  const n = parseInt(stressArg.slice(9), 10);
  const seed = parseInt(process.argv.find((a) => a.startsWith("--seed="))?.slice(7) ?? "99", 10);
  const st = [
    { name: "stress-fuzz", inputs: fuzz({ seed, count: n }), urlFns: false },
    { name: "stress-urlfuzz", inputs: slotFuzz("urlfuzz.json", { seed: seed + 1, count: n }), urlFns: true },
  ];
  const t = Date.now();
  const r = runElectron({ suites: st });
  for (const s of st) {
    fs.writeFileSync(path.join(fullDir, `${s.name}.jsonl`), s.inputs.map((x, i) => `[${JSON.stringify(x)},${r[s.name][i]}${s.urlFns ? ",1" : ""}]`).join("\n") + "\n");
  }
  console.log(`stress: ${2 * n} cases (seed ${seed}) in ${((Date.now() - t) / 1000).toFixed(1)} s → ${fullDir}`);
  process.exit(0);
}

const suites = [
  { name: "edge", inputs: edgeCases.map((c) => c.input), urlFns: false },
  { name: "url", inputs: urlCases, urlFns: true },
  { name: "matrix", inputs: matrix(), urlFns: false },
  { name: "emoji", inputs: emojiInputs, urlFns: false },
  { name: "fixture", inputs: fixture(), urlFns: false },
  { name: "fuzz", inputs: fuzz(), urlFns: false },
  { name: "urlfuzz", inputs: slotFuzz("urlfuzz.json"), urlFns: true },
];
const t0 = Date.now();
const res = runElectron({ suites });
console.log(`renderer: ${suites.reduce((n, s) => n + s.inputs.length, 0)} cases in ${((Date.now() - t0) / 1000).toFixed(1)} s`);

// edge + url: committed in full
const lines = [];
for (const name of ["edge", "url"]) {
  const s = suites.find((x) => x.name === name);
  s.inputs.forEach((input, i) => lines.push(`[${JSON.stringify(input)},${res[name][i]}${name === "url" ? ",1" : ""}]`));
}
fs.writeFileSync(path.join(here, "edge.jsonl"), lines.join("\n") + "\n");

// hashed suites
const CHUNK = 1000;
const sha = (s) => crypto.createHash("sha256").update(s, "utf8").digest("hex");
const meta = {};
for (const name of ["matrix", "emoji", "fixture", "fuzz", "urlfuzz"]) {
  const s = suites.find((x) => x.name === name);
  const recs = res[name];
  const inChunks = [], outChunks = [];
  for (let k = 0; k < s.inputs.length; k += CHUNK) {
    inChunks.push(sha(s.inputs.slice(k, k + CHUNK).map((x) => JSON.stringify(x) + "\n").join("")).slice(0, 16));
    outChunks.push(sha(recs.slice(k, k + CHUNK).map((x) => x + "\n").join("")).slice(0, 16));
  }
  meta[name] = { count: s.inputs.length, chunk: CHUNK, urlFns: s.urlFns, inputs: inChunks, records: outChunks };
  fs.writeFileSync(path.join(fullDir, `${name}.jsonl`), s.inputs.map((x, i) => `[${JSON.stringify(x)},${recs[i]}]`).join("\n") + "\n");
}
fs.writeFileSync(path.join(here, "suites.json"), JSON.stringify(meta, null, 1) + "\n");

if (process.argv.includes("--sweep")) {
  const t1 = Date.now();
  const sw = runElectron({ sweep: true }).sweep;
  fs.writeFileSync(path.join(here, "host-sweep.json"), "[\n" + sw.map((r) => JSON.stringify(r)).join(",\n") + "\n]\n");
  console.log(`host sweep: ${sw.length} ranges in ${((Date.now() - t1) / 1000).toFixed(1)} s`);
}
for (const s of suites) console.log(`${s.name}: ${s.inputs.length}`);

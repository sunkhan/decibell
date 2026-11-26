#!/usr/bin/env node
// Bundles vectors.ts (which imports the Electron client's real attachmentCrypto.ts modules and
// the thumbhash package) with the repo's esbuild, then runs it:
//
//   ELECTRON_RUN_AS_NODE=1 <electron> gen_vectors.mjs gen <dir> [--big]   regenerate (commit tests/vectors/data)
//   ELECTRON_RUN_AS_NODE=1 <electron> gen_vectors.mjs verify <dir>        open Rust-sealed files with main's code
//
// <electron> = electron-client/node_modules/electron/dist/electron: run it under the Electron
// client's own runtime, because ThumbHash at exact ties (a symmetric picture whose AC terms are
// mathematically 0) is decided by the last ulp of Math.cos, which differs between V8 builds
// (Node 26's V8 14.6 and Electron 33's V8 13.0 disagree on the 100x75 gradient vector). Plain
// `node` works too; the crypto is engine-independent. Needs electron-client/node_modules.
// Used by tests/vectors.rs at test time.
import { createRequire } from "node:module";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "../../../../..");
const electron = join(repo, "electron-client");
const require = createRequire(join(electron, "package.json"));
const esbuild = require("esbuild");

const work = mkdtempSync(join(tmpdir(), "dba-vectors-"));
try {
  const outfile = join(work, "vectors.bundle.mjs");
  await esbuild.build({
    entryPoints: [join(here, "vectors.ts")],
    bundle: true,
    platform: "node",
    format: "esm",
    target: "node20",
    outfile,
    nodePaths: [join(electron, "node_modules")],
    logLevel: "warning",
  });
  const m = await import(pathToFileURL(outfile).href);
  await m.run(process.argv.slice(2));
} finally {
  rmSync(work, { recursive: true, force: true });
}

// Loads KaTeX's internal tables (functions, environments, symbols, macros) straight from the
// TypeScript sources in electron-client/node_modules/katex/src, bundled in memory with the
// esbuild that ships in electron-client/node_modules. Shared by the corpus builder.

import { createRequire } from "node:module";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
export const repoRoot = path.resolve(here, "../../../../..");
export const electronRoot = path.join(repoRoot, "electron-client");
export const katexDir = path.join(electronRoot, "node_modules/katex");
export const requireFromElectron = createRequire(path.join(electronRoot, "package.json"));

export async function loadKatexInternals() {
    const esbuild = requireFromElectron("esbuild");
    const entry = `
import {_functions} from "./defineFunction";
import "./functions";
import {_environments} from "./defineEnvironment";
import "./environments";
import symbols from "./symbols";
import macros from "./macros";
module.exports = {_functions, _environments, symbols, macros};
`;
    const r = await esbuild.build({
        stdin: { contents: entry, resolveDir: path.join(katexDir, "src"), loader: "ts", sourcefile: "entry.ts" },
        bundle: true, write: false, format: "cjs", platform: "node", logLevel: "error",
    });
    const m = { exports: {} };
    new Function("module", "exports", "require", r.outputFiles[0].text)(m, m.exports, requireFromElectron);
    return {
        functions: m.exports._functions,
        environments: m.exports._environments,
        symbols: m.exports.symbols,
        macros: m.exports.macros,
    };
}

export function loadKatex() {
    return requireFromElectron("katex");
}

export const APP_OPTIONS = {
    throwOnError: false,
    errorColor: "#cc0000",
    trust: false,
    maxSize: 16,
    maxExpand: 256,
};

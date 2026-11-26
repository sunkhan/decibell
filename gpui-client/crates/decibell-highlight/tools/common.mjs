// Shared by the extractor and the reference (lowlight) runner: finds the
// Electron client's installed highlight.js / lowlight and reads which
// grammars CodeBlock.tsx registers, so both stay in step with the renderer.

import { readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

export const TOOLS_DIR = dirname(fileURLToPath(import.meta.url));
export const CRATE_DIR = resolve(TOOLS_DIR, "..");
export const ELECTRON_DIR = process.env.DECIBELL_ELECTRON_CLIENT
  ? resolve(process.env.DECIBELL_ELECTRON_CLIENT)
  : resolve(CRATE_DIR, "../../../electron-client");
export const HLJS_DIR = resolve(ELECTRON_DIR, "node_modules/highlight.js");
export const LOWLIGHT_DIR = resolve(ELECTRON_DIR, "node_modules/lowlight");
export const CODEBLOCK = resolve(ELECTRON_DIR, "src/features/chat/CodeBlock.tsx");

export function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

export function pkgVersion(dir) {
  return JSON.parse(readFileSync(resolve(dir, "package.json"), "utf8")).version;
}

/// What CodeBlock.tsx registers: grammar modules in registration order and
/// the extra aliases. Fails loudly when the file stops looking as expected.
export function readCodeBlock() {
  const src = readFileSync(CODEBLOCK, "utf8");
  const imports = new Map();
  for (const m of src.matchAll(
    /^import (\w+) from "highlight\.js\/lib\/languages\/([\w-]+)";/gm,
  )) {
    imports.set(m[1], m[2]);
  }
  const reg = /lowlight\.register\(\{([^}]*)\}\);/.exec(src);
  if (!reg) throw new Error("CodeBlock.tsx: lowlight.register({...}) not found");
  const names = reg[1]
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  for (const n of names) {
    if (!/^\w+$/.test(n)) throw new Error(`CodeBlock.tsx: unexpected register entry "${n}"`);
    if (!imports.has(n)) throw new Error(`CodeBlock.tsx: "${n}" registered but not imported`);
    if (imports.get(n) !== n) throw new Error(`CodeBlock.tsx: "${n}" imports module ${imports.get(n)}`);
  }
  if (names.length !== imports.size) {
    throw new Error("CodeBlock.tsx: imported grammars and registered grammars differ");
  }
  const aliases = {};
  const aliasCalls = [...src.matchAll(/lowlight\.registerAlias\(([^)]*)\)/g)];
  for (const call of aliasCalls) {
    // Only the object-literal form is used: { ini: ["toml"] }
    const body = /^\s*\{([\s\S]*)\}\s*$/.exec(call[1]);
    if (!body) throw new Error("CodeBlock.tsx: unsupported registerAlias form");
    for (const m of body[1].matchAll(/(\w+)\s*:\s*\[([^\]]*)\]/g)) {
      aliases[m[1]] = [...m[2].matchAll(/"([^"]+)"/g)].map((x) => x[1]);
    }
  }
  if (!/highlight\(value, \{ignoreIllegals: true, language\}\)/.test(
    readFileSync(resolve(LOWLIGHT_DIR, "lib/index.js"), "utf8"),
  )) {
    throw new Error("lowlight no longer calls highlight(value, {ignoreIllegals: true, language})");
  }
  if (!/lowlight\.highlight\("plaintext", code\)/.test(src)) {
    throw new Error("CodeBlock.tsx: the plaintext fallback changed");
  }
  return { names, aliases };
}

/// The ES-module files the renderer's bundler resolves (package `exports`,
/// `import` condition).
export function grammarFile(name) {
  return resolve(HLJS_DIR, "es/languages", `${name}.js`);
}
export const CORE_FILE = resolve(HLJS_DIR, "es/core.js");

export async function loadGrammar(name) {
  return (await import(pathToFileURL(grammarFile(name)).href)).default;
}
export async function loadCore() {
  return (await import(pathToFileURL(CORE_FILE).href)).default;
}
export async function loadLowlight() {
  return await import(pathToFileURL(resolve(LOWLIGHT_DIR, "lib/index.js")).href);
}

/// A lowlight set up exactly as CodeBlock.tsx does. `create()` returns the
/// instance; `create.withLanguages()` also returns the language objects
/// highlight.js compiles in place (lowlight keeps its highlight.js private),
/// which the reference runner uses to inspect matcher state.
export async function makeLowlight() {
  const { createLowlight } = await loadLowlight();
  const { names, aliases } = readCodeBlock();
  const grammars = {};
  for (const n of names) grammars[n] = await loadGrammar(n);
  const withLanguages = () => {
    const languages = {};
    const wrapped = {};
    for (const n of names) {
      wrapped[n] = (hljs) => (languages[n] = grammars[n](hljs));
    }
    const lowlight = createLowlight();
    lowlight.register(wrapped);
    lowlight.registerAlias(aliases);
    return { lowlight, languages };
  };
  const create = () => withLanguages().lowlight;
  create.withLanguages = withLanguages;
  return create;
}

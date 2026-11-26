#!/usr/bin/env node
// Builds tools/corpus/corpus.jsonl: {"id","src","tex","display"} per formula, deduplicated on
// (tex, display). Sources, in order:
//   spec-*   expressions extracted from KaTeX v0.18.4 test/katex-spec.ts (fetched, cached)
//   ss-*     test/screenshotter/ss_data.yaml entries (fetched, cached)
//   fn-*     every function in KaTeX's `_functions` with generated arguments
//   env-*    every environment in `_environments`
//   sym-*    every math- and text-mode symbol
//   macro-*  every built-in macro
//   hw-*     handwritten.txt (everyday formulas, errors, adversarial)
//   adv-*    generated adversarial input (deep nesting, long input, macro bombs)
//
// Usage: node build_corpus.mjs [--offline]   (--offline: use the cache, never fetch)

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { extractSpecExpressions } from "./extract_spec.mjs";
import { loadKatexInternals, requireFromElectron } from "./katex_internals.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const upstreamDir = path.join(here, "upstream");
const TAG = "v0.18.4";
const UPSTREAM = {
    "katex-spec.ts": `https://raw.githubusercontent.com/KaTeX/KaTeX/${TAG}/test/katex-spec.ts`,
    "ss_data.yaml": `https://raw.githubusercontent.com/KaTeX/KaTeX/${TAG}/test/screenshotter/ss_data.yaml`,
};
const offline = process.argv.includes("--offline");

async function fetchUpstream() {
    fs.mkdirSync(upstreamDir, { recursive: true });
    for (const [name, url] of Object.entries(UPSTREAM)) {
        const p = path.join(upstreamDir, name);
        if (fs.existsSync(p)) continue;
        if (offline) throw new Error(`missing ${p} and --offline given`);
        const res = await fetch(url);
        if (!res.ok) throw new Error(`fetch ${url}: ${res.status}`);
        fs.writeFileSync(p, await res.text());
        console.error(`fetched ${url}`);
    }
}

const entries = [];
const seen = new Set();
const counters = {};
function add(src, tex, display, idHint) {
    if (typeof tex !== "string") return;
    const key = (display ? "D" : "I") + tex;
    if (seen.has(key)) return;
    seen.add(key);
    counters[src] = (counters[src] || 0) + 1;
    const id = idHint ? `${src}-${idHint}${display ? "-D" : ""}` : `${src}-${String(counters[src]).padStart(4, "0")}`;
    entries.push({ id, src, tex, display: !!display });
}
function slug(s) {
    return s.replace(/^\\/, "").replace(/[^A-Za-z0-9]+/g, (m) =>
        [...m].map((c) => "_" + c.codePointAt(0).toString(16)).join("")).slice(0, 40) || "x";
}

// ---------------------------------------------------------------- spec + screenshotter
function specCorpus() {
    const src = fs.readFileSync(path.join(upstreamDir, "katex-spec.ts"), "utf8");
    const exprs = extractSpecExpressions(src).filter((s) => !/^<(span|m[a-z]+|math|svg)\b/.test(s));
    let n = 0;
    for (const tex of exprs) {
        add("spec", tex, false);
        if (n++ % 4 === 0) add("spec", tex, true);
    }
}

function ssCorpus() {
    const yaml = requireFromElectron("js-yaml");
    const data = yaml.load(fs.readFileSync(path.join(upstreamDir, "ss_data.yaml"), "utf8"));
    for (const [name, v] of Object.entries(data)) {
        const e = typeof v === "string" ? { tex: v } : v;
        if (typeof e.tex !== "string") continue;
        // Options we can't reproduce (macros, custom styles/pre/post) are dropped: the TeX is
        // still a useful input; with macros undefined it exercises the error path instead.
        const tex = e.tex.replace(/\n$/, "");
        add("ss", tex, !!e.display, slug(name));
        if (!e.display) add("ss", tex, true, slug(name));
    }
}

// ---------------------------------------------------------------- tables
const ARG = { color: "red", size: "1em", url: "x", raw: "x", math: "x", text: "ab", hbox: "ab", primitive: "x", original: "x" };
function argFor(type, optional) {
    const v = ARG[type ?? "original"] ?? "x";
    return optional ? `[${type === undefined || type === "original" || type === "math" ? "2" : v}]` : `{${v}}`;
}

const FN_OVERRIDES = {
    "\\left": ["\\left( \\frac ab \\right)", "\\left. x \\right|", "\\left\\{ x \\right\\}"],
    "\\right": ["\\left[ x^2 \\right]"],
    "\\middle": ["\\left( a \\middle| b \\right)", "\\left\\{ x \\middle\\| y \\right\\}"],
    "\\begin": [], "\\end": [],
    "\\\\": ["a \\\\ b", "a \\\\[1em] b"],
    "\\newline": ["a \\newline b"],
    "\\cr": ["\\begin{matrix} a \\cr b \\end{matrix}"],
    "\\verb": ["\\verb|a b|", "\\verb*|a b|"],
    "\\def": ["\\def\\foo#1{#1^2}\\foo{x}"],
    "\\gdef": ["\\gdef\\foo#1{#1^2}\\foo{x}"],
    "\\edef": ["\\def\\a{x}\\edef\\b{\\a\\a}\\b"],
    "\\xdef": ["\\def\\a{x}\\xdef\\b{\\a\\a}\\b"],
    "\\let": ["\\let\\foo=x \\foo", "\\let\\foo\\alpha\\foo"],
    "\\futurelet": ["\\futurelet\\foo\\bar x"],
    "\\global": ["\\global\\def\\foo{x}\\foo"],
    "\\long": ["\\long\\def\\foo{x}\\foo"],
    "\\operatorname": ["\\operatorname{sn} x", "\\operatorname{sn}_x^y"],
    "\\operatorname*": ["\\operatorname*{argmax}_x f"],
    "\\operatornamewithlimits": ["\\operatornamewithlimits{argmax}_x f"],
    "\\rule": ["\\rule{1em}{0.5em}", "\\rule[0.2em]{2em}{1em}"],
    "\\color": ["\\color{red} x + y", "a {\\color{#0a0} b} c"],
    "\\textcolor": ["\\textcolor{red}{x}", "\\textcolor{#00f}{x+y}"],
    "\\colorbox": ["\\colorbox{red}{ab}", "\\colorbox{#ffcc00}{$x^2$}"],
    "\\fcolorbox": ["\\fcolorbox{red}{yellow}{ab}"],
    "\\raisebox": ["\\raisebox{0.5em}{ab} c", "\\raisebox{-0.25em}{ab} c"],
    "\\genfrac": ["\\genfrac(]{0pt}{2}{a+1}{b+2}", "\\genfrac{}{}{1pt}{0}{a}{b}"],
    "\\sqrt": ["\\sqrt{x}", "\\sqrt[3]{x}", "\\sqrt{\\frac ab}"],
    "\\tag": [], "\\tag*": [],
    "\\over": ["a \\over b"], "\\choose": ["n \\choose k"], "\\atop": ["a \\atop b"],
    "\\brace": ["n \\brace k"], "\\brack": ["n \\brack k"], "\\above": ["a \\above 1pt b"],
    "\\mathchoice": ["\\mathchoice{D}{T}{S}{SS}"],
    "\\char": ["\\char\"41 \\char`a"],
    "\\kern": ["a\\kern1em b", "a\\kern{-0.5em} b"],
    "\\mkern": ["a\\mkern18mu b"], "\\hskip": ["a\\hskip 2em b"], "\\mskip": ["a\\mskip 9mu b"],
    "\\hspace": ["a\\hspace{1em}b", "a\\hspace*{1em}b"],
    "\\limits": [], "\\nolimits": [],
    "\\phantom": ["a\\phantom{xyz}b"], "\\hphantom": ["a\\hphantom{xyz}b"], "\\vphantom": ["a\\vphantom{\\frac ab}b"],
    "\\smash": ["\\smash{\\frac ab}", "\\smash[t]{y}", "\\smash[b]{y}"],
    "\\llap": ["a\\llap{bb}"], "\\rlap": ["\\rlap{bb}a"], "\\clap": ["a\\clap{bbb}c"],
    "\\mathllap": ["a\\mathllap{bb}"], "\\mathrlap": ["\\mathrlap{bb}a"], "\\mathclap": ["\\sum_{\\mathclap{1\\le i\\le n}} x"],
    "\\xrightarrow": ["A \\xrightarrow{f} B", "A \\xrightarrow[below]{above} B"],
    "\\overbrace": ["\\overbrace{a+b+c}^{n}"], "\\underbrace": ["\\underbrace{a+b+c}_{n}"],
    "\\overbracket": ["\\overbracket{a+b+c}^{n}"], "\\underbracket": ["\\underbracket{a+b+c}_{n}"],
    "\\cancel": ["\\cancel{x}", "\\cancel{a+b}"], "\\bcancel": ["\\bcancel{x}"], "\\xcancel": ["\\xcancel{ab}"],
    "\\sout": ["\\text{\\sout{abc}}"],
    "\\boxed": ["\\boxed{x^2}"], "\\fbox": ["\\fbox{ab}"],
    "\\angl": ["a_{\\angl n}"], "\\phase": ["\\phase{-78^\\circ}"],
    "\\includegraphics": ["\\includegraphics[height=1em]{x.png}"],
    "\\href": ["\\href{https://example.com}{x}"], "\\url": ["\\url{https://example.com}"],
    "\\htmlClass": ["\\htmlClass{foo}{x}"], "\\htmlId": ["\\htmlId{foo}{x}"],
    "\\htmlStyle": ["\\htmlStyle{color:red}{x}"], "\\htmlData": ["\\htmlData{foo=a}{x}"],
    "\\html@mathml": ["\\html@mathml{a}{b}"],
    "\\@char": ["\\@char{65}"],
    "\\relax": ["a\\relax b"],
    "\\noexpand": ["\\noexpand x"],
    "\\expandafter": ["\\expandafter x"],
    "\\TextOrMath": ["\\TextOrMath{a}{b}", "\\text{\\TextOrMath{a}{b}}"],
    "\\newcommand": ["\\newcommand\\foo[1]{#1^2}\\foo{x}", "\\newcommand{\\bar}{x}"],
    "\\renewcommand": ["\\renewcommand\\alpha{x}\\alpha"],
    "\\providecommand": ["\\providecommand\\foo{x}\\foo"],
    "\\message": ["\\message{hi} x"], "\\errmessage": ["\\errmessage{hi} x"], "\\show": ["\\show\\alpha x"],
    "\\dots": ["a \\dots b", "a + \\dots + b", "f(a, \\dots, b)"], "\\dotso": ["a \\dotso b"],
    "\\dotsc": ["a, \\dotsc, b"], "\\cdots": ["a + \\cdots + b"],
    "\\tag@literal": [],
    "\\@firstoftwo": ["\\@firstoftwo{a}{b}"], "\\@secondoftwo": ["\\@secondoftwo{a}{b}"],
    "\\@ifnextchar": ["\\@ifnextchar[{a}{b}[", "\\@ifnextchar[{a}{b}c"],
    "\\bra@ket": [], "\\bra@set": [],
    "\\bra": ["\\bra{\\phi}"], "\\ket": ["\\ket{\\psi}"], "\\braket": ["\\braket{\\phi|\\psi}"],
    "\\Bra": ["\\Bra{\\phi}"], "\\Ket": ["\\Ket{\\psi}"], "\\Braket": ["\\Braket{\\phi | \\frac{\\partial^2}{\\partial t^2} | \\psi}"],
    "\\set": ["\\set{x | x > 0}"], "\\Set": ["\\Set{x | \\frac x2 > 0}"],
};
const DISPLAY_TYPES = new Set(["op", "operatorname", "genfrac", "delimsizing", "leftright", "sqrt", "xArrow",
    "horizBrace", "accent", "accentUnder", "enclose", "array", "mclass", "styling", "sizing"]);

function fnCorpus(functions) {
    for (const name of Object.keys(functions).sort()) {
        const s = functions[name];
        const hint = slug(name);
        let texs = FN_OVERRIDES[name];
        if (!texs && s.type === "delimsizing") texs = [`${name}(`, `${name}\\{`, `${name}\\|`];
        if (!texs) {
            const nOpt = s.numOptionalArgs || 0;
            const total = s.numArgs + nOpt;
            const at = s.argTypes || [];
            if (s.infix) {
                texs = [`a ${name}${at[0] === "size" ? " 1pt" : ""} b`];
            } else {
                let req = "";
                for (let i = nOpt; i < total; i++) req += argFor(at[i], false);
                let opt = "";
                for (let i = 0; i < nOpt; i++) opt += argFor(at[i], true);
                const sep = /[A-Za-z@]$/.test(name) && total === 0 ? " " : "";
                texs = [`${name}${sep}${req}`];
                if (nOpt) texs.push(`${name}${opt}${req}`);
                if (total === 0 && s.type === "op") texs.push(`${name}_a^b x`);
                if (total === 0 && s.type === "atom") texs.push(`a ${name} b`);
            }
            if (s.allowedInMath === false) texs = texs.map((t) => `\\text{${t}}`);
            else if (s.allowedInText && !s.allowedInMath === false && s.type === "text") {
                // text-producing functions: also inside \text
            }
        }
        texs.forEach((tex, k) => {
            add("fn", tex, false, `${hint}-${k}`);
            if (DISPLAY_TYPES.has(s.type)) add("fn", tex, true, `${hint}-${k}`);
        });
        // also exercise text-mode usage of functions allowed in text
        if (s.allowedInText && !FN_OVERRIDES[name] && s.numArgs <= 1 && !s.infix && s.allowedInMath !== false) {
            const at = s.argTypes || [];
            const req = s.numArgs ? argFor(at[(s.numOptionalArgs || 0)], false) : "";
            const sep = /[A-Za-z@]$/.test(name) && !req ? " " : "";
            add("fn", `\\text{a ${name}${sep}${req} b}`, false, `${hint}-text`);
        }
    }
    // tags (display only) and \limits/\nolimits
    add("fn", "x^2 \\tag{1}", true, "tag");
    add("fn", "x^2 \\tag*{A}", true, "tag-star");
    add("fn", "\\sum\\limits_{i=1}^n i + \\int\\nolimits_0^1 x", false, "limits");
    add("fn", "\\sum\\limits_{i=1}^n i + \\int\\nolimits_0^1 x", true, "limits");
}

const ENV_BODIES = {
    "array": ["\\begin{array}{c|l} a & b \\\\ \\hline c & d \\end{array}", "\\begin{array}{rcl:c} 1 & 22 & 333 & 4 \\\\ \\hdashline x & y & z & w \\end{array}"],
    "darray": ["\\begin{darray}{cc} \\frac ab & c \\\\ d & e \\end{darray}"],
    "subarray": ["\\sum_{\\begin{subarray}{l} i<n \\\\ j<m \\end{subarray}} x"],
    "smallmatrix": ["\\begin{smallmatrix} a&b\\\\c&d \\end{smallmatrix}"],
    "cases": ["f(x) = \\begin{cases} a & x>0 \\\\ b & \\text{otherwise} \\end{cases}"],
    "dcases": ["f(x) = \\begin{dcases} \\frac a2 & x>0 \\\\ b & \\text{otherwise} \\end{dcases}"],
    "rcases": ["\\begin{rcases} a & x>0 \\\\ b & x<0 \\end{rcases} = y"],
    "drcases": ["\\begin{drcases} \\frac a2 & x>0 \\\\ b & x<0 \\end{drcases} = y"],
    "align": ["\\begin{align} a &= b + c \\\\ d &= e \\end{align}"],
    "align*": ["\\begin{align*} a &= b & c &= d \\\\ e &= f & g &= h \\end{align*}"],
    "aligned": ["\\begin{aligned} a &= b + c \\\\ d &= e \\end{aligned}"],
    "split": ["\\begin{split} a &= b + c \\\\ &= d \\end{split}"],
    "gathered": ["\\begin{gathered} a = b \\\\ c = d + e \\end{gathered}"],
    "gather": ["\\begin{gather} a = b \\\\ c = d + e \\end{gather}"],
    "gather*": ["\\begin{gather*} a = b \\\\ c = d + e \\end{gather*}"],
    "alignat": ["\\begin{alignat}{2} a &= b & c &= d \\\\ e &= f & g &= h \\end{alignat}"],
    "alignat*": ["\\begin{alignat*}{2} a &= b & c &= d \\\\ e &= f & g &= h \\end{alignat*}"],
    "alignedat": ["\\begin{alignedat}{2} a &= b & c &= d \\\\ e &= f & g &= h \\end{alignedat}"],
    "equation": ["\\begin{equation} a = b \\end{equation}"],
    "equation*": ["\\begin{equation*} a = b \\end{equation*}"],
    "CD": ["\\begin{CD} A @>a>> B \\\\ @VbVV @AAcA \\\\ C @= D \\end{CD}"],
};
function envCorpus(environments) {
    for (const name of Object.keys(environments).sort()) {
        let texs = ENV_BODIES[name];
        if (!texs) {
            if (/matrix\*$/.test(name)) {
                texs = [`\\begin{${name}}[r] -1 & 2 \\\\ 3 & -4 \\end{${name}}`, `\\begin{${name}} a & b \\\\ c & d \\end{${name}}`];
            } else {
                texs = [`\\begin{${name}} a & b \\\\ c & d \\end{${name}}`];
            }
        }
        texs.forEach((tex, k) => {
            add("env", tex, true, `${slug(name)}-${k}`);
            add("env", tex, false, `${slug(name)}-${k}`);
        });
    }
}

function symCorpus(symbols) {
    for (const mode of ["math", "text"]) {
        const plain = [];
        for (const key of Object.keys(symbols[mode]).sort()) {
            const info = symbols[mode][key];
            const isCmd = key.startsWith("\\");
            const g = info.group;
            if (!isCmd && (g === "mathord" || g === "textord") && [...key].length === 1 && key !== "\\") {
                plain.push(key);
                continue;
            }
            let tex;
            if (mode === "math") {
                const k = isCmd && /[A-Za-z]$/.test(key) ? key + " " : key;
                if (g === "bin" || g === "rel" || g === "punct" || g === "spacing") tex = `a ${k}b`;
                else if (g === "open") tex = `${k}x`;
                else if (g === "close") tex = `x${k}`;
                else if (g === "accent-token") tex = `${k}{x}`;
                else if (g === "op-token") tex = `${k}_a^b`;
                else tex = key;
            } else {
                const k = isCmd && /[A-Za-z]$/.test(key) ? key + " " : key;
                if (g === "accent-token") tex = `\\text{${key}{a}}`;
                else if (g === "spacing") tex = `\\text{a${k}b}`;
                else tex = `\\text{${key}}`;
            }
            add("sym", tex, false, `${mode}-${slug(key)}`);
            if (mode === "math" && g === "op-token") add("sym", tex, true, `${mode}-${slug(key)}`);
        }
        for (let i = 0; i < plain.length; i += 8) {
            const chunk = plain.slice(i, i + 8);
            const body = chunk.join(" ");
            add("sym", mode === "math" ? body : `\\text{${chunk.join("")}}`, false, `${mode}-plain-${i / 8}`);
        }
    }
}

function macroCorpus(macros, functions) {
    let n = 0;
    for (const name of Object.keys(macros).sort()) {
        if (FN_OVERRIDES[name]) {
            FN_OVERRIDES[name].forEach((tex, k) => add("macro", tex, false, `${slug(name)}-${k}`));
            continue;
        }
        if (functions[name]) continue; // covered by fn-*
        const body = macros[name];
        let numArgs = 0;
        if (typeof body === "string" && body.includes("#")) {
            const stripped = body.replace(/##/g, "");
            while (stripped.includes("#" + (numArgs + 1))) ++numArgs;
        }
        const isCmd = name.startsWith("\\");
        const sep = isCmd && /[A-Za-z@]$/.test(name) && numArgs === 0 ? " " : "";
        const tex = `${name}${sep}${"{x}".repeat(numArgs)}`;
        add("macro", tex, false, slug(name));
        if (n++ % 4 === 0) add("macro", tex, true, slug(name));
        add("macro", `\\text{${tex}}`, false, `${slug(name)}-text`);
    }
}

// ---------------------------------------------------------------- handwritten
function handwrittenCorpus() {
    const lines = fs.readFileSync(path.join(here, "handwritten.txt"), "utf8").split("\n");
    let section = "misc";
    const counts = {};
    for (const raw of lines) {
        if (raw.startsWith("## ")) { section = raw.slice(3).trim(); continue; }
        if (raw.startsWith("# ") || raw.trim() === "" || raw === "#") continue;
        let mode = "I";
        let tex = raw;
        const m = /^(D|B|I): ?/.exec(raw);
        if (m) { mode = m[1]; tex = raw.slice(m[0].length); }
        counts[section] = (counts[section] || 0) + 1;
        const hint = `${section}-${String(counts[section]).padStart(3, "0")}`;
        if (mode !== "D") add("hw", tex, false, hint);
        if (mode !== "I") add("hw", tex, true, hint);
    }
}

function adversarialCorpus() {
    const A = (hint, tex, display = false) => add("adv", tex, display, hint);
    A("braces-200", "{".repeat(200) + "x" + "}".repeat(200));
    A("braces-1000", "{".repeat(1000) + "x" + "}".repeat(1000));
    A("frac-30", "\\frac{".repeat(30) + "x" + "}{y}".repeat(30));
    A("frac-30", "\\frac{".repeat(30) + "x" + "}{y}".repeat(30), true);
    A("sup-30", "x^{".repeat(30) + "x" + "}".repeat(30));
    A("sub-30", "x_{".repeat(30) + "x" + "}".repeat(30));
    A("sqrt-30", "\\sqrt{".repeat(30) + "x" + "}".repeat(30));
    A("leftright-50", "\\left(".repeat(50) + "x" + "\\right)".repeat(50));
    A("leftright-50", "\\left(".repeat(50) + "x" + "\\right)".repeat(50), true);
    A("text-nest-50", "\\text{".repeat(50) + "a" + "}".repeat(50));
    A("long-sum-500", Array.from({ length: 500 }, (_, i) => `x_{${i}}`).join("+"));
    A("long-alpha-300", "\\alpha".repeat(300));
    A("matrix-20x20", "\\begin{pmatrix}" + Array.from({ length: 20 }, () => Array.from({ length: 20 }, (_, j) => `a_${j}`).join("&")).join("\\\\") + "\\end{pmatrix}", true);
    A("array-200cols", "\\begin{array}{" + "c".repeat(200) + "}" + Array.from({ length: 200 }, () => "x").join("&") + "\\end{array}", true);
    A("bomb-under", "\\def\\a{\\b\\b}\\def\\b{\\c\\c}\\def\\c{\\d\\d}\\def\\d{xx}\\a");
    A("bomb-over", "\\def\\a{\\b\\b\\b\\b}\\def\\b{\\c\\c\\c\\c}\\def\\c{\\d\\d\\d\\d}\\def\\d{xxxx}\\a");
    A("bomb-exp", "\\def\\a#1{#1#1}\\a{\\a{\\a{\\a{\\a{\\a{\\a{\\a{x}}}}}}}}");
    A("recursion", "\\def\\x{\\x}\\x");
    A("recursion-grow", "\\def\\x{x\\x}\\x");
    A("newcommand-loop", "\\newcommand\\x{\\x}\\x");
    A("rule-huge", "\\rule{100em}{100em}");
    A("rule-huge-pt", "\\rule{999999pt}{1em}");
    A("rule-negative", "a\\rule{-5em}{2em}b");
    A("kern-huge", "a\\kern10000em b");
    A("hspace-huge", "a\\hspace{999in}b");
    A("raisebox-huge", "\\raisebox{1000em}{x}");
    A("sizing-stack", "\\Huge\\Huge\\Huge x \\tiny\\tiny y");
    A("big-delim-stack", "\\Bigg(\\Bigg(\\Bigg(\\Bigg(x\\Bigg)\\Bigg)\\Bigg)\\Bigg)");
    A("tall-delim", "\\left(\\rule{1em}{15em}\\right)", true);
    A("vec-nest", "\\vec{\\vec{\\vec{\\vec{\\vec{x}}}}}");
    A("widehat-long", "\\widehat{" + "x".repeat(100) + "}");
    A("overbrace-long", "\\overbrace{" + "x+".repeat(60) + "x}^{n}");
    A("unicode-mix", "αβγ∑∫√∞≠≤≥ℝ𝔸𝕏ℵ⨁∮");
    A("cjk-text", "\\text{数学 한국어 日本語}");
    A("emoji", "\\text{🎉👍} x");
    A("control-chars", "a\u0007b");
    A("lone-backslash", "a\\");
    A("only-spaces", "   ");
    A("unbalanced-open", "{{{x");
    A("unbalanced-close", "x}}}");
    A("double-sup", "x^1^2");
    A("many-primes", "f" + "'".repeat(100));
    A("verb-unterminated", "\\verb|abc");
    A("tag-inline", "x \\tag{1}");
    A("deep-env", "\\begin{matrix}".repeat(20) + "x" + "\\end{matrix}".repeat(20));
}

// ---------------------------------------------------------------- main
await fetchUpstream();
const K = await loadKatexInternals();
handwrittenCorpus();
specCorpus();
ssCorpus();
fnCorpus(K.functions);
envCorpus(K.environments);
symCorpus(K.symbols);
macroCorpus(K.macros, K.functions);
adversarialCorpus();

const out = path.join(here, "corpus.jsonl");
fs.writeFileSync(out, entries.map((e) => JSON.stringify(e)).join("\n") + "\n");
const bySrc = {};
for (const e of entries) {
    const k = e.src + (e.display ? " (display)" : "");
    bySrc[k] = (bySrc[k] || 0) + 1;
}
console.error(`wrote ${entries.length} formulas to ${out}`);
console.error(bySrc);

// Extracts TeX expressions from KaTeX's test/katex-spec.ts without executing it: a small
// JavaScript lexer finds every string / template literal, decides from the surrounding code
// whether it is a TeX input (argument of expect / getBuilt / getParsed / toParseLike …) or
// something else (test names, expected types, error messages), and decodes it the way the
// test does (tagged templates are raw — KaTeX's `expect`/`r` tags use String.raw semantics —
// quoted strings and untagged templates are cooked). Adjacent literals joined with `+` are
// concatenated. Templates with ${…} substitutions are skipped.

function decodeEscapes(s) {
    let out = "";
    for (let i = 0; i < s.length; i++) {
        const c = s[i];
        if (c !== "\\") { out += c; continue; }
        const n = s[++i];
        switch (n) {
            case "n": out += "\n"; break;
            case "t": out += "\t"; break;
            case "r": out += "\r"; break;
            case "b": out += "\b"; break;
            case "f": out += "\f"; break;
            case "v": out += "\v"; break;
            case "0": out += "\0"; break;
            case "x": out += String.fromCharCode(parseInt(s.substr(i + 1, 2), 16)); i += 2; break;
            case "u":
                if (s[i + 1] === "{") {
                    const end = s.indexOf("}", i);
                    out += String.fromCodePoint(parseInt(s.slice(i + 2, end), 16));
                    i = end;
                } else {
                    out += String.fromCharCode(parseInt(s.substr(i + 1, 4), 16));
                    i += 4;
                }
                break;
            case "\r": if (s[i + 1] === "\n") i++; break; // line continuation
            case "\n": break;
            default: out += n;
        }
    }
    return out;
}

/** Tokenises JS/TS source into literal records with the code around them. */
export function lexLiterals(src) {
    const lits = [];
    let i = 0;
    let code = ""; // code text since the previous literal
    let lastSig = ""; // last significant code char (for regex detection)
    const n = src.length;

    function readQuoted(q) {
        let j = i + 1;
        let raw = "";
        while (j < n && src[j] !== q) {
            if (src[j] === "\\") { raw += src[j] + src[j + 1]; j += 2; continue; }
            raw += src[j++];
        }
        i = j + 1;
        return raw;
    }

    // Returns {raw, hasSubst}; i ends after the closing backtick.
    function readTemplate() {
        let j = i + 1;
        let raw = "";
        let hasSubst = false;
        while (j < n && src[j] !== "`") {
            if (src[j] === "\\") { raw += src[j] + src[j + 1]; j += 2; continue; }
            if (src[j] === "$" && src[j + 1] === "{") {
                hasSubst = true;
                // skip the substitution expression (balanced braces; nested literals)
                let depth = 1;
                j += 2;
                while (j < n && depth > 0) {
                    const c = src[j];
                    if (c === "{") depth++;
                    else if (c === "}") depth--;
                    else if (c === "\"" || c === "'") {
                        const save = i; i = j; readQuoted(c); j = i; i = save; continue;
                    } else if (c === "`") {
                        const save = i; i = j; readTemplate(); j = i; i = save; continue;
                    }
                    j++;
                }
                raw += "${…}";
                continue;
            }
            raw += src[j++];
        }
        i = j + 1;
        return { raw, hasSubst };
    }

    while (i < n) {
        const c = src[i];
        const c2 = src[i + 1];
        if (c === "/" && c2 === "/") {
            const e = src.indexOf("\n", i);
            i = e === -1 ? n : e;
            continue;
        }
        if (c === "/" && c2 === "*") {
            const e = src.indexOf("*/", i + 2);
            i = e === -1 ? n : e + 2;
            continue;
        }
        if (c === "/" && (lastSig === "" || "(,=:[!&|?{};+-*%<>~^".includes(lastSig))) {
            // regex literal
            let j = i + 1;
            let inClass = false;
            while (j < n) {
                const d = src[j];
                if (d === "\\") { j += 2; continue; }
                if (d === "[") inClass = true;
                else if (d === "]") inClass = false;
                else if (d === "/" && !inClass) break;
                else if (d === "\n") break;
                j++;
            }
            j++;
            while (j < n && /[a-z]/.test(src[j])) j++;
            code += src.slice(i, j);
            lastSig = ")";
            i = j;
            continue;
        }
        if (c === "\"" || c === "'") {
            const raw = readQuoted(c);
            lits.push({ kind: "str", raw, value: decodeEscapes(raw), before: code });
            code = "";
            lastSig = ")";
            continue;
        }
        if (c === "`") {
            const tagMatch = /([A-Za-z_$][\w$]*)\s*$/.exec(code);
            const tagged = tagMatch && /[\w$]$/.test(code);
            const { raw, hasSubst } = readTemplate();
            lits.push({
                kind: "tpl", raw, hasSubst,
                tag: tagged ? tagMatch[1] : null,
                value: hasSubst ? null : (tagged ? raw : decodeEscapes(raw)),
                before: code,
            });
            code = "";
            lastSig = ")";
            continue;
        }
        code += c;
        if (!/\s/.test(c)) lastSig = c;
        i++;
    }
    // attach following code to each literal
    for (let k = 0; k < lits.length; k++) {
        lits[k].after = k + 1 < lits.length ? lits[k + 1].before : code;
    }
    return lits;
}

const TEX_TAGS = new Set(["expect", "r", "getBuilt", "getParsed", "toParseLike", "toBuildLike",
    "toNotParse", "toNotBuild", "toParse", "toBuild"]);
const DENY_CALLEE = /\b(it|describe|test|toEqual|toBe|toMatch|toContain|toThrow|toThrowError|toFailWithParseError|toHaveProperty|toHaveLength|toWarn|toStrictEqual|toMatchObject|hasClass|setAttribute|getAttribute|querySelector|querySelectorAll|getElementsByClassName|createElement|import|require|log|warn|Error|ParseError|Token|toBeCalledWith|toHaveBeenCalledWith|join|split|replace|indexOf|includes|startsWith|endsWith|spyOn|fn|mockImplementation)\s*\(\s*$/;
const DENY_KEYS = /\b(type|family|mode|name|font|mclass|label|style|size|unit|href|src|alt|loc|classes|className|output|strict|errorColor|color|delim|left|right|tag|variant|mathvariant|encoding)\s*:\s*$/;

export function extractSpecExpressions(src) {
    const lits = lexLiterals(src);
    // merge `"a" + "b"` chains
    const merged = [];
    for (const l of lits) {
        const prev = merged[merged.length - 1];
        if (prev && l.before.trim() === "+" && prev.value != null && l.value != null) {
            prev.value += l.value;
            prev.after = l.after;
            continue;
        }
        merged.push({ ...l });
    }
    const out = [];
    for (const l of merged) {
        if (l.value == null) continue;
        const before = l.before.replace(/\s+/g, " ");
        const after = l.after.replace(/\s+/g, " ").trimStart();
        if (l.kind === "tpl" && l.tag) {
            if (!TEX_TAGS.has(l.tag)) continue;
        } else {
            if (/^\s*import\b/.test(before.split(";").pop() || "") || /\bfrom\s*$/.test(before)) continue;
            if (DENY_CALLEE.test(before)) continue;
            if (DENY_KEYS.test(before)) continue;
            if (after.startsWith(":")) continue; // object key
            if (/\[\s*$/.test(before) && /^\]/.test(after) && !/(=|\(|,)\s*\[\s*$/.test(before)) continue; // index access obj["x"]
            if (/(===|!==|==|!=)\s*$/.test(before) || /^(===|!==|==|!=)/.test(after)) continue; // comparisons
        }
        const v = l.value;
        // test-name-like English sentences
        if (/^[A-Za-z][A-Za-z ,.'()-]*$/.test(v) && v.split(" ").length >= 3) continue;
        out.push(v);
    }
    return out;
}

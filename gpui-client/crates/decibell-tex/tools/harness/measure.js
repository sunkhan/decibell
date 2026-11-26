// Runs inside the harness page (page.html). Renders formulas with the page's KaTeX and the
// app's options, then measures what Chromium laid out. All coordinates are CSS px relative to
// the formula's ORIGIN: x = the left edge where the formula starts, y = the baseline of its
// first line (positive downwards, like the DOM). See README.md for the record format.
"use strict";

(function () {
    const OPTIONS = { throwOnError: false, errorColor: "#cc0000", trust: false, maxSize: 16, maxExpand: 256 };
    const SLOT = 4000; // px between formulas (each starts on a whole-pixel y)
    const FACES = [
        ["KaTeX_AMS", "normal", 400], ["KaTeX_Caligraphic", "normal", 700], ["KaTeX_Caligraphic", "normal", 400],
        ["KaTeX_Fraktur", "normal", 700], ["KaTeX_Fraktur", "normal", 400], ["KaTeX_Main", "normal", 700],
        ["KaTeX_Main", "italic", 700], ["KaTeX_Main", "italic", 400], ["KaTeX_Main", "normal", 400],
        ["KaTeX_Math", "italic", 700], ["KaTeX_Math", "italic", 400], ["KaTeX_SansSerif", "normal", 700],
        ["KaTeX_SansSerif", "italic", 400], ["KaTeX_SansSerif", "normal", 400], ["KaTeX_Script", "normal", 400],
        ["KaTeX_Size1", "normal", 400], ["KaTeX_Size2", "normal", 400], ["KaTeX_Size3", "normal", 400],
        ["KaTeX_Size4", "normal", 400], ["KaTeX_Typewriter", "normal", 400],
    ];
    const PROBE_CSS = "display:inline-block;width:0;height:0;margin:0;padding:0;border:0;vertical-align:baseline";

    // 1e-7 keeps LayoutUnit values exact (multiples of 1/(64·DSF) px, e.g. 0.0078125 at DSF 2).
    const r4 = (v) => Math.round(v * 1e7) / 1e7;

    function hexColor(c) {
        const m = /^rgba?\(([^)]*)\)$/.exec(c);
        if (!m) return c;
        const p = m[1].split(/[,\s/]+/).filter(Boolean).map(Number);
        const h = (v) => Math.round(v).toString(16).padStart(2, "0");
        let s = "#" + h(p[0]) + h(p[1]) + h(p[2]);
        if (p.length > 3 && p[3] !== 1) s += h(p[3] * 255);
        return s;
    }
    const firstFamily = (f) => f.split(",")[0].trim().replace(/^["']|["']$/g, "");
    function fnv(s) {
        let h = 0x811c9dc5;
        for (let i = 0; i < s.length; i++) {
            h ^= s.charCodeAt(i);
            h = Math.imul(h, 0x01000193) >>> 0;
        }
        return h.toString(16).padStart(8, "0");
    }
    function rel(rect, o) {
        return { x: r4(rect.left - o.x), y: r4(rect.top - o.y), w: r4(rect.width), h: r4(rect.height) };
    }
    function intersect(a, b) {
        const l = Math.max(a.left, b.left), t = Math.max(a.top, b.top);
        const r = Math.min(a.right, b.right), bt = Math.min(a.bottom, b.bottom);
        return { left: l, top: t, right: Math.max(l, r), bottom: Math.max(t, bt), width: Math.max(0, r - l), height: Math.max(0, bt - t) };
    }

    async function init() {
        await Promise.all(FACES.map(([fam, style, weight]) => document.fonts.load(`${style} ${weight} 20px ${fam}`, "a")));
        await document.fonts.ready;
        const loaded = [];
        document.fonts.forEach((f) => { if (f.status === "loaded") loaded.push(`${f.family}/${f.style}/${f.weight}`); });
        return { loaded: loaded.length, dpr: window.devicePixelRatio, ua: navigator.userAgent, calibration: calibrate() };
    }

    // Synthetic styles: a face asked for in a weight / style its family does not have. Widths
    // of the same string per style tell whether Chromium's synthetic bold / oblique changes
    // advances. Also a plain run to check advances against hmtx × size.
    function calibrate() {
        const cases = [
            ["KaTeX_AMS", "normal", 400, "ABCDE"], ["KaTeX_AMS", "normal", 700, "ABCDE"], ["KaTeX_AMS", "italic", 400, "ABCDE"],
            ["KaTeX_Typewriter", "normal", 400, "ABCDE"], ["KaTeX_Typewriter", "normal", 700, "ABCDE"],
            ["KaTeX_Size1", "normal", 400, "()[]"], ["KaTeX_Size1", "normal", 700, "()[]"], ["KaTeX_Size1", "italic", 400, "()[]"],
            ["KaTeX_Main", "normal", 400, "aaaaaaaaaa"], ["KaTeX_Math", "normal", 400, "xxxxx"], ["KaTeX_Math", "italic", 400, "xxxxx"],
            ["KaTeX_Script", "normal", 700, "ABC"], ["KaTeX_Script", "normal", 400, "ABC"],
        ];
        const div = document.createElement("div");
        div.style.cssText = "position:absolute;left:0;top:0;font-size:19.36px;white-space:nowrap";
        document.body.appendChild(div);
        const out = cases.map(([f, st, w, s]) => {
            const span = document.createElement("span");
            span.style.cssText = `font-family:${f};font-style:${st};font-weight:${w}`;
            span.textContent = s;
            div.appendChild(span);
            div.appendChild(document.createElement("br"));
            return { f, st, w, s, span };
        }).map(({ f, st, w, s, span }) => ({ f, st, w, s, width: r4(span.getBoundingClientRect().width) }));
        div.remove();
        return out;
    }

    function textNodesOf(root) {
        const out = [];
        const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
        for (let n = w.nextNode(); n; n = w.nextNode()) {
            const p = n.parentElement;
            if (p.classList.contains("vlist-s")) continue; // the U+200B baseline helper
            if (p.closest(".katex-mathml")) continue;
            if (n.data.length === 0) continue;
            out.push(n);
        }
        return out;
    }

    // Per code point advance boxes of every text node: [{node, i, len, rect}]
    function glyphBoxes(nodes) {
        const range = document.createRange();
        const out = [];
        for (const node of nodes) {
            const s = node.data;
            for (let i = 0; i < s.length;) {
                const cp = s.codePointAt(i);
                const len = cp > 0xffff ? 2 : 1;
                range.setStart(node, i);
                range.setEnd(node, i + len);
                out.push({ node, i, len, rect: range.getBoundingClientRect() });
                i += len;
            }
        }
        return out;
    }

    // Same as tools/gen_markup.mjs: drop the screen-reader MathML half.
    function stripMathml(html) {
        const open = '<span class="katex-mathml">';
        const a = html.indexOf(open);
        if (a < 0) return html;
        const b = html.indexOf("</span>", a);
        return html.slice(0, a) + html.slice(b + "</span>".length);
    }

    function render(entry, slot) {
        const wrap = document.createElement("div");
        wrap.style.cssText = `position:absolute;left:0;top:${slot * SLOT}px`;
        let html, stripped = "", status = "ok", throwMsg = null;
        try {
            html = katex.renderToString(entry.tex, { ...OPTIONS, displayMode: entry.display });
            if (html.startsWith('<span class="katex-error"')) status = "error";
            stripped = status === "ok" ? stripMathml(html) : html;
        } catch (e) {
            status = "throw";
            throwMsg = `${e && e.name}: ${e && e.message}`;
            html = "";
        }
        if (entry.display) {
            wrap.innerHTML = `<div class="host" style="font-size:16px;width:1000px"><div class="rich-math-display">${html}</div></div>`;
        } else {
            wrap.innerHTML = `<div class="host" style="font-size:16px;width:20000px;line-height:normal"><span class="rich-math-inline">${html}</span></div>`;
        }
        document.body.appendChild(wrap);
        return { wrap, status, throwMsg, htmlHash: fnv(stripped) };
    }

    function measure(entry, st) {
        // hh: FNV-1a of the markup measured (as in markup.jsonl.gz, MathML removed), so a test
        // can tell the two vector files come from the same KaTeX build.
        const res = { id: entry.id, tex: entry.tex, display: entry.display, status: st.status, hh: st.htmlHash };
        const host = st.wrap.firstChild;
        const outer = host.firstChild; // .rich-math-inline / .rich-math-display
        if (st.status === "throw") {
            res.error = st.throwMsg;
            return res;
        }
        if (st.status === "error") {
            const span = outer.querySelector(".katex-error");
            const hr = host.getBoundingClientRect();
            const sr = span.getBoundingClientRect();
            res.error = span.getAttribute("title");
            res.box = { x: r4(sr.left - hr.left), y: r4(sr.top - hr.top), w: r4(sr.width), h: r4(sr.height) };
            const cs = getComputedStyle(span);
            res.errorFont = { f: firstFamily(cs.fontFamily), s: parseFloat(cs.fontSize), c: hexColor(cs.color) };
            return res;
        }
        const kh = outer.querySelector(".katex-html");

        // 1. glyph boxes without any probe (absolute coordinates)
        const nodes = textNodesOf(kh);
        const g0 = glyphBoxes(nodes);
        const kh0 = kh.getBoundingClientRect();

        // 2. origin: x = the host's left content edge (inline formulas start there; display
        // formulas are left-aligned by the app's CSS), y = the baseline of the first
        // .katex-base, read from a 0×0 probe inserted as that base's first child. (A probe
        // directly in .katex-html would open a line of its own when the formula starts with a
        // \\ — .katex-newline is display:block — and push everything down.)
        const op = document.createElement("span");
        op.className = "harness-probe";
        op.style.cssText = PROBE_CSS;
        const firstBase = kh.querySelector(":scope > .katex-base, :scope > .katex-tag");
        (firstBase || kh).insertBefore(op, (firstBase || kh).firstChild);
        const pr = op.getBoundingClientRect();
        const hostRect = host.getBoundingClientRect();
        const o = { x: hostRect.left, y: pr.bottom };

        // 3. a baseline probe before every text node
        const probes = nodes.map((n) => {
            const p = document.createElement("span");
            p.className = "harness-probe";
            p.style.cssText = PROBE_CSS;
            n.parentNode.insertBefore(p, n);
            return p;
        });
        const baselines = new Map();
        nodes.forEach((n, k) => baselines.set(n, probes[k].getBoundingClientRect().bottom));
        const g1 = glyphBoxes(nodes);
        const kh1 = kh.getBoundingClientRect();
        // An empty .katex-html (e.g. "\\displaystyle" alone) has no line box before the probe
        // gives it one, so its rect legitimately moves; compare it only when it has content.
        let shift = kh.querySelector(".katex-base") ? Math.max(Math.abs(kh1.left - kh0.left), Math.abs(kh1.top - kh0.top),
            Math.abs(kh1.width - kh0.width), Math.abs(kh1.height - kh0.height)) : 0;
        for (let k = 0; k < g0.length; k++) {
            shift = Math.max(shift, Math.abs(g0[k].rect.left - g1[k].rect.left), Math.abs(g0[k].rect.top - g1[k].rect.top));
        }
        res.probeShift = r4(shift);

        // geometry
        res.box = rel(kh1, o);
        res.bases = [...kh.querySelectorAll(":scope > .katex-base")].map((b) => rel(b.getBoundingClientRect(), o));
        const tag = kh.querySelector(":scope > .katex-tag");
        res.tag = tag ? rel(tag.getBoundingClientRect(), o) : null;
        res.newlines = kh.querySelectorAll(":scope > .katex-newline").length;
        const hr = host.getBoundingClientRect();
        const orr = outer.getBoundingClientRect();
        res.host = { w: r4(hr.width), h: r4(hr.height), originX: r4(o.x - hr.left), originY: r4(o.y - hr.top), outer: rel(orr, o) };

        const styleCache = new Map();
        const styleOf = (el) => {
            let s = styleCache.get(el);
            if (!s) {
                const cs = getComputedStyle(el);
                s = {
                    f: firstFamily(cs.fontFamily), s: parseFloat(cs.fontSize),
                    i: cs.fontStyle === "italic" || cs.fontStyle.startsWith("oblique"),
                    b: Number(cs.fontWeight), c: hexColor(cs.color), cls: el.className,
                };
                styleCache.set(el, s);
            }
            return s;
        };
        const nodeIndex = new Map(nodes.map((n, k) => [n, k]));
        res.glyphs = g1.map((g) => {
            const st = styleOf(g.node.parentElement);
            return {
                t: g.node.data.substr(g.i, g.len), n: nodeIndex.get(g.node), x: r4(g.rect.left - o.x), w: r4(g.rect.width),
                y: r4(baselines.get(g.node) - o.y), top: r4(g.rect.top - o.y), bot: r4(g.rect.bottom - o.y),
                f: st.f, s: st.s, i: st.i, b: st.b, c: st.c, cls: st.cls,
            };
        });

        // borders and backgrounds
        res.rects = [];
        for (const el of kh.querySelectorAll("*")) {
            if (el.closest("svg") || el.classList.contains("harness-probe")) continue;
            const cs = getComputedStyle(el);
            const sides = {};
            let any = false;
            for (const side of ["Top", "Right", "Bottom", "Left"]) {
                const w = parseFloat(cs[`border${side}Width`]);
                const style = cs[`border${side}Style`];
                if (w > 0 && style !== "none" && style !== "hidden") {
                    sides[side[0].toLowerCase()] = { w: r4(w), s: style, c: hexColor(cs[`border${side}Color`]) };
                    any = true;
                }
            }
            const bg = cs.backgroundColor;
            const hasBg = bg && bg !== "transparent" && bg !== "rgba(0, 0, 0, 0)";
            if (!any && !hasBg) continue;
            const rec = { ...rel(el.getBoundingClientRect(), o), cls: el.className };
            if (any) rec.border = sides;
            if (hasBg) rec.bg = hexColor(bg);
            res.rects.push(rec);
        }

        // svgs
        res.svgs = [];
        for (const svg of kh.querySelectorAll("svg")) {
            const r = svg.getBoundingClientRect();
            let clip = null;
            for (let a = svg.parentElement; a && a !== kh; a = a.parentElement) {
                const cs = getComputedStyle(a);
                if (cs.overflowX !== "visible" || cs.overflowY !== "visible") {
                    const ar = a.getBoundingClientRect();
                    clip = clip ? intersect(clip, ar) : ar;
                }
            }
            const rec = {
                ...rel(r, o),
                clip: clip ? rel(clip, o) : null,
                viewBox: svg.getAttribute("viewBox"),
                par: svg.getAttribute("preserveAspectRatio"),
                wa: svg.getAttribute("width"), ha: svg.getAttribute("height"),
                c: hexColor(getComputedStyle(svg).color),
                pcls: svg.parentElement.className,
                paths: [...svg.querySelectorAll("path")].map((p) => {
                    const d = p.getAttribute("d") || "";
                    return { h: fnv(d), n: d.length };
                }),
            };
            const lines = [...svg.querySelectorAll("line")].map((l) => ({
                x1: l.getAttribute("x1"), y1: l.getAttribute("y1"), x2: l.getAttribute("x2"), y2: l.getAttribute("y2"),
                sw: l.getAttribute("stroke-width"),
            }));
            if (lines.length) rec.lines = lines;
            res.svgs.push(rec);
        }
        return res;
    }

    function run(entries, firstSlot) {
        const states = entries.map((e, k) => render(e, firstSlot + k));
        void document.body.offsetHeight; // one layout for the batch
        const out = entries.map((e, k) => {
            try {
                return measure(e, states[k]);
            } catch (err) {
                return { id: e.id, tex: e.tex, display: e.display, status: "harness-error", error: String(err && err.stack || err) };
            }
        });
        for (const s of states) s.wrap.remove();
        return out;
    }

    window.harness = { init, run };
})();

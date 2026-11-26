// Runs the renderer's real code (bundled by ../gen.mjs, React stubbed so components return their
// element trees) and turns what it renders into canonical JSON, one record per input. The Rust
// test (tests/differential.rs) renders its Document to the same JSON and compares byte for byte.
//
// Record: [bubble, preview, embeds, lone, loneLabel, hasFormatting, plain, composer, openFence, urlFns?]
//   bubble / preview: items, adjacent text items of one style merged, empty text dropped:
//     [0, style, text]                         text run
//     [1, style, emoji, twemojiKey|null, px]   Twemoji (key null = no SVG, drawn as text)
//     [2, style, href, invite, inviteParse|null, mediaLabel|null, isGif]   autolink
//     [3, style, text]                         inline <code> (also a code block / math over the budget)
//     [4, style, lang|null, label, text]       CodeBlock (label = its header: lang if known, else "code")
//     [5, style, tex, display]                 MathTex
//     style bits: 1 <strong>, 2 <em>, 4 <u>, 8 <s>
//   embeds: extractLinks(content, 3) as [url, isInviteLink]
//   composer: [hasTokenizable, items] from RichInput's tokenize over one text node:
//     [0, text] | [1, native, twemojiKey|null]
//   urlFns (URL suite only): [parseInviteLink(input), isInviteLink(input), mediaLabelFor(input), isGifUrl(input)]
import { Fragment } from "react";
import MessageText from "@app/features/chat/MessageText";
import CodeBlock from "@app/features/chat/CodeBlock";
import MathTex from "@app/features/chat/MathTex";
import Twemoji from "@app/components/emoji/Twemoji";
import { onLinkClick } from "@app/lib/openExternal";
import {
  parseRichTextUncached,
  hasFormatting,
  loneLink,
  mediaLabelFor,
  extractLinks,
  richTextToPlain,
} from "@app/features/chat/richText";
import { parseInviteLink, isInviteLink } from "@app/features/servers/inviteLink";
import { __tokenize, __hasTokenizable, __hasOpenCodeFence } from "@app/components/editor/RichInput";
import { __isGifUrl } from "@app/features/chat/LinkEmbeds";

const invite = (u) => {
  const p = parseInviteLink(u);
  return p === null ? null : "host" in p ? [p.code, p.host, p.port] : [p.code];
};

function pushText(out, style, text) {
  if (text.length === 0) return;
  const last = out[out.length - 1];
  if (last && last[0] === 0 && last[1] === style) last[2] += text;
  else out.push([0, style, text]);
}

function items(tree) {
  const out = [];
  const walk = (n, style) => {
    if (n === null || n === undefined || n === false || n === true) return;
    if (typeof n === "string") return pushText(out, style, n);
    if (Array.isArray(n)) { for (const c of n) walk(c, style); return; }
    const { type: t, props: p } = n;
    if (t === Fragment) return walk(p.children, style);
    if (t === Twemoji) {
      const r = Twemoji(p);
      const html = r.props.dangerouslySetInnerHTML?.__html;
      out.push([1, style, p.emoji, html ? html.slice(3, -4) : null, p.size]);
      return;
    }
    if (t === "a") {
      if (p.children !== p.href) throw new Error("link text differs from href");
      out.push([2, style, p.href, p.onClick === onLinkClick ? 0 : 1, invite(p.href), mediaLabelFor(p.href), __isGifUrl(p.href) ? 1 : 0]);
      return;
    }
    if (t === "strong") return walk(p.children, style | 1);
    if (t === "em") return walk(p.children, style | 2);
    if (t === "u") return walk(p.children, style | 4);
    if (t === "s") return walk(p.children, style | 8);
    if (t === "code") {
      if (typeof p.children !== "string") throw new Error("code child not a string");
      out.push([3, style, p.children]);
      return;
    }
    if (t === CodeBlock) {
      const r = CodeBlock(p);
      const label = r.props.children[0].props.children[0].props.children;
      out.push([4, style, p.lang, label, p.text]);
      return;
    }
    if (t === MathTex) { out.push([5, style, p.tex, p.display ? 1 : 0]); return; }
    if (typeof t === "function" && t.name === "EmojiInfoPopover") return;
    throw new Error("unexpected element " + String(t?.name ?? t));
  };
  walk(tree, 0);
  return out;
}

function composer(input) {
  const root = document.createElement("div");
  root.appendChild(document.createTextNode(input));
  const has = __hasTokenizable(root) ? 1 : 0;
  __tokenize(root);
  const out = [];
  for (const n of root.childNodes) {
    if (n.nodeType === Node.TEXT_NODE) {
      if (n.textContent.length > 0) out.push([0, n.textContent]);
    } else {
      const i = n.querySelector("i");
      out.push([1, n.dataset.emoji, i ? i.textContent : null]);
    }
  }
  return [has, out];
}

function record(input, urlFns) {
  const lone = loneLink(input);
  const rec = [
    items(MessageText({ content: input })),
    items(MessageText({ content: input, preview: true, emojiSize: 13 })),
    extractLinks(input, 3).map((u) => [u, isInviteLink(u) ? 1 : 0]),
    lone,
    lone ? mediaLabelFor(lone) : null,
    hasFormatting(parseRichTextUncached(input)) ? 1 : 0,
    richTextToPlain(input),
    composer(input),
    __hasOpenCodeFence(input) ? 1 : 0,
  ];
  if (urlFns) rec.push([invite(input), isInviteLink(input) ? 1 : 0, mediaLabelFor(input), __isGifUrl(input) ? 1 : 0]);
  return JSON.stringify(rec);
}

// suites: [{ name, inputs, urlFns }] → { name: [record, …] }
globalThis.__runSuites = (suites) => {
  const out = {};
  for (const s of suites) out[s.name] = s.inputs.map((x) => record(x, s.urlFns));
  return out;
};

// Blink's URL host verdict per code point, three positions (middle of a label, label start, end
// of the TLD): "0" parse failure, "1" hostname has a dot or is localhost, "2" otherwise.
globalThis.__hostSweep = () => {
  const res = (s) => { try { const u = new URL(s); return u.hostname.includes(".") || u.hostname === "localhost" ? "1" : "2"; } catch { return "0"; } };
  const out = [];
  let prev = null, start = 0;
  for (let cp = 0x80; cp <= 0x10ffff; cp++) {
    if (cp >= 0xd800 && cp <= 0xdfff) continue;
    const ch = String.fromCodePoint(cp);
    const r = res("http://a" + ch + "b.com") + res("http://" + ch + "a.com") + res("http://a.b" + ch);
    if (r !== prev) { if (prev !== null) out.push([start, cp - 1, prev]); prev = r; start = cp; }
  }
  out.push([start, 0x10ffff, prev]);
  return out;
};

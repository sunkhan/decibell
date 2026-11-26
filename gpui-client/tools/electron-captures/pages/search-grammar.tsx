// Not a screen: the renderer's search grammar as data, for `decibell-search`'s tests
// (crates/decibell-search/src/renderer_grammar.json, checked by `grammar::tests`). Runs the real
// `searchTokens.ts`, `searchQuery.ts` and `highlight.tsx` on a table of inputs, and renders the
// real `DatePicker` for a few months (run.sh pins en-US: Sunday first).
//
// Regenerate after changing the cases or the renderer's grammar:
//   tools/electron-captures/run.sh --pages search-grammar --themes nocturne --out /tmp/sg
//   cp /tmp/sg/search-grammar-nocturne.json crates/decibell-search/src/renderer_grammar.json
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import DatePicker from "@app/features/search/DatePicker";
import { makeFragmentHighlighter, makeHighlighter } from "@app/features/search/highlight";
import {
  fold,
  isSearchable,
  parseDay,
  searchFragments,
  searchWords,
  toSearchArgs,
  withFilter,
  type SearchFilter,
} from "@app/features/search/searchQuery";
import { chipTokens, hasKindFor, resolveToken, trailingToken, type TokenContext } from "@app/features/search/searchTokens";
import type { ChannelInfo, ServerMember } from "@app/types";

const member = (uid: number, username: string, nickname = ""): ServerMember =>
  ({ uid, username, nickname, isOwner: false, isOnline: true, roleIds: [], joinedAt: "0", timedOutUntil: 0 }) as unknown as ServerMember;
const channel = (id: string, encrypted = false): ChannelInfo =>
  ({ id, name: id, type: "text", encrypted, myPermissions: 8192 | 16384 }) as unknown as ChannelInfo;

export const CTX: TokenContext = {
  members: [member(1, "alice"), member(2, "bob", "Bobby"), member(3, "Dana"), member(4, "İlkay"), member(5, "ışık", "Işık")],
  channels: [channel("general"), channel("off-topic"), channel("vault", true), channel("Oyun-Gecesi")],
};

const TEXTS = [
  "", " ", "from:", "from:da", "raid FROM:Da", "raid from:da ", "xfrom:da", "in:#gen", "in:", "has:", "has:IMAGE",
  "before:2026-10-01", "during:today", "after:yesterday", "from:in:x", "inside:", "ölçü has:im", "a\tb in:x",
  "wayland", "Wayland -x11", "\"exact phrase\" gg", "-only", "e-mail", "ab", "abc", "IŞIK", "ışık ılık", "Ünlü café",
  "naïve résumé", "Ⅻ ½", "日本語 テキスト", "emoji 🎉 test", "x \"unterminated", "  spaced   out  ", "C++ c#",
  "Đorđe Łódź", "ﬁne ﬂow", "ǅungla", "Straße", "Ångström", "ﬀ", "ά έ", "x-y-z", "-\"no\" yes", "- dash",
];

const QUERIES = [
  "raid from:dana has:image in:general gg",
  "from:@ALICE in:#Off-Topic has:pictures before:2026-10-03",
  "from:bobby from:nobody has:nothing in:vault",
  "during:2026-02-30 after:2026-10-3 before:today",
  "from:işık from:İLKAY in:oyun-gecesi",
  "  lots   of   space  ",
  "has:doc has:docs has:url has:sound",
];

const RESOLVE: [string, string][] = [
  ["from", "alice"], ["from", "@Dana"], ["from", "BOBBY"], ["from", "bo"], ["from", " "], ["from", "ışık"], ["from", "IŞIK"], ["from", "ilkay"],
  ["in", "#general"], ["in", "OFF-TOPIC"], ["in", "gen"], ["has", "Links"], ["has", "attachment"], ["has", "x"],
  ["before", "2026-1-5"], ["during", "today"], ["after", "2026-13-01"], ["after", "26-10-03"], ["before", "yesterday"],
];

const HIGHLIGHT: [string, string, boolean][] = [
  ["wayland", "Wayland? waylander wayland's #wayland", false],
  ["gg", "gg ggs GG! egg", false],
  ["cafe", "Café cafés CAFÉ café", false],
  ["isik", "IŞIK ışık isik ISIK", false],
  ["e-mail", "e-mail email mail e mails", false],
  ["\"exact phrase\"", "an exact phrase, exactly phrased", false],
  ["ayl", "Wayland waylander", true],
  ["aa", "aaaa", true],
  ["ıs", "IŞIK ışık", true],
  ["e", "café!", true],
  ["ab-c", "xab-cy ab c", true],
  ["日本", "日本語のテキスト", true],
];

const DAYS = [
  ["today", "2026-11-26"], ["Yesterday", "2026-11-26"], ["yesterday", "2026-03-01"], ["2026-10-03", "2026-11-26"], ["2026-10-3", "2026-11-26"],
  ["2026-02-30", "2026-11-26"], ["2024-02-29", "2026-11-26"], ["26-10-03", "2026-11-26"], [" 2026-1-1 ", "2026-11-26"], ["2026/10/03", "2026-11-26"],
];

const at = (day: string) => {
  const [y, m, d] = day.split("-").map(Number);
  return new Date(y, m - 1, d, 12, 0, 0);
};

function marked(parts: (string | { props: { children: unknown } })[]): string[] {
  return parts.filter((p) => typeof p !== "string").map((p) => String((p as { props: { children: unknown } }).props.children));
}

function calendars() {
  const host = document.createElement("div");
  host.style.cssText = "position:absolute;left:-10000px;top:0;width:360px";
  document.body.appendChild(host);
  const root = createRoot(host);
  const out: { active: string; days: string[]; heads: string[]; title: string }[] = [];
  for (const active of ["2026-11-15", "2026-02-10", "2026-03-31", "2025-12-01", "2026-08-01", "2024-02-29"]) {
    flushSync(() => root.render(<DatePicker active={active} onActive={() => {}} onPick={() => {}} />));
    out.push({
      active,
      days: [...host.querySelectorAll<HTMLElement>("[aria-selected]")].map((b) => b.getAttribute("aria-label") ?? ""),
      heads: [...host.querySelectorAll<HTMLElement>("[role=grid] > span")].map((s) => s.textContent ?? ""),
      title: host.querySelector("[role=grid]")?.getAttribute("aria-label") ?? "",
    });
  }
  root.unmount();
  host.remove();
  return out;
}

export default function SearchGrammar() {
  return <div className="p-4 text-[13px]">search grammar (data only)</div>;
}

export async function setup() {
  const filters: SearchFilter[][] = [];
  let acc: SearchFilter[] = [];
  for (const f of [
    { kind: "from", uid: 3, username: "carol" },
    { kind: "from", uid: 3, username: "carol2" },
    { kind: "before", date: "2026-10-01" },
    { kind: "before", date: "2026-09-01" },
    { kind: "has", has: "link" },
    { kind: "has", has: "image" },
    { kind: "inside" },
    { kind: "inside" },
    { kind: "in", channelId: "general" },
  ] as SearchFilter[]) {
    acc = withFilter(acc, f);
    filters.push(acc);
  }
  return {
    texts: TEXTS.map((text) => {
      const token = trailingToken(text);
      return {
        text,
        token: token ? { op: token.op, value: token.value, before: text.slice(0, token.start) } : null,
        fold: fold(text),
        words: searchWords(text),
        fragments: searchFragments(text),
        searchable: isSearchable({ text, filters: [] }),
        searchableInside: isSearchable({ text, filters: [{ kind: "inside" }] }),
        has: hasKindFor(text),
      };
    }),
    queries: QUERIES.map((text) => {
      const q = chipTokens({ text, filters: [] }, CTX);
      const args = toSearchArgs("1", q, "newest", 7);
      return { text, result: q, args: { query: args.query, channelIds: args.channelIds, authorUids: args.authorUids, has: args.has, matchWithinWords: args.matchWithinWords } };
    }),
    resolve: RESOLVE.map(([op, value]) => ({ op, value, filter: resolveToken(op as never, value, CTX) })),
    days: DAYS.map(([input, now]) => ({ input, now, day: parseDay(input, at(now)) })),
    withFilter: filters,
    highlight: HIGHLIGHT.map(([query, text, inside]) => {
      const h = inside ? makeFragmentHighlighter(searchFragments(query)) : makeHighlighter(searchWords(query));
      return { query, text, inside, marks: h ? marked(h(text, "k") as never) : null };
    }),
    calendars: calendars(),
  };
}

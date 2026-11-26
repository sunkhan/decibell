// Not a screen: the renderer's message times as data, for `decibell_model::regional`'s tests.
// Renders the real `MessageBubble` (header time, its tooltip, a grouped row's hover time) and
// `DayDivider` for a table of (message time, "now") pairs on the local wall clock, with `Date`'s
// no-argument constructor answering that "now", in every time format setting. run.sh pins ICU's
// locale (LC_ALL=en_US.UTF-8), so this is what `Intl` gives the renderer on an en-US machine.
//
// Regenerate the Rust fixture after changing the cases or the renderer's stamp logic:
//   tools/electron-captures/run.sh --pages regional-stamps --themes nocturne --out /tmp/rs
//   cp /tmp/rs/regional-stamps-nocturne.json crates/decibell-model/src/regional/renderer_stamps.json
import { useState } from "react";
import { flushSync } from "react-dom";
import { createRoot } from "react-dom/client";
import MessageBubble, { DayDivider } from "@app/features/chat/MessageBubble";
import { useUiStore, type TimeFormat } from "@app/stores/uiStore";
import type { Message } from "@app/types";

/// [year, month (1-12), day, hour, minute, second] on the local wall clock.
type Moment = [number, number, number, number, number, number];

// Around midnight, yesterday across a month, a year and a leap day, last week, last year, a
// message from "tomorrow" (a skewed clock), single-digit fields. No time falls in a DST change.
const PAIRS: [Moment, Moment][] = [
  [[2026, 11, 7, 0, 5, 0], [2026, 11, 7, 23, 59, 0]],
  [[2026, 11, 7, 12, 0, 0], [2026, 11, 7, 12, 30, 0]],
  [[2026, 11, 7, 9, 5, 0], [2026, 11, 7, 9, 6, 0]],
  [[2026, 11, 6, 23, 59, 0], [2026, 11, 7, 0, 1, 0]],
  [[2026, 11, 6, 0, 0, 0], [2026, 11, 7, 23, 59, 0]],
  [[2026, 11, 5, 23, 59, 0], [2026, 11, 7, 0, 0, 0]],
  [[2026, 10, 31, 9, 5, 0], [2026, 11, 7, 12, 0, 0]],
  [[2026, 10, 31, 18, 0, 0], [2026, 11, 1, 8, 0, 0]],
  [[2025, 12, 31, 23, 30, 0], [2026, 1, 1, 0, 10, 0]],
  [[2025, 11, 7, 14, 32, 5], [2026, 11, 7, 10, 0, 0]],
  [[2024, 2, 29, 13, 0, 0], [2024, 3, 1, 8, 0, 0]],
  [[2026, 11, 8, 10, 0, 0], [2026, 11, 7, 10, 0, 0]],
  [[2026, 1, 2, 3, 4, 0], [2026, 11, 7, 10, 0, 0]],
  [[2026, 12, 25, 21, 45, 0], [2026, 12, 26, 1, 0, 0]],
];
const FORMATS: TimeFormat[] = ["auto", "12h", "24h"];

type Case = {
  message: Moment;
  now: Moment;
  format: TimeFormat;
  stamp: string;
  title: string;
  compact: string;
  divider: string;
};

const RealDate = Date;
let fakeNow = RealDate.now();
/// `new Date()` is "now"; everything else is the real Date.
class FixedNowDate extends RealDate {
  constructor(...args: unknown[]) {
    if (args.length === 0) super(fakeNow);
    else super(...(args as [number, number]));
  }
}

const at = ([y, m, d, h, min, s]: Moment) => new RealDate(y, m - 1, d, h, min, s);

function message(epochSeconds: number): Message {
  return { id: 1, channelId: "general", sender: "alice", senderUid: 1, content: "hello", timestamp: String(epochSeconds), attachments: [] };
}

function measure(): Case[] {
  const host = document.createElement("div");
  host.style.cssText = "position:absolute;left:-10000px;top:0;width:800px";
  document.body.appendChild(host);
  const root = createRoot(host);
  const out: Case[] = [];
  (globalThis as { Date: DateConstructor }).Date = FixedNowDate as unknown as DateConstructor;
  try {
    for (const [msg, now] of PAIRS) {
      for (const format of FORMATS) {
        useUiStore.setState({ timeFormat: format });
        fakeNow = at(now).getTime();
        const epoch = Math.floor(at(msg).getTime() / 1000);
        flushSync(() => root.render(<MessageBubble key={`h${out.length}`} message={message(epoch)} grouped={false} serverId={null} />));
        const header = host.querySelector<HTMLElement>("span.tabular-nums[title]");
        const stamp = header?.textContent ?? "";
        const title = header?.title ?? "";
        flushSync(() => root.render(<MessageBubble key={`g${out.length}`} message={message(epoch)} grouped={true} serverId={null} />));
        const compact = host.querySelector<HTMLElement>("span.tabular-nums[title]")?.textContent ?? "";
        flushSync(() => root.render(<DayDivider key={`d${out.length}`} timestamp={String(epoch)} />));
        const divider = host.querySelector<HTMLElement>("[role=separator]")?.getAttribute("aria-label") ?? "";
        out.push({ message: msg, now, format, stamp, title, compact, divider });
      }
    }
  } finally {
    (globalThis as { Date: DateConstructor }).Date = RealDate;
    root.unmount();
    host.remove();
  }
  return out;
}

let shown: (cases: Case[]) => void = () => {};

export default function RegionalStamps() {
  const [cases, setCases] = useState<Case[]>([]);
  shown = setCases;
  const moment = (m: Moment) => `${m[0]}-${String(m[1]).padStart(2, "0")}-${String(m[2]).padStart(2, "0")} ${String(m[3]).padStart(2, "0")}:${String(m[4]).padStart(2, "0")}`;
  return (
    <div className="h-full overflow-hidden p-3 font-meta text-[11px] text-text-secondary">
      <table className="w-full">
        <tbody>
          {cases.map((c, i) => (
            <tr key={i}>
              <td className="pr-3 tabular-nums">{moment(c.message)}</td>
              <td className="pr-3 tabular-nums">{moment(c.now)}</td>
              <td className="pr-3">{c.format}</td>
              <td className="pr-3 text-text-primary">{c.stamp}</td>
              <td className="pr-3">{c.compact}</td>
              <td className="pr-3">{c.title}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export async function setup() {
  const cases = measure();
  flushSync(() => shown(cases));
  const resolved = new Intl.DateTimeFormat().resolvedOptions();
  return {
    generated: "tools/electron-captures/pages/regional-stamps.tsx (Electron, run.sh: LC_ALL=en_US.UTF-8): the real MessageBubble / DayDivider",
    locale: resolved.locale,
    timeZone: resolved.timeZone,
    navigatorLanguage: navigator.language,
    cases,
  };
}

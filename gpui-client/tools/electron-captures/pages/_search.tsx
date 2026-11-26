// Shared by the search pages (header-*, search-box-*, date-picker, search-results*): the renderer's
// real MainLayout (ChannelHeader, SearchBox, SearchResultsPanel) fed the GPUI search fixture
// (crates/decibell-search/src/fixture.rs, exported by
// `cargo run -p decibell-search --features fixtures --bin decibell-search-fixture -- pages/fixtures`):
// the chat fixture's events through the renderer's hooks (_shell.tsx), then what the GPUI fixture
// does as the user: the box's draft, a search and its answer (mapped as useSearchEvents does),
// the box's focus. `Date`'s no-argument constructor answers the fixture's pinned "today" (noon),
// as the GPUI screens pin `PinnedToday`.
import { useSearchStore } from "@app/features/search/searchStore";
import { mapStoredMessage } from "@app/features/chat/useChatEvents";
import type { SearchQuery } from "@app/features/search/searchQuery";
import type { MessageSearchReceivedPayload } from "@app/types";
import { ShellPage, replay, type ShellFixture } from "./_shell";
import { focusIn, frames } from "../runtime/harness";

export type SearchFixture = ShellFixture & {
  search: {
    today: string;
    draft: SearchQuery;
    focus: boolean;
    query: SearchQuery | null;
    answer: MessageSearchReceivedPayload | null;
  };
};

const RealDate = Date;
let pinned: number | null = null;
/// `new Date()` and `Date.now()` are the pinned day at noon; everything else is the real Date.
class PinnedDate extends RealDate {
  constructor(...args: unknown[]) {
    if (args.length === 0) super(pinned ?? RealDate.now());
    else super(...(args as [number, number]));
  }
  static now(): number {
    return pinned ?? RealDate.now();
  }
}

/// Call at the page's top level, before anything renders.
export function pinToday(day: string) {
  const [y, m, d] = day.split("-").map(Number);
  pinned = new RealDate(y, m - 1, d, 12, 0, 0).getTime();
  (globalThis as { Date: DateConstructor }).Date = PinnedDate as unknown as DateConstructor;
}

export const SearchPage = ShellPage;

function rect(el: Element | null) {
  if (!el) return null;
  const r = el.getBoundingClientRect();
  return { x: r.left, y: r.top, width: r.width, height: r.height };
}

export async function replaySearch(fixture: SearchFixture) {
  const shown = await replay(fixture);
  const s = fixture.search;
  const serverId = fixture.nav.server!;
  const store = useSearchStore.getState();
  store.setDraft(serverId, s.draft);
  if (s.query && s.answer) {
    store.run(serverId, s.query);
    const p = s.answer;
    useSearchStore.getState().receive({
      serverId: p.serverId,
      seq: useSearchStore.getState().seq,
      success: p.success,
      message: p.message,
      messages: (p.messages ?? []).map((m) => mapStoredMessage(p.serverId, m)),
      hasMore: p.hasMore,
      nextCursor: Number(p.nextCursor),
      nextOffset: Number(p.nextOffset),
      total: Number(p.total),
      totalCapped: p.totalCapped,
      textSkippedChannelIds: p.textSkippedChannelIds ?? [],
      partial: p.partial,
      retryAfterMs: Number(p.retryAfterMs),
      firstPage: p.firstPage,
    });
  }
  await frames(2);
  const input = document.querySelector<HTMLInputElement>('input[aria-label="Search messages"]');
  if (s.focus && input) focusIn(input);
  await frames(4);
  const panel = document.querySelector('aside[aria-label="Search results"]');
  const list = panel?.querySelector(".overflow-y-auto") ?? null;
  return {
    ...shown,
    box: rect(input?.closest("div[title]") ?? null),
    menu: rect(document.querySelector('[role="listbox"]')),
    panel: rect(panel),
    title: panel?.querySelector("span")?.textContent ?? null,
    rows: list
      ? [...list.children].map((el) => ({ text: (el.textContent ?? "").slice(0, 40), ...rect(el), offsetTop: (el as HTMLElement).offsetTop }))
      : [],
  };
}

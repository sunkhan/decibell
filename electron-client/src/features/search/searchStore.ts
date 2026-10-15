import { create } from "zustand";
import { invoke } from "../../lib/ipc";
import type { Message } from "../../types";
import {
  EMPTY_QUERY,
  isSearchable,
  matchesInside,
  searchWords,
  toSearchArgs,
  type SearchQuery,
  type SearchSort,
} from "./searchQuery";

/// Message search state (spec 2026-10-14). One search at a time: the
/// results panel belongs to the active server. The header box's own text
/// and chips live in `drafts` (per server), so switching channels keeps
/// them; `query` is what the results are for. Nothing is remembered past
/// the session: no recent searches, by owner decision (2026-10-14).

export type SearchStatus = "idle" | "loading" | "done" | "error";

/// No answer this long: the server predates search (it never replies).
const NO_ANSWER_MS = 8000;
/// Retries after a "too fast" / "busy" answer, before giving up.
const MAX_RETRIES = 3;

interface SearchState {
  /// Server the results are for; null = nothing searched yet.
  serverId: string | null;
  /// The results panel is showing. Closing keeps the results, so running
  /// the same search again just shows them.
  open: boolean;
  query: SearchQuery;
  sort: SearchSort;
  /// The answer being waited for / shown (answers to others are dropped).
  seq: number;
  status: SearchStatus;
  results: Message[];
  /// First page's count; -1 = not counted.
  total: number;
  totalCapped: boolean;
  hasMore: boolean;
  loadingMore: boolean;
  nextCursor: number;
  nextOffset: number;
  /// Encrypted channels whose text wasn't searched.
  textSkipped: string[];
  partial: boolean;
  error: string | null;
  /// The result last jumped to (ringed in the list).
  jumpedId: number;
  /// Header box contents per server.
  drafts: Record<string, SearchQuery>;
  /// Bumped to focus the header box (Ctrl+F).
  focusNonce: number;

  setDraft: (serverId: string, q: SearchQuery) => void;
  requestFocus: () => void;
  /// Runs `q` on `serverId` and opens the panel.
  run: (serverId: string, q: SearchQuery) => void;
  setSort: (sort: SearchSort) => void;
  loadMore: () => void;
  retry: () => void;
  close: () => void;
  setJumped: (id: number) => void;
  /// MESSAGE_SEARCH_RES, already mapped.
  receive: (p: SearchAnswer) => void;
}

export interface SearchAnswer {
  serverId: string;
  seq: number;
  success: boolean;
  message: string;
  messages: Message[];
  hasMore: boolean;
  nextCursor: number;
  nextOffset: number;
  total: number;
  totalCapped: boolean;
  textSkippedChannelIds: string[];
  partial: boolean;
  retryAfterMs: number;
  firstPage: boolean;
}

let nextSeq = 1;
let noAnswerTimer: ReturnType<typeof setTimeout> | null = null;
let retryTimer: ReturnType<typeof setTimeout> | null = null;
let retries = 0;
/// The request behind the current seq, for retries and "load more".
let lastPage: { cursor: number; offset: number } = { cursor: 0, offset: 0 };

function clearTimers() {
  if (noAnswerTimer) clearTimeout(noAnswerTimer);
  if (retryTimer) clearTimeout(retryTimer);
  noAnswerTimer = retryTimer = null;
}

export const useSearchStore = create<SearchState>((set, get) => {
  const send = (page: { cursor: number; offset: number }) => {
    const s = get();
    if (!s.serverId) return;
    lastPage = page;
    const seq = s.seq;
    if (noAnswerTimer) clearTimeout(noAnswerTimer);
    noAnswerTimer = setTimeout(() => {
      const now = get();
      if (now.seq !== seq || (now.status !== "loading" && !now.loadingMore)) return;
      set({
        status: "error",
        loadingMore: false,
        error: "The server didn't answer. It may need an update before it can search.",
      });
    }, NO_ANSWER_MS);
    invoke("search_messages", toSearchArgs(s.serverId, s.query, s.sort, seq, page)).catch((e) => {
      if (get().seq !== seq) return;
      clearTimers();
      set({ status: "error", loadingMore: false, error: String(e) || "Couldn't reach the server." });
    });
  };

  const start = (serverId: string, query: SearchQuery, sort: SearchSort) => {
    clearTimers();
    retries = 0;
    // Relevance needs words to rank by, and the index (not the inside-words scan).
    const effective =
      sort === "relevance" && (searchWords(query.text).length === 0 || matchesInside(query)) ? "newest" : sort;
    set({
      serverId,
      open: true,
      query,
      sort: effective,
      seq: nextSeq++,
      status: "loading",
      results: [],
      total: -1,
      totalCapped: false,
      hasMore: false,
      loadingMore: false,
      nextCursor: 0,
      nextOffset: 0,
      textSkipped: [],
      partial: false,
      error: null,
      jumpedId: 0,
    });
    send({ cursor: 0, offset: 0 });
  };

  return {
    serverId: null,
    open: false,
    query: EMPTY_QUERY,
    sort: "newest",
    seq: 0,
    status: "idle",
    results: [],
    total: -1,
    totalCapped: false,
    hasMore: false,
    loadingMore: false,
    nextCursor: 0,
    nextOffset: 0,
    textSkipped: [],
    partial: false,
    error: null,
    jumpedId: 0,
    drafts: {},
    focusNonce: 0,

    setDraft: (serverId, q) => set((s) => ({ drafts: { ...s.drafts, [serverId]: q } })),
    requestFocus: () => set((s) => ({ focusNonce: s.focusNonce + 1 })),

    run: (serverId, q) => {
      if (!isSearchable(q)) return;
      const query: SearchQuery = { text: q.text.trim(), filters: q.filters };
      const s = get();
      // Reopening the same finished search costs the server nothing.
      if (!s.open && s.serverId === serverId && s.status === "done" && JSON.stringify(s.query) === JSON.stringify(query)) {
        set({ open: true });
        return;
      }
      start(serverId, query, s.sort);
    },

    setSort: (sort) => {
      const s = get();
      if (!s.serverId || sort === s.sort) return;
      start(s.serverId, s.query, sort);
    },

    loadMore: () => {
      const s = get();
      if (!s.serverId || !s.hasMore || s.loadingMore || s.status !== "done") return;
      retries = 0;
      set({ loadingMore: true });
      send({ cursor: s.nextCursor, offset: s.nextOffset });
    },

    retry: () => {
      const s = get();
      if (s.serverId) start(s.serverId, s.query, s.sort);
    },

    close: () => set({ open: false }),

    setJumped: (id) => set({ jumpedId: id }),

    receive: (p) => {
      const s = get();
      if (p.serverId !== s.serverId || p.seq !== s.seq) return;
      if (noAnswerTimer) clearTimeout(noAnswerTimer);
      noAnswerTimer = null;
      if (!p.success) {
        // Rate limited / busy: the server says when to come back.
        if (p.retryAfterMs > 0 && retries < MAX_RETRIES) {
          retries++;
          const seq = s.seq;
          const page = lastPage;
          retryTimer = setTimeout(() => {
            if (get().seq === seq) send(page);
          }, Math.min(p.retryAfterMs, 5000));
          return;
        }
        set({ status: "error", loadingMore: false, error: p.message || "Search failed." });
        return;
      }
      retries = 0;
      const seen = p.firstPage ? new Set<number>() : new Set(s.results.map((m) => m.id));
      const fresh = p.messages.filter((m) => !seen.has(m.id));
      set({
        status: "done",
        loadingMore: false,
        results: p.firstPage ? fresh : [...s.results, ...fresh],
        hasMore: p.hasMore,
        nextCursor: p.nextCursor,
        nextOffset: p.nextOffset,
        partial: p.partial,
        error: null,
        ...(p.firstPage
          ? { total: p.total, totalCapped: p.totalCapped, textSkipped: p.textSkippedChannelIds }
          : {}),
      });
    },
  };
});

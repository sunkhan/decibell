import { create } from "zustand";
import { invoke } from "../lib/ipc";
import type { ResolvedInvite } from "../types";

// Memo of `resolve_invite_code` lookups for the invite cards in chat,
// keyed by the upper-cased code. Entries are only ever added or
// replaced (stable refs for selectors). A definitive answer — resolved,
// or central says unknown/expired — sticks for the session; a
// connectivity failure (central down, timeout) is retried when a card
// asks again after RETRY_MS, so cards heal once central is back.

export interface InviteResolveEntry {
  status: "loading" | "done";
  resolved: ResolvedInvite | null;
  /// Error text when `resolved` is null; null while loading.
  error: string | null;
  /// Central answered and the code is unknown or expired — as opposed
  /// to central being unreachable.
  invalid: boolean;
  at: number;
}

interface InviteResolveState {
  entries: Record<string, InviteResolveEntry>;
  request: (code: string) => void;
}

const RETRY_MS = 30_000;
const MAX_ENTRIES = 300;
const INVALID_RE = /unknown|expired|not found|invalid/i;

function withEntry(
  entries: Record<string, InviteResolveEntry>,
  code: string,
  entry: InviteResolveEntry,
): Record<string, InviteResolveEntry> {
  const next = { ...entries, [code]: entry };
  const keys = Object.keys(next);
  if (keys.length > MAX_ENTRIES) {
    for (const k of keys.slice(0, keys.length - MAX_ENTRIES)) delete next[k];
  }
  return next;
}

export const useInviteResolveStore = create<InviteResolveState>((set, get) => ({
  entries: {},
  request: (rawCode) => {
    const code = rawCode.trim().toUpperCase();
    if (!code) return;
    const cur = get().entries[code];
    if (cur) {
      const retryable =
        cur.status === "done" && cur.resolved === null && !cur.invalid;
      if (!retryable || Date.now() - cur.at < RETRY_MS) return;
    }
    set((s) => ({
      entries: withEntry(s.entries, code, {
        status: "loading",
        resolved: null,
        error: null,
        invalid: false,
        at: Date.now(),
      }),
    }));
    invoke<ResolvedInvite>("resolve_invite_code", { code }).then(
      (resolved) =>
        set((s) => ({
          entries: withEntry(s.entries, code, {
            status: "done",
            resolved,
            error: null,
            invalid: false,
            at: Date.now(),
          }),
        })),
      (e: unknown) => {
        const error = String(e).replace(/^Error:\s*/, "");
        set((s) => ({
          entries: withEntry(s.entries, code, {
            status: "done",
            resolved: null,
            error,
            invalid: INVALID_RE.test(error),
            at: Date.now(),
          }),
        }));
      },
    );
  },
}));

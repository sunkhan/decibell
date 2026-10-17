// Merging a fetched page of stored messages into a loaded slice. Channels
// (chatStore) and DMs (dmStore) share it. Pure, with no store access, so it
// can be checked outside the app.
//
// A slice is its stored rows by id ascending, then the rows with no id yet
// (optimistic sends, legacy rows) at the tail. A page is a contiguous run of
// stored rows, in any order: everything the server holds between its oldest
// and newest row (the byte budget only ever cuts the far end). Both facts
// let a page repair a slice that missed traffic (review B4): a row the slice
// has inside the page's range but the page lacks was deleted, a row the page
// has changed was edited, and a page entirely above the slice's newest row
// may sit past a gap.

/// The fields the merge reads.
export interface MergeRow {
  id?: number;
  nonce?: string;
  senderUid?: number;
  content: string;
  replyTo?: number;
  editedAt?: number;
  encrypted?: boolean;
  decryptError?: string;
  /// Optimistic rows: the newest stored id in the slice when it was sent.
  /// The message's row, if it was stored, has a higher id.
  sentAfterId?: number;
}

/// A row's server id, 0 for one that has none yet.
export function realId(m: { id?: number }): number {
  return typeof m.id === "number" && m.id > 0 ? m.id : 0;
}

/// The newest stored id in a slice (0 when it holds none).
export function newestStoredId(slice: { id?: number }[]): number {
  for (let i = slice.length - 1; i >= 0; i--) {
    const id = realId(slice[i]);
    if (id > 0) return id;
  }
  return 0;
}

/// The optimistic sends (by nonce) a page of stored rows confirms. A row
/// carrying the bubble's nonce settles it (a community hands the sender
/// its recent nonces back in history). Rows without one, as DM history and
/// rows a restarted community forgot, settle a text bubble whose content and
/// reply target they repeat, if they are ours, newer than the bubble's
/// `sentAfterId` and not already in the slice; bubbles take such rows
/// oldest first, so two identical sends can't both claim one row. An
/// attachment-only bubble has nothing to compare and waits for its nonce.
export function settledNonces(slice: MergeRow[], page: MergeRow[], ownUid: number): Set<string> {
  const out = new Set<string>();
  const pending = slice.filter((m) => realId(m) === 0 && !!m.nonce);
  if (pending.length === 0) return out;
  const waiting = new Set(pending.map((m) => m.nonce!));
  const known = new Set<number>();
  for (const m of slice) if (realId(m) > 0) known.add(realId(m));
  const unclaimed: MergeRow[] = [];
  for (const r of page) {
    const id = realId(r);
    if (id === 0) continue;
    if (r.nonce) {
      // Another session's send carries its own nonce: never ours to claim.
      if (waiting.delete(r.nonce)) out.add(r.nonce);
      continue;
    }
    if (ownUid > 0 && r.senderUid === ownUid && !known.has(id)) unclaimed.push(r);
  }
  if (waiting.size === 0 || unclaimed.length === 0) return out;
  unclaimed.sort((a, b) => realId(a) - realId(b));
  const claimed = new Set<number>();
  for (const m of pending) {
    if (!waiting.has(m.nonce!) || !m.content.trim()) continue;
    const row = unclaimed.find(
      (r) =>
        !claimed.has(realId(r)) &&
        realId(r) > (m.sentAfterId ?? 0) &&
        r.content === m.content &&
        (r.replyTo ?? 0) === (m.replyTo ?? 0),
    );
    if (row) {
      claimed.add(realId(row));
      out.add(m.nonce!);
    }
  }
  return out;
}

export interface MergedPage<T> {
  /// The new slice (the same array when nothing changed).
  list: T[];
  /// The page's rows went in. False for a page above the slice's newest
  /// row in `after` mode (or into a slice with no stored rows): it doesn't
  /// connect, so only the bubbles it confirms were settled.
  applied: boolean;
  /// `before` mode: the page lay entirely above the slice's newest row and
  /// replaced the stored rows (they may sit past a gap).
  replaced: boolean;
  /// The page reached below the slice's oldest row: its "older messages
  /// exist" flag now describes the slice's head.
  headExtended: boolean;
  /// `before` mode: the page reached the slice's newest row or past it,
  /// i.e. it was the channel's newest page and the slice is now at present.
  reachedNewest: boolean;
}

function unchanged(a: MergeRow, b: MergeRow): boolean {
  return (
    a.content === b.content &&
    (a.editedAt ?? 0) === (b.editedAt ?? 0) &&
    !!a.encrypted === !!b.encrypted &&
    (a.decryptError ?? "") === (b.decryptError ?? "")
  );
}

/// Merge a page into a slice. `afterId` > 0: the page holds the rows above
/// it (downward paging, a re-sync); 0: a newest or older page (rows below
/// a `before_id`). `skip` ids are never added (deletes awaiting the
/// server's answer). Pending bubbles the page confirms are dropped (see
/// settledNonces); the rest stay at the tail.
export function mergeStoredPage<T extends MergeRow>(
  slice: T[],
  page: T[],
  ownUid: number,
  afterId = 0,
  skip?: Set<number>,
): MergedPage<T> {
  const settled = settledNonces(slice, page, ownUid);
  const stored: T[] = [];
  const tail: T[] = [];
  for (const m of slice) {
    if (realId(m) > 0) stored.push(m);
    else if (!(m.nonce && settled.has(m.nonce))) tail.push(m);
  }
  const byId = new Map<number, T>();
  for (const m of page) {
    const id = realId(m);
    if (id > 0 && !skip?.has(id)) byId.set(id, m);
  }
  let rows = [...byId.values()].sort((a, b) => realId(a) - realId(b));
  const storedOldest = stored.length > 0 ? realId(stored[0]) : 0;
  const storedNewest = stored.length > 0 ? realId(stored[stored.length - 1]) : 0;
  const keepSlice = (applied: boolean, headExtended: boolean, reachedNewest: boolean): MergedPage<T> => ({
    list: settled.size > 0 ? [...stored, ...tail] : slice,
    applied,
    replaced: false,
    headExtended,
    reachedNewest,
  });

  if (afterId > 0) {
    // Above the newest row there may be a gap; into nothing, a reply to a
    // request from before the slice was reset.
    if (stored.length === 0 || afterId > storedNewest) return keepSlice(false, false, false);
    // Rows below the slice's oldest would hang under it past a gap when the
    // page doesn't reach up to it; they stay reachable by scrolling up.
    rows = rows.filter((r) => realId(r) >= storedOldest);
    if (rows.length === 0) return keepSlice(true, false, false);
    const lo = Math.max(afterId + 1, storedOldest);
    return {
      list: refresh(stored, rows, lo, realId(rows[rows.length - 1]), tail),
      applied: true,
      replaced: false,
      headExtended: false,
      reachedNewest: false,
    };
  }

  if (rows.length === 0) {
    // Nothing below the request's boundary (or an empty channel).
    return keepSlice(true, true, stored.length === 0);
  }
  const pageOldest = realId(rows[0]);
  const pageNewest = realId(rows[rows.length - 1]);
  if (stored.length === 0 || pageOldest > storedNewest) {
    return {
      list: [...rows, ...tail],
      applied: true,
      replaced: stored.length > 0,
      headExtended: true,
      reachedNewest: true,
    };
  }
  return {
    list: refresh(stored, rows, pageOldest, pageNewest, tail),
    applied: true,
    replaced: false,
    headExtended: pageOldest < storedOldest,
    reachedNewest: pageNewest >= storedNewest,
  };
}

/// Stored rows outside [lo, hi] as they are; inside it, the page's rows,
/// keeping the slice's object for a row that didn't change (rows are
/// memoised by identity); then the tail.
function refresh<T extends MergeRow>(stored: T[], rows: T[], lo: number, hi: number, tail: T[]): T[] {
  const old = new Map<number, T>();
  for (const m of stored) old.set(realId(m), m);
  const out: T[] = [];
  for (const m of stored) {
    const id = realId(m);
    if (id < lo || id > hi) out.push(m);
  }
  for (const r of rows) {
    const prev = old.get(realId(r));
    out.push(prev && unchanged(prev, r) ? prev : r);
  }
  out.sort((a, b) => realId(a) - realId(b));
  return [...out, ...tail];
}

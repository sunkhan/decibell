import { useCallback, useEffect, useRef, useState } from "react";
import type { GifResult } from "../../types";

// The GIFs tab of the emoji picker. Search box → Tenor (through main,
// see electron/main/gifs.ts), results in a two-column masonry, click
// sends. An empty query shows Tenor's featured feed so the tab is never
// blank; scrolling near the bottom pages in more.
//
// Each cell reserves its height from the preview's dimensions so the
// grid doesn't reflow as GIFs stream in.

const COLUMNS = 2;
/// Picker width 352 − 16 padding − one 4px gap, halved.
const COLUMN_PX = 166;
const CELL_MAX_PX = 300;
const CELL_MIN_PX = 60;
const SEARCH_DEBOUNCE_MS = 300;
const LOAD_MORE_PX = 300;

function cellHeight(g: GifResult): number {
  const w = g.previewWidth || g.width;
  const h = g.previewHeight || g.height;
  if (w <= 0 || h <= 0) return COLUMN_PX;
  return Math.max(CELL_MIN_PX, Math.min(CELL_MAX_PX, Math.round((COLUMN_PX * h) / w)));
}

/// Shortest-column-first placement — the usual masonry rule.
function intoColumns(results: GifResult[]): GifResult[][] {
  const cols: GifResult[][] = Array.from({ length: COLUMNS }, () => []);
  const heights = new Array<number>(COLUMNS).fill(0);
  for (const g of results) {
    let target = 0;
    for (let i = 1; i < COLUMNS; i++) if (heights[i] < heights[target]) target = i;
    cols[target].push(g);
    heights[target] += cellHeight(g) + 4;
  }
  return cols;
}

function dedupe(list: GifResult[]): GifResult[] {
  const seen = new Set<string>();
  return list.filter((g) => (seen.has(g.id) ? false : (seen.add(g.id), true)));
}

export default function GifPicker({ onPick }: { onPick: (gif: GifResult) => void }) {
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");
  const [results, setResults] = useState<GifResult[]>([]);
  const [next, setNext] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [configured, setConfigured] = useState<boolean | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  // Monotonic request id: a stale response (older query, or a page of a
  // query the user has since left) must not land on the current list.
  const seqRef = useRef(0);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  useEffect(() => {
    let alive = true;
    window.decibell.gifs.status().then(
      (s) => alive && setConfigured(s.configured),
      () => alive && setConfigured(false),
    );
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    const id = window.setTimeout(() => setDebounced(query.trim()), SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(id);
  }, [query]);

  const load = useCallback(async (q: string, pos: string | null) => {
    const seq = ++seqRef.current;
    setLoading(true);
    const r = await window.decibell.gifs
      .search(q, pos, navigator.language)
      .catch((e: unknown) => ({ ok: false as const, error: String(e) }));
    if (seq !== seqRef.current) return;
    setLoading(false);
    if (!r.ok) {
      setError(r.error === "not-configured" ? null : r.error);
      if (!pos) setResults([]);
      setNext(null);
      return;
    }
    setError(null);
    setResults((prev) => (pos ? dedupe([...prev, ...r.page.results]) : r.page.results));
    setNext(r.page.next);
  }, []);

  useEffect(() => {
    if (configured !== true) return;
    scrollRef.current?.scrollTo({ top: 0 });
    void load(debounced, null);
  }, [debounced, configured, load]);

  const onScroll = () => {
    const el = scrollRef.current;
    if (!el || loading || !next) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < LOAD_MORE_PX) {
      void load(debounced, next);
    }
  };

  const columns = intoColumns(results);

  return (
    <>
      <div className="shrink-0 px-3 pb-2 pt-2">
        <div
          className="flex items-center gap-2 rounded-md border border-border bg-bg-lighter px-3 transition-all focus-within:border-accent focus-within:shadow-ring"
          style={{ height: 36 }}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="shrink-0 text-text-muted">
            <circle cx="11" cy="11" r="8" />
            <line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search Tenor..."
            disabled={configured === false}
            className="flex-1 bg-transparent text-[13px] text-text-primary outline-none placeholder:text-text-muted disabled:cursor-not-allowed"
          />
          {query && (
            <button
              onClick={() => setQuery("")}
              className="text-text-muted transition-colors hover:text-text-secondary"
              title="Clear"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          )}
        </div>
      </div>

      <div
        ref={scrollRef}
        onScroll={onScroll}
        className="flex-1 overflow-y-auto px-2 pb-2 scrollbar-thin"
      >
        {configured === false ? (
          <Notice title="GIF search isn't set up for this build">
            Add a Tenor API key to <code className="font-mono">resources/tenor.json</code>{" "}
            or set <code className="font-mono">TENOR_API_KEY</code> — see HANDOFF.md.
          </Notice>
        ) : error ? (
          <Notice title="GIF search failed">{error}</Notice>
        ) : results.length === 0 ? (
          <Notice title={loading || configured === null ? "Loading…" : `No GIFs match "${debounced}"`} />
        ) : (
          <div className="flex gap-1">
            {columns.map((col, ci) => (
              <div key={ci} className="flex min-w-0 flex-1 flex-col gap-1">
                {col.map((g) => (
                  <button
                    key={g.id}
                    type="button"
                    onClick={() => onPick(g)}
                    title={g.title}
                    className="block w-full cursor-pointer overflow-hidden rounded-md bg-bg-lighter outline-accent -outline-offset-2 hover:outline-2"
                    style={{ height: cellHeight(g) }}
                  >
                    <img
                      src={g.preview}
                      alt={g.title}
                      loading="lazy"
                      draggable={false}
                      referrerPolicy="no-referrer"
                      className="h-full w-full object-cover"
                    />
                  </button>
                ))}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Tenor's API terms require visible "Powered by Tenor" attribution. */}
      <div className="flex h-[34px] shrink-0 items-center justify-end border-t border-border-divider px-3.5 font-meta text-[11px] text-text-muted">
        {loading && results.length > 0 ? <span className="mr-auto">Loading…</span> : null}
        Powered by Tenor
      </div>
    </>
  );
}

function Notice({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <div className="flex flex-col items-center justify-center gap-1.5 px-4 py-12 text-center">
      <span className="text-[12px] font-medium text-text-secondary">{title}</span>
      {children ? (
        <span className="text-[12px] leading-[1.5] text-text-muted [overflow-wrap:anywhere]">
          {children}
        </span>
      ) : null}
    </div>
  );
}

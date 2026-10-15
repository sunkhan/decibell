import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";
import { EMPTY_LIST } from "../../lib/empty";
import type { ChannelInfo, ServerMember } from "../../types";
import { UserAvatar } from "../../components/UserAvatar";
import { resolveDisplayName, useDisplayName } from "../../hooks/useDisplayName";
import { Icon, type IconName } from "../servers/tabs/settingsUi";
import { useMemberSearch } from "../servers/tabs/useMemberSearch";
import Switch from "../../components/Switch";
import DatePicker from "./DatePicker";
import { useSearchStore } from "./searchStore";
import {
  DATE_OPS,
  EMPTY_QUERY,
  HAS_KINDS,
  HAS_LABEL,
  dayKey,
  dayStart,
  isSearchable,
  matchesInside,
  parseDay,
  withFilter,
  type DateOp,
  type FilterKind,
  type HasKind,
  type SearchFilter,
  type SearchQuery,
} from "./searchQuery";
import {
  categoryOf,
  chipTokens,
  hasKindFor,
  isSearchableChannel,
  resolveToken,
  trailingToken,
  type TokenContext,
} from "./searchTokens";
import { legacyUid } from "../users/userDirectory";

/// The search box in a text channel's header (spec 2026-10-14). Filters
/// are chips before the text; typing `from:`, `in:`, `has:`, `before:`,
/// `during:` or `after:` opens a picker for it. Enter runs the search into
/// the results panel. Ctrl+F (⌘F) focuses it.

const DAY_FMT = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });

export function formatDay(day: string): string {
  const d = dayStart(day);
  return d ? DAY_FMT.format(d) : day;
}

type FilterOp = Exclude<FilterKind, "inside">;

const OP_INFO: Record<FilterOp, { icon: IconName; hint: string }> = {
  from: { icon: "user", hint: "a member" },
  in: { icon: "hash", hint: "a channel" },
  has: { icon: "paperclip", hint: "link, file, image, video, audio or document" },
  before: { icon: "calendar", hint: "a date" },
  during: { icon: "calendar", hint: "a date" },
  after: { icon: "calendar", hint: "a date" },
};

const HAS_ICON: Record<HasKind, IconName> = {
  link: "link",
  file: "paperclip",
  image: "image",
  video: "film",
  audio: "music",
  document: "file",
};

type Row =
  | { kind: "run"; text: string }
  | { kind: "op"; op: FilterOp }
  | { kind: "inside" }
  | { kind: "member"; member: ServerMember }
  | { kind: "channel"; channel: ChannelInfo }
  | { kind: "has"; has: HasKind }
  | { kind: "day"; day: string; label: string };

export default function SearchBox({ serverId, channelId }: { serverId: string; channelId: string }) {
  const stored = useSearchStore((s) => s.drafts[serverId]);
  const draft = stored ?? EMPTY_QUERY;
  const setDraftFor = useSearchStore((s) => s.setDraft);
  const run = useSearchStore((s) => s.run);
  const focusNonce = useSearchStore((s) => s.focusNonce);

  const me = useAuthStore((s) => s.username);
  const isOwner = useChatStore((s) => !!me && s.serverOwner[serverId] === me);
  const allChannels = useChatStore((s) => s.channelsByServer[serverId]) ?? EMPTY_LIST;
  const roster = useChatStore((s) => s.membersByServer[serverId]) ?? EMPTY_LIST;
  const channels = useMemo(
    () => allChannels.filter((c) => isSearchableChannel(c, isOwner)),
    [allChannels, isOwner],
  );
  // Members found on the server for the `from:` picker (offline ones too).
  const [found, setFound] = useState<ServerMember[]>(EMPTY_LIST);
  const members = useMemo(() => {
    if (found.length === 0) return roster;
    const seen = new Set(roster.map((m) => m.username));
    return [...roster, ...found.filter((m) => !seen.has(m.username))];
  }, [roster, found]);
  const ctx: TokenContext = useMemo(() => ({ members, channels }), [members, channels]);

  const setDraft = useCallback((q: SearchQuery) => setDraftFor(serverId, q), [serverId, setDraftFor]);
  const inputRef = useRef<HTMLInputElement>(null);
  const boxRef = useRef<HTMLDivElement>(null);
  const [focused, setFocused] = useState(false);
  const [menu, setMenu] = useState(false);
  // The chips + text row scrolls sideways when it outgrows the fixed-width
  // box; its left edge fades once scrolled so a cut-off chip doesn't show.
  const [scrolled, setScrolled] = useState(false);
  const [active, setActive] = useState(0);
  const token = trailingToken(draft.text);
  const dateOp = token && (DATE_OPS as string[]).includes(token.op) ? (token.op as DateOp) : null;
  const [calDay, setCalDay] = useState(() => dayKey(new Date()));

  // Ctrl+F / ⌘F from anywhere in the server view; the store nonce lets
  // other UI ask for focus too.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "f") {
        e.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  useEffect(() => {
    if (focusNonce > 0) inputRef.current?.focus();
  }, [focusNonce]);

  // A typed date jumps the calendar to it.
  useEffect(() => {
    if (!dateOp || !token) return;
    const d = parseDay(token.value);
    if (d) setCalDay(d);
  }, [dateOp, token?.value]); // eslint-disable-line react-hooks/exhaustive-deps

  const rows: Row[] = useMemo(() => {
    if (token) {
      const v = token.value.toLowerCase();
      switch (token.op) {
        case "from": {
          const q = v.replace(/^@/, "");
          return members
            .filter(
              (m) =>
                !q ||
                m.username.toLowerCase().includes(q) ||
                (m.nickname && m.nickname.toLowerCase().includes(q)),
            )
            .slice(0, 8)
            .map((member) => ({ kind: "member", member }) as Row);
        }
        case "in": {
          const q = v.replace(/^#/, "");
          const list = channels.filter((c) => !q || c.name.toLowerCase().includes(q));
          // The channel you're in first.
          list.sort((a, b) => Number(b.id === channelId) - Number(a.id === channelId));
          return list.slice(0, 10).map((channel) => ({ kind: "channel", channel }) as Row);
        }
        case "has":
          return HAS_KINDS.filter((k) => !v || k.startsWith(v) || hasKindFor(v) === k).map(
            (has) => ({ kind: "has", has }) as Row,
          );
        default: {
          const now = new Date();
          return [
            { kind: "day", day: dayKey(now), label: "Today" },
            { kind: "day", day: dayKey(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1)), label: "Yesterday" },
          ];
        }
      }
    }
    const out: Row[] = [];
    const text = draft.text.trim();
    if (text || draft.filters.length > 0) out.push({ kind: "run", text });
    for (const op of ["from", "in", "has", "before", "during", "after"] as FilterOp[]) out.push({ kind: "op", op });
    out.push({ kind: "inside" });
    return out;
  }, [token, members, channels, channelId, draft.text, draft.filters.length]);

  // A picker preselects its best match and typed words preselect "Search
  // for…"; the plain filter list waits for the arrow keys, so Enter in an
  // empty box doesn't insert a filter. Dates use the calendar's own day.
  const defaultActive = dateOp ? -1 : token || rows[0]?.kind === "run" ? 0 : -1;
  useEffect(() => {
    setActive(defaultActive);
  }, [token?.op, rows.length, defaultActive]);

  const focusInput = () => requestAnimationFrame(() => inputRef.current?.focus());

  const addChip = useCallback(
    (f: SearchFilter) => {
      const text = token ? draft.text.slice(0, token.start).replace(/\s+$/, "") : draft.text;
      setDraft({ text: text ? `${text} ` : "", filters: withFilter(draft.filters, f) });
      focusInput();
    },
    [draft, token, setDraft],
  );

  const startOp = (op: FilterOp) => {
    const base = draft.text.replace(/\s+$/, "");
    setDraft({ ...draft, text: `${base ? `${base} ` : ""}${op}:` });
    setMenu(true);
    focusInput();
  };

  const doRun = () => {
    const final = chipTokens(draft, ctx);
    if (!isSearchable(final)) return;
    setDraft(final);
    run(serverId, final);
    setMenu(false);
    inputRef.current?.blur();
  };

  const pick = (row: Row) => {
    switch (row.kind) {
      case "run":
        return doRun();
      case "op":
        return startOp(row.op);
      case "inside":
        setDraft({
          ...draft,
          filters: matchesInside(draft)
            ? draft.filters.filter((f) => f.kind !== "inside")
            : withFilter(draft.filters, { kind: "inside" }),
        });
        return focusInput();
      case "member":
        return addChip({ kind: "from", username: row.member.username });
      case "channel":
        return addChip({ kind: "in", channelId: row.channel.id });
      case "has":
        return addChip({ kind: "has", has: row.has });
      case "day":
        return dateOp && addChip({ kind: dateOp, date: row.day });
    }
  };

  const onText = (text: string) => {
    // A finished filter followed by a space becomes a chip right away.
    const m = /(^|\s)(from|in|has|before|during|after):(\S+)\s$/i.exec(text);
    if (m) {
      const f = resolveToken(m[2].toLowerCase() as FilterKind, m[3], ctx);
      if (f) {
        const rest = text.slice(0, m.index + m[1].length).replace(/\s+$/, "");
        setDraft({ text: rest ? `${rest} ` : "", filters: withFilter(draft.filters, f) });
        return;
      }
    }
    setDraft({ ...draft, text });
    setMenu(true);
  };

  const moveDay = (days: number) => {
    const d = dayStart(calDay, days);
    if (!d) return;
    const key = dayKey(d);
    const today = dayKey(new Date());
    setCalDay(key > today ? today : key);
  };

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    const input = e.currentTarget;
    if (e.key === "Escape") {
      e.stopPropagation();
      if (menu) setMenu(false);
      else input.blur();
      return;
    }
    if (e.key === "Backspace" && input.selectionStart === 0 && input.selectionEnd === 0 && draft.filters.length > 0) {
      e.preventDefault();
      setDraft({ ...draft, filters: draft.filters.slice(0, -1) });
      return;
    }
    if (!menu) {
      if (e.key === "Enter") doRun();
      else if (e.key === "ArrowDown") setMenu(true);
      return;
    }
    if (dateOp) {
      const steps: Record<string, number> = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 };
      if (e.key in steps) {
        e.preventDefault();
        moveDay(steps[e.key]);
        return;
      }
      if (e.key === "Enter" || e.key === "Tab") {
        e.preventDefault();
        addChip({ kind: dateOp, date: (token && parseDay(token.value)) || calDay });
      }
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (rows.length)
        setActive((a) => (a < 0 ? (e.key === "ArrowDown" ? 0 : rows.length - 1) : (a + (e.key === "ArrowDown" ? 1 : -1) + rows.length) % rows.length));
      return;
    }
    if (e.key === "Tab" && token && rows[active]) {
      e.preventDefault();
      pick(rows[active]);
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      if (rows[active]) pick(rows[active]);
      else doRun();
    }
  };

  const hasContent = draft.text.length > 0 || draft.filters.length > 0;

  return (
    <div
      ref={boxRef}
      onMouseDown={(e) => {
        // Clicks on the box's padding and chips focus the text.
        if (e.target !== inputRef.current) {
          e.preventDefault();
          inputRef.current?.focus();
        }
      }}
      title="Search messages (Ctrl+F)"
      // Centred over the 260 px members list (owner, 2026-10-14): 244 px with
      // the header's 8 px right padding leaves 8 px on either side of the
      // column, and the 16 px gap keeps the members toggle 8 px clear of it,
      // over the chat. Fixed width: the menu under it is wider anyway.
      className={`ml-4 flex h-8 w-[244px] shrink-0 cursor-text items-center gap-1 rounded-md border bg-bg-lighter pl-1.5 pr-1 font-sans text-[13px] font-normal tracking-normal ${
        focused ? "border-accent shadow-ring" : "border-border hover:border-accent/40"
      }`}
    >
      <div
        onScroll={(e) => setScrolled(e.currentTarget.scrollLeft > 0)}
        className={`flex min-w-0 flex-1 items-center gap-1 overflow-x-auto [scrollbar-width:none] ${
          scrolled ? "[mask-image:linear-gradient(to_right,transparent,black_2.5rem)]" : ""
        }`}
      >
        {draft.filters.map((f, i) => (
          <Chip
            key={`${f.kind}-${i}`}
            filter={f}
            serverId={serverId}
            channels={allChannels}
            onRemove={() => setDraft({ ...draft, filters: draft.filters.filter((_, j) => j !== i) })}
          />
        ))}
        <input
          ref={inputRef}
          value={draft.text}
          onChange={(e) => onText(e.target.value)}
          onKeyDown={onKeyDown}
          onFocus={() => {
            setFocused(true);
            setMenu(true);
          }}
          onBlur={() => {
            setFocused(false);
            setMenu(false);
          }}
          placeholder={draft.filters.length ? "" : "Search"}
          spellCheck={false}
          aria-label="Search messages"
          aria-expanded={menu}
          aria-autocomplete="list"
          className="h-full min-w-[56px] flex-1 bg-transparent pl-1 text-text-primary outline-none placeholder:text-text-muted"
        />
      </div>
      {hasContent ? (
        <button
          type="button"
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => {
            setDraft(EMPTY_QUERY);
            inputRef.current?.focus();
          }}
          aria-label="Clear search"
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
        >
          <Icon name="x" size={12} strokeWidth={2.5} />
        </button>
      ) : (
        <span className="flex h-6 w-6 shrink-0 items-center justify-center text-text-muted">
          <Icon name="search" size={14} />
        </span>
      )}
      {menu && (
        <SearchMenu anchor={boxRef}>
          {token?.op === "from" && token.value.replace(/^@/, "").trim() ? (
            <MemberLookup serverId={serverId} query={token.value.replace(/^@/, "").trim()} onFound={setFound} />
          ) : null}
          <MenuBody
            rows={rows}
            active={active}
            onActive={setActive}
            onPick={pick}
            token={token?.op ?? null}
            inside={matchesInside(draft)}
            serverId={serverId}
            channelId={channelId}
            allChannels={allChannels}
            calendar={
              dateOp ? (
                <DatePicker
                  active={calDay}
                  onActive={setCalDay}
                  onPick={(day) => addChip({ kind: dateOp, date: day })}
                />
              ) : null
            }
          />
        </SearchMenu>
      )}
    </div>
  );
}

/// Runs the server-side member search while a `from:` name is typed, so
/// offline members past the loaded roster can be picked.
function MemberLookup({
  serverId,
  query,
  onFound,
}: {
  serverId: string;
  query: string;
  onFound: (m: ServerMember[]) => void;
}) {
  const { members } = useMemberSearch(serverId, query, 0);
  useEffect(() => onFound(members), [members, onFound]);
  return null;
}

/// The dropdown under the box: portaled, right-aligned to it, never takes
/// focus (mousedown is swallowed so the input keeps it).
function SearchMenu({ anchor, children }: { anchor: React.RefObject<HTMLDivElement | null>; children: ReactNode }) {
  const [pos, setPos] = useState<{ top: number; right: number; width: number } | null>(null);
  useLayoutEffect(() => {
    const place = () => {
      const el = anchor.current;
      if (!el) return;
      const r = el.getBoundingClientRect();
      setPos({
        top: Math.round(r.bottom + 6),
        right: Math.max(8, Math.round(window.innerWidth - r.right)),
        width: Math.round(Math.min(Math.max(r.width, 360), r.right - 8)),
      });
    };
    place();
    window.addEventListener("resize", place);
    return () => window.removeEventListener("resize", place);
  }, [anchor]);
  if (!pos) return null;
  return createPortal(
    <div
      onMouseDown={(e) => e.preventDefault()}
      style={{ top: pos.top, right: pos.right, width: pos.width }}
      className="fixed z-[100] max-h-[min(520px,calc(100vh-80px))] animate-[dropIn_0.15s_ease] overflow-y-auto rounded-lg border border-border bg-bg-light py-1.5 font-sans shadow-float"
      role="listbox"
    >
      {children}
    </div>,
    document.body,
  );
}

function MenuBody({
  rows,
  active,
  onActive,
  onPick,
  token,
  inside,
  serverId,
  channelId,
  allChannels,
  calendar,
}: {
  rows: Row[];
  active: number;
  onActive: (i: number) => void;
  onPick: (r: Row) => void;
  token: FilterKind | null;
  inside: boolean;
  serverId: string;
  channelId: string;
  allChannels: ChannelInfo[];
  calendar: ReactNode;
}) {
  const label = (text: string) => (
    <div className="px-3 pb-1 pt-2 font-section text-section font-medium uppercase leading-none tracking-section text-text-muted">
      {text}
    </div>
  );
  const row = (r: Row, i: number, content: ReactNode) => (
    <div
      key={i}
      role="option"
      aria-selected={i === active}
      onMouseMove={() => i !== active && onActive(i)}
      onClick={() => onPick(r)}
      className={`mx-1.5 flex h-9 cursor-pointer items-center gap-2.5 rounded-md px-2 text-[13px] ${
        i === active ? "bg-surface-hover" : ""
      }`}
    >
      {content}
    </div>
  );

  if (token === "before" || token === "during" || token === "after") {
    return (
      <>
        {label(token === "before" ? "Before" : token === "after" ? "After" : "On")}
        {rows.map((r, i) =>
          r.kind === "day"
            ? row(r, i, (
                <>
                  <Icon name="calendar" size={15} className="text-text-muted" />
                  <span className="flex-1 font-medium text-text-primary">{r.label}</span>
                  <span className="text-[12px] tabular-nums text-text-muted">{formatDay(r.day)}</span>
                </>
              ))
            : null,
        )}
        <div className="mx-3 my-1.5 h-px bg-border-divider" />
        {calendar}
        <MenuHint keys={[["←→↑↓", "pick a day"], ["Enter", "add"], ["2026-10-03", "or type it"]]} />
      </>
    );
  }

  if (token === "from" || token === "in" || token === "has") {
    const title = token === "from" ? "From" : token === "in" ? "In channel" : "Has";
    return (
      <>
        {label(title)}
        {rows.length === 0 ? (
          <div className="px-3 py-2 text-[13px] text-text-muted">
            {token === "from" ? "No member by that name." : token === "in" ? "No channel by that name." : "Nothing by that name."}
          </div>
        ) : null}
        {rows.map((r, i) => {
          if (r.kind === "member") return row(r, i, <MemberRow serverId={serverId} member={r.member} />);
          if (r.kind === "channel")
            return row(r, i, (
              <>
                <Icon name={r.channel.encrypted ? "lock" : "hash"} size={15} className="text-text-muted" />
                <span className="min-w-0 flex-1 truncate font-medium text-text-primary">{r.channel.name}</span>
                {r.channel.id === channelId ? (
                  <span className="text-[12px] text-text-muted">this channel</span>
                ) : (
                  <span className="max-w-[40%] truncate text-[12px] text-text-muted">{categoryOf(allChannels, r.channel.id)}</span>
                )}
              </>
            ));
          if (r.kind === "has")
            return row(r, i, (
              <>
                <Icon name={HAS_ICON[r.has]} size={15} className="text-text-muted" />
                <span className="flex-1 font-medium text-text-primary">{HAS_LABEL[r.has]}</span>
              </>
            ));
          return null;
        })}
        <MenuHint keys={[["↑↓", "choose"], ["Enter", "add"], ["Esc", "close"]]} />
      </>
    );
  }

  let i = -1;
  const runRow = rows[0]?.kind === "run" ? rows[0] : null;
  const ops = rows.filter((r) => r.kind === "op");
  const insideRow = rows.find((r) => r.kind === "inside");
  return (
    <>
      {runRow && runRow.kind === "run"
        ? row(runRow, ++i, (
            <>
              <Icon name="search" size={15} className="text-text-muted" />
              <span className="min-w-0 flex-1 truncate text-text-secondary">
                {runRow.text ? (
                  <>
                    Search for <span className="font-semibold text-text-bright">{runRow.text}</span>
                  </>
                ) : (
                  "Search with these filters"
                )}
              </span>
              <Keycap>Enter</Keycap>
            </>
          ))
        : null}
      {label("Filters")}
      {ops.map((r) =>
        r.kind === "op"
          ? row(r, ++i, (
              <>
                <Icon name={OP_INFO[r.op].icon} size={15} className="text-text-muted" />
                <span className="font-semibold text-text-primary">{r.op}:</span>
                <span className="min-w-0 flex-1 truncate text-text-muted">{OP_INFO[r.op].hint}</span>
              </>
            ))
          : null,
      )}
      {insideRow ? (
        <>
          {label("Match")}
          {row(insideRow, ++i, (
            <>
              <span className="min-w-0 flex-1 truncate">
                <span className="font-medium text-text-primary">Inside words</span>
                <span className="ml-2 text-text-muted">abc also finds xabcy</span>
              </span>
              {/* The row toggles it; the switch only shows the state. */}
              <Switch checked={inside} onToggle={() => {}} label="Match inside words" />
            </>
          ))}
        </>
      ) : null}
      <MenuHint keys={[["↑↓", "choose"], ["Enter", "search"], ["Ctrl F", "from anywhere"]]} />
    </>
  );
}

function MemberRow({ serverId, member }: { serverId: string; member: ServerMember }) {
  const name = useDisplayName(serverId, member.uid ?? 0, member.username);
  return (
    <>
      <UserAvatar uid={member.uid ?? 0} name={member.username} size={22} />
      <span className="min-w-0 truncate font-medium text-text-primary">{name}</span>
      {name !== member.username ? (
        <span className="min-w-0 flex-1 truncate text-[12px] text-text-muted">{member.username}</span>
      ) : (
        <span className="flex-1" />
      )}
    </>
  );
}

function Keycap({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded-sm border border-border bg-bg-lighter px-1.5 py-px font-sans text-[11px] font-semibold text-text-muted">
      {children}
    </kbd>
  );
}

function MenuHint({ keys }: { keys: [string, string][] }) {
  return (
    <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border-divider px-3 pb-0.5 pt-2 text-[11px] text-text-muted">
      {keys.map(([k, v]) => (
        <span key={k} className="flex items-center gap-1.5">
          <Keycap>{k}</Keycap>
          {v}
        </span>
      ))}
    </div>
  );
}

/// A chip's visible text: what it filters on, then the value.
function chipText(f: SearchFilter, serverId: string, channels: ChannelInfo[]): [string, string] {
  switch (f.kind) {
    case "from":
      return ["from", resolveDisplayName(serverId, legacyUid(f.username), f.username)];
    case "in":
      return ["in", `#${channels.find((c) => c.id === f.channelId)?.name ?? "deleted-channel"}`];
    case "has":
      return ["has", HAS_LABEL[f.has].toLowerCase()];
    case "inside":
      return ["match", "inside words"];
    default:
      return [f.kind === "during" ? "on" : f.kind, formatDay(f.date)];
  }
}

function Chip({
  filter,
  serverId,
  channels,
  onRemove,
}: {
  filter: SearchFilter;
  serverId: string;
  channels: ChannelInfo[];
  onRemove: () => void;
}) {
  const [op, value] = chipText(filter, serverId, channels);
  return (
    <span className="inline-flex h-6 max-w-[180px] shrink-0 items-center gap-1 rounded-sm bg-accent-soft pl-1.5 pr-0.5 text-[12px] font-medium">
      <span className="text-text-muted">{op}:</span>
      <span className="min-w-0 truncate text-text-primary">{value}</span>
      <button
        type="button"
        onMouseDown={(e) => e.preventDefault()}
        onClick={(e) => {
          e.stopPropagation();
          onRemove();
        }}
        aria-label={`Remove ${op}: ${value}`}
        className="flex h-5 w-5 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
      >
        <Icon name="x" size={10} strokeWidth={3} />
      </button>
    </span>
  );
}

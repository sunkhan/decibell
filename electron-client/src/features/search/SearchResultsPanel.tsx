import { memo, useEffect, useMemo, useRef } from "react";
import { useChatStore } from "../../stores/chatStore";
import { EMPTY_LIST } from "../../lib/empty";
import type { Attachment, ChannelInfo, Message } from "../../types";
import { UserAvatar } from "../../components/UserAvatar";
import SegmentedControl from "../../components/SegmentedControl";
import { useDisplayName } from "../../hooks/useDisplayName";
import { userColor } from "../../utils/colors";
import MessageText from "../chat/MessageText";
import { LockGlyph } from "../chat/MessageBubble";
import { buildAttachmentUrl, formatFileSize, pickThumbnailSize } from "../chat/attachmentHelpers";
import { goToMessage } from "../chat/goToMessage";
import { Icon } from "../servers/tabs/settingsUi";
import { makeFragmentHighlighter, makeHighlighter, type Highlighter } from "./highlight";
import { useSearchStore } from "./searchStore";
import { matchesInside, searchFragments, searchWords, type SearchSort } from "./searchQuery";

/// Message search results (spec 2026-10-14): the right-hand panel, in place
/// of the member list while open. Grouped by channel in result order, the
/// matched words marked, infinite scroll, and Jump (or a click anywhere on
/// a result) opens the message in its channel.

const COUNT_FMT = new Intl.NumberFormat();
const TIME_FMT = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
const DATE_FMT = new Intl.DateTimeFormat(undefined, { day: "numeric", month: "short", year: "numeric" });

function when(ts: string): string {
  const d = new Date(Number(ts) * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const now = new Date();
  const time = TIME_FMT.format(d);
  if (d.toDateString() === now.toDateString()) return `Today at ${time}`;
  const y = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1);
  if (d.toDateString() === y.toDateString()) return `Yesterday at ${time}`;
  return `${DATE_FMT.format(d)} at ${time}`;
}

const SORTS: { value: SearchSort; label: string }[] = [
  { value: "newest", label: "Newest" },
  { value: "oldest", label: "Oldest" },
  { value: "relevance", label: "Relevant" },
];

export default function SearchResultsPanel() {
  const serverId = useSearchStore((s) => s.serverId);
  const query = useSearchStore((s) => s.query);
  const sort = useSearchStore((s) => s.sort);
  const status = useSearchStore((s) => s.status);
  const results = useSearchStore((s) => s.results);
  const total = useSearchStore((s) => s.total);
  const totalCapped = useSearchStore((s) => s.totalCapped);
  const hasMore = useSearchStore((s) => s.hasMore);
  const loadingMore = useSearchStore((s) => s.loadingMore);
  const textSkipped = useSearchStore((s) => s.textSkipped);
  const partial = useSearchStore((s) => s.partial);
  const error = useSearchStore((s) => s.error);
  const jumpedId = useSearchStore((s) => s.jumpedId);
  const { setSort, loadMore, retry, close, setJumped } = useSearchStore.getState();
  const channels = useChatStore((s) => (serverId ? s.channelsByServer[serverId] : undefined)) ?? EMPTY_LIST;

  const inside = matchesInside(query);
  const words = useMemo(() => searchWords(query.text), [query.text]);
  const highlight = useMemo(
    () => (inside ? makeFragmentHighlighter(searchFragments(query.text)) : makeHighlighter(words)),
    [inside, query.text, words],
  );

  // Esc closes the panel unless something above it (a menu, a modal)
  // takes the key first.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [close]);

  // Infinite scroll: the sentinel below the last result asks for more.
  const scrollRef = useRef<HTMLDivElement>(null);
  const sentinelRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const root = scrollRef.current;
    const el = sentinelRef.current;
    if (!root || !el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) loadMore();
      },
      { root, rootMargin: "400px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [loadMore, results.length, hasMore]);

  // A new search starts at the top.
  const seq = useSearchStore((s) => s.seq);
  useEffect(() => {
    scrollRef.current?.scrollTo({ top: 0 });
  }, [seq]);

  if (!serverId) return null;

  const title =
    status === "loading"
      ? "Searching…"
      : status === "error"
        ? "Search"
        : total === 0 && results.length === 0
          ? "No results"
          : total < 0
            ? "Results"
            : `${COUNT_FMT.format(total)}${totalCapped ? "+" : ""} ${total === 1 && !totalCapped ? "result" : "results"}`;

  const skippedNames = textSkipped
    .map((id) => channels.find((c) => c.id === id)?.name)
    .filter((n): n is string => !!n);

  return (
    <aside
      aria-label="Search results"
      className="flex w-[400px] shrink-0 flex-col border-l border-border bg-bg-dark font-sans max-lg:absolute max-lg:bottom-0 max-lg:right-0 max-lg:top-0 max-lg:z-30 max-lg:w-[360px] max-lg:max-w-[calc(100%-16px)] max-lg:shadow-float"
    >
      <div className="flex h-12 shrink-0 items-center gap-2 border-b border-border-divider pl-4 pr-2">
        <span className="min-w-0 flex-1 truncate text-[14px] font-semibold tabular-nums text-text-bright">{title}</span>
        <div className="w-[196px] shrink-0">
          <SegmentedControl
            options={SORTS.map((s) =>
              s.value !== "relevance"
                ? s
                : inside
                  ? { ...s, disabled: true, title: "Not available when matching inside words" }
                  : words.length === 0
                    ? { ...s, disabled: true, title: "Add words to sort by relevance" }
                    : s,
            )}
            value={sort}
            onChange={setSort}
          />
        </div>
        <button
          type="button"
          onClick={close}
          aria-label="Close search results"
          title="Close (Esc)"
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-secondary"
        >
          <Icon name="x" size={16} />
        </button>
      </div>

      {/* The server names them only when text was searched (words, inside
          words, or has: link). */}
      {skippedNames.length > 0 ? (
        <Notice icon={<LockGlyph size={13} />}>
          Text isn't searched in encrypted {skippedNames.length === 1 ? "channel" : "channels"}{" "}
          {skippedNames.map((n, i) => (
            <span key={n}>
              {i > 0 ? ", " : ""}
              <span className="font-semibold text-text-secondary">#{n}</span>
            </span>
          ))}
          . Other filters still find {skippedNames.length === 1 ? "its" : "their"} messages.
        </Notice>
      ) : null}
      {partial && status === "done" ? (
        <Notice icon={<Icon name="hourglass" size={13} />}>
          This search took too long, so it stopped early. Add a filter to narrow it down.
        </Notice>
      ) : null}

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        {status === "loading" ? (
          <Skeleton />
        ) : status === "error" ? (
          <Empty icon="alert" title="Search didn't work" body={error ?? "Something went wrong."}>
            <button
              type="button"
              onClick={retry}
              className="mt-3 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
            >
              Try again
            </button>
          </Empty>
        ) : results.length === 0 ? (
          <Empty
            icon="search"
            title="No results"
            body="Nothing matched. Try fewer words, other spellings, or remove a filter."
          />
        ) : (
          <ResultList
            serverId={serverId}
            results={results}
            channels={channels}
            highlight={highlight}
            jumpedId={jumpedId}
            onJump={(m) => {
              setJumped(m.id);
              goToMessage(serverId, m.channelId, m.id);
              // Narrow windows: the panel floats over the chat, so get out
              // of the way of the message (Enter in the box brings it back).
              if (window.matchMedia("(width < 64rem)").matches) close();
            }}
          />
        )}
        {status === "done" && results.length > 0 ? (
          <div ref={sentinelRef} className="flex h-10 items-center justify-center text-[12px] text-text-muted">
            {loadingMore ? "Loading more…" : hasMore ? "" : "That's everything."}
          </div>
        ) : null}
      </div>
    </aside>
  );
}

function Notice({ icon, children }: { icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="flex shrink-0 gap-2 border-b border-border-divider px-4 py-2.5 text-[12px] leading-snug text-text-muted">
      <span className="mt-px shrink-0">{icon}</span>
      <span className="min-w-0">{children}</span>
    </div>
  );
}

function Empty({
  icon,
  title,
  body,
  children,
}: {
  icon: "search" | "alert";
  title: string;
  body: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col items-center px-6 pt-16 text-center">
      <span className="flex h-12 w-12 items-center justify-center rounded-lg bg-bg-mid text-text-muted">
        <Icon name={icon} size={22} />
      </span>
      <div className="mt-3 text-[14px] font-semibold text-text-primary">{title}</div>
      <p className="mt-1 max-w-[260px] text-[13px] leading-snug text-text-muted">{body}</p>
      {children}
    </div>
  );
}

function Skeleton() {
  return (
    <div className="pt-3" aria-hidden>
      {[0.8, 0.55, 0.7, 0.45, 0.6].map((w, i) => (
        <div key={i} className="mb-2 flex gap-2.5 rounded-lg bg-bg-mid px-3 py-3">
          <div className="h-8 w-8 shrink-0 animate-pulse rounded-full bg-bg-lighter" />
          <div className="min-w-0 flex-1 space-y-2 pt-0.5">
            <div className="h-3 w-24 animate-pulse rounded-sm bg-bg-lighter" />
            <div className="h-3 animate-pulse rounded-sm bg-bg-lighter" style={{ width: `${w * 100}%` }} />
          </div>
        </div>
      ))}
    </div>
  );
}

function ResultList({
  serverId,
  results,
  channels,
  highlight,
  jumpedId,
  onJump,
}: {
  serverId: string;
  results: Message[];
  channels: ChannelInfo[];
  highlight: Highlighter | undefined;
  jumpedId: number;
  onJump: (m: Message) => void;
}) {
  const out: React.ReactNode[] = [];
  let prev = "";
  for (const m of results) {
    if (m.channelId !== prev) {
      prev = m.channelId;
      const ch = channels.find((c) => c.id === m.channelId);
      out.push(
        <button
          key={`ch-${m.id}`}
          type="button"
          onClick={() => goToMessage(serverId, m.channelId)}
          className="mb-1.5 mt-3 flex max-w-full items-center gap-1 rounded-sm px-1.5 text-[12px] font-semibold text-text-muted hover:text-text-primary"
        >
          <span>#</span>
          <span className="truncate">{ch?.name ?? "deleted-channel"}</span>
          {ch?.encrypted ? <LockGlyph size={11} /> : null}
        </button>,
      );
    }
    out.push(
      <SearchResult
        key={m.id}
        serverId={serverId}
        message={m}
        highlight={highlight}
        jumped={m.id === jumpedId}
        onJump={onJump}
      />,
    );
  }
  return <>{out}</>;
}

const SearchResult = memo(function SearchResult({
  serverId,
  message,
  highlight,
  jumped,
  onJump,
}: {
  serverId: string;
  message: Message;
  highlight: Highlighter | undefined;
  jumped: boolean;
  onJump: (m: Message) => void;
}) {
  const name = useDisplayName(serverId, message.senderUid ?? 0, message.sender);
  const replyName = useDisplayName(serverId, message.replyToSenderUid ?? 0, message.replyToSender ?? "");
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={(e) => {
        // Links, emoji (span role=button) and buttons inside do their own thing.
        const hit = (e.target as HTMLElement).closest("a, button, [role=button]");
        if (hit && hit !== e.currentTarget) return;
        onJump(message);
      }}
      onKeyDown={(e) => {
        if (e.key === "Enter" && e.target === e.currentTarget) onJump(message);
      }}
      className={`group relative mb-1.5 cursor-pointer rounded-lg border px-3 py-2.5 outline-none focus-visible:shadow-ring ${
        jumped ? "border-accent/50 bg-bg-light" : "border-transparent bg-bg-mid hover:bg-bg-light"
      }`}
    >
      {message.replyTo ? (
        <div className="mb-1 flex min-w-0 items-center gap-1.5 pl-[42px] text-[12px] text-text-muted">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0">
            <polyline points="9 17 4 12 9 7" />
            <path d="M20 18v-2a4 4 0 0 0-4-4H4" />
          </svg>
          {message.replyToSender ? (
            <>
              <span className="shrink-0 font-semibold" style={{ color: userColor(message.replyToSenderUid, message.replyToSender ?? "") }}>
                {replyName}
              </span>
              <span className="min-w-0 truncate">
                <MessageText content={message.replyToContent ?? ""} preview emojiSize={14} />
              </span>
            </>
          ) : (
            <span className="italic">Original message was deleted</span>
          )}
        </div>
      ) : null}
      <div className="flex gap-2.5">
        <UserAvatar uid={message.senderUid ?? 0} name={message.sender} size={32} />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2 pr-12">
            <span
              className="min-w-0 truncate font-channel text-[14px] font-semibold"
              style={{ color: userColor(message.senderUid, message.sender) }}
            >
              {name}
            </span>
            <span className="shrink-0 font-meta text-[12px] tabular-nums text-text-muted">{when(message.timestamp)}</span>
          </div>
          {message.decryptError ? (
            <div className="mt-0.5 flex items-center gap-1.5 text-body italic leading-body text-text-muted">
              <LockGlyph size={12} />
              <span>This message couldn't be decrypted.</span>
            </div>
          ) : message.content ? (
            <div className="mt-0.5 line-clamp-6 whitespace-pre-wrap break-words text-body leading-body text-text-primary [overflow-wrap:anywhere]">
              <MessageText content={message.content} highlight={highlight} />
              {message.editedAt ? <span className="ml-1 text-meta text-text-muted">(edited)</span> : null}
            </div>
          ) : null}
          <ResultAttachments serverId={serverId} attachments={message.attachments} highlight={highlight} />
        </div>
      </div>
      <span className="pointer-events-none absolute right-2 top-2 hidden rounded-sm border border-border bg-bg-lighter px-2 py-0.5 text-[12px] font-semibold text-text-secondary group-hover:block group-focus-visible:block">
        Jump
      </span>
    </div>
  );
});

function thumbUrl(serverId: string, a: Attachment): string | null {
  if (a.mime === "image/gif") return buildAttachmentUrl(serverId, a);
  const size = a.thumbnailSizeBytes > 0 ? pickThumbnailSize(a.thumbnailSizesMask, 160) : null;
  if (size !== null) return buildAttachmentUrl(serverId, a, { thumb: true, size });
  return a.kind === "image" ? buildAttachmentUrl(serverId, a) : null;
}

function ResultAttachments({
  serverId,
  attachments,
  highlight,
}: {
  serverId: string;
  attachments: Attachment[];
  highlight: Highlighter | undefined;
}) {
  if (attachments.length === 0) return null;
  const media = attachments.filter((a) => (a.kind === "image" || a.kind === "video") && a.purgedAt === 0);
  const files = attachments.filter((a) => !media.includes(a));
  return (
    <>
      {media.length > 0 ? (
        <div className="mt-2 flex gap-1.5">
          {media.slice(0, 4).map((a, i) => {
            const src = thumbUrl(serverId, a);
            const more = i === 3 && media.length > 4 ? media.length - 4 : 0;
            return (
              <div key={a.id} className="relative h-16 w-16 shrink-0 overflow-hidden rounded-md bg-bg-darkest" title={a.filename}>
                {src ? (
                  <img src={src} alt="" loading="lazy" className="h-full w-full object-cover" />
                ) : (
                  <span className="flex h-full w-full items-center justify-center text-text-muted">
                    <Icon name={a.kind === "video" ? "film" : "image"} size={18} />
                  </span>
                )}
                {a.kind === "video" && src ? (
                  <span className="absolute bottom-1 left-1 flex h-4 w-4 items-center justify-center rounded-full bg-black/60 text-white">
                    <svg width="8" height="8" viewBox="0 0 24 24" fill="currentColor"><path d="M8 5v14l11-7z" /></svg>
                  </span>
                ) : null}
                {more > 0 ? (
                  <span className="absolute inset-0 flex items-center justify-center bg-black/55 text-[13px] font-semibold text-white">
                    +{more}
                  </span>
                ) : null}
              </div>
            );
          })}
        </div>
      ) : null}
      {files.map((a) => (
        <div
          key={a.id}
          className="mt-2 flex max-w-full items-center gap-2 rounded-md border border-border-divider bg-bg-dark px-2.5 py-1.5"
        >
          <Icon name={a.kind === "audio" ? "music" : a.kind === "video" ? "film" : a.kind === "image" ? "image" : "file"} size={15} className="text-text-muted" />
          <span className="min-w-0 flex-1 truncate text-[13px] font-medium text-text-primary">
            {highlight ? highlight(a.filename, `f${a.id}`) : a.filename}
          </span>
          <span className="shrink-0 text-[12px] tabular-nums text-text-muted">
            {a.purgedAt > 0 ? "removed" : formatFileSize(a.sizeBytes)}
          </span>
        </div>
      ))}
    </>
  );
}

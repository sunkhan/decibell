import { useLayoutEffect, useRef } from "react";
import type { ViewerImage } from "../../../stores/imageViewerStore";
import { useChatStore } from "../../../stores/chatStore";
import { useDisplayName } from "../../../hooks/useDisplayName";
import { UserAvatar } from "../../../components/UserAvatar";
import { formatFileSize } from "../attachmentHelpers";
import { LinkIcon, Spinner } from "./icons";

// The viewer's chrome. It sits on the theme (raised cards, theme text and
// accent, theme radii), not on fixed dark scrims: the stage itself is the
// theme's chrome colour, so a card reads the same over the stage and over
// a zoomed picture.

export const CARD =
  "border border-border bg-bg-light/90 shadow-float backdrop-blur-md";

export const TOOL =
  "flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:pointer-events-none disabled:opacity-35";

export function Tool({
  title,
  onClick,
  disabled,
  children,
}: {
  title: string;
  onClick: () => void;
  disabled?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button type="button" title={title} aria-label={title} disabled={disabled} onClick={onClick} className={TOOL}>
      {children}
    </button>
  );
}

export function ToolDivider() {
  return <span aria-hidden className="mx-1 h-5 w-px shrink-0 bg-border" />;
}

const TIME_FMT = new Intl.DateTimeFormat(undefined, { hour: "2-digit", minute: "2-digit" });
const DATE_FMT = new Intl.DateTimeFormat();

/// The chat's own phrasing ("Yesterday, at 14:32"), with "Today" spelled
/// out: a bare time reads oddly once it's away from the message.
function postedAt(ts: string | undefined): string {
  if (!ts) return "";
  const epoch = parseInt(ts, 10);
  const date = isNaN(epoch) ? new Date(ts) : new Date(epoch * 1000);
  if (isNaN(date.getTime())) return "";
  const time = TIME_FMT.format(date);
  const now = new Date();
  if (date.toDateString() === now.toDateString()) return `Today, at ${time}`;
  const y = new Date(now);
  y.setDate(y.getDate() - 1);
  if (date.toDateString() === y.toDateString()) return `Yesterday, at ${time}`;
  return `${DATE_FMT.format(date)}, at ${time}`;
}

function hostOf(url: string): string {
  try {
    return new URL(url).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/// Who posted it, when, where; then the file. Link images name the site.
export function ViewerHeader({
  item,
  loading,
  failed,
  onRetry,
}: {
  item: ViewerImage;
  loading: boolean;
  failed: boolean;
  onRetry: () => void;
}) {
  const channelName = useChatStore((s) =>
    item.serverId && item.channelId
      ? s.channelsByServer[item.serverId]?.find((c) => c.id === item.channelId)?.name
      : undefined,
  );
  const uid = item.senderUid ?? 0;
  const name = useDisplayName(item.serverId, uid, item.sender ?? "");
  const hasSender = item.sender !== undefined;
  const dims = item.width > 0 && item.height > 0 ? `${item.width} × ${item.height}` : null;
  const size = item.sizeBytes ? formatFileSize(item.sizeBytes) : null;

  return (
    <div className="flex min-w-0 items-center gap-3">
      {hasSender ? (
        <UserAvatar uid={uid} name={name} size={36} />
      ) : (
        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-bg-lighter text-text-secondary">
          <LinkIcon size={16} />
        </span>
      )}
      <div className="min-w-0">
        <div className="flex min-w-0 items-baseline gap-2">
          <span className="truncate text-[14px] font-semibold text-text-primary">
            {hasSender ? name : hostOf(item.pageUrl ?? item.url)}
          </span>
          {hasSender && (
            <span className="shrink-0 font-meta text-[12px] tabular-nums text-text-muted">
              {postedAt(item.timestamp)}
              {channelName && <span className="font-channel"> · #{channelName}</span>}
            </span>
          )}
        </div>
        <div className="flex min-w-0 items-center gap-1.5 font-meta text-[12px] text-text-muted">
          <span className="truncate text-text-secondary">{item.filename}</span>
          {(dims || size) && (
            <span className="shrink-0 tabular-nums">
              {[dims, size].filter(Boolean).join(" · ")}
            </span>
          )}
          {loading && !failed && (
            <span className="flex shrink-0 items-center gap-1.5 text-text-muted">
              <span aria-hidden>·</span>
              <Spinner />
              Loading original
            </span>
          )}
          {failed && (
            <span className="flex shrink-0 items-center gap-1.5 text-error">
              <span aria-hidden className="text-text-muted">·</span>
              Couldn't load the original
              <button type="button" onClick={onRetry} className="font-semibold text-accent hover:underline">
                Retry
              </button>
            </span>
          )}
        </div>
      </div>
    </div>
  );
}

/// The message's images, the current one ringed in the accent.
export function Filmstrip({
  items,
  index,
  onPick,
  maxWidth,
}: {
  items: ViewerImage[];
  index: number;
  onPick: (i: number) => void;
  maxWidth: number;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const box = scroller.current;
    const el = box?.querySelector<HTMLElement>(`[data-strip-index="${index}"]`);
    if (!box || !el) return;
    box.scrollTo({ left: el.offsetLeft - (box.clientWidth - el.offsetWidth) / 2, behavior: "smooth" });
  }, [index]);

  return (
    <div className={`flex items-center gap-1 rounded-lg p-1.5 ${CARD}`} style={{ maxWidth }}>
      <div ref={scroller} className="flex min-w-0 items-center gap-1.5 overflow-x-auto p-1 [scrollbar-width:none]">
        {items.map((it, i) => {
          const current = i === index;
          return (
            <button
              key={it.key}
              type="button"
              data-strip-index={i}
              onClick={() => onPick(i)}
              title={it.filename}
              aria-label={`${it.filename}, ${i + 1} of ${items.length}`}
              aria-current={current || undefined}
              className={`h-11 w-11 shrink-0 overflow-hidden rounded-md bg-bg-lighter transition-[opacity,box-shadow] duration-150 ${
                current
                  ? "opacity-100 ring-2 ring-accent ring-offset-2 ring-offset-bg-light"
                  : "opacity-55 hover:opacity-100"
              }`}
            >
              <img src={it.thumbUrl ?? it.previewUrl ?? it.url} alt="" draggable={false} decoding="async" className="h-full w-full object-cover" />
            </button>
          );
        })}
      </div>
      <span className="shrink-0 px-2 font-meta text-[12px] tabular-nums text-text-muted">
        {index + 1} / {items.length}
      </span>
    </div>
  );
}

/// Where you are in a zoomed picture. Drag (or click) in it to move.
export function Minimap({
  src,
  rot,
  shownSize,
  view,
  onMove,
}: {
  src: string;
  rot: number;
  /// The picture's displayed aspect (after rotation).
  shownSize: { w: number; h: number };
  /// The visible part, as fractions of the displayed picture.
  view: { u0: number; v0: number; u1: number; v1: number };
  onMove: (u: number, v: number) => void;
}) {
  const MAX_W = 176;
  const MAX_H = 124;
  const k = Math.min(MAX_W / shownSize.w, MAX_H / shownSize.h);
  const w = Math.round(shownSize.w * k);
  const h = Math.round(shownSize.h * k);
  const quarter = Math.abs(rot) % 180 === 90;
  const move = (e: React.PointerEvent<HTMLDivElement>) => {
    const r = e.currentTarget.getBoundingClientRect();
    onMove((e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height);
  };
  return (
    <div className={`rounded-lg p-1.5 ${CARD}`}>
      <div
        className="relative cursor-grab overflow-hidden rounded-sm active:cursor-grabbing"
        style={{ width: w, height: h }}
        onPointerDown={(e) => {
          e.stopPropagation();
          e.currentTarget.setPointerCapture(e.pointerId);
          move(e);
        }}
        onPointerMove={(e) => {
          if (e.currentTarget.hasPointerCapture(e.pointerId)) move(e);
        }}
      >
        <img
          src={src}
          alt=""
          draggable={false}
          className="absolute left-1/2 top-1/2 max-w-none"
          style={{
            width: quarter ? h : w,
            height: quarter ? w : h,
            transform: `translate(-50%, -50%) rotate(${rot}deg)`,
          }}
        />
        <div
          className="absolute rounded-sm border-2 border-accent"
          style={{
            left: view.u0 * w,
            top: view.v0 * h,
            width: (view.u1 - view.u0) * w,
            height: (view.v1 - view.v0) * h,
            // Dims everything outside the visible part.
            boxShadow: "0 0 0 999px rgb(0 0 0 / 0.45)",
          }}
        />
      </div>
    </div>
  );
}

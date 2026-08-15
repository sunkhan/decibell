import { useEffect, useRef, useState } from "react";
import type { Attachment } from "../../types";
import {
  buildAttachmentUrl,
  formatDuration,
  formatFileSize,
  pickThumbnailSize,
} from "./attachmentHelpers";
import {
  reserveBox,
  maxImageWidth,
  gridRowCounts,
  GRID_GAP_PX,
  GRID_ROW_HEIGHT_PX,
  GRID_MAX_WIDTH_PX,
  GRID_MIN_WIDTH_PX,
} from "./attachmentSizing";
import { previewUrlFor } from "./attachmentPrefetch";
import { thumbHashToDataUrl } from "./thumbhash";
import { useImageViewerStore } from "../../stores/imageViewerStore";
import { useImageContextMenuStore } from "../../stores/imageContextMenuStore";
import { useActiveAudioStore } from "../../stores/activeAudioStore";
import { useActiveVideoStore } from "../../stores/activeVideoStore";
import { useVideoCacheVersionStore } from "../../stores/videoCacheVersionStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import {
  audioSeek,
  audioSetVolume,
  audioToggle,
  audioToggleMute,
} from "./audioController";
import {
  getCachedAudio,
  peekCachedAudio,
  cacheAudio,
} from "./audioPlaybackState";
import {
  cacheVideo,
  getCachedVideo,
} from "./videoPlaybackState";
import { toast } from "../../stores/toastStore";

interface Props {
  attachments: Attachment[];
  serverId: string | null;
}

/// Inline attachment renderer. Image grid uses native <img>; video and
/// audio go through Chromium's <video controls> / <audio controls>
/// which handle scrubbing, volume, fullscreen, and seek-to-keyframe
/// natively. No bespoke media controllers needed.
///
/// The persistent video/audio layer that survives channel switches
/// (PR-extra polish) isn't ported; for now players are bubble-local.
export default function AttachmentList({ attachments, serverId }: Props) {
  if (attachments.length === 0) return null;
  // Sort by `position` (server-assigned ordering field) so the grid
  // layout below matches send-time ordering rather than wire arrival
  // order. Tombstones too — keeps everything in the same order the
  // sender saw.
  const ordered = [...attachments].sort((a, b) => a.position - b.position);

  // Split into the visual grid (live image/video) vs. everything else
  // (audio + documents render as cards; tombstones get the strikethrough
  // treatment). The grid only kicks in for >=2 live media — single
  // images/videos keep their existing aspect-preserving layout.
  const gridable: Attachment[] = [];
  const remainder: Attachment[] = [];
  for (const a of ordered) {
    if (a.purgedAt === 0 && (a.kind === "image" || a.kind === "video")) {
      gridable.push(a);
    } else {
      remainder.push(a);
    }
  }
  const useGrid = gridable.length >= 2;

  return (
    <div className="mt-1 flex flex-col gap-1">
      {useGrid ? (
        <MediaGrid items={gridable} serverId={serverId} />
      ) : (
        gridable.map((a) => (
          <LiveAttachment key={a.id} attachment={a} serverId={serverId} />
        ))
      )}
      {remainder.map((a) =>
        a.purgedAt > 0 ? (
          <PurgedTombstone key={a.id} attachment={a} />
        ) : (
          <LiveAttachment key={a.id} attachment={a} serverId={serverId} />
        ),
      )}
    </div>
  );
}

function LiveAttachment({
  attachment,
  serverId,
  fillCell = false,
}: {
  attachment: Attachment;
  serverId: string | null;
  /// When true, render in grid-cell mode: fill the parent box, crop
  /// with object-cover, drop the standalone-card chrome (the grid
  /// container provides the rounded corners + gaps). When false the
  /// item reserves its sqrt-scaled aspect-ratio box and uses
  /// object-contain. Audio + documents always render as cards (the
  /// parameter is ignored for them).
  fillCell?: boolean;
}) {
  if (attachment.kind === "image") {
    return <ImageItem attachment={attachment} serverId={serverId} fillCell={fillCell} />;
  }
  if (attachment.kind === "video") {
    return <VideoItem attachment={attachment} serverId={serverId} fillCell={fillCell} />;
  }
  if (attachment.kind === "audio") {
    return <AudioItem attachment={attachment} serverId={serverId} />;
  }
  return <DocumentItem attachment={attachment} serverId={serverId} />;
}

function MediaGrid({
  items,
  serverId,
}: {
  items: Attachment[];
  serverId: string | null;
}) {
  // Container width tracks the chat viewport (sqrt-scaled max),
  // clamped to the grid's own min/max so cells stay readable on
  // narrow panels and don't sprawl on wide ones.
  const viewW = useChatStore((s) => s.chatViewSize?.width ?? 0);
  const cap = viewW > 0 ? maxImageWidth(viewW) : GRID_MAX_WIDTH_PX;
  const containerWidth = Math.min(GRID_MAX_WIDTH_PX, Math.max(GRID_MIN_WIDTH_PX, cap));

  const rows: Attachment[][] = [];
  let cursor = 0;
  for (const cols of gridRowCounts(items.length)) {
    rows.push(items.slice(cursor, cursor + cols));
    cursor += cols;
  }

  return (
    <div
      className="mt-2 flex flex-col overflow-hidden rounded-md border border-border-divider"
      style={{ width: containerWidth, gap: GRID_GAP_PX }}
    >
      {rows.map((row, ri) => (
        <div
          key={ri}
          className="flex"
          style={{ gap: GRID_GAP_PX, height: GRID_ROW_HEIGHT_PX }}
        >
          {row.map((att) => (
            <div key={att.id} className="min-w-0 flex-1 overflow-hidden">
              <LiveAttachment attachment={att} serverId={serverId} fillCell />
            </div>
          ))}
        </div>
      ))}
    </div>
  );
}

function ImageItem({
  attachment,
  serverId,
  fillCell = false,
}: {
  attachment: Attachment;
  serverId: string | null;
  fillCell?: boolean;
}) {
  const open = useImageViewerStore((s) => s.open);
  // Read live chat viewport so the sqrt-scaled cap reflects the
  // current panel size — narrow side panels render compact previews,
  // wide layouts get larger ones, smoothly. One subscription: the
  // stored object is only ever replaced by setChatViewSize, so its
  // reference is already stable across unrelated chatStore updates.
  const chatViewSize = useChatStore((s) => s.chatViewSize);

  // NOT an early return. buildAttachmentUrl yields null whenever the
  // server's attachment config hasn't landed yet (fresh connection,
  // reconnect) — and rendering nothing in that window collapsed the row
  // to zero height, then popped it back to full size once the config
  // arrived. That is a layout jump entirely separate from image
  // loading, and it also meant the placeholder had nothing to paint on.
  // The box is reserved from the attachment's own dimensions, so it can
  // hold its space and show the blur with no URL at all.
  const fullUrl = buildAttachmentUrl(serverId, attachment);

  const box = reserveBox(attachment, chatViewSize);

  // Same helper the prefetcher uses, so the URL warmed ahead of the
  // viewport is byte-for-byte the one requested here — a mismatch would
  // double the requests and still miss the cache.
  const previewSrc = previewUrlFor(attachment, serverId, chatViewSize) ?? fullUrl;

  // previewUrlFor falls back to the full-content URL when the server
  // has no thumbnail variant for this attachment; anything else is a
  // bounded (≤1280px) thumb whose decode is a few ms at most.
  const isThumbVariant = previewSrc !== null && previewSrc !== fullUrl;

  // Grid mode: parent owns the size (h-full w-full), cards round at
  // the grid container, cropped with object-cover to align with
  // adjacent cells. Standalone mode: reserve the sqrt-scaled box and
  // use object-contain to preserve the full image.
  // The ThumbHash blur sits on the wrapper's background, so it is
  // painted the instant the row mounts — before the <img> has bytes.
  // The image draws over it and the blur is simply never seen again;
  // no swap, no fade, nothing to time wrong. Falls back to the flat
  // surface fill when the attachment has no hash.
  const blurUrl = thumbHashToDataUrl(attachment.placeholder);
  const wrapperClass = fillCell
    ? "block h-full w-full cursor-pointer overflow-hidden bg-bg-secondary bg-cover bg-center"
    : "block cursor-pointer overflow-hidden rounded-lg border border-border bg-bg-secondary bg-cover bg-center";
  const wrapperStyle: React.CSSProperties = {
    ...(fillCell ? {} : { width: box.width, height: box.height }),
    ...(blurUrl ? { backgroundImage: `url(${blurUrl})` } : {}),
  };
  const imgClass = fillCell ? "h-full w-full object-cover" : "h-full w-full object-contain";

  return (
    <button
      type="button"
      onClick={() =>
        fullUrl &&
        open({
          url: fullUrl,
          filename: attachment.filename,
          width: attachment.width,
          height: attachment.height,
          serverId: serverId ?? undefined,
          attachmentId: Number(attachment.id),
          mime: attachment.mime,
        })
      }
      onContextMenu={(e) => {
        if (!serverId) return;
        e.preventDefault();
        useImageContextMenuStore.getState().show({
          x: e.clientX,
          y: e.clientY,
          serverId,
          attachmentId: Number(attachment.id),
          filename: attachment.filename,
          mime: attachment.mime,
          kind: "image",
        });
      }}
      className={wrapperClass}
      style={wrapperStyle}
    >
      {previewSrc && (
      <img
        src={previewSrc}
        alt={attachment.filename}
        className={imgClass}
        // Not loading="lazy": Virtuoso already only mounts rows at or
        // near the viewport, so lazy adds no saving and defers the
        // fetch until the row is *already visible* — the image then
        // pops in a beat after it scrolls in (see increaseViewportBy
        // on the Virtuoso instances).
        //
        // decoding: "sync" for thumbnail variants so the image is
        // presented atomically with its row — "async" guarantees the
        // row paints before the pixels, i.e. a visible pop on every
        // mount even when the bytes are cached. Thumbs are bounded
        // (≤1280px), and the prefetcher pre-decodes them, so the sync
        // cost is ~0 warm and a few ms cold. Full-size fallbacks keep
        // "async": a multi-MB original can take >100ms to decode and
        // must not block the scroll frame.
        decoding={isThumbVariant ? "sync" : "async"}
        draggable={false}
      />
      )}
    </button>
  );
}

function VideoItem({
  attachment,
  serverId,
  fillCell = false,
}: {
  attachment: Attachment;
  serverId: string | null;
  fillCell?: boolean;
}) {
  // The chat-side VideoItem is now just a *placeholder* that owns
  // the visual slot in the message bubble. The actual <video> element
  // lives in `PersistentVideoLayer` (mounted at app level) and is
  // overlaid on top of this placeholder via fixed positioning. That
  // architecture lets the video keep playing through Virtuoso row
  // unmounts (scroll-away).
  const channelId = useChatStore((s) => s.activeChannelId) ?? "";
  const placeholderRef = useRef<HTMLDivElement | null>(null);
  // Sqrt-scaled reserve box, same helper as ImageItem so a video and
  // image of the same dimensions get the same standalone box.
  const chatViewSize = useChatStore((s) => s.chatViewSize);
  const box = reserveBox(attachment, chatViewSize);

  // Am I the active video right now? If so, register my placeholder
  // element as the host so PersistentVideoLayer overlays its <video>
  // on top of me.
  const activeAttachmentId = useActiveVideoStore((s) => s.active?.attachmentId);
  const isActive = activeAttachmentId === attachment.id;
  useEffect(() => {
    if (!isActive) return;
    useActiveVideoStore.getState().setHostElement(placeholderRef.current);
    return () => {
      // Only clear if WE were the host (concurrent re-mounts of the
      // same id might race; checking identity prevents a wrong clear).
      if (useActiveVideoStore.getState().hostElement === placeholderRef.current) {
        useActiveVideoStore.getState().setHostElement(null);
      }
    };
  }, [isActive]);

  const onPlay = () => {
    if (!serverId) return;
    const url = buildAttachmentUrl(serverId, attachment);
    if (!url) return;
    if (!getCachedVideo(channelId, attachment.id)) {
      cacheVideo(channelId, {
        attachmentId: attachment.id,
        lastTime: 0,
        wasPlaying: true,
      });
    }
    useActiveVideoStore.getState().setActive({
      attachmentId: attachment.id,
      serverId,
      channelId,
      src: url,
      filename: attachment.filename,
      width: attachment.width || box.width,
      height: attachment.height || box.height,
    });
  };

  // Re-render when PersistentVideoLayer captures a poster frame for
  // *this* attachment. Scoped by id so one capture doesn't re-render
  // every video on screen.
  useVideoCacheVersionStore((s) => s.versions[String(attachment.id)] ?? 0);
  const liveCached = getCachedVideo(channelId, attachment.id);
  const livePoster = liveCached?.posterUrl ?? null;
  const lastTime = liveCached?.lastTime ?? 0;

  // Server-side thumbnail fallback when no live capture exists.
  // Pick a thumb size proportional to the rendered box × DPR — in
  // grid mode the box dims aren't authoritative (cell width comes
  // from the parent), so the sqrt-derived box is a reasonable proxy.
  const targetPx = Math.round(
    Math.max(box.width, box.height) * (window.devicePixelRatio || 1),
  );
  const thumbSize = pickThumbnailSize(attachment.thumbnailSizesMask, targetPx);
  const serverThumb =
    !livePoster && thumbSize !== null && attachment.thumbnailSizeBytes > 0
      ? buildAttachmentUrl(serverId, attachment, { thumb: true, size: thumbSize })
      : null;
  const posterUrl = livePoster ?? serverThumb;
  // Same placeholder treatment as ImageItem — a video's poster is
  // fetched exactly like an image and pops in the same way. Painted
  // under the poster so it's only ever seen before one exists.
  const blurUrl = thumbHashToDataUrl(attachment.placeholder);

  const fmtTime = (s: number) => {
    if (!isFinite(s) || s <= 0) return null;
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${sec.toString().padStart(2, "0")}`;
  };
  const resumeStamp = fmtTime(lastTime);

  // Grid mode: cell parent owns the size; drop the standalone-card
  // chrome (the grid container provides rounded corners + gaps).
  // Standalone mode: reserve the sqrt-scaled box and round here.
  // PersistentVideoLayer reads the placeholderRef's bounding rect via
  // ResizeObserver, so it picks up either box correctly.
  const onContextMenu = (e: React.MouseEvent) => {
    if (!serverId) return;
    e.preventDefault();
    useImageContextMenuStore.getState().show({
      x: e.clientX,
      y: e.clientY,
      serverId,
      attachmentId: Number(attachment.id),
      filename: attachment.filename,
      mime: attachment.mime,
      kind: "video",
    });
  };

  const metaRight = resumeStamp
    ? `Paused at ${resumeStamp}`
    : formatFileSize(attachment.sizeBytes);

  // The poster/play surface. In both modes this is what
  // PersistentVideoLayer overlays its <video> on, so it is always the
  // element `placeholderRef` points at — never a card wrapper that
  // includes the meta bar, or the video would cover the bar.
  const surface = (
    <button
      onClick={onPlay}
      disabled={isActive}
      title={attachment.filename}
      className="group relative flex h-full w-full cursor-pointer items-center justify-center bg-black bg-cover bg-center disabled:cursor-default"
      style={
        posterUrl
          ? { backgroundImage: `url(${posterUrl})` }
          : blurUrl
            ? { backgroundImage: `url(${blurUrl})` }
            : undefined
      }
    >
      {posterUrl && (
        <div className="pointer-events-none absolute inset-0 bg-black/30" />
      )}
      {!isActive && (
        <div className="relative flex h-12 w-12 items-center justify-center rounded-lg bg-accent transition-all group-hover:scale-110 group-hover:bg-accent-hover">
          <svg className="h-7 w-7 text-on-accent" viewBox="0 0 24 24" fill="currentColor">
            <path d="M8 5v14l11-7z" />
          </svg>
        </div>
      )}
      {/* Grid cells are too tight to give the filename its own row, so
          they keep the scrim treatment. Standalone videos get the
          solid bar below instead. */}
      {fillCell && (
        <div className="pointer-events-none absolute inset-x-0 bottom-0 flex items-center justify-between gap-2 bg-gradient-to-t from-black/85 via-black/50 to-transparent px-3.5 pb-2.5 pt-10 text-[11px]">
          <span className="min-w-0 truncate text-text-secondary">{attachment.filename}</span>
          <span className="shrink-0 tabular-nums text-text-muted">{metaRight}</span>
        </div>
      )}
    </button>
  );

  if (fillCell) {
    return (
      <div
        ref={placeholderRef}
        className="h-full w-full overflow-hidden bg-black"
        onContextMenu={onContextMenu}
      >
        {surface}
      </div>
    );
  }

  // Standalone card: video on top, opaque meta bar underneath. The
  // card must NOT clip its overflow — PersistentVideoLayer walks up
  // from the host's parent looking for the scroll container, and an
  // overflow-hidden card would masquerade as one, clipping the video
  // to itself instead of to the chat viewport.
  return (
    <div
      className="mt-2 rounded-lg border border-border bg-bg-light shadow-raised"
      style={{ width: box.width }}
      onContextMenu={onContextMenu}
    >
      {/* w-full, not width: box.width — the card is border-box, so its
          content area is box.width minus the 1px border on each side.
          Pinning the media to box.width pushed it 2px past the card's
          right edge and left the meta bar visibly short of the video
          it belongs to. The card keeps the full box.width footprint,
          same as a standalone image. */}
      <div
        ref={placeholderRef}
        className="w-full overflow-hidden rounded-t-lg bg-black"
        style={{ height: box.height }}
      >
        {surface}
      </div>
      <div className="flex items-center justify-between gap-3 px-[11px] py-2 font-mono text-[10.5px]">
        <span className="min-w-0 truncate text-text-muted" title={attachment.filename}>
          {attachment.filename}
        </span>
        <span className="video-meta-size shrink-0 tabular-nums text-text-muted">
          {metaRight}
        </span>
      </div>
    </div>
  );
}

function AudioIcon({ size = 18 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <line x1="4" y1="9" x2="4" y2="15" />
      <line x1="8" y1="6" x2="8" y2="18" />
      <line x1="12" y1="3" x2="12" y2="21" />
      <line x1="16" y1="6" x2="16" y2="18" />
      <line x1="20" y1="9" x2="20" y2="15" />
    </svg>
  );
}

function AudioItem({
  attachment,
  serverId,
}: {
  attachment: Attachment;
  serverId: string | null;
}) {
  // Presentational only: the actual <audio> element lives in
  // PersistentAudioLayer at app level so playback survives Virtuoso
  // row unmounts. This widget reads playback state from
  // useActiveAudioStore and dispatches commands via audioController.
  const channelId = useChatStore((s) => s.activeChannelId) ?? "";
  const activeAttachmentId = useActiveAudioStore((s) => s.active?.attachmentId);
  const isActive = activeAttachmentId === attachment.id;
  // Subscribe to playback fields only when active so inactive rows
  // don't re-render on every timeupdate tick of someone else's audio.
  const playing = useActiveAudioStore((s) => (isActive ? s.playing : false));
  const liveTime = useActiveAudioStore((s) => (isActive ? s.time : 0));
  const duration = useActiveAudioStore((s) => (isActive ? s.duration : 0));
  // For inactive rows, fall back to the cached lastTime so the
  // paused-at position stays visually pinned on the progress bar
  // when the user starts a different audio.
  const cachedLastTime = !isActive
    ? peekCachedAudio(channelId, attachment.id)?.lastTime ?? 0
    : 0;
  const time = isActive ? liveTime : cachedLastTime;
  // Volume + mute live in uiStore so any audio row's slider / mute /
  // wheel can adjust the persisted level — even before the user has
  // played anything. The persistent layer seeds new elements from
  // these values and mirrors element changes back here.
  const volume = useUiStore((s) => s.mediaAudioVolume);
  const muted = useUiStore((s) => s.mediaAudioMuted);
  const [error, setError] = useState<string | null>(null);
  const wrapperRef = useRef<HTMLDivElement | null>(null);

  const fmt = (s: number) => {
    if (!isFinite(s) || s < 0) return "0:00";
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${sec.toString().padStart(2, "0")}`;
  };
  // Use the live duration from the playing element when available,
  // otherwise fall back to the upload-time durationMs the server
  // shipped with the attachment metadata. Lets receivers see "3:45"
  // before they ever click play.
  const seededDuration = attachment.durationMs > 0 ? attachment.durationMs / 1000 : 0;
  const displayDuration = duration > 0 ? duration : seededDuration;
  const progress = displayDuration > 0 ? (time / displayDuration) * 100 : 0;

  const onPlay = () => {
    if (!serverId) return;
    if (isActive) {
      audioToggle();
      return;
    }
    const url = buildAttachmentUrl(serverId, attachment);
    if (!url) {
      setError("Attachment unavailable");
      return;
    }
    // Seed the cache entry so loadedmetadata in PersistentAudioLayer
    // can resume from any cached lastTime. New plays start at 0;
    // re-plays inherit whatever updateCachedAudioState wrote.
    const cached = getCachedAudio(channelId, attachment.id);
    if (!cached) {
      cacheAudio(channelId, { attachmentId: attachment.id, lastTime: 0 });
    }
    setError(null);
    useActiveAudioStore.getState().setActive({
      attachmentId: attachment.id,
      serverId,
      channelId,
      src: url,
      filename: attachment.filename,
    });
  };

  const onScrubDown = (e: React.MouseEvent<HTMLDivElement>) => {
    if (!isActive || !duration) return;
    const seek = (cx: number, track: HTMLElement) => {
      const r = track.getBoundingClientRect();
      const ratio = Math.max(0, Math.min(1, (cx - r.left) / r.width));
      audioSeek(ratio * duration);
    };
    seek(e.clientX, e.currentTarget);
    const track = e.currentTarget;
    const onMove = (ev: MouseEvent) => seek(ev.clientX, track);
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const onDownload = async () => {
    if (!serverId) return;
    const dest = await window.decibell.dialog.save({
      defaultPath: attachment.filename || "audio",
    });
    if (!dest) return;
    try {
      const res = await window.decibell.netFetch("", {
        method: "GET",
        attachmentTarget: {
          serverId,
          path: `/attachments/${attachment.id}`,
        },
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      await window.decibell.fs.writeFile(dest, new Uint8Array(res.body));
      toast.success("Audio saved", attachment.filename);
    } catch (err) {
      toast.error("Save failed", String(err));
    }
  };

  const onVolumeMouseDown = (e: React.MouseEvent<HTMLDivElement>) => {
    const seek = (cx: number, track: HTMLElement) => {
      const r = track.getBoundingClientRect();
      const ratio = Math.max(0, Math.min(1, (cx - r.left) / r.width));
      audioSetVolume(ratio);
    };
    seek(e.clientX, e.currentTarget);
    const track = e.currentTarget;
    const onMove = (ev: MouseEvent) => seek(ev.clientX, track);
    const onUp = () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  // Scroll-wheel volume — attached imperatively with passive:false
  // so preventDefault actually blocks the surrounding chat scroll.
  // React's synthetic onWheel ends up passive in this nested DOM,
  // so a JSX onWheel handler can change volume but can't suppress
  // the chat scroll.
  const volumeBarRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = volumeBarRef.current;
    if (!el) return;
    const handler = (e: WheelEvent) => {
      e.preventDefault();
      const step = e.deltaY < 0 ? 0.05 : -0.05;
      audioSetVolume((muted ? 0 : volume) + step);
    };
    el.addEventListener("wheel", handler, { passive: false });
    return () => el.removeEventListener("wheel", handler);
  }, [muted, volume]);

  // Focus + Space toggles play.
  const onWrapperKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const target = e.target as HTMLElement | null;
    const tag = target?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA" || target?.isContentEditable) return;
    if (e.key === " " || e.code === "Space") {
      e.preventDefault();
      onPlay();
    }
  };

  const volumeDisplay = muted ? 0 : volume;

  return (
    <div
      ref={wrapperRef}
      tabIndex={-1}
      onMouseDownCapture={() => wrapperRef.current?.focus()}
      onKeyDown={onWrapperKeyDown}
      className="mt-2 flex w-full max-w-[400px] flex-col gap-2.5 rounded-lg border border-border bg-bg-light p-3 shadow-float outline-none"
    >
      {/* Top row: kind icon + filename + size + small download button */}
      <div className="flex items-center gap-3">
        <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-accent-soft text-accent-bright">
          <AudioIcon size={18} />
        </div>
        <div className="min-w-0 flex-1">
          <div
            className="truncate text-sender font-medium text-text-primary"
            title={attachment.filename}
          >
            {attachment.filename}
          </div>
          <div className="font-mono text-[10.5px] text-text-muted">
            {attachment.durationMs > 0 && `${formatDuration(attachment.durationMs)} · `}
            {formatFileSize(attachment.sizeBytes)}
          </div>
        </div>
        <button
          onClick={onDownload}
          title="Download"
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
            <polyline points="7 10 12 15 17 10" />
            <line x1="12" y1="15" x2="12" y2="3" />
          </svg>
        </button>
      </div>

      {/* Bottom row: play/pause + scrub + time + volume */}
      <div className="flex items-center gap-2.5">
        <button
          onClick={onPlay}
          disabled={!!error}
          className="flex h-8 w-10 shrink-0 items-center justify-center rounded-md bg-accent text-on-accent transition-all hover:bg-accent-hover hover:scale-105 active:scale-95 disabled:cursor-not-allowed disabled:opacity-50"
          title={playing ? "Pause" : "Play"}
        >
          {error ? (
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          ) : playing ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
              <rect x="6" y="5" width="4" height="14" rx="1" />
              <rect x="14" y="5" width="4" height="14" rx="1" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor">
              <path d="M8 5v14l11-7z" />
            </svg>
          )}
        </button>

        <div
          onMouseDown={duration ? onScrubDown : undefined}
          className={`group relative flex h-3 flex-1 items-center ${duration ? "cursor-pointer" : "cursor-default"}`}
        >
          <div className="pointer-events-none absolute inset-x-0 h-[4px] rounded-full bg-bg-lighter" />
          <div
            className="pointer-events-none absolute h-[4px] rounded-full bg-accent"
            style={{ width: `${progress}%` }}
          />
          <div
            className="pointer-events-none absolute h-3 w-3 -translate-x-1/2 rounded-full border-2 border-accent bg-bg-light opacity-0 shadow-[0_0_6px_color-mix(in_srgb,var(--color-accent)_30%,transparent)] transition-opacity group-hover:opacity-100"
            style={{ left: `${progress}%` }}
          />
        </div>

        <span className="shrink-0 select-none font-mono text-[10px] tabular-nums text-text-muted">
          {fmt(time)} / {fmt(displayDuration)}
        </span>

        {/* Volume cluster — speaker toggle + thin slider. Wheel over
            the slider raises/lowers volume. Always interactive, even
            before the first play, so users can pre-tune the level. */}
        <button
          onClick={() => audioToggleMute()}
          title={muted ? "Unmute" : "Mute"}
          className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
        >
          {muted || volume === 0 ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
              <line x1="23" y1="9" x2="17" y2="15" />
              <line x1="17" y1="9" x2="23" y2="15" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
              <path d="M19.07 4.93a10 10 0 010 14.14M15.54 8.46a5 5 0 010 7.07" />
            </svg>
          )}
        </button>
        <div
          ref={volumeBarRef}
          onMouseDown={onVolumeMouseDown}
          title="Volume"
          className="group relative flex h-3 w-16 shrink-0 cursor-pointer items-center"
        >
          <div className="pointer-events-none absolute inset-x-0 h-[3px] rounded-full bg-bg-lighter" />
          <div
            className="pointer-events-none absolute h-[3px] rounded-full bg-accent"
            style={{ width: `${volumeDisplay * 100}%` }}
          />
          <div
            className="pointer-events-none absolute h-2.5 w-2.5 -translate-x-1/2 rounded-full border-2 border-accent bg-bg-light opacity-0 transition-opacity group-hover:opacity-100"
            style={{ left: `${volumeDisplay * 100}%` }}
          />
        </div>
      </div>

      {error && (
        <p className="text-[11px] text-error">Failed to load: {error}</p>
      )}
    </div>
  );
}

function DocumentItem({
  attachment,
  serverId,
}: {
  attachment: Attachment;
  serverId: string | null;
}) {
  const url = buildAttachmentUrl(serverId, attachment);

  // The <a download="..."> attribute is silently ignored when href
  // points to a different origin (Chromium spec). Our attachments are
  // served from the loopback media server (http://127.0.0.1:PORT) while
  // the renderer lives at file:// (prod) or http://localhost:5173 (dev),
  // so the browser falls back to the last path segment of the URL —
  // which is the numeric attachment id. To save under the real filename
  // we route through the same dialog.save + netFetch + fs.writeFile
  // path that the audio attachment download uses.
  const onDownload = async () => {
    if (!serverId) return;
    const dest = await window.decibell.dialog.save({
      defaultPath: attachment.filename || "attachment",
    });
    if (!dest) return;
    try {
      const res = await window.decibell.netFetch("", {
        method: "GET",
        attachmentTarget: {
          serverId,
          path: `/attachments/${attachment.id}`,
        },
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      await window.decibell.fs.writeFile(dest, new Uint8Array(res.body));
      toast.success("File saved", attachment.filename);
    } catch (err) {
      toast.error("Save failed", String(err));
    }
  };

  return (
    <button
      onClick={onDownload}
      disabled={!url}
      title={`Download ${attachment.filename}`}
      className={`flex max-w-[400px] items-center gap-3 rounded-lg border border-border bg-bg-secondary p-3 text-left transition-colors hover:bg-bg-light ${
        url ? "" : "pointer-events-none opacity-50"
      }`}
    >
      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-md bg-bg-light text-text-muted">
        <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate font-channel text-sm font-medium text-text-primary">
          {attachment.filename}
        </div>
        <div className="text-[11px] text-text-muted">
          {formatFileSize(attachment.sizeBytes)}
        </div>
      </div>
    </button>
  );
}

function PurgedTombstone({ attachment }: { attachment: Attachment }) {
  return (
    <div className="flex max-w-[400px] items-center gap-2 rounded-lg border border-dashed border-border bg-bg-secondary/50 p-3 text-[12px] italic text-text-muted">
      <span className="truncate">
        {attachment.filename || "Attachment"} (deleted by retention policy)
      </span>
    </div>
  );
}

import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { pickSavePath } from "./filePicker";
import type { Attachment } from "../../types";
import { getCachedImage, getOrFetchImage } from "./imageCache";
import { useChatStore } from "../../stores/chatStore";
import { useImageViewerStore } from "../../stores/imageViewerStore";
import { useImageContextMenuStore } from "../../stores/imageContextMenuStore";
import { useActiveVideoStore } from "../../stores/activeVideoStore";
import { useVideoCacheVersionStore } from "../../stores/videoCacheVersionStore";
import { cacheVideo, getCachedVideo } from "./tempVideoCache";
import { fetchThumbnail, getCachedThumbnail } from "./attachmentThumbnailCache";

// ---- shared helpers --------------------------------------------------------

function formatDurationBetween(from: number, to: number): string {
  const secs = Math.max(0, to - from);
  const days = secs / 86400;
  if (days < 2 / 24) {
    const minutes = Math.max(1, Math.round(secs / 60));
    return `${minutes} minute${minutes === 1 ? "" : "s"}`;
  }
  if (days < 2) {
    const hours = Math.max(1, Math.round(secs / 3600));
    return `${hours} hour${hours === 1 ? "" : "s"}`;
  }
  if (days < 14) {
    return `${Math.floor(days)} day${Math.floor(days) === 1 ? "" : "s"}`;
  }
  if (days < 60) {
    const weeks = Math.floor(days / 7);
    return `${weeks} week${weeks === 1 ? "" : "s"}`;
  }
  if (days < 365) {
    const months = Math.floor(days / 30);
    return `${months} month${months === 1 ? "" : "s"}`;
  }
  const years = Math.floor(days / 365);
  return `${years} year${years === 1 ? "" : "s"}`;
}

function formatBytes(n: number): string {
  if (n <= 0) return "";
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v.toFixed(v < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}

function kindLabel(kind: Attachment["kind"]): string {
  switch (kind) {
    case "image": return "Image";
    case "video": return "Video";
    case "document": return "File";
    case "audio": return "Audio";
    default: return "Attachment";
  }
}

// Max file size (in bytes) to auto-inline as a data URL for preview.
// 20 MB is generous for images while keeping memory-per-message bounded.
const INLINE_PREVIEW_CAP = 20 * 1024 * 1024;

// ---- components -----------------------------------------------------------

function Tombstone({ attachment }: { attachment: Attachment }) {
  const duration = attachment.createdAt > 0 && attachment.purgedAt > attachment.createdAt
    ? formatDurationBetween(attachment.createdAt, attachment.purgedAt)
    : null;
  return (
    <div className="mt-2 flex items-center gap-2.5 rounded-[10px] border border-border bg-bg-light/60 px-3 py-2">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-text-muted/15 text-text-muted">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="17 8 12 3 7 8" />
          <line x1="12" y1="3" x2="12" y2="15" />
          <line x1="4" y1="4" x2="20" y2="20" />
        </svg>
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium text-text-muted line-through">
          {attachment.filename || kindLabel(attachment.kind)}
        </div>
        <div className="truncate text-[11px] text-text-faint">
          {duration
            ? `This attachment was cleaned up by the server after ${duration}.`
            : "This attachment was cleaned up by the server."}
        </div>
      </div>
    </div>
  );
}

// Used before the chat view reports its first measurement, and as the
// fallback caps when an attachment has no intrinsic dimensions.
const PREVIEW_FALLBACK_MAX_W = 400;
const PREVIEW_FALLBACK_MAX_H = 360;
const PREVIEW_FALLBACK_W = 260;
const PREVIEW_FALLBACK_H = 180;

// Image dimensions scale as the **square root** of chat-view dimensions.
// A linear "75% of chat width" looks great on small/medium chats but
// produces unbearably large images in fullscreen layouts; sqrt grows
// monotonically yet sub-linearly, so the curve flattens out as chats
// widen — without resorting to a hard cap. The coefficients are tuned
// so a typical small panel (~500 × 600) renders images in the same
// neighbourhood (~400 × 390) the previous linear scaling produced.
// Floor reserves keep avatar + bubble padding intact on narrow panels.
const HORIZONTAL_BUBBLE_RESERVE_MIN = 80;
const VERTICAL_BUBBLE_RESERVE_MIN = 60;
const IMAGE_WIDTH_SQRT_COEFF = 18;
const IMAGE_HEIGHT_SQRT_COEFF = 16;
function maxImageWidth(viewWidth: number): number {
  return Math.max(
    120,
    Math.min(
      IMAGE_WIDTH_SQRT_COEFF * Math.sqrt(viewWidth),
      viewWidth - HORIZONTAL_BUBBLE_RESERVE_MIN,
    ),
  );
}
function maxImageHeight(viewHeight: number): number {
  return Math.max(
    120,
    Math.min(
      IMAGE_HEIGHT_SQRT_COEFF * Math.sqrt(viewHeight),
      viewHeight - VERTICAL_BUBBLE_RESERVE_MIN,
    ),
  );
}
interface ChatViewSize {
  width: number;
  height: number;
}

/** Compute the pixel box we'll reserve for this image. Scaled down so the
 *  image fits within the sqrt-derived caps, aspect ratio preserved. Small
 *  images render at natural size — we never upscale. */
function reserveBox(
  attachment: Attachment,
  viewSize: ChatViewSize | null,
): { width: number; height: number; known: boolean } {
  const w = attachment.width;
  const h = attachment.height;
  if (w <= 0 || h <= 0) {
    return { width: PREVIEW_FALLBACK_W, height: PREVIEW_FALLBACK_H, known: false };
  }
  const maxW = viewSize ? maxImageWidth(viewSize.width) : PREVIEW_FALLBACK_MAX_W;
  const maxH = viewSize ? maxImageHeight(viewSize.height) : PREVIEW_FALLBACK_MAX_H;
  const scale = Math.min(1, maxW / w, maxH / h);
  return {
    width: Math.max(1, Math.round(w * scale)),
    height: Math.max(1, Math.round(h * scale)),
    known: true,
  };
}

function ImagePlaceholderIcon() {
  return (
    <svg
      width="28"
      height="28"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      className="text-text-muted/70"
    >
      <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
      <circle cx="8.5" cy="8.5" r="1.5" />
      <polyline points="21 15 16 10 5 21" />
    </svg>
  );
}

function ImagePreview({
  attachment,
  serverId,
  fillCell = false,
}: {
  attachment: Attachment;
  serverId: string | null;
  // When true, fill the parent's box (used by the multi-image grid) and
  // crop with object-cover so adjacent cells line up to a clean grid.
  // When false (default, single-attachment case), reserve a sqrt-scaled
  // box and use object-contain to preserve the full image.
  fillCell?: boolean;
}) {
  // Inline preview prefers the server-side thumbnail (~30 KB JPEG) over
  // the original. Falls back to the full image only for legacy rows
  // uploaded before image thumbnails existed (thumbnailSizeBytes === 0).
  // The viewer always fetches the full bytes via openViewer below.
  const useServerThumb = attachment.thumbnailSizeBytes > 0;

  // Consult the right module-level cache first so a virtualized
  // unmount/remount during scroll doesn't re-fetch.
  const [url, setUrl] = useState<string | null>(() =>
    useServerThumb ? getCachedThumbnail(attachment.id) : getCachedImage(attachment.id),
  );
  const [error, setError] = useState<string | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const hasFetched = useRef(url !== null);

  // Open the full-screen viewer with every live image in the active
  // channel as the navigable list. We snapshot at click time so the
  // viewer doesn't churn when chatStore updates afterwards.
  const openViewer = () => {
    if (!serverId) return;
    const chat = useChatStore.getState();
    const channelId = chat.activeChannelId;
    if (!channelId) return;
    const messages = chat.messagesByChannel[channelId] ?? [];
    const siblings = messages
      .flatMap((m) => m.attachments)
      .filter((a) => a.kind === "image" && a.purgedAt === 0 && a.sizeBytes <= INLINE_PREVIEW_CAP);
    const startIdx = siblings.findIndex((a) => a.id === attachment.id);
    useImageViewerStore.getState().show(
      siblings.length > 0 ? siblings : [attachment],
      serverId,
      Math.max(0, startIdx),
    );
  };

  // Lazy-load: fetch only once the card scrolls into view. For thumbnail
  // path the bytes are tiny (~30 KB) so we don't gate on size. For the
  // legacy full-image fallback we still respect INLINE_PREVIEW_CAP.
  // Both paths dedup concurrent fetches in their respective caches, so
  // a rapid scroll over the same row doesn't fan out duplicate IPCs.
  useEffect(() => {
    if (!serverId) return;
    if (hasFetched.current) return;
    if (!useServerThumb && attachment.sizeBytes > INLINE_PREVIEW_CAP) return;
    const el = containerRef.current;
    if (!el) return;
    let cancelled = false;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        hasFetched.current = true;
        io.disconnect();
        const fetcher = useServerThumb
          ? fetchThumbnail(serverId, attachment.id)
          : getOrFetchImage(serverId, attachment.id, attachment.mime);
        fetcher
          .then((objectUrl) => {
            if (cancelled || !objectUrl) return;
            setUrl(objectUrl);
          })
          .catch((e) => {
            if (!cancelled) setError(String(e));
          });
      }
    }, { rootMargin: "200px 0px" });
    io.observe(el);
    return () => {
      cancelled = true;
      io.disconnect();
    };
  }, [serverId, attachment.id, attachment.sizeBytes, attachment.mime, useServerThumb]);

  const tooLargeForPreview = attachment.sizeBytes > INLINE_PREVIEW_CAP;
  // Subscribe to width and height as primitives so a chatStore set
  // that creates a new {width, height} object with identical values
  // doesn't re-render every mounted ImagePreview. zustand compares
  // primitives with Object.is, so unchanged dims = no re-render.
  const viewW = useChatStore((s) => s.chatViewSize?.width ?? 0);
  const viewH = useChatStore((s) => s.chatViewSize?.height ?? 0);
  const chatViewSize = viewW > 0 && viewH > 0 ? { width: viewW, height: viewH } : null;
  const box = reserveBox(attachment, chatViewSize);

  const buttonStyle: React.CSSProperties | undefined = fillCell
    ? undefined
    : { width: box.width, height: box.height };
  const buttonClass = fillCell
    ? "group relative block h-full w-full cursor-pointer overflow-hidden bg-bg-light transition-transform hover:scale-[1.005] disabled:cursor-default"
    : "group relative block cursor-pointer overflow-hidden rounded-[10px] border border-border-divider bg-bg-light transition-transform hover:scale-[1.005] disabled:cursor-default";
  const imgClass = fillCell
    ? "h-full w-full object-cover"
    : "h-full w-full object-contain";

  return (
    <div ref={containerRef} className={fillCell ? "h-full w-full" : "mt-2"}>
      <button
        onClick={() => url && openViewer()}
        onContextMenu={(e) => {
          if (!serverId) return;
          e.preventDefault();
          useImageContextMenuStore.getState().show({
            x: e.clientX,
            y: e.clientY,
            serverId,
            attachmentId: attachment.id,
            filename: attachment.filename,
          });
        }}
        className={buttonClass}
        style={buttonStyle}
        disabled={!url}
      >
        {url ? (
          <img
            src={url}
            alt={attachment.filename}
            className={imgClass}
            draggable={false}
            decoding="async"
            // Native lazy + intrinsic dimensions hint the engine to
            // decode at the box size rather than full source resolution
            // — a 12 MP phone photo decodes to ~48 MB of RGBA at source
            // res, ~150 KB at 200×200 px. With server thumbnails the
            // source is already small, but the hint keeps WebKit honest.
            loading="lazy"
            width={attachment.width > 0 ? attachment.width : undefined}
            height={attachment.height > 0 ? attachment.height : undefined}
          />
        ) : (
          <div className="flex h-full w-full flex-col items-center justify-center gap-1.5 bg-bg-dark/40 text-[11px] text-text-muted">
            <ImagePlaceholderIcon />
            <span className="px-2 text-center">
              {error
                ? `Failed to load: ${error}`
                : tooLargeForPreview
                  ? fillCell
                    ? formatBytes(attachment.sizeBytes)
                    : `${attachment.filename} • ${formatBytes(attachment.sizeBytes)} — download to view`
                  : box.known
                    ? ""
                    : attachment.filename}
            </span>
          </div>
        )}
        {url && !fillCell && (
          <div className="absolute bottom-0 left-0 right-0 bg-gradient-to-t from-black/60 to-transparent px-2 py-1 text-[10.5px] text-white opacity-0 transition-opacity group-hover:opacity-100">
            {attachment.filename}
            {attachment.sizeBytes ? ` • ${formatBytes(attachment.sizeBytes)}` : ""}
          </div>
        )}
      </button>
      {/* Only show the Download button when inline preview isn't happening:
          either the file is too big to inline or the fetch actually failed.
          In grid mode we suppress this since each cell is sized to fit the
          grid — a download button below would break the row alignment. */}
      {!fillCell && (tooLargeForPreview || error !== null) && (
        <DownloadButton attachment={attachment} serverId={serverId} />
      )}

    </div>
  );
}

function FileCard({ attachment, serverId, icon }: { attachment: Attachment; serverId: string | null; icon: React.ReactNode }) {
  const size = formatBytes(attachment.sizeBytes);
  return (
    <div className="mt-2 flex max-w-[360px] items-center gap-2.5 rounded-[10px] border border-border-divider bg-bg-light px-3 py-2">
      <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-accent-soft text-accent-bright">
        {icon}
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium text-text-primary">
          {attachment.filename}
        </div>
        <div className="truncate text-[11px] text-text-muted">
          {kindLabel(attachment.kind)}{size ? ` • ${size}` : ""}
        </div>
      </div>
      <DownloadButton attachment={attachment} serverId={serverId} inline />
    </div>
  );
}

interface DownloadProgressPayload {
  attachmentId: number;
  transferredBytes: number;
  totalBytes: number;
}

function DownloadButton({
  attachment,
  serverId,
  inline = false,
}: {
  attachment: Attachment;
  serverId: string | null;
  inline?: boolean;
}) {
  const [state, setState] = useState<"idle" | "running" | "done" | "error">("idle");
  const [error, setError] = useState<string | null>(null);
  const [transferred, setTransferred] = useState(0);
  const [total, setTotal] = useState(0);

  // Listen for download progress while a download is in flight. Each
  // DownloadButton subscribes only while its own download is running, so
  // there's no scaling cost across hundreds of historic attachments.
  useEffect(() => {
    if (state !== "running") return;
    let unlisten: (() => void) | null = null;
    listen<DownloadProgressPayload>(
      "attachment_download_progress",
      (event) => {
        if (event.payload.attachmentId !== attachment.id) return;
        setTransferred(event.payload.transferredBytes);
        setTotal(event.payload.totalBytes);
      },
    ).then((fn) => { unlisten = fn; });
    return () => { if (unlisten) unlisten(); };
  }, [state, attachment.id]);

  const onClick = async () => {
    if (!serverId) return;
    let destination: string | null = null;
    try {
      destination = await pickSavePath({
        title: "Save attachment",
        defaultName: attachment.filename || "download",
      });
    } catch (err) {
      setError(String(err));
      setState("error");
      return;
    }
    if (!destination) return;
    setTransferred(0);
    setTotal(0);
    setState("running");
    setError(null);
    try {
      await invoke("download_attachment", {
        req: {
          serverId,
          attachmentId: attachment.id,
          destinationPath: destination,
        },
      });
      setState("done");
    } catch (err) {
      setError(String(err));
      setState("error");
    }
  };

  const pct = total > 0 ? Math.min(100, (transferred / total) * 100) : 0;
  const label = state === "running"
      ? (total > 0 ? `Downloading ${pct.toFixed(0)}%` : "Downloading…")
    : state === "done" ? "Saved"
    : state === "error" ? "Retry"
    : "Download";

  if (inline) {
    return (
      <div className="flex shrink-0 flex-col items-end gap-1">
        <button
          onClick={onClick}
          title={error ?? label}
          disabled={state === "running"}
          className="rounded-md bg-accent-soft px-2.5 py-1.5 text-[11px] font-medium text-accent-bright transition-colors hover:bg-accent-mid disabled:opacity-60"
        >
          {label}
        </button>
        {state === "running" && total > 0 && (
          <div className="h-[3px] w-[80px] overflow-hidden rounded-full bg-bg-dark">
            <div
              className="h-full rounded-full bg-accent transition-[width] duration-150"
              style={{ width: `${pct}%` }}
            />
          </div>
        )}
      </div>
    );
  }
  return (
    <div className="mt-1.5">
      <button
        onClick={onClick}
        disabled={state === "running"}
        className="inline-flex items-center gap-1.5 rounded-md bg-accent-soft px-2.5 py-1.5 text-[11px] font-medium text-accent-bright transition-colors hover:bg-accent-mid disabled:opacity-60"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="7 10 12 15 17 10" />
          <line x1="12" y1="15" x2="12" y2="3" />
        </svg>
        {label}
      </button>
      {state === "running" && total > 0 && (
        <div className="mt-1.5 h-[3px] w-full max-w-[200px] overflow-hidden rounded-full bg-bg-dark">
          <div
            className="h-full rounded-full bg-accent transition-[width] duration-150"
            style={{ width: `${pct}%` }}
          />
        </div>
      )}
    </div>
  );
}

function AudioIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M9 18V5l12-2v13" />
      <circle cx="6" cy="18" r="3" />
      <circle cx="18" cy="16" r="3" />
    </svg>
  );
}
function DocIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
    </svg>
  );
}

// Default poster aspect ratio when a video doesn't carry intrinsic
// dimensions (uploaded before we extract them, or unsupported codec).
// 16:9 is the most common case for chat-shared clips.
const VIDEO_DEFAULT_W = 480;
const VIDEO_DEFAULT_H = 270;

function videoBox(attachment: Attachment, viewSize: ChatViewSize | null): { width: number; height: number } {
  const w = attachment.width > 0 ? attachment.width : VIDEO_DEFAULT_W;
  const h = attachment.height > 0 ? attachment.height : VIDEO_DEFAULT_H;
  const maxW = viewSize ? maxImageWidth(viewSize.width) : PREVIEW_FALLBACK_MAX_W;
  const maxH = viewSize ? maxImageHeight(viewSize.height) : PREVIEW_FALLBACK_MAX_H;
  const scale = Math.min(1, maxW / w, maxH / h);
  return {
    width: Math.max(1, Math.round(w * scale)),
    height: Math.max(1, Math.round(h * scale)),
  };
}

function VideoPlayer({
  attachment,
  serverId,
  fillCell = false,
}: {
  attachment: Attachment;
  serverId: string | null;
  fillCell?: boolean;
}) {
  // The chat-side VideoPlayer is now just a *placeholder* that owns
  // the visual slot in the message bubble. The actual <video> element
  // lives in `PersistentVideoLayer` (mounted at app level) and is
  // overlaid on top of this placeholder via fixed positioning. That
  // architecture lets the video keep playing through Virtuoso row
  // unmounts (scroll-away).
  const channelId = useChatStore((s) => s.activeChannelId) ?? "";
  const placeholderRef = useRef<HTMLDivElement | null>(null);
  const cachedAtMount = useRef(getCachedVideo(channelId, attachment.id)).current;
  const [, setTempPath] = useState<string | null>(cachedAtMount?.path ?? null);
  const [assetUrl, setAssetUrl] = useState<string | null>(cachedAtMount?.url ?? null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const viewW = useChatStore((s) => s.chatViewSize?.width ?? 0);
  const viewH = useChatStore((s) => s.chatViewSize?.height ?? 0);
  const chatViewSize = viewW > 0 && viewH > 0 ? { width: viewW, height: viewH } : null;
  const box = videoBox(attachment, chatViewSize);

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

  const activate = (url: string) => {
    if (!serverId) return;
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

  const onPlay = async () => {
    if (!serverId || loading) return;
    // Cached: just activate; the persistent layer reads the cached
    // lastTime and resumes. No re-download.
    if (assetUrl) {
      activate(assetUrl);
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const { path, url } = await invoke<{ path: string; url: string }>(
        "save_attachment_to_temp",
        {
          serverId,
          attachmentId: attachment.id,
          filename: attachment.filename,
        },
      );
      setTempPath(path);
      setAssetUrl(url);
      cacheVideo(channelId, {
        path,
        url,
        attachmentId: attachment.id,
        lastTime: 0,
        wasPlaying: true,
        posterUrl: null,
      });
      activate(url);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  // Subscribe to the cache version store so we re-render when
  // PersistentVideoLayer captures a poster frame for *any* attachment.
  // The cache itself is a plain Map; this is the reactivity bridge.
  useVideoCacheVersionStore((s) => s.version);

  // Read the cached entry on every render so the poster + lastTime
  // stay fresh — they're updated by PersistentVideoLayer (timeupdate
  // and unmount). cachedAtMount is just for the initial state.
  const liveCached = getCachedVideo(channelId, attachment.id);
  const livePoster = liveCached?.posterUrl ?? null;
  const lastTime = liveCached?.lastTime ?? 0;

  // Lazy-fetch the server-side thumbnail when this row is mounted and
  // the attachment carries one. Live captured posters (from playing the
  // video locally) win when both are present — they're a better
  // representation of "where the user left off" than the upload-time
  // first-frame thumbnail.
  const [serverThumb, setServerThumb] = useState<string | null>(() =>
    getCachedThumbnail(attachment.id),
  );
  useEffect(() => {
    if (!serverId) return;
    if (livePoster) return; // live capture takes priority — don't fetch
    if (attachment.thumbnailSizeBytes <= 0) return;
    if (serverThumb) return;
    let cancelled = false;
    fetchThumbnail(serverId, attachment.id).then((url) => {
      if (!cancelled && url) setServerThumb(url);
    });
    return () => { cancelled = true; };
  }, [serverId, attachment.id, attachment.thumbnailSizeBytes, livePoster, serverThumb]);

  const posterUrl = livePoster ?? serverThumb;
  const fmtTime = (s: number) => {
    if (!isFinite(s) || s <= 0) return null;
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${sec.toString().padStart(2, "0")}`;
  };
  const resumeStamp = fmtTime(lastTime);

  // Render the placeholder. When this video is the active one, the
  // PersistentVideoLayer overlays its <video> element on top of this
  // div via fixed positioning, so visually you see the player. When
  // it's not active, you see the poster button below — with the
  // captured frame as the background if one's been cached, so it
  // reads as "paused video" instead of "unclicked".
  // In grid mode the cell sets dimensions (h-full w-full) and the
  // outer card chrome is dropped — the grid container provides the
  // rounded corners + gaps. PersistentVideoLayer tracks the placeholder
  // ref's bounding rect via ResizeObserver, so it picks up either box.
  const wrapperClass = fillCell
    ? "h-full w-full overflow-hidden bg-bg-darkest"
    : "mt-2 overflow-hidden rounded-xl border border-border bg-bg-darkest";
  const wrapperStyle: React.CSSProperties | undefined = fillCell
    ? undefined
    : { width: box.width, height: box.height };

  return (
    <div ref={placeholderRef} className={wrapperClass} style={wrapperStyle}>
      <button
        onClick={onPlay}
        disabled={loading || !!error || isActive}
        title={attachment.filename}
        className="group relative flex h-full w-full cursor-pointer items-center justify-center bg-bg-darkest bg-cover bg-center disabled:cursor-default"
        style={posterUrl ? { backgroundImage: `url(${posterUrl})` } : undefined}
      >
        {posterUrl && !loading && !error && (
          <div className="pointer-events-none absolute inset-0 bg-black/30" />
        )}
        {loading ? (
          <div className="relative h-12 w-12 animate-spin rounded-full border-2 border-white/15 border-t-accent" />
        ) : error ? (
          <div className="relative px-4 text-center text-[12px] text-error">
            Failed to load: {error}
          </div>
        ) : (
          <div className="relative flex h-12 w-20 items-center justify-center rounded-xl bg-accent shadow-[0_4px_20px_rgba(56,143,255,0.35)] transition-all group-hover:scale-110 group-hover:bg-accent-hover group-hover:shadow-[0_6px_28px_rgba(56,143,255,0.45)]">
            <svg className="h-7 w-7 text-white" viewBox="0 0 24 24" fill="currentColor">
              <path d="M8 5v14l11-7z" />
            </svg>
          </div>
        )}
        <div className="pointer-events-none absolute inset-x-0 bottom-0 flex items-center justify-between gap-2 bg-gradient-to-t from-black/85 via-black/50 to-transparent px-3.5 pb-2.5 pt-10 text-[11px]">
          <span className="truncate text-text-secondary">{attachment.filename}</span>
          <span className="shrink-0 tabular-nums text-text-muted">
            {resumeStamp ? `Paused at ${resumeStamp}` : formatBytes(attachment.sizeBytes)}
          </span>
        </div>
      </button>
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
  fillCell?: boolean;
}) {
  switch (attachment.kind) {
    case "image":
      return <ImagePreview attachment={attachment} serverId={serverId} fillCell={fillCell} />;
    case "video":
      return <VideoPlayer attachment={attachment} serverId={serverId} fillCell={fillCell} />;
    case "audio":
      return <FileCard attachment={attachment} serverId={serverId} icon={<AudioIcon />} />;
    case "document":
    default:
      return <FileCard attachment={attachment} serverId={serverId} icon={<DocIcon />} />;
  }
}

// Discord-style row layouts. Sum of each entry equals the count.
// Picked to keep cells from getting too narrow at high counts while
// matching what readers visually expect from chat-attachment grids.
function gridRowCounts(n: number): number[] {
  switch (n) {
    case 2: return [2];
    case 3: return [3];
    case 4: return [2, 2];
    case 5: return [2, 3];
    case 6: return [3, 3];
    case 7: return [3, 4];
    case 8: return [4, 4];
    case 9: return [3, 3, 3];
    case 10: return [5, 5];
    default: return [n];
  }
}

const GRID_GAP_PX = 4;
const GRID_ROW_HEIGHT_PX = 180;
const GRID_MAX_WIDTH_PX = 540;
const GRID_MIN_WIDTH_PX = 320;

function MediaGrid({
  items,
  serverId,
}: {
  items: Attachment[];
  serverId: string | null;
}) {
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
      className="mt-2 flex flex-col overflow-hidden rounded-[10px] border border-border-divider"
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

export default function AttachmentList({
  attachments,
  serverId,
}: {
  attachments: Attachment[];
  serverId: string | null;
}) {
  if (attachments.length === 0) return null;
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
        a.purgedAt > 0
          ? <Tombstone key={a.id} attachment={a} />
          : <LiveAttachment key={a.id} attachment={a} serverId={serverId} />,
      )}
    </div>
  );
}

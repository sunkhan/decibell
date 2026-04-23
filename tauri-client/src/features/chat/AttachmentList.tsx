import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import type { Attachment } from "../../types";
import { useChatStore } from "../../stores/chatStore";

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

// Find the server id that owns a given message's attachment. Attachments
// don't carry their server id on the wire, so we resolve through the
// active server context — which is always the server whose channel the
// message belongs to (there's only one connected server per channel id).
function useAttachmentServerId(): string | null {
  return useChatStore((s) => s.activeServerId);
}

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

function ImagePreview({ attachment, serverId }: { attachment: Attachment; serverId: string | null }) {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const hasFetched = useRef(false);

  // Lazy-load: fetch the image blob only once the card scrolls into view.
  useEffect(() => {
    if (!serverId) return;
    if (hasFetched.current) return;
    if (attachment.sizeBytes > INLINE_PREVIEW_CAP) return; // too big to inline
    const el = containerRef.current;
    if (!el) return;
    const io = new IntersectionObserver((entries) => {
      if (entries.some((e) => e.isIntersecting)) {
        hasFetched.current = true;
        io.disconnect();
        invoke<string>("fetch_attachment_blob", {
          serverId,
          attachmentId: attachment.id,
          mime: attachment.mime || "application/octet-stream",
        })
          .then(setUrl)
          .catch((e) => setError(String(e)));
      }
    }, { rootMargin: "200px 0px" });
    io.observe(el);
    return () => io.disconnect();
  }, [serverId, attachment.id, attachment.sizeBytes, attachment.mime]);

  const tooLargeForPreview = attachment.sizeBytes > INLINE_PREVIEW_CAP;

  return (
    <div ref={containerRef} className="mt-2">
      <button
        onClick={() => url && setExpanded(true)}
        className="group relative overflow-hidden rounded-[10px] border border-border-divider bg-bg-light"
        style={{ maxWidth: 400 }}
        disabled={!url}
      >
        {url ? (
          <img
            src={url}
            alt={attachment.filename}
            className="max-h-[360px] max-w-full object-contain"
            draggable={false}
          />
        ) : (
          <div className="flex h-[120px] w-[240px] items-center justify-center text-[12px] text-text-muted">
            {error
              ? `Failed to load: ${error}`
              : tooLargeForPreview
                ? `${attachment.filename} (${formatBytes(attachment.sizeBytes)}) — download to view`
                : "Loading preview…"}
          </div>
        )}
        {url && (
          <div className="absolute bottom-0 left-0 right-0 bg-gradient-to-t from-black/60 to-transparent px-2 py-1 text-[10.5px] text-white opacity-0 transition-opacity group-hover:opacity-100">
            {attachment.filename}
            {attachment.sizeBytes ? ` • ${formatBytes(attachment.sizeBytes)}` : ""}
          </div>
        )}
      </button>
      {(tooLargeForPreview || !url) && (
        <DownloadButton attachment={attachment} serverId={serverId} />
      )}

      {expanded && url && (
        <div
          className="fixed inset-0 z-[60] flex items-center justify-center bg-black/80 p-8"
          onClick={() => setExpanded(false)}
        >
          <img src={url} alt={attachment.filename} className="max-h-full max-w-full object-contain" />
        </div>
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

  const onClick = async () => {
    if (!serverId) return;
    let destination: string | null = null;
    try {
      destination = await saveDialog({
        defaultPath: attachment.filename || "download",
        title: "Save attachment",
      });
    } catch (err) {
      setError(String(err));
      setState("error");
      return;
    }
    if (!destination) return;
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

  const label = state === "running" ? "Downloading…"
    : state === "done" ? "Saved"
    : state === "error" ? "Retry"
    : "Download";

  if (inline) {
    return (
      <button
        onClick={onClick}
        title={error ?? label}
        disabled={state === "running"}
        className="shrink-0 rounded-md bg-accent-soft px-2.5 py-1.5 text-[11px] font-medium text-accent-bright transition-colors hover:bg-accent-mid disabled:opacity-60"
      >
        {label}
      </button>
    );
  }
  return (
    <button
      onClick={onClick}
      disabled={state === "running"}
      className="mt-1.5 inline-flex items-center gap-1.5 rounded-md bg-accent-soft px-2.5 py-1.5 text-[11px] font-medium text-accent-bright transition-colors hover:bg-accent-mid disabled:opacity-60"
    >
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
        <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
        <polyline points="7 10 12 15 17 10" />
        <line x1="12" y1="15" x2="12" y2="3" />
      </svg>
      {label}
    </button>
  );
}

function VideoIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <polygon points="23 7 16 12 23 17 23 7" />
      <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
    </svg>
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

function LiveAttachment({ attachment, serverId }: { attachment: Attachment; serverId: string | null }) {
  switch (attachment.kind) {
    case "image":
      return <ImagePreview attachment={attachment} serverId={serverId} />;
    case "video":
      return <FileCard attachment={attachment} serverId={serverId} icon={<VideoIcon />} />;
    case "audio":
      return <FileCard attachment={attachment} serverId={serverId} icon={<AudioIcon />} />;
    case "document":
    default:
      return <FileCard attachment={attachment} serverId={serverId} icon={<DocIcon />} />;
  }
}

export default function AttachmentList({ attachments }: { attachments: Attachment[] }) {
  const serverId = useAttachmentServerId();
  if (attachments.length === 0) return null;
  const ordered = [...attachments].sort((a, b) => a.position - b.position);
  return (
    <div className="mt-1 flex flex-col gap-1">
      {ordered.map((a) =>
        a.purgedAt > 0
          ? <Tombstone key={a.id} attachment={a} />
          : <LiveAttachment key={a.id} attachment={a} serverId={serverId} />,
      )}
    </div>
  );
}

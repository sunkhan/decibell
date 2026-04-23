import type { Attachment } from "../../types";

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

function Tombstone({ attachment }: { attachment: Attachment }) {
  // "Cleaned up after N days/weeks/years" relative to the original upload time.
  // Falls back to "Cleaned up" if timestamps are missing.
  const duration = attachment.createdAt > 0 && attachment.purgedAt > attachment.createdAt
    ? formatDurationBetween(attachment.createdAt, attachment.purgedAt)
    : null;

  return (
    <div className="mt-2 flex items-center gap-2.5 rounded-[10px] border border-border bg-bg-light/60 px-3 py-2">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-text-muted/15 text-text-muted">
        {/* Generic "removed" glyph */}
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

function LivePlaceholder({ attachment }: { attachment: Attachment }) {
  // Attachments aren't transportable yet — their URL is blank. Render a
  // metadata row so the presence of the attachment is still visible.
  const size = formatBytes(attachment.sizeBytes);
  return (
    <div className="mt-2 flex items-center gap-2.5 rounded-[10px] border border-border-divider bg-bg-light px-3 py-2">
      <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-accent-soft text-accent-bright">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
        </svg>
      </div>
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-medium text-text-primary">
          {attachment.filename || kindLabel(attachment.kind)}
        </div>
        <div className="truncate text-[11px] text-text-muted">
          {kindLabel(attachment.kind)}
          {size ? ` • ${size}` : ""}
        </div>
      </div>
    </div>
  );
}

export default function AttachmentList({ attachments }: { attachments: Attachment[] }) {
  if (attachments.length === 0) return null;
  const ordered = [...attachments].sort((a, b) => a.position - b.position);
  return (
    <div className="mt-1 flex flex-col gap-1">
      {ordered.map((a) =>
        a.purgedAt > 0
          ? <Tombstone key={a.id} attachment={a} />
          : <LivePlaceholder key={a.id} attachment={a} />,
      )}
    </div>
  );
}

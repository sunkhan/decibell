import type { Attachment } from "../../types";
import {
  buildAttachmentUrl,
  formatDuration,
  formatFileSize,
  pickThumbnailSize,
} from "./attachmentHelpers";
import { useImageViewerStore } from "../../stores/imageViewerStore";

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
  const live = attachments.filter((a) => a.purgedAt === 0);
  const purged = attachments.filter((a) => a.purgedAt > 0);

  return (
    <div className="mt-2 flex flex-col gap-2">
      {live.map((a) => (
        <AttachmentItem key={a.id} attachment={a} serverId={serverId} />
      ))}
      {purged.map((a) => (
        <PurgedTombstone key={a.id} attachment={a} />
      ))}
    </div>
  );
}

function AttachmentItem({
  attachment,
  serverId,
}: {
  attachment: Attachment;
  serverId: string | null;
}) {
  if (attachment.kind === "image") {
    return <ImageItem attachment={attachment} serverId={serverId} />;
  }
  if (attachment.kind === "video") {
    return <VideoItem attachment={attachment} serverId={serverId} />;
  }
  if (attachment.kind === "audio") {
    return <AudioItem attachment={attachment} serverId={serverId} />;
  }
  return <DocumentItem attachment={attachment} serverId={serverId} />;
}

function ImageItem({
  attachment,
  serverId,
}: {
  attachment: Attachment;
  serverId: string | null;
}) {
  const fullUrl = buildAttachmentUrl(serverId, attachment);
  if (!fullUrl) return null;
  const thumbSize = pickThumbnailSize(attachment.thumbnailSizesMask, 640);
  const thumbUrl =
    thumbSize !== null && attachment.thumbnailSizeBytes > 0
      ? buildAttachmentUrl(serverId, attachment, { thumb: true, size: thumbSize })
      : null;
  const previewSrc = thumbUrl ?? fullUrl;
  const open = useImageViewerStore((s) => s.open);

  // Reserve an aspect-ratio box up front so layout doesn't jump when
  // the image loads. Falls back to a fixed height if dimensions are
  // unknown (legacy uploads).
  const w = attachment.width || 0;
  const h = attachment.height || 0;
  const maxW = 480;
  const ratioStyle =
    w > 0 && h > 0
      ? { aspectRatio: `${w} / ${h}`, width: Math.min(w, maxW) }
      : { height: 240, width: 320 };

  return (
    <button
      type="button"
      onClick={() =>
        open({
          url: fullUrl,
          filename: attachment.filename,
          width: attachment.width,
          height: attachment.height,
        })
      }
      className="block overflow-hidden rounded-xl border border-border bg-bg-secondary"
      style={ratioStyle}
    >
      <img
        src={previewSrc}
        alt={attachment.filename}
        className="h-full w-full object-cover"
        loading="lazy"
        draggable={false}
      />
    </button>
  );
}

function VideoItem({
  attachment,
  serverId,
}: {
  attachment: Attachment;
  serverId: string | null;
}) {
  const url = buildAttachmentUrl(serverId, attachment);
  if (!url) return null;
  return (
    <video
      src={url}
      controls
      preload="metadata"
      className="max-h-[420px] max-w-[480px] rounded-xl border border-border bg-black"
    >
      <track kind="captions" />
    </video>
  );
}

function AudioItem({
  attachment,
  serverId,
}: {
  attachment: Attachment;
  serverId: string | null;
}) {
  const url = buildAttachmentUrl(serverId, attachment);
  if (!url) return null;
  return (
    <div className="flex max-w-[480px] flex-col gap-2 rounded-xl border border-border bg-bg-secondary p-3">
      <div className="flex items-center gap-2">
        <span className="font-channel text-sm font-medium text-text-primary">
          {attachment.filename}
        </span>
        <span className="ml-auto text-[11px] text-text-muted">
          {attachment.durationMs > 0 && `${formatDuration(attachment.durationMs)} · `}
          {formatFileSize(attachment.sizeBytes)}
        </span>
      </div>
      <audio src={url} controls preload="metadata" className="w-full" />
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
  return (
    <a
      href={url ?? "#"}
      download={attachment.filename}
      target="_blank"
      rel="noopener noreferrer"
      className={`flex max-w-[420px] items-center gap-3 rounded-xl border border-border bg-bg-secondary p-3 transition-colors hover:bg-bg-light ${
        url ? "" : "pointer-events-none opacity-50"
      }`}
    >
      <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-bg-light text-text-muted">
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
    </a>
  );
}

function PurgedTombstone({ attachment }: { attachment: Attachment }) {
  return (
    <div className="flex max-w-[420px] items-center gap-2 rounded-xl border border-dashed border-border bg-bg-secondary/50 p-3 text-[12px] italic text-text-muted">
      <span className="truncate">
        {attachment.filename || "Attachment"} (deleted by retention policy)
      </span>
    </div>
  );
}

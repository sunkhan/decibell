import { memo, useEffect, useMemo, useState } from "react";
import { extractLinks } from "./richText";
import { useLinkPreviewStore } from "../../stores/linkPreviewStore";
import { useUiStore } from "../../stores/uiStore";
import { useChatStore } from "../../stores/chatStore";
import { useImageViewerStore } from "../../stores/imageViewerStore";
import {
  GIF_MAX_H,
  GIF_MAX_W,
  maxImageHeight,
  maxImageWidth,
  PREVIEW_FALLBACK_MAX_H,
  PREVIEW_FALLBACK_MAX_W,
  reserveBoxFor,
  type MediaKind,
} from "./attachmentSizing";
import { onLinkClick, onLinkAuxClick } from "../../lib/openExternal";
import { isInviteLink } from "../servers/inviteLink";
import InviteEmbed from "./InviteEmbed";
import type { LinkPreview, LinkPreviewImage } from "../../types";

// Preview cards for the links in a message, rendered under its
// attachments. The URLs come from the rich-text parse (so nothing in
// code / math / `<url>` gets a card), main does the unfurl
// (electron/main/linkPreview.ts), linkPreviewStore memoises it.
//
// A card appears only once its data is in — no skeleton. A skeleton
// that later vanishes (dead link, no metadata) would be a second
// layout change for nothing; one growth when the card lands is the
// same settle an image attachment has, and RealMessageList's
// ResizeObserver keeps the bottom pin / anchor across it. Images
// reserve their box from the declared or probed dimensions, so the
// card itself doesn't grow again when the pixels arrive.

const MAX_EMBEDS = 3;
/// Card width cap — Discord's 432px column plus our thumbnail gutter.
const CARD_MAX_WIDTH_PX = 520;
/// Card horizontal chrome: 4px edge + 12px padding each side.
const CARD_INNER_CHROME_PX = 28;
const THUMB_PX = 80;

type SitePreview = Extract<LinkPreview, { kind: "site" }>;

function LinkEmbeds({ content, sender }: { content: string; sender: string }) {
  const enabled = useUiStore((s) => s.linkPreviewsEnabled);
  const urls = useMemo(() => extractLinks(content, MAX_EMBEDS), [content]);
  if (urls.length === 0) return null;
  // Invite cards resolve against our own central, not the linked site,
  // so the link-previews privacy toggle doesn't gate them.
  return (
    <>
      {urls.map((url) =>
        isInviteLink(url) ? (
          <InviteEmbed key={url} href={url} sender={sender} />
        ) : enabled ? (
          <LinkEmbed key={url} url={url} />
        ) : null,
      )}
    </>
  );
}

export default memo(LinkEmbeds);

function LinkEmbed({ url }: { url: string }) {
  const entry = useLinkPreviewStore((s) => s.entries[url]);
  useEffect(() => {
    useLinkPreviewStore.getState().request(url);
  }, [url]);
  const preview = entry?.status === "done" ? entry.preview : null;
  if (!preview) return null;
  return preview.kind === "image" ? (
    <ImageEmbed image={preview} standalone gif={preview.gif} />
  ) : (
    <SiteCard preview={preview} />
  );
}

function filenameOf(url: string): string {
  try {
    const u = new URL(url);
    const last = u.pathname.split("/").filter(Boolean).pop();
    return last ? decodeURIComponent(last) : u.hostname;
  } catch {
    return url;
  }
}

/// An image with its box reserved from known dimensions (the same
/// sqrt-scaled cap the attachment list uses), or at natural size under
/// that cap when the size is unknown. Click opens the lightbox. A load
/// failure hides it — no broken-image glyph, no empty frame.
function ImageEmbed({
  image,
  standalone,
  gif = false,
  onError,
}: {
  image: LinkPreviewImage;
  /// Standalone = the message's link *is* the image: attachment
  /// styling (border, rounded-lg). Inside a card: rounded-md, no
  /// border, capped to the card's column.
  standalone: boolean;
  /// An animated file (a GIF-picker send): the tighter GIF ceiling.
  gif?: boolean;
  onError?: () => void;
}) {
  const open = useImageViewerStore((s) => s.open);
  const chatViewSize = useChatStore((s) => s.chatViewSize);
  const [failed, setFailed] = useState(false);
  if (failed) return null;

  const filename = filenameOf(image.url);
  const onOpen = () =>
    open({ url: image.url, filename, width: image.width, height: image.height });
  const fail = () => {
    setFailed(true);
    onError?.();
  };
  const kind: MediaKind = gif ? "gif" : "image";
  const box = reserveBoxFor(image.width, image.height, chatViewSize, kind);
  const frameClass = standalone
    ? "mt-1 flex cursor-pointer items-center justify-center overflow-hidden rounded-lg border border-border bg-bg-secondary"
    : "mt-2 flex max-w-full cursor-pointer items-center justify-center overflow-hidden rounded-md bg-bg-secondary";
  const imgProps = {
    src: image.url,
    alt: filename,
    decoding: "async" as const,
    draggable: false,
    referrerPolicy: "no-referrer" as const,
    onError: fail,
  };

  if (box.known) {
    let { width, height } = box;
    const innerMax = CARD_MAX_WIDTH_PX - CARD_INNER_CHROME_PX;
    if (!standalone && width > innerMax) {
      height = Math.round((height * innerMax) / width);
      width = innerMax;
    }
    // max-*, not h-full w-full: the box comes from what the page declared
    // (or the byte probe found), and a page can overstate its image. A
    // full-size <img> with object-contain would scale the pixels up to fill
    // the box; capped at natural size the image sits centred in the space
    // it was promised instead.
    return (
      <button type="button" onClick={onOpen} className={frameClass} style={{ width, height }}>
        <img {...imgProps} className="max-h-full max-w-full object-contain" />
      </button>
    );
  }

  const maxW = chatViewSize
    ? maxImageWidth(chatViewSize.width, kind)
    : gif ? GIF_MAX_W : PREVIEW_FALLBACK_MAX_W;
  const maxH = chatViewSize
    ? maxImageHeight(chatViewSize.height, kind)
    : gif ? GIF_MAX_H : PREVIEW_FALLBACK_MAX_H;
  return (
    <button type="button" onClick={onOpen} className={frameClass}>
      <img
        {...imgProps}
        className="block max-w-full object-contain"
        style={{ maxWidth: maxW, maxHeight: maxH }}
      />
    </button>
  );
}

function SiteCard({ preview }: { preview: SitePreview }) {
  const [imageFailed, setImageFailed] = useState(false);
  const image = imageFailed ? null : preview.image;
  const large = image !== null && preview.largeImage;
  const thumb = image !== null && !preview.largeImage;
  return (
    <div
      className={`mt-1 flex overflow-hidden rounded-md border-l-4 bg-bg-dark ${
        preview.color ? "" : "border-accent"
      }`}
      style={{
        maxWidth: CARD_MAX_WIDTH_PX,
        ...(preview.color ? { borderLeftColor: preview.color } : {}),
      }}
    >
      <div className="min-w-0 flex-1 px-3 py-2.5">
        {preview.siteName && (
          <div className="truncate font-meta text-micro text-text-muted">{preview.siteName}</div>
        )}
        {preview.title && (
          <a
            href={preview.url}
            rel="noreferrer"
            onClick={onLinkClick}
            onAuxClick={onLinkAuxClick}
            className="mt-0.5 line-clamp-2 block text-body font-semibold leading-body text-accent hover:underline [overflow-wrap:anywhere]"
          >
            {preview.title}
          </a>
        )}
        {preview.description && (
          <div className="mt-1 line-clamp-3 text-meta leading-body text-text-secondary [overflow-wrap:anywhere]">
            {preview.description}
          </div>
        )}
        {large && (
          <ImageEmbed image={image} standalone={false} onError={() => setImageFailed(true)} />
        )}
      </div>
      {thumb && (
        <div
          className="my-2.5 mr-3 shrink-0 overflow-hidden rounded-sm bg-bg-secondary"
          style={{ width: THUMB_PX, height: THUMB_PX }}
        >
          <img
            src={image.url}
            alt=""
            decoding="async"
            draggable={false}
            referrerPolicy="no-referrer"
            onError={() => setImageFailed(true)}
            className="h-full w-full object-cover"
          />
        </div>
      )}
    </div>
  );
}

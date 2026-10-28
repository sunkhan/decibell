import { useEffect, useMemo } from "react";
import { useImageContextMenuStore } from "../stores/imageContextMenuStore";
import { useMenuPosition } from "../hooks/useMenuPosition";
import { copyImage, copyLink, openInBrowser, saveImage } from "../features/chat/imageViewer/imageActions";

// Right-click menu for chat images and videos, inline and in the viewer.
//
//   - Copy image → the bytes through main's netFetch, as PNG, onto the
//     clipboard (imageActions.copyImage).
//   - Save as… → the download manager in main, with its save dialog;
//     the transfer shows in the Transfers panel (attachments).
//   - Copy link / Open in browser → link images.
export default function ImageContextMenu() {
  const open = useImageContextMenuStore((s) => s.open);
  const x = useImageContextMenuStore((s) => s.x);
  const y = useImageContextMenuStore((s) => s.y);
  const serverId = useImageContextMenuStore((s) => s.serverId);
  const attachmentId = useImageContextMenuStore((s) => s.attachmentId);
  const filename = useImageContextMenuStore((s) => s.filename);
  const kind = useImageContextMenuStore((s) => s.kind);
  const mime = useImageContextMenuStore((s) => s.mime);
  const url = useImageContextMenuStore((s) => s.url);
  const pageUrl = useImageContextMenuStore((s) => s.pageUrl);
  const close = useImageContextMenuStore((s) => s.close);

  // Keep the menu inside the viewport (measured, flips above the cursor
  // near the bottom edge). One anchor object per opening.
  const anchor = useMemo(() => (open ? { x, y } : null), [open, x, y]);
  const { ref: menuRef, style: menuStyle } = useMenuPosition(anchor);

  useEffect(() => {
    if (!open) return;
    const onMouseDown = () => close();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    const onScroll = () => close();
    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKey);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKey);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [open, close]);

  const isAttachment = serverId !== null && attachmentId !== null;
  if (!open || (!isAttachment && !url)) return null;

  const image = {
    key: "",
    url: url ?? "",
    filename: filename ?? (kind === "video" ? "video" : "image"),
    width: 0,
    height: 0,
    mime: mime ?? undefined,
    serverId: serverId ?? undefined,
    attachmentId: attachmentId ?? undefined,
    pageUrl: pageUrl ?? undefined,
  };
  const run = (fn: () => void) => (e: React.MouseEvent) => {
    e.stopPropagation();
    close();
    fn();
  };

  return (
    <div
      ref={menuRef}
      className="z-[80] min-w-[180px] rounded-md border border-border bg-bg-light p-1 shadow-float animate-[fadeUp_0.12s_ease_both]"
      style={menuStyle}
      onMouseDown={(e) => e.stopPropagation()}
    >
      {kind === "image" && (
        <MenuItem
          onClick={run(() => void copyImage(image))}
          icon={
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
              <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
            </svg>
          }
        >
          Copy image
        </MenuItem>
      )}
      {isAttachment && (
        <MenuItem
          onClick={run(() => saveImage(image, true, kind))}
          icon={
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
              <polyline points="7 10 12 15 17 10" />
              <line x1="12" y1="15" x2="12" y2="3" />
            </svg>
          }
        >
          Save as…
        </MenuItem>
      )}
      {!isAttachment && (
        <>
          <MenuItem
            onClick={run(() => void copyLink(image))}
            icon={
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M10 13a5 5 0 0 0 7.5.5l3-3a5 5 0 0 0-7-7l-1.7 1.7" />
                <path d="M14 11a5 5 0 0 0-7.5-.5l-3 3a5 5 0 0 0 7 7l1.7-1.7" />
              </svg>
            }
          >
            Copy link
          </MenuItem>
          <MenuItem
            onClick={run(() => openInBrowser(image))}
            icon={
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
                <polyline points="15 3 21 3 21 9" />
                <line x1="10" y1="14" x2="21" y2="3" />
              </svg>
            }
          >
            Open in browser
          </MenuItem>
        </>
      )}
    </div>
  );
}

function MenuItem({
  onClick,
  icon,
  children,
}: {
  onClick: (e: React.MouseEvent) => void;
  icon: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      onMouseDown={(e) => e.stopPropagation()}
      className="flex w-full items-center gap-2.5 rounded-sm px-3 py-1.5 text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
    >
      <span className="text-accent">{icon}</span>
      <span>{children}</span>
    </button>
  );
}

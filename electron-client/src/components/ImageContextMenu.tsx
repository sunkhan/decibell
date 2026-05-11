import { useEffect, useMemo } from "react";
import { useImageContextMenuStore } from "../stores/imageContextMenuStore";
import { toast } from "../stores/toastStore";

const MENU_WIDTH = 200;
const MENU_HEIGHT = 84; // approximate, used for edge-clamping

// Right-click menu for chat image and video attachments.
//
// Both actions stay 100% renderer-side using Chromium's built-ins
// instead of porting tauri-client's native download_attachment +
// copyAttachmentToClipboard:
//
//   - Copy → fetch the attachment URL, get a Blob, hand it to the
//     Web Clipboard API. Native code never sees the bytes.
//   - Save → fetch the URL, take an ArrayBuffer, and write it to the
//     user's chosen path via the fs:writeFile IPC (a thin wrapper
//     around fs.promises.writeFile in main).
//
// The decibell-attachment:// custom protocol handles auth and the
// HTTP round-trip transparently — the renderer treats attachments as
// fetch()-able URLs.
export default function ImageContextMenu() {
  const open = useImageContextMenuStore((s) => s.open);
  const x = useImageContextMenuStore((s) => s.x);
  const y = useImageContextMenuStore((s) => s.y);
  const serverId = useImageContextMenuStore((s) => s.serverId);
  const attachmentId = useImageContextMenuStore((s) => s.attachmentId);
  const filename = useImageContextMenuStore((s) => s.filename);
  const kind = useImageContextMenuStore((s) => s.kind);
  const close = useImageContextMenuStore((s) => s.close);

  // Keep the menu inside the viewport.
  const position = useMemo(() => {
    const left = Math.min(x, window.innerWidth - MENU_WIDTH - 8);
    const top = Math.min(y, window.innerHeight - MENU_HEIGHT - 8);
    return { left: Math.max(8, left), top: Math.max(8, top) };
  }, [x, y]);

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

  if (!open || !serverId || attachmentId === null) return null;

  // Fetch the attachment via the main-process net.fetch helper rather
  // than renderer-side fetch(decibell-attachment://...). The custom
  // protocol works for `<img src>` (image loads aren't CORS-checked)
  // but renderer fetch() against a non-renderer origin is rejected by
  // Chromium even with webSecurity off. netFetch goes straight
  // through main, sidesteps CORS, and the attachmentTarget shorthand
  // builds the upstream URL + adds the Bearer token automatically.
  const fetchAttachmentBytes = async (): Promise<{
    bytes: ArrayBuffer;
    mime: string;
  }> => {
    const res = await window.decibell.netFetch("", {
      method: "GET",
      attachmentTarget: {
        serverId,
        path: `/attachments/${attachmentId}`,
      },
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return {
      bytes: res.body,
      mime: res.headers["content-type"] ?? "application/octet-stream",
    };
  };

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    close();
    try {
      const { bytes, mime } = await fetchAttachmentBytes();
      let blob = new Blob([bytes], { type: mime });
      // Chromium's Clipboard API only accepts a small whitelist of
      // image MIME types (image/png is universally supported; some
      // builds also accept jpeg/gif). Convert anything else to PNG via
      // a canvas round-trip so the paste lands in any client.
      if (blob.type !== "image/png") {
        const bitmap = await createImageBitmap(blob);
        const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
        const ctx = canvas.getContext("2d");
        if (!ctx) throw new Error("Canvas 2D unavailable");
        ctx.drawImage(bitmap, 0, 0);
        blob = await canvas.convertToBlob({ type: "image/png" });
        bitmap.close();
      }
      await navigator.clipboard.write([
        new ClipboardItem({ "image/png": blob }),
      ]);
      toast.success("Image copied", filename ?? undefined);
    } catch (err) {
      toast.error("Copy failed", String(err));
    }
  };

  const handleSave = async (e: React.MouseEvent) => {
    e.stopPropagation();
    close();
    const isVideo = kind === "video";
    try {
      const dest = await window.decibell.dialog.save({
        defaultPath: filename || (isVideo ? "video" : "image"),
      });
      if (!dest) return;
      const { bytes } = await fetchAttachmentBytes();
      await window.decibell.fs.writeFile(dest, new Uint8Array(bytes));
      toast.success(isVideo ? "Video saved" : "Image saved", filename ?? undefined);
    } catch (err) {
      toast.error("Save failed", String(err));
    }
  };

  return (
    <div
      className="fixed z-[80] min-w-[180px] rounded-lg border border-border bg-bg-light p-1 shadow-[0_8px_32px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.02)] animate-[fadeUp_0.12s_ease_both]"
      style={position}
      onMouseDown={(e) => e.stopPropagation()}
    >
      {kind === "image" && (
        <MenuItem
          onClick={handleCopy}
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
      <MenuItem
        onClick={handleSave}
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
      className="flex w-full items-center gap-2.5 rounded-md px-3 py-1.5 text-[12.5px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
    >
      <span className="text-accent">{icon}</span>
      <span>{children}</span>
    </button>
  );
}

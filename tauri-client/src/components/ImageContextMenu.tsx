import { useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useImageContextMenuStore } from "../stores/imageContextMenuStore";
import { copyAttachmentToClipboard } from "../features/chat/imageCache";
import { toast } from "../stores/toastStore";
import { pickSavePath } from "../features/chat/filePicker";

const MENU_WIDTH = 200;
const MENU_HEIGHT = 84; // approximate, used for edge-clamping

export default function ImageContextMenu() {
  const open = useImageContextMenuStore((s) => s.open);
  const x = useImageContextMenuStore((s) => s.x);
  const y = useImageContextMenuStore((s) => s.y);
  const serverId = useImageContextMenuStore((s) => s.serverId);
  const attachmentId = useImageContextMenuStore((s) => s.attachmentId);
  const filename = useImageContextMenuStore((s) => s.filename);
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

  const handleCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    close();
    try {
      await copyAttachmentToClipboard(serverId, attachmentId);
      toast.success("Image copied", filename ?? undefined);
    } catch (err) {
      toast.error("Copy failed", String(err));
    }
  };

  const handleSave = async (e: React.MouseEvent) => {
    e.stopPropagation();
    close();
    try {
      const dest = await pickSavePath({
        title: "Save image",
        defaultName: filename || "image",
      });
      if (!dest) return;
      await invoke("download_attachment", {
        req: { serverId, attachmentId, destinationPath: dest },
      });
      toast.success("Image saved", filename ?? undefined);
    } catch (err) {
      toast.error("Save failed", String(err));
    }
  };

  return (
    <div
      className="fixed z-[80] min-w-[180px] rounded-lg border border-border bg-bg-secondary p-1 shadow-[0_8px_32px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.02)] animate-[fadeUp_0.12s_ease_both]"
      style={position}
      onMouseDown={(e) => e.stopPropagation()}
    >
      <MenuItem onClick={handleCopy} icon={
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
        </svg>
      }>Copy image</MenuItem>
      <MenuItem onClick={handleSave} icon={
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="7 10 12 15 17 10" />
          <line x1="12" y1="15" x2="12" y2="3" />
        </svg>
      }>Save as…</MenuItem>
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
      <span className="text-text-muted">{icon}</span>
      <span>{children}</span>
    </button>
  );
}

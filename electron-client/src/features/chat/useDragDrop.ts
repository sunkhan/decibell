import { useEffect } from "react";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import { enqueueUpload } from "./uploadAttachment";

function generatePendingId(): string {
  return `att-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;
}

/// Window-level drag/drop hook. Listens for files being dragged onto
/// the application, sets `dragActive` so the channel sidebar can
/// render a target overlay, and on drop enqueues uploads bound to
/// the active server + channel (or the channel under the cursor if
/// the drop target announced one).
///
/// Native browser drag/drop in Chromium gives us file paths through
/// the DataTransferItem.getAsFile() side of the event — no Tauri
/// listener needed, no second IPC round-trip.
export function useDragDrop() {
  const setDragActive = useUiStore((s) => s.setDragActive);
  const setDragHoveredKey = useUiStore((s) => s.setDragHoveredKey);

  useEffect(() => {
    let dragDepth = 0;

    const onDragEnter = (e: DragEvent) => {
      if (!e.dataTransfer || !Array.from(e.dataTransfer.types).includes("Files")) {
        return;
      }
      dragDepth += 1;
      setDragActive(true);
    };
    const onDragOver = (e: DragEvent) => {
      if (!e.dataTransfer || !Array.from(e.dataTransfer.types).includes("Files")) {
        return;
      }
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
      // Read the closest data-drop-target element so the sidebar can
      // highlight a specific channel as the active target.
      const target = (e.target as HTMLElement | null)?.closest?.(
        "[data-drop-target]",
      ) as HTMLElement | null;
      const dropKey = target?.getAttribute("data-drop-target") ?? null;
      setDragHoveredKey(dropKey);
    };
    const onDragLeave = () => {
      dragDepth = Math.max(0, dragDepth - 1);
      if (dragDepth === 0) {
        setDragActive(false);
        setDragHoveredKey(null);
      }
    };
    const onDrop = async (e: DragEvent) => {
      e.preventDefault();
      dragDepth = 0;
      setDragActive(false);
      const dropKey = useUiStore.getState().dragHoveredKey;
      setDragHoveredKey(null);

      const files: File[] = [];
      const dt = e.dataTransfer;
      if (!dt) return;
      for (let i = 0; i < dt.files.length; i++) {
        const f = dt.files.item(i);
        if (f) files.push(f);
      }
      if (files.length === 0) return;

      // Resolve target server/channel from data-drop-target marker
      // (channel:<serverId>:<channelId>) — falls back to active
      // channel if no element announced one.
      let serverId: string | null = null;
      let channelId: string | null = null;
      if (dropKey?.startsWith("channel:")) {
        const parts = dropKey.split(":");
        if (parts.length >= 3) {
          serverId = parts[1];
          channelId = parts.slice(2).join(":");
        }
      }
      if (!serverId || !channelId) {
        const chat = useChatStore.getState();
        serverId = chat.activeServerId;
        channelId = chat.activeChannelId;
      }
      if (!serverId || !channelId) return;

      for (const file of files) {
        const pendingId = generatePendingId();
        // Fire-and-forget. Errors land in attachmentsStore.
        enqueueUpload({ pendingId, serverId, channelId, file }).catch(() => {});
      }
      void useAttachmentsStore;
    };

    window.addEventListener("dragenter", onDragEnter);
    window.addEventListener("dragover", onDragOver);
    window.addEventListener("dragleave", onDragLeave);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onDragEnter);
      window.removeEventListener("dragover", onDragOver);
      window.removeEventListener("dragleave", onDragLeave);
      window.removeEventListener("drop", onDrop);
    };
  }, [setDragActive, setDragHoveredKey]);
}

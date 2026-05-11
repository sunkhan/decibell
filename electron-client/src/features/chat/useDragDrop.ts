import { useEffect } from "react";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import { queueUpload } from "./uploadAttachment";
import { chunkSourceFromFile } from "./chunkSource";

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
    // Drag tracking model:
    //
    //   dragenter — set dragActive=true when files first enter the
    //               window. Re-firing while moving across child
    //               elements is harmless (we guard with the equality
    //               check).
    //   dragover  — refresh the per-channel hovered key based on the
    //               closest data-drop-target ancestor. Equality-
    //               guarded so 60Hz dragover doesn't thrash the store.
    //   dragleave — fires for *every* child element the cursor
    //               crosses, NOT just when leaving the window. The
    //               canonical fix is to read `e.relatedTarget`: when
    //               the cursor moves between elements inside the
    //               window, relatedTarget is the new element (non-
    //               null). When the cursor actually leaves the
    //               window, relatedTarget is null. So we only clear
    //               state when relatedTarget is null.
    //   drop      — clears state immediately and dispatches uploads.
    //
    // The earlier dragenter/dragleave + counter approach was buggy
    // (the file-only guard on enter combined with unconditional
    // decrement on leave drove the counter negative, killing the
    // highlight mid-drag). The dragover-only + setTimeout reset
    // approach was even worse — it kept the entire app under
    // continuous re-render at 60Hz and froze the renderer. This
    // relatedTarget pattern is the standard browser idiom.

    const onDragEnter = (e: DragEvent) => {
      if (!e.dataTransfer || !Array.from(e.dataTransfer.types).includes("Files")) {
        return;
      }
      const ui = useUiStore.getState();
      if (!ui.dragActive) ui.setDragActive(true);
    };
    const onDragOver = (e: DragEvent) => {
      if (!e.dataTransfer || !Array.from(e.dataTransfer.types).includes("Files")) {
        return;
      }
      // preventDefault here is what tells the browser "drop allowed
      // here" — without it the drop event never fires.
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";

      const target = (e.target as HTMLElement | null)?.closest?.(
        "[data-drop-target]",
      ) as HTMLElement | null;
      const dropKey = target?.getAttribute("data-drop-target") ?? null;
      // Skip the store write when the value hasn't changed — zustand
      // notifies subscribers on every set() regardless of equality,
      // and dragover at 60Hz over the same target would otherwise
      // trigger a re-render storm of every channel row.
      const ui = useUiStore.getState();
      if (ui.dragHoveredKey !== dropKey) ui.setDragHoveredKey(dropKey);
    };
    const onDragLeave = (e: DragEvent) => {
      // relatedTarget is null only when the cursor leaves the window
      // entirely. Moving between elements inside the window keeps
      // relatedTarget non-null, which is exactly what we want — those
      // mid-drag transitions shouldn't clear the state.
      if (e.relatedTarget !== null) return;
      setDragActive(false);
      setDragHoveredKey(null);
    };
    const onDrop = async (e: DragEvent) => {
      e.preventDefault();
      const dropKey = useUiStore.getState().dragHoveredKey;
      setDragActive(false);
      setDragHoveredKey(null);

      const dt = e.dataTransfer;
      if (!dt) return;

      const files: File[] = [];
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
      let droppedOnSidebarTarget = false;
      if (dropKey?.startsWith("channel:")) {
        const parts = dropKey.split(":");
        if (parts.length >= 3) {
          serverId = parts[1];
          channelId = parts.slice(2).join(":");
          droppedOnSidebarTarget = true;
        }
      }
      if (!serverId || !channelId) {
        const chat = useChatStore.getState();
        serverId = chat.activeServerId;
        channelId = chat.activeChannelId;
      }
      if (!serverId || !channelId) return;

      // Drop landed on a sidebar channel target — navigate to that
      // channel so the user can see the upload progress (and the
      // resulting message they're about to send) without an extra
      // click. Only switch when something actually differs to avoid
      // store churn that re-renders the world.
      if (droppedOnSidebarTarget) {
        const chat = useChatStore.getState();
        const ui = useUiStore.getState();
        if (chat.activeServerId !== serverId) chat.setActiveServer(serverId);
        if (chat.activeChannelId !== channelId) chat.setActiveChannel(channelId);
        if (ui.activeView !== "server") ui.setActiveView("server");
      }

      for (const file of files) {
        // Fire-and-forget. queueUpload registers as `queued` only —
        // bytes don't leave the renderer until the user clicks send
        // (handleSend kicks off startQueuedUpload then). The
        // ChunkSource takes the streaming `decibell-file://` route
        // when the dropped file has a backing disk path (typical for
        // OS-dragged files); falls back to a Blob URL otherwise.
        void (async () => {
          try {
            const source = await chunkSourceFromFile(file);
            const pendingId = generatePendingId();
            queueUpload({
              pendingId,
              serverId: serverId!,
              channelId: channelId!,
              source,
            }).catch(() => {});
          } catch (e) {
            console.error("drop register:", file.name, e);
          }
        })();
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

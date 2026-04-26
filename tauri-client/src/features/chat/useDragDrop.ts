import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useUiStore } from "../../stores/uiStore";
import { useChatStore } from "../../stores/chatStore";
import { uploadAttachment } from "./uploadAttachment";

/**
 * Drop-target encoded in `data-drop-target` attributes:
 *
 *   "active-input"            → drop into the active channel's input
 *   "channel:<serverId>:<id>" → drop into a specific channel (sidebar)
 *
 * The hook reads them off the DOM via `elementFromPoint` rather than
 * subscribing to React refs because the Tauri drag-drop position arrives
 * outside React's tree. Walking up to a `data-drop-target` ancestor
 * keeps the markup decoupled from the lookup.
 */
interface DropTarget {
  key: string;
  serverId: string;
  channelId: string;
}

interface DragPayload {
  paths: string[];
  position: { x: number; y: number };
}

function findDropTarget(x: number, y: number): DropTarget | null {
  const el = document.elementFromPoint(x, y);
  if (!el) return null;
  const node = el.closest("[data-drop-target]") as HTMLElement | null;
  if (!node) return null;
  const key = node.getAttribute("data-drop-target") ?? "";

  if (key === "active-input") {
    const { activeServerId, activeChannelId } = useChatStore.getState();
    if (!activeServerId || !activeChannelId) return null;
    return { key, serverId: activeServerId, channelId: activeChannelId };
  }

  if (key.startsWith("channel:")) {
    const serverId = node.getAttribute("data-server-id");
    const channelId = node.getAttribute("data-channel-id");
    if (!serverId || !channelId) return null;
    return { key, serverId, channelId };
  }

  return null;
}

/**
 * Tauri reports drag positions in physical pixels; the DOM lives in CSS
 * pixels. Divide by devicePixelRatio so `elementFromPoint` lands on the
 * element the user is visually over (matters on HiDPI displays).
 */
function toCssPosition(p: { x: number; y: number }): { x: number; y: number } {
  const dpr = window.devicePixelRatio || 1;
  return { x: p.x / dpr, y: p.y / dpr };
}

export function useDragDrop() {
  const setDragActive = useUiStore((s) => s.setDragActive);
  const setDragHoveredKey = useUiStore((s) => s.setDragHoveredKey);

  useEffect(() => {
    const unlistens: Array<() => void> = [];

    listen<DragPayload>("tauri://drag-enter", () => {
      setDragActive(true);
    }).then((u) => unlistens.push(u));

    listen<DragPayload>("tauri://drag-over", (event) => {
      const { x, y } = toCssPosition(event.payload.position);
      const target = findDropTarget(x, y);
      setDragHoveredKey(target?.key ?? null);
    }).then((u) => unlistens.push(u));

    listen<DragPayload>("tauri://drag-leave", () => {
      setDragActive(false);
      setDragHoveredKey(null);
    }).then((u) => unlistens.push(u));

    listen<DragPayload>("tauri://drag-drop", async (event) => {
      setDragActive(false);
      setDragHoveredKey(null);
      const { paths, position } = event.payload;
      if (!paths || paths.length === 0) return;
      const { x, y } = toCssPosition(position);
      const target = findDropTarget(x, y);
      if (!target) return;

      // If the user dropped on a different channel than the active one,
      // switch to it first so they can see the upload progress and write
      // a caption. setActiveChannel also updates the LRU access order.
      const chat = useChatStore.getState();
      if (chat.activeChannelId !== target.channelId) {
        chat.setActiveServer(target.serverId);
        chat.setActiveChannel(target.channelId);
      }

      const cfg = chat.serverAttachmentConfig[target.serverId];
      const maxBytes = cfg?.maxBytes ?? 0;
      for (const filePath of paths) {
        await uploadAttachment({
          filePath,
          serverId: target.serverId,
          channelId: target.channelId,
          maxBytes,
        });
      }
    }).then((u) => unlistens.push(u));

    return () => {
      unlistens.forEach((u) => u());
    };
  }, [setDragActive, setDragHoveredKey]);
}

import { useEffect } from "react";
import { useChatStore } from "../../stores/chatStore";
import { enqueueUpload } from "./uploadAttachment";

function generatePendingId(): string {
  return `att-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;
}

/// Paste-to-attach. Listens for `paste` events on the window; when
/// the clipboard contains a File (image data, copied media), enqueue
/// the upload bound to the active channel.
///
/// Chromium's ClipboardEvent.clipboardData.files is populated for
/// image-bearing pastes (screenshots, copy-image-from-browser). No
/// permission prompt, no second IPC.
export function usePasteToAttach() {
  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const dt = e.clipboardData;
      if (!dt || dt.files.length === 0) return;

      // Only intercept paste when the active focus isn't a text input
      // expecting normal text paste. A paste in the message textarea
      // gets the file too, but we should check `items` for text/plain
      // and let the textarea keep its own paste behaviour for text.
      const target = e.target as HTMLElement | null;
      const isTextField =
        target?.tagName === "INPUT" ||
        target?.tagName === "TEXTAREA" ||
        target?.isContentEditable;
      // If there's text content too, prefer the text paste — only
      // intercept "pure file" paste events.
      const hasText = Array.from(dt.items).some((i) => i.kind === "string");
      if (isTextField && hasText) return;

      const chat = useChatStore.getState();
      if (!chat.activeServerId || !chat.activeChannelId) return;

      e.preventDefault();
      for (let i = 0; i < dt.files.length; i++) {
        const file = dt.files.item(i);
        if (!file) continue;
        const pendingId = generatePendingId();
        enqueueUpload({
          pendingId,
          serverId: chat.activeServerId,
          channelId: chat.activeChannelId,
          file,
        }).catch(() => {});
      }
    };

    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, []);
}

import { useEffect } from "react";
import { useChatStore } from "../../stores/chatStore";
import { queueUpload } from "./uploadAttachment";
import { chunkSourceFromFile } from "./chunkSource";

function generatePendingId(): string {
  return `att-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;
}

/// Paste-to-attach. Listens for `paste` events on the window; when
/// the clipboard contains a File (image data, copied media), enqueue
/// the upload bound to the active channel.
///
/// We read from `dt.items` rather than `dt.files`: cross-app image
/// pastes (e.g. Firefox right-click → Copy Image) often expose the
/// image as a file-kind item but leave `dt.files` empty, and we want
/// Discord-style behaviour where the image always wins over the
/// accompanying source URL / HTML that the browser also stuffs onto
/// the clipboard.
export function usePasteToAttach() {
  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const dt = e.clipboardData;
      if (!dt) return;

      // Pull every file-kind entry from `dt.items`. We intentionally
      // do NOT also iterate `dt.files`: it's spec-defined as a view
      // over the same file-kind items, so unioning would double-count
      // each clipboard entry. (Object-identity de-dup doesn't save us
      // here because `getAsFile()` and `FileList.item()` are both
      // specified to return fresh File instances on each call.)
      // Cross-app image pastes (Firefox/Chrome copy-image, screenshot
      // tools) all surface their image as a file-kind item, so
      // items-only is sufficient.
      const files: File[] = [];
      for (let i = 0; i < dt.items.length; i++) {
        const item = dt.items[i];
        if (item.kind !== "file") continue;
        const f = item.getAsFile();
        if (f) files.push(f);
      }
      if (files.length === 0) return;

      const chat = useChatStore.getState();
      if (!chat.activeServerId || !chat.activeChannelId) return;

      // We have at least one file — intercept the paste so the
      // editor doesn't also insert the accompanying text/HTML
      // representation (Firefox's copy-image ships an image + the
      // source page's URL; without preventDefault the URL would be
      // inserted into the input on top of our attachment chip).
      e.preventDefault();
      for (const file of files) {
        // Pasted files (clipboard images / screenshots) have no
        // backing disk path, so chunkSourceFromFile falls back to a
        // Blob URL. The bytes already live in Chromium's clipboard
        // store; no second copy.
        void (async () => {
          try {
            const source = await chunkSourceFromFile(file);
            const pendingId = generatePendingId();
            queueUpload({
              pendingId,
              serverId: chat.activeServerId!,
              channelId: chat.activeChannelId!,
              source,
            }).catch(() => {});
          } catch (e) {
            console.error("paste register:", file.name, e);
          }
        })();
      }
    };

    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, []);
}

import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import { uploadAttachment } from "./uploadAttachment";
import { formatBytes } from "./attachmentHelpers";

// Hard cap on a single pasted item, independent of the server's per-file
// cap. Above this, the JS→Rust IPC turns the byte array into a giant JSON
// number list and risks blowing up memory; the file picker remains the
// recommended path for very large files.
const MAX_PASTE_BYTES = 100 * 1024 * 1024;

/**
 * Listens for clipboard pastes anywhere in the app while the user is in
 * a server channel. If the clipboard carries File objects (image from a
 * screenshot tool, files copied from a file manager, etc.), the paste is
 * intercepted, written to a temp file via the Rust side, and run through
 * the same upload pipeline as the file picker and drag-drop.
 *
 * Plain text pastes are ignored — `clipboardData.files` is empty and we
 * never call `preventDefault`, so the input field receives the keystroke
 * normally.
 */
export function usePasteToAttach() {
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const serverAttachmentConfig = useChatStore((s) => s.serverAttachmentConfig);
  const activeView = useUiStore((s) => s.activeView);
  const activeModal = useUiStore((s) => s.activeModal);

  useEffect(() => {
    // Only intercept inside a server channel. DM view explicitly does not
    // support attachments, and modals (settings, channel admin, etc.)
    // need pastes to land in their own input fields.
    if (activeView !== "server") return;
    if (activeModal) return;
    if (!activeServerId || !activeChannelId) return;

    const cfg = serverAttachmentConfig[activeServerId];
    if (!cfg || cfg.port === 0) return;
    const maxBytes = cfg.maxBytes;

    const handler = async (e: ClipboardEvent) => {
      // Prefer `clipboardData.files` when populated (Windows / WebView2,
      // typical browsers): one canonical source, no duplicates. Fall back
      // to iterating `items` only when files is empty — that's the path
      // Linux WebKitGTK takes for screenshot tools, which expose images
      // via items with kind:"file" but leave files empty.
      //
      // The previous "iterate both, dedupe via Set<File>" approach broke
      // on Chromium-Windows because items[i].getAsFile() and files[i]
      // return different File object references for the same logical
      // file — Set identity dedup misses, two uploads run, the user
      // sees both ~doubled latency and a duplicated attachment.
      let collected: File[] = Array.from(e.clipboardData?.files ?? []);
      if (collected.length === 0) {
        for (const item of Array.from(e.clipboardData?.items ?? [])) {
          if (item.kind !== "file") continue;
          const f = item.getAsFile();
          if (f) collected.push(f);
        }
      }
      if (collected.length === 0) {
        // No file content via the JS clipboard. There are three remaining
        // cases worth handling:
        //   (a) file-manager copies — clipboard carries `text/uri-list`
        //       with `file://` URIs. We parse the paths and reuse the
        //       existing path-based upload pipeline (same as drag-drop).
        //   (b) screenshot tools / "Copy Image" from browsers — neither
        //       populate the JS clipboard with bytes; we fall back to a
        //       Rust-side OS-clipboard read.
        //   (c) plain text paste — bail and let the input field handle it.
        const types = Array.from(e.clipboardData?.types ?? []);
        if (types.includes("text/plain")) return;

        if (types.includes("text/uri-list")) {
          // WebKitGTK lists `text/uri-list` in `types` but `getData` for
          // non-text MIMEs is policy-gated and returns "". Fall back to
          // a Rust-side `wl-paste` read which doesn't go through that
          // gate. On platforms where getData works, we still take that
          // path first so we avoid an unnecessary IPC round-trip.
          let paths: string[] = [];
          const uriList = e.clipboardData?.getData("text/uri-list") ?? "";
          if (uriList) {
            paths = uriList
              .split(/\r?\n/)
              .map((line) => line.trim())
              .filter((line) => line.startsWith("file://"))
              .map((uri) => {
                try {
                  return decodeURIComponent(new URL(uri).pathname);
                } catch {
                  return null;
                }
              })
              .filter((p): p is string => p !== null);
          }
          if (paths.length === 0) {
            try {
              paths = await invoke<string[]>("read_clipboard_file_paths");
            } catch (err) {
              console.error("clipboard uri-list read failed", err);
            }
          }
          if (paths.length > 0) {
            e.preventDefault();
            for (const filePath of paths) {
              await uploadAttachment({
                filePath,
                serverId: activeServerId,
                channelId: activeChannelId,
                maxBytes,
              });
            }
            return;
          }
        }

        try {
          const img = await invoke<{ bytes: number[]; mime: string } | null>(
            "read_clipboard_image",
          );
          if (!img) return;
          // Default action for an empty/HTML-only paste is a no-op, so
          // not preventing default is fine — and it avoids racing with
          // the input field.
          const bytes = new Uint8Array(img.bytes);
          if (bytes.length > MAX_PASTE_BYTES) {
            toast.error(
              "Pasted image too large",
              `${formatBytes(bytes.length)} exceeds the ${formatBytes(MAX_PASTE_BYTES)} paste limit. Use the file picker for very large files.`,
            );
            return;
          }
          if (maxBytes > 0 && bytes.length > maxBytes) {
            toast.error(
              "Pasted image too large",
              `${formatBytes(bytes.length)} exceeds this server's ${formatBytes(maxBytes)} attachment limit.`,
            );
            return;
          }
          const ext = img.mime === "image/png" ? "png" : "bin";
          const filename = `pasted-${Date.now()}.${ext}`;
          const tempPath = await invoke<string>("save_paste_to_temp", {
            bytes: Array.from(bytes),
            filename,
          });
          await uploadAttachment({
            filePath: tempPath,
            serverId: activeServerId,
            channelId: activeChannelId,
            maxBytes,
          });
        } catch (err) {
          console.error("clipboard image fallback failed", err);
        }
        return;
      }
      e.preventDefault();
      const files = collected;

      for (const file of files) {
        const displayName = file.name || "(unnamed)";
        if (file.size > MAX_PASTE_BYTES) {
          toast.error(
            `${displayName} too large to paste`,
            `${formatBytes(file.size)} exceeds the ${formatBytes(MAX_PASTE_BYTES)} paste limit. Use the file picker for very large files.`,
          );
          continue;
        }
        if (maxBytes > 0 && file.size > maxBytes) {
          toast.error(
            `${displayName} is too large`,
            `${formatBytes(file.size)} exceeds this server's ${formatBytes(maxBytes)} attachment limit.`,
          );
          continue;
        }
        try {
          const buffer = await file.arrayBuffer();
          const tempPath = await invoke<string>("save_paste_to_temp", {
            bytes: Array.from(new Uint8Array(buffer)),
            filename: file.name || "paste",
          });
          await uploadAttachment({
            filePath: tempPath,
            serverId: activeServerId,
            channelId: activeChannelId,
            maxBytes,
          });
        } catch (err) {
          toast.error("Paste failed", String(err));
        }
      }
    };

    document.addEventListener("paste", handler);
    return () => document.removeEventListener("paste", handler);
  }, [activeView, activeModal, activeServerId, activeChannelId, serverAttachmentConfig]);
}

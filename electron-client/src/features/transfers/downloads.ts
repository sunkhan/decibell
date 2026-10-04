import { listen } from "../../lib/ipc";
import { channelKey } from "../../lib/channelKey";
import { useChatStore } from "../../stores/chatStore";
import { useTransfersStore } from "../../stores/transfersStore";
import { toast } from "../../stores/toastStore";
import type { Attachment, AttachmentKind, DownloadContext, DownloadView } from "../../types";
import { initUploads } from "./uploads";

// Renderer side of the download manager (electron/main/downloads.ts).

let initialised = false;

/// Seed the store from main and follow its events. Once per window.
export function initTransfers(): void {
  if (initialised) return;
  initialised = true;
  initUploads();
  void refreshDownloads();
  void listen<DownloadView>("downloads_changed", (e) => {
    useTransfersStore.getState().upsertDownload(e.payload);
  });
  void listen<{ ids: string[] }>("downloads_removed", (e) => {
    useTransfersStore.getState().removeDownloads(e.payload.ids);
  });
}

/// Full list from main, including the on-disk check that marks
/// finished files the user has since deleted.
export async function refreshDownloads(): Promise<void> {
  try {
    const list = await window.decibell.downloads.list();
    if (Array.isArray(list)) useTransfersStore.getState().setDownloads(list);
  } catch (e) {
    console.warn("[transfers] list failed:", e);
  }
}

/// Hand the download settings to main (startup and every change).
export function pushDownloadConfig(): void {
  const { downloadDir, askDownloadLocation } = useTransfersStore.getState();
  window.decibell.downloads
    .configure({ dir: downloadDir, askEachTime: askDownloadLocation })
    .catch((e) => console.warn("[transfers] configure failed:", e));
}

interface Located {
  attachment: Attachment | null;
  context: DownloadContext;
}

/// Find the attachment in the loaded messages, for its metadata and the
/// message it came from. Only on a click, so a scan is fine.
function locate(serverId: string, attachmentId: number): Located {
  const chat = useChatStore.getState();
  const serverName = chat.servers.find((s) => s.id === serverId)?.name ?? "";
  const channels = chat.channelsByServer[serverId] ?? [];
  for (const ch of channels) {
    const messages = chat.messagesByChannel[channelKey(serverId, ch.id)];
    if (!messages) continue;
    for (const m of messages) {
      const a = m.attachments.find((x) => x.id === attachmentId);
      if (a) {
        return {
          attachment: a,
          context: { serverName, channelId: ch.id, channelName: ch.name, messageId: m.id, sender: m.sender },
        };
      }
    }
  }
  const channelId = chat.activeServerId === serverId ? chat.activeChannelId ?? "" : "";
  return {
    attachment: null,
    context: {
      serverName,
      channelId,
      channelName: channels.find((c) => c.id === channelId)?.name ?? "",
      messageId: 0,
      sender: "",
    },
  };
}

/// Download an attachment through the manager: straight into the
/// download folder, or via a save dialog for "Save as…" (and when the
/// user asked to choose every time). Progress shows in the Transfers
/// panel; only a failure to start is toasted. Resolves whether it
/// started (false for a dismissed dialog or a failure).
export async function startDownload(
  serverId: string,
  attachmentId: number,
  opts: {
    saveAs?: boolean;
    /// Used when the attachment isn't in the loaded messages.
    fallback?: { filename?: string; mime?: string; kind?: AttachmentKind; sizeBytes?: number };
  } = {},
): Promise<boolean> {
  const { attachment, context } = locate(serverId, attachmentId);
  const fb = opts.fallback ?? {};
  try {
    const res = await window.decibell.downloads.start({
      serverId,
      attachmentId,
      filename: attachment?.filename ?? fb.filename ?? "attachment",
      mime: attachment?.mime ?? fb.mime ?? "application/octet-stream",
      kind: attachment?.kind ?? fb.kind ?? "document",
      sizeBytes: attachment?.sizeBytes ?? fb.sizeBytes ?? 0,
      saveAs: opts.saveAs ?? false,
      context,
    });
    if (res && "error" in res) {
      toast.error("Download failed", res.error);
      return false;
    }
    if (!res) return false;
    useTransfersStore.getState().noteStarted();
    return true;
  } catch (e) {
    toast.error("Download failed", String(e));
    return false;
  }
}

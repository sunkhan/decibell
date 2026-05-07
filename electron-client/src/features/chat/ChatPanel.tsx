import { useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import PendingAttachmentsRow from "./PendingAttachmentsRow";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import { pickFiles, ATTACHMENT_FILTERS } from "./filePicker";
import { enqueueUpload } from "./uploadAttachment";

function generateNonce(): string {
  return `n-${Date.now()}-${Math.floor(Math.random() * 1_000_000).toString(36)}`;
}

function generatePendingId(): string {
  return `att-${Date.now()}-${Math.floor(Math.random() * 1e9).toString(36)}`;
}

/// Read picked file paths into File objects via the preload-exposed
/// fs bridge (Node fs in main). `fetch('file://')` from the renderer
/// is blocked by Chromium's same-origin enforcement even with
/// webSecurity: false in some scenarios, so we route through main
/// for reliability.
async function pathsToFiles(paths: string[]): Promise<File[]> {
  const out: File[] = [];
  for (const p of paths) {
    try {
      const bytes = await window.decibell.fs.readFile(p);
      const filename = p.split(/[/\\]/).pop() ?? "file";
      // Guess MIME from extension. Browsers do better than this
      // server-side, but for "did the user pick an image" the
      // extension is enough. The actual server will sniff bytes.
      const mime = guessMime(filename);
      out.push(new File([bytes], filename, { type: mime }));
    } catch (e) {
      console.error("readFile:", p, e);
    }
  }
  return out;
}

function guessMime(filename: string): string {
  const ext = filename.toLowerCase().split(".").pop() ?? "";
  const map: Record<string, string> = {
    png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
    webp: "image/webp", bmp: "image/bmp", tiff: "image/tiff", avif: "image/avif",
    mp4: "video/mp4", webm: "video/webm", mov: "video/quicktime",
    mkv: "video/x-matroska", avi: "video/x-msvideo",
    mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg",
    m4a: "audio/mp4", flac: "audio/flac", opus: "audio/opus",
    pdf: "application/pdf", txt: "text/plain", json: "application/json",
  };
  return map[ext] ?? "application/octet-stream";
}

export default function ChatPanel() {
  const username = useAuthStore((s) => s.username);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const messagesByChannel = useChatStore((s) => s.messagesByChannel);
  const historyLoading = useChatStore((s) => s.historyLoading);
  const dragActive = useUiStore((s) => s.dragActive);
  const dragHoveredKey = useUiStore((s) => s.dragHoveredKey);

  const [draft, setDraft] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  const editorRef = useRef<RichInputHandle>(null);
  const virtuosoRef = useRef<VirtuosoHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);

  const channels = activeServerId ? channelsByServer[activeServerId] ?? [] : [];
  const channel = channels.find((c) => c.id === activeChannelId) ?? null;
  const channelName = channel?.name ?? activeChannelId ?? null;
  const messages = activeChannelId ? messagesByChannel[activeChannelId] ?? [] : [];
  const loading = activeChannelId ? historyLoading[activeChannelId] === true : false;
  const dropHoveredHere = dragHoveredKey === "active-input";

  useEffect(() => {
    if (!activeChannelId) return;
    const id = setTimeout(() => {
      virtuosoRef.current?.scrollToIndex({
        index: "LAST",
        behavior: "auto",
        align: "end",
      });
    }, 0);
    return () => clearTimeout(id);
  }, [activeChannelId, messages.length]);


  const bubbles = useMemo(() => {
    return messages.map((m, i) => ({
      message: m,
      grouped: shouldGroup(messages[i - 1], m),
      isLast: i === messages.length - 1,
    }));
  }, [messages]);

  const handlePickFiles = async () => {
    if (!activeServerId || !activeChannelId) return;
    const paths = await pickFiles({ multiple: true, filters: ATTACHMENT_FILTERS });
    if (!paths) return;
    const files = await pathsToFiles(paths);
    for (const file of files) {
      const pendingId = generatePendingId();
      enqueueUpload({
        pendingId,
        serverId: activeServerId,
        channelId: activeChannelId,
        file,
      }).catch(() => {});
    }
  };

  const handleSend = async () => {
    const content = (editorRef.current?.getValue() ?? "").trim();
    if (!activeServerId || !activeChannelId || !username) return;

    const channelPendings = useAttachmentsStore
      .getState()
      .selectForChannel(activeServerId, activeChannelId);
    if (!content && channelPendings.length === 0) return;

    const livePendings = channelPendings.filter((p) => p.status !== "failed");
    const pendingIds = livePendings.map((p) => p.pendingId);

    const nonce = generateNonce();
    useChatStore.getState().addMessage({
      id: 0,
      channelId: activeChannelId,
      sender: username,
      content,
      timestamp: String(Math.floor(Date.now() / 1000)),
      attachments: [],
      nonce,
      pendingAttachmentIds: pendingIds.length > 0 ? pendingIds : undefined,
    });
    editorRef.current?.clear();
    setDraft("");

    const waitForUploads = async (): Promise<number[]> => {
      const ids: number[] = [];
      while (true) {
        const current = pendingIds
          .map((id) => useAttachmentsStore.getState().pendings[id])
          .filter((p): p is NonNullable<typeof p> => Boolean(p));
        if (current.every((p) => p.status === "ready" || p.status === "failed")) {
          for (const p of current) {
            if (p.status === "ready" && p.attachmentId !== null) {
              ids.push(p.attachmentId);
            }
          }
          return ids;
        }
        await new Promise((r) => setTimeout(r, 100));
      }
    };

    let attachmentIds: number[] = [];
    if (pendingIds.length > 0) {
      attachmentIds = await waitForUploads();
    }

    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
        message: content,
        attachmentIds: attachmentIds.length > 0 ? attachmentIds : undefined,
        nonce,
      });
    } catch (err) {
      console.error("send_channel_message:", err);
    }

    for (const id of pendingIds) {
      useAttachmentsStore.getState().removePending(id);
    }
  };

  // Suppress the input wrapper's default drop-handling so dragged
  // files don't appear as a path string in the message input —
  // useDragDrop at the window level handles the upload.
  const suppressDrop = (e: React.DragEvent<HTMLElement>) => {
    e.preventDefault();
  };

  if (!activeServerId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid text-sm text-text-muted">
        Pick a server to start chatting.
      </div>
    );
  }

  if (!activeChannelId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid text-sm text-text-muted">
        Pick a channel.
      </div>
    );
  }

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      <div className="flex h-12 items-center border-b border-border-divider px-4 text-sm font-semibold text-text-bright">
        <span className="mr-1.5 text-text-muted">#</span>
        {channelName}
      </div>

      <div className="flex flex-1 flex-col">
        {loading && messages.length === 0 ? (
          <div className="flex flex-1 items-center justify-center text-sm text-text-muted">
            Loading history…
          </div>
        ) : messages.length === 0 ? (
          <div className="flex flex-1 items-center justify-center text-sm text-text-muted">
            No messages yet — say hello.
          </div>
        ) : (
          <Virtuoso
            ref={virtuosoRef}
            data={bubbles}
            initialTopMostItemIndex={Math.max(0, bubbles.length - 1)}
            followOutput="smooth"
            itemContent={(_index, item) => (
              <MessageBubble
                message={item.message}
                grouped={item.grouped}
                serverId={activeServerId}
                isLast={item.isLast}
              />
            )}
            className="flex-1"
          />
        )}
      </div>

      {/* Input bar — pending attachments live INSIDE the rounded chrome
          so adding files visibly expands the bar upward (Discord pattern).
          Drop-target wiring: the bar lights up while a drag is in flight,
          saturated state when the cursor is over the bar. */}
      <div className="px-3 py-2" data-drop-target="active-input">
        <div
          onDragOver={suppressDrop}
          onDrop={suppressDrop}
          className={`relative flex min-h-[54px] flex-col gap-2.5 rounded-xl border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)] ${
            dropHoveredHere
              ? "border-accent bg-accent-soft/50 animate-[dropTargetIn_0.18s_ease_both]"
              : dragActive
                ? "border-transparent animate-[dropPulse_1.6s_ease-in-out_infinite]"
                : "border-border"
          }`}
        >
          <PendingAttachmentsRow />
          <div className="flex items-center gap-2.5">
            {dropHoveredHere && (
              <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center gap-2 rounded-xl bg-gradient-to-b from-accent-soft/80 to-accent-soft/40 backdrop-blur-[3px]">
                <svg
                  width="20" height="20" viewBox="0 0 24 24" fill="none"
                  stroke="currentColor" strokeWidth="2" strokeLinecap="round"
                  strokeLinejoin="round"
                  className="animate-[dropTargetIn_0.18s_ease_both] text-accent-bright"
                >
                  <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
                  <polyline points="17 8 12 3 7 8" />
                  <line x1="12" y1="3" x2="12" y2="15" />
                </svg>
                <span className="text-[13px] font-semibold text-accent-bright">
                  Drop to upload to #{channelName ?? "channel"}
                </span>
              </div>
            )}
            <button
              onClick={handlePickFiles}
              title="Attach files"
              className="flex h-[34px] w-[34px] shrink-0 self-end items-center justify-center rounded-lg bg-surface-hover text-text-muted transition-colors hover:bg-accent-soft hover:text-accent"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <circle cx="12" cy="12" r="10" />
                <line x1="12" y1="8" x2="12" y2="16" />
                <line x1="8" y1="12" x2="16" y2="12" />
              </svg>
            </button>
            <RichInput
              ref={editorRef}
              onChange={setDraft}
              onEnter={handleSend}
              placeholder={`Message #${channelName ?? "channel"}`}
              maxHeight={160}
              className="flex-1 bg-transparent text-sm leading-snug text-text-primary"
            />
            <div className="flex shrink-0 self-end gap-1">
              <div className="relative">
                <button
                  ref={emojiTriggerRef}
                  onClick={() => setPickerOpen((v) => !v)}
                  title="Emoji"
                  className={`flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-md transition-colors ${
                    pickerOpen
                      ? "bg-surface-hover text-text-secondary"
                      : "text-text-muted hover:text-text-secondary"
                  }`}
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                    <circle cx="12" cy="12" r="10" />
                    <path d="M8 14s1.5 2 4 2 4-2 4-2" />
                    <line x1="9" y1="9" x2="9.01" y2="9" />
                    <line x1="15" y1="9" x2="15.01" y2="9" />
                  </svg>
                </button>
                {pickerOpen && (
                  <EmojiPicker
                    onSelect={(emoji) => {
                      editorRef.current?.insertEmoji(emoji);
                      editorRef.current?.focus();
                    }}
                    onClose={() => setPickerOpen(false)}
                    triggerRef={emojiTriggerRef}
                  />
                )}
              </div>
              <button
                onClick={handleSend}
                disabled={!draft.trim() && useAttachmentsStore
                  .getState()
                  .selectForChannel(activeServerId, activeChannelId)
                  .filter((p) => p.status !== "failed").length === 0}
                className="flex h-[34px] w-[34px] shrink-0 items-center justify-center rounded-md bg-accent text-white transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
                title="Send"
              >
                <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                  <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
                </svg>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

import { useState, useRef, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { Virtuoso } from "react-virtuoso";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useDraftsStore } from "../../stores/draftsStore";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import MessageBubble, { shouldGroup } from "./MessageBubble";
import { useChatEvents } from "./useChatEvents";
import WelcomeState from "./WelcomeState";
import EmojiPicker from "./EmojiPicker";
import RichInput, { type RichInputHandle } from "../../components/editor/RichInput";
import PendingAttachmentsRow from "./PendingAttachmentsRow";
import { kindFromMime, formatBytes } from "./attachmentHelpers";

// Must match the INITIAL_FIRST_INDEX seed in chatStore.prependHistory. Keeps
// Virtuoso's firstItemIndex anchored well above zero so any realistic amount
// of prepending still leaves room to decrement.
const INITIAL_FIRST_INDEX = 1_000_000_000;

// Stable key for a message (server id when known; ephemeral tuple otherwise).
// Shared between Virtuoso's computeItemKey and the grouping memo so the two
// stay in lockstep.
function messageKey(
  message: { id: number; timestamp: string; sender: string },
  fallbackIndex: number,
): string {
  return message.id !== 0
    ? String(message.id)
    : `eph-${message.timestamp}-${message.sender}-${fallbackIndex}`;
}

// Inner Virtuoso wrapper. Mounts only once messages are non-empty (parent
// gates this) and has key={channelId}, so this component's lifetime equals
// "one Virtuoso instance for one channel visit". That lets us capture
// initialTopMostItemIndex into useState's lazy initializer at first render
// and have it stay stable thereafter — Virtuoso then doesn't re-apply it
// on each prepend, which was fighting with firstItemIndex anchoring and
// causing the scroll-up stutter.
function MessagesView({
  serverId,
  channelName,
  messages,
  firstItemIndex,
  hasMoreHistory,
  historyLoading,
  fetchOlderPage,
}: {
  serverId: string | null;
  channelName: string | null;
  messages: import("../../types").Message[];
  firstItemIndex: number;
  hasMoreHistory: boolean;
  historyLoading: boolean;
  fetchOlderPage: () => void;
}) {
  // Captured once at mount, stable for the component's lifetime.
  const [initialTopMostItemIndex] = useState(() =>
    Math.max(0, messages.length - 1),
  );

  const groupedByMessageKey = useMemo(() => {
    const map = new Map<string, boolean>();
    for (let i = 0; i < messages.length; i++) {
      const key = messageKey(messages[i], i);
      map.set(key, i > 0 ? shouldGroup(messages[i - 1], messages[i]) : false);
    }
    return map;
  }, [messages]);

  return (
    <Virtuoso
      data={messages}
      firstItemIndex={firstItemIndex}
      initialTopMostItemIndex={initialTopMostItemIndex}
      followOutput="auto"
      increaseViewportBy={{ top: 800, bottom: 200 }}
      startReached={fetchOlderPage}
      rangeChanged={({ startIndex }) => {
        const position = startIndex - firstItemIndex;
        if (position <= 5) fetchOlderPage();
      }}
      computeItemKey={(index, message) => {
        if (!message) return `idx-${index}`;
        const position = index - firstItemIndex;
        return messageKey(message, position);
      }}
      components={{
        Header: () => {
          if (historyLoading) {
            return (
              <div className="flex justify-center py-2 text-[11px] text-text-muted">
                Loading older messages…
              </div>
            );
          }
          if (hasMoreHistory === false) {
            return (
              <div className="flex justify-center py-2 text-[11px] text-text-muted">
                Start of #{channelName ?? "channel"}
              </div>
            );
          }
          return null;
        },
      }}
      itemContent={(index, message) => {
        const position = index - firstItemIndex;
        const grouped =
          groupedByMessageKey.get(messageKey(message, position)) ?? false;
        return (
          <div className="px-4">
            <MessageBubble
              message={message}
              grouped={grouped}
              serverId={serverId}
            />
          </div>
        );
      }}
      style={{ flex: 1, minHeight: 0 }}
    />
  );
}

export function ChatHeader() {
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const membersPanelVisible = useUiStore((s) => s.membersPanelVisible);
  const toggleMembersPanel = useUiStore((s) => s.toggleMembersPanel);

  const channelName = activeServerId
    ? channelsByServer[activeServerId]?.find(
        (ch) => ch.id === activeChannelId
      )?.name
    : null;

  if (!activeChannelId) return null;

  return (
    <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border bg-bg-mid px-4">
      <span className="font-channel text-[16px] font-medium text-text-muted">#</span>
      <span className="font-display text-[15px] font-semibold text-text-bright">
        {channelName ?? activeChannelId}
      </span>
      <div className="h-5 w-px bg-border-divider" />
      <span className="flex-1 text-[13px] text-text-muted">
        The main hangout — say whatever
      </span>
      <div className="flex gap-1">
        <button className="flex h-8 w-8 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary">
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
        </button>
        <button
          onClick={toggleMembersPanel}
          className={`flex h-8 w-8 items-center justify-center rounded-md transition-colors ${
            membersPanelVisible
              ? "text-text-secondary bg-surface-hover"
              : "text-text-muted hover:bg-surface-hover hover:text-text-secondary"
          }`}
          title={membersPanelVisible ? "Hide members" : "Show members"}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <rect x="3" y="3" width="18" height="18" rx="2" /><line x1="9" y1="3" x2="9" y2="21" />
          </svg>
        </button>
      </div>
    </div>
  );
}

export default function ChatPanel({ hideHeader = false }: { hideHeader?: boolean }) {
  useChatEvents();

  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const messagesByChannel = useChatStore((s) => s.messagesByChannel);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const hasMoreHistory = useChatStore((s) => s.hasMoreHistory);
  const historyLoading = useChatStore((s) => s.historyLoading);
  const historyFetched = useChatStore((s) => s.historyFetched);
  const firstItemIndexByChannel = useChatStore((s) => s.firstItemIndexByChannel);
  const activeView = useUiStore((s) => s.activeView);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const editorRef = useRef<RichInputHandle>(null);
  const emojiTriggerRef = useRef<HTMLButtonElement>(null);

  const messages = activeChannelId
    ? messagesByChannel[activeChannelId] ?? []
    : [];

  const channelName = activeServerId
    ? channelsByServer[activeServerId]?.find(
        (ch) => ch.id === activeChannelId
      )?.name
    : null;

  // Virtuoso's sliding index for the oldest currently-loaded item. Decrements
  // on each prepend (managed in chatStore.prependHistory) so the virtualizer
  // can anchor the visible row without our own pre-scroll measurement dance.
  const firstItemIndex = activeChannelId
    ? firstItemIndexByChannel[activeChannelId] ?? INITIAL_FIRST_INDEX
    : INITIAL_FIRST_INDEX;

  // Bottom-start on channel open is now handled by MessagesView's lazy
  // useState capture of initialTopMostItemIndex — see that component for
  // the rationale. The previous imperative scrollToIndex + memoized
  // grouping logic moved there.

  useEffect(() => {
    setSendError(null);
    setPickerOpen(false);
    const stored = activeChannelId
      ? useDraftsStore.getState().channelDrafts[activeChannelId] ?? ""
      : "";
    editorRef.current?.setValue(stored);
    setInput(stored);
  }, [activeChannelId]);

  // First-open history fetch. Fires once per channel the user visits; server
  // caps the page size so this is bounded regardless of channel volume.
  useEffect(() => {
    if (!activeServerId || !activeChannelId) return;
    if (historyFetched[activeChannelId]) return;
    useChatStore.getState().markHistoryFetched(activeChannelId);
    useChatStore.getState().setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: 0,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId, false);
    });
  }, [activeServerId, activeChannelId, historyFetched]);

  // Virtuoso calls this whenever the top of the list scrolls into view
  // (including, helpfully, when the initial page doesn't fill the viewport
  // so there's no scrollbar to interact with). Our own guards prevent
  // double-fires while a request is already in flight.
  const fetchOlderPage = () => {
    if (!activeServerId || !activeChannelId) return;
    if (!hasMoreHistory[activeChannelId]) return;
    if (historyLoading[activeChannelId]) return;
    const firstId = messages.find((m) => m.id !== 0)?.id ?? 0;
    if (firstId === 0) return;
    useChatStore.getState().setHistoryLoading(activeChannelId, true);
    invoke("request_channel_history", {
      serverId: activeServerId,
      channelId: activeChannelId,
      beforeId: firstId,
      limit: 50,
    }).catch((err) => {
      console.error("request_channel_history (paginate) failed", err);
      useChatStore.getState().setHistoryLoading(activeChannelId!, false);
    });
  };

  // Auto-focus editor when user starts typing anywhere
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key.length !== 1) return;
      const target = e.target as HTMLElement | null;
      const tag = target?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (target?.isContentEditable) return;
      editorRef.current?.focus();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, []);

  const handleSend = async () => {
    const value = editorRef.current?.getValue() ?? input;
    if (!activeServerId || !activeChannelId) return;

    const pending = useAttachmentsStore.getState().selectForChannel(activeChannelId);
    const stillUploading = pending.some((a) => a.status === "uploading");
    if (stillUploading) {
      setSendError("Wait for uploads to finish.");
      return;
    }
    const readyIds = pending
      .filter((a) => a.status === "ready" && a.attachmentId)
      .map((a) => a.attachmentId as number);
    if (!value.trim() && readyIds.length === 0) return;

    setSending(true);
    setSendError(null);
    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
        message: value.trim(),
        attachmentIds: readyIds,
      });
      editorRef.current?.clear();
      setInput("");
      useDraftsStore.getState().clearChannelDraft(activeChannelId);
      setPickerOpen(false);
      useAttachmentsStore.getState().clearChannel(activeChannelId);
    } catch (err) {
      setSendError(String(err));
    } finally {
      setSending(false);
    }
  };

  // --- File picker + upload kick-off -------------------------------------

  const serverAttachmentConfig = useChatStore((s) => s.serverAttachmentConfig);

  const handleAttach = async () => {
    if (!activeServerId || !activeChannelId) return;
    const cfg = serverAttachmentConfig[activeServerId];
    if (!cfg || cfg.port === 0) {
      setSendError("This server does not support attachments.");
      return;
    }
    let picked: string[] | null = null;
    try {
      const selection = await openDialog({
        multiple: true,
        directory: false,
        title: "Choose files to attach",
      });
      if (!selection) return;
      picked = Array.isArray(selection) ? selection : [selection];
    } catch (err) {
      setSendError(`File picker failed: ${err}`);
      return;
    }
    if (!picked || picked.length === 0) return;

    for (const filePath of picked) {
      await startUpload(filePath);
    }
  };

  const startUpload = async (filePath: string) => {
    if (!activeServerId || !activeChannelId) return;
    type StatResult = {
      filename: string;
      sizeBytes: number;
      mime: string;
      width: number;
      height: number;
    };
    let meta: StatResult;
    try {
      meta = await invoke<StatResult>("stat_attachment_file", { path: filePath });
    } catch (err) {
      setSendError(`Could not read ${filePath}: ${err}`);
      return;
    }

    const cfg = serverAttachmentConfig[activeServerId];
    if (cfg && cfg.maxBytes > 0 && meta.sizeBytes > cfg.maxBytes) {
      setSendError(
        `${meta.filename} is ${formatBytes(meta.sizeBytes)}; this server's cap is ${formatBytes(cfg.maxBytes)}.`
      );
      return;
    }

    const pendingId = `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
    const entry: PendingAttachment = {
      pendingId,
      channelId: activeChannelId,
      filename: meta.filename,
      mime: meta.mime,
      kind: kindFromMime(meta.mime),
      totalBytes: meta.sizeBytes,
      transferredBytes: 0,
      status: "uploading",
    };
    useAttachmentsStore.getState().addPending(entry);

    invoke("upload_attachment", {
      req: {
        pendingId,
        serverId: activeServerId,
        channelId: activeChannelId,
        filePath,
        filename: meta.filename,
        mime: meta.mime,
        width: meta.width,
        height: meta.height,
      },
    }).catch((err) => {
      useAttachmentsStore.getState().markFailed(pendingId, String(err), false);
    });
  };

  const handleInputChange = (value: string) => {
    setInput(value);
    if (activeChannelId) {
      useDraftsStore.getState().setChannelDraft(activeChannelId, value);
    }
  };

  const insertEmoji = (emoji: string) => {
    editorRef.current?.insertEmoji(emoji);
  };

  // Empty-state short-circuits
  if (activeView === "home" || !activeChannelId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-mid">
        <p className="text-sm text-text-muted">
          Select a channel to start chatting
        </p>
      </div>
    );
  }

  const isEmpty = messages.length === 0;
  const isLoadingFirstPage =
    isEmpty &&
    (historyLoading[activeChannelId] ||
      !historyFetched[activeChannelId]);

  return (
    <div className="flex min-w-0 flex-1 flex-col bg-bg-mid">
      {/* Channel header */}
      {!hideHeader && <ChatHeader />}

      {/* Messages */}
      <div className="flex min-h-0 flex-1 flex-col">
        {isEmpty ? (
          <div className="flex flex-1 items-center justify-center">
            {isLoadingFirstPage ? (
              <span className="text-[12px] text-text-muted">Loading…</span>
            ) : (
              <WelcomeState channelName={channelName ?? "channel"} />
            )}
          </div>
        ) : (
          <MessagesView
            // key={channelId} ensures a fresh MessagesView instance per
            // channel, so the lazy useState that captures
            // initialTopMostItemIndex re-runs and lands at the newest
            // message every channel switch.
            key={activeChannelId}
            serverId={activeServerId}
            channelName={channelName ?? null}
            messages={messages}
            firstItemIndex={firstItemIndex}
            hasMoreHistory={hasMoreHistory[activeChannelId] === true}
            historyLoading={!!historyLoading[activeChannelId]}
            fetchOlderPage={fetchOlderPage}
          />
        )}
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Pending attachments (uploaded / uploading) for this channel */}
      {activeChannelId && <PendingAttachmentsRow channelId={activeChannelId} />}

      {/* Input bar. Matched top + bottom padding so the breathing room
          between the last message and the input row mirrors the gap between
          the input row and the bottom of the client. */}
      <div className="px-3 py-2">
        <div className="flex min-h-[54px] items-center gap-2.5 rounded-xl border border-border bg-bg-light px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
          <button
            onClick={handleAttach}
            title="Attach files"
            className="flex h-7 w-7 shrink-0 self-end items-center justify-center rounded-full bg-surface-hover text-text-muted transition-colors hover:bg-accent-soft hover:text-accent"
          >
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <circle cx="12" cy="12" r="10" /><line x1="12" y1="8" x2="12" y2="16" /><line x1="8" y1="12" x2="16" y2="12" />
            </svg>
          </button>
          <RichInput
            ref={editorRef}
            onChange={handleInputChange}
            onEnter={handleSend}
            disabled={sending}
            placeholder={`Message #${channelName ?? "channel"}`}
            className="flex-1 bg-transparent text-sm leading-snug text-text-primary"
            maxHeight={160}
          />
          <div className="flex shrink-0 self-end gap-1">
            <div className="relative">
              <button
                ref={emojiTriggerRef}
                onClick={() => setPickerOpen((v) => !v)}
                className={`flex h-7 w-7 cursor-pointer items-center justify-center rounded-md transition-colors ${
                  pickerOpen
                    ? "bg-surface-hover text-text-secondary"
                    : "text-text-muted hover:text-text-secondary"
                }`}
                title="Emoji"
              >
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <circle cx="12" cy="12" r="10" /><path d="M8 14s1.5 2 4 2 4-2 4-2" /><line x1="9" y1="9" x2="9.01" y2="9" /><line x1="15" y1="9" x2="15.01" y2="9" />
                </svg>
              </button>
              {pickerOpen && (
                <EmojiPicker
                  onSelect={(emoji) => insertEmoji(emoji)}
                  onClose={() => setPickerOpen(false)}
                  triggerRef={emojiTriggerRef}
                />
              )}
            </div>
            <button
              onClick={handleSend}
              disabled={sending || !input.trim()}
              className="flex h-[34px] w-[34px] shrink-0 items-center justify-center rounded-md bg-accent text-white transition-all hover:bg-accent-hover active:scale-95 disabled:cursor-not-allowed disabled:opacity-40"
            >
              <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor">
                <path d="M2.01 21L23 12 2.01 3 2 10l15 2-15 2z" />
              </svg>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
